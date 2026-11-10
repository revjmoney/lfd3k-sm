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

fn is_tierset(f: &QFrame) -> bool {
    f.buf
        .first()
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
    pending: Option<Tier>,
    pend_last: u32,
    pend_tries: u8,
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
            pending: None,
            pend_last: 0,
            pend_tries: 0,
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

    /// Drop the pending retrain and any of its TierSets still queued.
    fn clear_pending(&mut self) {
        self.pending = None;
        self.q_retain(|f| !is_tierset(f));
    }

    fn ka_payload(&self) -> [u8; 2] {
        self.last_snr.to_le_bytes()
    }

    fn reset_call(&mut self, role: Role, now: u32) {
        // Only last call's Hangup or CallReject may still go out.
        self.q_retain(|f| f.urgent);
        self.role = role;
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

    /// Start a call. False if busy or a name is invalid.
    pub fn dial(&mut self, me: &str, callee: &str, now: u32) -> bool {
        if self.state != State::Idle {
            return false;
        }
        let mut p = [0u8; QBUF];
        let Ok(plen) = frame::call_req_payload(me, callee, &mut p) else {
            return false;
        };
        let call_id = (now as u8) | 1;
        let h = Header {
            ftype: FrameType::CallReq,
            flags: 0,
            call_id,
            seq: 0,
            ack: 0,
        };
        let Ok(n) = frame::encode(&h, p.get(..plen).unwrap_or(&[]), &mut self.callreq) else {
            return false;
        };
        self.callreq_len = n as u8;
        self.call_id = call_id;
        self.reset_call(Role::Caller, now);
        self.state = State::Dialing;
        self.send_callreq(now);
        true
    }

    fn send_callreq(&mut self, now: u32) {
        let mut b = [0u8; QBUF];
        let n = self.callreq_len as usize;
        if let (Some(d), Some(s)) = (b.get_mut(..n), self.callreq.get(..n)) {
            d.copy_from_slice(s);
        }
        self.enq_raw(Tier::ROBUST, b.get(..n).unwrap_or(&[]));
        self.last_periodic = now;
        self.emit(Event::Ringing);
    }

    /// Answer a ringing call. False if nothing is ringing.
    pub fn answer(&mut self, sregs: &SRegs, now: u32) -> bool {
        if self.state != State::Ringing {
            return false;
        }
        let call_id = self.call_id;
        let snr = self.my_snr;
        self.reset_call(Role::Answerer, now);
        self.call_id = call_id;
        self.my_snr = snr;
        self.own_cap = cap_tier(sregs);
        self.cap = self.own_cap;
        self.state = State::AwaitTierSet;
        self.send_train();
        true
    }

    fn send_train(&mut self) {
        let s = self.my_snr.to_le_bytes();
        let p = [self.cap.index(), s[0], s[1]];
        self.enq(Tier::ROBUST, FrameType::CallAck, 0, &[], false, false);
        self.enq(Tier::ROBUST, FrameType::Train, 0, &p, false, false);
    }

    /// Hang up. True if a call (or call attempt) was up.
    pub fn hangup(&mut self, _now: u32) -> bool {
        match self.state {
            State::Idle => false,
            State::Ringing => {
                self.enq(Tier::ROBUST, FrameType::CallReject, 0, &[], false, true);
                self.go_idle();
                false
            }
            _ => {
                let t = self.listen_tier();
                self.q_len = 0;
                self.enq(t, FrameType::Hangup, 0, &[], false, true);
                self.enq(t, FrameType::Hangup, 0, &[], false, true);
                self.go_idle();
                true
            }
        }
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

    fn start_tierset(&mut self, t: Tier, now: u32) {
        self.train = t;
        self.state = State::AwaitTierAck;
        self.start = now;
        self.last_periodic = now;
        self.need_listen = true;
        self.enq(
            Tier::ROBUST,
            FrameType::TierSet,
            0,
            &[t.index()],
            false,
            false,
        );
    }

    /// Retrain request inside a call: TierSet on the current tier.
    fn request(&mut self, t: Tier, now: u32) {
        self.clear_pending();
        self.pending = Some(t);
        self.pend_last = now;
        self.pend_tries = 1;
        let cur = self.tier;
        self.enq(cur, FrameType::TierSet, 0, &[t.index()], true, false);
    }

    fn connect(&mut self, now: u32, snr: i16) {
        self.state = State::Connected;
        self.last_rx = now;
        self.last_tx = now;
        self.last_change = now;
        self.ewma_x32 = i32::from(snr) * 8;
        self.good_since = None;
        self.silence_done = false;
        self.need_listen = true;
        self.turn = true;
        self.burst = 0;
        self.emit(Event::Connect(self.tier));
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
                    self.probe_ms = air_ms(Tier::ROBUST, HEADER_LEN + 1)
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
                    self.probe_ms = air_ms(self.tier, KA_LEN)
                        .saturating_mul(3)
                        .saturating_add(200);
                    self.need_listen = true;
                    let p = self.ka_payload();
                    let t = self.tier;
                    self.enq(t, FrameType::Keepalive, FLAG_PING, &p, true, false);
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

    fn on_call_req(
        &mut self,
        p: &[u8],
        call_id: u8,
        info: RxInfo,
        me: &str,
        sregs: &SRegs,
        now: u32,
    ) {
        let Ok((_, callee)) = frame::parse_call_req(p) else {
            return;
        };
        if !callee.eq_ignore_ascii_case(me) || call_id == 0 {
            return;
        }
        if self.state == State::Idle {
            self.call_id = call_id;
            self.my_snr = info.snr_x4;
            self.state = State::Ringing;
            self.start = now;
            self.last_req = now;
            self.last_periodic = now;
            self.rings = 0;
            self.ring(sregs, now);
        } else if call_id == self.call_id {
            self.last_req = now;
            self.my_snr = info.snr_x4;
            if self.state == State::AwaitTierSet && !self.got_tierset {
                self.send_train();
                self.last_periodic = now;
            }
        } else {
            let own = self.call_id;
            self.call_id = call_id;
            self.enq(Tier::ROBUST, FrameType::CallReject, 0, &[], false, false);
            self.call_id = own;
        }
    }

    fn ring(&mut self, sregs: &SRegs, now: u32) {
        self.rings = self.rings.saturating_add(1);
        self.last_periodic = now;
        self.emit(Event::Ring);
        let s0 = reg(sregs, 0, 0);
        if s0 > 0 && u32::from(self.rings) >= s0 {
            self.answer(sregs, now);
        }
    }

    fn on_connected(&mut self, f: &frame::Frame<'_>, info: RxInfo, sregs: &SRegs, now: u32) {
        let h = f.header;
        self.last_rx = now;
        self.last_snr = info.snr_x4;
        self.silence_done = false;
        if self.state == State::Resync {
            self.state = State::Connected;
            self.ewma_x32 = i32::from(info.snr_x4) * 8;
            self.last_change = now;
            self.good_since = None;
            self.need_listen = true;
            if self.resync_prev != Tier::ROBUST {
                self.emit(Event::Retrain(Tier::ROBUST));
            }
            let prev = self.resync_prev;
            self.resync_prev = Tier::ROBUST;
            if self.role == Role::Caller {
                let peer = match (h.ftype, f.payload) {
                    (FrameType::Keepalive, [a, b]) => i16::from_le_bytes([*a, *b]),
                    _ => info.snr_x4,
                };
                // Never jump straight back to the tier that just died.
                let below = prev.slower().unwrap_or(Tier::ROBUST);
                let t = best_tier(info.snr_x4.min(peer), reg(sregs, 20, 20) as i16)
                    .min(self.cap)
                    .min(below);
                if t > Tier::ROBUST {
                    self.request(t, now);
                }
            }
        } else {
            let e = self.ewma_x32;
            self.ewma_x32 = e
                .saturating_add(i32::from(info.snr_x4))
                .saturating_sub(e / 8);
        }
        if h.flags & FLAG_MORE != 0 {
            self.turn = false;
            self.turn_start = now;
            self.turn_ms =
                air_ms(self.tier, HEADER_LEN + CHUNK).saturating_add(turn_gap_ms(self.tier));
        } else {
            self.turn = true;
            self.burst = 0;
        }
        let tier_arg = f.payload.first().and_then(|&i| Tier::from_index(i));
        match h.ftype {
            FrameType::Data => {
                self.rx.on_data(h.seq, f.payload);
                // Ack every Data frame, whatever on_data said.
                self.ack_owed = true;
                self.data_rx = Some(now);
            }
            FrameType::Ack => {
                if let [sack] = f.payload {
                    self.tx.on_ack(h.ack, *sack);
                }
            }
            FrameType::Keepalive => {
                if h.flags & FLAG_PING != 0 {
                    self.ka_reply = true;
                }
            }
            FrameType::TierSet => {
                let Some(t) = tier_arg else { return };
                if self.pending.is_some() && self.role == Role::Caller {
                    return; // caller's own request wins
                }
                self.clear_pending();
                // Above our cap: ack our cap instead, and move there.
                let t = t.min(self.own_cap);
                self.enq(info.tier, FrameType::TierAck, 0, &[t.index()], true, false);
                self.switch(t, now);
            }
            FrameType::TierAck => {
                if let (Some(a), Some(p)) = (tier_arg, self.pending) {
                    if a <= p {
                        self.clear_pending();
                        if a < p {
                            self.cap = self.cap.min(a); // peer's cap
                        }
                        self.switch(a, now);
                    }
                }
            }
            _ => {}
        }
    }

    fn switch(&mut self, t: Tier, now: u32) {
        self.last_change = now;
        self.good_since = None;
        self.need_listen = true;
        if t != self.tier {
            self.tier = t;
            self.emit(Event::Retrain(t));
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

    fn connected_timers(&mut self, sregs: &SRegs, now: u32) {
        let rto = rto_ms(self.tier);
        let silent = since(now, self.last_rx);
        if silent >= rto.saturating_mul(8) {
            self.resync_prev = self.tier;
            self.tier = Tier::ROBUST;
            self.state = State::Resync;
            // Already on SF12: the S22 count runs from the last frame heard.
            self.start = if self.resync_prev == Tier::ROBUST {
                self.last_rx
            } else {
                now
            };
            self.last_periodic = now.wrapping_sub(u32::MAX / 2);
            self.clear_pending();
            self.turn = true;
            self.need_listen = true;
            return;
        }
        if let Some(p) = self.pending {
            if since(now, self.pend_last) >= rto {
                if self.pend_tries >= 3 {
                    self.clear_pending(); // resync recovers if the peer moved
                } else {
                    self.pend_tries += 1;
                    self.pend_last = now;
                    let cur = self.tier;
                    self.enq(cur, FrameType::TierSet, 0, &[p.index()], true, false);
                }
            }
            return;
        }
        if silent >= rto.saturating_mul(4) && !self.silence_done {
            self.silence_done = true;
            if let Some(s) = self.tier.slower() {
                self.request(s, now);
            }
            return;
        }
        if since(now, self.last_change) < rto.saturating_mul(2) {
            return;
        }
        let s20 = reg(sregs, 20, 20) as i32;
        let ewma = self.ewma_x32 / 8;
        let floor = i32::from(self.tier.snr_floor_x4());
        if let Some(s) = self.tier.slower() {
            if ewma < floor + s20 - 8 {
                self.request(s, now);
                return;
            }
        }
        let up = self.tier.faster().filter(|f| *f <= self.cap);
        match up {
            Some(f) if ewma > i32::from(f.snr_floor_x4()) + s20 + 12 => {
                let g = *self.good_since.get_or_insert(now);
                if since(now, g) >= UP_HOLD_MS {
                    self.good_since = None;
                    self.request(f, now);
                }
            }
            _ => self.good_since = None,
        }
    }

    /// Send at most one frame if the radio is free, else re-listen.
    pub fn pump<R: Radio>(&mut self, radio: &mut R, now: u32) {
        if radio.busy() {
            return;
        }
        let urgent = self.q_len > 0 && self.q.get(self.q_head).is_some_and(|f| f.urgent);
        let turns = self.state == State::Connected;
        if turns && !urgent && !self.turn {
            if since(now, self.turn_start) >= self.turn_ms {
                self.turn = true;
                self.burst = 0;
            } else {
                self.relisten(radio);
                return;
            }
        }
        let mut buf = [0u8; MAX_FRAME];
        let Some((len, tier, end_turn, is_data)) = self.next_frame(&mut buf, now) else {
            self.relisten(radio);
            return;
        };
        let more = turns && !end_turn && self.burst + 1 < BURST_MAX && self.has_more(now);
        if more {
            if let Some(b0) = buf.get_mut(0) {
                *b0 |= FLAG_MORE;
            }
        }
        radio.send(tier, buf.get(..len).unwrap_or(&[]));
        self.need_listen = true;
        self.last_tx = now;
        if is_data {
            self.data_tx = Some(now);
        }
        if more {
            self.burst += 1;
        } else {
            self.turn = !turns;
            self.burst = 0;
            self.turn_start = now;
            let y = if self.role == Role::Answerer {
                TURN_YIELD_MS
            } else {
                0
            };
            self.turn_ms = air_ms(tier, len).saturating_add(turn_gap_ms(tier) + y);
        }
    }

    fn relisten<R: Radio>(&mut self, radio: &mut R) {
        let want = self.listen_tier();
        if self.need_listen || self.listening != Some(want) {
            radio.listen(want);
            self.listening = Some(want);
            self.need_listen = false;
        }
    }

    fn has_more(&mut self, now: u32) -> bool {
        self.q_len > 0 || self.ack_owed || self.stage(now)
    }

    /// Make sure a data frame is staged; true if one is.
    fn stage(&mut self, now: u32) -> bool {
        if self.state != State::Connected {
            return false;
        }
        if self.staged.is_some() {
            return true;
        }
        let rto = rto_ms(self.tier);
        if let Some((seq, d)) = self.tx.next_to_send(now, rto) {
            let n = d.len().min(CHUNK);
            if let (Some(dst), Some(src)) = (self.staged_buf.get_mut(..n), d.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.staged = Some((seq, n as u8));
        }
        self.staged.is_some()
    }

    /// Build the next frame: (len, tier, end_turn, is_data).
    fn next_frame(&mut self, out: &mut [u8], now: u32) -> Option<(usize, Tier, bool, bool)> {
        if self.q_len > 0 {
            let f = self.q.get(self.q_head).copied()?;
            self.q_head = (self.q_head + 1) % QLEN;
            self.q_len -= 1;
            let n = f.len as usize;
            out.get_mut(..n)?.copy_from_slice(f.buf.get(..n)?);
            return Some((n, f.tier, f.end_turn, false));
        }
        if self.state != State::Connected {
            return None;
        }
        let t = self.tier;
        let mk = |ft, flags, seq, ack, p: &[u8], out: &mut [u8], cid| {
            let h = Header {
                ftype: ft,
                flags,
                call_id: cid,
                seq,
                ack,
            };
            frame::encode(&h, p, out).ok()
        };
        if self.ack_owed {
            self.ack_owed = false;
            self.ka_reply = false;
            let (a, sack) = self.rx.ack_state();
            let n = mk(FrameType::Ack, 0, 0, a, &[sack], out, self.call_id)?;
            return Some((n, t, false, false));
        }
        if self.stage(now) {
            let (seq, len) = self.staged.take()?;
            self.ka_reply = false;
            let n = mk(
                FrameType::Data,
                0,
                seq,
                0,
                self.staged_buf.get(..len as usize)?,
                out,
                self.call_id,
            )?;
            return Some((n, t, false, true));
        }
        let rto = rto_ms(t);
        let mut idle = rto.saturating_mul(2);
        if self.role == Role::Answerer {
            idle = idle.saturating_add(rto / 2);
        }
        let quiet = since(now, self.last_tx) >= idle && since(now, self.last_rx) >= idle;
        if self.ka_reply || quiet {
            let flags = if self.ka_reply { 0 } else { FLAG_PING };
            self.ka_reply = false;
            let n = mk(
                FrameType::Keepalive,
                flags,
                0,
                0,
                &self.ka_payload(),
                out,
                self.call_id,
            )?;
            return Some((n, t, true, false));
        }
        None
    }
}
