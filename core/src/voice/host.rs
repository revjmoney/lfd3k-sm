//! The voice host protocol (VHP, spec 6): framing, messages and sessions
//! between a voice app and its modem. The full message reference (every
//! type byte and field) is the "Voice host protocol" section of
//! `docs/FEATURES.md`; this is the modem side.
//!
//! **Framing.** Each frame is COBS encoded and followed by one zero byte.
//! A decoded frame is a type byte and its payload, at most `VHP_FRAME_MAX`
//! (260) bytes. A frame that does not decode (a zero inside a COBS block,
//! too long, empty) is dropped silently; the next zero starts a new frame.
//! Strings are a length byte then that many bytes; integers are little
//! endian.
//!
//! **Sessions.** Up to `VHP_SESSIONS` (4): session 0 is the host stream
//! after `AT+VOICE` (USB or the V3's UART), sessions 1 to 3 are the
//! platform's (TCP, `voice_open`). Any frame keeps a session alive; with
//! none for `VOICE_HEARTBEAT_MS` it closes: a call it owns is hung up at
//! once, its registration lives on for `VOICE_GRACE_MS` (a phone that
//! drops Wi-Fi for a moment keeps its name). `Leave` closes it and drops
//! its registration at once. Session 0 then goes back to AT commands (with
//! NO CARRIER when it had a call up).
//!
//! **Ownership.** The session that dialed, or the session that answered,
//! owns the call: only it gets CallState, Floor, VoiceIn, ControlIn and
//! the end of call message, and only it may hang up, talk, send control
//! bytes or say VoiceReady. A Ring goes to every open session whose
//! registration the call rings; when one of them answers or declines, the
//! others hear AnsweredElsewhere or Declined; when the ring stops with no
//! answer here (the caller gave up or hung up), RingEnded.
//!
//! **Delivery.** Nothing is queued per message: each session keeps pending
//! flags, an error queue and one frame being written, and `voice_output`
//! builds the next frame when the last one has gone, in a fixed order
//! (HelloAck, Error, Ring, end of ring or call, CallState, Floor,
//! ControlIn, VoiceIn, AtReply, DirEntry). So an end of call or a state
//! change is never crowded out by voice. VoiceIn is the link's last heard
//! frame (one is kept; poll `voice_output` every tick). ControlIn reads
//! the call's ARQ stream only as fast as the app takes it; ControlOut that
//! the link cannot take at once is held, and `voice_input` takes no more
//! bytes from that session until it is.
//!
//! **AtLine.** One AT line at a time for the whole modem, run through the
//! same parser and dispatch as the terminal and the menu. Everything it
//! prints goes into one shared capture buffer (never to the terminal) and
//! out as AtReply frames, the last flagged final. AT+DIR? and AT+SCAN
//! finish later; their lines follow as they come.

use super::names::Name16;
use super::plan::TalkMode;
use super::regs::VOICE_REGS;
use super::sched::Floor;
use super::VOICE_DIR_MAX;
use crate::at::AtCmd;
use crate::directory::DIR_MAX;
use crate::link::{Event, Role, State};
use crate::modem::Modem;
use crate::radio::Radio;

/// The protocol version in Hello and HelloAck.
pub const VHP_VERSION: u8 = 1;
/// Sessions per modem: 0 is the host stream after `AT+VOICE`, 1 to 3 the
/// platform's (TCP in the firmware plan).
pub const VHP_SESSIONS: usize = 4;
/// Longest decoded frame: the type byte and the payload.
pub const VHP_FRAME_MAX: usize = 260;
/// Longest frame on the wire: COBS adds one byte per 254 and one more,
/// then the zero delimiter.
pub const VHP_WIRE_MAX: usize = VHP_FRAME_MAX + VHP_FRAME_MAX / 254 + 2;
/// A session with no frame for this long is closed.
pub const VOICE_HEARTBEAT_MS: u32 = 5_000;
/// Most text bytes in one AtReply frame.
pub const VHP_AT_CHUNK: usize = 240;
/// Most bytes in one ControlIn frame.
pub const VHP_CONTROL_MAX: usize = 255;
/// The AtLine capture buffer: the longest output of a command that answers
/// at once (AT+FWD? with 8 forwards, about 780 bytes) fits.
pub(crate) const CAP_BUF: usize = 832;
/// Errors waiting per session (more are dropped).
const ERRQ: usize = 4;
/// While connected, CallState goes out again this often (fresh SNR and
/// RSSI) even when nothing else changed.
const STATE_EVERY_MS: u32 = 1_000;

const _: () = assert!(VHP_AT_CHUNK + 2 <= VHP_FRAME_MAX);
const _: () = assert!(VHP_CONTROL_MAX < VHP_FRAME_MAX);
const _: () = assert!(super::VOICE_MAX < VHP_FRAME_MAX);
const _: () = assert!(VHP_SESSIONS == VOICE_REGS);

/// Message type bytes.
pub mod msg {
    // App to modem.
    pub const HELLO: u8 = 0x01;
    pub const REGISTER: u8 = 0x02;
    pub const UNREGISTER: u8 = 0x03;
    pub const CALL: u8 = 0x04;
    pub const ANSWER: u8 = 0x05;
    pub const DECLINE: u8 = 0x06;
    pub const HANGUP: u8 = 0x07;
    pub const VOICE_OUT: u8 = 0x08;
    pub const CONTROL_OUT: u8 = 0x09;
    pub const TALK_KEY: u8 = 0x0A;
    pub const HEARTBEAT: u8 = 0x0B;
    pub const LEAVE: u8 = 0x0C;
    pub const DIR_LIST: u8 = 0x0D;
    pub const VOICE_READY: u8 = 0x0E;
    pub const AT_LINE: u8 = 0x0F;
    // Modem to app.
    pub const HELLO_ACK: u8 = 0x81;
    pub const RING: u8 = 0x82;
    pub const CALL_STATE: u8 = 0x83;
    pub const ANSWERED_ELSEWHERE: u8 = 0x84;
    pub const DECLINED: u8 = 0x85;
    pub const NO_ANSWER: u8 = 0x86;
    pub const ENDED: u8 = 0x87;
    pub const VOICE_IN: u8 = 0x88;
    pub const CONTROL_IN: u8 = 0x89;
    pub const DIR_ENTRY: u8 = 0x8A;
    pub const DIR_END: u8 = 0x8B;
    pub const FLOOR: u8 = 0x8C;
    pub const ERROR: u8 = 0x8D;
    pub const AT_REPLY: u8 = 0x8E;
    pub const RING_ENDED: u8 = 0x8F;
}

/// Error codes (the second byte of an Error frame).
pub mod err {
    /// A type byte this modem does not know.
    pub const UNKNOWN: u8 = 1;
    /// The payload has the wrong length or a malformed string.
    pub const BAD_PAYLOAD: u8 = 2;
    /// No call (or ring) this session may act on: not ringing here, not
    /// the owner, or the call is not connected.
    pub const NO_CALL: u8 = 3;
    /// The modem said no: a bad name or label, no registration on this
    /// session, a call of any kind already up, the ring already over.
    pub const REFUSED: u8 = 4;
    /// Another AtLine is still running.
    pub const BUSY: u8 = 5;
    /// The AT command is not allowed through AtLine.
    pub const AT_REFUSED: u8 = 6;
}

/// CallState `state` byte.
pub mod call_state {
    pub const IDLE: u8 = 0;
    pub const DIALING: u8 = 1;
    pub const RINGING: u8 = 2;
    pub const TRAINING: u8 = 3;
    pub const CONNECTED: u8 = 4;
    pub const RESYNC: u8 = 5;
}

/// The AT commands an AtLine may not run: they would take the host link
/// away (AT+MODE=, AT+SIMNET=, AT+MENU, AT+VOICE), start or rejoin a data
/// call (ATD, ATDT, ATDP, ATDV, ATA, ATO) or dump the screen onto the host
/// stream (AT+FB, binary, which would break session 0's framing). The
/// queries AT+MODE? and AT+SIMNET? are allowed.
pub fn at_refused(cmd: &AtCmd<'_>) -> bool {
    matches!(
        cmd,
        AtCmd::Mode(_)
            | AtCmd::SimNet(_)
            | AtCmd::Menu
            | AtCmd::Voice
            | AtCmd::Online
            | AtCmd::Answer
            | AtCmd::Dial(_)
            | AtCmd::DialVoice(..)
            | AtCmd::FrameBuffer
    )
}

// Pending messages, one bit each.
const P_HELLO: u16 = 1 << 0;
const P_RING: u16 = 1 << 1;
const P_ANS_ELSE: u16 = 1 << 2;
const P_DECLINED: u16 = 1 << 3;
const P_NO_ANSWER: u16 = 1 << 4;
const P_RING_ENDED: u16 = 1 << 5;
const P_ENDED: u16 = 1 << 6;
const P_STATE: u16 = 1 << 7;
const P_FLOOR: u16 = 1 << 8;
/// The end of ring and end of call messages, in output order.
const ENDS: [(u16, u8); 5] = [
    (P_ANS_ELSE, msg::ANSWERED_ELSEWHERE),
    (P_DECLINED, msg::DECLINED),
    (P_NO_ANSWER, msg::NO_ANSWER),
    (P_RING_ENDED, msg::RING_ENDED),
    (P_ENDED, msg::ENDED),
];

/// COBS encode `src` into `dst` and add the zero delimiter. None when
/// `dst` is too small.
fn cobs_encode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut code_at = 0usize;
    let mut o = 1usize;
    let mut code = 1u8;
    for &b in src {
        if b == 0 {
            *dst.get_mut(code_at)? = code;
            code_at = o;
            o += 1;
            code = 1;
        } else {
            *dst.get_mut(o)? = b;
            o += 1;
            code += 1;
            if code == 0xFF {
                *dst.get_mut(code_at)? = code;
                code_at = o;
                o += 1;
                code = 1;
            }
        }
    }
    *dst.get_mut(code_at)? = code;
    *dst.get_mut(o)? = 0;
    Some(o + 1)
}

/// Decode one COBS frame (without its zero delimiter) into `dst`. None for
/// a zero inside, a block cut short, or a frame longer than `dst`.
fn cobs_decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    let mut o = 0usize;
    while i < src.len() {
        let code = *src.get(i)?;
        if code == 0 {
            return None;
        }
        i += 1;
        for _ in 1..code {
            let b = *src.get(i)?;
            if b == 0 {
                return None;
            }
            *dst.get_mut(o)? = b;
            o += 1;
            i += 1;
        }
        if code < 0xFF && i < src.len() {
            *dst.get_mut(o)? = 0;
            o += 1;
        }
    }
    Some(o)
}

/// A length-prefixed string and the bytes after it.
fn take_str(p: &[u8]) -> Option<(&str, &[u8])> {
    let (&l, rest) = p.split_first()?;
    let (s, rest) = rest.split_at_checked(usize::from(l))?;
    Some((core::str::from_utf8(s).ok()?, rest))
}

/// Exactly two strings.
fn two_strs(p: &[u8]) -> Option<(&str, &str)> {
    let (a, rest) = take_str(p)?;
    let (b, rest) = take_str(rest)?;
    rest.is_empty().then_some((a, b))
}

/// A frame being built: never writes past its buffer.
struct W<'a> {
    b: &'a mut [u8],
    n: usize,
    over: bool,
}

impl W<'_> {
    fn bytes(&mut self, s: &[u8]) -> &mut Self {
        let end = self.n.saturating_add(s.len());
        match self.b.get_mut(self.n..end) {
            Some(d) => {
                d.copy_from_slice(s);
                self.n = end;
            }
            None => self.over = true,
        }
        self
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        self.bytes(&[v])
    }

    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    fn i16(&mut self, v: i16) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    fn str(&mut self, s: &str) -> &mut Self {
        let b = s.as_bytes();
        let b = b.get(..b.len().min(255)).unwrap_or(&[]);
        self.u8(b.len() as u8).bytes(b)
    }

    fn done(&self) -> usize {
        if self.over {
            0
        } else {
            self.n
        }
    }
}

/// One host link session: its COBS decoder, the frame being written to it,
/// and what waits to be said.
pub(crate) struct Session {
    pub(crate) open: bool,
    last_ms: u32,
    /// Drop one LF if it is the very first byte (the LF after the CR of
    /// the `AT+VOICE` line).
    skip_lf: bool,
    /// Decoded bytes of the frame coming in (or a held ControlOut).
    rx: [u8; VHP_FRAME_MAX],
    rx_len: u16,
    /// COBS: bytes left in the current block, its code, a zero owed
    /// before the next block, and the frame is bad (dropped at its end).
    left: u8,
    code: u8,
    zero: bool,
    bad: bool,
    /// ControlOut bytes `rx[a..b]` the link has not taken yet.
    hold: Option<(u16, u16)>,
    /// The encoded frame being written and how much of it went.
    tx: [u8; VHP_WIRE_MAX],
    tx_len: u16,
    tx_pos: u16,
    /// Pending messages (`P_*`).
    pend: u16,
    /// Errors to report: (type byte, code).
    errs: [(u8, u8); ERRQ],
    err_len: u8,
    /// DirList in progress: next index (voice entries, then stations), the
    /// time asked (ages are as of then) and entries sent so far.
    dir: Option<(u8, u32, u8)>,
}

impl Session {
    const fn new() -> Self {
        Session {
            open: false,
            last_ms: 0,
            skip_lf: false,
            rx: [0; VHP_FRAME_MAX],
            rx_len: 0,
            left: 0,
            code: 0,
            zero: false,
            bad: false,
            hold: None,
            tx: [0; VHP_WIRE_MAX],
            tx_len: 0,
            tx_pos: 0,
            pend: 0,
            errs: [(0, 0); ERRQ],
            err_len: 0,
            dir: None,
        }
    }

    /// Back to closed and empty, in place; the receive buffer is wiped (it
    /// may have held an AT line with a password).
    fn reset(&mut self) {
        self.open = false;
        self.last_ms = 0;
        self.skip_lf = false;
        self.rx.fill(0);
        self.rx_len = 0;
        self.left = 0;
        self.code = 0;
        self.zero = false;
        self.bad = false;
        self.hold = None;
        self.tx_len = 0;
        self.tx_pos = 0;
        self.pend = 0;
        self.err_len = 0;
        self.dir = None;
    }

    fn put(&mut self, b: u8) {
        match self.rx.get_mut(usize::from(self.rx_len)) {
            Some(d) => {
                *d = b;
                self.rx_len += 1;
            }
            None => self.bad = true,
        }
    }

    /// One byte from the app. Some(length) when a whole good frame is in
    /// `rx[..length]`.
    fn feed(&mut self, b: u8) -> Option<usize> {
        if b == 0 {
            let n = usize::from(self.rx_len);
            let ok = !self.bad && self.left == 0 && n > 0;
            self.rx_len = 0;
            self.left = 0;
            self.code = 0;
            self.zero = false;
            self.bad = false;
            return ok.then_some(n);
        }
        if self.bad {
            return None;
        }
        if self.left == 0 {
            if self.zero {
                self.put(0);
            }
            self.code = b;
            self.left = b - 1;
            self.zero = false;
        } else {
            self.put(b);
            self.left -= 1;
        }
        if self.left == 0 {
            self.zero = self.code < 0xFF;
        }
        None
    }

    fn err(&mut self, t: u8, code: u8) {
        if let Some(e) = self.errs.get_mut(usize::from(self.err_len)) {
            *e = (t, code);
            self.err_len += 1;
        }
    }

    fn pop_err(&mut self) -> Option<(u8, u8)> {
        if self.err_len == 0 {
            return None;
        }
        let e = self.errs.first().copied();
        self.errs.rotate_left(1);
        self.err_len -= 1;
        e
    }
}

/// The text an AtLine printed, waiting to go out as AtReply frames.
pub(crate) struct Capture {
    /// The session whose AtLine runs (None: no AtLine).
    pub(crate) owner: Option<u8>,
    /// What the modem prints goes here now (set only around the AtLine's
    /// own output).
    pub(crate) on: bool,
    /// The final result code is in: the last of `buf` is the last reply.
    pub(crate) fin: bool,
    /// The AtLine's AT+DIR? listing or AT+SCAN still runs.
    pub(crate) dir: bool,
    pub(crate) scan: bool,
    buf: [u8; CAP_BUF],
    len: u16,
}

impl Capture {
    const fn new() -> Self {
        Capture {
            owner: None,
            on: false,
            fin: false,
            dir: false,
            scan: false,
            buf: [0; CAP_BUF],
            len: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.owner = None;
        self.on = false;
        self.fin = false;
        self.dir = false;
        self.scan = false;
        self.len = 0;
    }

    pub(crate) fn start(&mut self, session: u8) {
        self.reset();
        self.owner = Some(session);
    }

    pub(crate) fn free(&self) -> usize {
        CAP_BUF.saturating_sub(usize::from(self.len))
    }

    /// Add printed text, all or nothing (a line that does not fit is
    /// dropped). Nothing after the final result code.
    pub(crate) fn text(&mut self, t: &[u8]) {
        if self.fin || t.len() > self.free() {
            return;
        }
        let at = usize::from(self.len);
        if let Some(d) = self.buf.get_mut(at..at + t.len()) {
            d.copy_from_slice(t);
            self.len += t.len() as u16;
        }
    }

    /// The final result code was printed: nothing more follows.
    pub(crate) fn finish(&mut self) {
        self.fin = true;
        self.dir = false;
        self.scan = false;
    }
}

/// The parts of the call state whose change sends a CallState.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct CallKey {
    state: u8,
    flags: u8,
    sf: u8,
    codec: u8,
    mode: u8,
    frame_ms: u16,
    max_sealed: u8,
}

impl CallKey {
    const IDLE: CallKey = CallKey {
        state: call_state::IDLE,
        flags: 0,
        sf: 0,
        codec: 0,
        mode: 0,
        frame_ms: 0,
        max_sealed: 0,
    };
}

/// Voice host protocol state, owned by the `Modem`.
pub(crate) struct Vhp {
    pub(crate) s: [Session; VHP_SESSIONS],
    /// Session 0 is the host stream (after `AT+VOICE`).
    pub(crate) host: bool,
    /// The session that owns the voice call (dialed or answered it).
    pub(crate) owner: Option<u8>,
    /// The call id of the ring the sessions in `ring_mask` were told of.
    ring_call: u8,
    ring_mask: u8,
    /// What those sessions hear when that ring stops (0: RingEnded).
    ring_end: u16,
    /// What the owner hears when its call goes (0: Ended).
    call_end: u16,
    last_key: Option<CallKey>,
    state_at: u32,
    floor: Floor,
    pub(crate) cap: Capture,
}

impl Vhp {
    pub(crate) const fn new() -> Self {
        Vhp {
            s: [
                Session::new(),
                Session::new(),
                Session::new(),
                Session::new(),
            ],
            host: false,
            owner: None,
            ring_call: 0,
            ring_mask: 0,
            ring_end: 0,
            call_end: 0,
            last_key: None,
            state_at: 0,
            floor: Floor::Free,
            cap: Capture::new(),
        }
    }

    /// Back to `new` in place.
    pub(crate) fn reset(&mut self) {
        for s in self.s.iter_mut() {
            s.reset();
        }
        self.host = false;
        self.owner = None;
        self.ring_call = 0;
        self.ring_mask = 0;
        self.ring_end = 0;
        self.call_end = 0;
        self.last_key = None;
        self.state_at = 0;
        self.floor = Floor::Free;
        self.cap.reset();
    }

    /// Set `bit` on every open session in `mask`.
    fn tell(&mut self, mask: u8, bit: u16) {
        for (i, s) in self.s.iter_mut().enumerate() {
            if mask & (1 << i) != 0 && s.open {
                s.pend |= bit;
            }
        }
    }

    /// Session `s` dialed or answered: it owns the call. Other sessions
    /// rung for it hear AnsweredElsewhere.
    pub(crate) fn own(&mut self, s: u8) {
        self.owner = Some(s);
        self.call_end = 0;
        self.last_key = None;
        self.floor = Floor::Free;
        let others = self.ring_mask & !(1u8 << (s & 7));
        self.tell(others, P_ANS_ELSE);
        self.ring_mask = 0;
        self.ring_end = 0;
    }

    fn err(&mut self, i: usize, t: u8, code: u8) {
        if let Some(s) = self.s.get_mut(i) {
            s.err(t, code);
        }
    }

    fn pend(&mut self, i: usize, bit: u16) {
        if let Some(s) = self.s.get_mut(i) {
            s.pend |= bit;
        }
    }
}

/// The voice host protocol on the `Modem`.
impl Modem {
    /// Open session `session` (1 to 3; session 0 is the host stream and
    /// opens with `AT+VOICE`). False when out of range or already open.
    pub fn voice_open(&mut self, session: u8, now_ms: u32) -> bool {
        if session == 0 {
            return false;
        }
        let Some(s) = self.vhp.s.get_mut(usize::from(session)) else {
            return false;
        };
        if s.open {
            return false;
        }
        s.reset();
        s.open = true;
        s.last_ms = now_ms;
        true
    }

    /// Bytes from the app on `session`. Returns the bytes taken; offer the
    /// rest again later (a ControlOut the call cannot take yet holds the
    /// session's input). Bytes for a closed session are taken and dropped.
    pub fn voice_input<R: Radio>(
        &mut self,
        session: u8,
        bytes: &[u8],
        radio: &mut R,
        now_ms: u32,
    ) -> usize {
        let n = self.vhp_input(session, bytes, radio, now_ms);
        self.after(radio, now_ms);
        n
    }

    /// Bytes for the app on `session` (whole frames, each ending with its
    /// zero byte, possibly split across calls). Returns bytes written.
    pub fn voice_output(&mut self, session: u8, out: &mut [u8]) -> usize {
        let i = usize::from(session);
        if !self.vhp.s.get(i).is_some_and(|s| s.open) {
            return 0;
        }
        let mut n = 0;
        // Enough rounds for a long burst; the rest waits for the next call.
        for _ in 0..64 {
            let Some(s) = self.vhp.s.get_mut(i) else {
                break;
            };
            if s.tx_pos < s.tx_len {
                let (a, b) = (usize::from(s.tx_pos), usize::from(s.tx_len));
                let k = (b - a).min(out.len().saturating_sub(n));
                if let (Some(d), Some(src)) = (out.get_mut(n..n + k), s.tx.get(a..a + k)) {
                    d.copy_from_slice(src);
                }
                s.tx_pos += k as u16;
                n += k;
                if n >= out.len() {
                    break;
                }
                continue;
            }
            if !self.vhp_next(i) {
                break;
            }
        }
        n
    }

    /// Close `session` (the platform's socket closed). A call it owns is
    /// hung up; its registration lives on for `VOICE_GRACE_MS`. Closing
    /// session 0 ends the host stream's framed mode.
    pub fn voice_close(&mut self, session: u8, now_ms: u32) {
        self.vhp_close(session, false, now_ms);
    }

    /// `AT+VOICE` was answered OK: the host stream is session 0.
    pub(crate) fn vhp_host_open(&mut self, now: u32) {
        if let Some(s) = self.vhp.s.first_mut() {
            s.reset();
            s.open = true;
            s.last_ms = now;
            s.skip_lf = true;
        }
        self.vhp.host = true;
    }

    /// `voice_input` without the link pump (the host stream's
    /// `host_input` pumps once at its end). Stops after a frame that closed
    /// the session (Leave). While a ControlOut is held, whole frames are
    /// still taken and acted on (Hangup, Decline, Leave, TalkKey, Heartbeat
    /// and the rest), except a ControlOut or VoiceOut, which waits behind
    /// the held bytes, with everything after it.
    pub(crate) fn vhp_input<R: Radio>(
        &mut self,
        session: u8,
        bytes: &[u8],
        radio: &mut R,
        now: u32,
    ) -> usize {
        let i = usize::from(session);
        if !self.vhp.s.get(i).is_some_and(|s| s.open) {
            return bytes.len();
        }
        let mut used = 0;
        'outer: while self.vhp.s.get(i).is_some_and(|s| s.open) {
            let rest = bytes.get(used..).unwrap_or(&[]);
            if !self.vhp_flush_hold(i, now) {
                match self.vhp_held_frame(i, rest, radio, now) {
                    Some(k) => {
                        used += k;
                        continue;
                    }
                    None => break,
                }
            }
            if rest.is_empty() {
                break;
            }
            for &b in rest {
                used += 1;
                let Some(s) = self.vhp.s.get_mut(i) else {
                    break 'outer;
                };
                if core::mem::take(&mut s.skip_lf) && b == b'\n' {
                    continue;
                }
                let Some(n) = s.feed(b) else {
                    continue;
                };
                let mut f = [0u8; VHP_FRAME_MAX];
                if let (Some(d), Some(src)) = (f.get_mut(..n), s.rx.get_mut(..n)) {
                    d.copy_from_slice(src);
                    src.fill(0);
                }
                self.vhp_frame(i, f.get(..n).unwrap_or(&[]), radio, now);
                f.fill(0);
                if !self
                    .vhp
                    .s
                    .get(i)
                    .is_some_and(|s| s.open && s.hold.is_none())
                {
                    // Closed (Leave), or a ControlOut is now held.
                    continue 'outer;
                }
            }
            break;
        }
        used
    }

    /// One step of input while a ControlOut is held on session `i`: zero
    /// bytes between frames, or one whole frame from `rest`, decoded from
    /// the slice (the session's decoder buffer holds the held bytes). The
    /// bytes taken, or None to take nothing more now: no whole frame yet,
    /// or a ControlOut or VoiceOut, which waits. Offered bytes keep the
    /// session alive.
    fn vhp_held_frame<R: Radio>(
        &mut self,
        i: usize,
        rest: &[u8],
        radio: &mut R,
        now: u32,
    ) -> Option<usize> {
        if rest.is_empty() {
            return None;
        }
        self.voice_session_seen(i as u8, now);
        if let Some(s) = self.vhp.s.get_mut(i) {
            s.last_ms = now;
            s.skip_lf = false;
        }
        let zeros = rest.iter().take_while(|&&b| b == 0).count();
        if zeros > 0 {
            return Some(zeros);
        }
        let z = rest.iter().position(|&b| b == 0)?;
        let mut f = [0u8; VHP_FRAME_MAX];
        let n = cobs_decode(rest.get(..z).unwrap_or(&[]), &mut f).unwrap_or(0);
        if n > 0 {
            if matches!(f.first(), Some(&(msg::CONTROL_OUT | msg::VOICE_OUT))) {
                f.fill(0);
                return None;
            }
            self.vhp_frame(i, f.get(..n).unwrap_or(&[]), radio, now);
            f.fill(0);
        }
        Some(z + 1)
    }

    /// Push held ControlOut bytes. True when nothing is held any more
    /// (the session's heartbeat clock restarts then: it never runs out
    /// while the session only waits).
    fn vhp_flush_hold(&mut self, i: usize, now: u32) -> bool {
        let owner = self.vhp.owner == Some(i as u8);
        let up = owner && self.link.is_connected() && self.link.voice().is_some();
        let Some(s) = self.vhp.s.get_mut(i) else {
            return true;
        };
        let Some((a, b)) = s.hold else {
            return true;
        };
        if !up {
            // The call went: its control bytes go with it.
            s.hold = None;
            s.rx.fill(0);
            s.last_ms = now;
            return true;
        }
        let k = self
            .link
            .push_data(s.rx.get(usize::from(a)..usize::from(b)).unwrap_or(&[]));
        let a = a.saturating_add(k as u16);
        if a >= b {
            s.hold = None;
            s.rx.fill(0);
            s.last_ms = now;
            true
        } else {
            s.hold = Some((a, b));
            false
        }
    }

    /// One whole frame from session `i`.
    fn vhp_frame<R: Radio>(&mut self, i: usize, f: &[u8], radio: &mut R, now: u32) {
        let sid = i as u8;
        self.voice_session_seen(sid, now);
        if let Some(s) = self.vhp.s.get_mut(i) {
            s.last_ms = now;
        }
        let Some((&t, p)) = f.split_first() else {
            return;
        };
        if let Err(code) = self.vhp_msg(i, t, p, radio, now) {
            self.vhp.err(i, t, code);
        }
    }

    /// Act on one message. Err(code) is answered with Error.
    fn vhp_msg<R: Radio>(
        &mut self,
        i: usize,
        t: u8,
        p: &[u8],
        radio: &mut R,
        now: u32,
    ) -> Result<(), u8> {
        let sid = i as u8;
        let owner = self.vhp.owner == Some(sid);
        let call = owner && self.link.voice().is_some();
        let up = call && self.link.is_connected();
        let empty = |p: &[u8]| {
            if p.is_empty() {
                Ok(())
            } else {
                Err(err::BAD_PAYLOAD)
            }
        };
        match t {
            msg::HELLO => {
                let [_version] = p else {
                    return Err(err::BAD_PAYLOAD);
                };
                self.vhp.pend(i, P_HELLO);
            }
            msg::REGISTER => {
                let (name, label) = two_strs(p).ok_or(err::BAD_PAYLOAD)?;
                if !self.voice_register(sid, name, label, now) {
                    return Err(err::REFUSED);
                }
            }
            msg::UNREGISTER => {
                empty(p)?;
                self.voice_unregister(sid, now);
            }
            msg::CALL => {
                let (name, label) = two_strs(p).ok_or(err::BAD_PAYLOAD)?;
                let label = (!label.is_empty()).then_some(label);
                // Ownership is set by `voice_dial`.
                if !self.voice_dial(sid, name, label, now) {
                    return Err(err::REFUSED);
                }
            }
            msg::ANSWER => {
                empty(p)?;
                if self.vhp.ring_mask & (1 << i) == 0 {
                    return Err(err::NO_CALL);
                }
                let Some(Some(r)) = self.voice.regs.get(i).copied() else {
                    return Err(err::REFUSED);
                };
                // The Answer frame names this device; `voice_answer` makes
                // this session the owner.
                self.link.set_voice_answerer(sid, r.label);
                if !self.voice_answer(now) {
                    return Err(err::REFUSED);
                }
            }
            msg::DECLINE => {
                empty(p)?;
                let ringing = self.link.state() == State::Ringing && self.link.voice().is_some();
                if self.vhp.ring_mask & (1 << i) == 0 || !ringing {
                    return Err(err::NO_CALL);
                }
                self.voice_decline(now);
                let others = self.vhp.ring_mask & !(1u8 << i);
                self.vhp.tell(others, P_DECLINED);
                self.vhp.ring_mask = 0;
                self.vhp.ring_end = 0;
            }
            msg::HANGUP => {
                empty(p)?;
                if !call {
                    return Err(err::NO_CALL);
                }
                // The end (Ended) is seen by `vhp_poll`.
                self.link.hangup(now);
            }
            msg::VOICE_OUT => {
                // Lossy by design: anything that cannot go now is dropped,
                // silently (no Error per voice frame).
                let duplex = self
                    .link
                    .talk_plan()
                    .is_some_and(|pl| pl.mode == TalkMode::Duplex);
                if up && !p.is_empty() && (duplex || self.link.floor() == Floor::Mine) {
                    self.link.voice_push(p);
                }
            }
            msg::CONTROL_OUT => {
                if p.is_empty() {
                    return Err(err::BAD_PAYLOAD);
                }
                if !up {
                    return Err(err::NO_CALL);
                }
                let k = self.link.push_data(p);
                if k < p.len() {
                    // Hold the rest; no more input from this session until
                    // the link takes it.
                    let rest = p.get(k..).unwrap_or(&[]);
                    if let Some(s) = self.vhp.s.get_mut(i) {
                        if let Some(d) = s.rx.get_mut(..rest.len()) {
                            d.copy_from_slice(rest);
                            s.hold = Some((0, rest.len() as u16));
                        }
                    }
                }
            }
            msg::TALK_KEY => {
                let &[d] = p else {
                    return Err(err::BAD_PAYLOAD);
                };
                if d > 1 {
                    return Err(err::BAD_PAYLOAD);
                }
                if !call {
                    return Err(err::NO_CALL);
                }
                self.link.set_talk_key(d == 1);
            }
            msg::HEARTBEAT => empty(p)?,
            msg::LEAVE => {
                empty(p)?;
                self.vhp_close(sid, true, now);
            }
            msg::DIR_LIST => {
                empty(p)?;
                if let Some(s) = self.vhp.s.get_mut(i) {
                    s.dir = Some((0, now, 0));
                }
            }
            msg::VOICE_READY => {
                empty(p)?;
                if !up {
                    return Err(err::NO_CALL);
                }
                self.link.set_voice_ready();
            }
            msg::AT_LINE => {
                if self.vhp.cap.owner.is_some() {
                    return Err(err::BUSY);
                }
                self.vhp_at_line(sid, p, radio, now)?;
            }
            _ => return Err(err::UNKNOWN),
        }
        Ok(())
    }

    /// Close a session: a call it owns is hung up, its AtLine (and that
    /// line's listing or scan) is dropped. `leave`: the app said Leave, so
    /// its registration goes at once too.
    pub(crate) fn vhp_close(&mut self, session: u8, leave: bool, now: u32) {
        let i = usize::from(session);
        if !self.vhp.s.get(i).is_some_and(|s| s.open) {
            return;
        }
        let mut had_call = false;
        if self.vhp.owner == Some(session) {
            had_call = self.link.voice().is_some();
            if had_call {
                self.link.hangup(now);
            }
            self.vhp.owner = None;
        }
        self.vhp.ring_mask &= !(1u8 << (session & 7));
        if self.vhp.cap.owner == Some(session) {
            self.cap_cancel();
        }
        if leave {
            self.voice_unregister(session, now);
        }
        if let Some(s) = self.vhp.s.get_mut(i) {
            s.reset();
        }
        if i == 0 && self.vhp.host {
            // Back to AT commands.
            self.vhp.host = false;
            if had_call {
                self.result(crate::at::ResultCode::NoCarrier);
            }
        }
    }

    /// From `tick`: close sessions that sent nothing for
    /// `VOICE_HEARTBEAT_MS`.
    pub(crate) fn vhp_tick(&mut self, now: u32) {
        for i in 0..VHP_SESSIONS {
            let late = self.vhp.s.get(i).is_some_and(|s| {
                let d = now.wrapping_sub(s.last_ms);
                s.open && s.hold.is_none() && (VOICE_HEARTBEAT_MS..u32::MAX / 2).contains(&d)
            });
            if late {
                self.vhp_close(i as u8, false, now);
            }
        }
    }

    /// A link event (before the Modem's own handling).
    pub(crate) fn vhp_event(&mut self, e: Event) {
        match e {
            Event::VoiceRing(r) => {
                let id = self.link.call_id();
                if self.vhp.ring_mask != 0 && self.vhp.ring_call != id {
                    self.vhp_ring_over();
                }
                self.vhp.ring_call = id;
                let i = usize::from(r);
                if let Some(s) = self.vhp.s.get_mut(i).filter(|s| s.open) {
                    s.pend |= P_RING;
                    self.vhp.ring_mask |= 1 << (r & 7);
                }
            }
            Event::AnsweredElsewhere => {
                if self.vhp.owner.is_some() {
                    self.vhp.call_end = P_ANS_ELSE;
                } else {
                    self.vhp.ring_end = P_ANS_ELSE;
                }
            }
            Event::Declined => {
                if self.vhp.owner.is_some() && self.link.role() == Role::Caller {
                    self.vhp.call_end = P_DECLINED;
                } else {
                    self.vhp.ring_end = P_DECLINED;
                }
            }
            Event::NoAnswer if self.vhp.owner.is_some() => {
                self.vhp.call_end = P_NO_ANSWER;
            }
            _ => {}
        }
    }

    /// The ring the sessions in `ring_mask` were told of has stopped.
    fn vhp_ring_over(&mut self) {
        let bit = if self.vhp.ring_end != 0 {
            self.vhp.ring_end
        } else {
            P_RING_ENDED
        };
        let m = self.vhp.ring_mask;
        self.vhp.tell(m, bit);
        self.vhp.ring_mask = 0;
        self.vhp.ring_end = 0;
    }

    /// After every event batch (`Modem::after`): held control bytes, the end
    /// of a ring or of the owned call (some ends have no event: a hang-up
    /// here, a ring that times out), CallState and Floor changes.
    pub(crate) fn vhp_poll(&mut self, now: u32) {
        for i in 0..VHP_SESSIONS {
            self.vhp_flush_hold(i, now);
        }
        let ringing = (self.link.state() == State::Ringing && self.link.voice().is_some())
            .then(|| self.link.call_id());
        if self.vhp.ring_mask != 0 && ringing != Some(self.vhp.ring_call) {
            self.vhp_ring_over();
        }
        // An end reason nobody here was told of the ring or call for must
        // not stick to the next one.
        if self.vhp.ring_mask == 0 {
            self.vhp.ring_end = 0;
        }
        let Some(o) = self.vhp.owner else {
            self.vhp.call_end = 0;
            return;
        };
        let o = usize::from(o);
        if self.link.voice().is_none() {
            let bit = if self.vhp.call_end != 0 {
                self.vhp.call_end
            } else {
                P_ENDED
            };
            let floor = if self.vhp.floor != Floor::Free {
                P_FLOOR
            } else {
                0
            };
            self.vhp.owner = None;
            self.vhp.call_end = 0;
            self.vhp.floor = Floor::Free;
            self.vhp.last_key = None;
            self.vhp.pend(o, bit | P_STATE | floor);
            return;
        }
        let key = self.call_key();
        let fresh =
            self.link.is_connected() && now.wrapping_sub(self.vhp.state_at) >= STATE_EVERY_MS;
        if self.vhp.last_key != Some(key) || fresh {
            self.vhp.last_key = Some(key);
            self.vhp.state_at = now;
            self.vhp.pend(o, P_STATE);
        }
        let fl = self.link.floor();
        if fl != self.vhp.floor {
            self.vhp.floor = fl;
            self.vhp.pend(o, P_FLOOR);
        }
    }

    fn call_key(&self) -> CallKey {
        if self.link.voice().is_none() {
            return CallKey::IDLE;
        }
        let state = match self.link.state() {
            State::Idle => call_state::IDLE,
            State::Dialing | State::AwaitTrain => call_state::DIALING,
            State::Ringing => call_state::RINGING,
            State::AwaitTierSet | State::AwaitTierAck | State::Probe => call_state::TRAINING,
            State::Connected => call_state::CONNECTED,
            State::Resync => call_state::RESYNC,
        };
        let mut flags = 0;
        if self.link.voice_slots() {
            flags |= 1;
        }
        if self.link.role() == Role::Caller {
            flags |= 2;
        }
        let sf = self.link.tier().map_or(0, |t| t.sf());
        let (codec, mode, frame_ms, max_sealed) = match self.link.talk_plan() {
            Some(pl) => (
                pl.codec as u8,
                match pl.mode {
                    TalkMode::Duplex => 1,
                    TalkMode::Ptt => 2,
                    TalkMode::PttDelayed => 3,
                },
                pl.frame_ms,
                pl.max_sealed_bytes,
            ),
            None => (0, 0, 0, 0),
        };
        CallKey {
            state,
            flags,
            sf,
            codec,
            mode,
            frame_ms,
            max_sealed,
        }
    }

    /// Build the next frame for session `i` into its write buffer. False
    /// when nothing waits.
    fn vhp_next(&mut self, i: usize) -> bool {
        let mut f = [0u8; VHP_FRAME_MAX];
        let n = self.vhp_build(i, &mut f);
        if n == 0 {
            return false;
        }
        let Some(s) = self.vhp.s.get_mut(i) else {
            return false;
        };
        let m = cobs_encode(f.get(..n).unwrap_or(&[]), &mut s.tx).unwrap_or(0);
        s.tx_len = m as u16;
        s.tx_pos = 0;
        true
    }

    /// Take pending bit `bit` of session `i`: true when it was set.
    fn take_pend(&mut self, i: usize, bit: u16) -> bool {
        match self.vhp.s.get_mut(i) {
            Some(s) if s.pend & bit != 0 => {
                s.pend &= !bit;
                true
            }
            _ => false,
        }
    }

    /// The next message for session `i`, decoded, into `f`; its length, 0
    /// when nothing waits.
    fn vhp_build(&mut self, i: usize, f: &mut [u8; VHP_FRAME_MAX]) -> usize {
        let sid = i as u8;
        if self.take_pend(i, P_HELLO) {
            let mut w = W {
                b: f,
                n: 0,
                over: false,
            };
            w.u8(msg::HELLO_ACK)
                .u8(VHP_VERSION)
                .str(self.station_name());
            return w.done();
        }
        if let Some((t, code)) = self.vhp.s.get_mut(i).and_then(Session::pop_err) {
            let mut w = W {
                b: f,
                n: 0,
                over: false,
            };
            w.u8(msg::ERROR).u8(t).u8(code);
            return w.done();
        }
        if self.take_pend(i, P_RING) {
            let ringing =
                self.link.state() == State::Ringing && self.link.call_id() == self.vhp.ring_call;
            if let Some(v) = self.link.voice().filter(|_| ringing) {
                let mut w = W {
                    b: f,
                    n: 0,
                    over: false,
                };
                w.u8(msg::RING)
                    .str(v.peer_station.as_str())
                    .str(v.peer_name.as_str())
                    .str(v.peer_label.as_str());
                return w.done();
            }
        }
        for (bit, t) in ENDS {
            if self.take_pend(i, bit) {
                if let Some(d) = f.first_mut() {
                    *d = t;
                }
                return 1;
            }
        }
        if self.take_pend(i, P_STATE) {
            return self.vhp_call_state(f);
        }
        if self.take_pend(i, P_FLOOR) {
            let fl = match self.vhp.floor {
                Floor::Free => 0,
                Floor::Mine => 1,
                Floor::Theirs => 2,
            };
            let mut w = W {
                b: f,
                n: 0,
                over: false,
            };
            w.u8(msg::FLOOR).u8(fl);
            return w.done();
        }
        if self.vhp.owner == Some(sid) && self.link.voice().is_some() {
            // The call's control stream, as much as one frame holds.
            let k = match f.get_mut(1..1 + VHP_CONTROL_MAX) {
                Some(d) => self.link.read_data(d),
                None => 0,
            };
            if k > 0 {
                if let Some(d) = f.first_mut() {
                    *d = msg::CONTROL_IN;
                }
                return 1 + k;
            }
            let k = match f.get_mut(1..) {
                Some(d) => self.link.voice_pop(d),
                None => 0,
            };
            if k > 0 {
                if let Some(d) = f.first_mut() {
                    *d = msg::VOICE_IN;
                }
                return 1 + k;
            }
        }
        if self.vhp.cap.owner == Some(sid) {
            if self.vhp.cap.dir {
                // More of the listing, as the reply drains.
                self.pump_list();
            }
            let c = &mut self.vhp.cap;
            let len = usize::from(c.len);
            if len > 0 || c.fin {
                let k = len.min(VHP_AT_CHUNK);
                let last = c.fin && k == len;
                let mut w = W {
                    b: f,
                    n: 0,
                    over: false,
                };
                w.u8(msg::AT_REPLY)
                    .u8(u8::from(last))
                    .bytes(c.buf.get(..k).unwrap_or(&[]));
                c.buf.copy_within(k..len, 0);
                c.len = (len - k) as u16;
                if last {
                    c.reset();
                }
                return w.done();
            }
        }
        self.vhp_dir_next(i, f)
    }

    /// CallState for the owner, into `f`.
    fn vhp_call_state(&self, f: &mut [u8; VHP_FRAME_MAX]) -> usize {
        let k = self.call_key();
        let v = self.link.voice().copied();
        let id = self.link.call_id();
        let rx = self
            .call_rx
            .filter(|&(c, _)| c != 0 && c == id && v.is_some())
            .map(|(_, info)| info);
        let none = Name16::EMPTY;
        let (st, nm, lb) = v.map_or((none, none, none), |v| {
            (v.peer_station, v.peer_name, v.peer_label)
        });
        let mut w = W {
            b: f,
            n: 0,
            over: false,
        };
        w.u8(msg::CALL_STATE)
            .u8(k.state)
            .u8(k.flags)
            .u8(k.sf)
            .u8(k.codec)
            .u8(k.mode)
            .u16(k.frame_ms)
            .u8(k.max_sealed)
            .i16(rx.map_or(i16::MIN, |r| r.snr_x4))
            .i16(rx.map_or(i16::MIN, |r| r.rssi_dbm))
            .str(st.as_str())
            .str(nm.as_str())
            .str(lb.as_str());
        w.done()
    }

    /// The next DirEntry (voice devices first, then stations) or the
    /// DirEnd of a DirList in progress on session `i`, into `f`.
    fn vhp_dir_next(&mut self, i: usize, f: &mut [u8; VHP_FRAME_MAX]) -> usize {
        let Some(s) = self.vhp.s.get_mut(i) else {
            return 0;
        };
        let Some((next, at, count)) = s.dir else {
            return 0;
        };
        let mut w = W {
            b: f,
            n: 0,
            over: false,
        };
        let end = (VOICE_DIR_MAX + DIR_MAX) as u8;
        let mut j = next;
        while j < end {
            let k = usize::from(j);
            if let Some(e) = self.dir.voice().slot(k) {
                let heard = e.heard_since_boot();
                let age = if heard {
                    age_s(at, e.heard_ms)
                } else {
                    u32::MAX
                };
                w.u8(msg::DIR_ENTRY)
                    .u8(1)
                    .u8(u8::from(e.pinned) | (u8::from(heard) << 1))
                    .u8(e.best_tier.map_or(0, |t| t.sf()))
                    .u32(age)
                    .i16(i16::MIN)
                    .i16(i16::MIN)
                    .u8(0)
                    .str(e.name.as_str())
                    .str(e.label.as_str())
                    .str(e.station.as_str());
                s.dir = Some((j + 1, at, count.saturating_add(1)));
                return w.done();
            }
            if let Some(e) = k.checked_sub(VOICE_DIR_MAX).and_then(|x| self.dir.slot(x)) {
                let heard = e.heard_since_boot();
                let age = if heard {
                    age_s(at, e.heard_ms)
                } else {
                    u32::MAX
                };
                w.u8(msg::DIR_ENTRY)
                    .u8(0)
                    .u8(u8::from(e.pinned) | (u8::from(heard) << 1))
                    .u8(e.best_tier.map_or(0, |t| t.sf()))
                    .u32(age)
                    .i16(if heard { e.last_snr_x4 } else { i16::MIN })
                    .i16(if heard { e.last_rssi } else { i16::MIN })
                    .u8(e.services)
                    .str(e.name())
                    .str("")
                    .str("");
                s.dir = Some((j + 1, at, count.saturating_add(1)));
                return w.done();
            }
            j += 1;
        }
        s.dir = None;
        w.u8(msg::DIR_END).u8(count);
        w.done()
    }
}

/// Seconds from `heard` to `at`, never below 0 (heard again after the
/// list was asked for).
fn age_s(at: u32, heard: u32) -> u32 {
    let d = at.wrapping_sub(heard);
    if d > u32::MAX / 2 {
        0
    } else {
        d / 1000
    }
}
