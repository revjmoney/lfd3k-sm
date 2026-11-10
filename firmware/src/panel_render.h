/* Pure front panel drawing: no ESP-IDF calls, so the PC can compile it for mockups.
 * Output is a 128x64 SSD1306 framebuffer: 8 pages of 128 column bytes, bit 0 of
 * each byte is the top row of the page. */
#ifndef PANEL_RENDER_H
#define PANEL_RENDER_H
#include <stdint.h>
#include "lfd3k.h"

#define PANEL_FB_BYTES 1024

/* Draws the front panel for status s at time now_ms. Keeps a little animation
 * memory (last state) in statics; call it with non-decreasing now_ms. */
void panel_render(const lfd3k_status_t *s, uint32_t now_ms, uint8_t fb[PANEL_FB_BYTES]);

/* Boot splash: LFD3K-SM, r.420, BL4Z3B4ND. */
void panel_render_splash(uint8_t fb[PANEL_FB_BYTES]);

#endif
