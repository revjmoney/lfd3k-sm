# LFD3K-SM features log

## Features

| Name | Status | Date |
|------|--------|------|
| Speed tier ladder: 8 LoRa tiers SF12 to SF5 at BW 500 kHz, CR 4/5 (1172 to 62500 bps air rate), SNR floors, best-tier pick, time-on-air estimate | done | 2026-11-09 |
| Frame format: 5 byte header (air id byte first), 11 frame types, validated encode/decode (no panics), CallReq name payloads, Radio trait | done | 2026-11-09 |
| AT command set: ATD/ATDT, ATA, ATH, ATO, ATI, ATV, ATE, ATSn=v, ATSn?, AT&V, ATZ; verbose and numeric result codes; S registers with bounds | done | 2026-11-09 |
| Selective repeat ARQ: window 8, 200 byte chunks, SACK bitmap, timeout retransmit, wrapping u8 seq | done | 2026-11-09 |
| Call setup and training: ATDT by name on SF12 (DIALING, RINGING), RING and auto-answer (S0), CallAck plus Train with measured SNR, tier pick with S20 margin and S21 cap, TierSet and TierAck, Keepalive probe with step-down to SF12, CONNECT | done | 2026-11-09 |
| Half duplex air manners: MORE flag bursts of up to 4 frames, turn handover, acks at burst end, keepalives on quiet calls | done | 2026-11-09 |
| Auto retrain: SNR average (1/8), step down on low margin or 4 RTOs of silence, step up after 10 s of good margin, RETRAIN in verbose mode, data survives tier changes | done | 2026-11-09 |
| Resync: 8 RTOs of silence drops both ends to SF12, caller pings, then climbs back to the best tier the SNR allows, capped one tier below the tier that died; NO CARRIER after S22 seconds of silence | done | 2026-11-09 |
| Escape and online command mode: guard time S12, three S2 characters, guard time, OK; ATO back online; ATH hangs up (Hangup sent twice) | done | 2026-11-09 |
| ATI banner: name, version with release name, release tagline | done | 2026-11-09 |
| PC simulator: seeded half duplex LoRa channel (SNR profiles, loss by margin, collisions, drop hook, frame trace) and a two modem harness (CALLER and AXIS) | done | 2026-11-10 |
| Phase 1 demo: `cargo run -p lfd3k-sim -- [--snr <dB>] [--seed <n>] [--out <file>] [--trace <file>]`, two 4 KB ANSI screens, SNR dip and recovery, escape, ATO, AT&V, ATH | done | 2026-11-10 |
| Air id: every frame starts with one byte 0xD1 (0xD = LFD3K-SM, 1 = air protocol version 1); any other first byte decodes as Foreign and is ignored. Max payload 250 bytes | done | 2026-11-10 |
| Calling channel: 918.5 MHz (`CHANNEL_HZ`, SX1262 frequency word 0x3968_0000), BW 500 kHz, CR 4/5, preamble 8 symbols (12 at SF5 and SF6) | done | 2026-11-10 |
| SF5 and SF6 time on air fixed to the SX1261/2 datasheet form (payload numerator 8*PL + 16 - 4*SF + 20); SF5 with 1 payload byte is 2320 us | done | 2026-11-10 |
| TX power setting S30 (SX1262 chip output in dBm, signed) and the TX POWER line in AT&V | done | 2026-11-10 |
| FFI device glue: one static Modem plus SX1262 driver behind a C ABI (`lfd3k_init`, `lfd3k_poll`, `lfd3k_host_in`, `lfd3k_host_out`, `lfd3k_take_fb_request`, `lfd3k_status`), S30 changes applied to the radio at runtime, device panic handler stores `file:line` through the HAL and restarts | done | 2026-11-10 |
| AT+FB frame buffer dump request (the modem only raises a flag; the firmware writes the dump and OK), LAST CRASH line on ATI, RADIO line in AT&V | done | 2026-11-10 |
| Firmware platform: modem task (core 1, priority 20) runs the Rust core; SX1262 on SPI2 with the GC1109 front end, native USB carries only the modem stream (console on UART0), station name AXISMODEM-XXXX from the MAC, AT+FB dump streamed without blocking, crash note kept across the restart | done | 2026-11-10 |
| Firmware skeleton (Heltec V4, ESP-IDF via PlatformIO): links the Rust core as a static library; 16 MB flash, two 1.5 MB OTA slots, 1 MB littlefs. Phase 1 logged the core version once a second; phase 2 replaced that with the modem task (see Firmware platform above) | done | 2026-11-10 |
| Phase 3 task 1, core settings: hotspot, client network, up to 8 forwards (name, host, port, telnet or raw), published forward, S registers, E and V in one `Settings` model with a 899 byte versioned blob (magic LFS1, CRC-16/CCITT-FALSE, refused whole on any bad field); AT+WAP, AT+WSTA, AT+WIFI?, AT+FWD, AT+PUB, AT&W, ATZ reload; ATDT resolves a forward name or host:port (replies NO CARRIER until the Wi-Fi dial lands) | done | 2026-11-10 |
| Phase 3 task 2, wire call path: ATDT to a forward or host:port opens a TCP stream through the platform (`take_wire_request`, `wire_connected`, `wire_failed`, `wire_closed`, `wire_input`, `wire_output`, `take_wire_close`), prints DIALING, then CONNECT WIFI TCP, data mode with escape, ATO and ATH as on radio, NO CARRIER on refuse, close or S7 timeout; answered radio calls bridged to the AT+PUB forward (BRIDGE <name> on the host); telnet client filter for T mode (answers the AXIS negotiation, strips commands, doubles 0xFF, CR NUL without BINARY); two 2 KB wire rings with back pressure both ways; BUSY to radio callers during a Wi-Fi call; `wire_state()`; FakeWire server in the simulator | done | 2026-11-10 |
| Phase 3 task 4, firmware Wi-Fi, NVS and TCP: settings blob saved in NVS (namespace lfd3k, key settings; first boot generates a 12 character hotspot password with the hardware RNG and saves it; a save asked during a call waits until the call ends); WPA2 hotspot on channel 6 (max 4 clients, only when on) and client mode with 1, 2, 4 up to 30 s retry backoff, driver storage in RAM so the client password is never kept by the Wi-Fi driver; live `WIFI AP .. CLIENTS .. STA ..` status line once a second; one TCP client task (DNS, 15 s non-blocking connect, NODELAY, 2 KB stream buffers each way) joined to the wire call path with a socket generation counter; STACK FREE reported once a second | done (not yet flashed) | 2026-11-10 |
| Phase 3 task 4c, SimNet test mode (call the V4 from the PC over Wi-Fi with a simulated radio): `AT+SIMNET=<pc ip>` puts the SX1262 in standby and runs the radio over UDP port 4210 (`SimNetRadio` in core/src/simnet.rs, datagram `'L' 'S' 1 tier snr_x4 rssi` plus the frame, sender busy for the frame's airtime, receiver holds each frame for its airtime), `AT+SIMNET=0` back to the SX1262, never saved (every boot is on the real radio), AT&V `RADIO SIMNET <ip>`, OLED shows `SI` in place of `HS`; firmware UDP task (udp_link.c); PC far modem `cargo run -p lfd3k-sim --bin lfd3k-pcmodem` (real core, line mode terminal, `--snr`, `--profile dip`, `--fwd`, real TCP for Wi-Fi calls and bridges, `--script` mode). See docs/simnet.md | done (not yet flashed) | 2026-11-10 |
| Phase 3 task 5, Wi-Fi on the front panel: idle name row rotates every 3 s between the board name, `AP <ssid>`, `PW <pass>` (only while `PWSHOW 1`) and `IP <a.b.c.d>` (only with a client address); wire call screens `WIFI TCP` and `BRIDGE`; AT&V `PWSHOW <0|1>`; bench terminal redacts passwords and takes safe screenshots; docs/phase3-bench.md | done (not yet flashed) | 2026-11-10 |

## Settings

| Name | Default | Range |
|------|---------|-------|
| S0 auto-answer rings (0 = off) | 0 | 0 to 255 |
| S2 escape character | 43 (+) | 0 to 127 |
| S7 carrier wait seconds | 30 | 1 to 255 |
| S12 escape guard time, 1/50 s | 50 | 0 to 255 |
| S20 training margin, dB x4 | 20 (5 dB) | 0 to 200 |
| S21 tier cap, tier index (7 = SF5) | 7 | 0 to 7 |
| S22 resync timeout seconds | 20 | 1 to 255 |
| S30 TX power, dBm at the SX1262 chip (the V4 GC1109 adds about 12 dB, so the default is about 14 dBm out) | 2 | -9 to 22 |
| Hotspot on or off (AT+WAP=1 / AT+WAP=0) | on | on, off |
| Hotspot name and password (AT+WAP=ssid,pass) | modem name, generated | name 1 to 32 printable, password 8 to 63 printable, neither may contain a comma |
| Client network (AT+WSTA=ssid,pass) | none | same limits; AT+WSTA=0 forgets it |
| Forwards (AT+FWD) | none | 8 slots; name 1 to 16 printable without `:` or `,` (and not `0`), host 1 to 63 of `A-Za-z0-9.-`, port 1 to 65535, mode T or R (default T for port 23, else R) |
| Published forward (AT+PUB) | none | one forward name |

## AT commands

One command per line, case-insensitive. Out-of-range S register writes are rejected.

| Command | Meaning |
|---------|---------|
| AT | Attention, replies OK |
| ATD<name>, ATDT<name>, ATDP<name> | Dial a station by name (1 to 16 chars). A name that matches a forward, or the form `host:port`, is a Wi-Fi dial instead (see notes) |
| ATA | Answer |
| ATH, ATH0 | Hang up |
| ATO | Return online |
| ATI | Banner, version and release name, tagline |
| ATV0, ATV1 | Numeric or verbose result codes |
| ATE0, ATE1 | Echo off or on |
| ATSn=v, ATSn? | Set or read S register |
| AT&V | Show configuration (now with WIFI, PUB and FWD lines) |
| AT&W | Save the settings (the platform stores them; replies OK) |
| AT+WAP? | Print `WAP <ON|OFF> <ssid> <password>` (the hotspot password is shown on purpose; tools redact lines starting `WAP `) |
| AT+WAP=<ssid>,<pass> | Set the hotspot name and password (password 8 to 63 chars) |
| AT+WAP=1, AT+WAP=0 | Hotspot on or off |
| AT+WSTA? | Print `WSTA <ssid> ********` or `WSTA OFF` (the client password is never printed) |
| AT+WSTA=<ssid>,<pass> | Set the client network (password 8 to 63 chars) |
| AT+WSTA=0 | Forget the client network |
| AT+WIFI? | Print the platform's live Wi-Fi status line, or `WIFI OFF` before it reports one |
| AT+FWD? | List forwards as `FWD <name> <host>:<port> <T|R>` (wire mode) |
| AT+FWD=<name>,<host>,<port>[,T\|R] | Add or replace a forward (name compares case-insensitively). ERROR if all 8 slots are used |
| AT+FWD=<name> | Delete a forward (ERROR if there is none; clears AT+PUB if it pointed there) |
| AT+PUB? | Print `PUB <name>` or `PUB NONE` |
| AT+PUB=<name> | Bridge answered radio calls to this forward (ERROR if it does not exist) |
| AT+PUB=0 | Publish nothing |
| AT+FB | Dump the frame buffer (answered by the firmware: no OK from the modem) |
| AT+SIMNET=<a.b.c.d> | Test mode: use the simulated radio over UDP with that peer (the PC running lfd3k-pcmodem) instead of the SX1262. ERROR during any call. Not saved by AT&W |
| AT+SIMNET=0 | Back to the SX1262 (ERROR during any call) |
| AT+SIMNET? | Print `SIMNET <ip>` or `SIMNET OFF` |
| ATZ | Reset |

Result codes: OK 0, CONNECT 1, RING 2, NO CARRIER 3, ERROR 4, BUSY 7, NO ANSWER 8.
Verbose CONNECT reads CONNECT <tier name> <air bps>/ARQ on radio and CONNECT WIFI TCP on a Wi-Fi call. RETRAIN lines are verbose only.

Notes:
- ATD strips one leading T or P, as Hayes does. To dial a name that starts with T or P, use the full prefix: ATDT TOM or ATDP PAT (ATDTOM would dial OM).
- Names compare case-insensitively.
- Escape: no data for S12 (1/50 s), then three S2 characters each less than S12 apart, then nothing for S12. Anything else is data. S12=0 turns the escape off.
- Data mode is 8-bit clean: a RETRAIN during data mode is held and printed after the OK of the escape (verbose mode). AT&V shows the current tier and the last 4 retrains. ATO prints CONNECT with the current tier. NO CARRIER prints right after the received data that fits in the host buffer, never later.
- A tier above a modem's S21 cap is never used: asked for one, the modem acks its cap instead, and the asker stops asking above it.
- ATH during a call prints NO CARRIER; with no call it prints OK. ATZ restores the saved settings (S registers, echo, verbose, hotspot, client network, forwards, publish; power-on defaults when nothing was loaded or saved) and hangs up silently.
- ATA with nothing ringing prints NO CARRIER; ATA in online command mode (a call is up) prints ERROR and the call stays up.
- A partial command line typed before a call connects (during RING or while dialing) is dropped at CONNECT.
- The station name is 1 to 16 printable ASCII characters without `:` (Config::new refuses anything else), so a dial like `BBS:1` is always host:port, never a radio name. Radio dial names containing `:` print ERROR.
- ATI prints three lines: LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM, then v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M, then S0N1C B00M R4D10 PR0 MAX.
- ATI gets a fourth line `LAST CRASH: <note>` only when the previous boot ended in a panic (the note is `file:line`, up to 64 printable characters). With no crash it prints three lines, as before.
- AT&V prints `RADIO OK` or `RADIO FAIL <code> ERR 0x<4 hex>` after the TX POWER line, once the firmware has brought the radio up. Code -1 is a BUSY timeout, -2 no chip, -3 bad config; the hex word is the chip's device errors.
- S30 is the only signed register: ATS30=-9 is accepted (a leading minus works for S30 only), ATS30? prints the signed value, anything outside -9 to 22 prints ERROR. AT&V shows `TX POWER S30=<n> dBm chip, about <n+12> dBm out`.
- Command lines are up to 128 bytes. `+` command arguments split on commas: an SSID, forward name or password may not contain a comma. SSID and password are taken as typed; the fields of AT+FWD are trimmed, so `AT+FWD=bbs, bbs.example.com, 23` works.
- ATDT order: a forward name (any case) first, then `host:port`, else a radio dial. Use `ATDT`, not bare `ATD`, for host names that start with T or P.
- Wi-Fi dial (forward or host:port): `DIALING <target>` (verbose), then `CONNECT WIFI TCP` (numeric 1) when the platform's TCP connect succeeds, and data mode. No fake speed in the connect string. A refused or failed connect prints NO CARRIER; no connect within S7 seconds prints NO CARRIER and closes the attempt. When the far end closes, everything it sent is shown first (data mode), then NO CARRIER. The escape, ATO (prints CONNECT WIFI TCP again) and ATH (NO CARRIER, also while dialing) work as on a radio call; ATZ hangs up silently. ATDT or ATA while any call is up prints ERROR. A Wi-Fi dial also prints ERROR while the previous connection's close is still pending (the platform has not released that socket yet); try again a moment later.
- Wire mode: telnet (T) or raw (R), from the forward, or for host:port T on port 23 and R otherwise. In T mode the modem is a telnet client: it answers WILL ECHO, WILL SGA and WILL BINARY with DO, DO SGA and DO BINARY with WILL, DO NAWS with WILL NAWS plus an 80 x 25 window size, refuses everything else (DONT, WONT), never sends anything first, strips all commands and subnegotiations from what it receives (a subnegotiation is dropped whole up to IAC SE, however long; IAC IAC becomes one 0xFF; while the server does not send BINARY, CR NUL becomes CR), and sends 0xFF as IAC IAC. While our sending direction is not BINARY (the server has not said DO BINARY), CR LF goes out as CR LF and any other CR as CR NUL (RFC 854); a CR that ends the data so far is held until the next byte or 50 ms, then sent as CR NUL. Raw mode passes bytes untouched.
- Bridge (AT+PUB set): a radio call this modem answers is connected to the published forward only, never to any other host. The radio caller sees its normal CONNECT; what it sends before the wire is up is buffered (up to 2 KB, then the radio window holds it). The local host stays in command mode and sees `BRIDGE <name>` (verbose) once the wire connects, and NO CARRIER when the bridge ends. If the wire fails, does not connect within S7 seconds, or closes (after what it sent has reached the caller, at most S7 seconds), the radio call is hung up and the caller sees NO CARRIER. If the radio call drops, or the host types ATH or ATZ, the wire is closed. ATO during a bridge prints ERROR.
- One call at a time: a radio caller during a Wi-Fi call gets BUSY.
- `Modem::wire_state()`: 0 none, 1 dialing, 2 online, 3 bridging. `call_state()` shows a Wi-Fi call as dialing (1) or online (4, or 5 in online command mode). Lights during a Wi-Fi call: OH on from the dial, CD on once connected (and until NO CARRIER), SD and RD lit for 100 ms after wire traffic once connected, HS and EC off, no tier.
- AT&V adds `WIFI AP <ON|OFF> <ssid>  STA <ssid|OFF>`, `PWSHOW <0|1>` (1 while the generated hotspot password may be shown on the OLED), `PUB <name|NONE>` and `FWD <count>` after the RADIO line. When the firmware has reported the modem task's stack high-water mark (`lfd3k_set_stack_free`), AT&V ends that group with `STACK FREE <bytes>` (omitted until reported).
- AT&W sets a save-wanted flag the platform takes (`Modem::take_save_request`) and reads with `Modem::settings()`. `Modem::load_settings` applies a loaded copy at boot, including S0 auto-answer and S30. ATZ restores that copy.
- SimNet (AT+SIMNET): while it is on, AT&V prints `RADIO SIMNET <ip>` in place of the RADIO OK line (`RADIO SIMNET <ip> NET FAIL` when the firmware could not open the UDP socket; the modem stays on SimNet, `AT+SIMNET=<ip>` again retries and `AT+SIMNET=0` goes back to the SX1262), nothing is sent on RF (the SX1262 is in standby), `lfd3k_status_t.simnet` is 1 and the OLED labels the HS light `SI`. Datagrams go to `<ip>:4210` and only datagrams from `<ip>` are taken. A frame on a tier the modem is not listening on is not heard, as on the air. The idle scope shows the simulated channel (the RSSI the peer reports while a frame is received, else -110 dBm). The V4 tells the peer it hears it at +8 dB, -70 dBm. Steps for Rev: docs/simnet.md.
- `Modem::take_wifi_changed()` is true once after AT+WAP (any form that took effect), AT+WSTA (any form that took effect), ATZ or a settings load; the platform then re-reads `Modem::settings()` (a reference) and re-applies Wi-Fi. `note_ap_client_joined` also asks for a save.
- The hotspot password may be shown on the OLED (`Modem::show_ap_pass`) until a device joins (`note_ap_client_joined`) or AT&W saved with a client network set; the flag is part of the saved blob.

## Front panel (OLED)

- 0.96 inch SSD1306 (128x64) front panel drawn by the panel task (core 0, priority 2, 20 fps cap). The modem task publishes a status copy each loop; the panel never calls the core.
- Top row: eight blinky lights OH CD SD RD HS EC DC AA, boxed and filled when lit. SD and RD are held lit 80 ms so short bursts stay visible. DC stays dark until compression exists. The board LED (GPIO35) follows SD or RD.
- Middle: the state word (IDLE, DIALING, RING, TRAINING, ONLINE, CMD) or, in a call, the tier (`SF7 BW500`) with `ONLINE 21875 BPS` (or `CMD ...` in online command mode) and an eight cell tier ladder. `RADIO FAIL` replaces it when the radio is not usable.
- Bottom: `LFD3K-SM`, the last heard RSSI (dBm) and SNR (dB), and an RSSI bar (-130 to -30 dBm) and SNR bar (-20 to +10 dB).
- Animations: OH blinks while dialing (2.5 Hz), a sweep runs across the eight tiers while training and the chosen tier flashes when training ends, idle shows a breathing carrier bar (2 s), RING flashes inverted.
- Idle animations: the IDLE word disintegrates and reintegrates with flying 0 and 1 bits; a breathing scope draws the live radio channel (one RSSI sample every 50 ms from the SX1262, 64 samples, transmit samples marked); the idle meters are live; the lights shuffle on an 8 s loop, with the home order restored on every fourth loop.
- Live channel trace: `lfd3k_poll` samples GetRssiInst (one 3 byte SPI transaction) every 50 ms while listening and marks samples taken during a transmit; the modem task hands the 64 sample ring to the panel with each status copy.
- Wi-Fi rows (idle only): the name row (y 46..52) rotates every 3 s through the board name, `AP <ssid>` (when the hotspot is on), `PW <pass>` (only while the core says `PWSHOW 1`: until a device joins the hotspot or `AT&W` saved with a client network set) and `IP <a.b.c.d>` (when the client side has an address). Each row is cut to 21 characters. The password is never drawn when `PWSHOW` is 0 and is not even handed to the renderer then. The modem task fills `panel_net_t` once a second (`panel_render_set_net`, same pattern as the trace) and wipes its copies. The idle animations and live meters are untouched.
- Wire call screens: dialing shows `DIALING` with the dots; a Wi-Fi TCP call online shows big `WIFI TCP` with `ONLINE` (or `CMD` in online command state) and no tier ladder; a radio call bridged to a forward shows big `BRIDGE` with the radio tier line (`SF7 BW500 21875 BPS`) and the ladder. A pure Wi-Fi call does not show `RADIO FAIL`.
- Bench terminal secrets (tools/lfd3k_term.py): output and logs show `********` for the rest of a `WAP ` line after ON or OFF, after `PW ` on any line, and after the comma of an echoed `AT+WAP=` or `AT+WSTA=`; `--allow-secret` lifts it on the screen only (logs are always redacted). `--shot` needs `--shot-safe` (reads AT&V, sends AT+FB only after `PWSHOW 0`) or `--allow-secret`. A `--shot` or `--log` path inside the repo docs folder is refused unless AT&V says `PWSHOW 0`.
- Boot splash for 1 s: `LFD3K-SM`, `r.420`, `BL4Z3B4ND`.
- `panel_snapshot` returns the last drawn frame (1024 bytes, SSD1306 page order, bit 0 = top row) for AT+FB. The drawing code is the pure function `panel_render` (no ESP-IDF calls) so the PC tool can render the same screens.
- Own hand-made 5x7 ASCII font (font5x7.h), no library fonts.

## Change log

- 2026-11-10: Phase 3 task 6, tests: 72 new Rust tests (core integration 27 in `core/tests/telnet.rs` and `settings.rs`, sim 35 in `wire.rs`, `simnet.rs`, `phase3_props.rs`, ffi host 10 in `host_api.rs`) and 25 Python tests (`python -m unittest tools/test_lfd3k_term.py`, fakes only). They cover the telnet filter and CR rule, settings blob and AT commands, wire calls and the radio to forward bridge, SimNet alone and two modems over loopback UDP, the C ABI for settings, wire, SimNet and the channel trace, and the bench terminal redaction and PWSHOW gate. phase1-demo.txt regenerated (AT&V Wi-Fi lines).
- 2026-11-10: Phase 3 task 5, Wi-Fi rows and wire screens on the OLED, AT&V `PWSHOW`, bench terminal redaction and `--shot-safe`, panel_check covers the net rows and wire screens (new mockups idle_ap, idle_ip, idle_pw, wifi_tcp, bridge, with a fake password), docs/phase3-bench.md. No new Rust tests (task 6); the existing 72 pass.
- 2026-11-10: Phase 3 task 2 fix round 1: a long subnegotiation never leaks into data, the CR rule follows our sending direction only, CR LF stays CR LF and a lone CR is held up to 50 ms (RFC 854), incoming CR NUL becomes CR when the server is not BINARY, Wi-Fi dial is ERROR while a close is pending.
- 2026-11-10: Phase 3 task 2, wire call path (core/src/wire.rs), telnet client filter (core/src/telnet.rs), radio to forward bridge, BUSY during a Wi-Fi call, FakeWire in the simulator. No new tests yet (task 6); the existing 72 pass.
- 2026-11-10: Phase 3 task 1, core settings model (core/src/settings.rs), Wi-Fi, forward and publish AT commands, AT&W, ATZ reload, WIFI, PUB and FWD lines in AT&V. No new tests yet (task 6); the existing 72 pass.
- 2026-11-10: FFI device glue (lfd3k.h C ABI), AT+FB, LAST CRASH on ATI, RADIO line in AT&V, resetting device panic handler.
- 2026-11-09: Cargo workspace (core, sim, ffi) created.
- 2026-11-09: Call state machine and Modem: call setup, training, data, retrain, resync, escape, online command mode, ATI release name.
- 2026-11-10: Retrain notices kept off the data stream, tier cap honored both ways, resync and queue fixes.
- 2026-11-10: Simulator, demo transcript (docs/phase1-demo.txt) and firmware skeleton that builds with the core linked (not flashed).
- 2026-11-10: One lost keepalive reply no longer steps the tier down (re-probe after about 1 RTO, retrain only after two unanswered probes); answerer waits half an RTO longer before starting a retrain and TierSet retries are jittered, so both ends never retrain in lock step.
- 2026-11-10: Training probe retried once on the same tier (with up to 50 ms jitter) before stepping one tier slower, so one lost probe no longer costs a tier at connect.
- 2026-11-10: Phase 1 test suite: 14 core unit tests (tiers, frames, AT parser and result codes, S registers, ARQ, ATI banner), 12 two modem scenario tests on the simulator (connects at SF5 and SF12, data both ways, retrain down and up, link loss, hang-up, flow control, unguarded +++, +++ under flow control, lost TierAck, NO ANSWER, stray frames) and 4 proptest fuzz tests with a fixed seed (frames into idle modems including ring and auto-answer, 10,000 cases; decode, 10,000; AT lines, 10,000; frames into a fresh connected call per case, 3,000). 30 tests, all passing, none ignored.
- 2026-11-10: +++ under backpressure stays data: any data the host offers (even bytes the full send window refuses) counts against the escape guard time, and held + characters followed by more input are sent as data.
- 2026-11-10: Final review wave. ATA in online command mode prints ERROR and keeps the call (was NO CARRIER). A partial command line from before CONNECT is dropped. Config::new checks the station name (1 to 16 printable ASCII) and returns None otherwise. Escape timing fix found by the new fuzz test: a completed +++ whose closing guard has run out now escapes when the host's next byte arrives, even before the next tick (the byte was sent as data with the +++ before). New tests: S21 tier cap held against the answerer's up retrain, answerer cap lower than caller's, manual ATA with S0=0, BUSY for a third caller while in a call, numeric result codes, ATZ in a call, ATA online, partial line before CONNECT, Config name check, and a data mode escape fuzz (600 cases, fresh call each, S12 from 0 to 50, random gaps around the guard). 40 tests (15 core unit, 20 scenario, 5 fuzz), all passing, none ignored.
- 2026-11-10: Phase 2 task 1, core prep for the air: link.rs split into link/{mod,setup,data,retrain}.rs (no behavior change), air id byte 0xD1 in front of the header (HEADER_LEN 5, MAX_PAYLOAD 250, byte 1 now carries type and flags, so the MORE flag is set there), SF5/SF6 airtime fixed, S30 TX power, CHANNEL_HZ. Existing tests updated only for the header layout (frame byte offsets in frame.rs, calls.rs and fuzz.rs); no timing expectation needed to change.
- 2026-11-10: Phase 2 task 5, OLED front panel (SSD1306 over the IDF I2C master driver), lights, animations, splash, thread-safe snapshot for AT+FB.
- 2026-11-10: Phase 2 task 7, tests: 14 driver tests on a mock SX1262 bus (sx1262/tests/mock_bus.rs), 10 more core unit tests (air id, SF5 airtime, S30, AT+FB, ATI crash line, AT&V radio line), 7 FFI host tests with a fake HAL chip and a peer modem (ffi/tests/host_api.rs), and tools/panel_check (PC test of the panel renderer plus PNG mockups in docs/phase2-bench/mockups). phase1-demo.txt regenerated (S30 and TX POWER lines now in AT&V).
- 2026-11-10: Final review wave (phase 2). Data mode input change (160aaf8): in data mode `lfd3k_host_in` hands all offered bytes to the modem in one piece instead of one line at a time, so CRLF-heavy screens fill whole 200 byte ARQ chunks (command mode still feeds one line at a time and syncs S30 after each). Pinned by a new FFI host test (about 400 bytes of "x\r" in one call, first data frame carries 200 payload bytes). The build now fails with "Stale Rust core" when any Rust source is newer than firmware/lib/liblfd3k_ffi.a, and a new library relinks the app. The OLED shows a single "-" (not two) for RSSI and SNR before anything is heard. Bench sheet fixes (fc.exe, rebuild step, expected output). 72 tests.

- 2026-11-10: startup hardening after the first bench run. The SX1262 is held in reset before the front end gets power and gets 1 ms to settle after reset; all radio pins are set up with pulls off (no GPIO46 blip). Safe mode: two Rust panics in a row within 10 s of boot leave the radio and front end off, so AT, ATI (with LAST CRASH) and AT&V still work and AT&V shows RADIO FAIL -1. Task watchdog is 2 s (was 5 s).
- 2026-11-10: Phase 3 task 3, FFI for settings and the wire (C ABI in firmware/include/lfd3k.h: load/save/default settings, Wi-Fi config and status, wire request/event/in/out/close, `lfd3k_status_t.wire`, `lfd3k_set_stack_free`, AT&V `STACK FREE`). The 11 KB Modem now lives in a constant-initialised static and `lfd3k_init` resets it in place (init frame 128 B, deepest callee about 4 KB, was a 11.5 KB temporary). No new tests yet (task 6).
- 2026-11-10: Phase 3 task 4c, SimNet test mode: AT+SIMNET, `SimNetRadio` (core/src/simnet.rs), `Sx1262::standby`, ffi radio selector, `lfd3k_take_simnet`, `lfd3k_udp_in`, `lfd3k_hal_udp_send`, `lfd3k_status_t.simnet`, firmware UDP task, OLED `SI` label, PC tool `lfd3k-pcmodem` with a script mode and real TCP for Wi-Fi calls (tools/simnet-loopback.txt runs two instances against each other on 127.0.0.1). Wi-Fi power save is off (client-only mode used to default to modem sleep, which delays inbound packets). No new tests (task 6); the existing 72 pass.
- 2026-11-10: Phase 3 task 4, firmware platform side (firmware/src: settings_store, wifi_link, tcp_link, wire_pump; main.c boot order NVS, radio, core, load or default settings, Wi-Fi). No new tests (task 6).
