//! Saved settings: hotspot, client network, forwards, publish, S registers.
//!
//! Pure data, no I/O. The platform stores the blob (`to_blob`) and hands it
//! back at boot (`from_blob`); a blob with any problem is refused whole.
//!
//! Blob v2 (little endian, fixed offsets, 900 bytes used of `BLOB_LEN`):
//!
//! | offset | size | field |
//! |-------:|-----:|-------|
//! | 0 | 4 | magic `LFS1` |
//! | 4 | 1 | version (2) |
//! | 5 | 1 | flags: bit0 hotspot on, bit1 echo, bit2 verbose, bit3 show AP password, bit4 S31 = 0 (compression off), bit5 S32 = 1 (boot to menu) |
//! | 6 | 16 | S registers S0, S2, S7, S12, S20, S21, S22, S30 as u16 (S30 two's complement) |
//! | 22 | 1 | published forward index, 0xFF none |
//! | 23 | 33 | hotspot SSID: length byte, 32 bytes zero padded |
//! | 56 | 64 | hotspot password: length byte, 63 bytes zero padded |
//! | 120 | 33 | client SSID (length 0 = none) |
//! | 153 | 64 | client password |
//! | 217 | 680 | 8 forward records of 85 bytes |
//! | 897 | 1 | beacon mode: 0 ON, 1 QUIET, 2 HIDDEN |
//! | 898 | 2 | CRC-16/CCITT-FALSE of bytes 0..898 |
//!
//! Blob v1 (phase 3) is the same up to byte 896 with version 1 and the CRC
//! at 897 (899 bytes, no beacon byte). `from_blob` still loads it, with the
//! beacon mode ON; `to_blob` always writes v2, so the first save after an
//! upgrade migrates the stored copy. A v1 blob is accepted only at exactly
//! 899 bytes and a v2 blob only at exactly 900.
//!
//! S31 (phase 4, compression) lives in flags bit 4, stored inverted: a clear
//! bit is S31 = 1, the default. So the layout, the length and the version
//! stay as they were, and every v2 blob saved before S31 existed loads with
//! compression on. A v1 blob with bit 4 set is refused (v1 never wrote it).
//! An older build refuses a blob with bit 4 set (unknown flag) and falls
//! back to its defaults; that only happens after a downgrade with S31 = 0.
//!
//! S32 (phase 4, boot to menu) lives in flags bit 5, stored as is (set means
//! S32 = 1; the default 0 leaves the bit clear), by the same rule: layout,
//! length and version unchanged, every older v2 blob loads with S32 = 0, a
//! v1 blob with bit 5 set is refused.
//!
//! Forward record: used (0 or 1), name length, name\[16\], host length,
//! host\[63\], port u16, mode (0 telnet, 1 raw). An unused record is all zero.

use crate::at::{SRegs, REG_LZ, REG_MENU};
use crate::frame::valid_name;

/// Forward slots.
pub const MAX_FWD: usize = 8;
/// Longest forward name (same rule as station names).
pub const FWD_NAME: usize = 16;
/// Longest host name.
pub const HOST_MAX: usize = 63;
/// Longest SSID.
pub const SSID_MAX: usize = 32;
/// Longest WPA2 password.
pub const PASS_MAX: usize = 63;
/// Shortest WPA2 password.
pub const PASS_MIN: usize = 8;
/// Size of the buffer `to_blob` fills.
pub const BLOB_LEN: usize = 1024;

const MAGIC: [u8; 4] = *b"LFS1";
const VERSION: u8 = 2;
const VERSION_V1: u8 = 1;
const NO_PUB: u8 = 0xFF;
const NREGS: usize = 8;
const FWD_REC: usize = 85;
/// End of the forward records: the v1 CRC, the v2 beacon byte.
const OFF_FWD_END: usize = 217 + MAX_FWD * FWD_REC;
const OFF_CRC_V1: usize = OFF_FWD_END;
const BLOB_USED_V1: usize = OFF_CRC_V1 + 2;
const OFF_CRC: usize = OFF_FWD_END + 1;
const BLOB_USED: usize = OFF_CRC + 2;
const FLAG_AP: u8 = 1;
const FLAG_ECHO: u8 = 2;
const FLAG_VERBOSE: u8 = 4;
const FLAG_SHOW: u8 = 8;
/// S31 = 0 (inverted, so blobs from before S31 load with it on).
const FLAG_NO_LZ: u8 = 16;
/// S32 = 1 (boot to menu).
const FLAG_MENU: u8 = 32;
/// The registers in the 16 byte register field, in order. S31 and S32 are
/// the flags bits above; every other register in `SRegs::NUMBERS` is here.
const BLOB_REGS: [u8; NREGS] = [0, 2, 7, 12, 20, 21, 22, 30];

const _: () = assert!(SRegs::NUMBERS.len() == NREGS + 2);
const _: () = assert!(BLOB_USED <= BLOB_LEN);

fn printable(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// SSID: 1 to 32 printable ASCII, no comma (the AT argument separator).
pub fn valid_ssid(s: &str) -> bool {
    (1..=SSID_MAX).contains(&s.len()) && printable(s) && !s.contains(',')
}

/// Wi-Fi password: 8 to 63 printable ASCII, no comma.
pub fn valid_pass(s: &str) -> bool {
    (PASS_MIN..=PASS_MAX).contains(&s.len()) && printable(s) && !s.contains(',')
}

/// Forward name: a station name that also has no ':' or ',' (so `ATDT
/// host:port` and AT argument splitting stay unambiguous) and is not `0`
/// (`AT+PUB=0` means none).
pub fn valid_fwd_name(s: &str) -> bool {
    valid_name(s) && !s.contains([':', ',']) && s != "0"
}

/// Host: 1 to 63 characters of `A-Za-z0-9.-`.
pub fn valid_host(s: &str) -> bool {
    (1..=HOST_MAX).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Port text: decimal digits only, 1 to 65535.
pub fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u16>().ok().filter(|&p| p != 0)
}

/// `host:port` with a valid host and port.
pub fn parse_hostport(s: &str) -> Option<(&str, u16)> {
    let (h, p) = s.rsplit_once(':')?;
    if !valid_host(h) {
        return None;
    }
    Some((h, parse_port(p)?))
}

/// How a forward talks on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireMode {
    Telnet,
    Raw,
}

impl WireMode {
    /// Telnet for port 23, raw otherwise.
    pub fn default_for(port: u16) -> WireMode {
        if port == 23 {
            WireMode::Telnet
        } else {
            WireMode::Raw
        }
    }

    fn code(self) -> u8 {
        match self {
            WireMode::Telnet => 0,
            WireMode::Raw => 1,
        }
    }

    fn from_code(c: u8) -> Option<WireMode> {
        match c {
            0 => Some(WireMode::Telnet),
            1 => Some(WireMode::Raw),
            _ => None,
        }
    }
}

/// Privacy mode for discovery (`AT+BEACON`), saved in the blob.
///
/// ON sends beacons and answers scans; QUIET answers scans but sends no
/// beacons; HIDDEN sends nothing at all except answering calls from pinned
/// directory entries (`AT+DIR=+NAME`), and drops other callers silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BeaconMode {
    On,
    Quiet,
    Hidden,
}

impl BeaconMode {
    pub fn code(self) -> u8 {
        match self {
            BeaconMode::On => 0,
            BeaconMode::Quiet => 1,
            BeaconMode::Hidden => 2,
        }
    }

    pub fn from_code(c: u8) -> Option<BeaconMode> {
        match c {
            0 => Some(BeaconMode::On),
            1 => Some(BeaconMode::Quiet),
            2 => Some(BeaconMode::Hidden),
            _ => None,
        }
    }

    /// `ON`, `QUIET` or `HIDDEN`, as AT+BEACON takes and prints it.
    pub fn name(self) -> &'static str {
        match self {
            BeaconMode::On => "ON",
            BeaconMode::Quiet => "QUIET",
            BeaconMode::Hidden => "HIDDEN",
        }
    }

    /// The mode named `s` (any case).
    pub fn parse(s: &str) -> Option<BeaconMode> {
        [BeaconMode::On, BeaconMode::Quiet, BeaconMode::Hidden]
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(s))
    }
}

/// Copy `s` into a zeroed array; the length, or None if it does not fit.
fn fill<const N: usize>(dst: &mut [u8; N], s: &str) -> Option<u8> {
    if s.len() > N {
        return None;
    }
    *dst = [0; N];
    dst.get_mut(..s.len())?.copy_from_slice(s.as_bytes());
    u8::try_from(s.len()).ok()
}

/// Like `fill` but never fails: cuts at N bytes, non-printable becomes `?`.
fn fill_lossy<const N: usize>(dst: &mut [u8; N], s: &str) -> u8 {
    *dst = [0; N];
    let mut n = 0u8;
    for (d, b) in dst.iter_mut().zip(s.bytes()) {
        *d = if (0x20..=0x7E).contains(&b) { b } else { b'?' };
        n = n.saturating_add(1);
    }
    n
}

fn text(buf: &[u8], len: u8) -> &str {
    buf.get(..usize::from(len))
        .and_then(|b| core::str::from_utf8(b).ok())
        .unwrap_or("")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Forward {
    name: [u8; FWD_NAME],
    name_len: u8,
    host: [u8; HOST_MAX],
    host_len: u8,
    port: u16,
    mode: WireMode,
}

impl Forward {
    /// `None` unless name, host and port are valid.
    pub fn new(name: &str, host: &str, port: u16, mode: WireMode) -> Option<Forward> {
        if !valid_fwd_name(name) || !valid_host(host) || port == 0 {
            return None;
        }
        let mut f = Forward {
            name: [0; FWD_NAME],
            name_len: 0,
            host: [0; HOST_MAX],
            host_len: 0,
            port,
            mode,
        };
        f.name_len = fill(&mut f.name, name)?;
        f.host_len = fill(&mut f.host, host)?;
        Some(f)
    }

    pub fn name(&self) -> &str {
        text(&self.name, self.name_len)
    }

    pub fn host(&self) -> &str {
        text(&self.host, self.host_len)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn mode(&self) -> WireMode {
        self.mode
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    pub ap_on: bool,
    ap_ssid: [u8; SSID_MAX],
    ap_ssid_len: u8,
    ap_pass: [u8; PASS_MAX],
    ap_pass_len: u8,
    sta_ssid: [u8; SSID_MAX],
    sta_ssid_len: u8,
    sta_pass: [u8; PASS_MAX],
    sta_pass_len: u8,
    pub fwd: [Option<Forward>; MAX_FWD],
    /// Index into `fwd` of the forward answered radio calls are bridged to.
    pub publish: Option<u8>,
    pub sregs: SRegs,
    pub echo: bool,
    pub verbose: bool,
    /// The hotspot password may still be shown on the OLED.
    pub show_ap_pass: bool,
    /// Discovery privacy mode (AT+BEACON).
    pub beacon: BeaconMode,
}

/// Passwords are never printed.
impl core::fmt::Debug for Settings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Settings")
            .field("ap_on", &self.ap_on)
            .field("ap_ssid", &self.ap_ssid())
            .field("ap_pass", &"********")
            .field("sta_ssid", &self.sta_ssid())
            .field("sta_pass", &"********")
            .field("fwd", &self.fwd)
            .field("publish", &self.publish)
            .field("sregs", &self.sregs)
            .field("echo", &self.echo)
            .field("verbose", &self.verbose)
            .field("show_ap_pass", &self.show_ap_pass)
            .field("beacon", &self.beacon)
            .finish()
    }
}

impl Settings {
    /// Hotspot off, nothing named or set, no password: what a `Modem` holds
    /// before `reset` or `load_settings`. A constant, so it can sit in a static.
    pub const BLANK: Settings = Settings {
        ap_on: false,
        ap_ssid: [0; SSID_MAX],
        ap_ssid_len: 0,
        ap_pass: [0; PASS_MAX],
        ap_pass_len: 0,
        sta_ssid: [0; SSID_MAX],
        sta_ssid_len: 0,
        sta_pass: [0; PASS_MAX],
        sta_pass_len: 0,
        fwd: [None; MAX_FWD],
        publish: None,
        sregs: SRegs::new(),
        echo: true,
        verbose: true,
        show_ap_pass: true,
        beacon: BeaconMode::On,
    };

    /// Hotspot on, SSID = `name`, password `ap_pass` (8 to 63 characters),
    /// no client network, no forwards, nothing published, S registers at
    /// their power-on values, E1 V1, password still to be shown. Never
    /// fails: input that does not fit is cut and non-printable bytes become
    /// `?` (callers pass valid values; a short password makes a blob that
    /// `from_blob` refuses).
    pub fn defaults(name: &str, ap_pass: &str) -> Settings {
        let mut s = Settings {
            ap_on: true,
            ap_ssid: [0; SSID_MAX],
            ap_ssid_len: 0,
            ap_pass: [0; PASS_MAX],
            ap_pass_len: 0,
            sta_ssid: [0; SSID_MAX],
            sta_ssid_len: 0,
            sta_pass: [0; PASS_MAX],
            sta_pass_len: 0,
            fwd: [None; MAX_FWD],
            publish: None,
            sregs: SRegs::new(),
            echo: true,
            verbose: true,
            show_ap_pass: true,
            beacon: BeaconMode::On,
        };
        s.ap_ssid_len = fill_lossy(&mut s.ap_ssid, name);
        s.ap_pass_len = fill_lossy(&mut s.ap_pass, ap_pass);
        s
    }

    pub fn ap_ssid(&self) -> &str {
        text(&self.ap_ssid, self.ap_ssid_len)
    }

    pub fn ap_pass(&self) -> &str {
        text(&self.ap_pass, self.ap_pass_len)
    }

    /// Client network SSID, empty when none is set.
    pub fn sta_ssid(&self) -> &str {
        text(&self.sta_ssid, self.sta_ssid_len)
    }

    pub fn sta_pass(&self) -> &str {
        text(&self.sta_pass, self.sta_pass_len)
    }

    /// Set the hotspot name and password; false (nothing changed) if invalid.
    pub fn set_ap(&mut self, ssid: &str, pass: &str) -> bool {
        if !valid_ssid(ssid) || !valid_pass(pass) {
            return false;
        }
        let (mut s, mut p) = ([0; SSID_MAX], [0; PASS_MAX]);
        let (Some(sl), Some(pl)) = (fill(&mut s, ssid), fill(&mut p, pass)) else {
            return false;
        };
        (self.ap_ssid, self.ap_ssid_len) = (s, sl);
        (self.ap_pass, self.ap_pass_len) = (p, pl);
        true
    }

    /// Set the client network; false (nothing changed) if invalid.
    pub fn set_sta(&mut self, ssid: &str, pass: &str) -> bool {
        if !valid_ssid(ssid) || !valid_pass(pass) {
            return false;
        }
        let (mut s, mut p) = ([0; SSID_MAX], [0; PASS_MAX]);
        let (Some(sl), Some(pl)) = (fill(&mut s, ssid), fill(&mut p, pass)) else {
            return false;
        };
        (self.sta_ssid, self.sta_ssid_len) = (s, sl);
        (self.sta_pass, self.sta_pass_len) = (p, pl);
        true
    }

    /// Forget the client network.
    pub fn clear_sta(&mut self) {
        self.sta_ssid = [0; SSID_MAX];
        self.sta_ssid_len = 0;
        self.sta_pass = [0; PASS_MAX];
        self.sta_pass_len = 0;
    }

    /// Slot of the forward called `name` (case-insensitive).
    pub fn forward_index(&self, name: &str) -> Option<usize> {
        self.fwd.iter().position(|f| {
            f.as_ref()
                .is_some_and(|f| f.name().eq_ignore_ascii_case(name))
        })
    }

    /// The forward called `name` (case-insensitive).
    pub fn forward(&self, name: &str) -> Option<&Forward> {
        self.fwd.get(self.forward_index(name)?)?.as_ref()
    }

    pub fn fwd_count(&self) -> usize {
        self.fwd.iter().flatten().count()
    }

    /// Add a forward, or replace the one with the same name (any case).
    /// False when all slots are taken.
    pub fn set_forward(&mut self, f: Forward) -> bool {
        let idx = self
            .forward_index(f.name())
            .or_else(|| self.fwd.iter().position(Option::is_none));
        match idx.and_then(|i| self.fwd.get_mut(i)) {
            Some(slot) => {
                *slot = Some(f);
                true
            }
            None => false,
        }
    }

    /// Delete a forward; clears the publish setting if it pointed there.
    pub fn remove_forward(&mut self, name: &str) -> bool {
        let Some(i) = self.forward_index(name) else {
            return false;
        };
        if let Some(slot) = self.fwd.get_mut(i) {
            *slot = None;
        }
        if self.publish.is_some_and(|p| usize::from(p) == i) {
            self.publish = None;
        }
        true
    }

    /// Publish the forward called `name`; false if there is none.
    pub fn set_publish(&mut self, name: &str) -> bool {
        match self.forward_index(name).and_then(|i| u8::try_from(i).ok()) {
            Some(i) => {
                self.publish = Some(i);
                true
            }
            None => false,
        }
    }

    /// The forward answered radio calls go to.
    pub fn published(&self) -> Option<&Forward> {
        let i = usize::from(self.publish?);
        self.fwd.get(i)?.as_ref()
    }

    /// Boot to menu (S32 = 1): the menu opens on the first host byte.
    pub fn boot_menu(&self) -> bool {
        self.sregs.get(REG_MENU) == Some(1)
    }

    /// Same profile as `o`: every field but `show_ap_pass` (whether the
    /// generated hotspot password may still be shown is not a setting).
    pub fn same_profile(&self, o: &Settings) -> bool {
        self.ap_on == o.ap_on
            && self.ap_ssid() == o.ap_ssid()
            && self.ap_pass() == o.ap_pass()
            && self.sta_ssid() == o.sta_ssid()
            && self.sta_pass() == o.sta_pass()
            && self.fwd == o.fwd
            && self.publish == o.publish
            && self.sregs == o.sregs
            && self.echo == o.echo
            && self.verbose == o.verbose
            && self.beacon == o.beacon
    }

    /// Hotspot or client network settings differ from `o`.
    pub fn wifi_differs(&self, o: &Settings) -> bool {
        self.ap_on != o.ap_on
            || self.ap_ssid() != o.ap_ssid()
            || self.ap_pass() != o.ap_pass()
            || self.sta_ssid() != o.sta_ssid()
            || self.sta_pass() != o.sta_pass()
    }

    /// Serialise into `out` (zero filled first); returns the bytes used.
    pub fn to_blob(&self, out: &mut [u8; BLOB_LEN]) -> usize {
        *out = [0; BLOB_LEN];
        let mut w = Wr { out, pos: 0 };
        w.put(&MAGIC);
        w.put(&[VERSION]);
        let mut flags = 0;
        for (on, bit) in [
            (self.ap_on, FLAG_AP),
            (self.echo, FLAG_ECHO),
            (self.verbose, FLAG_VERBOSE),
            (self.show_ap_pass, FLAG_SHOW),
            (self.sregs.get(REG_LZ) == Some(0), FLAG_NO_LZ),
            (self.sregs.get(REG_MENU) == Some(1), FLAG_MENU),
        ] {
            if on {
                flags |= bit;
            }
        }
        w.put(&[flags]);
        for n in BLOB_REGS {
            w.put(&self.sregs.get(n).unwrap_or(0).to_le_bytes());
        }
        w.put(&[self.publish.unwrap_or(NO_PUB)]);
        w.put(&[self.ap_ssid_len]);
        w.put(&self.ap_ssid);
        w.put(&[self.ap_pass_len]);
        w.put(&self.ap_pass);
        w.put(&[self.sta_ssid_len]);
        w.put(&self.sta_ssid);
        w.put(&[self.sta_pass_len]);
        w.put(&self.sta_pass);
        for slot in &self.fwd {
            match slot {
                Some(f) => {
                    w.put(&[1, f.name_len]);
                    w.put(&f.name);
                    w.put(&[f.host_len]);
                    w.put(&f.host);
                    w.put(&f.port.to_le_bytes());
                    w.put(&[f.mode.code()]);
                }
                None => w.put(&[0; FWD_REC]),
            }
        }
        w.put(&[self.beacon.code()]);
        let end = w.pos;
        let crc = crc16(w.out.get(..end).unwrap_or(&[]));
        w.put(&crc.to_le_bytes());
        w.pos
    }

    /// Parse a blob (v2, or v1 migrated with the beacon mode ON); `None` on
    /// wrong magic, version, length, CRC or any field that fails validation.
    pub fn from_blob(b: &[u8]) -> Option<Settings> {
        let (version, off_crc) = match b.len() {
            BLOB_USED => (VERSION, OFF_CRC),
            BLOB_USED_V1 => (VERSION_V1, OFF_CRC_V1),
            _ => return None,
        };
        let body = b.get(..off_crc)?;
        let want = u16::from_le_bytes([*b.get(off_crc)?, *b.get(off_crc + 1)?]);
        if crc16(body) != want {
            return None;
        }
        let mut r = Rd { b: body, pos: 0 };
        if r.take(4)? != MAGIC || r.u8()? != version {
            return None;
        }
        let flags = r.u8()?;
        let mut known = FLAG_AP | FLAG_ECHO | FLAG_VERBOSE | FLAG_SHOW;
        if version == VERSION {
            known |= FLAG_NO_LZ | FLAG_MENU;
        }
        if flags & !known != 0 {
            return None;
        }
        let mut s = Settings::defaults("", "");
        s.ap_on = flags & FLAG_AP != 0;
        s.echo = flags & FLAG_ECHO != 0;
        s.verbose = flags & FLAG_VERBOSE != 0;
        s.show_ap_pass = flags & FLAG_SHOW != 0;
        for n in BLOB_REGS {
            if !s.sregs.set(n, r.u16()?) {
                return None;
            }
        }
        if !s.sregs.set(REG_LZ, u16::from(flags & FLAG_NO_LZ == 0)) {
            return None;
        }
        if !s.sregs.set(REG_MENU, u16::from(flags & FLAG_MENU != 0)) {
            return None;
        }
        let publish = r.u8()?;
        (s.ap_ssid, s.ap_ssid_len) = r.field(valid_ssid)?;
        (s.ap_pass, s.ap_pass_len) = r.field(valid_pass)?;
        (s.sta_ssid, s.sta_ssid_len) = r.field(|t| t.is_empty() || valid_ssid(t))?;
        (s.sta_pass, s.sta_pass_len) = r.field(|t| t.is_empty() || valid_pass(t))?;
        if (s.sta_ssid_len == 0) != (s.sta_pass_len == 0) {
            return None;
        }
        for slot in s.fwd.iter_mut() {
            *slot = Forward::read(r.take(FWD_REC)?)?;
        }
        for (i, f) in s.fwd.iter().flatten().enumerate() {
            if s.fwd
                .iter()
                .flatten()
                .skip(i + 1)
                .any(|g| g.name().eq_ignore_ascii_case(f.name()))
            {
                return None;
            }
        }
        s.publish = match publish {
            NO_PUB => None,
            i => {
                s.fwd.get(usize::from(i))?.as_ref()?;
                Some(i)
            }
        };
        s.beacon = if version == VERSION {
            BeaconMode::from_code(r.u8()?)?
        } else {
            BeaconMode::On
        };
        Some(s)
    }
}

impl Forward {
    /// One blob record: `Some(None)` for an unused slot, `None` if bad.
    fn read(rec: &[u8]) -> Option<Option<Forward>> {
        let mut r = Rd { b: rec, pos: 0 };
        match r.u8()? {
            0 => rec.iter().all(|&b| b == 0).then_some(None),
            1 => {
                let (name, name_len) = r.field::<FWD_NAME>(valid_fwd_name)?;
                let (host, host_len) = r.field::<HOST_MAX>(valid_host)?;
                let port = r.u16()?;
                let mode = WireMode::from_code(r.u8()?)?;
                if port == 0 {
                    return None;
                }
                Some(Some(Forward {
                    name,
                    name_len,
                    host,
                    host_len,
                    port,
                    mode,
                }))
            }
            _ => None,
        }
    }
}

/// CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection, no xorout.
/// Also used by the directory blob.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

struct Wr<'a> {
    out: &'a mut [u8],
    pos: usize,
}

impl Wr<'_> {
    fn put(&mut self, s: &[u8]) {
        let end = self.pos.saturating_add(s.len());
        if let Some(d) = self.out.get_mut(self.pos..end) {
            d.copy_from_slice(s);
            self.pos = end;
        }
    }
}

struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
    }

    /// A length byte plus `N` bytes: length within `N`, text accepted by
    /// `ok`, padding all zero.
    fn field<const N: usize>(&mut self, ok: fn(&str) -> bool) -> Option<([u8; N], u8)> {
        let len = self.u8()?;
        let buf: [u8; N] = self.take(N)?.try_into().ok()?;
        let used = usize::from(len);
        let t = core::str::from_utf8(buf.get(..used)?).ok()?;
        if !ok(t) || !buf.get(used..)?.iter().all(|&b| b == 0) {
            return None;
        }
        Some((buf, len))
    }
}
