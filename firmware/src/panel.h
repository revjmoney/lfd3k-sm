/* OLED front panel task and the framebuffer snapshot. */
#ifndef PANEL_H
#define PANEL_H
#include <stdint.h>
#include "lfd3k.h"
#include "panel_render.h"

/* Starts the panel task (core 0, priority 2). Call from the modem task after
 * hal_radio_init has switched Vext on. */
void panel_start(void);

/* Board name shown on the bottom row (max 16 chars). Call once at boot, before panel_start. */
void panel_set_name(const char *name);

/* Modem task only: hand the panel a copy of the latest status. The panel task
 * never calls lfd3k_*. Cheap (a 10 byte struct copy under a spinlock). */
void panel_publish(const lfd3k_status_t *s);

/* Modem task only: hand the panel a copy of the latest channel trace (about
 * 130 bytes, under the same spinlock as panel_publish). */
void panel_publish_trace(const rf_trace_t *t);

/* Copy the last fully drawn 128x64 SSD1306 framebuffer (1024 bytes, page
 * order, bit 0 = top row of each page) into out. Called from the modem task.
 * Waits at most 5 ms; on timeout returns the previous snapshot. */
void panel_snapshot(uint8_t out[1024]);

#endif
