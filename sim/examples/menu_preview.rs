//! Preview of the setup menu (AT+MENU): drives modems through every screen
//! and writes the host output (the whole session up to each snapshot) as
//! `.ans` files, plus a `fresh_*.ans` full paint of the same state and a
//! `slow_*.ans` run drained a few bytes per millisecond. `tools/menu_preview.py`
//! replays them on an 80x24 CP437 screen, checks that the three agree, and
//! draws the PNG images in docs/phase4-bench/menu.
//!
//! Run: `cargo run -p lfd3k-sim --example menu_preview -- <out dir>`.

use lfd3k_core::radio::{Radio, RxInfo};
use lfd3k_core::{Config, Directory, Modem, Settings, Tier};
use lfd3k_sim::{Harness, SimChannel};
use std::path::{Path, PathBuf};

struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

const NAME: &str = "AXISMODEM-4F1C";
const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";
const RIGHT: &[u8] = b"\x1bOC";
const ESC: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";

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
    fn new(rate: usize) -> Sess {
        let mut m = Modem::new(Config::new(NAME).expect("name"));
        m.load_settings(Settings::defaults(NAME, "kq7Rmx2Lp9"));
        m.set_wifi_status("WIFI AP AXISMODEM-4F1C 1 CLIENTS STA HomeNet 192.168.1.57");
        m.set_radio_diag(0, 0);
        m.set_stack_free(9216);
        m.set_heap_min(98_304);
        let mut d = Directory::new();
        let rx = |snr_x4: i16, rssi_dbm: i16| RxInfo {
            tier: Tier::Sf12,
            rssi_dbm,
            snr_x4,
        };
        d.heard("AXIS", 0b011, rx(38, -71), 55_000);
        d.heard("CALLER", 0b001, rx(-22, -109), 100_000);
        d.heard("REVTURN", 0b100, rx(14, -93), 5_000);
        d.heard("SIMIAN RADAR", 0, rx(-61, -118), 130_000);
        d.add_pinned("ROTATOCAM");
        d.add_pinned("AXIS");
        m.load_directory(d);
        let mut s = Sess {
            m,
            r: NullRadio,
            now: 200_000,
            out: Vec::new(),
            rate,
        };
        for line in [
            "AT+FWD=AXIS,bbs.axisbbs.example,23",
            "AT+FWD=WEATHER,wx.example.net,8023,R",
            "AT+FWD=MUD,mud.example.org,4000",
            "AT+PUB=AXIS",
            "AT+WSTA=HomeNet,password999",
            "AT&W",
        ] {
            s.typ(line.as_bytes());
            s.typ(b"\r");
        }
        s.m.take_save_request();
        s
    }

    fn drain(&mut self, max: usize) {
        let mut buf = [0u8; 4096];
        let n = self.m.host_output(&mut buf[..max.clamp(1, 4096)]);
        self.out.extend_from_slice(&buf[..n]);
    }

    /// Run `ms` milliseconds of ticks with the host draining.
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

    /// Type bytes one at a time, 20 ms apart.
    fn typ(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let n = self.m.host_input(&[b], &mut self.r, self.now);
            assert_eq!(n, 1);
            self.run(20);
        }
    }

    /// A lone Esc (then the 150 ms with nothing after it).
    fn esc(&mut self) {
        self.typ(ESC);
        self.run(200);
    }

    fn settle(&mut self) {
        self.run(if self.rate == 0 { 50 } else { 3000 });
    }
}

/// One snapshot: a key script from the main menu, played three ways.
fn snapshot(dir: &Path, name: &str, script: &[&[u8]]) {
    let play = |rate: usize| {
        let mut s = Sess::new(rate);
        s.typ(b"AT+MENU\r");
        for k in script {
            if *k == ESC {
                s.esc();
            } else {
                s.typ(k);
            }
        }
        s.settle();
        s.out
    };
    let fast = play(0);
    let slow = play(7);
    // Fresh: everything typed in one write, so the first paint already
    // shows the final state.
    let mut s = Sess::new(0);
    let mut all = b"AT+MENU\r".to_vec();
    for k in script {
        all.extend_from_slice(k);
    }
    let lone_esc = script.contains(&ESC);
    if lone_esc {
        // A lone Esc cannot be typed in one write (it would join the next
        // byte); the fresh paint is skipped for those.
        std::fs::write(dir.join(format!("fresh_{name}.ans")), b"").expect("write");
    } else {
        let n = s.m.host_input(&all, &mut s.r, s.now);
        assert_eq!(n, all.len());
        s.settle();
        std::fs::write(dir.join(format!("fresh_{name}.ans")), &s.out).expect("write");
    }
    std::fs::write(dir.join(format!("{name}.ans")), &fast).expect("write");
    std::fs::write(dir.join(format!("slow_{name}.ans")), &slow).expect("write");
    println!("{name}: {} bytes", fast.len());
}

fn main() {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/menu-preview"), PathBuf::from);
    std::fs::create_dir_all(&dir).expect("out dir");
    println!("size_of::<Modem>() = {}", core::mem::size_of::<Modem>());

    snapshot(&dir, "01-main", &[DOWN, DOWN, UP]);
    snapshot(&dir, "02-directory", &[ENTER, DOWN, DOWN]);
    snapshot(&dir, "03-directory-add", &[ENTER, b"a", b"KD4REV"]);
    snapshot(&dir, "04-settings", &[b"s", ENTER, DOWN, DOWN]);
    snapshot(
        &dir,
        "05-settings-edit",
        &[b"s", ENTER, DOWN, DOWN, ENTER, b"\x7f", b"7"],
    );
    snapshot(&dir, "06-forwards", &[b"f", DOWN]);
    snapshot(&dir, "07-forwards-delete", &[b"f", DOWN, b"d"]);
    snapshot(&dir, "08-wifi", &[b"w", DOWN, DOWN, DOWN]);
    snapshot(
        &dir,
        "09-wifi-password",
        &[b"w", DOWN, DOWN, DOWN, DOWN, ENTER, b"hunter22"],
    );
    snapshot(&dir, "10-about", &[b"a"]);
    snapshot(&dir, "11-link-idle", &[b"l"]);
    // An edit leaves unsaved changes: the status line says so, and Esc on
    // the main menu asks.
    snapshot(
        &dir,
        "12-unsaved-esc",
        &[b"s", ENTER, DOWN, RIGHT, ESC, ESC],
    );
    snapshot(&dir, "13-scan", &[ENTER, b"s"]);

    // Mid-call: CALLER dials AXIS, +++ then AT+MENU, Link Status.
    let mut h = Harness::new(SimChannel::constant_db(10, 7));
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(30_000, |h| h.screen_a().contains("CONNECT")));
    h.send_a_data(&[b'x'; 3000]);
    h.run_ms(3000);
    h.run_ms(1100);
    h.type_a("+++");
    h.run_ms(1100);
    h.type_a("AT+MENU\r");
    h.run_ms(100);
    h.type_a("l");
    h.send_b_data(&[b'y'; 3000]);
    h.run_ms(1500);
    std::fs::write(dir.join("14-link-call.ans"), h.screen_a_bytes()).expect("write");
    // Leave, then back to the call.
    h.type_a("\x1b");
    h.run_ms(300);
    h.type_a("\x1b");
    h.run_ms(300);
    h.type_a("ATO\r");
    h.run_ms(500);
    let tail = h.screen_a();
    let tail = &tail[tail.rfind("\x1b[2J").unwrap_or(0)..];
    println!("after leave: {:?}", &tail[..tail.len().min(120)]);

    // RING while in the menu (AXIS does not auto-answer here).
    let mut b = Config::new("AXIS").expect("name");
    b.sregs.set(0, 0);
    let mut h = Harness::with_configs(
        SimChannel::constant_db(10, 9),
        Config::new("CALLER").expect("name"),
        b,
    );
    h.type_b("AT+MENU\r");
    h.run_ms(200);
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(10_000, |h| h.b.call_state() == 2));
    h.run_ms(1100);
    std::fs::write(dir.join("15-ring.ans"), h.screen_b_bytes()).expect("write");
    h.type_b("\x1b");
    h.run_ms(300);
    let shown = h.screen_b();
    let tail = &shown[shown.rfind("\x1b[2J").unwrap_or(0)..];
    println!("ring leave: {:?}", &tail[..tail.len().min(80)]);
    h.type_b("ATA\r");
    let up = h.run_until(30_000, |h| h.screen_b().ends_with("/ARQ/LZ\r\n"));
    println!("ATA after leaving the menu connects: {up}");
}
