//! Call state machine: setup, training, data, retrain, resync, hang-up.
//!
//! No host text here: the Modem turns `Event`s into result codes and lines.
//! All time is u32 milliseconds compared with `now.wrapping_sub(start)`.
//!
//! Air manners (half duplex): every frame carries flag bit 0 (MORE) when the
//! sender sends another frame right after it. A side may only transmit while
//! it holds the turn; a frame without MORE hands the turn to the peer, which
//! takes it back by timeout if the peer stays silent (the answerer waits
//! longer, so idle collisions resolve). Acks are owed for every Data frame
//! and sent at the end of the peer's burst. Hangup ignores the turn.
//! Outbound control frames sit in a 4 entry queue; Data frames are built
//! from ArqTx at send time.
//!
//! Radio contract assumed: `busy()` is true from the start of an inbound
//! frame and during TX; after TX the core calls `listen` again (never while
//! busy) before expecting to hear anything.
//!
//! Compression (see `lz.rs` and `lz_stream`): the caller appends a capability
//! byte to its CallReq when its own S31 is 1. The byte offers `CAP_LZ`,
//! `CAP_BULK`, and `CAP_LZ_STREAM`. The answerer agrees to each bit it also
//! offers, and says so on its CallAck and as a 4th Train byte. The caller
//! reads the answer from the Train (the frame it cannot connect without), so
//! both ends always agree. A peer that does not know a bit ignores it.
//!
//! `CAP_BULK` (bit 1) uses a window of 16, and a turn that already has more
//! than 4 chunks queued sends 8 frames. A short turn stays at 4 frames.
//! Voice calls do not offer the bit: their slot frames carry an 8 bit bitmap.
//!
//! When both ends set `CAP_LZ_STREAM`, host bytes are compressed before they
//! enter ArqTx. A resend is those same bytes. The receiver decodes only the
//! in order stream from ArqRx. A normal Data frame does not carry `FLAG_LZ`.
//! That bit, on this call, asks for a dictionary reset or marks the frame
//! that starts the fresh one. The dictionary is reset on retrain and on
//! resync by a RESET token in that stream, and on link loss by dropping the
//! history (the receiver keeps it until bytes already in ARQ have been
//! delivered).
//!
//! Otherwise, when both ends set `CAP_LZ` and not the stream bit, compression
//! stays between ARQ and the air: ArqTx keeps chunks plain, and a Data frame
//! is compressed when it is built, so a resend compresses the same chunk
//! again to the same bytes. A received `FLAG_LZ` frame is decompressed before
//! ArqRx sees it; one that fails to decompress is dropped unacked.
//!
//! Voice calls (see `voice::call`): set up with type 15 sub-types (voice
//! CallReq, Answer, Decline) handed in by the Modem, which owns the
//! registrations; then today's Train, TierSet and TierAck, the voice Train
//! and setup TierSet carrying the answer token. Once connected the call
//! runs on data timing until both apps are ready for voice, then on the
//! voice scheduler's fixed slots (see `voice::sched`; wiring in `data.rs`).

use crate::arq::{ArqRx, ArqTx, CHUNK};
use crate::at::{SRegs, REG_LZ};
use crate::frame::{self, FrameType, Header, HEADER_LEN, MAX_FRAME};
use crate::lz_stream::{StreamDec, StreamEnc};
use crate::radio::{Radio, RxInfo};
use crate::tier::{best_tier, Tier};
use crate::voice::call::{self as vcall, VoiceCallInfo, VoiceLink};
use crate::voice::host::AirNote;
use crate::voice::sched::VoiceSched;

mod data;
mod retrain;
mod setup;

/// Header flag: another frame follows at once.
pub const FLAG_MORE: u8 = 1;
/// Header flag on Keepalive: please reply.
pub const FLAG_PING: u8 = 2;

const PERIOD_MS: u32 = 3000; // CallReq resend and RING cadence
/// Frames per turn on a classic call, and on a short turn of a bulk call.
/// A full burst of 4 Data frames lasts 4 airtimes, longer than the 3 airtime
/// part of the RTO, so the first frame of a full burst can be resent once
/// before its ack lands; staging prefers unsent frames, which keeps that
/// to the tail of a transfer.
const BURST_MAX: u8 = 4;
/// Frames per turn when `CAP_BULK` is agreed and more than `BURST_MAX`
/// chunks are already outstanding at the start of the turn. The turn gap
/// and the answerer's 100 ms yield stay as they are: a station that hears
/// the peer takes the turn at once, so those waits are only the silence
/// timeout. They cover a 10 ms tick and a radio that reports busy after it
/// has seen a preamble. Shrinking them is unmeasured on hardware.
const BURST_BULK: u8 = 8;
const TURN_YIELD_MS: u32 = 100;
const UP_HOLD_MS: u32 = 10_000;
const KA_LEN: usize = HEADER_LEN + 2;
/// Training probes per tier before stepping one tier slower.
const PROBE_TRIES: u8 = 2;
/// Most jitter added to a training probe retry.
const PROBE_JITTER_MS: u32 = 50;
const QLEN: usize = 4;
/// Queue slot bytes: the longest voice discovery frame (a voice scan
/// answer with four registrations) must fit (`send_discovery`).
const QBUF: usize = 132;
const _: () = assert!(QBUF >= crate::voice::VOICE_DISC_FRAME);
// The longest voice call request: header, sub-type, capability byte and
// five names of up to 16 bytes with their length bytes.
const _: () = assert!(QBUF >= HEADER_LEN + 2 + 5 * 17);
const EVLEN: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Ring,
    /// Caller sent a CallReq (verbose RINGING).
    Ringing,
    /// Caller computed the training SNR (dB x4).
    Training(i16),
    Connect(Tier),
    Retrain(Tier),
    NoCarrier,
    Busy,
    NoAnswer,
    /// A voice call rings this registration (repeated like RING).
    VoiceRing(u8),
    /// The voice call ringing or answered here was taken by another device.
    AnsweredElsewhere,
    /// The voice call was declined (on the caller), or declined on another
    /// device (on a device still ringing).
    Declined,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Caller,
    Answerer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle,
    /// Caller: CallReq every 3 s, waiting for CallAck or Train.
    Dialing,
    /// Caller: got CallAck, waiting for Train.
    AwaitTrain,
    /// Answerer: printing RING, waiting for ATA or S0.
    Ringing,
    /// Answerer: CallAck and Train sent, waiting for TierSet on SF12.
    AwaitTierSet,
    /// Caller: TierSet sent on SF12, waiting for TierAck.
    AwaitTierAck,
    /// Both: on the trained tier, Keepalive probe.
    Probe,
    Connected,
    /// Link lost: both on SF12, caller pings.
    Resync,
}

#[derive(Clone, Copy)]
struct QFrame {
    tier: Tier,
    len: u8,
    end_turn: bool,
    urgent: bool,
    buf: [u8; QBUF],
}

const EMPTY_Q: QFrame = QFrame {
    tier: Tier::ROBUST,
    len: 0,
    end_turn: false,
    urgent: false,
    buf: [0; QBUF],
};

fn since(now: u32, t: u32) -> u32 {
    now.wrapping_sub(t)
}

/// One xorshift32 step (13, 17, 5). A nonzero state never reaches zero.
/// Shared by the link's retry jitter and the discovery scheduler.
pub(crate) fn xorshift32(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

pub(crate) fn air_ms(t: Tier, len: usize) -> u32 {
    (t.airtime_us(len) / 1000).saturating_add(1)
}

/// RTO = 3 x airtime(full data frame) + 100 ms.
pub fn rto_ms(t: Tier) -> u32 {
    air_ms(t, HEADER_LEN + CHUNK)
        .saturating_mul(3)
        .saturating_add(100)
}

/// Preamble time in microseconds (12.25 symbols; 18.25 at SF5 and SF6).
fn preamble_us(t: Tier) -> u32 {
    let tsym = (1u32 << t.sf()) * 2;
    let quarters = if t.sf() <= 6 { 73 } else { 49 };
    tsym * quarters / 4
}

/// Silence before taking the turn back: two preambles at this tier plus
/// 20 ms, at least 40 ms. A real radio only reports busy once it has
/// detected the peer's preamble.
pub(crate) fn turn_gap_ms(t: Tier) -> u32 {
    (preamble_us(t) * 2 / 1000 + 20).max(40)
}

/// Caller's wait for a probe reply: 3 airtimes of a Keepalive plus 200 ms.
fn caller_probe_ms(t: Tier) -> u32 {
    air_ms(t, KA_LEN).saturating_mul(3).saturating_add(200)
}

fn is_tierset(f: &QFrame) -> bool {
    f.buf
        .get(1)
        .is_some_and(|b| b >> 4 == FrameType::TierSet as u8)
}

fn is_ping(f: &QFrame) -> bool {
    f.buf
        .get(1)
        .is_some_and(|b| b >> 4 == FrameType::Keepalive as u8 && b & FLAG_PING != 0)
}

fn reg(s: &SRegs, n: u8, d: u16) -> u32 {
    u32::from(s.get(n).unwrap_or(d))
}

fn cap_tier(s: &SRegs) -> Tier {
    Tier::from_index(reg(s, 21, 7).min(7) as u8).unwrap_or(Tier::Sf5)
}

/// S31 = 1: offer compression on this call.
fn lz_wanted(s: &SRegs) -> bool {
    reg(s, REG_LZ, 1) == 1
}

/// Capability bits this station offers on a data call.
///
/// `CAP_BULK` and `CAP_LZ_STREAM` ride in the same byte as `CAP_LZ`. The
/// byte is sent only when it is non-zero, and it is non-zero only when S31
/// is 1, so `ATS31=0` still sends the pre-capability CallReq (no extra byte)
/// that builds older than r.420 require. Each bit is agreed on its own.
/// Voice calls do not use this function.
fn offered_caps(s: &SRegs) -> u8 {
    if !lz_wanted(s) {
        return 0;
    }
    frame::CAP_LZ | frame::CAP_BULK | frame::CAP_LZ_STREAM
}

pub struct Link {
    state: State,
    role: Role,
    call_id: u8,
    tier: Tier,
    /// Highest tier this call may use: own S21, lowered by what the peer allows.
    cap: Tier,
    /// Own S21 cap for this call.
    own_cap: Tier,
    /// Tier being trained (AwaitTierAck, Probe).
    train: Tier,
    start: u32,
    last_periodic: u32,
    probe_ms: u32,
    /// Caller: training probes sent on this tier.
    probe_tries: u8,
    rings: u8,
    last_req: u32,
    got_tierset: bool,
    my_snr: i16,
    callreq: [u8; QBUF],
    callreq_len: u8,
    // Connected
    last_rx: u32,
    last_tx: u32,
    last_change: u32,
    ewma_x32: i32,
    good_since: Option<u32>,
    /// Since when the average has been under the step-down line.
    low_since: Option<u32>,
    /// Keepalive PINGs sent since we last heard the peer.
    ping_out: u8,
    ping_last: u32,
    ping_wait: u32,
    /// xorshift32 state for retry jitter, seeded from call id and role.
    rng: u32,
    pending: Option<Tier>,
    pend_last: u32,
    pend_tries: u8,
    pend_wait: u32,
    silence_done: bool,
    turn: bool,
    turn_start: u32,
    turn_ms: u32,
    burst: u8,
    /// Frames allowed in the turn that is underway (`BURST_MAX` or `BURST_BULK`).
    burst_cap: u8,
    ack_owed: bool,
    ka_reply: bool,
    last_snr: i16,
    resync_prev: Tier,
    staged: Option<(u8, u8)>,
    staged_buf: [u8; CHUNK],
    data_tx: Option<u32>,
    data_rx: Option<u32>,
    need_listen: bool,
    listening: Option<Tier>,
    tx: ArqTx,
    rx: ArqRx,
    q: [QFrame; QLEN],
    q_head: usize,
    q_len: usize,
    ev: [Event; EVLEN],
    ev_head: usize,
    ev_len: usize,
    /// The modem is busy with a Wi-Fi call: an incoming CallReq is
    /// rejected (the caller sees BUSY) even while the link is idle.
    engaged: bool,
    /// A Hangup asked for while the voice slots run: when (ms). The two
    /// Hangup frames wait in the queue for our own sub-slots (the far side
    /// may be sending: a push to talk holder fills its slots), then the link
    /// goes idle (`tick`).
    slot_hangup: Option<u32>,
    /// We offered compression on this call (own S31 at dial or answer).
    lz_mine: bool,
    /// The peer offered per frame LZ (`CAP_LZ`), or the Train agreed it.
    lz_peer: bool,
    /// The peer offered stream LZ (`CAP_LZ_STREAM`).
    stream_peer: bool,
    /// Both offered some compression: `/ARQ/LZ` and the DC light.
    /// Kept after the call ends, for AT&V.
    lz: bool,
    /// Stream LZ is in force: compress above ARQ, not per frame.
    lz_stream: bool,
    /// The call has ended. The decoder is cleared once ARQ has no more
    /// in order bytes to deliver.
    stream_dead: bool,
    /// Plaintext and compressed bytes already attributed to sent frames
    /// (stream mode ratio, so a resend can be counted again).
    cred_raw: u32,
    cred_comp: u32,
    enc: StreamEnc,
    dec: StreamDec,
    /// Decoder failed: drop compressed bytes until the peer's sync frame.
    lz_sync_wait: bool,
    /// Sequence of the Data frame that starts the fresh dictionary.
    lz_sync_at: Option<u8>,
    /// Sequence of the chunk we send after the peer asks us to reset.
    lz_sync_tx: Option<u8>,
    /// When the last sync request went out, and how many we have sent.
    lz_sync_ask_at: u32,
    lz_sync_asks: u8,
    /// We offered `CAP_BULK` on this data call.
    bulk_mine: bool,
    /// The peer offered `CAP_BULK`, or the Train's agreed bit (caller).
    bulk_peer: bool,
    /// Both offered, and this is not a voice call: window 16, long bursts
    /// only while more than 4 chunks are outstanding.
    bulk: bool,
    /// Data payload bytes of this (or the last) call, both directions:
    /// host bytes before compression and bytes on the air.
    lz_raw: u32,
    lz_air: u32,
    /// Call data bytes the host side moved: taken by `push_data` plus
    /// handed out by `read_data`. Wraps; never reset (the menu's throughput).
    moved: u32,
    /// Voice call state (see `voice::call`).
    voice: VoiceLink,
    /// Voice slots, floor and voice buffers (see `voice::sched`).
    vs: VoiceSched,
    /// Frames sent since the modem last took them (its AirEvents); more
    /// than fit are not noted (one send per pump, the modem takes them
    /// after every pump).
    air_tx: [AirNote; 2],
    air_tx_len: u8,
}

impl Default for Link {
    fn default() -> Self {
        Self::new()
    }
}

impl Link {
    pub const fn new() -> Self {
        Link {
            state: State::Idle,
            role: Role::Caller,
            call_id: 0,
            tier: Tier::ROBUST,
            cap: Tier::Sf5,
            own_cap: Tier::Sf5,
            train: Tier::ROBUST,
            start: 0,
            last_periodic: 0,
            probe_ms: 0,
            probe_tries: 0,
            rings: 0,
            last_req: 0,
            got_tierset: false,
            my_snr: 0,
            callreq: [0; QBUF],
            callreq_len: 0,
            last_rx: 0,
            last_tx: 0,
            last_change: 0,
            ewma_x32: 0,
            good_since: None,
            low_since: None,
            ping_out: 0,
            ping_last: 0,
            ping_wait: 0,
            rng: 1,
            pending: None,
            pend_last: 0,
            pend_tries: 0,
            pend_wait: 0,
            silence_done: false,
            turn: true,
            turn_start: 0,
            turn_ms: 0,
            burst: 0,
            burst_cap: BURST_MAX,
            ack_owed: false,
            ka_reply: false,
            last_snr: 0,
            resync_prev: Tier::ROBUST,
            staged: None,
            staged_buf: [0; CHUNK],
            data_tx: None,
            data_rx: None,
            need_listen: true,
            listening: None,
            tx: ArqTx::new(),
            rx: ArqRx::new(),
            q: [EMPTY_Q; QLEN],
            q_head: 0,
            q_len: 0,
            ev: [Event::Ring; EVLEN],
            ev_head: 0,
            ev_len: 0,
            engaged: false,
            slot_hangup: None,
            lz_mine: false,
            lz_peer: false,
            stream_peer: false,
            lz: false,
            lz_stream: false,
            stream_dead: false,
            cred_raw: 0,
            cred_comp: 0,
            enc: StreamEnc::new(),
            dec: StreamDec::new(),
            lz_sync_wait: false,
            lz_sync_at: None,
            lz_sync_tx: None,
            lz_sync_ask_at: 0,
            lz_sync_asks: 0,
            bulk_mine: false,
            bulk_peer: false,
            bulk: false,
            lz_raw: 0,
            lz_air: 0,
            moved: 0,
            voice: VoiceLink::new(),
            vs: VoiceSched::new(),
            air_tx: [AirNote::EMPTY; 2],
            air_tx_len: 0,
        }
    }

    /// Back to `new`, in place. The stream dictionaries are reseeded without
    /// building a second `Link` on the stack (that copy is the modem task's
    /// largest temporary, and the dictionaries are several KB).
    pub(crate) fn reinit(&mut self) {
        self.state = State::Idle;
        self.role = Role::Caller;
        self.call_id = 0;
        self.tier = Tier::ROBUST;
        self.cap = Tier::Sf5;
        self.own_cap = Tier::Sf5;
        self.train = Tier::ROBUST;
        self.start = 0;
        self.last_periodic = 0;
        self.probe_ms = 0;
        self.probe_tries = 0;
        self.rings = 0;
        self.last_req = 0;
        self.got_tierset = false;
        self.my_snr = 0;
        self.callreq = [0; QBUF];
        self.callreq_len = 0;
        self.last_rx = 0;
        self.last_tx = 0;
        self.last_change = 0;
        self.ewma_x32 = 0;
        self.good_since = None;
        self.low_since = None;
        self.ping_out = 0;
        self.ping_last = 0;
        self.ping_wait = 0;
        self.rng = 1;
        self.pending = None;
        self.pend_last = 0;
        self.pend_tries = 0;
        self.pend_wait = 0;
        self.silence_done = false;
        self.turn = true;
        self.turn_start = 0;
        self.turn_ms = 0;
        self.burst = 0;
        self.burst_cap = BURST_MAX;
        self.ack_owed = false;
        self.ka_reply = false;
        self.last_snr = 0;
        self.resync_prev = Tier::ROBUST;
        self.staged = None;
        self.staged_buf = [0; CHUNK];
        self.data_tx = None;
        self.data_rx = None;
        self.need_listen = true;
        self.listening = None;
        self.tx.reset();
        self.rx.reset();
        self.q = [EMPTY_Q; QLEN];
        self.q_head = 0;
        self.q_len = 0;
        self.ev = [Event::Ring; EVLEN];
        self.ev_head = 0;
        self.ev_len = 0;
        self.engaged = false;
        self.slot_hangup = None;
        self.lz_mine = false;
        self.lz_peer = false;
        self.stream_peer = false;
        self.lz = false;
        self.lz_stream = false;
        self.stream_dead = false;
        self.cred_raw = 0;
        self.cred_comp = 0;
        self.enc.reset_call();
        self.dec.reset_call();
        self.lz_sync_wait = false;
        self.lz_sync_at = None;
        self.lz_sync_tx = None;
        self.lz_sync_ask_at = 0;
        self.lz_sync_asks = 0;
        self.bulk_mine = false;
        self.bulk_peer = false;
        self.bulk = false;
        self.lz_raw = 0;
        self.lz_air = 0;
        self.moved = 0;
        self.voice = VoiceLink::new();
        self.vs.reset();
        self.air_tx = [AirNote::EMPTY; 2];
        self.air_tx_len = 0;
    }

    /// A frame just went to the radio: note it for the AirEvents.
    pub(crate) fn note_tx(&mut self, tier: Tier, frame: &[u8], now: u32) {
        if let Some(d) = self.air_tx.get_mut(usize::from(self.air_tx_len)) {
            *d = AirNote::new(tier, frame, None, now);
            self.air_tx_len += 1;
        }
    }

    /// The oldest frame sent that the modem has not taken yet.
    pub(crate) fn take_air_tx(&mut self) -> Option<AirNote> {
        if self.air_tx_len == 0 {
            return None;
        }
        let n = self.air_tx.first().copied();
        self.air_tx.rotate_left(1);
        self.air_tx_len -= 1;
        n
    }

    /// Compression is on for the current (or last) call.
    pub fn lz(&self) -> bool {
        self.lz
    }

    /// Host payload bytes over air payload bytes for the current (or last)
    /// call, both directions summed, times 10 and rounded; 10 when nothing
    /// was sent.
    pub fn lz_ratio_x10(&self) -> u32 {
        if self.lz_air == 0 {
            return 10;
        }
        let r = (u64::from(self.lz_raw) * 10 + u64::from(self.lz_air) / 2) / u64::from(self.lz_air);
        u32::try_from(r).unwrap_or(u32::MAX)
    }

    /// Call data bytes moved between the host side and the link, both
    /// directions (taken by `push_data`, handed out by `read_data`): resends
    /// and data still waiting in the link do not count. A wrapping counter;
    /// the menu's throughput is its difference over a second.
    pub fn data_bytes(&self) -> u32 {
        self.moved
    }

    /// Count one Data payload for the ratio.
    fn lz_count(&mut self, raw: usize, air: usize) {
        let (raw, air) = (raw as u32, air as u32);
        self.lz_raw = self.lz_raw.saturating_add(raw);
        self.lz_air = self.lz_air.saturating_add(air);
    }

    /// Store which compression bits the peer offered or agreed.
    fn take_peer_caps(&mut self, caps: u8) {
        self.lz_peer = caps & frame::CAP_LZ != 0;
        self.bulk_peer = caps & frame::CAP_BULK != 0;
        self.stream_peer = caps & frame::CAP_LZ_STREAM != 0;
    }

    /// Bits both ends have offered. Each bit stands on its own.
    fn agreed_caps(&self) -> u8 {
        let mut c = 0u8;
        if self.lz_mine && self.lz_peer {
            c |= frame::CAP_LZ;
        }
        if self.bulk_mine && self.bulk_peer {
            c |= frame::CAP_BULK;
        }
        if self.lz_mine && self.stream_peer {
            c |= frame::CAP_LZ_STREAM;
        }
        c
    }

    /// Retrain or resync: RESET goes into the compressed stream after the
    /// bytes already produced, then the sender's history returns to the preset.
    pub(super) fn arm_stream_reset(&mut self) {
        if !self.lz_stream {
            return;
        }
        self.enc.request_reset();
        self.stream_drain();
        // The first flush can fail while the pending buffer is full. Drain
        // makes room, then the bytes still in the lookahead go out after
        // the RESET token, against the fresh history.
        self.enc.flush_tail();
        self.stream_drain();
    }

    /// While true, an idle link answers every CallReq with CallReject.
    pub fn set_engaged(&mut self, on: bool) {
        self.engaged = on;
    }

    /// Who started the current (or last) call.
    pub fn role(&self) -> Role {
        self.role
    }

    /// True when no sent data waits for an acknowledgement.
    pub fn tx_idle(&self) -> bool {
        self.tx.idle()
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Connected or resyncing: the call exists.
    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected | State::Resync)
    }

    /// The radio under the link was swapped (or put to standby and back):
    /// the next pump calls `listen` again whatever it last asked for.
    pub fn radio_changed(&mut self) {
        self.need_listen = true;
        self.listening = None;
    }

    pub fn is_idle(&self) -> bool {
        self.state == State::Idle
    }

    /// Call id of the current call or call attempt (0 while idle).
    pub fn call_id(&self) -> u8 {
        self.call_id
    }

    /// Queue one discovery frame (Beacon, ScanReq, ScanResp or a type 15
    /// voice beacon or voice scan answer, already encoded, call id 0) on
    /// SF12. Taken only while the link is idle with
    /// nothing else queued, so it never delays a Hangup or CallReject and
    /// never goes out during a call; the next pump sends it and listens
    /// again afterwards. False when refused.
    pub fn send_discovery(&mut self, frame: &[u8]) -> bool {
        if self.state != State::Idle || self.q_len > 0 || frame.len() > QBUF {
            return false;
        }
        self.enq_raw(Tier::ROBUST, frame, false);
        self.q_len > 0
    }

    /// Current tier while a call is up or training.
    pub fn tier(&self) -> Option<Tier> {
        match self.state {
            State::Probe | State::Connected | State::Resync => Some(self.tier),
            _ => None,
        }
    }

    pub fn last_data_tx(&self) -> Option<u32> {
        self.data_tx
    }

    pub fn last_data_rx(&self) -> Option<u32> {
        self.data_rx
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        if self.ev_len == 0 {
            return None;
        }
        let e = self.ev.get(self.ev_head).copied();
        self.ev_head = (self.ev_head + 1) % EVLEN;
        self.ev_len -= 1;
        e
    }

    fn emit(&mut self, e: Event) {
        if self.ev_len < EVLEN {
            if let Some(s) = self.ev.get_mut((self.ev_head + self.ev_len) % EVLEN) {
                *s = e;
                self.ev_len += 1;
            }
        }
    }

    /// Queue host bytes for the peer; returns bytes accepted.
    pub fn push_data(&mut self, data: &[u8]) -> usize {
        let n = if self.is_connected() {
            self.arq_push(data)
        } else {
            0
        };
        self.moved = self.moved.wrapping_add(n as u32);
        n
    }

    /// Received in-order bytes. In stream mode these are plaintext from the
    /// decoder; the ARQ payload itself is the compressed stream.
    pub fn read_data(&mut self, out: &mut [u8]) -> usize {
        let n = if self.lz_stream {
            self.stream_read(out)
        } else {
            self.rx.read(out)
        };
        self.moved = self.moved.wrapping_add(n as u32);
        if self.stream_dead && !self.rx.has_ready() {
            self.dec.reset_call();
            self.stream_dead = false;
        }
        n
    }

    fn listen_tier(&self) -> Tier {
        match self.state {
            State::Probe | State::Connected => self.tier,
            _ => Tier::ROBUST,
        }
    }

    fn enq(
        &mut self,
        tier: Tier,
        ft: FrameType,
        flags: u8,
        payload: &[u8],
        end_turn: bool,
        urgent: bool,
    ) {
        if self.q_len >= QLEN {
            return;
        }
        let h = Header {
            ftype: ft,
            flags,
            call_id: self.call_id,
            seq: 0,
            ack: 0,
        };
        let mut f = QFrame {
            tier,
            len: 0,
            end_turn,
            urgent,
            buf: [0; QBUF],
        };
        if let Ok(n) = frame::encode(&h, payload, &mut f.buf) {
            f.len = n as u8;
            if let Some(s) = self.q.get_mut((self.q_head + self.q_len) % QLEN) {
                *s = f;
                self.q_len += 1;
            }
        }
    }

    fn enq_raw(&mut self, tier: Tier, bytes: &[u8], urgent: bool) {
        if self.q_len >= QLEN || bytes.len() > QBUF {
            return;
        }
        let mut f = QFrame {
            tier,
            len: bytes.len() as u8,
            end_turn: false,
            urgent,
            buf: [0; QBUF],
        };
        if let Some(d) = f.buf.get_mut(..bytes.len()) {
            d.copy_from_slice(bytes);
        }
        if let Some(s) = self.q.get_mut((self.q_head + self.q_len) % QLEN) {
            *s = f;
            self.q_len += 1;
        }
    }

    /// Keep only queued frames for which `keep` is true, in order.
    fn q_retain(&mut self, keep: fn(&QFrame) -> bool) {
        let mut out = [EMPTY_Q; QLEN];
        let mut n = 0;
        for i in 0..self.q_len {
            if let Some(f) = self.q.get((self.q_head + i) % QLEN) {
                if keep(f) {
                    if let Some(d) = out.get_mut(n) {
                        *d = *f;
                        n += 1;
                    }
                }
            }
        }
        self.q = out;
        self.q_head = 0;
        self.q_len = n;
    }

    /// Deterministic jitter in 0..=max ms. The seed mixes call id and role,
    /// so the two ends of a call never draw the same sequence.
    fn jitter(&mut self, max: u32) -> u32 {
        let x = xorshift32(self.rng);
        self.rng = x;
        x % max.saturating_add(1)
    }

    fn ka_payload(&self) -> [u8; 2] {
        self.last_snr.to_le_bytes()
    }

    fn reset_call(&mut self, role: Role, now: u32) {
        // Only last call's Hangup or CallReject may still go out.
        self.q_retain(|f| f.urgent);
        self.role = role;
        let mix = if role == Role::Answerer {
            0x5BD1_E995
        } else {
            0x9E37_79B9
        };
        self.rng = (u32::from(self.call_id).wrapping_mul(0x0101_0101) ^ mix) | 1;
        self.tx.reset();
        self.rx.reset();
        self.lz_mine = false;
        self.lz_peer = false;
        self.stream_peer = false;
        self.lz = false;
        self.lz_stream = false;
        self.stream_dead = false;
        self.cred_raw = 0;
        self.cred_comp = 0;
        self.enc.reset_call();
        self.dec.reset_call();
        self.lz_sync_wait = false;
        self.lz_sync_at = None;
        self.lz_sync_tx = None;
        self.lz_sync_ask_at = 0;
        self.lz_sync_asks = 0;
        self.bulk_mine = false;
        self.bulk_peer = false;
        self.bulk = false;
        self.burst_cap = BURST_MAX;
        self.lz_raw = 0;
        self.lz_air = 0;
        self.staged = None;
        self.pending = None;
        self.ack_owed = false;
        self.ka_reply = false;
        self.got_tierset = false;
        self.voice.on = false;
        self.vs.reset();
        self.start = now;
        self.last_periodic = now;
        self.tier = Tier::ROBUST;
        self.need_listen = true;
    }

    fn go_idle(&mut self) {
        self.slot_hangup = None;
        self.state = State::Idle;
        self.vs.reset();
        self.call_id = 0;
        self.tier = Tier::ROBUST;
        self.pending = None;
        self.staged = None;
        self.need_listen = true;
        // Link loss: drop the sender's dictionary now. The receiver's
        // dictionary stays until `read_data` has delivered what ARQ already
        // holds, then it is cleared too.
        if self.lz_stream {
            self.enc.reset_call();
            self.stream_dead = true;
        }
        self.lz_sync_wait = false;
        self.lz_sync_at = None;
        self.lz_sync_tx = None;
        self.lz_sync_asks = 0;
    }

    fn fail(&mut self, e: Event, bye: bool) {
        self.q_retain(|f| f.urgent);
        if bye {
            let t = self.listen_tier();
            self.enq(t, FrameType::Hangup, 0, &[], false, true);
        }
        self.emit(e);
        self.go_idle();
    }

    /// Handle one received frame.
    pub fn on_frame(&mut self, buf: &[u8], info: RxInfo, me: &str, sregs: &SRegs, now: u32) {
        let Ok(f) = frame::decode(buf) else { return };
        let h = f.header;
        if h.ftype == FrameType::CallReq {
            self.on_call_req(f.payload, h.call_id, info, me, sregs, now);
            return;
        }
        if self.state == State::Idle || h.call_id != self.call_id {
            return;
        }
        if self.voice.on && self.voice_frame_screen(h.ftype, f.payload) {
            return;
        }
        if h.ftype == FrameType::Hangup {
            if self.state != State::Ringing {
                self.emit(Event::NoCarrier);
            }
            self.go_idle();
            return;
        }
        let tier_arg = f.payload.first().and_then(|&i| Tier::from_index(i));
        match self.state {
            State::Dialing | State::AwaitTrain => match h.ftype {
                FrameType::CallAck => self.state = State::AwaitTrain,
                FrameType::CallReject => self.fail(Event::Busy, false),
                FrameType::Train => {
                    // A voice Train carries the token (`voice_frame_screen`
                    // bound it): the usual form is in front of it.
                    let p = if self.voice.on {
                        vcall::split_train(f.payload).map_or(&[][..], |(b, _)| b)
                    } else {
                        f.payload
                    };
                    let peer_snr = match p {
                        [_, a, b] | [_, a, b, _] => i16::from_le_bytes([*a, *b]),
                        _ => return,
                    };
                    // The caller cannot connect without a Train, so its 4th
                    // byte is the one place the agreed capability answer is read.
                    self.take_peer_caps(frame::parse_train_caps(p));
                    let peer_cap = tier_arg.unwrap_or(Tier::ROBUST);
                    self.own_cap = cap_tier(sregs);
                    self.cap = self.own_cap.min(peer_cap);
                    let snr = info.snr_x4.min(peer_snr);
                    let margin = reg(sregs, 20, 20) as i16;
                    let t = best_tier(snr, margin).min(self.cap);
                    self.my_snr = snr;
                    self.emit(Event::Training(snr));
                    self.start_tierset(t, now);
                }
                _ => {}
            },
            State::AwaitTierSet | State::Probe if self.role == Role::Answerer => match h.ftype {
                FrameType::TierSet => {
                    let Some(t) = tier_arg.map(|t| t.min(self.own_cap)) else {
                        return;
                    };
                    self.got_tierset = true;
                    // TierAck goes out on the tier we heard TierSet on.
                    self.enq(info.tier, FrameType::TierAck, 0, &[t.index()], false, false);
                    self.tier = t;
                    self.state = State::Probe;
                    self.start = now;
                    // Long enough for both caller probes (window W each,
                    // second one jittered), short enough to be back on SF12
                    // before the caller's step-down TierSet at about 2 W.
                    self.probe_ms = air_ms(Tier::ROBUST, HEADER_LEN + 1)
                        .saturating_add(caller_probe_ms(t))
                        .saturating_add(PROBE_JITTER_MS)
                        .saturating_add(air_ms(t, KA_LEN))
                        .saturating_add(120);
                    self.need_listen = true;
                }
                FrameType::Keepalive if self.state == State::Probe && h.flags & FLAG_PING != 0 => {
                    self.last_snr = info.snr_x4;
                    let p = self.ka_payload();
                    let t = self.tier;
                    self.enq(t, FrameType::Keepalive, 0, &p, true, false);
                    self.connect(now, info.snr_x4);
                }
                _ => {}
            },
            State::AwaitTierAck => {
                let acked = tier_arg.filter(|a| *a <= self.train);
                if let (FrameType::TierAck, Some(a)) = (h.ftype, acked) {
                    // A lower ack is the peer's cap.
                    self.cap = self.cap.min(a);
                    self.train = a;
                    self.tier = a;
                    self.state = State::Probe;
                    self.start = now;
                    self.probe_ms = caller_probe_ms(a);
                    self.probe_tries = 1;
                    self.need_listen = true;
                    self.send_probe();
                }
            }
            State::Probe => {
                // Caller: anything from the peer on the probe tier means it hears us.
                if matches!(
                    h.ftype,
                    FrameType::Keepalive | FrameType::Data | FrameType::Ack
                ) {
                    self.connect(now, info.snr_x4);
                    self.on_connected(&f, info, sregs, now);
                }
            }
            State::Connected | State::Resync => self.on_connected(&f, info, sregs, now),
            _ => {}
        }
    }

    /// Timers and transmit. Call every 10 ms or faster.
    pub fn tick<R: Radio>(&mut self, radio: &mut R, sregs: &SRegs, now: u32) {
        let s7 = reg(sregs, 7, 30).saturating_mul(1000);
        if self.voice.on && radio.busy() {
            // A voice answer waits for a quiet channel (`voice_await_tick`).
            self.voice.busy_at = now;
        }
        if let Some(at) = self.slot_hangup {
            // Both Hangups went out in our sub-slots (or the call ended some
            // other way): idle now. If no sub-slot came within three listen
            // periods, send one at once, as outside the slots.
            let lay = crate::voice::slot_layout(self.tier);
            let limit = lay
                .frame_ms
                .saturating_mul(u32::from(lay.listen_every.max(1)))
                .saturating_mul(3)
                .saturating_add(500);
            let stuck = since(now, at) >= limit;
            if self.q_len == 0 || stuck || self.state != State::Connected || !self.vs.slots {
                if stuck && self.q_len > 0 {
                    let t = self.tier;
                    self.q_len = 0;
                    self.enq(t, FrameType::Hangup, 0, &[], false, true);
                }
                self.go_idle();
            }
        }
        match self.state {
            State::Idle => {}
            State::Dialing => {
                if since(now, self.start) >= s7 {
                    self.fail(Event::NoAnswer, true);
                } else if since(now, self.last_periodic) >= PERIOD_MS {
                    self.send_callreq(now);
                }
            }
            // Before the S7 arm: a voice answerer that was never bound must
            // stay silent (no Hangup over someone else's call).
            State::AwaitTierSet if self.voice.on => self.voice_await_tick(s7, radio.busy(), now),
            State::AwaitTrain | State::AwaitTierSet | State::AwaitTierAck
                if since(now, self.start) >= s7 =>
            {
                self.fail(Event::NoCarrier, true);
            }
            State::AwaitTrain
                if self.voice.on && since(now, self.voice.bound_at) >= 3 * PERIOD_MS =>
            {
                self.voice_unbind(now);
            }
            State::AwaitTrain => {}
            State::AwaitTierSet => {
                if !self.got_tierset && since(now, self.last_periodic) >= PERIOD_MS {
                    self.send_train();
                    self.last_periodic = now;
                }
            }
            State::AwaitTierAck => {
                let every = air_ms(Tier::ROBUST, HEADER_LEN + 1)
                    .saturating_mul(3)
                    .saturating_add(200);
                if since(now, self.last_periodic) >= every {
                    self.last_periodic = now;
                    let (p, n) = self.voice.tierset(self.train.index());
                    self.enq(
                        Tier::ROBUST,
                        FrameType::TierSet,
                        0,
                        p.get(..n).unwrap_or(&[]),
                        false,
                        false,
                    );
                }
            }
            State::Ringing => {
                if since(now, self.start) >= s7 || since(now, self.last_req) >= 2 * PERIOD_MS + 1000
                {
                    self.go_idle();
                } else if since(now, self.last_periodic) >= PERIOD_MS {
                    self.ring(sregs, now);
                }
            }
            State::Probe => {
                if since(now, self.start) >= self.probe_ms {
                    match (self.role, self.tier.slower()) {
                        (Role::Caller, _) if self.probe_tries < PROBE_TRIES => {
                            // One more probe on the same tier, jittered.
                            self.probe_tries += 1;
                            self.start = now;
                            self.probe_ms = caller_probe_ms(self.tier)
                                .saturating_add(self.jitter(PROBE_JITTER_MS));
                            self.send_probe();
                        }
                        (Role::Caller, Some(s)) => self.start_tierset(s, now),
                        (Role::Caller, None) => self.fail(Event::NoCarrier, true),
                        (Role::Answerer, _) => {
                            self.state = State::AwaitTierSet;
                            self.start = now;
                            self.need_listen = true;
                        }
                    }
                }
            }
            State::Connected => self.connected_timers(sregs, now),
            State::Resync => {
                if since(now, self.start) >= reg(sregs, 22, 20).saturating_mul(1000) {
                    self.fail(Event::NoCarrier, false);
                } else if self.role == Role::Caller {
                    let every = air_ms(Tier::ROBUST, KA_LEN)
                        .saturating_mul(3)
                        .saturating_add(200);
                    if since(now, self.last_periodic) >= every {
                        self.last_periodic = now;
                        let p = self.ka_payload();
                        self.enq(
                            Tier::ROBUST,
                            FrameType::Keepalive,
                            FLAG_PING,
                            &p,
                            true,
                            false,
                        );
                    }
                }
            }
        }
        self.pump(radio, now);
    }
}
