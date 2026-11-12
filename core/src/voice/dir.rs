//! The voice directory (spec 5.3): voice names other modems carry, heard in
//! voice beacons and voice scan answers, plus the voice names pinned with
//! `AT+DIR=+V:<name>`.
//!
//! Fixed size (`VOICE_DIR_MAX` entries, no heap). An entry is one device: a
//! voice name and its label (both compared case-insensitively), with the
//! station it was last heard through. The same name and label heard through
//! another station moves the entry there. Entries never come from our own
//! station (the `Modem` checks), so our own registrations never show; the
//! same voice name registered on another modem does.
//!
//! Pins are per voice name: pinning `REV` pins every `REV` entry, and a
//! `REV` heard later on a new device is pinned too. A pinned name not heard
//! yet holds one placeholder entry with an empty label and station; the
//! first `REV` heard fills it in. When full, a new device replaces the
//! least recently heard unpinned entry; pinned entries are never replaced.
//! Only pinned names are saved (in the directory blob, see `directory.rs`).

use super::names::Name16;
use crate::tier::Tier;

pub const VOICE_DIR_MAX: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceEntry {
    pub name: Name16,
    /// Empty on a pinned name not heard yet.
    pub label: Name16,
    /// The modem it was heard through; empty until heard.
    pub station: Name16,
    pub best_tier: Option<Tier>,
    /// `now_ms` of the last time heard; meaningful only when heard.
    pub heard_ms: u32,
    pub pinned: bool,
    /// Heard since boot.
    heard: bool,
    /// Recency counter: larger is more recent.
    stamp: u32,
}

impl VoiceEntry {
    /// Heard since boot (a pinned or loaded name may not be).
    pub fn heard_since_boot(&self) -> bool {
        self.heard
    }
}

#[derive(Clone, Debug)]
pub struct VoiceDir {
    entries: [Option<VoiceEntry>; VOICE_DIR_MAX],
    clock: u32,
}

impl Default for VoiceDir {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceDir {
    pub const fn new() -> Self {
        VoiceDir {
            entries: [None; VOICE_DIR_MAX],
            clock: 0,
        }
    }

    /// Entries in slot order.
    pub fn iter(&self) -> impl Iterator<Item = &VoiceEntry> {
        self.entries.iter().flatten()
    }

    /// The entry in slot `i` (0 to `VOICE_DIR_MAX - 1`), if any.
    pub fn slot(&self, i: usize) -> Option<&VoiceEntry> {
        self.entries.get(i)?.as_ref()
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The voice name is pinned (HIDDEN mode lets only these ring).
    pub fn is_pinned(&self, name: &str) -> bool {
        self.iter().any(|e| e.pinned && e.name.is(name))
    }

    /// The entry for this device, if heard.
    pub fn get(&self, name: &str, label: &str) -> Option<&VoiceEntry> {
        self.slot(self.find(name, label)?)
    }

    fn find(&self, name: &str, label: &str) -> Option<usize> {
        self.entries.iter().position(|e| {
            e.as_ref()
                .is_some_and(|e| e.name.is(name) && e.label.is(label))
        })
    }

    fn next_stamp(&mut self) -> u32 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// A free slot, else the least recently heard unpinned one.
    fn slot_for_new(&self) -> Option<usize> {
        if let Some(i) = self.entries.iter().position(Option::is_none) {
            return Some(i);
        }
        let mut best: Option<(usize, u32)> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(e) = e {
                if !e.pinned && best.is_none_or(|(_, s)| e.stamp < s) {
                    best = Some((i, e.stamp));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// A device was heard through `station`. With `only_known` (HIDDEN
    /// mode) only a device or pinned name already here is refreshed. True
    /// when stored.
    pub(crate) fn heard(
        &mut self,
        name: &str,
        label: &str,
        station: &str,
        tier: Tier,
        now: u32,
        only_known: bool,
    ) -> bool {
        let (Some(n), Some(l), Some(st)) = (
            Name16::new(name),
            Name16::label(label),
            Name16::new(station),
        ) else {
            return false;
        };
        let pinned = self.is_pinned(name);
        let idx = match self.find(name, label) {
            Some(i) => i,
            // A pinned name not heard yet: its placeholder takes the device.
            None => match self.find(name, "") {
                Some(i) => i,
                None if only_known => return false,
                None => match self.slot_for_new() {
                    Some(i) => i,
                    None => return false,
                },
            },
        };
        let stamp = self.next_stamp();
        let Some(s) = self.entries.get_mut(idx) else {
            return false;
        };
        *s = Some(VoiceEntry {
            name: n,
            label: l,
            station: st,
            best_tier: Some(tier),
            heard_ms: now,
            pinned,
            heard: true,
            stamp,
        });
        true
    }

    /// Pin a voice name. Some(true) when something changed, Some(false)
    /// when it was already pinned, None for a bad name or no room for its
    /// placeholder (every entry pinned).
    pub(crate) fn pin(&mut self, name: &str) -> Option<bool> {
        let n = Name16::new(name)?;
        let mut found = false;
        let mut changed = false;
        for e in self.entries.iter_mut().flatten() {
            if e.name.is(name) {
                found = true;
                changed |= !e.pinned;
                e.pinned = true;
            }
        }
        if found {
            return Some(changed);
        }
        let i = self.slot_for_new()?;
        let stamp = self.next_stamp();
        *self.entries.get_mut(i)? = Some(VoiceEntry {
            name: n,
            label: Name16::EMPTY,
            station: Name16::EMPTY,
            best_tier: None,
            heard_ms: 0,
            pinned: true,
            heard: false,
            stamp,
        });
        Some(true)
    }

    /// Unpin a voice name: its heard entries stay (unpinned), a placeholder
    /// goes. False when the name was not pinned.
    pub(crate) fn unpin(&mut self, name: &str) -> bool {
        if !self.is_pinned(name) {
            return false;
        }
        for s in self.entries.iter_mut() {
            if let Some(e) = s.as_mut().filter(|e| e.name.is(name)) {
                if e.label.is_empty() {
                    *s = None;
                } else {
                    e.pinned = false;
                }
            }
        }
        true
    }

    /// Remove every unpinned entry; returns how many.
    pub(crate) fn clear_unpinned(&mut self) -> usize {
        let mut n = 0;
        for s in self.entries.iter_mut() {
            if s.as_ref().is_some_and(|e| !e.pinned) {
                *s = None;
                n += 1;
            }
        }
        n
    }

    /// Forget unpinned devices heard through `station` (our new name).
    pub(crate) fn remove_via(&mut self, station: &str) {
        for s in self.entries.iter_mut() {
            if s.as_ref()
                .is_some_and(|e| !e.pinned && e.station.is(station))
            {
                *s = None;
            }
        }
    }

    /// Every slot empty.
    pub(crate) fn clear(&mut self) {
        for s in self.entries.iter_mut() {
            *s = None;
        }
        self.clock = 0;
    }

    /// Each pinned voice name once (first entry of each name, slot order).
    pub fn pinned_names(&self) -> impl Iterator<Item = &Name16> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|e| e.pinned).map(|e| (i, e)))
            .filter(|(i, e)| {
                !self
                    .entries
                    .iter()
                    .take(*i)
                    .flatten()
                    .any(|o| o.pinned && o.name.is(e.name.as_str()))
            })
            .map(|(_, e)| &e.name)
    }

    /// Blob load: a pinned name not heard since boot. False if the name is
    /// bad, already here, or there is no room.
    pub(crate) fn load_pin(&mut self, name: &str) -> bool {
        if self.iter().any(|e| e.name.is(name)) {
            return false;
        }
        self.pin(name) == Some(true)
    }
}
