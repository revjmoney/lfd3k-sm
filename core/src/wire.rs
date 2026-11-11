//! The wire: a TCP stream the platform opens when the core asks for one.
//!
//! The core never touches sockets. It raises a `WireReq` (host and port),
//! the platform connects and reports back (`wire_connected`, `wire_failed`,
//! `wire_closed`), and bytes move through two fixed 2 KB rings:
//! - `rx`: clean bytes from the wire (after the telnet filter), on their way
//!   to the USB host or, in a bridge, into the radio link;
//! - `tx`: raw bytes for the wire (from the USB host or the radio link),
//!   telnet encoded only as they leave in `wire_output`.
//!
//! Platform contract (single modem task):
//! - Poll `take_wire_close` before `take_wire_request`: a close always
//!   refers to the socket that exists, a request always opens a new one.
//! - On close: read `wire_output` until it returns 0, then close the socket
//!   (and abort a connect still in progress). Events for a socket after its
//!   close are not reported.
//! - While a close is pending (not yet taken), a new Wi-Fi dial (ATDT to a
//!   forward or `host:port`) answers ERROR, so the old socket's last bytes
//!   are never thrown away by the next call.
//! - `wire_input` takes only what fits; keep the rest and offer it again.
//!   Deliver every received byte before reporting `wire_closed`.

use crate::settings::{FWD_NAME, HOST_MAX};
use crate::telnet::Telnet;

/// A lone CR at the end of the data waits this long for an LF.
pub const CR_HOLD_MS: u32 = 50;

/// Size of each wire ring.
pub const RING_CAP: usize = 2048;

/// What the platform should connect to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WireReq {
    host: [u8; HOST_MAX],
    host_len: u8,
    port: u16,
}

impl WireReq {
    /// `None` if the host is empty or longer than `HOST_MAX`.
    pub fn new(host: &str, port: u16) -> Option<WireReq> {
        let n = host.len();
        if n == 0 || n > HOST_MAX {
            return None;
        }
        let mut r = WireReq {
            host: [0; HOST_MAX],
            host_len: n as u8,
            port,
        };
        r.host
            .get_mut(..n)?
            .copy_from_slice(host.as_bytes().get(..n)?);
        Some(r)
    }

    pub fn host(&self) -> &str {
        self.host
            .get(..self.host_len as usize)
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Fixed byte ring.
pub(crate) struct Ring {
    buf: [u8; RING_CAP],
    head: usize,
    len: usize,
}

impl Ring {
    pub(crate) const fn new() -> Ring {
        Ring {
            buf: [0; RING_CAP],
            head: 0,
            len: 0,
        }
    }

    pub(crate) fn free(&self) -> usize {
        RING_CAP - self.len
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// As much of `s` as fits; returns bytes taken.
    pub(crate) fn push(&mut self, s: &[u8]) -> usize {
        let n = s.len().min(self.free());
        for &b in s.iter().take(n) {
            if let Some(d) = self.buf.get_mut((self.head + self.len) % RING_CAP) {
                *d = b;
                self.len += 1;
            }
        }
        n
    }

    /// Copy up to `out.len()` bytes from the front without taking them.
    pub(crate) fn peek(&self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for (i, d) in out.iter_mut().take(n).enumerate() {
            *d = self
                .buf
                .get((self.head + i) % RING_CAP)
                .copied()
                .unwrap_or(0);
        }
        n
    }

    /// Drop `n` bytes from the front.
    pub(crate) fn skip(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % RING_CAP;
        self.len -= n;
    }

    pub(crate) fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = self.peek(out);
        self.skip(n);
        n
    }
}

/// Where a wire connection is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    /// Requested at `since`; waiting for `wire_connected` (S7 seconds).
    Dialing {
        since: u32,
    },
    Online,
    /// The far end closed at `since`; what it sent is still being delivered.
    Ending {
        since: u32,
    },
}

/// The call the wire takes part in. A plain radio call is the link's own
/// business and shows here as `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Call {
    None,
    /// USB host dialed a forward or `host:port`.
    Wire(Phase),
    /// Answered radio call bridged to the published forward.
    Bridge(Phase),
}

/// Wire side state kept in the Modem.
pub(crate) struct Wire {
    pub(crate) call: Call,
    /// Request not yet taken by the platform.
    pub(crate) req: Option<WireReq>,
    /// The core wants the socket closed.
    pub(crate) close: bool,
    pub(crate) rx: Ring,
    pub(crate) tx: Ring,
    pub(crate) tn: Telnet,
    /// Second byte of an `IAC IAC` or CR NUL pair that did not fit.
    pub(crate) pend: Option<u8>,
    /// Bridge forward name for the `BRIDGE` line.
    pub(crate) name: [u8; FWD_NAME],
    pub(crate) name_len: u8,
    pub(crate) last_tx: Option<u32>,
    pub(crate) last_rx: Option<u32>,
    /// A held CR has waited `CR_HOLD_MS`: send it (as CR NUL) now.
    pub(crate) cr_flush: bool,
    /// Latest time any wire call saw (`Modem::wire_pump`, input), for the
    /// LFD3K-LINK rate limit (`output` has no clock of its own).
    pub(crate) clock: u32,
    /// When the last LFD3K-LINK report was written out on this connection.
    pub(crate) link_sent: Option<u32>,
    /// A report is due before the 10 s period (a retrain): sent once the
    /// 1 s limit allows.
    pub(crate) link_due: bool,
}

/// LFD3K-LINK reports go out at least this far apart.
pub const LINK_MIN_GAP_MS: u32 = 1_000;
/// And this often while nothing else asks for one.
pub const LINK_PERIOD_MS: u32 = 10_000;

impl Wire {
    pub(crate) const fn new() -> Wire {
        Wire {
            call: Call::None,
            req: None,
            close: false,
            rx: Ring::new(),
            tx: Ring::new(),
            tn: Telnet::new(),
            pend: None,
            name: [0; FWD_NAME],
            name_len: 0,
            last_tx: None,
            last_rx: None,
            cr_flush: false,
            clock: 0,
            link_sent: None,
            link_due: false,
        }
    }

    /// Back to `Wire::new()` in place (no 4 KB temporary).
    pub(crate) fn reset_all(&mut self) {
        self.call = Call::None;
        self.req = None;
        self.close = false;
        self.rx.clear();
        self.tx.clear();
        self.tn = Telnet::new();
        self.pend = None;
        self.name_len = 0;
        self.last_tx = None;
        self.last_rx = None;
        self.cr_flush = false;
        self.clock = 0;
        self.link_sent = None;
        self.link_due = false;
    }

    /// A new connection: no report sent on it yet.
    pub(crate) fn link_reset(&mut self) {
        self.link_sent = None;
        self.link_due = false;
    }

    /// An LFD3K-LINK report should go out now: the server agreed (DO 90),
    /// the call is online, and it is the first report, a retrain's, or the
    /// 10 s period, never within 1 s of the last one.
    pub(crate) fn link_wanted(&self) -> bool {
        let online = matches!(
            self.call,
            Call::Wire(Phase::Online) | Call::Bridge(Phase::Online)
        );
        if !online || !self.tn.link_on() {
            return false;
        }
        match self.link_sent {
            None => true,
            Some(t) => {
                let gap = self.clock.wrapping_sub(t);
                gap >= LINK_MIN_GAP_MS && (self.link_due || gap >= LINK_PERIOD_MS)
            }
        }
    }

    pub(crate) fn name(&self) -> &[u8] {
        self.name.get(..self.name_len as usize).unwrap_or(&[])
    }

    /// Bytes for the socket: a pending pair byte, then telnet replies, then
    /// the LFD3K-LINK offer and `report` (a whole `IAC SB 90 ... IAC SE`
    /// frame, see `link_wanted`), then user data (encoded here, so a reply
    /// never lands inside a pair). The offer and the report go out only
    /// whole, after every queued reply, and the report only when `out` also
    /// has room for all the caller data waiting (each byte counted twice for
    /// a possible `IAC IAC` or CR NUL): stats never hold caller data back.
    /// A report that does not fit stays due for a later call.
    pub(crate) fn output(&mut self, out: &mut [u8], report: Option<&[u8]>) -> usize {
        let mut n = 0;
        if let Some(p) = self.pend {
            match out.get_mut(n) {
                Some(d) => {
                    *d = p;
                    n += 1;
                    self.pend = None;
                }
                None => return 0,
            }
        }
        n += self.tn.pop_replies(out.get_mut(n..).unwrap_or(&mut []));
        // Closing (no call): nothing more will follow, so a CR is not held.
        let live = matches!(
            self.call,
            Call::Wire(Phase::Online) | Call::Bridge(Phase::Online)
        );
        if live && self.tn.replies_pending() == 0 {
            if self.tn.offer_due() && out.len().saturating_sub(n) >= 3 && self.tn.take_offer() {
                for c in [
                    crate::telnet::IAC,
                    crate::telnet::WILL,
                    crate::telnet::OPT_LINK,
                ] {
                    if let Some(d) = out.get_mut(n) {
                        *d = c;
                        n += 1;
                    }
                }
            }
            if let Some(r) = report.filter(|r| !r.is_empty()) {
                let room = out.len().saturating_sub(n);
                let backlog = self.tx.len().saturating_mul(2);
                if room >= r.len().saturating_add(backlog) {
                    if let Some(d) = out.get_mut(n..n + r.len()) {
                        d.copy_from_slice(r);
                        n += r.len();
                        self.link_sent = Some(self.clock);
                        self.link_due = false;
                    }
                }
            }
        }
        while n < out.len() {
            let mut two = [0u8; 2];
            let k = self.tx.peek(&mut two);
            let Some(&b) = two.first().filter(|_| k > 0) else {
                break;
            };
            let next = two.get(1).copied().filter(|_| k > 1);
            if b == b'\r' && self.tn.cr_rule() && next.is_none() {
                if live && !self.cr_flush {
                    // Wait for the next byte: CR LF or CR NUL.
                    break;
                }
                self.cr_flush = false;
            }
            self.tx.skip(1);
            let (first, mut second) = self.tn.encode(b, next);
            if b == b'\r' && self.tn.cr_rule() && next == Some(b'\n') {
                // CR LF goes out as one pair, so no reply lands inside it.
                self.tx.skip(1);
                second = next;
            }
            if let Some(d) = out.get_mut(n) {
                *d = first;
                n += 1;
            }
            if let Some(s) = second {
                match out.get_mut(n) {
                    Some(d) => {
                        *d = s;
                        n += 1;
                    }
                    None => {
                        self.pend = Some(s);
                        break;
                    }
                }
            }
        }
        n
    }

    /// Tick: a CR alone at the end of `tx` since the last push
    /// `CR_HOLD_MS` ago goes out as CR NUL at the next `output`.
    pub(crate) fn cr_timer(&mut self, now: u32) {
        let mut one = [0u8; 1];
        let lone_cr = self.tx.len() == 1 && self.tx.peek(&mut one) == 1 && &one == b"\r";
        if lone_cr
            && self.tn.cr_rule()
            && self
                .last_tx
                .is_none_or(|t| now.wrapping_sub(t) >= CR_HOLD_MS)
        {
            self.cr_flush = true;
        }
    }
}
