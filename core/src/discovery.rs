//! Discovery on the calling channel: beacons, scans and their payloads.
//!
//! All three frames go out on SF12 with call id 0, seq 0, ack 0, flags 0.
//! Payloads (a name is a length byte 1 to 16 plus that many printable ASCII
//! bytes without `:`, the station name rule):
//!
//! - Beacon (type 12): name, services (u8), modems heard (u8), time quality
//!   (u8, 0 for now). 4 + name bytes; a 16 byte name gives a 25 byte frame,
//!   about 0.37 s on air at SF12 BW500.
//! - ScanReq (type 13): scanner name, scan id (u8). 2 + name bytes.
//! - ScanResp (type 14): responder name, services (u8), SNR the ScanReq was
//!   heard at (i16 dB x4, little endian), scan id (u8). 5 + name bytes.
//!
//! Every parser requires the exact length: trailing bytes are a bad payload.
//!
//! Services bits: bit 0 accepts calls (auto-answer S0 on), bit 1 a forward
//! is published (AT+PUB), bit 2 the Wi-Fi hotspot is on. Other bits are 0
//! when sent and ignored (masked off) when heard.
//!
//! The scheduler state (`Discovery`) sits in the `Modem`, which runs it from
//! `tick` (see `Modem::disc_tick`). Timing rules: a beacon every 300 s plus
//! 0 to 60 s, first one a full interval after start and again a full
//! interval after any call ends; scan answers in one of 11 slots of 250 ms
//! with listen before talk; at most one discovery transmission per 10 s.

use crate::frame::{valid_name, NAME_MAX};

/// Services bit 0: the modem answers calls by itself (S0 > 0).
pub const SVC_ACCEPTS: u8 = 1;
/// Services bit 1: a forward is published (AT+PUB).
pub const SVC_FORWARD: u8 = 2;
/// Services bit 2: the Wi-Fi hotspot is on.
pub const SVC_HOTSPOT: u8 = 4;
/// The services bits this build knows.
pub const SVC_MASK: u8 = SVC_ACCEPTS | SVC_FORWARD | SVC_HOTSPOT;

/// Beacon period and the most random delay added to it.
pub const BEACON_EVERY_MS: u32 = 300_000;
pub const BEACON_JITTER_MS: u32 = 60_000;
/// How long `AT+SCAN` collects answers after its ScanReq went out.
pub const SCAN_WINDOW_MS: u32 = 4_000;
/// Scan answer slots: 11 of 250 ms (0 to 2.5 s after the ScanReq).
pub const SLOT_MS: u32 = 250;
pub const SLOTS: u8 = 11;
/// At most one discovery transmission per this many ms.
pub const DISC_GAP_MS: u32 = 10_000;
/// A scan whose ScanReq could not go out within this time gives ERROR.
pub const SCAN_SEND_LIMIT_MS: u32 = DISC_GAP_MS + SCAN_WINDOW_MS;
/// Directory entries heard within this window count in a beacon.
pub const HEARD_WINDOW_MS: u32 = 900_000;
/// Longest discovery payload (ScanResp with a 16 byte name).
pub const DISC_MAX: usize = 1 + NAME_MAX + 4;

/// A name as discovery and the directory accept it: a station name
/// (1 to 16 printable ASCII) without `:`.
pub fn valid_station(n: &str) -> bool {
    valid_name(n) && !n.contains(':')
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Beacon<'a> {
    pub name: &'a str,
    pub services: u8,
    pub heard: u8,
    pub time_quality: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScanReq<'a> {
    pub name: &'a str,
    pub scan_id: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScanResp<'a> {
    pub name: &'a str,
    pub services: u8,
    pub snr_x4: i16,
    pub scan_id: u8,
}

/// Name then `tail`; the payload length, or None if the name is invalid or
/// `out` too small.
fn put(name: &str, tail: &[u8], out: &mut [u8]) -> Option<usize> {
    if !valid_station(name) {
        return None;
    }
    let n = name.len();
    let total = 1 + n + tail.len();
    let dst = out.get_mut(..total)?;
    let (l, rest) = dst.split_at_mut(1);
    let (nm, t) = rest.split_at_mut(n);
    l.copy_from_slice(&[n as u8]);
    nm.copy_from_slice(name.as_bytes());
    t.copy_from_slice(tail);
    Some(total)
}

/// Name and the bytes after it, if the name is valid and exactly `tail`
/// bytes follow.
fn take(p: &[u8], tail: usize) -> Option<(&str, &[u8])> {
    let (&l, rest) = p.split_first()?;
    let (nm, t) = rest.split_at_checked(usize::from(l))?;
    if t.len() != tail {
        return None;
    }
    let name = core::str::from_utf8(nm).ok()?;
    valid_station(name).then_some((name, t))
}

pub fn beacon_payload(b: &Beacon<'_>, out: &mut [u8]) -> Option<usize> {
    put(b.name, &[b.services, b.heard, b.time_quality], out)
}

pub fn parse_beacon(p: &[u8]) -> Option<Beacon<'_>> {
    match take(p, 3)? {
        (name, &[services, heard, time_quality]) => Some(Beacon {
            name,
            services,
            heard,
            time_quality,
        }),
        _ => None,
    }
}

pub fn scan_req_payload(r: &ScanReq<'_>, out: &mut [u8]) -> Option<usize> {
    put(r.name, &[r.scan_id], out)
}

pub fn parse_scan_req(p: &[u8]) -> Option<ScanReq<'_>> {
    match take(p, 1)? {
        (name, &[scan_id]) => Some(ScanReq { name, scan_id }),
        _ => None,
    }
}

pub fn scan_resp_payload(r: &ScanResp<'_>, out: &mut [u8]) -> Option<usize> {
    let s = r.snr_x4.to_le_bytes();
    put(r.name, &[r.services, s[0], s[1], r.scan_id], out)
}

pub fn parse_scan_resp(p: &[u8]) -> Option<ScanResp<'_>> {
    match take(p, 4)? {
        (name, &[services, a, b, scan_id]) => Some(ScanResp {
            name,
            services,
            snr_x4: i16::from_le_bytes([a, b]),
            scan_id,
        }),
        _ => None,
    }
}

/// A scan answer waiting for its slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Answer {
    /// When the ScanReq was heard.
    pub at: u32,
    /// Slot 0 to 10; the answer is due at `at + slot * SLOT_MS`.
    pub slot: u8,
    pub snr_x4: i16,
    pub scan_id: u8,
}

/// An `AT+SCAN` in progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Scan {
    pub id: u8,
    /// When AT+SCAN was typed.
    pub asked: u32,
    /// When the ScanReq went out; the 4 s window runs from here.
    pub sent: Option<u32>,
    /// Answers counted for this scan id.
    pub hits: u8,
}

/// Discovery scheduler state, owned by the `Modem`.
pub(crate) struct Discovery {
    /// xorshift32 state; 0 until `start` seeds it.
    rng: u32,
    pub started: bool,
    /// Next beacon: (armed at, wait).
    pub beacon: Option<(u32, u32)>,
    /// Last discovery transmission (any of the three types).
    pub last_tx: Option<u32>,
    pub answer: Option<Answer>,
    pub scan: Option<Scan>,
    /// The link or the wire was busy at the last tick.
    pub in_call: bool,
}

impl Discovery {
    pub const fn new() -> Self {
        Discovery {
            rng: 0,
            started: false,
            beacon: None,
            last_tx: None,
            answer: None,
            scan: None,
            in_call: false,
        }
    }

    /// First tick or frame: seed from the station name (FNV-1a) and the time.
    pub fn start(&mut self, name: &str, now: u32) {
        if self.started {
            return;
        }
        let mut h: u32 = 0x811C_9DC5;
        for b in name.bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        self.rng = (h ^ now.rotate_left(16)) | 1;
        self.started = true;
    }

    /// Uniform-ish in 0..=max.
    pub fn rand(&mut self, max: u32) -> u32 {
        let x = crate::link::xorshift32(self.rng | 1);
        self.rng = x;
        x % max.saturating_add(1)
    }

    /// Arm the next beacon one full interval (plus jitter) from now.
    pub fn arm_beacon(&mut self, now: u32) {
        let wait = BEACON_EVERY_MS + self.rand(BEACON_JITTER_MS);
        self.beacon = Some((now, wait));
    }

    /// The 10 s discovery spacing allows a transmission now.
    pub fn gap_ok(&self, now: u32) -> bool {
        self.last_tx
            .is_none_or(|t| now.wrapping_sub(t) >= DISC_GAP_MS)
    }
}
