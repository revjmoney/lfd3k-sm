//! A `DatagramPort` over a non-blocking `UdpSocket`.
//!
//! This is the same port the PC app uses (`lfd3k-sim`'s `UdpPort`): datagrams
//! go to `peer`, and `recv` keeps only those from the peer's IP. It lives
//! here so the phone crate does not link the simulator binary's terminal
//! crate.

use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use lfd3k_core::DatagramPort;

pub struct UdpPort {
    sock: UdpSocket,
    peer: SocketAddr,
    start: Instant,
}

impl UdpPort {
    /// Binds `bind` (for example `0.0.0.0:4210`), non-blocking.
    pub fn bind(bind: &str, peer: SocketAddr) -> std::io::Result<UdpPort> {
        let sock = UdpSocket::bind(bind)?;
        sock.set_nonblocking(true)?;
        Ok(UdpPort {
            sock,
            peer,
            start: Instant::now(),
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// The next datagram from the peer into `buf`, if one is waiting.
    /// Datagrams from other addresses are read and dropped.
    pub fn recv(&self, buf: &mut [u8]) -> Option<usize> {
        loop {
            match self.sock.recv_from(buf) {
                Ok((n, from)) if from.ip() == self.peer.ip() => return Some(n),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    /// Milliseconds since the port was made (the clock `now_us` uses).
    pub fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

impl DatagramPort for UdpPort {
    fn send(&mut self, d: &[u8]) {
        let _ = self.sock.send_to(d, self.peer);
    }

    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}
