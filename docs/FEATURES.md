# LFD3K-SM features log

## Features

| Name | Status | Date |
|------|--------|------|
| Speed tier ladder: 8 LoRa tiers SF12 to SF5 at BW 500 kHz, CR 4/5 (1172 to 62500 bps air rate), SNR floors, best-tier pick, time-on-air estimate | done | 2026-11-09 |
| Frame format: 4 byte header, 11 frame types, validated encode/decode (no panics), CallReq name payloads, Radio trait | done | 2026-11-09 |
| AT command set: ATD/ATDT, ATA, ATH, ATO, ATI, ATV, ATE, ATSn=v, ATSn?, AT&V, ATZ; verbose and numeric result codes; S registers with bounds | done | 2026-11-09 |
| Selective repeat ARQ: window 8, 200 byte chunks, SACK bitmap, timeout retransmit, wrapping u8 seq | done | 2026-11-09 |
| Call setup and training: ATDT by name on SF12 (DIALING, RINGING), RING and auto-answer (S0), CallAck plus Train with measured SNR, tier pick with S20 margin and S21 cap, TierSet and TierAck, Keepalive probe with step-down to SF12, CONNECT | done | 2026-11-09 |
| Half duplex air manners: MORE flag bursts of up to 4 frames, turn handover, acks at burst end, keepalives on quiet calls | done | 2026-11-09 |
| Auto retrain: SNR average (1/8), step down on low margin or 4 RTOs of silence, step up after 10 s of good margin, RETRAIN in verbose mode, data survives tier changes | done | 2026-11-09 |
| Resync: 8 RTOs of silence drops both ends to SF12, caller pings, then jumps back up to the best tier; NO CARRIER after S22 seconds of silence | done | 2026-11-09 |
| Escape and online command mode: guard time S12, three S2 characters, guard time, OK; ATO back online; ATH hangs up (Hangup sent twice) | done | 2026-11-09 |
| ATI banner: name, version with release name, release tagline | done | 2026-11-09 |
| PC simulator: seeded half duplex LoRa channel (SNR profiles, loss by margin, collisions, drop hook, frame trace) and a two modem harness (CALLER and AXIS) | done | 2026-11-10 |
| Phase 1 demo: `cargo run -p lfd3k-sim -- [--snr <dB>] [--seed <n>] [--out <file>] [--trace <file>]`, two 4 KB ANSI screens, SNR dip and recovery, escape, ATO, AT&V, ATH | done | 2026-11-10 |
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
- ATI prints three lines: LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM, then v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M, then S0N1C B00M R4D10 PR0 MAX.

## Change log

- 2026-11-09: Cargo workspace (core, sim, ffi) created.
- 2026-11-09: Call state machine and Modem: call setup, training, data, retrain, resync, escape, online command mode, ATI release name.
- 2026-11-10: Retrain notices kept off the data stream, tier cap honored both ways, resync and queue fixes.
- 2026-11-10: Simulator, demo transcript (docs/phase1-demo.txt) and firmware skeleton that builds with the core linked (not flashed).
- 2026-11-10: One lost keepalive reply no longer steps the tier down (re-probe after about 1 RTO, retrain only after two unanswered probes); answerer waits half an RTO longer before starting a retrain and TierSet retries are jittered, so both ends never retrain in lock step.
- 2026-11-10: Training probe retried once on the same tier (with up to 50 ms jitter) before stepping one tier slower, so one lost probe no longer costs a tier at connect.
- 2026-11-10: Phase 1 test suite: 14 core unit tests (tiers, frames, AT parser and result codes, S registers, ARQ, ATI banner), 12 two modem scenario tests on the simulator (connects at SF5 and SF12, data both ways, retrain down and up, link loss, hang-up, flow control, unguarded +++, +++ under flow control, lost TierAck, NO ANSWER, stray frames) and 2 proptest fuzz tests of 10,000 cases each (frames and AT lines). 28 tests: 27 pass, 1 is ignored as an open bug (+++ typed while the send window is full escapes after the guard time).
