//! The directory: stations this modem has heard (beacons, scan answers,
//! real calls) or that the user pinned with `AT+DIR=+NAME`.
//!
//! Fixed size (`DIR_MAX` entries, no heap). Names compare case-insensitively
//! and follow the station name rule (`discovery::valid_station`). When full,
//! a new name replaces the least recently heard unpinned entry; pinned
//! entries are never replaced. The `Modem` keeps its own name out.
//!
//! Recency is a counter (`stamp`), not the clock, so the blob order and
//! eviction never depend on `heard_ms` wrapping.
//!
//! Blob v1 (little endian), `DIR_HEAD + n * DIR_REC + 2` bytes, at most
//! `DIR_BLOB_LEN`:
//!
//! - 0: magic `LFD1` (4 bytes)
//! - 4: version 1
//! - 5: record count n (0 to 32)
//! - 6: n records of 20 bytes, most recently heard first: name length (1),
//!   name (16, zero padded), services (1, known bits only), best tier index
//!   (1, 0 = SF12 to 7 = SF5, 0xFF none), pinned (1, 0 or 1)
//! - 6 + 20 n: CRC-16/CCITT-FALSE of everything before it (2)
//!
//! Stored: every pinned entry plus the 8 most recently heard unpinned ones.
//! Not stored: SNR, RSSI, heard time and hit count (a loaded entry counts as
//! not heard since boot). `from_blob` refuses wrong length, magic, version
//! or CRC, a bad name, padding that is not zero, unknown services bits, a
//! bad tier byte, a pinned byte other than 0 or 1, duplicate names (any
//! case), and more than 8 unpinned records.

use crate::discovery::{valid_station, SVC_MASK};
use crate::frame::NAME_MAX;
use crate::radio::RxInfo;
use crate::settings::crc16;
use crate::tier::{best_tier, Tier};

pub const DIR_MAX: usize = 32;
pub const DIR_BLOB_LEN: usize = 1024;
/// Unpinned entries kept in the blob.
pub const DIR_SAVE_RECENT: usize = 8;
/// Blob header bytes (magic, version, count).
pub const DIR_HEAD: usize = 6;
/// Blob record bytes.
pub const DIR_REC: usize = 20;
/// Margin `heard` uses for the best tier (the S20 default, 5 dB).
pub const DEFAULT_MARGIN_X4: i16 = 20;

const MAGIC: [u8; 4] = *b"LFD1";
const VERSION: u8 = 1;
const NO_TIER: u8 = 0xFF;

const _: () = assert!(DIR_HEAD + DIR_MAX * DIR_REC + 2 <= DIR_BLOB_LEN);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirEntry {
    name: [u8; NAME_MAX],
    name_len: u8,
    pub services: u8,
    pub last_snr_x4: i16,
    pub last_rssi: i16,
    pub best_tier: Option<Tier>,
    /// `now_ms` of the last time heard; meaningful only when `hits > 0`.
    pub heard_ms: u32,
    pub pinned: bool,
    /// Times heard since boot (0: pinned by hand or loaded, not heard yet).
    pub hits: u16,
    /// Recency counter: larger is more recent.
    stamp: u32,
}

impl DirEntry {
    fn new(name: &str) -> Option<DirEntry> {
        if !valid_station(name) {
            return None;
        }
        let mut e = DirEntry {
            name: [0; NAME_MAX],
            name_len: 0,
            services: 0,
            last_snr_x4: 0,
            last_rssi: 0,
            best_tier: None,
            heard_ms: 0,
            pinned: false,
            hits: 0,
            stamp: 0,
        };
        e.name
            .get_mut(..name.len())?
            .copy_from_slice(name.as_bytes());
        e.name_len = name.len() as u8;
        Some(e)
    }

    pub fn name(&self) -> &str {
        self.name
            .get(..usize::from(self.name_len))
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    /// Heard since boot.
    pub fn heard_since_boot(&self) -> bool {
        self.hits > 0
    }
}

#[derive(Clone, Debug)]
pub struct Directory {
    entries: [Option<DirEntry>; DIR_MAX],
    /// Last recency stamp handed out.
    clock: u32,
    /// Something the blob holds changed (see `take_changed`).
    changed: bool,
}

impl Default for Directory {
    fn default() -> Self {
        Self::new()
    }
}

impl Directory {
    pub const fn new() -> Self {
        Directory {
            entries: [None; DIR_MAX],
            clock: 0,
            changed: false,
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| {
            e.as_ref()
                .is_some_and(|e| e.name().eq_ignore_ascii_case(name))
        })
    }

    fn next_stamp(&mut self) -> u32 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// A slot for a new entry: a free one, else the least recently heard
    /// unpinned one (replaced). None when every entry is pinned.
    fn slot_for_new(&mut self) -> Option<usize> {
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
        let (i, _) = best?;
        self.changed = true;
        Some(i)
    }

    /// A station was heard: update it or add it (replacing the least
    /// recently heard unpinned entry when full). Best tier from the SNR
    /// with the default 5 dB margin; the `Modem` uses `heard_with_margin`
    /// with S20.
    pub fn heard(&mut self, name: &str, services: u8, info: RxInfo, now_ms: u32) {
        self.heard_with_margin(name, Some(services), info, now_ms, DEFAULT_MARGIN_X4);
    }

    /// `heard` with the tier margin given and services optional (`None`:
    /// the frame carries none, keep what is known). True when stored.
    pub fn heard_with_margin(
        &mut self,
        name: &str,
        services: Option<u8>,
        info: RxInfo,
        now_ms: u32,
        margin_x4: i16,
    ) -> bool {
        if !valid_station(name) {
            return false;
        }
        let idx = match self.find(name) {
            Some(i) => i,
            None => {
                let Some(i) = self.slot_for_new() else {
                    return false;
                };
                let Some(e) = DirEntry::new(name) else {
                    return false;
                };
                if let Some(s) = self.entries.get_mut(i) {
                    *s = Some(e);
                }
                self.changed = true;
                i
            }
        };
        let stamp = self.next_stamp();
        let tier = best_tier(info.snr_x4, margin_x4);
        let Some(Some(e)) = self.entries.get_mut(idx) else {
            return false;
        };
        if let Some(s) = services {
            let s = s & SVC_MASK;
            if e.services != s {
                e.services = s;
                self.changed = true;
            }
        }
        if e.best_tier != Some(tier) {
            e.best_tier = Some(tier);
            self.changed = true;
        }
        e.last_snr_x4 = info.snr_x4;
        e.last_rssi = info.rssi_dbm;
        e.heard_ms = now_ms;
        e.hits = e.hits.saturating_add(1);
        e.stamp = stamp;
        true
    }

    /// Pin a name (`AT+DIR=+NAME`), adding it if new. False if the name is
    /// invalid or every entry is already pinned.
    pub fn add_pinned(&mut self, name: &str) -> bool {
        if !valid_station(name) {
            return false;
        }
        if let Some(i) = self.find(name) {
            if let Some(Some(e)) = self.entries.get_mut(i) {
                if !e.pinned {
                    e.pinned = true;
                    self.changed = true;
                }
                return true;
            }
            return false;
        }
        let Some(i) = self.slot_for_new() else {
            return false;
        };
        let Some(mut e) = DirEntry::new(name) else {
            return false;
        };
        e.pinned = true;
        e.stamp = self.next_stamp();
        if let Some(s) = self.entries.get_mut(i) {
            *s = Some(e);
        }
        self.changed = true;
        true
    }

    /// Remove a name (`AT+DIR=-NAME`), pinned or not. False if absent.
    pub fn remove(&mut self, name: &str) -> bool {
        let Some(i) = self.find(name) else {
            return false;
        };
        if let Some(s) = self.entries.get_mut(i) {
            *s = None;
        }
        self.changed = true;
        true
    }

    /// Remove every unpinned entry (`AT+DIR=0`); returns how many.
    pub fn clear_unpinned(&mut self) -> usize {
        let mut n = 0;
        for s in self.entries.iter_mut() {
            if s.as_ref().is_some_and(|e| !e.pinned) {
                *s = None;
                n += 1;
            }
        }
        if n > 0 {
            self.changed = true;
        }
        n
    }

    pub fn get(&self, name: &str) -> Option<&DirEntry> {
        self.entries.get(self.find(name)?)?.as_ref()
    }

    /// Entries in slot order.
    pub fn iter(&self) -> impl Iterator<Item = &DirEntry> {
        self.entries.iter().flatten()
    }

    /// The entry in slot `i` (0 to `DIR_MAX - 1`), if any.
    pub fn slot(&self, i: usize) -> Option<&DirEntry> {
        self.entries.get(i)?.as_ref()
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Entries heard since boot within `within_ms` of `now_ms`.
    pub fn heard_count(&self, now_ms: u32, within_ms: u32) -> usize {
        self.iter()
            .filter(|e| e.hits > 0 && now_ms.wrapping_sub(e.heard_ms) < within_ms)
            .count()
    }

    /// True once after anything the blob holds changed (an entry added,
    /// replaced or removed, a pin, services or best tier). A heard update
    /// that only refreshes SNR, time or recency does not count, so beacons
    /// alone never wear the flash.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Serialise (see the module doc); returns the bytes used.
    pub fn to_blob(&self, out: &mut [u8; DIR_BLOB_LEN]) -> usize {
        *out = [0; DIR_BLOB_LEN];
        // Pick: all pinned, plus the most recent unpinned ones.
        let mut pick = [false; DIR_MAX];
        for (p, e) in pick.iter_mut().zip(self.entries.iter()) {
            *p = e.as_ref().is_some_and(|e| e.pinned);
        }
        for _ in 0..DIR_SAVE_RECENT {
            let mut best: Option<(usize, u32)> = None;
            for (i, e) in self.entries.iter().enumerate() {
                let taken = pick.get(i).copied().unwrap_or(true);
                if let Some(e) = e {
                    if !taken && best.is_none_or(|(_, s)| e.stamp > s) {
                        best = Some((i, e.stamp));
                    }
                }
            }
            match best.and_then(|(i, _)| pick.get_mut(i)) {
                Some(p) => *p = true,
                None => break,
            }
        }
        // Write, most recent first (the slot index breaks a tie).
        let mut used = [false; DIR_MAX];
        let mut n = 0usize;
        let mut pos = DIR_HEAD;
        loop {
            let mut best: Option<(usize, u32)> = None;
            for (i, e) in self.entries.iter().enumerate() {
                let want =
                    pick.get(i).copied().unwrap_or(false) && !used.get(i).copied().unwrap_or(true);
                if let (true, Some(e)) = (want, e) {
                    if best.is_none_or(|(_, s)| e.stamp > s) {
                        best = Some((i, e.stamp));
                    }
                }
            }
            let Some((i, _)) = best else { break };
            if let Some(u) = used.get_mut(i) {
                *u = true;
            }
            let Some(Some(e)) = self.entries.get(i) else {
                break;
            };
            let Some(rec) = out.get_mut(pos..pos + DIR_REC) else {
                break;
            };
            if let Some(b) = rec.get_mut(0) {
                *b = e.name_len;
            }
            if let Some(d) = rec.get_mut(1..1 + NAME_MAX) {
                d.copy_from_slice(&e.name);
            }
            if let Some(d) = rec.get_mut(17..20) {
                d.copy_from_slice(&[
                    e.services & SVC_MASK,
                    e.best_tier.map_or(NO_TIER, Tier::index),
                    u8::from(e.pinned),
                ]);
            }
            pos += DIR_REC;
            n += 1;
        }
        if let Some(d) = out.get_mut(..DIR_HEAD) {
            d.copy_from_slice(&[MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION, n as u8]);
        }
        let crc = crc16(out.get(..pos).unwrap_or(&[]));
        if let Some(d) = out.get_mut(pos..pos + 2) {
            d.copy_from_slice(&crc.to_le_bytes());
        }
        pos + 2
    }

    /// Parse a blob; None on anything invalid (see the module doc).
    pub fn from_blob(b: &[u8]) -> Option<Directory> {
        let n = usize::from(*b.get(5)?);
        if n > DIR_MAX || b.len() != DIR_HEAD + n * DIR_REC + 2 {
            return None;
        }
        let end = DIR_HEAD + n * DIR_REC;
        let body = b.get(..end)?;
        let want = u16::from_le_bytes([*b.get(end)?, *b.get(end + 1)?]);
        if crc16(body) != want || body.get(..4)? != MAGIC || *body.get(4)? != VERSION {
            return None;
        }
        let mut d = Directory::new();
        let mut unpinned = 0usize;
        for i in 0..n {
            let rec = body.get(DIR_HEAD + i * DIR_REC..DIR_HEAD + (i + 1) * DIR_REC)?;
            let len = usize::from(*rec.first()?);
            let name_buf = rec.get(1..1 + NAME_MAX)?;
            if len == 0 || len > NAME_MAX || !name_buf.get(len..)?.iter().all(|&x| x == 0) {
                return None;
            }
            let name = core::str::from_utf8(name_buf.get(..len)?).ok()?;
            let mut e = DirEntry::new(name)?;
            if d.find(name).is_some() {
                return None;
            }
            let (services, tier, pinned) = (*rec.get(17)?, *rec.get(18)?, *rec.get(19)?);
            if services & !SVC_MASK != 0 {
                return None;
            }
            e.services = services;
            e.best_tier = match tier {
                NO_TIER => None,
                t => Some(Tier::from_index(t)?),
            };
            e.pinned = match pinned {
                0 => false,
                1 => true,
                _ => return None,
            };
            if !e.pinned {
                unpinned += 1;
                if unpinned > DIR_SAVE_RECENT {
                    return None;
                }
            }
            // First record is the most recent: highest stamp.
            e.stamp = (n - i) as u32;
            *d.entries.get_mut(i)? = Some(e);
        }
        d.clock = n as u32;
        Some(d)
    }
}
