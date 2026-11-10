//! Host stand-in for the C HAL, scriptable from tests. One global state,
//! reached through `with`. Tests that use it must not run in parallel with
//! each other (hold their own lock, or use one test).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

pub struct FakeHal {
    /// Level `busy()` reports.
    pub busy: bool,
    /// Clock, microseconds. Every `now_us()` call adds `step_us` after reading.
    pub now_us: u64,
    pub step_us: u64,
    /// Last level passed to `fem_tx`.
    pub fem_tx: bool,
    /// Number of `reset()` calls.
    pub resets: u32,
    /// Every SPI transmit buffer, in order.
    pub xfers: Vec<Vec<u8>>,
    /// Replies consumed one per `spi_xfer`: copied into `rx` (zero padded or
    /// cut to its length). When empty, the chip stand-in below answers.
    pub replies: VecDeque<Vec<u8>>,
    /// Notes passed to `lfd3k_hal_panic`.
    pub panics: Vec<Vec<u8>>,

    // A small SX1262 stand-in that answers by opcode whenever `replies` is
    // empty, so tests need not count SPI transfers.
    /// Status byte returned by GetStatus (default 0x20: STDBY_RC).
    pub status: u8,
    /// IRQ register. SetTx sets TxDone; ClearIrqStatus clears bits.
    pub irq: u16,
    /// One byte registers, written by WriteRegister, read by ReadRegister.
    pub regs: BTreeMap<u16, u8>,
    /// GetDeviceErrors word.
    pub dev_errors: u16,
    /// Frame returned by GetRxBufferStatus and ReadBuffer.
    pub rx_payload: Vec<u8>,
    /// GetPacketStatus bytes.
    pub rssi_pkt: u8,
    pub snr_pkt: u8,
    /// SF from the last SetModulationParams.
    pub sf: u8,
    /// True between SetRx and the next SetTx or SetStandby.
    pub rx_mode: bool,
    /// (SF, frame) of every SetTx, oldest first. Tests drain it.
    pub tx_frames: Vec<(u8, Vec<u8>)>,
    /// Every datagram passed to `lfd3k_hal_udp_send`, oldest first.
    pub udp_out: Vec<Vec<u8>>,
    buffer: Vec<u8>,
}

impl FakeHal {
    const fn new() -> Self {
        FakeHal {
            busy: false,
            now_us: 0,
            step_us: 1,
            fem_tx: false,
            resets: 0,
            xfers: Vec::new(),
            replies: VecDeque::new(),
            panics: Vec::new(),
            status: 0x20,
            irq: 0,
            regs: BTreeMap::new(),
            dev_errors: 0,
            rx_payload: Vec::new(),
            rssi_pkt: 0,
            snr_pkt: 0,
            sf: 0,
            rx_mode: false,
            tx_frames: Vec::new(),
            udp_out: Vec::new(),
            buffer: Vec::new(),
        }
    }

    /// Hand the chip a received frame: RX_DONE (with preamble and header
    /// bits, as a real packet raises them).
    pub fn deliver(&mut self, frame: &[u8], rssi_pkt: u8, snr_pkt: u8) {
        self.rx_payload = frame.to_vec();
        self.rssi_pkt = rssi_pkt;
        self.snr_pkt = snr_pkt;
        self.irq |= 0x0002 | 0x0004 | 0x0010;
    }

    fn emulate(&mut self, tx: &[u8], rx: &mut [u8]) {
        let mut put = |i: usize, v: u8| {
            if let Some(d) = rx.get_mut(i) {
                *d = v;
            }
        };
        let b = |i: usize| tx.get(i).copied().unwrap_or(0);
        match b(0) {
            0xC0 => put(1, self.status),
            0x12 => {
                put(2, (self.irq >> 8) as u8);
                put(3, self.irq as u8);
            }
            0x02 => self.irq &= !u16::from_be_bytes([b(1), b(2)]),
            0x13 => {
                put(2, self.rx_payload.len() as u8);
                put(3, 0);
            }
            0x1E => {
                for (i, &v) in self.rx_payload.clone().iter().enumerate() {
                    put(3 + i, v);
                }
            }
            0x14 => {
                put(2, self.rssi_pkt);
                put(3, self.snr_pkt);
            }
            0x17 => {
                put(2, (self.dev_errors >> 8) as u8);
                put(3, self.dev_errors as u8);
            }
            0x0D => {
                self.regs.insert(u16::from_be_bytes([b(1), b(2)]), b(3));
            }
            0x1D => {
                let v = self
                    .regs
                    .get(&u16::from_be_bytes([b(1), b(2)]))
                    .copied()
                    .unwrap_or(0);
                put(4, v);
            }
            0x8B => self.sf = b(1),
            0x0E => self.buffer = tx.get(2..).unwrap_or(&[]).to_vec(),
            0x83 => {
                self.tx_frames.push((self.sf, self.buffer.clone()));
                self.irq |= 0x0001;
                self.rx_mode = false;
            }
            0x82 => self.rx_mode = true,
            0x80 => self.rx_mode = false,
            _ => {}
        }
    }
}

static STATE: Mutex<FakeHal> = Mutex::new(FakeHal::new());

fn lock() -> MutexGuard<'static, FakeHal> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the fake HAL state.
pub fn with<R>(f: impl FnOnce(&mut FakeHal) -> R) -> R {
    f(&mut lock())
}

/// Back to the initial state.
pub fn reset_all() {
    *lock() = FakeHal::new();
}

pub(crate) fn spi_xfer(tx: &[u8], rx: &mut [u8]) {
    let mut s = lock();
    s.xfers.push(tx.to_vec());
    for d in rx.iter_mut() {
        *d = 0;
    }
    match s.replies.pop_front() {
        Some(reply) => {
            for (i, d) in rx.iter_mut().enumerate() {
                *d = reply.get(i).copied().unwrap_or(0);
            }
        }
        None => s.emulate(tx, rx),
    }
}

pub(crate) fn busy() -> bool {
    lock().busy
}

pub(crate) fn reset() {
    lock().resets += 1;
}

pub(crate) fn fem_tx(on: bool) {
    lock().fem_tx = on;
}

pub(crate) fn now_us() -> u64 {
    let mut s = lock();
    let t = s.now_us;
    s.now_us = t.saturating_add(s.step_us);
    t
}

pub(crate) fn udp_send(d: &[u8]) {
    lock().udp_out.push(d.to_vec());
}

/// Used by tests that call the panic note path by hand.
pub fn panic(note: &[u8]) {
    lock().panics.push(note.to_vec());
}
