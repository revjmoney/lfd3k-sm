#include "udp_link.h"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include "esp_log.h"
#include "freertos/queue.h"
#include "lwip/sockets.h"

static const char *TAG = "udp";

#define QUEUE_DEPTH 8
#define SELECT_US   1000

enum { CMD_OPEN = 1, CMD_CLOSE = 2 };

typedef struct {
    uint8_t type;
    uint8_t ip[4];
} udp_cmd_t;

static QueueHandle_t s_cmdq;
static QueueHandle_t s_txq; /* modem -> socket */
static QueueHandle_t s_rxq; /* socket -> modem */
static TaskHandle_t s_notify;

static void drain_tx(void)
{
    static udp_dgram_t d; /* this task only */
    while (xQueueReceive(s_txq, &d, 0) == pdTRUE) {
    }
}

static int open_socket(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (fd < 0) {
        ESP_LOGW(TAG, "socket failed (%d)", errno);
        return -1;
    }
    struct sockaddr_in a = {
        .sin_family = AF_INET,
        .sin_port = htons(LFD3K_SIMNET_PORT),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(fd, (struct sockaddr *)&a, sizeof(a)) != 0) {
        ESP_LOGW(TAG, "bind :%d failed (%d)", LFD3K_SIMNET_PORT, errno);
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    return fd;
}

static void udp_task(void *arg)
{
    (void)arg;
    static udp_dgram_t d; /* this task only; kept off the small stack */
    int fd = -1;
    struct sockaddr_in peer = { .sin_family = AF_INET, .sin_port = htons(LFD3K_SIMNET_PORT) };
    for (;;) {
        udp_cmd_t c;
        /* Off: sleep on the command queue. On: only peek at it. */
        TickType_t wait = fd < 0 ? portMAX_DELAY : 0;
        while (xQueueReceive(s_cmdq, &c, wait) == pdTRUE) {
            wait = 0;
            if (c.type == CMD_OPEN) {
                memcpy(&peer.sin_addr.s_addr, c.ip, 4);
                if (fd < 0) {
                    fd = open_socket();
                }
                ESP_LOGI(TAG, "simnet on, peer %u.%u.%u.%u", c.ip[0], c.ip[1], c.ip[2], c.ip[3]);
            } else {
                if (fd >= 0) {
                    close(fd);
                    fd = -1;
                }
                ESP_LOGI(TAG, "simnet off");
            }
            drain_tx(); /* nothing from before the switch goes out */
        }
        if (fd < 0) {
            drain_tx();
            continue;
        }

        while (xQueueReceive(s_txq, &d, 0) == pdTRUE) {
            /* UDP: a datagram that cannot go is a frame lost on the air. */
            sendto(fd, d.data, d.len, 0, (struct sockaddr *)&peer, sizeof(peer));
        }

        fd_set rs;
        FD_ZERO(&rs);
        FD_SET(fd, &rs);
        struct timeval tv = { .tv_sec = 0, .tv_usec = SELECT_US };
        int r = select(fd + 1, &rs, NULL, NULL, &tv);
        if (r < 0) {
            vTaskDelay(pdMS_TO_TICKS(10)); /* never spin */
            continue;
        }
        if (r == 0) {
            continue;
        }
        bool got = false;
        for (int i = 0; i < QUEUE_DEPTH; i++) {
            struct sockaddr_in from;
            socklen_t fl = sizeof(from);
            ssize_t n = recvfrom(fd, d.data, sizeof(d.data), MSG_DONTWAIT, (struct sockaddr *)&from, &fl);
            if (n < 0) {
                break;
            }
            if (from.sin_addr.s_addr != peer.sin_addr.s_addr || n < LFD3K_SIMNET_HDR_LEN) {
                continue; /* only the peer, only whole headers */
            }
            d.len = (uint16_t)n;
            if (xQueueSend(s_rxq, &d, 0) == pdTRUE) { /* full: dropped, like a missed frame */
                got = true;
            }
        }
        if (got && s_notify) {
            xTaskNotifyGive(s_notify);
        }
    }
}

void udp_link_init(TaskHandle_t notify)
{
    s_notify = notify;
    s_cmdq = xQueueCreate(4, sizeof(udp_cmd_t));
    s_txq = xQueueCreate(QUEUE_DEPTH, sizeof(udp_dgram_t));
    s_rxq = xQueueCreate(QUEUE_DEPTH, sizeof(udp_dgram_t));
    configASSERT(s_cmdq && s_txq && s_rxq);
    xTaskCreatePinnedToCore(udp_task, "udp", 4096, NULL, 6, NULL, 0);
}

bool udp_link_open(const uint8_t ip[4])
{
    udp_cmd_t c = { .type = CMD_OPEN };
    memcpy(c.ip, ip, 4);
    return xQueueSend(s_cmdq, &c, 0) == pdTRUE;
}

bool udp_link_close(void)
{
    udp_cmd_t c = { .type = CMD_CLOSE };
    return xQueueSend(s_cmdq, &c, 0) == pdTRUE;
}

bool udp_link_recv(udp_dgram_t *out)
{
    return s_rxq && xQueueReceive(s_rxq, out, 0) == pdTRUE;
}

/* Rust HAL (modem task, inside lfd3k_poll or lfd3k_host_in). Never blocks:
 * a full queue drops the datagram, as a lost frame. */
void lfd3k_hal_udp_send(const uint8_t *d, size_t len)
{
    static udp_dgram_t item; /* modem task only */
    if (!s_txq || !d || len == 0 || len > sizeof(item.data)) {
        return;
    }
    item.len = (uint16_t)len;
    memcpy(item.data, d, len);
    xQueueSend(s_txq, &item, 0);
}
