//! Fuzzing with proptest: random frames and random AT lines must never
//! panic the core, and the data mode escape must follow its timing rule.
//!
//! Every test uses a fixed proptest seed (`fuzz_config`), so a run is the
//! same run everywhere (CI included). Each case builds all of its own state
//! (fresh modems, a fresh connected call), so a failure depends only on the
//! case's inputs: proptest shrinks it and prints the frames it injected.

use std::cell::Cell;

use lfd3k_core::frame::{call_req_payload, decode, encode, FrameType, Header, AIR_ID, HEADER_LEN};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_sim::{Harness, SimChannel};
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed, TestRunner};

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
    /// Rewrite byte 2 to the call id of the live call.
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
        if let (Some(id), Some(x)) = (call_id, b.get_mut(2)) {
            *x = id;
        }
    }
    if f.fix_type {
        if let Some(id) = b.first_mut() {
            *id = AIR_ID;
        }
        if let Some(x) = b.get_mut(1) {
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
        let mut cfg = Config::new("AXIS").unwrap();
        cfg.sregs.set(0, 1);
        let mut modems = [Modem::new(Config::new("CALLER").unwrap()), Modem::new(cfg)];
        // The call id a CallReq opened, for the follow-up frames.
        let call_id = frames
            .iter()
            .find(|f| f.bytes.get(1).map(|b| b >> 4) == Some(FrameType::CallReq as u8))
            .and_then(|f| f.bytes.get(2).copied());
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
            prop_assert_eq!(f.payload.len() + HEADER_LEN, bytes.len());
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
        let mut m = Modem::new(Config::new("CALLER").unwrap());
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

/// Cases for the data mode escape fuzz (a fresh connected call per case).
const ESCAPE_CASES: u32 = 600;

/// The wait before one host write, relative to the S12 guard `g` (ms).
#[derive(Clone, Copy, Debug)]
enum Gap {
    Zero,
    /// 1 to min(g - 1, 40) ms.
    Short(u32),
    /// g - k, k in 1..=15 (at least 1 ms).
    Under(u32),
    /// Exactly g.
    Exact,
    /// g + k, k in 0..=20: on, and just past, the guard edge.
    Over(u32),
    /// 2g + 20 ms or more.
    Long(u32),
}

impl Gap {
    fn ms(self, g: u32) -> u32 {
        match self {
            Gap::Zero => 0,
            Gap::Short(f) => 1 + f % g.saturating_sub(1).clamp(1, 40),
            Gap::Under(k) => g.saturating_sub(k).max(1),
            Gap::Exact => g,
            Gap::Over(k) => g + k,
            Gap::Long(f) => 2 * g + 20 + f % (g + 1),
        }
    }
}

fn gap() -> impl Strategy<Value = Gap> {
    prop_oneof![
        2 => Just(Gap::Zero),
        3 => (0u32..1000).prop_map(Gap::Short),
        2 => (1u32..=15).prop_map(Gap::Under),
        1 => Just(Gap::Exact),
        3 => (0u32..=20).prop_map(Gap::Over),
        3 => (0u32..1000).prop_map(Gap::Long),
    ]
}

/// One host write: a run of 1 to 4 '+', or random bytes biased to '+'.
fn chunk() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        4 => (1usize..=4).prop_map(|n| vec![b'+'; n]),
        3 => prop::collection::vec(prop_oneof![1 => Just(b'+'), 2 => any::<u8>()], 1..=12),
        1 => any::<u8>().prop_map(|b| vec![b]),
    ]
}

/// S12 values: 0 (escape off), small guards, the default 50.
fn s12() -> impl Strategy<Value = u16> {
    prop_oneof![
        2 => Just(0u16),
        6 => 1u16..=10,
        1 => Just(25u16),
        1 => Just(50u16),
    ]
}

/// The escape rule over the offered bytes `(time ms, byte)`, the last
/// byte offered at the end: three '+' at the end of the stream, the first
/// at least `g` after the byte before it, each next one less than `g` after
/// the previous. Returns the time the third '+' was offered. (Whether
/// `g` of silence followed is the caller's check; S12=0 never escapes.)
fn trailing_escape(fed: &[(u32, u8)], g: u32) -> Option<u32> {
    let n = fed.len();
    if g == 0 || n < 4 {
        return None;
    }
    let (p, a, b, c) = (fed[n - 4], fed[n - 3], fed[n - 2], fed[n - 1]);
    let pluses = a.1 == b'+' && b.1 == b'+' && c.1 == b'+';
    let timing = a.0 - p.0 >= g && b.0 - a.0 < g && c.0 - b.0 < g;
    (pluses && timing).then_some(c.0)
}

#[test]
fn fuzz_data_mode_escape() {
    let strategy = (
        s12(),
        1u64..64,
        prop::collection::vec((gap(), chunk()), 1..=10),
    );
    let escapes = Cell::new(0u32);
    let edge_escapes = Cell::new(0u32);
    let plain = Cell::new(0u32);
    let escape_off = Cell::new(0u32);
    let mut runner = TestRunner::new(fuzz_config(ESCAPE_CASES));
    let result = runner.run(&strategy, |(s12, seed, writes)| {
        let g = u32::from(s12) * 20;
        let mut h = Harness::new(SimChannel::constant_db(12, seed));
        h.type_a(&format!("ATS12={s12}\r"));
        h.run_ms(20);
        prop_assert!(h.screen_a().ends_with("\r\nOK\r\n"));
        h.type_a("ATDT AXIS\r");
        let ok = h.run_until(20_000, |h| h.a.is_connected() && h.b.is_connected());
        prop_assert!(ok, "no connect (seed {})", seed);
        let mark = h.screen_a_bytes().len();
        // A non '+' anchor byte, so the first guard is measured from a
        // byte this test offered.
        let mut fed: Vec<(u32, u8)> = Vec::new();
        prop_assert_eq!(h.feed_a(b"A"), 1);
        fed.push((h.now_ms, b'A'));
        let mut escaped: Option<u32> = None;
        let mut z_fed = false;
        for (gp, bytes) in &writes {
            h.run_ms(gp.ms(g));
            let now = h.now_ms;
            if let Some(third) = trailing_escape(&fed, g) {
                if now - third >= g {
                    // The guard after the +++ ran out: this write is a
                    // command line now. Offer one byte and stop.
                    if now - third < g + 10 {
                        edge_escapes.set(edge_escapes.get() + 1);
                    }
                    prop_assert_eq!(h.feed_a(b"z"), 1);
                    escaped = Some(third);
                    z_fed = true;
                    break;
                }
            }
            // Small writes: the send window always takes all of it, so
            // the offer times are exact.
            prop_assert_eq!(h.feed_a(bytes), bytes.len());
            fed.extend(bytes.iter().map(|&b| (now, b)));
        }
        let tail = trailing_escape(&fed, g);
        if escaped.is_none() && tail.is_some() {
            escaped = tail;
        }
        h.run_ms(g + 50);
        let screen = h.screen_a_bytes()[mark..].to_vec();
        let mut want: Vec<u8> = fed.iter().map(|e| e.1).collect();
        if escaped.is_some() {
            escapes.set(escapes.get() + 1);
            // The escape's three '+' are not data.
            want.truncate(want.len() - 3);
            // OK, then the echo of the one command byte if one was offered.
            let mut ok_text = b"\r\nOK\r\n".to_vec();
            if z_fed {
                ok_text.push(b'z');
            }
            prop_assert!(
                screen == ok_text,
                "escape expected, CALLER screen {:?}",
                String::from_utf8_lossy(&screen)
            );
        } else {
            if s12 == 0 {
                escape_off.set(escape_off.get() + 1);
            } else {
                plain.set(plain.get() + 1);
            }
            prop_assert!(
                screen.is_empty(),
                "no escape expected, CALLER screen {:?}",
                String::from_utf8_lossy(&screen)
            );
        }
        let arrived = h.run_until(20_000, |h| h.received_b().len() >= want.len());
        h.run_ms(200);
        prop_assert!(
            arrived,
            "AXIS got {} of {} bytes",
            h.received_b().len(),
            want.len()
        );
        prop_assert_eq!(h.received_b(), want);
        prop_assert!(h.a.is_connected() && h.b.is_connected());
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }
    // Both outcomes, the guard edge and S12=0 were all exercised.
    let (e, edge, p, off) = (
        escapes.get(),
        edge_escapes.get(),
        plain.get(),
        escape_off.get(),
    );
    assert!(
        e >= 50 && edge >= 5 && p >= 100 && off >= 50,
        "escapes {e} (edge {edge}), data only {p}, S12=0 {off}"
    );
}
