//! LFD3K-LINK reports on a bridged radio call (a radio caller reaching the
//! AXIS board through this modem): the first one when the server says
//! DO 90, one on every retrain, one every 10 s otherwise, never two within
//! 1 s. Times are the simulator's own clock, fed to the core.

use lfd3k_sim::{FakeWire, Harness, SimChannel, SnrProfile, WireScript};

const IAC: u8 = 255;
const SB: u8 = 250;
const WILL90: [u8; 3] = [IAC, 251, 90];
const DO90: [u8; 3] = [IAC, 253, 90];

fn rig(chan: SimChannel) -> Harness {
    let mut h = Harness::new(chan);
    h.attach_wire_b(FakeWire::new(WireScript::Telnet));
    h.type_b("AT+FWD=BBS,192.168.1.164,23\r");
    h.type_b("AT+PUB=BBS\r");
    h.run_ms(50);
    assert!(h.screen_b().ends_with("\r\nOK\r\n"), "{:?}", h.screen_b());
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| {
        h.screen_a().contains("CONNECT LORA") && h.screen_b().contains("BRIDGE BBS")
    });
    assert!(ok, "A: {:?}\nB: {:?}", h.screen_a(), h.screen_b());
    h
}

/// Every `IAC SB 90 ... IAC SE` in the raw bytes, as (offset, payload).
fn reports(raw: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= raw.len() {
        if raw[i..i + 3] == [IAC, SB, 90] {
            let end = raw[i + 3..]
                .windows(2)
                .position(|w| w == [IAC, 240])
                .expect("IAC SE");
            let text = String::from_utf8(raw[i + 3..i + 3 + end].to_vec()).unwrap();
            out.push((i, text));
            i += 3 + end + 2;
        } else {
            i += 1;
        }
    }
    out
}

fn retrains_of(report: &str) -> u32 {
    report
        .split(' ')
        .find_map(|f| f.strip_prefix("retrains="))
        .and_then(|v| v.parse().ok())
        .expect("a retrains count")
}

fn tier_of(report: &str) -> &str {
    report
        .split(' ')
        .find_map(|f| f.strip_prefix("tier="))
        .expect("a tier")
}

/// Runs `ms` more, noting the time each report arrives at the server and
/// keeping the radio call busy with a few bytes every 500 ms.
fn watch(h: &mut Harness, ms: u32, times: &mut Vec<u32>, seen: &mut usize) {
    let end = h.now_ms + ms;
    while h.now_ms < end {
        if h.now_ms.is_multiple_of(500) {
            h.send_a_data(b"x");
        }
        h.run_ms(1);
        let n = reports(&h.wire_received_b()).len();
        while *seen < n {
            times.push(h.now_ms);
            *seen += 1;
        }
    }
}

fn server_says_do(h: &mut Harness) {
    assert!(h.run_until(5_000, |h| {
        h.wire_received_b().windows(3).any(|w| w == WILL90)
    }));
    h.wire_b_mut().unwrap().send(&DO90);
}

#[test]
fn link_stats_rate_limited() {
    // A steady call: a report when DO 90 arrives, then one every 10 s.
    let mut h = rig(SimChannel::constant_db(12, 3));
    assert!(
        reports(&h.wire_received_b()).is_empty(),
        "none before DO 90"
    );
    let t_do = h.now_ms;
    server_says_do(&mut h);
    let (mut times, mut seen) = (Vec::new(), 0);
    watch(&mut h, 35_000, &mut times, &mut seen);
    assert_eq!(times.len(), 4, "reports at {times:?} (DO at {t_do})");
    assert!(
        times[0] - t_do < 300,
        "first at {} after DO",
        times[0] - t_do
    );
    for w in times.windows(2) {
        let gap = w[1] - w[0];
        assert!((10_000..=10_100).contains(&gap), "period gap {gap}");
    }
    let all = reports(&h.wire_received_b());
    assert!(all.iter().all(|(_, r)| tier_of(r) == tier_of(&all[0].1)));

    // A call whose SNR dips and recovers: retrains add reports between the
    // periodic ones, but never closer than 1 s, and the report carries the
    // new retrain count.
    static STEPS: [(u32, i16); 3] = [(0, 40), (30_000, -48), (60_000, 40)];
    let mut h = rig(SimChannel {
        profile: SnrProfile::Steps(&STEPS),
        seed: 4,
        drop: None,
    });
    server_says_do(&mut h);
    let (mut times, mut seen) = (Vec::new(), 0);
    watch(&mut h, 150_000, &mut times, &mut seen);
    let all = reports(&h.wire_received_b());
    assert_eq!(all.len(), times.len());
    for w in times.windows(2) {
        let gap = w[1] - w[0];
        assert!(gap >= 1_000, "two reports {gap} ms apart: {times:?}");
        assert!(gap <= 10_100, "a report is {gap} ms late: {times:?}");
    }
    let mut early = 0;
    for (w, p) in times.windows(2).zip(all.windows(2)) {
        if retrains_of(&p[0].1) != retrains_of(&p[1].1) {
            early += 1;
            assert!(
                w[1] - w[0] < 10_000,
                "a retrain waited for the period: {times:?}"
            );
        }
    }
    assert!(early >= 2, "no retrain reports (down and up): {all:?}");
    // And the periodic ones kept coming: at least one per 10 s overall.
    assert!(times.len() >= 15, "{times:?}");
}
