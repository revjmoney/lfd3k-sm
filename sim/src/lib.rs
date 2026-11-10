//! PC simulator: a seeded, half duplex LoRa channel and a two modem harness.
//!
//! # Channel model
//! - Two radios, index 0 (`CALLER`, modem `a`) and 1 (`AXIS`, modem `b`).
//! - A frame sent at time `t` on tier `T` occupies the air from `t` to
//!   `t + T.airtime_us(len)`. The other radio receives it only if, at `t`, it
//!   is listening on `T` and not transmitting, and it stays that way until
//!   the end (it does not send and does not `listen` on another tier).
//!   Intervals are half open: a radio whose transmission ends at `t`, or
//!   that starts listening at `t`, still hears a frame that starts at `t`.
//! - Overlapping receptions at a radio collide: both are lost.
//! - Loss from the SNR margin `m = snr - floor(T)` (dB x4):
//!   `m >= 8`: 0.5 %, `0 <= m < 8`: 10 %, `-8 <= m < 0`: 60 %, below: 100 %.
//!   Below -8 the preamble is not even detected (the radio never goes busy);
//!   otherwise the radio is busy from preamble end to frame end and the loss
//!   is drawn at the end (a CRC failure), using the SNR at that time.
//! - `drop` is consulted once per frame when it starts, with the send time
//!   in ms and the frame bytes; `true` loses that frame (preamble still seen).
//! - Delivery: `Modem::radio_rx` at the first 1 ms step at or after the
//!   frame's end, with `RxInfo { tier, snr_x4: profile at that time,
//!   rssi_dbm: -120 + snr dB }`.
//!
//! # Radio contract (core/src/radio.rs)
//! `busy()` is true while transmitting and while receiving a detected frame
//! (from preamble end to frame end). `send` stops reception until the core
//! calls `listen` again.
//!
//! # Time
//! `run_ms` advances in 1 ms steps. In each step, in this order:
//! 1. TX done: a modem whose transmission ended in this step is ticked at
//!    once (a firmware TX done interrupt does the same), so it can listen
//!    again or send the next frame of a burst.
//! 2. Deliveries, by end time, `a` before `b` on ties.
//! 3. Every 10 ms both modems are ticked, `a` first.
//!
//! # Received data
//! `received_a()` / `received_b()` return the bytes that modem's host saw
//! while in data mode. The host output (`screen_*`) is split into data
//! segments:
//! - A segment starts right after a verbose `\r\nCONNECT <...>\r\n` line
//!   found in command mode text (also after `ATO`).
//! - It ends at the escape: when this host typed three `+` while in the
//!   segment, a later tick's output that ends with `\r\nOK\r\n` (or with that
//!   OK plus one `\r\nRETRAIN ...\r\n` line) closes the segment where the OK
//!   starts. Bytes before the OK in the same output are still data. The
//!   `CONNECT` line that `ATO` prints opens a new segment.
//! - Or it ends when the modem drops the call (`is_connected()` false): at
//!   the start of the last `\r\nNO CARRIER\r\n`, or at the end of the output
//!   if there is none.
//!
//! The split needs verbose result codes (ATV1, the default) and the default
//! escape character `+`. Data that itself contains `\r\nNO CARRIER\r\n` is
//! still split correctly (the last one is used), and data containing
//! `\r\nOK\r\n` is only misread if a tick's data output ends with it while an
//! escape is pending.
//!
//! Limit: the split only looks for the escape's OK in tick output. The core
//! also fires a completed escape at the start of `host_input` when its guard
//! ran out before a tick did (the host typed again within 10 ms of the guard
//! end); then that side's segment stays open and its `received_*` would count
//! the OK and later command output as data. Tests that read `received_*` on
//! the escaping side wait past a tick after the guard (as `escape_a` does).
//!
//! # Fake wire
//! A `FakeWire` attached to a modem (`attach_wire_a` / `attach_wire_b`) plays
//! the platform plus a TCP server for that modem's Wi-Fi calls and bridges.
//! It is serviced once per 1 ms step, after that step's ticks, and only when
//! attached (a harness without one behaves exactly as before). Each service:
//! 1. a close the modem asked for: what `wire_output` still holds is read
//!    into `received`, then the connection is closed;
//! 2. a new request is recorded and answered per `WireScript`;
//! 3. a connect that is due calls `wire_connected` (after `connect_ms`) and
//!    queues the script's opening bytes;
//! 4. while open: everything from `wire_output` is recorded (and echoed,
//!    per script), then queued server bytes go to `wire_input` (what it
//!    refuses waits for the next step); a due close calls `wire_closed`.

pub mod rng;

use std::collections::VecDeque;

use lfd3k_core::frame::{decode, FrameType};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
pub use rng::Rng;

/// SNR over time, in dB x4.
#[derive(Clone, Copy, Debug)]
pub enum SnrProfile {
    Constant(i16),
    /// `(start ms, snr_x4)` in ascending start order. Before the first
    /// step the first value applies.
    Steps(&'static [(u32, i16)]),
}

impl SnrProfile {
    pub fn at(&self, ms: u32) -> i16 {
        match self {
            SnrProfile::Constant(s) => *s,
            SnrProfile::Steps(steps) => {
                let mut v = steps.first().map(|s| s.1).unwrap_or(0);
                for &(start, snr) in steps.iter() {
                    if start <= ms {
                        v = snr;
                    }
                }
                v
            }
        }
    }
}

pub struct SimChannel {
    pub profile: SnrProfile,
    pub seed: u64,
    /// Called once per frame at its start: `(send time ms, bytes)`; true drops it.
    pub drop: Option<fn(u32, &[u8]) -> bool>,
}

impl SimChannel {
    /// Constant SNR in whole dB, no drop hook.
    pub fn constant_db(snr_db: i16, seed: u64) -> SimChannel {
        SimChannel {
            profile: SnrProfile::Constant(snr_db.saturating_mul(4)),
            seed,
            drop: None,
        }
    }
}

/// Loss probability (per 1000) for an SNR margin in dB x4.
pub fn loss_x1000(margin_x4: i16) -> u32 {
    if margin_x4 >= 8 {
        5
    } else if margin_x4 >= 0 {
        100
    } else if margin_x4 >= -8 {
        600
    } else {
        1000
    }
}

/// Preamble time in microseconds (12.25 symbols; 18.25 at SF5 and SF6).
pub fn preamble_us(t: Tier) -> u64 {
    let tsym = (1u64 << t.sf()) * 2;
    let quarters = if t.sf() <= 6 { 73 } else { 49 };
    tsym * quarters / 4
}

/// What happened to a frame at the receiving radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Still on the air.
    Pending,
    Delivered,
    /// Lost to the SNR draw (CRC failure).
    Lost,
    /// Preamble not detected (margin below -8).
    Undetected,
    /// Overlapped another reception.
    Collided,
    /// Receiver was transmitting.
    HalfDuplex,
    /// Receiver was not listening on this tier.
    NotListening,
    /// The `drop` hook said so.
    Dropped,
}

/// One frame on the air.
#[derive(Clone, Copy, Debug)]
pub struct TraceEntry {
    pub t_ms: u32,
    pub end_us: u64,
    /// 0 = a (CALLER), 1 = b (AXIS).
    pub from: usize,
    pub tier: Tier,
    /// Decoded frame type; `None` if the core sent bytes that do not decode.
    pub ftype: Option<FrameType>,
    pub flags: u8,
    pub call_id: u8,
    pub seq: u8,
    pub len: usize,
    /// A Data frame whose sequence number was already sent in this call.
    pub resend: bool,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub frames_sent: [u32; 2],
    pub data_frames: [u32; 2],
    /// Data frames sent again (same sequence number, same call).
    pub resends: [u32; 2],
    pub delivered: u32,
    pub lost: u32,
    pub undetected: u32,
    pub collided: u32,
    pub half_duplex: u32,
    pub not_listening: u32,
    pub dropped: u32,
    /// Per sender, the tier of its in-call frames (Data, Ack, Keepalive)
    /// on each change: `(ms, tier)`.
    pub tiers: [Vec<(u32, Tier)>; 2],
}

struct Rx {
    trace: usize,
    tier: Tier,
    bytes: Vec<u8>,
    pre_end_us: u64,
    end_us: u64,
    /// Killed by our own send or a listen on another tier.
    alive: bool,
    hooked: bool,
    outcome: Outcome,
}

/// Simulated SX1262-like radio. Owned by the harness.
pub struct SimRadio {
    now_us: u64,
    tx_end_us: u64,
    listen: Option<Tier>,
    outbox: Vec<(Tier, Vec<u8>)>,
    rx: Vec<Rx>,
}

impl SimRadio {
    fn new() -> SimRadio {
        SimRadio {
            now_us: 0,
            tx_end_us: 0,
            listen: None,
            outbox: Vec::new(),
            rx: Vec::new(),
        }
    }

    fn transmitting(&self) -> bool {
        self.now_us < self.tx_end_us
    }

    fn kill_rx(&mut self, why: Outcome) {
        for r in self.rx.iter_mut().filter(|r| r.alive) {
            r.alive = false;
            if r.outcome == Outcome::Pending {
                r.outcome = why;
            }
        }
    }

    /// Tier the radio is listening on, if any.
    pub fn listening(&self) -> Option<Tier> {
        self.listen
    }
}

impl Radio for SimRadio {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        self.tx_end_us = self.now_us + u64::from(tier.airtime_us(frame.len()));
        self.listen = None;
        self.kill_rx(Outcome::HalfDuplex);
        self.outbox.push((tier, frame.to_vec()));
    }

    fn listen(&mut self, tier: Tier) {
        if self.listen != Some(tier) {
            self.kill_rx(Outcome::NotListening);
        }
        self.listen = Some(tier);
    }

    fn busy(&self) -> bool {
        self.transmitting()
            || self
                .rx
                .iter()
                .any(|r| r.alive && r.pre_end_us <= self.now_us && self.now_us < r.end_us)
    }
}

#[derive(Default)]
struct Host {
    screen: Vec<u8>,
    /// Data segments `[start, end)` in `screen`; `None` = still open.
    segs: Vec<(usize, Option<usize>)>,
    /// Where the next CONNECT search starts.
    scan: usize,
    plus_run: u8,
    plus_pending: bool,
}

const CONNECT_PAT: &[u8] = b"\r\nCONNECT ";
const NO_CARRIER_PAT: &[u8] = b"\r\nNO CARRIER\r\n";
const OK_PAT: &[u8] = b"\r\nOK\r\n";
const RETRAIN_PAT: &[u8] = b"\r\nRETRAIN ";
/// `send_*_data` gives up after this long without progress.
const STALL_MS: u32 = 120_000;

fn find(hay: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let h = hay.get(from..)?;
    h.windows(pat.len())
        .position(|w| w == pat)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let h = hay.get(from..)?;
    h.windows(pat.len())
        .rposition(|w| w == pat)
        .map(|p| p + from)
}

impl Host {
    fn open(&self) -> Option<usize> {
        match self.segs.last() {
            Some((s, None)) => Some(*s),
            _ => None,
        }
    }

    fn close(&mut self, at: usize) {
        if let Some(last) = self.segs.last_mut() {
            last.1 = Some(at);
        }
        self.scan = at;
        self.plus_pending = false;
        self.plus_run = 0;
    }

    /// Track data segments after `chunk_start..` was appended.
    fn update(&mut self, chunk_start: usize, connected: bool, was_tick: bool) {
        for _ in 0..4 {
            if let Some(start) = self.open() {
                let esc_ok = if self.plus_pending && was_tick {
                    self.escape_ok(chunk_start.max(start))
                } else {
                    None
                };
                if let Some(at) = esc_ok {
                    self.close(at);
                } else if !connected {
                    let at =
                        rfind(&self.screen, NO_CARRIER_PAT, start).unwrap_or(self.screen.len());
                    self.close(at);
                } else {
                    return;
                }
            }
            let Some(c) = find(&self.screen, CONNECT_PAT, self.scan) else {
                return;
            };
            let Some(eol) = find(&self.screen, b"\r\n", c + 2) else {
                return;
            };
            let start = eol + 2;
            self.segs.push((start, None));
            self.scan = start;
        }
    }

    /// The escape's OK in the output from `from`: the last `\r\nOK\r\n`,
    /// followed by nothing or by one `\r\nRETRAIN ...\r\n` line up to the end
    /// (what Modem::tick prints when it fires the escape). Bytes before it in
    /// the same output are still data.
    fn escape_ok(&self, from: usize) -> Option<usize> {
        let at = rfind(&self.screen, OK_PAT, from)?;
        let rest = self.screen.get(at + OK_PAT.len()..)?;
        let tail_ok = rest.is_empty()
            || (rest.starts_with(RETRAIN_PAT)
                && find(rest, b"\r\n", 2) == Some(rest.len().saturating_sub(2)));
        tail_ok.then_some(at)
    }

    fn fed(&mut self, bytes: &[u8]) {
        if self.open().is_none() {
            self.plus_run = 0;
            self.plus_pending = false;
            return;
        }
        for &b in bytes {
            self.plus_run = if b == b'+' {
                self.plus_run.saturating_add(1)
            } else {
                0
            };
        }
        self.plus_pending = self.plus_run >= 3;
    }

    fn received(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for &(s, e) in &self.segs {
            let e = e.unwrap_or(self.screen.len());
            v.extend_from_slice(self.screen.get(s..e).unwrap_or(&[]));
        }
        v
    }
}

/// What the AXIS telnet server sends on connect: `IAC WILL ECHO`,
/// `IAC WILL SGA`, `IAC DO SGA`, `IAC WILL BINARY`, `IAC DO BINARY`,
/// `IAC DO NAWS`.
pub const AXIS_OPENING: &[u8] = &[
    255, 251, 1, 255, 251, 3, 255, 253, 3, 255, 251, 0, 255, 253, 0, 255, 253, 31,
];

/// How a `FakeWire` server behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireScript {
    /// Every connect fails at once (`wire_failed`).
    Refuse,
    /// Connects never complete (the modem's S7 timer ends the dial).
    NeverAnswer,
    /// Echoes every byte it receives.
    Echo,
    /// Sends these bytes on connect, then echoes.
    Banner(&'static [u8]),
    /// Echoes, and closes once it has received this many bytes on the
    /// connection (after the echo has been delivered).
    CloseAfter(usize),
    /// AXIS telnet server: sends `AXIS_OPENING` on connect, then echoes the
    /// data it receives with telnet commands stripped (0xFF goes back as
    /// `IAC IAC`). `received` keeps the raw bytes, negotiation included.
    Telnet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WireConn {
    Closed,
    /// Connect completes at this time (ms).
    Connecting(u32),
    /// NeverAnswer: the connect hangs.
    Hanging,
    Open,
}

/// Server side telnet filter state (what AXIS does with incoming bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SrvTn {
    Data,
    Iac,
    Verb,
    Sb,
    SbIac,
}

/// Fake platform plus TCP server for one harness modem (see module docs).
pub struct FakeWire {
    pub script: WireScript,
    /// Time from request to `wire_connected`, ms (default 0: next step).
    pub connect_ms: u32,
    conn: WireConn,
    requests: Vec<(String, u16)>,
    received: Vec<u8>,
    /// Bytes received on the current connection.
    conn_rx: usize,
    to_modem: VecDeque<u8>,
    closes_asked: u32,
    close_now: bool,
    tn: SrvTn,
}

impl FakeWire {
    pub fn new(script: WireScript) -> FakeWire {
        FakeWire {
            script,
            connect_ms: 0,
            conn: WireConn::Closed,
            requests: Vec::new(),
            received: Vec::new(),
            conn_rx: 0,
            to_modem: VecDeque::new(),
            closes_asked: 0,
            close_now: false,
            tn: SrvTn::Data,
        }
    }

    /// Every connection request seen, in order: (host, port).
    pub fn requests(&self) -> &[(String, u16)] {
        &self.requests
    }

    /// Every byte the server received, over all connections, raw.
    pub fn received(&self) -> &[u8] {
        &self.received
    }

    /// Times the modem asked for the socket to be closed.
    pub fn closes_asked(&self) -> u32 {
        self.closes_asked
    }

    pub fn is_open(&self) -> bool {
        self.conn == WireConn::Open
    }

    /// Queue bytes for the modem now (while open).
    pub fn send(&mut self, bytes: &[u8]) {
        self.to_modem.extend(bytes.iter().copied());
    }

    /// Close from the server side at the next service.
    pub fn close(&mut self) {
        self.close_now = true;
    }

    fn echo(&mut self, bytes: &[u8]) {
        match self.script {
            WireScript::Echo | WireScript::Banner(_) | WireScript::CloseAfter(_) => {
                self.to_modem.extend(bytes.iter().copied());
            }
            WireScript::Telnet => {
                for &b in bytes {
                    self.tn = match (self.tn, b) {
                        (SrvTn::Data, 255) => SrvTn::Iac,
                        (SrvTn::Data, _) => {
                            self.to_modem.push_back(b);
                            SrvTn::Data
                        }
                        (SrvTn::Iac, 255) => {
                            self.to_modem.extend([255, 255]);
                            SrvTn::Data
                        }
                        (SrvTn::Iac, 251..=254) => SrvTn::Verb,
                        (SrvTn::Iac, 250) => SrvTn::Sb,
                        (SrvTn::Iac, _) | (SrvTn::Verb, _) => SrvTn::Data,
                        (SrvTn::Sb, 255) => SrvTn::SbIac,
                        (SrvTn::Sb, _) => SrvTn::Sb,
                        (SrvTn::SbIac, 240) => SrvTn::Data,
                        (SrvTn::SbIac, _) => SrvTn::Sb,
                    };
                }
            }
            WireScript::Refuse | WireScript::NeverAnswer => {}
        }
    }

    fn read_out(&mut self, m: &mut Modem) {
        let mut buf = [0u8; 256];
        loop {
            let n = m.wire_output(&mut buf);
            if n == 0 {
                break;
            }
            let got = buf.get(..n).unwrap_or(&[]).to_vec();
            self.received.extend_from_slice(&got);
            self.conn_rx += n;
            if self.conn == WireConn::Open {
                self.echo(&got);
            }
        }
    }

    fn shut(&mut self) {
        self.conn = WireConn::Closed;
        self.to_modem.clear();
        self.close_now = false;
    }

    fn service(&mut self, m: &mut Modem, now: u32) {
        if m.take_wire_close() {
            self.closes_asked += 1;
            if self.conn == WireConn::Open {
                self.read_out(m);
            }
            self.shut();
        }
        if let Some(req) = m.take_wire_request() {
            self.requests.push((req.host().to_string(), req.port()));
            self.shut();
            self.conn_rx = 0;
            self.tn = SrvTn::Data;
            match self.script {
                WireScript::Refuse => m.wire_failed(now),
                WireScript::NeverAnswer => self.conn = WireConn::Hanging,
                _ => self.conn = WireConn::Connecting(now.wrapping_add(self.connect_ms)),
            }
        }
        if let WireConn::Connecting(due) = self.conn {
            if now >= due {
                self.conn = WireConn::Open;
                m.wire_connected(now);
                match self.script {
                    WireScript::Banner(b) => self.send(b),
                    WireScript::Telnet => self.send(AXIS_OPENING),
                    _ => {}
                }
            }
        }
        if self.conn != WireConn::Open {
            return;
        }
        self.read_out(m);
        if !self.to_modem.is_empty() {
            let v: Vec<u8> = self.to_modem.iter().copied().collect();
            let n = m.wire_input(&v, now);
            self.to_modem.drain(..n.min(v.len()));
        }
        let limit_hit = matches!(self.script, WireScript::CloseAfter(n) if self.conn_rx >= n);
        if self.close_now || (limit_hit && self.to_modem.is_empty()) {
            self.shut();
            m.wire_closed(now);
        }
    }
}

/// Two modems on one simulated channel.
pub struct Harness {
    /// CALLER (radio 0).
    pub a: Modem,
    /// AXIS (radio 1), auto-answers (S0=1).
    pub b: Modem,
    /// Simulated time. Read only: advance it with `run_ms`.
    pub now_ms: u32,
    radios: [SimRadio; 2],
    chan: SimChannel,
    rng: Rng,
    hosts: [Host; 2],
    trace: Vec<TraceEntry>,
    stats: Stats,
    /// Per sender: (call id, next new sequence number) for resend counting.
    seqs: [Option<(u8, u8)>; 2],
    /// Fake wire per modem, serviced each step when attached.
    wires: [Option<FakeWire>; 2],
}

impl Harness {
    /// CALLER and AXIS with default settings, AXIS with S0=1.
    pub fn new(chan: SimChannel) -> Harness {
        let a = Config::new("CALLER").expect("valid name");
        let mut b = Config::new("AXIS").expect("valid name");
        b.sregs.set(0, 1);
        Harness::with_configs(chan, a, b)
    }

    pub fn with_configs(chan: SimChannel, a: Config, b: Config) -> Harness {
        let rng = Rng::new(chan.seed);
        Harness {
            a: Modem::new(a),
            b: Modem::new(b),
            now_ms: 0,
            radios: [SimRadio::new(), SimRadio::new()],
            chan,
            rng,
            hosts: [Host::default(), Host::default()],
            trace: Vec::new(),
            stats: Stats::default(),
            seqs: [None, None],
            wires: [None, None],
        }
    }

    // Wire side.

    /// Attach a fake TCP server (and platform) to CALLER's Wi-Fi side.
    pub fn attach_wire_a(&mut self, w: FakeWire) {
        self.wires[0] = Some(w);
    }

    /// Attach a fake TCP server (and platform) to AXIS's Wi-Fi side.
    pub fn attach_wire_b(&mut self, w: FakeWire) {
        self.wires[1] = Some(w);
    }

    pub fn wire_a(&self) -> Option<&FakeWire> {
        self.wires[0].as_ref()
    }

    pub fn wire_b(&self) -> Option<&FakeWire> {
        self.wires[1].as_ref()
    }

    pub fn wire_a_mut(&mut self) -> Option<&mut FakeWire> {
        self.wires[0].as_mut()
    }

    pub fn wire_b_mut(&mut self) -> Option<&mut FakeWire> {
        self.wires[1].as_mut()
    }

    /// Raw bytes CALLER's fake server received (empty if none attached).
    pub fn wire_received_a(&self) -> Vec<u8> {
        self.wire_a()
            .map_or_else(Vec::new, |w| w.received().to_vec())
    }

    /// Raw bytes AXIS's fake server received (empty if none attached).
    pub fn wire_received_b(&self) -> Vec<u8> {
        self.wire_b()
            .map_or_else(Vec::new, |w| w.received().to_vec())
    }

    fn service_wire(&mut self, side: usize) {
        let Some(mut w) = self.wires.get_mut(side).and_then(Option::take) else {
            return;
        };
        self.with(side, false, |m, _r, now| w.service(m, now));
        if let Some(slot) = self.wires.get_mut(side) {
            *slot = Some(w);
        }
    }

    // Host side.

    /// Types `s` on CALLER's terminal as given (no implicit `\r`).
    /// Same as `send_a_data`; returns the bytes accepted (all of them).
    pub fn type_a(&mut self, s: &str) -> usize {
        self.send_data(0, s.as_bytes())
    }

    pub fn type_b(&mut self, s: &str) -> usize {
        self.send_data(1, s.as_bytes())
    }

    /// Feeds all bytes to CALLER, advancing time 1 ms whenever `host_input`
    /// takes less than offered. Returns the bytes accepted (always all).
    ///
    /// # Panics
    /// If the modem accepts nothing for 120 simulated seconds: a test
    /// harness should fail loudly rather than hang or drop bytes silently.
    pub fn send_a_data(&mut self, data: &[u8]) -> usize {
        self.send_data(0, data)
    }

    pub fn send_b_data(&mut self, data: &[u8]) -> usize {
        self.send_data(1, data)
    }

    /// One `host_input` call on CALLER, no time advance. Returns bytes taken.
    pub fn feed_a(&mut self, data: &[u8]) -> usize {
        self.feed(0, data)
    }

    pub fn feed_b(&mut self, data: &[u8]) -> usize {
        self.feed(1, data)
    }

    /// Everything CALLER's host has received (lossy UTF-8).
    pub fn screen_a(&self) -> String {
        String::from_utf8_lossy(&self.hosts[0].screen).into_owned()
    }

    pub fn screen_b(&self) -> String {
        String::from_utf8_lossy(&self.hosts[1].screen).into_owned()
    }

    pub fn screen_a_bytes(&self) -> &[u8] {
        &self.hosts[0].screen
    }

    pub fn screen_b_bytes(&self) -> &[u8] {
        &self.hosts[1].screen
    }

    /// Call data CALLER's host received in data mode (see module docs).
    pub fn received_a(&self) -> Vec<u8> {
        self.hosts[0].received()
    }

    /// Call data AXIS's host received in data mode (see module docs).
    pub fn received_b(&self) -> Vec<u8> {
        self.hosts[1].received()
    }

    // Time.

    pub fn run_ms(&mut self, ms: u32) {
        for _ in 0..ms {
            self.step();
        }
    }

    /// Runs until `cond` holds (checked after each 1 ms step) or `max_ms`
    /// passes. True if `cond` held.
    pub fn run_until(&mut self, max_ms: u32, cond: impl Fn(&Harness) -> bool) -> bool {
        if cond(self) {
            return true;
        }
        for _ in 0..max_ms {
            self.step();
            if cond(self) {
                return true;
            }
        }
        false
    }

    // Channel.

    /// Hands a frame straight to CALLER's `radio_rx` as if heard on `tier`
    /// now, with the current profile SNR. Bypasses the air.
    pub fn inject_a(&mut self, frame: &[u8], tier: Tier) {
        self.inject(0, frame, tier);
    }

    pub fn inject_b(&mut self, frame: &[u8], tier: Tier) {
        self.inject(1, frame, tier);
    }

    pub fn snr_x4_now(&self) -> i16 {
        self.chan.profile.at(self.now_ms)
    }

    /// Every frame sent so far, in send order.
    pub fn trace(&self) -> &[TraceEntry] {
        &self.trace
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Data segments `[start, end)` of a host's screen bytes (0 = CALLER,
    /// 1 = AXIS); an open segment ends at the current end of the screen.
    pub fn segments(&self, side: usize) -> Vec<(usize, usize)> {
        self.hosts.get(side).map_or_else(Vec::new, |h| {
            h.segs
                .iter()
                .map(|&(s, e)| (s, e.unwrap_or(h.screen.len())))
                .collect()
        })
    }

    /// Radio 0 (CALLER) or 1 (AXIS).
    pub fn radio(&self, side: usize) -> Option<&SimRadio> {
        self.radios.get(side)
    }

    // Internals.

    fn send_data(&mut self, side: usize, data: &[u8]) -> usize {
        let mut used = 0;
        let mut stall = 0;
        while used < data.len() {
            let n = self.feed(side, data.get(used..).unwrap_or(&[]));
            used += n;
            if used >= data.len() {
                break;
            }
            stall = if n == 0 { stall + 1 } else { 0 };
            assert!(
                stall < STALL_MS,
                "sim stall: side {side} accepted nothing for {STALL_MS} ms at t={} ms, {used} of {} bytes taken",
                self.now_ms,
                data.len()
            );
            self.step();
        }
        used
    }

    fn feed(&mut self, side: usize, data: &[u8]) -> usize {
        let mut n = 0;
        self.with(side, false, |m, r, now| n = m.host_input(data, r, now));
        if let Some(h) = self.hosts.get_mut(side) {
            h.fed(data.get(..n).unwrap_or(&[]));
        }
        n
    }

    fn inject(&mut self, side: usize, frame: &[u8], tier: Tier) {
        let info = self.rx_info(tier);
        self.with(side, false, |m, r, now| m.radio_rx(frame, info, r, now));
    }

    fn rx_info(&self, tier: Tier) -> RxInfo {
        let snr_x4 = self.snr_x4_now();
        RxInfo {
            tier,
            rssi_dbm: (-120i16).saturating_add(snr_x4 / 4),
            snr_x4,
        }
    }

    fn tick(&mut self, side: usize) {
        self.with(side, true, |m, r, now| m.tick(r, now));
    }

    /// Runs `f` on one modem and its radio at `now`, then puts sent frames
    /// on the air and collects host output.
    fn with(
        &mut self,
        side: usize,
        was_tick: bool,
        f: impl FnOnce(&mut Modem, &mut SimRadio, u32),
    ) {
        let now = self.now_ms;
        let [ra, rb] = &mut self.radios;
        let (m, r) = if side == 0 {
            (&mut self.a, ra)
        } else {
            (&mut self.b, rb)
        };
        r.now_us = u64::from(now) * 1000;
        f(m, r, now);
        let out = std::mem::take(&mut r.outbox);
        for (tier, bytes) in out {
            self.start_frame(side, tier, bytes);
        }
        self.collect(side, was_tick);
    }

    fn collect(&mut self, side: usize, was_tick: bool) {
        let m = if side == 0 { &mut self.a } else { &mut self.b };
        let connected = m.is_connected();
        let Some(h) = self.hosts.get_mut(side) else {
            return;
        };
        let chunk_start = h.screen.len();
        let mut buf = [0u8; 256];
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            h.screen.extend_from_slice(buf.get(..n).unwrap_or(&[]));
        }
        h.update(chunk_start, connected, was_tick);
    }

    fn step(&mut self) {
        let prev_us = u64::from(self.now_ms) * 1000;
        self.now_ms = self.now_ms.wrapping_add(1);
        let now_us = u64::from(self.now_ms) * 1000;
        for r in self.radios.iter_mut() {
            r.now_us = now_us;
        }
        for side in 0..2 {
            let end = self.radios[side].tx_end_us;
            if end > prev_us && end <= now_us {
                self.tick(side);
            }
        }
        self.deliver_due(now_us);
        if self.now_ms.is_multiple_of(10) {
            self.tick(0);
            self.tick(1);
        }
        for side in 0..2 {
            if self.wires[side].is_some() {
                self.service_wire(side);
            }
        }
    }

    fn deliver_due(&mut self, now_us: u64) {
        loop {
            let mut next: Option<(u64, usize, usize)> = None;
            for (side, r) in self.radios.iter().enumerate() {
                for (i, rx) in r.rx.iter().enumerate() {
                    if rx.end_us <= now_us && next.is_none_or(|n| rx.end_us < n.0) {
                        next = Some((rx.end_us, side, i));
                    }
                }
            }
            let Some((_, side, i)) = next else { return };
            let rx = self.radios[side].rx.remove(i);
            let mut outcome = rx.outcome;
            if rx.alive && outcome == Outcome::Pending {
                let snr = self.snr_x4_now();
                let margin = snr.saturating_sub(rx.tier.snr_floor_x4());
                let lost = self.rng.chance(loss_x1000(margin));
                outcome = if rx.hooked {
                    Outcome::Dropped
                } else if lost {
                    Outcome::Lost
                } else {
                    Outcome::Delivered
                };
            }
            self.finish(rx.trace, outcome);
            if outcome == Outcome::Delivered {
                let info = self.rx_info(rx.tier);
                self.with(side, false, |m, r, now| m.radio_rx(&rx.bytes, info, r, now));
            }
        }
    }

    fn finish(&mut self, trace: usize, outcome: Outcome) {
        if let Some(t) = self.trace.get_mut(trace) {
            t.outcome = outcome;
        }
        let s = &mut self.stats;
        match outcome {
            Outcome::Delivered => s.delivered += 1,
            Outcome::Lost => s.lost += 1,
            Outcome::Undetected => s.undetected += 1,
            Outcome::Collided => s.collided += 1,
            Outcome::HalfDuplex => s.half_duplex += 1,
            Outcome::NotListening => s.not_listening += 1,
            Outcome::Dropped => s.dropped += 1,
            Outcome::Pending => {}
        }
    }

    fn start_frame(&mut self, from: usize, tier: Tier, bytes: Vec<u8>) {
        let now = self.now_ms;
        let now_us = u64::from(now) * 1000;
        let end_us = now_us + u64::from(tier.airtime_us(bytes.len()));
        let (ftype, flags, call_id, seq) = match decode(&bytes) {
            Ok(f) => (
                Some(f.header.ftype),
                f.header.flags,
                f.header.call_id,
                f.header.seq,
            ),
            Err(_) => (None, 0, 0, 0),
        };
        let is_data = ftype == Some(FrameType::Data);

        let mut resend = false;
        if let Some(st) = self.seqs.get_mut(from) {
            if is_data {
                match *st {
                    Some((c, next)) if c == call_id => {
                        if seq.wrapping_sub(next) < 128 {
                            *st = Some((c, seq.wrapping_add(1)));
                        } else {
                            resend = true;
                        }
                    }
                    _ => *st = Some((call_id, seq.wrapping_add(1))),
                }
            }
        }
        let s = &mut self.stats;
        if let Some(n) = s.frames_sent.get_mut(from) {
            *n += 1;
        }
        if is_data {
            if let Some(n) = s.data_frames.get_mut(from) {
                *n += 1;
            }
            if resend {
                if let Some(n) = s.resends.get_mut(from) {
                    *n += 1;
                }
            }
        }
        if let Some(log) = s.tiers.get_mut(from) {
            let in_call = matches!(
                ftype,
                Some(FrameType::Data | FrameType::Ack | FrameType::Keepalive)
            );
            if in_call && log.last().map(|t| t.1) != Some(tier) {
                log.push((now, tier));
            }
        }

        let hooked = self.chan.drop.is_some_and(|f| f(now, &bytes));
        let trace = self.trace.len();
        self.trace.push(TraceEntry {
            t_ms: now,
            end_us,
            from,
            tier,
            ftype,
            flags,
            call_id,
            seq,
            len: bytes.len(),
            resend,
            outcome: Outcome::Pending,
        });

        let to = 1 - from.min(1);
        let snr = self.chan.profile.at(now);
        let margin = snr.saturating_sub(tier.snr_floor_x4());
        let r = &mut self.radios[to];
        r.now_us = now_us;
        let early = if r.transmitting() {
            Some(Outcome::HalfDuplex)
        } else if r.listen != Some(tier) {
            Some(Outcome::NotListening)
        } else if margin < -8 {
            Some(Outcome::Undetected)
        } else {
            None
        };
        if let Some(o) = early {
            self.finish(trace, o);
            return;
        }
        let mut collided = false;
        for other in r.rx.iter_mut().filter(|x| x.alive && x.end_us > now_us) {
            collided = true;
            if other.outcome == Outcome::Pending {
                other.outcome = Outcome::Collided;
            }
        }
        r.rx.push(Rx {
            trace,
            tier,
            bytes,
            pre_end_us: now_us + preamble_us(tier),
            end_us,
            alive: true,
            hooked,
            outcome: if collided {
                Outcome::Collided
            } else {
                Outcome::Pending
            },
        });
    }
}
