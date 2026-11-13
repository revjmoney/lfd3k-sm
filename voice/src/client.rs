//! The call engine of a voice app (PC and phone): the voice host protocol
//! client (`docs/FEATURES.md`, "Voice host protocol") plus everything an app
//! does around a call: the FLOWTRON SCRAMBLER handshake over control
//! messages ([`crate::control`]), the trust check and the two words, codecs,
//! the jitter buffer, push to talk, the directory and linking devices.
//!
//! The engine does no I/O and reads no clock. The app (or a simulator)
//! moves bytes: [`Engine::take_modem_bytes`] to the modem, modem bytes into
//! [`Engine::on_modem_bytes`]; feeds 16 kHz mono audio through
//! [`Engine::mic`] and pulls it from [`Engine::speaker`]; calls
//! [`Engine::tick`] with its clock (milliseconds) every 10 to 50 ms; and
//! shows [`UiEvent`]s from [`Engine::poll_event`]. Given the same inputs and
//! times the engine does the same things at the same times (keys and link
//! codes come from the OS random source, but every message has a fixed
//! length, so timing does not depend on them).
//!
//! # A call, step by step
//!
//! 1. CallState connected: each side starts its handshake. The caller sends
//!    its commit; the answerer holds its hello until the commit arrived and
//!    `Handshake::on_peer` took it, then sends it; the caller answers with
//!    the reveal. Each side, once `finish` gave it a session, sends a sealed
//!    Hello ([`crate::control::MSG_HELLO`]).
//! 2. The peer's sealed Hello opens: the peer is live. The trust store gives
//!    its verdict for the name the user sees (the name dialed, or the
//!    caller's name on a call answered) and [`UiEvent::Connected`] carries
//!    the plan, the two words and the verdict.
//! 3. Audio stays held both ways (nothing sent, nothing played) until the
//!    verdict is Checked, or the user says the words match
//!    ([`Engine::accept_words`], stored) or talks anyway for this call
//!    ([`Engine::talk_anyway`]). A Conflict stays muted with a warning.
//! 4. Then the engine sends any device removals and VoiceReady. The modem
//!    starts the voice slots once both apps said it (CallState flags bit 0).
//! 5. Mic audio is cut into frames of the plan, encoded, sealed and sent as
//!    VoiceOut, only while the slots run and only on a duplex tier or while
//!    the modem says the floor is Mine (push to talk: TalkKey down, wait for
//!    Floor Mine). VoiceIn is opened, decoded into the jitter buffer (2
//!    frames deep in duplex, 1 in push to talk) and played; a frame lost in
//!    the middle of speech is concealed by the codec.
//!
//! A new codec pair and jitter buffer are made for every call and on every
//! talk plan change (a retrain).
//!
//! # Linking (spec 7.2)
//!
//! Linking runs inside a call between the two devices (control messages).
//! The device with the person key shows a code ([`Engine::start_link_offer`]);
//! the new device takes it ([`Engine::join_link`]) and, once a call to the
//! other device is live, sends the link request. The code is 6 digits, used
//! once and wiped after use or after [`LINK_CODE_MS`]. A wrong code makes
//! the offer send nothing; the joiner then times out with
//! `UiEvent::Error("link failed")` and may start again. After
//! [`UiEvent::Linked`] the app saves [`Engine::identity`] (the engine does
//! not know its file).

use std::collections::VecDeque;
use std::path::PathBuf;

use ed25519_dalek::{Signature, VerifyingKey};
use lfd3k_core::voice::host::{call_state, cobs_decode, cobs_encode, err, msg, VHP_WIRE_MAX};
use lfd3k_core::voice::{
    talk_plan, valid_label, CodecId, Floor, TalkMode, TalkPlan, VHP_FRAME_MAX, VHP_VERSION,
};
use lfd3k_core::Tier;
use zeroize::{Zeroize, Zeroizing};

use crate::codec::{new_codec, VoiceCodec};
use crate::control::{self, Control, ControlReader};
use crate::identity::{valid_voice_name, Identity};
use crate::jitter::Jitter;
use crate::link::{new_link_code, LinkJoin, LinkOffer};
use crate::scrambler::{Handshake, Role, Session, MSG_COMMIT, MSG_REVEAL};
use crate::trust::{Trust, TrustStore};

/// Send a Heartbeat when nothing else went to the modem for this long (the
/// modem closes a session after 5 s of silence).
pub const HEARTBEAT_MS: u32 = 1_500;
/// Ask the modem for its directory this often while no call is up.
pub const DIR_EVERY_MS: u32 = 15_000;
/// A link code shown (or typed) lives this long without being used.
pub const LINK_CODE_MS: u32 = 300_000;
/// The joiner gives up this long after its link request went out, plus
/// [`LINK_FRAMES`] voice frames of the current plan (control bytes move in
/// small listener pieces once the slots run: 24 bytes every 4 s at SF12).
pub const LINK_BASE_MS: u32 = 60_000;
/// See [`LINK_BASE_MS`].
pub const LINK_FRAMES: u32 = 30;
/// ControlOut frames carry at most this many stream bytes.
const CTRL_CHUNK: usize = 240;
/// Longest AtLine (bytes).
const AT_LINE_MAX: usize = 128;
/// Most frames concealed in a row in the middle of speech.
const CONCEAL_MAX: u8 = 4;
/// Push to talk: frames of speech kept from the key going down until the
/// floor is ours (sent first then; the far end hears the turn that much
/// later, but from its first word). Three: at SF12 the answerer may wait
/// about 6 s for the floor (its key reaches the caller in its listener
/// slot, every 4 s, and the caller announces the change in the next slot).
const PRE_FRAMES: usize = 3;

/// One line of the directory: a person's device, or a plain station.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirLine {
    /// The voice name (or the station name for a station).
    pub name: String,
    /// The device label (empty for a station, and for a pinned name not heard yet).
    pub label: String,
    /// The modem it was heard through (the station itself for a station).
    pub station: String,
    pub best_tier: Option<Tier>,
    /// The talk mode of the best tier (`CALL` or `PTT` in the list).
    pub mode_hint: TalkMode,
    /// Checked when the user has checked this name before (a hint only; a
    /// call is checked by key).
    pub trust: Trust,
    pub pinned: bool,
}

/// What the app shows.
#[derive(Clone, Debug)]
pub enum UiEvent {
    /// Someone calls: ANSWER or DECLINE.
    Ringing {
        name: String,
        label: String,
        station: String,
    },
    /// The scrambler is up and the peer is live: show the plan, the two
    /// words and the verdict (audio is held until Checked, WORDS MATCH or
    /// TALK ANYWAY).
    Connected {
        plan: TalkPlan,
        words: [&'static str; 2],
        trust: Trust,
    },
    /// A retrain changed the tier's plan (codec, mode, frame length).
    PlanChanged(TalkPlan),
    /// The push to talk floor changed.
    Floor(Floor),
    /// Another device picked up (or our pickup lost the race).
    AnsweredElsewhere,
    /// The call was declined (or another device declined it).
    Declined,
    /// Nobody answered.
    NoAnswer,
    /// The call (or the ring) is over.
    Ended,
    /// The modem's directory, people first.
    Directory(Vec<DirLine>),
    /// The link code to show on this device.
    LinkCode(String),
    /// Linking finished: on the joining device, save [`Engine::identity`].
    Linked,
    /// Something went wrong (shown as is).
    Error(String),
    /// AtLine output; `last` on the final piece (with the result code).
    AtReply { text: String, last: bool },
    /// Channel samples from the modem (Scope), oldest first: dBm and
    /// "taken while transmitting". The last was taken at `t_ms` (the
    /// modem's clock), each `step_ms` after the one before.
    Scope {
        t_ms: u32,
        step_ms: u8,
        samples: Vec<(i8, bool)>,
    },
    /// A frame the modem sent or heard (AirEvent).
    Air(AirFrame),
}

/// One frame on the air, from an AirEvent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AirFrame {
    /// Heard (true) or sent (false).
    pub rx: bool,
    /// Spreading factor (5 to 12).
    pub sf: u8,
    /// Header frame type (1 to 15; 0 for a frame that is not ours).
    pub ftype: u8,
    /// Type 15 sub-type, `0xFF` for other types.
    pub sub: u8,
    /// Bytes on the air.
    pub len: u8,
    /// When it started on the air (the modem's clock, ms).
    pub start_ms: u32,
    /// Its airtime (ms).
    pub air_ms: u16,
    /// What it was heard at (None for a frame sent).
    pub rssi_dbm: Option<i16>,
    pub snr_x4: Option<i16>,
}

/// Numbers from the current (or last) call, for the simulator and the UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct CallStats {
    /// When CallState first said connected (the `tick` clock).
    pub connected_ms: Option<u32>,
    /// When CallState first showed the voice slots running.
    pub slots_ms: Option<u32>,
    /// When the speaker first played a decoded voice frame.
    pub first_audio_ms: Option<u32>,
    /// Voice frames sealed and sent as VoiceOut.
    pub frames_sent: u32,
    /// Voice frames heard that opened.
    pub frames_heard: u32,
    /// VoiceIn frames that did not open (or came while audio was held).
    pub frames_dropped: u32,
    /// Frames sealed but not sent: larger than CallState's max_sealed
    /// (never expected; the plan sizes them).
    pub frames_oversized: u32,
    /// Frames the codec concealed.
    pub frames_concealed: u32,
    /// The tier as CallState shows it (spreading factor, 0 before training).
    pub sf: u8,
    /// The talk plan as CallState shows it.
    pub plan: Option<TalkPlan>,
    /// The last CallState `state` byte (`call_state`: 0 idle, 1 dialing,
    /// 2 ringing, 3 training, 4 connected, 5 resync).
    pub state: u8,
}

/// The codec as the app shows it (`CODEC2 2400`, `OPUS 10K`).
pub fn codec_name(c: CodecId) -> &'static str {
    match c {
        CodecId::Opus12k => "OPUS 12K",
        CodecId::Opus10k => "OPUS 10K",
        CodecId::Opus8k => "OPUS 8K",
        CodecId::C2_3200 => "CODEC2 3200",
        CodecId::C2_2400 => "CODEC2 2400",
        CodecId::C2_1300 => "CODEC2 1300",
        CodecId::C2_1200 => "CODEC2 1200",
        CodecId::C2_700C => "CODEC2 700C",
    }
}

/// The talk mode as the app shows it (`CALL`, `PTT`, `PTT DELAYED`).
pub fn mode_name(m: TalkMode) -> &'static str {
    match m {
        TalkMode::Duplex => "CALL",
        TalkMode::Ptt => "PTT",
        TalkMode::PttDelayed => "PTT DELAYED",
    }
}

/// The tier with this spreading factor.
pub fn tier_from_sf(sf: u8) -> Option<Tier> {
    Tier::LADDER.iter().copied().find(|t| t.sf() == sf)
}

fn codec_from_u8(c: u8) -> Option<CodecId> {
    Some(match c {
        1 => CodecId::Opus12k,
        2 => CodecId::Opus8k,
        3 => CodecId::C2_3200,
        4 => CodecId::C2_2400,
        5 => CodecId::C2_1300,
        6 => CodecId::C2_1200,
        7 => CodecId::C2_700C,
        8 => CodecId::Opus10k,
        _ => return None,
    })
}

fn mode_from_u8(m: u8) -> Option<TalkMode> {
    Some(match m {
        1 => TalkMode::Duplex,
        2 => TalkMode::Ptt,
        3 => TalkMode::PttDelayed,
        _ => return None,
    })
}

/// Reads a voice host protocol payload.
struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(a)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let b = b.get(..b.len().min(255)).unwrap_or(&[]);
    out.push(b.len() as u8);
    out.extend_from_slice(b);
}

/// The joining side of a link.
struct JoinState {
    join: Option<LinkJoin>,
    req: Vec<u8>,
    started_ms: u32,
    /// When the request went out (the timeout runs from then).
    sent_ms: Option<u32>,
}

/// One call, from Call or Answer to its end.
struct Call {
    placed: bool,
    state: u8,
    flags: u8,
    plan: Option<TalkPlan>,
    peer_station: String,
    peer_name: String,
    peer_label: String,
    /// The handshake in progress; `held_hello` is the answerer's hello,
    /// held until the commit has been taken.
    hs: Option<Handshake>,
    hs_started: bool,
    held_hello: Option<Vec<u8>>,
    session: Option<Session>,
    failed: bool,
    peer_live: bool,
    trust: Option<Trust>,
    /// Checked, WORDS MATCH or TALK ANYWAY: audio may flow.
    audio_ok: bool,
    ready_sent: bool,
    ctrl: ControlReader,
    floor: Floor,
    key_down: bool,
    enc: Option<Box<dyn VoiceCodec>>,
    dec: Option<Box<dyn VoiceCodec>>,
    jitter: Jitter,
    mic_buf: Vec<i16>,
    /// Push to talk: what was said with the key down while the floor was
    /// not ours yet (at most `PRE_FRAMES` frames, the newest), sent first
    /// once the floor is ours, so the first words of a turn are not lost.
    pre: Vec<i16>,
    /// TALK went up while we still waited for the floor with words kept:
    /// the modem keeps our key down until the floor comes and they are
    /// sent, then the key goes up.
    release_pending: bool,
    play: VecDeque<i16>,
    conceal_run: u8,
    peer_muted: bool,
}

impl Call {
    fn new(placed: bool, name: &str, label: &str, station: &str) -> Call {
        Call {
            placed,
            state: call_state::DIALING,
            flags: 0,
            plan: None,
            peer_station: station.to_string(),
            peer_name: name.to_string(),
            peer_label: label.to_string(),
            hs: None,
            hs_started: false,
            held_hello: None,
            session: None,
            failed: false,
            peer_live: false,
            trust: None,
            audio_ok: false,
            ready_sent: false,
            ctrl: ControlReader::new(),
            floor: Floor::Free,
            key_down: false,
            enc: None,
            dec: None,
            jitter: Jitter::new(2),
            mic_buf: Vec::new(),
            pre: Vec::new(),
            release_pending: false,
            play: VecDeque::new(),
            conceal_run: 0,
            peer_muted: false,
        }
    }

    fn connected(&self) -> bool {
        self.state == call_state::CONNECTED || self.state == call_state::RESYNC
    }

    fn slots(&self) -> bool {
        self.flags & 1 != 0
    }

    /// New codecs and jitter buffer for `plan`.
    fn set_plan(&mut self, plan: TalkPlan) {
        self.plan = Some(plan);
        self.enc = Some(new_codec(plan.codec, plan.frame_ms));
        self.dec = Some(new_codec(plan.codec, plan.frame_ms));
        self.jitter = Jitter::new(if plan.mode == TalkMode::Duplex { 2 } else { 1 });
        self.mic_buf.clear();
        self.pre.clear();
        self.conceal_run = 0;
    }

    /// Mic audio may be sent now.
    fn can_send(&self, muted: bool) -> bool {
        self.session.is_some()
            && self.audio_ok
            && !muted
            && self.connected()
            && self.slots()
            && self.plan.is_some_and(|p| {
                // Push to talk: the floor is ours and our own key is down
                // (a floor left over from a key the modem never heard go up
                // must not open the mic).
                p.mode == TalkMode::Duplex || (self.floor == Floor::Mine && self.key_down)
            })
    }

    /// Push to talk with our key down but the floor not ours yet: the mic
    /// is kept (`pre`) for when it is.
    fn waiting_for_floor(&self, muted: bool) -> bool {
        self.session.is_some()
            && self.audio_ok
            && !muted
            && self.connected()
            && self.slots()
            && self.key_down
            && self.floor != Floor::Mine
            && self.plan.is_some_and(|p| p.mode != TalkMode::Duplex)
    }
}

/// The call engine. See the module docs.
pub struct Engine {
    id: Identity,
    trust: TrustStore,
    trust_path: PathBuf,
    name: String,
    label: String,
    station: String,
    now: u32,
    /// COBS bytes of the frame coming in from the modem.
    rx: Vec<u8>,
    rx_bad: bool,
    tx: Vec<u8>,
    last_tx_ms: u32,
    events: VecDeque<UiEvent>,
    dir_building: Vec<DirLine>,
    dir_pending: bool,
    dir_due: u32,
    ringing: Option<(String, String, String)>,
    call: Option<Call>,
    /// Hangup sent: CallState for that call is ignored until it ends.
    hung_up: bool,
    stats: CallStats,
    muted: bool,
    link_code: Option<(Zeroizing<String>, u32)>,
    link_offer: Option<LinkOffer>,
    link_join: Option<JoinState>,
    /// Our own devices to publish as removed (signed by our person key).
    removals: Vec<[u8; 32]>,
}

impl Engine {
    /// A new engine for this identity; the trust store is read from (and
    /// saved to) `trust_path`. Queues the session start: a 0x00 byte, Hello
    /// and Register (the cert's name and label; a name or label the
    /// protocol refuses gives an Error event and no Register).
    pub fn new(id: Identity, trust_path: PathBuf) -> Self {
        let trust = TrustStore::load(&trust_path);
        let (name, label) = id
            .cert()
            .map(|c| (c.name.clone(), c.label.clone()))
            .unwrap_or_default();
        let mut e = Engine {
            id,
            trust,
            trust_path,
            name,
            label,
            station: String::new(),
            now: 0,
            rx: Vec::new(),
            rx_bad: false,
            tx: vec![0],
            last_tx_ms: 0,
            events: VecDeque::new(),
            dir_building: Vec::new(),
            dir_pending: false,
            dir_due: 0,
            ringing: None,
            call: None,
            hung_up: false,
            stats: CallStats::default(),
            muted: false,
            link_code: None,
            link_offer: None,
            link_join: None,
            removals: Vec::new(),
        };
        e.send(msg::HELLO, &[VHP_VERSION]);
        e.register();
        e
    }

    fn register(&mut self) {
        if valid_voice_name(&self.name) && valid_label(&self.label) {
            let mut p = Vec::new();
            push_str(&mut p, &self.name);
            push_str(&mut p, &self.label);
            self.send(msg::REGISTER, &p);
        } else {
            self.event(UiEvent::Error(
                "no valid voice name and label (name 1 to 16 printable, no ':'; label 1 to 8, no ':' or '/')".into(),
            ));
        }
    }

    // ---- What the app reads.

    /// This device's identity (save it after [`UiEvent::Linked`]).
    pub fn identity(&self) -> &Identity {
        &self.id
    }

    /// Our voice name and device label.
    pub fn me(&self) -> (&str, &str) {
        (&self.name, &self.label)
    }

    /// Our modem's station name (from HelloAck).
    pub fn station(&self) -> &str {
        &self.station
    }

    /// Numbers from the current or last call.
    pub fn stats(&self) -> CallStats {
        self.stats
    }

    /// The two words of the call, once the scrambler is up.
    pub fn words(&self) -> Option<[&'static str; 2]> {
        self.call.as_ref()?.session.as_ref().map(|s| s.words)
    }

    /// The push to talk floor (Free when no call).
    pub fn floor(&self) -> Floor {
        self.call.as_ref().map_or(Floor::Free, |c| c.floor)
    }

    /// The far side said it muted its mic.
    pub fn peer_muted(&self) -> bool {
        self.call.as_ref().is_some_and(|c| c.peer_muted)
    }

    /// The far side of the call: voice name, device label and modem
    /// station (as CallState gives them; label and station may be empty).
    pub fn peer(&self) -> Option<(String, String, String)> {
        self.call.as_ref().map(|c| {
            (
                c.peer_name.clone(),
                c.peer_label.clone(),
                c.peer_station.clone(),
            )
        })
    }

    /// True while a call is placed, ringing out, or up.
    pub fn in_call(&self) -> bool {
        self.call.is_some()
    }

    // ---- Bytes.

    /// Bytes from the modem (any split).
    pub fn on_modem_bytes(&mut self, b: &[u8]) {
        for &x in b {
            if x != 0 {
                if self.rx.len() < VHP_WIRE_MAX {
                    self.rx.push(x);
                } else {
                    self.rx_bad = true;
                }
                continue;
            }
            let mut f = [0u8; VHP_FRAME_MAX];
            let n = if self.rx_bad || self.rx.is_empty() {
                None
            } else {
                cobs_decode(&self.rx, &mut f)
            };
            self.rx.zeroize();
            self.rx.clear();
            self.rx_bad = false;
            if let Some(n) = n.filter(|&n| n > 0) {
                let frame = f.get(..n).unwrap_or(&[]).to_vec();
                self.on_frame(&frame);
            }
            f.zeroize();
        }
    }

    /// Bytes for the modem (everything queued).
    pub fn take_modem_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    /// Next thing to show.
    pub fn poll_event(&mut self) -> Option<UiEvent> {
        self.events.pop_front()
    }

    fn event(&mut self, e: UiEvent) {
        self.events.push_back(e);
    }

    fn send(&mut self, t: u8, p: &[u8]) {
        let mut f = Vec::with_capacity(1 + p.len());
        f.push(t);
        f.extend_from_slice(p);
        let mut buf = [0u8; VHP_WIRE_MAX];
        if let Some(n) = cobs_encode(&f, &mut buf) {
            self.tx.extend_from_slice(buf.get(..n).unwrap_or(&[]));
            self.last_tx_ms = self.now;
        }
        f.zeroize();
        buf.zeroize();
    }

    /// Put a control message on the call's stream (ControlOut frames).
    fn send_ctrl(&mut self, c: Control) {
        let bytes = c.encode();
        for chunk in bytes.chunks(CTRL_CHUNK) {
            self.send(msg::CONTROL_OUT, chunk);
        }
    }

    // ---- Audio.

    /// Mic audio, 16 kHz mono, any amount. Dropped unless it may be sent.
    pub fn mic(&mut self, pcm: &[i16]) {
        let muted = self.muted;
        let Some(c) = self.call.as_mut() else {
            return;
        };
        let fs = c.enc.as_ref().map_or(0, |e| e.frame_samples());
        if !c.can_send(muted) {
            c.mic_buf.clear();
            if c.waiting_for_floor(muted) {
                if !c.release_pending {
                    c.pre.extend_from_slice(pcm);
                    let over = c.pre.len().saturating_sub(fs * PRE_FRAMES);
                    c.pre.drain(..over);
                }
            } else {
                c.pre.clear();
                if std::mem::take(&mut c.release_pending) {
                    // No longer waiting (muted, or the call changed): the
                    // key the modem still holds goes up now.
                    self.talk_key(false);
                }
            }
            return;
        }
        if !c.pre.is_empty() {
            // The floor just became ours: what was said while waiting goes
            // out first.
            let mut b = std::mem::take(&mut c.pre);
            b.append(&mut c.mic_buf);
            c.mic_buf = b;
        }
        let release = std::mem::take(&mut c.release_pending);
        if !release {
            c.mic_buf.extend_from_slice(pcm);
        }
        if fs == 0 {
            return;
        }
        let mut frames = Vec::new();
        while c.mic_buf.len() >= fs {
            frames.push(c.mic_buf.drain(..fs).collect::<Vec<i16>>());
        }
        for f in frames {
            self.send_voice(&f);
        }
        if release {
            // TALK went up while waiting: the kept words are out, now the
            // key goes up (the rest goes out as the last frame).
            self.talk_key(false);
        }
    }

    /// Encode, seal and send one frame (short input is padded).
    fn send_voice(&mut self, frame: &[i16]) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        let max = c.plan.map_or(0, |p| usize::from(p.max_sealed_bytes));
        let (Some(enc), Some(s)) = (c.enc.as_mut(), c.session.as_mut()) else {
            return;
        };
        let mut bytes = Vec::new();
        enc.encode(frame, &mut bytes);
        let sealed = s.seal(&bytes);
        if sealed.is_empty() {
            // The frame counter is spent: never reuse a nonce.
            self.hangup();
            return;
        }
        if sealed.len() > max {
            // Larger than CallState's max_sealed: the modem would refuse it.
            self.stats.frames_oversized = self.stats.frames_oversized.saturating_add(1);
            return;
        }
        self.send(msg::VOICE_OUT, &sealed);
        self.stats.frames_sent = self.stats.frames_sent.saturating_add(1);
    }

    /// Speaker audio, 16 kHz mono: fills `out` (silence when nothing plays).
    pub fn speaker(&mut self, out: &mut [i16]) {
        let now = self.now;
        let mut i = 0;
        while i < out.len() {
            let Some(c) = self.call.as_mut().filter(|c| c.audio_ok) else {
                break;
            };
            if c.play.is_empty() {
                let was = c.jitter.started();
                match c.jitter.pop() {
                    Some(pcm) => {
                        c.play.extend(pcm);
                        c.conceal_run = 0;
                        if self.stats.first_audio_ms.is_none() {
                            self.stats.first_audio_ms = Some(now);
                        }
                    }
                    None => {
                        let lost_mid = was && c.jitter.started() && c.jitter.buffered() > 0;
                        let short = c.plan.is_some_and(|p| p.frame_ms <= 200);
                        let underrun = was && !c.jitter.started() && short && c.conceal_run == 0;
                        if (lost_mid && c.conceal_run < CONCEAL_MAX) || underrun {
                            if let Some(d) = c.dec.as_mut() {
                                let mut v = Vec::new();
                                d.conceal(&mut v);
                                c.play.extend(v);
                                c.conceal_run += 1;
                                self.stats.frames_concealed =
                                    self.stats.frames_concealed.saturating_add(1);
                            }
                        }
                    }
                }
                if c.play.is_empty() {
                    break;
                }
            }
            let n = c.play.len().min(out.len() - i);
            for (d, s) in out.iter_mut().skip(i).take(n).zip(c.play.drain(..n)) {
                *d = s;
            }
            i += n;
        }
        if let Some(rest) = out.get_mut(i..) {
            rest.fill(0);
        }
    }

    fn on_voice_in(&mut self, p: &[u8]) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        let opened = match (c.audio_ok && c.peer_live, c.session.as_mut()) {
            (true, Some(s)) => s.open(p),
            _ => None,
        };
        let (Some(plain), Some(ctr)) = (
            opened,
            p.get(..4)
                .and_then(|b| b.try_into().ok())
                .map(u32::from_le_bytes),
        ) else {
            self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
            return;
        };
        let Some(d) = c.dec.as_mut() else {
            return;
        };
        let mut pcm = Vec::new();
        d.decode(&plain, &mut pcm);
        c.jitter.push(ctr, pcm);
        self.stats.frames_heard = self.stats.frames_heard.saturating_add(1);
    }

    // ---- User actions.

    /// Call a voice name (`label`: one device, or None / empty for any).
    pub fn call(&mut self, name: &str, label: Option<&str>) {
        if self.call.is_some() || self.ringing.is_some() {
            self.event(UiEvent::Error("a call is already up".into()));
            return;
        }
        let name = name.trim();
        let label = label.map(str::trim).unwrap_or("");
        if !valid_voice_name(name) || (!label.is_empty() && !valid_label(label)) {
            self.event(UiEvent::Error(format!("not a voice name: {name}")));
            return;
        }
        let mut p = Vec::new();
        push_str(&mut p, name);
        push_str(&mut p, label);
        self.send(msg::CALL, &p);
        self.new_call(Call::new(true, name, label, ""));
    }

    fn new_call(&mut self, c: Call) {
        self.stats = CallStats::default();
        self.hung_up = false;
        self.call = Some(c);
    }

    /// Pick up the call that rings.
    pub fn answer(&mut self) {
        let Some((name, label, station)) = self.ringing.take() else {
            return;
        };
        if self.call.is_some() {
            return;
        }
        self.send(msg::ANSWER, &[]);
        self.new_call(Call::new(false, &name, &label, &station));
    }

    /// Decline the call that rings.
    pub fn decline(&mut self) {
        if self.ringing.take().is_some() {
            self.send(msg::DECLINE, &[]);
        }
    }

    /// Hang up (or stop dialing). The call's keys are wiped at once; the
    /// modem's Ended follows.
    pub fn hangup(&mut self) {
        if self.call.is_some() {
            self.send(msg::HANGUP, &[]);
            self.end_call();
            self.hung_up = true;
        }
    }

    /// The push to talk key (button or space bar). Sent to the modem only
    /// on a change. Key down only on a push to talk tier once audio may
    /// flow; key up whenever the modem was told the key is down, in any
    /// mode (so a key held across a retrain to duplex is never left down).
    /// On key up the last part of a frame still goes out while the floor is
    /// ours.
    pub fn talk_key(&mut self, down: bool) {
        let muted = self.muted;
        let Some(c) = self.call.as_mut() else {
            return;
        };
        if down && c.release_pending {
            // Down again before the kept words went out: go on talking.
            c.release_pending = false;
            return;
        }
        if c.key_down == down {
            return;
        }
        if !down && c.waiting_for_floor(muted) && !c.pre.is_empty() {
            // Let go before the floor came: keep the key down at the modem
            // until the words said meanwhile are sent (`mic`).
            c.release_pending = true;
            return;
        }
        let ptt = c.plan.is_some_and(|p| p.mode != TalkMode::Duplex);
        if down && (!ptt || !c.audio_ok || !c.connected()) {
            return;
        }
        // The tail is cut while the key still counts as down.
        let tail = if !down && c.can_send(muted) && !c.mic_buf.is_empty() {
            Some(std::mem::take(&mut c.mic_buf))
        } else {
            None
        };
        if let Some(t) = tail {
            self.send_voice(&t);
        }
        if let Some(c) = self.call.as_mut() {
            c.key_down = down;
            c.pre.clear();
            c.release_pending = false;
            if !down {
                c.mic_buf.clear();
            }
        }
        self.send(msg::TALK_KEY, &[u8::from(down)]);
    }

    /// The user compared the two words and they match: store the device
    /// (and its person key) as checked and let audio flow. Nothing for a
    /// Conflict, which stays muted.
    pub fn accept_words(&mut self) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        let (Some(s), Some(t)) = (c.session.as_ref(), c.trust) else {
            return;
        };
        if !c.peer_live {
            return;
        }
        if t == Trust::Conflict {
            let n = c.peer_name.clone();
            self.conflict_warning(&n);
            return;
        }
        self.trust.mark_checked(&c.peer_name, &c.peer_label, s);
        c.trust = Some(Trust::Checked);
        c.audio_ok = true;
        if let Err(e) = self.trust.save(&self.trust_path) {
            self.event(UiEvent::Error(format!(
                "could not save the checked contacts: {e}"
            )));
        }
        self.after_trust();
    }

    /// Talk without checking the words, for this call only. Nothing for a
    /// Conflict, which stays muted.
    pub fn talk_anyway(&mut self) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        if !c.peer_live || c.session.is_none() {
            return;
        }
        if c.trust == Some(Trust::Conflict) {
            let n = c.peer_name.clone();
            self.conflict_warning(&n);
            return;
        }
        c.audio_ok = true;
        self.after_trust();
    }

    fn conflict_warning(&mut self, name: &str) {
        self.event(UiEvent::Error(format!(
            "NOT THE {} YOU CHECKED: the call stays muted",
            name.to_ascii_uppercase()
        )));
    }

    /// Mute our mic (the far side is told once per change).
    pub fn set_mute(&mut self, on: bool) {
        if self.muted == on {
            return;
        }
        self.muted = on;
        self.send_mute();
    }

    fn send_mute(&mut self) {
        let on = self.muted;
        let Some(s) = self
            .call
            .as_mut()
            .filter(|c| c.peer_live)
            .and_then(|c| c.session.as_mut())
        else {
            return;
        };
        let sealed = s.seal(&[u8::from(on)]);
        if !sealed.is_empty() {
            self.send_ctrl(Control::Mute(sealed));
        }
    }

    /// Show a new link code on this device (it holds the person key). The
    /// code is also queued as [`UiEvent::LinkCode`]. It is used once (by the
    /// first link request in a call), and wiped after use or after
    /// [`LINK_CODE_MS`].
    pub fn start_link_offer(&mut self) -> String {
        self.link_offer = None;
        self.link_code = None;
        if self.id.person_pub().is_none() {
            // No code is made or shown: there is nothing to give.
            self.event(UiEvent::Error(
                "this device has no person key to link with".into(),
            ));
            return String::new();
        }
        let code = new_link_code();
        self.link_code = Some((Zeroizing::new(code.clone()), self.now));
        self.event(UiEvent::LinkCode(code.clone()));
        code
    }

    /// Join this person with the code shown on their other device. The link
    /// runs in a call to that device: the request goes out once the call is
    /// live. 6 digits; spaces around it are ignored.
    pub fn join_link(&mut self, code: &str) {
        let c = Zeroizing::new(code.trim().to_string());
        if c.len() != 6 || !c.bytes().all(|b| b.is_ascii_digit()) {
            self.event(UiEvent::Error("a link code is 6 digits".into()));
            return;
        }
        if self.id.cert().is_none() {
            self.event(UiEvent::Error("this device has no label to keep".into()));
            return;
        }
        let (join, req) = LinkJoin::start(&c);
        self.link_join = Some(JoinState {
            join: Some(join),
            req,
            started_ms: self.now,
            sent_ms: None,
        });
        self.link_kick();
    }

    /// Send the link request when a live call is up.
    fn link_kick(&mut self) {
        let live = self
            .call
            .as_ref()
            .is_some_and(|c| c.peer_live && c.session.is_some());
        let now = self.now;
        let Some(j) = self.link_join.as_mut().filter(|j| j.sent_ms.is_none()) else {
            return;
        };
        if !live {
            return;
        }
        j.sent_ms = Some(now);
        let req = j.req.clone();
        self.send_ctrl(Control::LinkMsg(req));
    }

    fn link_failed(&mut self, why: &str) {
        self.link_join = None;
        self.link_offer = None;
        let text = if why.is_empty() {
            "link failed".to_string()
        } else {
            format!("link failed: {why}")
        };
        self.event(UiEvent::Error(text));
    }

    /// Run an AT line on the modem (settings); the answer comes as
    /// [`UiEvent::AtReply`]. False when the line is too long.
    pub fn at_line(&mut self, line: &str) -> bool {
        if line.is_empty() || line.len() > AT_LINE_MAX {
            return false;
        }
        let b = Zeroizing::new(line.as_bytes().to_vec());
        self.send(msg::AT_LINE, &b);
        true
    }

    /// Ask the modem for its directory now ([`UiEvent::Directory`] follows).
    pub fn refresh_directory(&mut self) {
        if !self.dir_pending {
            self.dir_pending = true;
            self.dir_building.clear();
            self.dir_due = self.now.wrapping_add(DIR_EVERY_MS);
            self.send(msg::DIR_LIST, &[]);
        }
    }

    /// Publish one of our own devices as removed (a lost device): at each
    /// later call with a contact, signed by our person key. Kept for this
    /// engine's life only; the app keeps the list and gives it again at
    /// start.
    pub fn remove_my_device(&mut self, device_pub: [u8; 32]) {
        if !self.removals.contains(&device_pub) {
            self.removals.push(device_pub);
        }
    }

    /// Stop linking: the code shown here (if any) is wiped and an offer or
    /// a join in progress is dropped. Nothing is sent.
    pub fn cancel_link(&mut self) {
        self.link_code = None;
        self.link_offer = None;
        self.link_join = None;
    }

    /// A link code is shown or a link is running on this device.
    pub fn linking(&self) -> bool {
        self.link_code.is_some() || self.link_offer.is_some() || self.link_join.is_some()
    }

    /// The user's checked contacts (for the app's list).
    pub fn contacts(&self) -> Vec<crate::trust::ContactView> {
        self.trust.contacts()
    }

    /// Remove a device from a contact in our trust store (it then shows as
    /// a Conflict).
    pub fn remove_contact_device(&mut self, name: &str, device_pub: [u8; 32]) {
        self.trust.remove_device(name, device_pub);
        if let Err(e) = self.trust.save(&self.trust_path) {
            self.event(UiEvent::Error(format!(
                "could not save the checked contacts: {e}"
            )));
        }
    }

    /// Close the session (the app is quitting): the modem drops our
    /// registration at once and hangs up a call we own.
    pub fn leave(&mut self) {
        self.end_call();
        self.send(msg::LEAVE, &[]);
    }

    /// Time passes: heartbeats, the directory, link timeouts. `now_ms` is
    /// the app's clock (any start, it may wrap).
    pub fn tick(&mut self, now_ms: u32) {
        self.now = now_ms;
        if self.call.is_none()
            && self.ringing.is_none()
            && now_ms.wrapping_sub(self.dir_due) < u32::MAX / 2
        {
            // A DirList still open at the next due time lost its DirEnd
            // (a session reset, say): ask again rather than wait forever.
            self.dir_pending = false;
            self.refresh_directory();
        }
        if self
            .link_code
            .as_ref()
            .is_some_and(|(_, t)| now_ms.wrapping_sub(*t) >= LINK_CODE_MS)
        {
            self.link_code = None;
        }
        let frame_ms = self
            .call
            .as_ref()
            .and_then(|c| c.plan)
            .map_or(0, |p| u32::from(p.frame_ms));
        let late = self.link_join.as_ref().is_some_and(|j| match j.sent_ms {
            Some(t) => now_ms.wrapping_sub(t) >= LINK_BASE_MS + LINK_FRAMES * frame_ms,
            None => now_ms.wrapping_sub(j.started_ms) >= LINK_CODE_MS,
        });
        if late {
            self.link_failed("");
        }
        self.link_kick();
        if now_ms.wrapping_sub(self.last_tx_ms) >= HEARTBEAT_MS {
            self.send(msg::HEARTBEAT, &[]);
        }
    }

    // ---- Modem frames.

    fn on_frame(&mut self, f: &[u8]) {
        let Some((&t, p)) = f.split_first() else {
            return;
        };
        let mut r = Rd(p);
        match t {
            msg::HELLO_ACK => {
                let _ = r.u8();
                if let Some(s) = r.str() {
                    self.station = s;
                }
            }
            msg::RING => {
                let (Some(st), Some(n), Some(l)) = (r.str(), r.str(), r.str()) else {
                    return;
                };
                if self.ringing.is_none() && self.call.is_none() {
                    self.ringing = Some((n.clone(), l.clone(), st.clone()));
                    self.event(UiEvent::Ringing {
                        name: n,
                        label: l,
                        station: st,
                    });
                }
            }
            msg::CALL_STATE => self.on_call_state(&mut r),
            msg::ANSWERED_ELSEWHERE => self.on_end(UiEvent::AnsweredElsewhere),
            msg::DECLINED => self.on_end(UiEvent::Declined),
            msg::NO_ANSWER => self.on_end(UiEvent::NoAnswer),
            msg::RING_ENDED | msg::ENDED => self.on_end(UiEvent::Ended),
            msg::VOICE_IN => self.on_voice_in(p),
            msg::CONTROL_IN => {
                if let Some(c) = self.call.as_mut() {
                    c.ctrl.push(p);
                }
                self.on_control();
            }
            msg::FLOOR => {
                let fl = match r.u8() {
                    Some(1) => Floor::Mine,
                    Some(2) => Floor::Theirs,
                    _ => Floor::Free,
                };
                if let Some(c) = self.call.as_mut() {
                    if c.floor != fl {
                        c.floor = fl;
                        c.mic_buf.clear();
                        self.event(UiEvent::Floor(fl));
                    }
                }
            }
            msg::ERROR => {
                let (Some(ty), Some(code)) = (r.u8(), r.u8()) else {
                    return;
                };
                // Races with the end of a call are not worth showing.
                if code == err::NO_CALL && self.call.is_none() {
                    return;
                }
                let why = match code {
                    err::UNKNOWN => "unknown message",
                    err::BAD_PAYLOAD => "bad payload",
                    err::NO_CALL => "no call",
                    err::REFUSED => "refused",
                    err::BUSY => "busy",
                    err::AT_REFUSED => "not allowed from the app",
                    _ => "error",
                };
                self.event(UiEvent::Error(format!("modem: message 0x{ty:02X} {why}")));
                // A refused Call or Answer starts no call at the modem: no
                // CallState or end message will come, so end ours here.
                let refused_start = (ty == msg::CALL || ty == msg::ANSWER)
                    && self.call.as_ref().is_some_and(|c| !c.connected());
                if refused_start {
                    self.on_end(UiEvent::Ended);
                }
            }
            msg::AT_REPLY => {
                let last = r.u8().is_some_and(|f| f & 1 != 0);
                let text = String::from_utf8_lossy(r.0).into_owned();
                self.event(UiEvent::AtReply { text, last });
            }
            msg::DIR_ENTRY => self.on_dir_entry(&mut r),
            msg::DIR_END => {
                self.dir_pending = false;
                let lines = std::mem::take(&mut self.dir_building);
                self.event(UiEvent::Directory(lines));
            }
            msg::SCOPE => {
                let (Some(t_ms), Some(step_ms), Some(n), Some(mask)) =
                    (r.u32(), r.u8(), r.u8(), r.u8())
                else {
                    return;
                };
                let Some(d) = r.bytes(usize::from(n).min(8)) else {
                    return;
                };
                let samples = d
                    .iter()
                    .enumerate()
                    .map(|(k, &b)| (b as i8, mask & (1 << k) != 0))
                    .collect();
                self.event(UiEvent::Scope {
                    t_ms,
                    step_ms,
                    samples,
                });
            }
            msg::AIR_EVENT => {
                let (Some(flags), Some(sf), Some(ftype), Some(sub), Some(len)) =
                    (r.u8(), r.u8(), r.u8(), r.u8(), r.u8())
                else {
                    return;
                };
                let (Some(start_ms), Some(air_ms), Some(rssi), Some(snr)) =
                    (r.u32(), r.u16(), r.i16(), r.i16())
                else {
                    return;
                };
                let rx = flags & 1 != 0;
                self.event(UiEvent::Air(AirFrame {
                    rx,
                    sf,
                    ftype,
                    sub,
                    len,
                    start_ms,
                    air_ms,
                    rssi_dbm: (rx && rssi != i16::MIN).then_some(rssi),
                    snr_x4: (rx && snr != i16::MIN).then_some(snr),
                }));
            }
            _ => {}
        }
    }

    fn on_dir_entry(&mut self, r: &mut Rd<'_>) {
        let (Some(kind), Some(flags), Some(sf)) = (r.u8(), r.u8(), r.u8()) else {
            return;
        };
        let (Some(_age), Some(_snr), Some(_rssi), Some(_svc)) = (r.u32(), r.i16(), r.i16(), r.u8())
        else {
            return;
        };
        let (Some(name), Some(label), Some(via)) = (r.str(), r.str(), r.str()) else {
            return;
        };
        let best_tier = tier_from_sf(sf);
        let mode_hint = talk_plan(best_tier.unwrap_or(Tier::Sf12)).mode;
        let voice = kind == 1;
        let trust = if voice && self.trust.name_checked(&name) {
            Trust::Checked
        } else {
            Trust::NotChecked
        };
        let station = if voice { via } else { name.clone() };
        self.dir_building.push(DirLine {
            name,
            label,
            station,
            best_tier,
            mode_hint,
            trust,
            pinned: flags & 1 != 0,
        });
    }

    fn on_end(&mut self, e: UiEvent) {
        self.ringing = None;
        self.end_call();
        self.hung_up = false;
        self.event(e);
    }

    /// Drop the call: keys and codecs go (wiped on drop).
    fn end_call(&mut self) {
        if self.call.take().is_some() {
            let busy = self.link_offer.is_some()
                || self.link_join.as_ref().is_some_and(|j| j.sent_ms.is_some());
            if busy {
                self.link_failed("the call ended");
            }
        }
    }

    fn on_call_state(&mut self, r: &mut Rd<'_>) {
        let (Some(state), Some(flags), Some(sf), Some(codec), Some(mode)) =
            (r.u8(), r.u8(), r.u8(), r.u8(), r.u8())
        else {
            return;
        };
        let (Some(frame_ms), Some(max_sealed), Some(_snr), Some(_rssi)) =
            (r.u16(), r.u8(), r.i16(), r.i16())
        else {
            return;
        };
        let (Some(st), Some(name), Some(label)) = (r.str(), r.str(), r.str()) else {
            return;
        };
        self.stats.state = state;
        if state == call_state::IDLE {
            self.hung_up = false;
            self.end_call();
            return;
        }
        if self.hung_up {
            return;
        }
        let now = self.now;
        if self.call.is_none() {
            // A call we did not see start (the app restarted): follow it.
            self.new_call(Call::new(flags & 2 != 0, &name, &label, &st));
        }
        let plan = match (codec_from_u8(codec), mode_from_u8(mode)) {
            (Some(codec), Some(mode)) if frame_ms > 0 => Some(TalkPlan {
                codec,
                mode,
                frame_ms,
                max_sealed_bytes: max_sealed,
            }),
            _ => None,
        };
        let Some(c) = self.call.as_mut() else {
            return;
        };
        c.placed = flags & 2 != 0;
        if !st.is_empty() {
            c.peer_station = st;
        }
        if !label.is_empty() {
            c.peer_label = label;
        }
        if !name.is_empty() {
            c.peer_name = name;
        }
        let was_connected = c.connected();
        c.state = state;
        c.flags = flags;
        self.stats.sf = sf;
        self.stats.plan = plan;
        if flags & 1 != 0 && self.stats.slots_ms.is_none() {
            self.stats.slots_ms = Some(now);
        }
        let mut changed = None;
        let mut key_up = false;
        if let Some(p) = plan {
            if c.plan != Some(p) {
                let had = c.plan.is_some();
                c.set_plan(p);
                if had && c.peer_live {
                    changed = Some(p);
                }
                // A retrain to duplex: let go of the key, so the modem's key
                // is not still down when a later retrain brings back push
                // to talk.
                if p.mode == TalkMode::Duplex && c.key_down {
                    c.key_down = false;
                    key_up = true;
                }
            }
        }
        if key_up {
            self.send(msg::TALK_KEY, &[0]);
        }
        let Some(c) = self.call.as_mut() else {
            return;
        };
        let start = c.connected() && !was_connected && !c.hs_started;
        if c.connected() && self.stats.connected_ms.is_none() {
            self.stats.connected_ms = Some(now);
        }
        if let Some(p) = changed {
            self.event(UiEvent::PlanChanged(p));
        }
        if start {
            self.start_handshake();
        }
    }

    // ---- The scrambler.

    fn start_handshake(&mut self) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        if c.hs_started {
            return;
        }
        c.hs_started = true;
        let role = if c.placed {
            Role::Caller
        } else {
            Role::Answerer
        };
        let (hs, out) = Handshake::start(&self.id, role);
        c.hs = Some(hs);
        match role {
            Role::Caller => self.send_ctrl(Control::Handshake(out)),
            // The answerer's hello waits for the caller's commit.
            Role::Answerer => c.held_hello = Some(out),
        }
    }

    fn on_control(&mut self) {
        loop {
            let Some(c) = self.call.as_mut() else {
                return;
            };
            let Some(m) = c.ctrl.next_msg() else {
                if c.ctrl.broken() && !c.failed {
                    c.failed = true;
                    self.event(UiEvent::Error(
                        "control stream broken: the call stays muted".into(),
                    ));
                }
                return;
            };
            match m {
                Control::Handshake(h) => self.on_handshake(&h),
                Control::Hello(b) => self.on_sealed_hello(&b),
                Control::Mute(b) => {
                    if let Some(c) = self.call.as_mut().filter(|c| c.peer_live) {
                        if let Some(v) = c.session.as_mut().and_then(|s| s.open(&b)) {
                            c.peer_muted = v.first() == Some(&1);
                        }
                    }
                }
                Control::RemoveDevice(b) => self.on_remove(&b),
                Control::LinkMsg(l) => self.on_link(&l),
            }
        }
    }

    fn on_handshake(&mut self, m: &[u8]) {
        if self
            .call
            .as_ref()
            .is_some_and(|c| c.connected() && !c.hs_started)
        {
            self.start_handshake();
        }
        let Some(c) = self.call.as_mut() else {
            return;
        };
        if c.failed || c.session.is_some() {
            return;
        }
        let result = if c.placed {
            // Caller: the answerer's hello; send the reveal, then finish.
            let Some(mut hs) = c.hs.take() else {
                return;
            };
            match hs.on_peer(m) {
                Ok(Some(reveal)) => match hs.finish(&[]) {
                    Ok(s) => Ok(Some((reveal, s))),
                    Err(e) => Err(e),
                },
                Ok(None) => Err(crate::ScrambleError::BadHello),
                Err(e) => Err(e),
            }
        } else if m.first() == Some(&MSG_COMMIT) {
            // Answerer: the commit; only now may our hello go out.
            let Some(hs) = c.hs.as_mut() else {
                return;
            };
            match hs.on_peer(m) {
                Ok(_) => {
                    if let Some(h) = c.held_hello.take() {
                        self.send_ctrl(Control::Handshake(h));
                    }
                    return;
                }
                Err(e) => Err(e),
            }
        } else if m.first() == Some(&MSG_REVEAL) {
            let Some(hs) = c.hs.take() else {
                return;
            };
            hs.finish(m).map(|s| Some((Vec::new(), s)))
        } else {
            Err(crate::ScrambleError::BadHello)
        };
        match result {
            Ok(Some((reveal, s))) => {
                if !reveal.is_empty() {
                    self.send_ctrl(Control::Handshake(reveal));
                }
                let Some(c) = self.call.as_mut() else {
                    return;
                };
                c.session = Some(s);
                let sealed = c
                    .session
                    .as_mut()
                    .map(|s| s.seal(control::HELLO_TAG))
                    .unwrap_or_default();
                self.send_ctrl(Control::Hello(sealed));
            }
            Ok(None) => {}
            Err(e) => {
                if let Some(c) = self.call.as_mut() {
                    c.failed = true;
                    c.hs = None;
                    c.held_hello = None;
                }
                self.event(UiEvent::Error(format!(
                    "scrambler: {e}: the call stays muted"
                )));
            }
        }
    }

    /// The peer's key confirm: it is live once this opens.
    fn on_sealed_hello(&mut self, b: &[u8]) {
        let Some(c) = self.call.as_mut() else {
            return;
        };
        if c.peer_live {
            return;
        }
        let Some(s) = c.session.as_mut() else {
            return;
        };
        if s.open(b).as_deref() != Some(control::HELLO_TAG) {
            return;
        }
        c.peer_live = true;
        let s = c.session.as_ref().map(|s| {
            (
                s.words,
                self.trust.verdict(&c.peer_name, &c.peer_label, s),
                s.peer_cert.as_ref().map(|k| k.name.clone()),
            )
        });
        let Some((words, mut verdict, cert_name)) = s else {
            return;
        };
        // A cert that names someone else is not the person checked.
        if verdict == Trust::Checked
            && cert_name.is_some_and(|n| !n.eq_ignore_ascii_case(&c.peer_name))
        {
            verdict = Trust::NotChecked;
        }
        c.trust = Some(verdict);
        let plan = c
            .plan
            .unwrap_or_else(|| talk_plan(tier_from_sf(self.stats.sf).unwrap_or(Tier::Sf12)));
        let name = c.peer_name.clone();
        self.event(UiEvent::Connected {
            plan,
            words,
            trust: verdict,
        });
        // Mute and a link request go first: once VoiceReady is out the
        // stream moves in small listener pieces.
        if self.muted {
            self.send_mute();
        }
        self.link_kick();
        match verdict {
            Trust::Checked => {
                if let Some(c) = self.call.as_mut() {
                    c.audio_ok = true;
                }
                self.after_trust();
            }
            Trust::Conflict => self.conflict_warning(&name),
            Trust::NotChecked => {}
        }
    }

    /// The trust decision is made: removals (if any), then VoiceReady.
    fn after_trust(&mut self) {
        if self.call.as_ref().is_none_or(|c| c.ready_sent) {
            return;
        }
        if !self.removals.is_empty() {
            let mut out = Vec::new();
            for d in self.removals.clone() {
                let Some(sig) = self.id.sign_person(&control::removal_signed_bytes(&d)) else {
                    break;
                };
                let mut plain = Vec::with_capacity(96);
                plain.extend_from_slice(&d);
                plain.extend_from_slice(&sig);
                if let Some(s) = self.call.as_mut().and_then(|c| c.session.as_mut()) {
                    let sealed = s.seal(&plain);
                    if !sealed.is_empty() {
                        out.push(sealed);
                    }
                }
            }
            for sealed in out {
                self.send_ctrl(Control::RemoveDevice(sealed));
            }
        }
        if let Some(c) = self.call.as_mut() {
            c.ready_sent = true;
        }
        self.send(msg::VOICE_READY, &[]);
    }

    /// A removal from the peer: applied when its person key signed it and
    /// the peer is a checked contact.
    fn on_remove(&mut self, b: &[u8]) {
        let Some(c) = self.call.as_mut().filter(|c| c.peer_live) else {
            return;
        };
        let checked = c.trust == Some(Trust::Checked);
        let Some(s) = c.session.as_mut() else {
            return;
        };
        let Some(plain) = s.open(b).map(Zeroizing::new) else {
            return;
        };
        let (Some(dev), Some(sig)) = (
            plain.get(..32).and_then(|d| <[u8; 32]>::try_from(d).ok()),
            plain.get(32..).and_then(|d| <[u8; 64]>::try_from(d).ok()),
        ) else {
            return;
        };
        let Some(person) = s.peer_cert.as_ref().map(|k| k.person_pub) else {
            return;
        };
        let ok = VerifyingKey::from_bytes(&person).is_ok_and(|k| {
            k.verify_strict(
                &control::removal_signed_bytes(&dev),
                &Signature::from_bytes(&sig),
            )
            .is_ok()
        });
        if !ok || !checked {
            return;
        }
        let name = c.peer_name.clone();
        self.remove_contact_device(&name, dev);
    }

    // ---- Linking.

    fn on_link(&mut self, m: &[u8]) {
        match m.first().copied() {
            Some(crate::link::MSG_LINK_REQUEST) => {
                // Only from a live peer (its sealed Hello opened), and the
                // code is not spent on anything earlier.
                if !self.call.as_ref().is_some_and(|c| c.peer_live) {
                    return;
                }
                let Some((code, _)) = self.link_code.take() else {
                    return;
                };
                if self.id.person_pub().is_none() {
                    self.link_failed("this device has no person key");
                    return;
                }
                let (offer, out) = LinkOffer::start(&self.id, &code);
                drop(code);
                self.link_offer = Some(offer);
                self.send_ctrl(Control::LinkMsg(out));
            }
            Some(crate::link::MSG_LINK_OFFER) => {
                let Some(j) = self.link_join.as_mut().and_then(|j| j.join.as_mut()) else {
                    return;
                };
                match j.step(m) {
                    Ok(Some(out)) => self.send_ctrl(Control::LinkMsg(out)),
                    Ok(None) => {}
                    Err(e) => self.link_failed(&e.to_string()),
                }
            }
            Some(crate::link::MSG_LINK_JOIN) => {
                let Some(offer) = self.link_offer.take() else {
                    return;
                };
                match offer.step(m) {
                    Ok(out) => {
                        self.send_ctrl(Control::LinkMsg(out));
                        self.event(UiEvent::Linked);
                    }
                    // A wrong code sends nothing: the joiner times out.
                    Err(e) => self.link_failed(&e.to_string()),
                }
            }
            Some(crate::link::MSG_LINK_SEALED) => {
                let Some(j) = self.link_join.as_mut().and_then(|j| j.join.take()) else {
                    return;
                };
                self.link_join = None;
                match j.finish(m, &mut self.id) {
                    Ok(()) => {
                        if let Some(c) = self.id.cert() {
                            self.name = c.name.clone();
                            self.label = c.label.clone();
                        }
                        self.register();
                        self.event(UiEvent::Linked);
                    }
                    Err(e) => self.link_failed(&e.to_string()),
                }
            }
            _ => {}
        }
    }
}
