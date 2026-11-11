/* Front panel layout (128x64). Pure C, no ESP-IDF.
 *
 *  y  0..10   eight light boxes, 15 wide at x = 16*i: OH CD SD RD HS EC DC AA (DC lit while
 *             a radio call compresses, /ARQ/LZ)
 *  y 14..27   state word or tier name, 2x text, centred (Wi-Fi call: WIFI TCP, BRIDGE).
 *             While idle: the idle show (idle_show.c), y 12..29: the word changes every
 *             8 s loop into another language or a fact, with a transition each time.
 *             While an AT+SCAN runs: the radar (SCAN, a sweeping scope, one blip per
 *             answer, the count), y 12..37, in place of the idle show and the pulse
 *  y 30..36   rate line, 1x text, centred (online) / carrier pulse (idle, y 33..35)
 *  y 39..43   tier ladder, 8 cells 14 wide at x = 16*i (training, online)
 *  y 46..52   board name (up to 16 chars) at x 0; LFD3K-SM when none given. While idle it
 *             alternates every 3 s with Wi-Fi rows: AP <ssid>, PW <pass> (only while the
 *             core allows it), IP <a.b.c.d> (only with a client address); 21 chars at most
 *  y 55..60   RSSI text x 0, RSSI bar x 44..61, SNR text x 66, SNR bar x 110..127
 *             (last decoded frame; while idle with a channel trace: the live channel
 *             level and its rise above the noise floor)
 */
#include <stdio.h>
#include <string.h>
#include "font5x7.h"
#include "idle_show.h"
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

/* ---- Wi-Fi rows on the idle name line ---- */
#define NET_ROW_MS  3000u
#define NET_ROW_MAX 21 /* 21 cells of 6 px fit in 128 */

static panel_net_t s_net;
static int s_have_net;

void panel_render_set_net(const panel_net_t *n)
{
    if (n == NULL) {
        s_have_net = 0;
        memset(&s_net, 0, sizeof(s_net));
        return;
    }
    s_net = *n;
    /* never trust a missing NUL */
    s_net.ap_ssid[sizeof(s_net.ap_ssid) - 1] = 0;
    s_net.ap_pass[sizeof(s_net.ap_pass) - 1] = 0;
    s_net.ip[sizeof(s_net.ip) - 1] = 0;
    if (!s_net.show_pass) {
        memset(s_net.ap_pass, 0, sizeof(s_net.ap_pass)); /* the secret rule, again */
    }
    s_have_net = 1;
}

/* "<tag> <value>" cut to NET_ROW_MAX characters. */
static void net_row(char *out, size_t cap, const char *tag, const char *val)
{
    snprintf(out, cap, "%s %.*s", tag, (int)(NET_ROW_MAX - strlen(tag) - 1), val);
}

/* The text for the name row at now_ms: the board name, or the Wi-Fi row whose turn it is. */
static void name_row(char *out, size_t cap, const char *nm, uint32_t now_ms)
{
    char rows[4][NET_ROW_MAX + 1];
    int n = 0;
    snprintf(rows[n++], sizeof(rows[0]), "%s", nm);
    if (s_have_net && s_net.ap_on && s_net.ap_ssid[0]) {
        net_row(rows[n++], sizeof(rows[0]), "AP", s_net.ap_ssid);
        if (s_net.show_pass && s_net.ap_pass[0]) {
            net_row(rows[n++], sizeof(rows[0]), "PW", s_net.ap_pass);
        }
    }
    if (s_have_net && s_net.sta_up && s_net.ip[0]) {
        net_row(rows[n++], sizeof(rows[0]), "IP", s_net.ip);
    }
    snprintf(out, cap, "%s", rows[(now_ms / NET_ROW_MS) % (uint32_t)n]);
}

/* ---- Facts for the idle show ---- */
static panel_info_t s_info = PANEL_INFO_INIT;

void panel_render_set_info(const panel_info_t *info)
{
    if (info == NULL) {
        panel_info_t none = PANEL_INFO_INIT;
        s_info = none;
        return;
    }
    s_info = *info;
    s_info.last_caller[sizeof(s_info.last_caller) - 1] = 0;
    s_info.gps[sizeof(s_info.gps) - 1] = 0;
    if (s_info.hide_location) {
        memset(s_info.gps, 0, sizeof(s_info.gps)); /* never kept, never drawn */
    }
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

/* Hash for the lights shuffle. */
static uint32_t mix(uint32_t v)
{
    v ^= v >> 16;
    v *= 0x7FEB352Du;
    v ^= v >> 15;
    v *= 0x846CA68Bu;
    v ^= v >> 16;
    return v;
}

/* ---- Idle line: breathing scope of the real channel ----
 * The window still breathes (8..108 px wide, 2 s triangle, as before). Inside it the
 * latest RF trace is drawn as a waveform mirrored around y 34: height grows with the
 * signal above the noise floor (about 3 dB per pixel, up to 6 px), our own transmit
 * samples are full height, and the breath lifts the height by up to a third. With no
 * trace, or a quiet channel, it is the plain 3 px breathing bar. */
static rf_trace_t s_trace;
static int s_have_trace;

void panel_render_set_trace(const rf_trace_t *t)
{
    if (t == NULL || t->count == 0) {
        s_have_trace = 0;
        return;
    }
    s_trace = *t;
    if (s_trace.count > RF_TRACE_N) {
        s_trace.count = RF_TRACE_N;
    }
    s_have_trace = 1;
}

#define LINE_Y   34
#define LINE_AMP 6

/* Noise floor of the trace: the second lowest received sample (one dropout must not
 * set it). *newest_rx gets the newest received (not transmit) sample, or 0 if none. */
static int trace_floor(int *newest_rx)
{
    int lo1 = 0, lo2 = 0, nrx = 0, newest = 0;
    for (int i = 0; i < s_trace.count; i++) {
        if (s_trace.tx[i]) {
            continue;
        }
        int v = s_trace.dbm[i];
        newest = v;
        if (nrx == 0 || v < lo1) {
            lo2 = lo1;
            lo1 = v;
        } else if (nrx == 1 || v < lo2) {
            lo2 = v;
        }
        nrx++;
    }
    if (newest_rx) {
        *newest_rx = nrx ? newest : 0;
    }
    return nrx >= 2 ? lo2 : lo1;
}

static void idle_line(uint8_t *fb, uint32_t now_ms)
{
    int t = (int)(now_ms % 2000);
    int tri = t < 1000 ? t : 2000 - t; /* 0..1000 */
    int w = 8 + tri * 100 / 1000;
    int x0 = (W - w) / 2;

    if (!s_have_trace) {
        fill(fb, x0, LINE_Y - 1, w, 3, 1);
        return;
    }

    int floor_dbm = trace_floor(NULL);
    int lift = 1024 + tri * 341 / 1000; /* breath: x1.0 .. x1.33 */

    for (int c = 0; c < w; c++) {
        /* newest sample at the right edge of the window */
        int idx = s_trace.count - 1 - (w - 1 - c) * s_trace.count / (w > 1 ? w : 1);
        idx = clampi(idx, 0, s_trace.count - 1);
        int h;
        if (s_trace.tx[idx]) {
            h = LINE_AMP;
        } else {
            h = clampi((s_trace.dbm[idx] - floor_dbm) / 3, 0, LINE_AMP);
        }
        h = clampi(h * lift / 1024, 0, LINE_AMP);
        int x = x0 + c;
        /* the carrier: always a 1 px line, the signal grows it both ways */
        for (int y = LINE_Y - h; y <= LINE_Y + h; y++) {
            px(fb, x, y, 1);
        }
        if (h == 0 && (c % 2 == 0)) {
            px(fb, x, LINE_Y - 1, 1); /* a little texture on a quiet channel */
        }
    }
}

/* ---- Scan radar ----
 * While an AT+SCAN runs (status scanning = 1, the modem idle) the middle band shows SCAN
 * on the left, a round radar scope in the middle (centre 64,24, radius 11: y 13..35) with
 * a sweep line going round once every 1.6 s and a fading tail, one blip per answer so far
 * (each at a fixed place picked from its number, the newest blinking; the 12 newest are
 * drawn), and on the right the answer count with FOUND under it. Everything stays in
 * y 12..37. Pure function of now_ms and scan_hits; integer only. */
#define RADAR_CX    64
#define RADAR_CY    24
#define RADAR_R     11
#define RADAR_STEPS 32   /* angle steps per turn */
#define RADAR_STEP_MS 50 /* one turn in 1.6 s */
#define RADAR_BLIPS 12

/* 64 * sin(k * 2 pi / 32) for a quarter turn, k 0..8. */
static const int8_t QSIN[9] = {0, 12, 24, 36, 45, 53, 59, 63, 64};

static int sin32(int a)
{
    a &= RADAR_STEPS - 1;
    if (a < 8) return QSIN[a];
    if (a < 16) return QSIN[16 - a];
    if (a < 24) return -QSIN[a - 16];
    return -QSIN[32 - a];
}

static int cos32(int a)
{
    return sin32(a + 8);
}

/* Point at angle a, distance d from the centre (rounded to the nearest pixel). */
static void radar_pt(int a, int d, int *x, int *y)
{
    int dx = cos32(a) * d, dy = sin32(a) * d;
    *x = RADAR_CX + (dx >= 0 ? dx + 32 : dx - 32) / 64;
    *y = RADAR_CY + (dy >= 0 ? dy + 32 : dy - 32) / 64;
}

/* Midpoint circle; every `gap`th point only (1: solid). */
static void circle(uint8_t *fb, int cx, int cy, int r, int gap)
{
    int x = r, y = 0, err = 1 - r, k = 0;
    while (x >= y) {
        if (k++ % gap == 0) {
            px(fb, cx + x, cy + y, 1);
            px(fb, cx + y, cy + x, 1);
            px(fb, cx - y, cy + x, 1);
            px(fb, cx - x, cy + y, 1);
            px(fb, cx - x, cy - y, 1);
            px(fb, cx - y, cy - x, 1);
            px(fb, cx + y, cy - x, 1);
            px(fb, cx + x, cy - y, 1);
        }
        y++;
        if (err < 0) {
            err += 2 * y + 1;
        } else {
            x--;
            err += 2 * (y - x) + 1;
        }
    }
}

static void scan_radar(uint8_t *fb, uint8_t hits, uint32_t now_ms)
{
    text(fb, 2, 17, "SCAN", 2, 1);

    circle(fb, RADAR_CX, RADAR_CY, RADAR_R, 1);
    circle(fb, RADAR_CX, RADAR_CY, RADAR_R / 2, 2); /* dotted inner ring */
    px(fb, RADAR_CX, RADAR_CY, 1);

    int a = (int)((now_ms / RADAR_STEP_MS) % RADAR_STEPS);
    for (int back = 0; back < 3; back++) {
        int every = back + 1; /* the tail thins out behind the beam */
        for (int d = 1; d < RADAR_R; d++) {
            if (d % every == 0) {
                int x, y;
                radar_pt(a - back, d, &x, &y);
                px(fb, x, y, 1);
            }
        }
    }

    /* the newest RADAR_BLIPS answers, each at its own fixed place */
    int first = hits > RADAR_BLIPS ? hits - RADAR_BLIPS : 0;
    for (int i = first; i < hits; i++) {
        uint32_t h = mix((uint32_t)i * 0x9E3779B9u + 0xB11Fu);
        int x, y;
        radar_pt((int)(h % RADAR_STEPS), 3 + (int)((h >> 8) % 7), &x, &y); /* 3..9 from the centre */
        int solid = (i == hits - 1) && ((now_ms / 200) & 1);
        for (int j = -1; j <= 1; j++) {
            for (int k = -1; k <= 1; k++) {
                if (solid || j == 0 || k == 0) {
                    px(fb, x + j, y + k, 1);
                }
            }
        }
    }

    char n[8];
    snprintf(n, sizeof(n), "%u", (unsigned)hits);
    text(fb, 104 - text_w(n, 2) / 2, 14, n, 2, 1);
    text(fb, 104 - text_w("FOUND", 1) / 2, 31, "FOUND", 1, 1);
}

/* ---- Idle lights shuffle ----
 * While idle the eight light boxes slide into a new order once per 8 s loop, between
 * 1000 and 2200 ms of the loop; every fourth loop they sort back to OH CD SD RD HS EC
 * DC AA. Boxes moving right duck down up to 2 px on the way so they pass under the
 * ones moving left. Out of idle (a call, RADIO FAIL) they are at home at once. Each
 * light keeps showing its real state wherever it is. Pure function of now_ms. */
#define SHUF_LOOP_MS 8000u
#define SHUF_START   1000u
#define SHUF_LEN     1200u

static void light_order(uint32_t k, int slot[8])
{
    for (int i = 0; i < 8; i++) {
        slot[i] = i;
    }
    if (k % 4u == 0u) {
        return; /* home order */
    }
    uint32_t h = mix(k * 0x9E3779B9u + 0x5EEDu);
    for (int i = 7; i > 0; i--) {
        h = mix(h + (uint32_t)i);
        int j = (int)(h % (uint32_t)(i + 1));
        int t = slot[i];
        slot[i] = slot[j];
        slot[j] = t;
    }
}

static void light_layout(int idle, uint32_t now_ms, int lx[8], int ly[8], int moving[8])
{
    uint32_t k = now_ms / SHUF_LOOP_MS, t = now_ms % SHUF_LOOP_MS;
    int cur[8], prev[8];
    /* the lights hold still while the idle show runs its scroller */
    light_order(idle_show_light_slot(k), cur);
    light_order(k == 0u ? 0u : idle_show_light_slot(k - 1u), prev);
    for (int i = 0; i < 8; i++) {
        lx[i] = i * 16;
        ly[i] = 0;
        moving[i] = 0;
        if (!idle) {
            continue;
        }
        int from = prev[i], to = cur[i];
        if (t < SHUF_START) {
            lx[i] = from * 16; /* still in last loop's places */
        } else if (t >= SHUF_START + SHUF_LEN || from == to) {
            lx[i] = to * 16;
        } else {
            /* ease in and out: smoothstep in 1/1024 */
            int u = (int)((t - SHUF_START) * 1024u / SHUF_LEN);
            int e = u * u / 1024 * (3072 - 2 * u) / 1024;
            lx[i] = from * 16 + (to - from) * 16 * e / 1024;
            if (to > from) {
                ly[i] = 2 * 4 * e / 1024 * (1024 - e) / 1024; /* 0..2 px dip */
            }
            moving[i] = 1;
        }
    }
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

    int lx[8], ly[8], moving[8];
    light_layout(s->state == 0 && s->radio_ok, now_ms, lx, ly, moving);
    /* boxes that stay put first, the ones on the move on top */
    for (int pass = 0; pass < 2; pass++) {
        for (int i = 0; i < 8; i++) {
            if (moving[i] != pass) {
                continue;
            }
            int x = lx[i], y = ly[i];
            const char *lab = (i == 4 && s->simnet) ? "SI" : LIGHT[i]; /* SimNet test mode */
            if (lights & (1u << i)) {
                fill(fb, x, y, 15, 11, 1);
                text(fb, x + 2, y + 2, lab, 1, 0);
            } else {
                fill(fb, x, y, 15, 11, 0); /* a mover hides what it passes over */
                frame(fb, x, y, 15, 11);
                text(fb, x + 2, y + 2, lab, 1, 1);
            }
        }
    }

    int tier = (s->tier >= 0 && s->tier <= 7) ? s->tier : -1;

    if (!s->radio_ok && s->wire != 1 && s->wire != 2) { /* a pure Wi-Fi call needs no radio */
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
            if (s->wire == 2) { /* Wi-Fi TCP call: no tier, no ladder */
                text_c(fb, 14, "WIFI TCP", 2);
                text_c(fb, 30, s->state == 5 ? "CMD" : "ONLINE", 1);
            } else if (s->wire == 3) { /* radio call bridged to a forward */
                text_c(fb, 14, "BRIDGE", 2);
                if (tier >= 0) {
                    snprintf(line, sizeof(line), "SF%u BW500 %lu BPS", (unsigned)TIER_SF[tier],
                             (unsigned long)TIER_BPS[tier]);
                    text_c(fb, 30, line, 1);
                    ladder(fb, tier, -1, 0);
                } else {
                    text_c(fb, 30, s->state == 5 ? "CMD" : "ONLINE", 1);
                }
            } else if (in_call) {
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
            if (s->scanning) {
                scan_radar(fb, s->scan_hits, now_ms); /* AT+SCAN: sweep and blips */
            } else {
                idle_show_draw(fb, &s_info, now_ms); /* words, facts, transitions */
                idle_line(fb, now_ms); /* breathing scope of the channel */
            }
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
    if (s->state == 0 && s->radio_ok) {
        char row[NET_ROW_MAX + 17];
        name_row(row, sizeof(row), nm, now_ms);
        text(fb, 0, 46, row, 1, 1);
    } else {
        text(fb, 0, 46, nm, 1, 1);
    }

    int heard = s->rssi != 0 || s->snr_x4 != 0;
    char rv[16], sv[16];
    int rs = 0, sn = 0;
    int live = 0, live_dbm = 0;
    if (s->state == 0 && s->radio_ok && s_have_trace) {
        int floor_dbm = trace_floor(&live_dbm);
        live = live_dbm != 0;
        if (live) {
            /* idle: the channel right now. Left: its level. Right: how far the newest
             * reading stands above the noise floor (not a packet SNR). */
            int rise = live_dbm - floor_dbm;
            if (rise < 0) {
                rise = 0;
            }
            snprintf(rv, sizeof(rv), "%ddBm", live_dbm);
            snprintf(sv, sizeof(sv), "+%ddB", rise);
            rs = clampi((live_dbm + 130) * 16 / 100, 0, 16);
            sn = clampi(rise * 16 / 30, 0, 16);
        }
    }
    if (live) {
        /* filled above */
    } else if (heard) {
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
    bar(fb, 110, 55, 18, 6, sn); /* SNR -20..+10 dB; idle: rise above floor 0..30 dB */
}

void panel_render_splash(uint8_t fb[PANEL_FB_BYTES])
{
    memset(fb, 0, PANEL_FB_BYTES);
    frame(fb, 0, 0, W, H);
    text_c(fb, 10, "LFD3K-SM", 2);
    text_c(fb, 32, "r.420", 1);
    text_c(fb, 46, "BL4Z3B4ND", 1);
}
