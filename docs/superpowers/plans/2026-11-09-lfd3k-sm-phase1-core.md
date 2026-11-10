# LFD3K-SM Phase 1: Protocol Core on the PC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two simulated LFD3K-SM modems complete a full Hayes-style call on the PC (`ATDT AXIS`, training, `CONNECT LORA SF7 BW500 21875/ARQ`, data both ways, retrain down and up as the simulated signal changes, `ATH` / `NO CARRIER`), and the same Rust core compiles and links into an ESP-IDF firmware image for the Heltec V4.

**Architecture:** A `no_std`, no-heap Rust crate (`lfd3k-core`) holds all protocol logic and never touches hardware: the host feeds it terminal bytes, received radio frames and clock ticks, and it asks a `Radio` trait to send or listen. A `std` crate (`lfd3k-sim`) provides a seeded, half-duplex simulated LoRa channel and a demo binary. A PlatformIO ESP-IDF project (`firmware/`) links the core as a static library through a tiny C ABI crate (`lfd3k-ffi`) to prove the device toolchain; nothing is flashed in this phase.

**Tech Stack:** Rust stable (PC) and the Espressif Rust toolchain via `espup` (Xtensa ESP32-S3), Cargo workspace, PlatformIO `espressif32@6.11.0` with `framework = espidf`, dev-only `proptest` for fuzzing.

**Spec:** `docs/2026-11-09-lfd3k-sm-design.md` (sections 4, 5.1, 5.3 to 5.6, 8.1, 13, 15 step 1).

## Global Constraints

- Closed source. Repo is private and is never pushed to a public remote. No GPL or LGPL code in any crate or the firmware; dev-only test crates must be MIT/Apache. Record every third-party crate and license in `THIRD_PARTY.md`.
- `lfd3k-core` is `#![cfg_attr(not(test), no_std)]`, no `alloc`, no heap, no `unsafe` (the only `unsafe` lives in `lfd3k-ffi`). All buffers are fixed-size arrays in the structs.
- Never panic on any input: every parse returns `Result`/`Option`; no `unwrap`/`expect`/indexing that can go out of bounds in non-test code (use `get`).
- No hardware access in this phase: never open, flash, probe or reset any COM port. COM11 (Rev's V4) and COM15 (lathe) are never touched by Claude at all.
- User-facing text (result codes, `ATI` banner, FEATURES.md, README) never uses em dashes or double hyphens.
- Commits carry no AI attribution (Rev's rule; a global commit-msg hook also strips it).
- All tiers in this phase are LoRa at BW 500 kHz, CR 4/5. Tier names and air rates are exactly: `LORA SF12 BW500` 1172, `LORA SF11 BW500` 2148, `LORA SF10 BW500` 3906, `LORA SF9 BW500` 7031, `LORA SF8 BW500` 12500, `LORA SF7 BW500` 21875, `LORA SF6 BW500` 37500, `LORA SF5 BW500` 62500 (bits per second).
- `ATI` prints exactly `LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM` on its first line, `v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M` on its second and `S0N1C B00M R4D10 PR0 MAX` on its third (version, release name and tagline in separate consts) (Rev, 2026-11-09: the whole name with - LFD3K-SM).
- Per Rev's standing preference, build tasks end with a clean build and a commit; all tests are written and run in the final task (Task 8).
- Keep `docs/FEATURES.md` updated in the same task that adds a feature or setting.

## Review Focus

1. Terminal sends faster than the link can carry: `host_input` must accept only what fits and report it, never drop bytes silently or overflow (test: `flow_control_never_loses_bytes`).
2. `+++` appearing inside normal data without the guard time must pass through as data, not escape to command mode (test: `plus_plus_plus_without_guard_is_data`).
3. Garbage, truncated or replayed frames from the air must be ignored without panics or state corruption (test: `fuzz_frames_never_panic`, `stray_frames_ignored_while_idle`).
4. One side's tier switch ack is lost, leaving the two ends on different tiers: both must find each other again on SF12 and keep the call, or end it cleanly with `NO CARRIER` (test: `lost_tierset_ack_resyncs`).
5. Dialing a name nobody answers, or the answerer has auto-answer off and nobody types `ATA`: `NO ANSWER` after the timeout, and the modem is usable again (test: `dial_unknown_name_gives_no_answer`).

---

## File Structure

```
F:\lfd3k-sm\
  Cargo.toml                 workspace: core, sim, ffi
  rust-toolchain.toml        stable for the workspace (PC)
  THIRD_PARTY.md
  docs/FEATURES.md           feature/setting log
  core/                      lfd3k-core (no_std)
    Cargo.toml
    src/lib.rs               module wiring, VERSION, re-exports
    src/tier.rs              Tier, ladder, names, rates, SNR floors, airtime
    src/frame.rs             frame types, encode/decode
    src/at.rs                AT command parser, result codes, S registers
    src/arq.rs               selective-repeat send/receive windows
    src/link.rs              call state machine: setup, training, data, retrain, resync, hang-up
    src/modem.rs             Modem: ties AT + link, host byte I/O, status lights
    src/radio.rs             Radio trait, RxInfo
  sim/                       lfd3k-sim (std)
    Cargo.toml
    src/lib.rs               SimChannel, SnrProfile, two-modem Harness
    src/rng.rs               xorshift64* (no deps)
    src/main.rs              demo: prints both terminals of a scripted call
    tests/                   Task 8 scenario tests
  ffi/                       lfd3k-ffi (staticlib for the device)
    Cargo.toml
    src/lib.rs               extern "C" fns, panic handler
  firmware/                  PlatformIO ESP-IDF project (compile only)
    platformio.ini
    partitions.csv
    src/main.c
    src/CMakeLists.txt
    CMakeLists.txt
    build_core.ps1           builds lfd3k-ffi for xtensa-esp32s3-none-elf
```

---

### Task 1: Toolchain, workspace and logs

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `THIRD_PARTY.md`, `docs/FEATURES.md`, `core/Cargo.toml`, `core/src/lib.rs`, `sim/Cargo.toml`, `sim/src/lib.rs`, `sim/src/main.rs`, `ffi/Cargo.toml`, `ffi/src/lib.rs`
- Brain: create `D:\brain\Projects\LFD3K-SM.md`, `D:\brain\Tools\espup Rust ESP toolchain.md`; link both from `D:\brain\Rev J Money Brain.md` and from `D:\brain\Projects\AXIS BBS Firmware.md`

**Interfaces:**
- Produces: `lfd3k_core::VERSION: &str = "0.1.0"`.

- [ ] **Step 1: Install Rust and the ESP toolchain.** `winget install Rustlang.Rustup` (or rustup-init from rustup.rs), then `cargo install espup --locked`, `espup install --targets esp32s3`. espup writes an export script (`%USERPROFILE%\export-esp.ps1`); record its path. Expected: `rustc --version` and `cargo +esp --version` both print versions.
- [ ] **Step 2: Workspace.** Root `Cargo.toml` with `members = ["core", "sim", "ffi"]`, `resolver = "2"`, `[profile.release] opt-level = "s", lto = true, codegen-units = 1`. (Panic is set to abort only for the device build of `ffi`, in Task 7, so PC tests keep normal panics.) `rust-toolchain.toml`: `channel = "stable"`. `.gitignore`: `target/`, `firmware/.pio/`, `firmware/lib/*.a`.
- [ ] **Step 3: Crate shells.** `core`: `#![cfg_attr(not(test), no_std)]`, `#![forbid(unsafe_code)]`, `pub const VERSION: &str = "0.1.0";`. `sim`: depends on `lfd3k-core` by path; `main.rs` prints the version. `ffi`: `crate-type = ["staticlib"]`, `#![no_std]`, empty for now.
- [ ] **Step 4: Logs.** `THIRD_PARTY.md` (table: crate, version, license, used in: firmware or dev-only). `docs/FEATURES.md` with sections Features (name, status, date), Settings (name, default, range), Change log. Brain notes: LFD3K-SM project note (what it is, repo path, private, design doc and plan paths, status); espup tool note (install commands, export script path, Xtensa target name, Windows gotchas met).
- [ ] **Step 5: Build.** `cargo build --workspace`. Expected: success, no warnings.
- [ ] **Step 6: Commit.** `git add -A; git commit -m "chore: cargo workspace, logs"`.

### Task 2: Tiers

**Files:** Create `core/src/tier.rs`; modify `core/src/lib.rs`, `docs/FEATURES.md`.

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)] pub enum Tier { Sf12, Sf11, Sf10, Sf9, Sf8, Sf7, Sf6, Sf5 }` (declaration order = slowest to fastest).
  - `impl Tier { pub const LADDER: [Tier; 8]; pub fn sf(self) -> u8; pub fn name(self) -> &'static str; pub fn air_bps(self) -> u32; pub fn snr_floor_x4(self) -> i16; pub fn airtime_us(self, payload_len: usize) -> u32; pub fn slower(self) -> Option<Tier>; pub fn faster(self) -> Option<Tier>; pub fn index(self) -> u8; pub fn from_index(i: u8) -> Option<Tier>; pub const ROBUST: Tier = Tier::Sf12; }`
  - `pub fn best_tier(snr_x4: i16, margin_x4: i16) -> Tier`: fastest tier whose `snr_floor_x4 + margin_x4 <= snr_x4`, else `Sf12`.

- [ ] **Step 1: Implement `tier.rs`.** Names and rates exactly as in Global Constraints. SNR floors (dB, SX1262 datasheet demodulation limits), stored x4: SF5 -2.5, SF6 -5, SF7 -7.5, SF8 -10, SF9 -12.5, SF10 -15, SF11 -17.5, SF12 -20. `airtime_us` uses the Semtech LoRa time-on-air formula with BW 500 kHz, CR 4/5, explicit header, CRC on, preamble 8 symbols (SF5 and SF6 use the 12-symbol preamble rule of the SX1262), low-data-rate optimize off (symbol time stays under 16 ms at BW500 for every SF). Why one SNR measurement predicts every tier: at a fixed 500 kHz bandwidth the measured SNR does not depend on SF, so the core measures once on SF12 and picks from the floors.
- [ ] **Step 2: Log** the tier ladder in FEATURES.md. **Build** `cargo build --workspace`. **Commit** `feat(core): tier ladder`.

### Task 3: Frames and the Radio trait

**Files:** Create `core/src/frame.rs`, `core/src/radio.rs`; modify `core/src/lib.rs`.

**Interfaces:**
- Consumes: `Tier` (Task 2).
- Produces:
  - `pub const HEADER_LEN: usize = 4; pub const MAX_FRAME: usize = 255; pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN; pub const NAME_MAX: usize = 16;`
  - `#[repr(u8)] pub enum FrameType { CallReq = 1, CallAck = 2, CallReject = 3, Train = 4, TrainAck = 5, Data = 6, Ack = 7, TierSet = 8, TierAck = 9, Hangup = 10, Keepalive = 11 }`
  - `pub struct Header { pub ftype: FrameType, pub flags: u8, pub call_id: u8, pub seq: u8, pub ack: u8 }`. Wire: byte0 = `ftype << 4 | (flags & 0x0F)`, byte1 = call_id, byte2 = seq, byte3 = ack.
  - `pub struct Frame<'a> { pub header: Header, pub payload: &'a [u8] }`
  - `pub enum FrameError { TooShort, BadType, TooLong, BadPayload }`
  - `pub fn encode(header: &Header, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError>`; `pub fn decode(buf: &[u8]) -> Result<Frame<'_>, FrameError>`
  - Payload helpers: `pub fn call_req_payload(caller: &str, callee: &str, out: &mut [u8]) -> Result<usize, FrameError>` and `pub fn parse_call_req(p: &[u8]) -> Result<(&str, &str), FrameError>` (each name: 1 length byte then ASCII bytes, 1 to `NAME_MAX`, printable ASCII only); Train/TrainAck payload = `[tier_index, snr_x4 as i16 LE (2 bytes)]`; TierSet/TierAck payload = `[tier_index]`; Ack payload = `[sack_bitmap]` (bit i = frame `ack + 1 + i` received).
  - `radio.rs`: `pub trait Radio { fn send(&mut self, tier: Tier, frame: &[u8]); fn listen(&mut self, tier: Tier); fn busy(&self) -> bool; }` and `pub struct RxInfo { pub tier: Tier, pub rssi_dbm: i16, pub snr_x4: i16 }`.

- [ ] **Step 1: Implement** `frame.rs` and `radio.rs`. Decoding validates type, length and payload shape; never panics.
- [ ] **Step 2: Build, commit** `feat(core): frame format and radio trait`.

### Task 4: AT commands and S registers

**Files:** Create `core/src/at.rs`; modify `core/src/lib.rs`, `docs/FEATURES.md`.

**Interfaces:**
- Produces:
  - `pub enum AtCmd<'a> { Attention, Dial(&'a str), Answer, Hangup, Online, Info, Verbose(bool), Echo(bool), SetReg(u8, u16), GetReg(u8), ShowConfig, Reset }`
  - `pub enum AtError { Unknown, BadArg }`
  - `pub fn parse_line(line: &str) -> Result<AtCmd<'_>, AtError>`: case-insensitive `AT`, `ATDT<name>` (also `ATD<name>`, name trimmed, 1 to 16 chars), `ATA`, `ATH`/`ATH0`, `ATO`, `ATI`, `ATV0`/`ATV1`, `ATE0`/`ATE1`, `ATSn=v`, `ATSn?`, `AT&V`, `ATZ`. One command per line in v0.1.
  - `pub enum ResultCode { Ok, Error, Ring, NoCarrier, Busy, NoAnswer, Connect(Tier), Retrain(Tier) }` and `pub fn format_result(code: &ResultCode, verbose: bool, out: &mut [u8]) -> usize`. Verbose strings: `OK`, `ERROR`, `RING`, `NO CARRIER`, `BUSY`, `NO ANSWER`, `CONNECT <tier name> <air_bps>/ARQ`, `RETRAIN <tier name> <air_bps>`; each wrapped `\r\n...\r\n`. Numeric (V0): 0 OK, 4 ERROR, 2 RING, 3 NO CARRIER, 7 BUSY, 8 NO ANSWER, 1 CONNECT, retrain suppressed.
  - `pub struct SRegs` with `get(n) -> Option<u16>`, `set(n, v) -> bool`. Registers and defaults: S0 auto-answer rings 0 (0 = off), S2 escape char 43 (`+`), S7 carrier wait seconds 30, S12 escape guard time in 1/50 s 50, S20 training margin in dB x4 = 20 (5 dB), S21 tier cap (tier index, 7 = SF5), S22 resync timeout seconds 20.
- [ ] **Step 1: Implement** `at.rs`. **Log** commands and registers in FEATURES.md. **Build, commit** `feat(core): AT parser, result codes, S registers`.

### Task 5: Selective ARQ

**Files:** Create `core/src/arq.rs`; modify `core/src/lib.rs`.

**Interfaces:**
- Consumes: `MAX_PAYLOAD` (Task 3).
- Produces:
  - `pub const WINDOW: usize = 8; pub const CHUNK: usize = 200;` (data payload per frame in v0.1, under `MAX_PAYLOAD`).
  - `pub struct ArqTx { .. }`: `pub fn new() -> Self; pub fn free(&self) -> usize` (bytes it can still accept: `WINDOW * CHUNK` minus queued); `pub fn push(&mut self, data: &[u8]) -> usize` (returns bytes accepted); `pub fn next_to_send(&mut self, now_ms: u32, rto_ms: u32) -> Option<(u8, &[u8])>` (seq and chunk: first unsent, else oldest unacked past its timeout); `pub fn on_ack(&mut self, ack: u8, sack: u8)`; `pub fn idle(&self) -> bool`; `pub fn reset(&mut self)`.
  - `pub struct ArqRx { .. }`: `pub fn new() -> Self; pub fn on_data(&mut self, seq: u8, data: &[u8]) -> bool` (stores in-window frames, false if out of window); `pub fn ack_state(&self) -> (u8, u8)` (cumulative next expected, sack bitmap); `pub fn read(&mut self, out: &mut [u8]) -> usize` (in-order bytes only); `pub fn reset(&mut self)`.
  - Sequence numbers are `u8` modulo 256 with window 8; comparisons use wrapping arithmetic.
- [ ] **Step 1: Implement** `arq.rs` with fixed arrays `[[u8; CHUNK]; WINDOW]`. **Build, commit** `feat(core): selective repeat ARQ`.

### Task 6: Link state machine and Modem

**Files:** Create `core/src/link.rs`, `core/src/modem.rs`; modify `core/src/lib.rs`, `docs/FEATURES.md`.

**Interfaces:**
- Consumes: everything from Tasks 2 to 5.
- Produces:
  - `pub struct Config { pub name: [u8; 16], pub name_len: u8, pub sregs: SRegs }` with `Config::new(name: &str) -> Config`.
  - `pub struct Lights { pub oh: bool, pub cd: bool, pub sd: bool, pub rd: bool, pub hs: bool, pub ec: bool, pub aa: bool, pub tier: Option<Tier> }` (`hs` = tier at SF7 or faster; `sd`/`rd` = a data frame sent/received in the last 100 ms; `ec` = connected (ARQ always on); `aa` = S0 > 0).
  - `pub struct Modem { .. }` with:
    - `pub fn new(cfg: Config) -> Self`
    - `pub fn host_input<R: Radio>(&mut self, bytes: &[u8], radio: &mut R, now_ms: u32) -> usize` (bytes accepted; in command mode buffers a line up to 64 chars and runs it on `\r`; in data mode feeds ArqTx, and watches for the S12 guard, three S2 chars, S12 guard escape)
    - `pub fn radio_rx<R: Radio>(&mut self, frame: &[u8], info: RxInfo, radio: &mut R, now_ms: u32)`
    - `pub fn tick<R: Radio>(&mut self, radio: &mut R, now_ms: u32)` (call every 10 ms or faster)
    - `pub fn host_output(&mut self, out: &mut [u8]) -> usize` (result codes, echo, received data; internal ring buffer 1024 bytes)
    - `pub fn lights(&self, now_ms: u32) -> Lights`
    - `pub fn is_connected(&self) -> bool`
  - Call ids: the caller picks `call_id` = low byte of `now_ms` OR 1 (never 0); 0 means "no call".
- Behavior the implementer must follow (spec 5.4 to 5.6):
  1. Idle: listen on `Tier::ROBUST`. A `CallReq` whose callee matches our name (case-insensitive) prints `RING` (one per 3 s while ringing, up to S7); answer when the ring count reaches S0 (if S0 > 0) or on `ATA`. Frames for other names or calls are ignored.
  2. `ATDT name`: print `DIALING <name>` and `RINGING` in verbose mode, send `CallReq` on SF12, resend every 3 s until `CallAck`, `CallReject` (`BUSY`) or S7 seconds (`NO ANSWER`).
  3. Training: the answerer replies `CallAck`, then `Train` carrying the SNR it measured on the CallReq. The caller computes `t = best_tier(min(own snr, peer snr), S20)` capped by S21, prints `TRAINING SNR <x.x> dB` in verbose mode, and sends `TierSet(t)` on SF12. The answerer replies `TierAck` on SF12 and switches to `t`; the caller switches on `TierAck`. Then the caller sends `Keepalive` on `t`; if it gets one back within 3 airtimes plus 200 ms, both print `CONNECT`; if not, both step one tier slower and retry; at SF12 failure gives `NO CARRIER`.
  4. Data: ArqTx/ArqRx over `Data` and `Ack` frames on the current tier; the receiver sends `Ack` after each data frame or burst; RTO = 3 x airtime(full frame) + 100 ms; half duplex respected (never send while `radio.busy()`).
  5. Retrain: keep an EWMA of received SNR (alpha 1/8). If EWMA < floor + S20 - 8 (x4 units, i.e. 2 dB hysteresis) or 4 RTOs pass with nothing received, the side noticing sends `TierSet(slower)`; if EWMA > floor(faster) + S20 + 12 for 10 s, it sends `TierSet(faster)` (never above S21). Peer replies `TierAck` and switches; sender switches on `TierAck`. Verbose mode prints `RETRAIN ...`. ARQ state survives tier changes.
  6. Resync: if nothing is heard for 8 RTOs, both sides fall back to SF12 and exchange `Keepalive` there, then retrain upward. If S22 seconds pass with nothing heard on SF12, print `NO CARRIER` and go idle.
  7. `+++` with guard times goes to online-command mode (`OK`); `ATO` returns to data; `ATH` sends `Hangup` (twice) and prints `NO CARRIER` locally; a received `Hangup` prints `NO CARRIER`.
- [ ] **Step 1: Implement** `link.rs` (state enum and transitions, no I/O) and `modem.rs` (AT glue, buffers, lights). **Log** in FEATURES.md. **Build, commit** `feat(core): call setup, training, retrain, resync`.

### Task 7: Simulator, demo and firmware skeleton

**Files:** Create `sim/src/rng.rs`, `sim/src/lib.rs`, `sim/src/main.rs`; create `ffi/src/lib.rs`, everything under `firmware/`; modify `THIRD_PARTY.md`, `docs/FEATURES.md`, brain LFD3K-SM note.

**Interfaces:**
- Consumes: `Modem`, `Radio`, `RxInfo`, `Tier`.
- Produces:
  - `pub struct Rng(u64)` xorshift64*, `next_u32`, `chance(p_x1000: u32) -> bool`.
  - `pub enum SnrProfile { Constant(i16), Steps(&'static [(u32, i16)]) }` (ms, snr_x4).
  - `pub struct SimChannel { pub profile: SnrProfile, pub seed: u64, pub drop: Option<fn(u32, &[u8]) -> bool> }`: frame loss probability from margin `m = snr - floor(tier)` (x4): `m >= 8`: 0.5 %; `0 <= m < 8`: 10 %; `-8 <= m < 0`: 60 %; below: 100 %. A frame sent at tier A is only received by a radio listening on tier A. Half duplex: a radio that is sending hears nothing; overlapping sends collide (both lost). Delivery happens after `airtime_us`.
  - `pub struct Harness { pub a: Modem, pub b: Modem, pub now_ms: u32, .. }`: `new(chan: SimChannel) -> Self` (a named `CALLER`, b named `AXIS`, b has S0=1); `type_a(&mut self, s: &str)`, `type_b(..)`; `run_ms(&mut self, ms: u32)` (1 ms steps, ticks both every 10 ms); `screen_a() -> String`, `screen_b() -> String` (all host output so far); `send_a_data(&[u8])` (loops host_input until all accepted, advancing time); `received_b() -> Vec<u8>` and the mirror pair.
  - Demo `cargo run -p lfd3k-sim -- [--snr <dB>]`: CALLER types `ATV1`, `ATDT AXIS`, sends a 4 KB ANSI test screen, AXIS answers, sends one back, the SNR drops by 10 dB at t=30 s and recovers at t=60 s, CALLER sends `+++`, `ATH`. Prints both screens and a summary (tiers used, bytes, simulated seconds, retransmits).
  - `ffi`: `#[no_mangle] pub extern "C" fn lfd3k_core_version(out: *mut u8, len: usize) -> usize` (copies VERSION), panic handler that loops. Built with `cargo +esp build -p lfd3k-ffi --release --target xtensa-esp32s3-none-elf -Zbuild-std=core` (with `-C panic=abort` via `RUSTFLAGS` or `.cargo/config.toml` target section), output copied to `firmware/lib/liblfd3k_ffi.a` by `firmware/build_core.ps1`.
  - `firmware/`: PlatformIO `[env:heltec_v4]`, `platform = espressif32@6.11.0`, `board = esp32-s3-devkitc-1`, `framework = espidf`, `board_build.flash_size = 16MB`, `board_build.partitions = partitions.csv` (nvs, otadata, ota_0 1.5 MB, ota_1 1.5 MB, storage littlefs 1 MB, rest unused for now), linker flags to link `lib/liblfd3k_ffi.a`. `main.c` logs `LFD3K-SM core <version>` with `ESP_LOGI` once a second.
- [ ] **Step 1: Implement** the simulator and demo. Run `cargo run -p lfd3k-sim`: expected output ends with `NO CARRIER` on both screens and shows `CONNECT`, `RETRAIN` (down, then up).
- [ ] **Step 2: Implement** `ffi` and `firmware/`; run `firmware/build_core.ps1` then `pio run -d firmware -e heltec_v4`. Expected: `SUCCESS`, image size printed. Do not upload.
- [ ] **Step 3: Log** (FEATURES.md, THIRD_PARTY.md, brain note with the exact build commands and any toolchain gotchas). **Commit** `feat: simulator, demo, firmware skeleton`.

### Task 8: All tests

**Files:** Create `core/src/*` `#[cfg(test)] mod tests` blocks, `sim/tests/calls.rs`, `sim/tests/fuzz.rs`; modify `sim/Cargo.toml` (dev-dependency `proptest` 1.x, MIT/Apache), `THIRD_PARTY.md`.

- [ ] **Step 1: Unit tests (core).**
  - `tier::names_and_rates`: every tier's `name()` and `air_bps()` match Global Constraints exactly; `LADDER` is slowest to fastest; `from_index(index(t)) == Some(t)`; `from_index(8) == None`.
  - `tier::airtime_matches_reference`: SF7 BW500 CR4/5, 255-byte payload, 8-symbol preamble: `airtime_us` within 2 % of a value computed from the Semtech formula in the test; airtime strictly grows as SF grows for the same payload.
  - `tier::best_tier_picks_fastest_with_margin` (margin 20 = 5 dB): `best_tier(40, 20) == Sf5` (+10 dB); `best_tier(-24, 20) == Sf9` (-6 dB: SF9 needs -12.5 + 5 = -7.5, SF8 needs -5); `best_tier(-48, 20) == Sf11` (-12 dB); `best_tier(-160, 20) == Sf12` (nothing qualifies, falls back to the robust tier).
  - `frame::round_trip_every_type`, `frame::rejects_short_and_bad_type` (0, 1, 3 bytes; type 0 and 15), `frame::call_req_names` (valid, 17-char name rejected, non-printable rejected).
  - `at::parses_commands` (each command above, lowercase too), `at::result_strings` (exact verbose strings, e.g. `\r\nCONNECT LORA SF7 BW500 21875/ARQ\r\n`; numeric codes), `at::sregs_defaults_and_bounds`.
  - `arq::in_order_delivery_with_loss` (drop frames 2 and 5 of 8, resend, `read` returns the exact bytes in order), `arq::sack_resends_only_missing`, `arq::wraps_sequence_numbers` (send 600 chunks), `arq::push_respects_free`.
  - `modem::ati_banner` (exact two lines from Global Constraints).
- [ ] **Step 2: Scenario tests (`sim/tests/calls.rs`).**
  - `clean_call_connects_at_sf5`: SNR +12 dB, `ATDT AXIS` gives `CONNECT LORA SF5 BW500 62500/ARQ` on CALLER and a `CONNECT` on AXIS within 5 simulated s.
  - `weak_signal_connects_at_sf12`: SNR -16 dB connects at `LORA SF12 BW500`.
  - `data_arrives_intact_both_ways`: 16 KB random bytes each way at SNR 0 dB; received equals sent.
  - `retrain_down_and_up_keeps_data`: Steps profile +10 dB, then -12 dB at 20 s, then +10 dB at 60 s; a 32 KB transfer spanning the changes arrives intact; CALLER screen contains a slower `RETRAIN` then a faster `RETRAIN`; no `NO CARRIER` before `ATH`.
  - `link_loss_gives_no_carrier`: SNR drops to -40 dB at 10 s; both screens show `NO CARRIER` within S22 + 5 s; both modems accept `AT` with `OK` afterwards.
  - `hangup_both_sides`: `+++`, wait, `ATH`; both show `NO CARRIER`; `is_connected()` false on both.
  - Review Focus tests: `flow_control_never_loses_bytes` (CALLER pushes 64 KB as fast as `host_input` accepts; AXIS receives all 64 KB in order), `plus_plus_plus_without_guard_is_data` (data `a+++b` sent inside a stream arrives as data and CALLER stays connected), `lost_tierset_ack_resyncs` (drop hook drops the first `TierAck`; the call continues or ends with `NO CARRIER`, never hangs with `is_connected()` true and no traffic for over S22 s), `dial_unknown_name_gives_no_answer` (`ATDT NOBODY` gives `NO ANSWER` after S7 s, then `AT` gives `OK`), `stray_frames_ignored_while_idle` (inject a `Data` frame with a random call id into idle AXIS: no output, still answers a real call after).
- [ ] **Step 3: Fuzz (`sim/tests/fuzz.rs`, proptest).** `fuzz_frames_never_panic`: random byte vectors 0 to 300 long into `decode` and into `Modem::radio_rx` (idle and connected); `fuzz_at_lines_never_panic`: random strings into `host_input` in command mode. 10,000 cases each.
- [ ] **Step 4: Run everything.** `cargo test --workspace` (expected: all pass, 0 failures), `cargo clippy --workspace -- -D warnings` (expected: clean), `cargo run -p lfd3k-sim` (expected: demo transcript as in Task 7), `pio run -d firmware -e heltec_v4` (expected: SUCCESS). Save the demo transcript to `docs/phase1-demo.txt` and show it to Rev.
- [ ] **Step 5: Commit** `test: phase 1 unit, scenario and fuzz tests`. Update the brain LFD3K-SM note with the test count and the phase 1 status.
