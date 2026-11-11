/* PC tests of the picker's menu state machine (multimode/picker/src/picker_menu.c).
 * Time is simulated in milliseconds; the button level is fed on every poll. */
#include <stdio.h>
#include <string.h>
#include "picker_menu.h"
#include "tap.h"

static const bool ALL[PK_MODES] = {true, true, true, true};

typedef struct {
    pk_state_t s;
    uint32_t now;
} rig_t;

static void rig_init(rig_t *r, int last, const bool valid[PK_MODES], bool down, bool crashed, uint32_t t0)
{
    r->now = t0;
    pk_init(&r->s, last, valid, down, crashed, t0);
}

/* Poll every 10 ms for ms, with the button held as given. */
static void hold(rig_t *r, bool down, uint32_t ms)
{
    for (uint32_t t = 0; t < ms; t += 10) {
        r->now += 10;
        pk_update(&r->s, down, r->now);
    }
}

/* Poll once (a single edge). */
static void poll(rig_t *r, bool down)
{
    r->now += 10;
    pk_update(&r->s, down, r->now);
}

/* A short tap: down 100 ms, up 100 ms. */
static void tap(rig_t *r)
{
    hold(r, true, 100);
    hold(r, false, 100);
}

/* Get to the list by pressing during the logo and releasing. */
static void open_list(rig_t *r)
{
    hold(r, true, 100);
    hold(r, false, 100);
    CHECK_EQ(r->s.phase, PK_LIST);
}

static void names_and_slots(void)
{
    CHECK(strcmp(pk_mode_name(PK_LFD3K), "LFD3K-SM") == 0);
    CHECK(strcmp(pk_mode_name(PK_SIMIAN), "SIMIAN RADAR") == 0);
    CHECK(strcmp(pk_mode_name(PK_RNODE), "RNODE") == 0);
    CHECK(strcmp(pk_mode_name(PK_MESH), "MESH") == 0);
    CHECK(strcmp(pk_mode_name(-1), "?") == 0);
    CHECK(strcmp(pk_mode_name(PK_MODES), "?") == 0);
    /* ota_1, ota_2, ota_3, ota_0 as in the partition table. */
    CHECK_EQ(pk_mode_ota(PK_LFD3K), 1);
    CHECK_EQ(pk_mode_ota(PK_SIMIAN), 2);
    CHECK_EQ(pk_mode_ota(PK_RNODE), 3);
    CHECK_EQ(pk_mode_ota(PK_MESH), 0);
    CHECK_EQ(pk_mode_ota(-1), -1);
    CHECK_EQ(pk_mode_ota(PK_MODES), -1);
}

static void logo_runs_5s_then_starts_last_mode(void)
{
    rig_t r;
    rig_init(&r, PK_RNODE, ALL, false, false, 1000);
    CHECK_EQ(r.s.phase, PK_LOGO);
    hold(&r, false, 4980);
    CHECK_EQ(r.s.phase, PK_LOGO); /* 4980 ms in */
    poll(&r, false);              /* 4990 */
    CHECK_EQ(r.s.phase, PK_LOGO);
    poll(&r, false);              /* 5000 */
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_RNODE);
    /* Launch is terminal: nothing changes it. */
    hold(&r, true, 2000);
    hold(&r, false, 2000);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_RNODE);
}

static void first_boot_with_no_last_mode_picks_lfd3k(void)
{
    rig_t r;
    rig_init(&r, -1, ALL, false, false, 0);
    CHECK_EQ(r.s.sel, PK_LFD3K);
    CHECK_EQ(r.s.last, PK_LFD3K);
    hold(&r, false, PK_LOGO_MS);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_LFD3K);
    /* A stored value out of range is treated as none. */
    rig_init(&r, 7, ALL, false, false, 0);
    CHECK_EQ(r.s.last, PK_LFD3K);
    rig_init(&r, 200, ALL, false, false, 0);
    CHECK_EQ(r.s.last, PK_LFD3K);
}

static void picker_skips_empty_slot(void)
{
    const bool no_simian[PK_MODES] = {true, false, true, true};
    rig_t r;
    rig_init(&r, PK_LFD3K, no_simian, false, false, 0);
    open_list(&r);
    CHECK_EQ(r.s.sel, PK_LFD3K);
    tap(&r);
    CHECK_EQ(r.s.sel, PK_SIMIAN); /* the cursor may rest on it ... */

    /* ... but a long press on it shows NO IMAGE and never launches, not
     * during the hold and not on the release. */
    hold(&r, true, PK_LONG_MS + 500);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(pk_noimg(&r.s, r.now));
    hold(&r, false, 100);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_SIMIAN); /* the release did not advance either */

    /* The message times out. */
    hold(&r, false, PK_NOIMG_MS + 100);
    CHECK(!pk_noimg(&r.s, r.now));

    /* 15 s with no press on an empty slot: NO IMAGE, never a launch. */
    hold(&r, false, PK_IDLE_MS + 100);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);
    for (int i = 0; i < 5; i++) {
        hold(&r, false, PK_IDLE_MS);
        CHECK_EQ(r.s.phase, PK_LIST);
    }

    /* A good slot still starts. */
    tap(&r); /* RNODE */
    CHECK_EQ(r.s.sel, PK_RNODE);
    hold(&r, true, PK_LONG_MS + 20);
    hold(&r, false, 20);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_RNODE);

    /* Every slot empty: nothing can ever launch. */
    const bool none[PK_MODES] = {false, false, false, false};
    rig_init(&r, PK_MESH, none, false, false, 0);
    hold(&r, false, PK_LOGO_MS + 50);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(pk_noimg(&r.s, r.now));
    for (int i = 0; i < 6; i++) {
        tap(&r);
        hold(&r, true, PK_LONG_MS + 100);
        hold(&r, false, 100);
        hold(&r, false, PK_IDLE_MS + 100);
        CHECK_EQ(r.s.phase, PK_LIST);
    }
}

static void last_mode_slot_empty_opens_list_with_no_image(void)
{
    const bool no_rnode[PK_MODES] = {true, true, false, true};
    rig_t r;
    rig_init(&r, PK_RNODE, no_rnode, false, false, 0);
    hold(&r, false, PK_LOGO_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_RNODE); /* the marker stays on the last mode */
    CHECK(pk_noimg(&r.s, r.now));
    CHECK(r.s.launch < 0);
    CHECK(!pk_crash_note(&r.s));
    /* The idle timer starts from the list: after 15 s it tries the highlighted
     * (empty) mode, which only repeats NO IMAGE. */
    hold(&r, false, PK_IDLE_MS + 20);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(pk_noimg(&r.s, r.now));
    /* Move on to a mode with an image and start it. */
    tap(&r); /* MESH */
    hold(&r, true, PK_LONG_MS + 20);
    hold(&r, false, 20);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_MESH);
}

static void long_press_arms_and_launches_only_on_release(void)
{
    rig_t r;
    rig_init(&r, PK_LFD3K, ALL, false, false, 0);
    open_list(&r);
    r.s.sel = PK_SIMIAN;

    /* Held past 0.8 s: still in the list, armed, nothing launched. */
    hold(&r, true, PK_LONG_MS + 200);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.hold_fired);
    CHECK(r.s.launch < 0);
    /* Held for a long time more: still not launched. */
    hold(&r, true, 10000);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);
    /* The release launches the highlighted mode. */
    poll(&r, false);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_SIMIAN);

    /* A press just short of 0.8 s is a tap: it moves the cursor and starts nothing. */
    rig_init(&r, PK_LFD3K, ALL, false, false, 0);
    open_list(&r);
    hold(&r, true, PK_LONG_MS - 20);
    CHECK(!r.s.hold_fired);
    hold(&r, false, 20);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_SIMIAN);

    /* Taps wrap around the four modes. */
    for (int i = 0; i < 3; i++) {
        tap(&r);
    }
    CHECK_EQ(r.s.sel, PK_LFD3K);
    CHECK_EQ(r.s.phase, PK_LIST);
}

static void press_that_opened_the_list_is_not_a_tap(void)
{
    rig_t r;
    /* PRG held at power on: counts as a press during the logo, opens the list,
     * its release is ignored, so a long hold at boot never starts a mode. */
    rig_init(&r, PK_RNODE, ALL, true, false, 0);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, true, 3000);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, false, 100);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_RNODE);
    CHECK(r.s.launch < 0);

    /* A press during the logo: list opens on the press, the release is not a tap. */
    rig_init(&r, PK_SIMIAN, ALL, false, false, 0);
    hold(&r, false, 1000);
    poll(&r, true);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_SIMIAN);
    hold(&r, true, 2000);
    CHECK_EQ(r.s.phase, PK_LIST); /* held through 0.8 s but not armed */
    poll(&r, false);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_SIMIAN);
}

static void idle_start_after_15s(void)
{
    rig_t r;
    rig_init(&r, PK_SIMIAN, ALL, false, false, 0);
    open_list(&r);
    hold(&r, true, 100);
    poll(&r, false); /* the release: RNODE, idle timer restarts here */
    uint32_t last_edge = r.now;
    CHECK_EQ(r.s.sel, PK_RNODE);
    /* 14.99 s after the release: still waiting. */
    while (r.now - last_edge < PK_IDLE_MS - 10) {
        poll(&r, false);
    }
    CHECK_EQ(r.s.phase, PK_LIST);
    poll(&r, false); /* exactly 15 s since the release */
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_RNODE); /* the highlighted one */

    /* A press resets the timer. */
    rig_init(&r, PK_LFD3K, ALL, false, false, 0);
    open_list(&r);
    hold(&r, false, 10000);
    tap(&r);
    hold(&r, false, 10000);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, false, 5100);
    CHECK_EQ(r.s.phase, PK_LAUNCH);

    /* A held button does not count as idle. */
    rig_init(&r, PK_LFD3K, ALL, false, false, 0);
    open_list(&r);
    r.s.sel = PK_MESH;
    hold(&r, true, 100);
    hold(&r, true, PK_LONG_MS + 100); /* hold fired, waiting for release */
    hold(&r, true, PK_IDLE_MS * 2);
    CHECK_EQ(r.s.phase, PK_LIST);
}

static void crash_boot_opens_list_no_idle_start(void)
{
    rig_t r;
    rig_init(&r, PK_SIMIAN, ALL, false, true, 0);
    CHECK_EQ(r.s.phase, PK_LOGO);
    hold(&r, false, PK_LOGO_MS);
    /* Never restarts the mode that just crashed. */
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);
    CHECK_EQ(r.s.sel, PK_SIMIAN);
    CHECK(pk_crash_note(&r.s)); /* LAST MODE CRASHED */
    CHECK(!pk_noimg(&r.s, r.now));
    /* No idle start, however long we wait. */
    hold(&r, false, 10 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);
    CHECK(pk_crash_note(&r.s));

    /* A press hides the line; the idle start stays off for the boot. */
    tap(&r);
    CHECK(!pk_crash_note(&r.s));
    hold(&r, false, 10 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);

    /* The user can still pick a mode by hand. */
    hold(&r, true, PK_LONG_MS + 20);
    hold(&r, false, 20);
    CHECK_EQ(r.s.phase, PK_LAUNCH);

    /* A press during the logo of a crash boot: same, no auto start. */
    rig_init(&r, PK_MESH, ALL, false, true, 0);
    hold(&r, false, 500);
    poll(&r, true);
    poll(&r, false);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, false, 10 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);

    /* PRG held through a crash boot. */
    rig_init(&r, PK_MESH, ALL, true, true, 0);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, false, 10 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);

    /* Crash with the last mode's slot empty: list, crash line, no NO IMAGE needed. */
    const bool no_mesh[PK_MODES] = {true, true, true, false};
    rig_init(&r, PK_MESH, no_mesh, false, true, 0);
    hold(&r, false, PK_LOGO_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(pk_crash_note(&r.s));
    hold(&r, false, 5 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);

    /* A normal (not crashed) boot never shows the line. */
    rig_init(&r, PK_MESH, ALL, false, false, 0);
    open_list(&r);
    CHECK(!pk_crash_note(&r.s));
}

static void launch_failed_marks_slot_empty(void)
{
    rig_t r;
    rig_init(&r, PK_SIMIAN, ALL, false, false, 0);
    hold(&r, false, PK_LOGO_MS);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    CHECK_EQ(r.s.launch, PK_SIMIAN);
    /* The picker's full verify failed: back to the list with NO IMAGE, slot
     * dead, and it cannot be started again. */
    pk_launch_failed(&r.s, r.now);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK_EQ(r.s.sel, PK_SIMIAN);
    CHECK(!r.s.valid[PK_SIMIAN]);
    CHECK(pk_noimg(&r.s, r.now));
    CHECK(r.s.launch < 0);
    hold(&r, false, PK_IDLE_MS + 100); /* idle start on a dead slot */
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, true, PK_LONG_MS + 100);
    hold(&r, false, 100);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);

    /* Failed while PRG is still held: the release must not start anything. */
    rig_init(&r, PK_LFD3K, ALL, false, false, 0);
    open_list(&r);
    hold(&r, true, PK_LONG_MS + 50);
    hold(&r, false, 10);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    pk_launch_failed(&r.s, r.now);
    hold(&r, true, 500);
    hold(&r, false, 100);
    CHECK_EQ(r.s.phase, PK_LIST);
    CHECK(r.s.launch < 0);
    /* Calling it with no launch pending does no harm. */
    pk_launch_failed(&r.s, r.now);
    CHECK_EQ(r.s.phase, PK_LIST);
}

static void timer_wraps_after_49_days(void)
{
    rig_t r;
    rig_init(&r, PK_LFD3K, ALL, false, false, 0xFFFFFC00u);
    hold(&r, false, PK_LOGO_MS - 20);
    CHECK_EQ(r.s.phase, PK_LOGO);
    hold(&r, false, 20);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
    rig_init(&r, PK_LFD3K, ALL, false, true, 0xFFFFF000u);
    hold(&r, false, PK_LOGO_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    hold(&r, false, 20 * PK_IDLE_MS);
    CHECK_EQ(r.s.phase, PK_LIST);
    rig_init(&r, PK_LFD3K, ALL, true, false, 0xFFFFE000u);
    hold(&r, false, 100);
    hold(&r, false, PK_IDLE_MS + 100);
    CHECK_EQ(r.s.phase, PK_LAUNCH);
}

int main(void)
{
    RUN(names_and_slots);
    RUN(logo_runs_5s_then_starts_last_mode);
    RUN(first_boot_with_no_last_mode_picks_lfd3k);
    RUN(picker_skips_empty_slot);
    RUN(last_mode_slot_empty_opens_list_with_no_image);
    RUN(long_press_arms_and_launches_only_on_release);
    RUN(press_that_opened_the_list_is_not_a_tap);
    RUN(idle_start_after_15s);
    RUN(crash_boot_opens_list_no_idle_start);
    RUN(launch_failed_marks_slot_empty);
    RUN(timer_wraps_after_49_days);
    TAP_DONE();
}
