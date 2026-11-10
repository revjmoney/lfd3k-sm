//! Two modem scenario tests on the simulated channel. All seeded, so every
//! run is the same run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use lfd3k_core::frame::{call_req_payload, encode, FrameType, Header};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_sim::{Harness, Outcome, Rng, SimChannel, SnrProfile};

const CONNECT_SF5: &str = "\r\nCONNECT LORA SF5 BW500 62500/ARQ\r\n";
const CONNECT_SF12: &str = "\r\nCONNECT LORA SF12 BW500 1172/ARQ\r\n";
const NO_CARRIER: &str = "\r\nNO CARRIER\r\n";
const OK: &str = "\r\nOK\r\n";
/// Escape guard (S12 = 50, so 1 s) plus a little.
const GUARD_MS: u32 = 1_100;
/// Default S22.
const S22_MS: u32 = 20_000;

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..len).map(|_| r.next_u32() as u8).collect()
}

/// Runs in 50 ms slices until `cond` holds or `max_ms` passes. For checks
/// that are too costly to run every millisecond.
fn wait_for(h: &mut Harness, max_ms: u32, cond: impl Fn(&Harness) -> bool) -> bool {
    let end = h.now_ms + max_ms;
    while h.now_ms < end {
        if cond(h) {
            return true;
        }
        h.run_ms(50);
    }
    cond(h)
}

/// CALLER dials AXIS; panics unless both are connected within `max_ms`.
fn connect(h: &mut Harness, max_ms: u32) {
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(max_ms, |h| h.a.is_connected() && h.b.is_connected());
    assert!(
        ok,
        "no connect within {max_ms} ms\nCALLER: {:?}\nAXIS: {:?}",
        h.screen_a(),
        h.screen_b()
    );
}

/// Guard, `+++`, guard on CALLER. Returns CALLER's screen text from the
/// escape on.
fn escape_a(h: &mut Harness) -> String {
    h.run_ms(GUARD_MS);
    let mark = h.screen_a_bytes().len();
    h.type_a("+++");
    h.run_ms(GUARD_MS);
    String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned()
}

/// The tier of the first `RETRAIN` line in `text`.
fn retrain_tier(text: &str) -> Option<Tier> {
    let at = text.find("\r\nRETRAIN ")?;
    let line = text[at + 2..].split("\r\n").next()?;
    Tier::LADDER
        .into_iter()
        .find(|t| line.starts_with(&format!("RETRAIN {} ", t.name())))
}

fn fed_all_both_ways(h: &mut Harness, a: &[u8], b: &[u8]) {
    let (mut ia, mut ib) = (0, 0);
    let mut idle = 0;
    while ia < a.len() || ib < b.len() {
        let ea = (ia + 256).min(a.len());
        let eb = (ib + 256).min(b.len());
        let na = h.feed_a(&a[ia..ea]);
        let nb = h.feed_b(&b[ib..eb]);
        ia += na;
        ib += nb;
        idle = if na + nb == 0 { idle + 1 } else { 0 };
        assert!(idle < 60_000, "no bytes taken for 60 s at {} ms", h.now_ms);
        h.run_ms(1);
    }
}

#[test]
fn clean_call_connects_at_sf5() {
    let mut h = Harness::new(SimChannel::constant_db(12, 1));
    h.run_ms(100);
    let t0 = h.now_ms;
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(5_000, |h| {
        h.screen_a().contains("\r\nCONNECT ") && h.screen_b().contains("\r\nCONNECT ")
    });
    assert!(ok, "CALLER: {:?}\nAXIS: {:?}", h.screen_a(), h.screen_b());
    assert!(h.now_ms - t0 <= 5_000);
    let a = h.screen_a();
    let b = h.screen_b();
    assert!(a.contains(CONNECT_SF5), "CALLER: {a:?}");
    assert!(b.contains(CONNECT_SF5), "AXIS: {b:?}");
    assert!(a.contains("\r\nDIALING AXIS\r\n"), "CALLER: {a:?}");
    assert!(a.contains("\r\nRINGING\r\n"), "CALLER: {a:?}");
    assert!(a.contains("\r\nTRAINING SNR 12.0 dB\r\n"), "CALLER: {a:?}");
    assert!(b.contains("\r\nRING\r\n"), "AXIS: {b:?}");
    assert!(h.a.is_connected() && h.b.is_connected());
    let lights = h.a.lights(h.now_ms);
    assert!(lights.cd && lights.oh && lights.hs && lights.ec);
    assert_eq!(lights.tier, Some(Tier::Sf5));
    assert!(h.b.lights(h.now_ms).aa);
}

#[test]
fn weak_signal_connects_at_sf12() {
    let mut h = Harness::new(SimChannel::constant_db(-16, 2));
    connect(&mut h, 30_000);
    assert!(
        h.screen_a().contains(CONNECT_SF12),
        "CALLER: {:?}",
        h.screen_a()
    );
    assert!(
        h.screen_b().contains(CONNECT_SF12),
        "AXIS: {:?}",
        h.screen_b()
    );
    let msg = random_bytes(600, 22);
    h.send_a_data(&msg);
    assert!(wait_for(&mut h, 60_000, |h| h.received_b().len() >= msg.len()));
    assert_eq!(h.received_b(), msg);
    assert!(!h.a.lights(h.now_ms).hs);
}

#[test]
fn data_arrives_intact_both_ways() {
    let mut h = Harness::new(SimChannel::constant_db(0, 3));
    connect(&mut h, 10_000);
    let to_b = random_bytes(16 * 1024, 31);
    let to_a = random_bytes(16 * 1024, 32);
    fed_all_both_ways(&mut h, &to_b, &to_a);
    let done = wait_for(&mut h, 120_000, |h| {
        h.received_b().len() >= to_b.len() && h.received_a().len() >= to_a.len()
    });
    assert!(
        done,
        "received {} and {} bytes",
        h.received_b().len(),
        h.received_a().len()
    );
    assert!(h.received_b() == to_b, "CALLER to AXIS data differs");
    assert!(h.received_a() == to_a, "AXIS to CALLER data differs");
    // Both directions really carried data frames.
    let st = h.stats();
    assert!(st.data_frames[0] >= 80 && st.data_frames[1] >= 80);
    assert!(h.a.is_connected() && h.b.is_connected());
}

#[test]
fn retrain_down_and_up_keeps_data() {
    static STEPS: [(u32, i16); 3] = [(0, 40), (20_000, -48), (60_000, 40)];
    let mut h = Harness::new(SimChannel {
        profile: SnrProfile::Steps(&STEPS),
        seed: 4,
        drop: None,
    });
    connect(&mut h, 10_000);
    assert!(h.screen_a().contains(CONNECT_SF5));

    // 32 KB released to CALLER's host in 1 KB slices every 2 s from 4 s to
    // 66 s, fed as fast as CALLER takes it, so the transfer spans both SNR
    // changes. At 44 s (inside the dip) the host stops typing data and
    // escapes: the held RETRAIN shows after OK.
    let data = random_bytes(32 * 1024, 41);
    let mut sent = 0usize;
    let mut slow_seen: Option<Tier> = None;
    while sent < data.len() {
        if slow_seen.is_none() && h.now_ms >= 44_000 {
            let text = escape_a(&mut h);
            assert!(text.starts_with(OK), "escape text {text:?}");
            let t = retrain_tier(&text).expect("no RETRAIN after the escape in the dip");
            assert!(t < Tier::Sf5, "dip retrain {t:?} is not slower");
            assert!(h.now_ms < 60_000, "dip escape ran at {} ms", h.now_ms);
            slow_seen = Some(t);
            assert!(h.a.is_connected());
            let mark = h.screen_a_bytes().len();
            h.type_a("ATO\r");
            h.run_ms(200);
            let back = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
            assert!(back.contains("\r\nCONNECT LORA SF"), "ATO gave {back:?}");
        }
        let released = if h.now_ms < 4_000 {
            0
        } else {
            (((h.now_ms - 4_000) / 2_000 + 1) as usize * 1024).min(data.len())
        };
        if sent < released {
            sent += h.feed_a(&data[sent..released]);
        }
        assert!(h.now_ms < 300_000, "CALLER took only {sent} bytes");
        h.run_ms(1);
    }
    let slow = slow_seen.expect("dip escape did not run");

    let ok = wait_for(&mut h, 240_000, |h| h.received_b().len() >= data.len());
    assert!(
        ok,
        "only {} of {} bytes arrived",
        h.received_b().len(),
        data.len()
    );
    let done_ms = h.now_ms;
    assert!(h.received_b() == data, "data differs after the retrains");
    assert!(
        done_ms > 60_000,
        "transfer ended at {done_ms} ms, before the recovery"
    );

    // Wait for the climb above the dip tier, then escape again.
    let climbed = wait_for(&mut h, 120_000, |h| {
        h.stats().tiers[0]
            .iter()
            .any(|&(ms, t)| ms >= 60_000 && t > slow)
    });
    assert!(
        climbed,
        "no faster tier after 60 s: {:?}",
        h.stats().tiers[0]
    );
    let text = escape_a(&mut h);
    assert!(text.starts_with(OK), "escape text {text:?}");
    let fast = retrain_tier(&text).expect("no RETRAIN after the escape on recovery");
    assert!(
        fast > slow,
        "recovery retrain {fast:?} not faster than {slow:?}"
    );

    // Tier log: SF5, then slower in the dip, then faster after 60 s.
    let log = h.stats().tiers[0].clone();
    assert_eq!(log.first().map(|e| e.1), Some(Tier::Sf5), "{log:?}");
    let dip_low = log
        .iter()
        .filter(|&&(ms, _)| (20_000..60_000).contains(&ms))
        .map(|e| e.1)
        .min()
        .expect("no tier change in the dip");
    assert!(dip_low < Tier::Sf5, "{log:?}");
    assert!(
        log.iter().any(|&(ms, t)| ms >= 60_000 && t > dip_low),
        "{log:?}"
    );

    // AT&V lists the current tier and the retrains.
    h.type_a("AT&V\r");
    h.run_ms(200);
    let a = h.screen_a();
    let tail = &a[a.rfind("AT&V").unwrap()..];
    assert!(tail.contains("\r\nTIER LORA SF"), "{tail:?}");
    assert!(
        tail.contains("RETRAINS") && tail.contains(fast.name()),
        "{tail:?}"
    );

    // Never a NO CARRIER before ATH.
    assert!(h.a.is_connected() && h.b.is_connected());
    assert!(!h.screen_a().contains(NO_CARRIER), "CALLER dropped");
    assert!(!h.screen_b().contains(NO_CARRIER), "AXIS dropped");
    h.type_a("ATH\r");
    h.run_until(10_000, |h| !h.a.is_connected() && !h.b.is_connected());
    assert!(h.screen_a().ends_with(NO_CARRIER));
    assert!(h.screen_b().contains(NO_CARRIER));
}

#[test]
fn link_loss_gives_no_carrier() {
    static STEPS: [(u32, i16); 2] = [(0, 40), (10_000, -160)];
    let mut h = Harness::new(SimChannel {
        profile: SnrProfile::Steps(&STEPS),
        seed: 5,
        drop: None,
    });
    connect(&mut h, 5_000);
    h.send_a_data(b"before the drop");
    h.run_until(10_000, |h| h.now_ms >= 9_000);
    assert!(h.a.is_connected() && h.b.is_connected());
    assert!(!h.screen_a().contains(NO_CARRIER) && !h.screen_b().contains(NO_CARRIER));
    let deadline = 10_000 + S22_MS + 5_000;
    let gone = h.run_until(deadline - h.now_ms, |h| {
        !h.a.is_connected() && !h.b.is_connected()
    });
    assert!(gone, "still connected at {} ms", h.now_ms);
    h.run_ms(100);
    assert!(h.now_ms <= deadline + 100);
    assert!(
        h.screen_a().ends_with(NO_CARRIER),
        "CALLER: {:?}",
        h.screen_a()
    );
    assert!(
        h.screen_b().ends_with(NO_CARRIER),
        "AXIS: {:?}",
        h.screen_b()
    );
    assert_eq!(h.received_b(), b"before the drop");
    h.type_a("AT\r");
    h.type_b("AT\r");
    h.run_ms(100);
    assert!(
        h.screen_a().ends_with(&format!("AT\r{OK}")),
        "{:?}",
        h.screen_a()
    );
    assert!(
        h.screen_b().ends_with(&format!("AT\r{OK}")),
        "{:?}",
        h.screen_b()
    );
}

#[test]
fn hangup_both_sides() {
    let mut h = Harness::new(SimChannel::constant_db(10, 6));
    connect(&mut h, 5_000);
    h.send_a_data(b"hello AXIS");
    let text = escape_a(&mut h);
    assert_eq!(text, OK);
    assert!(h.a.is_connected() && h.b.is_connected());
    h.run_ms(500);
    h.type_a("ATH\r");
    let both = h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected());
    assert!(
        both,
        "CALLER {} AXIS {}",
        h.a.is_connected(),
        h.b.is_connected()
    );
    assert!(
        h.screen_a().ends_with(&format!("ATH\r{NO_CARRIER}")),
        "{:?}",
        h.screen_a()
    );
    assert!(h.screen_b().ends_with(NO_CARRIER), "{:?}", h.screen_b());
    assert_eq!(h.received_b(), b"hello AXIS");
    assert!(!h.a.lights(h.now_ms).cd && !h.b.lights(h.now_ms).cd);
    // A second ATH with no call is just OK.
    h.type_a("ATH\r");
    h.run_ms(50);
    assert!(h.screen_a().ends_with(&format!("ATH\r{OK}")));
    // Both can call again.
    connect(&mut h, 5_000);
}

#[test]
fn flow_control_never_loses_bytes() {
    let mut h = Harness::new(SimChannel::constant_db(10, 7));
    connect(&mut h, 5_000);
    let data = random_bytes(64 * 1024, 71);
    let mut at = 0;
    let mut short = 0;
    let mut idle = 0;
    while at < data.len() {
        let rest = &data[at..];
        let n = h.feed_a(rest);
        assert!(n <= rest.len());
        at += n;
        if n < rest.len() {
            short += 1;
            idle = if n == 0 { idle + 1 } else { 0 };
            assert!(idle < 60_000, "CALLER took nothing for 60 s");
            h.run_ms(1);
        }
    }
    assert!(short > 100, "backpressure only {short} times");
    let ok = wait_for(&mut h, 120_000, |h| h.received_b().len() >= data.len());
    assert!(ok, "{} of {} bytes", h.received_b().len(), data.len());
    assert!(h.received_b() == data, "AXIS data differs");
    assert!(h.a.is_connected() && h.b.is_connected());
}

#[test]
fn plus_plus_plus_under_backpressure_is_data() {
    // SF12: the send window stays full for seconds.
    let mut h = Harness::new(SimChannel::constant_db(-16, 12));
    connect(&mut h, 30_000);
    let mark = h.screen_a_bytes().len();
    let fill = random_bytes(4_000, 121)
        .into_iter()
        .filter(|&b| b != b'+')
        .collect::<Vec<u8>>();
    // Fill until CALLER refuses bytes for longer than the S12 guard.
    let mut at = 0;
    let mut refused_ms = 0;
    while refused_ms <= GUARD_MS {
        let n = h.feed_a(&fill[at..]);
        at += n;
        refused_ms = if n == 0 { refused_ms + 1 } else { 0 };
        assert!(at < fill.len(), "window never filled");
        h.run_ms(1);
    }
    // The host keeps typing: three + then more data, all offered at once.
    let tail: &[u8] = b"+++tail of the stream";
    h.send_a_data(tail);
    let mut sent = fill[..at].to_vec();
    sent.extend_from_slice(tail);
    let ok = wait_for(&mut h, 120_000, |h| h.received_b().len() >= sent.len());
    let since = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
    assert!(!since.contains(OK), "CALLER escaped: {since:?}");
    assert!(
        ok,
        "AXIS got {} of {} bytes",
        h.received_b().len(),
        sent.len()
    );
    assert!(h.received_b() == sent, "AXIS data differs");
}

#[test]
fn plus_plus_plus_without_guard_is_data() {
    let mut h = Harness::new(SimChannel::constant_db(10, 8));
    connect(&mut h, 5_000);
    let mark = h.screen_a_bytes().len();
    h.run_ms(GUARD_MS);
    // No guard before the +++ (a right before it), b right after.
    h.send_a_data(b"a+++b");
    h.run_ms(3_000);
    // Guard before, but data right after the +++.
    h.send_a_data(b"+++b");
    h.run_ms(3_000);
    // +++ then a guard, but data came just before it.
    h.send_a_data(b"c+++");
    h.run_ms(3_000);
    // Separate keystrokes, every gap under the guard: x, 200 ms, then each
    // + 300 ms apart, then y 300 ms after the last +.
    h.send_a_data(b"x");
    h.run_ms(200);
    for _ in 0..3 {
        h.send_a_data(b"+");
        h.run_ms(300);
    }
    h.send_a_data(b"y");
    h.run_ms(3_000);
    assert!(h.a.is_connected());
    let want: &[u8] = b"a+++b+++bc+++x+++y";
    assert!(h.a.is_connected());
    let since = String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned();
    assert!(!since.contains(OK), "CALLER escaped: {since:?}");
    assert!(
        wait_for(&mut h, 10_000, |h| h.received_b().len() >= want.len()),
        "AXIS got {:?}",
        h.received_b()
    );
    assert_eq!(h.received_b(), want);
    // Data mode still carries data, and a proper escape still works.
    h.send_a_data(b"!");
    let text = escape_a(&mut h);
    assert_eq!(text, OK);
    h.run_ms(1_000);
    let mut want = want.to_vec();
    want.push(b'!');
    assert_eq!(h.received_b(), want);
}

static TIERACK_DROPPED: AtomicBool = AtomicBool::new(false);

/// Drops the first TierAck on the air, nothing else.
fn drop_first_tierack(_ms: u32, bytes: &[u8]) -> bool {
    let is_tierack = bytes.first().map(|b| b >> 4) == Some(FrameType::TierAck as u8);
    is_tierack && !TIERACK_DROPPED.swap(true, Ordering::SeqCst)
}

#[test]
fn lost_tierset_ack_resyncs() {
    let mut h = Harness::new(SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 9,
        drop: Some(drop_first_tierack),
    });
    h.type_a("ATDT AXIS\r");
    // Never connected with no traffic for over S22.
    let mut last_frame = 0u32;
    let mut seen = 0usize;
    for _ in 0..60_000 {
        h.run_ms(1);
        let tr = h.trace();
        if tr.len() > seen {
            seen = tr.len();
            last_frame = h.now_ms;
        }
        if h.a.is_connected() || h.b.is_connected() {
            assert!(
                h.now_ms - last_frame <= S22_MS,
                "connected but silent since {last_frame} ms"
            );
        }
        if h.a.is_connected() && h.b.is_connected() {
            break;
        }
        if h.screen_a().contains(NO_CARRIER) && !h.a.is_connected() && !h.b.is_connected() {
            break;
        }
    }
    assert!(TIERACK_DROPPED.load(Ordering::SeqCst), "hook never fired");
    assert_eq!(h.stats().dropped, 1);
    let dropped: Vec<_> = h
        .trace()
        .iter()
        .filter(|e| e.outcome == Outcome::Dropped)
        .collect();
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].ftype, Some(FrameType::TierAck));
    // Seed 9 takes the resync branch: the call survives the lost TierAck.
    assert!(
        h.a.is_connected() && h.b.is_connected(),
        "call did not survive the lost TierAck
CALLER: {:?}
AXIS: {:?}",
        h.screen_a(),
        h.screen_b()
    );
    assert!(!h.screen_a().contains(NO_CARRIER) && !h.screen_b().contains(NO_CARRIER));
    {
        // The call carries data both ways after the lost ack.
        let ca = h.screen_a();
        let cb = h.screen_b();
        let line = |s: &str| {
            s[s.rfind("\r\nCONNECT ").unwrap()..]
                .split("\r\n")
                .nth(1)
                .unwrap()
                .to_string()
        };
        assert_eq!(line(&ca), line(&cb), "both ends must agree on the tier");
        let to_b = random_bytes(2_000, 91);
        let to_a = random_bytes(2_000, 92);
        fed_all_both_ways(&mut h, &to_b, &to_a);
        assert!(wait_for(&mut h, 60_000, |h| {
            h.received_b().len() >= to_b.len() && h.received_a().len() >= to_a.len()
        }));
        assert!(h.received_b() == to_b && h.received_a() == to_a);
        assert!(h.a.is_connected() && h.b.is_connected());
    }
}

#[test]
fn dial_unknown_name_gives_no_answer() {
    let mut h = Harness::new(SimChannel::constant_db(10, 10));
    h.run_ms(100);
    let t0 = h.now_ms;
    h.type_a("ATDT NOBODY\r");
    h.run_until(29_000, |_| false);
    assert!(
        !h.screen_a().contains("NO ANSWER"),
        "too early: {:?}",
        h.screen_a()
    );
    let got = h.run_until(8_000, |h| h.screen_a().contains("\r\nNO ANSWER\r\n"));
    assert!(got, "CALLER: {:?}", h.screen_a());
    let waited = h.now_ms - t0;
    // S7 = 30 s, give a few seconds for the last CallReq period.
    assert!(
        (30_000..=34_000).contains(&waited),
        "NO ANSWER after {waited} ms"
    );
    assert!(!h.screen_b().contains("RING"), "AXIS: {:?}", h.screen_b());
    assert!(!h.a.is_connected() && !h.b.is_connected());
    h.type_a("AT\r");
    h.run_ms(50);
    assert!(
        h.screen_a().ends_with(&format!("AT\r{OK}")),
        "{:?}",
        h.screen_a()
    );
}

#[test]
fn stray_frames_ignored_while_idle() {
    let mut h = Harness::new(SimChannel::constant_db(10, 11));
    let mut r = Rng::new(1111);
    let mut buf = [0u8; 255];
    let mut req = [0u8; 40];
    let n_req = call_req_payload("CALLER", "NOBODY", &mut req).unwrap();
    let kinds: [(FrameType, u8, Vec<u8>); 11] = [
        (FrameType::Data, 0, random_bytes(200, 111)),
        (FrameType::Data, 1, b"stray".to_vec()),
        (FrameType::Ack, 0, vec![0x0F]),
        (FrameType::Keepalive, 2, vec![0x10, 0x00]),
        (FrameType::TierSet, 0, vec![7]),
        (FrameType::TierAck, 0, vec![3]),
        (FrameType::Hangup, 0, vec![]),
        (FrameType::CallAck, 0, vec![]),
        (FrameType::Train, 0, vec![7, 40, 0]),
        (FrameType::CallReject, 0, vec![]),
        (FrameType::CallReq, 0, req[..n_req].to_vec()),
    ];
    for round in 0..4u8 {
        for (ftype, flags, payload) in &kinds {
            let hdr = Header {
                ftype: *ftype,
                flags: *flags,
                call_id: r.next_u32() as u8,
                seq: round,
                ack: 0,
            };
            let n = encode(&hdr, payload, &mut buf).unwrap();
            let tier = Tier::LADDER[(r.next_u32() % 8) as usize];
            h.inject_b(&buf[..n], tier);
            h.inject_b(&buf[..n], Tier::Sf12);
            h.run_ms(40);
        }
    }
    h.run_ms(5_000);
    assert_eq!(h.screen_b(), "", "AXIS printed something");
    assert!(h.trace().is_empty(), "AXIS sent {:?}", h.trace());
    assert!(!h.b.is_connected());
    assert!(!h.b.lights(h.now_ms).oh);
    // A real call still works.
    connect(&mut h, 5_000);
    assert!(
        h.screen_b().contains(CONNECT_SF5),
        "AXIS: {:?}",
        h.screen_b()
    );
    h.send_a_data(b"real call");
    assert!(wait_for(&mut h, 5_000, |h| h.received_b().len() >= 9));
    assert_eq!(h.received_b(), b"real call");
}

/// Header length: the first payload byte of a frame is at this index.
const HDR: usize = 4;

/// The frame type nibble of raw frame bytes.
fn ftype_of(bytes: &[u8]) -> Option<u8> {
    bytes.first().map(|b| b >> 4)
}

/// `(send ms, tier index asked)` of every TierSet on the air, per test.
static CAP_CALLER_TIERSETS: Mutex<Vec<(u32, u8)>> = Mutex::new(Vec::new());
static CAP_AXIS_TIERSETS: Mutex<Vec<(u32, u8)>> = Mutex::new(Vec::new());

fn log_tierset(log: &Mutex<Vec<(u32, u8)>>, ms: u32, bytes: &[u8]) {
    if ftype_of(bytes) == Some(FrameType::TierSet as u8) {
        if let Some(&i) = bytes.get(HDR) {
            log.lock().unwrap().push((ms, i));
        }
    }
}

fn log_caller_cap(ms: u32, bytes: &[u8]) -> bool {
    log_tierset(&CAP_CALLER_TIERSETS, ms, bytes);
    false
}

fn log_axis_cap(ms: u32, bytes: &[u8]) -> bool {
    log_tierset(&CAP_AXIS_TIERSETS, ms, bytes);
    false
}

fn cfg(name: &str, regs: &[(u8, u16)]) -> Config {
    let mut c = Config::new(name).unwrap();
    for &(n, v) in regs {
        assert!(c.sregs.set(n, v), "S{n}={v}");
    }
    c
}

/// Connects on a clean +12 dB channel (best tier SF5), checks CONNECT on
/// both sides at `cap`, then runs over 70 s with data both ways and checks
/// that no frame ever went faster than `cap`.
fn capped_call(
    a: Config,
    b: Config,
    cap: Tier,
    hook: fn(u32, &[u8]) -> bool,
    seed: u64,
) -> Harness {
    let mut h = Harness::with_configs(
        SimChannel {
            profile: SnrProfile::Constant(48),
            seed,
            drop: Some(hook),
        },
        a,
        b,
    );
    connect(&mut h, 10_000);
    let want = format!("\r\nCONNECT {} {}/ARQ\r\n", cap.name(), cap.air_bps());
    assert!(h.screen_a().contains(&want), "CALLER: {:?}", h.screen_a());
    assert!(h.screen_b().contains(&want), "AXIS: {:?}", h.screen_b());
    assert_eq!(h.a.lights(h.now_ms).tier, Some(cap));
    // Over 70 s: the SNR is good for faster tiers, so the 10 s up hold
    // runs out several times on any side whose cap allows it.
    let to_b = random_bytes(4_000, seed * 10 + 1);
    let to_a = random_bytes(4_000, seed * 10 + 2);
    fed_all_both_ways(&mut h, &to_b, &to_a);
    h.run_ms(70_000);
    assert!(
        h.received_b() == to_b && h.received_a() == to_a,
        "data differs"
    );
    for e in h.trace() {
        assert!(e.tier <= cap, "frame above the cap: {e:?}");
    }
    for side in 0..2 {
        let log = &h.stats().tiers[side];
        assert!(log.iter().all(|&(_, t)| t <= cap), "side {side}: {log:?}");
        assert_eq!(log.last().map(|e| e.1), Some(cap), "side {side}: {log:?}");
    }
    assert!(h.a.is_connected() && h.b.is_connected());
    assert!(!h.screen_a().contains("RETRAIN") && !h.screen_b().contains("RETRAIN"));
    h
}

#[test]
fn caller_tier_cap_holds_against_answerer_up_retrain() {
    // Ruling R9: CALLER S21 = SF7 on a channel good for SF5. CONNECT lands
    // at SF7, and AXIS (own S21 = SF5) asking for SF6 later is acked at SF7.
    let cap = Tier::Sf7;
    let a = cfg("CALLER", &[(21, u16::from(cap.index()))]);
    let b = cfg("AXIS", &[(0, 1)]);
    let h = capped_call(a, b, cap, log_caller_cap, 13);
    let connect_ms = h.stats().tiers[0].first().map(|e| e.0).unwrap();
    let log = CAP_CALLER_TIERSETS.lock().unwrap().clone();
    let ups: Vec<_> = log
        .iter()
        .filter(|&&(ms, i)| ms > connect_ms && i > cap.index())
        .collect();
    // AXIS really tried to go up (so the cap was tested), only one tier,
    // and stopped once CALLER acked its cap.
    assert!(!ups.is_empty(), "AXIS never asked above the cap: {log:?}");
    assert!(ups.len() <= 6, "AXIS kept asking: {log:?}");
    assert!(ups.iter().all(|&&(_, i)| i == cap.index() + 1), "{log:?}");
}

#[test]
fn answerer_tier_cap_lower_than_caller() {
    // AXIS S21 = SF8, CALLER default (SF5): CONNECT at SF8, and CALLER
    // learned the cap from Train, so nobody ever asks above it.
    let cap = Tier::Sf8;
    let a = cfg("CALLER", &[]);
    let b = cfg("AXIS", &[(0, 1), (21, u16::from(cap.index()))]);
    let _h = capped_call(a, b, cap, log_axis_cap, 14);
    let log = CAP_AXIS_TIERSETS.lock().unwrap().clone();
    assert!(!log.is_empty());
    assert!(
        log.iter().all(|&(_, i)| i <= cap.index()),
        "TierSet above the cap: {log:?}"
    );
}

#[test]
fn manual_answer_with_s0_zero() {
    let a = cfg("CALLER", &[]);
    let b = cfg("AXIS", &[]);
    let mut h = Harness::with_configs(SimChannel::constant_db(12, 15), a, b);
    // ATA with nothing ringing: NO CARRIER.
    h.type_b("ATA\r");
    h.run_ms(50);
    assert_eq!(h.screen_b(), format!("ATA\r{NO_CARRIER}"));
    let mark = h.screen_b_bytes().len();
    h.type_a("ATDT AXIS\r");
    // RING every 3 s, but S0=0 never answers by itself.
    let rang = h.run_until(10_000, |h| {
        h.screen_b()[mark..].matches("\r\nRING\r\n").count() >= 3
    });
    assert!(rang, "AXIS: {:?}", h.screen_b());
    assert!(
        h.trace().iter().all(|e| e.from == 0),
        "AXIS sent {:?}",
        h.trace()
    );
    assert!(!h.a.is_connected() && !h.b.is_connected());
    assert!(!h.b.lights(h.now_ms).aa);
    assert!(!h.screen_a().contains("CONNECT") && !h.screen_b().contains("CONNECT"));
    h.type_b("ATA\r");
    let ok = h.run_until(10_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "CALLER: {:?}\nAXIS: {:?}", h.screen_a(), h.screen_b());
    let b_text = h.screen_b();
    let ata = b_text.rfind("ATA\r").unwrap();
    assert!(b_text[ata..].contains(CONNECT_SF5), "AXIS: {b_text:?}");
    assert!(
        h.screen_a().contains(CONNECT_SF5),
        "CALLER: {:?}",
        h.screen_a()
    );
    h.send_a_data(b"answered by hand");
    assert!(wait_for(&mut h, 5_000, |h| h.received_b().len() >= 16));
    assert_eq!(h.received_b(), b"answered by hand");
}

/// A radio that keeps what it sends, for a third modem outside the harness.
#[derive(Default)]
struct CaptureRadio {
    sent: Vec<Vec<u8>>,
}

impl Radio for CaptureRadio {
    fn send(&mut self, _tier: Tier, frame: &[u8]) {
        self.sent.push(frame.to_vec());
    }
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

fn drain(m: &mut Modem) -> String {
    let mut all = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = m.host_output(&mut buf);
        if n == 0 {
            return String::from_utf8_lossy(&all).into_owned();
        }
        all.extend_from_slice(&buf[..n]);
    }
}

/// Every CallReject on the air, bytes as sent.
static BUSY_REJECTS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn keep_call_rejects(_ms: u32, bytes: &[u8]) -> bool {
    if ftype_of(bytes) == Some(FrameType::CallReject as u8) {
        BUSY_REJECTS.lock().unwrap().push(bytes.to_vec());
    }
    false
}

#[test]
fn third_caller_gets_busy() {
    let mut h = Harness::new(SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 16,
        drop: Some(keep_call_rejects),
    });
    connect(&mut h, 5_000);
    let live = h.trace().last().map(|e| e.call_id).unwrap();
    // THIRD dials AXIS. The harness has two radios, so THIRD's CallReq is
    // handed to AXIS directly; its call id must differ from the live one.
    let mut third = Modem::new(Config::new("THIRD").unwrap());
    let mut radio = CaptureRadio::default();
    let mut t = h.now_ms;
    if (t as u8 | 1) == live {
        t += 2;
    }
    third.host_input(b"ATDT AXIS\r", &mut radio, t);
    let req = radio.sent.first().cloned().expect("THIRD sent no CallReq");
    assert_eq!(ftype_of(&req), Some(FrameType::CallReq as u8));
    let id = req[1];
    assert_ne!(id, live);
    h.inject_b(&req, Tier::Sf12);
    let rejected = h.run_until(5_000, |h| {
        h.trace()
            .iter()
            .any(|e| e.from == 1 && e.ftype == Some(FrameType::CallReject) && e.call_id == id)
    });
    assert!(rejected, "AXIS sent no CallReject: {:?}", h.trace());
    // The CallReject reaches THIRD: BUSY, and THIRD is idle again.
    let rej = BUSY_REJECTS
        .lock()
        .unwrap()
        .iter()
        .find(|f| f.get(1) == Some(&id))
        .cloned()
        .expect("CallReject bytes not captured");
    let info = RxInfo {
        tier: Tier::Sf12,
        rssi_dbm: -110,
        snr_x4: 40,
    };
    third.radio_rx(&rej, info, &mut radio, t + 3_000);
    let screen = drain(&mut third);
    assert!(
        screen.starts_with("ATDT AXIS\r\r\nDIALING AXIS\r\n") && screen.ends_with("\r\nBUSY\r\n"),
        "THIRD: {screen:?}"
    );
    assert!(!third.lights(t + 3_000).oh);
    // AXIS printed nothing about it and the live call is unaffected.
    assert_eq!(
        h.screen_b().matches("\r\nRING\r\n").count(),
        1,
        "AXIS: {:?}",
        h.screen_b()
    );
    let to_b = random_bytes(3_000, 161);
    let to_a = random_bytes(3_000, 162);
    fed_all_both_ways(&mut h, &to_b, &to_a);
    assert!(wait_for(&mut h, 60_000, |h| {
        h.received_b().len() >= to_b.len() && h.received_a().len() >= to_a.len()
    }));
    assert!(h.received_b() == to_b && h.received_a() == to_a);
    assert!(h.a.is_connected() && h.b.is_connected());
    assert!(!h.screen_a().contains(NO_CARRIER) && !h.screen_b().contains(NO_CARRIER));
}

#[test]
fn numeric_result_codes() {
    let mut h = Harness::new(SimChannel::constant_db(12, 17));
    h.type_a("ATV0\r");
    h.run_ms(50);
    assert_eq!(h.screen_a(), "ATV0\r0\r");
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(5_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "CALLER: {:?}", h.screen_a());
    h.run_ms(100);
    // No DIALING, RINGING or TRAINING lines in numeric mode: just 1.
    assert_eq!(h.screen_a(), "ATV0\r0\rATDT AXIS\r1\r");
    let esc = escape_a(&mut h);
    assert_eq!(esc, "0\r");
    h.type_a("ATH\r");
    h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected());
    assert_eq!(h.screen_a(), "ATV0\r0\rATDT AXIS\r1\r0\rATH\r3\r");
    assert!(
        h.screen_b().ends_with(NO_CARRIER),
        "AXIS: {:?}",
        h.screen_b()
    );
    // A call the far end drops ends with 3 too, and an error is 4.
    connect(&mut h, 5_000);
    let mark = h.screen_a_bytes().len();
    h.run_ms(GUARD_MS);
    h.type_b("+++");
    h.run_ms(GUARD_MS);
    h.type_b("ATH\r");
    h.run_until(5_000, |h| !h.a.is_connected());
    h.run_ms(100);
    assert_eq!(&h.screen_a()[mark..], "3\r");
    h.type_a("ATX\r");
    h.run_ms(50);
    assert!(h.screen_a().ends_with("3\rATX\r4\r"), "{:?}", h.screen_a());
}

#[test]
fn atz_in_a_call_hangs_up_and_resets() {
    let mut h = Harness::new(SimChannel::constant_db(12, 18));
    h.type_a("ATS7=45\rATS20=40\rATS21=6\rATS22=33\r");
    h.run_ms(50);
    connect(&mut h, 5_000);
    h.send_a_data(b"before ATZ");
    let esc = escape_a(&mut h);
    assert!(esc.starts_with(OK), "{esc:?}");
    let mark = h.screen_a_bytes().len();
    h.type_a("ATZ\r");
    let gone = h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected());
    assert!(
        gone,
        "CALLER {} AXIS {}",
        h.a.is_connected(),
        h.b.is_connected()
    );
    h.run_ms(100);
    // ATZ hangs up silently on its own side; the far end sees NO CARRIER.
    assert_eq!(&h.screen_a()[mark..], format!("ATZ\r{OK}"));
    assert!(
        h.screen_b().ends_with(NO_CARRIER),
        "AXIS: {:?}",
        h.screen_b()
    );
    assert_eq!(h.received_b(), b"before ATZ");
    // S registers are back to the defaults.
    let mark = h.screen_a_bytes().len();
    h.type_a("ATS7?\rAT&V\r");
    h.run_ms(50);
    let text = h.screen_a()[mark..].to_string();
    assert!(text.starts_with("ATS7?\r\r\n030\r\n\r\nOK\r\n"), "{text:?}");
    assert!(
        text.contains("S00=000 S02=043 S07=030 S12=050 S20=020 S21=007 S22=020"),
        "{text:?}"
    );
    assert!(text.contains("\r\nTIER NONE\r\n"), "{text:?}");
    // And a fresh call connects at the uncapped tier.
    connect(&mut h, 5_000);
    assert!(
        h.screen_a()[mark..].contains(CONNECT_SF5),
        "{:?}",
        h.screen_a()
    );
}

#[test]
fn ata_online_is_error_and_call_stays_up() {
    let mut h = Harness::new(SimChannel::constant_db(12, 19));
    connect(&mut h, 5_000);
    let esc = escape_a(&mut h);
    assert_eq!(esc, OK);
    let mark = h.screen_a_bytes().len();
    h.type_a("ATA\r");
    h.run_ms(2_000);
    assert_eq!(&h.screen_a()[mark..], "ATA\r\r\nERROR\r\n");
    assert!(h.a.is_connected() && h.b.is_connected());
    h.type_a("ATO\r");
    h.run_ms(100);
    assert!(
        h.screen_a()[mark..].ends_with(CONNECT_SF5),
        "{:?}",
        h.screen_a()
    );
    h.send_a_data(b"still here");
    assert!(wait_for(&mut h, 5_000, |h| h.received_b().len() >= 10));
    assert_eq!(h.received_b(), b"still here");
    assert!(!h.screen_b().contains(NO_CARRIER));
}

#[test]
fn partial_line_before_connect_is_dropped() {
    let a = cfg("CALLER", &[]);
    let b = cfg("AXIS", &[(0, 2)]);
    let mut h = Harness::with_configs(SimChannel::constant_db(12, 20), a, b);
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(5_000, |h| h.screen_b().contains("\r\nRING\r\n")));
    // AXIS's host starts a line during the first RING and never ends it.
    h.type_b("AT");
    let ok = h.run_until(10_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "AXIS: {:?}", h.screen_b());
    assert!(
        h.screen_b().contains("RING\r\nAT\r\nRING\r\n"),
        "{:?}",
        h.screen_b()
    );
    // On the line after the escape, ATI is ATI (not "ATATI", an ERROR).
    h.run_ms(GUARD_MS);
    h.type_b("+++");
    h.run_ms(GUARD_MS);
    let mark = h.screen_b_bytes().len();
    h.type_b("ATI\r");
    h.run_ms(50);
    let text = h.screen_b()[mark..].to_string();
    assert!(
        text.starts_with("ATI\r\r\nLORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM\r\n")
            && text.ends_with(OK),
        "{text:?}"
    );
    // The same for a line started on CALLER while it dials.
    h.type_b("ATH\r");
    h.run_until(5_000, |h| !h.a.is_connected() && !h.b.is_connected());
    h.run_ms(500);
    h.type_a("ATDT AXIS\r");
    h.type_a("AT");
    let ok = h.run_until(15_000, |h| h.a.is_connected() && h.b.is_connected());
    assert!(ok, "CALLER: {:?}", h.screen_a());
    let esc = escape_a(&mut h);
    assert!(esc.starts_with(OK), "{esc:?}");
    let mark = h.screen_a_bytes().len();
    h.type_a("ATI\r");
    h.run_ms(50);
    let text = h.screen_a()[mark..].to_string();
    assert!(
        text.starts_with("ATI\r\r\nLORA FLOWTRON") && text.ends_with(OK),
        "{text:?}"
    );
}
