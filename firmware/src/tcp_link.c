#include "tcp_link.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/queue.h"
#include "freertos/stream_buffer.h"
#include "lwip/netdb.h"
#include "lwip/sockets.h"

static const char *TAG = "tcp";

#define BUF_BYTES      2048
#define CHUNK          512
#define CONNECT_MS     15000
#define FLUSH_MS       1000
#define HOST_CAP       64

enum { CMD_OPEN = 1, CMD_CLOSE = 2 };

typedef struct {
    uint8_t  type;
    uint32_t gen;
    uint16_t port;
    char     host[HOST_CAP];
} tcp_cmd_t;

static QueueHandle_t s_cmdq;
static QueueHandle_t s_evq;
static StreamBufferHandle_t s_rx; /* socket -> modem */
static StreamBufferHandle_t s_tx; /* modem -> socket */
static TaskHandle_t s_notify;
/* Bytes taken from s_tx but not yet sent on the socket (serve() resets them). */
static uint8_t s_txb[CHUNK];
static size_t s_txn, s_txo;

static int64_t now_ms(void)
{
    return esp_timer_get_time() / 1000;
}

static void wake(void)
{
    if (s_notify) {
        xTaskNotifyGive(s_notify);
    }
}

static void post(uint8_t type, uint32_t gen)
{
    tcp_event_t ev = { .type = type, .gen = gen };
    /* The queue only ever holds a couple of events; wait rather than lose one. */
    while (xQueueSend(s_evq, &ev, pdMS_TO_TICKS(50)) != pdTRUE) {
    }
    wake();
}

/* True when a close command for this socket arrived (it is consumed). */
static bool close_requested(void)
{
    tcp_cmd_t c;
    while (xQueueReceive(s_cmdq, &c, 0) == pdTRUE) {
        if (c.type == CMD_CLOSE) {
            return true;
        }
    }
    return false;
}

static void tx_discard(void)
{
    uint8_t b[CHUNK];
    while (xStreamBufferReceive(s_tx, b, sizeof(b), 0) > 0) {
    }
}

/* Waits for the close command of a socket that is already gone. */
static void wait_close(uint32_t gen)
{
    tcp_cmd_t c;
    do {
        xQueueReceive(s_cmdq, &c, portMAX_DELAY);
    } while (c.type != CMD_CLOSE);
    tx_discard();
    post(TCP_EV_CLOSE_ACK, gen);
}

/* Sends what the modem already queued, for at most FLUSH_MS. */
static void flush_tx(int fd)
{
    uint8_t b[CHUNK];
    int64_t end = now_ms() + FLUSH_MS;
    size_t n = s_txn - s_txo;
    memcpy(b, s_txb + s_txo, n); /* the leftover goes out first, then the buffer */
    s_txo = s_txn = 0;
    do {
        size_t off = 0;
        while (off < n && now_ms() < end) {
            fd_set w;
            FD_ZERO(&w);
            FD_SET(fd, &w);
            struct timeval tv = { .tv_sec = 0, .tv_usec = 50000 };
            if (select(fd + 1, NULL, &w, NULL, &tv) > 0) {
                ssize_t s = send(fd, b + off, n - off, 0);
                if (s < 0 && errno != EWOULDBLOCK && errno != EAGAIN) {
                    return;
                }
                if (s > 0) off += (size_t)s;
            }
        }
        if (off < n) {
            return;
        }
    } while ((n = xStreamBufferReceive(s_tx, b, sizeof(b), 0)) > 0);
}

/* Connects to host:port. Returns a connected, non-blocking, NODELAY socket, or
 * -1 on failure; *aborted is set when a close command came meanwhile. */
static int connect_to(const char *host, uint16_t port, bool *aborted)
{
    *aborted = false;
    char ps[8];
    snprintf(ps, sizeof(ps), "%u", (unsigned)port);
    struct addrinfo hints = { .ai_family = AF_INET, .ai_socktype = SOCK_STREAM };
    struct addrinfo *res = NULL;
    /* getaddrinfo cannot be interrupted; a close is honoured as soon as it returns. */
    if (getaddrinfo(host, ps, &hints, &res) != 0 || !res) {
        return -1;
    }
    int64_t end = now_ms() + CONNECT_MS;
    int fd = -1;
    for (struct addrinfo *ai = res; ai && fd < 0; ai = ai->ai_next) {
        if (close_requested()) {
            *aborted = true;
            break;
        }
        int s = socket(ai->ai_family, ai->ai_socktype, 0);
        if (s < 0) {
            continue;
        }
        fcntl(s, F_SETFL, fcntl(s, F_GETFL, 0) | O_NONBLOCK);
        int r = connect(s, ai->ai_addr, ai->ai_addrlen);
        bool ok = (r == 0);
        if (r < 0 && errno == EINPROGRESS) {
            for (;;) {
                if (close_requested()) {
                    *aborted = true;
                    break;
                }
                if (now_ms() >= end) {
                    break;
                }
                fd_set w;
                FD_ZERO(&w);
                FD_SET(s, &w);
                struct timeval tv = { .tv_sec = 0, .tv_usec = 100000 };
                if (select(s + 1, NULL, &w, NULL, &tv) > 0) {
                    int err = 0;
                    socklen_t el = sizeof(err);
                    ok = getsockopt(s, SOL_SOCKET, SO_ERROR, &err, &el) == 0 && err == 0;
                    break;
                }
            }
        }
        if (ok && !*aborted) {
            int one = 1;
            setsockopt(s, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
            fd = s;
        } else {
            close(s);
            if (*aborted || now_ms() >= end) {
                break;
            }
        }
    }
    freeaddrinfo(res);
    return fd;
}

/* Moves bytes until the far end goes (returns false) or a close is asked (true). */
static bool stream(int fd)
{
    uint8_t rxb[CHUNK];
    uint8_t *txb = s_txb;
    size_t txn = 0, txo = 0;
    s_txn = s_txo = 0;
    for (;;) {
        if (close_requested()) {
            s_txn = txn; /* flush_tx sends the unsent rest first */
            s_txo = txo;
            return true;
        }
        if (txo == txn) {
            txn = xStreamBufferReceive(s_tx, txb, sizeof(txb), 0);
            txo = 0;
        }
        size_t space = xStreamBufferSpacesAvailable(s_rx);
        fd_set rs, ws;
        FD_ZERO(&rs);
        FD_ZERO(&ws);
        if (space > 0) FD_SET(fd, &rs);
        if (txo < txn) FD_SET(fd, &ws);
        struct timeval tv = { .tv_sec = 0, .tv_usec = 20000 };
        int r = select(fd + 1, space > 0 ? &rs : NULL, txo < txn ? &ws : NULL, NULL, &tv);
        if (r < 0) {
            return false;
        }
        if (txo < txn && FD_ISSET(fd, &ws)) {
            ssize_t s = send(fd, txb + txo, txn - txo, 0);
            if (s > 0) {
                txo += (size_t)s;
            } else if (s < 0 && errno != EWOULDBLOCK && errno != EAGAIN) {
                return false;
            }
        }
        if (space > 0 && FD_ISSET(fd, &rs)) {
            size_t want = space < sizeof(rxb) ? space : sizeof(rxb);
            ssize_t n = recv(fd, rxb, want, 0);
            if (n > 0) {
                xStreamBufferSend(s_rx, rxb, (size_t)n, 0); /* fits: space was checked */
                wake();
            } else if (n == 0 || (errno != EWOULDBLOCK && errno != EAGAIN)) {
                return false;
            }
        }
        if (r == 0 && txo == txn && space == 0) {
            vTaskDelay(pdMS_TO_TICKS(10)); /* rx full: let the modem catch up */
        }
    }
}

static void serve(const tcp_cmd_t *c)
{
    bool aborted;
    int fd = connect_to(c->host, c->port, &aborted);
    if (aborted) {
        tx_discard();
        post(TCP_EV_CLOSE_ACK, c->gen);
        return;
    }
    if (fd < 0) {
        ESP_LOGI(TAG, "connect failed");
        post(TCP_EV_FAILED, c->gen);
        wait_close(c->gen);
        return;
    }
    ESP_LOGI(TAG, "connected");
    post(TCP_EV_CONNECTED, c->gen);
    if (stream(fd)) {
        flush_tx(fd);
        close(fd);
        tx_discard();
        post(TCP_EV_CLOSE_ACK, c->gen);
    } else {
        close(fd);
        post(TCP_EV_CLOSED, c->gen);
        wait_close(c->gen);
    }
}

static void tcp_task(void *arg)
{
    (void)arg;
    for (;;) {
        tcp_cmd_t c;
        xQueueReceive(s_cmdq, &c, portMAX_DELAY);
        if (c.type == CMD_OPEN) {
            serve(&c);
        } else {
            /* A close for a socket that is already gone. */
            tx_discard();
            post(TCP_EV_CLOSE_ACK, c.gen);
        }
    }
}

void tcp_link_init(TaskHandle_t notify)
{
    s_notify = notify;
    s_cmdq = xQueueCreate(4, sizeof(tcp_cmd_t));
    s_evq = xQueueCreate(8, sizeof(tcp_event_t));
    s_rx = xStreamBufferCreate(BUF_BYTES, 1);
    s_tx = xStreamBufferCreate(BUF_BYTES, 1);
    configASSERT(s_cmdq && s_evq && s_rx && s_tx);
    xTaskCreatePinnedToCore(tcp_task, "tcp", 6144, NULL, 6, NULL, 0);
}

bool tcp_link_open(uint32_t gen, const char *host, uint16_t port)
{
    tcp_cmd_t c = { .type = CMD_OPEN, .gen = gen, .port = port };
    strlcpy(c.host, host, sizeof(c.host));
    return xQueueSend(s_cmdq, &c, 0) == pdTRUE;
}

bool tcp_link_close(uint32_t gen)
{
    tcp_cmd_t c = { .type = CMD_CLOSE, .gen = gen };
    return xQueueSend(s_cmdq, &c, 0) == pdTRUE;
}

bool tcp_link_event(tcp_event_t *ev)
{
    return xQueueReceive(s_evq, ev, 0) == pdTRUE;
}

size_t tcp_link_rx_read(uint8_t *buf, size_t len)
{
    return xStreamBufferReceive(s_rx, buf, len, 0);
}

size_t tcp_link_rx_avail(void)
{
    return xStreamBufferBytesAvailable(s_rx);
}

void tcp_link_rx_discard(void)
{
    uint8_t b[CHUNK];
    while (xStreamBufferReceive(s_rx, b, sizeof(b), 0) > 0) {
    }
}

size_t tcp_link_tx_space(void)
{
    return xStreamBufferSpacesAvailable(s_tx);
}

size_t tcp_link_tx_write(const uint8_t *buf, size_t len)
{
    return xStreamBufferSend(s_tx, buf, len, 0);
}
