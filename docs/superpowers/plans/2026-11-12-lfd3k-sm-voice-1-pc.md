# LFD3K-SM Voice, Part 1 (engine, core, PC app on the simulator) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two PC voice apps call each other by person name through two real LFD3K-SM cores on SimNet (no radios), with Opus and Codec2 by tier, the FLOWTRON SCRAMBLER, device labels, "answered elsewhere", device linking, and WAV files per tier so Rev can hear every tier.

**Architecture:** The new `voice` crate (std, no desktop-only deps so it later builds for Android) holds everything audio and secret: codecs, scrambler, keys, trust store, jitter buffer and the voice host protocol client. The core gains a `voice` module (no_std, no heap): frame type 15 with sub-types, voice registrations, voice discovery, voice call setup and the voice scheduler, plus the voice host protocol server that the firmware will later put behind USB and TCP. The PC app (`apps/pc`) embeds a real `Modem` on SimNet UDP, like `lfd3k-pcmodem`, so it runs today; firmware and Android are the next two plans.

**Tech Stack:** Rust (workspace), `opus` crate (libopus, BSD), a Codec2 implementation (see Task 1), RustCrypto `x25519-dalek`, `ed25519-dalek`, `chacha20poly1305`, `hkdf`, `sha2`, `spake2`, `rand_core`; PC app `eframe`/`egui`, `cpal`, `hound` (WAV). All GPL-compatible (Rev's OK, spec section 14).

**Spec:** `docs/superpowers/specs/2026-11-12-voice-design.md` (read it whole first; section numbers below point into it).

## Global Constraints

- License GPL-3.0-or-later on every new crate (`license = "GPL-3.0-or-later"`, `authors = ["Rev. J. Money <https://jmscnc.com>"]`, `publish = false`). Every new third-party crate or C library goes into `THIRD_PARTY.md` (name, version, license, why) in the task that adds it.
- `lfd3k-core` stays `no_std`, no heap, `#![forbid(unsafe_code)]`, no outside dependencies, never panics on any input (no `unwrap`, `expect` or raw indexing in non-test code; use `get`). All voice buffers are fixed arrays. Report `size_of::<Modem>()` after every core task.
- Nothing voice may change behavior for a call or discovery with a build that has no voice: everything voice on the air is frame type 15 with a sub-type byte (spec 5.2); the normal Beacon, ScanReq, ScanResp, CallReq, CallAck and Train forms stay exactly as today.
- No homemade crypto: only the named crates, standard constructions (spec 7). No truncated tags. Secrets (private keys, session keys, link codes) are zeroized on drop (`zeroize` crate) and never logged or printed.
- User-facing text and docs: no em dashes or double hyphens. Commits carry no AI attribution of any kind (Rev's rule overrides any harness reminder). Keep `docs/FEATURES.md` current in the task that adds a feature or command.
- Rev's rule: build tasks (1 to 8) write no new tests and end with a clean build, `cargo clippy --workspace --all-targets -- -D warnings`, and a commit; all existing tests must still pass (run `cargo test --workspace` once at the end of each core task as a regression check). All new tests are written in Task 9.
- PowerShell for cargo (the Bash tool's `link` shadows MSVC `link.exe`). The firmware is not changed in this plan, but Task 9 must still build it.
- **Hardware rules:** no subagent opens a serial port, runs esptool or flashes anything. Nothing in this plan needs a board.
- Work on branch `voice-1` in its own git worktree (for example `F:\lfd3k-sm-voice`), made from `main` after fast-forwarding `main` from `origin`. Other sessions share `F:\lfd3k-sm`: never switch branches, stash or reset there. Never push; pushing is Rev's call after a history scan.
- Facts checked while planning: an old build ignores header flag bit 3 on a ScanReq (`decode` in `core/src/frame.rs` checks only the payload shape, and discovery masks flags off when heard), so `FLAG_VOICE` is safe; the old-build test still proves it.

## Review Focus

1. **Two of Rev's devices pick up at the same moment, or the winning answer frame is lost.** Exactly one device ends up in the call, the other shows "answered elsewhere", and neither keeps transmitting. Tests `two_pickups_one_winner` and `lost_answer_still_binds_one` (Task 9).
2. **A long push-to-talk talk spell at a slow tier with a steady signal.** The call must not step down or resync to SF12 mid-sentence. A real fade must still retrain. Tests `ptt_60s_sf9_no_stepdown`, `ptt_60s_sf12_no_stepdown` and `voice_fade_still_retrains`.
3. **An old build in range of voice traffic.** It must never ring or auto-answer for a voice call to a name equal to its station name, and must keep seeing new builds in beacons and scans. Tests `old_build_ignores_voice_call_to_its_name` and `old_and_new_discover_each_other` (Task 9, old core as a renamed dependency).
4. **Someone else calls themselves REV.** The caller's app must hold audio and show NOT CHECKED, or mute and warn on a conflict, before any voice plays. Tests `squatter_gets_not_checked_no_audio` and `changed_device_key_is_conflict`.
5. **The app or phone link drops mid-call (no leave frame).** The heartbeat timeout must end the registration and hang the call up cleanly, never leave a ringing or half-open call on the air. Test `session_timeout_hangs_up`.

---

### Task 1: `voice` crate: codecs and the tier plan

**Files:** Create `voice/Cargo.toml`, `voice/src/lib.rs`, `voice/src/codec.rs`, `voice/src/jitter.rs`, `core/src/voice/mod.rs`, `core/src/voice/plan.rs`. Modify the root `Cargo.toml` (members `voice`), `core/src/lib.rs`, `THIRD_PARTY.md`.

**Interfaces (Produces):**
```rust
// core/src/voice/plan.rs (no_std, single source of truth for tier choices)
#[derive(Clone, Copy, PartialEq, Eq, Debug)] #[repr(u8)]
pub enum CodecId { Opus12k = 1, Opus8k = 2, C2_3200 = 3, C2_2400 = 4, C2_1300 = 5, C2_1200 = 6, C2_700C = 7 }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TalkMode { Duplex, Ptt, PttDelayed }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TalkPlan { pub codec: CodecId, pub mode: TalkMode, pub frame_ms: u16, pub max_sealed_bytes: u8 }
pub const SEAL_OVERHEAD: usize = 20; // 4 byte counter + 16 byte tag
pub fn talk_plan(t: Tier) -> TalkPlan;
pub fn duplex_fits(t: Tier, sealed_bytes: usize, frame_ms: u16) -> bool; // 2 x air_ms + 2 x turn gap <= 80 % of frame_ms
// voice/src/codec.rs
pub const SAMPLE_RATE: u32 = 16_000; // engine audio, mono i16
pub trait VoiceCodec: Send { fn id(&self) -> CodecId; fn frame_samples(&self) -> usize; fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>); fn decode(&mut self, bytes: &[u8], pcm: &mut Vec<i16>); fn conceal(&mut self, pcm: &mut Vec<i16>); }
pub fn new_codec(id: CodecId, frame_ms: u16) -> Box<dyn VoiceCodec>;
// voice/src/jitter.rs
pub struct Jitter { /* by voice frame counter */ }
impl Jitter { pub fn new(depth_frames: usize) -> Self; pub fn push(&mut self, counter: u32, pcm: Vec<i16>); pub fn pop(&mut self) -> Option<Vec<i16>> /* None = conceal */; }
```

- [ ] **Step 1: Codec2 is the official C library.** The pure Rust `codec2` crate (0.3.1, checked 2026-11-12) stops at 1200 bps and has no 700C, which SF11 and SF12 need. Vendor the C `codec2` from its official GitHub release tag into `voice/codec2/` (show Rev the archive name and size before downloading) and build only the codec sources (not FreeDV modems) with the `cc` crate in `voice/build.rs`, behind a small `extern "C"` binding in `codec.rs` (the only `unsafe` in the crate, wrapped). This PC has MSVC `cl` 14.44 and MinGW `gcc`, no clang. Codec2 uses C99 `complex`, which MSVC may reject: if `cl` fails, stop and ask Rev to add Visual Studio's "C++ Clang tools for Windows" component, then build with `clang-cl`; do not switch the Rust target to GNU. Android (part 3) uses the NDK's clang. If 700C cannot be built at all, stop and report: SF11 and SF12 would then need 1200 bps as recorded voice messages, a change Rev must approve. Record everything in `THIRD_PARTY.md`.
- [ ] **Step 2: `talk_plan` follows spec 8.2.** SF5 Opus12k Duplex 100 ms; SF6 Opus8k Duplex 200 ms when `duplex_fits`, else C2_3200 Duplex 200 ms; SF7 C2_2400 Duplex 200 ms; SF8 C2_1300 Duplex 200 ms when `duplex_fits`, else Ptt; SF9 and SF10 C2_1200 Ptt 400 ms; SF11 C2_700C Ptt 1000 ms; SF12 C2_700C PttDelayed 2000 ms. `duplex_fits` uses `tier.rs` airtime and the link's turn gap (`turn_gap_ms` in `core/src/link/mod.rs`; make it `pub(crate)`). `max_sealed_bytes` is the encoded size plus `SEAL_OVERHEAD` and must be at most 249 (the type 15 sub-type byte takes one payload byte); if a row does not fit, shorten its `frame_ms` and note it in the module doc.
- [ ] **Step 3: Codecs.** Opus at 16 kHz in VOIP mode, CBR at the plan's rate, `conceal` via Opus PLC; Codec2 at 8 kHz with a simple 2:1 polyphase or windowed-sinc resampler each way (in `codec.rs`), `conceal` repeats the last frame faded by 6 dB per frame, then silence. A codec frame of `frame_ms` may contain several native Codec2 frames concatenated.
- [ ] **Step 4: Build, clippy, existing tests pass, commit** `voice: codecs, jitter buffer and the per-tier talk plan`.

### Task 2: The FLOWTRON SCRAMBLER (keys, handshake, sealed frames, trust)

**Files:** Create `voice/src/scrambler.rs`, `voice/src/identity.rs`, `voice/src/trust.rs`, `voice/src/words.rs`. Modify `voice/src/lib.rs`, `voice/Cargo.toml`, `THIRD_PARTY.md`.

**Interfaces (Produces):**
```rust
// identity.rs
pub struct Identity { /* device Ed25519 key, optional person Ed25519 key, optional DeviceCert */ }
pub struct DeviceCert { pub person_pub: [u8; 32], pub device_pub: [u8; 32], pub name: String, pub label: String, pub sig: [u8; 64] }
impl Identity { pub fn generate(name: &str, label: &str) -> Self; /* makes device key and person key, self-cert */ pub fn device_pub(&self) -> [u8; 32]; pub fn person_pub(&self) -> Option<[u8; 32]>; pub fn save(&self, path: &Path) -> io::Result<()>; pub fn load(path: &Path) -> io::Result<Self>; }
// scrambler.rs
pub enum Role { Caller, Answerer }
pub struct Handshake;
impl Handshake { pub fn start(id: &Identity, role: Role) -> (Handshake, Vec<u8> /* hello */); pub fn finish(self, peer_hello: &[u8]) -> Result<Session, ScrambleError>; }
pub struct Session { pub words: [&'static str; 2], pub peer_device: [u8; 32], pub peer_cert: Option<DeviceCert> }
impl Session { pub fn seal(&mut self, plain: &[u8]) -> Vec<u8>; pub fn open(&mut self, sealed: &[u8]) -> Option<Vec<u8>>; }
pub enum ScrambleError { BadHello, BadSignature, BadCert }
// trust.rs
pub enum Trust { Checked, NotChecked, Conflict }
pub struct TrustStore;
impl TrustStore { pub fn load(path: &Path) -> Self; pub fn save(&self, path: &Path) -> io::Result<()>; pub fn verdict(&self, name: &str, label: &str, s: &Session) -> Trust; pub fn mark_checked(&mut self, name: &str, label: &str, s: &Session); pub fn remove_device(&mut self, name: &str, device_pub: [u8; 32]); }
```

- [ ] **Step 1: Hello** = version byte, role byte, ephemeral X25519 public key, device Ed25519 public key, optional DeviceCert, and an Ed25519 signature by the device key over everything before it. `finish` checks the signature and the cert (person key signed `device_pub || name || label`), does X25519, and derives two 32 byte keys with HKDF-SHA256 (salt = both hellos in caller, answerer order; info `"FLOWTRON SCRAMBLER v1 c2a"` and `"... a2c"`).
- [ ] **Step 2: Sealed frame** = 4 byte little endian counter, then ChaCha20-Poly1305 ciphertext and 16 byte tag; nonce = direction byte, 7 zero bytes, counter. `open` drops counters at or below `highest - 64` and repeats inside a 64 bit window.
- [ ] **Step 3: Commit, then reveal (so the two words cannot be ground out).** The caller first sends only `commit = SHA-256("FLOWTRON COMMIT" || caller hello)`; the answerer then sends its full hello; only then does the caller reveal its hello, and the answerer checks it against the commit (mismatch: `ScrambleError::BadCommit`, call muted). Without this, someone in the middle could try tens of thousands of hellos until both sides show the same 16 bits of words. Handshake API: `Handshake::start` returns the commit for the caller and the hello for the answerer; add `Handshake::on_peer(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, ScrambleError>` for the middle step before `finish`.
- [ ] **Step 4: Two words.** `words.rs` holds the PGP word list (public domain; even list for word 1, odd list for word 2). The words are the first two bytes of SHA-256 over `"FLOWTRON WORDS" || caller hello || answerer hello`.
- [ ] **Step 5: Trust** follows spec 7.1: a contact is a name plus a set of checked device keys and an optional checked person key. Verdict: device key checked, or its cert signed by a checked person key and not removed, gives Checked; a known device label of that name presenting a different device key, or a removed key, gives Conflict; anything else NotChecked. Stored as JSON (`serde_json`) next to the identity file.
- [ ] **Step 6: Build, clippy, commit** `voice: FLOWTRON SCRAMBLER handshake, sealed frames, two words, trust store`.

### Task 3: Linking your own devices

**Files:** Create `voice/src/link.rs`. Modify `voice/src/lib.rs`, `voice/src/identity.rs`, `voice/Cargo.toml`, `THIRD_PARTY.md`.

**Interfaces (Produces):**
```rust
pub fn new_link_code() -> String; // 6 decimal digits from the OS RNG
pub struct LinkOffer; // the device that has the person key
pub struct LinkJoin;  // the new device
impl LinkOffer { pub fn start(id: &Identity, code: &str) -> (LinkOffer, Vec<u8>); pub fn step(self, msg: &[u8]) -> Result<Vec<u8> /* sealed person key + name */, LinkError>; }
impl LinkJoin { pub fn start(code: &str) -> (LinkJoin, Vec<u8>); pub fn step(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, LinkError>; pub fn finish(self, sealed: &[u8], id: &mut Identity) -> Result<(), LinkError>; }
```

- [ ] **Step 1:** SPAKE2 (Ed25519 group, `spake2` crate) keyed by the code with identities `"LFD3K LINK OFFER"` and `"LFD3K LINK JOIN"`, then key confirmation both ways (HMAC-SHA256 of the transcript), then the offer seals the person private key and the name with ChaCha20-Poly1305 under an HKDF of the SPAKE2 key. `finish` stores the person key and makes a new DeviceCert for this device's own key and label. A wrong code fails at key confirmation with `LinkError::WrongCode` and nothing sealed is sent.
- [ ] **Step 2:** Linking messages travel as control messages inside a voice call (spec 7.2); Task 8 carries them. Build, clippy, commit `voice: link a device to your name with a code`.

### Task 4: Core: type 15 frames, registrations and voice discovery

**Files:** Create `core/src/voice/frames.rs`, `core/src/voice/names.rs`. Modify `core/src/voice/mod.rs`, `core/src/frame.rs` (type 15 `Voice`, `FLAG_VOICE` = bit 3 on ScanReq), `core/src/discovery.rs`, `core/src/directory.rs` (pinned voice names in the blob: magic `LFD2`, still loads `LFD1`), `core/src/modem.rs`, `core/src/at.rs`, `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
#[repr(u8)] pub enum VSub { Beacon = 0, ScanResp = 1, CallReq = 2, Answer = 3, Decline = 4, Data = 5, Listener = 6 } // 7.. reserved for channels
pub fn encode_voice(sub: VSub, call_id: u8, seq: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError>;
pub fn decode_voice(frame: &[u8]) -> Option<(VSub, Header, &[u8])>; // unknown sub-type: None
pub const VOICE_REGS: usize = 4;
pub struct VoiceReg { pub name: Name16, pub label: Name16, pub session: u8, pub seen_ms: u32 }
pub const VOICE_DIR_MAX: usize = 16;
pub struct VoiceEntry { pub name: Name16, pub label: Name16, pub station: Name16, pub best_tier: Option<Tier>, pub heard_ms: u32, pub pinned: bool }
impl Modem { pub fn voice_regs(&self) -> &[Option<VoiceReg>; VOICE_REGS]; pub fn voice_dir(&self) -> &VoiceDir; }
```
`Name16` is new in `names.rs`: `pub struct Name16 { bytes: [u8; 16], len: u8 }` with `Name16::new(&str) -> Option<Name16>` (station name rules) and `as_str(&self) -> &str`; `directory.rs` keeps its own fields.

- [ ] **Step 1: Registrations** (spec 5.1): up to 4, name plus label (label 1 to 8 printable, no `:` or `/`, default given by the app), dropped 30 s after the session's last frame (`VOICE_GRACE_MS = 30_000`) or at once on Leave. Not saved.
- [ ] **Step 2: Voice beacon and scans** (spec 5.2): sub-type Beacon carries the station name and each registration's name and label; sent when there are registrations, at least 10 s after any discovery frame (existing `gap_ok`), every beacon interval and once within 15 s of a new registration. The normal beacon sets services bit 3 (`SVC_VOICE`) when registrations exist. A ScanReq with `FLAG_VOICE` set is answered by a voice ScanResp (sub-type ScanResp: the normal ScanResp fields plus the names and labels) in place of a normal ScanResp, same slot rules. `AT+SCAN` sets `FLAG_VOICE` and prints voice answers as `SCAN <station> SNR <x.x> dB <tier> VOICE <name>/<label> ...`. Privacy modes per spec 5.5.
- [ ] **Step 3: Voice directory** (spec 5.3): filled from voice beacons and voice scan answers, never our own registrations, oldest unpinned replaced. Pinned voice names go in the directory blob (`LFD2`); `AT+DIR=+V:<name>` pins, `AT+DIR=-V:<name>` unpins; `AT+DIR?` adds `DIR VOICE <name>/<label> VIA <station> <tier> <age> [PIN]` lines. `AT&V` adds `VOICE <regs> <dir entries>`.
- [ ] **Step 4: Build, clippy, existing tests pass, report `size_of::<Modem>()`, commit** `core: voice frames, registrations and voice discovery`.

### Task 5: Core: voice call setup

**Files:** Create `core/src/voice/call.rs`. Modify `core/src/link/mod.rs`, `core/src/link/setup.rs`, `core/src/modem.rs`, `core/src/at.rs`, `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
pub struct VoiceCallInfo { pub peer_name: Name16, pub peer_label: Name16, pub peer_station: Name16, pub token: u32, pub reg: u8 /* which registration */ }
impl Link { pub fn dial_voice(&mut self, me_station: &str, me: &str, me_label: &str, callee: &str, callee_label: Option<&str>, sregs: &SRegs, now: u32) -> bool; pub fn answer_voice(&mut self, token: u32, sregs: &SRegs, now: u32) -> bool; pub fn decline_voice(&mut self, now: u32); pub fn voice(&self) -> Option<&VoiceCallInfo>; }
// new link events
Event::VoiceRing(u8 /* reg */), Event::AnsweredElsewhere, Event::Declined
```

- [ ] **Step 1: Request** (spec 5.4): sub-type CallReq payload = capability byte, then length-prefixed caller station, caller name, caller label, callee name, callee label (empty = any). Sent at `Tier::ROBUST`, repeated every `PERIOD_MS` until S7, exactly like `send_callreq`. Grow `QBUF` to fit (5 names x 17 + 2) and report the new `size_of::<Modem>()`.
- [ ] **Step 2: Ringing:** a modem with a registration whose name matches (and label, when given), that is not the caller's own station plus label, emits `VoiceRing(reg)` and stays silent on the air. A modem already in a call sends nothing (busy stays silent). HIDDEN: only pinned voice names ring. S0 is ignored for voice.
- [ ] **Step 3: Answer:** `answer_voice` draws a random `token` (xorshift seeded as today), sends sub-type Answer (call id, token, answering station, label), then a Train whose payload is today's 3 or 4 byte form plus the 4 byte token. The caller binds to the first token it hears and sends TierSet with the token appended; TierAck as today. A modem whose token is not in the TierSet, or that hears an Answer for its ringing call id from another station, emits `AnsweredElsewhere` and goes idle without sending. A second Answer reaching an already bound caller gets no reply (the loser learns from the TierSet). A ringing modem that hears a TierSet for its call id (TierSet goes out at `Tier::ROBUST`, `start_tierset` in `setup.rs`) stops ringing at once. A late pickup (the winner was bound before this device answered) sends Answer and Train at most twice; with no TierSet carrying its token within `2 * PERIOD_MS` it emits `AnsweredElsewhere` and goes silent, so it never keeps transmitting at SF12 over a live call. Ringing still also ends as today when the requests stop (`State::Ringing` in `Link::tick`). After TierAck the call is connected and runs on today's machinery with voice added (Task 6).
- [ ] **Step 4: Decline:** `decline_voice` sends sub-type Decline twice; the caller emits `Declined` and goes idle; other modems ringing for that call id hear it, emit `Declined` and stop ringing.
- [ ] **Step 5: Terminal:** `ATDV<name>[/<label>]` places a voice call only when a voice session is open on this modem (else ERROR); results print as for ATDT (`RINGING`, `CONNECT ... /VOICE`, `NO ANSWER`, `DECLINED` as a new verbose result, numeric 3 like NO CARRIER). ATDT is unchanged.
- [ ] **Step 6: Build, clippy, existing tests pass, commit** `core: voice call setup by person name`.

### Task 6: Core: the voice scheduler

**Files:** Create `core/src/voice/sched.rs`. Modify `core/src/link/data.rs`, `core/src/link/retrain.rs`, `core/src/link/mod.rs`, `docs/FEATURES.md`.

**Interfaces (Produces):**
```rust
pub struct VoiceSched { /* slot clock, plan, who holds the PTT floor */ }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Floor { Mine, Theirs, Free }
impl Link { pub fn voice_push(&mut self, sealed: &[u8]) -> bool; /* queue one sealed voice frame, false if a frame is already waiting */ pub fn voice_pop(&mut self, out: &mut [u8]) -> usize; pub fn set_talk_key(&mut self, down: bool); pub fn talk_plan(&self) -> Option<TalkPlan>; pub fn floor(&self) -> Floor /* Mine, Theirs, Free */; }
```

- [ ] **Step 1: Slots** (spec 8.3): while a voice call is connected, `pump` uses fixed slots of the plan's `frame_ms` instead of burst and turn timing. Duplex: the caller's sub-slot first, then the answerer's; each side sends its queued sub-type Data frame, or a sub-type Listener frame (SNR x4, up to 32 bytes of pending ARQ Data and an Ack, a "still here" flag) when it has none. PTT: the floor holder sends Data each slot and yields one Listener sub-slot to the other side at least every `LISTEN_EVERY_MS = 1500` (scaled up to one per slot when `frame_ms` is longer). Floor requests and releases are control bytes (Task 8 defines them); a key down while `Floor::Theirs` waits.
- [ ] **Step 2: Before the first voice:** from CONNECT until both apps have sent `VoiceReady` (a control message after the scrambler handshake and trust decision), the call runs on today's data timing (bursts, turns, ARQ), so the handshake (about 300 to 600 bytes each way with commit and certs) moves at full data speed; the voice slots start after both `VoiceReady`s. In PTT after that the floor starts Free and the first key down takes it. Report the time from CONNECT to first audio per tier in the voicesim table.
- [ ] **Step 3: Timers:** Data and Listener frames count as heard frames and SNR samples for retrain, keepalive and NO CARRIER; while a voice call is up the RTO-based ping and step-down count only slots in which the peer was due to send. A tier change switches the plan at the next slot boundary and is reported through the existing `Retrain` event.
- [ ] **Step 4: Build, clippy, existing tests pass, report `size_of::<Modem>()`, commit** `core: voice scheduler with listener slots`.

### Task 7: Core: the voice host protocol

**Files:** Create `core/src/voice/host.rs`. Modify `core/src/modem.rs`, `core/src/at.rs`, `docs/FEATURES.md` (a "Voice host protocol" section documenting every message).

**Interfaces (Produces):**
```rust
pub const VHP_VERSION: u8 = 1;
pub const VHP_SESSIONS: usize = 4; // session 0 = USB after AT+VOICE; 1..3 = TCP (firmware plan)
impl Modem {
    pub fn voice_open(&mut self, session: u8, now_ms: u32) -> bool;
    pub fn voice_input<R: Radio>(&mut self, session: u8, bytes: &[u8], radio: &mut R, now_ms: u32) -> usize;
    pub fn voice_output(&mut self, session: u8, out: &mut [u8]) -> usize;
    pub fn voice_close(&mut self, session: u8, now_ms: u32);
}
```

- [ ] **Step 1: Framing:** COBS with a zero byte after each frame; decoded frame = type byte, payload, max 260 bytes. Bad frames are dropped silently.
- [ ] **Step 2: Messages** (app to modem): `0x01 Hello{version}`, `0x02 Register{name,label}`, `0x03 Unregister`, `0x04 Call{name,label}`, `0x05 Answer`, `0x06 Decline`, `0x07 Hangup`, `0x08 VoiceOut{sealed}`, `0x09 ControlOut{bytes}`, `0x0A TalkKey{down}`, `0x0B Heartbeat`, `0x0C Leave`, `0x0D DirList`, `0x0E VoiceReady`, `0x0F AtLine{line}` (one AT command line, run through the same AT parser and dispatch as the host terminal and the menu; commands that would take the host link away or start a data call are refused with Error: AT+MODE, AT+SIMNET, AT+MENU, AT+VOICE, ATO, ATA and ATD/ATDT/ATDP/ATDV; everything else, including AT&W and ATZ, is allowed). Modem to app: `0x81 HelloAck{version, station}`, `0x82 Ring{station,name,label}`, `0x83 CallState{state, tier, codec, mode, frame_ms, snr_x4, rssi}`, `0x84 AnsweredElsewhere`, `0x85 Declined`, `0x86 NoAnswer`, `0x87 Ended`, `0x88 VoiceIn{sealed}`, `0x89 ControlIn{bytes}`, `0x8A DirEntry{...}` then `0x8B DirEnd`, `0x8C Floor{Mine|Theirs|Free}`, `0x8D Error{code}`, `0x8E AtReply{text}` (everything the AT line printed, up to its final OK or ERROR, which ends the reply; long output such as AT&V or AT+DIR? spans several AtReply frames, the last one flagged final). Strings are length-prefixed. Control bytes ride the call's ARQ Data stream.
- [ ] **Step 3: Session life:** no frame for `VOICE_HEARTBEAT_MS = 5_000` closes the session (its registration then lives `VOICE_GRACE_MS`, but a call it owns is hung up at once). `AT+VOICE` on the host stream answers OK, then the host stream is session 0 in framed mode until Leave or the timeout, then back to AT mode with `NO CARRIER` if a call was up.
- [ ] **Step 4: Build, clippy, existing tests pass, commit** `core: voice host protocol`.

### Task 8: Engine client, simulator and the PC app

**Files:** Create `voice/src/client.rs` (protocol client and call engine), `voice/src/control.rs` (control message enum: Hello, FloorReq, FloorRel, Mute, LinkMsg, RemoveDevice), `sim/src/multi.rs` (N modems on one `SimChannel`, each with voice sessions), `sim/src/bin/lfd3k-voicesim.rs`, `apps/pc/Cargo.toml`, `apps/pc/src/main.rs`, `apps/pc/src/ui.rs`, `apps/pc/src/audio.rs`, `apps/pc/src/modem_sim.rs`, `tools/make_speech_wav.ps1`, `sim/testdata/speech-16k.wav`. Modify root `Cargo.toml` (members `apps/pc`), `sim/Cargo.toml` (dev-dep `voice`), `docs/FEATURES.md`, `docs/voice-pc.md` (new user guide), `README.md` (one Voice line), `THIRD_PARTY.md`.

**Interfaces (Produces):**
```rust
// voice/src/client.rs
pub struct DirLine { pub name: String, pub label: String, pub station: String, pub best_tier: Option<Tier>, pub mode_hint: TalkMode, pub trust: Trust, pub pinned: bool }
pub enum UiEvent { Ringing { name: String, label: String, station: String }, Connected { plan: TalkPlan, words: [&'static str; 2], trust: Trust }, PlanChanged(TalkPlan), Floor(Floor), AnsweredElsewhere, Declined, NoAnswer, Ended, Directory(Vec<DirLine>), LinkCode(String), Linked, Error(String) }
pub struct Engine;
impl Engine { pub fn new(id: Identity, trust_path: PathBuf) -> Self; pub fn on_modem_bytes(&mut self, b: &[u8]); pub fn take_modem_bytes(&mut self) -> Vec<u8>; pub fn mic(&mut self, pcm: &[i16]); pub fn speaker(&mut self, out: &mut [i16]); pub fn call(&mut self, name: &str, label: Option<&str>); pub fn answer(&mut self); pub fn decline(&mut self); pub fn hangup(&mut self); pub fn talk_key(&mut self, down: bool); pub fn accept_words(&mut self); pub fn talk_anyway(&mut self); pub fn start_link_offer(&mut self) -> String; pub fn join_link(&mut self, code: &str); pub fn poll_event(&mut self) -> Option<UiEvent>; pub fn tick(&mut self, now_ms: u32); }
// sim/src/multi.rs
pub struct Multi { pub modems: Vec<Modem>, pub now_ms: u32 }
impl Multi { pub fn new(chan: SimChannel, names: &[&str]) -> Multi; pub fn run_ms(&mut self, ms: u32); pub fn voice_send(&mut self, m: usize, session: u8, bytes: &[u8]); pub fn voice_recv(&mut self, m: usize, session: u8) -> Vec<u8>; pub fn frames_sent_by(&self, m: usize) -> usize; }
```

- [ ] **Step 1: Engine:** after `Connected` from the modem, runs the scrambler handshake over control bytes; holds audio (mutes both ways) until trust is Checked, or the user accepted the words or pressed "talk anyway" (spec 7.1); Conflict stays muted with a warning. Mic audio is cut to the current plan's frame, encoded, sealed, sent as VoiceOut; VoiceIn is opened, decoded into the jitter buffer (depth 2 frames in duplex, 1 in PTT), concealed on gaps. In PTT, mic audio is sent only while the floor is Mine.
- [ ] **Step 2: Multi harness and `lfd3k-voicesim`:** `Multi` reuses `SimRadio` and `SimChannel` (move shared pieces to `pub(crate)` as needed). `lfd3k-voicesim --tier <SF5..SF12|all> --snr <dB> --in <wav> --out <dir>` runs two modems and two engines (one plays the WAV, the other records) and writes `<dir>/<tier>.wav` plus a line per tier with codec, mode, frames lost and mouth-to-ear delay. `tools/make_speech_wav.ps1` makes a 20 s 16 kHz mono speech WAV with Windows `System.Speech` (no download) into `sim/testdata/speech-16k.wav`.
- [ ] **Step 3: PC app** `lfd3k-voice-pc` (eframe, cpal): `--name REV --label PC --bind 127.0.0.1:4211 --peer 127.0.0.1:4210 [--snr <dB>] [--cap-tier SF9] [--data <dir>]`; an embedded `Modem` on SimNet UDP like `lfd3k-pcmodem` (reuse its loop), the engine, cpal input and output at 16 kHz (resample if the device needs it). Window: directory list (people first, `CALL` or `PTT` hint), big CALL and HANG UP, RING screen with ANSWER and DECLINE, the tier, codec and mode line (`SF7 BW500 21875  CODEC2 2400  CALL`, `DELAYED` at SF12), the two words with WORDS MATCH and TALK ANYWAY, a big TALK button (held with mouse or space bar) only in PTT, "answered elsewhere" and "declined" banners, and Link a device (shows the code) and Link to my name (asks for it). Test helpers for a one-person demo: `--auto-answer --play <wav> --record <wav>` (the app answers, plays the WAV as its mic and records what it hears; this is the app's choice, the modem's S0 still never answers voice).
- [ ] **Step 3b: Settings, theme, branding, no scrolling (spec 4.1, Rev).** Settings screen with tabs Radio (S30 power with the out-of-safe-range warning, S21 tier cap, S0, S31, S32, S20, S7, S22), Privacy (AT+BEACON mode, pinned names and voice pins), Wi-Fi (hotspot on/off, name, password; client network and password, forget; live status), Forwards (8 slots, add, edit, delete, publish), Voice and keys (my name, my label, link a device, link to my name, checked contacts, remove device), About (ATI, AT&V readout, Simian logo, Rev. J. Money, the NOTICE.md attribution line); plus Directory and Scan (dial, pin, delete) and a Save / Discard bar when settings differ from the saved profile (the app compares AT&V before and after). Every change is an `AtLine` the app composes exactly like the menu (`core/src/menu/` shows each line); passwords masked always and wiped after sending. Theme from `F:\ROTATOCAM-PRO\www\css\site.css` tokens (graphite, `#7aa2f7` accent, `--warn` only for warnings, no shadows, 5 px radius on buttons only); Archivo and IBM Plex Mono (SIL OFL) bundled under `apps/pc/assets/fonts/` with their licenses (download from the official Google Fonts GitHub repo; Rev approved GPL-compatible assets and libraries); the Simian logo from `docs/img/simian-logo.png`. Every screen fits 1280 x 720 with no scroll area anywhere (egui `ScrollArea` is not used); lists show fixed rows with page buttons.
- [ ] **Step 4: Docs:** `docs/voice-pc.md` for Rev: run two apps on this PC (one with `--auto-answer --play`), call by name, force slow tiers with `--cap-tier`, listen to `lfd3k-voicesim` WAVs, link a device; use a headset for duplex calls (no software echo canceller in part 1); ham mode does not exist yet, so the scrambler is always on. FEATURES.md entries for every new command, message and setting. Build, clippy, existing tests pass, commit `voice: engine, voice simulator and the PC app`.

### Task 9: Tests, checks and the hand-over (all testing, Rev's rule)

**Files:** Create `voice/tests/codec.rs`, `voice/tests/scrambler.rs`, `voice/tests/link.rs`, `core/tests/voice_frames.rs`, `core/tests/voice_host.rs`, `sim/tests/voice_calls.rs`, `sim/tests/voice_compat.rs`, `sim/tests/voice_props.rs`. Modify `sim/Cargo.toml` (dev-dep `lfd3k-core-old = { package = "lfd3k-core", git = "https://github.com/revjmoney/lfd3k-sm", rev = "6364009" }` for the old build; 6364009 is already public, so this resolves on any machine), `docs/FEATURES.md`, `D:\brain\Projects\LFD3K-SM.md`.

- [ ] **Step 1: voice crate tests:** `talk_plan_matches_spec_table` (every tier's codec, mode, frame_ms; SF6 and SF8 duplex decisions printed and asserted against `duplex_fits`), `every_plan_fits_one_frame` (`max_sealed_bytes <= 249`), `codec_roundtrip_each_id` (speech WAV in, SNR of decoded vs input above a per-codec floor the implementer measures and pins), `conceal_never_panics`, `jitter_reorders_and_conceals`; scrambler `rfc_vectors_x25519_ed25519_chacha` (crate self-checks), `both_ends_same_words`, `tampered_frame_dropped`, `replayed_frame_dropped`, `old_counter_outside_window_dropped`, `bad_hello_signature_rejected`, `squatter_gets_not_checked_no_audio`, `changed_device_key_is_conflict`, `linked_device_trusted_by_existing_contact`, `wrong_link_code_fails_nothing_sealed`, `removed_device_not_checked`.
- [ ] **Step 2: core tests:** `voice_subtypes_roundtrip`, `voice_decode_never_panics` (proptest, 10,000 cases), `vhp_cobs_roundtrip_and_garbage` (proptest), `register_four_fifth_refused`, `registration_grace_30s`, `session_timeout_hangs_up`, `at_voice_enters_and_leaves_framed_mode`, `atdt_unchanged_with_voice_names`, `atdv_error_without_session`, `vhp_at_line_sets_and_reads_back` (S30, AT+BEACON, AT+WAP with the password never echoed, AT+FWD, AT&W then ATZ), `vhp_at_line_refuses_link_takers` (AT+MODE, AT+SIMNET, AT+MENU, ATD give Error).
- [ ] **Step 3: sim tests (Multi, seeded):** `call_by_name_rings_and_connects`, `call_by_name_and_label_rings_one`, `callers_own_device_never_rings`, `two_ringing_one_pickup_other_answered_elsewhere`, `two_pickups_one_winner`, `lost_answer_still_binds_one`, `late_pickup_sends_at_most_twice_then_elsewhere`, `ringer_stops_on_tierset`, `connect_to_first_audio_each_tier` (asserts the reported times; SF12 under 60 s), `bad_commit_reveal_mutes`, `decline_ends_everywhere`, `busy_device_silent_others_ring`, `hidden_rings_only_pinned_voice`, `s0_never_answers_voice`, `talk_mode_switches_on_retrain`, `ptt_60s_sf9_no_stepdown`, `ptt_60s_sf12_no_stepdown`, `voice_fade_still_retrains`, `loss_is_gap_not_stall` (20 % loss, call stays up, frames keep flowing), `duplex_delay_under_target` (SF5 and SF6 under 400 ms, SF7 and SF8 under 600 ms, from `lfd3k-voicesim` timing), `voice_beacon_spacing_10s`, `voice_scan_answers`, `data_call_unchanged_between_voice_builds`.
- [ ] **Step 4: old build compatibility** (`voice_compat.rs`, old core 6364009 vs new): `old_build_ignores_voice_call_to_its_name` (old station named REV, new caller dials voice REV: old sends nothing, never rings, also with S0=1), `old_and_new_discover_each_other` (beacons and scans both ways), `old_new_data_call_works`.
- [ ] **Step 5: Run everything:** `cargo test --workspace` (report the new total and the count before this plan), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`; `powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1`, then `pio run -d firmware -e heltec_v4` and `pio run -d firmware -e heltec_v3_single` must build (report flash and RAM, and `size_of::<Modem>()`; nothing flashed); run `lfd3k-voicesim --tier all --in sim/testdata/speech-16k.wav --out docs/voice-bench/wav` and commit the WAVs and the timing table.
- [ ] **Step 6: Screenshots:** start two PC apps with the `--auto-answer --play` helper and capture a PNG per step at 1280 x 720 (directory, ringing, connected with words, NOT CHECKED, PTT at SF9, DELAYED at SF12, answered elsewhere with a third app, link code, and every Settings tab with a change made and saved, then read back through AT&V), checking each screen has nothing cut off and no scroll bar, into `docs/voice-bench/pc/`, and send them to Rev.
- [ ] **Step 7: Hand-over:** update FEATURES.md statuses, the brain note's Voice section (what works, test count, WAV folder, how to run), commit `voice part 1: tests, WAVs, screenshots`. Do not push.

## Next plans (not in this one)

- **Voice part 2, firmware:** USB framed mode on V4 and V3 (`AT+VOICE` to session 0), the TCP voice listener (sessions 1 to 3) and mDNS for the voice port, RAM check on the V3, PC app serial backend; bench with Rev.
- **Voice part 3, Android:** `com.simian.*` app, native audio and engine via the NDK, small Kotlin service for ringing anytime, hotspot-bound socket; bench in the shop.
- **Voice part 4:** group channels (SHOPNET).
