//! Phase 3 tests of the settings, the blob and the Wi-Fi, forward and
//! publish AT commands, through the public API of the core (a `Modem` on a
//! radio that hears nothing).

use lfd3k_core::settings::{parse_hostport, valid_fwd_name, BLOB_LEN};
use lfd3k_core::{Config, Forward, Modem, Radio, SRegs, Settings, Tier, WireMode, MAX_FWD};

struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

const AP_PASS: &str = "hotspotpw234";
const STA_PASS: &str = "Rev-Home-Secret-99";

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
    String::from_utf8_lossy(&all).into_owned()
}

/// A modem with first-boot settings loaded (hotspot on, password shown),
/// echo off so replies hold only what the modem prints.
fn modem() -> Modem {
    let mut m = Modem::new(Config::new("AXISMODEM-1234").unwrap());
    m.load_settings(Settings::defaults("AXISMODEM-1234", AP_PASS));
    m.take_wifi_changed();
    run(&mut m, "ATE0", 0);
    m
}

/// One AT line typed at `now`; returns what the modem printed.
fn run(m: &mut Modem, line: &str, now: u32) -> String {
    let mut r = NullRadio;
    let mut text = line.as_bytes().to_vec();
    text.push(b'\r');
    assert_eq!(m.host_input(&text, &mut r, now), text.len());
    output(m)
}

fn ok(m: &mut Modem, line: &str) {
    let t = run(m, line, 0);
    assert!(t.ends_with("\r\nOK\r\n"), "{line}: {t:?}");
}

fn error(m: &mut Modem, line: &str) {
    let t = run(m, line, 0);
    assert!(t.ends_with("\r\nERROR\r\n"), "{line}: {t:?}");
}

fn blob_of(s: &Settings) -> Vec<u8> {
    let mut b = [0u8; BLOB_LEN];
    let n = s.to_blob(&mut b);
    b[..n].to_vec()
}

/// CRC-16/CCITT-FALSE, written out here so a test can forge a blob that is
/// wrong in one field only and still carries a good checksum.
fn crc16(data: &[u8]) -> u16 {
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

fn fix_crc(blob: &mut [u8]) {
    let n = blob.len() - 2;
    let c = crc16(&blob[..n]).to_le_bytes();
    blob[n..].copy_from_slice(&c);
}

fn sample() -> Settings {
    let mut s = Settings::defaults("AXISMODEM-1234", AP_PASS);
    assert!(s.set_sta("HomeNet", STA_PASS));
    assert!(s.set_forward(Forward::new("AXIS", "192.168.1.164", 23, WireMode::Telnet).unwrap()));
    assert!(s.set_forward(Forward::new("echo", "10.0.0.9", 7000, WireMode::Raw).unwrap()));
    assert!(s.set_publish("axis"));
    s.sregs.set(7, 45);
    s.echo = false;
    s.verbose = false;
    s
}

// Offsets in the v1 blob (see core/src/settings.rs): forward records start at
// 217, 85 bytes each; the name length is the second byte of a record.
const FWD0: usize = 217;
const FWD_REC: usize = 85;

#[test]
fn settings_blob_rejects_garbage() {
    let good = blob_of(&sample());
    assert_eq!(good.len(), 917);
    assert!(Settings::from_blob(&good).is_some());

    // Wrong lengths: nothing, short, long, a whole 1 KB NVS page.
    assert!(Settings::from_blob(&[]).is_none());
    assert!(Settings::from_blob(&good[..898]).is_none());
    assert!(Settings::from_blob(&good[..4]).is_none());
    let mut long = good.clone();
    long.push(0);
    assert!(Settings::from_blob(&long).is_none());
    assert!(Settings::from_blob(&[0u8; BLOB_LEN]).is_none());
    assert!(Settings::from_blob(&[0xFFu8; BLOB_LEN]).is_none());
    assert!(
        Settings::from_blob(&[0xFFu8; 899]).is_none(),
        "erased flash"
    );

    // Bad checksum: every single bit of the body flips to a rejection.
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            assert!(Settings::from_blob(&b).is_none(), "byte {i} bit {bit}");
        }
    }

    // A forged blob with a GOOD checksum but a wrong field is refused too.
    let mut b = good.clone();
    b[4] = 4; // version
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none(), "old or new version");
    let mut b = good.clone();
    b[4] = 0;
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none());
    let mut b = good.clone();
    b[0] = b'X'; // magic
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none());
    let mut b = good.clone();
    b[5] |= 0x80; // unknown flag
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none());
    let mut b = good.clone();
    b[22] = 5; // publish points at an empty slot
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none());
    let mut b = good.clone();
    b[6 + 2 * 2] = 0xFF; // S7 low byte out of range
    b[6 + 2 * 2 + 1] = 0xFF;
    fix_crc(&mut b);
    assert!(Settings::from_blob(&b).is_none());

    // The good one still loads, and loading it does not need the garbage path.
    assert_eq!(Settings::from_blob(&good), Some(sample()));
}

#[test]
fn blob_duplicate_forward_rejected() {
    let mut s = Settings::defaults("AXIS", AP_PASS);
    assert!(s.set_forward(Forward::new("AXIS", "a.example", 23, WireMode::Telnet).unwrap()));
    assert!(s.set_forward(Forward::new("MORE", "b.example", 24, WireMode::Raw).unwrap()));
    let mut b = blob_of(&s);
    assert!(Settings::from_blob(&b).is_some());
    // Rename the second forward "more" to "axis" (a different case on purpose).
    let rec = FWD0 + FWD_REC;
    assert_eq!(&b[rec + 2..rec + 6], b"MORE");
    b[rec + 2..rec + 6].copy_from_slice(b"axis");
    fix_crc(&mut b);
    assert!(
        Settings::from_blob(&b).is_none(),
        "two forwards with the same name (any case)"
    );
    // The same edit to a name that is NOT a duplicate is fine (the forger works).
    b[rec + 2..rec + 6].copy_from_slice(b"MOR2");
    fix_crc(&mut b);
    let s2 = Settings::from_blob(&b).expect("distinct names load");
    assert_eq!(s2.forward("mor2").unwrap().host(), "b.example");
}

#[test]
fn station_name_with_colon_rejected() {
    assert!(Config::new("AXIS:1").is_none());
    assert!(Config::new(":").is_none());
    assert!(Config::new("AXIS").is_some());
    // The same rule keeps forward names and `host:port` apart.
    assert!(!valid_fwd_name("a:b"));
    assert!(!valid_fwd_name("a,b"));
    assert!(!valid_fwd_name("0"));
    assert!(valid_fwd_name("AXIS"));
    let mut m = modem();
    error(&mut m, "AT+FWD=a:b,host.example,23");
    error(&mut m, "AT+FWD=0,host.example,23");
    // A radio dial for a name with a colon that is not a forward or a valid
    // host:port is ERROR, never put on the air.
    let t = run(&mut m, "ATDT BAD:NAME:", 0);
    assert!(
        t.ends_with("\r\nERROR\r\n") && !t.contains("DIALING"),
        "{t:?}"
    );
}

#[test]
fn debug_never_prints_passwords() {
    let s = sample();
    assert_eq!(s.ap_pass(), AP_PASS);
    assert_eq!(s.sta_pass(), STA_PASS);
    for text in [format!("{s:?}"), format!("{s:#?}")] {
        assert!(!text.contains(AP_PASS), "{text}");
        assert!(!text.contains(STA_PASS), "{text}");
        assert!(text.contains("********"));
        // The names are not secret and still show.
        assert!(text.contains("HomeNet") && text.contains("AXISMODEM-1234"));
    }
}

#[test]
fn fwd_add_replace_delete_list() {
    let mut m = modem();
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    ok(&mut m, "AT+FWD=echo,10.0.0.9,7000");
    let t = run(&mut m, "AT+FWD?", 0);
    assert_eq!(
        t,
        "\r\nFWD AXIS 192.168.1.164:23 T\r\n\r\nFWD echo 10.0.0.9:7000 R\r\n\r\nOK\r\n"
    );
    // Same name in another case replaces, in the same slot, with a new mode.
    ok(&mut m, "AT+FWD=axis,new.example,2300,R");
    let t = run(&mut m, "AT+FWD?", 0);
    assert!(
        t.starts_with("\r\nFWD axis new.example:2300 R\r\n"),
        "{t:?}"
    );
    assert_eq!(t.matches("FWD ").count(), 2);
    // Explicit mode letters, any case, and fields trimmed.
    ok(&mut m, "AT+FWD=raw23, host.example , 23 , r");
    ok(&mut m, "AT+FWD=tel99,host.example,99,T");
    let t = run(&mut m, "AT+FWD?", 0);
    assert!(t.contains("FWD raw23 host.example:23 R"), "{t:?}");
    assert!(t.contains("FWD tel99 host.example:99 T"), "{t:?}");
    // Delete (case-insensitive), and deleting what is not there is an error.
    ok(&mut m, "AT+FWD=ECHO");
    error(&mut m, "AT+FWD=echo");
    assert!(!run(&mut m, "AT+FWD?", 0).contains("FWD echo"));
    assert_eq!(m.settings().fwd_count(), 3);
    // Bad arguments.
    for bad in [
        "AT+FWD=x,host,0",
        "AT+FWD=x,host,65536",
        "AT+FWD=x,host,23,X",
        "AT+FWD=x,host,23,T,extra",
        "AT+FWD=x,bad host,23",
        "AT+FWD=x,host",
        "AT+FWD=,host,23",
        "AT+FWD=this-name-is-way-too-long,host,23",
    ] {
        error(&mut m, bad);
    }
    // The table holds MAX_FWD and a ninth is refused; replacing still works.
    let mut m = modem();
    for i in 0..MAX_FWD {
        ok(&mut m, &format!("AT+FWD=f{i},h{i}.example,23"));
    }
    error(&mut m, "AT+FWD=extra,h.example,23");
    ok(&mut m, "AT+FWD=f3,other.example,23");
    assert_eq!(m.settings().forward("f3").unwrap().host(), "other.example");
    assert_eq!(m.settings().fwd_count(), MAX_FWD);
}

#[test]
fn pub_unknown_forward_error() {
    let mut m = modem();
    error(&mut m, "AT+PUB=NOPE");
    assert_eq!(run(&mut m, "AT+PUB?", 0), "\r\nPUB NONE\r\n\r\nOK\r\n");
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    ok(&mut m, "AT+PUB=axis");
    assert_eq!(run(&mut m, "AT+PUB?", 0), "\r\nPUB AXIS\r\n\r\nOK\r\n");
    error(&mut m, "AT+PUB=NOPE");
    assert_eq!(
        run(&mut m, "AT+PUB?", 0),
        "\r\nPUB AXIS\r\n\r\nOK\r\n",
        "a failed AT+PUB changes nothing"
    );
    // Deleting the published forward unpublishes it.
    ok(&mut m, "AT+FWD=AXIS");
    assert_eq!(run(&mut m, "AT+PUB?", 0), "\r\nPUB NONE\r\n\r\nOK\r\n");
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    ok(&mut m, "AT+PUB=AXIS");
    ok(&mut m, "AT+PUB=0");
    assert_eq!(run(&mut m, "AT+PUB?", 0), "\r\nPUB NONE\r\n\r\nOK\r\n");
}

#[test]
fn atdt_host_port_parses() {
    for (s, want) in [
        ("192.168.1.164:23", Some(("192.168.1.164", 23))),
        ("example.com:8080", Some(("example.com", 8080))),
        ("a-b.c:1", Some(("a-b.c", 1))),
        ("host:65535", Some(("host", 65535))),
        ("host:0", None),
        ("host:65536", None),
        ("host:", None),
        (":23", None),
        ("host", None),
        ("ho st:23", None),
        ("host:2x", None),
        ("host:-1", None),
        ("a:b:23", None),
    ] {
        assert_eq!(parse_hostport(s), want, "{s}");
    }

    // On a modem: telnet by default for 23, raw otherwise; DIALING echoes the
    // target as typed; the platform gets exactly host and port.
    for (line, host, port) in [
        ("ATDT 192.168.1.164:23", "192.168.1.164", 23),
        ("ATDT example.com:8080", "example.com", 8080),
    ] {
        let mut m = modem();
        let t = run(&mut m, line, 0);
        assert_eq!(t, format!("\r\nDIALING {}\r\n", &line[5..]), "{t:?}");
        assert!(m.wire_active());
        assert_eq!(m.wire_state(), 1);
        let req = m.take_wire_request().expect("a request");
        assert_eq!((req.host(), req.port()), (host, port));
        assert!(m.take_wire_request().is_none(), "taken once");
    }
    // A forward wins over host:port parsing, and gives its own host and mode.
    let mut m = modem();
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    run(&mut m, "ATDT axis", 0);
    let req = m.take_wire_request().unwrap();
    assert_eq!((req.host(), req.port()), ("192.168.1.164", 23));
}

#[test]
fn atdt_bad_host_is_radio_or_error() {
    // A plain name that is no forward is a radio dial (as in phase 1/2).
    let mut m = modem();
    let t = run(&mut m, "ATDT SOMEONE", 0);
    assert!(t.starts_with("\r\nDIALING SOMEONE\r\n"), "{t:?}");
    assert!(!m.wire_active());
    assert!(m.take_wire_request().is_none());

    // Anything with a colon that is not a good host:port is ERROR, and
    // nothing is dialed (no wire request, no DIALING, no radio call).
    for line in [
        "ATDT host:0",
        "ATDT host:70000",
        "ATDT bad_host:23",
        "ATDT host:",
        "ATDT :23",
        "ATDT a:b:c",
        "ATDT host name:23",
    ] {
        let mut m = modem();
        let t = run(&mut m, line, 0);
        assert!(t.ends_with("\r\nERROR\r\n"), "{line}: {t:?}");
        assert!(!t.contains("DIALING"), "{line}: {t:?}");
        assert!(
            !m.wire_active() && m.take_wire_request().is_none(),
            "{line}"
        );
        assert_eq!(m.call_state(), 0, "{line}: no radio call either");
    }
    // A host longer than 63 characters is not a host.
    let mut m = modem();
    let long = format!("ATDT {}.example:23", "h".repeat(70));
    let t = run(&mut m, &long, 0);
    assert!(t.ends_with("\r\nERROR\r\n"), "{t:?}");
    assert!(m.take_wire_request().is_none());
}

#[test]
fn wap_password_length_checked() {
    let mut m = modem();
    ok(&mut m, &format!("AT+WAP=Net,{}", "p".repeat(8)));
    ok(&mut m, &format!("AT+WAP=Net,{}", "p".repeat(63)));
    error(&mut m, &format!("AT+WAP=Net,{}", "p".repeat(7)));
    error(&mut m, &format!("AT+WAP=Net,{}", "p".repeat(64)));
    error(&mut m, "AT+WAP=Net,");
    error(&mut m, "AT+WAP=Net");
    error(&mut m, "AT+WAP=,password1");
    error(&mut m, &format!("AT+WAP={},password1", "s".repeat(33)));
    ok(&mut m, &format!("AT+WAP={},password1", "s".repeat(32)));
    // A refused line changes nothing.
    ok(&mut m, "AT+WAP=Keep,password1");
    error(&mut m, "AT+WAP=Other,short");
    let t = run(&mut m, "AT+WAP?", 0);
    assert!(t.contains("WAP ON Keep password1"), "{t:?}");
    // Same limits for the client network.
    ok(&mut m, &format!("AT+WSTA=Home,{}", "q".repeat(63)));
    error(&mut m, &format!("AT+WSTA=Home,{}", "q".repeat(64)));
    error(&mut m, "AT+WSTA=Home,1234567");
    // Power: only 0 and 1.
    ok(&mut m, "AT+WAP=0");
    assert!(run(&mut m, "AT+WAP?", 0).contains("WAP OFF Keep"));
    ok(&mut m, "AT+WAP=1");
    error(&mut m, "AT+WAP=2");
}

#[test]
fn wsta_never_prints_password() {
    let mut m = modem();
    ok(&mut m, &format!("AT+WSTA=HomeNet,{STA_PASS}"));
    let mut all = String::new();
    for q in [
        "AT+WSTA?", "AT&V", "AT+WAP?", "AT+WIFI?", "ATI", "AT+FWD?", "AT+PUB?", "ATS7?",
    ] {
        all += &run(&mut m, q, 0);
    }
    assert!(all.contains("WSTA HomeNet ********\r\n"), "{all:?}");
    assert!(!all.contains(STA_PASS), "{all:?}");
    assert!(!all.contains("Secret"), "{all:?}");
    // The platform status line is the platform's business; the modem prints
    // what it is given, so the platform must not put the password there. The
    // modem itself never copies the password into it.
    m.set_wifi_status("STA HomeNet 192.168.1.50 -61 dBm");
    let t = run(&mut m, "AT+WIFI?", 0);
    assert!(t.contains("STA HomeNet 192.168.1.50"), "{t:?}");
    assert!(!t.contains(STA_PASS));
    // And after a save and a reset.
    ok(&mut m, "AT&W");
    ok(&mut m, "ATZ");
    assert!(!run(&mut m, "AT&V", 0).contains(STA_PASS));
}

#[test]
fn wsta_query_masks_password() {
    let mut m = modem();
    assert_eq!(run(&mut m, "AT+WSTA?", 0), "\r\nWSTA OFF\r\n\r\nOK\r\n");
    ok(&mut m, "AT+WSTA=Home Net 2,abcdefgh1");
    // The name keeps its spaces, the password is eight stars whatever its length.
    assert_eq!(
        run(&mut m, "AT+WSTA?", 0),
        "\r\nWSTA Home Net 2 ********\r\n\r\nOK\r\n"
    );
    ok(&mut m, &format!("AT+WSTA=Home Net 2,{}", "z".repeat(63)));
    assert_eq!(
        run(&mut m, "AT+WSTA?", 0),
        "\r\nWSTA Home Net 2 ********\r\n\r\nOK\r\n"
    );
    // The hotspot password, by contrast, is shown (Rev needs it to join).
    assert!(run(&mut m, "AT+WAP?", 0).contains(&format!("WAP ON AXISMODEM-1234 {AP_PASS}")));
    // Forget.
    ok(&mut m, "AT+WSTA=0");
    assert_eq!(run(&mut m, "AT+WSTA?", 0), "\r\nWSTA OFF\r\n\r\nOK\r\n");
}

#[test]
fn atw_sets_save_and_settings_roundtrip() {
    let mut m = modem();
    assert!(!m.take_save_request());
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    ok(&mut m, "AT+FWD=echo,10.0.0.9,7000");
    ok(&mut m, "AT+PUB=echo");
    ok(&mut m, &format!("AT+WSTA=HomeNet,{STA_PASS}"));
    ok(&mut m, "ATS7=45");
    ok(&mut m, "ATS0=2");
    assert!(!m.take_save_request(), "nothing saves by itself");
    ok(&mut m, "AT&W");
    assert!(m.take_save_request(), "AT&W asks the platform to save");
    assert!(!m.take_save_request(), "once");

    let blob = blob_of(m.settings());
    let back = Settings::from_blob(&blob).expect("the saved blob loads");
    assert_eq!(&back, m.settings());
    assert_eq!(back.sregs.get(7), Some(45));
    assert_eq!(back.sregs.get(0), Some(2));
    assert_eq!(back.published().unwrap().name(), "echo");
    assert_eq!(back.sta_ssid(), "HomeNet");
    assert_eq!(back.sta_pass(), STA_PASS);
    assert_eq!(back.forward("axis").unwrap().host(), "192.168.1.164");
    assert_eq!(back.forward("echo").unwrap().mode(), WireMode::Raw);

    // A fresh modem that loads it is the same modem.
    let mut m2 = Modem::new(Config::new("AXISMODEM-1234").unwrap());
    m2.load_settings(back);
    run(&mut m2, "ATE0", 0);
    assert_eq!(run(&mut m2, "ATS7?", 0), "\r\n045\r\n\r\nOK\r\n");
    assert_eq!(run(&mut m2, "AT+PUB?", 0), "\r\nPUB echo\r\n\r\nOK\r\n");
    let t = run(&mut m2, "AT+FWD?", 0);
    assert!(t.contains("FWD AXIS 192.168.1.164:23 T") && t.contains("FWD echo 10.0.0.9:7000 R"));
    assert!(run(&mut m2, "AT+WSTA?", 0).contains("WSTA HomeNet ********"));
}

#[test]
fn settings_blob_keeps_sregs_and_ev() {
    let mut m = modem();
    ok(&mut m, "ATS7=45");
    ok(&mut m, "ATS12=80");
    ok(&mut m, "ATS30=7");
    ok(&mut m, "ATS22=33");
    // V0: numeric results, "0\r".
    assert_eq!(run(&mut m, "ATV0", 0), "0\r");
    assert_eq!(run(&mut m, "ATE0", 0), "0\r");
    assert_eq!(run(&mut m, "AT&W", 0), "0\r");
    let s = m.settings();
    assert!(!s.echo && !s.verbose);
    let blob = blob_of(s);

    let back = Settings::from_blob(&blob).unwrap();
    assert!(!back.echo && !back.verbose, "E0 and V0 survive");
    for (n, v) in [(7, 45), (12, 80), (22, 33)] {
        assert_eq!(back.sregs.get(n), Some(v), "S{n}");
    }
    assert_eq!(back.sregs.get(30), Some(7));

    // Loaded into a fresh modem they are live at once: numeric replies, no echo.
    let mut m2 = Modem::new(Config::new("X").unwrap());
    m2.load_settings(back);
    assert_eq!(run(&mut m2, "ATS7?", 0), "\r\n045\r\n0\r");
    assert_eq!(m2.tx_power_dbm(), 7);
    // And a blob made with E1 V1 comes back with them on.
    let on = Settings::from_blob(&blob_of(&Settings::defaults("X", AP_PASS))).unwrap();
    assert!(on.echo && on.verbose);
    // Every register in the table survives with its extreme values.
    let mut s = Settings::defaults("X", AP_PASS);
    for n in SRegs::NUMBERS {
        let max = (0..=u16::MAX).rev().find(|&v| s.sregs.clone().set(n, v));
        let max = max.unwrap();
        assert!(s.sregs.set(n, max));
        let back = Settings::from_blob(&blob_of(&s)).unwrap();
        assert_eq!(back.sregs.get(n), Some(max), "S{n}");
    }
}

#[test]
fn atz_restores_saved_settings() {
    let mut m = modem();
    ok(&mut m, "AT+FWD=AXIS,192.168.1.164,23");
    ok(&mut m, "ATS7=45");
    ok(&mut m, "AT&W");
    m.take_save_request();
    // Edits after the save...
    ok(&mut m, "AT+FWD=other,10.1.1.1,80");
    ok(&mut m, "AT+PUB=other");
    ok(&mut m, "ATS7=5");
    ok(&mut m, "AT+WAP=Changed,newpass123");
    ok(&mut m, "AT+WSTA=Home,password999");
    assert_eq!(m.settings().fwd_count(), 2);
    // ...are gone after ATZ, which brings the saved profile back.
    ok(&mut m, "ATZ");
    assert_eq!(m.settings().fwd_count(), 1);
    assert!(m.settings().forward("other").is_none());
    assert!(m.settings().published().is_none());
    assert_eq!(m.settings().sregs.get(7), Some(45));
    assert_eq!(m.settings().ap_ssid(), "AXISMODEM-1234");
    assert_eq!(m.settings().ap_pass(), AP_PASS);
    assert_eq!(m.settings().sta_ssid(), "");
    assert_eq!(run(&mut m, "ATS7?", 0), "\r\n045\r\n\r\nOK\r\n");
    assert!(!m.take_save_request(), "ATZ does not save");

    // No saved profile yet (never AT&W): ATZ goes to what was loaded at boot.
    let mut m = modem();
    ok(&mut m, "ATS7=5");
    ok(&mut m, "ATZ");
    assert_eq!(m.settings().sregs.get(7), Some(30));
}

#[test]
fn pwshow_line_follows_show_ap_pass() {
    let mut m = modem();
    let v = |m: &mut Modem| run(m, "AT&V", 0);
    assert!(m.show_ap_pass());
    assert!(v(&mut m).contains("\r\nPWSHOW 1\r\n"));
    // Saving with no client network keeps the password on show.
    ok(&mut m, "AT&W");
    m.take_save_request();
    assert!(m.show_ap_pass() && v(&mut m).contains("\r\nPWSHOW 1\r\n"));
    // Saving once with a client network set hides it.
    ok(&mut m, &format!("AT+WSTA=HomeNet,{STA_PASS}"));
    assert!(
        v(&mut m).contains("\r\nPWSHOW 1\r\n"),
        "not before the save"
    );
    ok(&mut m, "AT&W");
    assert!(!m.show_ap_pass());
    let t = v(&mut m);
    assert!(
        t.contains("\r\nPWSHOW 0\r\n") && !t.contains("PWSHOW 1"),
        "{t:?}"
    );
    // It stays hidden after ATZ (a hidden password never comes back).
    ok(&mut m, "ATZ");
    assert!(!m.show_ap_pass() && v(&mut m).contains("\r\nPWSHOW 0\r\n"));

    // The other way to hide it: a device joins the hotspot.
    let mut m = modem();
    assert!(v(&mut m).contains("PWSHOW 1"));
    m.note_ap_client_joined();
    assert!(!m.show_ap_pass());
    assert!(v(&mut m).contains("\r\nPWSHOW 0\r\n"));
    // The saved copy says hidden too (so a reset does not show it again).
    ok(&mut m, "ATZ");
    assert!(v(&mut m).contains("\r\nPWSHOW 0\r\n"));
    // PWSHOW sits with the Wi-Fi lines, after WIFI and before PUB.
    let t = v(&mut m);
    let (w, p, u) = (
        t.find("WIFI AP").unwrap(),
        t.find("PWSHOW").unwrap(),
        t.find("PUB ").unwrap(),
    );
    assert!(w < p && p < u, "{t:?}");
}

#[test]
fn note_ap_client_joined_requests_save() {
    let mut m = modem();
    assert!(!m.take_save_request());
    m.note_ap_client_joined();
    assert!(m.take_save_request(), "the platform must persist the hide");
    assert!(!m.take_save_request());
    // What is saved says the password is hidden.
    let blob = blob_of(m.settings());
    let s = Settings::from_blob(&blob).unwrap();
    assert!(!s.show_ap_pass);
    // Another join changes nothing more but asking again is harmless.
    m.note_ap_client_joined();
    assert!(!m.show_ap_pass());
}

#[test]
fn wap_user_set_hides_password() {
    let mut m = modem();
    assert!(m.show_ap_pass());
    // A refused AT+WAP changes nothing.
    error(&mut m, "AT+WAP=Net,short");
    assert!(m.show_ap_pass());
    // The on/off switch is not a password the user chose.
    ok(&mut m, "AT+WAP=0");
    assert!(m.show_ap_pass());
    // A name and password the user typed are not shown on the OLED.
    ok(&mut m, "AT+WAP=MyHotspot,my-own-pass-1");
    assert!(!m.show_ap_pass());
    assert!(run(&mut m, "AT&V", 0).contains("\r\nPWSHOW 0\r\n"));
    // ATZ goes back to the saved generated password: it stays hidden.
    ok(&mut m, "ATZ");
    assert_eq!(m.settings().ap_pass(), AP_PASS);
    assert!(!m.show_ap_pass());
    // A later AT&W saves the hidden flag with the user's password.
    ok(&mut m, "AT+WAP=MyHotspot,my-own-pass-1");
    ok(&mut m, "AT&W");
    let s = Settings::from_blob(&blob_of(m.saved_settings())).unwrap();
    assert!(!s.show_ap_pass);
    assert_eq!(s.ap_pass(), "my-own-pass-1");
}

#[test]
fn hotspot_join_saves_only_flag() {
    let mut m = modem();
    // Unsaved live changes: a trial auto-answer, a client network, a publish.
    ok(&mut m, "ATS0=1");
    ok(&mut m, &format!("AT+WSTA=HomeNet,{STA_PASS}"));
    ok(&mut m, "AT+FWD=a,h.example,23");
    ok(&mut m, "AT+PUB=a");
    assert!(!m.take_save_request());
    m.note_ap_client_joined();
    assert!(m.take_save_request(), "the cleared flag is persisted");
    // What gets written is the saved profile with only the flag changed.
    let s = Settings::from_blob(&blob_of(m.saved_settings())).unwrap();
    assert!(!s.show_ap_pass);
    assert_eq!(s.sregs.get(0), Some(0), "S0 was never saved");
    assert_eq!(s.sta_ssid(), "");
    assert_eq!(s.fwd_count(), 0);
    assert!(s.published().is_none());
    assert_eq!(s.ap_pass(), AP_PASS);
    // The live settings keep the unsaved work, and the live flag agrees.
    assert_eq!(m.settings().sregs.get(0), Some(1));
    assert_eq!(m.settings().sta_ssid(), "HomeNet");
    assert!(!m.show_ap_pass());
    // The flag was already 0: a second join asks for nothing.
    m.note_ap_client_joined();
    assert!(!m.take_save_request());
    // ATZ restores the saved copy and the password stays hidden.
    ok(&mut m, "ATZ");
    assert_eq!(m.settings().sregs.get(0), Some(0));
    assert!(!m.show_ap_pass());
}

#[test]
fn heap_min_line_in_atv() {
    let mut m = modem();
    assert!(
        !run(&mut m, "AT&V", 0).contains("HEAP MIN"),
        "nothing until the platform reports"
    );
    m.set_heap_min(123_456);
    let t = run(&mut m, "AT&V", 0);
    assert!(t.contains("\r\nHEAP MIN 123456\r\n"), "{t:?}");
    m.set_stack_free(7340);
    let t = run(&mut m, "AT&V", 0);
    let (s, h) = (t.find("STACK FREE").unwrap(), t.find("HEAP MIN").unwrap());
    assert!(s < h, "HEAP MIN follows STACK FREE: {t:?}");
    m.set_heap_min(0);
    assert!(run(&mut m, "AT&V", 0).contains("\r\nHEAP MIN 0\r\n"));
}

#[test]
fn wifi_changed_is_set_by_wap_wsta_atz_and_load() {
    let mut m = modem();
    assert!(!m.take_wifi_changed());
    for (line, changes) in [
        ("AT+WAP=0", true),
        ("AT+WAP=1", true),
        ("AT+WAP=Net,password1", true),
        ("AT+WSTA=Home,password1", true),
        ("AT+WSTA=0", true),
        ("ATZ", true),
        // Refused or unrelated commands do not.
        ("AT+WAP=Net,short", false),
        ("AT+WSTA=Home,short", false),
        ("AT+WAP?", false),
        ("AT+WSTA?", false),
        ("AT+FWD=a,h.example,23", false),
        ("ATS7=40", false),
        ("AT&W", false),
    ] {
        let _ = run(&mut m, line, 0);
        assert_eq!(m.take_wifi_changed(), changes, "{line}");
        assert!(!m.take_wifi_changed(), "{line}: taken once");
    }
    m.load_settings(Settings::defaults("X", AP_PASS));
    assert!(m.take_wifi_changed(), "load");
}

#[test]
fn atv_lines_stack_free_and_simnet() {
    let mut m = modem();
    let t = run(&mut m, "AT&V", 0);
    assert!(
        !t.contains("STACK FREE"),
        "nothing until the platform reports"
    );
    assert!(!t.contains("SIMNET"));
    m.set_stack_free(7340);
    assert!(run(&mut m, "AT&V", 0).contains("\r\nSTACK FREE 7340\r\n"));
    m.set_stack_free(0);
    assert!(run(&mut m, "AT&V", 0).contains("\r\nSTACK FREE 0\r\n"));
    // The Wi-Fi group: WIFI, PWSHOW, PUB, FWD, STACK FREE in that order.
    let t = run(&mut m, "AT&V", 0);
    let pos: Vec<usize> = [
        "WIFI AP ON AXISMODEM-1234  STA OFF",
        "PWSHOW",
        "PUB NONE",
        "FWD 0",
        "STACK FREE",
    ]
    .iter()
    .map(|k| t.find(k).unwrap_or_else(|| panic!("{k} missing: {t:?}")))
    .collect();
    assert!(pos.windows(2).all(|w| w[0] < w[1]), "{t:?}");

    // RADIO SIMNET lines.
    ok(&mut m, "AT+SIMNET=192.168.1.50");
    assert_eq!(m.take_simnet_request(), Some(Some([192, 168, 1, 50])));
    assert_eq!(m.take_simnet_request(), None, "taken once");
    assert_eq!(m.simnet(), Some([192, 168, 1, 50]));
    let t = run(&mut m, "AT&V", 0);
    assert!(t.contains("\r\nRADIO SIMNET 192.168.1.50\r\n"), "{t:?}");
    assert!(!t.contains("NET FAIL"));
    assert_eq!(
        run(&mut m, "AT+SIMNET?", 0),
        "\r\nSIMNET 192.168.1.50\r\n\r\nOK\r\n"
    );
    m.set_simnet_net_fail(true);
    let t = run(&mut m, "AT&V", 0);
    assert!(
        t.contains("\r\nRADIO SIMNET 192.168.1.50 NET FAIL\r\n"),
        "{t:?}"
    );
    ok(&mut m, "AT+SIMNET=0");
    assert_eq!(m.take_simnet_request(), Some(None));
    let t = run(&mut m, "AT&V", 0);
    assert!(!t.contains("SIMNET"), "{t:?}");
    assert_eq!(run(&mut m, "AT+SIMNET?", 0), "\r\nSIMNET OFF\r\n\r\nOK\r\n");
    // Refused forms.
    for bad in [
        "AT+SIMNET=0.0.0.0",
        "AT+SIMNET=1.2.3",
        "AT+SIMNET=1.2.3.256",
        "AT+SIMNET=host",
    ] {
        error(&mut m, bad);
    }
    assert_eq!(m.take_simnet_request(), None);
}
