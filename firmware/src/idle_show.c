/* Idle text show (see idle_show.h). Pure C, no ESP-IDF, no floats, no heap.
 *
 * Schedule. Time is cut into 8 s loop slots, n = now_ms / 8000. Slots come in groups of
 * S + 12, where S is the number of whole slots the scroller message needs (2 for the
 * current text). One scroller per group starts at slot p = 1 + hash(group) % 11:
 *   slot p - 1      PRE: its item stays up the whole loop (no transition)
 *   slots p..p+S-1  SCROLL: the message scrolls in from the right on a sine wave,
 *                   pushing the previous item off to the left
 *   slot p + S      TAIL: the message runs out during 0..4000 ms, then a normal
 *                   transition brings in the next item over an empty band
 * Every other slot is NORMAL: item(n) for 0..4000 ms, a transition 4000..7000 ms, then
 * item(n + 1). Items are picked by hash, weighted; an odd slot never repeats either
 * neighbour, so no item shows twice in a row. The transition is hash(n) % 12.
 *
 * Band. Items are drawn into an 18 row mask, rows 0..17 = y 12..29: 2x text at y 14..27
 * (Ş's cedilla to y 29), 1x text at y 18..24, the script bitmaps (16 or 17 rows) centred
 * on y 21. Each column of the mask is a uint32_t, bit 0 = y 12. */
#include <stdio.h>
#include <string.h>
#include "font5x7.h"
#include "idle_show.h"
#include "idle_words.h"

#define W 128
#define H 64
#define BAND_Y 12
#define BAND_H 18
#define BAND_ALL ((1u << BAND_H) - 1u)
#define MID_ROW 9 /* centre row of the band (y 21) */
#define CX 64     /* items are centred on x 64 */

/* The long demo scroller. ASCII only (anything outside the font draws '?'); Rev may
 * change the words: the show works out how many loops it needs. */
static const char SCROLL_TEXT[] =
    "BUILT BY: REV J. MONEY, AUTHOR OF ROTATOCAM!   OPEN SOURCE DATA MODEM RADIO TOY!";
#define SCROLL_PX_S  60 /* top speed, pixels per second */
#define SCROLL_EXTRA 12 /* normal slots per group besides the scroller */

static const char *const TRANS_NAME[TR_COUNT] = {
    "EXPLODE", "BLOCKS", "SINE SCROLL", "SPLIT FLAP", "TYPEWRITER", "DRIP MELT",
    "GLITCH", "MATRIX RAIN", "STARFIELD WARP", "CRT POWER", "VENETIAN BLINDS", "PIXELATE",
};

typedef struct {
    const char *name;
    uint8_t weight;
    const char *text;     /* UTF-8, our 5x7 font */
    const idle_bmp_t *bmp; /* or a pre-rendered bitmap */
} item_def_t;

static const item_def_t ITEMS[IS_ITEMS] = {
    {"ENGLISH", 3, "IDLE", NULL},
    {"MANDARIN", 1, NULL, &IDLE_BMP_ZH},
    {"JAPANESE", 1, NULL, &IDLE_BMP_JA},
    {"HINDI", 1, NULL, &IDLE_BMP_HI},
    {"SPANISH", 1, "INACTIVO", NULL},
    {"FRENCH", 1, "INACTIF", NULL},
    {"ARABIC", 1, NULL, &IDLE_BMP_AR},
    {"BENGALI", 1, NULL, &IDLE_BMP_BN},
    {"PORTUGUESE", 1, "OCIOSO", NULL},
    {"RUSSIAN", 1, "\xD0\x9F\xD0\xA0\xD0\x9E\xD0\xA1\xD0\xA2\xD0\x9E\xD0\x99", NULL}, /* ПРОСТОЙ */
    {"URDU", 1, NULL, &IDLE_BMP_UR},
    {"INDONESIAN", 1, "SIAGA", NULL},
    {"GERMAN", 1, "LEERLAUF", NULL},
    {"SWAHILI", 1, "TULIVU", NULL},
    {"TURKISH", 1, "BO\xC5\x9E" "TA", NULL}, /* BOŞTA */
    {"CLOCK", 1, NULL, NULL},
    {"UPTIME", 1, NULL, NULL},
    {"TEMP", 1, NULL, NULL},
    {"HEARD", 1, NULL, NULL},
    {"CALLER", 1, NULL, NULL},
    {"GPS", 1, NULL, NULL},
};

static const int8_t SIN64[64] = {
    0, 12, 25, 37, 49, 60, 71, 81, 90, 98, 106, 112, 117, 122, 125, 126,
    127, 126, 125, 122, 117, 112, 106, 98, 90, 81, 71, 60, 49, 37, 25, 12,
    0, -12, -25, -37, -49, -60, -71, -81, -90, -98, -106, -112, -117, -122, -125, -126,
    -127, -126, -125, -122, -117, -112, -106, -98, -90, -81, -71, -60, -49, -37, -25, -12,
};

/* 3x5 bits, our own: rows top to bottom, bit 2 = left column. */
static const uint8_t BIT0[5] = {7, 5, 5, 5, 7};
static const uint8_t BIT1[5] = {2, 6, 2, 2, 7};

typedef struct {
    uint32_t col[W];
    int x0, x1; /* ink columns [x0, x1); equal when empty */
    int r0, r1; /* ink rows [r0, r1) */
    int cell;   /* letter cell width in px (typing step) */
    int rtl;
} mask_t;

static mask_t s_a, s_b; /* panel task only */

/* ---- small helpers ---- */

static uint32_t mix(uint32_t v)
{
    v ^= v >> 16;
    v *= 0x7FEB352Du;
    v ^= v >> 15;
    v *= 0x846CA68Bu;
    v ^= v >> 16;
    return v;
}

static int clampi(int v, int lo, int hi)
{
    return v < lo ? lo : (v > hi ? hi : v);
}

static int mini(int a, int b)
{
    return a < b ? a : b;
}

static int maxi(int a, int b)
{
    return a > b ? a : b;
}

/* smoothstep, u and result in 0..1024 */
static int smooth(int u)
{
    u = clampi(u, 0, 1024);
    return u * u / 1024 * (3072 - 2 * u) / 1024;
}

static void px(uint8_t *fb, int x, int y)
{
    if (x < 0 || x >= W || y < 0 || y >= H) {
        return;
    }
    fb[(y >> 3) * W + x] |= (uint8_t)(1u << (y & 7));
}

/* a pixel in band row r (dropped outside the band) */
static void bpx(uint8_t *fb, int x, int r)
{
    if (r < 0 || r >= BAND_H) {
        return;
    }
    px(fb, x, BAND_Y + r);
}

static void draw_col(uint8_t *fb, int x, uint32_t bits)
{
    bits &= BAND_ALL;
    for (int r = 0; bits; r++, bits >>= 1) {
        if (bits & 1u) {
            px(fb, x, BAND_Y + r);
        }
    }
}

static uint32_t mcol(const mask_t *m, int x)
{
    return (x >= 0 && x < W) ? m->col[x] : 0u;
}

static int empty(const mask_t *m)
{
    return m->x1 <= m->x0;
}

static void tiny_bit(uint8_t *fb, int x, int y, int one, int clip_band)
{
    const uint8_t *g = one ? BIT1 : BIT0;
    for (int r = 0; r < 5; r++) {
        for (int c = 0; c < 3; c++) {
            if (g[r] & (4u >> c)) {
                if (clip_band) {
                    bpx(fb, x + c, y + r - BAND_Y);
                } else {
                    px(fb, x + c, y + r);
                }
            }
        }
    }
}

/* ---- glyphs and text ---- */

static const uint8_t *glyph(uint32_t cp)
{
    if (cp >= FONT_FIRST && cp <= FONT_LAST) {
        return &font5x7[(cp - FONT_FIRST) * 5];
    }
    for (int i = 0; i < FONT_EXT_N; i++) {
        if (font5x7_ext[i].cp == cp) {
            return font5x7_ext[i].col;
        }
    }
    return &font5x7[('?' - FONT_FIRST) * 5];
}

/* Next code point of a UTF-8 string (1 to 3 bytes); a broken sequence gives '?'. */
static uint32_t utf8_next(const char **ps)
{
    const unsigned char *s = (const unsigned char *)*ps;
    uint32_t c = s[0];
    if (c < 0x80) {
        *ps += 1;
        return c;
    }
    if ((c & 0xE0) == 0xC0 && (s[1] & 0xC0) == 0x80) {
        *ps += 2;
        return ((c & 0x1F) << 6) | (s[1] & 0x3F);
    }
    if ((c & 0xF0) == 0xE0 && (s[1] & 0xC0) == 0x80 && (s[2] & 0xC0) == 0x80) {
        *ps += 3;
        return ((c & 0x0F) << 12) | ((uint32_t)(s[1] & 0x3F) << 6) | (s[2] & 0x3F);
    }
    *ps += 1;
    return '?';
}

/* One glyph column at 2x, as band bits (rows 2..15, the cedilla 16..17). */
static uint32_t col2x(uint8_t g)
{
    uint32_t b = 0;
    for (int r = 0; r < 8; r++) {
        if (g & (1u << r)) {
            b |= 3u << (2 + 2 * r);
        }
    }
    return b;
}

static void extents(mask_t *m)
{
    uint32_t any = 0;
    m->x0 = m->x1 = CX;
    for (int x = 0; x < W; x++) {
        if (m->col[x]) {
            if (!any) {
                m->x0 = x;
            }
            m->x1 = x + 1;
            any |= m->col[x];
        }
    }
    m->r0 = m->r1 = MID_ROW;
    if (any) {
        int r = 0;
        while (!(any & (1u << r))) {
            r++;
        }
        m->r0 = r;
        r = BAND_H - 1;
        while (!(any & (1u << r))) {
            r--;
        }
        m->r1 = r + 1;
    }
}

static void text_mask(mask_t *m, const char *t)
{
    int n = 0;
    for (const char *p = t; *p && n < 40; n++) {
        (void)utf8_next(&p);
    }
    if (n == 0) {
        return;
    }
    int sc = (n * 12 - 2 > 124) ? 1 : 2;
    int w = n * 6 * sc - sc;
    int x = (W - w) / 2;
    int row0 = sc == 2 ? 2 : 6;
    m->cell = 6 * sc;
    const char *p = t;
    for (int i = 0; i < n && *p; i++, x += 6 * sc) {
        const uint8_t *g = glyph(utf8_next(&p));
        for (int c = 0; c < 5; c++) {
            uint32_t bits = 0;
            for (int r = 0; r < 8; r++) {
                if (g[c] & (1u << r)) {
                    bits |= (sc == 2 ? 3u : 1u) << (row0 + r * sc);
                }
            }
            for (int k = 0; k < sc; k++) {
                int xx = x + c * sc + k;
                if (xx >= 0 && xx < W) {
                    m->col[xx] |= bits & BAND_ALL;
                }
            }
        }
    }
}

static void bmp_mask(mask_t *m, const idle_bmp_t *b)
{
    int x = (W - b->w) / 2;
    int top = (BAND_H - b->h) / 2;
    for (int i = 0; i < b->w; i++) {
        if (x + i >= 0 && x + i < W) {
            m->col[x + i] = (b->cols[i] << top) & BAND_ALL;
        }
    }
    m->cell = 12;
    m->rtl = b->rtl;
}

/* The text of an info item; 0 when it is not available. */
static int info_text(const panel_info_t *in, int item, char *out, size_t cap)
{
    out[0] = 0;
    switch (item) {
    case IS_CLOCK:
        if (!in->time_valid || in->hour > 23 || in->minute > 59) {
            return 0;
        }
        snprintf(out, cap, "%02u:%02u", (unsigned)in->hour, (unsigned)in->minute);
        return 1;
    case IS_UPTIME: {
        uint32_t s = in->uptime_s;
        if (s == 0) {
            return 0;
        }
        uint32_t d = s / 86400u, h = s / 3600u % 24u, m = s / 60u % 60u;
        if (d) {
            snprintf(out, cap, "UP %luD %luH", (unsigned long)d, (unsigned long)h);
        } else if (h) {
            snprintf(out, cap, "UP %luH %luM", (unsigned long)h, (unsigned long)m);
        } else if (m) {
            snprintf(out, cap, "UP %luM", (unsigned long)m);
        } else {
            snprintf(out, cap, "UP %luS", (unsigned long)s);
        }
        return 1;
    }
    case IS_TEMP:
        if (!in->temp_valid) {
            return 0;
        }
        snprintf(out, cap, "%dC", (int)in->temp_c);
        return 1;
    case IS_HEARD:
        if (in->heard == 0) {
            return 0;
        }
        snprintf(out, cap, "HEARD %u", (unsigned)in->heard);
        return 1;
    case IS_CALLER:
        if (!in->last_caller[0]) {
            return 0;
        }
        snprintf(out, cap, "%.16s", in->last_caller);
        return 1;
    case IS_GPS:
        if (!in->gps[0] || in->hide_location) {
            return 0; /* the location stays off the screen unless asked */
        }
        snprintf(out, cap, "%.21s", in->gps); /* 21 cells fit at 1x */
        return 1;
    default:
        return 0;
    }
}

int idle_show_available(const panel_info_t *in, int item)
{
    if (item < 0 || item >= IS_ITEMS) {
        return 0;
    }
    if (item < IS_CLOCK) {
        return 1;
    }
    char t[32];
    return info_text(in, item, t, sizeof(t));
}

static void make_mask(mask_t *m, const panel_info_t *in, int item)
{
    memset(m, 0, sizeof(*m));
    m->cell = 12;
    if (item >= 0 && item < IS_ITEMS) {
        const item_def_t *d = &ITEMS[item];
        if (d->bmp) {
            bmp_mask(m, d->bmp);
        } else if (d->text) {
            text_mask(m, d->text);
        } else {
            char t[32];
            text_mask(m, info_text(in, item, t, sizeof(t)) ? t : "IDLE");
        }
    }
    extents(m);
}

static void draw_mask(uint8_t *fb, const mask_t *m)
{
    for (int x = m->x0; x < m->x1; x++) {
        draw_col(fb, x, m->col[x]);
    }
}

/* ---- schedule ---- */

static uint32_t scroll_w(void)
{
    return (uint32_t)(sizeof(SCROLL_TEXT) - 1) * 12u - 2u;
}

uint32_t idle_show_scroll_ms(void)
{
    uint32_t need = (W + scroll_w()) * 1000u / SCROLL_PX_S;
    uint32_t s = need > IDLE_TR_AT ? (need - IDLE_TR_AT + IDLE_SLOT_MS - 1u) / IDLE_SLOT_MS : 1u;
    if (s < 1u) {
        s = 1u;
    }
    return s * IDLE_SLOT_MS + IDLE_TR_AT;
}

static uint32_t scroll_slots(void)
{
    return (idle_show_scroll_ms() - IDLE_TR_AT) / IDLE_SLOT_MS;
}

static int pick(const panel_info_t *in, uint32_t h, int ex1, int ex2)
{
    int tot = 0;
    for (int i = 0; i < IS_ITEMS; i++) {
        if (i != ex1 && i != ex2 && idle_show_available(in, i)) {
            tot += ITEMS[i].weight;
        }
    }
    if (tot == 0) {
        return IS_IDLE;
    }
    int r = (int)(h % (uint32_t)tot);
    for (int i = 0; i < IS_ITEMS; i++) {
        if (i != ex1 && i != ex2 && idle_show_available(in, i)) {
            if (r < ITEMS[i].weight) {
                return i;
            }
            r -= ITEMS[i].weight;
        }
    }
    return IS_IDLE;
}

static int raw_item(const panel_info_t *in, uint32_t n)
{
    return pick(in, mix(n * 0x9E3779B1u + 0x1D1Eu), -1, -1);
}

static int item_at(const panel_info_t *in, uint32_t n)
{
    if (!(n & 1u)) {
        return raw_item(in, n);
    }
    return pick(in, mix(n * 0x85EBCA77u + 0x0DD5u), raw_item(in, n - 1u), raw_item(in, n + 1u));
}

static uint32_t group_start_scroll(uint32_t n, uint32_t S)
{
    uint32_t G = S + SCROLL_EXTRA;
    uint32_t g = n / G;
    return g * G + 1u + mix(g * 0xC2B2AE35u + 0x5C01u) % (SCROLL_EXTRA - 1u);
}

static int role_of(uint32_t n, uint32_t *p_out)
{
    uint32_t S = scroll_slots();
    uint32_t p = group_start_scroll(n, S);
    if (p_out) {
        *p_out = p;
    }
    if (n + 1u == p) {
        return SLOT_PRE_SCROLL;
    }
    if (n >= p && n < p + S) {
        return SLOT_SCROLL;
    }
    if (n == p + S) {
        return SLOT_SCROLL_TAIL;
    }
    return SLOT_NORMAL;
}

/* PC preview only: a fixed list of slots instead of the hashed schedule. */
static const idle_slot_t *s_script;
static uint32_t s_script_n;

void idle_show_set_script(const idle_slot_t *slots, uint32_t count)
{
    s_script = count ? slots : NULL;
    s_script_n = count;
}

void idle_show_slot(const panel_info_t *in, uint32_t n, idle_slot_t *o)
{
    if (s_script) {
        *o = s_script[n % s_script_n];
        return;
    }
    o->role = role_of(n, &o->scroll_at);
    o->item = (o->role == SLOT_NORMAL || o->role == SLOT_PRE_SCROLL) ? item_at(in, n) : IS_BLANK;
    o->next = item_at(in, n + 1u);
    o->trans = (int)(mix(n * 0x27D4EB2Fu + 0x7A11u) % TR_COUNT);
}

uint32_t idle_show_light_slot(uint32_t n)
{
    uint32_t p;
    int r;
    if (s_script) {
        idle_slot_t o = s_script[n % s_script_n];
        r = o.role;
        p = o.scroll_at;
    } else {
        r = role_of(n, &p);
    }
    return (r == SLOT_SCROLL || r == SLOT_SCROLL_TAIL) ? p - 1u : n;
}

/* ---- sine wave drawing (scroller and SINE SCROLL) ----
 * Column x moves down by env * (1 + 3.25 sin) px: about 6 px peak to peak, wavelength
 * 32 px, the phase running with time; y kept to 12..31 (clear of the lights above and
 * of the breathing line below). */
static void wave_col(uint8_t *fb, int x, uint32_t bits, int env, uint32_t phase)
{
    if (!bits) {
        return;
    }
    int s = SIN64[(uint32_t)(x * 2 + (int)phase) & 63u];
    int num = env * (4 * 127 + 13 * s);
    const int den = 1024 * 127 * 4;
    int dy = num >= 0 ? (num + den / 2) / den : -((-num + den / 2) / den);
    for (int r = 0; r < BAND_H; r++) {
        if (bits & (1u << r)) {
            int y = BAND_Y + r + dy;
            if (y >= 12 && y <= 31) {
                px(fb, x, y);
            }
        }
    }
}

static uint32_t scroll_text_col(uint32_t mi)
{
    uint32_t ci = mi / 12u, gc = (mi % 12u) / 2u;
    if (ci >= sizeof(SCROLL_TEXT) - 1u || gc >= 5u) {
        return 0;
    }
    return col2x(glyph((unsigned char)SCROLL_TEXT[ci])[gc]);
}

void idle_show_draw_scroller(uint8_t *fb, const panel_info_t *in, int from, uint32_t u)
{
    uint32_t total = idle_show_scroll_ms();
    if (u > total) {
        u = total;
    }
    make_mask(&s_a, in, from);
    uint32_t dist = W + scroll_w();
    uint32_t pos = (uint32_t)((uint64_t)u * dist / total);
    int env = u < 400u ? (int)(u * 1024u / 400u) : 1024;
    uint32_t phase = u / 16u;
    for (int x = 0; x < W; x++) {
        uint32_t src = (uint32_t)x + pos;
        uint32_t bits = src < W ? s_a.col[src] : scroll_text_col(src - W);
        wave_col(fb, x, bits, env, phase);
    }
}

/* ---- transitions: A (s_a) to B (s_b), tau 0..IDLE_TR_MS ---- */

/* (a) EXPLODE: 2x2 cells break off left to right into flying dots with 0/1 bits, then
 * home onto the new item's cells. Dots fly over x 0..127, y 13..44 (as the old idle
 * word did). */
#define EX_SPREAD 600
#define EX_HOME   2000
#define EX_BACK   500
#define EX_JITTER 400
#define FLY_Y0 13
#define FLY_Y1 44
#define NCELLS (64 * 9)

static uint16_t s_ca[NCELLS], s_cb[NCELLS];

static int cells(const mask_t *m, uint16_t *out)
{
    int n = 0;
    for (int cx = 0; cx < 64; cx++) {
        uint32_t c = m->col[2 * cx] | m->col[2 * cx + 1];
        for (int cy = 0; cy < 9; cy++) {
            if ((c >> (2 * cy)) & 3u) {
                out[n++] = (uint16_t)(cx * 9 + cy);
            }
        }
    }
    return n;
}

static void draw_cell(uint8_t *fb, const mask_t *m, int cell)
{
    int x = cell / 9 * 2, r = cell % 9 * 2;
    for (int i = 0; i < 2; i++) {
        for (int j = 0; j < 2; j++) {
            if (m->col[x + i] & (1u << (r + j))) {
                bpx(fb, x + i, r + j);
            }
        }
    }
}

static int bounce(int start_q, int speed_q, int dt, int lo, int hi)
{
    int lo_q = lo * 256, span = (hi - lo) * 256;
    if (span <= 0) {
        return lo;
    }
    long long raw = (long long)start_q - lo_q + (long long)speed_q * dt;
    long long per = 2LL * span;
    long long p = raw % per;
    if (p < 0) {
        p += per;
    }
    if (p > span) {
        p = per - p;
    }
    return (int)((lo_q + p) / 256);
}

static int speed_from(uint32_t h)
{
    int s = 10 + (int)(h % 21);
    return (h & 0x100u) ? s : -s;
}

static void tr_explode(uint8_t *fb, int tau)
{
    int na = cells(&s_a, s_ca), nb = cells(&s_b, s_cb);
    int np = maxi(na, nb);
    int ax0 = empty(&s_a) ? 0 : s_a.x0;
    for (int i = 0; i < np; i++) {
        uint32_t h1 = mix((uint32_t)i * 2654435761u + 0x1F3Du);
        uint32_t h2 = mix(h1 ^ 0xA5A5A5A5u);
        int sx, sy, brk, own = 0;
        if (na) {
            int sc = i < na ? s_ca[i] : s_ca[h2 % (uint32_t)na];
            own = i < na;
            sx = sc / 9 * 2;
            sy = BAND_Y + sc % 9 * 2;
            brk = maxi(sx - ax0, 0) * EX_SPREAD / 64 + (int)(h1 % 120u);
            if (tau < brk) {
                if (own) {
                    draw_cell(fb, &s_a, sc);
                }
                continue;
            }
        } else {
            sx = (int)(h1 % 128u);
            sy = FLY_Y0 + (int)(h2 % 20u);
            brk = (int)(h1 % 600u);
            if (tau < brk) {
                continue;
            }
        }
        int dt = tau - brk;
        int fx = bounce(sx * 256, speed_from(h1), dt, 0, W - 1);
        int fy = bounce(sy * 256, speed_from(h2) * 2 / 3, dt, FLY_Y0, FLY_Y1);

        int back = EX_HOME + (int)(h2 % EX_JITTER);
        if (tau >= back) {
            int ex, ey, land = 0, ec = 0;
            if (nb) {
                ec = i < nb ? s_cb[i] : s_cb[h1 % (uint32_t)nb];
                land = i < nb;
                ex = ec / 9 * 2;
                ey = BAND_Y + ec % 9 * 2;
            } else {
                ex = (int)(h2 % 128u);
                ey = FLY_Y0 + (int)(h1 % 20u);
            }
            int k = tau - back;
            if (k >= EX_BACK) {
                if (land) {
                    draw_cell(fb, &s_b, ec);
                }
                continue;
            }
            int u = (EX_BACK - k) * 1024 / EX_BACK;
            int e = 1024 - u * u / 1024;
            fx = fx + (ex - fx) * e / 1024;
            fy = fy + (ey - fy) * e / 1024;
        }
        px(fb, fx, fy);
    }
    /* the bits: born as the word breaks, gone before it is whole again */
    if (tau >= 200 && tau < EX_HOME + 300) {
        int x0 = empty(&s_a) ? 40 : s_a.x0;
        for (int b = 0; b < 7; b++) {
            uint32_t h1 = mix((uint32_t)(b + 101) * 40503u);
            uint32_t h2 = mix(h1 + 0x9E3779B9u);
            int sx = (x0 + (int)(h1 % 46u)) * 256;
            int sy = (14 + (int)(h2 % 10u)) * 256;
            int x = bounce(sx, speed_from(h2) * 3 / 2, tau, 0, W - 3);
            int y = bounce(sy, speed_from(h1), tau, FLY_Y0, FLY_Y1 - 4);
            int one = (int)((h1 >> 3) + (uint32_t)tau / (180u + h2 % 200u)) & 1;
            tiny_bit(fb, x, y, one, 0);
        }
    }
}

/* (b) BLOCKS: 4x4 blocks of the old item flash inverted and go dark in a random order,
 * then the new item's blocks flash on in another. */
static void block(uint8_t *fb, const mask_t *m, int bx, int by, int inv)
{
    for (int i = 0; i < 4; i++) {
        uint32_t c = m->col[bx * 4 + i];
        for (int j = 0; j < 4; j++) {
            int r = by * 4 + j;
            int on = (c >> r) & 1u;
            if (on != inv) {
                bpx(fb, bx * 4 + i, r);
            }
        }
    }
}

static void tr_blocks(uint8_t *fb, int tau)
{
    for (int bx = 0; bx < 32; bx++) {
        uint32_t ca = 0, cb = 0;
        for (int i = 0; i < 4; i++) {
            ca |= s_a.col[bx * 4 + i];
            cb |= s_b.col[bx * 4 + i];
        }
        for (int by = 0; by < 5; by++) {
            uint32_t h = mix((uint32_t)(bx * 5 + by) * 0x9E3779B9u + 0xB10Cu);
            uint32_t h2 = mix(h ^ 0x51u);
            if ((ca >> (by * 4)) & 0xFu) {
                int off = 80 + (int)(h % 1200u);
                if (tau < off - 80) {
                    block(fb, &s_a, bx, by, 0);
                } else if (tau < off) {
                    block(fb, &s_a, bx, by, 1);
                }
            }
            if ((cb >> (by * 4)) & 0xFu) {
                int on = 1400 + (int)(h2 % 1200u);
                if (tau >= on + 80) {
                    block(fb, &s_b, bx, by, 0);
                } else if (tau >= on) {
                    block(fb, &s_b, bx, by, 1);
                }
            }
        }
    }
}

/* (c) SINE SCROLL: the old item leaves to the left, the new comes in from the right, every
 * column on a moving sine that flattens as the new item settles. */
static void tr_sine(uint8_t *fb, int tau)
{
    int e = smooth(tau * 1024 / 2600);
    int s = 128 * e / 1024;
    int env = mini(1024, mini(tau * 1024 / 300, (1024 - e) * 3)); /* full wave until it settles */
    for (int x = 0; x < W; x++) {
        int src = x + s;
        uint32_t bits = src < W ? s_a.col[src] : s_b.col[src - W];
        wave_col(fb, x, bits, env, (uint32_t)tau / 16u);
    }
}

/* (d) SPLIT FLAP: 12 px cells flip like an airport board, top half then bottom half,
 * through two or three random letters before landing on the new item. */
#define FLIP_MS 220

static uint32_t face_col(int face, int nflip, uint32_t h, int x, int cx)
{
    if (face == 0) {
        return mcol(&s_a, x);
    }
    if (face >= nflip) {
        return mcol(&s_b, x);
    }
    int gc = (x - cx) / 2;
    if (gc >= 5) {
        return 0;
    }
    uint32_t letter = 'A' + mix(h + (uint32_t)face * 0x632BE5ABu) % 26u;
    return col2x(glyph(letter)[gc]);
}

static void tr_flap(uint8_t *fb, int tau)
{
    int anchor = !empty(&s_b) ? s_b.x0 : (!empty(&s_a) ? s_a.x0 : 4);
    for (int cx = anchor % 12 - 12; cx < W; cx += 12) {
        uint32_t any = 0;
        for (int i = 0; i < 12; i++) {
            any |= mcol(&s_a, cx + i) | mcol(&s_b, cx + i);
        }
        if (!any) {
            continue;
        }
        uint32_t h = mix((uint32_t)(cx + 32) * 0x2C1B3C6Du + 0xF1A9u);
        int nflip = 3 + (int)(h % 2u);
        int start = (cx + 12) * 500 / 140 + (int)((h >> 8) % 150u);
        int j = tau < start ? -1 : (tau - start) / FLIP_MS;
        for (int i = 0; i < 12; i++) {
            int x = cx + i;
            if (x < 0 || x >= W) {
                continue;
            }
            if (j < 0 || j >= nflip) {
                draw_col(fb, x, j < 0 ? mcol(&s_a, x) : mcol(&s_b, x));
                continue;
            }
            uint32_t cur = face_col(j, nflip, h, x, cx), nxt = face_col(j + 1, nflip, h, x, cx);
            int f = (tau - start) % FLIP_MS * 1024 / FLIP_MS;
            uint32_t out = 0;
            for (int r = 0; r < BAND_H; r++) {
                int bit;
                if (f < 512) {
                    int k = 1024 - 2 * f; /* the old top half folding down */
                    if (r < MID_ROW) {
                        int src = MID_ROW - (MID_ROW - r) * 1024 / maxi(k, 1);
                        bit = src >= 0 ? (int)((cur >> src) & 1u) : (int)((nxt >> r) & 1u);
                    } else {
                        bit = (int)((cur >> r) & 1u);
                    }
                } else {
                    int k = 2 * f - 1024; /* the new bottom half unfolding */
                    if (r < MID_ROW) {
                        bit = (int)((nxt >> r) & 1u);
                    } else {
                        int src = k > 0 ? MID_ROW + (r - MID_ROW) * 1024 / k : BAND_H;
                        bit = src < BAND_H ? (int)((nxt >> src) & 1u) : (int)((cur >> r) & 1u);
                    }
                }
                if (bit) {
                    out |= 1u << r;
                }
            }
            draw_col(fb, x, out);
        }
    }
}

/* (e) TYPEWRITER: a block cursor erases the old item from its end, then types the new
 * one a letter at a time (right to left for Arabic and Urdu) and blinks a moment. */
static void cursor(uint8_t *fb, int x, int w, const mask_t *m)
{
    int r0 = empty(m) ? 2 : m->r0, r1 = empty(m) ? 16 : m->r1;
    for (int i = 0; i < w; i++) {
        for (int r = r0; r < r1; r++) {
            bpx(fb, x + i, r);
        }
    }
}

static void tr_type(uint8_t *fb, int tau)
{
    if (tau < 1000) {
        if (empty(&s_a)) {
            return;
        }
        int cell = s_a.cell, cw = cell - 2;
        int steps = (s_a.x1 - s_a.x0 + cell - 1) / cell;
        int k = mini(tau, 800) * (steps + 1) / 800;
        int cur_on = tau < 800 || ((tau - 800) / 100) % 2 == 0;
        if (!s_a.rtl) {
            int edge = maxi(s_a.x1 - k * cell, s_a.x0);
            for (int x = s_a.x0; x < edge; x++) {
                draw_col(fb, x, s_a.col[x]);
            }
            if (cur_on) {
                cursor(fb, edge + 1, cw, &s_a);
            }
        } else {
            int edge = mini(s_a.x0 + k * cell, s_a.x1);
            for (int x = edge; x < s_a.x1; x++) {
                draw_col(fb, x, s_a.col[x]);
            }
            if (cur_on) {
                cursor(fb, edge - cw - 1, cw, &s_a);
            }
        }
        return;
    }
    int cell = s_b.cell, cw = cell - 2;
    int steps = (s_b.x1 - s_b.x0 + cell - 1) / cell;
    int k = tau >= 2200 ? steps : 1 + (tau - 1000) * steps / 1200;
    int cur_on = tau < 2200 || ((tau - 2200) / 200) % 2 == 0;
    if (!s_b.rtl) {
        int edge = mini(s_b.x0 + k * cell, s_b.x1);
        for (int x = s_b.x0; x < edge; x++) {
            draw_col(fb, x, s_b.col[x]);
        }
        if (cur_on) {
            cursor(fb, edge + 1, cw, &s_b);
        }
    } else {
        int edge = maxi(s_b.x1 - k * cell, s_b.x0);
        for (int x = edge; x < s_b.x1; x++) {
            draw_col(fb, x, s_b.col[x]);
        }
        if (cur_on) {
            cursor(fb, edge - cw - 1, cw, &s_b);
        }
    }
}

/* (f) DRIP MELT: pairs of columns of the old item slide down at random speeds, falling
 * faster as they go, out of the band; the new item shows behind them from the top. */
static void tr_drip(uint8_t *fb, int tau)
{
    for (int x = 0; x < W; x++) {
        uint32_t h = mix((uint32_t)(x / 2) * 0x2545F491u + 0xD819u);
        int st = (int)(h % 700u);
        uint32_t g = 12u + (h >> 8) % 29u;
        uint32_t dt = tau > st ? (uint32_t)(tau - st) : 0u;
        uint32_t drop = dt * dt / 1000u * g / 1000u; /* rows */
        uint32_t a = s_a.col[x], b = s_b.col[x];
        uint32_t out;
        if (drop >= BAND_H) {
            out = b;
        } else {
            out = ((a << drop) & BAND_ALL) | (b & ((1u << drop) - 1u));
        }
        draw_col(fb, x, out);
    }
}

/* (g) GLITCH: frames of horizontal slices from either item, shifted sideways at random,
 * some inverted, the new item winning more slices as it goes; then a snap to the new. */
static void tr_glitch(uint8_t *fb, int tau)
{
    if (tau >= 1500) {
        draw_mask(fb, &s_b);
        return;
    }
    int lo = mini(empty(&s_a) ? s_b.x0 : s_a.x0, s_b.x0) - 6;
    int hi = maxi(empty(&s_a) ? s_b.x1 : s_a.x1, s_b.x1) + 6;
    uint32_t f = (uint32_t)tau / 70u;
    int pnew = tau * 1024 / 1500;
    int r = 0;
    for (uint32_t si = 0; r < BAND_H; si++) {
        uint32_t hh = mix(f * 0x9E3779B9u + si * 0x85EBCA6Bu + 0x611Cu);
        int hgt = 1 + (int)(hh % 4u);
        const mask_t *src = (int)((hh >> 4) % 1024u) < pnew ? &s_b : &s_a;
        int dx = ((hh >> 14) % 4u == 0u) ? 0 : (int)((hh >> 16) % 25u) - 12;
        int inv = (hh >> 26) % 6u == 0u;
        for (int rr = r; rr < r + hgt && rr < BAND_H; rr++) {
            for (int x = lo; x < hi; x++) {
                int bit = (int)((mcol(src, x - dx) >> rr) & 1u);
                if (bit != inv) {
                    bpx(fb, x, rr);
                }
            }
        }
        r += hgt;
    }
}

/* (h) MATRIX RAIN: streams of tiny 0/1 bits fall down 4 px lanes; where a stream has
 * passed, the old item is gone and the new item's pixels are lit. */
static void tr_rain(uint8_t *fb, int tau)
{
    for (int L = 0; L < 32; L++) {
        int x = L * 4;
        uint32_t any = 0;
        for (int i = 0; i < 4; i++) {
            any |= s_a.col[x + i] | s_b.col[x + i];
        }
        uint32_t h = mix((uint32_t)L * 0x7FEB352Du + 0x3A7u);
        int st = (int)((h >> 4) % 1400u);
        int v = 25 + (int)((h >> 12) % 30u); /* rows per second */
        int head = tau < st ? -100 : (tau - st) * v / 1000 - 6;
        uint32_t below = head <= 0 ? 0u : (head >= BAND_H ? BAND_ALL : (1u << head) - 1u);
        for (int i = 0; i < 4; i++) {
            draw_col(fb, x + i, (s_b.col[x + i] & below) | (s_a.col[x + i] & ~below));
        }
        if (!any && (h & 1u)) {
            continue; /* a dry lane: no stream this time */
        }
        if (head > -100) {
            int d = (int)((h >> 20) + (uint32_t)tau / 120u) & 1;
            tiny_bit(fb, x, BAND_Y + head - 4, d, 1);
            tiny_bit(fb, x, BAND_Y + head - 11, !d, 1);
        }
    }
}

/* (i) STARFIELD WARP: stars stream out from the centre; the old item shrinks into the
 * centre and is gone, the new one grows out of it (nearest neighbour). */
static void draw_scaled(uint8_t *fb, const mask_t *m, int s, int sx_only)
{
    if (s <= 24) {
        return;
    }
    for (int x = m->x0; x < m->x1; x++) {
        uint32_t c = m->col[x];
        for (int r = 0; c >> r; r++) {
            if ((c >> r) & 1u) {
                int dx = sx_only ? x : CX + (x - CX) * s / 1024;
                int dr = MID_ROW + (r - MID_ROW) * s / 1024;
                bpx(fb, dx, dr);
            }
        }
    }
}

static void tr_warp(uint8_t *fb, int tau)
{
    if (tau < 1100) {
        draw_scaled(fb, &s_a, 1024 - smooth(tau * 1024 / 1100), 0);
    } else if (tau >= 1500) {
        draw_scaled(fb, &s_b, tau >= 2700 ? 1024 : smooth((tau - 1500) * 1024 / 1200), 0);
    }
    for (int i = 0; i < 64; i++) {
        uint32_t h = mix((uint32_t)i * 0x165667B1u + 0x57A5u);
        int birth = (int)(h % 2100u), life = 450 + (int)((h >> 12) % 400u); /* gone by 2950 */
        if (tau < birth || tau >= birth + life) {
            continue;
        }
        int dxv = (int)((h >> 4) % 141u) - 70;
        int dyv = (int)((h >> 20) % 25u) - 12;
        int u = (tau - birth) * 1024 / life;
        int f = u * u / 1024;
        int x = CX + dxv * f / 1024;
        int r = MID_ROW + dyv * f / 1024;
        bpx(fb, x, r);
        if (f > 300) { /* fast stars streak */
            bpx(fb, x - (dxv > 0 ? 1 : -1), r);
        }
    }
}

/* (j) CRT POWER: the old item squashes into a line, the line shrinks to a dot that
 * lingers, then the same backwards into the new item. */
static void squash(uint8_t *fb, const mask_t *m, int sy)
{
    for (int x = m->x0; x < m->x1; x++) {
        uint32_t c = m->col[x];
        for (int r = 0; c >> r; r++) {
            if ((c >> r) & 1u) {
                bpx(fb, x, MID_ROW + (r - MID_ROW) * sy / 1024);
            }
        }
    }
}

static void hline(uint8_t *fb, int x0, int x1, int r)
{
    for (int x = x0; x < x1; x++) {
        bpx(fb, x, r);
    }
}

static void tr_crt(uint8_t *fb, int tau)
{
    int ahw = (s_a.x1 - s_a.x0) / 2, bhw = (s_b.x1 - s_b.x0) / 2;
    if (tau < 450) {
        int e = smooth(tau * 1024 / 450);
        squash(fb, &s_a, 1024 - e);
        if (e > 800 && ahw > 0) {
            hline(fb, s_a.x0, s_a.x1, MID_ROW);
        }
    } else if (tau < 800) {
        int hw = ahw * (1024 - smooth((tau - 450) * 1024 / 350)) / 1024;
        hline(fb, CX - hw, CX + hw + 1, MID_ROW);
    } else if (tau < 1300) {
        hline(fb, CX - 1, CX + 1, MID_ROW); /* the dot lingers */
        if (tau < 1200) {
            hline(fb, CX - 1, CX + 1, MID_ROW - 1);
        }
    } else if (tau < 1650) {
        int hw = bhw * smooth((tau - 1300) * 1024 / 350) / 1024;
        hline(fb, CX - hw, CX + hw + 1, MID_ROW);
    } else if (tau < 2100) {
        int e = smooth((tau - 1650) * 1024 / 450);
        squash(fb, &s_b, e);
        if (e < 224) {
            hline(fb, s_b.x0, s_b.x1, MID_ROW);
        }
    } else {
        draw_mask(fb, &s_b);
    }
}

/* (k) VENETIAN BLINDS: 2 px slats close over the old item top to bottom (a lit line and
 * a dark line each), then open the same way onto the new one. */
static void tr_blinds(uint8_t *fb, int tau)
{
    int lo = mini(empty(&s_a) ? s_b.x0 : s_a.x0, s_b.x0) - 2;
    int hi = maxi(empty(&s_a) ? s_b.x1 : s_a.x1, s_b.x1) + 2;
    lo = maxi(lo, 0);
    hi = mini(hi, W);
    for (int i = 0; i < BAND_H / 2; i++) {
        int pc = clampi((tau - i * 70) * 1024 / 300, 0, 1024);
        int po = clampi((tau - 1400 - i * 70) * 1024 / 300, 0, 1024);
        const mask_t *m;
        int top, bot; /* 0 dark, 1 lit, 2 item */
        if (po > 0) {
            m = &s_b;
            top = po < 683 ? 1 : 2;
            bot = po < 341 ? 0 : 2;
        } else {
            m = &s_a;
            top = pc < 341 ? 2 : 1;
            bot = pc < 683 ? 2 : 0;
        }
        for (int x = lo; x < hi; x++) {
            uint32_t c = m->col[x];
            int rt = 2 * i, rb = 2 * i + 1;
            if (top == 1 || (top == 2 && ((c >> rt) & 1u))) {
                bpx(fb, x, rt);
            }
            if (bot == 2 && ((c >> rb) & 1u)) {
                bpx(fb, x, rb);
            }
        }
    }
}

/* (l) PIXELATE: the old item coarsens to 2x2, 4x4, 8x8 block averages, swaps for the
 * new one at 8x8 and sharpens back. A block is lit when 30 % of it is. */
static void draw_pixelated(uint8_t *fb, const mask_t *m, int b)
{
    if (b <= 1) {
        draw_mask(fb, m);
        return;
    }
    for (int bx = 0; bx < W / b; bx++) {
        for (int br = 0; br * b < BAND_H; br++) {
            int n = 0, area = 0;
            for (int i = 0; i < b; i++) {
                for (int j = 0; j < b && br * b + j < BAND_H; j++) {
                    area++;
                    n += (int)((m->col[bx * b + i] >> (br * b + j)) & 1u);
                }
            }
            if (n > 0 && n * 10 >= area * 3) {
                for (int i = 0; i < b; i++) {
                    for (int j = 0; j < b; j++) {
                        bpx(fb, bx * b + i, br * b + j);
                    }
                }
            }
        }
    }
}

static void tr_pixel(uint8_t *fb, int tau)
{
    static const int AT[8] = {180, 380, 600, 900, 1200, 1420, 1640, 1 << 30};
    static const int B[8] = {1, 2, 4, 8, 8, 4, 2, 1};
    int i = 0;
    while (tau >= AT[i]) {
        i++;
    }
    draw_pixelated(fb, i < 4 ? &s_a : &s_b, B[i]);
}

void idle_show_draw_trans(uint8_t *fb, const panel_info_t *in, int from, int to, int trans, uint32_t tau)
{
    make_mask(&s_a, in, from);
    make_mask(&s_b, in, to);
    if (tau >= IDLE_TR_MS) {
        draw_mask(fb, &s_b);
        return;
    }
    int t = (int)tau;
    switch (trans) {
    case TR_EXPLODE: tr_explode(fb, t); break;
    case TR_BLOCKS: tr_blocks(fb, t); break;
    case TR_SINE: tr_sine(fb, t); break;
    case TR_FLAP: tr_flap(fb, t); break;
    case TR_TYPE: tr_type(fb, t); break;
    case TR_DRIP: tr_drip(fb, t); break;
    case TR_GLITCH: tr_glitch(fb, t); break;
    case TR_RAIN: tr_rain(fb, t); break;
    case TR_WARP: tr_warp(fb, t); break;
    case TR_CRT: tr_crt(fb, t); break;
    case TR_BLINDS: tr_blinds(fb, t); break;
    default: tr_pixel(fb, t); break;
    }
}

void idle_show_draw_item(uint8_t *fb, const panel_info_t *in, int item)
{
    make_mask(&s_a, in, item);
    draw_mask(fb, &s_a);
}

void idle_show_draw(uint8_t *fb, const panel_info_t *in, uint32_t now_ms)
{
    uint32_t n = now_ms / IDLE_SLOT_MS, t = now_ms % IDLE_SLOT_MS;
    idle_slot_t s;
    idle_show_slot(in, n, &s);
    if (s.role == SLOT_SCROLL || (s.role == SLOT_SCROLL_TAIL && t < IDLE_TR_AT)) {
        int from;
        if (s_script) {
            from = s_script[(s.scroll_at - 1u) % s_script_n].item;
        } else {
            from = item_at(in, s.scroll_at - 1u);
        }
        idle_show_draw_scroller(fb, in, from, (n - s.scroll_at) * IDLE_SLOT_MS + t);
        return;
    }
    if (s.role == SLOT_PRE_SCROLL || t < IDLE_TR_AT) {
        idle_show_draw_item(fb, in, s.item);
        return;
    }
    idle_show_draw_trans(fb, in, s.item, s.next, s.trans, t - IDLE_TR_AT);
}

const char *idle_show_item_name(int item)
{
    return (item >= 0 && item < IS_ITEMS) ? ITEMS[item].name : "BLANK";
}

const char *idle_show_trans_name(int trans)
{
    return (trans >= 0 && trans < TR_COUNT) ? TRANS_NAME[trans] : "?";
}
