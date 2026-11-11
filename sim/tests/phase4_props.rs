//! Phase 4 property tests (fixed seed, like `fuzz.rs` and `phase3_props.rs`):
//! the LZ codec, the directory and its blob, discovery frames and the menu
//! never panic and keep their invariants on random and hostile input.

use lfd3k_core::discovery::{
    beacon_payload, parse_beacon, parse_scan_req, parse_scan_resp, scan_req_payload,
    scan_resp_payload, valid_station, Beacon, ScanReq, ScanResp, DISC_GAP_MS,
};
use lfd3k_core::frame::{decode, encode, FrameType, Header};
use lfd3k_core::lz::{compress, decompress, MIN_GAIN, PRESET};
use lfd3k_core::settings::BeaconMode;
use lfd3k_core::{Config, Directory, Modem, Radio, RxInfo, Settings, Tier, DIR_BLOB_LEN, DIR_MAX};
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};

fn cfg(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0x1FD3_0420),
        ..ProptestConfig::default()
    }
}

struct Rec {
    now: u32,
    sent: Vec<(u32, Vec<u8>)>,
}

impl Radio for Rec {
    fn send(&mut self, _tier: Tier, frame: &[u8]) {
        self.sent.push((self.now, frame.to_vec()));
    }
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn info(snr_x4: i16) -> RxInfo {
    RxInfo {
        tier: Tier::Sf12,
        rssi_dbm: -100,
        snr_x4,
    }
}

// ---- LZ ----

/// Input that compresses: pieces of the preset, runs, repeats of earlier
/// bytes and some noise, up to one chunk.
fn compressible() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        (0..PRESET.len() - 4, 3usize..60).prop_map(|(at, n)| {
            let end = (at + n).min(PRESET.len());
            PRESET[at..end].to_vec()
        }),
        (any::<u8>(), 3usize..60).prop_map(|(b, n)| vec![b; n]),
        proptest::collection::vec(any::<u8>(), 1..12),
        proptest::collection::vec(
            prop_oneof![Just(0x1Bu8), Just(b'['), Just(b'1'), Just(b';'), Just(b'm')],
            2..20
        ),
    ];
    proptest::collection::vec(piece, 1..12).prop_map(|ps| {
        let mut v: Vec<u8> = ps.concat();
        v.truncate(200);
        v
    })
}

proptest! {
    #![proptest_config(cfg(3_000))]

    /// Any bytes at all, into any output size: no panic, and when it says
    /// Ok(n) then n fits the output.
    #[test]
    fn lz_decode_never_panics(
        raw in proptest::collection::vec(any::<u8>(), 0..400),
        out_len in 0usize..300,
    ) {
        let mut out = vec![0u8; out_len];
        if let Ok(n) = decompress(&raw, &mut out) { prop_assert!(n <= out_len) }
    }

    /// Token streams that look right (literal runs and matches with every
    /// distance), so the match paths are reached: still no panic, never a
    /// read before the preset, and a good result is exactly what was written.
    #[test]
    fn lz_decode_token_streams_never_panic(
        toks in proptest::collection::vec(
            prop_oneof![
                proptest::collection::vec(any::<u8>(), 1..10)
                    .prop_map(|d| { let mut v = vec![(d.len() - 1) as u8]; v.extend(d); v }),
                (0u8..32, 0u16..1024).prop_map(|(m, d)| vec![0x80 | (m << 2) | ((d >> 8) as u8 & 3), d as u8]),
                any::<u8>().prop_map(|b| vec![b]),
            ],
            0..40,
        ),
        cut in any::<prop::sample::Index>(),
        out_len in 0usize..260,
    ) {
        let mut raw: Vec<u8> = toks.concat();
        if !raw.is_empty() && cut.index(3) == 0 {
            let k = cut.index(raw.len());
            raw.truncate(k);
        }
        let mut out = vec![0u8; out_len];
        if let Ok(n) = decompress(&raw, &mut out) {
            prop_assert!(n <= out_len);
        }
    }

    /// What `compress` returns decodes to the input exactly, is at least 8
    /// bytes shorter, and is the same every time.
    #[test]
    fn lz_roundtrip(input in compressible()) {
        let mut out = [0u8; 400];
        if let Some(n) = compress(&input, &mut out) {
            prop_assert!(n + MIN_GAIN <= input.len());
            let mut back = [0u8; 400];
            let k = decompress(&out[..n], &mut back).expect("own output decodes");
            prop_assert_eq!(&back[..k], &input[..]);
            let mut again = [0u8; 400];
            let m = compress(&input, &mut again).expect("again");
            prop_assert_eq!(&again[..m], &out[..n]);
        }
    }

    /// Long runs and pieces of the preset are always worth it: Some, and
    /// round trips (so the property above is not vacuous).
    #[test]
    fn lz_roundtrip_always_some_for_runs(b in any::<u8>(), n in 30usize..=200, at in 0usize..400, k in 40usize..120) {
        let run = vec![b; n];
        let mut out = [0u8; 400];
        let c = compress(&run, &mut out).expect("a run compresses");
        let mut back = [0u8; 400];
        let m = decompress(&out[..c], &mut back).expect("decodes");
        prop_assert_eq!(&back[..m], &run[..]);
        let end = (at + k).min(PRESET.len());
        let piece = &PRESET[at.min(PRESET.len() - 41)..end.max(at.min(PRESET.len() - 41) + 41).min(PRESET.len())];
        let c = compress(piece, &mut out).expect("a preset piece compresses");
        let m = decompress(&out[..c], &mut back).expect("decodes");
        prop_assert_eq!(&back[..m], piece);
    }

    /// Random noise: whatever happens it is a clean round trip or None.
    #[test]
    fn lz_roundtrip_noise(input in proptest::collection::vec(any::<u8>(), 0..=200)) {
        let mut out = [0u8; 400];
        if let Some(n) = compress(&input, &mut out) {
            let mut back = [0u8; 400];
            let k = decompress(&out[..n], &mut back).expect("decodes");
            prop_assert_eq!(&back[..k], &input[..]);
        }
    }
}

// ---- Directory ----

fn blob(d: &Directory) -> Vec<u8> {
    let mut b = [0u8; DIR_BLOB_LEN];
    let n = d.to_blob(&mut b);
    b[..n].to_vec()
}

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

/// What a directory that was accepted from a blob must satisfy.
fn check_loaded(d: &Directory) -> Result<(), TestCaseError> {
    prop_assert!(d.len() <= DIR_MAX);
    let names: Vec<String> = d.iter().map(|e| e.name().to_ascii_lowercase()).collect();
    for (i, n) in names.iter().enumerate() {
        prop_assert!(valid_station(n), "bad name {n:?}");
        prop_assert!(!names[i + 1..].contains(n), "duplicate {n:?}");
    }
    prop_assert!(d.iter().filter(|e| !e.pinned).count() <= 8);
    // It saves and loads again to the same thing.
    let b = blob(d);
    let again = Directory::from_blob(&b).expect("a loaded directory saves a blob that loads");
    prop_assert_eq!(blob(&again), b);
    Ok(())
}

fn some_directory(k: usize) -> Directory {
    let mut d = Directory::new();
    for i in 0..k {
        if i % 3 == 0 {
            d.add_pinned(&format!("PIN{i}"));
        } else {
            d.heard(
                &format!("HEARD{i}"),
                (i % 8) as u8,
                info((i as i16) * 5 - 40),
                i as u32,
            );
        }
    }
    d
}

proptest! {
    #![proptest_config(cfg(3_000))]

    /// Any bytes: refused, or a valid directory. A refused blob leaves the
    /// live directory empty, never half filled, even when it held entries.
    #[test]
    fn dir_blob_in_place_load_rejects_garbage(
        raw in proptest::collection::vec(any::<u8>(), 0..1100),
        preload in 0usize..20,
    ) {
        let mut d = some_directory(preload);
        let ok = d.load_blob(&raw);
        if ok {
            check_loaded(&d)?;
        } else {
            prop_assert!(d.is_empty(), "a rejected blob left {} entries", d.len());
        }
        prop_assert_eq!(Directory::from_blob(&raw).is_some(), ok);
    }

    /// Edits of a valid blob, with and without a repaired checksum (so the
    /// field checks, not only the CRC, are reached).
    #[test]
    fn dir_blob_edited_never_panics(
        k in 0usize..30,
        edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..6),
        fix in any::<bool>(),
        cut in proptest::option::of(any::<prop::sample::Index>()),
        preload in 0usize..12,
    ) {
        let mut b = blob(&some_directory(k));
        for (i, v) in &edits {
            let at = i.index(b.len());
            b[at] = *v;
        }
        if let Some(c) = cut {
            let n = c.index(b.len() + 1);
            b.truncate(n);
        }
        if fix && b.len() >= 8 {
            let n = b.len() - 2;
            let c = crc16(&b[..n]).to_le_bytes();
            b[n..].copy_from_slice(&c);
        }
        let mut d = some_directory(preload);
        if d.load_blob(&b) {
            check_loaded(&d)?;
        } else {
            prop_assert!(d.is_empty());
        }
    }

    /// Random heard, pin, remove and clear: the directory never grows past
    /// 32, a pinned entry is never evicted by hearing, and names stay unique.
    #[test]
    fn directory_ops_keep_invariants(
        ops in proptest::collection::vec((0u8..6, 0u8..48), 0..300),
    ) {
        let mut d = Directory::new();
        let mut pinned: Vec<String> = Vec::new();
        for (t, n) in ops {
            let name = format!("N{n}");
            match t {
                0..=2 => {
                    d.heard(&name, 1, info(30), 100);
                }
                3 => {
                    if d.add_pinned(&name) && !pinned.contains(&name) {
                        pinned.push(name);
                    }
                }
                4 => {
                    if d.remove(&name) {
                        pinned.retain(|p| *p != name);
                    }
                }
                _ => {
                    d.clear_unpinned();
                }
            }
            prop_assert!(d.len() <= DIR_MAX);
            for p in &pinned {
                prop_assert!(d.get(p).is_some_and(|e| e.pinned), "pin {p} lost");
            }
        }
        let names: Vec<&str> = d.iter().map(|e| e.name()).collect();
        let mut u = names.clone();
        u.sort_unstable();
        u.dedup();
        prop_assert_eq!(u.len(), names.len());
    }
}

// ---- Discovery frames ----

fn station() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("OWN".to_string()),
        Just("own".to_string()),
        "[A-Z0-9]{1,16}",
        "[ -~]{1,16}",
    ]
}

proptest! {
    #![proptest_config(cfg(2_000))]

    /// The payload parsers take any bytes without panicking, and what they
    /// accept encodes back to the same bytes.
    #[test]
    fn discovery_payloads_roundtrip_and_never_panic(
        raw in proptest::collection::vec(any::<u8>(), 0..40),
        name in station(),
        a in any::<u8>(), b in any::<u8>(), snr in any::<i16>(),
    ) {
        let mut out = [0u8; 40];
        if let Some(x) = parse_beacon(&raw) {
            let n = beacon_payload(&x, &mut out).expect("a parsed beacon encodes");
            prop_assert_eq!(&out[..n], &raw[..]);
        }
        if let Some(x) = parse_scan_req(&raw) {
            let n = scan_req_payload(&x, &mut out).expect("encodes");
            prop_assert_eq!(&out[..n], &raw[..]);
        }
        if let Some(x) = parse_scan_resp(&raw) {
            let n = scan_resp_payload(&x, &mut out).expect("encodes");
            prop_assert_eq!(&out[..n], &raw[..]);
        }
        if valid_station(&name) {
            let n = beacon_payload(&Beacon { name: &name, services: a, heard: b, time_quality: 0 }, &mut out).expect("valid name");
            let p = parse_beacon(&out[..n]).expect("parses");
            prop_assert_eq!((p.name, p.services, p.heard), (name.as_str(), a, b));
            let n = scan_resp_payload(&ScanResp { name: &name, services: a, snr_x4: snr, scan_id: b }, &mut out).expect("valid name");
            let p = parse_scan_resp(&out[..n]).expect("parses");
            prop_assert_eq!((p.snr_x4, p.scan_id), (snr, b));
            let n = scan_req_payload(&ScanReq { name: &name, scan_id: a }, &mut out).expect("valid name");
            prop_assert_eq!(parse_scan_req(&out[..n]).map(|r| r.scan_id), Some(a));
        } else {
            let refused = beacon_payload(&Beacon { name: &name, services: 0, heard: 0, time_quality: 0 }, &mut out).is_none();
            prop_assert!(refused);
        }
    }
}

#[derive(Clone, Debug)]
enum Heard {
    Beacon(String, u8),
    ScanReq(String, u8),
    ScanResp(String, u8, i16, u8),
    CallReq(String, u8),
    /// A discovery frame with a random payload.
    Junk(u8, Vec<u8>),
}

fn heard() -> impl Strategy<Value = Heard> {
    prop_oneof![
        (station(), any::<u8>()).prop_map(|(n, s)| Heard::Beacon(n, s)),
        (station(), any::<u8>()).prop_map(|(n, i)| Heard::ScanReq(n, i)),
        (station(), any::<u8>(), any::<i16>(), any::<u8>())
            .prop_map(|(n, s, r, i)| Heard::ScanResp(n, s, r, i)),
        (station(), 1u8..=255).prop_map(|(n, i)| Heard::CallReq(n, i)),
        (12u8..=14, proptest::collection::vec(any::<u8>(), 0..30))
            .prop_map(|(t, p)| Heard::Junk(t, p)),
    ]
}

fn frame_of(h: &Heard) -> Vec<u8> {
    let mut p = [0u8; 64];
    let (ft, call_id, n) = match h {
        Heard::Beacon(n, s) => (
            FrameType::Beacon,
            0,
            beacon_payload(
                &Beacon {
                    name: n,
                    services: *s,
                    heard: 0,
                    time_quality: 0,
                },
                &mut p,
            ),
        ),
        Heard::ScanReq(n, i) => (
            FrameType::ScanReq,
            0,
            scan_req_payload(
                &ScanReq {
                    name: n,
                    scan_id: *i,
                },
                &mut p,
            ),
        ),
        Heard::ScanResp(n, s, r, i) => (
            FrameType::ScanResp,
            0,
            scan_resp_payload(
                &ScanResp {
                    name: n,
                    services: *s,
                    snr_x4: *r,
                    scan_id: *i,
                },
                &mut p,
            ),
        ),
        Heard::CallReq(n, id) => (
            FrameType::CallReq,
            *id,
            lfd3k_core::frame::call_req_payload(n, "OWN", &mut p).ok(),
        ),
        Heard::Junk(t, junk) => {
            let ft = FrameType::from_u8(*t).expect("12..=14");
            p[..junk.len()].copy_from_slice(junk);
            (ft, 0, Some(junk.len()))
        }
    };
    let n = n.unwrap_or(0);
    let hdr = Header {
        ftype: ft,
        flags: 0,
        call_id,
        seq: 0,
        ack: 0,
    };
    let mut out = [0u8; 255];
    let k = encode(&hdr, &p[..n], &mut out).expect("encode");
    out[..k].to_vec()
}

proptest! {
    #![proptest_config(cfg(400))]

    /// Any stream of discovery frames and calls, in any privacy mode, with
    /// any time between them: no panic; the directory stays within 32 and
    /// never holds our own name; no two discovery frames of ours are closer
    /// than 10 s; and a HIDDEN modem sends nothing unless a pinned name
    /// called it.
    #[test]
    fn discovery_frames_never_panic_and_keep_manners(
        mode in 0u8..3,
        pin in any::<bool>(),
        evs in proptest::collection::vec((heard(), 1u32..4_000, any::<i16>()), 0..80),
        scan_at in proptest::option::of(0usize..80),
    ) {
        let mut m = Modem::new(Config::new("OWN").expect("name"));
        let mut r = Rec { now: 0, sent: Vec::new() };
        let mut now = 0u32;
        let line = |m: &mut Modem, r: &mut Rec, now: u32, s: &str| {
            let mut t = s.as_bytes().to_vec();
            t.push(b'\r');
            m.host_input(&t, r, now);
        };
        line(&mut m, &mut r, now, ["AT+BEACON=ON", "AT+BEACON=QUIET", "AT+BEACON=HIDDEN"][usize::from(mode)]);
        if pin {
            line(&mut m, &mut r, now, "AT+DIR=+PINNED");
        }
        let mut buf = [0u8; 512];
        let mut allowed_hidden_reply = false;
        for (i, (ev, dt, snr)) in evs.iter().enumerate() {
            // Time passes in 1 s steps (and a bit), ticking as it goes.
            let end = now + dt;
            while now < end {
                now += (end - now).min(250);
                r.now = now;
                m.tick(&mut r, now);
                while m.host_output(&mut buf) > 0 {}
            }
            if scan_at == Some(i) {
                line(&mut m, &mut r, now, "AT+SCAN");
            }
            if mode == 2 && matches!(ev, Heard::CallReq(n, _) if n.eq_ignore_ascii_case("PINNED")) {
                allowed_hidden_reply = true;
            }
            let f = frame_of(ev);
            r.now = now;
            m.radio_rx(&f, info(*snr), &mut r, now);
            while m.host_output(&mut buf) > 0 {}
            prop_assert!(m.directory().len() <= DIR_MAX);
            prop_assert!(m.directory().get("OWN").is_none(), "own name in the directory");
        }
        for _ in 0..40 {
            now += 250;
            r.now = now;
            m.tick(&mut r, now);
            while m.host_output(&mut buf) > 0 {}
        }
        // Spacing of what discovery sent.
        let mut last: Option<u32> = None;
        for (t, b) in &r.sent {
            let f = decode(b).expect("everything we send is a valid frame");
            if matches!(f.header.ftype, FrameType::Beacon | FrameType::ScanReq | FrameType::ScanResp) {
                if let Some(l) = last {
                    prop_assert!(t - l >= DISC_GAP_MS, "{} ms between discovery frames", t - l);
                }
                last = Some(*t);
            }
        }
        if mode == 2 && !allowed_hidden_reply {
            prop_assert!(r.sent.is_empty(), "HIDDEN sent {} frames", r.sent.len());
        }
        if mode == 1 {
            prop_assert!(r.sent.iter().all(|(_, b)| decode(b).map(|f| f.header.ftype != FrameType::Beacon).unwrap_or(true)), "QUIET beaconed");
        }
        prop_assert_eq!(
            m.beacon_mode(),
            [BeaconMode::On, BeaconMode::Quiet, BeaconMode::Hidden][usize::from(mode)]
        );
    }
}

// ---- The menu ----

const AP: &str = "kq7Rmx2Lp9";
const STA: &str = "Rev-Home-Secret-99";

#[derive(Clone, Debug)]
enum Key {
    Bytes(Vec<u8>),
    Wait(u32),
}

fn key() -> impl Strategy<Value = Key> {
    prop_oneof![
        Just(Key::Bytes(b"\x1b[A".to_vec())),
        Just(Key::Bytes(b"\x1b[B".to_vec())),
        Just(Key::Bytes(b"\x1b[C".to_vec())),
        Just(Key::Bytes(b"\x1bOD".to_vec())),
        Just(Key::Bytes(b"\r".to_vec())),
        Just(Key::Bytes(b"\r\n".to_vec())),
        Just(Key::Bytes(b"\x1b".to_vec())),
        Just(Key::Bytes(b"\t".to_vec())),
        Just(Key::Bytes(b"\x7f".to_vec())),
        proptest::collection::vec(0x20u8..0x7F, 1..6).prop_map(Key::Bytes),
        proptest::collection::vec(any::<u8>(), 1..8).prop_map(Key::Bytes),
        (0u32..300).prop_map(Key::Wait),
        proptest::collection::vec(prop_oneof![Just(b';'), Just(b'1'), Just(b' ')], 1..40).prop_map(
            |mut v| {
                let mut s = b"\x1b[".to_vec();
                s.append(&mut v);
                Key::Bytes(s)
            }
        ),
    ]
}

proptest! {
    #![proptest_config(cfg(300))]

    /// Any keys, any timing, any host speed: the menu never panics or takes
    /// the modem away (the keys are always taken and Esc gets out in a few
    /// presses), a password never reaches the host, and after leaving the AT
    /// parser answers.
    #[test]
    fn menu_random_keys_never_wedge_or_leak(
        keys in proptest::collection::vec(key(), 0..120),
        rate in prop_oneof![Just(0usize), 1usize..10],
    ) {
        let mut set = Settings::defaults("MENUTEST", AP);
        set.set_sta("HomeNet", STA);
        let mut m = Modem::new(Config::new("MENUTEST").expect("name"));
        m.load_settings(set);
        let mut r = Rec { now: 0, sent: Vec::new() };
        let mut out: Vec<u8> = Vec::new();
        let mut now = 1_000u32;
        let step = |m: &mut Modem, r: &mut Rec, now: &mut u32, ms: u32, out: &mut Vec<u8>| {
            for _ in 0..ms {
                *now += 1;
                r.now = *now;
                m.tick(r, *now);
                let mut buf = [0u8; 4096];
                let n = m.host_output(&mut buf[..if rate == 0 { 4096 } else { rate }]);
                out.extend_from_slice(&buf[..n]);
            }
        };
        let take = |m: &mut Modem, r: &mut Rec, now: u32, bytes: &[u8]| {
            let mut used = 0;
            let mut tries = 0;
            while used < bytes.len() && tries < 100_000 {
                let n = m.host_input(&bytes[used..], r, now);
                used += n;
                tries += 1;
                if n == 0 { break; }
            }
            used
        };
        prop_assert_eq!(take(&mut m, &mut r, now, b"AT+MENU\r"), 8);
        for k in &keys {
            match k {
                Key::Bytes(b) => {
                    let n = take(&mut m, &mut r, now, b);
                    prop_assert_eq!(n, b.len(), "the modem refused keys");
                    step(&mut m, &mut r, &mut now, 5, &mut out);
                }
                Key::Wait(ms) => step(&mut m, &mut r, &mut now, *ms, &mut out),
            }
        }
        // Esc gets out in a few presses, whatever state we are in.
        for _ in 0..8 {
            if !m.in_menu() {
                break;
            }
            take(&mut m, &mut r, now, b"\x1b");
            step(&mut m, &mut r, &mut now, 200, &mut out);
        }
        prop_assert!(!m.in_menu(), "Esc did not leave");
        step(&mut m, &mut r, &mut now, if rate == 0 { 50 } else { 6_000 }, &mut out);
        let text = String::from_utf8_lossy(&out).into_owned();
        prop_assert!(!text.contains(AP) && !text.contains(STA), "a password reached the host");
        // The modem is a modem again.
        let mark = out.len();
        take(&mut m, &mut r, now, b"\rAT\r");
        step(&mut m, &mut r, &mut now, if rate == 0 { 50 } else { 600 }, &mut out);
        prop_assert!(out[mark..].ends_with(b"\r\nOK\r\n") || out[mark..].ends_with(b"\r\nERROR\r\nOK\r\n") || String::from_utf8_lossy(&out[mark..]).contains("OK"));
    }
}
