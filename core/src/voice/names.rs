//! Voice names and device labels (spec 5.1).
//!
//! A voice name follows the station name rule (`discovery::valid_station`):
//! 1 to 16 printable ASCII without `:`, compared case-insensitively. A
//! device label is 1 to 8 printable ASCII without `:` or `/`, so `REV/PC`
//! always splits at its last `/`. Both are held in a `Name16`.

use crate::discovery::valid_station;
use crate::frame::NAME_MAX;

/// Longest device label, in bytes.
pub const LABEL_MAX: usize = 8;

/// A name of up to 16 bytes held in place (no heap). Built only through
/// `new` or `label`, so it is always valid; `EMPTY` (length 0) is the one
/// exception and stands for "none" (a pinned voice name never heard has no
/// label or station yet).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Name16 {
    bytes: [u8; NAME_MAX],
    len: u8,
}

/// A device label: 1 to 8 printable ASCII, no `:` or `/`.
pub fn valid_label(s: &str) -> bool {
    (1..=LABEL_MAX).contains(&s.len())
        && s.bytes()
            .all(|b| (0x20..=0x7E).contains(&b) && b != b':' && b != b'/')
}

impl Name16 {
    /// No name.
    pub const EMPTY: Name16 = Name16 {
        bytes: [0; NAME_MAX],
        len: 0,
    };

    /// A voice or station name (the station name rule), else None.
    pub fn new(s: &str) -> Option<Name16> {
        if !valid_station(s) {
            return None;
        }
        Self::copy(s)
    }

    /// A device label (`valid_label`), else None.
    pub fn label(s: &str) -> Option<Name16> {
        if !valid_label(s) {
            return None;
        }
        Self::copy(s)
    }

    fn copy(s: &str) -> Option<Name16> {
        let mut n = Name16::EMPTY;
        n.bytes.get_mut(..s.len())?.copy_from_slice(s.as_bytes());
        n.len = u8::try_from(s.len()).ok()?;
        Some(n)
    }

    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Same name, ignoring ASCII case.
    pub fn is(&self, s: &str) -> bool {
        self.as_str().eq_ignore_ascii_case(s)
    }
}
