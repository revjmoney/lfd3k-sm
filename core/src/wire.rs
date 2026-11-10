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
//! - `wire_input` takes only what fits; keep the rest and offer it again.
//!   Deliver every received byte before reporting `wire_closed`.

use crate::settings::HOST_MAX;
use crate::telnet::Telnet;

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

    pub(crate) fn pop1(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let b = self.buf.get(self.head).copied();
        self.head = (self.head + 1) % RING_CAP;
        self.len -= 1;
        b
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
    pub(crate) name: [u8; 16],
    pub(crate) name_len: u8,
    pub(crate) last_tx: Option<u32>,
    pub(crate) last_rx: Option<u32>,
}

impl Wire {
    pub(crate) fn new() -> Wire {
        Wire {
            call: Call::None,
            req: None,
            close: false,
            rx: Ring::new(),
            tx: Ring::new(),
            tn: Telnet::new(),
            pend: None,
            name: [0; 16],
            name_len: 0,
            last_tx: None,
            last_rx: None,
        }
    }

    pub(crate) fn name(&self) -> &[u8] {
        self.name.get(..self.name_len as usize).unwrap_or(&[])
    }

    /// Bytes for the socket: a pending pair byte, then telnet replies, then
    /// user data (encoded here, so a reply never lands inside a pair).
    pub(crate) fn output(&mut self, out: &mut [u8]) -> usize {
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
        while n < out.len() {
            let Some(b) = self.tx.pop1() else { break };
            let (first, second) = self.tn.encode(b);
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
}
