# LFD3K-SM Voice Part 2 (Firmware) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real voice calls over the air: the PC app drives a real V4 or V3 over USB (session 0) or over Wi-Fi TCP (sessions 1 to 3), finds the modem by mDNS, and two PC apps call each other through the V4 and the V3 on the radio.

**Architecture:** The core already speaks the voice host protocol (`core/src/voice/host.rs`, part 1): session 0 is the host stream after `AT+VOICE`, which the firmware already pumps, so USB voice works on any build that has the voice core. This plan adds the rest: FFI calls for sessions 1 to 3 and the Scope feed, a firmware TCP voice listener (`voice_link.c`) pumped from the main loop, the voice port in the mDNS advert, real-modem backends in the PC app (serial, TCP, mDNS find), and a voice build of Rev's private V3 two-mode image. The modem still sees only sealed bytes.

**Tech Stack:** Rust (`core`, `ffi`, `apps/pc`), C on ESP-IDF via PlatformIO (`firmware/`), `serialport` 4 (MPL-2.0) and `mdns-sd` (MIT/Apache-2.0) in the PC app.

**Spec:** `docs/superpowers/specs/2026-11-12-voice-design.md` (approved 2026-11-12), sections 4, 6 and 10. Part 1's plan `docs/superpowers/plans/2026-11-12-lfd3k-sm-voice-1-pc.md` Task 7 defines every protocol message used here.

## Global Constraints

- License GPLv3 or later plus `NOTICE.md`; every new crate goes into `THIRD_PARTY.md` (name, version, license, why) as it lands and is named in the report.
- `core/` and `ffi/` keep no outside dependencies.
- The modem never sees keys or audio: voice and control bytes stay opaque.
- Voice port: **4220** TCP (not 23, not SimNet's 4210, not the PC app's 4211). mDNS: TXT key `voice=4220` on the existing `_lfd3k._tcp` service.
- TCP voice sessions are sessions **1 to 3**; session 0 stays the host stream (`VHP_SESSIONS` = 4).
- Access: accept a TCP voice client only when its address is inside the hotspot's subnet or the Wi-Fi client's subnet (spec 6); everyone else is closed at once.
- Rev's V3 two-mode image (branch `v3-two-mode`, worktree `F:\lfd3k-sm-v3`) is **private: never pushed, never public**. The V3 never gets `AT+MODE`.
- Flashing is Rev's call: ask before each board. V4 on COM11 here (modem slot ota_1 at 0x4A0000 only). V3 on the i5 (`/dev/ttyUSB0` there), flashed from the i5, its MAC (in the brain note) checked before any write, its call bridge (`~/emu/v3bridge.py`) stopped first and restarted after.
- Serial ports: Windows opens with DTR and RTS held False before open; Linux (CP2102) leaves them raised. Never toggle them one at a time.
- No em dashes or double hyphens in prose for Rev; no AI attribution in commits; scan history before any push; do not push (Rev decides).
- Tests are batched in the last task (Rev's standing rule); tasks below say which tests that task owns, and the last task writes and runs them.

## Review Focus

1. **The app dies without Leave** (killed, crash, laptop sleeps, phone Wi-Fi drops): the session must close within 5 s (`VOICE_HEARTBEAT_MS`), a call it owned hangs up, the TCP socket is closed and its slot reused, and USB returns to AT mode. Pinned by `voice_link_closes_dead_session` (bench) and `ffi_session_timeout_reports_closed` (Task 5).
2. **A fourth TCP client** while sessions 1 to 3 are open: it is closed at once and the three open sessions keep working. Pinned by bench step "fourth client" (Task 5).
3. **Partial and garbage bytes on a TCP session** (frames split across reads, a client sending random bytes or one huge line): nothing crashes, leftovers are kept when `lfd3k_voice_in` takes fewer bytes, garbage frames drop silently. Pinned by `ffi_voice_in_split_frames` and `ffi_voice_in_garbage_survives` (Task 5).
4. **Opening the serial port resets the board** (DTR/RTS): the PC app must open COM11 and the V3 without a reset. Pinned by bench step "open without reset" (the boot log never appears and the station stays in a call across an app restart).
5. **Voice and data on one modem at once** (V3: AXIS bridge in AT mode on its UART while a TCP voice app is registered): a data call still rings the bridge; a voice call while a data call is up stays silent (busy); the bridge works again after a voice call. Pinned by bench step "bridge still answers" (Task 5).

---

### Task 1: FFI voice sessions and the Scope feed

**Files:**
- Modify: `core/src/voice/host.rs` (one accessor), `ffi/src/api.rs`, `ffi/include/lfd3k.h`, `docs/FEATURES.md`

**Interfaces:**
- Consumes: `Modem::voice_open/voice_input/voice_output/voice_close/scope_sample` (part 1).
- Produces (core): `pub fn voice_session_open(&self, session: u8) -> bool` on `Modem` (false out of range).
- Produces (C ABI, all return 0/false before init):
```c
bool   lfd3k_voice_open(uint8_t session, uint32_t now_ms);   /* 1..3 */
size_t lfd3k_voice_in(uint8_t session, const uint8_t *buf, size_t len, uint32_t now_ms); /* bytes taken */
size_t lfd3k_voice_out(uint8_t session, uint8_t *buf, size_t cap);                       /* bytes written */
void   lfd3k_voice_close(uint8_t session, uint32_t now_ms);
bool   lfd3k_voice_alive(uint8_t session); /* false once the core closed it (timeout or Leave) */
```

- [ ] **Step 1:** Add `voice_session_open` (reads `self.vhp.s[session].open`).
- [ ] **Step 2:** Add the five functions to `ffi/src/api.rs` with `# Safety` docs in the file's style (null or valid for `len` / `cap`), and their prototypes in `ffi/include/lfd3k.h`.
- [ ] **Step 3:** In `lfd3k_poll`, wherever `d.trace.sample` takes a new sample, also call `d.modem.scope_sample(dbm, tx, now_ms)` with that sample, so open sessions get Scope frames (one sample per 50 ms, as the trace already does).
- [ ] **Step 4:** FEATURES.md: "Voice over TCP (sessions 1 to 3)" line under the voice host protocol section.
- [ ] **Step 5:** `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, existing tests pass; commit `ffi: voice sessions 1 to 3 and the Scope feed`.

### Task 2: Firmware TCP voice listener and the mDNS voice port

**Files:**
- Create: `firmware/src/voice_link.c`, `firmware/src/voice_link.h`, `multimode/tests/test_voice_peer.c` (or the firmware's own C test folder if the runner is moved; reuse `multimode/tests/run.ps1`'s compiler setup)
- Modify: `firmware/src/main.c` (start and pump), `firmware/src/mdns_link.c` (TXT `voice`), `firmware/CMakeLists.txt` or `firmware/src/CMakeLists.txt` (new source), `docs/FEATURES.md`

**Interfaces:**
- Consumes: Task 1's `lfd3k_voice_*`.
- Produces:
```c
#define VOICE_PORT 4220
void voice_link_start(void);               /* after Wi-Fi is up; the listener task */
void voice_link_pump(uint32_t now_ms);     /* main loop, same task and right after lfd3k_poll */
bool voice_peer_ok(uint32_t peer, uint32_t ap_ip, uint32_t ap_mask, uint32_t sta_ip, uint32_t sta_mask); /* network byte order; a zero ip means that interface is down */
```

- [ ] **Step 1: Listener task:** one FreeRTOS task with `listen(VOICE_PORT)`, non-blocking sockets and `select`; accept, check `voice_peer_ok` against the AP and STA `esp_netif_get_ip_info` (close and log `voice: refused <ip>` otherwise), take a free slot 1 to 3 (none free: close at once, log `voice: full`). Per slot two `StreamBuffer`s of 1536 bytes (socket to modem, modem to socket) and a state the main loop reads (`opening`, `open`, `closing`). The task copies socket bytes into the rx buffer (backpressure: stop reading that socket while the buffer is full, never drop) and tx buffer bytes to the socket. A socket error or EOF marks the slot `closing`.
- [ ] **Step 2: Main loop pump** (`voice_link_pump`, called right after `lfd3k_poll` in `main.c`, so every FFI call stays on the modem task): for `opening` slots call `lfd3k_voice_open` (false: mark `closing`); for open slots feed rx bytes through `lfd3k_voice_in` keeping what it did not take for the next pump, drain `lfd3k_voice_out` into the tx buffer only while the buffer has room for the bytes (keep them otherwise), and when `lfd3k_voice_alive` turns false close the socket; for `closing` slots call `lfd3k_voice_close` and free the slot.
- [ ] **Step 3: mDNS:** add TXT item `voice=4220` beside `name` and `ver` in `mdns_link.c` (both where the service is added and where TXT is updated on rename).
- [ ] **Step 4:** `voice_peer_ok` is a pure function in `voice_link.c` behind a small header section the C test can include; its test cases are written in Task 5.
- [ ] **Step 5:** `powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1`, then `pio run -d firmware -e heltec_v4` and `pio run -d firmware -e heltec_v3_single` build with no new warnings; note flash and RAM in the commit message body. FEATURES.md: the voice port, the subnet rule (a known limit: anyone on the LAN can register a name), three sessions. Commit `firmware: TCP voice listener on 4220 and the mDNS voice port`.

### Task 3: PC app on a real modem (USB, TCP, mDNS find)

**Files:**
- Create: `apps/pc/src/link.rs`, `apps/pc/src/find.rs`
- Modify: `apps/pc/src/main.rs` (args), `apps/pc/src/modem_sim.rs` (the worker gets a backend), `apps/pc/Cargo.toml`, `THIRD_PARTY.md`, `docs/voice-pc.md`, `docs/FEATURES.md`

**Interfaces:**
- Consumes: the voice host protocol frames (COBS, part 1 Task 7); `Engine::on_modem_bytes`, `Engine::take_modem_bytes` (the engine already sends Heartbeat every 1.5 s).
- Produces:
```rust
// link.rs
pub enum ModemSpec { Sim, Serial(String), Tcp(String, u16), Find(Option<String>) }
pub fn parse_modem_spec(s: &str) -> Result<ModemSpec, String>; // "sim", "COM11", "/dev/ttyUSB0", "tcp:HOST[:PORT]" (port default 4220), "find", "find:STATION"
pub enum ModemLink { Serial(SerialLink), Tcp(TcpLink) }
impl ModemLink {
    pub fn open(spec: &ModemSpec, timeout_ms: u32) -> Result<ModemLink, String>; // Serial: AT+VOICE then framed mode
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String>;               // non-blocking, 0 when nothing
    pub fn close(self);                                                            // sends Leave first
}
pub fn voice_handshake(reply: &[u8]) -> HandshakeStep; // pub enum HandshakeStep { Need, Ok(usize /* bytes used */), Error(String) }
// find.rs
pub fn voice_port_from_txt(txt: &[(&str, &str)]) -> Option<u16>;
pub fn find_modem(station: Option<&str>, timeout_ms: u32) -> Result<(String /* station */, std::net::SocketAddr), String>;
```

- [ ] **Step 1: Args:** `--modem <spec>` (default `sim`, today's behavior unchanged). `--bind`, `--peer`, `--snr`, `--station` apply to `sim` only (an error with a real modem). With a real modem the station name comes from HelloAck.
- [ ] **Step 2: Serial:** `serialport` 4; on Windows set DTR and RTS false before the port opens (`dtr_on_open(false)` plus the crate's RTS equivalent; if the crate cannot hold RTS before open, say so in the report and measure on the bench), on Linux leave both raised. 115200 8N1. Send `AT+VOICE\r`, read until `voice_handshake` returns Ok (it skips the echo and blank lines and finds `OK`; `ERROR` or 3 s without OK is an error naming the port). Bytes after OK belong to framed mode.
- [ ] **Step 3: TCP and find:** `TcpStream` with nodelay, non-blocking after connect (5 s connect timeout). `find_modem` browses `_lfd3k._tcp.local.` with `mdns-sd` for up to `timeout_ms`, takes the first service with a `voice` TXT port (or the one whose `name` TXT matches `station`, case-insensitive) and returns its IPv4 address and port.
- [ ] **Step 4: Worker backend:** the worker's modem side becomes `enum Backend { Sim { modem, radio }, Real(ModemLink) }`; engine bytes go to the backend and backend bytes to the engine each tick; a link error shows `MODEM LOST: <reason>` and the app returns to the start screen. `--cap-tier` on a real modem: read S21 with an `AtLine` `AT&V` (parse `S21=`), send `ATS21=<n>`, and restore the old value on exit (no AT&W). Quit sends Leave and closes the link.
- [ ] **Step 5:** THIRD_PARTY.md rows for `serialport` and `mdns-sd`. `docs/voice-pc.md`: a "With a real modem" section (`--modem COM11`, `--modem find`, `--modem tcp:<ip>`, headset advice, one app per modem session). FEATURES.md. Build, clippy, existing tests pass; commit `voice-pc: real modems over USB, TCP and mDNS`.

### Task 4: Voice build of Rev's V3 two-mode image (private)

**Files:**
- Modify (worktree `F:\lfd3k-sm-v3`, branch `v3-two-mode`, never pushed): merge `main` in.

- [ ] **Step 1:** In `F:\lfd3k-sm-v3`, `git merge main` (main is far ahead: the site redesigns, the LRA rename, release images, voice parts 1 and 2). Conflicts resolve to main's version except the V3 two-mode pieces this branch owns (`ModeTarget::Bench`, `Modem::set_mode_layout`, the per-board mode table in `mode_switch.c`, env `picker_v3` and `heltec_v3`, `multimode/partitions-multimode-v3.csv`). Ledger every conflict and its resolution.
- [ ] **Step 2:** `cargo test --workspace` passes on the branch; `pio run -d firmware -e heltec_v3` and `-e picker_v3` build. Rebuild the full image with `F:\lfd3k-sm\backups\v3-pieces\build_v3_two_mode.ps1` only if the picker changed; otherwise the bench flashes the modem slot ota_1 (0x220000) alone.
- [ ] **Step 3:** Commit the merge on `v3-two-mode`. Do not push.

### Task 5: Tests, bench with the boards, recordings and hand-over (all testing, Rev's rule)

**Files:**
- Create: `ffi/tests/voice_sessions.rs` (or `#[cfg(test)]` in `ffi/src/api.rs`, following the crate's existing test style with `fake_hal`), `multimode/tests/test_voice_peer.c`, `apps/pc/tests/link.rs`
- Modify: `multimode/tests/run.ps1` (add the new C test), `docs/FEATURES.md`, `docs/voice-pc.md`, `D:\brain\Projects\LFD3K-SM.md`

- [ ] **Step 1: FFI tests:** `ffi_voice_hello_roundtrip` (open session 1, COBS Hello in, HelloAck with the station name out), `ffi_voice_session_zero_refused` (`lfd3k_voice_open(0, ..)` false), `ffi_voice_out_before_init_is_zero`, `ffi_voice_in_split_frames` (a Hello split into 1-byte writes is answered once), `ffi_voice_in_garbage_survives` (10,000 random bytes, then a good Hello still answered), `ffi_session_timeout_reports_closed` (no frames for 5,001 ms of polls: `lfd3k_voice_alive` false), `ffi_scope_frames_flow` (an open session gets a 0x90 Scope frame after 5 polls 50 ms apart).
- [ ] **Step 2: C test** `test_voice_peer.c`: hotspot 192.168.4.1/24 and STA 192.168.1.159/24: 192.168.4.2 ok, 192.168.1.20 ok, 10.0.0.5 refused, 192.168.5.2 refused, STA down (ip 0) and 192.168.1.20 refused, both down refuses everything.
- [ ] **Step 3: PC app tests:** `parse_modem_spec_cases` (sim, COM11, /dev/ttyUSB0, tcp:10.1.2.3, tcp:host:4300, find, find:AXISMODEM-E4B0, an empty or `tcp:` spec is an error), `handshake_skips_echo` (`AT+VOICE\r\r\nOK\r\n` then a frame: Ok with the right byte count), `handshake_error` (`ERROR\r\n`), `voice_port_from_txt_cases` (`voice=4220` gives 4220; missing, `voice=abc`, `voice=0` give None), `tcp_link_roundtrip` (a local listener replies with a HelloAck frame; the link reads it whole).
- [ ] **Step 4: Run everything:** `cargo test --workspace` (report the total and the count before this plan), clippy with `-D warnings`, `cargo fmt --check`, `multimode/tests/run.ps1`, `firmware\build_core.ps1`, then `pio run` for `heltec_v4`, `heltec_v4_single`, `heltec_v3_single` and (in the V3 worktree) `heltec_v3`; report flash and RAM for each.
- [ ] **Step 5: Flash (ask Rev first, one board at a time):** V4 on COM11: read the MAC first and compare with the brain's V4 note, flash only ota_1 at 0x4A0000 with the multimode `heltec_v4` app; `ATI` shows the new build, `AT&V` RADIO OK, Wi-Fi kept. V3 on the i5: stop `~/emu/v3bridge.py` from a script (never `pkill -f` inline in ssh), check its MAC (brain note), flash the `heltec_v3` app to ota_1 (0x220000), `AT&V` on the i5 shows the new build, restart the bridge with its old arguments.
- [ ] **Step 6: Wi-Fi for the V3:** the V3 needs its Wi-Fi client on Rev's network for the TCP bench. Stop the bridge, open a terminal on the i5's `/dev/ttyUSB0` for Rev (in a Terminal tab), and Rev types `AT+WSTA=...` and `AT&W` himself (Claude never sees or types the password). Restart the bridge.
- [ ] **Step 7: Bench (one call per check, keep calls few):** on this PC, app A `--modem COM11 --name REV --label PC` and app B `--modem find:AXISMODEM-E4B0 --name SHOP --label PC`. Check and screenshot each: B found the V3 by mDNS; **open without reset** (A's open prints no boot log; restart A during idle and the V4 keeps its uptime on `AT&V`); A calls SHOP, B rings, answer, words match, talk both ways at the tier the link gives; `--cap-tier SF9` and `SF12` show PTT and DELAYED; hang up from each side; decline; the monitor shows real Scope and AirEvents; **app dies without Leave** (kill B mid-call: the call ends on A within about 5 s, B restarts and registers again); **fourth client** (three `nc`-style TCP clients to the V3:4220 from this PC, a fourth is closed, B still works); **bridge still answers** (a data call from the V4 terminal to AXISMODEM-E4B0 reaches the i5 AXIS with B registered; a voice call during that data call stays silent; voice works again after). Read the V3's `AT&V` HEAP MIN and STACK FREE before and after (stop and tell Rev if HEAP MIN falls under 30 KB).
- [ ] **Step 8: Radio recording (promised in part 1):** play the Bark AXIS BBS conversation sides through A and B (`--play`/`--record`) at SF5, SF7, SF9 and SF12 (one call each), and write stereo WAVs plus a timing line per tier into `docs/voice-bench/radio/` (each under about 3 MB). Capture footage for the testing video into `local/video-src/radio/` (not in git): app window PNGs, the boards' OLEDs through `AT+FB`, and a `timeline.csv`.
- [ ] **Step 9: Hand-over:** FEATURES.md statuses, `docs/voice-pc.md`, the brain note's Voice section (what works on hardware, test count, heap numbers, WAV folder, how to run). Commit `voice part 2: tests, bench and radio recordings`. Do not push; send Rev the screenshots and WAVs.

## Not in this plan

- **The V4 as a USB drive with install scripts** (Rev's option 1, noted for part 2 in the brain): it moves the V4's USB port from the chip's built-in serial to a software USB stack (TinyUSB composite CDC plus MSC), which changes how COM11 is flashed and how the other three modes find their USB port, and it needs a new 128 KB partition at 0xFE0000 and a published PC app release. Left out on purpose; Rev decides whether it becomes its own plan.
- Android app (part 3) and group channels (part 4).
