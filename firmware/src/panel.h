/* OLED front panel task and the framebuffer snapshot. */
#ifndef PANEL_H
#define PANEL_H
#include <stdint.h>
#include "lfd3k.h"

/* Starts the panel task (core 0, priority 2). Call from the modem task after
 * hal_radio_init has switched Vext on. */
void panel_start(void);

/* Modem task only: hand the panel a copy of the latest status. The panel task
 * never calls lfd3k_*. Cheap (8 bytes under a spinlock). */
void panel_publish(const lfd3k_status_t *s);

/* Copy the last fully drawn 128x64 SSD1306 framebuffer (1024 bytes, page
 * order, bit 0 = top row of each page) into out. Called from the modem task.
 * Waits at most 5 ms; on timeout returns the previous snapshot. */
void panel_snapshot(uint8_t out[1024]);

#endif
