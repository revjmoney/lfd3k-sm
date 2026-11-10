//! Hayes AT command parser, result codes and S registers.
//!
//! One command per line in v0.1. Parsing is case-insensitive, tolerates
//! surrounding whitespace, and never panics.

use crate::settings::{
    parse_hostport, parse_port, valid_fwd_name, valid_host, valid_pass, valid_ssid, WireMode,
};
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
    /// `AT+FB`: dump the frame buffer (the firmware answers).
    FrameBuffer,
    /// `AT&W`: save the settings.
    Save,
    /// `AT+WAP?`
    WapQuery,
    /// `AT+WAP=0` / `AT+WAP=1`
    WapPower(bool),
    /// `AT+WAP=<ssid>,<pass>`
    WapSet {
        ssid: &'a str,
        pass: &'a str,
    },
    /// `AT+WSTA?`
    WstaQuery,
    /// `AT+WSTA=0`
    WstaForget,
    /// `AT+WSTA=<ssid>,<pass>`
    WstaSet {
        ssid: &'a str,
        pass: &'a str,
    },
    /// `AT+WIFI?`
    WifiQuery,
    /// `AT+FWD?`
    FwdList,
    /// `AT+FWD=<name>`
    FwdDelete(&'a str),
    /// `AT+FWD=<name>,<host>,<port>[,T|R]`
    FwdSet {
        name: &'a str,
        host: &'a str,
        port: u16,
        mode: WireMode,
    },
    /// `AT+PUB?`
    PubQuery,
    /// `AT+PUB=0`
    PubNone,
    /// `AT+PUB=<name>`
    Pub(&'a str),
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
            } else if arg.eq_ignore_ascii_case("W") {
                Ok(AtCmd::Save)
            } else {
                Err(AtError::Unknown)
            }
        }
        b'+' => parse_plus(arg),
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

/// Argument form of a `+` command.
#[derive(Clone, Copy)]
enum Form<'a> {
    Query,
    Set(&'a str),
}

/// `+FB`, `+WAP`, `+WSTA`, `+WIFI`, `+FWD`, `+PUB` (`arg` is after the `+`).
///
/// Arguments are split on commas only (an SSID or name may not contain
/// one, nor may a password). SSID and password are taken as typed (inner
/// and edge spaces kept, apart from the trim of the whole line); the fields
/// of `+FWD` are trimmed so `AT+FWD=a, b.com, 23` works.
fn parse_plus(arg: &str) -> Result<AtCmd<'_>, AtError> {
    let Some(i) = arg.find(['=', '?']) else {
        return if arg.eq_ignore_ascii_case("FB") {
            Ok(AtCmd::FrameBuffer)
        } else {
            Err(AtError::Unknown)
        };
    };
    let name = arg.get(..i).ok_or(AtError::Unknown)?.trim_end();
    let rest = arg.get(i..).ok_or(AtError::Unknown)?;
    let known = ["WAP", "WSTA", "WIFI", "FWD", "PUB"];
    if !known.iter().any(|k| name.eq_ignore_ascii_case(k)) {
        return Err(AtError::Unknown);
    }
    let form = match rest.strip_prefix('=') {
        Some(v) => Form::Set(v),
        None if rest == "?" => Form::Query,
        None => return Err(AtError::BadArg),
    };
    if name.eq_ignore_ascii_case("WAP") {
        parse_wap(form)
    } else if name.eq_ignore_ascii_case("WSTA") {
        parse_wsta(form)
    } else if name.eq_ignore_ascii_case("WIFI") {
        match form {
            Form::Query => Ok(AtCmd::WifiQuery),
            Form::Set(_) => Err(AtError::BadArg),
        }
    } else if name.eq_ignore_ascii_case("FWD") {
        parse_fwd(form)
    } else {
        parse_pub(form)
    }
}

/// `<ssid>,<pass>`, both valid.
fn ssid_pass(v: &str) -> Result<(&str, &str), AtError> {
    let (ssid, pass) = v.split_once(',').ok_or(AtError::BadArg)?;
    if valid_ssid(ssid) && valid_pass(pass) {
        Ok((ssid, pass))
    } else {
        Err(AtError::BadArg)
    }
}

fn parse_wap(form: Form<'_>) -> Result<AtCmd<'_>, AtError> {
    match form {
        Form::Query => Ok(AtCmd::WapQuery),
        Form::Set("0") => Ok(AtCmd::WapPower(false)),
        Form::Set("1") => Ok(AtCmd::WapPower(true)),
        Form::Set(v) => ssid_pass(v).map(|(ssid, pass)| AtCmd::WapSet { ssid, pass }),
    }
}

fn parse_wsta(form: Form<'_>) -> Result<AtCmd<'_>, AtError> {
    match form {
        Form::Query => Ok(AtCmd::WstaQuery),
        Form::Set("0") => Ok(AtCmd::WstaForget),
        Form::Set(v) => ssid_pass(v).map(|(ssid, pass)| AtCmd::WstaSet { ssid, pass }),
    }
}

fn parse_fwd(form: Form<'_>) -> Result<AtCmd<'_>, AtError> {
    let Form::Set(v) = form else {
        return Ok(AtCmd::FwdList);
    };
    let mut it = v.split(',').map(str::trim);
    let name = it
        .next()
        .filter(|n| valid_fwd_name(n))
        .ok_or(AtError::BadArg)?;
    let (host, port, mode) = (it.next(), it.next(), it.next());
    if it.next().is_some() {
        return Err(AtError::BadArg);
    }
    let (Some(host), Some(port)) = (host, port) else {
        return if host.is_none() {
            Ok(AtCmd::FwdDelete(name))
        } else {
            Err(AtError::BadArg)
        };
    };
    let port = parse_port(port).ok_or(AtError::BadArg)?;
    if !valid_host(host) {
        return Err(AtError::BadArg);
    }
    let mode = match mode {
        None => WireMode::default_for(port),
        Some(m) if m.eq_ignore_ascii_case("T") => WireMode::Telnet,
        Some(m) if m.eq_ignore_ascii_case("R") => WireMode::Raw,
        Some(_) => return Err(AtError::BadArg),
    };
    Ok(AtCmd::FwdSet {
        name,
        host,
        port,
        mode,
    })
}

fn parse_pub(form: Form<'_>) -> Result<AtCmd<'_>, AtError> {
    match form {
        Form::Query => Ok(AtCmd::PubQuery),
        Form::Set("0") => Ok(AtCmd::PubNone),
        Form::Set(n) if valid_fwd_name(n.trim()) => Ok(AtCmd::Pub(n.trim())),
        Form::Set(_) => Err(AtError::BadArg),
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
    // Up to 16 bytes is a station or forward name; longer is only valid as
    // `host:port` (the modem decides which it is).
    if name.is_empty() || (name.len() > MAX_NAME && parse_hostport(name).is_none()) {
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
    // S30 (TX power in dBm) is the only signed register: it alone takes a
    // leading '-', stored as the two's complement of an i16.
    let (neg, digits) = match val.strip_prefix('-') {
        Some(d) if n == REG_TX_POWER => (true, d),
        _ => (false, val),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AtError::BadArg);
    }
    if n == REG_TX_POWER {
        let m: i16 = digits.parse().map_err(|_| AtError::BadArg)?;
        let v = if neg { m.wrapping_neg() } else { m };
        return Ok(AtCmd::SetReg(n, v as u16));
    }
    let v: u16 = digits.parse().map_err(|_| AtError::BadArg)?;
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
    /// A Wi-Fi (TCP) call: `CONNECT WIFI TCP`, numeric 1.
    ConnectWifi,
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
    let mut b = Buf {
        out,
        pos: 0,
        over: false,
    };
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
            ResultCode::ConnectWifi => b.put(b"CONNECT WIFI TCP"),
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
            ResultCode::Connect(_) | ResultCode::ConnectWifi => 1,
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

/// S30: SX1262 chip output power in dBm (signed, -9 to 22, default 2).
pub const REG_TX_POWER: u8 = 30;

/// (register, min, max, default). Values are compared as i16; S30 is stored
/// as the two's complement of its i16 value, the others are never negative.
const REGS: [(u8, i16, i16, i16); 8] = [
    (0, 0, 255, 0),
    (2, 0, 127, 43),
    (7, 1, 255, 30),
    (12, 0, 255, 50),
    (20, 0, 200, 20),
    (21, 0, 7, 7),
    (22, 1, 255, 20),
    (REG_TX_POWER, -9, 22, 2),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SRegs {
    vals: [u16; 8],
}

impl Default for SRegs {
    fn default() -> Self {
        Self::new()
    }
}

impl SRegs {
    /// Register numbers present, for `AT&V`.
    pub const NUMBERS: [u8; 8] = [0, 2, 7, 12, 20, 21, 22, 30];

    pub const fn new() -> Self {
        let mut vals = [0u16; 8];
        let mut i = 0;
        while i < vals.len() {
            vals[i] = REGS[i].3 as u16;
            i += 1;
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
        let Some(i) = Self::index(n) else {
            return false;
        };
        let Some(r) = REGS.get(i) else { return false };
        let sv = v as i16;
        if sv < r.1 || sv > r.2 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let ok: &[(&str, AtCmd)] = &[
            ("AT", AtCmd::Attention),
            ("at", AtCmd::Attention),
            ("  AT  ", AtCmd::Attention),
            ("ATDT AXIS", AtCmd::Dial("AXIS")),
            ("ATDTAXIS", AtCmd::Dial("AXIS")),
            ("atdt axis", AtCmd::Dial("axis")),
            ("ATD AXIS", AtCmd::Dial("AXIS")),
            ("ATDAXIS", AtCmd::Dial("AXIS")),
            ("ATDP BBS", AtCmd::Dial("BBS")),
            ("ATDT ABCDEFGHIJKLMNOP", AtCmd::Dial("ABCDEFGHIJKLMNOP")),
            ("ATDT  SPACED NAME  ", AtCmd::Dial("SPACED NAME")),
            ("ATA", AtCmd::Answer),
            ("ata", AtCmd::Answer),
            ("ATH", AtCmd::Hangup),
            ("ATH0", AtCmd::Hangup),
            ("ath0", AtCmd::Hangup),
            ("ATO", AtCmd::Online),
            ("ato", AtCmd::Online),
            ("ATI", AtCmd::Info),
            ("ati", AtCmd::Info),
            ("ATV0", AtCmd::Verbose(false)),
            ("ATV1", AtCmd::Verbose(true)),
            ("atv1", AtCmd::Verbose(true)),
            ("ATE0", AtCmd::Echo(false)),
            ("ATE1", AtCmd::Echo(true)),
            ("ate0", AtCmd::Echo(false)),
            ("ATS7=45", AtCmd::SetReg(7, 45)),
            ("ats22=255", AtCmd::SetReg(22, 255)),
            ("ATS0=1", AtCmd::SetReg(0, 1)),
            ("ATS7?", AtCmd::GetReg(7)),
            ("ats12?", AtCmd::GetReg(12)),
            ("AT&V", AtCmd::ShowConfig),
            ("at&v", AtCmd::ShowConfig),
            ("ATZ", AtCmd::Reset),
            ("atz", AtCmd::Reset),
        ];
        for (line, want) in ok {
            assert_eq!(parse_line(line), Ok(*want), "{line:?}");
        }
        let bad: &[(&str, AtError)] = &[
            ("", AtError::Unknown),
            ("A", AtError::Unknown),
            ("HELLO", AtError::Unknown),
            ("ATX", AtError::Unknown),
            ("AT&F", AtError::Unknown),
            ("ATDT", AtError::BadArg),
            ("ATD", AtError::BadArg),
            ("ATDT ABCDEFGHIJKLMNOPQ", AtError::BadArg),
            ("ATA1", AtError::BadArg),
            ("ATH1", AtError::BadArg),
            ("ATV2", AtError::BadArg),
            ("ATE", AtError::BadArg),
            ("ATS7", AtError::BadArg),
            ("ATS7=", AtError::BadArg),
            ("ATS7=x", AtError::BadArg),
            ("ATS7=-1", AtError::BadArg),
            ("ATS7=99999", AtError::BadArg),
            ("ATS=5", AtError::BadArg),
            ("ATS999=1", AtError::BadArg),
        ];
        for (line, want) in bad {
            assert_eq!(parse_line(line), Err(*want), "{line:?}");
        }
    }

    fn fmt(code: ResultCode, verbose: bool) -> Vec<u8> {
        let mut b = [0u8; 64];
        let n = format_result(&code, verbose, &mut b);
        b[..n].to_vec()
    }

    #[test]
    fn result_strings() {
        let verbose: &[(ResultCode, &str)] = &[
            (ResultCode::Ok, "\r\nOK\r\n"),
            (ResultCode::Error, "\r\nERROR\r\n"),
            (ResultCode::Ring, "\r\nRING\r\n"),
            (ResultCode::NoCarrier, "\r\nNO CARRIER\r\n"),
            (ResultCode::Busy, "\r\nBUSY\r\n"),
            (ResultCode::NoAnswer, "\r\nNO ANSWER\r\n"),
            (
                ResultCode::Connect(Tier::Sf7),
                "\r\nCONNECT LORA SF7 BW500 21875/ARQ\r\n",
            ),
            (
                ResultCode::Connect(Tier::Sf5),
                "\r\nCONNECT LORA SF5 BW500 62500/ARQ\r\n",
            ),
            (
                ResultCode::Connect(Tier::Sf12),
                "\r\nCONNECT LORA SF12 BW500 1172/ARQ\r\n",
            ),
            (
                ResultCode::Retrain(Tier::Sf11),
                "\r\nRETRAIN LORA SF11 BW500 2148\r\n",
            ),
            (
                ResultCode::Retrain(Tier::Sf6),
                "\r\nRETRAIN LORA SF6 BW500 37500\r\n",
            ),
        ];
        for (code, want) in verbose {
            assert_eq!(fmt(*code, true), want.as_bytes(), "{code:?}");
        }
        let numeric: &[(ResultCode, &str)] = &[
            (ResultCode::Ok, "0\r"),
            (ResultCode::Connect(Tier::Sf5), "1\r"),
            (ResultCode::Ring, "2\r"),
            (ResultCode::NoCarrier, "3\r"),
            (ResultCode::Error, "4\r"),
            (ResultCode::Busy, "7\r"),
            (ResultCode::NoAnswer, "8\r"),
            (ResultCode::Retrain(Tier::Sf9), ""),
        ];
        for (code, want) in numeric {
            assert_eq!(fmt(*code, false), want.as_bytes(), "{code:?}");
        }
        // Never a truncated line: too small a buffer gives 0.
        let mut small = [0u8; 10];
        assert_eq!(
            format_result(&ResultCode::Connect(Tier::Sf7), true, &mut small),
            0
        );
        assert_eq!(format_result(&ResultCode::Ok, false, &mut [0u8; 1]), 0);
        assert_eq!(format_result(&ResultCode::Ok, true, &mut []), 0);
    }

    #[test]
    fn sregs_defaults_and_bounds() {
        let s = SRegs::new();
        assert_eq!(SRegs::default(), s);
        // (register, default, min, max) from the plan and FEATURES.md.
        let table: [(u8, u16, u16, u16); 7] = [
            (0, 0, 0, 255),
            (2, 43, 0, 127),
            (7, 30, 1, 255),
            (12, 50, 0, 255),
            (20, 20, 0, 200),
            (21, 7, 0, 7),
            (22, 20, 1, 255),
        ];
        for (n, def, min, max) in table {
            assert_eq!(s.get(n), Some(def), "S{n} default");
            let mut r = SRegs::new();
            assert!(r.set(n, min), "S{n}={min}");
            assert_eq!(r.get(n), Some(min));
            assert!(r.set(n, max), "S{n}={max}");
            assert_eq!(r.get(n), Some(max));
            assert!(!r.set(n, max + 1), "S{n}={} accepted", max + 1);
            assert_eq!(r.get(n), Some(max), "rejected write changed S{n}");
            if min > 0 {
                assert!(!r.set(n, min - 1), "S{n}={} accepted", min - 1);
                assert_eq!(r.get(n), Some(max));
            }
            assert!(!r.set(n, u16::MAX));
        }
        for n in [1u8, 3, 6, 8, 13, 19, 23, 255] {
            assert_eq!(s.get(n), None, "S{n} should not exist");
            assert!(!SRegs::new().set(n, 0));
        }
        assert_eq!(SRegs::NUMBERS, [0, 2, 7, 12, 20, 21, 22, 30]);
    }

    #[test]
    fn s30_default_and_range() {
        let mut r = SRegs::new();
        assert_eq!(r.get(REG_TX_POWER), Some(2));
        // Both ends are accepted, negative values as two's complement.
        assert!(r.set(REG_TX_POWER, 22));
        assert_eq!(r.get(REG_TX_POWER), Some(22));
        assert!(r.set(REG_TX_POWER, (-9i16) as u16));
        assert_eq!(r.get(REG_TX_POWER).map(|v| v as i16), Some(-9));
        assert!(r.set(REG_TX_POWER, 0));
        // One step outside is refused and the old value stays.
        assert!(!r.set(REG_TX_POWER, 23));
        assert!(!r.set(REG_TX_POWER, (-10i16) as u16));
        // 65527 is -9 as a raw u16 (valid); the text form is refused below.
        assert!(r.set(REG_TX_POWER, 65_527));
        assert!(r.set(REG_TX_POWER, 0));
        assert_eq!(r.get(REG_TX_POWER), Some(0));
        // Parsing: signed only for S30.
        assert_eq!(
            parse_line("ATS30=-5"),
            Ok(AtCmd::SetReg(30, (-5i16) as u16))
        );
        assert_eq!(parse_line("ATS30=10"), Ok(AtCmd::SetReg(30, 10)));
        assert_eq!(parse_line("ATS30?"), Ok(AtCmd::GetReg(30)));
        assert_eq!(parse_line("ATS7=-1"), Err(AtError::BadArg));
        assert_eq!(parse_line("ATS30=-"), Err(AtError::BadArg));
        assert_eq!(parse_line("ATS30=99999"), Err(AtError::BadArg));
        assert_eq!(parse_line("ATS30=65527"), Err(AtError::BadArg));
        assert_eq!(parse_line("ATS30=65527"), Err(AtError::BadArg));
    }

    #[test]
    fn s30_set_ffff_reads_minus_one() {
        // 0xFFFF is -1 as a two's complement i16: accepted by design.
        let mut r = SRegs::new();
        assert!(r.set(REG_TX_POWER, 0xFFFF));
        assert_eq!(r.get(REG_TX_POWER), Some(0xFFFF));
        assert_eq!(r.get(REG_TX_POWER).map(|v| v as i16), Some(-1));
        // The other registers have no negative range.
        assert!(!r.set(7, 0xFFFF));
    }
}
