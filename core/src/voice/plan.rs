//! The talk plan: codec, talk mode and voice frame length for each tier
//! (spec 8.2). Every size is computed here so the apps and the modem agree.
//!
//! Duplex only where it fits (`duplex_fits`, applied to every tier): both
//! voice frames on air plus the scheduler's slot guard after each
//! (`SLOT_GUARD_MS`, 20 ms) leave at least 20 % of `frame_ms` free. A voice
//! frame on air is the voice scheduler's Data frame: header, sub-type byte,
//! `DATA_HEAD` (3) body bytes and the sealed packet. The guard replaced the
//! data turn gap (40 ms and up): slots need no carrier sense. The 20 ms
//! guard assumes the 10 ms core tick and a quick TX to RX turnaround; the
//! firmware part must measure it on hardware.
//!
//! | Tier | Codec | Mode | frame_ms | encoded | sealed | both ways + guards |
//! |---|---|---|---|---|---|---|
//! | SF5 | Opus 10 kbps | Duplex | 120 | 150 | 170 | 92 of 96 ms allowed |
//! | SF6 | Opus 8 kbps | Duplex | 200 | 200 | 220 | 146 of 160 |
//! | SF7 | Codec2 2400 | Duplex | 200 | 60 | 80 | 118 of 160 |
//! | SF8 | Codec2 1300 | Duplex | 200 | 35 | 55 | 148 of 160 |
//! | SF9, SF10 | Codec2 1200 | Push to talk | 400 | 60 | 80 | |
//! | SF11 | Codec2 700C | Push to talk | 1000 | 100 | 120 | |
//! | SF12 | Codec2 700C | Push to talk, delayed | 2000 | 200 | 220 | |
//!
//! SF5: 12 kbps Opus in 100 ms frames does not fit (about 102 of 80 ms with
//! guards), 10 kbps in 120 ms does. Each tier falls back to the next
//! smaller choice if its first does not fit (SF5: Opus 8 kbps, then Codec2
//! 3200; SF6: Codec2 3200; SF8: push to talk with 400 ms frames). Sealed
//! sizes stay at most 249 bytes (the sub-type byte takes one of 250).
//!
//! Codec frame sizes: Opus is constant bit rate, so a frame is rate x time
//! (an Opus frame longer than 100 ms is several equal packets back to back:
//! 100 ms packets when the frame is a multiple of 100 ms, else 20 ms ones).
//! Codec2 frames are whole bytes each (3200: 20 ms in 8 bytes, 2400: 20 ms
//! in 6, 1300: 40 ms in 7, 1200: 40 ms in 6, 700C: 40 ms in 4) and a voice
//! frame is several of them concatenated.

use crate::frame::{HEADER_LEN, MAX_PAYLOAD};
use crate::link::air_ms;
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
    Opus10k = 8,
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

/// Silence the voice scheduler keeps after every frame in a slot (covers
/// the 10 ms tick and the radio's turnaround; to be measured on hardware).
pub const SLOT_GUARD_MS: u32 = 20;
/// Voice scheduler Data body bytes before the sealed packet: flags, late,
/// SACK (`voice::sched`).
pub const DATA_HEAD: usize = 3;

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
        CodecId::Opus10k => 10_000 * ms / 8000,
        CodecId::Opus8k => 8_000 * ms / 8000,
        CodecId::C2_3200 => ms / 20 * 8,
        CodecId::C2_2400 => ms / 20 * 6,
        CodecId::C2_1300 => ms / 40 * 7,
        CodecId::C2_1200 => ms / 40 * 6,
        CodecId::C2_700C => ms / 40 * 4,
    }
}

/// Both directions fit in one slot: two Data frames on air (header,
/// sub-type byte, `DATA_HEAD` and the `sealed_bytes` packet) plus the slot
/// guard after each take at most 80 % of `frame_ms`, which leaves the rest
/// for control, keepalives and probes.
pub fn duplex_fits(t: Tier, sealed_bytes: usize, frame_ms: u16) -> bool {
    let air = air_ms(t, HEADER_LEN + 1 + DATA_HEAD + sealed_bytes);
    let used = air.saturating_add(SLOT_GUARD_MS).saturating_mul(2);
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

/// The talk plan for a tier (spec 8.2): the first choice that passes
/// `duplex_fits` on the duplex tiers, push to talk on SF9 and slower.
pub fn talk_plan(t: Tier) -> TalkPlan {
    let fits =
        |codec: CodecId, ms: u16| duplex_fits(t, encoded_bytes(codec, ms) + SEAL_OVERHEAD, ms);
    let duplex = |choices: &[(CodecId, u16)], fallback: TalkPlan| {
        choices
            .iter()
            .find(|(c, ms)| fits(*c, *ms))
            .map_or(fallback, |&(c, ms)| plan(c, TalkMode::Duplex, ms))
    };
    match t {
        Tier::Sf5 => duplex(
            &[
                (CodecId::Opus10k, 120),
                (CodecId::Opus8k, 120),
                (CodecId::C2_3200, 200),
            ],
            plan(CodecId::C2_2400, TalkMode::Ptt, 200),
        ),
        Tier::Sf6 => duplex(
            &[(CodecId::Opus8k, 200), (CodecId::C2_3200, 200)],
            plan(CodecId::C2_2400, TalkMode::Ptt, 400),
        ),
        Tier::Sf7 => duplex(
            &[(CodecId::C2_2400, 200)],
            plan(CodecId::C2_2400, TalkMode::Ptt, 400),
        ),
        Tier::Sf8 => duplex(
            &[(CodecId::C2_1300, 200)],
            plan(CodecId::C2_1300, TalkMode::Ptt, 400),
        ),
        Tier::Sf9 | Tier::Sf10 => plan(CodecId::C2_1200, TalkMode::Ptt, 400),
        Tier::Sf11 => plan(CodecId::C2_700C, TalkMode::Ptt, 1000),
        Tier::Sf12 => plan(CodecId::C2_700C, TalkMode::PttDelayed, 2000),
    }
}
