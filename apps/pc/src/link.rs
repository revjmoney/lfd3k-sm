//! The link to a real modem: a USB serial port (`AT+VOICE`, then voice
//! host protocol frames as session 0) or a TCP connection to the
//! firmware's voice port (4220; framed from the first byte, sessions 1
//! to 3). `--modem find` looks the modem up by mDNS first ([`crate::find`]).
//!
//! The bytes on the link are the voice host protocol's COBS frames; the
//! call `Engine` makes and reads them. This module only moves bytes,
//! never blocks the modem thread for long, and sends one Leave when the
//! link closes.
//!
//! Serial lines: the V4 (ESP32-S3 USB-Serial/JTAG) and the V3 (CP2102)
//! reset when DTR or RTS change. On Windows both are held low from the
//! first port setting (the `serialport` crate writes DTR and RTS disabled
//! in one `SetCommState` right after the port opens; it has no way to set
//! RTS before that). On Linux both stay raised as the kernel raises them
//! at open. Neither is ever changed afterwards, and never one at a time.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use lfd3k_core::voice::host::{cobs_encode, msg};

/// The firmware's TCP voice port (TXT `voice=4220`).
pub const VOICE_PORT: u16 = 4220;
/// The serial speed of both boards (8N1).
const BAUD: u32 = 115_200;
/// `AT+VOICE` must answer OK within this.
const HANDSHAKE_MS: u64 = 3_000;
/// A TCP connect gives up after this.
const CONNECT_MS: u64 = 5_000;
/// A serial write gives up after this (the link is then lost).
const SERIAL_WRITE_MS: u64 = 500;
/// Most bytes kept for the modem when TCP cannot take them yet; more than
/// this and the link counts as lost.
const TCP_PENDING_MAX: usize = 64 * 1024;

/// Which modem `--modem` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModemSpec {
    /// The app's own simulated modem on SimNet UDP (today's behavior).
    Sim,
    /// A serial port: `COM11`, `/dev/ttyUSB0`.
    Serial(String),
    /// A modem on the network by host and port.
    Tcp(String, u16),
    /// A modem found by mDNS, optionally by its station name.
    Find(Option<String>),
}

/// Read a `--modem` value: `sim`, `COM11`, `/dev/ttyUSB0`,
/// `tcp:HOST[:PORT]` (port 4220 when left out), `find`, `find:STATION`.
pub fn parse_modem_spec(s: &str) -> Result<ModemSpec, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(
            "--modem: empty (want sim, COM11, /dev/ttyUSB0, tcp:HOST[:PORT] or find[:STATION])"
                .into(),
        );
    }
    let lower = s.to_ascii_lowercase();
    if lower == "sim" {
        return Ok(ModemSpec::Sim);
    }
    if lower == "find" {
        return Ok(ModemSpec::Find(None));
    }
    if lower.starts_with("find:") {
        let st = s.get(5..).unwrap_or("").trim();
        if st.is_empty() {
            return Err("--modem find: needs a station name after the colon".into());
        }
        return Ok(ModemSpec::Find(Some(st.to_string())));
    }
    if lower.starts_with("tcp:") {
        let rest = s.get(4..).unwrap_or("").trim();
        return parse_host_port(rest).map(|(h, p)| ModemSpec::Tcp(h, p));
    }
    let com = lower
        .strip_prefix("com")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if com || s.starts_with('/') || s.starts_with(r"\\.\") {
        return Ok(ModemSpec::Serial(s.to_string()));
    }
    Err(format!(
        "--modem {s}: not sim, a serial port (COM11, /dev/ttyUSB0), tcp:HOST[:PORT] or find[:STATION]"
    ))
}

/// `HOST`, `HOST:PORT`, `[V6]` or `[V6]:PORT`.
fn parse_host_port(s: &str) -> Result<(String, u16), String> {
    let bad = || format!("--modem tcp:{s}: want tcp:HOST or tcp:HOST:PORT");
    if s.is_empty() {
        return Err(bad());
    }
    let port = |p: &str| -> Result<u16, String> {
        match p.parse::<u16>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("--modem tcp:{s}: port {p} is not 1 to 65535")),
        }
    };
    if let Some(r) = s.strip_prefix('[') {
        let (host, after) = r.split_once(']').ok_or_else(bad)?;
        if host.is_empty() {
            return Err(bad());
        }
        return match after {
            "" => Ok((host.to_string(), VOICE_PORT)),
            a => Ok((
                host.to_string(),
                port(a.strip_prefix(':').ok_or_else(bad)?)?,
            )),
        };
    }
    match s.split_once(':') {
        None => Ok((s.to_string(), VOICE_PORT)),
        Some((h, p)) if !h.is_empty() && !p.contains(':') => Ok((h.to_string(), port(p)?)),
        _ => Err(bad()),
    }
}

/// Where the `AT+VOICE` answer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeStep {
    /// No final answer yet: read more.
    Need,
    /// OK: this many bytes were the answer; the rest are frames.
    Ok(usize),
    /// The modem said ERROR (or the numeric 4).
    Error(String),
}

/// Read the modem's answer to `AT+VOICE` so far. The echo of the command,
/// blank lines and any other text before the result are skipped; `OK` (or
/// `0` with numeric result codes) is the end; `ERROR` (or `4`) fails. A
/// line ends at CR LF, LF, or (numeric results) a lone CR; a verbose `OK`
/// counts only once its LF is in, so no line end is left for the frames.
pub fn voice_handshake(reply: &[u8]) -> HandshakeStep {
    let mut start = 0usize;
    let mut i = 0usize;
    while let Some(&b) = reply.get(i) {
        if b != b'\r' && b != b'\n' {
            i += 1;
            continue;
        }
        let line = reply.get(start..i).unwrap_or(&[]);
        let text = String::from_utf8_lossy(line);
        let t = text.trim();
        // The end of this line: CR LF is one line end.
        let mut end = i + 1;
        if b == b'\r' {
            match reply.get(i + 1) {
                Some(b'\n') => end = i + 2,
                Some(_) => {}
                // A CR at the end: its LF may still come.
                None if t.eq_ignore_ascii_case("OK") => return HandshakeStep::Need,
                None => {}
            }
        }
        if t.eq_ignore_ascii_case("OK") || t == "0" {
            return HandshakeStep::Ok(end);
        }
        if t.eq_ignore_ascii_case("ERROR") || t == "4" {
            return HandshakeStep::Error("the modem answered ERROR to AT+VOICE".into());
        }
        start = end;
        i = end;
    }
    HandshakeStep::Need
}

/// A modem on a serial port, in framed mode.
pub struct SerialLink {
    port: Box<dyn serialport::SerialPort>,
    name: String,
    /// Frame bytes that came in with the OK.
    early: Vec<u8>,
}

/// A modem on TCP (framed from the first byte).
pub struct TcpLink {
    s: TcpStream,
    addr: SocketAddr,
    /// Bytes the socket did not take yet, sent first next time.
    pending: Vec<u8>,
    /// The station's name when mDNS found it.
    station: Option<String>,
}

impl TcpLink {
    /// Connect to `addr` (no delay, then non-blocking).
    pub fn connect(addr: SocketAddr, timeout_ms: u32) -> Result<TcpLink, String> {
        let t = Duration::from_millis(u64::from(timeout_ms.max(1)));
        let s = TcpStream::connect_timeout(&addr, t)
            .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
        s.set_nodelay(true)
            .and_then(|()| s.set_nonblocking(true))
            .map_err(|e| format!("{addr}: {e}"))?;
        Ok(TcpLink {
            s,
            addr,
            pending: Vec::new(),
            station: None,
        })
    }

    /// Send what is pending, as far as the socket takes it now.
    fn flush(&mut self) -> Result<(), String> {
        while !self.pending.is_empty() {
            match self.s.write(&self.pending) {
                Ok(0) => return Err(format!("{}: the connection closed", self.addr)),
                Ok(n) => {
                    self.pending.drain(..n.min(self.pending.len()));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("{}: {e}", self.addr)),
            }
        }
        Ok(())
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > TCP_PENDING_MAX {
            return Err(format!("{}: the modem stopped taking data", self.addr));
        }
        self.flush()
    }

    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.flush()?;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.s.read(buf) {
                Ok(0) => return Err(format!("{}: the modem closed the connection", self.addr)),
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(0),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("{}: {e}", self.addr)),
            }
        }
    }

    /// Send the last bytes (up to a short wait) and close.
    fn finish(mut self) {
        let until = Instant::now() + Duration::from_millis(500);
        while !self.pending.is_empty() && Instant::now() < until {
            if self.flush().is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = self.s.shutdown(std::net::Shutdown::Both);
    }
}

impl SerialLink {
    /// Open `name` at 115200 8N1 with the board safe line handling, send
    /// `AT+VOICE`, and wait for its OK.
    pub fn open(name: &str) -> Result<SerialLink, String> {
        let b = serialport::new(name, BAUD)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(Duration::from_millis(SERIAL_WRITE_MS));
        // Windows: DTR off as the port opens (RTS goes off in the same
        // port setting with no flow control). Linux: both left raised.
        #[cfg(windows)]
        let b = b.dtr_on_open(false);
        let mut port = b.open().map_err(|e| format!("cannot open {name}: {e}"))?;
        // Old text from before (a boot log, a prompt) is not the answer.
        let _ = port.clear(serialport::ClearBuffer::Input);
        // AT+VOICE is the first thing the modem gets, in one write: with
        // boot to menu (S32) any other first byte opens the menu.
        port.write_all(b"AT+VOICE\r")
            .and_then(|()| port.flush())
            .map_err(|e| format!("{name}: {e}"))?;
        let start = Instant::now();
        let mut reply: Vec<u8> = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = serial_read(&mut *port, &mut buf).map_err(|e| format!("{name}: {e}"))?;
            reply.extend_from_slice(buf.get(..n).unwrap_or(&[]));
            match voice_handshake(&reply) {
                HandshakeStep::Ok(used) => {
                    let early = reply.get(used..).unwrap_or(&[]).to_vec();
                    return Ok(SerialLink {
                        port,
                        name: name.to_string(),
                        early,
                    });
                }
                HandshakeStep::Error(e) => return Err(format!("{name}: {e}")),
                HandshakeStep::Need => {}
            }
            if start.elapsed() >= Duration::from_millis(HANDSHAKE_MS) {
                return Err(format!(
                    "{name}: no OK to AT+VOICE within 3 s (is it an LFD3K-SM modem in command mode?)"
                ));
            }
            if n == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.port
            .write_all(bytes)
            .map_err(|e| format!("{}: {e}", self.name))
    }

    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if !self.early.is_empty() {
            let n = self.early.len().min(buf.len());
            if let (Some(d), Some(s)) = (buf.get_mut(..n), self.early.get(..n)) {
                d.copy_from_slice(s);
            }
            self.early.drain(..n);
            return Ok(n);
        }
        serial_read(&mut *self.port, buf).map_err(|e| format!("{}: {e}", self.name))
    }
}

/// What has come in on the port now, without waiting (0 when nothing).
fn serial_read(port: &mut dyn serialport::SerialPort, buf: &mut [u8]) -> std::io::Result<usize> {
    let ready = port.bytes_to_read().map_err(std::io::Error::from)? as usize;
    if ready == 0 || buf.is_empty() {
        return Ok(0);
    }
    let want = ready.min(buf.len());
    match port.read(buf.get_mut(..want).unwrap_or(&mut [])) {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => Ok(0),
        Err(e) if e.kind() == ErrorKind::Interrupted => Ok(0),
        Err(e) => Err(e),
    }
}

/// A real modem, by USB or TCP.
pub enum ModemLink {
    Serial(SerialLink),
    Tcp(TcpLink),
}

impl ModemLink {
    /// Open the link `spec` names. `timeout_ms` is how long `find` looks
    /// and a TCP connect waits; a serial `AT+VOICE` gets 3 s. `Sim` is not
    /// a link (an error).
    pub fn open(spec: &ModemSpec, timeout_ms: u32) -> Result<ModemLink, String> {
        match spec {
            ModemSpec::Sim => Err("the simulated modem has no link".into()),
            ModemSpec::Serial(name) => SerialLink::open(name).map(ModemLink::Serial),
            ModemSpec::Tcp(host, port) => {
                let addr = (host.as_str(), *port)
                    .to_socket_addrs()
                    .map_err(|e| format!("{host}: {e}"))?
                    .next()
                    .ok_or_else(|| format!("{host}: no address"))?;
                TcpLink::connect(addr, timeout_ms.min(CONNECT_MS as u32)).map(ModemLink::Tcp)
            }
            ModemSpec::Find(station) => {
                let (name, addr) = crate::find::find_modem(station.as_deref(), timeout_ms)?;
                let mut t = TcpLink::connect(addr, CONNECT_MS as u32)?;
                t.station = Some(name);
                Ok(ModemLink::Tcp(t))
            }
        }
    }

    /// Bytes to the modem (whole frames from the engine).
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            ModemLink::Serial(l) => l.send(bytes),
            ModemLink::Tcp(l) => l.send(bytes),
        }
    }

    /// Bytes from the modem now; 0 when nothing came. Never waits.
    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            ModemLink::Serial(l) => l.recv(buf),
            ModemLink::Tcp(l) => l.recv(buf),
        }
    }

    /// For the status line: `USB COM11`, `TCP 192.168.4.1:4220`, with the
    /// station mDNS found.
    pub fn describe(&self) -> String {
        match self {
            ModemLink::Serial(l) => format!("USB {}", l.name),
            ModemLink::Tcp(l) => match &l.station {
                Some(s) => format!("TCP {} ({s})", l.addr),
                None => format!("TCP {}", l.addr),
            },
        }
    }

    /// Leave the session (one Leave frame) and close the link. On USB
    /// the modem goes back to AT commands; on TCP the firmware closes
    /// the connection.
    pub fn close(mut self) {
        let _ = self.send(&leave_frame());
        match self {
            ModemLink::Serial(mut l) => {
                let _ = l.port.flush();
            }
            ModemLink::Tcp(l) => l.finish(),
        }
    }
}

/// The voice host protocol's Leave, framed.
fn leave_frame() -> Vec<u8> {
    let mut b = [0u8; 4];
    let n = cobs_encode(&[msg::LEAVE], &mut b).unwrap_or(0);
    b.get(..n).unwrap_or(&[]).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parse_modem_spec_cases() {
        assert_eq!(parse_modem_spec("sim"), Ok(ModemSpec::Sim));
        assert_eq!(
            parse_modem_spec("COM11"),
            Ok(ModemSpec::Serial("COM11".into()))
        );
        assert_eq!(
            parse_modem_spec("/dev/ttyUSB0"),
            Ok(ModemSpec::Serial("/dev/ttyUSB0".into()))
        );
        assert_eq!(
            parse_modem_spec("tcp:10.1.2.3"),
            Ok(ModemSpec::Tcp("10.1.2.3".into(), VOICE_PORT))
        );
        assert_eq!(
            parse_modem_spec("tcp:host:4300"),
            Ok(ModemSpec::Tcp("host".into(), 4300))
        );
        assert_eq!(parse_modem_spec("find"), Ok(ModemSpec::Find(None)));
        assert_eq!(
            parse_modem_spec("find:AXISMODEM-E4B0"),
            Ok(ModemSpec::Find(Some("AXISMODEM-E4B0".into())))
        );
        assert!(parse_modem_spec("").is_err());
        assert!(parse_modem_spec("tcp:").is_err());
    }

    /// A frame as the modem sends it.
    fn ack_frame() -> Vec<u8> {
        let mut b = [0u8; 40];
        let n = cobs_encode(&[msg::HELLO_ACK, 1, 3, b'R', b'E', b'V'], &mut b).unwrap();
        b[..n].to_vec()
    }

    #[test]
    fn handshake_skips_echo() {
        let frame = ack_frame();
        let mut reply = b"AT+VOICE\r\r\nOK\r\n".to_vec();
        let ok_len = reply.len();
        // Without the frame yet the answer is already complete.
        assert_eq!(voice_handshake(&reply), HandshakeStep::Ok(ok_len));
        reply.extend_from_slice(&frame);
        match voice_handshake(&reply) {
            HandshakeStep::Ok(n) => {
                assert_eq!(n, ok_len);
                assert_eq!(&reply[n..], &frame[..]);
            }
            other => panic!("want Ok, got {other:?}"),
        }
        // Not complete yet: only the echo, or an OK whose LF has not come.
        assert_eq!(voice_handshake(b"AT+VOICE\r\r\n"), HandshakeStep::Need);
        assert_eq!(voice_handshake(b"AT+VOICE\r\r\nOK\r"), HandshakeStep::Need);
    }

    #[test]
    fn handshake_error() {
        assert!(matches!(
            voice_handshake(b"ERROR\r\n"),
            HandshakeStep::Error(_)
        ));
        assert!(matches!(
            voice_handshake(b"AT+VOICE\r\r\nERROR\r\n"),
            HandshakeStep::Error(_)
        ));
    }

    #[test]
    fn tcp_link_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let frame = ack_frame();
        let reply = frame.clone();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut b = [0u8; 64];
            // Read one frame (it ends with a zero), answer, keep it open
            // until the client has read.
            while !got.ends_with(&[0]) {
                let n = s.read(&mut b).unwrap();
                assert!(n > 0, "the client closed early");
                got.extend_from_slice(&b[..n]);
            }
            s.write_all(&reply).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            got
        });
        let mut link = TcpLink::connect(addr, 2_000).unwrap();
        let mut hello = [0u8; 8];
        let n = cobs_encode(&[msg::HELLO, 1], &mut hello).unwrap();
        link.send(&hello[..n]).unwrap();
        let mut rx = Vec::new();
        let mut buf = [0u8; 64];
        let until = Instant::now() + Duration::from_secs(3);
        while rx.len() < frame.len() && Instant::now() < until {
            let n = link.recv(&mut buf).unwrap();
            rx.extend_from_slice(&buf[..n]);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(rx, frame, "the whole HelloAck frame came in");
        assert_eq!(srv.join().unwrap(), hello[..n].to_vec());
    }
}
