//! The full-screen setup menu (`AT+MENU`): input decoder and state machine.
//!
//! Pure logic, no I/O and no knowledge of `Modem`. The modem hands every
//! host byte to `Decoder::feed` while the menu is open (the AT parser and
//! the `+++` escape see nothing), passes each key with a read-only `View`
//! of its state to `Ui::key`, and carries out the `Action` that comes back.
//! Edits never change settings here: they compose the AT command line the
//! user would type (`AT+BEACON=QUIET`, `ATS30=7`, `AT+WSTA=Home,pass`...)
//! and ask the modem to run it (`Action::Run`) through the AT parser and
//! the AT dispatch, so the menu takes exactly what the AT commands take.
//! Field steps are checked early with the same validators the parser uses
//! (`valid_ssid`, `valid_host`, `parse_port`, ...).
//!
//! Screens (see `screens.rs`): main menu, Directory and Scan, Link Status,
//! Settings, Forwards, Wi-Fi, About. Keys: arrows (`ESC [ A..D` and
//! `ESC O A..D`), Enter (CR, LF or CR LF), Esc (an ESC with nothing after it
//! for 150 ms), Backspace (BS or DEL), Tab, and printable keys (the first
//! letter of an item jumps to it; letters are commands on the directory
//! and forward lists). Other escape sequences are swallowed whole.

pub mod render;
pub mod screens;

use crate::at::{AtCmd, ResultCode};
use crate::directory::{Directory, DIR_MAX};
use crate::discovery::valid_station;
use crate::lan::FoundList;
use crate::radio::RxInfo;
use crate::settings::{
    parse_port, valid_fwd_name, valid_host, valid_pass, valid_ssid, BeaconMode, Settings, WireMode,
    MAX_FWD,
};
use crate::tier::Tier;
pub use render::Render;

/// An ESC with nothing after it for this long is the Esc key.
pub const ESC_MS: u32 = 150;
/// Live rows (status line, ages, link figures) are compared this often.
pub const REFRESH_MS: u32 = 1000;
/// Longest text typed into one field (a WPA2 password).
pub const EDIT_MAX: usize = 63;
/// Longest AT line the menu composes (`AT+WSTA=` + 32 + `,` + 63 = 104).
pub const LINE_MAX: usize = 128;
/// Longest message on the help line.
pub const MSG_MAX: usize = 76;
/// Directory rows shown at once.
pub const DIR_ROWS: u8 = 15;

/// Main menu items: name (its first letter is the hot key) and what it is.
pub const MAIN: [(&str, &str); 8] = [
    ("Directory and Scan", "stations heard, scan, dial, pin"),
    ("Link Status", "tier, SNR, RSSI, retrains, speed"),
    ("Settings", "privacy, power, tier cap, answer"),
    ("Forwards", "Wi-Fi forwards and publish"),
    ("Wi-Fi", "hotspot and client network"),
    ("About", "banner, version, radio, memory"),
    ("Save settings", "keep them after a reboot (AT&W)"),
    ("Exit menu", "back to AT commands"),
];
/// Settings screen fields.
pub const SETTINGS: [&str; 7] = [
    "Station name",
    "Privacy mode",
    "TX power (S30)",
    "Tier cap (S21)",
    "Auto-answer (S0)",
    "Compression (S31)",
    "Boot to menu (S32)",
];
/// Wi-Fi screen fields.
pub const WIFI: [&str; 6] = [
    "Hotspot",
    "Hotspot name",
    "Hotspot password",
    "Client network",
    "Client password",
    "Forget client network",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    Back,
    /// A printable ASCII byte.
    Char(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dec {
    Ground,
    /// ESC seen, waiting for `[`, `O`, or the timeout.
    Esc,
    /// Inside `ESC [`: parameter bytes seen so far.
    Csi(u8),
    /// `ESC O` seen.
    Ss3,
}

/// Host bytes to keys.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    st: Dec,
    esc_at: u32,
    /// The last byte was CR: a LF right after it is part of the same Enter.
    last_cr: bool,
}

impl Decoder {
    /// Fresh decoder; `after_cr` swallows a LF that ends the line that
    /// opened the menu (`AT+MENU` CR LF).
    pub const fn new(after_cr: bool) -> Decoder {
        Decoder {
            st: Dec::Ground,
            esc_at: 0,
            last_cr: after_cr,
        }
    }

    /// One byte in, up to two keys out (a lone ESC followed by an ordinary
    /// byte gives Esc and that byte's key).
    pub fn feed(&mut self, b: u8, now: u32) -> [Option<Key>; 2] {
        let mut out = Keys::default();
        // A lone ESC that already waited long enough is the Esc key, even
        // when no tick ran in between.
        if self.st == Dec::Esc && now.wrapping_sub(self.esc_at) >= ESC_MS {
            self.st = Dec::Ground;
            out.push(Key::Esc);
        }
        match self.st {
            Dec::Ground => {
                let k = self.ground(b, now);
                out.push_opt(k);
            }
            Dec::Esc => match b {
                b'[' => self.st = Dec::Csi(0),
                b'O' => self.st = Dec::Ss3,
                0x1B => {
                    out.push(Key::Esc);
                    self.esc_at = now;
                }
                _ => {
                    out.push(Key::Esc);
                    self.st = Dec::Ground;
                    let k = self.ground(b, now);
                    out.push_opt(k);
                }
            },
            Dec::Csi(n) => match b {
                0x40..=0x7E => {
                    self.st = Dec::Ground;
                    out.push_opt(arrow(b));
                }
                // Parameters and intermediates: swallowed however many
                // come (the count only saturates) until the final byte.
                0x20..=0x3F => self.st = Dec::Csi(n.saturating_add(1).min(16)),
                _ => {
                    self.st = Dec::Ground;
                    let k = self.ground(b, now);
                    out.push_opt(k);
                }
            },
            Dec::Ss3 => {
                self.st = Dec::Ground;
                if (0x40..=0x7E).contains(&b) {
                    out.push_opt(arrow(b));
                } else {
                    // Not an SS3 final (ESC, CR, ...): an ordinary byte.
                    let k = self.ground(b, now);
                    out.push_opt(k);
                }
            }
        }
        out.k
    }

    fn ground(&mut self, b: u8, now: u32) -> Option<Key> {
        let cr = core::mem::take(&mut self.last_cr);
        match b {
            0x1B => {
                self.st = Dec::Esc;
                self.esc_at = now;
                None
            }
            b'\r' => {
                self.last_cr = true;
                Some(Key::Enter)
            }
            b'\n' if cr => None,
            b'\n' => Some(Key::Enter),
            8 | 0x7F => Some(Key::Back),
            9 => Some(Key::Tab),
            0x20..=0x7E => Some(Key::Char(b)),
            _ => None,
        }
    }

    /// From the tick: a lone ESC that waited `ESC_MS` is the Esc key.
    pub fn timeout(&mut self, now: u32) -> Option<Key> {
        if self.st == Dec::Esc && now.wrapping_sub(self.esc_at) >= ESC_MS {
            self.st = Dec::Ground;
            Some(Key::Esc)
        } else {
            None
        }
    }
}

fn arrow(b: u8) -> Option<Key> {
    match b {
        b'A' => Some(Key::Up),
        b'B' => Some(Key::Down),
        b'C' => Some(Key::Right),
        b'D' => Some(Key::Left),
        _ => None,
    }
}

#[derive(Default)]
struct Keys {
    k: [Option<Key>; 2],
    n: usize,
}

impl Keys {
    fn push(&mut self, k: Key) {
        if let Some(d) = self.k.get_mut(self.n) {
            *d = Some(k);
            self.n += 1;
        }
    }

    fn push_opt(&mut self, k: Option<Key>) {
        if let Some(k) = k {
            self.push(k);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Main,
    Dir,
    Link,
    Settings,
    Fwd,
    Wifi,
    About,
}

/// A field being typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// This station's name (`AT+NAME=<name>`; empty: the default).
    Name,
    /// A station name to pin (`AT+DIR=+NAME`).
    DirAdd,
    /// A new value for S register n (`ATSn=v`).
    Sreg(u8),
    /// Hotspot name (`AT+WAP=<name>,<current password>`).
    ApSsid,
    /// Hotspot password (`AT+WAP=<current name>,<password>`), masked.
    ApPass,
    /// Client network name, then its password (`AT+WSTA=<name>,<pass>`).
    StaSsid,
    /// Client network password, masked.
    StaPass,
    /// Forward name, host, port, mode (`AT+FWD=<name>,<host>,<port>,<T|R>`).
    FwdName,
    FwdHost,
    FwdPort,
    FwdMode,
}

impl Step {
    /// Prompt, longest text, masked.
    pub fn info(self) -> (&'static str, u8, bool) {
        match self {
            Step::Name => ("Station name, empty for the default", 16, false),
            Step::DirAdd => ("Name to pin", 16, false),
            Step::Sreg(_) => ("New value", 5, false),
            Step::ApSsid => ("Hotspot name", 32, false),
            Step::ApPass => ("Hotspot password", 63, true),
            Step::StaSsid => ("Client network name", 32, false),
            Step::StaPass => ("Client network password", 63, true),
            Step::FwdName => ("Forward name", 16, false),
            Step::FwdHost => ("Host", 63, false),
            Step::FwdPort => ("Port", 5, false),
            Step::FwdMode => ("Mode, T telnet or R raw", 1, false),
        }
    }
}

/// A yes or no question.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ask {
    /// Leaving with unsaved changes.
    Discard,
    DirDelete,
    FwdDelete,
}

/// What the modem should do after a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// Close the menu (clear screen, OK).
    Leave,
    /// Back to the saved profile, then leave.
    DiscardLeave,
    /// `AT&W`, then leave.
    SaveLeave,
    /// Leave, then dial the directory entry in this slot like `ATDT <name>`.
    Dial(u8),
    /// Start a scan like `AT+SCAN` (results go to the directory).
    Scan,
    /// Run the composed AT line (`Ui::take_line`) and report with `Ui::ran`.
    Run,
    /// Add the highlighted found host (by its address and port, so a list
    /// that changed meanwhile never swaps it for another) as a telnet
    /// forward and publish it (`AT+FWD=...,T` then `AT+PUB=...`), and report
    /// with `Ui::found_added`, `Ui::found_taken` or `Ui::found_gone`.
    AddFound {
        ip: [u8; 4],
        port: u16,
    },
}

/// What a `Run` was, for its message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ran {
    Setting,
    Save,
    Dir,
}

/// The commands a menu line may run: settings, forwards, directory, save.
pub fn allowed(cmd: &AtCmd<'_>) -> bool {
    matches!(
        cmd,
        AtCmd::SetReg(..)
            | AtCmd::Beacon(_)
            | AtCmd::WapPower(_)
            | AtCmd::WapSet { .. }
            | AtCmd::WstaSet { .. }
            | AtCmd::WstaForget
            | AtCmd::FwdSet { .. }
            | AtCmd::FwdDelete(_)
            | AtCmd::Pub(_)
            | AtCmd::PubNone
            | AtCmd::DirPin(_)
            | AtCmd::DirRemove(_)
            | AtCmd::Save
            | AtCmd::Name(_)
    )
}

/// Read-only modem state for keys and rows.
pub struct View<'a> {
    pub name: &'a str,
    pub set: &'a Settings,
    pub dir: &'a Directory,
    pub now: u32,
    /// `Modem::call_state` and `Modem::wire_state`.
    pub call_state: u8,
    pub wire_state: u8,
    /// Tier of the call that is up.
    pub tier: Option<Tier>,
    pub last_rx: Option<RxInfo>,
    /// Last retrains, oldest first.
    pub retrains: &'a [Option<Tier>],
    pub lz: bool,
    pub lz_x10: u32,
    /// Call data bytes per second (both directions) over the last second.
    pub bps: u32,
    /// The platform's Wi-Fi status line (empty: none yet).
    pub wifi: &'a [u8],
    pub radio: Option<(i8, u16)>,
    pub simnet: Option<[u8; 4]>,
    pub simnet_fail: bool,
    pub stack_free: Option<u32>,
    pub heap_min: Option<u32>,
    pub note: &'a [u8],
    pub scanning: bool,
    pub scan_hits: u8,
    /// A caller is ringing us.
    pub ringing: bool,
    /// Live settings differ from the saved profile.
    pub unsaved: bool,
    /// Telnet servers the mDNS browse found on the LAN.
    pub found: &'a FoundList,
}

/// Menu screen state.
pub struct Ui {
    pub screen: Screen,
    pub main_sel: u8,
    /// Selected row on the current screen.
    pub sel: u8,
    /// First directory entry shown.
    pub top: u8,
    pub step: Option<Step>,
    edit: [u8; EDIT_MAX],
    edit_len: u8,
    pub ask: Option<Ask>,
    /// The name a question is about.
    subject: [u8; 16],
    subject_len: u8,
    msg: [u8; MSG_MAX],
    msg_len: u8,
    pub msg_err: bool,
    /// The AT line being composed (may hold a password: wiped after use).
    line: [u8; LINE_MAX],
    line_len: u8,
    ran: Ran,
    /// Forward slot being edited (its port and mode fill the fields).
    fwd_edit: Option<u8>,
    /// The found host under the highlight (address, port), kept when the
    /// list changes so Enter adds exactly that host.
    pub found_at: Option<([u8; 4], u16)>,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

/// The `i`th occupied directory slot, in slot order (as `AT+DIR?` lists).
pub fn dir_slot(d: &Directory, i: u8) -> Option<usize> {
    (0..DIR_MAX)
        .filter(|&s| d.slot(s).is_some())
        .nth(usize::from(i))
}

fn wrap_next(i: u8, n: u8) -> u8 {
    if n == 0 {
        0
    } else {
        (i + 1) % n
    }
}

fn wrap_prev(i: u8, n: u8) -> u8 {
    if n == 0 {
        0
    } else {
        (i + n - 1) % n
    }
}

/// Item `from + 1` onwards (wrapping) whose label starts with `c` (any
/// case), and whether it is the only one.
fn jump(labels: &[&str], from: u8, c: u8) -> Option<(u8, bool)> {
    let n = labels.len();
    let starts = |i: usize| {
        labels
            .get(i)
            .and_then(|l| l.bytes().next())
            .is_some_and(|f| f.eq_ignore_ascii_case(&c))
    };
    let count = (0..n).filter(|&i| starts(i)).count();
    let first = (1..=n)
        .map(|k| (usize::from(from) + k) % n.max(1))
        .find(|&i| starts(i))?;
    Some((first as u8, count == 1))
}

impl Ui {
    pub const fn new() -> Ui {
        Ui {
            screen: Screen::Main,
            main_sel: 0,
            sel: 0,
            top: 0,
            step: None,
            edit: [0; EDIT_MAX],
            edit_len: 0,
            ask: None,
            subject: [0; 16],
            subject_len: 0,
            msg: [0; MSG_MAX],
            msg_len: 0,
            msg_err: false,
            line: [0; LINE_MAX],
            line_len: 0,
            ran: Ran::Setting,
            fwd_edit: None,
            found_at: None,
        }
    }

    /// Remember the found host now under the highlight (none on a forward
    /// slot or off the Forwards screen).
    pub fn track_found(&mut self, v: &View) {
        self.found_at = match self.sel.checked_sub(MAX_FWD as u8) {
            Some(i) if self.screen == Screen::Fwd => {
                v.found.new_host(v.set, i).map(|h| (h.ip(), h.port()))
            }
            _ => None,
        };
    }

    /// Text typed so far.
    pub fn edit(&self) -> &[u8] {
        self.edit.get(..usize::from(self.edit_len)).unwrap_or(&[])
    }

    /// The name a question is about.
    pub fn subject(&self) -> &[u8] {
        self.subject
            .get(..usize::from(self.subject_len))
            .unwrap_or(&[])
    }

    /// Message for the help line (empty: none).
    pub fn msg(&self) -> &[u8] {
        self.msg.get(..usize::from(self.msg_len)).unwrap_or(&[])
    }

    fn say(&mut self, err: bool, s: &str) {
        self.msg_len = 0;
        self.msg_err = err;
        for (d, &c) in self.msg.iter_mut().zip(s.as_bytes()) {
            *d = c;
            self.msg_len += 1;
        }
    }

    /// Copy the composed AT line into `out` and wipe it; returns its length.
    pub fn take_line(&mut self, out: &mut [u8; LINE_MAX]) -> usize {
        let n = usize::from(self.line_len).min(LINE_MAX);
        out.copy_from_slice(&self.line);
        self.line = [0; LINE_MAX];
        self.line_len = 0;
        n
    }

    /// Compose `s` as the AT line to run.
    pub fn set_line(&mut self, s: &[u8]) {
        self.line = [0; LINE_MAX];
        self.line_len = 0;
        self.add(s);
    }

    /// Append to the AT line (cut at `LINE_MAX`; the parser then refuses it).
    fn add(&mut self, s: &[u8]) {
        for &c in s {
            if let Some(d) = self.line.get_mut(usize::from(self.line_len)) {
                *d = c;
                self.line_len += 1;
            }
        }
    }

    fn add_num(&mut self, v: i32) {
        if v < 0 {
            self.add(b"-");
        }
        let mut tmp = [0u8; 10];
        let mut i = tmp.len();
        let mut u = v.unsigned_abs();
        loop {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'0' + (u % 10) as u8;
            }
            u /= 10;
            if u == 0 || i == 0 {
                break;
            }
        }
        let t = tmp;
        self.add(t.get(i..).unwrap_or(&[]));
    }

    fn set_subject(&mut self, s: &str) {
        self.subject_len = 0;
        for (d, &c) in self.subject.iter_mut().zip(s.as_bytes()) {
            *d = c;
            self.subject_len += 1;
        }
    }

    /// Start typing a field, the text box filled with `with`.
    fn begin(&mut self, step: Step, with: &[u8]) {
        self.step = Some(step);
        self.edit = [0; EDIT_MAX];
        self.edit_len = 0;
        let max = usize::from(step.info().1);
        for (d, &c) in self.edit.iter_mut().zip(with.iter().take(max)) {
            *d = c;
            self.edit_len += 1;
        }
    }

    /// Stop typing: the text box (it may hold a password) is wiped.
    fn end_edit(&mut self) {
        self.step = None;
        self.edit = [0; EDIT_MAX];
        self.edit_len = 0;
    }

    /// Everything that may hold a password is wiped.
    pub fn wipe(&mut self) {
        self.end_edit();
        self.line = [0; LINE_MAX];
        self.line_len = 0;
    }

    fn run(&mut self, ran: Ran) -> Action {
        self.ran = ran;
        Action::Run
    }

    /// The modem ran the line: `ok` is whether the AT dispatch said OK.
    pub fn ran(&mut self, ok: bool) {
        match (ok, self.ran) {
            (true, Ran::Save) => self.say(false, "Saved, like AT&W. It stays after a reboot."),
            (true, Ran::Dir) => self.say(false, "Done."),
            (true, Ran::Setting) => self.say(
                false,
                "Done, live now. Save settings keeps it after a reboot.",
            ),
            (false, _) => self.say(true, "Not taken: the same rules as the AT command apply."),
        }
    }

    /// The modem ran the two lines for a found host (`Action::AddFound`):
    /// `ok` when both said OK; `name` is the forward name used.
    pub fn found_added(&mut self, ok: bool, name: &str) {
        if !ok {
            self.say(
                true,
                "Not added: all 8 forwards are in use, or the AT rules refused it.",
            );
            return;
        }
        self.msg_len = 0;
        self.msg_err = false;
        let parts: [&[u8]; 3] = [
            b"Added ",
            name.as_bytes(),
            b" and published it. Save settings keeps it.",
        ];
        for &c in parts.iter().flat_map(|p| p.iter()) {
            if let Some(d) = self.msg.get_mut(usize::from(self.msg_len)) {
                *d = c;
                self.msg_len += 1;
            }
        }
    }

    /// The highlighted found host left the list before Enter: nothing was
    /// added. The highlight now tracks the row it is on.
    pub fn found_gone(&mut self, v: &View) {
        self.say(
            true,
            "That host left the list: nothing added. Check the row and try again.",
        );
        self.track_found(v);
    }

    /// A found host's forward name already belongs to another forward:
    /// nothing was changed.
    pub fn found_taken(&mut self, name: &str) {
        self.msg_len = 0;
        self.msg_err = true;
        let parts: [&[u8]; 3] = [
            b"A forward named ",
            name.as_bytes(),
            b" exists: delete or rename it first.",
        ];
        for &c in parts.iter().flat_map(|p| p.iter()) {
            if let Some(d) = self.msg.get_mut(usize::from(self.msg_len)) {
                *d = c;
                self.msg_len += 1;
            }
        }
    }

    /// The modem tried to start a scan.
    pub fn scan_started(&mut self, ok: bool) {
        if ok {
            self.say(false, "Scanning. Answers land in the list as they arrive.");
        } else {
            self.say(
                true,
                "No scan now: a call is up, HIDDEN mode, or a scan runs.",
            );
        }
    }

    /// One key. The result tells the modem what to do.
    pub fn key(&mut self, k: Key, v: &View) -> Action {
        self.msg_len = 0;
        if let Some(s) = self.step {
            return self.edit_key(k, s, v);
        }
        if let Some(a) = self.ask {
            return self.ask_key(k, a);
        }
        match self.screen {
            Screen::Main => self.main_key(k, v),
            Screen::Dir => self.dir_key(k, v),
            Screen::Settings => self.settings_key(k, v),
            Screen::Fwd => self.fwd_key(k, v),
            Screen::Wifi => self.wifi_key(k, v),
            Screen::Link | Screen::About => {
                if matches!(k, Key::Esc | Key::Left | Key::Enter) {
                    self.screen = Screen::Main;
                }
                Action::None
            }
        }
    }

    fn leave(&mut self, v: &View) -> Action {
        if v.unsaved {
            self.ask = Some(Ask::Discard);
            Action::None
        } else {
            Action::Leave
        }
    }

    fn main_key(&mut self, k: Key, v: &View) -> Action {
        let n = MAIN.len() as u8;
        match k {
            Key::Up => self.main_sel = wrap_prev(self.main_sel, n),
            Key::Down | Key::Tab => self.main_sel = wrap_next(self.main_sel, n),
            Key::Enter | Key::Right => return self.open_item(v),
            Key::Esc => return self.leave(v),
            Key::Char(c) => {
                let labels = MAIN.map(|m| m.0);
                if let Some((i, only)) = jump(&labels, self.main_sel, c) {
                    self.main_sel = i;
                    if only {
                        return self.open_item(v);
                    }
                }
            }
            _ => {}
        }
        Action::None
    }

    fn open_item(&mut self, v: &View) -> Action {
        let to = match self.main_sel {
            0 => Screen::Dir,
            1 => Screen::Link,
            2 => Screen::Settings,
            3 => Screen::Fwd,
            4 => Screen::Wifi,
            5 => Screen::About,
            6 => {
                self.set_line(b"AT&W");
                return self.run(Ran::Save);
            }
            _ => return self.leave(v),
        };
        self.screen = to;
        self.sel = 0;
        self.top = 0;
        Action::None
    }

    fn ask_key(&mut self, k: Key, a: Ask) -> Action {
        let c = match k {
            Key::Char(c) => c.to_ascii_uppercase(),
            Key::Esc => 0x1B,
            _ => return Action::None,
        };
        match (a, c) {
            // Esc always leaves: at this question it leaves without saving.
            (Ask::Discard, b'Y' | 0x1B) => {
                self.ask = None;
                Action::DiscardLeave
            }
            (Ask::Discard, b'S') => {
                self.ask = None;
                Action::SaveLeave
            }
            (Ask::Discard, b'N') => {
                self.ask = None;
                Action::None
            }
            (Ask::DirDelete | Ask::FwdDelete, b'Y') => {
                self.ask = None;
                self.run(if a == Ask::DirDelete {
                    Ran::Dir
                } else {
                    Ran::Setting
                })
            }
            (Ask::DirDelete | Ask::FwdDelete, b'N' | 0x1B) => {
                self.ask = None;
                self.line = [0; LINE_MAX];
                self.line_len = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn dir_key(&mut self, k: Key, v: &View) -> Action {
        let n = v.dir.len().min(DIR_MAX) as u8;
        self.sel = self.sel.min(n.saturating_sub(1));
        let entry = dir_slot(v.dir, self.sel).and_then(|s| v.dir.slot(s));
        match k {
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Tab => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Key::Esc | Key::Left => self.screen = Screen::Main,
            Key::Enter => {
                if let Some(s) = dir_slot(v.dir, self.sel) {
                    return Action::Dial(s as u8);
                }
            }
            Key::Char(c) => match c.to_ascii_uppercase() {
                b'S' => return Action::Scan,
                b'A' => self.begin(Step::DirAdd, b""),
                b'P' => match entry {
                    Some(e) if e.pinned => self.say(false, "Already pinned."),
                    Some(e) => {
                        self.set_line(b"AT+DIR=+");
                        self.add(e.name().as_bytes());
                        return self.run(Ran::Dir);
                    }
                    None => {}
                },
                b'D' => {
                    if let Some(e) = entry {
                        self.set_line(b"AT+DIR=-");
                        self.add(e.name().as_bytes());
                        self.set_subject(e.name());
                        self.ask = Some(Ask::DirDelete);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + DIR_ROWS {
            self.top = self.sel + 1 - DIR_ROWS;
        }
        Action::None
    }

    fn settings_key(&mut self, k: Key, v: &View) -> Action {
        let n = SETTINGS.len() as u8;
        let step: i32 = match k {
            Key::Up => {
                self.sel = wrap_prev(self.sel, n);
                return Action::None;
            }
            Key::Down | Key::Tab => {
                self.sel = wrap_next(self.sel, n);
                return Action::None;
            }
            Key::Esc => {
                self.screen = Screen::Main;
                return Action::None;
            }
            Key::Char(c) => {
                if let Some((i, _)) = jump(&SETTINGS, self.sel, c) {
                    self.sel = i;
                }
                return Action::None;
            }
            Key::Left => -1,
            Key::Right => 1,
            Key::Enter => 0,
            _ => return Action::None,
        };
        let reg = |n: u8| i32::from(v.set.sregs.get(n).unwrap_or(0) as i16);
        match self.sel {
            0 => {
                if step == 0 {
                    self.begin(Step::Name, v.name.as_bytes());
                }
                Action::None
            }
            1 => {
                let modes = [BeaconMode::On, BeaconMode::Quiet, BeaconMode::Hidden];
                let i = modes.iter().position(|m| *m == v.set.beacon).unwrap_or(0);
                let j = (if step < 0 { i + 2 } else { i + 1 }) % 3;
                let m = modes.get(j).copied().unwrap_or(BeaconMode::On);
                self.set_line(b"AT+BEACON=");
                self.add(m.name().as_bytes());
                self.run(Ran::Setting)
            }
            s @ 2..=4 => {
                let r = [30u8, 21, 0].get(usize::from(s - 2)).copied().unwrap_or(0);
                if step == 0 {
                    let mut t = [0u8; 8];
                    let len = fmt_i32(reg(r), &mut t);
                    self.begin(Step::Sreg(r), t.get(..len).unwrap_or(&[]));
                    Action::None
                } else {
                    self.set_line(b"ATS");
                    self.add_num(i32::from(r));
                    self.add(b"=");
                    self.add_num(reg(r) + step);
                    self.run(Ran::Setting)
                }
            }
            s => {
                let r = if s == 5 { 31 } else { 32 };
                self.set_line(b"ATS");
                self.add_num(i32::from(r));
                self.add(if reg(r) == 0 { b"=1" } else { b"=0" });
                self.run(Ran::Setting)
            }
        }
    }

    fn wifi_key(&mut self, k: Key, v: &View) -> Action {
        let n = WIFI.len() as u8;
        match k {
            Key::Up => self.sel = wrap_prev(self.sel, n),
            Key::Down | Key::Tab => self.sel = wrap_next(self.sel, n),
            Key::Esc => self.screen = Screen::Main,
            Key::Char(c) => {
                if let Some((i, _)) = jump(&WIFI, self.sel, c) {
                    self.sel = i;
                }
            }
            Key::Enter | Key::Left | Key::Right => {
                let sta = v.set.sta_ssid();
                match self.sel {
                    0 => {
                        self.set_line(if v.set.ap_on {
                            b"AT+WAP=0"
                        } else {
                            b"AT+WAP=1"
                        });
                        return self.run(Ran::Setting);
                    }
                    _ if k != Key::Enter => {}
                    1 => self.begin(Step::ApSsid, v.set.ap_ssid().as_bytes()),
                    2 => {
                        self.set_line(b"AT+WAP=");
                        self.add(v.set.ap_ssid().as_bytes());
                        self.add(b",");
                        self.begin(Step::ApPass, b"");
                    }
                    3 => self.begin(Step::StaSsid, sta.as_bytes()),
                    4 if sta.is_empty() => {
                        self.say(true, "No client network yet: set the client network first.")
                    }
                    4 => {
                        self.set_line(b"AT+WSTA=");
                        self.add(sta.as_bytes());
                        self.add(b",");
                        self.begin(Step::StaPass, b"");
                    }
                    _ if sta.is_empty() => self.say(false, "No client network to forget."),
                    _ => {
                        self.set_line(b"AT+WSTA=0");
                        return self.run(Ran::Setting);
                    }
                }
            }
            _ => {}
        }
        Action::None
    }

    fn fwd_key(&mut self, k: Key, v: &View) -> Action {
        // The 8 forward slots, then one row per found host not a forward yet.
        let n = MAX_FWD as u8 + v.found.new_count(v.set);
        self.sel = self.sel.min(n.saturating_sub(1));
        let found = self.sel.checked_sub(MAX_FWD as u8);
        let cur = v.set.fwd.get(usize::from(self.sel)).copied().flatten();
        match k {
            Key::Up => {
                self.sel = wrap_prev(self.sel, n);
                self.track_found(v);
            }
            Key::Down | Key::Tab => {
                self.sel = wrap_next(self.sel, n);
                self.track_found(v);
            }
            Key::Esc | Key::Left => self.screen = Screen::Main,
            Key::Enter => match (found, cur) {
                // Only this key on a found row ever adds it: the host the
                // highlight was on (the modem refuses it if it is gone).
                (Some(_), _) => {
                    if self.found_at.is_none() {
                        self.track_found(v);
                    }
                    if let Some((ip, port)) = self.found_at {
                        return Action::AddFound { ip, port };
                    }
                }
                (None, Some(f)) => self.fwd_begin_edit(self.sel, f.name(), f.host()),
                (None, None) => self.fwd_begin_add(v),
            },
            Key::Char(c) => match (c.to_ascii_uppercase(), cur) {
                (b'A', _) => self.fwd_begin_add(v),
                (b'E', Some(f)) => self.fwd_begin_edit(self.sel, f.name(), f.host()),
                (b'D', Some(f)) => {
                    self.set_line(b"AT+FWD=");
                    self.add(f.name().as_bytes());
                    self.set_subject(f.name());
                    self.ask = Some(Ask::FwdDelete);
                }
                (b'P', Some(f)) => {
                    if v.set.publish == Some(self.sel) {
                        self.set_line(b"AT+PUB=0");
                    } else {
                        self.set_line(b"AT+PUB=");
                        self.add(f.name().as_bytes());
                    }
                    return self.run(Ran::Setting);
                }
                _ => {}
            },
            _ => {}
        }
        Action::None
    }

    fn fwd_begin_add(&mut self, v: &View) {
        if v.set.fwd_count() >= MAX_FWD {
            self.say(true, "All 8 forwards are in use: delete one first.");
            return;
        }
        self.fwd_edit = None;
        self.set_line(b"");
        self.begin(Step::FwdName, b"");
    }

    fn fwd_begin_edit(&mut self, slot: u8, name: &str, host: &str) {
        self.fwd_edit = Some(slot);
        self.set_line(b"AT+FWD=");
        self.add(name.as_bytes());
        self.add(b",");
        self.begin(Step::FwdHost, host.as_bytes());
    }

    fn edit_key(&mut self, k: Key, step: Step, v: &View) -> Action {
        match k {
            Key::Char(c) => {
                if self.edit_len < step.info().1 {
                    if let Some(d) = self.edit.get_mut(usize::from(self.edit_len)) {
                        *d = c;
                        self.edit_len += 1;
                    }
                }
                Action::None
            }
            Key::Back => {
                self.edit_len = self.edit_len.saturating_sub(1);
                if let Some(d) = self.edit.get_mut(usize::from(self.edit_len)) {
                    *d = 0;
                }
                Action::None
            }
            Key::Esc => {
                self.wipe();
                self.fwd_edit = None;
                Action::None
            }
            Key::Enter => self.accept(step, v),
            _ => Action::None,
        }
    }

    /// Enter in a text box: check the field, then the next field or run.
    fn accept(&mut self, step: Step, v: &View) -> Action {
        let mut buf = [0u8; EDIT_MAX];
        let n = usize::from(self.edit_len).min(EDIT_MAX);
        if let (Some(d), Some(s)) = (buf.get_mut(..n), self.edit.get(..n)) {
            d.copy_from_slice(s);
        }
        let text = core::str::from_utf8(buf.get(..n).unwrap_or(&[])).unwrap_or("");
        let act = self.accept_text(step, text, v);
        buf.fill(0);
        act
    }

    fn accept_text(&mut self, step: Step, text: &str, v: &View) -> Action {
        let next = |ui: &mut Ui, s: Step, with: &[u8]| {
            ui.begin(s, with);
            Action::None
        };
        match step {
            Step::Name => {
                if !(text.is_empty() || valid_station(text)) {
                    self.say(true, "A name is 1 to 16 printable characters, no colon.");
                    return Action::None;
                }
                self.end_edit();
                self.set_line(b"AT+NAME=");
                self.add(text.as_bytes());
                self.run(Ran::Setting)
            }
            Step::DirAdd => {
                if !valid_station(text) {
                    self.say(true, "A name is 1 to 16 printable characters, no colon.");
                    return Action::None;
                }
                self.end_edit();
                self.set_line(b"AT+DIR=+");
                self.add(text.as_bytes());
                self.run(Ran::Dir)
            }
            Step::Sreg(r) => {
                self.end_edit();
                self.set_line(b"ATS");
                self.add_num(i32::from(r));
                self.add(b"=");
                self.add(text.as_bytes());
                self.run(Ran::Setting)
            }
            Step::ApSsid => {
                if !valid_ssid(text) {
                    self.say(
                        true,
                        "A network name is 1 to 32 printable characters, no comma.",
                    );
                    return Action::None;
                }
                self.end_edit();
                self.set_line(b"AT+WAP=");
                self.add(text.as_bytes());
                self.add(b",");
                self.add(v.set.ap_pass().as_bytes());
                self.run(Ran::Setting)
            }
            Step::ApPass | Step::StaPass => {
                if !valid_pass(text) {
                    self.say(
                        true,
                        "A password is 8 to 63 printable characters, no comma.",
                    );
                    return Action::None;
                }
                self.end_edit();
                self.add(text.as_bytes());
                self.run(Ran::Setting)
            }
            Step::StaSsid => {
                if !valid_ssid(text) {
                    self.say(
                        true,
                        "A network name is 1 to 32 printable characters, no comma.",
                    );
                    return Action::None;
                }
                self.set_line(b"AT+WSTA=");
                self.add(text.as_bytes());
                self.add(b",");
                next(self, Step::StaPass, b"")
            }
            Step::FwdName => {
                if !valid_fwd_name(text) {
                    self.say(
                        true,
                        "A forward name is 1 to 16 characters, no colon or comma.",
                    );
                    return Action::None;
                }
                if v.set.forward(text).is_some() {
                    self.say(true, "That name is taken: edit that forward instead.");
                    return Action::None;
                }
                self.set_line(b"AT+FWD=");
                self.add(text.as_bytes());
                self.add(b",");
                next(self, Step::FwdHost, b"")
            }
            Step::FwdHost => {
                if !valid_host(text) {
                    self.say(
                        true,
                        "A host is 1 to 63 of A to Z, a to z, 0 to 9, dot, dash.",
                    );
                    return Action::None;
                }
                self.add(text.as_bytes());
                self.add(b",");
                let mut t = [0u8; 8];
                let len = match self.editing(v) {
                    Some(f) => fmt_i32(i32::from(f.port()), &mut t),
                    None => fmt_i32(23, &mut t),
                };
                next(self, Step::FwdPort, t.get(..len).unwrap_or(&[]))
            }
            Step::FwdPort => {
                let Some(port) = parse_port(text) else {
                    self.say(true, "A port is a number from 1 to 65535.");
                    return Action::None;
                };
                self.add(text.as_bytes());
                self.add(b",");
                let mode = match self.editing(v) {
                    Some(f) => f.mode(),
                    None => WireMode::default_for(port),
                };
                let m: &[u8] = if mode == WireMode::Telnet { b"T" } else { b"R" };
                next(self, Step::FwdMode, m)
            }
            Step::FwdMode => {
                if !(text.eq_ignore_ascii_case("T") || text.eq_ignore_ascii_case("R")) {
                    self.say(true, "Mode is T (telnet) or R (raw TCP).");
                    return Action::None;
                }
                self.end_edit();
                self.fwd_edit = None;
                self.add(text.as_bytes());
                self.run(Ran::Setting)
            }
        }
    }

    /// The forward being edited, if this is an edit.
    fn editing<'v>(&self, v: &'v View) -> Option<&'v crate::settings::Forward> {
        v.set
            .fwd
            .get(usize::from(self.fwd_edit?))
            .and_then(Option::as_ref)
    }
}

/// `v` in decimal into `out`; returns the length.
fn fmt_i32(v: i32, out: &mut [u8; 8]) -> usize {
    let mut tmp = [0u8; 8];
    let mut i = tmp.len();
    let mut u = v.unsigned_abs();
    loop {
        i -= 1;
        if let Some(d) = tmp.get_mut(i) {
            *d = b'0' + (u % 10) as u8;
        }
        u /= 10;
        if u == 0 || i == 1 {
            break;
        }
    }
    if v < 0 {
        i -= 1;
        if let Some(d) = tmp.get_mut(i) {
            *d = b'-';
        }
    }
    let s = tmp.get(i..).unwrap_or(&[]);
    if let Some(d) = out.get_mut(..s.len()) {
        d.copy_from_slice(s);
    }
    s.len()
}

/// Everything the menu keeps while it is open.
pub struct Menu {
    pub open: bool,
    pub dec: Decoder,
    pub ui: Ui,
    pub rd: Render,
    /// Latest time seen, for rows composed outside a tick.
    pub now: u32,
    /// Last once a second refresh.
    refreshed: u32,
    /// `Link::data_bytes` at the last refresh, and the rate since.
    bytes: u32,
    pub bps: u32,
    /// A NO CARRIER came while the menu was open: printed on leave.
    pub held_nc: bool,
    /// The last result code the AT dispatch gave while the menu was open.
    pub last: Option<ResultCode>,
    /// Live settings differ from the saved profile (updated each refresh).
    pub unsaved: bool,
}

impl Default for Menu {
    fn default() -> Self {
        Self::new()
    }
}

impl Menu {
    pub const fn new() -> Menu {
        Menu {
            open: false,
            dec: Decoder::new(false),
            ui: Ui::new(),
            rd: Render::new(),
            now: 0,
            refreshed: 0,
            bytes: 0,
            bps: 0,
            held_nc: false,
            last: None,
            unsaved: false,
        }
    }

    /// Open at the main menu; `after_cr`: the line that opened it ended in
    /// CR (a LF right after belongs to it). `bytes` is the call data count.
    pub fn start(&mut self, now: u32, bytes: u32, after_cr: bool) {
        self.open = true;
        self.dec = Decoder::new(after_cr);
        self.ui.wipe();
        self.ui = Ui::new();
        self.rd.start();
        self.now = now;
        self.refreshed = now;
        self.bytes = bytes;
        self.bps = 0;
        self.held_nc = false;
        self.last = None;
    }

    /// Closed: nothing typed is kept.
    pub fn stop(&mut self) {
        self.open = false;
        self.ui.wipe();
        self.ui = Ui::new();
        self.dec = Decoder::new(false);
    }

    /// From the tick: once a second the throughput is sampled and every row
    /// is compared again. True when that happened.
    pub fn tick(&mut self, now: u32, bytes: u32) -> bool {
        self.now = now;
        let dt = now.wrapping_sub(self.refreshed);
        if dt < REFRESH_MS {
            return false;
        }
        let moved = bytes.wrapping_sub(self.bytes);
        self.bps = u32::try_from(u64::from(moved) * 1000 / u64::from(dt.max(1))).unwrap_or(0);
        self.bytes = bytes;
        self.refreshed = now;
        self.rd.refresh();
        true
    }
}
