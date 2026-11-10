//! Radio abstraction. Implemented by the simulator and by firmware.
//!
//! Contract the core relies on:
//! - `busy()` is true while transmitting, and while receiving a frame once
//!   its preamble has been detected (as an SX1262 reports it).
//! - After `send`, the radio does not receive until the core calls `listen`
//!   again; the core calls `listen()` after each send (once the radio is no
//!   longer busy) to return to receive, and never calls `send` or `listen`
//!   while `busy()` is true.
//! - A received frame is handed to the core after its airtime, with the
//!   tier it was heard on in `RxInfo::tier`.

use crate::tier::Tier;

pub trait Radio {
    /// Transmit one frame at the given tier.
    fn send(&mut self, tier: Tier, frame: &[u8]);
    /// Switch to receive on the given tier.
    fn listen(&mut self, tier: Tier);
    /// True while transmitting or receiving a packet.
    fn busy(&self) -> bool;
}

/// Receive metadata delivered with each frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RxInfo {
    pub tier: Tier,
    pub rssi_dbm: i16,
    pub snr_x4: i16,
}
