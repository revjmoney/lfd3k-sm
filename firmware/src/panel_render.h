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

/* Wi-Fi strings for the idle bottom row. All fields NUL terminated. ap_pass is only
 * filled (and only drawn) while show_pass is 1; the caller wipes it otherwise. */
typedef struct {
    char ap_ssid[33];
    char ap_pass[64];
    uint8_t show_pass;
    char ip[16]; /* dotted quad, empty without one */
    uint8_t ap_on;
    uint8_t sta_up; /* client side has an IP */
} panel_net_t;

/* Hand the renderer the latest Wi-Fi strings (NULL: none, the name row stays). */
void panel_render_set_net(const panel_net_t *n);

/* Facts for the idle show's info items (the idle word rotates through languages and
 * these). Each item is shown only while its field is set. Strings NUL terminated. */
typedef struct {
    uint8_t time_valid;     /* 1 once SNTP synced: hour and minute are local time */
    uint8_t hour, minute;   /* 24 h */
    uint32_t uptime_s;      /* 0: no uptime item */
    uint8_t temp_valid;
    int8_t temp_c;          /* chip temperature */
    uint16_t heard;         /* modems heard; 0: no HEARD item */
    char last_caller[17];   /* empty: none */
    char gps[24];           /* coordinates as text; empty: none */
    uint8_t hide_location;  /* 1 (default): the gps text is never drawn */
} panel_info_t;

/* Start every panel_info_t from this so the location stays hidden unless asked. */
#define PANEL_INFO_INIT {.hide_location = 1}

/* Hand the renderer the latest facts (NULL: back to none, location hidden). */
void panel_render_set_info(const panel_info_t *info);

/* Draws the front panel for status s (name: board name, NULL or empty shows LFD3K-SM) at time now_ms. Keeps a little animation
 * memory (last state) in statics; call it with non-decreasing now_ms. */
void panel_render(const lfd3k_status_t *s, const char *name, uint32_t now_ms, uint8_t fb[PANEL_FB_BYTES]);

/* Boot splash: LFD3K-SM, r.420, BL4Z3B4ND. */
void panel_render_splash(uint8_t fb[PANEL_FB_BYTES]);

#endif
