//! Task 9 property tests for voice (proptest lives in the sim crate; core
//! has no dev dependencies): type 15 frames and their bodies never panic
//! the decoders or a modem, COBS framing round trips and survives garbage,
//! and a modem's voice host protocol and an app's engine take any bytes.
//! Fixed seeds (ChaCha), so a run is the same everywhere.

use lfd3k_core::frame::{self, FrameType, Header};
use lfd3k_core::voice::call::{parse_answer, parse_req, split_train, tierset_token};
use lfd3k_core::voice::frames::{parse_voice_beacon, parse_voice_scan_resp};
use lfd3k_core::voice::host::{cobs_decode, cobs_encode, msg, VHP_FRAME_MAX};
use lfd3k_core::voice::{decode_voice, VSub};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_voice::{Engine, Identity};
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};

fn cfg(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0x1FD3_0915),
        ..ProptestConfig::default()
    }
}

struct Rec;

impl Radio for Rec {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn info() -> RxInfo {
    RxInfo {
        tier: Tier::Sf12,
        rssi_dbm: -90,
        snr_x4: 20,
    }
}

/// Run every parser that applies to a decoded type 15 frame.
fn parse_all(f: &[u8]) {
    let _ = frame::decode(f);
    if let Some((sub, _, p)) = decode_voice(f) {
        match sub {
            VSub::Beacon => {
                if let Some(b) = parse_voice_beacon(p) {
                    assert!(b.items.count() <= 4);
                }
            }
            VSub::ScanResp => {
                if let Some((_, items)) = parse_voice_scan_resp(p) {
                    assert!(items.count() <= 4);
                }
            }
            VSub::CallReq => {
                let _ = parse_req(p);
            }
            VSub::Answer => {
                let _ = parse_answer(p);
            }
            _ => {}
        }
    }
    let _ = split_train(f);
    let _ = tierset_token(f);
}

/// A type 15 frame with a sub-type 0 to 8 (7 and 8 unknown) and any body.
fn voice_frame() -> impl Strategy<Value = Vec<u8>> {
    (
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
        0u8..9,
        proptest::collection::vec(any::<u8>(), 0..250),
    )
        .prop_map(|(call, seq, ack, sub, body)| {
            let mut f = vec![0xD1, 0xF0, call, seq, ack, sub];
            f.extend(body);
            f.truncate(255);
            f
        })
}

proptest! {
    #![proptest_config(cfg(10_000))]

    #[test]
    fn voice_decode_never_panics(
        f in prop_oneof![voice_frame(), proptest::collection::vec(any::<u8>(), 0..300)],
        reg in any::<bool>(),
    ) {
        parse_all(&f);
        // And a modem hearing it (idle, or with a voice name registered,
        // so a request to that name would ring) never panics either.
        let mut m = Modem::new(Config::new("ALPHA").unwrap());
        let mut r = Rec;
        m.tick(&mut r, 10);
        if reg {
            m.voice_open(1, 10);
            m.voice_register(1, "BOB", "PHONE", 10);
        }
        m.radio_rx(&f, info(), &mut r, 20);
        m.tick(&mut r, 30);
        let mut buf = [0u8; 512];
        while m.voice_output(1, &mut buf) > 0 {}
        while m.host_output(&mut buf) > 0 {}
    }
}

proptest! {
    #![proptest_config(cfg(2_000))]

    #[test]
    fn vhp_cobs_roundtrip_and_garbage(
        frame in proptest::collection::vec(any::<u8>(), 1..=VHP_FRAME_MAX),
        garbage in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        // Round trip: no zero inside, one zero at the end, same bytes back.
        let mut w = [0u8; 300];
        let n = cobs_encode(&frame, &mut w).unwrap();
        prop_assert_eq!(w[n - 1], 0);
        prop_assert!(!w[..n - 1].contains(&0));
        prop_assert!(n <= frame.len() + frame.len() / 254 + 2);
        let mut back = [0u8; 300];
        let k = cobs_decode(&w[..n - 1], &mut back).unwrap();
        prop_assert_eq!(&back[..k], &frame[..]);
        // Too small an output is refused, not overrun.
        let mut tiny = [0u8; 4];
        if n > 4 {
            prop_assert!(cobs_encode(&frame, &mut tiny).is_none());
        }
        // Garbage never panics the decoder, and what it returns fits.
        for part in garbage.split(|&b| b == 0) {
            if let Some(k) = cobs_decode(part, &mut back) {
                prop_assert!(k <= back.len());
            }
        }
        // A modem's session takes garbage (then a good frame) and stays sane.
        let mut m = Modem::new(Config::new("ALPHA").unwrap());
        let mut r = Rec;
        m.tick(&mut r, 10);
        m.voice_open(1, 10);
        let took = m.voice_input(1, &garbage, &mut r, 20);
        prop_assert!(took <= garbage.len());
        let mut hello = [0u8; 8];
        let h = cobs_encode(&[msg::HELLO, 1], &mut hello).unwrap();
        let mut good = vec![0u8];
        good.extend_from_slice(&hello[..h]);
        m.voice_input(1, &good, &mut r, 30);
        let mut out = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = m.voice_output(1, &mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        let frames: Vec<Vec<u8>> = out
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| cobs_decode(p, &mut back).map(|k| back[..k].to_vec()))
            .collect();
        prop_assert!(frames.iter().any(|f| f.first() == Some(&msg::HELLO_ACK)), "HelloAck after the garbage");
        // An app's engine takes any modem bytes.
        let mut e = Engine::new(Identity::generate("REV", "PC").unwrap(), std::env::temp_dir().join("lfd3k-props-trust.json"));
        e.on_modem_bytes(&garbage);
        e.on_modem_bytes(&out);
        e.tick(100);
        while e.poll_event().is_some() {}
        let _ = e.take_modem_bytes();
    }
}

#[test]
fn every_voice_frame_kind_parses_after_encode() {
    // The sub-types this build knows decode as type 15; the rest do not.
    for sub in 0..=255u8 {
        let h = Header {
            ftype: FrameType::Voice,
            flags: 0,
            call_id: 1,
            seq: 0,
            ack: 0,
        };
        let mut out = [0u8; 16];
        let n = frame::encode(&h, &[sub], &mut out).unwrap();
        assert_eq!(
            decode_voice(&out[..n]).is_some(),
            VSub::from_u8(sub).is_some(),
            "sub-type {sub}"
        );
    }
}
