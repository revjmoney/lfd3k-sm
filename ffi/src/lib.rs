//! C ABI over `lfd3k-core` and the `sx1262` driver for the ESP-IDF firmware.
//!
//! no_std on the bare-metal device target (xtensa-esp32s3-none-elf). The host
//! build keeps std only so `cargo build --workspace` and the host tests work;
//! there the C HAL is replaced by `fake_hal`. This is the only crate in the
//! workspace allowed to use `unsafe`.
#![cfg_attr(target_os = "none", no_std)]

mod api;
#[cfg(not(target_os = "none"))]
pub mod fake_hal;
mod hal;
pub mod panic;

pub use api::*;
