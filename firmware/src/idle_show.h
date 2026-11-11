/* Idle text show: the word in the middle of the idle panel changes every 8 s loop
 * into another language or a fact, with a different transition each time, and now and
 * then a long sine scroller message. Pure C, no ESP-IDF, every frame a pure function
 * of now_ms and the info struct (the PC renders the same pictures). */
#ifndef IDLE_SHOW_H
#define IDLE_SHOW_H
#include <stdint.h>
#include "panel_render.h"

/* Items. Weights: IDLE 3, every other 1 (info items only while available). */
enum {
    IS_IDLE, IS_ZH, IS_JA, IS_HI, IS_ES, IS_FR, IS_AR, IS_BN, IS_PT, IS_RU, IS_UR, IS_ID, IS_DE, IS_SW, IS_TR,
    IS_CLOCK, IS_UPTIME, IS_TEMP, IS_HEARD, IS_CALLER, IS_GPS,
    IS_ITEMS,
    IS_BLANK = IS_ITEMS /* nothing: the band after the scroller has gone */
};

/* Transitions (names in idle_show_trans_name). */
enum {
    TR_EXPLODE, TR_BLOCKS, TR_SINE, TR_FLAP, TR_TYPE, TR_DRIP, TR_GLITCH, TR_RAIN,
    TR_WARP, TR_CRT, TR_BLINDS, TR_PIXEL,
    TR_COUNT
};

#define IDLE_SLOT_MS 8000u /* one loop */
#define IDLE_TR_AT   4000u /* the transition starts this far into a loop */
#define IDLE_TR_MS   3000u /* and is over by then */

/* Slot roles in the loop schedule. */
enum { SLOT_NORMAL, SLOT_PRE_SCROLL, SLOT_SCROLL, SLOT_SCROLL_TAIL };

typedef struct {
    int role;
    int item;            /* shown at the start of the loop (IS_BLANK in a scroller tail) */
    int next;            /* the item the transition lands on */
    int trans;           /* TR_* */
    uint32_t scroll_at;  /* first scroller slot of this group */
} idle_slot_t;

/* The middle band of the idle panel (y 12..29; the scroller and its wave y 12..31;
 * the explode particles fly over y 13..44 as before). ORs pixels into fb. */
void idle_show_draw(uint8_t *fb, const panel_info_t *info, uint32_t now_ms);

/* The loop whose light order applies in loop slot (the lights hold still through a
 * scroller). */
uint32_t idle_show_light_slot(uint32_t slot);

/* For the PC check and preview. */
void idle_show_slot(const panel_info_t *info, uint32_t slot, idle_slot_t *out);
void idle_show_draw_item(uint8_t *fb, const panel_info_t *info, int item);
void idle_show_draw_trans(uint8_t *fb, const panel_info_t *info, int from, int to, int trans, uint32_t tau);
void idle_show_draw_scroller(uint8_t *fb, const panel_info_t *info, int from, uint32_t u);
uint32_t idle_show_scroll_ms(void); /* scroller length, from its start to the tail transition */
/* PC preview only (never called on the device): play these slots, slot n using
 * slots[n % count], instead of the hashed schedule. count 0 goes back to the hash. */
void idle_show_set_script(const idle_slot_t *slots, uint32_t count);
int idle_show_available(const panel_info_t *info, int item);
const char *idle_show_item_name(int item);
const char *idle_show_trans_name(int trans);

#endif
