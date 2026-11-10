/* PC test for the pure C panel renderer (firmware/src/panel_render.c).
 *
 *  panel_check                 run the checks, exit 0 when all pass
 *  panel_check --dump <dir>    also write the mockup framebuffers as <name>.fb.txt
 *                              ("FB:" plus 2048 hex digits, the AT+FB format)
 *
 * Checks: every tier -1..7 x state 0..5 x radio_ok 0/1 with extreme RSSI and
 * SNR and a 16 character name, at several times, must leave the guard bytes
 * around the 1024 byte buffer untouched, draw something, and be repeatable.
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
}

int main(int argc, char **argv)
{
    exhaustive();
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
