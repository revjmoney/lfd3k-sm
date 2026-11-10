/* OLED panel. Task 5 replaces the stub in panel.c with the real display. */
#ifndef PANEL_H
#define PANEL_H
#include <stdint.h>

/* Copy the current 128x64 SSD1306 framebuffer (1024 bytes, page order as the
 * controller stores it) into out. Called from the modem task. */
void panel_snapshot(uint8_t out[1024]);

#endif
