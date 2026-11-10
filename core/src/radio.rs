//! Radio abstraction. Implemented by the simulator and by firmware.

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
