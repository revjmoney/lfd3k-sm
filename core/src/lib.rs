#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod tier;

pub use tier::{best_tier, Tier};

pub const VERSION: &str = "0.1.0";
