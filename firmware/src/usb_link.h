/* The modem stream over the native USB-Serial/JTAG port.
 * Two tasks (priority 5) move bytes between the USB driver and two stream
 * buffers; neither calls Rust. The modem task only ever uses the
 * non-blocking calls below. */
#ifndef USB_LINK_H
#define USB_LINK_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

void usb_link_init(void);
/* Wake this task whenever USB bytes arrive. */
void usb_link_set_notify(TaskHandle_t task);
/* Up to len received bytes, waiting at most `wait` ticks. */
size_t usb_link_read(uint8_t *buf, size_t len, TickType_t wait);
/* Queue bytes for the host without blocking; returns how many fit. */
size_t usb_link_write(const uint8_t *buf, size_t len);
/* Free bytes in the TX buffer. */
size_t usb_link_tx_space(void);
/* Waits up to timeout_ms for everything queued so far to leave the USB port
 * (TX buffer empty, the writer idle, the driver done). True when it did;
 * false on timeout (no host reading). */
bool usb_link_flush(uint32_t timeout_ms);

#endif
