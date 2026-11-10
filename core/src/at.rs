//! Hayes AT command parser, result codes and S registers.
//!
//! One command per line in v0.1. Parsing is case-insensitive, tolerates
//! surrounding whitespace, and never panics.

use crate::tier::Tier;

/// Longest accepted dial name, in bytes.
pub const MAX_NAME: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AtCmd<'a> {
    Attention,
    Dial(&'a str),
    Answer,
    Hangup,
    Online,
    Info,
    Verbose(bool),
    Echo(bool),
    SetReg(u8, u16),
    GetReg(u8),
    ShowConfig,
    Reset,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AtError {
    /// Not a command this modem knows.
    Unknown,
    /// Known command with a missing, malformed or out-of-range argument.
    BadArg,
}

/// Parse one command line (without the terminating CR).
pub fn parse_line(line: &str) -> Result<AtCmd<'_>, AtError> {
    let line = line.trim();
    let head = line.get(..2).ok_or(AtError::Unknown)?;
    if !head.eq_ignore_ascii_case("AT") {
        return Err(AtError::Unknown);
    }
    let rest = line.get(2..).ok_or(AtError::Unknown)?.trim_start();
    let first = match rest.bytes().next() {
        None => return Ok(AtCmd::Attention),
        Some(b) => b.to_ascii_uppercase(),
    };
    let arg = rest.get(1..).ok_or(AtError::BadArg)?.trim();
    match first {
        b'D' => parse_dial(arg),
        b'A' => none_arg(arg, AtCmd::Answer),
        b'H' => zero_arg(arg, AtCmd::Hangup),
        b'O' => zero_arg(arg, AtCmd::Online),
        b'I' => zero_arg(arg, AtCmd::Info),
        b'Z' => zero_arg(arg, AtCmd::Reset),
        b'V' => bool_arg(arg).map(AtCmd::Verbose),
        b'E' => bool_arg(arg).map(AtCmd::Echo),
        b'S' => parse_sreg(arg),
        b'&' => {
            if arg.eq_ignore_ascii_case("V") {
                Ok(AtCmd::ShowConfig)
            } else {
                Err(AtError::Unknown)
            }
        }
        _ => Err(AtError::Unknown),
    }
}

fn none_arg<'a>(arg: &str, cmd: AtCmd<'a>) -> Result<AtCmd<'a>, AtError> {
    if arg.is_empty() {
        Ok(cmd)
    } else {
        Err(AtError::BadArg)
    }
}

fn zero_arg<'a>(arg: &str, cmd: AtCmd<'a>) -> Result<AtCmd<'a>, AtError> {
    if arg.is_empty() || arg == "0" {
        Ok(cmd)
    } else {
        Err(AtError::BadArg)
    }
}

fn bool_arg(arg: &str) -> Result<bool, AtError> {
    match arg {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(AtError::BadArg),
    }
}

fn parse_dial(arg: &str) -> Result<AtCmd<'_>, AtError> {
    // Strip one leading T or P dial modifier, as in ATDTAXIS.
    let mut name = arg;
    if let Some(b) = name.bytes().next() {
        if b.eq_ignore_ascii_case(&b'T') || b.eq_ignore_ascii_case(&b'P') {
            name = name.get(1..).unwrap_or("");
        }
    }
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(AtError::BadArg);
    }
    Ok(AtCmd::Dial(name))
}

fn parse_sreg(arg: &str) -> Result<AtCmd<'_>, AtError> {
    let digits = arg.bytes().take_while(|b| b.is_ascii_digit()).count();
    let num = arg.get(..digits).ok_or(AtError::BadArg)?;
    let tail = arg.get(digits..).ok_or(AtError::BadArg)?;
    let n: u8 = num.parse().map_err(|_| AtError::BadArg)?;
    let tail = tail.trim_start();
    if tail == "?" {
        return Ok(AtCmd::GetReg(n));
    }
    let val = tail.strip_prefix('=').ok_or(AtError::BadArg)?.trim();
    if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AtError::BadArg);
    }
    let v: u16 = val.parse().map_err(|_| AtError::BadArg)?;
    Ok(AtCmd::SetReg(n, v))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResultCode {
    Ok,
    Error,
    Ring,
    NoCarrier,
    Busy,
    NoAnswer,
    Connect(Tier),
    Retrain(Tier),
}

/// Fixed buffer writer that records overflow instead of panicking.
struct Buf<'a> {
    out: &'a mut [u8],
    pos: usize,
    over: bool,
}

impl Buf<'_> {
    fn put(&mut self, s: &[u8]) {
        let end = self.pos.saturating_add(s.len());
        match self.out.get_mut(self.pos..end) {
            Some(dst) => {
                dst.copy_from_slice(s);
                self.pos = end;
            }
            None => self.over = true,
        }
    }

    fn put_u32(&mut self, mut v: u32) {
        let mut tmp = [0u8; 10];
        let mut i = tmp.len();
        loop {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'0' + (v % 10) as u8;
            }
            v /= 10;
            if v == 0 || i == 0 {
                break;
            }
        }
        self.put(tmp.get(i..).unwrap_or(&[]));
    }
}

/// Format a result code into `out`, returning bytes written.
///
/// If the whole text does not fit, nothing is meaningful and 0 is returned
/// (never a truncated line). Verbose form is `\r\n<text>\r\n`; numeric form
/// (verbose = false) is `<digit>\r`. RETRAIN is suppressed in numeric mode
/// and returns 0.
pub fn format_result(code: &ResultCode, verbose: bool, out: &mut [u8]) -> usize {
    let mut b = Buf { out, pos: 0, over: false };
    if verbose {
        b.put(b"\r\n");
        match code {
            ResultCode::Ok => b.put(b"OK"),
            ResultCode::Error => b.put(b"ERROR"),
            ResultCode::Ring => b.put(b"RING"),
            ResultCode::NoCarrier => b.put(b"NO CARRIER"),
            ResultCode::Busy => b.put(b"BUSY"),
            ResultCode::NoAnswer => b.put(b"NO ANSWER"),
            ResultCode::Connect(t) => {
                b.put(b"CONNECT ");
                b.put(t.name().as_bytes());
                b.put(b" ");
                b.put_u32(t.air_bps());
                b.put(b"/ARQ");
            }
            ResultCode::Retrain(t) => {
                b.put(b"RETRAIN ");
                b.put(t.name().as_bytes());
                b.put(b" ");
                b.put_u32(t.air_bps());
            }
        }
        b.put(b"\r\n");
    } else {
        let n = match code {
            ResultCode::Ok => 0,
            ResultCode::Connect(_) => 1,
            ResultCode::Ring => 2,
            ResultCode::NoCarrier => 3,
            ResultCode::Error => 4,
            ResultCode::Busy => 7,
            ResultCode::NoAnswer => 8,
            ResultCode::Retrain(_) => return 0,
        };
        b.put_u32(n);
        b.put(b"\r");
    }
    if b.over {
        0
    } else {
        b.pos
    }
}

/// (register, min, max, default)
const REGS: [(u8, u16, u16, u16); 7] = [
    (0, 0, 255, 0),
    (2, 0, 127, 43),
    (7, 1, 255, 30),
    (12, 0, 255, 50),
    (20, 0, 200, 20),
    (21, 0, 7, 7),
    (22, 1, 255, 20),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SRegs {
    vals: [u16; 7],
}

impl Default for SRegs {
    fn default() -> Self {
        Self::new()
    }
}

impl SRegs {
    /// Register numbers present, for `AT&V`.
    pub const NUMBERS: [u8; 7] = [0, 2, 7, 12, 20, 21, 22];

    pub fn new() -> Self {
        let mut vals = [0u16; 7];
        for (v, r) in vals.iter_mut().zip(REGS.iter()) {
            *v = r.3;
        }
        SRegs { vals }
    }

    fn index(n: u8) -> Option<usize> {
        REGS.iter().position(|r| r.0 == n)
    }

    pub fn get(&self, n: u8) -> Option<u16> {
        Self::index(n).and_then(|i| self.vals.get(i).copied())
    }

    /// Set a register; false if it does not exist or `v` is out of bounds.
    pub fn set(&mut self, n: u8, v: u16) -> bool {
        let Some(i) = Self::index(n) else { return false };
        let Some(r) = REGS.get(i) else { return false };
        if v < r.1 || v > r.2 {
            return false;
        }
        match self.vals.get_mut(i) {
            Some(slot) => {
                *slot = v;
                true
            }
            None => false,
        }
    }
}
