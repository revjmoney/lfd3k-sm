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

use crate::arq::{ArqRx, ArqTx, CHUNK};
use crate::at::SRegs;
use crate::frame::{self, FrameType, Header, HEADER_LEN, MAX_FRAME};
use crate::radio::{Radio, RxInfo};
use crate::tier::{best_tier, Tier};

mod data;
mod retrain;
mod setup;

/// Header flag: another frame follows at once.
pub const FLAG_MORE: u8 = 1;
/// Header flag on Keepalive: please reply.
pub const FLAG_PING: u8 = 2;

const PERIOD_MS: u32 = 3000; // CallReq resend and RING cadence
/// Frames per turn. A full burst of 4 Data frames lasts 4 airtimes, longer
/// than the 3 airtime RTO, so the first frame of a full burst can be resent
/// once before its ack lands; staging prefers unsent frames, which keeps that
/// to the tail of a transfer.
const BURST_MAX: u8 = 4;
const TURN_YIELD_MS: u32 = 100;
const UP_HOLD_MS: u32 = 10_000;
const KA_LEN: usize = HEADER_LEN + 2;
/// Training probes per tier before stepping one tier slower.
const PROBE_TRIES: u8 = 2;
/// Most jitter added to a training probe retry.
const PROBE_JITTER_MS: u32 = 50;
const QLEN: usize = 4;
const QBUF: usize = 40;
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

fn air_ms(t: Tier, len: usize) -> u32 {
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
fn turn_gap_ms(t: Tier) -> u32 {
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

fn reg(s: &SRegs, n: u8, d: u16) -> u32 {
    u32::from(s.get(n).unwrap_or(d))
}

fn cap_tier(s: &SRegs) -> Tier {
    Tier::from_index(reg(s, 21, 7).min(7) as u8).unwrap_or(Tier::Sf5)
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
}

impl Default for Link {
    fn default() -> Self {
        Self::new()
    }
}

impl Link {
    pub fn new() -> Self {
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
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Connected or resyncing: the call exists.
    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected | State::Resync)
    }

    pub fn is_idle(&self) -> bool {
        self.state == State::Idle
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
        if self.is_connected() {
            self.tx.push(data)
        } else {
            0
        }
    }

    /// Received in-order bytes.
    pub fn read_data(&mut self, out: &mut [u8]) -> usize {
        self.rx.read(out)
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

    fn enq_raw(&mut self, tier: Tier, bytes: &[u8]) {
        if self.q_len >= QLEN || bytes.len() > QBUF {
            return;
        }
        let mut f = QFrame {
            tier,
            len: bytes.len() as u8,
            end_turn: false,
            urgent: false,
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
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
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
        self.staged = None;
        self.pending = None;
        self.ack_owed = false;
        self.ka_reply = false;
        self.got_tierset = false;
        self.start = now;
        self.last_periodic = now;
        self.tier = Tier::ROBUST;
        self.need_listen = true;
    }

    fn go_idle(&mut self) {
        self.state = State::Idle;
        self.call_id = 0;
        self.tier = Tier::ROBUST;
        self.pending = None;
        self.staged = None;
        self.need_listen = true;
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
                    let peer_snr = match f.payload {
                        [_, a, b] => i16::from_le_bytes([*a, *b]),
                        _ => return,
                    };
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
        match self.state {
            State::Idle => {}
            State::Dialing => {
                if since(now, self.start) >= s7 {
                    self.fail(Event::NoAnswer, true);
                } else if since(now, self.last_periodic) >= PERIOD_MS {
                    self.send_callreq(now);
                }
            }
            State::AwaitTrain | State::AwaitTierSet | State::AwaitTierAck
                if since(now, self.start) >= s7 =>
            {
                self.fail(Event::NoCarrier, true);
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
                    let t = self.train;
                    self.enq(
                        Tier::ROBUST,
                        FrameType::TierSet,
                        0,
                        &[t.index()],
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
