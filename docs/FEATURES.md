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
| Firmware skeleton (Heltec V4, ESP-IDF via PlatformIO): links the Rust core as a static library and logs LFD3K-SM core <version> once a second; 16 MB flash, two 1.5 MB OTA slots, 1 MB littlefs | done | 2026-11-10 |

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

## AT commands

One command per line, case-insensitive. Out-of-range S register writes are rejected.

| Command | Meaning |
|---------|---------|
| AT | Attention, replies OK |
| ATD<name>, ATDT<name>, ATDP<name> | Dial a station by name (1 to 16 chars) |
| ATA | Answer |
| ATH, ATH0 | Hang up |
| ATO | Return online |
| ATI | Banner, version and release name, tagline |
| ATV0, ATV1 | Numeric or verbose result codes |
| ATE0, ATE1 | Echo off or on |
| ATSn=v, ATSn? | Set or read S register |
| AT&V | Show configuration |
| AT+FB | Dump the frame buffer (answered by the firmware: no OK from the modem) |
| ATZ | Reset |

Result codes: OK 0, CONNECT 1, RING 2, NO CARRIER 3, ERROR 4, BUSY 7, NO ANSWER 8.
Verbose CONNECT reads CONNECT <tier name> <air bps>/ARQ. RETRAIN lines are verbose only.

Notes:
- ATD strips one leading T or P, as Hayes does. To dial a name that starts with T or P, use the full prefix: ATDT TOM or ATDP PAT (ATDTOM would dial OM).
- Names compare case-insensitively.
- Escape: no data for S12 (1/50 s), then three S2 characters each less than S12 apart, then nothing for S12. Anything else is data. S12=0 turns the escape off.
- Data mode is 8-bit clean: a RETRAIN during data mode is held and printed after the OK of the escape (verbose mode). AT&V shows the current tier and the last 4 retrains. ATO prints CONNECT with the current tier. NO CARRIER prints right after the received data that fits in the host buffer, never later.
- A tier above a modem's S21 cap is never used: asked for one, the modem acks its cap instead, and the asker stops asking above it.
- ATH during a call prints NO CARRIER; with no call it prints OK. ATZ resets the S registers, echo and verbose, and hangs up silently.
- ATA with nothing ringing prints NO CARRIER; ATA in online command mode (a call is up) prints ERROR and the call stays up.
- A partial command line typed before a call connects (during RING or while dialing) is dropped at CONNECT.
- The station name is 1 to 16 printable ASCII characters (Config::new refuses anything else).
- ATI prints three lines: LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM, then v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M, then S0N1C B00M R4D10 PR0 MAX.

- ATI gets a fourth line `LAST CRASH: <note>` only when the previous boot ended in a panic (the note is `file:line`, up to 64 printable characters). With no crash it prints three lines, as before.
- AT&V prints `RADIO OK` or `RADIO FAIL <code> ERR 0x<4 hex>` after the TX POWER line, once the firmware has brought the radio up. Code -1 is a BUSY timeout, -2 no chip, -3 bad config; the hex word is the chip's device errors.
- S30 is the only signed register: ATS30=-9 is accepted (a leading minus works for S30 only), ATS30? prints the signed value, anything outside -9 to 22 prints ERROR. AT&V shows `TX POWER S30=<n> dBm chip, about <n+12> dBm out`.

## Change log

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
