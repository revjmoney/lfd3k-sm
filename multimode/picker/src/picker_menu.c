#include "picker_menu.h"

static const char *const NAMES[PK_MODES] = {"LFD3K-SM", "SIMIAN RADAR", "RNODE", "MESH"};
static const int OTA[PK_MODES] = {1, 2, 3, 0}; /* ota_1, ota_2, ota_3, ota_0 */

const char *pk_mode_name(int mode)
{
    return (mode >= 0 && mode < PK_MODES) ? NAMES[mode] : "?";
}

int pk_mode_ota(int mode)
{
    return (mode >= 0 && mode < PK_MODES) ? OTA[mode] : -1;
}

static void to_list(pk_state_t *s, uint32_t now)
{
    s->phase = PK_LIST;
    s->phase_ms = now;
    s->input_ms = now;
    s->armed = false; /* a press already held (the one that opened the list) never counts */
    s->hold_fired = false;
}

static void no_image(pk_state_t *s, uint32_t now)
{
    s->noimg_until = now + PK_NOIMG_MS;
    if (s->noimg_until == 0) {
        s->noimg_until = 1;
    }
}

/* Start a mode if its slot holds an image; otherwise NO IMAGE and stay. */
static void start(pk_state_t *s, int mode, uint32_t now)
{
    if (s->valid[mode]) {
        s->phase = PK_LAUNCH;
        s->phase_ms = now;
        s->launch = mode;
    } else {
        no_image(s, now);
    }
}

void pk_init(pk_state_t *s, int last, const bool valid[PK_MODES], bool down, bool crashed,
             uint32_t now_ms)
{
    if (last < 0 || last >= PK_MODES) {
        last = PK_LFD3K;
    }
    s->phase = PK_LOGO;
    s->phase_ms = now_ms;
    s->input_ms = now_ms;
    s->sel = (uint8_t)last;
    s->last = (uint8_t)last;
    for (int i = 0; i < PK_MODES; i++) {
        s->valid[i] = valid[i];
    }
    s->down = down;
    s->armed = false;
    s->hold_fired = false;
    s->down_ms = now_ms;
    s->noimg_until = 0;
    s->launch = -1;
    s->crashed = crashed;
    s->touched = false;
    if (down) {
        to_list(s, now_ms);
    }
}

void pk_update(pk_state_t *s, bool down, uint32_t now)
{
    bool pressed = down && !s->down;
    bool released = !down && s->down;
    s->down = down;
    if (s->noimg_until && (int32_t)(now - s->noimg_until) >= 0) {
        s->noimg_until = 0;
    }

    switch (s->phase) {
    case PK_LOGO:
        if (pressed) {
            to_list(s, now);
            s->sel = s->last;
        } else if (now - s->phase_ms >= PK_LOGO_MS) {
            s->sel = s->last;
            if (s->crashed) {
                to_list(s, now); /* never restart a mode that just crashed */
            } else if (s->valid[s->last]) {
                start(s, s->last, now);
            } else {
                to_list(s, now);
                no_image(s, now);
            }
        }
        break;

    case PK_LIST:
        if (pressed) {
            s->armed = true;
            s->hold_fired = false;
            s->down_ms = now;
            s->input_ms = now;
            s->touched = true;
        } else if (released) {
            if (s->armed) {
                if (s->hold_fired) {
                    start(s, s->sel, now); /* launch only once PRG is up */
                } else {
                    s->sel = (uint8_t)((s->sel + 1) % PK_MODES);
                }
            }
            s->armed = false;
            s->hold_fired = false;
            s->input_ms = now;
        } else if (down && s->armed && !s->hold_fired && now - s->down_ms >= PK_LONG_MS) {
            if (s->valid[s->sel]) {
                s->hold_fired = true; /* wait for the release */
            } else {
                no_image(s, now);
                s->armed = false; /* the release of this press does nothing */
            }
        } else if (!down && !s->crashed && now - s->input_ms >= PK_IDLE_MS) {
            s->input_ms = now; /* restart the idle timer whatever happens */
            start(s, s->sel, now);
        }
        break;

    case PK_LAUNCH:
        break;
    }
}

void pk_launch_failed(pk_state_t *s, uint32_t now_ms)
{
    int m = s->launch;
    if (m >= 0 && m < PK_MODES) {
        s->valid[m] = false;
        s->sel = (uint8_t)m;
    }
    s->launch = -1;
    to_list(s, now_ms); /* a press still held must be released first (armed off) */
    no_image(s, now_ms);
}

bool pk_crash_note(const pk_state_t *s)
{
    return s->crashed && !s->touched;
}

bool pk_noimg(const pk_state_t *s, uint32_t now_ms)
{
    return s->noimg_until != 0 && (int32_t)(now_ms - s->noimg_until) < 0;
}
