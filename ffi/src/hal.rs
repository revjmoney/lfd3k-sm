//! The C HAL (SPI, pins, clock, panic store) the firmware provides, wrapped
//! as the `sx1262::Bus` the driver runs on. On the host the same calls land
//! in `fake_hal`.

use lfd3k_core::DatagramPort;
use sx1262::Bus;

#[cfg(target_os = "none")]
mod sys {
    extern "C" {
        fn lfd3k_hal_spi_xfer(tx: *const u8, rx: *mut u8, len: usize);
        fn lfd3k_hal_busy() -> bool;
        fn lfd3k_hal_reset();
        fn lfd3k_hal_fem_tx(on: bool);
        fn lfd3k_hal_now_us() -> u64;
        fn lfd3k_hal_panic(note: *const u8, len: usize);
        fn lfd3k_hal_udp_send(d: *const u8, len: usize);
    }

    pub fn udp_send(d: &[u8]) {
        // SAFETY: `d` is valid for `d.len()` bytes; the HAL copies it into a
        // queue and never keeps the pointer.
        unsafe { lfd3k_hal_udp_send(d.as_ptr(), d.len()) }
    }

    pub fn spi_xfer(tx: &[u8], rx: &mut [u8]) {
        let len = tx.len().min(rx.len());
        // SAFETY: both slices are valid for `len` bytes and distinct (one
        // shared, one exclusive borrow); the HAL only reads `tx` and writes
        // `rx` within `len`.
        unsafe { lfd3k_hal_spi_xfer(tx.as_ptr(), rx.as_mut_ptr(), len) }
    }

    pub fn busy() -> bool {
        // SAFETY: no arguments; the HAL reads a GPIO.
        unsafe { lfd3k_hal_busy() }
    }

    pub fn reset() {
        // SAFETY: no arguments; the HAL pulses the reset pin.
        unsafe { lfd3k_hal_reset() }
    }

    pub fn fem_tx(on: bool) {
        // SAFETY: plain value argument; the HAL drives a GPIO.
        unsafe { lfd3k_hal_fem_tx(on) }
    }

    pub fn now_us() -> u64 {
        // SAFETY: no arguments; the HAL reads the monotonic timer.
        unsafe { lfd3k_hal_now_us() }
    }

    pub fn panic(note: &[u8]) {
        // SAFETY: `note` is valid for `note.len()` bytes; the HAL copies it.
        unsafe { lfd3k_hal_panic(note.as_ptr(), note.len()) }
    }
}

#[cfg(not(target_os = "none"))]
use crate::fake_hal as sys;

/// Zero sized `Bus` over the C HAL.
pub struct HalBus;

impl Bus for HalBus {
    fn xfer(&mut self, tx: &[u8], rx: &mut [u8]) {
        sys::spi_xfer(tx, rx);
    }

    fn busy(&self) -> bool {
        sys::busy()
    }

    fn reset(&mut self) {
        sys::reset();
    }

    fn fem(&mut self, tx: bool) {
        sys::fem_tx(tx);
    }

    fn now_us(&self) -> u64 {
        sys::now_us()
    }
}

/// Zero sized `DatagramPort` over the C HAL's UDP queue (AT+SIMNET).
pub struct HalUdp;

impl DatagramPort for HalUdp {
    fn send(&mut self, d: &[u8]) {
        sys::udp_send(d);
    }

    fn now_us(&self) -> u64 {
        sys::now_us()
    }
}

/// Store a crash note and restart (device only; the panic handler uses it).
#[cfg(target_os = "none")]
pub fn panic(note: &[u8]) {
    sys::panic(note);
}
