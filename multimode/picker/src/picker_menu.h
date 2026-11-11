/* Picker menu logic: pure state machine, no ESP headers, PC-testable.
 * Feed it the debounced PRG level and the time on every poll; read back the
 * phase. When the phase becomes PK_LAUNCH the caller starts s.launch. */
#ifndef PICKER_MENU_H
#define PICKER_MENU_H
#include <stdbool.h>
#include <stdint.h>

#define PK_MODES     4
#define PK_LOGO_MS   5000u  /* splash length */
#define PK_LONG_MS   800u   /* hold this long to start */
#define PK_IDLE_MS   15000u /* no press in the list: start the highlighted mode */
#define PK_NOIMG_MS  1500u  /* NO IMAGE message time */

/* Menu order. */
enum { PK_LFD3K = 0, PK_SIMIAN = 1, PK_RNODE = 2, PK_MESH = 3 };

/* Display name and app slot (ota subtype index) of a mode. */
const char *pk_mode_name(int mode);
int pk_mode_ota(int mode);

typedef enum { PK_LOGO, PK_LIST, PK_LAUNCH } pk_phase_t;

typedef struct {
    pk_phase_t phase;
    uint32_t phase_ms;       /* when the phase began */
    uint32_t input_ms;       /* last button edge in the list (idle timer) */
    uint8_t sel;             /* highlighted mode */
    uint8_t last;            /* last used mode (marked) */
    bool valid[PK_MODES];    /* slot holds an app image */
    bool down;               /* debounced button level, true = pressed */
    bool armed;              /* the current press began inside the list */
    bool hold_fired;         /* the current press was held 0.8 s: start on release */
    uint32_t down_ms;        /* when the current press began */
    uint32_t noimg_until;    /* show NO IMAGE until this time (0 = off) */
    int launch;              /* mode to start while phase == PK_LAUNCH */
    bool crashed;            /* this boot follows a mode crash (panic, watchdog,
                                brownout): no auto start, no idle start */
    bool touched;            /* a press happened in the list (hides the crash line) */
} pk_state_t;

/* last: last used mode, or -1 when none was ever stored (then LFD3K-SM).
 * down: button level at start; a press already held counts as a press during
 * the logo (opens the list, its release is ignored).
 * crashed: the previous mode ended in a panic, watchdog or brownout reset.
 * Then the logo ends in the list (LAST MODE CRASHED) instead of starting the
 * last mode, and the 15 s idle start is off for this boot. */
void pk_init(pk_state_t *s, int last, const bool valid[PK_MODES], bool down, bool crashed,
             uint32_t now_ms);

/* One poll: debounced level and time. */
void pk_update(pk_state_t *s, bool down, uint32_t now_ms);

/* The caller could not start s->launch (no image after all): mark the slot
 * empty and go back to the list with NO IMAGE. */
void pk_launch_failed(pk_state_t *s, uint32_t now_ms);

/* True while the LAST MODE CRASHED line should show. */
bool pk_crash_note(const pk_state_t *s);

/* True while the NO IMAGE message is up. */
bool pk_noimg(const pk_state_t *s, uint32_t now_ms);

#endif
