//! Fuzzing with proptest: random frames and random AT lines must never
//! panic the core.
//!
//! Every test uses a fixed proptest seed (`fuzz_config`), so a run is the
//! same run everywhere (CI included). Each case builds all of its own state
//! (fresh modems, a fresh connected call), so a failure depends only on the
//! case's inputs: proptest shrinks it and prints the frames it injected.

use lfd3k_core::frame::{call_req_payload, decode, encode, FrameType, Header};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_sim::{Harness, SimChannel};
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};

/// Fixed seed for every fuzz test. Change it to explore a new sample.
const FUZZ_SEED: u64 = 0x1FD3_0420;

fn fuzz_config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(FUZZ_SEED),
        ..ProptestConfig::default()
    }
}

/// A radio that hears nothing and finishes every send at once.
struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn output(m: &mut Modem) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = m.host_output(&mut buf);
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// One fuzzed frame: raw bytes, or a CallReq built valid and then mutated.
#[derive(Clone, Debug)]
struct Fz {
    bytes: Vec<u8>,
    /// A CallReq for AXIS (any case) with a nonzero call id, unmutated.
    clean_call_for_axis: bool,
    /// Rewrite the type nibble to a valid type (1 to 11).
    fix_type: bool,
    /// Rewrite byte 1 to the call id of the live call.
    use_call_id: bool,
}

fn raw_frame() -> impl Strategy<Value = Fz> {
    (
        prop::collection::vec(any::<u8>(), 0..=300),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(bytes, fix_type, use_call_id)| Fz {
            bytes,
            clean_call_for_axis: false,
            fix_type,
            use_call_id,
        })
}

/// A valid CallReq (callee AXIS in any case, or a random name; random
/// caller, call id and flags), then 0 to 3 byte mutations and maybe a cut.
fn call_req_frame() -> impl Strategy<Value = Fz> {
    (
        any::<u8>(),
        0u8..16,
        "[ -~]{1,16}",
        prop_oneof![
            3 => Just("AXIS".to_string()),
            1 => Just("axis".to_string()),
            1 => Just("aXiS".to_string()),
            1 => "[ -~]{1,16}",
        ],
        prop::collection::vec((any::<usize>(), any::<u8>()), 0..4),
        prop::option::weighted(0.1, any::<usize>()),
    )
        .prop_map(|(call_id, flags, caller, callee, muts, cut)| {
            let mut p = [0u8; 40];
            let n = call_req_payload(&caller, &callee, &mut p).unwrap_or(0);
            let h = Header {
                ftype: FrameType::CallReq,
                flags,
                call_id,
                seq: 0,
                ack: 0,
            };
            let mut buf = [0u8; 64];
            let len = encode(&h, &p[..n], &mut buf).unwrap_or(0);
            let mut bytes = buf[..len].to_vec();
            let mut changed = false;
            for (i, v) in &muts {
                if !bytes.is_empty() {
                    let at = i % bytes.len();
                    changed |= bytes[at] != *v;
                    bytes[at] = *v;
                }
            }
            if let Some(c) = cut {
                let keep = c % (bytes.len() + 1);
                changed |= keep < bytes.len();
                bytes.truncate(keep);
            }
            Fz {
                clean_call_for_axis: !changed
                    && call_id != 0
                    && callee.eq_ignore_ascii_case("AXIS"),
                bytes,
                fix_type: false,
                use_call_id: false,
            }
        })
}

fn any_frame() -> impl Strategy<Value = Fz> {
    prop_oneof![raw_frame(), call_req_frame()]
}

/// Applies the patches; `call_id` is the live call's id, if any.
fn patched(f: &Fz, call_id: Option<u8>) -> Vec<u8> {
    let mut b = f.bytes.clone();
    if f.use_call_id {
        if let (Some(id), Some(x)) = (call_id, b.get_mut(1)) {
            *x = id;
        }
    }
    if f.fix_type {
        if let Some(x) = b.first_mut() {
            *x = ((*x % 11 + 1) << 4) | (*x & 0x0F);
        }
    }
    b
}

fn connected_harness(seed: u64) -> Harness {
    let mut h = Harness::new(SimChannel::constant_db(10, seed));
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "fuzz setup could not connect (seed {seed})");
    h
}

/// The call id of the live call, from the last frame on the air.
fn live_call_id(h: &Harness) -> Option<u8> {
    h.trace()
        .iter()
        .rev()
        .find(|e| e.ftype.is_some())
        .map(|e| e.call_id)
}

proptest! {
    #![proptest_config(fuzz_config(10_000))]

    /// Random bytes into `decode`, and frame sequences (random bytes or
    /// mutated CallReqs, patched to a valid type and to the call id the
    /// first CallReq opened) into two idle modems: CALLER, and AXIS with
    /// S0=1, so ring and auto-answer run with fuzzed follow-up frames.
    #[test]
    fn fuzz_frames_never_panic(
        bytes in prop::collection::vec(any::<u8>(), 0..=300),
        frames in prop::collection::vec(any_frame(), 1..=4),
        tier_i in 0usize..8,
        gap_ticks in 0u32..20,
        snr in -200i16..200,
    ) {
        // decode: never panics; whatever decodes re-encodes to the same bytes.
        for b in std::iter::once(&bytes).chain(frames.iter().map(|f| &f.bytes)) {
            if let Ok(f) = decode(b) {
                let mut out = [0u8; 300];
                let n = encode(&f.header, f.payload, &mut out).unwrap();
                prop_assert_eq!(&out[..n], &b[..]);
            }
        }

        let tier = Tier::LADDER[tier_i];
        let info = RxInfo { tier, rssi_dbm: -120 + snr / 4, snr_x4: snr };
        let mut cfg = Config::new("AXIS");
        cfg.sregs.set(0, 1);
        let mut modems = [Modem::new(Config::new("CALLER")), Modem::new(cfg)];
        // The call id a CallReq opened, for the follow-up frames.
        let call_id = frames
            .iter()
            .find(|f| f.bytes.first().map(|b| b >> 4) == Some(FrameType::CallReq as u8))
            .and_then(|f| f.bytes.get(1).copied());
        for (side, m) in modems.iter_mut().enumerate() {
            let mut r = NullRadio;
            let mut now = 1_000 * side as u32;
            for (i, f) in frames.iter().enumerate() {
                m.radio_rx(&patched(f, call_id), info, &mut r, now);
                let out = output(m);
                if i == 0 && side == 1 && f.clean_call_for_axis {
                    // A valid CallReq for AXIS rings and auto-answers.
                    prop_assert!(
                        out.windows(8).any(|w| w == b"\r\nRING\r\n"),
                        "no RING for {:?}: {:?}", f.bytes, String::from_utf8_lossy(&out)
                    );
                }
                for _ in 0..=gap_ticks {
                    now += 10;
                    m.tick(&mut r, now);
                }
                output(m);
            }
        }
    }

    /// Random bytes into `decode` only, many cases (cheap).
    #[test]
    fn fuzz_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=300)) {
        if let Ok(f) = decode(&bytes) {
            prop_assert_eq!(f.payload.len() + 4, bytes.len());
        }
    }

    /// Random command lines into a fresh modem in command mode.
    #[test]
    fn fuzz_at_lines_never_panic(
        line in prop_oneof![
            any::<String>(),
            "(?i)AT[ADHOIVEZS&0-9?=TP ,.+*!-]{0,70}",
            "(?i)ATS[0-9]{0,4}[=?][0-9]{0,6}",
            "(?i)ATD[TP]? ?[ -~]{0,20}",
        ],
        raw in prop::collection::vec(any::<u8>(), 0..80),
        use_raw in any::<bool>(),
    ) {
        let mut m = Modem::new(Config::new("CALLER"));
        let mut r = NullRadio;
        let mut input: Vec<u8> = if use_raw { raw } else { line.into_bytes() };
        input.push(b'\r');
        let taken = m.host_input(&input, &mut r, 0);
        // Command mode takes every byte.
        prop_assert_eq!(taken, input.len());
        for k in 1..=10u32 {
            m.tick(&mut r, 10 * k);
            output(&mut m);
        }
        prop_assert!(!m.is_connected());
        // Still a working command line afterwards (after a possible dial
        // gives up, ATH cancels it).
        m.host_input(b"\rATH\rATE1\rATV1\rAT\r", &mut r, 200);
        let out = output(&mut m);
        prop_assert!(out.ends_with(b"AT\r\r\nOK\r\n"), "tail {:?}", String::from_utf8_lossy(&out));
    }
}

proptest! {
    #![proptest_config(fuzz_config(CONNECTED_CASES))]

    /// Frame sequences injected into a fresh connected call per case (each
    /// case connects its own harness, so failures replay and shrink). Half
    /// the frames carry the live call id and half a valid type, so the in
    /// call paths run. Proptest prints the injected frames on failure.
    #[test]
    fn fuzz_connected_frames_never_panic(
        frames in prop::collection::vec(
            (any_frame(), 0usize..8, 0usize..2, 0u32..40),
            1..=4,
        ),
        seed in 1u64..64,
    ) {
        let mut h = connected_harness(seed);
        let id = live_call_id(&h);
        for (f, tier_i, side, wait) in &frames {
            let frame = patched(f, id);
            let tier = Tier::LADDER[*tier_i];
            if *side == 0 {
                h.inject_a(&frame, tier);
            } else {
                h.inject_b(&frame, tier);
            }
            h.run_ms(*wait);
        }
        // The modems keep running after whatever the frames did.
        h.run_ms(200);
    }
}

/// Cases for the connected fuzz test (a fresh call costs about 1.5 s of
/// simulated time per case).
const CONNECTED_CASES: u32 = 3_000;
