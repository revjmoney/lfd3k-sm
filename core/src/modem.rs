//! The Modem: Hayes AT glue, host buffers and lights around a `Link`.
//!
//! Host output is one byte stream (like a real modem's serial line): echo,
//! result codes, verbose info lines (`\r\nDIALING <name>\r\n`, `\r\nRINGING\r\n`,
//! `\r\nTRAINING SNR <x.x> dB\r\n`) and, in data mode, received call bytes.
//! Data mode is 8-bit transparent: nothing but call bytes reaches the host
//! while online. A RETRAIN that happens in data mode is kept as a notice and
//! printed after the `OK` of the escape (verbose only); AT&V shows the
//! current tier and the last retrains. NO CARRIER ends a call as on a real
//! modem, right after the received in-order data that fits in the host
//! buffer (the call never leaves NO CARRIER pending).

use crate::at::{format_result, parse_line, AtCmd, ResultCode, SRegs};
use crate::frame::NAME_MAX;
use crate::link::{Event, Link};
use crate::radio::{Radio, RxInfo};
use crate::tier::Tier;

/// ATI line 1.
pub const BANNER: &str = "LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM";
/// ATI line 2 is `v` + VERSION + space + RELEASE_NAME.
pub const RELEASE_NAME: &str = "r.420 BL4Z3B4ND ULTR4M0D3M";
/// ATI line 3.
pub const RELEASE_TAGLINE: &str = "S0N1C B00M R4D10 PR0 MAX";

const OUT_CAP: usize = 1024;
/// Output space kept free of call data so result codes always fit.
const OUT_RESERVE: usize = 96;
const LINE_CAP: usize = 64;
const LIGHT_MS: u32 = 100;
/// Retrains remembered for AT&V.
const HISTORY: usize = 4;

pub struct Config {
    pub name: [u8; 16],
    pub name_len: u8,
    pub sregs: SRegs,
}

impl Config {
    /// Station name (truncated to 16 bytes) with default S registers.
    pub fn new(name: &str) -> Config {
        let mut c = Config {
            name: [0; NAME_MAX],
            name_len: 0,
            sregs: SRegs::new(),
        };
        let mut n = name.len().min(NAME_MAX);
        while !name.is_char_boundary(n) {
            n -= 1;
        }
        if let (Some(d), Some(s)) = (c.name.get_mut(..n), name.as_bytes().get(..n)) {
            d.copy_from_slice(s);
            c.name_len = n as u8;
        }
        c
    }
}

/// `<tier name> <bps>`.
fn put_tier(t: &mut Txt, tier: Tier) {
    t.put(tier.name().as_bytes())
        .put(b" ")
        .num(tier.air_bps(), 1);
}

fn name_of(cfg: &Config) -> &str {
    cfg.name
        .get(..cfg.name_len as usize)
        .and_then(|b| core::str::from_utf8(b).ok())
        .unwrap_or("")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lights {
    pub oh: bool,
    pub cd: bool,
    pub sd: bool,
    pub rd: bool,
    pub hs: bool,
    pub ec: bool,
    pub aa: bool,
    pub tier: Option<Tier>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Command,
    Data,
    OnlineCommand,
}

struct OutRing {
    buf: [u8; OUT_CAP],
    head: usize,
    len: usize,
}

impl OutRing {
    fn free(&self) -> usize {
        OUT_CAP - self.len
    }

    /// All or nothing.
    fn push(&mut self, s: &[u8]) -> bool {
        if s.len() > self.free() {
            return false;
        }
        for &b in s {
            if let Some(d) = self.buf.get_mut((self.head + self.len) % OUT_CAP) {
                *d = b;
                self.len += 1;
            }
        }
        true
    }

    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for d in out.iter_mut().take(n) {
            *d = self.buf.get(self.head).copied().unwrap_or(0);
            self.head = (self.head + 1) % OUT_CAP;
        }
        self.len -= n;
        n
    }
}

/// Small text builder for info lines.
struct Txt {
    b: [u8; 160],
    n: usize,
}

impl Txt {
    fn new() -> Self {
        Txt { b: [0; 160], n: 0 }
    }

    fn put(&mut self, s: &[u8]) -> &mut Self {
        for &c in s {
            if let Some(d) = self.b.get_mut(self.n) {
                *d = c;
                self.n += 1;
            }
        }
        self
    }

    /// Decimal, zero padded to `width` digits.
    fn num(&mut self, v: u32, width: usize) -> &mut Self {
        let mut tmp = [b'0'; 10];
        let mut i = tmp.len();
        let mut v = v;
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
        let start = i.min(tmp.len().saturating_sub(width));
        self.put(tmp.get(start..).unwrap_or(&[]))
    }

    fn bytes(&self) -> &[u8] {
        self.b.get(..self.n).unwrap_or(&[])
    }
}

pub struct Modem {
    cfg: Config,
    link: Link,
    mode: Mode,
    out: OutRing,
    line: [u8; LINE_CAP],
    line_len: usize,
    line_over: bool,
    echo: bool,
    verbose: bool,
    /// S2 characters held as a possible escape (0 to 3).
    esc: u8,
    esc_last: u32,
    last_host: u32,
    /// Latest retrain seen in data mode, printed on escape.
    retrain_note: Option<Tier>,
    /// Last retrains, oldest first.
    retrains: [Option<Tier>; HISTORY],
    /// NO CARRIER waits until received data has drained.
    closing: bool,
}

impl Modem {
    pub fn new(cfg: Config) -> Self {
        Modem {
            cfg,
            link: Link::new(),
            mode: Mode::Command,
            out: OutRing {
                buf: [0; OUT_CAP],
                head: 0,
                len: 0,
            },
            line: [0; LINE_CAP],
            line_len: 0,
            line_over: false,
            echo: true,
            verbose: true,
            esc: 0,
            esc_last: 0,
            last_host: 0,
            retrain_note: None,
            retrains: [None; HISTORY],
            closing: false,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.link.is_connected()
    }

    /// Bytes from the host. Command modes always take every byte; data
    /// mode returns only the bytes the call could queue.
    pub fn host_input<R: Radio>(&mut self, bytes: &[u8], radio: &mut R, now_ms: u32) -> usize {
        let mut used = 0;
        while used < bytes.len() {
            let rest = bytes.get(used..).unwrap_or(&[]);
            if self.mode == Mode::Data {
                used += self.data_input(rest, now_ms);
                break;
            }
            if let Some(&b) = rest.first() {
                self.cmd_byte(b, radio, now_ms);
            }
            used += 1;
        }
        self.after(radio, now_ms);
        used
    }

    pub fn radio_rx<R: Radio>(&mut self, frame: &[u8], info: RxInfo, radio: &mut R, now_ms: u32) {
        self.link
            .on_frame(frame, info, name_of(&self.cfg), &self.cfg.sregs, now_ms);
        self.after(radio, now_ms);
    }

    pub fn tick<R: Radio>(&mut self, radio: &mut R, now_ms: u32) {
        if self.mode == Mode::Data
            && self.esc > 0
            && now_ms.wrapping_sub(self.esc_last) >= self.guard()
        {
            if self.esc == 3 {
                self.esc = 0;
                self.mode = Mode::OnlineCommand;
                self.result(ResultCode::Ok);
                if let Some(t) = self.retrain_note.take() {
                    self.result(ResultCode::Retrain(t));
                }
            } else {
                self.flush_esc();
            }
        }
        self.link.tick(radio, &self.cfg.sregs, now_ms);
        self.after(radio, now_ms);
    }

    pub fn host_output(&mut self, out: &mut [u8]) -> usize {
        self.out.pop(out)
    }

    pub fn lights(&self, now_ms: u32) -> Lights {
        let recent = |t: Option<u32>| t.is_some_and(|t| now_ms.wrapping_sub(t) < LIGHT_MS);
        let cd = self.link.is_connected();
        let tier = if cd { self.link.tier() } else { None };
        Lights {
            oh: !self.link.is_idle(),
            cd,
            sd: cd && recent(self.link.last_data_tx()),
            rd: cd && recent(self.link.last_data_rx()),
            hs: tier.is_some_and(|t| t >= Tier::Sf7),
            ec: cd,
            aa: self.cfg.sregs.get(0).unwrap_or(0) > 0,
            tier,
        }
    }

    fn guard(&self) -> u32 {
        u32::from(self.cfg.sregs.get(12).unwrap_or(50)) * 20
    }

    fn esc_char(&self) -> u8 {
        self.cfg.sregs.get(2).unwrap_or(43).min(127) as u8
    }

    /// Push held escape characters as data. True when none are left.
    fn flush_esc(&mut self) -> bool {
        let c = self.esc_char();
        let held = [c; 3];
        let n = self
            .link
            .push_data(held.get(..self.esc as usize).unwrap_or(&[]));
        self.esc = self.esc.saturating_sub(n.min(3) as u8);
        self.esc == 0
    }

    /// Escape rule: S12 guard of silence, three S2 characters each less than
    /// S12 apart, then S12 of silence (checked in tick). Otherwise data.
    fn data_input(&mut self, bytes: &[u8], now: u32) -> usize {
        let guard = self.guard();
        let c = self.esc_char();
        let mut used = 0;
        for &b in bytes {
            if self.esc > 0
                && (b != c || self.esc >= 3 || now.wrapping_sub(self.esc_last) >= guard)
                && !self.flush_esc()
            {
                break;
            }
            let start = self.esc == 0 && now.wrapping_sub(self.last_host) >= guard;
            if b == c && (start || self.esc > 0) {
                self.esc += 1;
                self.esc_last = now;
            } else if self.link.push_data(&[b]) == 0 {
                break;
            }
            self.last_host = now;
            used += 1;
        }
        used
    }

    fn cmd_byte<R: Radio>(&mut self, b: u8, radio: &mut R, now: u32) {
        if self.echo {
            self.out.push(&[b]);
        }
        match b {
            b'\r' => {
                self.run_line(radio, now);
                self.line_len = 0;
                self.line_over = false;
            }
            b'\n' => {}
            8 | 127 => self.line_len = self.line_len.saturating_sub(1),
            _ => match self.line.get_mut(self.line_len) {
                Some(d) => {
                    *d = b;
                    self.line_len += 1;
                }
                None => self.line_over = true,
            },
        }
    }

    fn run_line<R: Radio>(&mut self, radio: &mut R, now: u32) {
        if self.line_len == 0 {
            return;
        }
        let mut copy = [0u8; LINE_CAP];
        let n = self.line_len.min(LINE_CAP);
        if let (Some(d), Some(s)) = (copy.get_mut(..n), self.line.get(..n)) {
            d.copy_from_slice(s);
        }
        let parsed = core::str::from_utf8(copy.get(..n).unwrap_or(&[]))
            .ok()
            .map(parse_line);
        match parsed {
            Some(Ok(cmd)) if !self.line_over => self.exec(cmd, radio, now),
            _ => self.result(ResultCode::Error),
        }
    }

    fn exec<R: Radio>(&mut self, cmd: AtCmd<'_>, radio: &mut R, now: u32) {
        let _ = radio;
        match cmd {
            AtCmd::Attention => self.result(ResultCode::Ok),
            AtCmd::Dial(name) => {
                self.fresh_call();
                if self.mode != Mode::Command || !self.link.dial(name_of(&self.cfg), name, now) {
                    self.result(ResultCode::Error);
                } else if self.verbose {
                    self.info(Txt::new().put(b"DIALING ").put(name.as_bytes()).bytes());
                }
            }
            AtCmd::Answer => {
                self.fresh_call();
                if !self.link.answer(&self.cfg.sregs, now) {
                    self.result(ResultCode::NoCarrier);
                }
            }
            AtCmd::Hangup => {
                if self.link.hangup(now) {
                    self.mode = Mode::Command;
                    self.result(ResultCode::NoCarrier);
                } else {
                    self.result(ResultCode::Ok);
                }
            }
            AtCmd::Online => match self.link.tier() {
                Some(t) if self.link.is_connected() => {
                    self.mode = Mode::Data;
                    self.esc = 0;
                    self.last_host = now;
                    self.result(ResultCode::Connect(t));
                }
                _ => self.result(ResultCode::NoCarrier),
            },
            AtCmd::Info => {
                let mut t = Txt::new();
                t.put(BANNER.as_bytes())
                    .put(b"\r\nv")
                    .put(crate::VERSION.as_bytes());
                t.put(b" ")
                    .put(RELEASE_NAME.as_bytes())
                    .put(b"\r\n")
                    .put(RELEASE_TAGLINE.as_bytes());
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::Verbose(v) => {
                self.verbose = v;
                self.result(ResultCode::Ok);
            }
            AtCmd::Echo(e) => {
                self.echo = e;
                self.result(ResultCode::Ok);
            }
            AtCmd::SetReg(n, v) => {
                let ok = self.cfg.sregs.set(n, v);
                self.result(if ok {
                    ResultCode::Ok
                } else {
                    ResultCode::Error
                });
            }
            AtCmd::GetReg(n) => match self.cfg.sregs.get(n) {
                Some(v) => {
                    self.info(Txt::new().num(u32::from(v), 3).bytes());
                    self.result(ResultCode::Ok);
                }
                None => self.result(ResultCode::Error),
            },
            AtCmd::ShowConfig => {
                let mut t = Txt::new();
                t.put(b"E")
                    .num(u32::from(self.echo), 1)
                    .put(b" V")
                    .num(u32::from(self.verbose), 1);
                t.put(b" NAME ")
                    .put(name_of(&self.cfg).as_bytes())
                    .put(b"\r\n");
                for (i, r) in SRegs::NUMBERS.iter().enumerate() {
                    if i > 0 {
                        t.put(b" ");
                    }
                    let v = self.cfg.sregs.get(*r).unwrap_or(0);
                    t.put(b"S")
                        .num(u32::from(*r), 2)
                        .put(b"=")
                        .num(u32::from(v), 3);
                }
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"TIER ");
                match self.link.tier().filter(|_| self.link.is_connected()) {
                    Some(c) => put_tier(&mut t, c),
                    None => {
                        t.put(b"NONE");
                    }
                }
                t.put(b"\r\nRETRAINS");
                let mut any = false;
                for r in self.retrains.iter().flatten() {
                    t.put(if any { b", " } else { b" " });
                    put_tier(&mut t, *r);
                    any = true;
                }
                if !any {
                    t.put(b" NONE");
                }
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::Reset => {
                self.fresh_call();
                self.link.hangup(now);
                self.mode = Mode::Command;
                self.cfg.sregs = SRegs::new();
                self.echo = true;
                self.verbose = true;
                self.result(ResultCode::Ok);
            }
        }
    }

    fn result(&mut self, code: ResultCode) {
        let mut b = [0u8; 64];
        let n = format_result(&code, self.verbose, &mut b);
        self.out.push(b.get(..n).unwrap_or(&[]));
    }

    /// `\r\n<text>\r\n`.
    fn info(&mut self, text: &[u8]) {
        let mut t = Txt::new();
        t.put(b"\r\n").put(text).put(b"\r\n");
        self.out.push(t.bytes());
    }

    /// Drain link events, move received data to the host, send if possible.
    fn after<R: Radio>(&mut self, radio: &mut R, now: u32) {
        while let Some(e) = self.link.poll_event() {
            match e {
                Event::Ring => self.result(ResultCode::Ring),
                Event::Ringing => {
                    if self.verbose {
                        self.info(b"RINGING");
                    }
                }
                Event::Training(snr) => {
                    if self.verbose {
                        let s = i32::from(snr);
                        let tenths = (s.unsigned_abs() * 10 + 2) / 4;
                        let mut t = Txt::new();
                        t.put(b"TRAINING SNR ").put(if s < 0 { b"-" } else { b"" });
                        t.num(tenths / 10, 1)
                            .put(b".")
                            .num(tenths % 10, 1)
                            .put(b" dB");
                        self.info(t.bytes());
                    }
                }
                Event::Connect(t) => {
                    self.fresh_call();
                    self.mode = Mode::Data;
                    self.esc = 0;
                    self.last_host = now;
                    self.result(ResultCode::Connect(t));
                }
                Event::Retrain(t) => {
                    self.retrains.rotate_left(1);
                    if let Some(last) = self.retrains.last_mut() {
                        *last = Some(t);
                    }
                    if self.mode == Mode::Data {
                        self.retrain_note = Some(t);
                    } else {
                        self.result(ResultCode::Retrain(t));
                    }
                }
                Event::NoCarrier => {
                    // Deliver what fits, then NO CARRIER at once (the
                    // reserve keeps room for it), so it never outlives the call.
                    self.closing = true;
                    self.drain();
                    self.mode = Mode::Command;
                    self.retrain_note = None;
                }
                Event::Busy => self.result(ResultCode::Busy),
                Event::NoAnswer => self.result(ResultCode::NoAnswer),
            }
        }
        if self.mode == Mode::Data {
            self.drain();
        }
        self.link.pump(radio, now);
    }

    /// Move received call data to the host, keeping OUT_RESERVE free. When
    /// `closing`, print NO CARRIER after the data (or as soon as the ring is
    /// full) and clear `closing` before returning.
    fn drain(&mut self) {
        let mut tmp = [0u8; 256];
        loop {
            let room = self.out.free().saturating_sub(OUT_RESERVE).min(tmp.len());
            let n = if room == 0 {
                0
            } else {
                self.link.read_data(tmp.get_mut(..room).unwrap_or(&mut []))
            };
            if n == 0 {
                break;
            }
            self.out.push(tmp.get(..n).unwrap_or(&[]));
        }
        if self.closing {
            self.closing = false;
            self.result(ResultCode::NoCarrier);
        }
    }

    /// A new call or a reset: nothing from the old call may follow.
    fn fresh_call(&mut self) {
        self.closing = false;
        self.retrain_note = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A radio that never hears anything and finishes every send at once.
    struct NullRadio;

    impl Radio for NullRadio {
        fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
        fn listen(&mut self, _tier: Tier) {}
        fn busy(&self) -> bool {
            false
        }
    }

    fn output(m: &mut Modem) -> String {
        let mut all = Vec::new();
        let mut buf = [0u8; 100];
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(all).unwrap()
    }

    #[test]
    fn ati_banner() {
        let mut m = Modem::new(Config::new("CALLER"));
        let mut r = NullRadio;
        assert_eq!(m.host_input(b"ATI\r", &mut r, 0), 4);
        assert_eq!(
            output(&mut m),
            "ATI\r\
             \r\nLORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM\
             \r\nv0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M\
             \r\nS0N1C B00M R4D10 PR0 MAX\r\n\
             \r\nOK\r\n"
        );
        // Lowercase, echo off: the same three lines, then OK.
        m.host_input(b"ATE0\r", &mut r, 10);
        output(&mut m);
        m.host_input(b"ati\r", &mut r, 20);
        let text = output(&mut m);
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines,
            [
                "LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM",
                "v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M",
                "S0N1C B00M R4D10 PR0 MAX",
                "OK",
            ]
        );
        assert!(!text.contains('\u{2014}') && !text.contains("--"));
    }
}
