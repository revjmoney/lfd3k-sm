//! C ABI for the Android voice app (`lfd3k-voice-android`).
//!
//! The firmware crate `lfd3k-ffi` is the ESP-IDF boundary and stays
//! untouched. This crate is the phone boundary: a NativeActivity UI calls
//! these functions, and the call engine (`lfd3k-voice`) plus the modem
//! core stay in Rust. `unsafe` lives only in `abi.rs`, which turns C
//! pointers into Rust borrows.

mod abi;
mod chirp;
mod link;
mod session;
mod udp;
