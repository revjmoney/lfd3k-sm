# Voice part 2 fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lock modem settings over Wi-Fi voice sessions, decide `--cap-tier` wrapping when a line is sent, and time out a dead session that holds a ControlOut.

**Architecture:** Core (`core/src/voice/host.rs`, `core/src/modem.rs`) owns the read-only rule and the timeout; the PC app (`apps/pc`) reuses the core's rule to grey out its screens; the firmware needs no change (it never grants settings).

**Tech Stack:** Rust no_std core, eframe PC app.

**Spec:** `docs/superpowers/specs/2026-11-15-voice-2-fixes-design.md`

## Global Constraints

- cargo runs from PowerShell with `LFD3K_CLANG_CL` set; `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all clean.
- No em dashes or double hyphens in prose; no AI attribution in commits.
- Core stays no_std, no heap, no panics on input (index with `get`).

## Review Focus

- A TCP app that sends `ats21=4` lower case or with spaces: refused (the parser decides, not a string match).
- `ATS21?` on TCP: allowed (GetReg).
- A live app on USB whose ControlOut waits more than 5 s on a slow tier but keeps sending Heartbeat: stays open.
- The same held bytes offered again every pump with nothing new: session closes after 5 s.
- AT&W typed before the `--cap-tier` S21 read answers: saved profile keeps the old S21.

---

### Task 1: Core read-only AtLine for sessions 1 to 3

**Files:** Modify `core/src/voice/host.rs` (Session `full: bool`, `at_read_only`, `err::AT_LOCKED = 7`, `voice_grant_settings`), `core/src/modem.rs` (`vhp_at_line`), `sim/src/multi.rs` (grant on open), `voice/src/client.rs` (error text). Test: `core/tests/voice_lock.rs` (new) or the existing voice host test file.

**Interfaces produced:** `pub fn at_read_only(cmd: &AtCmd<'_>) -> bool`; `pub mod err { pub const AT_LOCKED: u8 = 7; }`; `impl Modem { pub fn voice_grant_settings(&mut self, session: u8) -> bool }` (false if not open).

- [ ] Tests: on a session opened with `voice_open` and no grant, AtLine `ATS30=5`, `ats21=4`, `AT&W`, `ATZ`, `AT+WSTA=X,Y`, `AT+DIR=+V:REV`, `AT+DIR=0`, `ATE0`, `ATH`, `AT+NAME=X`, `garbage` each get Error(AT_LINE, AT_LOCKED) and S30 is unchanged; `AT&V`, `ATI`, `ATS21?`, `AT+FWD?`, `AT+DIR?`, `AT+SCAN` answer; after `voice_grant_settings(1)` `ATS30=5` works; session 0 after AT+VOICE can `ATS30=5`; a reopened session is read-only again.
- [ ] Implement; `Multi` grants every session it opens; existing tests stay green.
- [ ] Commit.

### Task 2: Hold timeout

**Files:** Modify `core/src/voice/host.rs` (`vhp_tick`, `vhp_held_frame`, `vhp_flush_hold`). Test in the same voice host test file.

- [ ] Tests: (a) session 0 holds a ControlOut on a connected voice call whose link takes nothing, the same bytes are offered every 100 ms and nothing new: closed after 5 s, the call hung up and the host stream back to AT commands; (b) same, but a Heartbeat frame arrives every 1.5 s behind the held one: stays open past 10 s; (c) held bytes drain slowly (link takes some every second): stays open.
- [ ] Implement: drop `s.hold.is_none()` from `vhp_tick`; in `vhp_held_frame` refresh `last_ms` only when zeros or a whole non-ControlOut frame are taken; in `vhp_flush_hold` refresh when `k > 0`.
- [ ] Commit.

### Task 3: PC app

**Files:** Modify `apps/pc/src/modem_sim.rs` (grant on the SimNet modem's session, `Snap.locked`, cap wrapping in `at_next`, S21 edit ends the cap), `apps/pc/src/ui.rs` (locked note, greyed Settings and directory edits, no Save bar when locked).

- [ ] Tests (unit, in modem_sim.rs): `wrap_at_send` replaces `queue_window_line`: AT&W with cap -> three lines, ATZ with cap -> marks the reset tag, `ATS21=3` with cap -> ends the cap with the note; AT&W queued while `cap_old` is None but set by the time it is sent -> wrapped. `ui::line_allowed(locked, line)` false for `AT&W` when locked, true for `AT+SCAN`.
- [ ] Implement; Snap `locked` true for `Backend::Real(ModemLink::Tcp(_))`.
- [ ] Commit.

### Task 4: Docs and bench

- [ ] docs/FEATURES.md (new row, fix the "known limit" text), docs/voice-pc.md ("Anyone on the modem's Wi-Fi" paragraph and the `--cap-tier` row), FEATURES voice host protocol error list.
- [ ] Full test, clippy, fmt; firmware builds `heltec_v4` and (in F:\lfd3k-sm-v3two after merging) `heltec_v3`.
- [ ] Bench after Rev's go to flash: TCP app shows locked Settings; `AT&W` over TCP refused; USB app saves; screenshots.
