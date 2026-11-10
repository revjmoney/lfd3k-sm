#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod frame;
pub mod radio;
pub mod tier;

pub use radio::{Radio, RxInfo};
pub use tier::{best_tier, Tier};

pub const VERSION: &str = "0.1.0";
