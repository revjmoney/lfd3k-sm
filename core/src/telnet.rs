//! Telnet client filter for the wire side of a call (both directions).
//!
//! Incoming (wire to caller): a byte state machine strips every telnet
//! command, option negotiation and subnegotiation, turns `IAC IAC` into one
//! 0xFF and answers the server's requests:
//! - `WILL ECHO`, `WILL SGA`, `WILL BINARY` get `DO`;
//! - `DO SGA`, `DO BINARY` get `WILL`;
//! - `DO NAWS` gets `WILL NAWS` plus `SB NAWS 0 80 0 25 SE`;
//! - any other `WILL` gets `DONT`, any other `DO` gets `WONT`;
//! - `WONT` / `DONT` for an option that was on get `DONT` / `WONT`.
//!
//! Each option is answered once while its state does not change, so two
//! sides can never loop. Nothing is ever sent unprompted (the server speaks
//! first). Subnegotiations are dropped whole, however long, up to `IAC SE`
//! (nothing is buffered: no option we parse has a subnegotiation).
//! While the server does not send BINARY (no `WILL BINARY` from it), an
//! incoming CR NUL is the NVT bare CR and becomes CR.
//!
//! Replies wait in a small queue that goes out ahead of user data. The
//! caller only feeds a byte while `can_take` is true (room for the largest
//! reply), so no reply is ever lost.
//!
//! Outgoing (caller to wire): 0xFF is sent as `IAC IAC`. While our sending
//! direction is not BINARY (the server has not said `DO BINARY` to our
//! `WILL BINARY`), CR LF goes out as CR LF and any other CR as CR NUL
//! (RFC 854). `encode` returns the pair so the caller never splits it with a
//! reply; the caller decides what follows a CR (see `Wire::output`, which
//! holds a CR at the end of the data until the next byte or 50 ms).
//!
//! Raw mode passes both directions untouched.
//!
//! LFD3K-LINK (option 90, see `link_payload`): once the server has sent its
//! opening negotiation (at least one `WILL`/`WONT`/`DO`/`DONT`, and the
//! input batch that held it is done), the filter wants to offer
//! `IAC WILL 90` (`take_offer`, sent by `Wire::output` after the replies).
//! `IAC DO 90` turns the option on (`link_on`): only then does the modem send
//! `IAC SB 90 <stats> IAC SE` reports. `DONT 90` (or no answer at all) keeps
//! it off for the whole connection; a server that never negotiates is never
//! offered it, and raw mode never is.

use crate::settings::WireMode;

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const SE: u8 = 240;

pub const OPT_BINARY: u8 = 0;
pub const OPT_ECHO: u8 = 1;
pub const OPT_SGA: u8 = 3;
pub const OPT_NAWS: u8 = 31;
/// LFD3K-LINK: the modem's link stats for an AXIS BBS (unassigned by IANA).
pub const OPT_LINK: u8 = 90;
/// Longest LFD3K-LINK payload (what AXIS accepts).
pub const LINK_PAYLOAD_MAX: usize = 160;
/// Longest whole report frame: `IAC SB 90`, the payload, `IAC SE`.
pub const LINK_FRAME_MAX: usize = LINK_PAYLOAD_MAX + 5;

/// Terminal size sent for NAWS: 80 columns, 25 rows.
const NAWS_REPLY: [u8; 12] = [
    IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 80, 0, 25, IAC, SE,
];
/// Largest reply one incoming command can cause.
pub const REPLY_MAX: usize = NAWS_REPLY.len();
const RQ_CAP: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum In {
    Data,
    Iac,
    /// After `IAC WILL|WONT|DO|DONT`, waiting for the option byte.
    Verb(u8),
    /// Inside `IAC SB`: everything is dropped until `IAC SE`.
    Sb,
    /// `IAC` seen inside a subnegotiation.
    SbIac,
}

/// LFD3K-LINK option state for this connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LinkOpt {
    /// Not offered yet (the server has not negotiated anything yet).
    Idle,
    /// The opening is done: `IAC WILL 90` should go out.
    Offer,
    /// `IAC WILL 90` went out; waiting for DO or DONT.
    Offered,
    /// The server said `DO 90`: reports may be sent.
    On,
    /// Refused (`DONT 90`) or turned off: never offered again.
    Off,
}

/// Option state: what the server does (`him`) and what we do (`us`).
#[derive(Clone, Copy, Default)]
struct Opts {
    him_binary: bool,
    him_echo: bool,
    him_sga: bool,
    us_binary: bool,
    us_sga: bool,
    us_naws: bool,
}

pub struct Telnet {
    on: bool,
    st: In,
    o: Opts,
    rq: [u8; RQ_CAP],
    rq_head: usize,
    rq_len: usize,
    /// Last data byte was a CR from a server not sending BINARY.
    cr_seen: bool,
    /// The server has sent at least one option negotiation.
    neg_seen: bool,
    link: LinkOpt,
}

impl Default for Telnet {
    fn default() -> Self {
        Self::new()
    }
}

impl Opts {
    const OFF: Opts = Opts {
        him_binary: false,
        him_echo: false,
        him_sga: false,
        us_binary: false,
        us_sga: false,
        us_naws: false,
    };
}

impl Telnet {
    pub const fn new() -> Self {
        Telnet {
            on: false,
            st: In::Data,
            o: Opts::OFF,
            rq: [0; RQ_CAP],
            rq_head: 0,
            rq_len: 0,
            cr_seen: false,
            neg_seen: false,
            link: LinkOpt::Idle,
        }
    }

    /// LFD3K-LINK is on: the server answered `IAC DO 90`.
    pub fn link_on(&self) -> bool {
        self.on && self.link == LinkOpt::On
    }

    /// End of an input batch: once the server's opening negotiation has
    /// been seen (and no command is half read), the offer becomes due.
    pub fn opening_done(&mut self) {
        if self.on && self.neg_seen && self.st == In::Data && self.link == LinkOpt::Idle {
            self.link = LinkOpt::Offer;
        }
    }

    /// True once when `IAC WILL 90` should be sent now (the caller writes
    /// it whole, after every queued reply).
    pub fn take_offer(&mut self) -> bool {
        if self.on && self.link == LinkOpt::Offer {
            self.link = LinkOpt::Offered;
            true
        } else {
            false
        }
    }

    /// `IAC WILL 90` is waiting to go out.
    pub fn offer_due(&self) -> bool {
        self.on && self.link == LinkOpt::Offer
    }

    /// Start of a new connection.
    pub fn reset(&mut self, mode: WireMode) {
        *self = Self::new();
        self.on = mode == WireMode::Telnet;
    }

    pub fn is_telnet(&self) -> bool {
        self.on
    }

    /// BINARY agreed both ways.
    pub fn binary(&self) -> bool {
        self.o.us_binary && self.o.him_binary
    }

    /// Outgoing CR follows the NVT rule (CR LF or CR NUL): telnet mode and
    /// our sending direction is not BINARY.
    pub fn cr_rule(&self) -> bool {
        self.on && !self.o.us_binary
    }

    /// Room for the largest reply: the next incoming byte may be fed.
    pub fn can_take(&self) -> bool {
        !self.on || RQ_CAP - self.rq_len >= REPLY_MAX
    }

    /// Reply bytes waiting to go out.
    pub fn replies_pending(&self) -> usize {
        self.rq_len
    }

    /// One byte from the wire. Returns the clean data byte, if any.
    /// Feed only while `can_take` is true.
    pub fn input(&mut self, b: u8) -> Option<u8> {
        if !self.on {
            return Some(b);
        }
        match self.st {
            In::Data => {
                let after_cr = core::mem::take(&mut self.cr_seen);
                if b == IAC {
                    self.st = In::Iac;
                    None
                } else if after_cr && b == 0 && !self.o.him_binary {
                    // NVT CR NUL is a bare CR.
                    None
                } else {
                    self.cr_seen = b == b'\r' && !self.o.him_binary;
                    Some(b)
                }
            }
            In::Iac => {
                self.st = In::Data;
                match b {
                    IAC => return Some(IAC),
                    WILL | WONT | DO | DONT => self.st = In::Verb(b),
                    SB => self.st = In::Sb,
                    // NOP, GA, a stray SE and every other command: dropped.
                    _ => {}
                }
                None
            }
            In::Verb(v) => {
                self.st = In::Data;
                self.negotiate(v, b);
                None
            }
            In::Sb => {
                if b == IAC {
                    self.st = In::SbIac;
                }
                None
            }
            In::SbIac => {
                // IAC SE ends it; IAC IAC (or anything malformed) stays inside.
                self.st = if b == SE { In::Data } else { In::Sb };
                None
            }
        }
    }

    fn negotiate(&mut self, verb: u8, opt: u8) {
        self.neg_seen = true;
        if opt == OPT_LINK && matches!(verb, DO | DONT) {
            self.negotiate_link(verb == DO);
            return;
        }
        let o = &mut self.o;
        let reply: Option<(u8, bool)> = match verb {
            WILL => {
                let flag = match opt {
                    OPT_ECHO => Some(&mut o.him_echo),
                    OPT_SGA => Some(&mut o.him_sga),
                    OPT_BINARY => Some(&mut o.him_binary),
                    _ => None,
                };
                match flag {
                    Some(f) if *f => None,
                    Some(f) => {
                        *f = true;
                        Some((DO, false))
                    }
                    None => Some((DONT, false)),
                }
            }
            WONT => {
                let flag = match opt {
                    OPT_ECHO => Some(&mut o.him_echo),
                    OPT_SGA => Some(&mut o.him_sga),
                    OPT_BINARY => Some(&mut o.him_binary),
                    _ => None,
                };
                match flag {
                    Some(f) if *f => {
                        *f = false;
                        Some((DONT, false))
                    }
                    _ => None,
                }
            }
            DO => {
                let flag = match opt {
                    OPT_SGA => Some((&mut o.us_sga, false)),
                    OPT_BINARY => Some((&mut o.us_binary, false)),
                    OPT_NAWS => Some((&mut o.us_naws, true)),
                    _ => None,
                };
                match flag {
                    Some((f, _)) if *f => None,
                    Some((f, naws)) => {
                        *f = true;
                        Some((WILL, naws))
                    }
                    None => Some((WONT, false)),
                }
            }
            DONT => {
                let flag = match opt {
                    OPT_SGA => Some(&mut o.us_sga),
                    OPT_BINARY => Some(&mut o.us_binary),
                    OPT_NAWS => Some(&mut o.us_naws),
                    _ => None,
                };
                match flag {
                    Some(f) if *f => {
                        *f = false;
                        Some((WONT, false))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        match reply {
            Some((_, true)) => self.queue(&NAWS_REPLY),
            Some((v, false)) => self.queue(&[IAC, v, opt]),
            None => {}
        }
    }

    /// `DO 90` (`yes`) or `DONT 90` from the server. Our own offer's answer
    /// needs no reply; a `DO 90` before we offered is agreed with `WILL 90`;
    /// `DONT 90` while on gets `WONT 90`. Once off it stays off.
    fn negotiate_link(&mut self, yes: bool) {
        let reply = match (self.link, yes) {
            (LinkOpt::Offered, true) => {
                self.link = LinkOpt::On;
                None
            }
            (LinkOpt::Idle | LinkOpt::Offer, true) => {
                self.link = LinkOpt::On;
                Some(WILL)
            }
            (LinkOpt::On, false) => {
                self.link = LinkOpt::Off;
                Some(WONT)
            }
            (LinkOpt::Idle | LinkOpt::Offer | LinkOpt::Offered, false) => {
                self.link = LinkOpt::Off;
                None
            }
            // Already on (a repeated DO), or off for good: a DO after a
            // refusal gets WONT, as for any option we do not do.
            (LinkOpt::On, true) => None,
            (LinkOpt::Off, true) => Some(WONT),
            (LinkOpt::Off, false) => None,
        };
        if let Some(v) = reply {
            self.queue(&[IAC, v, OPT_LINK]);
        }
    }

    fn queue(&mut self, bytes: &[u8]) {
        if RQ_CAP - self.rq_len < bytes.len() {
            return;
        }
        for &b in bytes {
            if let Some(d) = self.rq.get_mut((self.rq_head + self.rq_len) % RQ_CAP) {
                *d = b;
                self.rq_len += 1;
            }
        }
    }

    /// Move queued replies into `out`; returns bytes written.
    pub fn pop_replies(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.rq_len);
        for d in out.iter_mut().take(n) {
            *d = self.rq.get(self.rq_head).copied().unwrap_or(0);
            self.rq_head = (self.rq_head + 1) % RQ_CAP;
        }
        self.rq_len -= n;
        n
    }

    /// One byte for the wire: the first byte to send and, if it expands,
    /// the second one (`IAC IAC`, CR NUL). `next` is the byte after it, if
    /// known: a CR followed by LF stays bare.
    pub fn encode(&self, b: u8, next: Option<u8>) -> (u8, Option<u8>) {
        if !self.on {
            (b, None)
        } else if b == IAC {
            (IAC, Some(IAC))
        } else if b == b'\r' && self.cr_rule() && next != Some(b'\n') {
            (b'\r', Some(0))
        } else {
            (b, None)
        }
    }
}

/// The values of one LFD3K-LINK report. A `None` value leaves its key out
/// (the server then treats it as unknown).
#[derive(Clone, Copy, Debug)]
pub struct LinkStats<'a> {
    /// This modem's station name; empty sends `modem=-`.
    pub modem: &'a str,
    /// A bridged radio call (`kind=radio`); false: a USB Wi-Fi call
    /// (`kind=wifi tier=WIFI`).
    pub radio: bool,
    /// The radio caller's name; empty sends `caller=-`.
    pub caller: &'a str,
    /// Spreading factor of the current tier (5 to 12).
    pub sf: Option<u8>,
    /// Air bits per second of the current tier.
    pub bps: Option<u32>,
    /// SNR of the last frame heard, quarter dB.
    pub snr_x4: Option<i16>,
    /// RSSI of the last frame heard, dBm.
    pub rssi: Option<i16>,
    /// Retrains on this call.
    pub retrains: Option<u32>,
    /// Compression ratio times 10; `Some(0)` sends `lz=0` (compression off).
    pub lz_x10: Option<u32>,
}

/// Bounded text for one payload; a field that would not fit whole is left
/// out (it never does: the longest payload is well under the limit).
struct Pay {
    b: [u8; LINK_PAYLOAD_MAX],
    n: usize,
}

impl Pay {
    /// One ` key=value` field (no leading space for the first), whole or
    /// not at all. Bytes of `val` outside letters, digits and `- _ .` become
    /// `_` (no spaces inside values; only printable ASCII ever goes out).
    fn field(&mut self, key: &[u8], val: &[u8]) {
        let sep = usize::from(self.n > 0);
        let need = sep + key.len() + 1 + val.len();
        if self.n + need > LINK_PAYLOAD_MAX {
            return;
        }
        let safe = |c: u8| {
            if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.') {
                c
            } else {
                b'_'
            }
        };
        let bytes = b" "
            .iter()
            .take(sep)
            .copied()
            .chain(key.iter().copied())
            .chain(core::iter::once(b'='))
            .chain(val.iter().map(|&c| safe(c)));
        for c in bytes {
            if let Some(d) = self.b.get_mut(self.n) {
                *d = c;
                self.n += 1;
            }
        }
    }
}

/// Decimal text of `v` with an optional minus sign and one decimal (`tenths`
/// true: `v` is in tenths). Returns the length written into `out`.
fn dec(v: i64, tenths: bool, out: &mut [u8; 24]) -> usize {
    let mut tmp = [0u8; 24];
    let mut i = tmp.len();
    let mut u = v.unsigned_abs();
    let mut digits = 0;
    loop {
        if tenths && digits == 1 {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'.';
            }
        }
        i -= 1;
        if let Some(d) = tmp.get_mut(i) {
            *d = b'0' + (u % 10) as u8;
        }
        u /= 10;
        digits += 1;
        if (u == 0 && (!tenths || digits >= 2)) || i < 3 {
            break;
        }
    }
    if v < 0 {
        i -= 1;
        if let Some(d) = tmp.get_mut(i) {
            *d = b'-';
        }
    }
    let s = tmp.get(i..).unwrap_or(&[]);
    if let Some(d) = out.get_mut(..s.len()) {
        d.copy_from_slice(s);
    }
    s.len()
}

/// The LFD3K-LINK payload (format version 1, printable ASCII, at most
/// `LINK_PAYLOAD_MAX` bytes), space-separated `key=value`:
/// `v=1 modem=<name> kind=radio|wifi caller=<name or -> tier=<SF5..SF12|WIFI>
/// bps=<n> snr=<x.x> rssi=<n> retrains=<n> lz=<ratio or 0>`. Values are
/// clamped to what the server accepts: SNR within 99.9 dB either way, RSSI
/// -200 to 50, retrains up to 999999, the ratio up to 99.9. Names keep only
/// letters, digits and `- _ .` (anything else becomes `_`).
pub fn link_payload(s: &LinkStats<'_>, out: &mut [u8; LINK_PAYLOAD_MAX]) -> usize {
    let mut p = Pay {
        b: [0; LINK_PAYLOAD_MAX],
        n: 0,
    };
    let mut t = [0u8; 24];
    p.field(b"v", b"1");
    let modem = s.modem.as_bytes();
    p.field(
        b"modem",
        if modem.is_empty() {
            b"-"
        } else {
            modem.get(..24).unwrap_or(modem)
        },
    );
    p.field(b"kind", if s.radio { b"radio" } else { b"wifi" });
    let caller = s.caller.as_bytes();
    p.field(
        b"caller",
        if caller.is_empty() {
            b"-"
        } else {
            caller.get(..24).unwrap_or(caller)
        },
    );
    match (s.radio, s.sf) {
        (false, _) => p.field(b"tier", b"WIFI"),
        (true, Some(sf)) => {
            let n = dec(i64::from(sf.clamp(5, 12)), false, &mut t);
            let mut tier = [b'S', b'F', 0, 0];
            for (d, &c) in tier.iter_mut().skip(2).zip(t.iter().take(n)) {
                *d = c;
            }
            p.field(b"tier", tier.get(..2 + n.min(2)).unwrap_or(&[]));
        }
        (true, None) => {}
    }
    if let Some(v) = s.bps {
        let n = dec(i64::from(v.min(100_000_000)), false, &mut t);
        p.field(b"bps", t.get(..n).unwrap_or(&[]));
    }
    if let Some(v) = s.snr_x4 {
        // Quarter dB to tenths, rounded half away from zero.
        let q = i64::from(v);
        let tenths = ((q.abs() * 10 + 2) / 4).min(999);
        let n = dec(if q < 0 { -tenths } else { tenths }, true, &mut t);
        p.field(b"snr", t.get(..n).unwrap_or(&[]));
    }
    if let Some(v) = s.rssi {
        let n = dec(i64::from(v.clamp(-200, 50)), false, &mut t);
        p.field(b"rssi", t.get(..n).unwrap_or(&[]));
    }
    if let Some(v) = s.retrains {
        let n = dec(i64::from(v.min(999_999)), false, &mut t);
        p.field(b"retrains", t.get(..n).unwrap_or(&[]));
    }
    if let Some(v) = s.lz_x10 {
        let n = if v == 0 {
            dec(0, false, &mut t)
        } else {
            dec(i64::from(v.min(999)), true, &mut t)
        };
        p.field(b"lz", t.get(..n).unwrap_or(&[]));
    }
    debug_assert!(p.b.iter().take(p.n).all(|&c| (0x20..0x7F).contains(&c)));
    *out = p.b;
    p.n
}

/// The whole report frame: `IAC SB 90 <payload> IAC SE`. The payload is
/// printable ASCII, so it never holds an IAC (0xFF); one would be doubled.
/// Returns the frame length.
pub fn link_frame(s: &LinkStats<'_>, out: &mut [u8; LINK_FRAME_MAX]) -> usize {
    let mut pay = [0u8; LINK_PAYLOAD_MAX];
    let n = link_payload(s, &mut pay);
    debug_assert!(!pay.iter().take(n).any(|&c| c == IAC));
    let mut k = 0;
    let mut put = |c: u8, k: &mut usize| {
        if let Some(d) = out.get_mut(*k) {
            *d = c;
            *k += 1;
        }
    };
    for c in [IAC, SB, OPT_LINK] {
        put(c, &mut k);
    }
    for &c in pay.iter().take(n) {
        if c == IAC {
            // Cannot happen (printable ASCII); doubled per RFC 855 anyway,
            // never past the frame (the payload then loses its last bytes).
            if k + 4 > LINK_FRAME_MAX {
                break;
            }
            put(IAC, &mut k);
        }
        if k + 3 > LINK_FRAME_MAX {
            break;
        }
        put(c, &mut k);
    }
    put(IAC, &mut k);
    put(SE, &mut k);
    k
}
