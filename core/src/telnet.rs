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
//! first). Subnegotiations are dropped; one longer than `SB_MAX` bytes ends
//! the subnegotiation and the filter goes back to data.
//!
//! Replies wait in a small queue that goes out ahead of user data. The
//! caller only feeds a byte while `can_take` is true (room for the largest
//! reply), so no reply is ever lost.
//!
//! Outgoing (caller to wire): 0xFF is sent as `IAC IAC`; CR is sent as CR NUL
//! unless BINARY is on both ways (our `WILL BINARY` accepted by the server's
//! `DO BINARY`, and the server's `WILL BINARY`). `encode` returns the pair so
//! the caller never splits it with a reply.
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
/// Longest subnegotiation body kept track of before giving up on it.
pub const SB_MAX: u8 = 64;
const RQ_CAP: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum In {
    Data,
    Iac,
    /// After `IAC WILL|WONT|DO|DONT`, waiting for the option byte.
    Verb(u8),
    /// Inside `IAC SB`, with the body length so far.
    Sb(u8),
    /// `IAC` seen inside a subnegotiation.
    SbIac(u8),
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
}

impl Default for Telnet {
    fn default() -> Self {
        Self::new()
    }
}

impl Telnet {
    pub fn new() -> Self {
        Telnet {
            on: false,
            st: In::Data,
            o: Opts::default(),
            rq: [0; RQ_CAP],
            rq_head: 0,
            rq_len: 0,
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

    /// BINARY agreed both ways: CR goes out bare.
    pub fn binary(&self) -> bool {
        self.o.us_binary && self.o.him_binary
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
                if b == IAC {
                    self.st = In::Iac;
                    None
                } else {
                    Some(b)
                }
            }
            In::Iac => {
                self.st = In::Data;
                match b {
                    IAC => return Some(IAC),
                    WILL | WONT | DO | DONT => self.st = In::Verb(b),
                    SB => self.st = In::Sb(0),
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
            In::Sb(n) => {
                self.st = if b == IAC {
                    In::SbIac(n)
                } else if n >= SB_MAX {
                    In::Data
                } else {
                    In::Sb(n + 1)
                };
                None
            }
            In::SbIac(n) => {
                self.st = match b {
                    SE => In::Data,
                    IAC if n < SB_MAX => In::Sb(n + 1),
                    _ => In::Data,
                };
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
    /// the second one (`IAC IAC`, CR NUL).
    pub fn encode(&self, b: u8) -> (u8, Option<u8>) {
        if !self.on {
            (b, None)
        } else if b == IAC {
            (IAC, Some(IAC))
        } else if b == b'\r' && !self.binary() {
            (b'\r', Some(0))
        } else {
            (b, None)
        }
    }
}
