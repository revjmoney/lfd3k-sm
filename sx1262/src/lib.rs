//! SX1262 LoRa driver for LFD3K-SM (Heltec V4).
//!
//! Written fresh against the SX1261/2 datasheet. Implements the core's
//! `Radio` trait over a small `Bus` trait so it runs on the ESP32-S3 and on
//! a PC mock bus.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod cmd;
mod driver;

pub use driver::{
    Bus, Error, Sx1262, BUSY_LIMIT_US, POWER_MAX_DBM, POWER_MIN_DBM, PREAMBLE_GUARD_EXTRA_SYMBOLS,
    RX_MARGIN_US, TX_MARGIN_US,
};
