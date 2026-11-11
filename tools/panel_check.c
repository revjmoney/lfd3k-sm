/* PC test for the pure C panel renderer (firmware/src/panel_render.c).
 *
 *  panel_check                 run the checks, exit 0 when all pass
 *  panel_check --dump <dir>    also write the mockup framebuffers as <name>.fb.txt
 *                              ("FB:" plus 2048 hex digits, the AT+FB format)
 *
 * Checks: every tier -1..7 x state 0..5 x radio_ok 0/1 with extreme RSSI and
 * SNR and a 16 character name, at several times, must leave the guard bytes
 * around the 1024 byte buffer untouched, draw something, and be repeatable.
 * Then the Wi-Fi rows and wire screens: wire 0..3 x state x tier x radio_ok with a 32
 * character ssid, a 63 character password and a 15 character ip (same guard checks),
 * the idle name row never reaching the last two columns, and the password never drawn
 * while show_pass is 0.
 * Then the idle show: every item into every item with every transition at many time
 * steps, and the scroller from every item, inside the guards and inside its rows
 * (y 12..29; the sine waves 12..31; the explode dots 12..44), each transition ending
 * exactly on the new item; the schedule never shows an item twice in a row and never
 * picks GPS while hide_location is 1; a long idle run through the whole panel.
 * Then the scan radar: with scanning set, every row outside y 12..37 equals the idle
 * frame at the same time, the band is drawn, the sweep moves every 50 ms, each answer
 * changes the band, and outside idle the flag changes nothing. Then the idle show fed
 * with real data: heard counts 0..65535 and caller names under the station name rule,
 * items shown exactly when set, inside the band, through every transition both ways.
 *
 *  With --dump it also writes <dir>/idle/items.txt (every item) and <dir>/idle/show.txt
 *  (a scripted preview of every transition and the scroller, one frame per 100 ms,
 *  "label TAB FB:hex" per line); tools/idle_preview.py makes the GIF and contact sheet.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <direct.h>
#else
#include <sys/stat.h>
#endif
#include "idle_show.h"
#include "panel_render.h"

#define GUARD 64
#define CANARY 0xA5

typedef struct {
    uint8_t pre[GUARD];
    uint8_t fb[PANEL_FB_BYTES];
    uint8_t post[GUARD];
} guarded_t;

static int failures = 0;
static long renders = 0;

static void fail(const char *what, int tier, int state, int ok, int rssi, int snr, unsigned long t)
{
    printf("FAIL %s: tier %d state %d radio_ok %d rssi %d snr_x4 %d now %lu\n", what, tier, state, ok, rssi,
           snr, t);
    failures++;
}

static void arm(guarded_t *g)
{
    memset(g->pre, CANARY, GUARD);
    memset(g->post, CANARY, GUARD);
    memset(g->fb, 0x5A, PANEL_FB_BYTES); /* the renderer must clear it itself */
}

static int guards_ok(const guarded_t *g)
{
    for (int i = 0; i < GUARD; i++) {
        if (g->pre[i] != CANARY || g->post[i] != CANARY) {
            return 0;
        }
    }
    return 1;
}

static int lit(const uint8_t *fb)
{
    int n = 0;
    for (int i = 0; i < PANEL_FB_BYTES; i++) {
        for (int b = 0; b < 8; b++) {
            n += (fb[i] >> b) & 1;
        }
    }
    return n;
}

static void check_one(const lfd3k_status_t *s, const char *name, uint32_t now)
{
    guarded_t g;
    arm(&g);
    panel_render(s, name, now, g.fb);
    renders++;
    if (!guards_ok(&g)) {
        fail("guard bytes overwritten", s->tier, s->state, s->radio_ok, s->rssi, s->snr_x4, now);
    }
    int n = lit(g.fb);
    if (n < 20 || n > PANEL_FB_BYTES * 8 - 20) {
        fail("blank or solid frame", s->tier, s->state, s->radio_ok, s->rssi, s->snr_x4, now);
    }
    /* Same input, same output (the animation memory only matters when the
     * state changes, which it does not between these two calls). */
    uint8_t again[PANEL_FB_BYTES];
    memset(again, 0x33, sizeof(again));
    panel_render(s, name, now, again);
    if (memcmp(g.fb, again, PANEL_FB_BYTES) != 0) {
        fail("not repeatable", s->tier, s->state, s->radio_ok, s->rssi, s->snr_x4, now);
    }
}

static void exhaustive(void)
{
    static const int16_t RSSI[] = {0, -1, -30, -130, -200, 32767, -32768};
    static const int16_t SNR[] = {0, 1, -80, 40, -32768, 32767};
    static const char *const NAMES[] = {"AXISMODEM-1234", "ABCDEFGHIJKLMNOP", "", NULL,
                                        "THIS NAME IS WAY TOO LONG FOR THE PANEL", "~!@#$%^&*()_+{}|"};
    static const uint32_t TIMES[] = {0, 199, 200, 1000, 1999, 123457, 4000000000u};
    uint32_t now = 0;
    for (int tier = -1; tier <= 7; tier++) {
        for (int state = 0; state <= 5; state++) {
            for (int ok = 0; ok <= 1; ok++) {
                for (size_t r = 0; r < sizeof(RSSI) / sizeof(RSSI[0]); r++) {
                    for (size_t q = 0; q < sizeof(SNR) / sizeof(SNR[0]); q++) {
                        lfd3k_status_t s;
                        memset(&s, 0, sizeof(s));
                        s.lights = (uint8_t)(0xFF - (tier + 1) * 7);
                        s.state = (uint8_t)state;
                        s.tier = (int8_t)tier;
                        s.rssi = RSSI[r];
                        s.snr_x4 = SNR[q];
                        s.radio_ok = (uint8_t)ok;
                        const char *nm = NAMES[(r + q + (size_t)(tier + 1)) % (sizeof(NAMES) / sizeof(NAMES[0]))];
                        now += 37; /* non-decreasing, as the contract says */
                        check_one(&s, nm, now);
                    }
                }
                /* Each time of the animation table, with the 16 character name. */
                for (size_t t = 0; t < sizeof(TIMES) / sizeof(TIMES[0]); t++) {
                    lfd3k_status_t s;
                    memset(&s, 0, sizeof(s));
                    s.state = (uint8_t)state;
                    s.tier = (int8_t)tier;
                    s.rssi = -75;
                    s.snr_x4 = 36;
                    s.radio_ok = (uint8_t)ok;
                    if (TIMES[t] >= now) {
                        now = TIMES[t];
                    }
                    check_one(&s, "ABCDEFGHIJKLMNOP", now);
                }
            }
        }
    }
    /* A scan in every state (the core only scans while idle, the renderer must not care). */
    static const uint8_t HITS[] = {0, 1, 12, 13, 255};
    for (int state = 0; state <= 5; state++) {
        for (size_t h = 0; h < sizeof(HITS) / sizeof(HITS[0]); h++) {
            for (int ok = 0; ok <= 1; ok++) {
                lfd3k_status_t s;
                memset(&s, 0, sizeof(s));
                s.state = (uint8_t)state;
                s.tier = (int8_t)(state == 4 ? 5 : -1);
                s.radio_ok = (uint8_t)ok;
                s.scanning = 1;
                s.scan_hits = HITS[h];
                s.heard = 65535;
                now += 211;
                check_one(&s, "ABCDEFGHIJKLMNOP", now);
            }
        }
    }
    /* Out of range state and tier values from a confused caller. */
    lfd3k_status_t s;
    memset(&s, 0, sizeof(s));
    s.state = 200;
    s.tier = 100;
    s.radio_ok = 1;
    check_one(&s, "X", now + 1);
    s.tier = -100;
    s.state = 4;
    check_one(&s, "X", now + 2);
}

static void fill_net(panel_net_t *n, int ssid_len, int pass_len, int show, const char *ip, int ap_on, int sta_up)
{
    memset(n, 0, sizeof(*n));
    for (int i = 0; i < ssid_len; i++) {
        n->ap_ssid[i] = (char)(i % 2 ? 'W' : 'M'); /* wide letters */
    }
    for (int i = 0; i < pass_len; i++) {
        n->ap_pass[i] = (char)(i % 2 ? 'W' : 'M');
    }
    n->show_pass = (uint8_t)show;
    strncpy(n->ip, ip, sizeof(n->ip) - 1);
    n->ap_on = (uint8_t)ap_on;
    n->sta_up = (uint8_t)sta_up;
}

static void wifi_checks(void)
{
    static const char *const IPS[] = {"", "1.2.3.4", "255.255.255.255"};
    uint32_t now = 1000;
    /* wire screens and every net row, bounds with the longest strings */
    for (int cfg = 0; cfg < 6; cfg++) {
        panel_net_t n;
        switch (cfg) {
        case 0: fill_net(&n, 32, 63, 1, IPS[2], 1, 1); break;
        case 1: fill_net(&n, 32, 63, 0, IPS[2], 1, 1); break;
        case 2: fill_net(&n, 1, 8, 1, IPS[1], 1, 0); break;
        case 3: fill_net(&n, 0, 0, 0, IPS[0], 0, 0); break;
        case 4: fill_net(&n, 32, 63, 1, IPS[2], 0, 1); break;
        default: fill_net(&n, 20, 12, 1, IPS[1], 1, 1); break;
        }
        panel_render_set_net(&n);
        for (int wire = 0; wire <= 3; wire++) {
            for (int state = 0; state <= 5; state++) {
                for (int tier = -1; tier <= 7; tier++) {
                    for (int ok = 0; ok <= 1; ok++) {
                        lfd3k_status_t s;
                        memset(&s, 0, sizeof(s));
                        s.lights = 0xB7;
                        s.state = (uint8_t)state;
                        s.tier = (int8_t)tier;
                        s.rssi = -75;
                        s.snr_x4 = 36;
                        s.radio_ok = (uint8_t)ok;
                        s.wire = (uint8_t)wire;
                        s.simnet = (uint8_t)(wire & 1);
                        now += 1013;
                        check_one(&s, "AXISMODEM-7F3A", now);
                    }
                }
            }
        }
    }
    /* idle name row: nothing in the last two columns (x 126, 127), y 46..52, for a full
     * rotation of every row, and the password absent when show_pass is 0 */
    panel_net_t full, quiet;
    fill_net(&full, 32, 63, 1, IPS[2], 1, 1);
    fill_net(&quiet, 32, 63, 0, IPS[2], 1, 1);
    panel_net_t quiet_empty = quiet;
    memset(quiet_empty.ap_pass, 0, sizeof(quiet_empty.ap_pass));
    lfd3k_status_t s;
    memset(&s, 0, sizeof(s));
    s.radio_ok = 1;
    s.tier = -1;
    for (uint32_t t = 40000; t < 40000 + 14000; t += 250) {
        uint8_t a[PANEL_FB_BYTES], b[PANEL_FB_BYTES];
        panel_render_set_net(&full);
        panel_render(&s, "AXISMODEM-7F3A", t, a);
        for (int y = 46; y <= 52; y++) {
            for (int x = 126; x < 128; x++) {
                if (a[(y >> 3) * 128 + x] & (1u << (y & 7))) {
                    printf("FAIL name row overflows at t %lu\n", (unsigned long)t);
                    failures++;
                    y = 99;
                    break;
                }
            }
        }
        panel_render_set_net(&quiet);
        panel_render(&s, "AXISMODEM-7F3A", t, a);
        panel_render_set_net(&quiet_empty);
        panel_render(&s, "AXISMODEM-7F3A", t, b);
        renders += 3;
        if (memcmp(a, b, PANEL_FB_BYTES) != 0) {
            printf("FAIL password drawn while show_pass is 0 (t %lu)\n", (unsigned long)t);
            failures++;
        }
    }
    /* the four rows really differ over a rotation (name, AP, PW, IP) */
    {
        uint8_t row[4][PANEL_FB_BYTES];
        panel_net_t n;
        fill_net(&n, 5, 12, 1, "10.0.0.2", 1, 1);
        panel_render_set_net(&n);
        for (int i = 0; i < 4; i++) {
            panel_render(&s, "AXISMODEM-7F3A", 48000u + (uint32_t)i * 3000u, row[i]);
        }
        for (int i = 0; i < 4; i++) {
            for (int j = i + 1; j < 4; j++) {
                if (memcmp(&row[i][46 / 8 * 128], &row[j][46 / 8 * 128], 128) == 0) {
                    printf("FAIL name row %d equals row %d\n", i, j);
                    failures++;
                }
            }
        }
    }
    panel_render_set_net(NULL);
}

/* ---- idle show ---- */

static panel_info_t full_info(int hide)
{
    panel_info_t in = PANEL_INFO_INIT;
    in.time_valid = 1;
    in.hour = 23;
    in.minute = 59;
    in.uptime_s = 3u * 86400u + 4u * 3600u + 5u;
    in.temp_valid = 1;
    in.temp_c = -10;
    in.heard = 65535;
    strcpy(in.last_caller, "MMMMMMMMMMMMMMMM"); /* 16 wide letters */
    strcpy(in.gps, "00.0000N 000.0000E FAKE");
    in.hide_location = (uint8_t)hide;
    return in;
}

/* Lit pixels outside rows y0..y1 of the screen? */
static int outside_rows(const uint8_t *fb, int y0, int y1)
{
    for (int y = 0; y < 64; y++) {
        if (y >= y0 && y <= y1) {
            continue;
        }
        for (int x = 0; x < 128; x++) {
            if (fb[(y >> 3) * 128 + x] & (1u << (y & 7))) {
                return 1;
            }
        }
    }
    return 0;
}

static void idle_fail(const char *what, int a, int b, int tr, unsigned long t)
{
    printf("FAIL idle show %s: %s -> %s by %s at %lu\n", what, idle_show_item_name(a), idle_show_item_name(b),
           idle_show_trans_name(tr), t);
    failures++;
}

static void idle_checks(void)
{
    panel_info_t in = full_info(0);
    guarded_t g;
    /* every item alone: inside the band and something drawn */
    for (int a = 0; a < IS_ITEMS; a++) {
        arm(&g);
        memset(g.fb, 0, PANEL_FB_BYTES);
        idle_show_draw_item(g.fb, &in, a);
        renders++;
        if (!guards_ok(&g) || outside_rows(g.fb, 12, 29) || lit(g.fb) < 20) {
            idle_fail("item", a, a, 0, 0);
        }
    }
    /* every pair, every transition, many steps */
    for (int a = 0; a <= IS_ITEMS; a++) { /* IS_ITEMS: the empty band after the scroller */
        for (int b = 0; b < IS_ITEMS; b++) {
            for (int tr = 0; tr < TR_COUNT; tr++) {
                int y1 = tr == TR_EXPLODE ? 44 : (tr == TR_SINE ? 31 : 29);
                for (uint32_t t = 0; t <= IDLE_TR_MS + 40; t += 41) {
                    arm(&g);
                    memset(g.fb, 0, PANEL_FB_BYTES);
                    idle_show_draw_trans(g.fb, &in, a, b, tr, t);
                    renders++;
                    if (!guards_ok(&g)) {
                        idle_fail("guard bytes overwritten", a, b, tr, t);
                    }
                    if (outside_rows(g.fb, 12, y1)) {
                        idle_fail("drew outside its rows", a, b, tr, t);
                    }
                }
                /* the last frame is exactly the new item */
                uint8_t end[PANEL_FB_BYTES], want[PANEL_FB_BYTES];
                memset(end, 0, sizeof(end));
                memset(want, 0, sizeof(want));
                idle_show_draw_trans(end, &in, a, b, tr, IDLE_TR_MS - 1);
                idle_show_draw_item(want, &in, b);
                if (memcmp(end, want, sizeof(end)) != 0) {
                    idle_fail("does not land on the new item", a, b, tr, IDLE_TR_MS - 1);
                }
            }
        }
        for (uint32_t u = 0; u <= idle_show_scroll_ms() + 100; u += 53) {
            arm(&g);
            memset(g.fb, 0, PANEL_FB_BYTES);
            idle_show_draw_scroller(g.fb, &in, a, u);
            renders++;
            if (!guards_ok(&g) || outside_rows(g.fb, 12, 31)) {
                idle_fail("scroller", a, a, 0, u);
            }
        }
    }
    /* the schedule: no repeats, GPS only when allowed, every item and transition used */
    panel_info_t hid = full_info(1);
    long scrolls = 0, words = 0, gps = 0, seen[IS_ITEMS] = {0}, trs[TR_COUNT] = {0};
    int last = -1;
    for (uint32_t n = 0; n < 200000u; n++) {
        idle_slot_t s;
        idle_show_slot(&hid, n, &s);
        if (s.role == SLOT_NORMAL || s.role == SLOT_PRE_SCROLL) {
            words++;
            if (s.item == last) {
                printf("FAIL idle show repeats %s at slot %lu\n", idle_show_item_name(s.item), (unsigned long)n);
                failures++;
            }
            last = s.item;
            if (s.item >= 0 && s.item < IS_ITEMS) {
                seen[s.item]++;
            }
            if (s.item == IS_GPS) {
                gps++;
            }
        } else {
            if (s.role == SLOT_SCROLL && s.scroll_at == n) {
                scrolls++;
            }
            last = -1;
        }
        if (s.role == SLOT_NORMAL || s.role == SLOT_SCROLL_TAIL) {
            trs[s.trans]++;
            if (s.role == SLOT_NORMAL && s.next == s.item) {
                printf("FAIL idle show next equals item at slot %lu\n", (unsigned long)n);
                failures++;
            }
        }
    }
    if (gps) {
        printf("FAIL idle show picked GPS %ld times while hidden\n", gps);
        failures++;
    }
    for (int i = 0; i < IS_GPS; i++) {
        if (seen[i] == 0) {
            printf("FAIL idle show never shows %s\n", idle_show_item_name(i));
            failures++;
        }
    }
    for (int i = 0; i < TR_COUNT; i++) {
        if (trs[i] == 0) {
            printf("FAIL idle show never uses %s\n", idle_show_trans_name(i));
            failures++;
        }
    }
    printf("idle show: %ld loops with a word, %ld scrollers (1 in %ld loops), IDLE %ld, clock %ld\n", words,
           scrolls, scrolls ? (words + scrolls) / scrolls : 0, seen[IS_IDLE], seen[IS_CLOCK]);
    /* a long idle run through the whole panel with every info item set */
    panel_render_set_info(&in);
    lfd3k_status_t st;
    memset(&st, 0, sizeof(st));
    st.radio_ok = 1;
    st.tier = -1;
    st.lights = 0x80;
    for (uint32_t t = 5000000u; t < 5000000u + 400000u; t += 50) {
        check_one(&st, "AXISMODEM-7F3A", t);
    }
    panel_render_set_info(NULL);
}

/* ---- scan radar ---- */

#define SCAN_Y0 12
#define SCAN_Y1 37

/* Do rows y0..y1 of a and b differ? */
static int rows_differ(const uint8_t *a, const uint8_t *b, int y0, int y1)
{
    for (int y = y0; y <= y1; y++) {
        for (int x = 0; x < 128; x++) {
            uint8_t m = (uint8_t)(1u << (y & 7));
            if ((a[(y >> 3) * 128 + x] & m) != (b[(y >> 3) * 128 + x] & m)) {
                return 1;
            }
        }
    }
    return 0;
}

static void scan_fail(const char *what, int hits, unsigned long t)
{
    printf("FAIL scan radar %s: hits %d at %lu\n", what, hits, t);
    failures++;
}

static void scan_checks(void)
{
    static const int HITS[] = {0, 1, 2, 3, 7, 11, 12, 13, 100, 254, 255};
    /* the idle frame to compare against holds IDLE (no transition, so nothing below
     * y 37 from an EXPLODE); the lights shuffle and name row still run */
    static const idle_slot_t only_idle = {SLOT_PRE_SCROLL, IS_IDLE, IS_IDLE, TR_EXPLODE, 0};
    idle_show_set_script(&only_idle, 1);
    lfd3k_status_t s;
    memset(&s, 0, sizeof(s));
    s.radio_ok = 1;
    s.tier = -1;
    s.lights = 0x80;
    for (size_t h = 0; h < sizeof(HITS) / sizeof(HITS[0]); h++) {
        for (uint32_t t = 900000u; t < 900000u + 4000u; t += 37) {
            uint8_t idle[PANEL_FB_BYTES], scan[PANEL_FB_BYTES];
            s.scanning = 0;
            s.scan_hits = 0;
            panel_render(&s, "AXISMODEM-7F3A", t, idle);
            s.scanning = 1;
            s.scan_hits = (uint8_t)HITS[h];
            check_one(&s, "AXISMODEM-7F3A", t); /* guards, something drawn, repeatable */
            panel_render(&s, "AXISMODEM-7F3A", t, scan);
            renders += 2;
            /* the radar only replaces the middle band: every other row is the idle frame's */
            if (rows_differ(idle, scan, 0, SCAN_Y0 - 1) || rows_differ(idle, scan, SCAN_Y1 + 1, 63)) {
                scan_fail("drew outside y 12..37", HITS[h], t);
            }
            if (!rows_differ(idle, scan, SCAN_Y0, SCAN_Y1)) {
                scan_fail("band same as idle", HITS[h], t);
            }
        }
    }
    /* the sweep moves (one angle step every 50 ms), and each answer adds a blip */
    s.scanning = 1;
    for (uint32_t t = 910000u; t < 910000u + 1600u; t += 50) {
        uint8_t a[PANEL_FB_BYTES], b[PANEL_FB_BYTES];
        s.scan_hits = 0;
        panel_render(&s, "AXISMODEM-7F3A", t, a);
        panel_render(&s, "AXISMODEM-7F3A", t + 50, b);
        if (!rows_differ(a, b, SCAN_Y0, SCAN_Y1)) {
            scan_fail("sweep did not move", 0, t);
        }
        for (int h = 1; h <= 12; h++) {
            s.scan_hits = (uint8_t)(h - 1);
            panel_render(&s, "AXISMODEM-7F3A", t, a);
            s.scan_hits = (uint8_t)h;
            panel_render(&s, "AXISMODEM-7F3A", t, b);
            renders += 2;
            if (!rows_differ(a, b, SCAN_Y0, SCAN_Y1)) {
                scan_fail("answer added no blip", h, t);
            }
        }
        renders += 2;
    }
    /* outside idle the scan flag changes nothing */
    for (int state = 1; state <= 5; state++) {
        uint8_t a[PANEL_FB_BYTES], b[PANEL_FB_BYTES];
        s.state = (uint8_t)state;
        s.tier = 5;
        s.scanning = 0;
        panel_render(&s, "AXISMODEM-7F3A", 920000u, a);
        s.scanning = 1;
        s.scan_hits = 9;
        panel_render(&s, "AXISMODEM-7F3A", 920000u, b);
        renders += 2;
        if (memcmp(a, b, PANEL_FB_BYTES) != 0) {
            scan_fail("drawn outside idle", 9, (unsigned long)state);
        }
    }
    idle_show_set_script(NULL, 0);
}

/* ---- the idle show fed with real data: heard count and last caller ---- */

static void real_info_checks(void)
{
    /* heard counts the core can give (0 to 32 directory entries; the field is wider) and
     * caller names under the station name rule (1 to 16 printable characters, no colon) */
    static const uint16_t HEARD[] = {0, 1, 2, 9, 10, 31, 32, 99, 65535};
    static const char *const CALLERS[] = {"", "A", "PCMODEM", "AXISMODEM-4F1C", "ABCDEFGHIJKLMNOP",
                                          "REV J MONEY", "~!@#$%^&*()_+{}|", "WWWWWWWWWWWWWWWW"};
    guarded_t g;
    for (size_t h = 0; h < sizeof(HEARD) / sizeof(HEARD[0]); h++) {
        for (size_t c = 0; c < sizeof(CALLERS) / sizeof(CALLERS[0]); c++) {
            panel_info_t in = PANEL_INFO_INIT;
            in.heard = HEARD[h];
            strcpy(in.last_caller, CALLERS[c]);
            if (idle_show_available(&in, IS_HEARD) != (HEARD[h] > 0)) {
                printf("FAIL HEARD item availability, heard %u\n", (unsigned)HEARD[h]);
                failures++;
            }
            if (idle_show_available(&in, IS_CALLER) != (CALLERS[c][0] != 0)) {
                printf("FAIL caller item availability, caller \"%s\"\n", CALLERS[c]);
                failures++;
            }
            for (int item = IS_HEARD; item <= IS_CALLER; item++) {
                if (!idle_show_available(&in, item)) {
                    continue;
                }
                arm(&g);
                memset(g.fb, 0, PANEL_FB_BYTES);
                idle_show_draw_item(g.fb, &in, item);
                renders++;
                if (!guards_ok(&g) || outside_rows(g.fb, 12, 29) || lit(g.fb) < 20) {
                    printf("FAIL %s item, heard %u caller \"%s\"\n", idle_show_item_name(item),
                           (unsigned)HEARD[h], CALLERS[c]);
                    failures++;
                }
                /* every transition into and out of it stays in its rows and lands */
                for (int tr = 0; tr < TR_COUNT; tr++) {
                    int y1 = tr == TR_EXPLODE ? 44 : (tr == TR_SINE ? 31 : 29);
                    for (int dir = 0; dir < 2; dir++) {
                        int a = dir ? item : IS_IDLE, b = dir ? IS_IDLE : item;
                        for (uint32_t t = 0; t < IDLE_TR_MS; t += 97) {
                            arm(&g);
                            memset(g.fb, 0, PANEL_FB_BYTES);
                            idle_show_draw_trans(g.fb, &in, a, b, tr, t);
                            renders++;
                            if (!guards_ok(&g) || outside_rows(g.fb, 12, y1)) {
                                idle_fail("real data transition", a, b, tr, t);
                            }
                        }
                    }
                }
            }
        }
    }
    /* the schedule shows them when set and never when not */
    panel_info_t on = PANEL_INFO_INIT, off = PANEL_INFO_INIT;
    on.heard = 3;
    strcpy(on.last_caller, "AXISMODEM-4F1C");
    long seen_on[IS_ITEMS] = {0}, seen_off[IS_ITEMS] = {0};
    for (uint32_t n = 0; n < 50000u; n++) {
        idle_slot_t a, b;
        idle_show_slot(&on, n, &a);
        idle_show_slot(&off, n, &b);
        if (a.item >= 0 && a.item < IS_ITEMS) seen_on[a.item]++;
        if (a.next >= 0 && a.next < IS_ITEMS) seen_on[a.next]++;
        if (b.item >= 0 && b.item < IS_ITEMS) seen_off[b.item]++;
        if (b.next >= 0 && b.next < IS_ITEMS) seen_off[b.next]++;
    }
    if (!seen_on[IS_HEARD] || !seen_on[IS_CALLER]) {
        printf("FAIL idle show never picks HEARD or the caller when set\n");
        failures++;
    }
    if (seen_off[IS_HEARD] || seen_off[IS_CALLER]) {
        printf("FAIL idle show picks HEARD or the caller when not set\n");
        failures++;
    }
}

static void hexline(FILE *f, const char *label, const uint8_t *fb)
{
    fprintf(f, "%s\tFB:", label);
    for (int i = 0; i < PANEL_FB_BYTES; i++) {
        fprintf(f, "%02X", fb[i]);
    }
    fputs("\n", f);
}

/* The preview script: twelve loops, one per transition, then the scroller. */
static void idle_dump(const char *dir)
{
    static const int SEQ[] = {IS_IDLE, IS_ZH, IS_ES, IS_CLOCK, IS_HI, IS_RU, IS_TEMP, IS_AR, IS_SV, IS_KO,
                              IS_UPTIME, IS_EL, IS_TH, IS_HE, IS_PL, IS_JA};
    enum { NSLOT = 17 };
    static idle_slot_t sc[NSLOT];
    for (int i = 0; i < 12; i++) {
        sc[i] = (idle_slot_t){SLOT_NORMAL, SEQ[i], SEQ[i + 1], i, 0};
    }
    sc[12] = (idle_slot_t){SLOT_PRE_SCROLL, SEQ[12], IS_BLANK, 0, 13};
    sc[13] = (idle_slot_t){SLOT_SCROLL, IS_BLANK, IS_BLANK, 0, 13};
    sc[14] = (idle_slot_t){SLOT_SCROLL, IS_BLANK, IS_BLANK, 0, 13};
    sc[15] = (idle_slot_t){SLOT_SCROLL_TAIL, IS_BLANK, SEQ[13], TR_RAIN, 13};
    sc[16] = (idle_slot_t){SLOT_NORMAL, SEQ[13], SEQ[14], TR_GLITCH, 0};

    char path[512];
    snprintf(path, sizeof(path), "%s/idle", dir);
#ifdef _WIN32
    _mkdir(path);
#else
    mkdir(path, 0755);
#endif
    panel_info_t in = full_info(1);
    in.hour = 14;
    in.minute = 5;
    in.temp_c = 38;
    in.heard = 3;
    in.last_caller[0] = 0;
    snprintf(path, sizeof(path), "%s/idle/items.txt", dir);
    FILE *f = fopen(path, "wb");
    if (!f) {
        printf("FAIL cannot write %s\n", path);
        failures++;
        return;
    }
    panel_info_t all = in;
    strcpy(all.last_caller, "AXISMODEM-7F3A");
    strcpy(all.gps, "00.0000N 000.0000E"); /* fake, nowhere: preview only */
    all.hide_location = 0;
    for (int a = 0; a < IS_ITEMS; a++) {
        uint8_t fb[PANEL_FB_BYTES];
        memset(fb, 0, sizeof(fb));
        idle_show_draw_item(fb, &all, a);
        hexline(f, idle_show_item_name(a), fb);
    }
    fclose(f);

    snprintf(path, sizeof(path), "%s/idle/show.txt", dir);
    f = fopen(path, "wb");
    if (!f) {
        printf("FAIL cannot write %s\n", path);
        failures++;
        return;
    }
    idle_show_set_script(sc, NSLOT);
    panel_render_set_info(&in);
    panel_net_t n;
    memset(&n, 0, sizeof(n));
    strcpy(n.ap_ssid, "AXISMODEM-7F3A");
    strcpy(n.ip, "192.168.1.57");
    n.ap_on = 1;
    n.sta_up = 1;
    panel_render_set_net(&n);
    /* a noisy live channel with a burst of our own transmit, so the scope is busy */
    rf_trace_t tr;
    memset(&tr, 0, sizeof(tr));
    for (int i = 0; i < RF_TRACE_N; i++) {
        tr.dbm[i] = (int8_t)(-112 + (i * 37 % 11) + ((i % 13 == 5) ? 18 : 0));
        tr.tx[i] = (uint8_t)(i >= 40 && i < 46);
    }
    tr.count = RF_TRACE_N;
    lfd3k_status_t st;
    memset(&st, 0, sizeof(st));
    st.radio_ok = 1;
    st.tier = -1;
    st.lights = 0x80;
    for (int slot = 0; slot < NSLOT; slot++) {
        panel_render_set_trace(slot % 2 ? &tr : NULL); /* odd loops with a busy scope */
        uint32_t t0 = 3000, t1 = 7400;
        if (slot == 0) {
            t0 = 0;
        } else if (slot == 12) {
            t0 = 6000;
            t1 = 8000;
        } else if (slot >= 13 && slot <= 15) {
            t0 = 0;
            t1 = 8000;
        } else if (slot == 16) {
            t0 = 0;
            t1 = 1500;
        }
        for (uint32_t t = t0; t < t1; t += 100) {
            uint8_t fb[PANEL_FB_BYTES];
            panel_render(&st, "AXISMODEM-7F3A", (uint32_t)slot * IDLE_SLOT_MS + t, fb);
            char label[64];
            const idle_slot_t *s = &sc[slot];
            int in_tr = (s->role == SLOT_NORMAL || s->role == SLOT_SCROLL_TAIL) && t >= IDLE_TR_AT &&
                        t < IDLE_TR_AT + IDLE_TR_MS;
            if (s->role == SLOT_SCROLL || (s->role == SLOT_SCROLL_TAIL && t < IDLE_TR_AT)) {
                snprintf(label, sizeof(label), "SCROLLER");
            } else if (in_tr) {
                snprintf(label, sizeof(label), "%s", idle_show_trans_name(s->trans));
            } else {
                int item = (s->role != SLOT_PRE_SCROLL && t >= IDLE_TR_AT) ? s->next : s->item;
                snprintf(label, sizeof(label), "%s", idle_show_item_name(item));
            }
            hexline(f, label, fb);
        }
    }
    fclose(f);
    idle_show_set_script(NULL, 0);
    panel_render_set_info(NULL);
    panel_render_set_net(NULL);
    panel_render_set_trace(NULL);
}

static void splash_check(void)
{
    guarded_t g;
    arm(&g);
    panel_render_splash(g.fb);
    if (!guards_ok(&g) || lit(g.fb) < 100) {
        printf("FAIL splash\n");
        failures++;
    }
}

static void dump(const char *dir, const char *name, const uint8_t *fb)
{
    char path[512];
    snprintf(path, sizeof(path), "%s/%s.fb.txt", dir, name);
    FILE *f = fopen(path, "wb");
    if (!f) {
        printf("FAIL cannot write %s\n", path);
        failures++;
        return;
    }
    fputs("FB:", f);
    for (int i = 0; i < PANEL_FB_BYTES; i++) {
        fprintf(f, "%02X", fb[i]);
    }
    fputs("\n", f);
    fclose(f);
}

static void mockups(const char *dir)
{
    uint8_t fb[PANEL_FB_BYTES];
    lfd3k_status_t s;
    /* the idle mockups show the plain IDLE word (every time used here is before the
     * transition of its loop) */
    static const idle_slot_t only_idle = {SLOT_NORMAL, IS_IDLE, IS_ES, TR_EXPLODE, 0};
    idle_show_set_script(&only_idle, 1);

    memset(&s, 0, sizeof(s));
    s.lights = 0x80; /* AA */
    s.tier = -1;
    s.radio_ok = 1;
    s.rssi = -92;
    s.snr_x4 = 22;
    panel_render(&s, "AXISMODEM-7F3A", 10000, fb);
    dump(dir, "idle", fb);

    memset(&s, 0, sizeof(s));
    s.lights = 0x01; /* OH */
    s.state = 1;
    s.tier = -1;
    s.radio_ok = 1;
    s.rssi = -92;
    s.snr_x4 = 22;
    panel_render(&s, "AXISMODEM-7F3A", 10750, fb); /* OH on, three dots */
    dump(dir, "dialing", fb);

    memset(&s, 0, sizeof(s));
    s.lights = 0x01;
    s.state = 3;
    s.tier = -1;
    s.radio_ok = 1;
    s.rssi = -71;
    s.snr_x4 = 40;
    panel_render(&s, "AXISMODEM-7F3A", 12270, fb); /* sweep in the middle of the ladder */
    dump(dir, "training", fb);

    memset(&s, 0, sizeof(s));
    s.lights = 0xB7; /* OH CD SD HS EC AA */
    s.state = 4;
    s.tier = 5;
    s.radio_ok = 1;
    s.rssi = -88;
    s.snr_x4 = 28;
    panel_render(&s, "AXISMODEM-7F3A", 20000, fb);
    dump(dir, "online_sf7", fb);

    s.tier = 7;
    s.rssi = -61;
    s.snr_x4 = 48;
    panel_render(&s, "AXISMODEM-7F3A", 21000, fb);
    dump(dir, "online_sf5", fb);

    memset(&s, 0, sizeof(s));
    s.tier = -1;
    s.radio_ok = 0;
    panel_render(&s, "AXISMODEM-7F3A", 22000, fb); /* frame blink on */
    dump(dir, "radio_fail", fb);

    /* Wi-Fi rows. FAKE password: never put a real one here. Times pick the row:
     * the rotation is (now / 3000) % rows, and the idle word is solid at these times. */
    panel_net_t n;
    memset(&s, 0, sizeof(s));
    s.lights = 0x80;
    s.tier = -1;
    s.radio_ok = 1;
    s.rssi = -92;
    s.snr_x4 = 22;
    memset(&n, 0, sizeof(n));
    strcpy(n.ap_ssid, "AXISMODEM-7F3A");
    strcpy(n.ip, "192.168.1.57");
    n.ap_on = 1;
    n.sta_up = 1;
    panel_render_set_net(&n); /* rows: name, AP, IP */
    panel_render(&s, "AXISMODEM-7F3A", 3500, fb);
    dump(dir, "idle_ap", fb);
    panel_render(&s, "AXISMODEM-7F3A", 8500, fb);
    dump(dir, "idle_ip", fb);
    strcpy(n.ap_pass, "EXAMPLEPASS1");
    n.show_pass = 1;
    panel_render_set_net(&n); /* rows: name, AP, PW, IP */
    panel_render(&s, "AXISMODEM-7F3A", 8500, fb);
    dump(dir, "idle_pw", fb);
    panel_render_set_net(NULL);

    memset(&s, 0, sizeof(s));
    s.lights = 0x93; /* OH CD HS AA */
    s.state = 4;
    s.tier = -1;
    s.radio_ok = 1;
    s.wire = 2;
    panel_render(&s, "AXISMODEM-7F3A", 31000, fb);
    dump(dir, "wifi_tcp", fb);

    memset(&s, 0, sizeof(s));
    s.lights = 0xB7;
    s.state = 4;
    s.tier = 5;
    s.radio_ok = 1;
    s.wire = 3;
    s.rssi = -88;
    s.snr_x4 = 28;
    panel_render(&s, "AXISMODEM-7F3A", 32000, fb);
    dump(dir, "bridge", fb);

    /* a compressing radio call: DC lit (OH CD SD HS EC DC AA) */
    s.wire = 0;
    s.lights = 0xF7;
    panel_render(&s, "AXISMODEM-7F3A", 33000, fb);
    dump(dir, "online_lz", fb);

    /* AT+SCAN with three answers so far (the newest blip solid at this time) */
    memset(&s, 0, sizeof(s));
    s.lights = 0x80;
    s.tier = -1;
    s.radio_ok = 1;
    s.scanning = 1;
    s.scan_hits = 3;
    panel_render(&s, "AXISMODEM-7F3A", 34250, fb);
    dump(dir, "scan", fb);

    /* the heard count and the last caller in the idle show, with a renamed station */
    panel_info_t in = PANEL_INFO_INIT;
    in.heard = 4;
    strcpy(in.last_caller, "PCMODEM");
    panel_render_set_info(&in);
    static const idle_slot_t heard = {SLOT_NORMAL, IS_HEARD, IS_IDLE, TR_EXPLODE, 0};
    static const idle_slot_t caller = {SLOT_NORMAL, IS_CALLER, IS_IDLE, TR_EXPLODE, 0};
    s.scanning = 0;
    s.scan_hits = 0;
    idle_show_set_script(&heard, 1);
    panel_render(&s, "REVJ", 40100, fb);
    dump(dir, "idle_heard", fb);
    idle_show_set_script(&caller, 1);
    panel_render(&s, "REVJ", 40100, fb);
    dump(dir, "idle_caller", fb);
    panel_render_set_info(NULL);
    idle_show_set_script(NULL, 0);
}

int main(int argc, char **argv)
{
    exhaustive();
    wifi_checks();
    splash_check();
    idle_checks();
    scan_checks();
    real_info_checks();
    if (argc == 3 && strcmp(argv[1], "--dump") == 0) {
        mockups(argv[2]);
        idle_dump(argv[2]);
    }
    if (failures) {
        printf("panel_check: %d FAILED of %ld renders\n", failures, renders);
        return 1;
    }
    printf("panel_check: ok, %ld renders, guards intact\n", renders);
    return 0;
}
