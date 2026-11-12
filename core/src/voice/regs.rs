//! Voice name registrations (spec 5.1).
//!
//! A voice app on a host link session registers its voice name and device
//! label with its modem. One registration per session; session `s` (0 to
//! 3) owns slot `s`, so at most `VOICE_REGS` exist. A registration lasts
//! while its session sends frames (`Modem::voice_session_seen`) and is
//! dropped `VOICE_GRACE_MS` after the last one (a phone that briefly loses
//! Wi-Fi keeps its name), or at once on unregister. Never saved.

use super::names::Name16;

/// Registrations per modem (one per host link session).
pub const VOICE_REGS: usize = 4;
/// A registration is dropped this long after its session's last frame.
pub const VOICE_GRACE_MS: u32 = 30_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceReg {
    pub name: Name16,
    pub label: Name16,
    pub session: u8,
    /// `now_ms` of the session's last frame (or the registration).
    pub seen_ms: u32,
}

/// The registrations and the voice beacon request, owned by the `Modem`.
pub(crate) struct VoiceState {
    pub regs: [Option<VoiceReg>; VOICE_REGS],
    /// A voice beacon is wanted: after a normal beacon, or soon after a
    /// new or changed registration. Sent by the discovery scheduler.
    pub beacon_due: bool,
}

impl VoiceState {
    pub const fn new() -> Self {
        VoiceState {
            regs: [None; VOICE_REGS],
            beacon_due: false,
        }
    }

    /// Register (or replace) the name and label of `session`. False, and
    /// nothing changed, for a session over 3, a bad name or a bad label.
    pub fn register(&mut self, session: u8, name: &str, label: &str, now: u32) -> bool {
        let (Some(n), Some(l)) = (Name16::new(name), Name16::label(label)) else {
            return false;
        };
        let Some(slot) = self.regs.get_mut(usize::from(session)) else {
            return false;
        };
        let same = slot.is_some_and(|r| r.name.is(name) && r.label.is(label));
        *slot = Some(VoiceReg {
            name: n,
            label: l,
            session,
            seen_ms: now,
        });
        if !same {
            self.beacon_due = true;
        }
        true
    }

    pub fn unregister(&mut self, session: u8) {
        if let Some(s) = self.regs.get_mut(usize::from(session)) {
            *s = None;
        }
    }

    /// The session sent a frame: its registration lives on.
    pub fn seen(&mut self, session: u8, now: u32) {
        if let Some(Some(r)) = self.regs.get_mut(usize::from(session)) {
            r.seen_ms = now;
        }
    }

    /// Drop registrations whose session went quiet `VOICE_GRACE_MS` ago.
    pub fn expire(&mut self, now: u32) {
        for s in self.regs.iter_mut() {
            if s.is_some_and(|r| now.wrapping_sub(r.seen_ms) >= VOICE_GRACE_MS) {
                *s = None;
            }
        }
    }

    pub fn count(&self) -> usize {
        self.regs.iter().flatten().count()
    }

    pub fn any(&self) -> bool {
        self.regs.iter().any(Option::is_some)
    }
}
