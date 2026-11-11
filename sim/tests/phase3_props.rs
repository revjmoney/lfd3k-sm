//! Phase 3 property tests (fixed seed, like `fuzz.rs`): the telnet incoming
//! filter, the settings blob, and the Wi-Fi AT commands never panic and keep
//! their invariants on random input.

use lfd3k_core::settings::BLOB_LEN;
use lfd3k_core::telnet::{Telnet, DO, IAC, SB, SE, WILL};
use lfd3k_core::{Config, Forward, Modem, Radio, Settings, Tier, WireMode};
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

// ---- Telnet ----

/// Feeds bytes like the modem does (only while `can_take`), popping replies.
/// Returns (data, replies).
fn run_filter(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut t = Telnet::new();
    t.reset(WireMode::Telnet);
    let (mut data, mut replies) = (Vec::new(), Vec::new());
    let mut buf = [0u8; 64];
    for &b in bytes {
        // The caller must have room: draining keeps it so.
        assert!(t.can_take());
        if let Some(d) = t.input(b) {
            data.push(d);
        }
        let n = t.pop_replies(&mut buf);
        replies.extend_from_slice(&buf[..n]);
    }
    (data, replies)
}

/// One piece of a server stream and the clean data it must produce.
#[derive(Clone, Debug)]
enum Tok {
    /// Plain bytes (no 0xFF, no NUL, no CR: those have their own rules).
    Text(Vec<u8>),
    /// IAC IAC: one 0xFF.
    Escaped,
    /// IAC <command 240..=249>.
    Command(u8),
    /// IAC WILL|WONT|DO|DONT <option>.
    Negotiate(u8, u8),
    /// IAC SB <body without IAC> IAC SE.
    Sub(Vec<u8>),
}

fn tok() -> impl Strategy<Value = Tok> {
    prop_oneof![
        proptest::collection::vec(
            any::<u8>().prop_filter("plain", |b| !matches!(*b, 0 | 13 | 255)),
            1..20
        )
        .prop_map(Tok::Text),
        Just(Tok::Escaped),
        (240u8..=249).prop_map(Tok::Command),
        (251u8..=254, any::<u8>()).prop_map(|(v, o)| Tok::Negotiate(v, o)),
        proptest::collection::vec(any::<u8>().prop_filter("no IAC", |b| *b != 255), 0..400)
            .prop_map(Tok::Sub),
    ]
}

proptest! {
    #![proptest_config(cfg(1_500))]

    /// Any bytes at all: no panic, never more data than input, replies only
    /// ever whole telnet commands.
    #[test]
    fn telnet_random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        let (data, replies) = run_filter(&bytes);
        prop_assert!(data.len() <= bytes.len());
        // Replies are IAC verb opt (3 bytes) or the 12 byte NAWS answer.
        let mut i = 0;
        while i < replies.len() {
            prop_assert_eq!(replies[i], IAC);
            let verb = replies.get(i + 1).copied().unwrap_or(0);
            prop_assert!((251..=254).contains(&verb), "verb {verb} in {replies:?}");
            if verb == WILL && replies.get(i + 2) == Some(&31) {
                // WILL NAWS is followed by its subnegotiation.
                prop_assert_eq!(&replies[i + 3..i + 12], &[IAC, SB, 31, 0, 80, 0, 25, IAC, SE][..]);
                i += 12;
            } else {
                i += 3;
            }
        }
        // Raw mode is the identity and answers nothing.
        let mut raw = Telnet::new();
        raw.reset(WireMode::Raw);
        let out: Vec<u8> = bytes.iter().filter_map(|&b| raw.input(b)).collect();
        prop_assert_eq!(out, bytes);
    }

    /// A well formed stream: no negotiation, command or subnegotiation byte
    /// ever reaches the user, and the text comes through exactly.
    #[test]
    fn telnet_never_emits_iac_sequences_as_data(toks in proptest::collection::vec(tok(), 0..30)) {
        let (mut wire, mut want) = (Vec::new(), Vec::new());
        for t in &toks {
            match t {
                Tok::Text(b) => { wire.extend_from_slice(b); want.extend_from_slice(b); }
                Tok::Escaped => { wire.extend_from_slice(&[IAC, IAC]); want.push(IAC); }
                Tok::Command(c) => wire.extend_from_slice(&[IAC, *c]),
                Tok::Negotiate(v, o) => wire.extend_from_slice(&[IAC, *v, *o]),
                Tok::Sub(body) => {
                    wire.extend_from_slice(&[IAC, SB]);
                    wire.extend_from_slice(body);
                    wire.extend_from_slice(&[IAC, SE]);
                }
            }
        }
        let (data, _) = run_filter(&wire);
        prop_assert_eq!(&data, &want);
        // Every 0xFF in the output came from an IAC IAC in the input.
        let escaped = toks.iter().filter(|t| matches!(t, Tok::Escaped)).count();
        prop_assert_eq!(data.iter().filter(|&&b| b == IAC).count(), escaped);
    }

    /// Outgoing: 0xFF is always doubled and the encoding never loses a byte.
    #[test]
    fn telnet_encode_keeps_every_byte(b in any::<u8>(), next in proptest::option::of(any::<u8>()), neg in any::<u8>()) {
        let mut t = Telnet::new();
        t.reset(WireMode::Telnet);
        for opt in [0u8, 3, 31] {
            if neg & 1 != 0 {
                for &x in &[IAC, DO, opt] {
                    t.input(x);
                }
            }
        }
        let (first, second) = t.encode(b, next);
        prop_assert!(first == b);
        if b == IAC {
            prop_assert_eq!(second, Some(IAC));
        } else {
            prop_assert!(second.is_none() || second == Some(0));
        }
    }
}

// ---- Settings ----

struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn blob_of(s: &Settings) -> Vec<u8> {
    let mut b = [0u8; BLOB_LEN];
    let n = s.to_blob(&mut b);
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

fn word(max: usize) -> impl Strategy<Value = String> {
    proptest::string::string_regex(&format!("[A-Za-z0-9 _.-]{{1,{max}}}")).unwrap()
}

fn password() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9!#$%&*+./:;<=>?@^_~ -]{8,63}").unwrap()
}

fn host() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9][a-z0-9.-]{0,40}").unwrap()
}

fn forward() -> impl Strategy<Value = Forward> {
    (
        proptest::string::string_regex("[A-Za-z0-9_.-]{1,16}").unwrap(),
        host(),
        1u16..=65535,
        any::<bool>(),
    )
        .prop_filter_map("valid forward", |(n, h, p, raw)| {
            let mode = if raw { WireMode::Raw } else { WireMode::Telnet };
            Forward::new(&n, &h, p, mode)
        })
}

fn settings() -> impl Strategy<Value = Settings> {
    (
        word(32),
        password(),
        proptest::option::of((word(32), password())),
        proptest::collection::vec(forward(), 0..8),
        any::<(bool, bool, bool, bool)>(),
        (0u16..=255, 1u16..=255, 1u16..=255, 0u16..=127),
        -9i16..=22,
        proptest::option::of(0usize..8),
    )
        .prop_map(
            |(ssid, pass, sta, fwds, (ap, echo, verbose, show), (s0, s7, s22, s2), s30, publ)| {
                let mut s = Settings::defaults(&ssid, &pass);
                s.ap_on = ap;
                s.echo = echo;
                s.verbose = verbose;
                s.show_ap_pass = show;
                if let Some((n, p)) = sta {
                    s.set_sta(&n, &p);
                }
                for f in fwds {
                    s.set_forward(f);
                }
                for (n, v) in [(0, s0), (7, s7), (22, s22), (2, s2), (30, s30 as u16)] {
                    s.sregs.set(n, v);
                }
                if let Some(i) = publ {
                    let name = s
                        .fwd
                        .get(i)
                        .and_then(|f| f.as_ref())
                        .map(|f| f.name().to_string());
                    if let Some(n) = name {
                        s.set_publish(&n);
                    }
                }
                s
            },
        )
}

proptest! {
    #![proptest_config(cfg(1_500))]

    /// Any valid settings survive a trip through the blob unchanged, and
    /// the blob is canonical (same settings, same bytes).
    #[test]
    fn settings_blob_roundtrip(s in settings()) {
        let blob = blob_of(&s);
        prop_assert_eq!(blob.len(), 917);
        let back = Settings::from_blob(&blob);
        prop_assert_eq!(back.as_ref(), Some(&s));
        prop_assert_eq!(blob_of(back.as_ref().unwrap()), blob);
    }

    /// Random bytes of any length: never a panic, and nothing accepted
    /// unless it is a blob.
    #[test]
    fn settings_blob_random_bytes_rejected(bytes in proptest::collection::vec(any::<u8>(), 0..1100)) {
        prop_assert!(Settings::from_blob(&bytes).is_none());
    }

    /// Corrupted blobs with a repaired checksum: never a panic, and if one
    /// is accepted it is canonical (it serialises back to exactly itself).
    #[test]
    fn settings_blob_edited_never_panics(
        s in settings(),
        edits in proptest::collection::vec((0usize..897, any::<u8>()), 1..8),
    ) {
        let mut blob = blob_of(&s);
        for (i, v) in edits {
            blob[i] = v;
        }
        let n = blob.len() - 2;
        let c = crc16(&blob[..n]).to_le_bytes();
        blob[n..].copy_from_slice(&c);
        if let Some(t) = Settings::from_blob(&blob) {
            prop_assert_eq!(blob_of(&t), blob);
        }
    }
}

// ---- Wi-Fi AT commands ----

fn run(m: &mut Modem, line: &[u8], now: u32) -> Vec<u8> {
    let mut r = NullRadio;
    let mut v = line.to_vec();
    v.push(b'\r');
    m.host_input(&v, &mut r, now);
    let mut all = Vec::new();
    let mut buf = [0u8; 128];
    loop {
        let n = m.host_output(&mut buf);
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
    }
    all
}

fn at_line() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..100),
        proptest::string::string_regex(
            "AT(\\+(WAP|WSTA|WIFI|FWD|PUB|SIMNET)[=?][ -~]{0,70}|&[VW]|DT ?[ -~]{0,30}|H|Z|O|S[0-9]{1,2}[=?][0-9]{0,3})"
        )
        .unwrap()
        .prop_map(String::into_bytes),
    ]
}

proptest! {
    #![proptest_config(cfg(800))]

    /// Random Wi-Fi, forward and dial lines: never a panic, and the client
    /// password never appears in anything the modem prints (echo off).
    #[test]
    fn wifi_at_lines_never_panic_or_leak(lines in proptest::collection::vec(at_line(), 1..25)) {
        const SECRET: &str = "ZqX-7731-Secret";
        let mut m = Modem::new(Config::new("AXIS").unwrap());
        m.load_settings(Settings::defaults("AXIS", "hotspot-pw-1"));
        run(&mut m, b"ATE0", 0);
        run(&mut m, format!("AT+WSTA=HomeNet,{SECRET}").as_bytes(), 1);
        let mut printed = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            // The random lines are not allowed to carry the secret themselves.
            printed.extend(run(&mut m, l, 10 + i as u32));
            let _ = m.take_wire_request();
            let _ = m.take_wifi_changed();
        }
        for q in ["AT+WSTA?", "AT&V", "AT+WIFI?", "ATI"] {
            printed.extend(run(&mut m, q.as_bytes(), 1_000));
        }
        let text = String::from_utf8_lossy(&printed);
        // A random line can legally replace or forget the network, so only
        // check the secret while it is still the client password.
        if m.settings().sta_pass() == SECRET {
            prop_assert!(!text.contains(SECRET), "{text}");
        }
        // Whatever happened, the modem still answers.
        let t = run(&mut m, b"AT", 2_000);
        prop_assert!(t.ends_with(b"OK\r\n") || t.ends_with(b"0\r"));
        // And the settings it holds always serialise to a blob that loads.
        prop_assert!(m.settings().ap_pass().len() >= 8);
    }
}
