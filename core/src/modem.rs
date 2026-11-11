//! The Modem: Hayes AT glue, host buffers and lights around a `Link`.
//!
//! Host output is one byte stream (like a real modem's serial line): echo,
//! result codes, verbose info lines (`\r\nDIALING <name>\r\n`, `\r\nRINGING\r\n`,
//! `\r\nTRAINING SNR <x.x> dB\r\n`) and, in data mode, received call bytes.
//! Data mode is 8-bit transparent: nothing but call bytes reaches the host
//! while online. A RETRAIN that happens in data mode is kept as a notice and
//! printed after the `OK` of the escape (verbose only); AT&V shows the
//! current tier and the last retrains. NO CARRIER ends a call as on a real
//! modem, right after the received in-order data that fits in the host
//! buffer (the call never leaves NO CARRIER pending).
//!
//! Wi-Fi calls (see `wire.rs`): ATDT to a forward or `host:port` prints
//! `DIALING <target>`, raises a `WireReq` and waits S7 seconds for
//! `wire_connected`, which prints `CONNECT WIFI TCP` and enters data mode;
//! the escape, ATO and ATH work as on radio. A refused, failed or timed out
//! dial, or the far end closing, prints NO CARRIER (after the received data,
//! in data mode). An answered radio call with `AT+PUB` set is bridged to that
//! forward: the host stays in command mode and sees only `BRIDGE <name>`
//! (verbose) when the wire connects and NO CARRIER when the bridge ends.
//! One call at a time: a radio caller during a Wi-Fi call gets BUSY.
//!
//! Discovery (see `discovery.rs` and `directory.rs`): `tick` runs the beacon
//! timer, the scan answer slots and `AT+SCAN`, all only while no call
//! exists (radio or Wi-Fi). Beacon, ScanReq and ScanResp frames are handled
//! here and never reach the link. Heard stations (beacons, scan answers, a
//! caller's CallReq to us, the answerer's CallAck or Train on our dial) go
//! into the directory, never our own name. In HIDDEN mode a CallReq to us
//! from a name that is not a pinned directory entry (`AT+DIR=+NAME`) is
//! dropped before the link sees it, so nothing at all is sent (no RING, no
//! CallReject).

use crate::at::{format_result, parse_line, AtCmd, ResultCode, SRegs, REG_TX_POWER};
use crate::directory::{Directory, DIR_MAX};
use crate::discovery::{
    self, Answer, Beacon, Discovery, Scan, ScanReq, ScanResp, DISC_MAX, HEARD_WINDOW_MS,
    SCAN_SEND_LIMIT_MS, SCAN_WINDOW_MS, SLOTS, SLOT_MS, SVC_ACCEPTS, SVC_FORWARD, SVC_HOTSPOT,
};
use crate::frame::{valid_name, FrameType, Header, HEADER_LEN, NAME_MAX};
use crate::link::{Event, Link, Role, State};
use crate::radio::{Radio, RxInfo};
use crate::settings::{parse_hostport, BeaconMode, Forward, Settings, WireMode, MAX_FWD};
use crate::tier::{best_tier, Tier};
use crate::wire::{Call, Phase, Wire, WireReq};

/// ATI line 1.
pub const BANNER: &str = "LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM";
/// ATI line 2 is `v` + VERSION + space + RELEASE_NAME.
pub const RELEASE_NAME: &str = "r.420 BL4Z3B4ND ULTR4M0D3M";
/// ATI line 3.
pub const RELEASE_TAGLINE: &str = "S0N1C B00M R4D10 PR0 MAX";

/// Gain of the Heltec V4 GC1109 front end, shown in AT&V.
const GC1109_GAIN_DB: i32 = 12;

const OUT_CAP: usize = 1024;
/// Output space kept free of call data so result codes always fit.
const OUT_RESERVE: usize = 96;
/// Longest command line: `AT+WSTA=` + 32 + `,` + 63 is 104 bytes.
const LINE_CAP: usize = 128;
/// Longest Wi-Fi status line kept for `AT+WIFI?`.
pub const WIFI_STATUS_MAX: usize = 96;
/// Longest crash note kept for ATI.
pub const NOTE_MAX: usize = 64;
const LIGHT_MS: u32 = 100;
/// Retrains remembered for AT&V.
const HISTORY: usize = 4;
/// Least time between two directory saves asked by changes (flash wear).
const DIR_SAVE_GAP_MS: u32 = 60_000;

pub struct Config {
    pub name: [u8; 16],
    pub name_len: u8,
    pub sregs: SRegs,
}

impl Config {
    /// No name: only for the constant `Modem::blank`, never for a live modem.
    pub const BLANK: Config = Config {
        name: [0; NAME_MAX],
        name_len: 0,
        sregs: SRegs::new(),
    };

    /// Station name with default S registers. `None` unless the name is
    /// 1 to 16 printable ASCII characters (the same rule as a CallReq name),
    /// so a modem can never hold a name it could not dial or answer to.
    pub fn new(name: &str) -> Option<Config> {
        if !valid_name(name) || name.contains(':') {
            return None;
        }
        let mut c = Config {
            name: [0; NAME_MAX],
            name_len: 0,
            sregs: SRegs::new(),
        };
        let n = name.len();
        let (d, s) = (c.name.get_mut(..n)?, name.as_bytes().get(..n)?);
        d.copy_from_slice(s);
        c.name_len = n as u8;
        Some(c)
    }
}

/// `<tier name> <bps>`.
fn put_tier(t: &mut Txt, tier: Tier) {
    t.put(tier.name().as_bytes())
        .put(b" ")
        .num(tier.air_bps(), 1);
}

/// `a.b.c.d`.
fn put_ip(t: &mut Txt, ip: [u8; 4]) {
    for (i, b) in ip.iter().enumerate() {
        if i > 0 {
            t.put(b".");
        }
        t.num(u32::from(*b), 1);
    }
}

/// SNR in dB x4 as `<x.x>`: tenths rounded, a minus sign when negative.
fn put_snr(t: &mut Txt, snr_x4: i16) {
    let s = i32::from(snr_x4);
    let tenths = (s.unsigned_abs() * 10 + 2) / 4;
    t.put(if s < 0 { b"-" } else { b"" });
    t.num(tenths / 10, 1).put(b".").num(tenths % 10, 1);
}

fn name_of(cfg: &Config) -> &str {
    cfg.name
        .get(..cfg.name_len as usize)
        .and_then(|b| core::str::from_utf8(b).ok())
        .unwrap_or("")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lights {
    pub oh: bool,
    pub cd: bool,
    pub sd: bool,
    pub rd: bool,
    pub hs: bool,
    pub ec: bool,
    /// Data compression: a radio call is up and compresses (`/ARQ/LZ`).
    pub dc: bool,
    pub aa: bool,
    pub tier: Option<Tier>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Command,
    Data,
    OnlineCommand,
}

struct OutRing {
    buf: [u8; OUT_CAP],
    head: usize,
    len: usize,
}

impl OutRing {
    fn free(&self) -> usize {
        OUT_CAP - self.len
    }

    /// All or nothing.
    fn push(&mut self, s: &[u8]) -> bool {
        if s.len() > self.free() {
            return false;
        }
        for &b in s {
            if let Some(d) = self.buf.get_mut((self.head + self.len) % OUT_CAP) {
                *d = b;
                self.len += 1;
            }
        }
        true
    }

    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for d in out.iter_mut().take(n) {
            *d = self.buf.get(self.head).copied().unwrap_or(0);
            self.head = (self.head + 1) % OUT_CAP;
        }
        self.len -= n;
        n
    }
}

/// Small text builder for info lines.
struct Txt {
    b: [u8; 256],
    n: usize,
}

impl Txt {
    fn new() -> Self {
        Txt { b: [0; 256], n: 0 }
    }

    fn put(&mut self, s: &[u8]) -> &mut Self {
        for &c in s {
            if let Some(d) = self.b.get_mut(self.n) {
                *d = c;
                self.n += 1;
            }
        }
        self
    }

    /// Decimal, zero padded to `width` digits.
    fn num(&mut self, v: u32, width: usize) -> &mut Self {
        let mut tmp = [b'0'; 10];
        let mut i = tmp.len();
        let mut v = v;
        loop {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'0' + (v % 10) as u8;
            }
            v /= 10;
            if v == 0 || i == 0 {
                break;
            }
        }
        let start = i.min(tmp.len().saturating_sub(width));
        self.put(tmp.get(start..).unwrap_or(&[]))
    }

    /// Signed decimal, no padding.
    fn int(&mut self, v: i32) -> &mut Self {
        if v < 0 {
            self.put(b"-");
        }
        self.num(v.unsigned_abs(), 1)
    }

    /// S register value as `AT S n ?` prints it: zero padded to 3 digits,
    /// S30 signed.
    fn sreg_val(&mut self, n: u8, v: u16) -> &mut Self {
        if n == REG_TX_POWER {
            let s = i32::from(v as i16);
            if s < 0 {
                self.put(b"-");
            }
            self.num(s.unsigned_abs(), 3)
        } else {
            self.num(u32::from(v), 3)
        }
    }

    /// Hexadecimal, 4 digits.
    fn hex16(&mut self, v: u16) -> &mut Self {
        for shift in [12u32, 8, 4, 0] {
            let d = ((v >> shift) & 0xF) as u8;
            self.put(&[if d < 10 { b'0' + d } else { b'A' + d - 10 }]);
        }
        self
    }

    fn bytes(&self) -> &[u8] {
        self.b.get(..self.n).unwrap_or(&[])
    }
}

pub struct Modem {
    cfg: Config,
    link: Link,
    mode: Mode,
    out: OutRing,
    line: [u8; LINE_CAP],
    line_len: usize,
    line_over: bool,
    echo: bool,
    verbose: bool,
    /// S2 characters held as a possible escape (0 to 3).
    esc: u8,
    esc_last: u32,
    /// The held S2 characters can no longer be an escape (more input was
    /// offered after them); they only wait to be sent as data.
    esc_dead: bool,
    last_host: u32,
    /// Latest retrain seen in data mode, printed on escape.
    retrain_note: Option<Tier>,
    /// Last retrains, oldest first.
    retrains: [Option<Tier>; HISTORY],
    /// NO CARRIER waits until received data has drained.
    closing: bool,
    /// `AT+FB` was seen; the firmware answers it.
    fb_request: bool,
    /// Crash note from the last boot (printable ASCII), for ATI.
    note: [u8; NOTE_MAX],
    note_len: usize,
    /// Radio bring-up result for AT&V: (code, device errors word).
    radio_diag: Option<(i8, u16)>,
    /// Metadata of the last frame handed to `radio_rx`.
    last_rx: Option<RxInfo>,
    /// Hotspot, client network, forwards and publish setting. Its `sregs`,
    /// `echo` and `verbose` mirror the live `cfg.sregs`, `echo` and
    /// `verbose` (copied after every command line by `sync_set`).
    set: Settings,
    /// The profile `ATZ` restores: what was loaded or last saved.
    saved: Settings,
    /// `AT&W` was entered; the platform saves `settings()`.
    save_req: bool,
    /// Hotspot or client settings changed (AT+WAP, AT+WSTA, ATZ).
    wifi_changed: bool,
    /// Last line from `set_wifi_status`.
    wifi: [u8; WIFI_STATUS_MAX],
    wifi_len: usize,
    /// Wi-Fi call state, wire request and close flags, the two wire rings
    /// and the telnet filter.
    wire: Wire,
    /// Modem task stack headroom reported by the firmware, bytes.
    stack_free: Option<u32>,
    /// Lowest free heap since boot reported by the firmware, bytes.
    heap_min: Option<u32>,
    /// `AT+SIMNET`: the simulated radio's peer while it is on. Never saved.
    simnet: Option<[u8; 4]>,
    /// A switch the platform has not taken yet (see `take_simnet_request`).
    simnet_req: Option<Option<[u8; 4]>>,
    /// The platform could not open the SimNet UDP socket (AT&V `NET FAIL`).
    simnet_fail: bool,
    /// Stations heard or pinned (AT+DIR). Saved by the platform on its own
    /// schedule (`take_dir_save`), not part of the settings profile.
    dir: Directory,
    /// Beacon timer, scan answer slot, AT+SCAN state.
    disc: Discovery,
    /// The directory changed since the last save.
    dir_dirty: bool,
    /// A directory save is wanted (taken by `take_dir_save`).
    dir_save: bool,
    /// When the directory was last saved (or the modem started).
    dir_saved_at: u32,
    /// `AT+DIR?` listing in progress: next slot to print and the time the
    /// command was typed (ages are as of then).
    dir_list: Option<(u8, u32)>,
    /// The name of the current radio dial, credited when it answers.
    dial_name: [u8; NAME_MAX],
    dial_len: u8,
}

fn onoff(on: bool) -> &'static [u8] {
    if on {
        b"ON"
    } else {
        b"OFF"
    }
}

impl Modem {
    /// Builds a `Modem` by value (the host tests and the sim use this). It is
    /// large (about 11 KB); the firmware does not call it: `lfd3k_init` starts
    /// from the const `Modem::blank()` in a static slot and calls `reset`.
    #[inline(never)]
    pub fn new(cfg: Config) -> Self {
        let mut m = Self::blank();
        m.reset(cfg);
        m
    }

    /// A nameless modem in its initial state, as a constant: a static can
    /// hold it, and `reset` then makes it a real one in place (the `Modem` is
    /// over 11 KB, too big to build on a small task stack and move).
    pub const fn blank() -> Self {
        Modem {
            cfg: Config::BLANK,
            link: Link::new(),
            mode: Mode::Command,
            out: OutRing {
                buf: [0; OUT_CAP],
                head: 0,
                len: 0,
            },
            line: [0; LINE_CAP],
            line_len: 0,
            line_over: false,
            echo: true,
            verbose: true,
            esc: 0,
            esc_last: 0,
            esc_dead: false,
            last_host: 0,
            retrain_note: None,
            retrains: [None; HISTORY],
            closing: false,
            fb_request: false,
            note: [0; NOTE_MAX],
            note_len: 0,
            radio_diag: None,
            last_rx: None,
            set: Settings::BLANK,
            saved: Settings::BLANK,
            save_req: false,
            wifi_changed: false,
            wifi: [0; WIFI_STATUS_MAX],
            wifi_len: 0,
            wire: Wire::new(),
            stack_free: None,
            heap_min: None,
            simnet: None,
            simnet_req: None,
            simnet_fail: false,
            dir: Directory::new(),
            disc: Discovery::new(),
            dir_dirty: false,
            dir_save: false,
            dir_saved_at: 0,
            dir_list: None,
            dial_name: [0; NAME_MAX],
            dial_len: 0,
        }
    }

    /// Back to the state `Modem::new(cfg)` gives, assigning field by field in
    /// place (the largest temporary is one field, never a whole `Modem`).
    pub fn reset(&mut self, cfg: Config) {
        // Until the platform calls `load_settings` there is no hotspot
        // password and the hotspot is off.
        self.set = Settings::defaults(name_of(&cfg), "");
        self.set.ap_on = false;
        self.saved.clone_from(&self.set);
        self.cfg = cfg;
        self.link = Link::new();
        self.mode = Mode::Command;
        self.out.head = 0;
        self.out.len = 0;
        self.line_len = 0;
        self.line_over = false;
        self.echo = true;
        self.verbose = true;
        self.esc = 0;
        self.esc_last = 0;
        self.esc_dead = false;
        self.last_host = 0;
        self.retrain_note = None;
        self.retrains = [None; HISTORY];
        self.closing = false;
        self.fb_request = false;
        self.note_len = 0;
        self.radio_diag = None;
        self.last_rx = None;
        self.save_req = false;
        self.wifi_changed = false;
        self.wifi_len = 0;
        self.wire.reset_all();
        self.stack_free = None;
        self.heap_min = None;
        self.simnet = None;
        self.simnet_req = None;
        self.simnet_fail = false;
        self.dir = Directory::new();
        self.disc = Discovery::new();
        self.dir_dirty = false;
        self.dir_save = false;
        self.dir_saved_at = 0;
        self.dir_list = None;
        self.dial_len = 0;
    }

    /// True once when the platform should save the directory
    /// (`directory().to_blob`): after `AT&W`, or after a change once 60 s
    /// have passed since the last save (flash wear). Changes are entries
    /// added, replaced or removed, pins, services and best tiers; a beacon
    /// that only refreshes SNR or time asks for nothing.
    pub fn take_dir_save(&mut self) -> bool {
        core::mem::take(&mut self.dir_save)
    }

    /// The directory, for the platform's blob and the menu.
    pub fn directory(&self) -> &Directory {
        &self.dir
    }

    /// Boot: the directory loaded from the platform's blob
    /// (`Directory::from_blob`). An entry with this modem's own name is
    /// dropped (and a save asked for later).
    pub fn load_directory(&mut self, d: Directory) {
        self.dir = d;
        let dropped = self.dir.remove(name_of(&self.cfg));
        self.dir.take_changed();
        self.dir_dirty = dropped;
    }

    /// The privacy mode in force (AT+BEACON).
    pub fn beacon_mode(&self) -> BeaconMode {
        self.set.beacon
    }

    /// An AT+SCAN is in progress (waiting to send or collecting answers).
    pub fn scanning(&self) -> bool {
        self.disc.scan.is_some()
    }

    /// Answers counted by the scan in progress (0 when none).
    pub fn scan_hits(&self) -> u8 {
        self.disc.scan.map_or(0, |s| s.hits)
    }

    /// The platform's live Wi-Fi status line for `AT+WIFI?`
    /// (`WIFI AP <ssid> <n> CLIENTS STA <ssid|OFF> <ip|NO IP>`). Up to
    /// `WIFI_STATUS_MAX` bytes are kept; non-printable bytes become `?`.
    pub fn set_wifi_status(&mut self, line: &str) {
        self.wifi_len = 0;
        for (d, c) in self.wifi.iter_mut().zip(line.bytes()) {
            *d = if (0x20..0x7F).contains(&c) { c } else { b'?' };
            self.wifi_len += 1;
        }
    }

    /// Ask the platform to save `settings()` (as `AT&W` does). First boot
    /// uses it after loading the defaults.
    pub fn request_save(&mut self) {
        self.save_req = true;
    }

    /// The firmware's modem task stack high-water mark (bytes still free),
    /// shown by `AT&V` as `STACK FREE <n>`.
    pub fn set_stack_free(&mut self, bytes: u32) {
        self.stack_free = Some(bytes);
    }

    /// The firmware's lowest free heap since boot (bytes), shown by `AT&V`
    /// as `HEAP MIN <n>`.
    pub fn set_heap_min(&mut self, bytes: u32) {
        self.heap_min = Some(bytes);
    }

    /// True once after `AT&W` was entered (clears the flag). The platform
    /// then stores `settings()`.
    pub fn take_save_request(&mut self) -> bool {
        core::mem::take(&mut self.save_req)
    }

    /// True once after AT+WAP, AT+WSTA (any form that took effect) or ATZ
    /// (clears the flag): the platform re-reads `settings()` and re-applies Wi-Fi.
    pub fn take_wifi_changed(&mut self) -> bool {
        core::mem::take(&mut self.wifi_changed)
    }

    /// The current settings, live S registers, E and V included.
    pub fn settings(&self) -> &Settings {
        &self.set
    }

    /// The saved profile (what `AT&W` last stored, what `ATZ` restores). This
    /// is what the platform writes to flash, never the live settings.
    pub fn saved_settings(&self) -> &Settings {
        &self.saved
    }

    fn sync_set(&mut self) {
        self.set.sregs = self.cfg.sregs;
        self.set.echo = self.echo;
        self.set.verbose = self.verbose;
    }

    /// Boot: make `s` (from `Settings::from_blob`) the current settings and
    /// the profile `ATZ` restores. Registers, echo and verbose apply now.
    pub fn load_settings(&mut self, s: Settings) {
        self.cfg.sregs = s.sregs;
        self.echo = s.echo;
        self.verbose = s.verbose;
        self.saved.clone_from(&s);
        self.set = s;
        self.wifi_changed = true;
    }

    /// True while the generated hotspot password may be shown on the OLED:
    /// until a device joins the hotspot (`note_ap_client_joined`) or `AT&W`
    /// saved with a client network set. Kept in the saved blob.
    pub fn show_ap_pass(&self) -> bool {
        self.set.show_ap_pass
    }

    /// The platform saw a device join the hotspot: stop showing the password.
    /// Only the cleared flag is saved (the save is built from the saved
    /// profile, so other unsaved settings stay unsaved), and a save is asked
    /// for only when the saved flag was still 1.
    pub fn note_ap_client_joined(&mut self) {
        self.set.show_ap_pass = false;
        if self.saved.show_ap_pass {
            self.saved.show_ap_pass = false;
            self.save_req = true;
        }
    }

    /// Remember the previous boot's crash note for ATI. Only printable
    /// ASCII is kept (anything else becomes `?`), at most `NOTE_MAX` bytes.
    pub fn set_last_panic(&mut self, note: &[u8]) {
        self.note_len = 0;
        for (d, &c) in self.note.iter_mut().zip(note.iter()) {
            *d = if (0x20..0x7F).contains(&c) { c } else { b'?' };
            self.note_len += 1;
        }
    }

    /// Radio bring-up result for AT&V: `code` 0 is OK, negative is the
    /// driver error (-1 busy timeout, -2 no chip, -3 config); `errs` is the
    /// chip's device-errors word.
    pub fn set_radio_diag(&mut self, code: i8, errs: u16) {
        self.radio_diag = Some((code, errs));
    }

    /// True once after `AT+FB` was entered (clears the flag).
    pub fn take_fb_request(&mut self) -> bool {
        core::mem::take(&mut self.fb_request)
    }

    /// Taken once after `AT+SIMNET` switched radios: `Some(Some(ip))` use the
    /// simulated radio with that peer, `Some(None)` back to the SX1262. Only
    /// accepted while no call exists. The platform swaps the radio and then
    /// calls `radio_changed`.
    pub fn take_simnet_request(&mut self) -> Option<Option<[u8; 4]>> {
        self.simnet_req.take()
    }

    /// The simulated radio's peer while `AT+SIMNET` has it on.
    pub fn simnet(&self) -> Option<[u8; 4]> {
        self.simnet
    }

    /// The platform's SimNet socket state: `true` when it could not be
    /// opened (socket or bind failed). AT&V then shows `NET FAIL` after the
    /// `RADIO SIMNET <ip>` line's address; the modem stays on SimNet and
    /// `AT+SIMNET=0` (or `AT+SIMNET=<ip>` again, a retry) recovers.
    pub fn set_simnet_net_fail(&mut self, fail: bool) {
        self.simnet_fail = fail;
    }

    /// The radio was swapped: listen again on the next pump.
    pub fn radio_changed(&mut self) {
        self.link.radio_changed();
    }

    /// Metadata of the last received frame, if any.
    pub fn last_rx(&self) -> Option<RxInfo> {
        self.last_rx
    }

    /// Current tier while a call is up or training.
    pub fn tier(&self) -> Option<Tier> {
        self.link.tier()
    }

    /// Call state for the status light panel: 0 idle, 1 dialing, 2 ringing,
    /// 3 training, 4 online (data mode), 5 command-online.
    /// A Wi-Fi call shows as 1 while dialing and 4 or 5 once online; a
    /// bridged radio call shows its radio state.
    pub fn call_state(&self) -> u8 {
        match self.wire.call {
            Call::Wire(Phase::Dialing { .. }) => return 1,
            Call::Wire(_) => return if self.mode == Mode::Data { 4 } else { 5 },
            _ => {}
        }
        match self.link.state() {
            State::Idle => 0,
            State::Dialing | State::AwaitTrain => 1,
            State::Ringing => 2,
            State::AwaitTierSet | State::AwaitTierAck | State::Probe => 3,
            State::Connected | State::Resync => {
                if self.mode == Mode::Data {
                    4
                } else {
                    5
                }
            }
        }
    }

    /// A radio call or an online Wi-Fi call exists.
    pub fn is_connected(&self) -> bool {
        self.link.is_connected() || self.wire_online()
    }

    fn wire_online(&self) -> bool {
        matches!(
            self.wire.call,
            Call::Wire(Phase::Online | Phase::Ending { .. })
        )
    }

    /// Wire state for the status field: 0 none, 1 dialing, 2 online,
    /// 3 bridging (a radio call bridged to the published forward).
    pub fn wire_state(&self) -> u8 {
        match self.wire.call {
            Call::None => 0,
            Call::Wire(Phase::Dialing { .. }) => 1,
            Call::Wire(_) => 2,
            Call::Bridge(_) => 3,
        }
    }

    /// A Wi-Fi call or a bridge is in progress (any phase).
    pub fn wire_active(&self) -> bool {
        self.wire.call != Call::None
    }

    /// The platform should open a TCP connection to this host and port
    /// (taken once). Take `take_wire_close` first.
    pub fn take_wire_request(&mut self) -> Option<WireReq> {
        self.wire.req.take()
    }

    /// True once when the core wants the socket closed (ATH, ATZ, S7
    /// timeout, the bridged radio call ended). Drain `wire_output` first.
    pub fn take_wire_close(&mut self) -> bool {
        core::mem::take(&mut self.wire.close)
    }

    /// The requested connection is up.
    pub fn wire_connected(&mut self, now_ms: u32) {
        match self.wire.call {
            Call::Wire(Phase::Dialing { .. }) => {
                self.wire.call = Call::Wire(Phase::Online);
                self.fresh_call();
                self.mode = Mode::Data;
                self.esc = 0;
                self.esc_dead = false;
                self.last_host = now_ms;
                self.result(ResultCode::ConnectWifi);
            }
            Call::Bridge(Phase::Dialing { .. }) => {
                self.wire.call = Call::Bridge(Phase::Online);
                if self.verbose {
                    let mut t = Txt::new();
                    t.put(b"BRIDGE ").put(self.wire.name());
                    self.info(t.bytes());
                }
            }
            // Nothing asked for this connection (any more): close it.
            Call::None => self.wire.close = true,
            // A repeated report for a live connection: nothing to do.
            _ => {}
        }
        self.wire_pump(now_ms);
    }

    /// The connection could not be made (refused, DNS failure, platform
    /// timeout) or broke.
    pub fn wire_failed(&mut self, now_ms: u32) {
        self.wire_gone(now_ms);
    }

    /// The far end closed or reset the connection.
    pub fn wire_closed(&mut self, now_ms: u32) {
        self.wire_gone(now_ms);
    }

    fn wire_gone(&mut self, now: u32) {
        match self.wire.call {
            Call::Wire(Phase::Dialing { .. }) => {
                self.end_wire(true);
                self.mode = Mode::Command;
                self.result(ResultCode::NoCarrier);
            }
            Call::Wire(Phase::Online) => {
                self.wire.call = Call::Wire(Phase::Ending { since: now });
            }
            Call::Bridge(Phase::Dialing { .. }) => self.end_bridge(true, now),
            Call::Bridge(Phase::Online) => {
                self.wire.call = Call::Bridge(Phase::Ending { since: now });
            }
            _ => {}
        }
        self.wire_pump(now);
    }

    /// Bytes received from the socket. Returns the bytes taken; offer the
    /// rest again later. Bytes for no call are taken and dropped.
    pub fn wire_input(&mut self, bytes: &[u8], now_ms: u32) -> usize {
        match self.wire.call {
            Call::Wire(Phase::Online) | Call::Bridge(Phase::Online) => {}
            Call::Wire(Phase::Dialing { .. }) | Call::Bridge(Phase::Dialing { .. }) => return 0,
            _ => return bytes.len(),
        }
        let mut used = 0;
        for &b in bytes {
            if self.wire.rx.free() == 0 || !self.wire.tn.can_take() {
                break;
            }
            if let Some(d) = self.wire.tn.input(b) {
                self.wire.rx.push(&[d]);
            }
            used += 1;
        }
        if used > 0 {
            self.wire.last_rx = Some(now_ms);
        }
        self.wire_pump(now_ms);
        used
    }

    /// Bytes for the socket (telnet replies first). Returns bytes written.
    pub fn wire_output(&mut self, out: &mut [u8]) -> usize {
        match self.wire.call {
            Call::Wire(Phase::Online) | Call::Bridge(Phase::Online) | Call::None => {
                self.wire.output(out)
            }
            _ => 0,
        }
    }

    /// True while host bytes are call data (online, not escaped by `+++`).
    pub fn in_data_mode(&self) -> bool {
        self.mode == Mode::Data
    }

    /// Bytes from the host. Command modes always take every byte; data
    /// mode returns only the bytes the call could queue.
    pub fn host_input<R: Radio>(&mut self, bytes: &[u8], radio: &mut R, now_ms: u32) -> usize {
        // A completed escape whose closing guard has run out is an escape
        // even if no tick ran since: the bytes after it are commands.
        if !bytes.is_empty() {
            self.escape_timer(now_ms);
        }
        let mut used = 0;
        while used < bytes.len() {
            let rest = bytes.get(used..).unwrap_or(&[]);
            if self.mode == Mode::Data {
                used += self.data_input(rest, now_ms);
                break;
            }
            if let Some(&b) = rest.first() {
                self.cmd_byte(b, radio, now_ms);
            }
            used += 1;
        }
        self.after(radio, now_ms);
        used
    }

    pub fn radio_rx<R: Radio>(&mut self, frame: &[u8], info: RxInfo, radio: &mut R, now_ms: u32) {
        self.last_rx = Some(info);
        self.disc_start(now_ms);
        if self.screen(frame, info, now_ms) {
            // A radio caller during a Wi-Fi call gets BUSY.
            self.link
                .set_engaged(matches!(self.wire.call, Call::Wire(_)));
            self.link
                .on_frame(frame, info, name_of(&self.cfg), &self.cfg.sregs, now_ms);
        }
        self.after(radio, now_ms);
    }

    pub fn tick<R: Radio>(&mut self, radio: &mut R, now_ms: u32) {
        self.escape_timer(now_ms);
        self.disc_tick(radio, now_ms);
        self.dir_save_timer(now_ms);
        // A discovery frame queued above goes out in this pump.
        self.link.tick(radio, &self.cfg.sregs, now_ms);
        self.after(radio, now_ms);
    }

    /// No call of any kind: the radio link is idle and no Wi-Fi call or
    /// bridge exists. Discovery only transmits then.
    fn air_idle(&self) -> bool {
        self.link.is_idle() && self.wire.call == Call::None
    }

    /// S20 as the tier margin (dB x4).
    fn margin(&self) -> i16 {
        i16::try_from(self.cfg.sregs.get(20).unwrap_or(20)).unwrap_or(20)
    }

    /// First tick or frame: seed the discovery random numbers from the
    /// name and the time, and start the directory save clock.
    fn disc_start(&mut self, now: u32) {
        if !self.disc.started {
            self.dir_saved_at = now;
            self.disc.start(name_of(&self.cfg), now);
        }
    }

    /// A station was heard: into the directory with the S20 margin, unless
    /// it carries our own name.
    fn dir_heard(&mut self, name: &str, services: Option<u8>, info: RxInfo, now: u32) {
        if name.eq_ignore_ascii_case(name_of(&self.cfg)) {
            return;
        }
        let m = self.margin();
        self.dir.heard_with_margin(name, services, info, now, m);
    }

    /// Look at a received frame before the link does. Discovery frames are
    /// handled here and never reach the link; a CallReq to us feeds the
    /// directory, or in HIDDEN mode is dropped when the caller is not a
    /// pinned entry; the answerer of our dial is credited on its CallAck or Train.
    /// False when the link must not see the frame.
    fn screen(&mut self, buf: &[u8], info: RxInfo, now: u32) -> bool {
        let Ok(f) = crate::frame::decode(buf) else {
            return true;
        };
        let h = f.header;
        match h.ftype {
            FrameType::Beacon => {
                if let Some(b) = discovery::parse_beacon(f.payload) {
                    self.dir_heard(b.name, Some(b.services), info, now);
                }
                false
            }
            FrameType::ScanReq => {
                self.on_scan_req(f.payload, info, now);
                false
            }
            FrameType::ScanResp => {
                self.on_scan_resp(f.payload, info, now);
                false
            }
            FrameType::CallReq => self.screen_call_req(f.payload, h.call_id, info, now),
            FrameType::CallAck | FrameType::Train => {
                let dialing = matches!(self.link.state(), State::Dialing | State::AwaitTrain);
                if dialing && h.call_id != 0 && h.call_id == self.link.call_id() {
                    let mut n = [0u8; NAME_MAX];
                    let len = usize::from(self.dial_len).min(NAME_MAX);
                    if let (Some(d), Some(s)) = (n.get_mut(..len), self.dial_name.get(..len)) {
                        d.copy_from_slice(s);
                    }
                    if let Ok(name) = core::str::from_utf8(n.get(..len).unwrap_or(&[])) {
                        self.dir_heard(name, None, info, now);
                    }
                }
                true
            }
            _ => true,
        }
    }

    fn screen_call_req(&mut self, p: &[u8], call_id: u8, info: RxInfo, now: u32) -> bool {
        let Ok((caller, callee)) = crate::frame::parse_call_req(p) else {
            return true;
        };
        if !callee.eq_ignore_ascii_case(name_of(&self.cfg)) {
            return true;
        }
        if self.set.beacon == BeaconMode::Hidden && !self.dir.get(caller).is_some_and(|e| e.pinned)
        {
            // Not pinned (AT+DIR=+NAME): drop it whole, so nothing is sent.
            return false;
        }
        // The link ignores call id 0; so does the directory.
        if call_id != 0 {
            self.dir_heard(caller, None, info, now);
        }
        true
    }

    fn on_scan_req(&mut self, p: &[u8], info: RxInfo, now: u32) {
        let Some(r) = discovery::parse_scan_req(p) else {
            return;
        };
        if r.name.eq_ignore_ascii_case(name_of(&self.cfg)) {
            return; // our own scan echoed back
        }
        let hidden = self.set.beacon == BeaconMode::Hidden;
        // HIDDEN: a scan only refreshes a name already known (call rights
        // come from pinning alone, this just keeps strangers out of the list).
        if !hidden || self.dir.get(r.name).is_some() {
            self.dir_heard(r.name, None, info, now);
        }
        if hidden || !self.air_idle() || self.disc.answer.is_some() {
            return;
        }
        let slot = self.disc.rand(u32::from(SLOTS - 1)) as u8;
        self.disc.answer = Some(Answer {
            at: now,
            slot,
            snr_x4: info.snr_x4,
            scan_id: r.scan_id,
        });
    }

    fn on_scan_resp(&mut self, p: &[u8], info: RxInfo, now: u32) {
        let Some(r) = discovery::parse_scan_resp(p) else {
            return;
        };
        if r.name.eq_ignore_ascii_case(name_of(&self.cfg)) {
            return;
        }
        // The worse of the two directions decides the tier.
        let snr = info.snr_x4.min(r.snr_x4);
        let both = RxInfo {
            snr_x4: snr,
            ..info
        };
        // HIDDEN: an overheard answer only refreshes a name already known,
        // like a ScanReq.
        if self.set.beacon != BeaconMode::Hidden || self.dir.get(r.name).is_some() {
            self.dir_heard(r.name, Some(r.services), both, now);
        }
        let print = self.verbose && self.mode != Mode::Data;
        let Some(scan) = self.disc.scan.as_mut() else {
            return;
        };
        let live = scan.id == r.scan_id
            && scan
                .sent
                .is_some_and(|t| now.wrapping_sub(t) < SCAN_WINDOW_MS);
        if !live {
            return;
        }
        scan.hits = scan.hits.saturating_add(1);
        if print {
            let tier = best_tier(snr, self.margin());
            let mut t = Txt::new();
            t.put(b"SCAN ").put(r.name.as_bytes()).put(b" SNR ");
            put_snr(&mut t, snr);
            t.put(b" dB ").put(tier.name().as_bytes());
            self.info(t.bytes());
        }
    }

    /// Services byte for our beacon and scan answers.
    fn services(&self) -> u8 {
        let mut s = 0;
        if self.cfg.sregs.get(0).unwrap_or(0) > 0 {
            s |= SVC_ACCEPTS;
        }
        if self.set.published().is_some() {
            s |= SVC_FORWARD;
        }
        if self.set.ap_on {
            s |= SVC_HOTSPOT;
        }
        s
    }

    /// Hand one discovery frame (call id 0) to the idle link.
    fn send_disc(&mut self, ft: FrameType, payload: &[u8]) -> bool {
        let h = Header {
            ftype: ft,
            flags: 0,
            call_id: 0,
            seq: 0,
            ack: 0,
        };
        let mut b = [0u8; HEADER_LEN + DISC_MAX];
        match crate::frame::encode(&h, payload, &mut b) {
            Ok(n) => self.link.send_discovery(b.get(..n).unwrap_or(&[])),
            Err(_) => false,
        }
    }

    /// The discovery scheduler (from `tick`): beacon timer, scan answer
    /// slot with listen before talk, AT+SCAN. At most one discovery frame
    /// per call and per 10 s, only on an idle radio with no call.
    fn disc_tick<R: Radio>(&mut self, radio: &R, now: u32) {
        self.disc_start(now);
        if !self.air_idle() {
            // A call holds all discovery; the beacon timer restarts after it.
            self.disc.in_call = true;
            self.disc.answer = None;
            if self.disc.scan.take().is_some() && self.mode != Mode::Data {
                self.result(ResultCode::Error);
            }
            return;
        }
        if core::mem::take(&mut self.disc.in_call) && self.disc.beacon.is_some() {
            self.disc.arm_beacon(now);
        }
        match self.set.beacon {
            BeaconMode::On => {
                if self.disc.beacon.is_none() {
                    self.disc.arm_beacon(now);
                }
            }
            BeaconMode::Quiet => self.disc.beacon = None,
            BeaconMode::Hidden => {
                self.disc.beacon = None;
                self.disc.answer = None;
                // A scan whose ScanReq has not gone out yet never will.
                if self.disc.scan.is_some_and(|s| s.sent.is_none()) {
                    self.disc.scan = None;
                    self.result(ResultCode::Error);
                }
            }
        }
        // Scan window over, or a ScanReq that never found air.
        if let Some(s) = self.disc.scan {
            match s.sent {
                Some(t) if now.wrapping_sub(t) >= SCAN_WINDOW_MS => {
                    self.disc.scan = None;
                    if !self.verbose {
                        let mut t = Txt::new();
                        t.num(u32::from(s.hits), 1);
                        self.info(t.bytes());
                    }
                    self.result(ResultCode::Ok);
                }
                None if now.wrapping_sub(s.asked) >= SCAN_SEND_LIMIT_MS => {
                    self.disc.scan = None;
                    self.result(ResultCode::Error);
                }
                _ => {}
            }
        }
        // A scan answer whose slot has come.
        if let Some(mut a) = self.disc.answer {
            if now.wrapping_sub(a.at) < u32::from(a.slot) * SLOT_MS {
                return;
            }
            if !self.disc.gap_ok(now) {
                self.disc.answer = None;
                return;
            }
            let mut sent = false;
            if !radio.busy() {
                let mut p = [0u8; DISC_MAX];
                let r = ScanResp {
                    name: name_of(&self.cfg),
                    services: self.services(),
                    snr_x4: a.snr_x4,
                    scan_id: a.scan_id,
                };
                let n = discovery::scan_resp_payload(&r, &mut p).unwrap_or(0);
                sent = n > 0 && self.send_disc(FrameType::ScanResp, p.get(..n).unwrap_or(&[]));
            }
            if sent {
                self.disc.last_tx = Some(now);
                self.disc.answer = None;
            } else if a.slot + 1 < SLOTS {
                // Channel busy: listen before talk, try a later slot.
                a.slot += 1;
                self.disc.answer = Some(a);
            } else {
                self.disc.answer = None;
            }
            return;
        }
        if radio.busy() || !self.disc.gap_ok(now) {
            return;
        }
        // The ScanReq of an AT+SCAN.
        if let Some(mut s) = self.disc.scan.filter(|s| s.sent.is_none()) {
            let mut p = [0u8; DISC_MAX];
            let r = ScanReq {
                name: name_of(&self.cfg),
                scan_id: s.id,
            };
            let n = discovery::scan_req_payload(&r, &mut p).unwrap_or(0);
            if n > 0 && self.send_disc(FrameType::ScanReq, p.get(..n).unwrap_or(&[])) {
                s.sent = Some(now);
                self.disc.scan = Some(s);
                self.disc.last_tx = Some(now);
            }
            return;
        }
        // The beacon, never while a scan collects answers.
        if let Some((from, wait)) = self.disc.beacon {
            if now.wrapping_sub(from) < wait || self.disc.scan.is_some() {
                return;
            }
            let mut p = [0u8; DISC_MAX];
            let heard = self.dir.heard_count(now, HEARD_WINDOW_MS).min(255) as u8;
            let b = Beacon {
                name: name_of(&self.cfg),
                services: self.services(),
                heard,
                time_quality: 0,
            };
            let n = discovery::beacon_payload(&b, &mut p).unwrap_or(0);
            if n > 0 && self.send_disc(FrameType::Beacon, p.get(..n).unwrap_or(&[])) {
                self.disc.last_tx = Some(now);
                self.disc.arm_beacon(now);
            }
        }
    }

    /// Ask for a directory save once it changed and 60 s have passed since
    /// the last one.
    fn dir_save_timer(&mut self, now: u32) {
        if self.dir.take_changed() {
            self.dir_dirty = true;
        }
        if self.dir_dirty && now.wrapping_sub(self.dir_saved_at) >= DIR_SAVE_GAP_MS {
            self.dir_dirty = false;
            self.dir_save = true;
            self.dir_saved_at = now;
        }
    }

    /// Print `AT+DIR?` lines while the host output has room, then OK.
    fn pump_list(&mut self) {
        // Data mode is 8-bit clean: a listing ends when a call takes the host.
        if self.mode == Mode::Data {
            self.dir_list = None;
            return;
        }
        while let Some((next, at)) = self.dir_list {
            let mut i = usize::from(next);
            while i < DIR_MAX && self.dir.slot(i).is_none() {
                i += 1;
            }
            let Some(e) = self.dir.slot(i) else {
                if self.out.free() >= 16 {
                    self.dir_list = None;
                    self.result(ResultCode::Ok);
                }
                return;
            };
            let mut t = Txt::new();
            t.put(b"DIR ").put(e.name().as_bytes()).put(b" ");
            match e.best_tier {
                Some(tier) => t.put(tier.name().as_bytes()),
                None => t.put(b"NONE"),
            };
            t.put(b" ");
            if e.heard_since_boot() {
                // Heard again after the command was typed: age 0, never
                // a wrapped "negative" age.
                let d = at.wrapping_sub(e.heard_ms);
                let d = if d > u32::MAX / 2 { 0 } else { d };
                t.num(d / 1000, 1);
            } else {
                t.put(b"-");
            }
            if e.pinned {
                t.put(b" PIN");
            }
            if self.out.free() < t.n + 4 + OUT_RESERVE {
                return;
            }
            self.info(t.bytes());
            self.dir_list = Some(((i + 1).min(DIR_MAX) as u8, at));
        }
    }

    /// Held escape characters: release dead ones as data; once the closing
    /// guard has passed, three are the escape (OK) and fewer are data.
    fn escape_timer(&mut self, now_ms: u32) {
        if self.mode != Mode::Data || self.esc == 0 {
            return;
        }
        if self.esc_dead {
            self.flush_esc(now_ms);
        } else if now_ms.wrapping_sub(self.esc_last) >= self.guard() {
            if self.esc == 3 {
                self.esc = 0;
                self.esc_dead = false;
                self.mode = Mode::OnlineCommand;
                self.result(ResultCode::Ok);
                if let Some(t) = self.retrain_note.take() {
                    self.result(ResultCode::Retrain(t));
                }
            } else {
                self.flush_esc(now_ms);
            }
        }
    }

    pub fn host_output(&mut self, out: &mut [u8]) -> usize {
        let n = self.out.pop(out);
        // A long AT+DIR? listing continues as the host takes the text.
        if self.dir_list.is_some() {
            self.pump_list();
        }
        n
    }

    /// S30: SX1262 chip output power in dBm (-9 to 22).
    pub fn tx_power_dbm(&self) -> i8 {
        let v = self.cfg.sregs.get(REG_TX_POWER).unwrap_or(2) as i16;
        v.clamp(-9, 22) as i8
    }

    pub fn lights(&self, now_ms: u32) -> Lights {
        let recent = |t: Option<u32>| t.is_some_and(|t| now_ms.wrapping_sub(t) < LIGHT_MS);
        if let Call::Wire(ph) = self.wire.call {
            // Wi-Fi call: OH and CD as on radio, SD and RD on wire
            // traffic, no radio tier (HS and EC off).
            let up = !matches!(ph, Phase::Dialing { .. });
            return Lights {
                oh: true,
                cd: up,
                sd: up && recent(self.wire.last_tx),
                rd: up && recent(self.wire.last_rx),
                hs: false,
                ec: false,
                dc: false,
                aa: self.cfg.sregs.get(0).unwrap_or(0) > 0,
                tier: None,
            };
        }
        let cd = self.link.is_connected();
        let tier = if cd { self.link.tier() } else { None };
        Lights {
            oh: !self.link.is_idle(),
            cd,
            sd: cd && recent(self.link.last_data_tx()),
            rd: cd && recent(self.link.last_data_rx()),
            hs: tier.is_some_and(|t| t >= Tier::Sf7),
            ec: cd,
            dc: cd && self.link.lz(),
            aa: self.cfg.sregs.get(0).unwrap_or(0) > 0,
            tier,
        }
    }

    fn guard(&self) -> u32 {
        u32::from(self.cfg.sregs.get(12).unwrap_or(50)) * 20
    }

    fn esc_char(&self) -> u8 {
        self.cfg.sregs.get(2).unwrap_or(43).min(127) as u8
    }

    /// Push held escape characters as data. True when none are left.
    fn flush_esc(&mut self, now: u32) -> bool {
        let done = self.flush_held(now);
        if done {
            self.esc_dead = false;
        }
        done
    }

    fn flush_held(&mut self, now: u32) -> bool {
        let c = self.esc_char();
        let held = [c; 3];
        let n = self.call_push(held.get(..self.esc as usize).unwrap_or(&[]), now);
        self.esc = self.esc.saturating_sub(n.min(3) as u8);
        self.esc == 0
    }

    /// Escape rule: S12 guard of silence, three S2 characters each less than
    /// S12 apart, then S12 of silence (checked by `escape_timer`, from tick
    /// and at the start of the next host input). Otherwise data.
    ///
    /// Any non-empty offer counts as host activity for the guard, even if
    /// the call refuses the bytes (send window full). Held S2 characters
    /// followed by any further offered byte are released as data.
    fn data_input(&mut self, bytes: &[u8], now: u32) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        let guard = self.guard();
        let c = self.esc_char();
        let quiet_before = now.wrapping_sub(self.last_host) >= guard;
        self.last_host = now;
        let mut used = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if self.esc > 0
                && (self.esc_dead
                    || b != c
                    || self.esc >= 3
                    || now.wrapping_sub(self.esc_last) >= guard)
            {
                self.esc_dead = true;
                if !self.flush_esc(now) {
                    break;
                }
            }
            let start = self.esc == 0 && i == 0 && quiet_before;
            if b == c && (start || self.esc > 0) {
                self.esc += 1;
                self.esc_last = now;
            } else if self.call_push(&[b], now) == 0 {
                break;
            }
            used += 1;
        }
        used
    }

    fn cmd_byte<R: Radio>(&mut self, b: u8, radio: &mut R, now: u32) {
        if self.echo {
            self.out.push(&[b]);
        }
        match b {
            b'\r' => {
                self.run_line(radio, now);
                self.line_len = 0;
                self.line_over = false;
            }
            b'\n' => {}
            8 | 127 => self.line_len = self.line_len.saturating_sub(1),
            _ => match self.line.get_mut(self.line_len) {
                Some(d) => {
                    *d = b;
                    self.line_len += 1;
                }
                None => self.line_over = true,
            },
        }
    }

    fn run_line<R: Radio>(&mut self, radio: &mut R, now: u32) {
        if self.line_len == 0 {
            return;
        }
        let mut copy = [0u8; LINE_CAP];
        let n = self.line_len.min(LINE_CAP);
        if let (Some(d), Some(s)) = (copy.get_mut(..n), self.line.get(..n)) {
            d.copy_from_slice(s);
        }
        let parsed = core::str::from_utf8(copy.get(..n).unwrap_or(&[]))
            .ok()
            .map(parse_line);
        match parsed {
            Some(Ok(cmd)) if !self.line_over => self.exec(cmd, radio, now),
            _ => self.result(ResultCode::Error),
        }
        self.sync_set();
    }

    fn exec<R: Radio>(&mut self, cmd: AtCmd<'_>, radio: &mut R, now: u32) {
        let _ = radio;
        match cmd {
            AtCmd::Attention => self.result(ResultCode::Ok),
            AtCmd::Dial(name) => {
                self.fresh_call();
                let fwd = self.set.forward(name).copied();
                let hp = if fwd.is_none() {
                    parse_hostport(name)
                } else {
                    None
                };
                if self.mode == Mode::Command && (fwd.is_some() || hp.is_some()) {
                    // Wi-Fi dial: one call at a time.
                    // A close still pending: the old socket is not released yet.
                    let free = self.link.is_idle() && !self.wire_active() && !self.wire.close;
                    let ok = free
                        && match (&fwd, hp) {
                            (Some(f), _) => self.start_wire(f.host(), f.port(), f.mode(), b"", now),
                            (None, Some((h, p))) => {
                                self.start_wire(h, p, WireMode::default_for(p), b"", now)
                            }
                            _ => false,
                        };
                    if !ok {
                        self.result(ResultCode::Error);
                    } else if self.verbose {
                        self.info(Txt::new().put(b"DIALING ").put(name.as_bytes()).bytes());
                    }
                } else if self.mode != Mode::Command
                    || name.contains(':')
                    || self.wire_active()
                    || !self
                        .link
                        .dial(name_of(&self.cfg), name, &self.cfg.sregs, now)
                {
                    self.result(ResultCode::Error);
                } else {
                    // Remember who we dialed: its CallAck feeds the directory.
                    self.dial_len = 0;
                    for (d, &c) in self.dial_name.iter_mut().zip(name.as_bytes()) {
                        *d = c;
                        self.dial_len += 1;
                    }
                    if self.verbose {
                        self.info(Txt::new().put(b"DIALING ").put(name.as_bytes()).bytes());
                    }
                }
            }
            AtCmd::Answer => {
                // Already in a call (online command mode): nothing to
                // answer, and the call stays up.
                if self.mode != Mode::Command || self.link.is_connected() || self.wire_active() {
                    self.result(ResultCode::Error);
                    return;
                }
                self.fresh_call();
                if !self.link.answer(&self.cfg.sregs, now) {
                    self.result(ResultCode::NoCarrier);
                }
            }
            AtCmd::Hangup => {
                if self.wire_active() {
                    // Like a radio call: NO CARRIER, also while dialing.
                    self.link.hangup(now);
                    self.end_wire(false);
                    self.mode = Mode::Command;
                    self.result(ResultCode::NoCarrier);
                } else if self.link.hangup(now) {
                    self.mode = Mode::Command;
                    self.result(ResultCode::NoCarrier);
                } else {
                    self.result(ResultCode::Ok);
                }
            }
            AtCmd::Online => match (self.wire.call, self.link.tier()) {
                (Call::Wire(Phase::Online), _) => {
                    self.mode = Mode::Data;
                    self.esc = 0;
                    self.esc_dead = false;
                    self.last_host = now;
                    self.result(ResultCode::ConnectWifi);
                }
                // A bridge keeps the host out of the call; a Wi-Fi dial
                // still in progress has nothing to return to.
                (Call::Wire(_) | Call::Bridge(_), _) => self.result(ResultCode::Error),
                (Call::None, Some(t)) if self.link.is_connected() => {
                    self.mode = Mode::Data;
                    self.esc = 0;
                    self.esc_dead = false;
                    self.last_host = now;
                    self.result(self.connect_code(t));
                }
                _ => self.result(ResultCode::NoCarrier),
            },
            AtCmd::Info => {
                let mut t = Txt::new();
                t.put(BANNER.as_bytes())
                    .put(b"\r\nv")
                    .put(crate::VERSION.as_bytes());
                t.put(b" ")
                    .put(RELEASE_NAME.as_bytes())
                    .put(b"\r\n")
                    .put(RELEASE_TAGLINE.as_bytes());
                if self.note_len > 0 {
                    t.put(b"\r\nLAST CRASH: ")
                        .put(self.note.get(..self.note_len).unwrap_or(&[]));
                }
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::FrameBuffer => self.fb_request = true,
            AtCmd::Verbose(v) => {
                self.verbose = v;
                self.result(ResultCode::Ok);
            }
            AtCmd::Echo(e) => {
                self.echo = e;
                self.result(ResultCode::Ok);
            }
            AtCmd::SetReg(n, v) => {
                let ok = self.cfg.sregs.set(n, v);
                self.result(if ok {
                    ResultCode::Ok
                } else {
                    ResultCode::Error
                });
            }
            AtCmd::GetReg(n) => match self.cfg.sregs.get(n) {
                Some(v) => {
                    let mut t = Txt::new();
                    t.sreg_val(n, v);
                    self.info(t.bytes());
                    self.result(ResultCode::Ok);
                }
                None => self.result(ResultCode::Error),
            },
            AtCmd::ShowConfig => {
                let mut t = Txt::new();
                t.put(b"E")
                    .num(u32::from(self.echo), 1)
                    .put(b" V")
                    .num(u32::from(self.verbose), 1);
                t.put(b" NAME ")
                    .put(name_of(&self.cfg).as_bytes())
                    .put(b"\r\n");
                for (i, r) in SRegs::NUMBERS.iter().enumerate() {
                    if i > 0 {
                        t.put(b" ");
                    }
                    let v = self.cfg.sregs.get(*r).unwrap_or(0);
                    t.put(b"S").num(u32::from(*r), 2).put(b"=");
                    t.sreg_val(*r, v);
                }
                self.info(t.bytes());
                let dbm = self.tx_power_dbm();
                let mut t = Txt::new();
                t.put(b"TX POWER S30=").int(i32::from(dbm));
                t.put(b" dBm chip, about ")
                    .int(i32::from(dbm) + GC1109_GAIN_DB)
                    .put(b" dBm out");
                self.info(t.bytes());
                if let Some(ip) = self.simnet {
                    let mut t = Txt::new();
                    t.put(b"RADIO SIMNET ");
                    put_ip(&mut t, ip);
                    if self.simnet_fail {
                        t.put(b" NET FAIL");
                    }
                    self.info(t.bytes());
                } else if let Some((code, errs)) = self.radio_diag {
                    let mut t = Txt::new();
                    if code == 0 {
                        t.put(b"RADIO OK");
                    } else {
                        t.put(b"RADIO FAIL ").int(i32::from(code));
                        t.put(b" ERR 0x").hex16(errs);
                    }
                    self.info(t.bytes());
                }
                let mut t = Txt::new();
                t.put(b"WIFI AP ").put(onoff(self.set.ap_on));
                t.put(b" ").put(self.set.ap_ssid().as_bytes());
                t.put(b"  STA ");
                match self.set.sta_ssid() {
                    "" => t.put(b"OFF"),
                    ssid => t.put(ssid.as_bytes()),
                };
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"PWSHOW ")
                    .put(if self.set.show_ap_pass { b"1" } else { b"0" });
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"PUB ");
                match self.set.published() {
                    Some(f) => t.put(f.name().as_bytes()),
                    None => t.put(b"NONE"),
                };
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"FWD ").num(self.set.fwd_count() as u32, 1);
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"BEACON ").put(self.set.beacon.name().as_bytes());
                self.info(t.bytes());
                let mut t = Txt::new();
                t.put(b"DIR ").num(self.dir.len() as u32, 1);
                self.info(t.bytes());
                if let Some(n) = self.stack_free {
                    let mut t = Txt::new();
                    t.put(b"STACK FREE ").num(n, 1);
                    self.info(t.bytes());
                }
                if let Some(n) = self.heap_min {
                    let mut t = Txt::new();
                    t.put(b"HEAP MIN ").num(n, 1);
                    self.info(t.bytes());
                }
                let mut t = Txt::new();
                t.put(b"TIER ");
                match self.link.tier().filter(|_| self.link.is_connected()) {
                    Some(c) => put_tier(&mut t, c),
                    None => {
                        t.put(b"NONE");
                    }
                }
                t.put(b"\r\nRETRAINS");
                let mut any = false;
                for r in self.retrains.iter().flatten() {
                    t.put(if any { b", " } else { b" " });
                    put_tier(&mut t, *r);
                    any = true;
                }
                if !any {
                    t.put(b" NONE");
                }
                self.info(t.bytes());
                // Compression on the current or last radio call: host bytes
                // over air bytes of the Data payloads, both directions.
                let r = self.link.lz_ratio_x10();
                let mut t = Txt::new();
                t.put(b"LZ ").put(onoff(self.link.lz())).put(b" RATIO ");
                t.num(r / 10, 1).put(b".").num(r % 10, 1);
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::Reset => {
                self.fresh_call();
                self.link.hangup(now);
                if self.wire_active() {
                    self.end_wire(false);
                }
                self.mode = Mode::Command;
                // Back to the saved profile (power-on defaults if none was
                // loaded). A password that was hidden stays hidden.
                self.cfg.sregs = self.saved.sregs;
                self.echo = self.saved.echo;
                self.verbose = self.saved.verbose;
                let shown = self.set.show_ap_pass && self.saved.show_ap_pass;
                self.set.clone_from(&self.saved);
                self.set.show_ap_pass = shown;
                self.wifi_changed = true;
                // A scan or a listing in progress ends silently.
                self.disc.scan = None;
                self.dir_list = None;
                self.result(ResultCode::Ok);
            }
            AtCmd::Save => {
                // The directory is saved with the settings.
                self.dir.take_changed();
                self.dir_dirty = false;
                self.dir_save = true;
                self.dir_saved_at = now;
                if !self.set.sta_ssid().is_empty() {
                    self.set.show_ap_pass = false;
                }
                self.saved.clone_from(&self.set);
                self.saved.sregs = self.cfg.sregs;
                self.saved.echo = self.echo;
                self.saved.verbose = self.verbose;
                self.save_req = true;
                self.result(ResultCode::Ok);
            }
            AtCmd::WapQuery => {
                let mut t = Txt::new();
                t.put(b"WAP ").put(onoff(self.set.ap_on));
                t.put(b" ").put(self.set.ap_ssid().as_bytes());
                t.put(b" ").put(self.set.ap_pass().as_bytes());
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::WapPower(on) => {
                self.set.ap_on = on;
                self.wifi_changed = true;
                self.result(ResultCode::Ok);
            }
            AtCmd::WapSet { ssid, pass } => {
                let ok = self.set.set_ap(ssid, pass);
                if ok {
                    // A password the user chose is not shown: only the
                    // generated first-boot one is.
                    self.set.show_ap_pass = false;
                }
                self.wifi_changed |= ok;
                self.ok_or_error(ok);
            }
            AtCmd::WstaQuery => {
                let mut t = Txt::new();
                match self.set.sta_ssid() {
                    "" => t.put(b"WSTA OFF"),
                    ssid => t.put(b"WSTA ").put(ssid.as_bytes()).put(b" ********"),
                };
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::WstaForget => {
                self.set.clear_sta();
                self.wifi_changed = true;
                self.result(ResultCode::Ok);
            }
            AtCmd::WstaSet { ssid, pass } => {
                let ok = self.set.set_sta(ssid, pass);
                self.wifi_changed |= ok;
                self.ok_or_error(ok);
            }
            AtCmd::WifiQuery => {
                if self.wifi_len == 0 {
                    self.info(b"WIFI OFF");
                } else {
                    let mut t = Txt::new();
                    t.put(self.wifi.get(..self.wifi_len).unwrap_or(&[]));
                    self.info(t.bytes());
                }
                self.result(ResultCode::Ok);
            }
            AtCmd::FwdList => {
                for i in 0..MAX_FWD {
                    let mut t = Txt::new();
                    if let Some(Some(f)) = self.set.fwd.get(i) {
                        t.put(b"FWD ").put(f.name().as_bytes()).put(b" ");
                        t.put(f.host().as_bytes()).put(b":");
                        t.num(u32::from(f.port()), 1);
                        t.put(if f.mode() == WireMode::Telnet {
                            b" T"
                        } else {
                            b" R"
                        });
                    }
                    if t.n > 0 {
                        self.info(t.bytes());
                    }
                }
                self.result(ResultCode::Ok);
            }
            AtCmd::FwdDelete(name) => {
                let ok = self.set.remove_forward(name);
                self.ok_or_error(ok);
            }
            AtCmd::FwdSet {
                name,
                host,
                port,
                mode,
            } => {
                let ok =
                    Forward::new(name, host, port, mode).is_some_and(|f| self.set.set_forward(f));
                self.ok_or_error(ok);
            }
            AtCmd::PubQuery => {
                let mut t = Txt::new();
                t.put(b"PUB ");
                match self.set.published() {
                    Some(f) => t.put(f.name().as_bytes()),
                    None => t.put(b"NONE"),
                };
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::PubNone => {
                self.set.publish = None;
                self.result(ResultCode::Ok);
            }
            AtCmd::Pub(name) => {
                let ok = self.set.set_publish(name);
                self.ok_or_error(ok);
            }
            AtCmd::SimNetQuery => {
                let mut t = Txt::new();
                t.put(b"SIMNET ");
                match self.simnet {
                    Some(ip) => put_ip(&mut t, ip),
                    None => {
                        t.put(b"OFF");
                    }
                }
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::SimNet(ip) => {
                // Only with no call at all: the radio is swapped under the link.
                let idle = self.mode == Mode::Command
                    && self.link.is_idle()
                    && !self.wire_active()
                    && !self.wire.close;
                if idle {
                    self.simnet = ip;
                    self.simnet_req = Some(ip);
                    self.simnet_fail = false;
                }
                self.ok_or_error(idle);
            }
            AtCmd::BeaconQuery => {
                let mut t = Txt::new();
                t.put(b"BEACON ").put(self.set.beacon.name().as_bytes());
                self.info(t.bytes());
                self.result(ResultCode::Ok);
            }
            AtCmd::Beacon(m) => {
                self.set.beacon = m;
                self.result(ResultCode::Ok);
            }
            AtCmd::Scan => {
                // Only with no call at all, never in HIDDEN (a ScanReq
                // carries our name), one scan at a time.
                let ok = self.mode == Mode::Command
                    && self.air_idle()
                    && self.set.beacon != BeaconMode::Hidden
                    && self.disc.scan.is_none();
                if ok {
                    self.disc_start(now);
                    let id = self.disc.rand(255) as u8;
                    self.disc.scan = Some(Scan {
                        id,
                        asked: now,
                        sent: None,
                        hits: 0,
                    });
                } else {
                    self.result(ResultCode::Error);
                }
            }
            AtCmd::DirList => self.dir_list = Some((0, now)),
            AtCmd::DirPin(name) => {
                let ok =
                    !name.eq_ignore_ascii_case(name_of(&self.cfg)) && self.dir.add_pinned(name);
                self.ok_or_error(ok);
            }
            AtCmd::DirRemove(name) => {
                let ok = self.dir.remove(name);
                self.ok_or_error(ok);
            }
            AtCmd::DirClear => {
                self.dir.clear_unpinned();
                self.result(ResultCode::Ok);
            }
        }
    }

    fn ok_or_error(&mut self, ok: bool) {
        self.result(if ok {
            ResultCode::Ok
        } else {
            ResultCode::Error
        });
    }

    fn result(&mut self, code: ResultCode) {
        let mut b = [0u8; 64];
        let n = format_result(&code, self.verbose, &mut b);
        self.out.push(b.get(..n).unwrap_or(&[]));
    }

    /// The radio CONNECT for tier `t`: `/ARQ/LZ` when the call compresses.
    fn connect_code(&self, t: Tier) -> ResultCode {
        if self.link.lz() {
            ResultCode::ConnectLz(t)
        } else {
            ResultCode::Connect(t)
        }
    }

    /// `\r\n<text>\r\n`.
    fn info(&mut self, text: &[u8]) {
        let mut t = Txt::new();
        t.put(b"\r\n").put(text).put(b"\r\n");
        self.out.push(t.bytes());
    }

    /// Drain link events, move received data to the host, send if possible.
    fn after<R: Radio>(&mut self, radio: &mut R, now: u32) {
        while let Some(e) = self.link.poll_event() {
            match e {
                Event::Ring => self.result(ResultCode::Ring),
                Event::Ringing => {
                    if self.verbose {
                        self.info(b"RINGING");
                    }
                }
                Event::Training(snr) => {
                    if self.verbose {
                        let mut t = Txt::new();
                        t.put(b"TRAINING SNR ");
                        put_snr(&mut t, snr);
                        t.put(b" dB");
                        self.info(t.bytes());
                    }
                }
                // The link connects once per call; never restart a bridge.
                Event::Connect(_) if matches!(self.wire.call, Call::Bridge(_)) => {}
                Event::Connect(t) => {
                    self.fresh_call();
                    // Access policy: an answered call goes to the published
                    // forward only, never anywhere else.
                    let publ = if self.link.role() == Role::Answerer && !self.wire_active() {
                        self.set.published().copied()
                    } else {
                        None
                    };
                    let bridged = match publ {
                        Some(f) => {
                            self.start_wire(f.host(), f.port(), f.mode(), f.name().as_bytes(), now)
                        }
                        None => false,
                    };
                    if !bridged {
                        self.mode = Mode::Data;
                        self.esc = 0;
                        self.esc_dead = false;
                        self.last_host = now;
                        self.result(self.connect_code(t));
                    }
                }
                Event::Retrain(t) => {
                    self.retrains.rotate_left(1);
                    if let Some(last) = self.retrains.last_mut() {
                        *last = Some(t);
                    }
                    if self.mode == Mode::Data {
                        self.retrain_note = Some(t);
                    } else {
                        self.result(ResultCode::Retrain(t));
                    }
                }
                Event::NoCarrier if matches!(self.wire.call, Call::Bridge(_)) => {
                    // The radio caller is gone: close the wire.
                    self.end_wire(false);
                    self.mode = Mode::Command;
                    self.retrain_note = None;
                    self.result(ResultCode::NoCarrier);
                }
                Event::NoCarrier => {
                    // Deliver what fits, then NO CARRIER at once (the
                    // reserve keeps room for it), so it never outlives the call.
                    self.closing = true;
                    self.drain();
                    self.mode = Mode::Command;
                    self.retrain_note = None;
                }
                Event::Busy => self.result(ResultCode::Busy),
                Event::NoAnswer => self.result(ResultCode::NoAnswer),
            }
        }
        if self.mode == Mode::Data && self.wire.call == Call::None {
            self.drain();
        }
        self.wire_pump(now);
        self.link.pump(radio, now);
        if self.dir_list.is_some() {
            self.pump_list();
        }
    }

    /// Host data in data mode: into the Wi-Fi call or the radio link.
    /// Returns bytes taken.
    fn call_push(&mut self, data: &[u8], now: u32) -> usize {
        match self.wire.call {
            Call::Wire(Phase::Online) => {
                let n = self.wire.tx.push(data);
                if n > 0 {
                    self.wire.last_tx = Some(now);
                }
                n
            }
            // The far end has closed: what the host still types is dropped.
            Call::Wire(_) => data.len(),
            _ => self.link.push_data(data),
        }
    }

    /// Start a Wi-Fi call (`name` empty) or a bridge to forward `name`.
    fn start_wire(&mut self, host: &str, port: u16, mode: WireMode, name: &[u8], now: u32) -> bool {
        let Some(req) = WireReq::new(host, port) else {
            return false;
        };
        let w = &mut self.wire;
        w.req = Some(req);
        w.rx.clear();
        w.tx.clear();
        w.pend = None;
        w.tn.reset(mode);
        w.last_tx = None;
        w.last_rx = None;
        w.cr_flush = false;
        w.name_len = 0;
        for (d, &c) in w.name.iter_mut().zip(name.iter()) {
            *d = c;
            w.name_len += 1;
        }
        let ph = Phase::Dialing { since: now };
        w.call = if name.is_empty() {
            Call::Wire(ph)
        } else {
            Call::Bridge(ph)
        };
        true
    }

    /// Forget the wire call. `gone`: the platform reported the socket
    /// failed or closed. Otherwise a close is asked for whenever a socket
    /// may exist (request taken while dialing, or online).
    fn end_wire(&mut self, gone: bool) {
        let taken = self.wire.req.take().is_none();
        let open = !gone
            && match self.wire.call {
                Call::Wire(Phase::Dialing { .. }) | Call::Bridge(Phase::Dialing { .. }) => taken,
                Call::Wire(Phase::Online) | Call::Bridge(Phase::Online) => true,
                _ => false,
            };
        self.wire.close |= open;
        self.wire.call = Call::None;
        self.wire.rx.clear();
        if !open {
            // No socket to drain into.
            self.wire.tx.clear();
            self.wire.pend = None;
        }
    }

    /// End a bridge: hang up the radio call (the caller sees NO CARRIER
    /// through the link's Hangup) and tell the host.
    fn end_bridge(&mut self, gone: bool, now: u32) {
        self.link.hangup(now);
        self.end_wire(gone);
        self.mode = Mode::Command;
        self.retrain_note = None;
        self.result(ResultCode::NoCarrier);
    }

    fn s7_ms(&self) -> u32 {
        u32::from(self.cfg.sregs.get(7).unwrap_or(30)).saturating_mul(1000)
    }

    /// Move wire data along and run the wire timers (S7 for a dial or a
    /// bridge waiting for its connection, and for the drain after a close).
    fn wire_pump(&mut self, now: u32) {
        let s7 = self.s7_ms();
        match self.wire.call {
            Call::None => {}
            Call::Wire(Phase::Dialing { since }) => {
                if now.wrapping_sub(since) >= s7 {
                    self.end_wire(false);
                    self.mode = Mode::Command;
                    self.result(ResultCode::NoCarrier);
                }
            }
            Call::Wire(Phase::Online) => {
                if self.mode == Mode::Data {
                    self.drain_wire();
                }
                self.wire.cr_timer(now);
            }
            Call::Wire(Phase::Ending { since }) => {
                // In data mode everything the far end sent is shown first;
                // in online command mode only what fits, as on radio.
                self.drain_wire();
                if self.mode != Mode::Data
                    || self.wire.rx.is_empty()
                    || now.wrapping_sub(since) >= s7
                {
                    self.end_wire(true);
                    self.mode = Mode::Command;
                    self.result(ResultCode::NoCarrier);
                }
            }
            Call::Bridge(Phase::Dialing { since }) => {
                self.link_to_wire(now);
                if now.wrapping_sub(since) >= s7 {
                    self.end_bridge(false, now);
                }
            }
            Call::Bridge(Phase::Online) => {
                self.link_to_wire(now);
                self.wire_to_link();
                self.wire.cr_timer(now);
            }
            Call::Bridge(Phase::Ending { since }) => {
                // Hand the caller what the far end sent before it closed.
                self.wire_to_link();
                let done = self.wire.rx.is_empty() && self.link.tx_idle();
                if done || !self.link.is_connected() || now.wrapping_sub(since) >= s7 {
                    self.end_bridge(true, now);
                }
            }
        }
    }

    /// Wire bytes to the host, keeping OUT_RESERVE free.
    fn drain_wire(&mut self) {
        let mut tmp = [0u8; 256];
        loop {
            let room = self.out.free().saturating_sub(OUT_RESERVE).min(tmp.len());
            let n = if room == 0 {
                0
            } else {
                self.wire.rx.pop(tmp.get_mut(..room).unwrap_or(&mut []))
            };
            if n == 0 {
                break;
            }
            self.out.push(tmp.get(..n).unwrap_or(&[]));
        }
    }

    /// Bridge: radio caller's bytes into the wire ring, as far as it has
    /// room (the rest waits in the link, whose window then holds the caller).
    fn link_to_wire(&mut self, now: u32) {
        let mut tmp = [0u8; 256];
        loop {
            let room = self.wire.tx.free().min(tmp.len());
            let n = if room == 0 {
                0
            } else {
                self.link.read_data(tmp.get_mut(..room).unwrap_or(&mut []))
            };
            if n == 0 {
                break;
            }
            self.wire.tx.push(tmp.get(..n).unwrap_or(&[]));
            self.wire.last_tx = Some(now);
        }
    }

    /// Bridge: wire bytes into the radio link, as many as it takes.
    fn wire_to_link(&mut self) {
        let mut tmp = [0u8; 256];
        loop {
            let n = self.wire.rx.peek(&mut tmp);
            if n == 0 {
                break;
            }
            let k = self.link.push_data(tmp.get(..n).unwrap_or(&[]));
            self.wire.rx.skip(k);
            if k < n {
                break;
            }
        }
    }

    /// Move received call data to the host, keeping OUT_RESERVE free. When
    /// `closing`, print NO CARRIER after the data (or as soon as the ring is
    /// full) and clear `closing` before returning.
    fn drain(&mut self) {
        let mut tmp = [0u8; 256];
        loop {
            let room = self.out.free().saturating_sub(OUT_RESERVE).min(tmp.len());
            let n = if room == 0 {
                0
            } else {
                self.link.read_data(tmp.get_mut(..room).unwrap_or(&mut []))
            };
            if n == 0 {
                break;
            }
            self.out.push(tmp.get(..n).unwrap_or(&[]));
        }
        if self.closing {
            self.closing = false;
            self.result(ResultCode::NoCarrier);
        }
    }

    /// A new call or a reset: nothing from the old call may follow.
    fn fresh_call(&mut self) {
        self.closing = false;
        self.retrain_note = None;
        // A partial command line typed before the call (during RING or
        // while dialing) must not be glued onto the first command after it.
        self.line_len = 0;
        self.line_over = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A radio that never hears anything and finishes every send at once.
    struct NullRadio;

    impl Radio for NullRadio {
        fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
        fn listen(&mut self, _tier: Tier) {}
        fn busy(&self) -> bool {
            false
        }
    }

    fn output(m: &mut Modem) -> String {
        let mut all = Vec::new();
        let mut buf = [0u8; 100];
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(all).unwrap()
    }

    #[test]
    fn ati_banner() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        let mut r = NullRadio;
        assert_eq!(m.host_input(b"ATI\r", &mut r, 0), 4);
        assert_eq!(
            output(&mut m),
            "ATI\r\
             \r\nLORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM\
             \r\nv0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M\
             \r\nS0N1C B00M R4D10 PR0 MAX\r\n\
             \r\nOK\r\n"
        );
        // Lowercase, echo off: the same three lines, then OK.
        m.host_input(b"ATE0\r", &mut r, 10);
        output(&mut m);
        m.host_input(b"ati\r", &mut r, 20);
        let text = output(&mut m);
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines,
            [
                "LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM",
                "v0.1.0 r.420 BL4Z3B4ND ULTR4M0D3M",
                "S0N1C B00M R4D10 PR0 MAX",
                "OK",
            ]
        );
        assert!(!text.contains('\u{2014}') && !text.contains("--"));
    }

    #[test]
    fn config_name_is_checked() {
        // Valid: 1 to 16 printable ASCII characters, kept as given.
        for name in ["A", "AXIS", "CALLER", "sixteen chars 16", " ~!", "a b"] {
            let c = Config::new(name).expect(name);
            assert_eq!(name_of(&c), name);
            assert_eq!(c.sregs, SRegs::new());
        }
        // Invalid: empty, too long, control characters, non-ASCII. No
        // truncation, no panic.
        for name in [
            "",
            "seventeen chars 17",
            "AXIS\r",
            "TAB\tNAME",
            "\u{7f}",
            "caf\u{e9}",
            "\u{1F4FB}",
        ] {
            assert!(Config::new(name).is_none(), "{name:?} accepted");
        }
    }

    fn run(m: &mut Modem, line: &[u8], now: u32) -> String {
        let mut r = NullRadio;
        assert_eq!(m.host_input(line, &mut r, now), line.len());
        output(m)
    }

    #[test]
    fn at_fb_sets_request_without_ok() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        assert!(!m.take_fb_request());
        // Echo only: no OK, no ERROR, no other text.
        assert_eq!(run(&mut m, b"AT+FB\r", 0), "AT+FB\r");
        assert!(m.take_fb_request());
        assert!(!m.take_fb_request(), "the flag is taken once");
        // Lowercase works too, and with echo off nothing at all is printed.
        run(&mut m, b"ATE0\r", 10);
        assert_eq!(run(&mut m, b"at+fb\r", 20), "");
        assert!(m.take_fb_request());
        // A normal command afterwards is unaffected.
        assert_eq!(run(&mut m, b"AT\r", 30), "\r\nOK\r\n");
        assert!(!m.take_fb_request());
    }

    #[test]
    fn ati_shows_last_crash() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        // No note: three lines only.
        assert!(!run(&mut m, b"ATI\r", 0).contains("LAST CRASH"));
        m.set_last_panic(b"core/src/link/mod.rs:412");
        let t = run(&mut m, b"ATI\r", 10);
        assert!(
            t.contains("\r\nLAST CRASH: core/src/link/mod.rs:412\r\n"),
            "{t:?}"
        );
        assert!(t.ends_with("\r\nOK\r\n"));
        // Non-printable bytes become '?', and an overlong note is cut.
        m.set_last_panic(&[b'a', 0x00, b'b', 0x1b, 0xff, b'c']);
        assert!(run(&mut m, b"ATI\r", 20).contains("LAST CRASH: a?b??c\r\n"));
        let long = [b'x'; 200];
        m.set_last_panic(&long);
        let t = run(&mut m, b"ATI\r", 30);
        let line = t
            .split("\r\n")
            .find(|l| l.starts_with("LAST CRASH: "))
            .unwrap();
        assert_eq!(line.len(), "LAST CRASH: ".len() + NOTE_MAX);
        // An empty note clears it again.
        m.set_last_panic(b"");
        assert!(!run(&mut m, b"ATI\r", 40).contains("LAST CRASH"));
    }

    #[test]
    fn s30_default_and_range() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        assert_eq!(m.tx_power_dbm(), 2);
        let t = run(&mut m, b"AT&V\r", 0);
        assert!(t.contains("S30=002"), "{t:?}");
        assert!(
            t.contains("TX POWER S30=2 dBm chip, about 14 dBm out"),
            "{t:?}"
        );
        assert!(run(&mut m, b"ATS30=22\r", 10).ends_with("\r\nOK\r\n"));
        assert_eq!(m.tx_power_dbm(), 22);
        assert!(run(&mut m, b"ATS30=-9\r", 20).ends_with("\r\nOK\r\n"));
        assert_eq!(m.tx_power_dbm(), -9);
        let t = run(&mut m, b"AT&V\r", 30);
        assert!(t.contains("S30=-009"), "{t:?}");
        assert!(
            t.contains("TX POWER S30=-9 dBm chip, about 3 dBm out"),
            "{t:?}"
        );
        assert!(run(&mut m, b"ATS30?\r", 40).contains("-009"));
        // ATZ puts it back to 2.
        run(&mut m, b"ATZ\r", 50);
        assert_eq!(m.tx_power_dbm(), 2);
    }

    #[test]
    fn ats30_out_of_range_is_error() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        run(&mut m, b"ATS30=7\r", 0);
        for cmd in ["ATS30=23\r", "ATS30=-10\r", "ATS30=65527\r"] {
            let t = run(&mut m, cmd.as_bytes(), 10);
            assert!(t.ends_with("\r\nERROR\r\n"), "{cmd}: {t:?}");
            assert_eq!(m.tx_power_dbm(), 7, "{cmd} must not change S30");
        }
    }

    #[test]
    fn s30_set_ffff_reads_minus_one() {
        // The register accepts the raw 0xFFFF (-1 dBm) by design; the text
        // form of -1 is the normal way to get there.
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        assert!(m.cfg.sregs.set(REG_TX_POWER, 0xFFFF));
        assert_eq!(m.tx_power_dbm(), -1);
        assert!(run(&mut m, b"ATS30?\r", 0).contains("-001"));
        run(&mut m, b"ATS30=5\r", 10);
        run(&mut m, b"ATS30=-1\r", 20);
        assert_eq!(m.tx_power_dbm(), -1);
    }

    #[test]
    fn radio_diag_shows_in_atv() {
        let mut m = Modem::new(Config::new("CALLER").unwrap());
        // Before any diagnosis (the simulator) the line is absent.
        let t = run(&mut m, b"AT&V\r", 0);
        assert!(!t.contains("RADIO"), "{t:?}");
        m.set_radio_diag(0, 0);
        assert!(run(&mut m, b"AT&V\r", 10).contains("\r\nRADIO OK\r\n"));
        m.set_radio_diag(-2, 0x0160);
        let t = run(&mut m, b"AT&V\r", 20);
        assert!(t.contains("\r\nRADIO FAIL -2 ERR 0x0160\r\n"), "{t:?}");
    }
}
