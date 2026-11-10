//! Host stand-in for the C HAL, scriptable from tests. One global state,
//! reached through `with`. Tests that use it must not run in parallel with
//! each other (hold their own lock, or use one test).

use std::collections::VecDeque;
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
    /// cut to its length). Empty queue replies all zeros.
    pub replies: VecDeque<Vec<u8>>,
    /// Notes passed to `lfd3k_hal_panic`.
    pub panics: Vec<Vec<u8>>,
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
    let reply = s.replies.pop_front().unwrap_or_default();
    for (i, d) in rx.iter_mut().enumerate() {
        *d = reply.get(i).copied().unwrap_or(0);
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

/// Used by tests that call the panic note path by hand.
pub fn panic(note: &[u8]) {
    lock().panics.push(note.to_vec());
}
