/* One TCP client on its own task. The modem task talks to it only through a
 * command queue, an event queue and two 2 KB stream buffers; this file never
 * calls lfd3k_*. Every command and event carries the socket generation, so
 * the modem task can drop anything that belongs to a socket it has closed. */
#ifndef TCP_LINK_H
#define TCP_LINK_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

enum {
    TCP_EV_CONNECTED = 1, /* same numbers as lfd3k_wire_event */
    TCP_EV_FAILED    = 2,
    TCP_EV_CLOSED    = 3, /* far end closed or the socket broke; all bytes already in rx */
    TCP_EV_CLOSE_ACK = 4, /* the close command was handled: socket gone, buffers idle */
};

typedef struct {
    uint8_t  type;
    uint32_t gen;
} tcp_event_t;

/* Creates the task (priority 6, core 0), queues and buffers. The task wakes
 * `notify` (the modem task) whenever it posts an event or received bytes. */
void tcp_link_init(TaskHandle_t notify);

/* Modem task, none of these block. False when the queue is full (retry). */
bool tcp_link_open(uint32_t gen, const char *host, uint16_t port);
/* Flushes what is in the tx buffer (up to 1 s), closes, then posts CLOSE_ACK.
 * Also aborts a connect in progress. */
bool tcp_link_close(uint32_t gen);
bool tcp_link_event(tcp_event_t *ev);

size_t tcp_link_rx_read(uint8_t *buf, size_t len);
size_t tcp_link_rx_avail(void);
void   tcp_link_rx_discard(void);
size_t tcp_link_tx_space(void);
size_t tcp_link_tx_write(const uint8_t *buf, size_t len);

#endif
