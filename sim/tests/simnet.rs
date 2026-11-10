//! Phase 3: the simulated radio over datagrams (`SimNetRadio`), alone with a
//! hand-driven clock, and end to end between two modems over real UDP on
//! 127.0.0.1 (ephemeral ports, so parallel runs never collide).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lfd3k_core::simnet::{build, parse, DGRAM_MAX, FRAME_MAX, HDR_LEN, RSSI_FLOOR, VERSION};
use lfd3k_core::{Config, DatagramPort, Modem, Radio, SimNetRadio, Tier};
use lfd3k_sim::UdpPort;
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};

/// A datagram port with a clock the test moves and a record of what was sent.
#[derive(Clone)]
struct MockPort {
    now_us: Rc<Cell<u64>>,
    sent: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl MockPort {
    fn new() -> MockPort {
        MockPort {
            now_us: Rc::new(Cell::new(0)),
            sent: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn set(&self, us: u64) {
        self.now_us.set(us);
    }
}

impl DatagramPort for MockPort {
    fn send(&mut self, d: &[u8]) {
        self.sent.borrow_mut().push(d.to_vec());
    }

    fn now_us(&self) -> u64 {
        self.now_us.get()
    }
}

fn dgram(tier: Tier, snr_x4: i16, rssi: i16, frame: &[u8]) -> Vec<u8> {
    let mut d = [0u8; DGRAM_MAX];
    let n = build(tier, snr_x4, rssi, frame, &mut d);
    assert!(n > 0);
    d[..n].to_vec()
}

fn frame(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

fn rig() -> (MockPort, SimNetRadio<MockPort>) {
    let p = MockPort::new();
    (p.clone(), SimNetRadio::new(p))
}

#[test]
fn simnet_frame_delivered_after_airtime() {
    let (clock, mut r) = rig();
    r.listen(Tier::Sf5);
    let f = frame(40);
    let air = u64::from(Tier::Sf5.airtime_us(40));
    assert!(air > 0);
    clock.set(1_000);
    r.on_datagram(&dgram(Tier::Sf5, -12, -97, &f));
    let mut buf = [0u8; 255];

    // From arrival on, the radio is busy (a receive in progress), the RSSI
    // is the frame's, and the frame is not handed over early.
    assert!(r.busy());
    assert_eq!(r.rx_rssi(), -97);
    assert_eq!(r.next_due_us(), Some(air));
    clock.set(1_000 + air - 1);
    assert!(r.service(&mut buf).is_none(), "one microsecond early");
    assert!(r.busy());

    // At arrival plus airtime it is delivered whole, with the sender's
    // SNR and RSSI and the tier it was sent on.
    clock.set(1_000 + air);
    let (n, info) = r.service(&mut buf).expect("delivered on time");
    assert_eq!(&buf[..n], &f[..]);
    assert_eq!(
        (info.tier, info.snr_x4, info.rssi_dbm),
        (Tier::Sf5, -12, -97)
    );
    assert!(!r.busy());
    assert_eq!(r.rx_rssi(), RSSI_FLOOR);
    assert_eq!(r.next_due_us(), None);
    assert!(r.service(&mut buf).is_none(), "only once");

    // Longer frames take longer: airtime is the real one for the length.
    let long = frame(200);
    let air2 = u64::from(Tier::Sf5.airtime_us(200));
    assert!(air2 > air);
    clock.set(5_000);
    r.on_datagram(&dgram(Tier::Sf5, 0, -80, &long));
    clock.set(5_000 + air2 - 1);
    assert!(r.service(&mut buf).is_none());
    clock.set(5_000 + air2);
    assert_eq!(r.service(&mut buf).map(|(n, _)| n), Some(200));

    // Two back to back are received one after the other (no collisions).
    clock.set(50_000);
    r.on_datagram(&dgram(Tier::Sf5, 0, -80, &f));
    r.on_datagram(&dgram(Tier::Sf5, 0, -81, &f));
    clock.set(50_000 + air);
    assert_eq!(r.service(&mut buf).map(|(n, _)| n), Some(40));
    assert!(
        r.service(&mut buf).is_none(),
        "the second is still on the air"
    );
    clock.set(50_000 + 2 * air);
    assert_eq!(r.service(&mut buf).map(|(_, i)| i.rssi_dbm), Some(-81));
    // A third while two are held is dropped.
    clock.set(90_000);
    for _ in 0..3 {
        r.on_datagram(&dgram(Tier::Sf5, 0, -80, &f));
    }
    clock.set(90_000 + 10 * air);
    assert!(r.service(&mut buf).is_some());
    assert!(r.service(&mut buf).is_some());
    assert!(r.service(&mut buf).is_none(), "at most two are held");
}

#[test]
fn simnet_wrong_tier_dropped() {
    let (clock, mut r) = rig();
    r.listen(Tier::Sf12);
    let mut buf = [0u8; 255];
    clock.set(100);
    r.on_datagram(&dgram(Tier::Sf5, 0, -80, &frame(30)));
    assert!(!r.busy(), "a frame on another SF is never heard");
    assert_eq!(r.next_due_us(), None);
    clock.set(10_000_000);
    assert!(r.service(&mut buf).is_none());
    // Switching to its tier afterwards does not bring it back.
    r.listen(Tier::Sf5);
    assert!(r.service(&mut buf).is_none());

    // Listening on a tier and then moving away loses a held frame.
    let air = u64::from(Tier::Sf5.airtime_us(30));
    clock.set(20_000_000);
    r.on_datagram(&dgram(Tier::Sf5, 0, -80, &frame(30)));
    assert!(r.busy());
    r.listen(Tier::Sf9);
    clock.set(20_000_000 + air);
    assert!(r.service(&mut buf).is_none());
    assert!(!r.busy());

    // The gap after our own send: not listening yet, a frame arrives and is
    // held; it is delivered only if the radio then listens on its tier.
    let f = frame(30);
    let mut r2 = SimNetRadio::new(clock.clone());
    clock.set(0);
    r2.send(Tier::Sf5, &frame(10));
    clock.set(u64::from(Tier::Sf5.airtime_us(10)));
    assert!(!r2.transmitting());
    r2.on_datagram(&dgram(Tier::Sf7, 0, -80, &f));
    r2.listen(Tier::Sf7);
    let t = clock.now_us() + u64::from(Tier::Sf7.airtime_us(30));
    clock.set(t);
    assert_eq!(
        r2.service(&mut buf).map(|(n, i)| (n, i.tier)),
        Some((30, Tier::Sf7))
    );
    // Same but it listens on another tier: not delivered.
    let mut r3 = SimNetRadio::new(clock.clone());
    r3.on_datagram(&dgram(Tier::Sf7, 0, -80, &f));
    r3.listen(Tier::Sf5);
    clock.set(clock.now_us() + 10_000_000);
    assert!(r3.service(&mut buf).is_none());
}

#[test]
fn simnet_malformed_datagram_ignored() {
    let good = dgram(Tier::Sf5, 8, -70, &frame(20));
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("one byte", vec![b'L']),
        ("header only, no frame", good[..HDR_LEN].to_vec()),
        ("short header", good[..HDR_LEN - 1].to_vec()),
        ("frame too long", {
            let mut d = good[..HDR_LEN].to_vec();
            d.extend(std::iter::repeat_n(0xAA, FRAME_MAX + 1));
            d
        }),
    ];
    for (what, i, v) in [
        ("magic 0", 0, b'X'),
        ("magic 1", 1, b'X'),
        ("version", 2, VERSION + 1),
        ("version 0", 2, 0),
        ("tier 8", 3, 8),
        ("tier 255", 3, 255),
    ] {
        let mut d = good.clone();
        d[i] = v;
        cases.push((what, d));
    }
    for (what, d) in &cases {
        assert!(parse(d).is_none(), "{what}");
        let (clock, mut r) = rig();
        r.listen(Tier::Sf5);
        r.on_datagram(d);
        assert!(!r.busy(), "{what}");
        assert_eq!(r.next_due_us(), None, "{what}: nothing held");
        clock.set(100_000_000);
        let mut buf = [0u8; 255];
        assert!(r.service(&mut buf).is_none(), "{what}");
    }
    // The longest good one is accepted, and build refuses the bad sizes.
    let max = dgram(Tier::Sf5, 0, 0, &frame(FRAME_MAX));
    assert_eq!(max.len(), DGRAM_MAX);
    assert_eq!(parse(&max).map(|p| p.3.len()), Some(FRAME_MAX));
    let mut out = [0u8; DGRAM_MAX];
    assert_eq!(build(Tier::Sf5, 0, 0, &[], &mut out), 0);
    assert_eq!(build(Tier::Sf5, 0, 0, &[0u8; FRAME_MAX + 1], &mut out), 0);
    // A flood of junk between good datagrams changes nothing.
    let (clock, mut r) = rig();
    r.listen(Tier::Sf5);
    for (_, d) in &cases {
        r.on_datagram(d);
    }
    r.on_datagram(&good);
    clock.set(1_000_000);
    let mut buf = [0u8; 255];
    assert_eq!(r.service(&mut buf).map(|(n, _)| n), Some(20));
}

#[test]
fn simnet_rx_during_tx_lost() {
    let (clock, mut r) = rig();
    r.listen(Tier::Sf5);
    let mut buf = [0u8; 255];
    let f = frame(60);
    let air = u64::from(Tier::Sf5.airtime_us(60));

    // Our send goes out at once as a datagram that parses back.
    clock.set(1_000);
    r.send(Tier::Sf5, &f);
    {
        let sent = clock.sent.borrow();
        assert_eq!(sent.len(), 1);
        let (t, snr, rssi, body) = parse(&sent[0]).expect("a datagram");
        assert_eq!((t, snr, rssi, body), (Tier::Sf5, 32, -70, &f[..]));
    }
    assert!(r.transmitting() && r.busy());
    assert_eq!(r.next_due_us(), Some(air));

    // A frame arriving while we transmit is lost, even after the end.
    clock.set(1_000 + air / 2);
    r.on_datagram(&dgram(Tier::Sf5, 0, -60, &frame(20)));
    clock.set(1_000 + air);
    assert!(r.service(&mut buf).is_none(), "tx done, nothing was held");
    assert!(!r.transmitting() && !r.busy());
    r.listen(Tier::Sf5);
    clock.set(1_000 + 20 * air);
    assert!(r.service(&mut buf).is_none(), "it was lost, not queued");

    // After the transmission a new one is received normally.
    clock.set(2_000_000);
    r.on_datagram(&dgram(Tier::Sf5, 0, -60, &frame(20)));
    clock.set(3_000_000);
    assert_eq!(r.service(&mut buf).map(|(n, _)| n), Some(20));

    // Sending also throws away what was already held (half duplex).
    clock.set(4_000_000);
    r.on_datagram(&dgram(Tier::Sf5, 0, -60, &frame(20)));
    assert!(r.busy());
    r.send(Tier::Sf5, &frame(5));
    clock.set(9_000_000);
    r.listen(Tier::Sf5);
    assert!(
        r.service(&mut buf).is_none(),
        "the held frame died with our send"
    );

    // set_peer_hears changes what the peer will hear of us.
    r.set_peer_hears(-20, -100);
    r.send(Tier::Sf5, &frame(5));
    let sent = clock.sent.borrow();
    let (_, snr, rssi, _) = parse(sent.last().unwrap()).unwrap();
    assert_eq!((snr, rssi), (-20, -100));
    // A frame that cannot be carried is not sent.
    drop(sent);
    let n = clock.sent.borrow().len();
    r.send(Tier::Sf5, &[]);
    r.send(Tier::Sf5, &[0u8; FRAME_MAX + 1]);
    assert_eq!(clock.sent.borrow().len(), n);
}

fn cfg() -> ProptestConfig {
    ProptestConfig {
        cases: 2_000,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0x51_4E_E7),
        ..ProptestConfig::default()
    }
}

fn datagram_like() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Anything at all.
        proptest::collection::vec(any::<u8>(), 0..300),
        // A plausible header with a random tail and random fields.
        (
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            0u8..12,
            any::<[u8; 4]>(),
            proptest::collection::vec(any::<u8>(), 0..270)
        )
            .prop_map(|(m0, m1, v, t, rest, tail)| {
                let mut d = if (m0 ^ m1) & 1 == 0 {
                    vec![b'L', b'S', VERSION]
                } else {
                    vec![m0, m1, v]
                };
                d.push(t);
                d.extend_from_slice(&rest);
                d.extend_from_slice(&tail);
                d
            }),
    ]
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn simnet_random_datagrams_never_panic(
        ds in proptest::collection::vec(datagram_like(), 1..6),
        listen in 0u8..8,
        gaps in proptest::collection::vec(0u64..400_000, 1..12),
    ) {
        let (clock, mut r) = rig();
        let tier = Tier::from_index(listen).unwrap();
        r.listen(tier);
        let mut buf = [0u8; 255];
        let mut t = 0u64;
        for (i, d) in ds.iter().enumerate() {
            t += gaps.get(i).copied().unwrap_or(1);
            clock.set(t);
            r.on_datagram(d);
            let _ = r.busy();
            let _ = r.rx_rssi();
            let _ = r.next_due_us();
            if let Some((n, info)) = r.service(&mut buf) {
                // Whatever is delivered is a whole, valid, short enough frame
                // on the tier we listen on.
                prop_assert!((1..=FRAME_MAX).contains(&n));
                prop_assert_eq!(info.tier, tier);
            }
            if i % 3 == 0 {
                r.send(tier, d.get(..d.len().min(FRAME_MAX)).unwrap_or(&[]));
            }
        }
        clock.set(t + 100_000_000);
        while let Some((n, _)) = r.service(&mut buf) {
            prop_assert!((1..=FRAME_MAX).contains(&n));
        }
        // And parse never panics on any bytes, and agrees with build.
        for d in &ds {
            if let Some((tr, snr, rssi, f)) = parse(d) {
                prop_assert!(!f.is_empty() && f.len() <= FRAME_MAX);
                let mut out = [0u8; DGRAM_MAX];
                let n = build(tr, snr, rssi, f, &mut out);
                prop_assert_eq!(&out[..n], &d[..n]);
            }
        }
    }
}

// ---- Two modems over real UDP on loopback. ----

struct Node {
    modem: Modem,
    radio: SimNetRadio<UdpPort>,
    input: VecDeque<u8>,
    screen: Vec<u8>,
}

impl Node {
    fn new(name: &str, answer: bool, port: UdpPort) -> Node {
        let mut cfg = Config::new(name).unwrap();
        if answer {
            cfg.sregs.set(0, 1);
        }
        let mut radio = SimNetRadio::new(port);
        // What the peer hears of us: a clean 12 dB link.
        radio.set_peer_hears(48, -60);
        Node {
            modem: Modem::new(cfg),
            radio,
            input: VecDeque::new(),
            screen: Vec::new(),
        }
    }

    fn type_(&mut self, s: &str) {
        self.input.extend(s.bytes());
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.screen).into_owned()
    }

    /// One pass of the loop the PC modem runs: datagrams in, frames due,
    /// host input, tick, host output.
    fn step(&mut self) {
        let now = self.radio.port().now_ms();
        let mut buf = [0u8; 1024];
        while let Some(n) = self.radio.port().recv(&mut buf) {
            self.radio.on_datagram(&buf[..n]);
        }
        let mut frame = [0u8; 255];
        while let Some((n, info)) = self.radio.service(&mut frame) {
            self.modem.radio_rx(&frame[..n], info, &mut self.radio, now);
        }
        if !self.input.is_empty() {
            let data = self.modem.in_data_mode();
            let seg: Vec<u8> = self
                .input
                .iter()
                .copied()
                .scan(false, |stop, c| {
                    if *stop {
                        return None;
                    }
                    *stop = !data && c == b'\r';
                    Some(c)
                })
                .collect();
            let k = self
                .modem
                .host_input(&seg, &mut self.radio, now)
                .min(seg.len());
            self.input.drain(..k);
        }
        self.modem.tick(&mut self.radio, now);
        let mut out = [0u8; 256];
        loop {
            let n = self.modem.host_output(&mut out);
            if n == 0 {
                break;
            }
            self.screen.extend_from_slice(&out[..n]);
        }
    }
}

/// Steps both nodes (about every millisecond of real time) until `cond`.
fn pump(a: &mut Node, b: &mut Node, max: Duration, cond: impl Fn(&Node, &Node) -> bool) -> bool {
    let start = Instant::now();
    loop {
        a.step();
        b.step();
        if cond(a, b) {
            return true;
        }
        if start.elapsed() > max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn pump_for(a: &mut Node, b: &mut Node, d: Duration) {
    pump(a, b, d, |_, _| false);
}

fn pair() -> (Node, Node) {
    let lo: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut pa = UdpPort::bind("127.0.0.1:0", lo).expect("bind A");
    let mut pb = UdpPort::bind("127.0.0.1:0", lo).expect("bind B");
    let (aa, ab) = (pa.local_addr().unwrap(), pb.local_addr().unwrap());
    assert_ne!(aa.port(), ab.port());
    pa.set_peer(ab);
    pb.set_peer(aa);
    (Node::new("CALLER", false, pa), Node::new("AXIS", true, pb))
}

#[test]
fn two_modems_over_simnet_full_call() {
    let started = Instant::now();
    let (mut a, mut b) = pair();
    let limit = Duration::from_secs(15);
    // A short escape guard so the test does not wait whole seconds.
    a.type_("ATS12=5\r");
    pump_for(&mut a, &mut b, Duration::from_millis(100));
    assert!(a.text().ends_with("\r\nOK\r\n"), "{:?}", a.text());

    // Dial: the call is set up over UDP datagrams on loopback.
    a.type_("ATDT AXIS\r");
    let ok = pump(&mut a, &mut b, limit, |a, b| {
        a.text().contains("CONNECT LORA") && b.text().contains("CONNECT LORA")
    });
    assert!(ok, "A: {:?}\nB: {:?}", a.text(), b.text());
    assert!(a.text().contains("\r\nDIALING AXIS\r\n"));
    assert!(b.text().contains("\r\nRING\r\n"));
    assert!(a.modem.is_connected() && b.modem.is_connected());

    // Data both ways.
    let mark_a = a.screen.len();
    let mark_b = b.screen.len();
    a.type_("hello from the caller");
    b.type_("and hello back from the answerer");
    let ok = pump(&mut a, &mut b, limit, |a, b| {
        String::from_utf8_lossy(&a.screen[mark_a..]).contains("and hello back from the answerer")
            && String::from_utf8_lossy(&b.screen[mark_b..]).contains("hello from the caller")
    });
    assert!(ok, "A: {:?}\nB: {:?}", a.text(), b.text());

    // +++ (after the guard), then ATH: NO CARRIER on both screens.
    pump_for(&mut a, &mut b, Duration::from_millis(250));
    let mark = a.screen.len();
    a.type_("+++");
    let ok = pump(&mut a, &mut b, limit, |a, _| {
        String::from_utf8_lossy(&a.screen[mark..]).ends_with("\r\nOK\r\n")
    });
    assert!(ok, "no OK after +++: {:?}", a.text());
    assert!(a.modem.is_connected(), "+++ keeps the call");
    a.type_("ATH\r");
    let ok = pump(&mut a, &mut b, limit, |a, b| {
        a.text().ends_with("\r\nNO CARRIER\r\n") && b.text().ends_with("\r\nNO CARRIER\r\n")
    });
    assert!(ok, "A: {:?}\nB: {:?}", a.text(), b.text());
    assert!(!a.modem.is_connected() && !b.modem.is_connected());
    assert_eq!((a.modem.call_state(), b.modem.call_state()), (0, 0));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn two_pairs_do_not_collide() {
    // Two independent pairs at once: ephemeral ports mean neither hears the
    // other's datagrams (this is what keeps parallel test runs apart).
    let (mut a1, mut b1) = pair();
    let (mut a2, mut b2) = pair();
    a1.type_("ATDT AXIS\r");
    let limit = Duration::from_secs(15);
    let ok = pump(&mut a1, &mut b1, limit, |a, b| {
        a.text().contains("CONNECT LORA") && b.text().contains("CONNECT LORA")
    });
    assert!(ok);
    pump_for(&mut a2, &mut b2, Duration::from_millis(300));
    assert!(
        b2.text().is_empty(),
        "pair 2 heard nothing: {:?}",
        b2.text()
    );
    assert_eq!(a2.modem.call_state(), 0);
}
