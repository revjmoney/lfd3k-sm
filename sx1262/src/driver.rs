//! Interrupt-driven SX1262 LoRa driver implementing `lfd3k_core::Radio`.
//!
//! State machine (kept in fields so `busy()` can take `&self`):
//!
//! - `Idle`: chip in standby (or not initialized). Not busy.
//! - `Tx`: SetTx issued. Busy until TxDone, the Timeout IRQ, or a time
//!   guard of `airtime_us(len) + TX_MARGIN_US` (then forced to standby).
//! - `Rx(Listening)`: continuous receive, nothing heard. Not busy.
//! - `Rx(Preamble)`: PreambleDetected seen. Busy until HeaderValid or the
//!   false-preamble guard of `(preamble + 16)` symbols, which re-arms RX.
//! - `Rx(Header)`: HeaderValid seen. Busy until RxDone, CrcErr, HeaderErr,
//!   the Timeout IRQ, or the max-packet guard `airtime_us(255) + 2 ms`.
//!
//! Every busy state has a time exit, so `busy()` can never stick true.
//! Any BUSY-pin wait over the limit (10 ms at runtime, 50 ms during `init`)
//! sets `failed()` and ends in Idle with the front end in receive position.

use lfd3k_core::{Radio, RxInfo, Tier, CHANNEL_HZ};

use crate::cmd;

/// Hardware access the driver needs. Implemented by the firmware (ESP32-S3
/// SPI and GPIO) and by the PC mock bus in tests.
pub trait Bus {
    /// Full-duplex SPI transfer with NSS low for the whole call.
    /// The driver always passes `tx.len() == rx.len()`.
    fn xfer(&mut self, tx: &[u8], rx: &mut [u8]);
    /// BUSY pin level.
    fn busy(&self) -> bool;
    /// NRST low 1 ms, then high.
    fn reset(&mut self);
    /// GPIO46 PA mode: true while transmitting.
    fn fem(&mut self, tx: bool);
    /// Monotonic microseconds.
    fn now_us(&self) -> u64;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// BUSY stayed high past the limit (10 ms at runtime, 50 ms in `init`).
    BusyTimeout,
    /// GetStatus after init did not report STDBY_RC or STDBY_XOSC.
    NoChip,
    /// A frame did not fit, or a register read back wrong after init.
    Config,
}

/// Longest BUSY wait at runtime before the chip is declared failed.
pub const BUSY_LIMIT_US: u64 = 10_000;
/// Longest BUSY wait during `init` (reset boot, TCXO start plus Calibrate,
/// CalibrateImage all land on the command after them).
pub const INIT_BUSY_LIMIT_US: u64 = 50_000;
/// Backstop on BUSY polls in case `now_us` never advances. Still far above
/// 50 ms of real polling on the ESP32-S3 (each poll takes well over 10 ns).
const BUSY_POLL_CAP: u32 = 5_000_000;
/// Extra time allowed past the computed airtime before a TX is abandoned.
pub const TX_MARGIN_US: u64 = 10_000;
/// Extra time allowed past the longest packet after HeaderValid.
pub const RX_MARGIN_US: u64 = 2_000;
/// Symbols allowed after PreambleDetected beyond the preamble itself.
pub const PREAMBLE_GUARD_EXTRA_SYMBOLS: u64 = 16;
/// Power limits of the SX1262 high power PA, in dBm.
pub const POWER_MIN_DBM: i8 = -9;
pub const POWER_MAX_DBM: i8 = 22;

/// Largest SPI frame: ReadBuffer header (3) plus 255 data bytes.
const SCRATCH: usize = 3 + 255;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RxPhase {
    Listening,
    Preamble { deadline: u64 },
    Header { deadline: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Tx { deadline: u64 },
    Rx { tier: Tier, phase: RxPhase },
}

pub struct Sx1262<B: Bus> {
    bus: B,
    state: State,
    failed: bool,
    ready: bool,
    busy_limit_us: u64,
    power: i8,
    power_dirty: bool,
    cur_tier: Option<Tier>,
    cur_len: Option<u8>,
}

fn clamp_power(p: i8) -> i8 {
    p.clamp(POWER_MIN_DBM, POWER_MAX_DBM)
}

/// LoRa symbol time at BW 500 kHz: 2^SF / 500 kHz = 2^SF * 2 us.
fn symbol_us(tier: Tier) -> u64 {
    (1u64 << u32::from(tier.sf().min(12))) * 2
}

impl<B: Bus> Sx1262<B> {
    pub const fn new(bus: B) -> Self {
        Sx1262 {
            bus,
            state: State::Idle,
            failed: false,
            ready: false,
            busy_limit_us: BUSY_LIMIT_US,
            power: 2,
            power_dirty: false,
            cur_tier: None,
            cur_len: None,
        }
    }

    /// The bus, for inspection.
    pub fn bus(&self) -> &B {
        &self.bus
    }

    /// The bus, for test harnesses that drive pins.
    pub fn bus_mut(&mut self) -> &mut B {
        &mut self.bus
    }

    /// True after a BUSY timeout, a missing chip, or a failed init.
    /// Cleared only by a successful `init`.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Reset and configure the chip. Leaves it in standby; the core's
    /// first `listen` starts receive.
    pub fn init(&mut self, power_dbm: i8) -> Result<(), Error> {
        self.failed = false;
        self.ready = false;
        self.state = State::Idle;
        self.cur_tier = None;
        self.cur_len = None;
        self.power = clamp_power(power_dbm);
        self.power_dirty = false;
        self.busy_limit_us = INIT_BUSY_LIMIT_US;
        let r = self.init_inner();
        self.busy_limit_us = BUSY_LIMIT_US;
        if r.is_err() {
            self.failed = true;
            self.bus.fem(false);
        } else {
            self.ready = true;
        }
        r
    }

    fn init_inner(&mut self) -> Result<(), Error> {
        self.bus.fem(false);
        self.bus.reset();
        self.wait_busy()?;
        self.write(&cmd::set_standby(cmd::STDBY_RC))?;
        self.write(&cmd::set_dio3_as_tcxo(cmd::TCXO_1V8, cmd::TCXO_DELAY_STEPS))?;
        self.write(&cmd::calibrate(cmd::CALIBRATE_ALL))?;
        // TCXO setup flags an XOSC start error before the TCXO is up.
        self.write(&cmd::clear_device_errors())?;
        self.write(&cmd::set_regulator_mode(cmd::REGULATOR_DC_DC))?;
        self.write(&cmd::calibrate_image(cmd::IMAGE_902_928))?;
        self.write(&cmd::set_dio2_as_rf_switch(true))?;
        self.write(&cmd::set_packet_type(cmd::PACKET_TYPE_LORA))?;
        self.write(&cmd::set_rf_frequency(CHANNEL_HZ))?;
        self.write(&cmd::set_pa_config(cmd::PA_CONFIG))?;
        self.write(&cmd::write_register(cmd::REG_OCP, cmd::OCP_140MA))?;
        self.write(&cmd::set_tx_params(self.power, cmd::RAMP_40US))?;
        let [sync_msb, sync_lsb] = cmd::SYNC_WORD;
        self.write(&cmd::write_register(cmd::REG_SYNC_WORD_MSB, sync_msb))?;
        self.write(&cmd::write_register(cmd::REG_SYNC_WORD_LSB, sync_lsb))?;
        self.write(&cmd::set_buffer_base_address(0, 0))?;
        self.write(&cmd::set_rx_tx_fallback_mode(cmd::FALLBACK_STDBY_XOSC))?;
        self.write(&cmd::set_dio_irq_params(cmd::IRQ_USED, cmd::IRQ_USED, 0, 0))?;
        // Errata 15.2: better resistance to antenna mismatch.
        let clamp = self.read_register(cmd::REG_TX_CLAMP)?;
        self.write(&cmd::write_register(cmd::REG_TX_CLAMP, clamp | 0x1E))?;
        // Boosted receive gain: a few dB more sensitivity for a little more current.
        // Written after Calibrate (which would not touch it) and kept until reset.
        self.write(&cmd::write_register(cmd::REG_RX_GAIN, cmd::RX_GAIN_BOOSTED))?;
        self.write(&cmd::clear_irq_status(cmd::IRQ_ALL))?;

        let status = self.get_status()?;
        let mode = cmd::chip_mode(status);
        if !(cmd::MODE_STDBY_RC..=cmd::MODE_STDBY_XOSC).contains(&mode) {
            return Err(Error::NoChip);
        }
        if self.read_register(cmd::REG_SYNC_WORD_MSB)? != sync_msb
            || self.read_register(cmd::REG_SYNC_WORD_LSB)? != sync_lsb
        {
            return Err(Error::Config);
        }
        Ok(())
    }

    /// The chip's device-errors word (GetDeviceErrors, opcode 0x17), for
    /// diagnostics after a failed `init`. Does not clear it.
    pub fn device_errors(&mut self) -> Result<u16, Error> {
        let mut rx = [0u8; 4];
        self.read(&cmd::get_device_errors(), &mut rx)?;
        let [_, _, hi, lo] = rx;
        Ok(u16::from_be_bytes([hi, lo]))
    }

    /// True while a transmission is in progress (SetTx issued, not done).
    pub fn transmitting(&self) -> bool {
        matches!(self.state, State::Tx { .. })
    }

    /// Instantaneous channel RSSI in dBm (GetRssiInst, one 3-byte SPI
    /// transaction). Only valid while receiving: `None` in Tx, Idle, before
    /// init, after a failure, or when BUSY stays high past the runtime
    /// limit (which also fails the driver, as for any command). Does not
    /// touch the receive state.
    pub fn rssi_inst(&mut self) -> Option<i16> {
        if self.failed || !self.ready || !matches!(self.state, State::Rx { .. }) {
            return None;
        }
        let mut rx = [0u8; 3];
        if self.read(&cmd::get_rssi_inst(), &mut rx).is_err() {
            self.abort();
            return None;
        }
        let [_, _, raw] = rx;
        Some(-(i16::from(raw) / 2))
    }

    /// Stop whatever the chip is doing (a transmission is cut off) and put
    /// it in standby with the front end in receive position. Nothing is
    /// sent or heard until the next `send` or `listen`. A BUSY timeout
    /// fails the driver, as for any command.
    pub fn standby(&mut self) {
        self.bus.fem(false);
        if self.failed || !self.ready {
            self.state = State::Idle;
            return;
        }
        if self.write(&cmd::set_standby(cmd::STDBY_XOSC)).is_err() {
            self.abort();
            return;
        }
        self.state = State::Idle;
    }

    /// Set chip output power in dBm, clamped to -9..=22. Applied now when
    /// the chip is idle, otherwise at the next `send`. Before `init` the
    /// value is only stored, and `init(power_dbm)` replaces it: init's
    /// argument wins.
    pub fn set_power(&mut self, power_dbm: i8) -> Result<(), Error> {
        self.power = clamp_power(power_dbm);
        self.power_dirty = true;
        if self.ready && !self.failed && self.state == State::Idle {
            let r = self.write(&cmd::set_tx_params(self.power, cmd::RAMP_40US));
            if r.is_err() {
                self.abort();
                return r;
            }
            self.power_dirty = false;
        }
        Ok(())
    }

    /// Call on every DIO1 edge and before every core tick. Reads and clears
    /// IRQs, applies the false-preamble and max-packet timeouts, and returns
    /// a received frame when one completed.
    pub fn service(&mut self, buf: &mut [u8; 255]) -> Option<(usize, RxInfo)> {
        if self.failed || !self.ready {
            return None;
        }
        match self.state {
            State::Idle => None,
            State::Tx { deadline } => {
                self.service_tx(deadline);
                None
            }
            State::Rx { tier, phase } => self.service_rx(tier, phase, buf),
        }
    }

    fn service_tx(&mut self, deadline: u64) {
        let irq = match self.take_irq() {
            Ok(i) => i,
            Err(_) => {
                self.abort();
                return;
            }
        };
        if irq & (cmd::IRQ_TX_DONE | cmd::IRQ_TIMEOUT) != 0 {
            // Fallback mode already put the chip in STDBY_XOSC.
            self.bus.fem(false);
            self.state = State::Idle;
            return;
        }
        if self.bus.now_us() >= deadline {
            // TX never reported done: force standby so busy() clears.
            let r = self.write(&cmd::set_standby(cmd::STDBY_XOSC));
            self.bus.fem(false);
            self.state = State::Idle;
            if r.is_err() {
                self.abort();
            }
        }
    }

    fn service_rx(
        &mut self,
        tier: Tier,
        phase: RxPhase,
        buf: &mut [u8; 255],
    ) -> Option<(usize, RxInfo)> {
        let irq = match self.take_irq() {
            Ok(i) => i,
            Err(_) => {
                self.abort();
                return None;
            }
        };
        if irq & (cmd::IRQ_CRC_ERR | cmd::IRQ_HEADER_ERR) != 0 {
            self.rearm(tier);
            return None;
        }
        if irq & cmd::IRQ_RX_DONE != 0 {
            self.state = State::Rx {
                tier,
                phase: RxPhase::Listening,
            };
            return match self.read_frame(tier, buf) {
                Ok(Some(f)) => Some(f),
                Ok(None) => None,
                Err(_) => {
                    self.abort();
                    None
                }
            };
        }
        if irq & cmd::IRQ_TIMEOUT != 0 {
            self.rearm(tier);
            return None;
        }
        let now = self.bus.now_us();
        let mut phase = phase;
        if irq & cmd::IRQ_HEADER_VALID != 0 {
            let max = u64::from(tier.airtime_us(255)).saturating_add(RX_MARGIN_US);
            phase = RxPhase::Header {
                deadline: now.saturating_add(max),
            };
        } else if irq & cmd::IRQ_PREAMBLE_DETECTED != 0 && phase == RxPhase::Listening {
            let syms = u64::from(tier.preamble_symbols()) + PREAMBLE_GUARD_EXTRA_SYMBOLS;
            phase = RxPhase::Preamble {
                deadline: now.saturating_add(syms.saturating_mul(symbol_us(tier))),
            };
        }
        let expired = match phase {
            RxPhase::Listening => false,
            RxPhase::Preamble { deadline } | RxPhase::Header { deadline } => now >= deadline,
        };
        if expired {
            self.rearm(tier);
        } else {
            self.state = State::Rx { tier, phase };
        }
        None
    }

    /// Read a completed frame out of the chip.
    fn read_frame(
        &mut self,
        tier: Tier,
        buf: &mut [u8; 255],
    ) -> Result<Option<(usize, RxInfo)>, Error> {
        let mut st = [0u8; 4];
        self.read(&cmd::get_rx_buffer_status(), &mut st)?;
        let [_, _, len, start] = st;
        let len = usize::from(len);

        let mut tx = [0u8; SCRATCH];
        let mut rx = [0u8; SCRATCH];
        let hdr = cmd::read_buffer_header(start);
        let total = hdr.len() + len;
        let txs = tx.get_mut(..total).ok_or(Error::Config)?;
        if let Some(h) = txs.get_mut(..hdr.len()) {
            h.copy_from_slice(&hdr);
        }
        let rxs = rx.get_mut(..total).ok_or(Error::Config)?;
        self.read(txs, rxs)?;
        let data = rxs.get(hdr.len()..total).ok_or(Error::Config)?;
        let dst = buf.get_mut(..len).ok_or(Error::Config)?;
        dst.copy_from_slice(data);

        let mut ps = [0u8; 5];
        self.read(&cmd::get_packet_status(), &mut ps)?;
        let [_, _, rssi_pkt, snr_pkt, _] = ps;
        if len == 0 {
            return Ok(None);
        }
        let info = RxInfo {
            tier,
            rssi_dbm: -(i16::from(rssi_pkt) / 2),
            snr_x4: i16::from(snr_pkt as i8),
        };
        Ok(Some((len, info)))
    }

    /// Restart continuous receive after a dropped or false packet.
    fn rearm(&mut self, tier: Tier) {
        let r = self.rearm_inner();
        match r {
            Ok(()) => {
                self.state = State::Rx {
                    tier,
                    phase: RxPhase::Listening,
                }
            }
            Err(_) => self.abort(),
        }
    }

    fn rearm_inner(&mut self) -> Result<(), Error> {
        self.write(&cmd::set_standby(cmd::STDBY_XOSC))?;
        self.write(&cmd::clear_irq_status(cmd::IRQ_ALL))?;
        self.write(&cmd::set_rx(cmd::RX_CONTINUOUS))
    }

    /// Per-tier modulation setup (only when the tier changes) and packet
    /// length (only when it changes). Chip must be in standby.
    fn configure(&mut self, tier: Tier, len: u8) -> Result<(), Error> {
        if self.cur_tier != Some(tier) {
            self.cur_tier = None;
            self.cur_len = None;
            self.write(&cmd::set_modulation_params(
                tier.sf(),
                cmd::BW_500,
                cmd::CR_4_5,
                cmd::LDRO_OFF,
            ))?;
            self.errata_bw500()?;
            self.cur_tier = Some(tier);
        }
        if self.cur_len != Some(len) {
            self.cur_len = None;
            self.write(&cmd::set_packet_params(
                tier.preamble_symbols(),
                cmd::HEADER_EXPLICIT,
                len,
                cmd::CRC_ON,
                cmd::IQ_STANDARD,
            ))?;
            self.cur_len = Some(len);
        }
        Ok(())
    }

    /// Errata 15.1: register 0x0889 bit 2 cleared for BW 500 kHz.
    fn errata_bw500(&mut self) -> Result<(), Error> {
        let v = self.read_register(cmd::REG_SENSITIVITY)?;
        self.write(&cmd::write_register(cmd::REG_SENSITIVITY, v & !0x04))
    }

    fn send_inner(&mut self, tier: Tier, frame: &[u8]) -> Result<(), Error> {
        let len = u8::try_from(frame.len()).map_err(|_| Error::Config)?;
        self.write(&cmd::set_standby(cmd::STDBY_XOSC))?;
        if self.power_dirty {
            self.write(&cmd::set_tx_params(self.power, cmd::RAMP_40US))?;
            self.power_dirty = false;
        }
        self.configure(tier, len)?;
        // Errata 15.1 is worded "before any packet transmission".
        self.errata_bw500()?;

        let mut tx = [0u8; SCRATCH];
        let hdr = cmd::write_buffer_header(0);
        let total = hdr.len() + frame.len();
        let txs = tx.get_mut(..total).ok_or(Error::Config)?;
        if let Some(h) = txs.get_mut(..hdr.len()) {
            h.copy_from_slice(&hdr);
        }
        if let Some(d) = txs.get_mut(hdr.len()..) {
            d.copy_from_slice(frame);
        }
        self.write(txs)?;

        self.write(&cmd::clear_irq_status(cmd::IRQ_ALL))?;
        self.bus.fem(true);
        self.write(&cmd::set_tx(cmd::TX_NO_TIMEOUT))?;
        let guard = u64::from(tier.airtime_us(frame.len())).saturating_add(TX_MARGIN_US);
        self.state = State::Tx {
            deadline: self.bus.now_us().saturating_add(guard),
        };
        Ok(())
    }

    fn listen_inner(&mut self, tier: Tier) -> Result<(), Error> {
        self.write(&cmd::set_standby(cmd::STDBY_XOSC))?;
        self.configure(tier, 255)?;
        self.write(&cmd::clear_irq_status(cmd::IRQ_ALL))?;
        self.write(&cmd::set_rx(cmd::RX_CONTINUOUS))?;
        self.state = State::Rx {
            tier,
            phase: RxPhase::Listening,
        };
        Ok(())
    }

    /// Give up on the current operation: front end to receive, Idle.
    fn abort(&mut self) {
        self.bus.fem(false);
        self.state = State::Idle;
        self.failed = true;
    }

    /// Read and clear the IRQs that are set (only those bits, so an IRQ
    /// landing between the read and the clear is not lost).
    fn take_irq(&mut self) -> Result<u16, Error> {
        let mut rx = [0u8; 4];
        self.read(&cmd::get_irq_status(), &mut rx)?;
        let [_, _, hi, lo] = rx;
        let irq = u16::from_be_bytes([hi, lo]);
        if irq != 0 {
            self.write(&cmd::clear_irq_status(irq))?;
        }
        Ok(irq)
    }

    fn get_status(&mut self) -> Result<u8, Error> {
        let mut rx = [0u8; 2];
        self.read(&cmd::get_status(), &mut rx)?;
        let [_, status] = rx;
        Ok(status)
    }

    fn read_register(&mut self, addr: u16) -> Result<u8, Error> {
        let mut rx = [0u8; 5];
        self.read(&cmd::read_register(addr), &mut rx)?;
        let [_, _, _, _, value] = rx;
        Ok(value)
    }

    /// Wait for BUSY low, at most `busy_limit_us` (runtime or init limit).
    fn wait_busy(&mut self) -> Result<(), Error> {
        let start = self.bus.now_us();
        let mut polls: u32 = 0;
        while self.bus.busy() {
            polls = polls.saturating_add(1);
            let waited = self.bus.now_us().saturating_sub(start);
            if waited > self.busy_limit_us || polls > BUSY_POLL_CAP {
                self.failed = true;
                return Err(Error::BusyTimeout);
            }
        }
        Ok(())
    }

    /// Command with no reply data.
    fn write(&mut self, tx: &[u8]) -> Result<(), Error> {
        let mut rx = [0u8; SCRATCH];
        let rxs = rx.get_mut(..tx.len()).ok_or(Error::Config)?;
        self.read(tx, rxs)
    }

    /// Command with reply; `rx.len()` must equal `tx.len()`.
    fn read(&mut self, tx: &[u8], rx: &mut [u8]) -> Result<(), Error> {
        if tx.len() != rx.len() {
            return Err(Error::Config);
        }
        self.wait_busy()?;
        self.bus.xfer(tx, rx);
        Ok(())
    }
}

impl<B: Bus> Radio for Sx1262<B> {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        if self.failed || !self.ready || frame.is_empty() || frame.len() > 255 {
            return;
        }
        if self.send_inner(tier, frame).is_err() {
            self.abort();
        }
    }

    fn listen(&mut self, tier: Tier) {
        self.bus.fem(false);
        if self.failed || !self.ready {
            return;
        }
        if self.listen_inner(tier).is_err() {
            self.abort();
        }
    }

    fn busy(&self) -> bool {
        match self.state {
            State::Idle => false,
            State::Tx { .. } => true,
            State::Rx { phase, .. } => phase != RxPhase::Listening,
        }
    }
}
