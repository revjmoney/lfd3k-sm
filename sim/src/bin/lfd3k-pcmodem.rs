//! The PC as the far modem: a real `Modem` on a simulated radio over UDP
//! (`SimNetRadio` with `UdpPort`), for calling the V4 after `AT+SIMNET`.
//!
//! `cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer <ip> [options]`
//!
//! - `--peer <ip>`: the V4 (or another pcmodem). Required.
//! - `--port <n>`: the peer's UDP port (default 4210).
//! - `--bind <addr:port>`: our UDP socket (default `0.0.0.0:4210`).
//! - `--name <NAME>`: our station name (default `PCMODEM`).
//! - `--snr <dB>`: what the peer hears from us (default 8).
//! - `--profile dip`: that SNR drops 12 dB from 15 s to 35 s into each call.
//! - `--fwd NAME=host:port[,T|R]`: a Wi-Fi forward, set with `AT+FWD` at
//!   start (repeatable), so `ATDT NAME` or `AT+PUB=NAME` work at once. No
//!   mode: telnet (`T`) for port 23, raw (`R`) otherwise.
//! - `--script <file> [--section <name>]`: unattended run (see below).
//! - `--raw`: raw console (see below). Not with `--script`.
//!
//! Terminal, default: line mode. Each line typed is sent with a CR; a line
//! that is exactly the escape (`+++`) is sent without one, so wait a second
//! before and after it. A line ending in `\` is sent without the `\` and
//! without a CR (hotkeys for a BBS menu: `m\` is just `m`). Host output goes to stdout as is (UTF-8 where valid,
//! other bytes as CP437).
//!
//! Terminal, `--raw` (the `crossterm` crate): every key goes to the modem as
//! it is pressed, as a VT100 terminal would send it, so `AT+MENU` works:
//! arrows `ESC [ A..D`, Esc, Enter as CR, Backspace as BS, Tab, Home and
//! End, Delete as DEL, Ctrl+letter as its control code (Ctrl+C included).
//! Ctrl+] quits and puts the console back. Host output is shown as CP437
//! (the menu's box drawing), with ANSI processing switched on.
//!
//! Discovery: the loop feeds every received frame to `radio_rx` and calls
//! `tick` every millisecond, so this modem beacons (privacy mode ON, every
//! 300 to 360 s), answers scans and runs `AT+SCAN` over SimNet like any
//! other. Its directory lives only in memory (no blob is saved).
//!
//! Wi-Fi calls and bridges: the modem's wire requests are served with a real
//! `TcpStream` (non-blocking, polled in the same loop), following the
//! platform contract in `core/src/wire.rs`.
//!
//! Script: one command per line, `#` comments, `[name]` starts a section
//! (with `--section name` only lines before the first section and in that
//! section run; without it, all lines run):
//! - `send <text>`: types the text and a CR;
//! - `sendraw <text>`: types the text with no CR (for `+++`);
//! - `expect <text> <timeout s>`: waits until the text shows in the host
//!   output or in the bytes received over the wire, after the last match;
//! - `sleep <ms>`.
//!
//! Everything is logged with timestamps (`>>` typed, `<<` host output, `<w`
//! wire bytes, `*` notes); exit 0 when the script ends, 1 on a
//! failed expect.

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use lfd3k_core::{Config, Modem, SimNetRadio};
use lfd3k_sim::UdpPort;

const DIP_START_MS: u32 = 15_000;
const DIP_LEN_MS: u32 = 20_000;
const DIP_DB: i16 = 12;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Most bytes kept for a socket that is not taking them; past this the
/// modem's own wire ring and backpressure hold the rest.
const WIRE_OUT_CAP: usize = 64 * 1024;

struct Args {
    peer: IpAddr,
    port: u16,
    bind: String,
    name: String,
    snr_db: i16,
    dip: bool,
    fwds: Vec<String>,
    script: Option<String>,
    section: Option<String>,
    raw: bool,
}

fn usage() -> String {
    "usage: lfd3k-pcmodem --peer <ip> [--port <n>] [--bind <addr:port>] [--name <NAME>] \
     [--snr <dB>] [--profile dip] [--fwd NAME=host:port[,T|R]]... [--script <file> [--section <name>]] \
     [--raw]"
        .to_string()
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        peer: IpAddr::from([0, 0, 0, 0]),
        port: lfd3k_core::simnet::PORT,
        bind: format!("0.0.0.0:{}", lfd3k_core::simnet::PORT),
        name: "PCMODEM".to_string(),
        snr_db: 8,
        dip: false,
        fwds: Vec::new(),
        script: None,
        section: None,
        raw: false,
    };
    let mut peer = None;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || {
            it.next()
                .ok_or_else(|| format!("{k} needs a value\n{}", usage()))
        };
        match k.as_str() {
            "--peer" => peer = Some(v()?.parse::<IpAddr>().map_err(|e| format!("--peer: {e}"))?),
            "--port" => a.port = v()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--bind" => a.bind = v()?,
            "--name" => a.name = v()?,
            "--snr" => a.snr_db = v()?.parse().map_err(|e| format!("--snr: {e}"))?,
            "--profile" => match v()?.as_str() {
                "dip" => a.dip = true,
                p => return Err(format!("unknown profile {p} (only dip)")),
            },
            "--fwd" => a.fwds.push(v()?),
            "--script" => a.script = Some(v()?),
            "--section" => a.section = Some(v()?),
            "--raw" => a.raw = true,
            "-h" | "--help" => return Err(usage()),
            _ => return Err(format!("unknown option {k}\n{}", usage())),
        }
    }
    a.peer = peer.ok_or_else(usage)?;
    if a.raw && a.script.is_some() {
        return Err(format!(
            "--raw and --script do not go together\n{}",
            usage()
        ));
    }
    Ok(a)
}

// ---- Raw console (--raw). ----

/// Puts the console back in its normal mode when dropped (every exit path).
struct RawGuard;

impl RawGuard {
    fn enable() -> std::io::Result<RawGuard> {
        crossterm::terminal::enable_raw_mode()?;
        // Windows: let the console act on the menu's ANSI sequences.
        #[cfg(windows)]
        let _ = crossterm::ansi_support::supports_ansi();
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The bytes a VT100 terminal sends for a key; `None` for Ctrl+] (quit).
/// Keys with no byte form give an empty vector.
fn key_bytes(k: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Char(']') if ctrl => return None,
        KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_uppercase() as u8) & 0x1F]
        }
        KeyCode::Char('[') if ctrl => vec![0x1B],
        KeyCode::Char(c) if ctrl && (c == ' ' || c == '@') => vec![0],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x08],
        KeyCode::Delete => vec![0x7F],
        KeyCode::Esc => vec![0x1B],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        _ => Vec::new(),
    })
}

/// Key presses (and repeats; Windows also reports releases, which are
/// skipped) as bytes on the channel; an empty vector means quit.
fn raw_keys(tx: mpsc::Sender<Vec<u8>>) {
    use crossterm::event::{read, Event, KeyEventKind};
    loop {
        let Ok(ev) = read() else {
            let _ = tx.send(Vec::new());
            return;
        };
        let Event::Key(k) = ev else { continue };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        match key_bytes(&k) {
            None => {
                let _ = tx.send(Vec::new());
                return;
            }
            Some(b) if b.is_empty() => {}
            Some(b) => {
                if tx.send(b).is_err() {
                    return;
                }
            }
        }
    }
}

/// `NAME=host:port[,T|R]` as the `AT+FWD` line (no mode: telnet for port
/// 23, raw otherwise, as `AT+FWD` decides).
fn fwd_line(spec: &str) -> Option<String> {
    let (name, rest) = spec.split_once('=')?;
    let (hp, mode) = match rest.split_once(',') {
        Some((hp, m)) if m.eq_ignore_ascii_case("T") || m.eq_ignore_ascii_case("R") => {
            (hp, format!(",{m}"))
        }
        Some(_) => return None,
        None => (rest, String::new()),
    };
    let (host, port) = hp.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some(format!("AT+FWD={name},{host},{port}{mode}\r"))
}

// ---- Wire: the modem's TCP connection. ----

enum Wire {
    Idle,
    Connecting(Receiver<std::io::Result<TcpStream>>),
    Open {
        s: TcpStream,
        /// Received bytes the modem has not taken yet.
        pend: Vec<u8>,
        /// Bytes from `wire_output` not yet written.
        out: Vec<u8>,
        eof: bool,
    },
}

fn connect(host: String, port: u16) -> Receiver<std::io::Result<TcpStream>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| {
            let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no address");
            for a in (host.as_str(), port).to_socket_addrs()? {
                match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                    Ok(s) => return Ok(s),
                    Err(e) => last = e,
                }
            }
            Err(last)
        })();
        let _ = tx.send(r);
    });
    rx
}

/// Writes what it can of `out` without blocking. False on a broken socket.
fn write_some(s: &mut TcpStream, out: &mut Vec<u8>) -> bool {
    while !out.is_empty() {
        match s.write(out) {
            Ok(0) => return false,
            Ok(n) => {
                out.drain(..n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    true
}

/// One poll of the platform side of the wire. `got` collects the bytes
/// received from the socket (for `expect` and the log).
fn wire_service(w: &mut Wire, m: &mut Modem, now: u32, got: &mut Vec<u8>, log: &mut Log) {
    let mut buf = [0u8; 1024];
    // 1. Close first: it always refers to the socket that exists.
    if m.take_wire_close() {
        if let Wire::Open { s, out, .. } = w {
            loop {
                let n = m.wire_output(&mut buf);
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            let _ = s.set_nonblocking(false);
            let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = s.write_all(out);
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if !matches!(w, Wire::Idle) {
            log.note(now, "wire closed (modem asked)");
        }
        // A connect still in progress is abandoned (its socket is dropped).
        *w = Wire::Idle;
    }
    // 2. A request opens a new socket.
    if let Some(req) = m.take_wire_request() {
        log.note(
            now,
            &format!("wire connecting to {}:{}", req.host(), req.port()),
        );
        *w = Wire::Connecting(connect(req.host().to_string(), req.port()));
    }
    // 3. Connect result.
    if let Wire::Connecting(rx) = w {
        match rx.try_recv() {
            Ok(Ok(s)) => {
                let _ = s.set_nonblocking(true);
                let _ = s.set_nodelay(true);
                log.note(now, "wire connected");
                *w = Wire::Open {
                    s,
                    pend: Vec::new(),
                    out: Vec::new(),
                    eof: false,
                };
                m.wire_connected(now);
            }
            Ok(Err(e)) => {
                log.note(now, &format!("wire connect failed: {e}"));
                *w = Wire::Idle;
                m.wire_failed(now);
            }
            Err(TryRecvError::Disconnected) => {
                *w = Wire::Idle;
                m.wire_failed(now);
            }
            Err(TryRecvError::Empty) => {}
        }
    }
    // 4. Move bytes.
    let mut closed = false;
    if let Wire::Open { s, pend, out, eof } = w {
        while out.len() < WIRE_OUT_CAP {
            let n = m.wire_output(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        if !write_some(s, out) {
            *eof = true;
        }
        if pend.is_empty() && !*eof {
            match s.read(&mut buf) {
                Ok(0) => *eof = true,
                Ok(n) => {
                    pend.extend_from_slice(&buf[..n]);
                    got.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => *eof = true,
            }
        }
        if !pend.is_empty() {
            let k = m.wire_input(pend, now).min(pend.len());
            pend.drain(..k);
        }
        // Every received byte is delivered before the close is reported.
        closed = *eof && pend.is_empty();
    }
    if closed {
        log.note(now, "wire closed by the far end");
        *w = Wire::Idle;
        m.wire_closed(now);
    }
}

// ---- Log and script. ----

struct Log {
    script: bool,
    /// `--raw`: host bytes as CP437, notes end in CR LF.
    raw: bool,
    line: Vec<u8>,
    /// Start of a UTF-8 sequence cut at the end of the last host chunk.
    utf8_tail: Vec<u8>,
}

/// CP437 upper half (0x80 to 0xFF).
const CP437_HI: &str = "\
ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»\
░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

/// One byte as CP437 (ASCII below 0x80).
fn cp437(c: u8) -> char {
    if c < 0x80 {
        char::from(c)
    } else {
        CP437_HI.chars().nth(usize::from(c - 0x80)).unwrap_or('?')
    }
}

impl Log {
    fn stamp(now: u32) -> String {
        format!("[{:>4}.{:03}]", now / 1000, now % 1000)
    }

    fn note(&mut self, now: u32, s: &str) {
        if self.script {
            println!("{} *  {s}", Self::stamp(now));
        } else if self.raw {
            eprint!("[pcmodem] {s}\r\n"); // raw mode: no newline translation
        } else {
            eprintln!("[pcmodem] {s}");
        }
    }

    /// Host output: raw on the terminal, one stamped line per line in a script.
    fn host(&mut self, now: u32, b: &[u8]) {
        if !self.script {
            // Raw: every byte as CP437, since the menu's box corners and
            // lines (CD BB, C4 BF, ...) are also valid two byte UTF-8.
            let text = if self.raw {
                b.iter().map(|&c| cp437(c)).collect()
            } else {
                self.decode(b)
            };
            let mut o = std::io::stdout();
            let _ = o.write_all(text.as_bytes());
            let _ = o.flush();
            return;
        }
        for &c in b {
            match c {
                b'\n' | b'\r' => self.flush(now),
                c => self.line.push(c),
            }
        }
    }

    /// Terminal text: UTF-8 as sent, any other byte read as CP437 (BBS
    /// screens), since the Windows console refuses invalid UTF-8. A UTF-8
    /// sequence cut at the end of `b` waits for the next call.
    fn decode(&mut self, b: &[u8]) -> String {
        let mut all = std::mem::take(&mut self.utf8_tail);
        all.extend_from_slice(b);
        let mut out = String::new();
        let mut rest: &[u8] = &all;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(good).unwrap_or(""));
                    match e.error_len() {
                        None => {
                            self.utf8_tail = bad.to_vec();
                            break;
                        }
                        Some(n) => {
                            out.extend(bad[..n].iter().map(|&c| cp437(c)));
                            rest = &bad[n..];
                        }
                    }
                }
            }
        }
        out
    }

    fn flush(&mut self, now: u32) {
        if !self.line.is_empty() {
            println!(
                "{} << {}",
                Self::stamp(now),
                String::from_utf8_lossy(&self.line)
            );
            self.line.clear();
        }
    }
}

enum Step {
    Send(Vec<u8>),
    Expect(String, u32),
    Sleep(u32),
}

fn load_script(path: &str, section: Option<&str>) -> Result<Vec<Step>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut steps = Vec::new();
    let mut cur: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            cur = Some(name.trim().to_string());
            continue;
        }
        if let (Some(want), Some(c)) = (section, cur.as_deref()) {
            if want != c {
                continue;
            }
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let bad = || format!("{path}:{}: bad line: {line}", i + 1);
        steps.push(match cmd {
            "send" => Step::Send(format!("{rest}\r").into_bytes()),
            "sendraw" => Step::Send(rest.as_bytes().to_vec()),
            "sleep" => Step::Sleep(rest.trim().parse().map_err(|_| bad())?),
            "expect" => {
                let (t, secs) = rest.rsplit_once(' ').ok_or_else(bad)?;
                let secs: f64 = secs.parse().map_err(|_| bad())?;
                Step::Expect(t.trim().to_string(), (secs * 1000.0) as u32)
            }
            _ => return Err(bad()),
        });
    }
    Ok(steps)
}

struct Runner {
    steps: VecDeque<Step>,
    /// When the current step started.
    since: Option<u32>,
    /// Host output and wire bytes seen so far, and where the next expect
    /// starts looking in each.
    host: Vec<u8>,
    host_at: usize,
    wire: Vec<u8>,
    wire_at: usize,
}

fn find(hay: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let h = hay.get(from..)?;
    if pat.is_empty() {
        return Some(from);
    }
    h.windows(pat.len())
        .position(|w| w == pat)
        .map(|p| p + from)
}

enum Run {
    Going,
    Done,
    Failed,
}

impl Runner {
    fn step(&mut self, now: u32, input: &mut VecDeque<u8>, log: &mut Log) -> Run {
        loop {
            let Some(s) = self.steps.front() else {
                return Run::Done;
            };
            let since = *self.since.get_or_insert(now);
            match s {
                Step::Send(b) => {
                    log.flush(now);
                    println!(
                        "{} >> {}",
                        Log::stamp(now),
                        String::from_utf8_lossy(b).trim_end_matches('\r')
                    );
                    input.extend(b.iter().copied());
                }
                Step::Sleep(ms) => {
                    if now.wrapping_sub(since) < *ms {
                        return Run::Going;
                    }
                }
                Step::Expect(t, ms) => {
                    let pat = t.as_bytes();
                    if let Some(p) = find(&self.host, pat, self.host_at) {
                        self.host_at = p + pat.len();
                    } else if let Some(p) = find(&self.wire, pat, self.wire_at) {
                        self.wire_at = p + pat.len();
                    } else if now.wrapping_sub(since) >= *ms {
                        log.flush(now);
                        println!(
                            "{} !! expect \"{t}\" timed out after {ms} ms",
                            Log::stamp(now)
                        );
                        return Run::Failed;
                    } else {
                        return Run::Going;
                    }
                    log.flush(now);
                    println!("{} ok expect \"{t}\"", Log::stamp(now));
                }
            }
            self.steps.pop_front();
            self.since = None;
        }
    }
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let Some(cfg) = Config::new(&a.name) else {
        eprintln!("--name must be 1 to 16 printable characters, no colon");
        return ExitCode::from(2);
    };
    let peer = SocketAddr::new(a.peer, a.port);
    let port = match UdpPort::bind(&a.bind, peer) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot bind {}: {e}", a.bind);
            return ExitCode::from(2);
        }
    };
    let mut runner = match &a.script {
        Some(p) => match load_script(p, a.section.as_deref()) {
            Ok(steps) => Some(Runner {
                steps: steps.into(),
                since: None,
                host: Vec::new(),
                host_at: 0,
                wire: Vec::new(),
                wire_at: 0,
            }),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let mut log = Log {
        script: runner.is_some(),
        raw: a.raw,
        line: Vec::new(),
        utf8_tail: Vec::new(),
    };
    // Held for the whole run: dropping it (any return) restores the console.
    let _raw_guard = if a.raw {
        match RawGuard::enable() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("cannot put the console in raw mode: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    let lines = if a.raw {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || raw_keys(tx));
        Some(rx)
    } else if runner.is_none() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            for l in std::io::stdin().lock().lines() {
                let Ok(l) = l else { break };
                let l = l.trim_end_matches(['\r', '\n']).to_string();
                let mut b = l.into_bytes();
                if b.last() == Some(&b'\\') {
                    b.pop(); // "x\": the keys alone, no CR (a BBS hotkey menu)
                } else if b != b"+++" {
                    b.push(b'\r');
                }
                if tx.send(b).is_err() {
                    break;
                }
            }
        });
        Some(rx)
    } else {
        None
    };

    let mut modem = Modem::new(cfg);
    let mut radio = SimNetRadio::new(port);
    let base_x4 = a.snr_db.saturating_mul(4);
    radio.set_peer_hears(base_x4, -78 + a.snr_db);
    log.note(
        radio.port().now_ms(),
        &format!(
            "{} on {} (simulated radio), peer {peer}, peer hears {} dB{}",
            a.name,
            a.bind,
            a.snr_db,
            if a.dip { ", profile dip" } else { "" }
        ),
    );
    if a.raw {
        log.note(
            radio.port().now_ms(),
            "raw console: keys go straight to the modem, Ctrl+] quits",
        );
    }

    let mut input: VecDeque<u8> = VecDeque::new();
    for f in &a.fwds {
        match fwd_line(f) {
            Some(l) => input.extend(l.into_bytes()),
            None => {
                eprintln!("--fwd wants NAME=host:port[,T|R], got {f}");
                return ExitCode::from(2);
            }
        }
    }

    let mut wire = Wire::Idle;
    let mut conn_since: Option<u32> = None;
    let mut buf = [0u8; 1024];
    let mut frame = [0u8; 255];
    let mut out = [0u8; 512];
    loop {
        let now = radio.port().now_ms();

        // Datagrams in, then frames whose airtime has passed.
        while let Some(n) = radio.port().recv(&mut buf) {
            radio.on_datagram(&buf[..n]);
        }
        while let Some((n, info)) = radio.service(&mut frame) {
            modem.radio_rx(&frame[..n], info, &mut radio, now);
        }

        // What the peer hears of us.
        let st = modem.call_state();
        if st == 4 || st == 5 {
            conn_since.get_or_insert(now);
        } else if st == 0 {
            conn_since = None;
        }
        let dip = a.dip
            && conn_since.is_some_and(|c| {
                let t = now.wrapping_sub(c);
                (DIP_START_MS..DIP_START_MS + DIP_LEN_MS).contains(&t)
            });
        let db = if dip { a.snr_db - DIP_DB } else { a.snr_db };
        radio.set_peer_hears(db.saturating_mul(4), -78 + db);

        // Host input: typed lines or the script.
        if let Some(rx) = &lines {
            while let Ok(b) = rx.try_recv() {
                if a.raw && b.is_empty() {
                    log.note(now, "Ctrl+] pressed, bye");
                    return ExitCode::SUCCESS; // the guard restores the console
                }
                input.extend(b);
            }
        }
        if let Some(r) = &mut runner {
            match r.step(now, &mut input, &mut log) {
                Run::Going => {}
                Run::Done => {
                    log.flush(now);
                    println!("{} == script done", Log::stamp(now));
                    return ExitCode::SUCCESS;
                }
                Run::Failed => return ExitCode::from(1),
            }
        }
        if !input.is_empty() {
            // Command lines one at a time (as the firmware feeds them).
            let (head, _) = input.as_slices();
            let end = if modem.in_data_mode() {
                head.len()
            } else {
                head.iter()
                    .position(|&c| c == b'\r')
                    .map_or(head.len(), |i| i + 1)
            };
            let seg: Vec<u8> = head[..end].to_vec();
            let k = modem.host_input(&seg, &mut radio, now).min(seg.len());
            input.drain(..k);
        }

        modem.tick(&mut radio, now);

        let mut got = Vec::new();
        wire_service(&mut wire, &mut modem, now, &mut got, &mut log);
        if let Some(r) = &mut runner {
            r.wire.extend_from_slice(&got);
            if !got.is_empty() {
                let text: String = String::from_utf8_lossy(&got)
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .collect();
                for l in text.lines().filter(|l| !l.trim().is_empty()) {
                    println!("{} <w {}", Log::stamp(now), l.trim_end());
                }
            }
        }

        loop {
            let n = modem.host_output(&mut out);
            if n == 0 {
                break;
            }
            if let Some(r) = &mut runner {
                r.host.extend_from_slice(&out[..n]);
            }
            log.host(now, &out[..n]);
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}
