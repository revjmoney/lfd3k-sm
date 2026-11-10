//! Simulated radio over datagrams (UDP on the V4 and on the PC), for calling
//! the one real V4 from a PC that runs the same core.
//!
//! Datagram (port 4210 at both ends): `'L' 'S' 1 tier snr_x4(i16 LE)
//! rssi(i16 LE)` then the frame bytes (1 to 255). The sender fills in the
//! tier and the SNR and RSSI the receiver should "hear".
//!
//! Timing, against the `Radio` contract in `radio.rs`:
//! - TX: `send` puts the datagram on the wire at once and stays `busy()` for
//!   `Tier::airtime_us(len)`; `service` ends the transmission when that time
//!   has passed (it must be polled, as for the SX1262 driver).
//! - RX: a datagram arrives whole. The receiver holds it for its airtime from
//!   arrival (`busy()` true, like a receive after preamble detection) and
//!   `service` hands it over at arrival plus airtime, with `RxInfo` from the
//!   header. So end to end a frame takes one airtime plus the network delay,
//!   as on the air, and the peer goes busy shortly after the sender starts,
//!   as a real receiver does on the preamble. (Holding at the sender too
//!   would make every frame take two airtimes, and the core's turn and probe
//!   timers, which assume one, then fire before the reply is even heard on
//!   SF9 to SF12.)
//! - Half duplex: a datagram that arrives while transmitting is lost, and
//!   `send` drops everything held. One for a tier other than the one being
//!   listened on is dropped on arrival (a wrong SF is not heard). One that
//!   arrives in the short gap after a transmission, before the core has
//!   called `listen` again, is held; it counts as busy, and is delivered,
//!   only while the radio listens on its tier (the core calls `listen` as
//!   soon as the transmission ends, because the radio is not busy then).
//! - Held datagrams are received one after the other (each from the later of
//!   its arrival and the end of the one before); there are no collisions.
//!   At most `RX_Q` are held; more are dropped.
//!
//! no_std, no heap, fixed buffers, never panics.

use crate::radio::{Radio, RxInfo};
use crate::tier::Tier;

/// UDP port used at both ends.
pub const PORT: u16 = 4210;
/// Header bytes before the frame.
pub const HDR_LEN: usize = 8;
/// Longest frame carried.
pub const FRAME_MAX: usize = 255;
/// Longest datagram.
pub const DGRAM_MAX: usize = HDR_LEN + FRAME_MAX;
/// Header version byte.
pub const VERSION: u8 = 1;
/// Datagrams held at once.
const RX_Q: usize = 2;
/// RSSI floor reported by `rx_rssi` when nothing is being received.
pub const RSSI_FLOOR: i16 = -110;

/// Moves whole datagrams to the peer and tells the time.
pub trait DatagramPort {
    /// Send one datagram to the peer. Must not block.
    fn send(&mut self, d: &[u8]);
    /// Monotonic microseconds.
    fn now_us(&self) -> u64;
}

#[derive(Clone, Copy)]
struct Held {
    buf: [u8; FRAME_MAX],
    len: usize,
    info: RxInfo,
    start_us: u64,
    end_us: u64,
}

const EMPTY: Held = Held {
    buf: [0; FRAME_MAX],
    len: 0,
    info: RxInfo {
        tier: Tier::ROBUST,
        rssi_dbm: 0,
        snr_x4: 0,
    },
    start_us: 0,
    end_us: 0,
};

/// A `Radio` that sends and receives frames as datagrams through `T`.
pub struct SimNetRadio<T: DatagramPort> {
    port: T,
    /// Transmitting until this time.
    tx_end_us: Option<u64>,
    listen: Option<Tier>,
    rx: [Held; RX_Q],
    rx_head: usize,
    rx_len: usize,
    /// What the peer should hear of us (header fields of what we send).
    peer_snr_x4: i16,
    peer_rssi: i16,
}

/// Parses a datagram: (tier, snr_x4, rssi, frame).
pub fn parse(d: &[u8]) -> Option<(Tier, i16, i16, &[u8])> {
    let h: [u8; HDR_LEN] = d.get(..HDR_LEN)?.try_into().ok()?;
    let [m0, m1, ver, tier, s0, s1, r0, r1] = h;
    if m0 != b'L' || m1 != b'S' || ver != VERSION {
        return None;
    }
    let tier = Tier::from_index(tier)?;
    let frame = d.get(HDR_LEN..)?;
    if frame.is_empty() || frame.len() > FRAME_MAX {
        return None;
    }
    Some((
        tier,
        i16::from_le_bytes([s0, s1]),
        i16::from_le_bytes([r0, r1]),
        frame,
    ))
}

/// Builds a datagram into `out`; returns its length (0 if the frame is
/// empty or too long).
pub fn build(tier: Tier, snr_x4: i16, rssi: i16, frame: &[u8], out: &mut [u8; DGRAM_MAX]) -> usize {
    if frame.is_empty() || frame.len() > FRAME_MAX {
        return 0;
    }
    let [s0, s1] = snr_x4.to_le_bytes();
    let [r0, r1] = rssi.to_le_bytes();
    let hdr = [b'L', b'S', VERSION, tier.index(), s0, s1, r0, r1];
    let n = HDR_LEN + frame.len();
    if let Some(h) = out.get_mut(..HDR_LEN) {
        h.copy_from_slice(&hdr);
    }
    match out.get_mut(HDR_LEN..n) {
        Some(d) => d.copy_from_slice(frame),
        None => return 0,
    }
    n
}

impl<T: DatagramPort> SimNetRadio<T> {
    /// Idle (not listening), the peer hears +8 dB at -70 dBm.
    pub const fn new(port: T) -> Self {
        SimNetRadio {
            port,
            tx_end_us: None,
            listen: None,
            rx: [EMPTY; RX_Q],
            rx_head: 0,
            rx_len: 0,
            peer_snr_x4: 32,
            peer_rssi: -70,
        }
    }

    pub fn port(&self) -> &T {
        &self.port
    }

    pub fn port_mut(&mut self) -> &mut T {
        &mut self.port
    }

    /// The SNR (quarter dB) and RSSI written into what we send: what the
    /// peer should hear.
    pub fn set_peer_hears(&mut self, snr_x4: i16, rssi: i16) {
        self.peer_snr_x4 = snr_x4;
        self.peer_rssi = rssi;
    }

    /// Back to idle: nothing held, not transmitting, not listening.
    pub fn stop(&mut self) {
        self.tx_end_us = None;
        self.listen = None;
        self.rx_len = 0;
        self.rx_head = 0;
    }

    /// True while a transmission is in progress.
    pub fn transmitting(&self) -> bool {
        self.tx_end_us.is_some_and(|e| self.port.now_us() < e)
    }

    fn front(&self) -> Option<&Held> {
        if self.rx_len == 0 {
            None
        } else {
            self.rx.get(self.rx_head)
        }
    }

    fn pop(&mut self) {
        if self.rx_len > 0 {
            self.rx_head = (self.rx_head + 1) % RX_Q;
            self.rx_len -= 1;
        }
    }

    /// The held datagram being received now, if the radio listens on its tier.
    fn receiving(&self) -> Option<&Held> {
        let now = self.port.now_us();
        self.front()
            .filter(|h| self.listen == Some(h.info.tier) && h.start_us <= now && now < h.end_us)
    }

    /// RSSI for the channel trace: that of the frame being received, else
    /// the floor (-110 dBm).
    pub fn rx_rssi(&self) -> i16 {
        self.receiving().map_or(RSSI_FLOOR, |h| h.info.rssi_dbm)
    }

    /// A datagram from the peer. Malformed ones, wrong-tier ones, any while
    /// transmitting, and any beyond the queue are dropped.
    pub fn on_datagram(&mut self, d: &[u8]) {
        let Some((tier, snr_x4, rssi, frame)) = parse(d) else {
            return;
        };
        if self.transmitting() || self.rx_len >= RX_Q {
            return;
        }
        if self.listen.is_some_and(|t| t != tier) {
            return;
        }
        let now = self.port.now_us();
        let prev_end = if self.rx_len == 0 {
            now
        } else {
            let last = (self.rx_head + self.rx_len - 1) % RX_Q;
            self.rx.get(last).map_or(now, |h| h.end_us)
        };
        let start = now.max(prev_end);
        let slot = (self.rx_head + self.rx_len) % RX_Q;
        let Some(h) = self.rx.get_mut(slot) else {
            return;
        };
        h.len = frame.len();
        if let Some(b) = h.buf.get_mut(..frame.len()) {
            b.copy_from_slice(frame);
        }
        h.info = RxInfo {
            tier,
            rssi_dbm: rssi,
            snr_x4,
        };
        h.start_us = start;
        h.end_us = start.saturating_add(u64::from(tier.airtime_us(frame.len())));
        self.rx_len += 1;
    }

    /// Poll often (every tick): ends a finished transmission and returns a
    /// held frame whose airtime has passed.
    pub fn service(&mut self, buf: &mut [u8; 255]) -> Option<(usize, RxInfo)> {
        let now = self.port.now_us();
        if self.tx_end_us.is_some_and(|e| now >= e) {
            self.tx_end_us = None;
        }
        while let Some(h) = self.front() {
            if now < h.end_us {
                return None;
            }
            let (len, info) = (h.len, h.info);
            let heard = self.listen == Some(info.tier);
            if heard {
                if let (Some(d), Some(s)) = (buf.get_mut(..len), h.buf.get(..len)) {
                    d.copy_from_slice(s);
                }
            }
            self.pop();
            if heard {
                return Some((len, info));
            }
        }
        None
    }
}

impl<T: DatagramPort> Radio for SimNetRadio<T> {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        if frame.is_empty() || frame.len() > FRAME_MAX {
            return;
        }
        let mut d = [0u8; DGRAM_MAX];
        let n = build(tier, self.peer_snr_x4, self.peer_rssi, frame, &mut d);
        // Half duplex: nothing held survives our own transmission.
        self.rx_len = 0;
        self.rx_head = 0;
        self.listen = None;
        let now = self.port.now_us();
        self.tx_end_us = Some(now.saturating_add(u64::from(tier.airtime_us(frame.len()))));
        self.port.send(d.get(..n).unwrap_or(&[]));
    }

    fn listen(&mut self, tier: Tier) {
        if self.listen.is_some_and(|t| t != tier) {
            self.rx_len = 0;
            self.rx_head = 0;
        }
        // Held frames on another tier (from the gap after a send) are not
        // heard; `service` drops them when their time comes.
        self.listen = Some(tier);
    }

    fn busy(&self) -> bool {
        if self.transmitting() {
            return true;
        }
        self.receiving().is_some()
    }
}
