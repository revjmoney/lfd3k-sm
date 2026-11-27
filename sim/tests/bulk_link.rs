//! Bulk ARQ (`CAP_BULK`): burst of 8 and window of 16 only while a bulk
//! transfer is queued. Numbers printed here are simulated, not on air.
//!
//! The channel can skip the SNR loss draw (`no_snr_loss`) so a throughput
//! figure is the protocol. A separate run drops data frames on purpose.

use std::cell::Cell;

use lfd3k_core::arq::{CHUNK, WINDOW, WINDOW_BULK};
use lfd3k_core::frame::FrameType;
use lfd3k_core::{Config, Tier};
use lfd3k_sim::{Harness, Rng, SimChannel};

const MORE: u8 = 1;

thread_local! {
    static DROP_N: Cell<u32> = const { Cell::new(0) };
}

/// Drop every 5th Data frame. Setup frames are left alone.
fn drop_every_fifth(_ms: u32, frame: &[u8]) -> bool {
    if frame.get(1).map(|b| b >> 4) != Some(FrameType::Data as u8) {
        return false;
    }
    DROP_N.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        n % 5 == 4
    })
}

fn bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| r.next_u32() as u8).collect()
}

fn harness(s31: u16, seed: u64, drop: Option<fn(u32, &[u8]) -> bool>) -> Harness {
    let mut a = Config::new("CALLER").unwrap();
    let mut b = Config::new("AXIS").unwrap();
    a.sregs.set(31, s31);
    b.sregs.set(31, s31);
    b.sregs.set(0, 1);
    Harness::with_configs(
        SimChannel {
            profile: lfd3k_sim::SnrProfile::Constant(12 * 4),
            seed,
            drop,
            no_snr_loss: true,
        },
        a,
        b,
    )
}

fn connect(h: &mut Harness) {
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(8_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "CALLER: {}\nAXIS: {}", h.screen_a(), h.screen_b());
    assert_eq!(h.a.lights(h.now_ms).tier, Some(Tier::Sf5));
}

fn kbps_x10(nbytes: usize, ms: u32) -> u32 {
    if ms == 0 {
        return 0;
    }
    // Tenths of a kilobit per second. bits/ms = kilobits/s.
    u32::try_from((nbytes as u64) * 80 / u64::from(ms)).unwrap_or(0)
}

fn fmt_kbps(x10: u32) -> String {
    format!("{}.{}", x10 / 10, x10 % 10)
}

/// Longest run of `kind` frames from `from` (a MORE run plus its last frame).
fn max_run(h: &Harness, from: usize, kind: FrameType) -> usize {
    let mut best = 0usize;
    let mut run = 0usize;
    for t in h.trace() {
        if t.from != from || t.ftype != Some(kind) {
            continue;
        }
        run += 1;
        if t.flags & MORE == 0 {
            best = best.max(run);
            run = 0;
        }
    }
    best.max(run)
}

fn ack_lens(h: &Harness) -> Vec<usize> {
    h.trace()
        .iter()
        .filter(|t| t.ftype == Some(FrameType::Ack))
        .map(|t| t.len)
        .collect()
}

struct Xfer {
    ms: u32,
    kbps_x10: u32,
    resends: u32,
    burst: usize,
}

/// One way, CALLER to AXIS, clock started at the first byte offered.
fn one_way(s31: u16, payload: &[u8], seed: u64) -> Xfer {
    let mut h = harness(s31, seed, None);
    h.run_ms(50);
    connect(&mut h);
    let took = h.feed_a(payload);
    let t0 = h.now_ms;
    if took < payload.len() {
        h.send_a_data(&payload[took..]);
    }
    let ok = h.run_until(60_000, |h| h.received_b().len() >= payload.len());
    assert!(ok, "got {} of {}", h.received_b().len(), payload.len());
    assert_eq!(h.received_b(), payload);
    let ms = h.now_ms.wrapping_sub(t0);
    Xfer {
        ms,
        kbps_x10: kbps_x10(payload.len(), ms),
        resends: h.stats().resends[0],
        burst: max_run(&h, 0, FrameType::Data),
    }
}

fn both_ways(s31: u16, n: usize, seed: u64) -> (u32, u32) {
    let to_b = bytes(n, seed);
    let to_a = bytes(n, seed ^ 0xA5A5);
    let mut h = harness(s31, seed, None);
    h.run_ms(50);
    connect(&mut h);
    let t0 = h.now_ms;
    let mut ia = 0;
    let mut ib = 0;
    let mut idle = 0u32;
    while ia < n || ib < n {
        let ea = (ia + 256).min(n);
        let eb = (ib + 256).min(n);
        let na = if ia < n { h.feed_a(&to_b[ia..ea]) } else { 0 };
        let nb = if ib < n { h.feed_b(&to_a[ib..eb]) } else { 0 };
        ia += na;
        ib += nb;
        idle = if na + nb == 0 { idle + 1 } else { 0 };
        assert!(idle < 60_000, "stalled at {}", h.now_ms);
        h.run_ms(1);
    }
    let ok = h.run_until(60_000, |h| {
        h.received_b().len() >= n && h.received_a().len() >= n
    });
    assert!(
        ok,
        "got {} and {}",
        h.received_b().len(),
        h.received_a().len()
    );
    assert_eq!(h.received_b(), to_b);
    assert_eq!(h.received_a(), to_a);
    let ms = h.now_ms.wrapping_sub(t0);
    (ms, kbps_x10(n, ms))
}

#[test]
fn bulk_window_and_classic_window() {
    let mut wide = harness(1, 1, None);
    wide.run_ms(20);
    connect(&mut wide);
    let took = wide.feed_a(&[0x3C; 5_000]);
    assert!(
        took > (WINDOW_BULK - 1) * CHUNK && took <= WINDOW_BULK * CHUNK,
        "bulk window took {took}"
    );
    let mut narrow = harness(0, 1, None);
    narrow.run_ms(20);
    connect(&mut narrow);
    let took_n = narrow.feed_a(&[0x3C; 5_000]);
    assert!(
        took_n > (WINDOW - 1) * CHUNK && took_n <= WINDOW * CHUNK,
        "classic window took {took_n}"
    );
}

#[test]
fn simulated_sf5_throughput_and_short_turns() {
    let n = 4 * 1024;
    let payload = bytes(n, 0xB011);
    let classic = one_way(0, &payload, 11);
    let bulk = one_way(1, &payload, 11);
    let (c_ms, c_each) = both_ways(0, n, 12);
    let (b_ms, b_each) = both_ways(1, n, 12);

    eprintln!(
        "simulated SF5, {n} bytes, no SNR loss: one-way classic {} kbps ({} ms, burst {}, resends {}), bulk {} kbps ({} ms, burst {}, resends {}); each direction of a two-way transfer classic {} kbps ({} ms), bulk {} kbps ({} ms)",
        fmt_kbps(classic.kbps_x10),
        classic.ms,
        classic.burst,
        classic.resends,
        fmt_kbps(bulk.kbps_x10),
        bulk.ms,
        bulk.burst,
        bulk.resends,
        fmt_kbps(c_each),
        c_ms,
        fmt_kbps(b_each),
        b_ms,
    );

    assert_eq!(classic.burst, 4, "classic burst");
    assert_eq!(bulk.burst, 8, "bulk burst");
    assert_eq!(classic.resends, 0, "classic spurious resend");
    assert_eq!(bulk.resends, 0, "bulk spurious resend");
    assert!(
        bulk.kbps_x10 >= classic.kbps_x10,
        "bulk slower than classic"
    );
    // A clean SF5 transfer sits well above the old rough estimate, because
    // the turn gap is not waited when the peer answers.
    assert!(classic.kbps_x10 > 400, "classic {}", classic.kbps_x10);
    assert!(bulk.kbps_x10 > 400, "bulk {}", bulk.kbps_x10);

    let mut h = harness(1, 11, None);
    h.run_ms(20);
    connect(&mut h);
    assert!(
        ack_lens(&h).is_empty() || ack_lens(&h).iter().all(|&n| n == 7),
        "acks before data {:?}",
        ack_lens(&h)
    );
    // Rebuild from the one_way traces by checking a fresh bulk call's acks
    // inside one_way is awkward; check the frames of a dedicated send.
    let mut h = harness(1, 21, None);
    h.run_ms(20);
    connect(&mut h);
    let msg = bytes(n, 3);
    h.send_a_data(&msg);
    assert!(h.run_until(60_000, |h| h.received_b().len() >= msg.len()));
    let lens = ack_lens(&h);
    assert!(!lens.is_empty());
    assert!(lens.iter().all(|&n| n == 7), "bulk ack lens {lens:?}");
    let mut h = harness(0, 21, None);
    h.run_ms(20);
    connect(&mut h);
    h.send_a_data(&msg);
    assert!(h.run_until(60_000, |h| h.received_b().len() >= msg.len()));
    let lens = ack_lens(&h);
    assert!(lens.iter().all(|&n| n == 6), "classic ack lens {lens:?}");

    // A short turn on a call that agreed CAP_BULK is still one frame.
    let line = bytes(16, 99);
    let short_c = one_way(0, &line, 30);
    let short_b = one_way(1, &line, 30);
    eprintln!(
        "simulated SF5 short turn (16 bytes): classic {} ms, bulk-agreed {} ms",
        short_c.ms, short_b.ms
    );
    assert_eq!(short_b.burst, 1, "short turn grew a burst");
    assert_eq!(short_c.burst, 1);
    assert!(
        short_b.ms <= short_c.ms.saturating_add(80),
        "short turn slower when bulk is agreed: {} vs {}",
        short_b.ms,
        short_c.ms
    );
}

#[test]
fn loss_still_delivers_with_retransmits() {
    DROP_N.with(|c| c.set(0));
    let n = 2 * 1024;
    let payload = bytes(n, 0x1055);
    for s31 in [0u16, 1] {
        DROP_N.with(|c| c.set(0));
        let mut h = harness(s31, 40 + u64::from(s31), Some(drop_every_fifth));
        h.run_ms(20);
        connect(&mut h);
        let t0 = h.now_ms;
        h.send_a_data(&payload);
        let ok = h.run_until(90_000, |h| h.received_b().len() >= payload.len());
        assert!(
            ok,
            "s31={s31} got {} resends {}",
            h.received_b().len(),
            h.stats().resends[0]
        );
        assert_eq!(h.received_b(), payload, "s31={s31}");
        assert!(h.stats().resends[0] > 0, "s31={s31} did not retransmit");
        let lens = ack_lens(&h);
        let want = if s31 == 1 { 7 } else { 6 };
        assert!(lens.iter().all(|&n| n == want), "s31={s31} acks {lens:?}");
        let ms = h.now_ms.wrapping_sub(t0);
        eprintln!(
            "simulated SF5 loss (every 5th data frame), s31={s31}: {} ms, {} resends, {} kbps",
            ms,
            h.stats().resends[0],
            fmt_kbps(kbps_x10(n, ms))
        );
    }
}
