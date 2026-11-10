/* Front panel layout (128x64). Pure C, no ESP-IDF.
 *
 *  y  0..10   eight light boxes, 15 wide at x = 16*i: OH CD SD RD HS EC DC AA
 *  y 14..27   state word or tier name, 2x text, centred
 *  y 30..36   rate line, 1x text, centred (online) / carrier pulse (idle, y 33..35)
 *  y 39..43   tier ladder, 8 cells 14 wide at x = 16*i (training, online)
 *  y 46..52   board name (up to 16 chars) at x 0; LFD3K-SM when none given
 *  y 55..60   RSSI text x 0, RSSI bar x 44..61, SNR text x 66, SNR bar x 110..127
 */
#include <stdio.h>
#include <string.h>
#include "font5x7.h"
#include "panel_render.h"

#define W 128
#define H 64

static void px(uint8_t *fb, int x, int y, int on)
{
    if (x < 0 || x >= W || y < 0 || y >= H) {
        return;
    }
    uint8_t m = (uint8_t)(1u << (y & 7));
    if (on) {
        fb[(y >> 3) * W + x] |= m;
    } else {
        fb[(y >> 3) * W + x] &= (uint8_t)~m;
    }
}

static void fill(uint8_t *fb, int x, int y, int w, int h, int on)
{
    for (int j = 0; j < h; j++) {
        for (int i = 0; i < w; i++) {
            px(fb, x + i, y + j, on);
        }
    }
}

static void frame(uint8_t *fb, int x, int y, int w, int h)
{
    for (int i = 0; i < w; i++) {
        px(fb, x + i, y, 1);
        px(fb, x + i, y + h - 1, 1);
    }
    for (int j = 0; j < h; j++) {
        px(fb, x, y + j, 1);
        px(fb, x + w - 1, y + j, 1);
    }
}

static int text_w(const char *t, int sc)
{
    int n = (int)strlen(t);
    return n ? n * 6 * sc - sc : 0;
}

/* on = 1 draws lit pixels, on = 0 clears them (inverted text on a filled box). */
static void text(uint8_t *fb, int x, int y, const char *t, int sc, int on)
{
    for (; *t; t++, x += 6 * sc) {
        unsigned c = (unsigned char)*t;
        if (c < FONT_FIRST || c > FONT_LAST) {
            c = '?';
        }
        const uint8_t *g = &font5x7[(c - FONT_FIRST) * 5];
        for (int col = 0; col < 5; col++) {
            for (int row = 0; row < 7; row++) {
                if (g[col] & (1u << row)) {
                    fill(fb, x + col * sc, y + row * sc, sc, sc, on);
                }
            }
        }
    }
}

static void text_c(uint8_t *fb, int y, const char *t, int sc)
{
    text(fb, (W - text_w(t, sc)) / 2, y, t, sc, 1);
}

static const char *const LIGHT[8] = {"OH", "CD", "SD", "RD", "HS", "EC", "DC", "AA"};
static const uint16_t TIER_SF[8] = {12, 11, 10, 9, 8, 7, 6, 5};
static const uint32_t TIER_BPS[8] = {1172, 2148, 3906, 7031, 12500, 21875, 37500, 62500};

#define SNAP_MS 360 /* flash of the chosen tier after training ends */

static uint8_t s_last_state = 0;
static uint32_t s_train_end_ms = 0;
static int s_have_last = 0;

static int clampi(int v, int lo, int hi)
{
    return v < lo ? lo : (v > hi ? hi : v);
}

static void ladder(uint8_t *fb, int sel, int sweep, int snap_flash)
{
    for (int i = 0; i < 8; i++) {
        int x = i * 16;
        frame(fb, x, 39, 14, 5);
        int lit = (i == sel) || (i == sweep);
        if (i == sel && snap_flash) {
            lit = 0;
        }
        if (lit) {
            fill(fb, x, 39, 14, 5, 1);
        }
    }
}

static void bar(uint8_t *fb, int x, int y, int w, int h, int filled)
{
    frame(fb, x, y, w, h);
    fill(fb, x + 1, y + 1, clampi(filled, 0, w - 2), h - 2, 1);
}

void panel_render(const lfd3k_status_t *s, const char *name, uint32_t now_ms, uint8_t fb[PANEL_FB_BYTES])
{
    memset(fb, 0, PANEL_FB_BYTES);

    if (s_have_last && s_last_state == 3 && s->state != 3) {
        s_train_end_ms = now_ms;
    }
    s_last_state = s->state;
    s_have_last = 1;

    uint8_t lights = s->lights;
    if (s->state == 1) { /* dialing: OH blinks at 2.5 Hz */
        if ((now_ms / 200) & 1) {
            lights |= 1u;
        } else {
            lights &= (uint8_t)~1u;
        }
    }
    lights &= (uint8_t)~(1u << 6); /* DC stays off until compression exists */

    for (int i = 0; i < 8; i++) {
        int x = i * 16;
        if (lights & (1u << i)) {
            fill(fb, x, 0, 15, 11, 1);
            text(fb, x + 2, 2, LIGHT[i], 1, 0);
        } else {
            frame(fb, x, 0, 15, 11);
            text(fb, x + 2, 2, LIGHT[i], 1, 1);
        }
    }

    int tier = (s->tier >= 0 && s->tier <= 7) ? s->tier : -1;

    if (!s->radio_ok) {
        text_c(fb, 16, "RADIO FAIL", 2);
        text_c(fb, 33, "CHECK BOARD", 1);
        if ((now_ms / 500) & 1) {
            frame(fb, 6, 12, 116, 26);
        }
    } else {
        char line[24];
        int in_call = (s->state == 4 || s->state == 5) && tier >= 0;
        switch (s->state) {
        case 1:
            text_c(fb, 14, "DIALING", 2);
            {
                int dots = (int)((now_ms / 250) % 4);
                memcpy(line, "   ", 4);
                for (int i = 0; i < dots && i < 3; i++) line[i] = '.';
                text_c(fb, 33, line, 1);
            }
            break;
        case 2:
            if ((now_ms / 400) & 1) {
                fill(fb, 26, 12, 76, 18, 1);
                text(fb, (W - text_w("RING", 2)) / 2, 14, "RING", 2, 0);
            } else {
                text_c(fb, 14, "RING", 2);
            }
            break;
        case 3: {
            text_c(fb, 14, "TRAINING", 2);
            int p = (int)((now_ms / 90) % 14);
            ladder(fb, -1, p < 8 ? p : 14 - p, 0);
            break;
        }
        case 4:
        case 5:
            if (in_call) {
                snprintf(line, sizeof(line), "SF%u BW500", (unsigned)TIER_SF[tier]);
                text_c(fb, 14, line, 2);
                snprintf(line, sizeof(line), "%s %lu BPS", s->state == 5 ? "CMD" : "ONLINE",
                         (unsigned long)TIER_BPS[tier]);
                text_c(fb, 30, line, 1);
                int snap = s_train_end_ms != 0 && (uint32_t)(now_ms - s_train_end_ms) < SNAP_MS &&
                           (((now_ms - s_train_end_ms) / 60) & 1) == 0;
                ladder(fb, tier, -1, snap);
            } else {
                text_c(fb, 14, s->state == 5 ? "CMD" : "ONLINE", 2);
            }
            break;
        default: {
            text_c(fb, 14, "IDLE", 2);
            /* carrier pulse: a bar that breathes, 2 s triangle */
            int t = (int)(now_ms % 2000);
            int tri = t < 1000 ? t : 2000 - t;
            int w = 8 + tri * 100 / 1000;
            fill(fb, (W - w) / 2, 33, w, 3, 1);
            break;
        }
        }
    }

    /* bottom: board name, then RSSI and SNR readings each followed by a bar */
    char nm[17];
    size_t nl = 0;
    if (name) {
        for (; name[nl] && nl < 16; nl++) {
            nm[nl] = name[nl];
        }
    }
    if (nl == 0) {
        memcpy(nm, "LFD3K-SM", 8);
        nl = 8;
    }
    nm[nl] = 0;
    text(fb, 0, 46, nm, 1, 1);

    int heard = s->rssi != 0 || s->snr_x4 != 0;
    char rv[16], sv[16];
    int rs = 0, sn = 0;
    if (heard) {
        snprintf(rv, sizeof(rv), "%ddBm", (int)s->rssi);
        int t10 = ((int)s->snr_x4 * 25 + ((int)s->snr_x4 < 0 ? -5 : 5)) / 10; /* tenths of a dB, rounded */
        int a10 = t10 < 0 ? -t10 : t10;
        snprintf(sv, sizeof(sv), "%c%d.%ddB", t10 < 0 ? '-' : '+', a10 / 10, a10 % 10);
        rs = clampi(((int)s->rssi + 130) * 16 / 100, 0, 16);
        sn = clampi(((int)s->snr_x4 + 80) * 16 / 120, 0, 16);
    } else {
        snprintf(rv, sizeof(rv), "-");
        snprintf(sv, sizeof(sv), "-");
    }
    text(fb, 0, 55, rv, 1, 1);   /* up to 7 chars, ends by x 41 */
    bar(fb, 44, 55, 18, 6, rs);  /* RSSI -130..-30 dBm */
    text(fb, 66, 55, sv, 1, 1);  /* up to 7 chars, ends by x 107 */
    bar(fb, 110, 55, 18, 6, sn); /* SNR -20..+10 dB */
}

void panel_render_splash(uint8_t fb[PANEL_FB_BYTES])
{
    memset(fb, 0, PANEL_FB_BYTES);
    frame(fb, 0, 0, W, H);
    text_c(fb, 10, "LFD3K-SM", 2);
    text_c(fb, 32, "r.420", 1);
    text_c(fb, 46, "BL4Z3B4ND", 1);
}
