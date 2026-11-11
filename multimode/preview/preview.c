/* PC preview of the picker screens: runs the same drawing and menu code as
 * the board and writes raw 1024 byte frames for make_preview.py.
 * Build (from the repo root):
 *   gcc -O2 -I multimode/picker/src -I firmware/src multimode/preview/preview.c \
 *       multimode/picker/src/picker_draw.c multimode/picker/src/picker_menu.c -o preview.exe
 *   preview.exe logo.raw menu.raw */
#include <stdio.h>
#include "picker_draw.h"
#include "picker_menu.h"

static uint8_t fb[PK_FB_BYTES];

static void put(FILE *f)
{
    fwrite(fb, 1, sizeof(fb), f);
}

int main(int argc, char **argv)
{
    if (argc < 3) {
        fprintf(stderr, "usage: preview logo.raw menu.raw\n");
        return 2;
    }
    FILE *lf = fopen(argv[1], "wb");
    FILE *mf = fopen(argv[2], "wb");
    if (!lf || !mf) {
        return 1;
    }
    /* logo: 40 ms frames (the board's frame time) over 5 s */
    for (uint32_t t = 0; t <= PK_LOGO_MS; t += 40) {
        pk_draw_logo(fb, t);
        put(lf);
    }

    /* menu states */
    bool valid[PK_MODES] = {true, false, true, true};
    pk_state_t s;
    uint32_t now = 1000;
    pk_init(&s, PK_RNODE, valid, false, false, now);
    pk_update(&s, true, now += 20); /* PRG during the logo opens the list */
    pk_update(&s, false, now += 80);
    pk_update(&s, false, now += 2000);
    pk_draw_list(fb, &s, now); /* 1: list on the last used mode */
    put(mf);

    pk_update(&s, true, now += 10); /* short press: next */
    pk_update(&s, false, now += 120);
    pk_update(&s, false, now += 500);
    pk_draw_list(fb, &s, now); /* 2: MESH highlighted */
    put(mf);

    pk_update(&s, true, now += 10);
    pk_update(&s, false, now += 120);
    pk_update(&s, true, now += 400);
    pk_update(&s, true, now += 450);
    pk_draw_list(fb, &s, now); /* 3: LFD3K-SM, held for 450 ms (hold bar) */
    put(mf);

    pk_update(&s, false, now += 20); /* released before 0.8 s: a tap */
    pk_update(&s, false, now += 300);
    pk_draw_list(fb, &s, now); /* 4: SIMIAN RADAR (no image) highlighted */
    put(mf);

    pk_update(&s, true, now += 300);
    pk_update(&s, true, now += 820);
    pk_draw_list(fb, &s, now + 100); /* 5: long press on an empty slot: NO IMAGE */
    put(mf);

    pk_update(&s, false, now += 200);
    pk_update(&s, true, now += 100);
    pk_update(&s, false, now += 100);
    for (int i = 0; i < 50 && s.phase == PK_LIST; i++) {
        pk_update(&s, false, now += 500);
    }
    fprintf(stderr, "after idle: phase %d launch %d\n", (int)s.phase, s.launch);
    pk_draw_launch(fb, s.launch, 300); /* 6: idle start of RNODE */
    put(mf);
    pk_draw_launch(fb, PK_SIMIAN, 600); /* 7: long name card */
    put(mf);

    /* 8: held past 0.8 s on a good slot: nothing starts until release */
    pk_init(&s, PK_LFD3K, valid, false, false, now);
    pk_update(&s, true, now += 20);
    pk_update(&s, false, now += 100);
    pk_update(&s, true, now += 300);
    pk_update(&s, true, now += 900);
    fprintf(stderr, "held: phase %d (1 = still list)\n", (int)s.phase);
    pk_draw_list(fb, &s, now);
    put(mf);
    pk_update(&s, false, now += 30);
    fprintf(stderr, "released: phase %d launch %d\n", (int)s.phase, s.launch);

    /* 9: boot after a crash: the logo ends in the list, no idle start */
    pk_init(&s, PK_MESH, valid, false, true, now);
    for (int i = 0; i < 60; i++) {
        pk_update(&s, false, now += 500);
    }
    fprintf(stderr, "crashed after 30 s: phase %d (1 = list)\n", (int)s.phase);
    pk_draw_list(fb, &s, now);
    put(mf);

    fclose(lf);
    fclose(mf);
    return 0;
}
