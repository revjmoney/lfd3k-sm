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

use crate::at::{format_result, parse_line, AtCmd, ResultCode, SRegs, REG_TX_POWER};
use crate::frame::{valid_name, NAME_MAX};
use crate::link::{Event, Link, State};
use crate::radio::{Radio, RxInfo};
use crate::tier::Tier;

/// ATI line 1.
pub const BANNER: &str = "LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM";
/// ATI line 2 is `v` + VERSION + space + RELEASE_NAME.
pub const RELEASE_NAME: &str = "r.420 BL4Z3B4ND ULTR4M0D3M";
/// ATI line 3.
pub const RELEASE_TAGLINE: &str = "S0N1C B00M R4D10 PR0 MAX";

/// Gain of the Heltec V4 GC1109 front end, shown in AT&V.
const GC1109_GAIN_DB: i32 = 12;

const OUT_CAP: usize = 1024;
/// Output space kept free of call data so result codes always fit.
const OUT_RESERVE: usize = 96;
const LINE_CAP: usize = 64;
/// Longest crash note kept for ATI.
pub const NOTE_MAX: usize = 64;
const LIGHT_MS: u32 = 100;
/// Retrains remembered for AT&V.
const HISTORY: usize = 4;

pub struct Config {
    pub name: [u8; 16],
    pub name_len: u8,
    pub sregs: SRegs,
}

impl Config {
    /// Station name with default S registers. `None` unless the name is
    /// 1 to 16 printable ASCII characters (the same rule as a CallReq name),
    /// so a modem can never hold a name it could not dial or answer to.
    pub fn new(name: &str) -> Option<Config> {
        if !valid_name(name) {
            return None;
        }
        let mut c = Config {
            name: [0; NAME_MAX],
            name_len: 0,
            sregs: SRegs::new(),
        };
        let n = name.len();
        let (d, s) = (c.name.get_mut(..n)?, name.as_bytes().get(..n)?);
        d.copy_from_slice(s);
        c.name_len = n as u8;
        Some(c)
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
    b: [u8; 256],
    n: usize,
}

impl Txt {
    fn new() -> Self {
        Txt { b: [0; 256], n: 0 }
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

    /// Signed decimal, no padding.
    fn int(&mut self, v: i32) -> &mut Self {
        if v < 0 {
            self.put(b"-");
        }
        self.num(v.unsigned_abs(), 1)
    }

    /// S register value as `AT S n ?` prints it: zero padded to 3 digits,
    /// S30 signed.
    fn sreg_val(&mut self, n: u8, v: u16) -> &mut Self {
        if n == REG_TX_POWER {
            let s = i32::from(v as i16);
            if s < 0 {
                self.put(b"-");
            }
            self.num(s.unsigned_abs(), 3)
        } else {
            self.num(u32::from(v), 3)
        }
    }

    /// Hexadecimal, 4 digits.
    fn hex16(&mut self, v: u16) -> &mut Self {
        for shift in [12u32, 8, 4, 0] {
            let d = ((v >> shift) & 0xF) as u8;
            self.put(&[if d < 10 { b'0' + d } else { b'A' + d - 10 }]);
        }
        self
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
    /// The held S2 characters can no longer be an escape (more input was
    /// offered after them); they only wait to be sent as data.
    esc_dead: bool,
    last_host: u32,
    /// Latest retrain seen in data mode, printed on escape.
    retrain_note: Option<Tier>,
    /// Last retrains, oldest first.
    retrains: [Option<Tier>; HISTORY],
    /// NO CARRIER waits until received data has drained.
    closing: bool,
    /// `AT+FB` was seen; the firmware answers it.
    fb_request: bool,
    /// Crash note from the last boot (printable ASCII), for ATI.
    note: [u8; NOTE_MAX],
    note_len: usize,
    /// Radio bring-up result for AT&V: (code, device errors word).
    radio_diag: Option<(i8, u16)>,
    /// Metadata of the last frame handed to `radio_rx`.
    last_rx: Option<RxInfo>,
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
            esc_dead: false,
            last_host: 0,
            retrain_note: None,
            retrains: [None; HISTORY],
            closing: false,
            fb_request: false,
            note: [0; NOTE_MAX],
            note_len: 0,
            radio_diag: None,
            last_rx: None,
        }
    }

    /// Remember the previous boot's crash note for ATI. Only printable
    /// ASCII is kept (anything else becomes `?`), at most `NOTE_MAX` bytes.
    pub fn set_last_panic(&mut self, note: &[u8]) {
        self.note_len = 0;
        for (d, &c) in self.note.iter_mut().zip(note.iter()) {
            *d = if (0x20..0x7F).contains(&c) { c } else { b'?' };
            self.note_len += 1;
        }
    }

    /// Radio bring-up result for AT&V: `code` 0 is OK, negative is the
    /// driver error (-1 busy timeout, -2 no chip, -3 config); `errs` is the
    /// chip's device-errors word.
    pub fn set_radio_diag(&mut self, code: i8, errs: u16) {
        self.radio_diag = Some((code, errs));
    }

    /// True once after `AT+FB` was entered (clears the flag).
    pub fn take_fb_request(&mut self) -> bool {
        core::mem::take(&mut self.fb_request)
    }

    /// Metadata of the last received frame, if any.
    pub fn last_rx(&self) -> Option<RxInfo> {
        self.last_rx
    }

    /// Current tier while a call is up or training.
    pub fn tier(&self) -> Option<Tier> {
        self.link.tier()
    }

    /// Call state for the status light panel: 0 idle, 1 dialing, 2 ringing,
    /// 3 training, 4 online (data mode), 5 command-online.
    pub fn call_state(&self) -> u8 {
        match self.link.state() {
            State::Idle => 0,
            State::Dialing | State::AwaitTrain => 1,
            State::Ringing => 2,
            State::AwaitTierSet | State::AwaitTierAck | State::Probe => 3,
            State::Connected | State::Resync => {
                if self.mode == Mode::Data {
                    4
                } else {
                    5
                }
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.link.is_connected()
    }

    /// Bytes from the host. Command modes always take every byte; data
    /// mode returns only the bytes the call could queue.
    pub fn host_input<R: Radio>(&mut self, bytes: &[u8], radio: &mut R, now_ms: u32) -> usize {
        // A completed escape whose closing guard has run out is an escape
        // even if no tick ran since: the bytes after it are commands.
        if !bytes.is_empty() {
            self.escape_timer(now_ms);
        }
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
        self.last_rx = Some(info);
        self.link
            .on_frame(frame, info, name_of(&self.cfg), &self.cfg.sregs, now_ms);
        self.after(radio, now_ms);
    }

    pub fn tick<R: Radio>(&mut self, radio: &mut R, now_ms: u32) {
        self.escape_timer(now_ms);
        self.link.tick(radio, &self.cfg.sregs, now_ms);
        self.after(radio, now_ms);
    }

    /// Held escape characters: release dead ones as data; once the closing
    /// guard has passed, three are the escape (OK) and fewer are data.
    fn escape_timer(&mut self, now_ms: u32) {
        if self.mode != Mode::Data || self.esc == 0 {
            return;
        }
        if self.esc_dead {
            self.flush_esc();
        } else if now_ms.wrapping_sub(self.esc_last) >= self.guard() {
            if self.esc == 3 {
                self.esc = 0;
                self.esc_dead = false;
                self.mode = Mode::OnlineCommand;
                self.result(ResultCode::Ok);
                if let Some(t) = self.retrain_note.take() {
                    self.result(ResultCode::Retrain(t));
                }
            } else {
                self.flush_esc();
            }
        }
    }

    pub fn host_output(&mut self, out: &mut [u8]) -> usize {
        self.out.pop(out)
    }

    /// S30: SX1262 chip output power in dBm (-9 to 22).
    pub fn tx_power_dbm(&self) -> i8 {
        let v = self.cfg.sregs.get(REG_TX_POWER).unwrap_or(2) as i16;
        v.clamp(-9, 22) as i8
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
        let done = self.flush_held();
        if done {
            self.esc_dead = false;
        }
        done
    }

    fn flush_held(&mut self) -> bool {
        let c = self.esc_char();
        let held = [c; 3];
        let n = self
            .link
            .push_data(held.get(..self.esc as usize).unwrap_or(&[]));
        self.esc = self.esc.saturating_sub(n.min(3) as u8);
        self.esc == 0
    }

    /// Escape rule: S12 guard of silence, three S2 characters each less than
    /// S12 apart, then S12 of silence (checked by `escape_timer`, from tick
    /// and at the start of the next host input). Otherwise data.
    ///
    /// Any non-empty offer counts as host activity for the guard, even if
    /// the call refuses the bytes (send window full). Held S2 characters
    /// followed by any further offered byte are released as data.
    fn data_input(&mut self, bytes: &[u8], now: u32) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        let guard = self.guard();
        let c = self.esc_char();
        let quiet_before = now.wrapping_sub(self.last_host) >= guard;
        self.last_host = now;
        let mut used = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if self.esc > 0
                && (self.esc_dead
                    || b != c
                    || self.esc >= 3
                    || now.wrapping_sub(self.esc_last) >= guard)
            {
                self.esc_dead = true;
                if !self.flush_esc() {
                    break;
                }
            }
            let start = self.esc == 0 && i == 0 && quiet_before;
            if b == c && (start || self.esc > 0) {
                self.esc += 1;
                self.esc_last = now;
            } else if self.link.push_data(&[b]) == 0 {
                break;
            }
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
                // Already in a call (online command mode): nothing to
                // answer, and the call stays up.
                if self.mode != Mode::Command || self.link.is_connected() {
                    self.result(ResultCode::Error);
                    return;
                }
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
                    self.esc_dead = false;
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
                if self.note_len > 0 {
                    t.put(b"\r\nLAST CRASH: ")
                        .put(self.note.get(..self.note_len).unwrap_or(&[]));
                }
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::FrameBuffer => self.fb_request = true,
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
                    let mut t = Txt::new();
                    t.sreg_val(n, v);
                    self.info(t.bytes());
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
                    t.put(b"S").num(u32::from(*r), 2).put(b"=");
                    t.sreg_val(*r, v);
                }
                self.info(t.bytes());
                let dbm = self.tx_power_dbm();
                let mut t = Txt::new();
                t.put(b"TX POWER S30=").int(i32::from(dbm));
                t.put(b" dBm chip, about ")
                    .int(i32::from(dbm) + GC1109_GAIN_DB)
                    .put(b" dBm out");
                self.info(t.bytes());
                if let Some((code, errs)) = self.radio_diag {
                    let mut t = Txt::new();
                    if code == 0 {
                        t.put(b"RADIO OK");
                    } else {
                        t.put(b"RADIO FAIL ").int(i32::from(code));
                        t.put(b" ERR 0x").hex16(errs);
                    }
                    self.info(t.bytes());
                }
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
                    self.esc_dead = false;
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
        // A partial command line typed before the call (during RING or
        // while dialing) must not be glued onto the first command after it.
        self.line_len = 0;
        self.line_over = false;
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
        let mut m = Modem::new(Config::new("CALLER").unwrap());
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

    #[test]
    fn config_name_is_checked() {
        // Valid: 1 to 16 printable ASCII characters, kept as given.
        for name in ["A", "AXIS", "CALLER", "sixteen chars 16", " ~!", "a b"] {
            let c = Config::new(name).expect(name);
            assert_eq!(name_of(&c), name);
            assert_eq!(c.sregs, SRegs::new());
        }
        // Invalid: empty, too long, control characters, non-ASCII. No
        // truncation, no panic.
        for name in [
            "",
            "seventeen chars 17",
            "AXIS\r",
            "TAB\tNAME",
            "\u{7f}",
            "caf\u{e9}",
            "\u{1F4FB}",
        ] {
            assert!(Config::new(name).is_none(), "{name:?} accepted");
        }
    }
}
