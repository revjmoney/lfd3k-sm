#include "picker_draw.h"
#include <string.h>
#include "font5x7.h"
#include "logo_bitmaps.h"

#define W 128
#define H 64

/* ---- primitives (same conventions as the LFD3K-SM panel) ---- */

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

static int get(const uint8_t *fb, int x, int y)
{
    if (x < 0 || x >= W || y < 0 || y >= H) {
        return 0;
    }
    return (fb[(y >> 3) * W + x] >> (y & 7)) & 1;
}

static void fill(uint8_t *fb, int x, int y, int w, int h, int on)
{
    for (int j = 0; j < h; j++) {
        for (int i = 0; i < w; i++) {
            px(fb, x + i, y + j, on);
        }
    }
}

static void hline(uint8_t *fb, int x0, int x1, int y, int on)
{
    for (int x = x0; x <= x1; x++) {
        px(fb, x, y, on);
    }
}

static int text_w(const char *t)
{
    int n = (int)strlen(t);
    return n ? n * 6 - 1 : 0;
}

/* Draws the first n characters of t (n < 0: all). on = 0 clears pixels. */
static void text_n(uint8_t *fb, int x, int y, const char *t, int n, int on)
{
    for (; *t && n != 0; t++, x += 6, n--) {
        unsigned c = (unsigned char)*t;
        if (c < FONT_FIRST || c > FONT_LAST) {
            c = '?';
        }
        const uint8_t *g = &font5x7[(c - FONT_FIRST) * 5];
        for (int col = 0; col < 5; col++) {
            for (int row = 0; row < 7; row++) {
                if (g[col] & (1u << row)) {
                    px(fb, x + col, y + row, on);
                }
            }
        }
    }
}

static void text(uint8_t *fb, int x, int y, const char *t, int on)
{
    text_n(fb, x, y, t, -1, on);
}

static void text_c(uint8_t *fb, int y, const char *t, int on)
{
    text(fb, (W - text_w(t)) / 2, y, t, on);
}

static void text_scaled(uint8_t *fb, int x, int y, const char *t, int sc)
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
                    fill(fb, x + col * sc, y + row * sc, sc, sc, 1);
                }
            }
        }
    }
}

/* ---- the AXIS BBS splash ---- */

typedef struct {
    const uint32_t *rows;
    int w;
    int x;
} glyph_t;

#define LOGO_GAP 6
#define LOGO_Y   2
#define LOGO_X0  ((W - (LOGO_A_W + LOGO_X_W + LOGO_I_W + LOGO_S_W + 3 * LOGO_GAP)) / 2)
#define LOGO_X1  (LOGO_X0 + LOGO_A_W + LOGO_X_W + LOGO_I_W + LOGO_S_W + 3 * LOGO_GAP - 1)

static const glyph_t LETTERS[4] = {
    {LOGO_A, LOGO_A_W, LOGO_X0},
    {LOGO_X, LOGO_X_W, LOGO_X0 + LOGO_A_W + LOGO_GAP},
    {LOGO_I, LOGO_I_W, LOGO_X0 + LOGO_A_W + LOGO_X_W + 2 * LOGO_GAP},
    {LOGO_S, LOGO_S_W, LOGO_X0 + LOGO_A_W + LOGO_X_W + LOGO_I_W + 3 * LOGO_GAP},
};

/* Timeline, ms into the logo. */
#define T_ON_END    250u   /* CRT line opens from the centre */
#define T_PASS      700u   /* one interlaced scan pass (even rows, then odd rows) */
#define T_SCAN0     T_ON_END
#define T_SCAN_END  (T_SCAN0 + 2 * T_PASS)
#define T_HINT0     900u   /* PRESS PRG FOR MODES types in */
#define T_HINT_CH   30u
#define T_RULE0     T_SCAN_END
#define T_RULE_LEN  350u
#define T_SWEEP0    (T_RULE0 + T_RULE_LEN)
#define T_SWEEP_P   1300u  /* one sweep pass under the letters */
#define T_GLINT_LEN 520u

static void letter_row(uint8_t *fb, const glyph_t *g, int r)
{
    uint32_t bits = g->rows[r];
    for (int i = 0; i < g->w; i++) {
        if (bits & (1u << (g->w - 1 - i))) {
            px(fb, g->x + i, LOGO_Y + r, 1);
        }
    }
}

/* Row r is revealed once the scan beam of its pass has gone by. */
static int row_shown(int r, uint32_t t)
{
    if (t < T_SCAN0) {
        return 0;
    }
    uint32_t pass = (uint32_t)(r & 1);
    uint32_t start = T_SCAN0 + pass * T_PASS;
    if (t < start) {
        return 0;
    }
    uint32_t done = (t - start) * (LOGO_H / 2) / T_PASS; /* rows of this parity done */
    return (uint32_t)(r >> 1) < done;
}

static void draw_letters(uint8_t *fb, uint32_t t)
{
    for (int r = 0; r < LOGO_H; r++) {
        if (!row_shown(r, t)) {
            continue;
        }
        for (int k = 0; k < 4; k++) {
            letter_row(fb, &LETTERS[k], r);
        }
    }
    /* The beam: a dotted line on the row being written, bright over the letters. */
    if (t >= T_SCAN0 && t < T_SCAN_END) {
        uint32_t pass = (t - T_SCAN0) / T_PASS;
        uint32_t into = (t - T_SCAN0) % T_PASS;
        int r = (int)(into * (LOGO_H / 2) / T_PASS) * 2 + (int)pass;
        if (r < LOGO_H) {
            int y = LOGO_Y + r;
            for (int x = LOGO_X0 - 6; x <= LOGO_X1 + 6; x++) {
                if (((x + (int)pass) & 1) == 0) {
                    px(fb, x, y, 1);
                }
            }
            for (int k = 0; k < 4; k++) {
                letter_row(fb, &LETTERS[k], r);
            }
        }
    }
}

/* A dark diagonal glint crossing the finished letters. */
static void glint(uint8_t *fb, uint32_t t, uint32_t t0)
{
    if (t < t0 || t >= t0 + T_GLINT_LEN) {
        return;
    }
    int span = (LOGO_X1 - LOGO_X0) + LOGO_H + 20;
    int g = LOGO_X0 - LOGO_H / 2 - 10 + (int)((t - t0) * (uint32_t)span / T_GLINT_LEN);
    for (int r = 0; r < LOGO_H; r++) {
        int y = LOGO_Y + r;
        int x0 = g + (LOGO_H - r) * 2 / 3;
        for (int x = x0; x < x0 + 4; x++) {
            if (get(fb, x, y)) {
                px(fb, x, y, 0);
            }
        }
        /* a thin lit edge just behind it */
        int xe = x0 - 3;
        if (get(fb, xe, y)) {
            px(fb, xe, y, 0);
        }
    }
}

#define BBS_Y   32
#define RULE_Y  35
#define HINT_Y  44
#define DOTS_Y  56

static void bbs_line(uint8_t *fb, uint32_t t)
{
    static const char bbs[] = "B B S";
    int tw = text_w(bbs);
    int tx = (W - tw) / 2;
    if (t < T_RULE0) {
        return;
    }
    uint32_t dt = t - T_RULE0;
    /* letters drop in one by one */
    int chars = (int)(dt / 90u) + 1;
    text_n(fb, tx, BBS_Y, bbs, chars > 5 ? 5 : chars, 1);

    /* rules grow outward from the text */
    int lmax = tx - 5 - LOGO_X0;
    int len = dt >= T_RULE_LEN ? lmax : (int)(dt * (uint32_t)lmax / T_RULE_LEN);
    hline(fb, tx - 5 - len, tx - 5, RULE_Y, 1);
    hline(fb, tx + tw + 4, tx + tw + 4 + len, RULE_Y, 1);

    /* the sweep: a bright block running along the rules under AXIS */
    if (t >= T_SWEEP0) {
        uint32_t into = (t - T_SWEEP0) % T_SWEEP_P;
        int travel = (LOGO_X1 - LOGO_X0) + 24;
        int sx = LOGO_X0 - 12 + (int)(into * (uint32_t)travel / T_SWEEP_P);
        for (int x = sx; x < sx + 12; x++) {
            int on_rule = (x >= LOGO_X0 && x <= tx - 5) || (x >= tx + tw + 4 && x <= LOGO_X1);
            if (!on_rule) {
                continue;
            }
            px(fb, x, RULE_Y - 1, 1);
            px(fb, x, RULE_Y + 1, 1);
        }
        /* a fading tail: every other pixel above and below */
        for (int x = sx - 10; x < sx; x++) {
            int on_rule = (x >= LOGO_X0 && x <= tx - 5) || (x >= tx + tw + 4 && x <= LOGO_X1);
            if (on_rule && ((x - sx) & 1) == 0) {
                px(fb, x, RULE_Y - 1, 1);
                px(fb, x, RULE_Y + 1, 1);
            }
        }
        /* the text glows as the sweep passes under it */
        if (sx + 12 > tx && sx < tx + tw) {
            fill(fb, tx - 2, BBS_Y - 1, tw + 4, 9, 1);
            text(fb, tx, BBS_Y, bbs, 0);
        }
    }
}

static void dot(uint8_t *fb, int x, int y, int style)
{
    /* style 2: full 5x5 rounded dot, 1: small 3x3, 0: hollow ring */
    if (style == 2) {
        fill(fb, x + 1, y, 3, 5, 1);
        fill(fb, x, y + 1, 5, 3, 1);
    } else if (style == 1) {
        fill(fb, x + 1, y + 1, 3, 3, 1);
    } else {
        hline(fb, x + 1, x + 3, y, 1);
        hline(fb, x + 1, x + 3, y + 4, 1);
        fill(fb, x, y + 1, 1, 3, 1);
        fill(fb, x + 4, y + 1, 1, 3, 1);
    }
}

static void countdown(uint8_t *fb, uint32_t t)
{
    int left = 5 - (int)(t / 1000u); /* whole seconds still to go */
    if (left < 0) {
        left = 0;
    }
    int x0 = (W - (5 * 5 + 4 * 6)) / 2;
    for (int i = 0; i < 5; i++) {
        int style = 0;
        if (i < left - 1) {
            style = 2;
        } else if (i == left - 1) {
            /* the second running out: full, then it shrinks */
            style = (t % 1000u) < 600u ? 2 : 1;
        }
        dot(fb, x0 + i * 11, DOTS_Y, style);
    }
}

void pk_draw_logo(uint8_t fb[PK_FB_BYTES], uint32_t t)
{
    memset(fb, 0, PK_FB_BYTES);

    /* CRT switch-on: a line opening from the centre. */
    if (t < T_ON_END) {
        int half = 2 + (int)(t * 62u / T_ON_END);
        hline(fb, W / 2 - half, W / 2 + half - 1, LOGO_Y + LOGO_H / 2, 1);
        px(fb, W / 2 - 1, LOGO_Y + LOGO_H / 2 - 1, 1);
        px(fb, W / 2, LOGO_Y + LOGO_H / 2 + 1, 1);
    }

    draw_letters(fb, t);
    glint(fb, t, T_SCAN_END + 150u);
    glint(fb, t, 3500u);
    bbs_line(fb, t);

    if (t >= T_HINT0) {
        static const char hint[] = "PRESS PRG FOR MODES";
        int n = (int)((t - T_HINT0) / T_HINT_CH) + 1;
        int len = (int)strlen(hint);
        int x = (W - text_w(hint)) / 2;
        text_n(fb, x, HINT_Y, hint, n, 1);
        if (n < len) { /* a block cursor while it types */
            fill(fb, x + n * 6, HINT_Y, 5, 7, 1);
        }
    }
    countdown(fb, t);
}

/* ---- the mode list ---- */

/* 4x5 tag letters for the in-row "NO IMAGE" (the 5x7 font does not fit
 * beside SIMIAN RADAR). Column bytes, bit 0 = top. */
typedef struct {
    char c;
    uint8_t w;
    uint8_t col[5];
} tag_glyph_t;

static const tag_glyph_t TAG[] = {
    {'N', 4, {0x1F, 0x02, 0x04, 0x1F, 0}},
    {'O', 4, {0x0E, 0x11, 0x11, 0x0E, 0}},
    {'I', 3, {0x11, 0x1F, 0x11, 0, 0}},
    {'M', 5, {0x1F, 0x02, 0x04, 0x02, 0x1F}},
    {'A', 4, {0x1E, 0x05, 0x05, 0x1E, 0}},
    {'G', 4, {0x0E, 0x11, 0x15, 0x1D, 0}},
    {'E', 4, {0x1F, 0x15, 0x15, 0x11, 0}},
    {' ', 2, {0, 0, 0, 0, 0}},
};

static const tag_glyph_t *tag_glyph(char c)
{
    for (unsigned i = 0; i < sizeof(TAG) / sizeof(TAG[0]); i++) {
        if (TAG[i].c == c) {
            return &TAG[i];
        }
    }
    return &TAG[sizeof(TAG) / sizeof(TAG[0]) - 1];
}

static int tag_w(const char *t)
{
    int w = 0;
    for (; *t; t++) {
        w += tag_glyph(*t)->w + 1;
    }
    return w ? w - 1 : 0;
}

static void tag(uint8_t *fb, int x, int y, const char *t, int on)
{
    for (; *t; t++) {
        const tag_glyph_t *g = tag_glyph(*t);
        for (int c = 0; c < g->w; c++) {
            for (int r = 0; r < 5; r++) {
                if (g->col[c] & (1u << r)) {
                    px(fb, x + c, y + r, on);
                }
            }
        }
        x += g->w + 1;
    }
}

#define HEAD_H  9
#define ROW_Y0  10
#define ROW_H   11
#define FOOT_Y  54

void pk_draw_list(uint8_t fb[PK_FB_BYTES], const pk_state_t *s, uint32_t now)
{
    memset(fb, 0, PK_FB_BYTES);

    /* header: inverted bar */
    fill(fb, 0, 0, W, HEAD_H, 1);
    text(fb, 3, 1, "AXIS BBS", 0);
    text(fb, W - 3 - text_w("MODES"), 1, "MODES", 0);

    for (int i = 0; i < PK_MODES; i++) {
        int y = ROW_Y0 + i * ROW_H;
        int sel = (i == s->sel);
        int on = sel ? 0 : 1;
        if (sel) {
            fill(fb, 0, y, W, ROW_H - 2, 1);
            /* hold progress: the bar's bottom edge empties as PRG is held */
            if (s->phase == PK_LIST && s->down && s->armed) {
                uint32_t held = s->hold_fired ? PK_LONG_MS : now - s->down_ms;
                if (held > PK_LONG_MS) {
                    held = PK_LONG_MS;
                }
                int len = (int)(held * (uint32_t)W / PK_LONG_MS);
                hline(fb, 0, len - 1, y + ROW_H - 3, 0);
            }
        }
        /* last used: a small arrow in the margin */
        if (i == s->last) {
            for (int k = 0; k < 3; k++) {
                fill(fb, 2 + k, y + 1 + k, 1, 7 - 2 * k, on);
            }
        }
        text(fb, 8, y + 1, pk_mode_name(i), on);
        if (!s->valid[i]) {
            const char *t = "NO IMAGE";
            tag(fb, W - 2 - tag_w(t), y + 2, t, on);
        }
    }

    /* footer */
    if (pk_noimg(s, now)) {
        if (((s->noimg_until - now) / 250u) % 2 == 0) {
            fill(fb, 0, FOOT_Y - 1, W, 9, 1);
            text_c(fb, FOOT_Y, "NO IMAGE", 0);
        } else {
            text_c(fb, FOOT_Y, "NO IMAGE", 1);
        }
    } else if (s->phase == PK_LIST && s->hold_fired) {
        text_c(fb, FOOT_Y, "RELEASE TO START", 1);
    } else if (pk_crash_note(s)) {
        text_c(fb, FOOT_Y, "LAST MODE CRASHED", 1);
        hline(fb, 4, W - 5, H - 1, 1); /* a solid rule marks it as a warning */
    } else if (s->valid[s->sel]) {
        text_c(fb, FOOT_Y, "TAP NEXT  HOLD START", 1);
    } else {
        text_c(fb, FOOT_Y, "NO IMAGE  TAP NEXT", 1);
    }

    /* idle bar: the last row of pixels runs out over 15 s (off after a crash) */
    if (s->phase == PK_LIST && !s->down && !s->crashed) {
        uint32_t idle = now - s->input_ms;
        if (idle > PK_IDLE_MS) {
            idle = PK_IDLE_MS;
        }
        int len = (int)((PK_IDLE_MS - idle) * (uint32_t)W / PK_IDLE_MS);
        for (int x = 0; x < len; x += 2) {
            px(fb, x, H - 1, 1);
        }
    }
}

/* ---- starting card ---- */

void pk_draw_launch(uint8_t fb[PK_FB_BYTES], int mode, uint32_t t)
{
    memset(fb, 0, PK_FB_BYTES);
    const char *name = pk_mode_name(mode);
    text_c(fb, 8, "STARTING", 1);
    int sc = (text_w(name) * 2 + 1 <= W - 4) ? 2 : 1;
    int w = (int)strlen(name) * 6 * sc - sc;
    int y = sc == 2 ? 24 : 28;
    text_scaled(fb, (W - w) / 2, y, name, sc);
    /* a bar that fills while the slot is set and the board restarts */
    int bx = 14, bw = W - 28;
    hline(fb, bx, bx + bw - 1, 50, 1);
    hline(fb, bx, bx + bw - 1, 54, 1);
    fill(fb, bx, 50, 1, 5, 1);
    fill(fb, bx + bw - 1, 50, 1, 5, 1);
    int len = (int)((t > 600u ? 600u : t) * (uint32_t)(bw - 4) / 600u);
    fill(fb, bx + 2, 52, len, 1, 1);
}
