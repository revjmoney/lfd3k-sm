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
        }
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
