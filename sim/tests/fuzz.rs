//! Fuzzing with proptest: random frames and random AT lines must never
//! panic the core. 10,000 cases each.

use std::cell::RefCell;

use lfd3k_core::frame::{decode, encode};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_sim::{Harness, SimChannel};
use proptest::prelude::*;

/// A radio that hears nothing and finishes every send at once.
struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn drain(m: &mut Modem) -> usize {
    let mut buf = [0u8; 256];
    let mut total = 0;
    loop {
        let n = m.host_output(&mut buf);
        if n == 0 {
            return total;
        }
        total += n;
    }
}

thread_local! {
    /// One connected call reused across cases; rebuilt when a case ends it.
    static CALL: RefCell<Option<Harness>> = const { RefCell::new(None) };
}

fn connected_harness(seed: u64) -> Harness {
    let mut h = Harness::new(SimChannel::constant_db(10, seed));
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "fuzz setup could not connect");
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
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn fuzz_frames_never_panic(
        bytes in prop::collection::vec(any::<u8>(), 0..=300),
        patch in 0u8..4,
        tier_i in 0usize..8,
        side in 0usize..2,
        wait in 0u32..30,
        snr in -200i16..200,
    ) {
        let tier = Tier::LADDER[tier_i];

        // decode: never panics; whatever decodes re-encodes to the same bytes.
        if let Ok(f) = decode(&bytes) {
            let mut out = [0u8; 300];
            let n = encode(&f.header, f.payload, &mut out).unwrap();
            prop_assert_eq!(&out[..n], &bytes[..]);
        }

        // Idle modems (one auto-answers).
        let mut cfg = Config::new("AXIS");
        cfg.sregs.set(0, 1);
        let mut idle = [Modem::new(Config::new("CALLER")), Modem::new(cfg)];
        let info = RxInfo { tier, rssi_dbm: -120 + snr / 4, snr_x4: snr };
        for (i, m) in idle.iter_mut().enumerate() {
            let mut r = NullRadio;
            let now = 1_000 * i as u32 + wait;
            m.radio_rx(&bytes, info, &mut r, now);
            for k in 1..=5u32 {
                m.tick(&mut r, now + 10 * k);
            }
            drain(m);
        }

        // A connected call. Half the cases carry the live call id, half a
        // valid frame type, so the in-call paths really run.
        CALL.with(|cell| {
            let mut slot = cell.borrow_mut();
            let h = slot.get_or_insert_with(|| connected_harness(u64::from(wait) + 1));
            let mut frame = bytes.clone();
            if patch & 1 == 1 {
                if let (Some(id), Some(b)) = (live_call_id(h), frame.get_mut(1)) {
                    *b = id;
                }
            }
            if patch & 2 == 2 {
                if let Some(b) = frame.get_mut(0) {
                    *b = ((*b % 11 + 1) << 4) | (*b & 0x0F);
                }
            }
            if side == 0 {
                h.inject_a(&frame, tier);
            } else {
                h.inject_b(&frame, tier);
            }
            h.run_ms(wait);
            if !(h.a.is_connected() && h.b.is_connected()) {
                *slot = None;
            }
        });
    }

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
            drain(&mut m);
        }
        prop_assert!(!m.is_connected());
        // Still a working command line afterwards (after a possible dial
        // gives up, ATH cancels it).
        m.host_input(b"\rATH\rATE1\rATV1\rAT\r", &mut r, 200);
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        prop_assert!(out.ends_with(b"AT\r\r\nOK\r\n"), "tail {:?}", String::from_utf8_lossy(&out));
    }
}
