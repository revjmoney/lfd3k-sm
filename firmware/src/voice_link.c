#include "voice_link.h"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include "esp_log.h"
#include "esp_netif.h"
#include "freertos/FreeRTOS.h"
#include "freertos/stream_buffer.h"
#include "freertos/task.h"
#include "lfd3k.h"
#include "lwip/inet.h"
#include "lwip/sockets.h"

static const char *TAG = "voice";

#define SLOTS        3      /* sessions 1 to 3 */
#define VBUF_BYTES   1536   /* each stream buffer, each way */
#define IO_CHUNK     512    /* listener's socket reads */
#define TXB_BYTES    256    /* per slot: taken from tx, not yet sent */
#define RX_PEND      272    /* per slot: from rx, not yet taken by the core (a whole
                             * COBS frame: VHP_FRAME_MAX 260 decoded, 264 on the wire) */
#define OUT_CHUNK    256    /* pump's lfd3k_voice_out chunk */
#define SELECT_US    10000  /* listener wakes at least every 10 ms */
#define TASK_STACK   4096

/* Slot life (s_state, changed only under s_mux):
 *   FREE    -> OPENING  listener accepted a socket
 *   OPENING -> OPEN     pump: lfd3k_voice_open said yes
 *   OPENING -> CLOSING  pump: lfd3k_voice_open said no
 *   OPEN    -> CLOSING  pump: lfd3k_voice_alive went false (timeout or Leave)
 *   any but FREE -> CLOSED  listener: socket closed (EOF, error, or CLOSING seen)
 *   CLOSED  -> FREE     pump: lfd3k_voice_close done, buffers reset
 * The listener alone touches the socket; the pump alone calls lfd3k_*. A slot
 * is free again only when both sides are done with it. */
enum { VS_FREE = 0, VS_OPENING, VS_OPEN, VS_CLOSING, VS_CLOSED };

static portMUX_TYPE s_mux = portMUX_INITIALIZER_UNLOCKED;
static volatile uint8_t s_state[SLOTS];

/* rx: socket to modem (listener writes, pump reads); tx: modem to socket (pump
 * writes, listener reads). Static, so no heap and no fragmentation. */
static StreamBufferHandle_t s_rx[SLOTS];
static StreamBufferHandle_t s_tx[SLOTS];
static StaticStreamBuffer_t s_rx_sb[SLOTS];
static StaticStreamBuffer_t s_tx_sb[SLOTS];
static uint8_t s_rx_store[SLOTS][VBUF_BYTES + 1];
static uint8_t s_tx_store[SLOTS][VBUF_BYTES + 1];

static TaskHandle_t s_notify; /* the modem task */

/* Listener task only. */
static int s_fd[SLOTS] = { -1, -1, -1 };
static uint8_t s_txb[SLOTS][TXB_BYTES];
static size_t s_txn[SLOTS], s_txo[SLOTS];
static uint8_t s_io[IO_CHUNK];

/* Modem task (pump) only. */
static uint8_t s_pend[SLOTS][RX_PEND];
static size_t s_pend_len[SLOTS];
static bool s_core_open[SLOTS];
static uint8_t s_out[OUT_CHUNK];

static uint8_t get_state(int i)
{
    portENTER_CRITICAL(&s_mux);
    uint8_t st = s_state[i];
    portEXIT_CRITICAL(&s_mux);
    return st;
}

/* Moves slot i from `from` to `to`; false when another side changed it first. */
static bool move_state(int i, uint8_t from, uint8_t to)
{
    bool ok;
    portENTER_CRITICAL(&s_mux);
    ok = s_state[i] == from;
    if (ok) {
        s_state[i] = to;
    }
    portEXIT_CRITICAL(&s_mux);
    return ok;
}

static void wake(void)
{
    if (s_notify) {
        xTaskNotifyGive(s_notify);
    }
}

/* Listener task. */

/* Address and mask of a Wi-Fi interface, both 0 while it is down. */
static void net_of(const char *ifkey, uint32_t *ip, uint32_t *mask)
{
    *ip = 0;
    *mask = 0;
    esp_netif_t *n = esp_netif_get_handle_from_ifkey(ifkey);
    esp_netif_ip_info_t ii;
    if (n && esp_netif_is_netif_up(n) && esp_netif_get_ip_info(n, &ii) == ESP_OK) {
        *ip = ii.ip.addr;
        *mask = ii.netmask.addr;
    }
}

static bool peer_allowed(uint32_t peer)
{
    uint32_t ap_ip, ap_mask, sta_ip, sta_mask;
    net_of("WIFI_AP_DEF", &ap_ip, &ap_mask);
    net_of("WIFI_STA_DEF", &sta_ip, &sta_mask);
    return voice_peer_ok(peer, ap_ip, ap_mask, sta_ip, sta_mask);
}

/* The socket of slot i is gone: close it and hand the slot to the pump. */
static void drop(int i)
{
    if (s_fd[i] >= 0) {
        close(s_fd[i]);
        s_fd[i] = -1;
    }
    s_txn[i] = s_txo[i] = 0;
    portENTER_CRITICAL(&s_mux);
    if (s_state[i] != VS_FREE) {
        s_state[i] = VS_CLOSED;
    }
    portEXIT_CRITICAL(&s_mux);
    ESP_LOGI(TAG, "session %d closed", i + 1);
    wake();
}

static int open_listener(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (fd < 0) {
        return -1;
    }
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons(VOICE_PORT) };
    sa.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0 || listen(fd, 2) != 0) {
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    ESP_LOGI(TAG, "listening on %d", VOICE_PORT);
    return fd;
}

static void accept_one(int lfd)
{
    struct sockaddr_in sa;
    socklen_t sl = sizeof(sa);
    int fd = accept(lfd, (struct sockaddr *)&sa, &sl);
    if (fd < 0) {
        /* Not just "nothing waiting": select would wake at once on every
         * pass, so give core 0 (IDLE0, the task watchdog) a moment. */
        if (errno != EAGAIN && errno != EWOULDBLOCK) {
            vTaskDelay(pdMS_TO_TICKS(10));
        }
        return;
    }
    char ip[16];
    inet_ntop(AF_INET, &sa.sin_addr, ip, sizeof(ip));
    if (!peer_allowed(sa.sin_addr.s_addr)) {
        ESP_LOGW(TAG, "refused %s", ip);
        close(fd);
        return;
    }
    int i = 0;
    while (i < SLOTS && !(s_fd[i] < 0 && get_state(i) == VS_FREE)) {
        i++;
    }
    if (i == SLOTS) {
        ESP_LOGW(TAG, "full");
        close(fd);
        return;
    }
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    s_fd[i] = fd;
    s_txn[i] = s_txo[i] = 0;
    move_state(i, VS_FREE, VS_OPENING); /* only this task leaves FREE */
    ESP_LOGI(TAG, "session %d from %s", i + 1, ip);
    wake();
}

static void voice_task(void *arg)
{
    (void)arg;
    int lfd = -1;
    for (;;) {
        if (lfd < 0) {
            lfd = open_listener();
            if (lfd < 0) {
                ESP_LOGW(TAG, "listen on %d failed, retry in 1 s", VOICE_PORT);
                vTaskDelay(pdMS_TO_TICKS(1000));
                continue;
            }
        }
        fd_set rs, ws;
        FD_ZERO(&rs);
        FD_ZERO(&ws);
        FD_SET(lfd, &rs);
        int maxfd = lfd;
        bool live[SLOTS];
        for (int i = 0; i < SLOTS; i++) {
            uint8_t st = get_state(i);
            live[i] = false;
            if (s_fd[i] < 0) {
                continue;
            }
            if (st == VS_CLOSING) {
                drop(i); /* the pump asked: the core closed the session */
                continue;
            }
            if (st != VS_OPENING && st != VS_OPEN) {
                continue;
            }
            live[i] = true;
            if (s_txo[i] == s_txn[i]) {
                s_txn[i] = xStreamBufferReceive(s_tx[i], s_txb[i], TXB_BYTES, 0);
                s_txo[i] = 0;
            }
            /* Backpressure: a full rx buffer leaves the socket unread, never dropped. */
            if (xStreamBufferSpacesAvailable(s_rx[i]) > 0) {
                FD_SET(s_fd[i], &rs);
            }
            if (s_txo[i] < s_txn[i]) {
                FD_SET(s_fd[i], &ws);
            }
            if (s_fd[i] > maxfd) {
                maxfd = s_fd[i];
            }
        }
        struct timeval tv = { .tv_sec = 0, .tv_usec = SELECT_US };
        int r = select(maxfd + 1, &rs, &ws, NULL, &tv);
        if (r < 0) {
            vTaskDelay(pdMS_TO_TICKS(10)); /* never spin */
            continue;
        }
        if (r == 0) {
            continue;
        }
        for (int i = 0; i < SLOTS; i++) {
            if (!live[i] || s_fd[i] < 0) {
                continue;
            }
            int fd = s_fd[i];
            if (FD_ISSET(fd, &ws)) {
                ssize_t s = send(fd, s_txb[i] + s_txo[i], s_txn[i] - s_txo[i], 0);
                if (s > 0) {
                    s_txo[i] += (size_t)s;
                } else if (s < 0 && errno != EWOULDBLOCK && errno != EAGAIN) {
                    drop(i);
                    continue;
                }
            }
            if (FD_ISSET(fd, &rs)) {
                size_t space = xStreamBufferSpacesAvailable(s_rx[i]);
                size_t want = space < sizeof(s_io) ? space : sizeof(s_io);
                if (want == 0) {
                    continue;
                }
                ssize_t n = recv(fd, s_io, want, 0);
                if (n > 0) {
                    xStreamBufferSend(s_rx[i], s_io, (size_t)n, 0); /* fits: space was checked */
                    wake();
                } else if (n == 0 || (errno != EWOULDBLOCK && errno != EAGAIN)) {
                    drop(i);
                }
            }
        }
        if (FD_ISSET(lfd, &rs)) {
            accept_one(lfd);
        }
    }
}

void voice_link_start(void)
{
    s_notify = xTaskGetCurrentTaskHandle();
    for (int i = 0; i < SLOTS; i++) {
        s_rx[i] = xStreamBufferCreateStatic(VBUF_BYTES, 1, s_rx_store[i], &s_rx_sb[i]);
        s_tx[i] = xStreamBufferCreateStatic(VBUF_BYTES, 1, s_tx_store[i], &s_tx_sb[i]);
        configASSERT(s_rx[i] && s_tx[i]);
    }
    xTaskCreatePinnedToCore(voice_task, "voice", TASK_STACK, NULL, 6, NULL, 0);
}

/* Modem task pump. */

/* Socket bytes into the core; what it does not take stays for the next pump. */
static void feed(int i, uint8_t session, uint32_t now_ms)
{
    for (int k = 0; k < 8; k++) {
        if (s_pend_len[i] < RX_PEND) {
            s_pend_len[i] += xStreamBufferReceive(s_rx[i], s_pend[i] + s_pend_len[i],
                                                  RX_PEND - s_pend_len[i], 0);
        }
        size_t len = s_pend_len[i];
        if (len == 0) {
            return;
        }
        size_t taken = lfd3k_voice_in(session, s_pend[i], len, now_ms);
        if (taken > len) taken = len;
        memmove(s_pend[i], s_pend[i] + taken, len - taken);
        s_pend_len[i] = len - taken;
        if (taken == 0) {
            return;
        }
    }
}

/* Core output into the tx buffer, never more than it has room for. */
static void drain(int i, uint8_t session)
{
    for (;;) {
        size_t space = xStreamBufferSpacesAvailable(s_tx[i]);
        if (space == 0) {
            return;
        }
        size_t want = space < sizeof(s_out) ? space : sizeof(s_out);
        size_t n = lfd3k_voice_out(session, s_out, want);
        if (n == 0) {
            return;
        }
        xStreamBufferSend(s_tx[i], s_out, n, 0); /* only this task writes tx, so it fits */
    }
}

void voice_link_pump(uint32_t now_ms)
{
    if (!s_notify) {
        return; /* not started */
    }
    for (int i = 0; i < SLOTS; i++) {
        uint8_t session = (uint8_t)(i + 1); /* never 0: that is the host stream */
        switch (get_state(i)) {
        case VS_OPENING:
            if (lfd3k_voice_open(session, now_ms)) {
                s_core_open[i] = true;
                move_state(i, VS_OPENING, VS_OPEN); /* false: the socket already went (CLOSED) */
            } else {
                ESP_LOGW(TAG, "session %u did not open", (unsigned)session);
                move_state(i, VS_OPENING, VS_CLOSING);
            }
            break;
        case VS_OPEN:
            feed(i, session, now_ms);
            drain(i, session);
            if (!lfd3k_voice_alive(session)) {
                /* The core closed it (timeout or Leave); lfd3k_voice_close on
                 * CLOSED is then a no-op. The listener closes the socket. */
                move_state(i, VS_OPEN, VS_CLOSING);
            }
            break;
        case VS_CLOSED:
            if (s_core_open[i]) {
                feed(i, session, now_ms); /* a Leave sent just before the FIN still counts */
                lfd3k_voice_close(session, now_ms);
                s_core_open[i] = false;
            }
            xStreamBufferReset(s_rx[i]);
            xStreamBufferReset(s_tx[i]);
            s_pend_len[i] = 0;
            move_state(i, VS_CLOSED, VS_FREE);
            break;
        default:
            break;
        }
    }
}
