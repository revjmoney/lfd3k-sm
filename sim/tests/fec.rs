//! Simulated burst parity. The channel is the seeded simulator, not a
//! radio. Numbers below are from that model.
//!
//! Loss rates are the chance that a Data frame (a chunk or a parity
//! frame) is dropped by the hook, on top of the simulator's own SNR
//! loss. Throughput is delivered host bytes divided by simulated
//! milliseconds, one direction.

use std::cell::Cell;

use lfd3k_core::arq::{CHUNK, WINDOW_BULK};
use lfd3k_core::frame::{FrameType, FLAG_PARITY};
use lfd3k_core::{Config, Tier};
use lfd3k_sim::{Harness, Outcome, SimChannel, SnrProfile};

const BYTES: usize = 8 * 1024;

thread_local! {
    static DROP_PCT: Cell<u8> = const { Cell::new(0) };
    static DROP_SEED: Cell<u32> = const { Cell::new(1) };
    static DROP_N: Cell<u32> = const { Cell::new(0) };
}

fn drop_data(t_ms: u32, frame: &[u8]) -> bool {
    let pct = DROP_PCT.with(Cell::get);
    if pct == 0 {
        return false;
    }
    let Some(b0) = frame.get(1) else {
        return false;
    };
    if b0 >> 4 != FrameType::Data as u8 {
        return false;
    }
    let seq = u32::from(frame.get(3).copied().unwrap_or(0));
    let seed = DROP_SEED.with(Cell::get);
    let mut x = t_ms.wrapping_mul(0x9E37_79B9)
        ^ seq.wrapping_mul(0x85EB_CA6B)
        ^ (frame.len() as u32)
        ^ seed;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    (x % 100) < u32::from(pct)
}

/// Bytes with no repeated runs, so stream LZ cannot shrink them. The FEC
/// timing tests need the same air load they had before stream LZ.
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

fn modem(name: &str, s31: u16, auto: bool) -> Config {
    let mut c = Config::new(name).expect("name");
    assert!(c.sregs.set(31, s31));
    if auto {
        assert!(c.sregs.set(0, 1));
    }
    c
}

struct Run {
    ms: u32,
    resends: u32,
    parity: u32,
    data: u32,
    lost_chunks: u32,
}

fn run_transfer(loss_pct: u8, fec: bool, seed: u64) -> Run {
    DROP_PCT.with(|c| c.set(loss_pct));
    DROP_SEED.with(|c| c.set(seed as u32));
    let s31 = if fec { 1 } else { 0 };
    let mut h = Harness::with_configs(
        SimChannel {
            profile: SnrProfile::Constant(12 * 4),
            seed,
            drop: Some(drop_data),
            no_snr_loss: false,
        },
        modem("CALLER", s31, false),
        modem("AXIS", s31, true),
    );
    h.run_ms(50);
    h.type_a("ATDT AXIS\r");
    assert!(
        h.run_until(8_000, |h| h.a.is_connected() && h.b.is_connected()),
        "no connect fec={fec} loss={loss_pct} caller {:?}",
        h.screen_a()
    );
    let t0 = h.now_ms;
    let msg = noise(BYTES, seed as u32);
    let mut sent = 0;
    while sent < msg.len() {
        sent += h.feed_a(&msg[sent..(sent + 400).min(msg.len())]);
        h.run_ms(1);
        assert!(h.now_ms < t0 + 120_000, "sender stalled at {sent}");
    }
    assert!(
        h.run_until(120_000, |h| h.received_b().len() >= msg.len()),
        "delivered {} of {} fec={fec} loss={loss_pct}",
        h.received_b().len(),
        msg.len()
    );
    assert_eq!(h.received_b(), msg, "fec={fec} loss={loss_pct}");
    let st = h.stats();
    let lost_chunks = h
        .trace()
        .iter()
        .filter(|e| {
            e.ftype == Some(FrameType::Data)
                && e.flags & FLAG_PARITY == 0
                && matches!(e.outcome, Outcome::Lost | Outcome::Dropped)
        })
        .count() as u32;
    Run {
        ms: h.now_ms - t0,
        resends: st.resends[0] + st.resends[1],
        parity: st.parity_frames[0] + st.parity_frames[1],
        data: st.data_frames[0] + st.data_frames[1],
        lost_chunks,
    }
}

fn bps(bytes: usize, ms: u32) -> u32 {
    if ms == 0 {
        return 0;
    }
    ((bytes as u64) * 1000 / u64::from(ms)) as u32
}

/// Simulated. A high SNR link that loses nothing sends no parity, so the
/// two runs take about the same time.
#[test]
fn simulated_clean_link_sends_no_parity() {
    let on = run_transfer(0, true, 11);
    let off = run_transfer(0, false, 11);
    eprintln!(
        "simulated clean: fec ms {} resends {} parity {} data {} lost {}; nofec ms {} resends {} lost {}",
        on.ms, on.resends, on.parity, on.data, on.lost_chunks, off.ms, off.resends, off.lost_chunks
    );
    if on.lost_chunks == 0 {
        assert_eq!(on.parity, 0, "parity on a transfer that lost no chunk");
    }
    // Parity must not start before a chunk is actually lost.
    assert!(
        on.parity == 0 || on.lost_chunks > 0,
        "parity {} with no lost chunk",
        on.parity
    );
    // No continuous tax: within 15 percent of the run that cannot send it.
    assert!(
        on.ms * 100 <= off.ms * 115 + 500,
        "fec {} ms vs nofec {} ms",
        on.ms,
        off.ms
    );
}

/// Simulated. At several drop rates, parity should cut resends. At the
/// high rate it should also raise delivered bytes per simulated second,
/// because a resend costs a round trip and one parity frame costs one
/// airtime.
#[test]
fn simulated_loss_parity_versus_arq_only() {
    // (drop percent, seed)
    let rates = [(5u8, 21u64), (10, 22), (20, 23)];
    for (pct, seed) in rates {
        let on = run_transfer(pct, true, seed);
        let off = run_transfer(pct, false, seed);
        let on_bps = bps(BYTES, on.ms);
        let off_bps = bps(BYTES, off.ms);
        eprintln!(
            "simulated loss {pct}%: fec {on_bps} B/s {} ms resends {} parity {} lost {}; nofec {off_bps} B/s {} ms resends {} lost {}",
            on.ms, on.resends, on.parity, on.lost_chunks, off.ms, off.resends, off.lost_chunks
        );
        assert!(
            on.parity > 0,
            "{pct}%: expected parity once chunks were lost"
        );
        // Same tier: parity frames cost airtime, so a light loss rate is
        // slower. Resends stay in the same range (a hole can still push
        // later chunks out of the window of 8). The tier-hold test is
        // where parity beats stepping down.
        assert!(
            on.resends <= off.resends.saturating_add(4),
            "{pct}%: fec resends {} far above nofec {}",
            on.resends,
            off.resends
        );
        assert!(
            on_bps * 100 >= off_bps * 60,
            "{pct}%: fec {on_bps} B/s under 60% of nofec {off_bps} B/s"
        );
    }
}

/// Simulated. SNR falls from 12 dB to -1 dB, which is under SF5's
/// step-down line and about a 10 percent frame loss in this model.
/// Without parity the call steps to a slower tier. With parity it stays
/// on SF5. SF6's air rate is 37500 against SF5's 62500, about 40 percent
/// less.
#[test]
fn simulated_marginal_snr_stays_on_sf5() {
    // (0 ms, 12 dB), (3.5 s, -1 dB). Values are dB times 4.
    static STEPS: [(u32, i16); 2] = [(0, 48), (3_500, -4)];
    let mut elapsed = [0u32; 2];
    for (i, fec) in [false, true].into_iter().enumerate() {
        let s31 = if fec { 1 } else { 0 };
        let mut h = Harness::with_configs(
            SimChannel {
                profile: SnrProfile::Steps(&STEPS),
                seed: 31,
                drop: None,
                no_snr_loss: false,
            },
            modem("CALLER", s31, false),
            modem("AXIS", s31, true),
        );
        h.run_ms(50);
        h.type_a("ATDT AXIS\r");
        assert!(
            h.run_until(8_000, |h| h.a.is_connected() && h.b.is_connected()),
            "no connect fec={fec} {:?}",
            h.screen_a()
        );
        assert!(
            h.screen_a().contains("CONNECT LORA SF5"),
            "trained below SF5: {}",
            h.screen_a()
        );
        let msg = noise(64 * 1024, 31);
        let mut sent = 0;
        while sent < msg.len() {
            sent += h.feed_a(&msg[sent..(sent + 400).min(msg.len())]);
            h.run_ms(1);
            assert!(h.now_ms < 180_000, "stalled at {sent} fec={fec}");
        }
        assert!(
            h.run_until(180_000, |h| h.received_b().len() >= msg.len()),
            "got {} fec={fec}",
            h.received_b().len()
        );
        assert_eq!(h.received_b(), msg);
        let tier = h.a.lights(h.now_ms).tier;
        let parity = h.stats().parity_frames[0] + h.stats().parity_frames[1];
        let resends = h.stats().resends[0] + h.stats().resends[1];
        eprintln!(
            "simulated marginal: fec={fec} tier={tier:?} parity={parity} resends={resends} ms={} tiers={:?}",
            h.now_ms,
            h.stats().tiers[0]
        );
        // RETRAIN is held off the data stream until the host escapes, so
        // the tier the modem is on is the check.
        elapsed[i] = h.now_ms;
        if fec {
            assert_eq!(tier, Some(Tier::Sf5), "stepped down despite parity");
            assert!(parity > 0, "marginal link never armed parity");
        } else {
            assert!(
                tier.is_some_and(|t| t < Tier::Sf5),
                "stayed on SF5 with parity off, tier={tier:?}"
            );
        }
    }
    eprintln!(
        "simulated marginal times: nofec {} ms, fec {} ms",
        elapsed[0], elapsed[1]
    );
    assert!(
        elapsed[1] < elapsed[0],
        "parity on SF5 took {} ms, step down took {} ms",
        elapsed[1],
        elapsed[0]
    );
}

/// Longest back to back Data run (MORE frames plus the frame that ends it).
fn max_data_run(h: &Harness) -> usize {
    let mut best = 0usize;
    let mut run = 0usize;
    for t in h.trace() {
        if t.from != 0 || t.ftype != Some(FrameType::Data) {
            continue;
        }
        run += 1;
        if t.flags & 1 == 0 {
            best = best.max(run);
            run = 0;
        }
    }
    best.max(run)
}

/// Simulated. CAP_BULK (burst 8, window 16) and CAP_FEC together. The
/// parity frame is the eighth frame of the turn, not a ninth, and a
/// missing chunk past the classic window of 8 is still rebuilt.
#[test]
fn simulated_bulk_burst_carries_parity() {
    DROP_PCT.with(|c| c.set(10));
    DROP_SEED.with(|c| c.set(44));
    let mut h = Harness::with_configs(
        SimChannel {
            profile: SnrProfile::Constant(12 * 4),
            seed: 44,
            drop: Some(drop_data),
            no_snr_loss: true,
        },
        modem("CALLER", 1, false),
        modem("AXIS", 1, true),
    );
    h.run_ms(50);
    h.type_a("ATDT AXIS\r");
    assert!(
        h.run_until(8_000, |h| h.a.is_connected() && h.b.is_connected()),
        "no connect {:?}",
        h.screen_a()
    );
    let msg = noise(8 * 1024, 44);
    let took = h.feed_a(&msg[..msg.len().min(5_000)]);
    // Stream LZ is also agreed, so the window holds compressed bytes plus
    // a lookahead. Incompressible input still stops near 16 chunks, which
    // is more than the classic window of 8.
    assert!(
        took > 8 * CHUNK && took <= WINDOW_BULK * CHUNK + 64 + 256,
        "bulk window took {took}, classic would stop at {}",
        8 * CHUNK
    );
    let mut sent = took;
    while sent < msg.len() {
        sent += h.feed_a(&msg[sent..(sent + 400).min(msg.len())]);
        h.run_ms(1);
        assert!(h.now_ms < 120_000, "stalled at {sent}");
    }
    assert!(
        h.run_until(120_000, |h| h.received_b().len() >= msg.len()),
        "got {}",
        h.received_b().len()
    );
    assert_eq!(h.received_b(), msg);
    let parity = h.stats().parity_frames[0] + h.stats().parity_frames[1];
    let resends = h.stats().resends[0] + h.stats().resends[1];
    let run = max_data_run(&h);
    eprintln!(
        "simulated bulk+parity: parity={parity} resends={resends} burst={run} ms={}",
        h.now_ms
    );
    assert!(parity > 0, "bulk loss never armed parity");
    assert_eq!(run, 8, "parity did not ride in the bulk burst of 8");
}

/// Drop every 5th data chunk. Parity frames are left alone so a group
/// can still be rebuilt.
fn drop_every_fifth_chunk(_ms: u32, frame: &[u8]) -> bool {
    if frame.get(1).map(|b| b >> 4) != Some(FrameType::Data as u8) {
        return false;
    }
    if frame.get(1).is_some_and(|b| b & FLAG_PARITY != 0) {
        return false;
    }
    DROP_N.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        n % 5 == 4
    })
}

/// Simulated. S31 = 1 agrees bulk ARQ (burst 8, window 16), burst parity,
/// and stream LZ together. A repeated line shrinks on the air. Dropping
/// every 5th chunk leaves a selective hole, so parity arms, and the
/// compressed bytes that arrive or are rebuilt still decode.
#[test]
fn simulated_bulk_fec_and_stream_lz_together() {
    DROP_N.with(|c| c.set(0));
    let mut h = Harness::with_configs(
        SimChannel {
            profile: SnrProfile::Constant(12 * 4),
            seed: 77,
            drop: Some(drop_every_fifth_chunk),
            no_snr_loss: true,
        },
        modem("CALLER", 1, false),
        modem("AXIS", 1, true),
    );
    h.run_ms(50);
    h.type_a("ATDT AXIS\r");
    assert!(
        h.run_until(8_000, |h| h.a.is_connected() && h.b.is_connected()),
        "no connect {:?}",
        h.screen_a()
    );
    assert!(
        h.screen_a().contains("/ARQ/LZ"),
        "stream LZ was not agreed: {}",
        h.screen_a()
    );
    // A repeated line compresses. The counter bytes keep the compressed
    // stream long enough to fill a bulk burst.
    let mut msg = Vec::new();
    while msg.len() < 12 * 1024 {
        let n = msg.len();
        msg.extend_from_slice(b"\x1b[1;37;44mMAIN MENU\x1b[0m\r\n");
        msg.push((n % 251) as u8);
    }
    let took = h.feed_a(&msg);
    assert!(
        took > 8 * CHUNK,
        "bulk window took {took}, classic would stop near {}",
        8 * CHUNK
    );
    let mut sent = took;
    while sent < msg.len() {
        sent += h.feed_a(&msg[sent..(sent + 800).min(msg.len())]);
        h.run_ms(1);
        assert!(h.now_ms < 120_000, "stalled at {sent}");
    }
    assert!(
        h.run_until(120_000, |h| h.received_b().len() >= msg.len()),
        "got {}",
        h.received_b().len()
    );
    assert_eq!(h.received_b(), msg);
    let parity = h.stats().parity_frames[0] + h.stats().parity_frames[1];
    let run = max_data_run(&h);
    let acks = h
        .trace()
        .iter()
        .filter(|t| t.ftype == Some(FrameType::Ack))
        .map(|t| t.len)
        .collect::<Vec<_>>();
    let data_flags = h
        .trace()
        .iter()
        .filter(|t| t.from == 0 && t.ftype == Some(FrameType::Data) && t.flags & FLAG_PARITY == 0)
        .map(|t| t.flags)
        .collect::<Vec<_>>();
    let air: usize = h
        .trace()
        .iter()
        .filter(|t| {
            t.from == 0
                && t.ftype == Some(FrameType::Data)
                && t.flags & FLAG_PARITY == 0
                && !t.resend
        })
        .map(|t| t.len.saturating_sub(5))
        .sum();
    eprintln!(
        "simulated bulk+fec+lz: took={took} parity={parity} burst={run} air={air} plain={} ms={}",
        msg.len(),
        h.now_ms
    );
    assert!(parity > 0, "loss never armed parity");
    assert_eq!(run, 8, "parity did not ride in the bulk burst of 8");
    assert!(
        acks.iter().any(|&n| n == 7),
        "bulk ack is two SACK bytes (frame len 7), got {acks:?}"
    );
    assert!(
        data_flags.iter().all(|f| f & 4 == 0),
        "stream data set per frame FLAG_LZ: {data_flags:?}"
    );
    assert!(
        air < msg.len(),
        "stream LZ did not shrink the air load: {air} vs {}",
        msg.len()
    );
}
