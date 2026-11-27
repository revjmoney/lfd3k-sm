//! The link to a real modem: TCP to the firmware's voice port (4220, framed
//! from the first byte) or a byte pipe the NDK fills from USB host bulk
//! endpoints. `find` looks the modem up by mDNS first, the same
//! `_lfd3k._tcp` service the PC app browses.
//!
//! USB itself (permission, CDC-ACM line coding, CP210x setup, bulk
//! transfers) stays in the NDK. This module only moves bytes. On close it
//! sends one Leave frame.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lfd3k_core::voice::host::{cobs_encode, msg};

/// The firmware's TCP voice port (TXT `voice=4220`).
pub const VOICE_PORT: u16 = 4220;
/// `AT+VOICE` must answer OK within this (both tries).
const HANDSHAKE_MS: u64 = 9_000;
/// No OK by now: `AT+VOICE` once more. A session left framed takes the
/// first `AT+VOICE` as frame bytes; that session times out after 5 s.
const HANDSHAKE_RESEND_MS: u64 = 5_500;
const VOICE_CMD: &[u8] = b"AT+VOICE\r";
const CLOSED_HINT: &str = "the modem closed the connection (3 voice apps already connected, or this phone is not on the modem's network)";
const CONNECT_MS: u64 = 5_000;
const TCP_PENDING_MAX: usize = 64 * 1024;
const FIND_MS: u32 = 5_000;
const SERVICE: &str = "_lfd3k._tcp.local.";

/// Bytes the USB pump and the session thread share.
pub struct UsbPipe {
    rx: Mutex<VecDeque<u8>>,
    tx: Mutex<VecDeque<u8>>,
    /// The UI is leaving: `AT+VOICE` should stop waiting.
    cancel: AtomicBool,
}

impl UsbPipe {
    pub fn new() -> Arc<UsbPipe> {
        Arc::new(UsbPipe {
            rx: Mutex::new(VecDeque::new()),
            tx: Mutex::new(VecDeque::new()),
            cancel: AtomicBool::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn push_rx(&self, b: &[u8]) {
        let mut q = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(b);
        if q.len() > 256 * 1024 {
            let drop_n = q.len() - 128 * 1024;
            q.drain(..drop_n);
        }
    }

    pub fn pop_rx(&self, buf: &mut [u8]) -> usize {
        let mut q = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let n = buf.len().min(q.len());
        for (i, b) in q.drain(..n).enumerate() {
            if let Some(d) = buf.get_mut(i) {
                *d = b;
            }
        }
        n
    }

    pub fn clear_rx(&self) {
        self.rx.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn push_tx(&self, b: &[u8]) {
        let mut q = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(b);
        if q.len() > TCP_PENDING_MAX {
            let drop_n = q.len() - TCP_PENDING_MAX / 2;
            q.drain(..drop_n);
        }
    }

    pub fn pop_tx(&self, buf: &mut [u8]) -> usize {
        let mut q = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let n = buf.len().min(q.len());
        for (i, b) in q.drain(..n).enumerate() {
            if let Some(d) = buf.get_mut(i) {
                *d = b;
            }
        }
        n
    }

    /// Put bytes back at the front (a short USB write).
    pub fn unget_tx(&self, b: &[u8]) {
        let mut q = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        for &x in b.iter().rev() {
            q.push_front(x);
        }
    }
}

/// Where the `AT+VOICE` answer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeStep {
    Need,
    Ok(usize),
    Error(String),
}

/// Read the modem's answer to `AT+VOICE` so far. Echo, blank lines and any
/// other text before the result are skipped. `OK` (or numeric `0`) ends
/// it. `ERROR` (or numeric `4`) fails.
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
        let mut end = i + 1;
        if b == b'\r' {
            match reply.get(i + 1) {
                Some(b'\n') => end = i + 2,
                Some(_) => {}
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

fn voice_wait(pipe: &UsbPipe) -> Result<Vec<u8>, String> {
    pipe.push_tx(VOICE_CMD);
    let start = Instant::now();
    let mut resent = false;
    let mut reply: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = pipe.pop_rx(&mut buf);
        reply.extend_from_slice(buf.get(..n).unwrap_or(&[]));
        match voice_handshake(&reply) {
            HandshakeStep::Ok(used) => return Ok(reply.get(used..).unwrap_or(&[]).to_vec()),
            HandshakeStep::Error(e) => return Err(e),
            HandshakeStep::Need => {}
        }
        let t = start.elapsed();
        if pipe.cancelled() {
            return Err("closed while waiting for AT+VOICE".into());
        }
        if t >= Duration::from_millis(HANDSHAKE_MS) {
            return Err(
                "no OK to AT+VOICE within 9 s. If the app just quit, the modem may still be in voice mode: wait 5 s and try again. Otherwise, is it an LFD3K-SM modem in command mode?"
                    .into(),
            );
        }
        if !resent && t >= Duration::from_millis(HANDSHAKE_RESEND_MS) {
            pipe.clear_rx();
            reply.clear();
            pipe.push_tx(VOICE_CMD);
            resent = true;
            continue;
        }
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn pick_addr(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let mut first = None;
    for a in addrs {
        if a.is_ipv4() {
            return Some(a);
        }
        first.get_or_insert(a);
    }
    first
}

pub struct TcpLink {
    s: TcpStream,
    pub addr: SocketAddr,
    pending: Vec<u8>,
    pub station: Option<String>,
}

impl TcpLink {
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
                Ok(0) => return Err(format!("{}: {CLOSED_HINT}", self.addr)),
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(0),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                    ) =>
                {
                    return Err(format!("{}: {CLOSED_HINT}", self.addr))
                }
                Err(e) => return Err(format!("{}: {e}", self.addr)),
            }
        }
    }

    pub fn describe(&self) -> String {
        match &self.station {
            Some(s) => format!("TCP {} ({s})", self.addr),
            None => format!("TCP {}", self.addr),
        }
    }

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

pub struct UsbLink {
    pipe: Arc<UsbPipe>,
    early: Vec<u8>,
    label: String,
}

impl UsbLink {
    pub fn open(pipe: Arc<UsbPipe>, label: String) -> Result<UsbLink, String> {
        let early = voice_wait(&pipe).map_err(|e| format!("USB: {e}"))?;
        Ok(UsbLink { pipe, early, label })
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pipe.push_tx(bytes);
        Ok(())
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
        Ok(self.pipe.pop_rx(buf))
    }

    pub fn describe(&self) -> String {
        if self.label.is_empty() {
            "USB".to_string()
        } else {
            format!("USB {}", self.label)
        }
    }
}

pub enum RealLink {
    Tcp(TcpLink),
    Usb(UsbLink),
}

impl RealLink {
    pub fn open_tcp(host: &str, port: u16) -> Result<RealLink, String> {
        let addrs = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("{host}: {e}"))?;
        let addr = pick_addr(addrs).ok_or_else(|| format!("{host}: no address"))?;
        TcpLink::connect(addr, CONNECT_MS as u32).map(RealLink::Tcp)
    }

    pub fn open_find(station: Option<&str>) -> Result<RealLink, String> {
        let (name, addr) = find_modem(station, FIND_MS)?;
        let mut t = TcpLink::connect(addr, CONNECT_MS as u32)?;
        t.station = Some(name);
        Ok(RealLink::Tcp(t))
    }

    pub fn open_usb(pipe: Arc<UsbPipe>, label: String) -> Result<RealLink, String> {
        UsbLink::open(pipe, label).map(RealLink::Usb)
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            RealLink::Tcp(l) => l.send(bytes),
            RealLink::Usb(l) => l.send(bytes),
        }
    }

    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            RealLink::Tcp(l) => l.recv(buf),
            RealLink::Usb(l) => l.recv(buf),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            RealLink::Tcp(l) => l.describe(),
            RealLink::Usb(l) => l.describe(),
        }
    }

    pub fn close(mut self) {
        let leave = leave_frame();
        let _ = self.send(&leave);
        match self {
            RealLink::Tcp(l) => l.finish(),
            RealLink::Usb(_) => std::thread::sleep(Duration::from_millis(150)),
        }
    }
}

pub fn leave_frame() -> Vec<u8> {
    let mut b = [0u8; 8];
    let n = cobs_encode(&[msg::LEAVE], &mut b).unwrap_or(0);
    b.get(..n).unwrap_or(&[]).to_vec()
}

fn voice_port_from_txt(txt: &[(&str, &str)]) -> Option<u16> {
    let (_, v) = txt.iter().find(|(k, _)| k.eq_ignore_ascii_case("voice"))?;
    match v.trim().parse::<u16>() {
        Ok(p) if p > 0 => Some(p),
        _ => None,
    }
}

fn station_from_txt(txt: &[(&str, &str)]) -> Option<String> {
    txt.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("name"))
        .map(|(_, v)| v.trim().to_string())
}

/// Browse for up to `timeout_ms` and give the first modem with a voice
/// port, or the one whose station name is `station`.
pub fn find_modem(station: Option<&str>, timeout_ms: u32) -> Result<(String, SocketAddr), String> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    let mdns = ServiceDaemon::new().map_err(|e| format!("mDNS: {e}"))?;
    let rx = mdns
        .browse(SERVICE)
        .map_err(|e| format!("mDNS browse: {e}"))?;
    let until = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let mut seen: Vec<String> = Vec::new();
    let mut found = None;
    while found.is_none() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(ev) = rx.recv_timeout(left) else {
            break;
        };
        let ServiceEvent::ServiceResolved(s) = ev else {
            continue;
        };
        let props: Vec<(String, String)> = s
            .txt_properties
            .iter()
            .map(|p| (p.key().to_string(), p.val_str().to_string()))
            .collect();
        let txt: Vec<(&str, &str)> = props
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let name = station_from_txt(&txt).unwrap_or_default();
        let Some(port) = voice_port_from_txt(&txt) else {
            seen.push(format!("{name} (no voice port)"));
            continue;
        };
        if station.is_some_and(|want| !want.eq_ignore_ascii_case(&name)) {
            seen.push(name);
            continue;
        }
        let mut v4: Vec<_> = s
            .addresses
            .iter()
            .map(|a| a.to_ip_addr())
            .filter_map(|a| match a {
                IpAddr::V4(v) => Some(v),
                IpAddr::V6(_) => None,
            })
            .collect();
        v4.sort();
        let Some(ip) = v4.first().copied() else {
            seen.push(format!("{name} (no IPv4 address)"));
            continue;
        };
        found = Some((name, SocketAddr::from((ip, port))));
    }
    let _ = mdns.stop_browse(SERVICE);
    let _ = mdns.shutdown();
    found.ok_or_else(|| {
        let what = station.map_or_else(
            || "no LFD3K-SM modem with a voice port".to_string(),
            |s| format!("no LFD3K-SM modem named {s} with a voice port"),
        );
        if seen.is_empty() {
            format!("{what} found on the network in {} s", timeout_ms / 1000)
        } else {
            format!(
                "{what} found in {} s (heard: {})",
                timeout_ms / 1000,
                seen.join(", ")
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_skips_echo_and_takes_ok() {
        let r = b"AT+VOICE\r\n\r\nOK\r\n";
        assert_eq!(voice_handshake(r), HandshakeStep::Ok(r.len()));
        assert_eq!(voice_handshake(b"OK"), HandshakeStep::Need);
        assert_eq!(
            voice_handshake(b"ERROR\r\n"),
            HandshakeStep::Error("the modem answered ERROR to AT+VOICE".into())
        );
        let early = b"OK\r\n\x01\x02";
        match voice_handshake(early) {
            HandshakeStep::Ok(n) => assert_eq!(&early[n..], &[1, 2]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn voice_port_txt() {
        assert_eq!(voice_port_from_txt(&[("voice", "4220")]), Some(4220));
        assert_eq!(voice_port_from_txt(&[("VOICE", " 4220 ")]), Some(4220));
        assert_eq!(voice_port_from_txt(&[("name", "REV")]), None);
        assert_eq!(voice_port_from_txt(&[("voice", "0")]), None);
    }
}
