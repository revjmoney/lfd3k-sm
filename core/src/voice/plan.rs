//! The talk plan: codec, talk mode and voice frame length for each tier
//! (spec 8.2). Every size is computed here so the apps and the modem agree.
//!
//! Frame lengths are the guide values of the spec; none had to be shortened
//! to stay under 249 sealed bytes (the type 15 sub-type byte takes one of the
//! 250 payload bytes). The longest is SF12: 2000 ms of 700C is 200 bytes,
//! 220 sealed.
//!
//! Codec frame sizes: Opus is constant bit rate, so a frame is rate x time
//! (an Opus frame longer than 100 ms is two 100 ms packets back to back).
//! Codec2 frames are whole bytes each (3200: 20 ms in 8 bytes, 2400: 20 ms
//! in 6, 1300: 40 ms in 7, 1200: 40 ms in 6, 700C: 40 ms in 4) and a voice
//! frame is several of them concatenated.
//!
//! SF8 does not pass `duplex_fits` with 1300 bps voice (two 61 byte frames
//! plus two turn gaps take 184 ms of 200), so SF8 is push-to-talk with 400 ms
//! frames (70 bytes, 90 sealed). SF6 does
//! not pass it with 8 kbps Opus either (two 226 byte frames plus two gaps take
//! 186 ms of 200), so SF6 uses Codec2 3200.

use crate::frame::{HEADER_LEN, MAX_PAYLOAD};
use crate::link::{air_ms, turn_gap_ms};
use crate::tier::Tier;

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CodecId {
    Opus12k = 1,
    Opus8k = 2,
    C2_3200 = 3,
    C2_2400 = 4,
    C2_1300 = 5,
    C2_1200 = 6,
    C2_700C = 7,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TalkMode {
    Duplex,
    Ptt,
    PttDelayed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TalkPlan {
    pub codec: CodecId,
    pub mode: TalkMode,
    pub frame_ms: u16,
    pub max_sealed_bytes: u8,
}

/// 4 byte counter plus 16 byte tag.
pub const SEAL_OVERHEAD: usize = 20;

/// Bytes of one type 15 payload left for the sealed voice packet: the
/// sub-type byte takes one.
const MAX_SEALED: usize = MAX_PAYLOAD - 1;

/// Encoded bytes of one voice frame of `frame_ms` (before sealing).
pub fn encoded_bytes(codec: CodecId, frame_ms: u16) -> usize {
    let ms = usize::from(frame_ms);
    match codec {
        CodecId::Opus12k => 12_000 * ms / 8000,
        CodecId::Opus8k => 8_000 * ms / 8000,
        CodecId::C2_3200 => ms / 20 * 8,
        CodecId::C2_2400 => ms / 20 * 6,
        CodecId::C2_1300 => ms / 40 * 7,
        CodecId::C2_1200 => ms / 40 * 6,
        CodecId::C2_700C => ms / 40 * 4,
    }
}

/// Both directions fit in one slot: two frames on air plus two turn gaps take
/// at most 80 % of `frame_ms`, which leaves the rest for control, keepalives
/// and probes. `sealed_bytes` is the sealed voice packet; the type 15 frame
/// adds the 5 byte header and the sub-type byte.
pub fn duplex_fits(t: Tier, sealed_bytes: usize, frame_ms: u16) -> bool {
    let air = air_ms(t, HEADER_LEN + 1 + sealed_bytes);
    let used = air
        .saturating_mul(2)
        .saturating_add(turn_gap_ms(t).saturating_mul(2));
    used.saturating_mul(10) <= u32::from(frame_ms) * 8
}

fn plan(codec: CodecId, mode: TalkMode, frame_ms: u16) -> TalkPlan {
    let sealed = encoded_bytes(codec, frame_ms) + SEAL_OVERHEAD;
    TalkPlan {
        codec,
        mode,
        frame_ms,
        max_sealed_bytes: sealed.min(MAX_SEALED) as u8,
    }
}

/// The talk plan for a tier (spec 8.2).
pub fn talk_plan(t: Tier) -> TalkPlan {
    let fits = |codec: CodecId, ms: u16| {
        duplex_fits(t, encoded_bytes(codec, ms) + SEAL_OVERHEAD, ms)
    };
    match t {
        Tier::Sf5 => plan(CodecId::Opus12k, TalkMode::Duplex, 100),
        Tier::Sf6 => {
            if fits(CodecId::Opus8k, 200) {
                plan(CodecId::Opus8k, TalkMode::Duplex, 200)
            } else {
                plan(CodecId::C2_3200, TalkMode::Duplex, 200)
            }
        }
        Tier::Sf7 => plan(CodecId::C2_2400, TalkMode::Duplex, 200),
        Tier::Sf8 => {
            if fits(CodecId::C2_1300, 200) {
                plan(CodecId::C2_1300, TalkMode::Duplex, 200)
            } else {
                plan(CodecId::C2_1300, TalkMode::Ptt, 400)
            }
        }
        Tier::Sf9 | Tier::Sf10 => plan(CodecId::C2_1200, TalkMode::Ptt, 400),
        Tier::Sf11 => plan(CodecId::C2_700C, TalkMode::Ptt, 1000),
        Tier::Sf12 => plan(CodecId::C2_700C, TalkMode::PttDelayed, 2000),
    }
}
