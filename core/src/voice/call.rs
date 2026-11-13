//! Voice call setup by person name (spec 5.4).
//!
//! A voice call is placed to a voice name (and optionally a device label),
//! not to a station. Its setup frames are type 15 sub-types, so an older
//! build never rings for one; after the answer the call runs on today's
//! machinery (Train, TierSet, TierAck, then the connected call), with a 4
//! byte answer token added to the voice Train and to the setup TierSet.
//!
//! Bodies (after the sub-type byte; a string is a length byte then its
//! bytes; the call id is the header's):
//!
//! - CallReq (sub-type 2): capability byte (`CAP_LZ` as on a CallReq), the
//!   caller's station name, voice name and device label, the callee's voice
//!   name and device label (length 0 = any device). Repeated every 3 s on
//!   SF12 until answered or S7 runs out.
//! - Answer (sub-type 3): answer token (u32 little endian), the answering
//!   station name and the answering device label.
//! - Decline (sub-type 4): empty.
//!
//! The voice Train is today's 3 or 4 byte Train payload plus the token; the
//! setup TierSet is the tier byte plus the token. Only the two ends of a
//! voice call read those extra bytes (each knows it is in a voice call), so
//! data calls and older builds see exactly today's frames. Retrain
//! TierSets inside a connected call stay one byte.
//!
//! Every parser requires the exact length and valid names and labels.

use super::frames::{encode_voice, VSub};
use super::names::{valid_label, Name16};
use super::regs::{VoiceReg, VOICE_REGS};
use crate::discovery::valid_station;
use crate::frame::{FrameError, CAP_LZ};

/// The far end of a voice call (or call attempt), for the host side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceCallInfo {
    /// Caller side: the voice name we dialed. Answerer side: the caller's.
    pub peer_name: Name16,
    /// The far device label: the answering device's once it answered (until
    /// then the label we dialed, or empty for any); the caller's on the
    /// answering side.
    pub peer_label: Name16,
    /// The far station: the answering modem once its Answer was heard
    /// (empty before, or when only its Train got through); the caller's
    /// modem on the answering side.
    pub peer_station: Name16,
    /// The answer token: drawn by the answering device; on the caller, the
    /// one it bound to (0 until bound).
    pub token: u32,
    /// Our registration (session) in this call: the one that dialed, or the
    /// first matching one that rang.
    pub reg: u8,
}

impl VoiceCallInfo {
    pub const EMPTY: VoiceCallInfo = VoiceCallInfo {
        peer_name: Name16::EMPTY,
        peer_label: Name16::EMPTY,
        peer_station: Name16::EMPTY,
        token: 0,
        reg: 0,
    };
}

/// A voice answerer sends its Answer and Train at most this many times.
pub(crate) const ANSWER_SENDS: u8 = 2;

/// `now` has reached `at` (wrapping u32 milliseconds).
pub(crate) fn reached(now: u32, at: u32) -> bool {
    now.wrapping_sub(at) < u32::MAX / 2
}

/// Voice call state the `Link` carries beside its usual call state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VoiceLink {
    /// The current (or last) call is a voice call. Kept after the call ends
    /// (the Modem reads it for the end of call result); cleared when a data
    /// call starts.
    pub on: bool,
    pub info: VoiceCallInfo,
    /// Caller: bound to `info.token`, since `bound_at` (by an Answer).
    pub bound: bool,
    pub bound_at: u32,
    /// Caller: the label we dialed (empty for any device).
    pub want_label: Name16,
    /// Answerer: our station and the device label of `info.reg`.
    pub me_station: Name16,
    pub me_label: Name16,
    /// Registrations ringing, one bit each.
    pub ring_mask: u8,
    /// Answerer: Answer and Train sends so far, the time of the last, and
    /// when the next may go out.
    pub sends: u8,
    pub sent_at: u32,
    pub next_at: u32,
    /// Answerer: the last time (ms) the radio was busy, hearing a frame or
    /// sending one (see `quiet_ms`).
    pub busy_at: u32,
    /// The call we declined and when its last request was heard: its
    /// repeats must not ring again (the caller may have missed the Decline).
    pub declined: Option<(u8, u32)>,
}

impl VoiceLink {
    pub const fn new() -> Self {
        VoiceLink {
            on: false,
            info: VoiceCallInfo::EMPTY,
            bound: false,
            bound_at: 0,
            want_label: Name16::EMPTY,
            me_station: Name16::EMPTY,
            me_label: Name16::EMPTY,
            ring_mask: 0,
            sends: 0,
            sent_at: 0,
            next_at: 0,
            busy_at: 0,
            declined: None,
        }
    }

    /// A new voice call: everything but the declined call memory.
    pub fn start(&mut self, info: VoiceCallInfo) {
        *self = VoiceLink {
            on: true,
            info,
            declined: self.declined,
            ..VoiceLink::new()
        };
    }

    /// Answerer: when to send the answer again if no TierSet comes,
    /// `PERIOD_MS` plus up to 1 s after `now`. The jitter comes from our
    /// random token, so two devices that picked up together do not repeat
    /// in step.
    pub fn resend_after(&self, now: u32, period: u32) -> u32 {
        now.wrapping_add(period)
            .wrapping_add(self.info.token % 1000)
    }

    /// Answerer: the caller asked again (it has not heard us): answer again
    /// soon, after up to 0.7 s of jitter from our token.
    pub fn resend_soon(&self, now: u32) -> u32 {
        now.wrapping_add((self.info.token >> 10) % 700)
    }

    /// Answerer: how long the channel must have been quiet before our
    /// Answer goes out, 150 to 1349 ms (the jitter from our random token
    /// and the send count).
    /// Another device that picked up the same call sends its Train the
    /// moment its Answer ends, and a frame is only detected once its
    /// preamble is through (about 100 ms at SF12), so sending as soon as
    /// the channel looks free would land on that Train; and the caller
    /// answers that Train with its TierSet at once, which this lets us
    /// hear first.
    pub fn quiet_ms(&self) -> u32 {
        // Drawn again for each send from our token and the send count, so
        // two devices whose first draws came close do not stay in step.
        let k = u32::from(self.sends)
            .wrapping_add(1)
            .wrapping_mul(0x9E37_79B9);
        150 + crate::link::xorshift32((self.info.token ^ k) | 1) % 1200
    }

    /// The setup TierSet payload: the tier index, plus the token in a voice
    /// call. Returns the buffer and its length.
    pub fn tierset(&self, tier: u8) -> ([u8; 5], usize) {
        let t = self.info.token.to_le_bytes();
        let p = [tier, t[0], t[1], t[2], t[3]];
        (p, if self.on { 5 } else { 1 })
    }
}

/// A parsed voice call request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceReq<'a> {
    pub caps: u8,
    pub station: &'a str,
    pub name: &'a str,
    pub label: &'a str,
    pub callee: &'a str,
    /// Empty = any device.
    pub callee_label: &'a str,
}

/// Length byte then the string; advances `pos`.
fn put_str(s: &str, out: &mut [u8], pos: &mut usize) -> Option<()> {
    let n = u8::try_from(s.len()).ok()?;
    *out.get_mut(*pos)? = n;
    let end = pos.checked_add(1)?.checked_add(s.len())?;
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

/// Whole voice CallReq frame (header with `call_id`, sub-type, body) into
/// `out`; None for a bad name or label, or `out` too small.
pub fn encode_req(call_id: u8, r: &VoiceReq<'_>, out: &mut [u8]) -> Option<usize> {
    let ok = valid_station(r.station)
        && valid_station(r.name)
        && valid_label(r.label)
        && valid_station(r.callee)
        && (r.callee_label.is_empty() || valid_label(r.callee_label));
    if !ok {
        return None;
    }
    let mut p = [0u8; 1 + 5 * 17];
    let mut pos = 1;
    *p.first_mut()? = r.caps & CAP_LZ;
    for s in [r.station, r.name, r.label, r.callee, r.callee_label] {
        put_str(s, &mut p, &mut pos)?;
    }
    encode_voice(VSub::CallReq, call_id, 0, p.get(..pos)?, out).ok()
}

pub fn parse_req(p: &[u8]) -> Option<VoiceReq<'_>> {
    let (&caps, rest) = p.split_first()?;
    let (station, rest) = take_str(rest)?;
    let (name, rest) = take_str(rest)?;
    let (label, rest) = take_str(rest)?;
    let (callee, rest) = take_str(rest)?;
    let (callee_label, rest) = take_str(rest)?;
    let ok = rest.is_empty()
        && valid_station(station)
        && valid_station(name)
        && valid_label(label)
        && valid_station(callee)
        && (callee_label.is_empty() || valid_label(callee_label));
    ok.then_some(VoiceReq {
        caps: caps & CAP_LZ,
        station,
        name,
        label,
        callee,
        callee_label,
    })
}

/// Whole Answer frame into `out`.
pub fn encode_answer(
    call_id: u8,
    token: u32,
    station: &str,
    label: &str,
    out: &mut [u8],
) -> Result<usize, FrameError> {
    if !valid_station(station) || !valid_label(label) {
        return Err(FrameError::BadPayload);
    }
    let mut p = [0u8; 4 + 17 + 9];
    let mut pos = 4;
    p.get_mut(..4)
        .ok_or(FrameError::TooShort)?
        .copy_from_slice(&token.to_le_bytes());
    put_str(station, &mut p, &mut pos).ok_or(FrameError::TooShort)?;
    put_str(label, &mut p, &mut pos).ok_or(FrameError::TooShort)?;
    encode_voice(
        VSub::Answer,
        call_id,
        0,
        p.get(..pos).ok_or(FrameError::TooShort)?,
        out,
    )
}

/// (token, answering station, answering label) of an Answer body.
pub fn parse_answer(p: &[u8]) -> Option<(u32, &str, &str)> {
    let (t, rest) = p.split_at_checked(4)?;
    let &[a, b, c, d] = t else {
        return None;
    };
    let (station, rest) = take_str(rest)?;
    let (label, rest) = take_str(rest)?;
    let ok = rest.is_empty() && valid_station(station) && valid_label(label);
    ok.then_some((u32::from_le_bytes([a, b, c, d]), station, label))
}

/// Whole Decline frame into `out`.
pub fn encode_decline(call_id: u8, out: &mut [u8]) -> Result<usize, FrameError> {
    encode_voice(VSub::Decline, call_id, 0, &[], out)
}

/// A voice Train payload: today's 3 or 4 byte form, then the token.
pub fn train_payload(base: &[u8], token: u32, out: &mut [u8; 8]) -> usize {
    let n = base.len().min(4);
    let t = token.to_le_bytes();
    if let Some(d) = out.get_mut(..n) {
        d.copy_from_slice(base.get(..n).unwrap_or(&[]));
    }
    if let Some(d) = out.get_mut(n..n + 4) {
        d.copy_from_slice(&t);
    }
    n + 4
}

/// The 3 or 4 byte Train form and the token of a voice Train payload.
pub fn split_train(p: &[u8]) -> Option<(&[u8], u32)> {
    if p.len() != 7 && p.len() != 8 {
        return None;
    }
    let (base, t) = p.split_at_checked(p.len() - 4)?;
    let &[a, b, c, d] = t else {
        return None;
    };
    Some((base, u32::from_le_bytes([a, b, c, d])))
}

/// The token of a setup TierSet payload in a voice call.
pub fn tierset_token(p: &[u8]) -> Option<u32> {
    match p {
        [_, a, b, c, d] => Some(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

/// The registrations a voice request rings, one bit each: the voice name
/// matches (and the label, when one was given), and the registration is not
/// the caller's own device (our station and its label both equal the
/// caller's). Station names never match. The privacy check is the Modem's.
pub fn ring_mask(regs: &[Option<VoiceReg>; VOICE_REGS], r: &VoiceReq<'_>, me_station: &str) -> u8 {
    let mut mask = 0u8;
    for (i, reg) in regs.iter().enumerate() {
        let Some(reg) = reg else { continue };
        let own = me_station.eq_ignore_ascii_case(r.station) && reg.label.is(r.label);
        let hit =
            reg.name.is(r.callee) && (r.callee_label.is_empty() || reg.label.is(r.callee_label));
        if hit && !own {
            mask |= 1 << (i & 7);
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_ms_range_and_formula() {
        // V223.LRA.90 clause 9.3: Q = 150 + (xorshift32((token XOR k) OR 1)
        // mod 1200) ms, k = (n + 1) * 0x9E3779B9, n the sends so far.
        let mut v = VoiceLink::new();
        let mut seen = [false; 12];
        let mut tok = 0x1234_5678u32;
        for _ in 0..5_000 {
            tok = crate::link::xorshift32(tok);
            v.info.token = tok;
            for n in 0..3u8 {
                v.sends = n;
                let q = v.quiet_ms();
                assert!((150..=1349).contains(&q), "{q}");
                let k = u32::from(n).wrapping_add(1).wrapping_mul(0x9E37_79B9);
                assert_eq!(q, 150 + crate::link::xorshift32((tok ^ k) | 1) % 1200);
                if let Some(b) = seen.get_mut(((q - 150) / 100) as usize) {
                    *b = true;
                }
            }
        }
        assert!(seen.iter().all(|&b| b), "the whole range is used");
        // Drawn again per send: the same token gives different waits.
        v.info.token = 0xDEAD_BEEF;
        v.sends = 0;
        let a = v.quiet_ms();
        v.sends = 1;
        assert_ne!(a, v.quiet_ms());
        // Known values, so a port can check its xorshift32.
        v.sends = 0;
        v.info.token = 1;
        let k = 0x9E37_79B9u32;
        assert_eq!(v.quiet_ms(), 150 + crate::link::xorshift32(k | 1) % 1200);
    }
}
