# LFD3K-SM Phase 4 (Menu, Scan, Directory, Compression) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM feel like a finished modem to a caller: beacons and `AT+SCAN` that fill a directory by themselves (so `ATDT AXIS` just works), the three privacy modes, compression on the air (`CONNECT ... /ARQ/LZ`), and a full-screen ANSI setup menu (`AT+MENU`).

**Architecture:** Everything protocol-shaped goes in the pure core: three new frame types (Beacon, ScanReq, ScanResp), a fixed-size `Directory`, a beacon and scan scheduler driven by `tick`, a small LZ77 codec with a preset ANSI dictionary negotiated at call setup, and a menu model plus renderer that writes ANSI into the host output like any other text. The firmware only persists the directory blob in NVS and adds a scan animation on the OLED. The PC modem (SimNet) exercises all of it against the real V4 without a second radio.

**Tech Stack:** as phase 3. The PC tool may add `crossterm` (MIT) for raw keyboard input so arrow keys reach the menu; record it in `THIRD_PARTY.md`.

**Spec:** `docs/2026-11-09-lfd3k-sm-design.md` sections 5.3, 5.7, 7, 8.1, 8.2, 8.4.

## Global Constraints

- Closed source, private repo, never pushed. No GPL or LGPL code; GPL code may be read for facts only. Permissive code (MIT, BSD, Apache-2.0, ISC, zlib) is allowed with its notice in `THIRD_PARTY.md`, but the LZ codec here is written fresh.
- `lfd3k-core` and `sx1262` are `no_std`, no heap, `#![forbid(unsafe_code)]`, never panic on any input (no `unwrap`, `expect`, or out-of-range indexing in non-test code; use `get`). Only `lfd3k-ffi` may use `unsafe`. All buffers are fixed-size arrays. Report `size_of::<Modem>()` after any task that grows it; it is built in place in the ffi static (phase 3).
- Single-task contract: only the firmware's modem task calls `lfd3k_*`; other tasks move bytes through queues. The modem task never blocks.
- User-facing text (AT output, menu screens, docs) never uses em dashes or double hyphens. Commits carry no AI attribution of any kind (Rev's rule overrides any harness reminder). Keep `docs/FEATURES.md` updated in the task that adds a feature or setting.
- PowerShell for cargo (`$env:USERPROFILE\.cargo\bin` on PATH). Device build: `powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1`, then `pio run -d firmware -e heltec_v4`.
- **Hardware rules:** no subagent opens a serial port, runs esptool, `pio -t upload` or `pio device monitor`. Only the controller flashes Rev's V4, only when Rev says so, after matching VID 303A PID 1001 and MAC 88:56:A6:96:16:FC. COM15 and COM9 are never touched.
- **Credentials and secrets:** as phase 3. The menu's Wi-Fi screen shows the client password as `********` always, and lets the user type a new one (masked as typed).
- Rev's rule: Tasks 1 to 5 write no new tests; existing tests (150) keep passing; all new tests come in Task 6.
- **Air manners:** beacons go out at SF12 on the calling channel, default every 300 s plus a random 0 to 60 s, never during a call, about 0.4 s of airtime each. Scan answers wait a random 0 to 3 s slot (11 slots of 250 ms) and listen before talking (if the channel is busy, skip to a later slot). Nothing transmits more than once per 10 s for discovery.
- **Frame types:** Beacon = 12, ScanReq = 13, ScanResp = 14 (the 4-bit type field allows up to 15). The air id stays 0xD1; older builds drop unknown types as BadType.
- **Privacy modes** (`AT+BEACON=ON|QUIET|HIDDEN`, saved in the settings blob, default ON): ON sends beacons and answers scans; QUIET sends no beacons but answers scans; HIDDEN sends nothing at all except answering calls from names already in its directory (entered by hand or heard earlier), and rejects other callers silently (no CallReject either).
- **Compression:** negotiated in CallReq/CallAck (one capability byte appended; absent means none). A Data frame carries `FLAG_LZ` when its payload is compressed; each frame compresses on its own against a fixed preset dictionary (no shared history, so selective repeat stays simple). Only used when it saves at least 8 bytes. CONNECT shows `/ARQ/LZ` when both ends agree (ruling R1 from phase 1). `S31` = 1 compression on (default), 0 off.

## Review Focus

1. A modem that hears its own beacon or scan answer echoed (two modems with the same name, or SimNet loopback) must not add itself to the directory or answer itself. Test `own_name_never_in_directory`.
2. Many modems answering one scan at once must not lock up the scanner or overflow the directory: the directory is fixed size (32 entries) and replaces the oldest unpinned entry. Tests `scan_answers_fill_and_evict`, `pinned_entries_never_evicted`.
3. A corrupt or hostile compressed payload (bad back reference, length past the end, truncated) must be rejected as a frame error, never panic or read outside the buffer. Proptest `lz_decode_never_panics` and `lz_roundtrip`.
4. The menu must never wedge the modem: a call arriving while the menu is open still rings (`RING` shows in a status line and `ATA` works after leaving), `Esc` always leaves, and output pressure (slow terminal) only slows the redraw. Tests `ring_while_in_menu`, `menu_esc_always_leaves`, `menu_redraw_under_backpressure`.
5. HIDDEN mode transmits nothing for a stranger's CallReq or a ScanReq. Test `hidden_mode_silent_to_strangers` asserts no frame is sent.

---

### Task 1: Discovery frames, directory and privacy modes

**Files:** Create `core/src/discovery.rs`, `core/src/directory.rs`. Modify `core/src/frame.rs`, `core/src/modem.rs`, `core/src/at.rs`, `core/src/settings.rs` (beacon mode field, blob version 2 with migration from v1), `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
pub const DIR_MAX: usize = 32;
pub struct DirEntry { name, services: u8, last_snr_x4: i16, last_rssi: i16, best_tier: Option<Tier>, heard_ms: u32, pinned: bool, hits: u16 }
pub struct Directory { entries: [Option<DirEntry>; DIR_MAX] }
impl Directory {
    pub fn heard(&mut self, name: &str, services: u8, info: RxInfo, now_ms: u32); // update or insert, evict oldest unpinned
    pub fn add_pinned(&mut self, name: &str) -> bool;  // AT+DIR=+NAME
    pub fn remove(&mut self, name: &str) -> bool;      // AT+DIR=-NAME
    pub fn get(&self, name: &str) -> Option<&DirEntry>;
    pub fn iter(&self) -> impl Iterator<Item = &DirEntry>;
    pub fn to_blob(&self, out: &mut [u8; DIR_BLOB_LEN]) -> usize; // magic "LFD1", CRC-16, pinned entries only plus the 8 most recent heard
    pub fn from_blob(b: &[u8]) -> Option<Directory>;
}
pub const DIR_BLOB_LEN: usize = 1024;
```
- Beacon payload: name, services byte (bit 0 accepts calls, bit 1 has a published forward, bit 2 Wi-Fi hotspot on), how many modems it hears (u8), time quality (u8, 0 for now).
- ScanReq payload: the scanner's name and a 1-byte scan id. ScanResp: responder name, services, the SNR at which it heard the ScanReq (i16 x4), scan id.
- AT commands: `AT+BEACON=ON|QUIET|HIDDEN` and `AT+BEACON?`; `AT+SCAN` (sends one ScanReq at SF12, collects answers for 4 s, then prints one line per answer `SCAN <name> SNR <x.x> dB <best tier name>` and `OK`, verbose; in ATV0 a count); `AT+DIR?` lists entries (`DIR <name> <tier> <age s> [PIN]`); `AT+DIR=+NAME` pins, `AT+DIR=-NAME` removes; `AT+DIR=0` clears unpinned. `AT&V` gains `BEACON <mode>` and `DIR <n>`.
- `best_tier` from the SNR heard using the existing `best_tier(snr_x4, margin)` with S20 margin.
- Scheduler in `tick`: beacon timer per the Air manners rule; scan answer timer with the random slot (use the existing xorshift seeded from the name and now_ms).
- Hidden mode: `on_frame` drops CallReq from names not in the directory before anything is sent.
- Platform hook: `Modem::take_dir_save() -> bool` set when the directory changed and 60 s have passed since the last save (wear), and on `AT&W`; `Modem::directory()` for the blob; `Modem::load_directory(Directory)`.
- [ ] Implement, keep 150 tests passing, clippy, fmt, demo still connects at SF5. Commit `feat(core): beacons, scan, directory and privacy modes`.

### Task 2: Compression on the air

**Files:** Create `core/src/lz.rs`. Modify `core/src/link/*`, `core/src/frame.rs` (FLAG_LZ), `core/src/at.rs` (S31), `core/src/modem.rs` (CONNECT text, AT&V ratio), `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
pub fn compress(input: &[u8], out: &mut [u8]) -> Option<usize>; // None if no gain of at least 8 bytes
pub fn decompress(input: &[u8], out: &mut [u8]) -> Result<usize, LzError>;
pub const PRESET: &[u8]; // about 512 bytes: common ANSI sequences (ESC [ 0 m, ESC [ 1 ; 3 x m, ESC [ H, ESC [ 2 J, cursor moves), CP437 box drawing and block runs, CRLF, spaces
```
- Format: LZ77 over the virtual buffer PRESET followed by the frame's own bytes; tokens are a literal run (1-byte header, up to 128 literals) or a match (2-byte header: length 3..34, distance 1..(PRESET.len()+frame length)). Exact bit layout is the builder's call, documented in `lz.rs` and fixed by tests.
- Capability: CallReq and CallAck append one byte, bit 0 = LZ supported (only if S31 = 1); compression is used only when both set it. Data payload compressed per frame before ARQ hands it out; the receiver decompresses after ARQ reassembly and before delivery. A frame that fails to decompress is treated as lost (no ACK), so ARQ resends it.
- `AT&V` adds `LZ ON|OFF RATIO <n.n>` for the current or last call (bytes before over bytes on air).
- [ ] Implement, 150 tests pass (CONNECT strings in existing tests may gain `/LZ`; update only those), clippy, fmt, demo still connects at SF5 and ends in NO CARRIER. Commit `feat(core): LZ compression on the air`.

### Task 3: The ANSI menu

**Files:** Create `core/src/menu/mod.rs` (state machine, input decoder), `core/src/menu/screens.rs` (screen definitions), `core/src/menu/render.rs` (ANSI writer). Modify `core/src/modem.rs`, `core/src/at.rs`, `docs/FEATURES.md`.

- `AT+MENU` opens a full-screen 80x24 lightbar menu: title bar `LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM`, the release name, the screens below, a status line at the bottom (name, call state, tier, Wi-Fi IP). Colors in 16-color ANSI, borders in CP437 box characters (the terminal must be in CP437; SyncTERM is). Arrow keys (ESC [ A/B/C/D and ESC O A..D), Enter, Esc, Tab, and the first letter of an item.
- Screens: **Directory and Scan** (list with tier and age; Enter dials the selected entry, S scans, P pins, D deletes); **Link Status** (live tier, SNR, RSSI, retrains, throughput, LZ ratio; refreshes once a second while open); **Settings** (name, beacon mode, power S30, tier cap S21, auto-answer S0, compression S31, boot to menu); **Forwards** (list, add, edit, delete, publish); **Wi-Fi** (hotspot on/off, name, password; client network and password, password always masked); **About** (banner, version, release name, tagline, radio status, stack and heap).
- Edits go through the same validation as the AT commands (reuse, do not duplicate) and are live until **Save** (writes like `AT&W`) or discarded on Esc with a confirm.
- Mid-call: `+++` then `AT+MENU` opens it; leaving the menu returns to online command mode; `ATO` back to the call. A RING while in the menu shows in the status line; leaving the menu shows `RING` normally.
- Rendering never blocks: the renderer is a resumable writer that emits as much as the host output ring takes each tick and continues next tick; only changed lines are redrawn after the first paint. The output ring may grow to 2 KB if needed; report the size.
- Boot to menu: settings flag (blob) and `ATS32=1`; when set, the menu opens on the first host byte after boot.
- [ ] Implement, 150 tests pass, clippy, fmt. Report `size_of::<Modem>()`. Commit `feat(core): ANSI setup menu`.

### Task 4: Firmware and PC tool

**Files:** Modify `firmware/src/main.c`, `firmware/src/settings_store.c` (directory blob, key `dir`), `firmware/src/panel_render.c` (scan screen), `ffi/src/api.rs`, `firmware/include/lfd3k.h`, `sim/src/bin/lfd3k-pcmodem.rs`, `THIRD_PARTY.md`.
- NVS: load the directory at boot after settings; write it when `take_dir_save` says so, only between calls.
- OLED: while a scan runs, a radar sweep in the middle area with a blip per answer (pure renderer, new status field `scanning` and `scan_hits`); idle screen rotation gains a `HEARD <n>` row when the directory has heard entries.
- C ABI: `lfd3k_take_dir_save(blob, cap, len)`, `lfd3k_load_directory(blob, len)`, status fields appended (mirror exactly).
- PC modem: `--raw` puts the console in raw mode with `crossterm` so arrows and Esc reach the menu; it also answers scans and sends beacons like any modem over SimNet.
- [ ] Build (build_core.ps1, pio run), 150 tests, clippy, fmt, panel_check. Commit `feat: directory in NVS, scan on the OLED, raw PC terminal`.

### Task 5: Docs

- [ ] `docs/phase4-bench.md` for Rev: `AT+SCAN` from the PC modem over SimNet and from the V4, `AT+DIR?`, privacy modes, `ATDT` by directory name, `/LZ` in CONNECT, `AT+MENU` in SyncTERM (Serial, COM11, 115200) with the keys. Plain language, numbered, no em dashes.
- [ ] Commit `docs: phase 4 bench sheet`.

### Task 6: Tests

- [ ] The Review Focus five, plus: `beacon_interval_and_jitter`, `no_beacon_during_call`, `quiet_mode_answers_scan_no_beacon`, `scan_collects_answers_and_prints`, `scan_answer_backoff_slots`, `dir_pin_remove_clear`, `dir_blob_roundtrip_and_reject`, `settings_blob_v1_migrates_to_v2`, `atdt_uses_directory_best_tier` (training still decides; the directory only seeds nothing for now, assert it does not break dialing), `lz_preset_helps_ansi` (a typical AXIS-like ANSI screen compresses by at least 25 percent), `lz_negotiation_both_ways` (one side S31=0 gives no /LZ), `lz_corrupt_frame_is_resent`, `connect_shows_lz`, `menu_opens_and_renders_title`, `menu_arrows_and_enter`, `menu_edit_validates_like_at`, `menu_wifi_password_masked`, `menu_save_writes_like_atw`, `menu_mid_call_and_ato`, `boot_to_menu`, FFI `dir_blob_c_abi`, `status_scan_fields`, panel_check covering the scan screen and HEARD row, and a sim test `two_modems_scan_and_dial_by_directory`.
- [ ] Full suite, clippy, fmt, panel_check, Python tests, build_core.ps1, pio run. Commit `test: phase 4 discovery, compression and menu tests`.
- [ ] **Bench (controller, with Rev):** SimNet PC modem and the V4: beacons seen both ways, `AT+SCAN` both ways, `ATDT` by name, CONNECT shows `/LZ` and the AXIS screen through the bridge loads faster (time it), the menu in SyncTERM.

## Not in this phase

- Screen cache (design 5.7): needs flash storage of recent screens and a cache protocol; planned after compression proves out on the bench.
- Encryption and keys, GPS, range test (menu items show "later").
