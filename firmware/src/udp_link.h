/* The SimNet UDP socket (AT+SIMNET) on its own task. The modem task talks to
 * it only through queues; this file never calls lfd3k_*. The socket exists
 * only while SimNet is on: bound to 0.0.0.0:4210, it takes datagrams from the
 * peer's IP alone and sends to peer:4210. It also provides the Rust HAL call
 * lfd3k_hal_udp_send (a non-blocking queue write, modem task only). */
#ifndef UDP_LINK_H
#define UDP_LINK_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lfd3k.h"

typedef struct {
    uint16_t len;
    uint8_t  data[LFD3K_SIMNET_DGRAM_MAX];
} udp_dgram_t;

/* Creates the task (priority 6, core 0) and its queues (8 datagrams each way).
 * The task wakes `notify` (the modem task) whenever a datagram arrives. */
void udp_link_init(TaskHandle_t notify);

/* Modem task, none of these block. False when the command queue is full (retry). */
bool udp_link_open(const uint8_t ip[4]);
bool udp_link_close(void);
/* One received datagram, if any. */
bool udp_link_recv(udp_dgram_t *out);

#endif
