/* Replays a modem status log through the real front panel renderer
 * (firmware/src/panel_render.c), for the testing video: what the OLED of a
 * simulated modem would have shown.
 *
 *   panel_replay <status.csv> <board name> <out.txt> [every_ms]
 *
 * status.csv is written by `lfd3k-voice-pc --trace <dir>`: every 250 ms the
 * fields of lfd3k_status_t as lfd3k_status fills them, plus the last 64
 * scope samples (hex dBm, then 0/1 transmit flags). Every line is rendered
 * (the renderer keeps a little animation memory); one frame every every_ms
 * (default 1000) goes to out.txt as "<unix_ms> FB:<2048 hex digits>".
 * tools/voice_video.py turns them into PNGs with fb2png.
 *
 * Build (as tools/panel_check.ps1 does):
 *   gcc -std=c11 -O1 -Wall -Wextra -Werror -I firmware/src -I firmware/include
 *       -o panel_replay tools/panel_replay.c firmware/src/panel_render.c
 *       firmware/src/idle_show.c
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "idle_show.h"
#include "panel_render.h"

static int hexval(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
}

int main(int argc, char **argv)
{
    if (argc < 4) {
        fprintf(stderr, "usage: panel_replay <status.csv> <board name> <out.txt> [every_ms]\n");
        return 2;
    }
    long long every = argc > 4 ? atoll(argv[4]) : 1000;
    FILE *in = fopen(argv[1], "r");
    if (!in) {
        fprintf(stderr, "cannot read %s\n", argv[1]);
        return 1;
    }
    FILE *out = fopen(argv[3], "w");
    if (!out) {
        fprintf(stderr, "cannot write %s\n", argv[3]);
        fclose(in);
        return 1;
    }
    char line[1024];
    long long first = -1, next = 0;
    int frames = 0;
    uint8_t fb[PANEL_FB_BYTES];
    if (!fgets(line, sizeof(line), in)) { /* header */
        fclose(in);
        fclose(out);
        return 1;
    }
    while (fgets(line, sizeof(line), in)) {
        long long unix_ms;
        int lights, state, tier, rssi, snr, radio_ok, wire, simnet, scanning, hits, heard;
        char dbm[200] = "", tx[100] = "";
        int n = sscanf(line, "%lld,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%199[0-9A-Fa-f],%99[01]",
                       &unix_ms, &lights, &state, &tier, &rssi, &snr, &radio_ok, &wire, &simnet,
                       &scanning, &hits, &heard, dbm, tx);
        if (n < 12) {
            continue;
        }
        if (first < 0) {
            first = unix_ms;
            next = unix_ms;
        }
        lfd3k_status_t s;
        memset(&s, 0, sizeof(s));
        s.lights = (uint8_t)lights;
        s.state = (uint8_t)state;
        s.tier = (int8_t)tier;
        s.rssi = (int16_t)rssi;
        s.snr_x4 = (int16_t)snr;
        s.radio_ok = (uint8_t)radio_ok;
        s.wire = (uint8_t)wire;
        s.simnet = (uint8_t)simnet;
        s.scanning = (uint8_t)scanning;
        s.scan_hits = (uint8_t)hits;
        s.heard = (uint16_t)heard;
        rf_trace_t t;
        memset(&t, 0, sizeof(t));
        size_t nd = strlen(dbm) / 2, nt = strlen(tx);
        size_t cnt = nd < nt ? nd : nt;
        if (cnt > RF_TRACE_N) cnt = RF_TRACE_N;
        for (size_t i = 0; i < cnt; i++) {
            int hi = hexval(dbm[2 * i]), lo = hexval(dbm[2 * i + 1]);
            t.dbm[i] = (int8_t)(uint8_t)((hi < 0 ? 0 : hi) * 16 + (lo < 0 ? 0 : lo));
            t.tx[i] = tx[i] == '1';
        }
        t.count = (uint8_t)cnt;
        panel_render_set_trace(&t);
        /* The firmware's clock starts at boot; start the replay 10 s in. */
        uint32_t now_ms = (uint32_t)(unix_ms - first + 10000);
        panel_render(&s, argv[2], now_ms, fb);
        if (unix_ms >= next) {
            next += every;
            if (next <= unix_ms) next = unix_ms + every;
            fprintf(out, "%lld FB:", unix_ms);
            for (int i = 0; i < PANEL_FB_BYTES; i++) {
                fprintf(out, "%02X", fb[i]);
            }
            fputs("\n", out);
            frames++;
        }
    }
    fclose(in);
    fclose(out);
    printf("%d frames\n", frames);
    return frames > 0 ? 0 : 1;
}
