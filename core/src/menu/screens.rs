//! The menu's rows: every row is a pure function of the menu state and a
//! `View` of the modem, composed into a `Row` on demand.
//!
//! Layout (rows 1 to 24, 80 columns):
//!
//! | row | what |
//! |----:|------|
//! | 1 | title bar `LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM`, white on blue |
//! | 2 | version and release name, yellow on blue |
//! | 3 | top of the double box with the screen name |
//! | 4 to 21 | screen content (18 rows, 78 columns inside the box) |
//! | 22 | bottom of the box |
//! | 23 | keys for this screen, or the last message (green, or red for a refusal) |
//! | 24 | status line: name, call state, tier, Wi-Fi address, RING, SCAN, UNSAVED |
//!
//! A text box or a yes or no question is a single line box drawn over
//! content rows 15 to 17. Passwords are never composed into a row: they
//! always show as `********`, and typing one shows one `*` per character.

use super::render::{attr, Row, COLS};
use super::{dir_slot, Ask, Screen, Step, Ui, View, MAIN, SETTINGS, WIFI};
use crate::directory::DIR_MAX;
use crate::discovery::{SVC_ACCEPTS, SVC_FORWARD, SVC_HOTSPOT};
use crate::modem::{BANNER, RELEASE_NAME, RELEASE_TAGLINE};
use crate::settings::{BeaconMode, WireMode, MAX_FWD};
use crate::tier::Tier;

const TITLE: u8 = attr(15, 4);
const SUB: u8 = attr(11, 4);
const BORDER: u8 = attr(14, 0);
const HEAD: u8 = attr(15, 0);
const TEXT: u8 = attr(7, 0);
const LABEL: u8 = attr(6, 0);
const VALUE: u8 = attr(15, 0);
const DIM: u8 = attr(8, 0);
const KEY: u8 = attr(11, 0);
const COLHEAD: u8 = attr(14, 0);
const BAR: u8 = attr(0, 6);
const MSG_OK: u8 = attr(10, 0);
const MSG_ERR: u8 = attr(9, 0);
const STATUS: u8 = attr(0, 7);
const STATUS_HOT: u8 = attr(15, 1);
const STATUS_WARN: u8 = attr(1, 7);
const BOX: u8 = attr(15, 4);
const BOX_KEY: u8 = attr(11, 4);
const FIELD: u8 = attr(0, 7);
const LIT: u8 = attr(10, 0);
const ART: u8 = attr(12, 0);
const TAG: u8 = attr(13, 0);

/// First content row (0 based screen row).
const TOP: u8 = 3;
/// Content rows.
const BODY: u8 = 18;
/// Content rows the text box or question covers.
const BOX_ROWS: core::ops::RangeInclusive<u8> = 14..=16;
const BOX_L: u8 = 6;
const BOX_R: u8 = 73;

/// Compose screen row `r` (0 to 23).
pub fn row(r: u8, ui: &Ui, v: &View) -> Row {
    let mut w = Row::new(r);
    match r {
        0 => {
            w.center(TITLE, BANNER.as_bytes(), 0, COLS);
            w.finish(TITLE, false);
        }
        1 => {
            let mut t = [0u8; 64];
            let n = version_line(&mut t);
            w.center(SUB, t.get(..n).unwrap_or(&[]), 0, COLS);
            w.finish(SUB, false);
        }
        2 => {
            w.cell(BORDER, 0xC9);
            w.rep(BORDER, 0xCD, 2);
            w.cell(BORDER, 0xB5);
            w.str(HEAD, " ")
                .str(HEAD, screen_name(ui.screen))
                .str(HEAD, " ");
            w.cell(BORDER, 0xC6);
            while w.col() < (COLS - 1).min(w.limit) {
                w.cell(BORDER, 0xCD);
            }
            w.cell(BORDER, 0xBB);
        }
        21 => {
            w.cell(BORDER, 0xC8);
            w.rep(BORDER, 0xCD, COLS - 2);
            w.cell(BORDER, 0xBC);
        }
        22 => help(&mut w, ui, v),
        23 => status(&mut w, v),
        _ => {
            let i = r.saturating_sub(TOP).min(BODY - 1);
            w.cell(BORDER, 0xBA);
            w.limit = COLS - 1;
            if BOX_ROWS.contains(&i) && (ui.step.is_some() || ui.ask.is_some()) {
                overlay(&mut w, i, ui);
            } else {
                body(&mut w, i, ui, v);
            }
            w.pad_to(TEXT, COLS - 1);
            w.limit = COLS;
            w.cell(BORDER, 0xBA);
        }
    }
    w
}

/// `v<version>  <release name>`.
fn version_line(out: &mut [u8; 64]) -> usize {
    let mut n = 0;
    for part in [
        b"v".as_slice(),
        crate::VERSION.as_bytes(),
        b"  ",
        RELEASE_NAME.as_bytes(),
    ] {
        for &c in part {
            if let Some(d) = out.get_mut(n) {
                *d = c;
                n += 1;
            }
        }
    }
    n
}

fn screen_name(s: Screen) -> &'static str {
    match s {
        Screen::Main => "Main Menu",
        Screen::Dir => "Directory and Scan",
        Screen::Link => "Link Status",
        Screen::Settings => "Settings",
        Screen::Fwd => "Forwards",
        Screen::Wifi => "Wi-Fi",
        Screen::About => "About",
    }
}

fn body(w: &mut Row, i: u8, ui: &Ui, v: &View) {
    match ui.screen {
        Screen::Main => main_body(w, i, ui),
        Screen::Dir => dir_body(w, i, ui, v),
        Screen::Link => link_body(w, i, v),
        Screen::Settings => settings_body(w, i, ui, v),
        Screen::Fwd => fwd_body(w, i, ui, v),
        Screen::Wifi => wifi_body(w, i, ui, v),
        Screen::About => about_body(w, i, v),
    }
}

fn main_body(w: &mut Row, i: u8, ui: &Ui) {
    if i == 1 {
        w.center(TAG, RELEASE_TAGLINE.as_bytes(), 1, COLS - 1);
        return;
    }
    if i < 3 || i.is_multiple_of(2) {
        return;
    }
    let k = (i - 3) / 2;
    let Some((name, what)) = MAIN.get(usize::from(k)) else {
        return;
    };
    let sel = k == ui.main_sel;
    w.pad_to(TEXT, 12);
    w.cell(KEY, if sel { 0xAF } else { b' ' });
    w.cell(TEXT, b' ');
    let (hot, nm, desc) = if sel {
        (BAR, BAR, BAR)
    } else {
        (KEY, VALUE, TEXT)
    };
    w.cell(nm, b' ');
    let b = name.as_bytes();
    w.text(hot, b.get(..1).unwrap_or(&[]));
    w.text(nm, b.get(1..).unwrap_or(&[]));
    w.pad_to(nm, 37);
    w.str(desc, what);
    w.pad_to(desc, 70);
}

/// Age of a directory entry: `45s`, `12m`, `3h`, `99h`, or `-` when not
/// heard since boot.
fn age(w: &mut Row, a: u8, v: &View, heard: bool, at: u32) {
    if !heard {
        w.cell(a, b'-');
        return;
    }
    let d = v.now.wrapping_sub(at);
    let s = if d > u32::MAX / 2 { 0 } else { d / 1000 };
    let (n, unit) = if s < 60 {
        (s, b's')
    } else if s < 3600 {
        (s / 60, b'm')
    } else {
        ((s / 3600).min(99), b'h')
    };
    w.num(a, n).cell(a, unit);
}

/// SNR in dB x4 as `x.x`.
fn snr(w: &mut Row, a: u8, x4: i16) {
    let s = i32::from(x4);
    let tenths = (s.unsigned_abs() * 10 + 2) / 4;
    if s < 0 {
        w.cell(a, b'-');
    }
    w.num(a, tenths / 10).cell(a, b'.').num(a, tenths % 10);
}

fn dir_body(w: &mut Row, i: u8, ui: &Ui, v: &View) {
    let n = v.dir.len().min(DIR_MAX) as u8;
    match i {
        0 => {
            w.pad_to(TEXT, 2).str(COLHEAD, "NAME");
            w.pad_to(TEXT, 20).str(COLHEAD, "BEST TIER");
            w.pad_to(TEXT, 37).str(COLHEAD, "AGE");
            w.pad_to(TEXT, 43).str(COLHEAD, "SNR dB");
            w.pad_to(TEXT, 51).str(COLHEAD, "RSSI");
            w.pad_to(TEXT, 58).str(COLHEAD, "PIN");
            w.pad_to(TEXT, 63).str(COLHEAD, "OFFERS");
        }
        1 => {
            w.pad_to(TEXT, 2).rep(DIM, 0xC4, COLS - 5);
        }
        2..=16 => {
            let k = ui.top.saturating_add(i - 2);
            let Some(e) = dir_slot(v.dir, k).and_then(|s| v.dir.slot(s)) else {
                if i == 2 && n == 0 {
                    w.pad_to(TEXT, 2);
                    w.str(DIM, "No stations yet. Press S to scan, or A to add a name.");
                }
                return;
            };
            let sel = k == ui.sel.min(n.saturating_sub(1));
            let (a, d) = if sel { (BAR, BAR) } else { (VALUE, TEXT) };
            w.pad_to(TEXT, 1).cell(a, b' ');
            w.str(a, e.name()).pad_to(a, 20);
            match e.best_tier {
                Some(t) => w.str(d, t.name()),
                None => w.str(d, "NONE"),
            };
            w.pad_to(a, 37);
            age(w, d, v, e.heard_since_boot(), e.heard_ms);
            w.pad_to(a, 43);
            if e.heard_since_boot() {
                snr(w, d, e.last_snr_x4);
                w.pad_to(a, 51);
                w.int(d, i32::from(e.last_rssi));
            } else {
                w.cell(d, b'-').pad_to(a, 51).cell(d, b'-');
            }
            w.pad_to(a, 58);
            if e.pinned {
                w.str(if sel { BAR } else { KEY }, "PIN");
            }
            w.pad_to(a, 63);
            for (bit, word) in [
                (SVC_ACCEPTS, "CALLS "),
                (SVC_FORWARD, "FWD "),
                (SVC_HOTSPOT, "AP"),
            ] {
                if e.services & bit != 0 {
                    w.str(d, word);
                }
            }
            w.pad_to(a, COLS - 2);
        }
        17 => {
            let pinned = v.dir.iter().filter(|e| e.pinned).count();
            w.pad_to(TEXT, 2);
            w.num(VALUE, u32::from(n)).str(TEXT, " of 32 stations, ");
            w.num(VALUE, pinned as u32).str(TEXT, " pinned.");
            if v.scanning {
                w.str(KEY, "  Scanning: ")
                    .num(KEY, u32::from(v.scan_hits))
                    .str(KEY, " answers so far.");
            }
        }
        _ => {}
    }
}

/// `x` columns of label, then the value column.
fn label(w: &mut Row, a: u8, s: &str, at: u8) {
    w.pad_to(TEXT, 3).str(a, s).pad_to(a, at);
}

fn state_word(v: &View) -> &'static str {
    match v.wire_state {
        1 => return "DIALING WIFI",
        2 => return "WIFI CALL",
        3 => return "BRIDGE",
        _ => {}
    }
    match v.call_state {
        1 => "DIALING",
        2 => "RINGING",
        3 => "TRAINING",
        4 => "ONLINE",
        5 => "ONLINE CMD",
        _ => "IDLE",
    }
}

fn link_body(w: &mut Row, i: u8, v: &View) {
    const AT: u8 = 24;
    match i {
        1 => {
            label(w, LABEL, "Call", AT);
            w.str(VALUE, state_word(v));
        }
        2 => {
            label(w, LABEL, "Tier", AT);
            match v.tier {
                Some(t) => {
                    w.str(VALUE, t.name()).str(TEXT, "  ");
                    w.num(VALUE, t.air_bps()).str(TEXT, " bps on the air");
                }
                None => {
                    w.str(DIM, "none, no radio call is up");
                }
            }
        }
        3 => {
            label(w, LABEL, "Tier ladder", AT);
            w.str(DIM, "SF12 ");
            for t in Tier::LADDER {
                let lit = v.tier.is_some_and(|c| t <= c);
                w.cell(if lit { LIT } else { DIM }, if lit { 0xDB } else { 0xB0 });
                w.cell(TEXT, b' ');
            }
            w.str(DIM, "SF5");
        }
        4 => {
            label(w, LABEL, "Last SNR", AT);
            match v.last_rx {
                Some(rx) => {
                    snr(w, VALUE, rx.snr_x4);
                    w.str(TEXT, " dB");
                }
                None => {
                    w.str(DIM, "nothing heard yet");
                }
            }
        }
        5 => {
            label(w, LABEL, "Last RSSI", AT);
            match v.last_rx {
                Some(rx) => {
                    w.int(VALUE, i32::from(rx.rssi_dbm)).str(TEXT, " dBm");
                }
                None => {
                    w.str(DIM, "nothing heard yet");
                }
            }
        }
        6 => {
            label(w, LABEL, "Last frame on", AT);
            match v.last_rx {
                Some(rx) => w.str(VALUE, rx.tier.name()),
                None => w.str(DIM, "-"),
            };
        }
        7 => {
            label(w, LABEL, "Retrains", AT);
            let mut any = false;
            for t in v.retrains.iter().flatten() {
                if any {
                    w.str(TEXT, ", ");
                }
                w.str(VALUE, "SF").num(VALUE, u32::from(t.sf()));
                any = true;
            }
            if !any {
                w.str(DIM, "none");
            }
        }
        8 => {
            label(w, LABEL, "Throughput", AT);
            w.num(VALUE, v.bps)
                .str(TEXT, " bytes/s, both ways, last second");
        }
        9 => {
            label(w, LABEL, "Compression", AT);
            w.str(VALUE, if v.lz { "ON" } else { "OFF" });
            w.str(TEXT, "  ratio ")
                .num(VALUE, v.lz_x10 / 10)
                .cell(VALUE, b'.')
                .num(VALUE, v.lz_x10 % 10);
        }
        10 => {
            label(w, LABEL, "Wi-Fi", AT);
            if v.wifi.is_empty() {
                w.str(DIM, "no status yet");
            } else {
                w.text(VALUE, v.wifi);
            }
        }
        12 => {
            w.pad_to(TEXT, 3);
            w.str(DIM, "Live: these figures update once a second.");
        }
        _ => {}
    }
}

/// Colors for one field row: label, value, note.
fn field_attrs(sel: bool) -> (u8, u8, u8) {
    if sel {
        (BAR, BAR, BAR)
    } else {
        (LABEL, VALUE, DIM)
    }
}

fn onoff(on: bool) -> &'static str {
    if on {
        "ON"
    } else {
        "OFF"
    }
}

fn settings_body(w: &mut Row, i: u8, ui: &Ui, v: &View) {
    const AT: u8 = 26;
    let reg = |n: u8| v.set.sregs.get(n).unwrap_or(0);
    if (2..2 + SETTINGS.len() as u8).contains(&i) {
        let k = i - 2;
        let sel = k == ui.sel;
        let (l, a, d) = field_attrs(sel);
        w.pad_to(TEXT, 2).cell(l, b' ');
        w.str(l, SETTINGS.get(usize::from(k)).copied().unwrap_or(""));
        w.pad_to(l, AT);
        match k {
            0 => {
                w.str(a, v.name).str(
                    d,
                    if v.set.name().is_empty() {
                        "  the firmware default"
                    } else {
                        "  set with AT+NAME"
                    },
                );
            }
            1 => {
                w.str(a, v.set.beacon.name()).str(
                    d,
                    match v.set.beacon {
                        BeaconMode::On => "  beacons and answers scans",
                        BeaconMode::Quiet => "  answers scans, no beacons",
                        BeaconMode::Hidden => "  silent, pinned callers only",
                    },
                );
            }
            2 => {
                let dbm = i32::from(reg(30) as i16);
                w.int(a, dbm).str(a, " dBm");
                if v.fe_gain_db == 0 {
                    w.str(d, "  at the antenna");
                } else {
                    w.str(d, "  chip, about ")
                        .int(d, dbm + v.fe_gain_db)
                        .str(d, " dBm out");
                }
            }
            3 => {
                let c = reg(21);
                w.num(a, u32::from(c));
                if let Some(t) = Tier::from_index(c.min(7) as u8) {
                    w.str(d, "  up to ").str(d, t.name());
                }
            }
            4 => {
                match reg(0) {
                    0 => w.str(a, "0").str(d, "  off, answer with ATA"),
                    n => w
                        .num(a, u32::from(n))
                        .str(d, "  answer on ring ")
                        .num(d, u32::from(n)),
                };
            }
            5 => {
                w.str(a, onoff(reg(31) == 1))
                    .str(d, "  LZ on radio calls when both ends agree");
            }
            _ => {
                w.str(a, onoff(reg(32) == 1))
                    .str(d, "  menu on the first key after boot");
            }
        }
        w.pad_to(if sel { BAR } else { TEXT }, COLS - 2);
        return;
    }
    match i {
        11 => {
            w.pad_to(TEXT, 3);
            w.str(
                DIM,
                "Enter edits or switches, Left and Right step. Changes are live;",
            );
        }
        12 => {
            w.pad_to(TEXT, 3);
            w.str(
                DIM,
                "Save settings on the main menu keeps them after a reboot.",
            );
        }
        _ => {}
    }
}

fn wifi_body(w: &mut Row, i: u8, ui: &Ui, v: &View) {
    const AT: u8 = 28;
    if (2..2 + WIFI.len() as u8).contains(&i) {
        let k = i - 2;
        let sel = k == ui.sel;
        let (l, a, d) = field_attrs(sel);
        let sta = v.set.sta_ssid();
        w.pad_to(TEXT, 2).cell(l, b' ');
        w.str(l, WIFI.get(usize::from(k)).copied().unwrap_or(""));
        w.pad_to(l, AT);
        match k {
            0 => {
                w.str(a, onoff(v.set.ap_on));
            }
            1 => {
                w.str(a, v.set.ap_ssid());
            }
            2 => {
                w.str(a, "********");
            }
            3 if sta.is_empty() => {
                w.str(d, "none");
            }
            3 => {
                w.str(a, sta);
            }
            4 if sta.is_empty() => {
                w.str(d, "none");
            }
            4 => {
                w.str(a, "********");
            }
            _ => {
                w.str(d, "Enter forgets it");
            }
        }
        w.pad_to(if sel { BAR } else { TEXT }, COLS - 2);
        return;
    }
    match i {
        9 => {
            w.pad_to(TEXT, 3).str(LABEL, "Live status");
        }
        10 => {
            w.pad_to(TEXT, 3);
            if v.wifi.is_empty() {
                w.str(DIM, "no status yet");
            } else {
                w.text(VALUE, v.wifi);
            }
        }
        12 => {
            w.pad_to(TEXT, 3);
            w.str(DIM, "Passwords always show as ********. Changes are live;");
        }
        13 => {
            w.pad_to(TEXT, 3);
            w.str(
                DIM,
                "Save settings on the main menu keeps them after a reboot.",
            );
        }
        _ => {}
    }
}

fn fwd_body(w: &mut Row, i: u8, ui: &Ui, v: &View) {
    match i {
        0 => {
            w.pad_to(TEXT, 2).str(COLHEAD, "#");
            w.pad_to(TEXT, 5).str(COLHEAD, "NAME");
            w.pad_to(TEXT, 23).str(COLHEAD, "HOST:PORT");
            w.pad_to(TEXT, 62).str(COLHEAD, "MODE");
            w.pad_to(TEXT, 70).str(COLHEAD, "PUB");
        }
        1 => {
            w.pad_to(TEXT, 2).rep(DIM, 0xC4, COLS - 5);
        }
        2..=9 => {
            let k = i - 2;
            let sel = k == ui.sel;
            let (a, d) = if sel { (BAR, BAR) } else { (VALUE, TEXT) };
            w.pad_to(TEXT, 1).cell(a, b' ');
            w.num(d, u32::from(k) + 1).pad_to(a, 5);
            match v.set.fwd.get(usize::from(k)).and_then(Option::as_ref) {
                Some(f) => {
                    w.str(a, f.name()).pad_to(a, 23);
                    w.limit = 61;
                    w.str(d, f.host()).cell(d, b':').num(d, u32::from(f.port()));
                    w.limit = COLS - 1;
                    w.pad_to(a, 62);
                    w.str(
                        d,
                        if f.mode() == WireMode::Telnet {
                            "TELNET"
                        } else {
                            "RAW"
                        },
                    );
                    w.pad_to(a, 70);
                    if v.set.publish == Some(k) {
                        w.str(if sel { BAR } else { KEY }, "YES");
                    }
                }
                None => {
                    w.str(if sel { BAR } else { DIM }, "(empty)");
                }
            }
            w.pad_to(a, COLS - 2);
        }
        10 => {
            w.pad_to(TEXT, 3).str(LABEL, "Answered radio calls go to: ");
            match v.set.published() {
                Some(f) => w.str(VALUE, f.name()),
                None => w.str(DIM, "nothing published"),
            };
        }
        11 => {
            w.pad_to(TEXT, 3);
            w.str(
                DIM,
                "ATDT <name> dials a forward over Wi-Fi. T is telnet, R raw TCP.",
            );
        }
        12 => {
            w.pad_to(TEXT, 3).num(VALUE, v.set.fwd_count() as u32);
            w.str(DIM, " of ")
                .num(DIM, MAX_FWD as u32)
                .str(DIM, " used.");
        }
        14..=17 => found_row(w, i - 14, ui, v),
        _ => {}
    }
}

/// A telnet server the mDNS browse found that is not a forward yet:
/// `Found: <name> <ip>:<port>  Enter adds <forward> forward and publishes
/// it`. The name shows at most 16 characters; when the whole line would not
/// fit the box, the tail is the shorter `Enter adds and publishes it`.
fn found_row(w: &mut Row, k: u8, ui: &Ui, v: &View) {
    let Some(h) = v.found.new_host(v.set, k) else {
        return;
    };
    let sel = ui.sel.checked_sub(MAX_FWD as u8) == Some(k);
    let (a, d) = if sel { (BAR, BAR) } else { (KEY, TEXT) };
    let name = h.name().as_bytes();
    let name = name.get(..16).unwrap_or(name);
    let mut ip = [0u8; 15];
    let ip_len = h.ip_text(&mut ip);
    let ip = ip.get(..ip_len).unwrap_or(&[]);
    let port = h.port();
    let port_len = match port {
        0..=9 => 1,
        10..=99 => 2,
        100..=999 => 3,
        1000..=9999 => 4,
        _ => 5,
    };
    let fwd = h.fwd_pick().as_bytes();
    // Columns: 1 marker, "Found: " 7, name, space, ip:port, 2 spaces.
    let head = 1 + 7 + name.len() + 1 + ip.len() + 1 + port_len + 2;
    let long = 11 + fwd.len() + 25;
    let room = usize::from(COLS - 2).saturating_sub(1);
    w.pad_to(TEXT, 1).cell(a, if sel { 0xAF } else { b' ' });
    w.str(a, "Found: ").text(d, name).cell(d, b' ');
    w.text(d, ip).cell(d, b':').num(d, u32::from(port));
    w.str(d, "  ");
    if head + long <= room {
        w.str(a, "Enter").str(d, " adds ").text(d, fwd);
        w.str(d, " forward and publishes it");
    } else {
        w.str(a, "Enter").str(d, " adds and publishes it");
    }
    if sel {
        w.pad_to(a, COLS - 2);
    }
}

fn about_body(w: &mut Row, i: u8, v: &View) {
    const L: u8 = 18;
    const AT: u8 = 36;
    match i {
        1 => {
            w.center(VALUE, BANNER.as_bytes(), 1, COLS - 1);
        }
        2 => {
            let mut t = [0u8; 64];
            let n = version_line(&mut t);
            w.center(KEY, t.get(..n).unwrap_or(&[]), 1, COLS - 1);
        }
        3 => {
            w.center(TAG, RELEASE_TAGLINE.as_bytes(), 1, COLS - 1);
        }
        4 => {
            w.pad_to(TEXT, 15);
            for k in 0..48u8 {
                let c = [0xB0, 0xB1, 0xB2, 0xDB, 0xB2, 0xB1].get(usize::from(k % 6));
                let a = if (k / 6).is_multiple_of(2) { ART } else { TAG };
                w.cell(a, c.copied().unwrap_or(b' '));
            }
        }
        6 => {
            label(w, LABEL, "", L);
            w.str(LABEL, "Station name")
                .pad_to(TEXT, AT)
                .str(VALUE, v.name);
        }
        7 => {
            label(w, LABEL, "", L);
            w.str(LABEL, "Radio").pad_to(TEXT, AT);
            if let Some(ip) = v.simnet {
                w.str(VALUE, "SIMNET ");
                for (k, b) in ip.iter().enumerate() {
                    if k > 0 {
                        w.cell(VALUE, b'.');
                    }
                    w.num(VALUE, u32::from(*b));
                }
                if v.simnet_fail {
                    w.str(MSG_ERR, " NET FAIL");
                }
            } else {
                match v.radio {
                    Some((0, _)) => {
                        w.str(MSG_OK, "OK");
                    }
                    Some((code, errs)) => {
                        w.str(MSG_ERR, "FAIL ").int(MSG_ERR, i32::from(code));
                        w.str(MSG_ERR, " ERR 0x");
                        for shift in [12u32, 8, 4, 0] {
                            let d = ((errs >> shift) & 0xF) as u8;
                            w.cell(MSG_ERR, if d < 10 { b'0' + d } else { b'A' + d - 10 });
                        }
                    }
                    None => {
                        w.str(DIM, "not reported yet");
                    }
                }
            }
        }
        8 | 9 => {
            label(w, LABEL, "", L);
            let (name, val) = if i == 8 {
                ("Stack free", v.stack_free)
            } else {
                ("Heap low point", v.heap_min)
            };
            w.str(LABEL, name).pad_to(TEXT, AT);
            match val {
                Some(n) => w.num(VALUE, n).str(TEXT, " bytes"),
                None => w.str(DIM, "not reported yet"),
            };
        }
        10 => {
            label(w, LABEL, "", L);
            w.str(LABEL, "Last crash").pad_to(TEXT, AT);
            if v.note.is_empty() {
                w.str(DIM, "none");
            } else {
                w.text(VALUE, v.note);
            }
        }
        11 => {
            label(w, LABEL, "", L);
            w.str(LABEL, "Directory").pad_to(TEXT, AT);
            w.num(VALUE, v.dir.len() as u32)
                .str(TEXT, " of 32 stations");
        }
        12 => {
            label(w, LABEL, "", L);
            w.str(LABEL, "Privacy mode").pad_to(TEXT, AT);
            w.str(VALUE, v.set.beacon.name());
        }
        15 => {
            w.center(DIM, b"Best in SyncTERM with the CP437 font.", 1, COLS - 1);
        }
        _ => {}
    }
}

/// The text box or question over content rows 15 to 17.
fn overlay(w: &mut Row, i: u8, ui: &Ui) {
    w.pad_to(TEXT, BOX_L);
    let first = *BOX_ROWS.start();
    if i == first || i == *BOX_ROWS.end() {
        let (l, r) = if i == first {
            (0xDA, 0xBF)
        } else {
            (0xC0, 0xD9)
        };
        w.cell(BOX, l);
        while w.col() < BOX_R.min(w.limit) {
            w.cell(BOX, 0xC4);
        }
        w.cell(BOX, r);
        return;
    }
    w.cell(BOX, 0xB3).cell(BOX, b' ');
    if let Some(step) = ui.step {
        let (prompt, max, masked) = step.info();
        match step {
            Step::Sreg(n) => {
                w.str(BOX, "New value for S").num(BOX, u32::from(n));
            }
            _ => {
                w.str(BOX, prompt);
            }
        }
        w.str(BOX, ": ");
        let room = usize::from(BOX_R.saturating_sub(w.col()).saturating_sub(2));
        let text = ui.edit();
        let skip = text.len().saturating_sub(room.saturating_sub(1));
        let start = w.col();
        for &c in text.get(skip..).unwrap_or(&[]) {
            w.cell(FIELD, if masked { b'*' } else { c });
        }
        if text.len() < usize::from(max) {
            w.cell(FIELD, 0xDB);
        }
        let end = start.saturating_add(room as u8);
        w.pad_to(FIELD, end);
    } else if let Some(a) = ui.ask {
        let what = |w: &mut Row| {
            w.text(BOX_KEY, ui.subject());
        };
        match a {
            Ask::Discard => {
                w.str(BOX, "Unsaved changes. ");
                w.str(BOX_KEY, "Y").str(BOX, " discard and leave, ");
                w.str(BOX_KEY, "S").str(BOX, " save and leave, ");
                w.str(BOX_KEY, "N").str(BOX, " stay");
            }
            Ask::DirDelete | Ask::FwdDelete => {
                w.str(
                    BOX,
                    if a == Ask::DirDelete {
                        "Delete "
                    } else {
                        "Delete forward "
                    },
                );
                what(w);
                w.str(BOX, "? ");
                w.str(BOX_KEY, "Y").str(BOX, " yes, ");
                w.str(BOX_KEY, "N").str(BOX, " no");
            }
        }
    }
    w.pad_to(BOX, BOX_R);
    w.cell(BOX, 0xB3);
}

/// Key hints, or the last message.
fn help(w: &mut Row, ui: &Ui, v: &View) {
    w.cell(TEXT, b' ');
    let msg = ui.msg();
    if !msg.is_empty() {
        w.text(if ui.msg_err { MSG_ERR } else { MSG_OK }, msg);
        w.finish(TEXT, false);
        return;
    }
    let keys: &[(&str, &str)] = if ui.step.is_some() {
        &[
            ("Enter", "accept"),
            ("Backspace", "erase"),
            ("Esc", "cancel"),
        ]
    } else if ui.ask == Some(Ask::Discard) {
        &[
            ("Y", "discard"),
            ("S", "save"),
            ("N", "stay"),
            ("Esc", "discard"),
        ]
    } else if ui.ask.is_some() {
        &[("Y", "yes"), ("N", "no")]
    } else {
        match ui.screen {
            Screen::Main => &[
                ("Up Down", "move"),
                ("Enter", "open"),
                ("Letter", "jump"),
                ("Esc", "leave"),
            ],
            Screen::Dir => &[
                ("Enter", "dial"),
                ("S", "scan"),
                ("P", "pin"),
                ("A", "add"),
                ("D", "delete"),
                ("Esc", "back"),
            ],
            Screen::Settings => &[
                ("Up Down", "move"),
                ("Enter", "edit"),
                ("Left Right", "step"),
                ("Esc", "back"),
            ],
            Screen::Fwd if ui.sel >= MAX_FWD as u8 => &[
                ("Enter", "add as forward and publish"),
                ("Up Down", "move"),
                ("Esc", "back"),
            ],
            Screen::Fwd => &[
                ("A", "add"),
                ("Enter", "edit"),
                ("D", "delete"),
                ("P", "publish"),
                ("Esc", "back"),
            ],
            Screen::Wifi => &[("Up Down", "move"), ("Enter", "edit"), ("Esc", "back")],
            Screen::Link | Screen::About => &[("Esc", "back")],
        }
    };
    for (k, word) in keys {
        w.str(KEY, k)
            .cell(TEXT, b' ')
            .str(TEXT, word)
            .str(TEXT, "  ");
    }
    if v.scanning && ui.screen != Screen::Dir {
        w.str(KEY, "Scanning");
    }
    w.finish(TEXT, false);
}

/// The client address from the platform's Wi-Fi line (its last word), or
/// `NO IP`.
fn wifi_ip(line: &[u8]) -> &[u8] {
    let last = line.rsplit(|&c| c == b' ').next().unwrap_or(&[]);
    if last.is_empty() || last == b"IP" {
        b"NO IP"
    } else {
        last
    }
}

fn status(w: &mut Row, v: &View) {
    // Never column 80 of row 24 (an autowrap terminal would scroll).
    w.limit = COLS - 1;
    let sep = |w: &mut Row| {
        w.cell(STATUS, b' ').cell(STATUS, 0xB3).cell(STATUS, b' ');
    };
    w.cell(STATUS, b' ').str(STATUS, v.name);
    sep(w);
    w.str(STATUS, state_word(v));
    sep(w);
    match v.tier {
        Some(t) => {
            w.str(STATUS, "SF").num(STATUS, u32::from(t.sf()));
            w.cell(STATUS, b' ')
                .num(STATUS, t.air_bps())
                .str(STATUS, " bps");
        }
        None => {
            w.str(STATUS, "NO TIER");
        }
    }
    sep(w);
    // Flags before the Wi-Fi address, so a long address is what gets cut.
    if v.ringing {
        w.str(STATUS_HOT, " RING ").cell(STATUS, b' ');
    }
    if v.scanning {
        w.str(STATUS_WARN, "SCAN ")
            .num(STATUS_WARN, u32::from(v.scan_hits));
        w.cell(STATUS, b' ');
    }
    if v.unsaved {
        w.str(STATUS_WARN, "UNSAVED").cell(STATUS, b' ');
    }
    if v.ringing || v.scanning || v.unsaved {
        w.cell(STATUS, 0xB3).cell(STATUS, b' ');
    }
    if v.wifi.is_empty() {
        w.str(STATUS, "WIFI OFF");
    } else {
        let ip = wifi_ip(v.wifi);
        if ip != b"NO IP" {
            w.str(STATUS, "IP ");
        }
        w.text(STATUS, ip);
    }
    w.finish(STATUS, true);
}
