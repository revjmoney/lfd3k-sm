//! Frame type 15 (`FrameType::Voice`): everything voice on the air.
//!
//! The 5 byte header is the usual one (type 15); the first payload byte is
//! the sub-type (`VSub`), the rest is the sub-type's body. `frame::decode`
//! accepts a type 15 frame only when its sub-type byte is one this build
//! knows; a bare header or an unknown sub-type is `BadType`, so it is
//! dropped like an unknown frame type. Older builds drop every type 15
//! frame as `BadType`.
//!
//! Discovery bodies (call id 0, seq 0, flags 0, sent on SF12 like the
//! normal discovery frames; a name or label is a length byte plus that many
//! bytes; an item is a voice name then its device label):
//!
//! - Beacon (sub-type 0): station name, item count (0 to 4), the items.
//! - ScanResp (sub-type 1): the normal ScanResp fields (station name,
//!   services, SNR the ScanReq was heard at as i16 dB x4 little endian,
//!   scan id), then item count (0 to 4) and the items. Sent in place of the
//!   normal ScanResp to a ScanReq with `FLAG_VOICE`.
//!
//! Every parser requires the exact length (trailing bytes are a bad body),
//! valid station names (`discovery::valid_station`) and valid labels
//! (`names::valid_label`).

use super::names::{valid_label, LABEL_MAX};
use super::regs::{VoiceReg, VOICE_REGS};
use crate::discovery::{valid_station, ScanResp};
use crate::frame::{self, FrameError, FrameType, Header, HEADER_LEN, MAX_PAYLOAD, NAME_MAX};

/// Type 15 sub-types. 7 and up are reserved for channels.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VSub {
    /// Discovery: our station and its registered voice names.
    Beacon = 0,
    /// Discovery: the answer to a ScanReq with `FLAG_VOICE`.
    ScanResp = 1,
    /// Voice call request (Task 5).
    CallReq = 2,
    /// Voice answer (Task 5).
    Answer = 3,
    /// Voice decline (Task 5).
    Decline = 4,
    /// Voice data (Task 6).
    Data = 5,
    /// Listener slot (Task 6).
    Listener = 6,
}

impl VSub {
    pub fn from_u8(v: u8) -> Option<VSub> {
        Some(match v {
            0 => VSub::Beacon,
            1 => VSub::ScanResp,
            2 => VSub::CallReq,
            3 => VSub::Answer,
            4 => VSub::Decline,
            5 => VSub::Data,
            6 => VSub::Listener,
            _ => return None,
        })
    }
}

/// Longest item: voice name and label with their length bytes.
pub const VOICE_ITEM_MAX: usize = 1 + NAME_MAX + 1 + LABEL_MAX;
/// Longest voice discovery body (a ScanResp with four items), sub-type
/// byte not counted.
pub const VOICE_DISC_MAX: usize = 1 + NAME_MAX + 4 + 1 + VOICE_REGS * VOICE_ITEM_MAX;
/// Longest voice discovery frame on the air (header, sub-type, body).
pub const VOICE_DISC_FRAME: usize = HEADER_LEN + 1 + VOICE_DISC_MAX;

// The sub-type byte plus the longest body fit one frame.
const _: () = assert!(VOICE_DISC_MAX < MAX_PAYLOAD);

/// A type 15 frame: header (flags 0, ack 0), sub-type, payload.
pub fn encode_voice(
    sub: VSub,
    call_id: u8,
    seq: u8,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, FrameError> {
    if payload.len() >= MAX_PAYLOAD {
        return Err(FrameError::TooLong);
    }
    let h = Header {
        ftype: FrameType::Voice,
        flags: 0,
        call_id,
        seq,
        ack: 0,
    };
    let total = HEADER_LEN + 1 + payload.len();
    let dst = out.get_mut(..total).ok_or(FrameError::TooShort)?;
    let (head, body) = dst.split_at_mut(HEADER_LEN);
    frame::encode(&h, &[], head)?;
    let (s, rest) = body.split_first_mut().ok_or(FrameError::TooShort)?;
    *s = sub as u8;
    rest.copy_from_slice(payload);
    Ok(total)
}

/// Sub-type, header and the payload after the sub-type byte of a type 15
/// frame; None for anything else (another type, a bad frame, an unknown
/// sub-type).
pub fn decode_voice(frame: &[u8]) -> Option<(VSub, Header, &[u8])> {
    let f = frame::decode(frame).ok()?;
    if f.header.ftype != FrameType::Voice {
        return None;
    }
    let (&s, rest) = f.payload.split_first()?;
    Some((VSub::from_u8(s)?, f.header, rest))
}

/// Length byte then the string; advances `pos`.
fn put_str(s: &str, out: &mut [u8], pos: &mut usize) -> Option<()> {
    let n = u8::try_from(s.len()).ok()?;
    *out.get_mut(*pos)? = n;
    let end = pos.checked_add(1 + s.len())?;
    out.get_mut(*pos + 1..end)?.copy_from_slice(s.as_bytes());
    *pos = end;
    Some(())
}

/// A length-prefixed string and the bytes after it.
fn take_str(p: &[u8]) -> Option<(&str, &[u8])> {
    let (&l, rest) = p.split_first()?;
    let (s, rest) = rest.split_at_checked(usize::from(l))?;
    Some((core::str::from_utf8(s).ok()?, rest))
}

/// Item count and items of every registration.
fn put_items(regs: &[Option<VoiceReg>; VOICE_REGS], out: &mut [u8], pos: &mut usize) -> Option<()> {
    let n = u8::try_from(regs.iter().flatten().count()).ok()?;
    *out.get_mut(*pos)? = n;
    *pos += 1;
    for r in regs.iter().flatten() {
        put_str(r.name.as_str(), out, pos)?;
        put_str(r.label.as_str(), out, pos)?;
    }
    Some(())
}

/// The voice names and labels of a parsed beacon or scan answer, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceItems<'a> {
    raw: &'a [u8],
    left: u8,
}

impl<'a> Iterator for VoiceItems<'a> {
    /// (voice name, device label)
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        let (name, rest) = take_str(self.raw)?;
        let (label, rest) = take_str(rest)?;
        self.raw = rest;
        self.left -= 1;
        Some((name, label))
    }
}

/// Count byte and exactly that many valid items filling `p`.
fn check_items(p: &[u8]) -> Option<VoiceItems<'_>> {
    let (&n, items) = p.split_first()?;
    if usize::from(n) > VOICE_REGS {
        return None;
    }
    let mut rest = items;
    for _ in 0..n {
        let (name, r) = take_str(rest)?;
        let (label, r) = take_str(r)?;
        if !valid_station(name) || !valid_label(label) {
            return None;
        }
        rest = r;
    }
    rest.is_empty().then_some(VoiceItems {
        raw: items,
        left: n,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceBeacon<'a> {
    pub station: &'a str,
    pub items: VoiceItems<'a>,
}

/// Voice beacon body (after the sub-type byte); the length, or None if the
/// station name is invalid or `out` too small.
pub fn voice_beacon_body(
    station: &str,
    regs: &[Option<VoiceReg>; VOICE_REGS],
    out: &mut [u8],
) -> Option<usize> {
    if !valid_station(station) {
        return None;
    }
    let mut pos = 0;
    put_str(station, out, &mut pos)?;
    put_items(regs, out, &mut pos)?;
    Some(pos)
}

pub fn parse_voice_beacon(p: &[u8]) -> Option<VoiceBeacon<'_>> {
    let (station, rest) = take_str(p)?;
    if !valid_station(station) {
        return None;
    }
    Some(VoiceBeacon {
        station,
        items: check_items(rest)?,
    })
}

/// Voice scan answer body (after the sub-type byte): the normal ScanResp
/// fields of `r`, then the items.
pub fn voice_scan_resp_body(
    r: &ScanResp<'_>,
    regs: &[Option<VoiceReg>; VOICE_REGS],
    out: &mut [u8],
) -> Option<usize> {
    if !valid_station(r.name) {
        return None;
    }
    let mut pos = 0;
    put_str(r.name, out, &mut pos)?;
    let s = r.snr_x4.to_le_bytes();
    out.get_mut(pos..pos + 4)?
        .copy_from_slice(&[r.services, s[0], s[1], r.scan_id]);
    pos += 4;
    put_items(regs, out, &mut pos)?;
    Some(pos)
}

pub fn parse_voice_scan_resp(p: &[u8]) -> Option<(ScanResp<'_>, VoiceItems<'_>)> {
    let (name, rest) = take_str(p)?;
    if !valid_station(name) {
        return None;
    }
    let (fixed, rest) = rest.split_at_checked(4)?;
    let &[services, a, b, scan_id] = fixed else {
        return None;
    };
    let r = ScanResp {
        name,
        services,
        snr_x4: i16::from_le_bytes([a, b]),
        scan_id,
    };
    Some((r, check_items(rest)?))
}
