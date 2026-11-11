//! Phase 4 scenario tests on two modems over the simulated channel: LZ on the
//! air, the DC light, discovery between two modems, privacy modes, the menu
//! with a call. All seeded, so every run is the same run.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use lfd3k_core::frame::{decode, encode, FrameType, FLAG_LZ};
use lfd3k_core::{Config, Tier};
use lfd3k_sim::{FakeWire, Harness, Outcome, SimChannel, WireScript};

const CONNECT_SF5_LZ: &str = "\r\nCONNECT LORA SF5 BW500 62500/ARQ/LZ\r\n";
const CONNECT_SF5_PLAIN: &str = "\r\nCONNECT LORA SF5 BW500 62500/ARQ\r\n";
const GUARD_MS: u32 = 1_100;

/// BBS style ANSI and CP437 text of `len` bytes (a menu screen repeated with
/// the numbers changing), the kind of thing LZ is for.
fn ansi(len: usize) -> Vec<u8> {
    let mut s: Vec<u8> = b"\x1b[0m\x1b[2J\x1b[H".to_vec();
    let mut row = 0;
    while s.len() < len {
        let fg = 1 + row % 6;
        s.extend_from_slice(format!("\x1b[1;3{fg}m[{row:02}]\x1b[0;37m Read messages ").as_bytes());
        s.extend_from_slice(&[0xC4; 12]);
        s.extend_from_slice(format!("\x1b[1;30m.......{}\x1b[0m\r\n", row * 7).as_bytes());
        row += 1;
    }
    s.truncate(len);
    s
}

/// CALLER dials AXIS and waits for both to be connected.
fn connect(h: &mut Harness) {
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(30_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(
        ok,
        "no connect\nCALLER: {:?}\nAXIS: {:?}",
        h.screen_a(),
        h.screen_b()
    );
    // CONNECT is printed with the connect event.
    h.run_ms(50);
}

fn escape_a(h: &mut Harness) {
    h.run_ms(GUARD_MS);
    h.type_a("+++");
    h.run_ms(GUARD_MS);
}

fn cfg(name: &str, s0: u16, s31: u16) -> Config {
    let mut c = Config::new(name).expect("name");
    c.sregs.set(0, s0);
    c.sregs.set(31, s31);
    c
}

/// The `LZ ... RATIO x.y` line of AT&V as ratio x10.
fn lz_line(text: &str) -> Option<(bool, u32)> {
    let at = text.rfind("\r\nLZ ")?;
    let line = text[at + 2..].split("\r\n").next()?;
    let mut w = line.split(' ');
    w.next()?;
    let on = w.next()? == "ON";
    w.next()?;
    let r = w.next()?;
    let (a, b) = r.split_once('.')?;
    Some((on, a.parse::<u32>().ok()? * 10 + b.parse::<u32>().ok()?))
}

#[test]
fn connect_shows_lz() {
    let mut h = Harness::new(SimChannel::constant_db(12, 1));
    connect(&mut h);
    assert!(
        h.screen_a().contains(CONNECT_SF5_LZ),
        "CALLER: {:?}",
        h.screen_a()
    );
    assert!(
        h.screen_b().contains(CONNECT_SF5_LZ),
        "AXIS: {:?}",
        h.screen_b()
    );
    // Send BBS screens both ways; they arrive intact and in fewer air bytes.
    let a = ansi(1_800);
    let b = ansi(1_300);
    h.send_a_data(&a);
    h.send_b_data(&b);
    assert!(h.run_until(60_000, |h| h.received_b().len() >= a.len()
        && h.received_a().len() >= b.len()));
    assert_eq!(h.received_b(), a);
    assert_eq!(h.received_a(), b);
    // AT&V on CALLER, mid call: compression on, ratio well above 1.
    escape_a(&mut h);
    let mark = h.screen_a_bytes().len();
    h.type_a("AT&V\r");
    h.run_ms(200);
    let v = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
    let (on, r) = lz_line(&v).unwrap_or_else(|| panic!("no LZ line in {v:?}"));
    assert!(on, "{v:?}");
    assert!(
        r >= 20,
        "ratio x10 {r}: ANSI should at least halve on the air"
    );
    // After hang up the numbers stay for the last call; CONNECT after ATO
    // still says LZ.
    h.type_a("ATO\r");
    h.run_ms(200);
    assert!(
        h.screen_a().rfind("/ARQ/LZ\r\n") > h.screen_a().rfind("AT&V"),
        "{:?}",
        h.screen_a()
    );
}

#[test]
fn lz_negotiation_both_ways() {
    for (a31, b31) in [(1, 1), (1, 0), (0, 1), (0, 0)] {
        let mut h = Harness::with_configs(
            SimChannel::constant_db(12, 3),
            cfg("CALLER", 0, a31),
            cfg("AXIS", 1, b31),
        );
        connect(&mut h);
        let want = if a31 == 1 && b31 == 1 {
            CONNECT_SF5_LZ
        } else {
            CONNECT_SF5_PLAIN
        };
        assert!(
            h.screen_a().contains(want),
            "{a31}/{b31} CALLER: {:?}",
            h.screen_a()
        );
        assert!(
            h.screen_b().contains(want),
            "{a31}/{b31} AXIS: {:?}",
            h.screen_b()
        );
        let lz = a31 == 1 && b31 == 1;
        assert_eq!(h.a.lights(h.now_ms).dc, lz, "{a31}/{b31} CALLER DC");
        assert_eq!(h.b.lights(h.now_ms).dc, lz, "{a31}/{b31} AXIS DC");
        // Data is the same either way; no frame carries FLAG_LZ unless both agreed.
        let data = ansi(900);
        h.send_a_data(&data);
        assert!(h.run_until(60_000, |h| h.received_b().len() >= data.len()));
        assert_eq!(h.received_b(), data);
        let any_lz = h
            .trace()
            .iter()
            .any(|t| t.flags & FLAG_LZ != 0 && t.ftype == Some(FrameType::Data));
        assert_eq!(any_lz, lz, "{a31}/{b31}");
        // The radio frames: with LZ the screen takes fewer air bytes.
        let air: usize = h
            .trace()
            .iter()
            .filter(|t| t.from == 0 && t.ftype == Some(FrameType::Data))
            .map(|t| t.len)
            .sum();
        if lz {
            assert!(
                air < data.len() * 2 / 3,
                "{air} air bytes for {} data bytes",
                data.len()
            );
        } else {
            assert!(air >= data.len(), "{air}");
        }
        escape_a(&mut h);
        let mark = h.screen_a_bytes().len();
        h.type_a("AT&V\r");
        h.run_ms(200);
        let v = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
        let (on, r) = lz_line(&v).expect("LZ line");
        assert_eq!(on, lz, "{v:?}");
        if !lz {
            assert_eq!(r, 10, "no compression: ratio 1.0");
        }
    }
}

static CORRUPT_CAP: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static CORRUPT_DONE: AtomicBool = AtomicBool::new(false);

fn corrupt_hook(_t: u32, f: &[u8]) -> bool {
    if let Ok(fr) = decode(f) {
        if fr.header.ftype == FrameType::Data
            && fr.header.flags & FLAG_LZ != 0
            && !CORRUPT_DONE.swap(true, Ordering::SeqCst)
        {
            *CORRUPT_CAP.lock().unwrap() = Some(f.to_vec());
            return true;
        }
    }
    false
}

#[test]
fn lz_corrupt_frame_is_resent() {
    let mut chan = SimChannel::constant_db(12, 4);
    chan.drop = Some(corrupt_hook);
    let mut h = Harness::new(chan);
    connect(&mut h);
    let data = ansi(1_000);
    h.feed_a(&data[..200]);
    assert!(h.run_until(20_000, |_| CORRUPT_DONE.load(Ordering::SeqCst)));
    let good = CORRUPT_CAP
        .lock()
        .unwrap()
        .clone()
        .expect("captured the first LZ frame");
    let f = decode(&good).expect("frame");
    let hdr = f.header;
    // The same frame as the radio would deliver it, but with a compressed
    // payload that is not valid: a match 1024 back with nothing there. The
    // frame itself is well formed, so only the codec can refuse it.
    let mut bad = [0u8; 255];
    let n = encode(&hdr, &[0x00, b'x', 0x83, 0xFF, 0x83, 0xFF], &mut bad).expect("encode");
    let sent_b = h.trace().iter().filter(|t| t.from == 1).count();
    h.inject_b(&bad[..n], Tier::Sf5);
    // The truncated copy too: a literal run that claims more than it has.
    let n2 = encode(&hdr, &[0x7F, b'x'], &mut bad).expect("encode");
    h.inject_b(&bad[..n2], Tier::Sf5);
    h.run_ms(20);
    assert_eq!(
        h.trace().iter().filter(|t| t.from == 1).count(),
        sent_b,
        "a frame that does not decode is not acknowledged"
    );
    assert!(
        h.received_b().is_empty(),
        "nothing delivered from a bad frame: {:?}",
        h.received_b()
    );
    // The rest of the data goes on, and the lost frame is resent by ARQ and
    // arrives, intact and in order.
    h.feed_a(&data[200..]);
    let ok = h.run_until(60_000, |h| h.received_b().len() >= data.len());
    assert!(ok, "got {} of {}", h.received_b().len(), data.len());
    assert_eq!(h.received_b(), data);
    assert!(
        h.stats().resends[0] >= 1,
        "the sender resent: {:?}",
        h.stats().resends
    );
    // The resend carries the same bytes as the first try: FLAG_LZ again.
    let lz_data = h
        .trace()
        .iter()
        .filter(|t| t.from == 0 && t.ftype == Some(FrameType::Data) && t.flags & FLAG_LZ != 0)
        .count();
    assert!(lz_data >= 2, "{lz_data}");
}

static DROP_ACK_DONE: AtomicBool = AtomicBool::new(false);
static DROP_TRAIN_DONE: AtomicBool = AtomicBool::new(false);
static DROP_BOTH_ACK: AtomicU32 = AtomicU32::new(0);
static DROP_BOTH_TRAIN: AtomicU32 = AtomicU32::new(0);

fn drop_first_callack(_t: u32, f: &[u8]) -> bool {
    matches!(decode(f), Ok(fr) if fr.header.ftype == FrameType::CallAck)
        && !DROP_ACK_DONE.swap(true, Ordering::SeqCst)
}

fn drop_first_train(_t: u32, f: &[u8]) -> bool {
    matches!(decode(f), Ok(fr) if fr.header.ftype == FrameType::Train)
        && !DROP_TRAIN_DONE.swap(true, Ordering::SeqCst)
}

fn drop_first_two_of_both(_t: u32, f: &[u8]) -> bool {
    match decode(f).map(|fr| fr.header.ftype) {
        Ok(FrameType::CallAck) => DROP_BOTH_ACK.fetch_add(1, Ordering::SeqCst) < 2,
        Ok(FrameType::Train) => DROP_BOTH_TRAIN.fetch_add(1, Ordering::SeqCst) < 2,
        _ => false,
    }
}

#[test]
fn lz_one_lost_callack_both_ends_agree() {
    type Hook = fn(u32, &[u8]) -> bool;
    let hooks: [(&str, Hook); 3] = [
        ("first CallAck lost", drop_first_callack),
        ("first Train lost", drop_first_train),
        ("two of each lost", drop_first_two_of_both),
    ];
    for (what, hook) in hooks {
        for (a31, b31) in [(1, 1), (1, 0)] {
            DROP_ACK_DONE.store(false, Ordering::SeqCst);
            DROP_TRAIN_DONE.store(false, Ordering::SeqCst);
            DROP_BOTH_ACK.store(0, Ordering::SeqCst);
            DROP_BOTH_TRAIN.store(0, Ordering::SeqCst);
            let mut chan = SimChannel::constant_db(12, 5);
            chan.drop = Some(hook);
            let mut h = Harness::with_configs(chan, cfg("CALLER", 0, a31), cfg("AXIS", 1, b31));
            connect(&mut h);
            h.run_ms(100);
            let want_drops = if what.starts_with("two") { 4 } else { 1 };
            assert!(
                h.stats().dropped >= want_drops,
                "{what} ({a31}/{b31}): the hook dropped {} frames",
                h.stats().dropped
            );
            let a = h.screen_a().contains("/ARQ/LZ\r\n");
            let b = h.screen_b().contains("/ARQ/LZ\r\n");
            assert_eq!(
                a,
                b,
                "{what} ({a31}/{b31}): the two ends disagree\nCALLER {:?}\nAXIS {:?}",
                h.screen_a(),
                h.screen_b()
            );
            assert_eq!(a, a31 == 1 && b31 == 1, "{what} ({a31}/{b31})");
            assert_eq!(
                h.a.lights(h.now_ms).dc,
                b && h.b.lights(h.now_ms).dc,
                "{what}"
            );
            // And the data crosses either way (a one sided LZ would garble it).
            let data = ansi(700);
            h.send_a_data(&data);
            h.send_b_data(&data);
            assert!(h.run_until(60_000, |h| h.received_b().len() >= data.len()
                && h.received_a().len() >= data.len()));
            assert_eq!(h.received_b(), data, "{what}");
            assert_eq!(h.received_a(), data, "{what}");
        }
    }
}

#[test]
fn dc_light_follows_lz() {
    // Dark idle, lit with LZ on both ends, dark with LZ off, dark again
    // after a hang up, and dark on a Wi-Fi call that follows a radio LZ call.
    let mut h = Harness::new(SimChannel::constant_db(12, 6));
    assert!(!h.a.lights(h.now_ms).dc && !h.b.lights(h.now_ms).dc);
    connect(&mut h);
    assert!(
        h.a.lights(h.now_ms).dc && h.b.lights(h.now_ms).dc,
        "LZ call: DC lit"
    );
    assert!(h.a.lights(h.now_ms).cd);
    escape_a(&mut h);
    h.type_a("ATH\r");
    assert!(h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected()));
    h.run_ms(1_000);
    assert!(
        !h.a.lights(h.now_ms).dc && !h.b.lights(h.now_ms).dc,
        "hung up: DC dark"
    );
    // A Wi-Fi call on the modem that just had the LZ call.
    h.attach_wire_a(FakeWire::new(WireScript::Echo));
    h.type_a("AT+FWD=SRV,example.com,7000\r");
    h.run_ms(100);
    h.type_a("ATDT SRV\r");
    assert!(
        h.run_until(5_000, |h| h.screen_a().contains("CONNECT WIFI")),
        "{:?}",
        h.screen_a()
    );
    let l = h.a.lights(h.now_ms);
    assert!(l.cd, "carrier is up");
    assert!(!l.dc, "a Wi-Fi call never compresses: DC dark");
    // LZ off on one side: dark.
    let mut h = Harness::with_configs(
        SimChannel::constant_db(12, 6),
        cfg("CALLER", 0, 0),
        cfg("AXIS", 1, 1),
    );
    connect(&mut h);
    assert!(!h.a.lights(h.now_ms).dc && !h.b.lights(h.now_ms).dc);
    assert!(h.a.lights(h.now_ms).cd && h.b.lights(h.now_ms).cd);
}

#[test]
fn two_modems_scan_and_dial_by_directory() {
    let mut h = Harness::new(SimChannel::constant_db(12, 7));
    h.run_ms(100);
    // CALLER scans: AXIS answers in its slot, CALLER prints it.
    h.type_a("AT+SCAN\r");
    assert!(
        h.run_until(30_000, |h| h.screen_a().contains("\r\nOK\r\n")),
        "{:?}",
        h.screen_a()
    );
    let a = h.screen_a();
    assert!(a.contains("\r\nSCAN AXIS SNR 12.0 dB LORA SF"), "{a:?}");
    // Both ends now know each other: AXIS heard the ScanReq, CALLER the answer.
    assert!(h.a.directory().get("AXIS").is_some());
    assert!(h.b.directory().get("CALLER").is_some());
    let e = h.a.directory().get("AXIS").unwrap();
    assert_eq!(
        e.services & lfd3k_core::discovery::SVC_ACCEPTS,
        1,
        "AXIS auto-answers"
    );
    // The air: one ScanReq from CALLER, one ScanResp from AXIS, both at SF12.
    let req = h
        .trace()
        .iter()
        .filter(|t| t.ftype == Some(FrameType::ScanReq))
        .count();
    let resp = h
        .trace()
        .iter()
        .filter(|t| t.ftype == Some(FrameType::ScanResp))
        .count();
    assert_eq!((req, resp), (1, 1));
    assert!(h
        .trace()
        .iter()
        .filter(|t| matches!(t.ftype, Some(FrameType::ScanReq | FrameType::ScanResp)))
        .all(|t| t.tier == Tier::Sf12 && t.call_id == 0 && t.outcome == Outcome::Delivered));
    // The list as AT+DIR? shows it.
    let mark = h.screen_a_bytes().len();
    h.type_a("AT+DIR?\r");
    h.run_ms(100);
    let d = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
    assert!(d.contains("\r\nDIR AXIS LORA SF"), "{d:?}");
    // Pin it, then dial by that name: training still decides the tier, the
    // call connects at SF5 with LZ, like any other.
    h.type_a("AT+DIR=+AXIS\r");
    h.run_ms(50);
    h.type_a("ATDT axis\r");
    assert!(h.run_until(30_000, |h| h.a.is_connected() && h.b.is_connected()));
    h.run_ms(100);
    assert!(h.screen_a().contains(CONNECT_SF5_LZ), "{:?}", h.screen_a());
    assert!(h.screen_b().contains(CONNECT_SF5_LZ), "{:?}", h.screen_b());
    // No discovery frame during the call (30 s of it).
    let before = h.trace().len();
    h.run_ms(30_000);
    assert!(h.trace()[before..].iter().all(|t| !matches!(
        t.ftype,
        Some(FrameType::Beacon | FrameType::ScanReq | FrameType::ScanResp)
    )));
    // A scan during a call is an error.
    escape_a(&mut h);
    let mark = h.screen_a_bytes().len();
    h.type_a("AT+SCAN\r");
    h.run_ms(200);
    assert!(String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).contains("ERROR"));
}

#[test]
fn no_beacon_during_call() {
    let mut h = Harness::new(SimChannel::constant_db(12, 8));
    connect(&mut h);
    // Far longer than two beacon intervals inside the call.
    h.run_ms(700_000);
    assert!(
        h.a.is_connected() && h.b.is_connected(),
        "the call held: {:?}",
        h.screen_a()
    );
    let disc = |h: &Harness| {
        h.trace()
            .iter()
            .filter(|t| {
                matches!(
                    t.ftype,
                    Some(FrameType::Beacon | FrameType::ScanReq | FrameType::ScanResp)
                )
            })
            .map(|t| (t.t_ms, t.from))
            .collect::<Vec<_>>()
    };
    assert!(
        disc(&h).is_empty(),
        "discovery frames during a call: {:?}",
        disc(&h)
    );
    // Hang up. The beacon timer restarts: a full interval after the call.
    escape_a(&mut h);
    h.type_a("ATH\r");
    assert!(h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected()));
    let hung = h.now_ms;
    h.run_ms(250_000);
    assert!(
        disc(&h).is_empty(),
        "a beacon came too soon after the call: {:?}",
        disc(&h)
    );
    h.run_ms(150_000);
    let d = disc(&h);
    assert!(!d.is_empty(), "no beacon 400 s after the call");
    for (t, from) in d {
        let after = t - hung;
        assert!(
            (300_000..=362_000).contains(&after),
            "side {from} beaconed {after} ms after the call"
        );
    }
}

#[test]
fn ten_second_rule_holds_over_a_long_mixed_trace() {
    let mut h = Harness::new(SimChannel::constant_db(12, 9));
    // CALLER scans every 12.5 s for 10 minutes; both beacon in between.
    for _ in 0..48 {
        h.type_a("AT+SCAN\r");
        h.run_ms(12_500);
    }
    h.run_ms(60_000);
    for side in 0..2 {
        let mut last: Option<u32> = None;
        let mut n = 0;
        for t in h.trace().iter().filter(|t| {
            t.from == side
                && matches!(
                    t.ftype,
                    Some(FrameType::Beacon | FrameType::ScanReq | FrameType::ScanResp)
                )
        }) {
            if let Some(l) = last {
                assert!(
                    t.t_ms - l >= 10_000,
                    "side {side}: discovery frames {} ms apart at {}",
                    t.t_ms - l,
                    t.t_ms
                );
            }
            last = Some(t.t_ms);
            n += 1;
        }
        assert!(n >= 5, "side {side} sent only {n}");
    }
    // The scans got answers, and the modem is still a modem.
    assert!(
        h.screen_a().matches("\r\nSCAN AXIS ").count() >= 20,
        "{}",
        h.screen_a().matches("\r\nSCAN AXIS ").count()
    );
    h.type_a("AT\r");
    h.run_ms(100);
    assert!(h.screen_a().ends_with("\r\nOK\r\n"));
}

#[test]
fn hidden_modem_in_a_pair_is_silent_until_pinned() {
    let mut h = Harness::new(SimChannel::constant_db(12, 10));
    h.type_b("AT+BEACON=HIDDEN\r");
    h.run_ms(100);
    // CALLER beacons (and B hears it) over the first 7 minutes.
    h.run_ms(420_000);
    assert!(
        h.trace()
            .iter()
            .any(|t| t.from == 0 && t.ftype == Some(FrameType::Beacon)),
        "CALLER should have beaconed"
    );
    assert!(h.b.directory().get("CALLER").is_some(), "a beacon is heard");
    // The stranger calls the HIDDEN modem: nothing at all comes back.
    h.type_a("ATDT AXIS\r");
    assert!(
        h.run_until(60_000, |h| h.screen_a().contains("NO ANSWER")),
        "{:?}",
        h.screen_a()
    );
    assert_eq!(h.stats().frames_sent[1], 0, "HIDDEN sent a frame");
    assert!(!h.screen_b().contains("RING"));
    // Pin the caller by hand: now the door opens.
    h.type_b("AT+DIR=+CALLER\r");
    h.run_ms(100);
    connect(&mut h);
    assert!(h.screen_b().contains("RING"));
    assert!(h.screen_a().contains(CONNECT_SF5_LZ));
}

#[test]
fn own_name_never_in_directory_two_modems_one_name() {
    // Two modems with one name (or a SimNet loopback): each hears the other's
    // beacon and scan answers under its own name, and neither lists it.
    // They start under two names, so their beacon timers differ (a timer
    // is seeded from the name), then both are renamed to the same one.
    let mut h = Harness::with_configs(
        SimChannel::constant_db(12, 11),
        cfg("TWINA", 0, 1),
        cfg("TWINB", 1, 1),
    );
    h.run_ms(50);
    h.type_a("AT+NAME=TWIN\r");
    h.type_b("AT+NAME=TWIN\r");
    h.run_ms(50);
    assert!(h.screen_a().ends_with("\r\nOK\r\n") && h.screen_b().ends_with("\r\nOK\r\n"));
    h.run_ms(800_000);
    let beacons = h
        .trace()
        .iter()
        .filter(|t| t.ftype == Some(FrameType::Beacon))
        .count();
    assert!(beacons >= 2, "{beacons} beacons on the air");
    assert!(h.a.directory().is_empty() && h.b.directory().is_empty());
    h.type_a("AT+SCAN\r");
    assert!(h.run_until(30_000, |h| h.screen_a().contains("\r\nOK\r\n")));
    assert!(!h.screen_a().contains("SCAN TWIN"), "{:?}", h.screen_a());
    assert_eq!(
        h.trace()
            .iter()
            .filter(|t| t.ftype == Some(FrameType::ScanResp))
            .count(),
        0,
        "a modem never answers its own name's scan"
    );
    assert!(h.a.directory().is_empty() && h.b.directory().is_empty());
}

// ---- the menu with a call ----

#[test]
fn ring_while_in_menu() {
    let mut h = Harness::with_configs(
        SimChannel::constant_db(10, 12),
        cfg("CALLER", 0, 1),
        cfg("AXIS", 0, 1),
    );
    h.type_b("AT+MENU\r");
    h.run_ms(300);
    assert!(h.b.in_menu());
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(20_000, |h| h.b.call_state() == 2));
    h.run_ms(1_200);
    // RING shows in the status line of the menu; the menu is still there.
    let s = h.screen_b();
    let tail = &s[s.rfind("\x1b[2J").expect("menu painted")..];
    assert!(tail.contains(" RING "), "{tail:?}");
    assert!(h.b.in_menu());
    // Esc leaves: the screen is restored, OK, then the call still rings.
    h.type_b("\x1b");
    h.run_ms(400);
    assert!(!h.b.in_menu());
    let s = h.screen_b();
    let tail = &s[s.rfind("\x1b[2J").expect("restore")..];
    assert!(
        tail.contains("\r\nOK\r\n") && tail.contains("\r\nRING\r\n"),
        "{tail:?}"
    );
    // ATA works after leaving.
    h.type_b("ATA\r");
    assert!(
        h.run_until(30_000, |h| h.a.is_connected() && h.b.is_connected()),
        "{:?}",
        h.screen_b()
    );
    let s = h.screen_b();
    let tail = &s[s.rfind("\r\nRING\r\n").expect("RING printed") + 2..];
    let connect = tail
        .lines()
        .find(|l| l.starts_with("CONNECT LORA"))
        .unwrap_or_else(|| panic!("no CONNECT after ATA: {tail:?}"));
    assert!(connect.ends_with("/ARQ/LZ"), "{connect:?}");
}

#[test]
fn menu_mid_call_and_ato() {
    let mut h = Harness::new(SimChannel::constant_db(10, 13));
    connect(&mut h);
    h.send_a_data(&ansi(600));
    h.run_ms(3_000);
    escape_a(&mut h);
    h.type_a("AT+MENU\r");
    h.run_ms(300);
    assert!(h.a.in_menu());
    h.type_a("l");
    h.run_ms(300);
    // The Link screen: a call is up in online command mode.
    let s = h.screen_a();
    let tail = &s[s.rfind("\x1b[2J").expect("menu painted")..];
    assert!(tail.contains("Link Status"), "{tail:?}");
    assert!(tail.contains("ONLINE CMD"), "{tail:?}");
    assert!(tail.contains("Compression"), "{tail:?}");
    // AXIS sends while the menu is open: the data waits, the call holds.
    let msg = ansi(500);
    h.send_b_data(&msg);
    h.run_ms(5_000);
    assert!(h.a.is_connected(), "the call holds under the menu");
    // Esc, Esc: out of the menu, back at the OK prompt with the call up.
    h.type_a("\x1b");
    h.run_ms(300);
    h.type_a("\x1b");
    h.run_ms(400);
    assert!(!h.a.in_menu());
    let s = h.screen_a();
    assert!(s[s.rfind("\x1b[2J").unwrap()..].contains("\r\nOK\r\n"));
    // ATO goes back to the call: CONNECT again, and what AXIS sent arrives.
    h.type_a("ATO\r");
    assert!(
        h.run_until(30_000, |h| h.received_a().len() >= msg.len()),
        "{} of {}",
        h.received_a().len(),
        msg.len()
    );
    assert_eq!(h.received_a(), msg);
    assert!(h.screen_a().contains("/ARQ/LZ\r\n"));
    // And data still goes both ways.
    let more = ansi(300);
    h.send_a_data(&more);
    assert!(h.run_until(30_000, |h| h.received_b().len() >= 600 + more.len()));
}
