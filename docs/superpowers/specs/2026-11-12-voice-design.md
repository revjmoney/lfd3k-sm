# LFD3K-SM voice calls: design

Status: approved by Rev, 2026-11-12 (plan: `docs/superpowers/plans/2026-11-12-lfd3k-sm-voice-1-pc.md`). Owner: Rev. J. Money. Brainstormed with Rev on 2026-11-12. Builds on `docs/2026-11-09-lfd3k-sm-design.md` (section 14 there parked voice; this document replaces that paragraph) and `docs/FEATURES.md`.

Marking: **Rev:** is what Rev said or decided. **Claude's call:** is a choice made for him that he can change. Everything else is left to the builder unless stated. Code references were checked against `main` at 6364009.

## 1. Intent

Rev, in his words: voice calls "between phones or pcs with the modems", "a directory with autofinding names", "and we could just call them like normal".

The first real picture (Rev): **Rev on his phone in the shop calling his PC in the house.** The phone joins the shop modem's hotspot (the V3), the PC has the V4 on USB, and LoRa carries the call between the two modems. It is for both shop-to-house intercom and much longer range, like on the road (Rev: "both really").

Why: the LFD3K-SM already makes LoRa feel like a phone line for data. Voice is the obvious next thing a "modem" between two people should do, and nothing in the hobby LoRa world lets you just call a person by name and talk.

Success looks like: Rev taps or types a name, the far device rings, he picks up, and on a fast link the two people simply talk like a phone call; farther away it falls back to walkie-talkie style by itself and still gets through.

## 2. Goals, non-goals, anti-goals

Goals (this project)
- One-to-one voice calls by a person's name between the phone app and the PC app (Rev: "phone-to-PC calls by name" first).
- Calls to **anyone with a modem**, by name, like a phone (Rev).
- A name is a **person**, not a device: dialing REV "rings whatever I'm on" (Rev).
- **Normal phone call feel when the link is fast enough**, push-to-talk when it is not (Rev). The switch is automatic.
- Voice is encrypted end to end by the apps (the FLOWTRON SCRAMBLER, section 7), using proven ciphers (Rev: "use the proven ones").
- Existing data calls, discovery, directory and privacy modes keep working unchanged, including between a new build and an old build.

Goals for a second voice project (designed here, built after calls work; Claude's call on the order, following Rev's "calls first")
- Named group channels like SHOPNET: everyone on it hears whoever is talking, walkie-talkie style (Rev: "talking to everyone in the shop at once, like a walkie talkie").
- Channels can be listed or unlisted, each with or without a password (Rev: "both, public or private with or without a password").

Non-goals (for now)
- A standalone handheld (I2S mic, amp and PTT button on the Heltec). Later option.
- Voice to or from the plain AT terminal. Voice needs a voice app.
- Voice over the internet, or phone-to-PC directly over the home Wi-Fi without LoRa.
- Conference calls with three or more people outside a channel.
- Voicemail, recording, call history beyond the app's recent list.
- Interop with any other voice system (M17, FreeDV, Meshtastic, phones).

Anti-goals (failure even if it "works")
- A homemade cipher, or one that a stranger with a modem can listen to (Rev: so "a weird kid down the road" or a van parked outside "can't just pull up a sha256 decryptor").
- Being fooled into talking to a stranger who took someone's name.
- Voice starving or breaking data calls, beacons or discovery for stations that do not use voice, or an old build mistaking a voice call for a data call.
- Fake quality: the app shows the real tier and talk mode, as the modem shows real tiers today.
- A call that stalls silently. Lost voice is a short gap, never a frozen call.
- Crashes, hangs or memory creep on the modem (same as the base design).

## 3. Constraints

- **License:** the repo is GPLv3 or later plus the `NOTICE.md` attribution term. Codec2 (LGPL-2.1) and permissive crypto crates are compatible. The old "no LGPL" rule in the base design is history.
- **Libraries:** Rev OKs any GPL-compatible library for voice (section 14): Codec2, Opus, cpal, a GUI crate, crypto crates, a PAKE crate, Android pieces. Each goes into `THIRD_PARTY.md` as it lands and is named in that step's report. `core/` and `ffi/` keep no outside dependencies (the modem side needs none for voice).
- **Link:** half duplex LoRa at BW500, CR 4/5, 918.5 MHz calling channel, tiers SF12 (1,172 bps air) to SF5 (62,500 bps air), max 250 byte payload, 5 byte frame header with the 0xD1 air id, frame type in a 4 bit nibble with types 1 to 14 used (so **15 is the only free type**, `core/src/frame.rs`), header flag bit 3 free, capability bits 1 to 3 free but **ignored** by current builds.
- **Hardware:** Heltec V4 (USB-Serial/JTAG host link, Wi-Fi) and Heltec V3 (CP2102 UART0 at 115,200 baud, no RTS/CTS wired, resets when DTR/RTS toggle, no PSRAM). The modems do no audio work: no codec, no keys. They carry sealed frames.
- **Phone:** Android. Rev's apps are native `com.simian.*` builds (Simian Scope: NDK, AAudio, NativeActivity); test phone is the Galaxy A17.
- **Rules from Rev:** no em dashes in prose for Rev; no AI attribution in commits; scan history before any push (public repo); V3 never gets AT+MODE; flashing is Rev's call.

### 3.1 Legal check (US, not legal advice)

- The modem runs under FCC Part 15.247 (digital modulation, 902 to 928 MHz, 6 dB bandwidth at least 500 kHz, power and PSD limits). Those rules govern the emission, not its content. Digital voice is data to the radio, so voice frames at the same tiers, power and bandwidth are covered by what the modem already does.
- Encryption is allowed on Part 15.
- **Ham mode** (the planned callsign mode on 33 cm, base design section 17) forbids encryption meant to hide meaning. If ham mode is on, the apps do not scramble: one-to-one calls go in the clear with the app showing `CLEAR (HAM MODE)`, and password channels are greyed out. Ham mode does not exist in the firmware yet; this is the rule for when it does.
- Part 15.247 digital modulation has no duty cycle limit, but air manners still matter (section 8.4).

## 4. Architecture

```
 phone app  <= Wi-Fi =>  shop modem (V3)  <= LoRa =>  house modem (V4)  <= USB =>  PC app
 (mic, Codec2,           (carries sealed              (carries sealed               (same as
  scrambler, ringer)      voice, rings, names)         voice, rings, names)          the phone)
```

- **Apps** do everything audio and secret: microphone and speaker, Codec2, the scrambler, jitter buffer, ringing, the UI, the contact list and its trust store.
- **Modems** do everything radio: name registration, voice names in discovery, call setup by voice name, voice frames with no retries, the talk mode timing, and the directory.
- **Host link between app and modem:** the **voice host protocol** (section 6). On the PC over the USB serial port; on the phone over TCP to the modem.

Where the code lives (Claude's call): all in this repo, GPL.
- `core/`: voice frames, voice call setup, voice names in the directory and discovery, the talk mode scheduler, the voice host protocol parser (no_std, no heap, PC tested like the rest).
- `voice/` (new Rust crate, std): the shared app engine: Codec2 binding, scrambler, jitter buffer, tier-to-codec table, contact trust store, voice host protocol client. Used by both apps so they behave the same.
- `apps/pc/` (new): the PC app in Rust, audio through `cpal`, a small window with the directory, a big call and hang-up button and a big TALK button. Builder picks the GUI crate (permissive license).
- `apps/android/` (new): package within `com.simian.*` (builder's call), native AAudio with the voice-communication input preset (Android's own echo canceller), the `voice` crate built through the NDK, plus a small Java or Kotlin part so it rings anytime (Rev, section 14).
- `sim/`: two simulated modems plus two `voice` engines, recorded audio in and out, loss and tier profiles, so the whole path runs on the PC.

### 4.1 The apps run the whole modem, and how they look (Rev, 2026-11-12)

- **Full control (Rev):** "phone and pc app should all be able to control all settings and options of the radio while using it/having control of it". While an app holds a voice session it can read and change every modem setting the AT commands and the `AT+MENU` reach: station name, privacy mode, S registers (power, tier cap, auto-answer, compression, boot to menu and the rest), Wi-Fi hotspot and client network (passwords always masked), forwards and publish, directory pins and deletes, scan, save (AT&W) and reset to saved (ATZ), and the AT&V and ATI readouts. The app composes the same AT lines the menu does and sends them through the voice host protocol (a VHP message carrying one AT line, answered with the modem's output), so the app takes exactly what the AT commands take. Passwords typed in the app are never shown or logged.
- **Look (Rev):** "the theme of rotatocam.com and my branding". The rotatocam.com site theme (`F:\ROTATOCAM-PRO\www\css\site.css`): graphite surfaces (`--bg #0e0f11`, `--surface #16181b`, hairlines `--rule #24272c`), text `#e8eaed` down to `#6b7079`, accent system-blue `#7aa2f7` (hover `#9db8ff`), `--ok #8bbf5a`, and `--warn #d9a441` only for warnings (the scrambler's NOT CHECKED and conflict screens, RF power above the safe default). Fonts Archivo (sans) and IBM Plex Mono (numbers, tiers, codes), both SIL OFL, bundled with the app. Depth from surface value plus a 1 px hairline, no shadows; 5 px radius on buttons only, panels square. Branding: the LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM name, the Simian monkey logo (`docs/img/simian-logo.png`), "Rev. J. Money" and the `NOTICE.md` attribution line on the About screen.
- **No scrolling (Rev):** "everything should fit on each screen with no scrolling". Every screen fits its window (PC: down to 1280 x 720; phone: the A17 portrait screen) with no scroll bars: long content is split into tabs or pages (for example Settings tabs Radio, Privacy, Wi-Fi, Forwards, Voice and keys, About), and lists show a fixed number of rows with page buttons.

## 5. Calls by name

### 5.1 Voice names and device labels

- A voice name is a person's name, like REV (Rev: "my name, like REV, rings whatever i'm on"). Same rules as a station name: 1 to 16 printable ASCII, no `:`, case-insensitive.
- Each app install has one voice name and a short **device label** (default `PHONE` or `PC`, editable), Claude's call. Dialing `REV` rings all of REV's devices; dialing `REV/PC` rings only that one.
- **The caller's own device never rings** (Claude's call). So in Rev's first picture, with both phone and PC named REV, the phone dialing REV rings only the PC. If Rev also has a second phone up, `REV/PC` picks the PC.
- When an app connects it **registers** its name and label with its modem; the modem holds them while the app is connected, plus a short grace period for a phone that briefly drops Wi-Fi (builder's call, around 30 s). Up to 4 registrations per modem (Claude's call). Not saved by AT&W.
- **Voice names are their own namespace** (Claude's call): a voice name may equal a station name. `ATDT <name>` keeps its meaning today (forward, then host:port, then a station), so data calls never change; voice calls are placed from the app. A terminal can place one with a new command (for example `ATDV REV`, builder's call), which gives ERROR with no voice app attached.

### 5.2 Voice names on the air

- Voice frames, voice call setup and voice discovery all use **frame type 15 with a sub-type byte** as the first payload byte (Claude's call). Type 15 is the last free type, so the sub-type keeps room for everything voice and channels will need. Current builds drop type 15 as BadType, so nothing voice can be mistaken for data by an old build.
- **Voice beacon** (type 15 sub-type): the modem's station name and its registered voice names and labels. Sent at least 10 s after the normal beacon (the existing one-discovery-frame-per-10-s spacing holds), and once soon after a new name registers, so a new phone shows up in nearby directories within about a minute. The normal beacon gets the next free services bit as a "has voice names" hint (current builds mask unknown bits off).
- **Scans:** a new build's ScanReq asks for voice names with a flag (spare header flag bit 3, or another form old builds ignore; builder's call). A modem with voice names answers that ScanReq with a voice scan answer (type 15) in place of its normal ScanResp, in the same slot rules. An old scanner never sets the flag and gets normal answers. Builder proves in the sim that old and new builds still see each other in scans and beacons.
- Privacy modes: ON sends voice beacons and voice scan answers; QUIET sends no voice beacons but answers voice scans; HIDDEN sends neither.

### 5.3 The directory and who is who

- The modem directory gains voice entries: name, label, and the modem it was heard through. Size budget is the builder's call within RAM (32 stations today).
- The apps show one combined list: people first (name, devices, best tier, a `CALL` or `PTT` hint), then plain stations. `AT+DIR?` lists voice entries marked `VOICE`.
- **Who someone is comes from the app's identity key, not the modem name** (Claude's call). Station names are free text and nothing on the air is signed, so a modem name is only a hint; the app shows `REV (new modem)` when a name turns up through an unfamiliar modem, nothing more. Trust lives in each app's contact list (section 7.1). Anyone can still call themselves REV, make a "call REV" ring them, or answer first if they are heard first; but before any audio plays the caller's app checks the answering device's key (section 7.1). Being rung by a stranger is a known limit; being fooled into talking to one is not allowed.
- Pins: an app can pin a voice name in its modem's directory. HIDDEN mode rings only pinned voice callers (section 5.5).

### 5.4 Placing and answering a call

Voice calls get their own setup frames under type 15 (sub-types for voice call request, voice answer, voice decline), Claude's call, because today's CallReq has room for only two names and the capability bit would not keep current builds from ringing a station named REV as a data call. After the answer, the call runs on today's call machinery (call id, Train, TierSet, TierAck, ARQ Data, Keepalive, Hangup), with voice frames added.

- **Request:** the caller's modem sends a voice call request on the calling channel at SF12, repeating it every `PERIOD_MS` until answered or S7 runs out, as `tick` does for CallReq today (`core/src/link/mod.rs`). It carries the call id, the caller's station name, the caller's voice name and label, the callee's voice name and optional label, and the capability byte. The builder grows the setup queue slot to fit (today's QBUF is 40 bytes, `core/src/link/mod.rs`).
- **Ringing:** every modem with a matching registration rings that app (Rev: rings whatever I'm on), except the caller's own device. Ringing sends nothing on the air, as today: a ringing modem stays silent until answered (`answer` and `send_train` in `core/src/link/setup.rs`).
- **A busy device stays silent** (Claude's call): a modem already in a call does not reject a voice call request for one of its voice names, so the person's other devices keep ringing. Today's CallReject-means-BUSY stays for data calls only.
- **Answer:** the first device picked up sends a voice answer that carries **the answering modem's station name, label and a random answer token**; its Train carries the same token. The caller binds to the first answer token it hears and puts that token in its TierSet; a modem whose token is not in the TierSet stops, shows "answered elsewhere", and sends nothing more. Other modems still ringing hear the voice answer for that call id and stop ringing too. This replaces "frames say who sent them", which today's CallAck and Train do not (`setup.rs`, `modem.rs`). The builder proves it in the sim with two answering modems, including two pickups at the same moment and a lost voice answer.
- **Decline** (Claude's call, like a phone): declining on any device sends a voice decline, which ends the call for the caller (`DECLINED` in the app) and stops the other devices ringing.
- **No answer** within S7: NO ANSWER, as today. If nobody has any matching registration in range, the caller just gets NO ANSWER.
- **Ending:** either side hangs up with today's Hangup; the far app shows the call ended; a terminal sees NO CARRIER.
- **Auto-answer (S0) never answers a voice call** (Claude's call): a person picks up.
- One call at a time per modem, voice or data, as today.

### 5.5 Privacy modes

- ON: voice names in beacons and voice scan answers.
- QUIET: no voice beacons; answers voice scans.
- HIDDEN: no voice beacons, no scan answers; only voice callers whose voice name is pinned in the directory can ring; everyone else gets NO ANSWER and nothing is transmitted, as for data today. The pin is a name check only (anyone could claim that name), so the app's key check (section 7.1) still applies to a HIDDEN call.

## 6. Voice host protocol (app to modem)

- **USB (PC, V4 or V3):** the app sends an AT command (builder's name, for example `AT+VOICE`) and the port switches to framed binary mode. Leaving: the app's leave frame, or no frame from the app for a timeout (builder's call, a few seconds; the app sends a heartbeat). Not DTR: neither host link handles DTR today, and toggling DTR or RTS can reset the board.
- **TCP (phone):** new work in the firmware, which today has no TCP listener (`firmware/src/tcp_link.c` only connects out). The modem listens on its own voice port (builder's call, not 23, not SimNet's 4210), up to 4 sessions, framed mode from the start. mDNS: `_lfd3k._tcp` is advertised today on port 23 (`firmware/src/mdns_link.c`) with nothing listening; the builder adds the voice port to it (a TXT key or a separate service type) so the phone finds the modem on the LAN; on the hotspot the phone uses the gateway address. Builder checks the V3's RAM budget (no PSRAM) for the listener and 4 sessions.
- Framing: COBS or length prefix (builder's call) with a type byte. Messages, at least: hello and version, register and unregister, directory list and updates, call, answer, decline, hang up, ring, answered elsewhere, declined, call state (tier, talk mode, SNR, RSSI), voice out, voice in, control out, control in, talk key down and up, heartbeat, leave.
- The modem treats voice and scrambler handshakes as opaque bytes; it never sees keys or audio.
- Access (Claude's call): the modem takes TCP voice sessions only from its own hotspot and LAN subnets. On the hotspot that means someone who knows its WPA2 password; on the LAN, anyone on Rev's network can register names. Written down as a known limit; a pairing code is a later option.
- Bandwidth: the V3's 115,200 baud UART carries 3,200 bps voice with plenty of margin.

## 7. The FLOWTRON SCRAMBLER (encryption)

Rev: "use the proven ones, flowtron scrambler". Our own name and protocol around standard, well tested primitives. No homemade cipher.

- **Per call keys:** each app makes a fresh X25519 key pair for every call. The apps exchange public keys, each signed with the app's identity key, over the call's reliable control path (section 8.1), and derive one key per direction with HKDF-SHA256. Keys are wiped at hang-up, so a recorded call cannot be opened later even with a stolen phone.
- **Every voice frame** is sealed with ChaCha20-Poly1305 or AES-256-GCM (builder's call, one of the two, from a permissive, well known crate). Nonce from direction plus a frame counter. A frame that fails to open, or a replayed or old counter, is dropped as a gap.
- **Two word check:** at the start of a call both apps show two words derived from the session and both identity keys (for example `WRENCH TANGO`, from a fixed word list). Matching words mean nobody is in the middle. The caller commits to its hello (sends a hash first) and reveals it only after the answerer's hello arrives, so nobody in the middle can try thousands of keys until the 16 bits of words match (the ZRTP way; added while planning).
- **Overhead:** a 16 byte tag plus a short counter per voice frame. Truncated tags are not allowed; frame sizes in section 8.2 account for it.
- **Password channels** (section 9): channel key from name and password through Argon2id with deliberately slow settings.
- **Ham mode:** no scrambler (section 3.1).
- Lives only in the `voice` crate. The modem firmware has no voice crypto.

### 7.1 Identity keys and people with several devices

- Each app install has one long term **identity key pair**, made on first run and kept in app storage.
- **A contact is a name plus a set of checked device keys.** Rev's phone and Rev's PC are two devices under REV. When the answering device's key:
  - is checked: the app shows `REV` and audio plays;
  - is new for that contact: the app shows `REV/PC (NOT CHECKED)` with the two words, and holds audio until the user taps "words match" (or "talk anyway" for this call only);
  - conflicts (a device the user checked now presents a different key, or a removed device): `NOT THE REV YOU CHECKED`, muted, with a loud warning.
- A device linked to a person (section 7.2) counts as checked for every contact who has checked that person.

### 7.2 Linking your own devices

Rev: link devices with a code.
- The first device a person sets up makes a **person key** (Ed25519) along with its device key. A contact who checks the two words with any of that person's devices stores the person key; from then on, any device whose key is signed by that person key counts as checked.
- **Linking:** on the device that already has the person key (Rev's phone), "Link a device" shows a short code. On the new device (the PC), "Link to my name" asks for it. The two apps run a password-authenticated key exchange keyed by the code (a SPAKE2 or CPace style exchange from a permissive crate; builder's call), then the old device sends the person key and the name over that sealed channel, and the new device signs its own device key with it. A wrong code fails with nothing learned.
- **Transport for linking:** whatever reaches between the two devices: both on the same modem or LAN through the voice host protocol, or over a voice call between them (control path). Builder's call; at least one must work in Rev's first picture (phone on the V3, PC on the V4, so over LoRa through a call).
- **Unlinking a lost device:** a "remove device" on any linked device publishes a removal signed by the person key at the next call with each contact; that device then shows `NOT THE REV YOU CHECKED`. Full revocation reach (contacts who never call again) is a known limit.
- The person key is stored like the identity key (app storage). Losing every linked device means starting a new person key; contacts see `NOT CHECKED` and check the words again.

## 8. On the air

### 8.1 Voice frames and the control path

- Voice frames are type 15 with a voice sub-type, one sealed voice packet each. **No ARQ, no retries, no acks**: a lost frame is a short gap that the decoder conceals.
- Control in a voice call (scrambler handshake, talk key up and down, mute) rides the call's existing reliable ARQ Data stream, so it is never lost.

### 8.2 Talk modes by tier

Rev: "like a normal phone call when it's fast enough". The mode follows the tier by itself:

| Tier | Talk mode | Codec2 mode (Claude's call, builder tunes) | Voice per frame (guide) |
|---|---|---|---|
| SF5 | **Duplex** (phone call) | **Opus** about 12 kbps (Rev) | 100 to 200 ms |
| SF6 | Duplex | **Opus** 6 to 8 kbps if it fits, else Codec2 3200 | 200 ms |
| SF7 | Duplex | 2400 | 200 ms |
| SF8 | Duplex **only if it fits**, else push-to-talk | 1300 | 200 ms |
| SF9, SF10 | **Push-to-talk** | 1200 | 400 ms |
| SF11 | Push-to-talk | 700C | 1 s |
| SF12 | Push-to-talk, delayed | 700C | about 2 s |

- Airtime checks: a 105 byte frame (200 ms of 3,200 bps voice plus overhead) is about 15 ms on air at SF5 and about 45 ms at SF7, so both directions fit easily in each 200 ms slot. At SF8 two frames plus two 40 ms turn gaps take about 188 ms of every 200 ms, so SF8 is duplex only if the builder proves in the sim that at least 20 % of each slot stays free for control, keepalives and probes; else SF8 is push-to-talk. At SF12, 2 s of 700C plus overhead is about 1.6 to 1.7 s on air, so the listener hears the talker about 2 s late; the app shows `DELAYED`.
- Retrain mid-call: when the tier changes, codec and talk mode change at the next slot and the app shows it ("SWITCHED TO PUSH-TO-TALK"). Target latency (Claude's call): mouth to ear under 400 ms in duplex at SF5 and SF6, under 600 ms at SF7 and SF8.
- **Opus** (BSD, Rev's choice for the fast tiers): at SF5 a 200 ms frame of 12 kbps Opus is about 320 bytes, over the 250 byte payload, so SF5 uses 100 ms frames (about 170 bytes with overhead, about 23 ms on air). At SF6, 200 ms of 6 to 8 kbps Opus is about 170 to 220 bytes, about 40 to 51 ms on air, so two frames plus two 40 ms gaps take 160 to 182 ms of 200: SF6 uses Opus only if it passes the same 20 % free rule as SF8, else Codec2 3200. The codec switch on retrain is the same as a mode switch. The builder confirms each figure with the airtime formula.
- Echo: the phone uses Android's voice communication path (built-in echo cancel). The PC app recommends a headset in duplex; a software echo canceller is the builder's call (permissive license).

### 8.3 The voice scheduler (new, not today's data timing)

Today's link timers would break voice if reused as is: a side that hears nothing for 2 RTOs pings, after 4 RTOs plus two unanswered pings it steps down a tier, after 8 RTOs it drops to SF12 (`core/src/link/data.rs`, `core/src/link/retrain.rs`; RTO is 3 airtimes of a full 205 byte frame plus 100 ms, about 0.34 s at SF7 and 0.87 s at SF9). A push-to-talk talker would hear nothing back for seconds and fall to SF12 mid-sentence; SNR based retrain also stops on the side that hears nothing. Also today's turn handover waits a full frame's airtime plus the gap after a MORE frame, and every Data frame needs an Ack. So:

- During a voice call the link runs a **voice scheduler** with fixed slots per tier instead of the data burst and turn timing. Voice frames are not acked.
- **Both sides always get a slot.** In duplex each slot is "my voice, your voice". In push-to-talk the talker yields a short **listener slot** at least every 1 to 2 s (builder's call) where the listener sends a small frame: its SNR, any control bytes waiting (ARQ Data and Acks ride here), and "still here". A side that has no voice to send in its duplex slot sends that small frame instead (silence suppression without silence on the air).
- These small frames feed today's retrain and NO CARRIER logic, which stays as it is but counts voice and listener frames as signs of life and SNR samples. The builder writes the exact rule and proves in the sim that a 60 s push-to-talk talk spell at SF9 and SF12 causes no step-down at a steady SNR, and that a real fade still retrains.
- Push-to-talk turn taking: a TALK press while the other side talks waits for its key-up (shown as busy); key down and up travel as control bytes.

### 8.4 Air manners

- Voice calls run on the calling channel like data calls today (traffic channels are not built yet), and keep discovery frames off the channel while they run, same as data.
- NO CARRIER after S22 seconds of silence, as today.

## 9. Group channels (second voice project, designed now)

- A channel has a name (same rules as voice names, for example SHOPNET), a fixed tier chosen by whoever creates it (default SF7, Claude's call), a listed or unlisted flag and an optional password (Rev).
  - Listed and open: in scans, anyone joins.
  - Listed with a password: in scans with a lock; joining needs the password.
  - Unlisted and open: never advertised; join by typing the exact name.
  - Unlisted with a password: name and password.
- **Always push-to-talk** (several talkers share one half duplex link). Listen before talk; the app shows who is talking; a TALK press while someone else talks waits or shows busy.
- **Wi-Fi first:** phones on the same modem's hotspot hear each other through the modem with no LoRa airtime. A modem puts a channel on LoRa only while at least one of its own apps has joined it.
- Channel frames are type 15 sub-types, broadcast with no call id binding, sealed with the channel key. An open channel's key comes from its name alone, so frames are not plain audio, but anyone who knows the name can listen (that is what open means).
- Frame layout and how modems learn about each other's members are for that project's own plan.

## 10. Testing

- **PC first:** the sim runs two modems and two `voice` engines end to end with recorded speech on every tier, with loss and retrain profiles. Checks: call by name and by `NAME/LABEL`; caller's own device never rings; two ringing modems with one pickup, two pickups at once, a lost voice answer; decline ends everywhere; busy device stays silent; HIDDEN and pin rules; talk mode switch on retrain; no step-down during long talk spells at steady SNR; gaps on loss never stall the call; old and new builds: an old build ignores every type 15 frame, never rings for a voice call, and still sees new builds in beacons and scans (and the other way).
- **Scrambler tests:** standard test vectors for the primitives, tampered frame dropped, replayed frame dropped, both ends show the same two words, checked, new and conflicting device keys behave as in section 7.1; linking with the right code makes the new device trusted by an existing contact, a wrong code fails and reveals nothing, a removed device shows as not checked.
- **Listening check:** the sim writes a WAV per tier so Rev can hear each tier before any hardware test.
- **Hardware bench (flashing is Rev's call):** V3 in the shop with the A17 on its hotspot, V4 on the PC; a call each way, a tier walk (S21 caps force slow tiers), a hang-up from each side. Testing is batched at the end (Rev's standing preference), with a screenshot of each tested step.

## 11. Build order (proposed)

1. `voice` crate: Codec2 and Opus bindings, scrambler, person and device keys, linking, trust store, jitter buffer, tier table; WAV round trip tests.
2. Core: type 15 sub-types, voice names, voice discovery, voice call setup, the voice scheduler, the voice host protocol; sim tests.
3. PC app over USB, and over the sim so two PC apps can call each other with no radios.
4. Firmware: USB framed mode on V4 and V3, the TCP voice listener, mDNS for the voice port.
5. Android app.
6. Bench: phone in the shop to the PC.
7. Later project: group channels.

## 12. What is left to the builder

GUI crate for the PC app; exact frame, sub-type and message layouts; framing (COBS or length prefix); TCP port number and how mDNS advertises it; grace period and heartbeat timeout; jitter buffer depth; exact Codec2 mode per tier and the SF8 duplex decision (within section 8.2); slot lengths and the listener slot interval; which AEAD and which permissive crates; the terminal voice dial command name; the Android package name within `com.simian.*`.

## 13. Decisions log

Rev (2026-11-12): phone in the shop calling the PC is the first picture; both intercom and long range; normal phone call feel when fast enough; anyone with a modem calls by name; a name is a person and rings whatever they are on; group channel for talking to everyone in the shop like a walkie-talkie; channels listed or unlisted, with or without a password; phone-to-PC calls by name first; encryption with proven ciphers, named the FLOWTRON SCRAMBLER; the phone rings anytime; Opus on the fast tiers; any GPL-compatible library is OK; own devices linked with a code.

Claude's calls: talk mode follows the tier (duplex SF5 to SF7, SF8 if it fits, push-to-talk below); Opus rates and the SF6 rule, Codec2 rates per tier; person key plus device keys, PAKE linking; apps do all audio and crypto, modems carry sealed frames; Rust `voice` crate shared by both apps; PC app in Rust over USB; Android app native with AAudio; phone reaches the modem over the hotspot or the LAN; device labels (`REV/PC`); the caller's own device never rings; up to 4 registrations per modem, not saved; voice names their own namespace, `ATDT` unchanged; everything voice under frame type 15 with a sub-type byte; voice answer carries the answerer's station name and a token, the caller's TierSet names the winner; busy devices stay silent; decline ends the call everywhere; S0 never answers voice; trust comes from app identity keys (a contact is a name plus checked device keys), the modem name only a hint; a voice scheduler with listener slots instead of today's data timing; per call X25519 keys signed by identity keys, HKDF, ChaCha20-Poly1305 or AES-256-GCM, two word check, Argon2id for channel passwords; ham mode means no scrambler; channels default to SF7, always push-to-talk, Wi-Fi first, and are a second project after calls.

## 14. Rev's answers (2026-11-12)

1. **The phone rings anytime** (in the background, screen locked), like a real phone. The Android app gets a small Java or Kotlin part: a foreground service with its notification while it listens, the microphone and notification permissions, a wake lock while ringing or in a call, a multicast lock for mDNS, and a socket bound to the modem's hotspot network even though it has no internet. Audio, codec and crypto stay native.
2. **Opus on the fast tiers.** Opus (BSD) on SF5, and on SF6 if it fits; Codec2 everywhere slower (section 8.2).
3. **Libraries: any GPL-compatible library is OK** without asking first. Each one goes into `THIRD_PARTY.md` (name, version, license, why) as it lands, and Rev is told in the report for that step.
4. **Link your own devices with a code** (section 7.2), so people who trust Rev's phone trust his PC too.
