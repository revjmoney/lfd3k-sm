# LFD3K-SM features log

## Features

| Name | Status | Date |
|------|--------|------|
| Speed tier ladder: 8 LoRa tiers SF12 to SF5 at BW 500 kHz, CR 4/5 (1172 to 62500 bps air rate), SNR floors, best-tier pick, time-on-air estimate | done | 2026-11-09 |
| Frame format: 4 byte header, 11 frame types, validated encode/decode (no panics), CallReq name payloads, Radio trait | done | 2026-11-09 |
| AT command set: ATD/ATDT, ATA, ATH, ATO, ATI, ATV, ATE, ATSn=v, ATSn?, AT&V, ATZ; verbose and numeric result codes; S registers with bounds | done | 2026-11-09 |
| Selective repeat ARQ: window 8, 200 byte chunks, SACK bitmap, timeout retransmit, wrapping u8 seq | done | 2026-11-09 |

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
| ATI | Banner and version |
| ATV0, ATV1 | Numeric or verbose result codes |
| ATE0, ATE1 | Echo off or on |
| ATSn=v, ATSn? | Set or read S register |
| AT&V | Show configuration |
| ATZ | Reset |

Result codes: OK 0, CONNECT 1, RING 2, NO CARRIER 3, ERROR 4, BUSY 7, NO ANSWER 8.
Verbose CONNECT reads CONNECT <tier name> <air bps>/ARQ. RETRAIN lines are verbose only.

## Change log

- 2026-11-09: Cargo workspace (core, sim, ffi) created.

