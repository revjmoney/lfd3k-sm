# LFD3K-SM Phase 3 (Wi-Fi and AXIS hookup) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM its Wi-Fi side: hotspot and client mode with saved settings, named forwards (for example `AXIS` = the board's telnet port), dialing a forward or `host:port` from the USB terminal over Wi-Fi, and answering a radio call by bridging it to a published forward, which is how a radio caller reaches an AXIS BBS.

**Architecture:** The pure core gains a settings model (names, Wi-Fi, forwards, published service), new AT commands, and a second call path called the **wire**: a TCP stream the platform opens on the core's request. The core keeps all AT, escape, result code and bridging logic; the platform only runs Wi-Fi and one TCP socket and moves bytes. A radio call answered with a published forward is bridged link to wire inside the core. Settings persist as one versioned blob the platform stores in NVS.

**Tech Stack:** as phase 2, plus ESP-IDF Wi-Fi (AP+STA), esp_netif, lwIP sockets, NVS.

**Spec:** `docs/2026-11-09-lfd3k-sm-design.md` sections 6, 8.1, 8.5, 10, 15 step 4.

## Global Constraints

- Closed source, private repo, never pushed. No GPL or LGPL code. GPL code (AXIS BBS is GPL, Rev's own) may be read for facts such as which telnet options it negotiates, then written fresh; never copied. Record any new crate in `THIRD_PARTY.md`.
- `lfd3k-core` and `sx1262` are `no_std`, no heap, `#![forbid(unsafe_code)]`, never panic on any input (no `unwrap`, `expect`, or out-of-range indexing in non-test code; use `get`). Only `lfd3k-ffi` may use `unsafe`. All buffers are fixed-size arrays.
- Single-task contract: only the firmware's modem task calls any `lfd3k_*` Rust function; other tasks (USB, Wi-Fi, TCP, panel) only move bytes through stream buffers or queues. The modem task never blocks (no blocking socket, NVS or Wi-Fi calls on it except the short NVS save below).
- User-facing text never uses em dashes or double hyphens. Commits carry no AI attribution. Keep `docs/FEATURES.md` updated in the task that adds a feature or setting.
- Run cargo from PowerShell (Git Bash's link.exe shadows MSVC); prepend `$env:USERPROFILE\.cargo\bin` if needed. Device build: `powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1`, then `pio run -d firmware -e heltec_v4` (`$env:USERPROFILE\.platformio\penv\Scripts` on PATH). The firmware CMake stops with "Stale Rust core" if a `.rs` is newer than the `.a`.
- Board facts (unchanged from phase 2): V4 pins in `firmware/src/board.h`; console and logs on UART0 only (no USB bridge on this board), so `AT` over USB is the only window into the device.
- Existing interfaces to build on: `core/src/modem.rs` (`Modem`, `Config`, `host_input`, `tick`, `host_output`, `call_state`, `set_radio_diag`, `take_fb_request`), `core/src/at.rs` (`AtCmd`, `parse_line`, `SRegs`), `ffi/src/api.rs` (C ABI, `fake_hal` for host tests), `firmware/include/lfd3k.h`, `firmware/src/main.c` (modem task loop), `sim/src/lib.rs` (two-modem harness).
- **Hardware rules:** no subagent opens a serial port, runs esptool, `pio -t upload` or `pio device monitor`. Only the controller flashes Rev's V4 and only when Rev says so in chat, after matching VID 303A PID 1001 and MAC 88:56:A6:96:16:FC. COM15 (lathe) and COM9 (V3) are never touched.
- **Credentials:** Claude never types Rev's home Wi-Fi password. Rev enters `AT+WSTA` himself. The modem never prints a stored client password (shown as `********`); the hotspot password it generated itself is shown, because Rev needs it to join.
- Rev's rule: Tasks 1 to 5 write no new tests; existing tests keep passing; all new tests come in Task 6.
- **Connect strings:** a wire call prints `CONNECT WIFI TCP` (honest name, no fake speed). A radio call bridged to a forward prints nothing extra on the caller's side beyond the normal radio `CONNECT`.
- **First boot (design 8.5):** hotspot name = the modem name (`AXISMODEM-XXXX`), hotspot password = 12 random characters from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (no look-alikes), generated once with the hardware RNG and saved. Hotspot is on by default, WPA2, channel 6, max 4 clients. Client mode off until `AT+WSTA` is set.
- **Telnet:** AXIS boards speak telnet on port 23: on connect the server sends `IAC WILL ECHO, IAC WILL SGA, IAC DO SGA, IAC WILL BINARY, IAC DO BINARY, IAC DO NAWS` and filters IAC from what it receives (facts read from `F:\axis-bbs\src\net\telnet.cpp`). Each forward and `host:port` dial has a mode: telnet (`T`) or raw (`R`); default `T` for port 23, `R` otherwise. In telnet mode the core is a telnet client on the wire side: it answers `WILL ECHO/SGA/BINARY` with `DO`, `DO SGA/BINARY` with `WILL`, `DO NAWS` with `WILL NAWS` plus `SB NAWS 0 80 0 25 SE`, refuses everything else (`DONT`/`WONT`), strips all negotiation and subnegotiation from incoming bytes, turns `IAC IAC` into one 0xFF, and doubles 0xFF going out; when BINARY was not agreed, an outgoing CR is sent as CR NUL. The caller (USB terminal or the radio caller) only ever sees clean bytes.
- **Hotspot:** no routing or NAPT between the hotspot side and the client side; hotspot devices reach only the modem itself (later phases add the bridge).
- **Secrets on screen and in files:** the generated hotspot password is shown on the OLED only until the first device joins the hotspot or until `AT&W` has saved it once with a client network set, and in `AT+WAP?`. Nothing that contains it or Rev's client password (screenshots, logs, bench transcripts) is committed or sent to chat; bench tools redact `WAP` lines when logging.
- **Access policy (design 6):** an answered radio call is bridged only to the one forward named by `AT+PUB`; with none published, answered calls behave as today (data to USB). Never bridge a radio caller to an arbitrary `host:port`.

## Review Focus

1. A forward or host that does not answer must give `NO CARRIER` after a timeout (S7 seconds), never a hang; a refused or reset connection gives `NO CARRIER` at once. Tests `wire_connect_timeout_no_carrier`, `wire_refused_no_carrier`.
2. The far TCP end closing mid-call gives `NO CARRIER` and returns to command mode; `+++` then `ATH` closes the socket. Tests `wire_remote_close`, `wire_escape_and_hangup`.
3. A burst from the TCP side faster than USB drains must not lose bytes or block the modem task: the core only takes what its ring holds, the platform keeps the rest. Test `wire_backpressure_no_loss`.
4. Corrupt or old settings blobs in NVS (wrong version, bad length, bad checksum) load defaults and keep the first-boot credentials logic, never crash. Test `settings_blob_rejects_garbage` and a proptest over random blobs.
5. A radio caller to a modem with a published forward that cannot be reached gets `NO CARRIER` on the caller's side, and the answering modem goes back to idle. Sim test `bridge_forward_down_caller_gets_no_carrier`.

---

### Task 1: Core settings, forwards and AT commands

**Files:** Create `core/src/settings.rs`. Modify `core/src/at.rs`, `core/src/modem.rs`, `core/src/lib.rs`, `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
pub const MAX_FWD: usize = 8;
pub const FWD_NAME: usize = 16;   // same rule as station names
pub const HOST_MAX: usize = 63;
pub const SSID_MAX: usize = 32;
pub const PASS_MAX: usize = 63;   // WPA2: 8..=63
pub enum WireMode { Telnet, Raw }
pub struct Forward { name, host, port: u16, mode: WireMode }        // fixed arrays + lengths
pub struct Settings { ap_on: bool, ap_ssid, ap_pass, sta_ssid, sta_pass, fwd: [Option<Forward>; MAX_FWD], publish: Option<u8 /*fwd index*/>, sregs: SRegs, echo: bool, verbose: bool }
impl Settings {
    pub fn defaults(name: &str, ap_pass: &str) -> Settings;
    pub fn to_blob(&self, out: &mut [u8; BLOB_LEN]) -> usize;   // magic "LFS1", version 1, fields, CRC-16/CCITT
    pub fn from_blob(b: &[u8]) -> Option<Settings>;             // None on any problem
    pub fn forward(&self, name: &str) -> Option<&Forward>;      // case-insensitive
}
pub const BLOB_LEN: usize = 1024;
```
New AT commands (all in command mode, plain `OK`/`ERROR`):
- `AT+WAP=<ssid>,<pass>` set hotspot (pass 8..63 chars); `AT+WAP=0` / `AT+WAP=1` hotspot off/on; `AT+WAP?` prints `WAP ON <ssid> <pass>`.
- `AT+WSTA=<ssid>,<pass>` set client network; `AT+WSTA=0` forget it; `AT+WSTA?` prints `WSTA <ssid> ********` or `WSTA OFF`.
- `AT+WIFI?` prints the platform's live status line (`WIFI AP <ssid> <n> CLIENTS STA <ssid|OFF> <ip|NO IP>`), which the platform supplies through `Modem::set_wifi_status(&str)`.
- `AT+FWD=<name>,<host>,<port>[,T|R]` add or replace (mode default per Global Constraints); `AT+FWD=<name>` delete; `AT+FWD?` lists `FWD <name> <host>:<port>` lines.
- `AT+PUB=<name>` publish a forward for answered radio calls; `AT+PUB=0` none; `AT+PUB?`.
- The v1 blob holds the S registers (all of them, including S0 auto-answer and S30 power) and E/V, so a modem with `AT+PUB` and `ATS0=1` saved still answers after a power cycle.
- Until Task 2 lands, `ATDT` to a forward or `host:port` replies `NO CARRIER` at once (no radio dial is started).
- `AT&W` saves (sets a "save wanted" flag the platform takes with `Modem::take_save_request() -> bool` and reads with `Modem::settings()`); `ATZ` keeps today's register reset and also reloads saved settings (platform calls `Modem::load_settings(Settings)` at boot; ATZ restores that copy).
- `ATDT <target>`: if `<target>` matches a forward name, dial it over Wi-Fi; if it has the form `<host>:<port>` (host 1..63 of `A-Za-z0-9.-`, port 1..65535), dial that; otherwise dial by radio as today.
- `AT&V` adds `WIFI` and `PUB` lines.

- [ ] **Step 1:** `settings.rs` as above, no heap, CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF).
- [ ] **Step 2:** AT parser: `+` commands parsed from the existing `AT` head; arguments split on the first commas only (ssid may not contain a comma; passwords may contain any printable ASCII except comma, documented).
- [ ] **Step 3:** Modem holds `Settings`, applies the commands, `AT&V` lines, save/load hooks. FEATURES.md rows.
- [ ] **Step 4:** cargo test (existing 72 pass), clippy, fmt. Commit `feat(core): settings, Wi-Fi and forward AT commands`.

### Task 2: The wire call path in the core

**Files:** Create `core/src/wire.rs`, `core/src/telnet.rs` (the telnet client filter from Global Constraints, a small byte state machine, both directions). Modify `core/src/modem.rs`, `sim/src/lib.rs` (fake wire endpoint for later tests).

**Size:** after this task report `core::mem::size_of::<Modem>()` (print it from a host test or a `const` assert). Phase 2 Modem was about 5.2 KB and is built on the modem task stack inside `lfd3k_init`; Task 3 must build it in place in the static (no stack copy) if it grew past 8 KB.

**Interfaces (Produces):**
```rust
pub struct WireReq { host: [u8; HOST_MAX], host_len: u8, port: u16 }   // with accessors
impl Modem {
    pub fn take_wire_request(&mut self) -> Option<WireReq>; // platform opens TCP
    pub fn wire_connected(&mut self, now_ms: u32);
    pub fn wire_failed(&mut self, now_ms: u32);          // refused, DNS fail, timeout from platform
    pub fn wire_closed(&mut self, now_ms: u32);          // far end closed or reset
    pub fn wire_input(&mut self, bytes: &[u8], now_ms: u32) -> usize; // from TCP, returns bytes taken
    pub fn wire_output(&mut self, out: &mut [u8]) -> usize;           // to TCP
    pub fn take_wire_close(&mut self) -> bool;           // core wants the socket closed (ATH, timeout)
    pub fn wire_active(&self) -> bool;
}
```
- USB dial over Wi-Fi: `DIALING <target>` (verbose), request, then on `wire_connected` print `CONNECT WIFI TCP` and enter data mode: host bytes go to `wire_output`, `wire_input` bytes go to the host ring. `+++` guard, `ATO`, `ATH` work exactly as on radio calls. S7 seconds with no `wire_connected` gives `NO CARRIER` and sets close. `wire_failed`/`wire_closed` give `NO CARRIER`.
- Bridge for answered radio calls: when a radio call is answered and `publish` is set, the core requests the wire to that forward right after `CONNECT`; link data goes to `wire_output`, `wire_input` goes into the link; host (USB) gets only a status line `BRIDGE <fwd name>` in verbose mode. If the wire fails or closes, the core hangs up the radio call (the caller sees `NO CARRIER`); if the radio call drops, the core closes the wire.
- One call at a time: a radio call arriving during a wire call is answered `BUSY` (existing path).
- Buffers: a 2 KB wire-to-host or wire-to-link ring and a 2 KB the other way, fixed arrays. Back-pressure: `wire_input` takes only what fits.
- Sim: `sim/src/lib.rs` gains a `FakeWire` that plays a TCP server (scripted greeting, echo, close, refuse, never answer) attached to a harness modem, so Task 6 can test both paths. Existing harness behavior unchanged.
- [ ] Steps: implement, keep the 72 tests passing, clippy, fmt, demo unchanged. Commit `feat(core): wire call path and radio-to-forward bridge`.

### Task 3: FFI for settings and the wire

**Files:** Modify `ffi/src/api.rs`, `ffi/src/fake_hal.rs`, `firmware/include/lfd3k.h`.

**Interfaces (Produces, C):**
```c
int    lfd3k_load_settings(const uint8_t *blob, size_t len);   /* 0 ok, -1 rejected (defaults kept) */
bool   lfd3k_take_save(uint8_t *blob, size_t cap, size_t *len); /* true when AT&W asked; fills the blob */
void   lfd3k_default_settings(const char *ap_pass);             /* first boot: hotspot password from the RNG */
void   lfd3k_get_wifi_config(lfd3k_wifi_cfg_t *out);            /* ap_on, ap_ssid, ap_pass, sta_ssid, sta_pass, flag sta_set; NUL-terminated fixed arrays */
bool   lfd3k_take_wifi_changed(void);                           /* AT+WAP/AT+WSTA changed it: platform re-applies */
void   lfd3k_set_wifi_status(const char *line);
bool   lfd3k_take_wire_request(char *host, size_t cap, uint16_t *port);
void   lfd3k_wire_event(int ev, uint32_t now_ms);               /* 1 connected, 2 failed, 3 closed */
size_t lfd3k_wire_in(const uint8_t *buf, size_t len, uint32_t now_ms);
size_t lfd3k_wire_out(uint8_t *buf, size_t len);
bool   lfd3k_take_wire_close(void);
```
`lfd3k_status_t` gains `uint8_t wire` (0 none, 1 dialing, 2 online, 3 bridging) appended at the end (struct grows; keep `#[repr(C)]` order and mirror it).
- Build the Modem in place in the static holder (write fields into the `MaybeUninit` slot or construct with a `const fn`-friendly init) so its size no longer lands on the modem task stack.
- `lfd3k_set_stack_free(uint32_t bytes)`: the firmware reports the modem task's stack high-water mark; `AT&V` prints `STACK FREE <n>`.
- [ ] Steps: implement with the same single-task contract, extend fake_hal for host tests, build_core.ps1, cargo test, clippy. Commit `feat(ffi): settings and wire C ABI`.

### Task 4: Firmware Wi-Fi, NVS and the TCP socket

**Files:** Create `firmware/src/wifi_link.c/.h`, `firmware/src/settings_store.c/.h`. Modify `main.c`, `CMakeLists.txt`, `sdkconfig.defaults`, `partitions.csv` only if NVS is too small (it is 0x6000 today; the blob is 1 KB, fine).

- [ ] **Step 0 (boot order):** NVS init, then `lfd3k_init`, then load settings (or default and save), then hand the Wi-Fi config to the Wi-Fi task, then start Wi-Fi. Radio init stays where it is.
- [ ] **Step 1:** `settings_store`: NVS namespace `lfd3k`, key `settings` blob. Boot: load; if missing or rejected, generate the 12-char hotspot password with `esp_random()` (rejection sampling over the 32-symbol alphabet), call `lfd3k_default_settings`, save. `lfd3k_take_save` results are written from the modem task to NVS (nvs_set_blob + commit) only when no call is active; a save asked during a call is held and written right after the call ends.
- [ ] **Step 2:** `wifi_link` runs on its own Wi-Fi task: esp_netif + esp_wifi in AP+STA (AP only when no client network), `esp_wifi_set_storage(WIFI_STORAGE_RAM)` so the driver never keeps its own NVS copy of the client password (then `AT+WSTA=0` really forgets it). AP WPA2-PSK, channel 6, max 4. STA connects with retry backoff (1, 2, 4 ... 30 s). The modem task copies the config into a queue message when `lfd3k_take_wifi_changed` is true; the Wi-Fi task does the stop, set config and start. The Wi-Fi task posts a status line back; the modem task passes it to `lfd3k_set_wifi_status` once a second.
- [ ] **Step 2b:** modem task stack: keep 16 KB if Task 3 builds the Modem in place, else 24 KB; report `uxTaskGetStackHighWaterMark` through `lfd3k_set_stack_free` once a second.
- [ ] **Step 3:** One TCP client on its own task (priority 6, core 0): on a wire request, `getaddrinfo` then non-blocking connect with a 15 s limit, then a loop moving bytes between the socket and two 2 KB stream buffers; events (connected, failed, closed) go to the modem task through a small queue. The modem task alone calls `lfd3k_wire_*`. Close requests close the socket. TCP_NODELAY on.
- [ ] **Step 4:** Modem loop additions: drain events, push socket RX into `lfd3k_wire_in` keeping leftovers, pull `lfd3k_wire_out` into the socket TX buffer only as space allows, poll `lfd3k_take_wire_request`/`lfd3k_take_wire_close`/`lfd3k_take_save`/`lfd3k_take_wifi_changed`. Never block.
- [ ] **Step 5:** `pio run` succeeds, record RAM and flash (Wi-Fi adds a few hundred KB; must stay under the 1.5 MB slot). Commit `feat(firmware): Wi-Fi hotspot and client, NVS settings, TCP wire`.

### Task 4b: Live channel trace for the idle scope (Rev, 2026-11-10)

The renderer side is done (`panel_render_set_trace`, `rf_trace_t` in `firmware/src/panel_render.h`, idle scope and live idle meters). This task feeds it real data.
- [ ] `sx1262`: `pub fn rssi_inst(&mut self) -> Option<i16>` (GetRssiInst 0x15: status byte then RssiInst; dBm = -RssiInst/2). Only valid while listening; returns None in Tx or when failed.
- [ ] `ffi`: a 64-sample ring sampled every 50 ms from `lfd3k_poll` while listening (each sample an `int8` dBm clamped to -128..0, plus a tx flag for samples taken while transmitting); C entry `size_t lfd3k_rf_trace(int8_t *dbm, uint8_t *tx, size_t cap)` copies oldest first and returns the count.
- [ ] Firmware: the modem task copies the trace once per loop alongside `lfd3k_status` and passes both to the panel task (same lock as `panel_publish`); the panel task calls `panel_render_set_trace` before `panel_render`.
- [ ] Commit `feat: live channel trace for the idle scope`.

### Task 4c: Call the V4 from the PC over Wi-Fi (simulated radio, Rev, 2026-11-10)

Rev has one V4 and no second LoRa radio. This test mode lets the PC be the far modem over Wi-Fi, running the real core, so a whole call (dial, ring, training, CONNECT, data, retrain, `+++`, hang-up, and the `AT+PUB` bridge to AXIS) runs on the real V4 with only the RF simulated.
- **Datagram format (UDP port 4210 both ends):** `'L' 'S' ver=1 tier_index snr_x4(i16 LE) rssi(i16 LE)` then the frame bytes. The sender fills tier and the SNR/RSSI the receiver should "hear" (the PC chooses it from a profile so retrains happen; the V4 reports what it heard of the PC as a fixed +8 dB unless told otherwise).
- **Timing:** the simulated radio stays `busy()` for `Tier::airtime_us(len)` after `send` and only then puts the datagram on the wire, and a received datagram is delivered to the core only when its airtime has elapsed since arrival of the first byte (it arrives whole; the receiver holds it for the frame's airtime, during which `busy()` is true like a preamble-detected receive). This keeps the link timers honest.
- [ ] Core or ffi: a `SimNetRadio` implementing `Radio` behind a small trait for "datagram out / datagram in" so the same code runs in `sim` (std UDP) and in `ffi` (C HAL `lfd3k_hal_udp_send(const uint8_t*, size_t)` plus `lfd3k_udp_in(const uint8_t*, size_t, uint32_t now_ms)`).
- [ ] `AT+SIMNET=<pc ip>` switches the V4 to the simulated radio (the SX1262 is put to standby, nothing transmits on RF); `AT+SIMNET=0` back to the SX1262. Not saved by `AT&W` (always boots on the real radio). `AT&V` shows `RADIO SIMNET <ip>`; the OLED shows `SIM` in place of the HS light label while it is on.
- [ ] PC tool: `cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer <v4 ip> [--name PCMODEM] [--snr <dB>|--profile dip]`: a real `Modem` with the std UDP `SimNetRadio`, raw terminal (stdin to `host_input`, `host_output` to stdout), so Rev types `ATDT AXISMODEM-16FC` on the PC. `--script` mode for unattended tests (send lines, wait for strings, log).
- [ ] Firmware: a UDP socket task (same pattern as the TCP one; no Rust calls off the modem task).
- [ ] Commit `feat: call the V4 from the PC over Wi-Fi with a simulated radio`.

### After phase 3 (Rev, 2026-11-10): AXIS side, its own plan in the AXIS repo

Not part of this plan; recorded here so it is not lost. Built in `F:\axis-bbs` (GPL, separate program): (1) the AXIS build option that finds its modem by itself and sets the forward and publish, (2) radio callers and link stats on sysop screens, (3) plain menus for slow radio tiers, (4) Sysop Net and User Net with their own bases and menus, (5) later the ANSI world map. Start once phase 3 is on the V4 and the PC to AXIS test call works.

### Task 5: OLED and docs

- [ ] OLED idle screen bottom area alternates every 3 s between the name row and a Wi-Fi row: `AP <ssid>`, plus `PW <pass>` only under the Secrets rule in Global Constraints (the core exposes a `show_ap_pass` flag); with a client IP: `IP <a.b.c.d>`. `tools/lfd3k_term.py` redacts `WAP` and `PW` lines and refuses `--shot` while the password is on screen unless `--allow-secret` is given, and never into the repo. Online wire call: middle shows `WIFI TCP` and `ONLINE`; bridging: `BRIDGE` plus the forward name. Keep `panel_render` pure (add the Wi-Fi strings to its inputs). Re-run `tools/panel_check.ps1` (new mockups).
- [ ] `docs/phase3-bench.md` for Rev: join steps (`AT+WSTA` typed by Rev), `AT+FWD=AXIS,<board ip>,23`, `ATDT AXIS`, `AT+PUB=AXIS`, the hotspot join for a phone or laptop, and how to read `AT+WIFI?`.
- [ ] Commit `feat(firmware): Wi-Fi on the front panel, bench sheet`.

### Task 6: Tests

- [ ] Core/sim tests: the Review Focus five, plus `telnet_answers_axis_negotiation` (feed the exact AXIS opening bytes, expect the replies and NAWS 80x25, clean output), `telnet_iac_iac_and_ff_escape`, `telnet_subneg_stripped`, `raw_mode_passes_ff`, `settings_blob_keeps_sregs_and_ev`, `fwd_add_replace_delete_list`, `atdt_host_port_parses`, `atdt_bad_host_is_radio_or_error`, `wap_password_length_checked`, `wsta_never_prints_password`, `atw_sets_save_and_settings_roundtrip`, `atz_restores_saved_settings`, `wire_dial_connect_echo` (USB dial to FakeWire, CONNECT WIFI TCP, echo round trip), `wire_escape_ato_resumes`, `radio_call_bridged_to_published_forward` (sim: modem A dials B by radio, B has `AT+PUB=AXIS` to a FakeWire server that sends a banner; A's host receives the banner, A's typing reaches the server), `bridge_radio_drop_closes_wire`, `radio_call_during_wire_call_is_busy`, `pub_unknown_forward_error`, settings proptest.
- [ ] FFI host tests: `wire_bytes_cross_in_order`, `settings_load_save_roundtrip_c_abi`, `status_wire_field`.
- [ ] Panel check still passes with Wi-Fi rows (bounds, long ssid and IP).
- [ ] Full suite, clippy, fmt, demo, build_core.ps1, pio run. Commit `test: phase 3 settings, wire and bridge tests`.
- [ ] **Bench (controller, needs Rev):** Rev flashes or says to flash; Rev types `AT+WSTA` himself; then `AT+WIFI?`, `AT+FWD=AXIS,<CYD ip>,23`, `ATDT AXIS` reaches the AXIS login screen over Wi-Fi from the USB terminal and stops there (no login is typed through the logged terminal); screenshot the OLED (no password on screen); `+++`, `ATH`. Hotspot: confirm the AP is visible with the shown name. The radio caller to AXIS path is proven in the simulator only until a second LoRa radio exists.
