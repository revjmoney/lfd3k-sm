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
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
}

int main(int argc, char **argv)
{
    exhaustive();
    wifi_checks();
    splash_check();
    if (argc == 3 && strcmp(argv[1], "--dump") == 0) {
        mockups(argv[2]);
    }
    if (failures) {
        printf("panel_check: %d FAILED of %ld renders\n", failures, renders);
        return 1;
    }
    printf("panel_check: ok, %ld renders, guards intact\n", renders);
    return 0;
}
