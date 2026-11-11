//! Phase 4 tests of the AT+MENU screen on one modem: the keys, the rows it
//! draws (replayed on a small 80x24 terminal), the edits and their checks,
//! the passwords, saving, boot to menu and what happens when the host is
//! slow. The two modem cases (a call arriving, a call going on) are in
//! `sim/tests/phase4.rs`.

use lfd3k_core::menu::render::{COLS, LEAVE, PROLOGUE, ROWS};
use lfd3k_core::menu::{Decoder, Key, MAIN};
use lfd3k_core::settings::BeaconMode;
use lfd3k_core::{Config, Directory, Found, Modem, Radio, RxInfo, Settings, Tier, BLOB_LEN};

struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

const NAME: &str = "AXISMODEM-4F1C";
const AP_PASS: &str = "kq7Rmx2Lp9";
const STA_PASS: &str = "Rev-Home-Secret-99";
const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";
const LEFT: &[u8] = b"\x1b[D";
const RIGHT: &[u8] = b"\x1bOC";
const ESC: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";
const BACK: &[u8] = b"\x7f";

/// What an 80x24 terminal shows after a stream of the menu's output: cursor
/// moves (`ESC [ r ; c H`), clear screen, erase to end of line, colors
/// (ignored) and cells. Bytes above 0x7F stay as they are (CP437).
struct Term {
    cells: [[u8; COLS as usize]; ROWS as usize],
    r: usize,
    c: usize,
}

impl Term {
    fn replay(out: &[u8]) -> Term {
        let mut t = Term {
            cells: [[b' '; COLS as usize]; ROWS as usize],
            r: 0,
            c: 0,
        };
        let mut i = 0;
        while i < out.len() {
            let b = out[i];
            i += 1;
            if b == 0x1B && out.get(i) == Some(&b'[') {
                i += 1;
                let start = i;
                while i < out.len() && !(0x40..=0x7E).contains(&out[i]) {
                    i += 1;
                }
                let params = String::from_utf8_lossy(&out[start..i.min(out.len())]).into_owned();
                let fin = out.get(i).copied().unwrap_or(0);
                i += 1;
                match fin {
                    b'H' => {
                        let mut it = params.split(';');
                        let r: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                        let c: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                        t.r = r.saturating_sub(1).min(ROWS as usize - 1);
                        t.c = c.saturating_sub(1).min(COLS as usize - 1);
                    }
                    b'J' => {
                        t.cells = [[b' '; COLS as usize]; ROWS as usize];
                    }
                    b'K' => {
                        for c in t.c..COLS as usize {
                            t.cells[t.r][c] = b' ';
                        }
                    }
                    _ => {}
                }
            } else if b == b'\r' {
                t.c = 0;
            } else if b == b'\n' {
                t.r = (t.r + 1).min(ROWS as usize - 1);
            } else if b >= 0x20 {
                assert!(
                    t.c < COLS as usize,
                    "wrote past column 80 on row {}",
                    t.r + 1
                );
                t.cells[t.r][t.c] = b;
                if t.c + 1 < COLS as usize {
                    t.c += 1;
                }
            }
        }
        t
    }

    /// Row `r` (0 based) as text, CP437 bytes shown as `#`.
    fn row(&self, r: usize) -> String {
        self.cells[r]
            .iter()
            .map(|&b| if b < 0x80 { b as char } else { '#' })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn cell(&self, r: usize, c: usize) -> u8 {
        self.cells[r][c]
    }

    fn all(&self) -> String {
        (0..ROWS as usize)
            .map(|r| self.row(r))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One modem on a silent radio, its host output kept whole.
struct Sess {
    m: Modem,
    r: NullRadio,
    now: u32,
    out: Vec<u8>,
    /// Bytes the host takes per millisecond (0: everything at once).
    rate: usize,
}

impl Sess {
    fn with(rate: usize, s: Settings) -> Sess {
        let mut m = Modem::new(Config::new(NAME).expect("name"));
        m.load_settings(s);
        m.set_wifi_status("WIFI AP AXISMODEM-4F1C 1 CLIENTS STA HomeNet 192.168.1.57");
        m.set_radio_diag(0, 0);
        let mut d = Directory::new();
        let rx = |snr_x4: i16, rssi_dbm: i16| RxInfo {
            tier: Tier::Sf12,
            rssi_dbm,
            snr_x4,
        };
        d.heard("AXIS", 0b011, rx(38, -71), 55_000);
        d.heard("CALLER", 0b001, rx(-22, -109), 100_000);
        d.add_pinned("ROTATOCAM");
        m.load_directory(d);
        let mut s = Sess {
            m,
            r: NullRadio,
            now: 200_000,
            out: Vec::new(),
            rate,
        };
        for line in ["AT+WSTA=HomeNet,Rev-Home-Secret-99", "AT&W"] {
            s.typ(line.as_bytes());
            s.typ(b"\r");
        }
        s.m.take_save_request();
        s.out.clear();
        s
    }

    fn new(rate: usize) -> Sess {
        Sess::with(rate, Settings::defaults(NAME, AP_PASS))
    }

    fn drain(&mut self, max: usize) {
        let mut buf = [0u8; 4096];
        let n = self.m.host_output(&mut buf[..max.clamp(1, 4096)]);
        self.out.extend_from_slice(&buf[..n]);
    }

    fn run(&mut self, ms: u32) {
        for _ in 0..ms {
            self.now += 1;
            self.m.tick(&mut self.r, self.now);
            if self.rate == 0 {
                loop {
                    let before = self.out.len();
                    self.drain(4096);
                    if self.out.len() == before {
                        break;
                    }
                }
            } else {
                self.drain(self.rate);
            }
        }
    }

    /// Bytes one at a time, 20 ms apart; every byte must be taken.
    fn typ(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let n = self.m.host_input(&[b], &mut self.r, self.now);
            assert_eq!(n, 1);
            self.run(20);
        }
    }

    fn esc(&mut self) {
        self.typ(ESC);
        self.run(200);
    }

    fn settle(&mut self) {
        self.run(if self.rate == 0 { 50 } else { 4000 });
    }

    fn open(&mut self) {
        self.typ(b"AT+MENU\r");
        self.settle();
        assert!(self.m.in_menu());
    }

    fn keys(&mut self, ks: &[&[u8]]) {
        for k in ks {
            if *k == ESC {
                self.esc();
            } else {
                self.typ(k);
            }
        }
        self.settle();
    }

    fn term(&self) -> Term {
        Term::replay(&self.out)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }
}

fn blob_of(s: &Settings) -> Vec<u8> {
    let mut b = [0u8; BLOB_LEN];
    let n = s.to_blob(&mut b);
    b[..n].to_vec()
}

#[test]
fn menu_opens_and_renders_title() {
    let mut s = Sess::new(0);
    s.typ(b"AT+MENU\r");
    s.settle();
    assert!(s.m.in_menu());
    // The line is echoed, then: reset colors, clear, home, hide the cursor.
    // No OK yet.
    assert!(
        s.out.starts_with(b"AT+MENU\r"),
        "{:?}",
        &s.out[..20.min(s.out.len())]
    );
    assert_eq!(&s.out[8..8 + PROLOGUE.len()], PROLOGUE);
    assert!(!s.text().contains("OK"), "OK comes when the menu closes");
    let t = s.term();
    assert!(
        t.row(0)
            .contains("LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM"),
        "{:?}",
        t.row(0)
    );
    assert!(t.row(1).contains("v0.1.0"), "{:?}", t.row(1));
    assert!(t.row(1).contains("r.420 BL4Z3B4ND ULTR4M0D3M"));
    assert!(t.row(2).contains(" Main Menu "), "{:?}", t.row(2));
    for (name, _) in MAIN {
        assert!(t.all().contains(name), "{name} missing:\n{}", t.all());
    }
    // The key line and the status line: name, call state, no tier, Wi-Fi.
    assert!(t.row(22).contains("Esc leave"), "{:?}", t.row(22));
    let st = t.row(23);
    assert!(
        st.contains(NAME) && st.contains("IDLE") && st.contains("NO TIER"),
        "{st:?}"
    );
    assert!(st.contains("IP 192.168.1.57"), "{st:?}");
    // The bottom right cell is never written (no scroll on an autowrap
    // terminal): the last row ends at column 79.
    assert_eq!(t.cell(23, 79), b' ');
    // Every other row is a full 80 cells of box or fill.
    assert!(
        t.cell(2, 0) == 0xC9 && t.cell(2, 79) == 0xBB,
        "top of the box"
    );
    assert!(
        t.cell(21, 0) == 0xC8 && t.cell(21, 79) == 0xBC,
        "bottom of the box"
    );
    // Esc leaves: restore the screen, then OK.
    s.esc();
    assert!(!s.m.in_menu());
    assert!(
        s.out.ends_with(&[LEAVE, b"\r\nOK\r\n"].concat()),
        "{:?}",
        &s.out[s.out.len() - 40..]
    );
    // The AT parser works again at once.
    s.typ(b"AT\r");
    assert!(s.out.ends_with(b"\r\nOK\r\n"));
}

#[test]
fn menu_arrows_and_enter() {
    let mut s = Sess::new(0);
    s.open();
    let marker = |s: &Sess| -> usize {
        let t = s.term();
        (0..MAIN.len())
            .find(|k| t.cell(6 + 2 * k, 12) == 0xAF)
            .expect("one selected row")
    };
    assert_eq!(marker(&s), 0);
    s.keys(&[DOWN]);
    assert_eq!(marker(&s), 1);
    // Both arrow spellings (ESC [ and ESC O), Tab, and wrapping.
    s.keys(&[RIGHT]);
    assert!(s.term().row(2).contains("Link Status"), "Right opens");
    s.keys(&[ESC]);
    assert!(s.term().row(2).contains("Main Menu"));
    s.keys(&[b"\t"]);
    assert_eq!(marker(&s), 2);
    s.keys(&[UP, UP, UP]);
    assert_eq!(marker(&s), MAIN.len() - 1, "Up wraps to the last item");
    s.keys(&[DOWN]);
    assert_eq!(marker(&s), 0, "Down wraps to the first");
    // Enter opens, Esc comes back; every screen by Enter.
    for (k, title) in [
        (0, "Directory and Scan"),
        (1, "Link Status"),
        (2, "Settings"),
        (3, "Forwards"),
        (4, "Wi-Fi"),
        (5, "About"),
    ] {
        let mut s = Sess::new(0);
        s.open();
        for _ in 0..k {
            s.keys(&[DOWN]);
        }
        s.keys(&[ENTER]);
        assert!(
            s.term().row(2).contains(&format!(" {title} ")),
            "{title}: {:?}",
            s.term().row(2)
        );
        s.keys(&[ESC]);
        assert!(s.term().row(2).contains("Main Menu"), "{title}");
        assert!(s.m.in_menu());
    }
    // A hot key jumps to its item (and opens it when it is the only one).
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"a"]);
    assert!(s.term().row(2).contains(" About "));
    s.keys(&[ESC, b"w"]);
    assert!(s.term().row(2).contains(" Wi-Fi "));
    // CR LF and LF alone are one Enter too.
    s.keys(&[ESC, DOWN, b"\r\n"]);
    assert!(s.term().row(2).contains(" About "), "{:?}", s.term().row(2));
    s.keys(&[ESC, UP, UP, b"\n"]);
    assert!(
        s.term().row(2).contains(" Forwards "),
        "{:?}",
        s.term().row(2)
    );
    // Exit menu by Enter on its row.
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[UP, ENTER]);
    assert!(!s.m.in_menu());
    assert!(s.out.ends_with(b"\r\nOK\r\n"));
}

#[test]
fn menu_edit_validates_like_at() {
    // The name box: the same rule as AT+NAME.
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"s", ENTER, ENTER]);
    let t = s.term();
    assert!(
        t.all().contains("Station name, empty for the default"),
        "{}",
        t.all()
    );
    // The box starts with the current name: erase it, type a bad one.
    s.keys(&[&BACK.repeat(20)]);
    s.keys(&[b"A:B", ENTER]);
    let t = s.term();
    assert!(
        t.row(22)
            .contains("A name is 1 to 16 printable characters, no colon."),
        "{:?}",
        t.row(22)
    );
    assert_eq!(s.m.station_name(), NAME, "a refused name changes nothing");
    // Still in the box: fix it, and the name is live.
    s.keys(&[BACK, BACK, BACK, b"REVJ", ENTER]);
    assert_eq!(s.m.station_name(), "REVJ");
    assert!(s.term().all().contains("REVJ"));
    assert!(
        s.term().row(23).contains("UNSAVED"),
        "{:?}",
        s.term().row(23)
    );
    // The box takes no more than 16.
    s.keys(&[
        ENTER,
        &BACK.repeat(20),
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        ENTER,
    ]);
    assert_eq!(s.m.station_name(), "ABCDEFGHIJKLMNOP");

    // S30 by the menu and by the AT command end in the same place for every
    // input: valid, out of range, not a number, empty.
    for v in [
        "7", "22", "99", "-50", "-9", "abc", "", "1x", "0", "65535", "00012",
    ] {
        let mut s = Sess::new(0);
        s.open();
        // Settings, down to TX power (row 2), edit.
        s.keys(&[b"s", ENTER, DOWN, DOWN, ENTER, &BACK.repeat(8)]);
        s.keys(&[v.as_bytes(), ENTER]);
        let via_menu = s.m.settings().sregs.get(30);
        let mut a = Sess::new(0);
        a.typ(format!("ATS30={v}\r").as_bytes());
        let via_at = a.m.settings().sregs.get(30);
        assert_eq!(via_menu, via_at, "S30={v:?}");
        // Esc out of whatever state the box is in.
        for _ in 0..5 {
            if !s.m.in_menu() {
                break;
            }
            s.keys(&[ESC]);
            if s.term().row(22).contains("Unsaved") || s.term().all().contains("Unsaved changes") {
                s.keys(&[b"y"]);
            }
        }
    }
    // The privacy mode steps with Left and Right and runs AT+BEACON.
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"s", ENTER, DOWN, RIGHT]);
    assert_eq!(s.m.settings().beacon, BeaconMode::Quiet);
    s.keys(&[RIGHT]);
    assert_eq!(s.m.settings().beacon, BeaconMode::Hidden);
    s.keys(&[RIGHT]);
    assert_eq!(s.m.settings().beacon, BeaconMode::On);
    s.keys(&[LEFT]);
    assert_eq!(s.m.settings().beacon, BeaconMode::Hidden);
    assert!(s.term().all().contains("silent, pinned callers only"));
    // Compression and boot to menu toggle S31 and S32.
    s.keys(&[DOWN, DOWN, DOWN, DOWN, ENTER]);
    assert_eq!(s.m.settings().sregs.get(31), Some(0));
    s.keys(&[ENTER]);
    assert_eq!(s.m.settings().sregs.get(31), Some(1));
    s.keys(&[DOWN, ENTER]);
    assert_eq!(s.m.settings().sregs.get(32), Some(1));
}

#[test]
fn menu_directory_keys() {
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[ENTER]);
    let t = s.term();
    assert!(t.row(2).contains("Directory and Scan"));
    for name in ["AXIS", "CALLER", "ROTATOCAM"] {
        assert!(t.all().contains(name), "{name}\n{}", t.all());
    }
    assert!(t.all().contains("3 of 32 stations, 1 pinned."));
    // Add a name by hand: checked, then pinned.
    s.keys(&[b"a", b"BAD:NAME", ENTER]);
    assert!(s.term().row(22).contains("1 to 16 printable characters"));
    s.keys(&[&BACK.repeat(8), b"KD4REV", ENTER]);
    assert!(s.m.directory().get("KD4REV").is_some_and(|e| e.pinned));
    // Pin a heard one: the selected row (sorted, so look it up).
    s.keys(&[b"p"]);
    let pinned = s.m.directory().iter().filter(|e| e.pinned).count();
    assert!(pinned >= 3, "{pinned}");
    // Delete asks first: N keeps it, Y removes it.
    let n = s.m.directory().len();
    s.keys(&[b"d"]);
    assert!(s.term().all().contains("Delete "));
    s.keys(&[b"n"]);
    assert_eq!(s.m.directory().len(), n);
    s.keys(&[b"d", b"y"]);
    assert_eq!(s.m.directory().len(), n - 1);
    // A scan from the menu starts and says so.
    s.keys(&[b"s"]);
    assert!(s.m.scanning());
    assert!(
        s.term().row(23).contains("SCAN 0"),
        "{:?}",
        s.term().row(23)
    );
    // Leaving with a scan running ends it silently.
    s.keys(&[ESC, ESC]);
    assert!(!s.m.scanning(), "a scan does not outlive the menu");
}

#[test]
fn menu_wifi_password_masked() {
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"w"]);
    let t = s.term();
    assert!(t.row(2).contains(" Wi-Fi "));
    let all = t.all();
    assert!(all.contains("Hotspot password"), "{all}");
    assert!(all.contains("********"), "{all}");
    assert!(!all.contains(AP_PASS) && !all.contains(STA_PASS), "{all}");
    // Client password (row 4): the box shows one star per character typed.
    s.keys(&[DOWN, DOWN, DOWN, DOWN, ENTER]);
    s.keys(&[b"hunter22secret"]);
    let t = s.term();
    let box_row = (0..24)
        .find(|r| t.row(*r).contains("Client network password"))
        .expect("password box");
    let line = t.row(box_row);
    assert_eq!(line.matches('*').count(), 14, "{line:?}");
    assert!(!t.all().contains("hunter22secret"));
    s.keys(&[ENTER]);
    assert_eq!(s.m.settings().sta_pass(), "hunter22secret");
    let all = s.term().all();
    assert!(
        all.contains("********") && !all.contains("hunter22secret"),
        "{all}"
    );
    // A short password is refused with the AT rule, and nothing changes.
    s.keys(&[ENTER, b"short", ENTER]);
    assert!(
        s.term().row(22).contains("8 to 63"),
        "{:?}",
        s.term().row(22)
    );
    assert_eq!(s.m.settings().sta_pass(), "hunter22secret");
    s.keys(&[ESC]);
    // The hotspot password is masked the same way.
    s.keys(&[UP, UP, ENTER, b"newhotspotpw1"]);
    let t = s.term();
    let line = (0..24)
        .map(|r| t.row(r))
        .find(|l| l.contains("Hotspot password:"))
        .expect("box");
    assert!(line.matches('*').count() == 13, "{line:?}");
    s.keys(&[ENTER]);
    assert_eq!(s.m.settings().ap_pass(), "newhotspotpw1");
    // Nothing the host ever received holds a password, old or new.
    let text = s.text();
    for pw in [
        AP_PASS,
        STA_PASS,
        "hunter22secret",
        "newhotspotpw1",
        "short",
    ] {
        assert!(!text.contains(pw), "{pw} was sent to the host");
    }
}

#[test]
fn menu_never_renders_client_password() {
    // Every screen, fast and under a slow host, with passwords in the
    // profile, while typing one, after leaving, and after a discard.
    for rate in [0usize, 5] {
        let mut s = Sess::new(rate);
        s.typ(b"ATE0\rAT+WAP=Spot,hotspotpw234\r");
        s.out.clear();
        s.open();
        // Every screen once.
        for _ in 0..6 {
            s.keys(&[ENTER]);
            s.keys(&[ESC]);
            s.keys(&[DOWN]);
        }
        // The client password box, typed into and cancelled, then again and
        // accepted, with screens in between.
        s.keys(&[b"w", DOWN, DOWN, DOWN, DOWN, ENTER, b"typedsecret99"]);
        assert!(
            s.term().all().contains("Client network password"),
            "the box is open"
        );
        assert!(
            s.term().all().contains("*************"),
            "one star per character"
        );
        s.keys(&[ESC]);
        assert_eq!(
            s.m.settings().sta_pass(),
            STA_PASS,
            "cancel changes nothing"
        );
        s.keys(&[ENTER, b"typedsecret99", ENTER]);
        assert_eq!(s.m.settings().sta_pass(), "typedsecret99");
        s.keys(&[UP, ENTER, ESC, ESC]);
        s.keys(&[b"l"]);
        s.keys(&[ESC, b"a"]);
        s.keys(&[ESC]);
        s.run(2_500);
        for _ in 0..6 {
            if s.m.in_menu() {
                s.keys(&[ESC]);
            }
        }
        assert!(!s.m.in_menu());
        let text = s.text();
        for pw in [AP_PASS, STA_PASS, "hotspotpw234", "typedsecret99"] {
            assert!(!text.contains(pw), "rate {rate}: {pw} reached the host");
        }
        // The network name is shown, only the passwords are hidden.
        assert!(text.contains("HomeNet"), "rate {rate}");
    }
}

#[test]
fn menu_save_writes_like_atw() {
    let mut s = Sess::new(0);
    s.open();
    // A change in Settings: privacy mode to QUIET.
    s.keys(&[b"s", ENTER, DOWN, RIGHT]);
    assert_eq!(s.m.settings().beacon, BeaconMode::Quiet);
    assert_eq!(s.m.saved_settings().beacon, BeaconMode::On, "not saved yet");
    assert!(s.term().row(23).contains("UNSAVED"));
    assert!(!s.m.take_save_request());
    s.keys(&[ESC]);
    // Main menu (the selection is on Settings), Save settings is item 7 of 8.
    s.keys(&[DOWN, DOWN, DOWN, DOWN, ENTER]);
    assert!(s.m.take_save_request(), "the same save request AT&W makes");
    assert!(!s.m.take_save_request(), "once");
    assert_eq!(s.m.saved_settings().beacon, BeaconMode::Quiet);
    let t = s.term();
    assert!(t.all().contains("Saved, like AT&W"), "{}", t.all());
    assert!(!t.row(23).contains("UNSAVED"), "{:?}", t.row(23));
    // Byte for byte what a person gets by typing the commands.
    let mut a = Sess::new(0);
    a.typ(b"AT+BEACON=QUIET\rAT&W\r");
    assert!(a.m.take_save_request());
    assert_eq!(blob_of(s.m.saved_settings()), blob_of(a.m.saved_settings()));
    // Esc on the main menu now leaves without a question.
    s.keys(&[ESC]);
    assert!(!s.m.in_menu());

    // Leaving with unsaved changes asks; S saves and leaves, Y discards, N stays.
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"s", ENTER, DOWN, RIGHT, ESC, ESC]);
    assert!(s.m.in_menu());
    assert!(
        s.term().all().contains("Unsaved changes."),
        "{}",
        s.term().all()
    );
    s.keys(&[b"n"]);
    assert!(s.m.in_menu() && s.m.settings().beacon == BeaconMode::Quiet);
    s.keys(&[ESC, b"s"]);
    assert!(!s.m.in_menu());
    assert!(s.m.take_save_request());
    assert_eq!(s.m.saved_settings().beacon, BeaconMode::Quiet);
    assert!(s.out.ends_with(b"\r\nOK\r\n"));
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[b"s", ENTER, DOWN, RIGHT, ESC, ESC, b"y"]);
    assert!(!s.m.in_menu());
    assert_eq!(
        s.m.settings().beacon,
        BeaconMode::On,
        "discard goes back to the saved profile"
    );
    assert!(!s.m.take_save_request());
}

#[test]
fn menu_esc_always_leaves() {
    // From every screen and every state, a few Esc leave the menu: no state
    // needs a different key to get out. (A box and a question take one more.)
    let paths: Vec<(&str, Vec<&[u8]>)> = vec![
        ("main", vec![]),
        ("directory", vec![ENTER]),
        ("directory add box", vec![ENTER, b"a", b"AB"]),
        ("directory delete ask", vec![ENTER, b"d"]),
        ("link", vec![DOWN, ENTER]),
        ("settings", vec![DOWN, DOWN, ENTER]),
        ("settings name box", vec![DOWN, DOWN, ENTER, ENTER]),
        (
            "settings unsaved ask",
            vec![DOWN, DOWN, ENTER, DOWN, RIGHT, ESC],
        ),
        ("forwards", vec![b"f"]),
        ("forwards add box", vec![b"f", b"a"]),
        ("wifi", vec![b"w"]),
        (
            "wifi password box",
            vec![b"w", DOWN, DOWN, DOWN, DOWN, ENTER, b"abc"],
        ),
        ("about", vec![b"a"]),
        (
            "a long junk key stream",
            vec![
                b"\x1b[1;2;3;4;5;6;7;8;9;1;2;3;4;5;6;7;8;9;0~",
                b"\x00\x01\x02",
                b"\xff",
            ],
        ),
    ];
    for (what, setup) in paths {
        for rate in [0usize, 3] {
            let mut s = Sess::new(rate);
            s.open();
            for k in &setup {
                if *k == ESC {
                    s.esc();
                } else {
                    s.typ(k);
                }
            }
            let mut n = 0;
            while s.m.in_menu() && n < 6 {
                s.esc();
                n += 1;
            }
            assert!(
                !s.m.in_menu(),
                "{what} (rate {rate}) still open after {n} Esc"
            );
            // And the modem is a modem again.
            s.run(if rate == 0 { 50 } else { 4000 });
            s.typ(b"AT\r");
            s.run(if rate == 0 { 50 } else { 600 });
            assert!(
                s.out.ends_with(b"\r\nOK\r\n"),
                "{what}: {:?}",
                &s.out[s.out.len().saturating_sub(30)..]
            );
        }
    }
}

#[test]
fn menu_csi_overlong_swallowed() {
    // The decoder alone: a CSI with hundreds of parameter bytes is one
    // sequence, its final byte gives one key or none, and nothing in the
    // middle comes out as a key.
    let mut d = Decoder::new(false);
    let mut keys = Vec::new();
    fn feed(d: &mut Decoder, keys: &mut Vec<Key>, b: u8, now: u32) {
        for k in d.feed(b, now).into_iter().flatten() {
            keys.push(k);
        }
    }
    feed(&mut d, &mut keys, 0x1B, 0);
    feed(&mut d, &mut keys, b'[', 1);
    for i in 0..400u32 {
        feed(&mut d, &mut keys, b"1;9 "[i as usize % 4], 2);
    }
    assert!(keys.is_empty(), "{keys:?}");
    feed(&mut d, &mut keys, b'A', 3);
    assert_eq!(keys, [Key::Up], "the final byte is the one key");
    keys.clear();
    // A CSI that ends in a final byte that is no arrow gives nothing.
    feed(&mut d, &mut keys, 0x1B, 10);
    feed(&mut d, &mut keys, b'[', 11);
    for _ in 0..300 {
        feed(&mut d, &mut keys, b'2', 12);
    }
    feed(&mut d, &mut keys, b'~', 13);
    assert!(keys.is_empty());
    // And the decoder is back to normal: a letter is a letter.
    feed(&mut d, &mut keys, b'x', 20);
    assert_eq!(keys, [Key::Char(b'x')]);
    keys.clear();
    // A CSI cut by a control byte ends there and the byte is handled.
    feed(&mut d, &mut keys, 0x1B, 30);
    feed(&mut d, &mut keys, b'[', 31);
    feed(&mut d, &mut keys, b'1', 32);
    feed(&mut d, &mut keys, b'\r', 33);
    assert_eq!(keys, [Key::Enter]);

    // Through the modem: the same stream moves the selection once and the
    // menu is not wedged afterwards.
    let mut s = Sess::new(0);
    s.open();
    let mut seq = b"\x1b[".to_vec();
    seq.extend(std::iter::repeat_n(b"0;1;".iter().copied(), 60).flatten());
    seq.push(b'B');
    s.m.host_input(&seq, &mut s.r, s.now);
    s.run(50);
    let t = s.term();
    assert_eq!(t.cell(8, 12), 0xAF, "one Down: Link Status ... Settings");
    assert_eq!(t.cell(6, 12), b' ');
    s.keys(&[ENTER]);
    assert!(s.term().row(2).contains(" Settings ") || s.term().row(2).contains(" Link Status "));
    s.keys(&[ESC, ESC]);
    assert!(!s.m.in_menu());
}

#[test]
fn menu_redraw_under_backpressure() {
    // A slow terminal: the screen takes longer to paint, never a different
    // picture, and typing is never refused. The same keys at 1, 2, 7 and
    // unlimited bytes per millisecond end on the same screen.
    let script: &[&[u8]] = &[DOWN, DOWN, ENTER, DOWN, RIGHT, b"\t"];
    let mut grids = Vec::new();
    for rate in [0usize, 7, 2, 1] {
        let mut s = Sess::new(rate);
        s.typ(b"AT+MENU\r");
        for k in script {
            // Input is taken at once even while the redraw is far behind.
            let n = s.m.host_input(k, &mut s.r, s.now);
            assert_eq!(n, k.len(), "rate {rate}");
            s.run(30);
        }
        s.run(if rate == 0 { 100 } else { 20_000 });
        // Nothing is left waiting.
        let mut buf = [0u8; 64];
        assert_eq!(s.m.host_output(&mut buf), 0, "rate {rate}: still drawing");
        grids.push((rate, s.term().all()));
    }
    for (rate, g) in &grids[1..] {
        assert_eq!(g, &grids[0].1, "rate {rate} shows another screen");
    }
    // While the host is not reading at all, the modem still takes keys and
    // Esc leaves; the leave text waits its turn and then arrives whole.
    let mut s = Sess::new(0);
    s.open();
    s.rate = 1;
    for _ in 0..30 {
        s.keys(&[DOWN]);
    }
    s.esc();
    s.rate = 0;
    s.run(3_000);
    assert!(!s.m.in_menu());
    assert!(s.out.ends_with(b"\r\nOK\r\n"));
    s.typ(b"AT\r");
    assert!(s.out.ends_with(b"\r\nOK\r\n"));
}

#[test]
fn boot_to_menu() {
    // S32=1 survives the blob; the first host bytes after boot open the menu
    // and are not an AT command; ATZ and a later key do not open it again.
    let mut set = Settings::defaults(NAME, AP_PASS);
    assert!(set.sregs.set(32, 1));
    let blob = blob_of(&set);
    let back = Settings::from_blob(&blob).expect("blob");
    assert!(back.boot_menu());
    let mut m = Modem::new(Config::new(NAME).expect("name"));
    m.load_settings(back);
    let mut r = NullRadio;
    assert!(!m.in_menu(), "nothing opens before a key");
    let took = m.host_input(b"AT\r", &mut r, 10);
    assert_eq!(took, 3, "the bytes are taken");
    assert!(m.in_menu(), "the first byte opens the menu");
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    for t in 11..60 {
        m.tick(&mut r, t);
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    assert!(out.starts_with(PROLOGUE));
    assert!(Term::replay(&out).row(2).contains(" Main Menu "));
    // The AT line that came with that first key was not run: no OK.
    assert!(!String::from_utf8_lossy(&out).contains("OK"));
    m.host_input(b"\x1b", &mut r, 100);
    m.tick(&mut r, 400);
    while m.host_output(&mut buf) > 0 {}
    assert!(!m.in_menu());
    m.host_input(b"ATZ\r", &mut r, 500);
    m.host_input(b"x", &mut r, 600);
    assert!(!m.in_menu(), "only the first bytes after power on");
    // Off by default.
    let mut m = Modem::new(Config::new(NAME).expect("name"));
    m.load_settings(Settings::defaults(NAME, AP_PASS));
    m.host_input(b"AT\r", &mut r, 10);
    assert!(!m.in_menu());
}

#[test]
fn mdns_found_board_needs_keypress() {
    // A board the mDNS browse found is only shown. Without a key the modem
    // adds nothing, publishes nothing and opens no connection, however long
    // it sits there; Esc and the host walking away change nothing either.
    let mut s = Sess::new(0);
    let forwards = s.m.settings().fwd_count();
    s.open();
    s.keys(&[DOWN, DOWN, DOWN, ENTER]);
    assert!(s.term().row(2).contains(" Forwards "));
    let board = Found::new(b"axis-bbs", [192, 168, 1, 164], 23).expect("a board");
    s.m.set_found_hosts([board]);
    s.settle();
    let shown = s.term().all();
    assert!(
        shown.contains("Found: axis-bbs 192.168.1.164:23"),
        "{shown}"
    );
    s.run(30_000);
    assert!(s.m.take_wire_request().is_none(), "dialed on its own");
    assert!(!s.m.wire_active(), "a call started on its own");
    assert_eq!(s.m.settings().fwd_count(), forwards, "forward added");
    assert!(s.m.settings().published().is_none(), "published");
    assert!(!s.m.take_save_request());
    assert!(s.m.in_menu());
    // The list is refreshed by the next browse: still nothing happens.
    s.m.set_found_hosts([board]);
    s.run(5_000);
    assert!(s.m.take_wire_request().is_none() && !s.m.wire_active());
    // Moving the highlight onto the row and off it again is not a key to
    // add it either (only Enter is).
    s.keys(&[DOWN, UP]);
    assert_eq!(s.m.settings().fwd_count(), forwards);
    assert!(s.m.take_wire_request().is_none() && !s.m.wire_active());
    // AT+FOUND? lists it and still adds nothing.
    let mut s = Sess::new(0);
    s.m.set_found_hosts([board]);
    s.typ(b"AT+FOUND?\r");
    s.run(100);
    assert!(
        s.text().contains("FOUND axis-bbs 192.168.1.164:23"),
        "{}",
        s.text()
    );
    assert_eq!(s.m.settings().fwd_count(), forwards);
    assert!(s.m.take_wire_request().is_none() && !s.m.wire_active());

    // Enter on the row adds the forward and publishes it, and even then no
    // call is started: dialing is a separate ATDT.
    let mut s = Sess::new(0);
    s.open();
    s.keys(&[DOWN, DOWN, DOWN, ENTER]);
    s.m.set_found_hosts([board]);
    s.settle();
    let n = s.m.settings().fwd_count();
    // Down to the found row: it follows the last forward row.
    for _ in 0..40 {
        s.keys(&[DOWN]);
        if s.m.settings().fwd_count() != n {
            break;
        }
        // Stop when the highlighted row is the found one.
        let row_of_found = (0..ROWS as usize).find(|&r| s.term().row(r).contains("Found:"));
        if let Some(r) = row_of_found {
            if (0..COLS as usize).any(|c| s.term().cell(r, c) == 0xAF) {
                break;
            }
        }
    }
    s.keys(&[ENTER]);
    assert_eq!(s.m.settings().fwd_count(), n + 1, "Enter adds the forward");
    assert!(s.m.settings().published().is_some(), "and publishes it");
    s.run(5_000);
    assert!(s.m.take_wire_request().is_none(), "Enter never dials");
    assert!(!s.m.wire_active());
}
