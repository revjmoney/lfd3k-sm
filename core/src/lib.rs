#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod arq;
pub mod at;
pub mod directory;
pub mod discovery;
pub mod frame;
pub mod link;
pub mod lz;
pub mod menu;
pub mod modem;
pub mod radio;
pub mod settings;
pub mod simnet;
pub mod telnet;
pub mod tier;
pub mod wire;

pub use arq::{ArqRx, ArqTx};
pub use at::{format_result, parse_line, AtCmd, AtError, ModeTarget, ResultCode, SRegs};
pub use directory::{DirEntry, Directory, DIR_BLOB_LEN, DIR_MAX};
pub use link::{Event, Link};
pub use modem::{Config, Lights, Modem};
pub use radio::{Radio, RxInfo};
pub use settings::BeaconMode;
pub use settings::{Forward, Settings, WireMode, BLOB_LEN, MAX_FWD};
pub use simnet::{DatagramPort, SimNetRadio};
pub use tier::{best_tier, Tier};
pub use wire::WireReq;

pub const VERSION: &str = "0.1.0";

/// Calling channel: 918.5 MHz (SX1262 frequency word 0x3968_0000).
pub const CHANNEL_HZ: u32 = 918_500_000;
