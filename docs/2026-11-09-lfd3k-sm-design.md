# LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM (LFD3K-SM): Design

Status: draft for Rev's review, 2026-11-09. Owner: Rev. J. Money. Open source since 2026-11-12: GPLv3 or later with an attribution term (`NOTICE.md`); the closed-source lines below are history. The Heltec V3 is supported too since 2026-11-12 (the modem alone, see `docs/heltec-v3.md`); the V4-only lines below are history as well.
Brain notes: `D:\brain\Projects\AXIS BBS Firmware.md` (LFD3K-SM and ring sections), `D:\brain\Projects\Simian Radar.md`, `D:\brain\Reference\Heltec V4 MeshCore Node.md`.

## 1. Intent

A Heltec WiFi LoRa 32 V4 becomes a long-range modem that anyone can use. Plug it into any computer and any terminal program and it behaves like an external Hayes modem from 1994: `ATDT AXIS`, training, `CONNECT LORA SF7 BW500 21875/ARQ/LZ`, blinking front-panel lights, `NO CARRIER`. Beyond terminal calls it links anything: a Wi-Fi hotspot whose traffic rides the radio to a far modem, dial-up style PPP, a plain serial pipe, and fire-and-forget datagrams. It is "Wi-Fi, but for very long distances."

Why: Rev's AXIS BBS boards (ESP32 CYD, run on power alone) should take calls over the air, and the modem is good enough to sell on its own and as an AXIS kit (a CYD plus an LFD3K-SM). The existing stacks (Meshtastic, MeshCore, Reticulum) are either chat-oriented, slow for ANSI, or not built for a modem experience. LFD3K-SM is full custom, built for speed and the best user experience.

Success looks like: a caller with a V4 and SyncTERM dials Rev's board from miles away and gets the full ANSI BBS at usable speed; the link speeds up and slows down by itself as conditions change, without dropping; and the OLED looks like a real modem doing real work.

## 2. Goals, non-goals, anti-goals

Goals
- Hayes-style modem behavior over LoRa, GFSK and Wi-Fi, with honest tier names.
- Automatic speed selection (training) and automatic speed change during a call (retrain), including handover between Wi-Fi and LoRa without dropping.
- Full ANSI BBS sessions at practical speed (compression, caching).
- Connect anything: terminal, PPP, Wi-Fi hotspot bridge, serial pipe, datagrams.
- Zero-setup discovery, with privacy settings up to fully hidden.
- A full-screen ANSI setup menu inside the modem, a web settings page, and an animated OLED.
- GPS time and GPS position, position private by default.
- AXIS BBS integration over Wi-Fi, a two-tier ring network, and an ANSI world map of boards.
- One V4 that can also boot RNode, Meshtastic and Simian Radar from a mode picker.

Non-goals (for now)
- Voice (parked, see section 14).
- Being compatible with Meshtastic, MeshCore or Reticulum on the air.
- Modern web browsing at full speed (the link is dial-up class on LoRa).
- Ham-band operation (encryption is not allowed there; LFD3K-SM targets 902 to 928 MHz unlicensed).

Anti-goals (failure even if it "works")
- Crashes, hangs, or slowly running out of memory.
- A modem that ships with a shared default Wi-Fi password.
- Leaking a user's position or presence when they chose quiet or hidden.
- Any GPL or LGPL code compiled into the closed firmware.
- Fake modem speeds ("56000") on the screen.
- A laggy UI because the screen or Wi-Fi starved the radio.

## 3. Constraints

Hardware (Rev's board, from the brain)
- Heltec WiFi LoRa 32 V4: ESP32-S3 (V4 = 2 MB PSRAM, V4-R8 = 8 MB; read the silkscreen), 16 MB flash, SX1262, PA up to ~28 dBm (V4.3 has the KCT8103L FEM), 0.96" 128x64 SSD1306 OLED (I2C SDA 17, SCL 18, RST 21), PRG button GPIO0, LED GPIO35, L76K GPS (ESP receives on GPIO39, transmits on GPIO38).
- **Vext GPIO36 is active LOW on Rev's board** (LOW powers OLED and GPS). Upstream variant files say HIGH; do not copy them.
- Uploads: esptool 4.9 with watchdog reset after upload (esptool 4.5 cannot reset this board over native USB). Full flash backups need `--no-stub --after no-reset` on the V4.
- Supported in v1: the plain V4 (ESP32-S3R2, quad PSRAM), which Rev's Simian Radar board uses with LED GPIO35 and Vext GPIO36 working. The V4-R8 (octal PSRAM) uses GPIO35 to 37 for PSRAM, so it needs its own pin map and memory budget later.
- Second test radio: Heltec V3 (SX1262, 8 MB flash, no PSRAM, CP2102 on COM9, currently RNode, Meshtastic backup exists). A second V4 is recommended for testing.

Rules
- **COM11 (Rev's V4) and COM15 (lathe controller) are never flashed, opened, probed or reset by Claude.** Rev flashes the V4. Take a full flash backup before any reflash.
- **Closed source.** Only permissive third-party code (MIT, BSD, Apache-2.0, ISC, zlib), notices kept in `THIRD_PARTY.md`. GPL code (Meshtastic, RNode firmware, TC2) may be read for ideas, then written fresh; never copied or closely paraphrased. No LGPL in the closed image (the Arduino-ESP32 core and Codec2 are LGPL-2.1), so no Arduino.
- The repo is private. Never push it to a public remote.
- No AI attribution in commits.

Legal (US, FCC Part 15.247, 902 to 928 MHz; not legal advice)
- Wideband digital modulation: 6 dB bandwidth at least 500 kHz, up to 1 W (30 dBm) conducted, reduce power 1 dB per dB of antenna gain above 6 dBi. All LoRa tiers use BW 500 kHz for this reason.
- **Power spectral density limit: 8 dBm in any 3 kHz band.** LoRa BW500 at 30 dBm is about 7.8 dBm/3 kHz, right at the limit, so every tier gets its own power cap with margin, set by measurement.
- Hopping alternative (20 dB bandwidth counts here): 50 or more channels under 250 kHz (1 W, 0.4 s dwell per 20 s), or 25 to 49 channels of 250 to 500 kHz (0.25 W, 0.4 s per 10 s). Not used in v1.
- Encryption is allowed on this band.
- GFSK tier is **experimental and may be dropped**: the SX1262 tops out at 300 kbps, 200 kHz deviation and a 467 kHz receive filter, so meeting a 500 kHz 6 dB bandwidth while still receiving well is unproven. It must be proven on an SDR and on air before it joins the ladder.
- These settings suit hobby use. Selling units is a separate question (see section 17).

## 4. Architecture

Two images per device in v1: the LFD3K-SM application (A/B slots) and a small mode picker (factory app). Other modes are separate, unmodified open-source images in their own slots (section 12).

Inside LFD3K-SM:
- **Core (Rust, `no_std`, no heap):** frame format, link state machine (call setup, training, retrain, handover, hang-up), selective ARQ, compression and screen cache, directory and discovery logic, AT command parser and S registers, menu model, ring message format. Pure logic, no hardware access, same code on the PC and on the device. Fuzzed and tested on the PC against a simulated radio channel.
- **Platform (C/C++, ESP-IDF):** boot, FreeRTOS tasks, Wi-Fi (AP, STA, AP+STA, ESP-NOW, ESP-NOW long-range), lwIP sockets (split TCP endpoints, forwards, SOCKS, PPP), USB serial (TinyUSB CDC or USB-Serial/JTAG), NVS settings, LittleFS for the directory and caches, GPS UART, hardware AES (mbedTLS or direct). Calls into the core through a small C ABI.
- **Drivers (our own, C or Rust):** SX1262 over SPI, interrupt-driven on DIO1 with no busy waiting (Semtech's BSD driver and RadioLib (MIT) as references); SSD1306 framebuffer driver; L76K NMEA parser.
- **Tasks and cores:** radio task at highest priority on one core; Wi-Fi on the other (ESP-IDF default); display task lowest priority, frame-rate capped, never blocks the radio; USB and AT on their own task.

Unavoidable vendor code: Espressif's Wi-Fi/PHY binary blobs, FreeRTOS, lwIP, the ESP-IDF bootloader.

Memory rule: all buffers sized at boot; no allocation after boot in the core; the platform layer keeps allocation to ESP-IDF internals.

Toolchain: PlatformIO with `framework = espidf` (ESP-IDF already present at `~/.platformio/packages/framework-espidf`), plus Rust for Xtensa via `espup` (not installed yet). The Rust core builds as a static library linked into the ESP-IDF build. PC tests: `cargo test` for the core, plus a PC harness that runs two cores against the simulated channel.

## 5. The radio link

### 5.1 Tiers

Fastest first. Air rates are calculated; "real" and range are estimates until the range test measures them.

| Tier name | Settings | Air rate | Real (est.) |
|---|---|---|---|
| `WIFI LAN` | both modems on the same IP network | Mbps | Mbps |
| `ESPNOW` | direct, 2.4 GHz | ~1 Mbps | ~0.5 to 1 Mbps |
| `ESPNOW LR` | Espressif long-range mode | 250 / 500 kbps | ~200 kbps |
| `GFSK` (experimental, see section 3) | up to 300 kbps | unproven | unproven |
| `LORA SF5 BW500` | SF5, 500 kHz, CR 4/5 | 62,500 bps | ~40 kbps |
| `LORA SF6 BW500` | SF6 | 37,500 | ~25 kbps |
| `LORA SF7 BW500` | SF7 | 21,875 | ~13 kbps |
| `LORA SF8 BW500` | SF8 | 12,500 | ~8 kbps |
| `LORA SF9 BW500` | SF9 | 7,031 | ~4.5 kbps |
| `LORA SF10 BW500` | SF10 | 3,906 | ~2.5 kbps |
| `LORA SF11 BW500` | SF11 | 2,148 | ~1.2 kbps |
| `LORA SF12 BW500` | SF12 | 1,172 | ~0.7 kbps |

The screen and `CONNECT` string always show the real tier name and air rate, e.g. `CONNECT LORA SF7 BW500 21875/ARQ/LZ`.

### 5.2 Channels

A calling channel (all beacons, scans and call setup, at the most robust tier) plus traffic channels. A gateway with several radios, or one radio time-sharing, assigns each call a traffic channel ("rotary"); `BUSY` when none is free. v1 can run calls on the calling channel and add traffic channels when the rotary lands. Exact frequencies are the builder's call within 902 to 928 MHz, avoiding the Meshtastic and MeshCore US defaults where possible.

### 5.3 Frames

Our own compact frame, no TCP/IP on the air. LoRa's own CRC catches corrupted frames.
- Header (about 3 to 4 bytes): frame type, stream id, sequence, ack/selective-ack info, flags (compressed, encrypted, more).
- Payload up to the tier's packet size (LoRa max 255 bytes).
- Addressing: short call ids assigned at call setup; full modem ids only in setup and discovery frames.
- Encrypted frames (when on) add a nonce/counter and auth tag.

### 5.4 Call setup and training

1. Caller sends a call request to a name on the calling channel at the most robust tier (`DIALING`, `RINGING` on the caller; `RING` on the answerer, which answers by itself if `AA` is on or on `ATA`).
2. Both sides exchange probe bursts up the tier ladder and measure RSSI and SNR both ways. They settle on the fastest tier with a safety margin and print `CONNECT ...`. In verbose mode the caller sees the training lines.
3. If the two modems are also reachable over Wi-Fi LAN or ESP-NOW, those tiers win.

### 5.5 During the call: auto range and handover

- Both ends keep measuring signal and frame loss. Sustained loss or low margin steps down a tier (`RETRAIN ...` in verbose mode); sustained good margin probes one tier up.
- Moving between Wi-Fi and radio tiers carries the call over without dropping: streams, sequence numbers and buffers survive the switch.
- Link lost past a timeout gives `NO CARRIER`.

### 5.6 Reliability and air manners

- Selective ARQ: only lost frames are resent.
- Listen before talk using the SX1262's channel activity detection (CAD). CAD only sees LoRa preambles at the same SF and BW, so it is partial protection; ARQ and backoff handle the rest.
- Multiple calls: the frame format carries a call id from day one so one radio can multiplex several calls later. v1 firmware allows one active call per radio (Claude's call); more calls and the rotary come with the gateway work.
- Window and timer values adapt per tier.

### 5.7 Speed boosters

- Compression: heatshrink or LZ4 (both permissive) with a preset dictionary seeded with common ANSI sequences.
- Screen cache: the caller's modem keeps recently seen screen data in flash; repeated screens are sent as a reference. Exact scheme is the builder's call, with a PC test proving correctness when the cache is cold, full or stale.

### 5.8 Encryption

Off by default (like a phone line). When on: AES-256-GCM (hardware AES), keys per directory entry or per network, sequence numbers for replay protection. Key setup UI in the menu.

## 6. Ways in

| Way in | Behavior |
|---|---|
| USB terminal | Hayes AT commands, calls carry raw bytes |
| USB PPP | `ATDT` then PPP, like dial-up; the far modem terminates it and reaches its LAN or internet |
| Wi-Fi hotspot bridge | devices join the modem's Wi-Fi; their TCP connections end at the modem (split TCP) and reopen at the far side |
| Serial pipe | two devices with serial ports linked transparently |
| Datagrams | connectionless messages for sensors, alarms and scripts |

Destinations through the far side: fixed forwards (e.g. hotspot port 23 = AXIS telnet), a SOCKS proxy, and dial by name.

**Access policy for answered calls (Claude's call, safe by default):** a caller not in the directory reaches only services the owner published (e.g. the AXIS telnet forward). PPP, SOCKS, the hotspot bridge and the serial pipe into the owner's network are allowed only for directory contacts the owner marked as trusted. Per-service switches in the menu. Strangers never get a way into the owner's LAN or internet by default.

**USB:** TinyUSB composite device with two serial ports: port 1 is the modem (with DCD, RI and DTR-drop hangup, `AT&C` / `AT&D`, needed by PPP dialers), port 2 is the debug log. Because TinyUSB takes over the native USB port, flashing uses a "reboot to download mode" command (`AT+FLASH` and the 1200-baud touch) instead of the current auto-reset, with the BOOT button as the fallback.

## 7. Discovery and privacy

- **Beacons ON by default:** name, services, how well it hears others, time quality; sent at SF12 on the calling channel every few minutes (about 0.4 s of airtime).
- **Quiet:** no beacons; still answers `AT+SCAN`.
- **Hidden:** no beacons, no scan answers; answers only calls from modems already in its own directory (entered by hand, optionally with a key); otherwise transmits nothing.
- `AT+SCAN` asks who is out there; modems answer with random backoff. Results fill the directory automatically (name, services, signal, best expected tier), so `ATDT AXIS` works with no setup.
- Later: modems share who they hear so calls can relay.

## 8. User interfaces

### 8.1 AT commands
`ATDT name`, `ATA`, `ATH`, `+++` and `ATO`, `ATI` (shows the full LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM banner), `AT&V`, `ATV0/1`, `ATE`, `ATS` registers (tier caps, auto-answer, beacon mode, timeouts), `AT+SCAN`, `AT+MENU`, `AT+GPS`, `AT&W` and `ATZ`. Result codes: `OK`, `ERROR`, `RING`, `CONNECT ...`, `BUSY`, `NO ANSWER`, `NO CARRIER`, `RETRAIN ...` (verbose).

### 8.2 Terminal menu
Full-screen ANSI lightbar menu (`AT+MENU`, or `+++` then `AT+MENU` mid-call, `ATO` back to the call). Starts in plain AT mode by default; boot-to-menu is a setting.
- Directory / Scan (pick to dial)
- Link Status (live meter, tier, throughput, retransmits, retrain history)
- Settings (name, beacon mode, power, tier caps, auto-answer)
- Encryption and Keys
- Forwards
- Wi-Fi (hotspot on/off, name, password, hidden, channel, max devices; client mode network and password; both at once on a shared channel)
- GPS (fix, satellites, sharing settings)
- Range Test
- Firmware / About

### 8.3 Web page
The same settings on the hotspot, plus a live map dot and a JSON status endpoint for scripts.

### 8.4 OLED and LED
- Modem front panel with indicator lights driven by real state: OH, CD, SD, RD, HS, EC, DC, AA. The board LED flickers with SD and RD.
- Tier name and rate, signal bars, throughput graph.
- Animations: dialing, training sweep that snaps to the chosen tier, retrain slide, radar sweep while scanning, idle carrier pulse.
- Screens: idle (name, Wi-Fi name and password on first boot, beacon mode), call, scan, GPS, big-digit range test. PRG button cycles screens.
- About 30 frames a second, lowest priority.

### 8.5 First boot
Unique modem name (e.g. `AXISMODEM-4F1C`) and random hotspot password, shown on the OLED and in the menu. No shared defaults.

## 9. GPS

- Time: GPS first, then internet time over Wi-Fi, then time from neighbor modems in beacons, then uptime only. Used for timestamps, beacon and rotary timing, and replay protection.
- Position, local (always available to the owner): `AT+GPS` with a streaming option, NMEA passthrough over USB (the modem looks like a GPS to map programs), menu, OLED, web page, JSON.
- Position, over the air: Off (default), chosen directory contacts, or everyone in beacons; optional coarse rounding (~1 km) and an expiry.
- Range test logs signal, tier and coordinates, building a coverage map.

## 10. AXIS BBS integration

- **Hookup is Wi-Fi.** The modem joins the same Wi-Fi as the CYD (or the CYD joins the modem's hotspot). A radio call to the board's name is forwarded to the board's telnet port. Radio calls need no AXIS change at all.
- An AXIS build option adds extras: finding its modem by itself, showing radio callers and link stats on sysop screens, and the ring sync below. AXIS (GPL, Rev's own) and LFD3K-SM stay separate programs on separate devices.
- A radio caller on a slow tier gets plain menus instead of heavy art (AXIS learns the tier from the modem).

## 11. The ring network (AXIS side)

Scope note (Claude's call): the ring, its bases and menus, and the world map are built in the AXIS repo (GPL) as their own project with their own plan, after LFD3K-SM's link works. Who hosts the internet side (peer to peer between boards, or a hub Rev runs) is decided in that project's design. This doc records what Rev wants from it.

Two tiers:
- **Sysop Net:** every AXIS board joins by default (a sysop can turn it off in Network Setup). Sysops only: support, announcements, update notices, bug reports. Internet first, LFD3K-SM radio as the fallback. Rev moderates.
- **User Net:** opt-in per board, joined with a key from Rev or any member board (Claude's call, changeable). Sysops pick which areas to carry. Callers read and post.

Both get their own network message bases (separate from local bases, origin lines showing board and path) and their own sysop control menus:
- This Node (name, network address, origin line, contact)
- Peers / Uplinks (per peer: areas carried, priority, enabled)
- Network Areas
- Sync and Routing (schedule, radio first or internet first, size limits, hold big posts for internet)
- Gateway Radios (channels, rotary size, beacons, power, tier caps)
- Traffic and Logs
- Moderation (hold or kill a post network-wide, block a node, review held posts)

**ANSI world map of boards:** boards opt in to share a city-level location over the Sysop Net. The BBS draws an ANSI map with half-block characters (world, continent, state zoom; outlines from public-domain Natural Earth data). Dots are colored by link type (internet, radio, both), bright when online, clustered when close. Pick a dot for board info and call it by telnet or `ATDT`. The CYD's own color screen can show the same map.

## 12. Multi-mode V4

Modes: LFD3K-SM, RNode, Meshtastic, Simian Radar. MeshCore mode dropped.
- A tiny mode picker is the factory app. PRG (GPIO0) is a strapping pin, so holding it at power-on would start the chip's download mode instead; the picker plays a **5-second animated AXIS BBS logo** after boot (Rev, 2026-11-09), and a PRG press during those 5 seconds opens the mode list on the OLED. Without a press it boots the last mode.
- Proposed 16 MB layout (to be proven): picker and boot ~0.1 MB, LFD3K-SM A/B 2 x 1.5 MB, Simian Radar 1.5 MB, Simian Radar camera DB ~3 MB (shrunk from ~9 MB; the whole US is 1.99 MB today), RNode 1.5 MB, Meshtastic 2.5 MB, LFD3K-SM data 1 MB. About 12.6 MB.
- Open images (Simian Radar GPLv3, RNode, Meshtastic) sit beside the closed one, never linked into it. Selling units with them on board means shipping a GPLv3 source offer for each, and checking the Meshtastic trademark policy before using that name on a product.
- Risks to prove on a real board: the borrowed firmwares living with one shared partition table and NVS, and blocking their own updaters so they cannot overwrite other slots.

## 13. Reliability and testing

- PC first: the Rust core is tested and fuzzed on the PC (AT parser, frame parser, compression, ARQ, training and retrain logic), and two cores talk through a simulated channel with loss, fading, collisions and handover events.
- No allocation after boot, task watchdog with sub-second recovery, crash notes kept in RTC memory and shown after reboot (same idea as AXIS), A/B updates with automatic rollback.
- On air: two radios (V3 now; a second V4 preferred). Range test mode turns the estimates in section 5.1 into measurements.

## 14. Later

- **Voice** between two laptops or phones: Opus (BSD) at 6 to 16 kbps on fast tiers, near full duplex on Wi-Fi; push-to-talk at a low bit rate on slow tiers. Codec2 is LGPL, so not in the closed image.
- **Simian Radar mesh** moves off MeshCore onto Meshtastic and/or LFD3K-SM. Simian Radar (GPL) must speak the LFD3K-SM air format with its own fresh implementation, never link LFD3K-SM code.
- Relay through other modems, frequency hopping, a phone app (Flutter).

## 15. Build order (proposed)

1. Repo, toolchain (`espup`, ESP-IDF via PlatformIO), Rust core skeleton with PC tests and the simulated channel.
2. SX1262 driver; two radios exchange frames at one fixed tier; minimal AT (`ATDT`, `ATA`, `ATH`, `CONNECT`, `NO CARRIER`); a terminal call end to end; OLED front panel.
3. Training, retrain, selective ARQ, compression, discovery and scan, directory, the ANSI menu.
4. Wi-Fi hotspot and client, forwards (this alone lets radio callers reach AXIS), ESP-NOW tiers and handover, web page, GPS.
5. AXIS build option, Sysop Net and User Net, network bases and menus, the world map.
6. Mode picker and multi-mode layout.
7. PPP, SOCKS, serial pipe, datagrams, encryption, gateway rotary.

## 16. Decisions log

Rev's decisions: full custom protocol; closed source; Rust where good and C/C++ for Wi-Fi; minimal libraries; honest tier names; old-school negotiation and auto range; no handshake sound; encryption open by default, a setting; beacons on by default, quiet and fully hidden modes; Wi-Fi tiers on top of the ladder; hotspot with settable name and password; terminal menu with submenus; Wi-Fi hookup to AXIS; two-tier ring (Sysop Net default on, internet first, radio fallback); ring network bases and menus; GPS time and position for any use; OLED blinky lights and animations; keep Simian Radar, RNode and Meshtastic as modes, drop MeshCore; the ANSI world map; sell AXIS kits; voice later.

Claude's calls (Rev can change): User Net join by key; quiet mode still answers scans; starts in AT mode with boot-to-menu as a setting; first-boot random credentials; all LoRa tiers at BW 500 kHz; calling channel plus traffic channels.

More Claude's calls after the builder check: one active call per radio in v1 (frame format ready for more); safe access policy for unknown callers; TinyUSB two-port USB with real modem control lines and a reboot-to-download flash path; mode picker opened by a PRG press after boot (not at power-on); plain V4 only in v1; the ring and world map are a separate AXIS project; GFSK experimental.

To be measured: real throughput and range per tier, per-tier power caps against the PSD limit, GFSK feasibility, ESP-NOW LR on the S3, whether the borrowed firmwares accept the shared flash layout.

## 17. Rev's answers (2026-11-09)

1. **Power and FCC:** power and every radio setting adjustable to the hardware limits. Defaults are FCC Part 15 safe (per-tier caps); going past them shows a warning that the user is responsible. **Ham mode** for licensed users (902 to 928 MHz is also the 33 cm ham band): enter a callsign, encryption is forced off, the callsign is sent as station ID (in beacons and at least every 10 minutes during a call), and power unlocks to the hardware maximum (an external amplifier is the user's business). No certification work in v1.
2. **Copy protection:** none for now (no secure boot or flash encryption). Normal esptool backup and reflash keep working.

## 18. Phase 2 decisions (Rev, 2026-11-10)
- Second radio: wait for a second Heltec V4. The V3 on COM9 stays RNode (not flashed). Phase 2 is built and bench-tested on Rev's one V4 first; the first two-radio call happens when the second V4 arrives.
- Rev's V4 (COM11): OK to replace Simian Radar with LFD3K-SM for phase 2. Rev flashes it himself after a full backup (`--no-stub --after no-reset`); the backup restores Radar any time.
- COM11 terminal: Claude may open COM11 as a serial terminal only, once LFD3K-SM is on it, with DTR and RTS held still. Never flash, reset or probe it; flashing stays Rev's.
- Bench check without a second radio: Rev's NESDR SMArt v5 with SDRangel's LoRa (ChirpChat) decoder can confirm the V4's frames on air.
- V4 front end (facts from MeshCore variants/heltec_v4, MIT, as used by Simian Radar): SX1262 NSS 8, SCK 9, MOSI 10, MISO 11, RST 12, BUSY 13, DIO1 14, TCXO 1.8 V on DIO3, DIO2 as RF switch; GC1109 FEM power LDO GPIO7 HIGH, enable GPIO2 HIGH, PA mode GPIO46. SX1262 setting 10 gives about 22 dBm at the antenna (FEM gain), so power defaults must count the FEM.

## 19. Mode picker decisions (Rev, 2026-11-10)
- Research: `F:\lfd3k-sm\docs\research\2026-11-10-multimode-research.md` (RNode 1.86 "Heltec V4PA" build exists and runs from any slot; Meshtastic heltec-v4 v2.7.26; Simian Radar uses labels "littlefs" and "camdb"; our own bootloader can boot all of them; picker runs every boot through a bootloader override plus an RTC token).
- One full wipe of the V4 is OK when the picker is ready (Claude asks again right before). Everything gets reinstalled: LFD3K-SM (Rev retypes AT+WSTA), Simian Radar plus camdb, RNode (re-provision), Meshtastic (fresh).
- Meshtastic mode: a **patched build** from source (no OTA loader writing into other slots, no nvs_flash_erase on factory reset), kept as its own GPL repo separate from the closed modem; source must be kept with any binary Rev ships.
- Downloads OK from the official GitHub releases (RNode_Firmware and meshtastic/firmware); Claude shows file names and sizes first. Cloning the Meshtastic source repo is asked separately.
- Meshtastic trademark: do not put the name on a sold product without asking trademark@meshtastic.org; the M-PWRD logo is allowed.
