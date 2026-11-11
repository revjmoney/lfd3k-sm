/* Picker screens drawn into a 128x64 SSD1306 frame (1024 bytes, 8 pages of
 * 128 column bytes, bit 0 = top). Pure drawing, no ESP headers: the PC
 * preview compiles this same file. */
#ifndef PICKER_DRAW_H
#define PICKER_DRAW_H
#include <stdint.h>
#include "picker_menu.h"

#define PK_FB_BYTES 1024

/* The AXIS BBS splash at t_ms into the 5 s logo. */
void pk_draw_logo(uint8_t fb[PK_FB_BYTES], uint32_t t_ms);

/* The mode list. */
void pk_draw_list(uint8_t fb[PK_FB_BYTES], const pk_state_t *s, uint32_t now_ms);

/* The short "starting" card shown while the chosen mode is armed. */
void pk_draw_launch(uint8_t fb[PK_FB_BYTES], int mode, uint32_t t_ms);

#endif
