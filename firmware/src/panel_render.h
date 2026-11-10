/* Pure front panel drawing: no ESP-IDF calls, so the PC can compile it for mockups.
 * Output is a 128x64 SSD1306 framebuffer: 8 pages of 128 column bytes, bit 0 of
 * each byte is the top row of the page. */
#ifndef PANEL_RENDER_H
#define PANEL_RENDER_H
#include <stdint.h>
#include "lfd3k.h"

#define PANEL_FB_BYTES 1024

/* Live channel trace for the idle line: the SX1262's instant RSSI on our channel,
 * one sample every RF_TRACE_MS while listening, oldest first. tx marks samples taken
 * while we were transmitting (we know those exactly). */
#define RF_TRACE_N  64
#define RF_TRACE_MS 50
typedef struct {
    int8_t dbm[RF_TRACE_N]; /* -128..0, oldest first */
    uint8_t tx[RF_TRACE_N]; /* 1 while we were transmitting */
    uint8_t count;          /* valid samples, 0..RF_TRACE_N (newest at the end) */
} rf_trace_t;

/* Hand the renderer the latest trace (NULL or count 0: a plain breathing line). */
void panel_render_set_trace(const rf_trace_t *t);

/* Draws the front panel for status s (name: board name, NULL or empty shows LFD3K-SM) at time now_ms. Keeps a little animation
 * memory (last state) in statics; call it with non-decreasing now_ms. */
void panel_render(const lfd3k_status_t *s, const char *name, uint32_t now_ms, uint8_t fb[PANEL_FB_BYTES]);

/* Boot splash: LFD3K-SM, r.420, BL4Z3B4ND. */
void panel_render_splash(uint8_t fb[PANEL_FB_BYTES]);

#endif
