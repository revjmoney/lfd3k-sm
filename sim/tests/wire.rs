//! Phase 3: Wi-Fi (wire) calls and the radio to forward bridge, on the
//! simulated channel with a `FakeWire` as the platform and the TCP server.
//! Everything is seeded and runs on simulated time, so each run is the same.

use lfd3k_core::{Config, Modem, Radio, Tier};
use lfd3k_sim::{FakeWire, Harness, SimChannel, WireScript, AXIS_OPENING};

const NO_CARRIER: &str = "\r\nNO CARRIER\r\n";
const CONNECT_WIFI: &str = "\r\nCONNECT WIFI TCP\r\n";
/// Escape guard (S12 = 50 -> 1 s) plus a little.
const GUARD_MS: u32 = 1_100;

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            b"abcdefghijklmnopqrstuvwxyz0123456789"[(x >> 24) as usize % 36]
        })
        .collect()
}

fn count(hay: &[u8], pat: &[u8]) -> usize {
    hay.windows(pat.len()).filter(|w| *w == pat).count()
}

/// CALLER with a fake server on its Wi-Fi side and a forward `SRV` to it.
/// Port 23 is telnet, any other raw (the default of AT+FWD).
fn caller_with(script: WireScript, port: u16) -> Harness {
    let mut h = Harness::new(SimChannel::constant_db(12, 7));
    h.attach_wire_a(FakeWire::new(script));
    h.type_a(&format!("AT+FWD=SRV,example.com,{port}\r"));
    h.run_ms(50);
    assert!(h.screen_a().ends_with("\r\nOK\r\n"), "{:?}", h.screen_a());
    h
}

/// Dials `SRV` and runs until the call is online.
fn dial_srv(h: &mut Harness) {
    h.type_a("ATDT SRV\r");
    let ok = h.run_until(5_000, |h| h.screen_a().contains(CONNECT_WIFI));
    assert!(ok, "no connect: {:?}", h.screen_a());
}

/// Guard, `+++`, guard on CALLER; returns the screen text from the escape on.
fn escape_a(h: &mut Harness) -> String {
    h.run_ms(GUARD_MS);
    let mark = h.screen_a_bytes().len();
    h.type_a("+++");
    h.run_ms(GUARD_MS);
    String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned()
}

fn tail_a(h: &Harness, mark: usize) -> String {
    String::from_utf8_lossy(&h.screen_a_bytes()[mark..]).into_owned()
}

#[test]
fn wire_dial_connect_echo() {
    let mut h = caller_with(WireScript::Echo, 7000);
    h.type_a("ATDT SRV\r");
    assert!(h.run_until(5_000, |h| h.screen_a().contains(CONNECT_WIFI)));
    let s = h.screen_a();
    assert!(s.contains("\r\nDIALING SRV\r\n"), "{s:?}");
    assert!(!s.contains("CONNECT LORA"), "{s:?}");
    assert_eq!(
        h.wire_a().unwrap().requests(),
        [("example.com".to_string(), 7000)]
    );
    assert!(h.a.in_data_mode());
    assert_eq!(h.a.wire_state(), 2);
    assert!(h.a.is_connected());
    assert!(h.a.lights(h.now_ms).cd, "carrier detect on");
    // Data goes out and comes back, byte for byte (raw: nothing is added).
    let msg = b"hello, wire\r\nsecond line \xFF end";
    h.send_a_data(msg);
    h.run_ms(300);
    assert_eq!(h.wire_received_a(), msg);
    assert_eq!(h.received_a(), msg, "the echo comes back");
    // The radio was never used.
    assert_eq!(h.stats().frames_sent, [0, 0]);
}

#[test]
fn wire_connect_timeout_no_carrier() {
    let mut h = caller_with(WireScript::NeverAnswer, 7000);
    h.type_a("ATS7=5\r");
    h.run_ms(20);
    let t0 = h.now_ms;
    h.type_a("ATDT SRV\r");
    h.run_ms(4_900);
    assert!(!h.screen_a().contains("NO CARRIER"), "{:?}", h.screen_a());
    assert_eq!(h.a.wire_state(), 1, "still dialing at 4.9 s");
    assert!(h.run_until(500, |h| h.screen_a().contains("NO CARRIER")));
    let took = h.now_ms - t0;
    assert!(
        (5_000..=5_050).contains(&took),
        "NO CARRIER after {took} ms"
    );
    // Never a hang: the modem is back in command mode and answers.
    assert_eq!(h.a.wire_state(), 0);
    assert!(!h.a.in_data_mode());
    // The socket that was being opened is told to stop.
    h.run_ms(20);
    assert_eq!(h.wire_a().unwrap().closes_asked(), 1);
    let mark = h.screen_a_bytes().len();
    h.type_a("AT\r");
    h.run_ms(20);
    assert!(tail_a(&h, mark).ends_with("\r\nOK\r\n"));
    // The default S7 (30 s) is the limit when nothing is set.
    let mut h = caller_with(WireScript::NeverAnswer, 7000);
    h.type_a("ATDT SRV\r");
    h.run_ms(29_900);
    assert!(!h.screen_a().contains("NO CARRIER"));
    assert!(h.run_until(300, |h| h.screen_a().contains("NO CARRIER")));
}

#[test]
fn wire_refused_no_carrier() {
    let mut h = caller_with(WireScript::Refuse, 7000);
    let t0 = h.now_ms;
    h.type_a("ATDT SRV\r");
    assert!(h.run_until(1_000, |h| h.screen_a().contains("NO CARRIER")));
    assert!(h.now_ms - t0 < 100, "at once, not after S7");
    let s = h.screen_a();
    assert!(
        s.contains("\r\nDIALING SRV\r\n") && s.ends_with(NO_CARRIER),
        "{s:?}"
    );
    assert_eq!(h.a.wire_state(), 0);
    // Nothing to close: the platform said the socket is gone.
    h.run_ms(50);
    assert_eq!(h.wire_a().unwrap().closes_asked(), 0);
    // And the next dial works at once.
    h.wire_a_mut().unwrap().script = WireScript::Echo;
    dial_srv(&mut h);
}

#[test]
fn wire_remote_close() {
    let mut h = caller_with(WireScript::Banner(b"bye\r\n"), 7000);
    dial_srv(&mut h);
    h.run_ms(100);
    h.wire_a_mut().unwrap().close();
    assert!(h.run_until(1_000, |h| h.screen_a().ends_with(NO_CARRIER)));
    // What the server sent before closing is shown first, then NO CARRIER.
    assert_eq!(h.received_a(), b"bye\r\n");
    assert!(h.screen_a().ends_with(&format!("bye\r\n{NO_CARRIER}")));
    assert_eq!(h.a.wire_state(), 0);
    assert!(!h.a.in_data_mode() && !h.a.is_connected());
    h.run_ms(50);
    assert_eq!(h.wire_a().unwrap().closes_asked(), 0, "already gone");
    let mark = h.screen_a_bytes().len();
    h.type_a("AT\r");
    h.run_ms(20);
    assert!(tail_a(&h, mark).ends_with("\r\nOK\r\n"));
    // Closed while online-command mode (after +++): NO CARRIER at once.
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    escape_a(&mut h);
    h.wire_a_mut().unwrap().close();
    assert!(h.run_until(1_000, |h| h.screen_a().ends_with(NO_CARRIER)));
    assert_eq!(h.a.wire_state(), 0);
}

#[test]
fn wire_escape_and_hangup() {
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    h.send_a_data(b"before");
    let esc = escape_a(&mut h);
    assert!(esc.ends_with("\r\nOK\r\n"), "{esc:?}");
    assert!(!h.a.in_data_mode());
    assert!(h.wire_a().unwrap().is_open(), "+++ keeps the socket");
    assert_eq!(h.wire_received_a(), b"before", "the +++ is not sent");
    // ATH closes the socket.
    let mark = h.screen_a_bytes().len();
    h.type_a("ATH\r");
    h.run_ms(50);
    assert!(
        tail_a(&h, mark).ends_with(NO_CARRIER),
        "{:?}",
        tail_a(&h, mark)
    );
    assert_eq!(h.wire_a().unwrap().closes_asked(), 1);
    assert!(!h.wire_a().unwrap().is_open());
    assert_eq!(h.a.wire_state(), 0);
    // A second ATH is plain OK (no call).
    let mark = h.screen_a_bytes().len();
    h.type_a("ATH\r");
    h.run_ms(20);
    assert_eq!(tail_a(&h, mark), "ATH\r\r\nOK\r\n");
    assert_eq!(h.wire_a().unwrap().closes_asked(), 1);
}

#[test]
fn ath_on_wire_call_prints_no_carrier() {
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    escape_a(&mut h);
    let mark = h.screen_a_bytes().len();
    h.type_a("ATH\r");
    h.run_ms(50);
    // Echo of the line, then NO CARRIER (as for a radio call), not OK.
    assert_eq!(tail_a(&h, mark), format!("ATH\r{NO_CARRIER}"));

    // Also while the dial is still pending.
    let mut h = caller_with(WireScript::NeverAnswer, 7000);
    h.type_a("ATDT SRV\r");
    h.run_ms(200);
    assert_eq!(h.a.wire_state(), 1);
    let mark = h.screen_a_bytes().len();
    h.type_a("ATH\r");
    h.run_ms(50);
    assert_eq!(tail_a(&h, mark), format!("ATH\r{NO_CARRIER}"));
    assert_eq!(
        h.wire_a().unwrap().closes_asked(),
        1,
        "the half open socket"
    );
    assert_eq!(h.a.wire_state(), 0);
}

#[test]
fn atdt_forward_during_pending_close_is_error() {
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    escape_a(&mut h);
    let mark = h.screen_a_bytes().len();
    // ATH and ATDT in one burst: the platform has not taken the close yet.
    h.type_a("ATH\rATDT SRV\r");
    let t = tail_a(&h, mark);
    assert!(t.contains(NO_CARRIER), "{t:?}");
    assert!(t.ends_with("\r\nERROR\r\n"), "{t:?}");
    assert!(!t.contains("DIALING"), "{t:?}");
    assert_eq!(
        h.wire_a().unwrap().requests().len(),
        1,
        "nothing was requested"
    );
    // Once the platform has taken the close, dialing works again.
    h.run_ms(20);
    assert_eq!(h.wire_a().unwrap().closes_asked(), 1);
    dial_srv_again(&mut h);
    assert_eq!(h.wire_a().unwrap().requests().len(), 2);
    // host:port is held back the same way.
    escape_a(&mut h);
    let mark = h.screen_a_bytes().len();
    h.type_a("ATH\rATDT example.com:7000\r");
    assert!(tail_a(&h, mark).ends_with("\r\nERROR\r\n"));
}

fn dial_srv_again(h: &mut Harness) {
    let mark = h.screen_a_bytes().len();
    h.type_a("ATDT SRV\r");
    let ok = h.run_until(5_000, |h| tail_a(h, mark).contains(CONNECT_WIFI));
    assert!(ok, "{:?}", tail_a(h, mark));
}

#[test]
fn wire_escape_ato_resumes() {
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    h.send_a_data(b"one");
    h.run_ms(100);
    let esc = escape_a(&mut h);
    assert!(esc.ends_with("\r\nOK\r\n"), "{esc:?}");
    // Commands work in the call, the socket stays up.
    let mark = h.screen_a_bytes().len();
    h.type_a("ATI\r");
    h.run_ms(50);
    assert!(tail_a(&h, mark).contains("LFD3K-SM"));
    h.type_a("ATO\r");
    h.run_ms(50);
    assert!(
        tail_a(&h, mark).ends_with(CONNECT_WIFI),
        "{:?}",
        tail_a(&h, mark)
    );
    assert!(h.a.in_data_mode());
    h.send_a_data(b"two");
    h.run_ms(200);
    assert_eq!(h.wire_received_a(), b"onetwo", "no loss across the escape");
    assert_eq!(h.wire_a().unwrap().requests().len(), 1, "same connection");
    assert!(h.received_a().ends_with(b"two"));
}

#[test]
fn wire_backpressure_no_loss() {
    // 1. A 60 KB burst from the server: the core takes what its ring holds,
    //    the platform (FakeWire) keeps the rest, nothing is lost or reordered.
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    let burst = pattern(60_000, 11);
    h.wire_a_mut().unwrap().send(&burst);
    h.run_ms(3_000);
    assert_eq!(h.received_a(), burst);

    // 2. In online command mode (after +++) the server keeps sending: the
    //    core stops taking at its ring size and loses nothing; ATO delivers it.
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    escape_a(&mut h);
    let burst = pattern(9_000, 12);
    h.wire_a_mut().unwrap().send(&burst);
    h.run_ms(500);
    assert!(
        h.wire_a().unwrap().is_open(),
        "a slow reader never closes the call"
    );
    let shown = h.screen_a_bytes().len();
    h.type_a("ATO\r");
    h.run_ms(2_000);
    let got = h.received_a();
    assert!(got.ends_with(&burst[burst.len() - 100..]), "tail arrived");
    assert_eq!(got, burst, "all {} bytes, in order", burst.len());
    assert!(h.screen_a_bytes().len() > shown);

    // 3. Straight on the Modem API: wire_input takes only what fits, and the
    //    answer is immediate (the modem task never waits for a reader).
    struct Null;
    impl Radio for Null {
        fn send(&mut self, _t: Tier, _f: &[u8]) {}
        fn listen(&mut self, _t: Tier) {}
        fn busy(&self) -> bool {
            false
        }
    }
    let mut r = Null;
    let mut m = Modem::new(Config::new("A").unwrap());
    m.host_input(b"ATE0\rAT+FWD=S,h.example,7000\rATDT S\r", &mut r, 0);
    assert!(m.take_wire_request().is_some());
    m.wire_connected(1);
    // Escape to online command mode so nothing drains to the host.
    m.host_input(b"+++", &mut r, 2_000);
    m.tick(&mut r, 4_000);
    assert!(!m.in_data_mode());
    let big = pattern(10_000, 13);
    let took = m.wire_input(&big, 4_001);
    assert!(took > 0 && took < big.len(), "took {took} of {}", big.len());
    assert!(took <= 2_048, "the ring is 2 KB, took {took}");
    assert_eq!(m.wire_input(&big[took..], 4_002), 0, "full: nothing more");
    // Uplink: host bytes beyond the wire ring are reported as not taken.
    m.host_input(b"ATO\r", &mut r, 4_100);
    assert!(m.in_data_mode());
    let up = pattern(5_000, 14);
    let n = m.host_input(&up, &mut r, 4_200);
    assert!(n > 0 && n <= 2_048, "took {n}");
    let mut out = vec![0u8; 8_192];
    let w = m.wire_output(&mut out);
    assert_eq!(&out[..w], &up[..w], "in order, as typed (raw mode)");
}

#[test]
fn telnet_cr_lf_passes() {
    // Port 23, a server that negotiates nothing: NVT rules for our CR.
    let mut h = caller_with(WireScript::Echo, 23);
    dial_srv(&mut h);
    h.send_a_data(b"a\r\nb");
    h.run_ms(200);
    assert_eq!(h.wire_received_a(), [b'a', 13, 10, b'b'], "CR LF stays");
}

#[test]
fn telnet_lone_cr_gets_nul() {
    let mut h = caller_with(WireScript::Echo, 23);
    dial_srv(&mut h);
    h.send_a_data(b"a\rb");
    h.run_ms(200);
    assert_eq!(h.wire_received_a(), [b'a', 13, 0, b'b']);
    // And 0xFF is doubled on the way out.
    h.send_a_data(&[0xFF, b'x']);
    h.run_ms(200);
    assert_eq!(&h.wire_received_a()[4..], [0xFF, 0xFF, b'x']);
}

#[test]
fn telnet_split_cr_flushes_after_50ms() {
    let mut h = caller_with(WireScript::Echo, 23);
    dial_srv(&mut h);
    h.send_a_data(b"x\r");
    h.run_ms(30);
    assert_eq!(h.wire_received_a(), b"x", "the CR waits for a possible LF");
    // The LF arrives inside the hold: CR LF goes out together.
    h.send_a_data(b"\n");
    h.run_ms(100);
    assert_eq!(h.wire_received_a(), [b'x', 13, 10]);
    // A CR that nothing follows goes out as CR NUL after 50 ms.
    h.send_a_data(b"y\r");
    h.run_ms(30);
    assert_eq!(h.wire_received_a(), [b'x', 13, 10, b'y']);
    h.run_ms(40);
    assert_eq!(h.wire_received_a(), [b'x', 13, 10, b'y', 13, 0]);
}

#[test]
fn telnet_incoming_cr_nul_collapsed() {
    let mut h = caller_with(WireScript::Echo, 23);
    dial_srv(&mut h);
    h.wire_a_mut().unwrap().send(b"OK\r\0x\r\ny");
    h.run_ms(100);
    assert_eq!(h.received_a(), b"OK\rx\r\ny");
    // Raw mode (another port) is not touched.
    let mut h = caller_with(WireScript::Echo, 7000);
    dial_srv(&mut h);
    h.wire_a_mut().unwrap().send(b"OK\r\0x");
    h.run_ms(100);
    assert_eq!(h.received_a(), b"OK\r\0x");
}

#[test]
fn telnet_axis_opening_on_a_wire_call() {
    // The real AXIS opening over a USB dial: the user sees clean text, the
    // server gets the answers, then data with bare CR (BINARY agreed).
    let mut h = caller_with(WireScript::Telnet, 23);
    dial_srv(&mut h);
    h.run_ms(200);
    assert_eq!(h.received_a(), b"", "negotiation is invisible");
    let got = h.wire_received_a();
    assert_eq!(got.len(), 27, "{got:?}");
    assert_eq!(&got[..3], [255, 253, 1]);
    h.send_a_data(b"hi\r");
    h.run_ms(200);
    assert_eq!(&h.wire_received_a()[27..], b"hi\r", "no NUL: BINARY agreed");
    assert_eq!(h.received_a(), b"hi\r", "echoed, clean");
}

// ---- Bridge: an answered radio call goes to the published forward. ----

/// CALLER (radio) and AXIS, whose forward BBS (port `port`) is published and
/// served by a fake server on AXIS's Wi-Fi side.
fn bridge_rig(script: WireScript, port: u16) -> Harness {
    let mut h = Harness::new(SimChannel::constant_db(12, 3));
    h.attach_wire_b(FakeWire::new(script));
    h.type_b(&format!("AT+FWD=BBS,192.168.1.164,{port}\r"));
    h.type_b("AT+PUB=BBS\r");
    h.run_ms(50);
    assert!(h.screen_b().ends_with("\r\nOK\r\n"), "{:?}", h.screen_b());
    h
}

fn bridge_up(h: &mut Harness) {
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| {
        h.screen_a().contains("CONNECT LORA") && h.screen_b().contains("BRIDGE BBS")
    });
    assert!(ok, "A: {:?}\nB: {:?}", h.screen_a(), h.screen_b());
}

#[test]
fn radio_call_bridged_to_published_forward() {
    let mut h = bridge_rig(WireScript::Banner(b"WELCOME TO THE BBS\r\n"), 2323);
    bridge_up(&mut h);
    h.run_ms(3_000);
    // The server's banner reaches the radio caller's USB.
    assert_eq!(h.received_a(), b"WELCOME TO THE BBS\r\n");
    assert_eq!(
        h.wire_b().unwrap().requests(),
        [("192.168.1.164".to_string(), 2323)]
    );
    // What the caller types reaches the server, and the echo comes back.
    h.send_a_data(b"login: rev\r\n");
    h.run_ms(4_000);
    assert_eq!(h.wire_received_b(), b"login: rev\r\n");
    assert_eq!(h.received_a(), b"WELCOME TO THE BBS\r\nlogin: rev\r\n");
    assert_eq!(h.b.wire_state(), 3);
    // The caller's side shows nothing about Wi-Fi.
    let a = h.screen_a();
    assert!(!a.contains("BRIDGE") && !a.contains("WIFI"), "{a:?}");
    assert!(a.contains("CONNECT LORA SF5"), "{a:?}");
}

#[test]
fn bridge_answerer_usb_shows_bridge_only() {
    let mut h = bridge_rig(WireScript::Banner(b"SECRET BANNER\r\n"), 2323);
    let mark = h.screen_b_bytes().len();
    bridge_up(&mut h);
    h.run_ms(3_000);
    h.send_a_data(b"typed by the caller");
    h.run_ms(4_000);
    let b = String::from_utf8_lossy(&h.screen_b_bytes()[mark..]).into_owned();
    // The answerer's terminal never sees the call or its data.
    assert!(!b.contains("CONNECT"), "{b:?}");
    assert!(
        !b.contains("SECRET BANNER") && !b.contains("typed by"),
        "{b:?}"
    );
    assert_eq!(b.matches("\r\nBRIDGE BBS\r\n").count(), 1, "{b:?}");
    assert!(!b.contains("NO CARRIER"), "still up: {b:?}");
    // It stays in command mode (the host cannot join the call).
    assert!(!h.b.in_data_mode());
    let mark2 = h.screen_b_bytes().len();
    h.type_b("ATO\r");
    h.run_ms(50);
    let t = String::from_utf8_lossy(&h.screen_b_bytes()[mark2..]).into_owned();
    assert!(t.ends_with("\r\nERROR\r\n"), "ATO in a bridge: {t:?}");
    // Every line on that screen is one of the expected ones.
    for line in b.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(
            ["RING", "BRIDGE BBS", "RETRAIN"]
                .iter()
                .any(|k| line.starts_with(k)),
            "unexpected line {line:?} in {b:?}"
        );
    }
    // When the call ends the only thing it prints is NO CARRIER.
    h.wire_b_mut().unwrap().close();
    assert!(h.run_until(10_000, |h| h.screen_b().ends_with(NO_CARRIER)));
}

#[test]
fn bridge_radio_drop_closes_wire() {
    let mut h = bridge_rig(WireScript::Echo, 2323);
    bridge_up(&mut h);
    h.run_ms(500);
    assert!(h.wire_b().unwrap().is_open());
    escape_a(&mut h);
    h.type_a("ATH\r");
    assert!(h.run_until(10_000, |h| h.screen_b().ends_with(NO_CARRIER)));
    h.run_ms(100);
    let w = h.wire_b().unwrap();
    assert_eq!(w.closes_asked(), 1, "the wire is closed");
    assert!(!w.is_open());
    assert_eq!(h.b.wire_state(), 0);
    assert_eq!(h.b.call_state(), 0, "back to idle");
    assert!(h.screen_a().ends_with(NO_CARRIER));
    // The answerer is ready for the next caller and bridges it again.
    let mark = h.screen_a_bytes().len();
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| h.wire_b().unwrap().requests().len() == 2);
    assert!(ok, "second call: {:?}", tail_a(&h, mark));
}

#[test]
fn bridge_wire_close_hangs_up_caller() {
    let mut h = bridge_rig(WireScript::Banner(b"bye\r\n"), 2323);
    bridge_up(&mut h);
    h.run_ms(2_000);
    h.wire_b_mut().unwrap().close();
    assert!(h.run_until(15_000, |h| h.screen_a().ends_with(NO_CARRIER)));
    // The caller still got what the server sent first.
    assert_eq!(h.received_a(), b"bye\r\n");
    assert!(h.run_until(5_000, |h| h.b.call_state() == 0 && h.b.wire_state() == 0));
}

#[test]
fn bridge_forward_down_caller_gets_no_carrier() {
    // Connection refused: the radio caller is hung up at once.
    let mut h = bridge_rig(WireScript::Refuse, 2323);
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(20_000, |h| {
        h.screen_a().contains("CONNECT LORA") && h.screen_a().ends_with(NO_CARRIER)
    });
    assert!(ok, "A: {:?}\nB: {:?}", h.screen_a(), h.screen_b());
    assert!(h.run_until(5_000, |h| h.screen_b().ends_with(NO_CARRIER)));
    // The answering modem is idle again, and nothing data-like ever showed.
    assert_eq!((h.b.call_state(), h.b.wire_state()), (0, 0));
    assert!(!h.screen_b().contains("CONNECT"));
    assert_eq!(h.received_a(), b"");

    // A server that never answers: S7 on the answerer ends it.
    let mut h = bridge_rig(WireScript::NeverAnswer, 2323);
    h.type_b("ATS7=4\r");
    h.run_ms(50);
    h.type_a("ATDT AXIS\r");
    let ok = h.run_until(30_000, |h| h.screen_a().ends_with(NO_CARRIER));
    assert!(ok, "A: {:?}\nB: {:?}", h.screen_a(), h.screen_b());
    assert!(h.run_until(5_000, |h| h.b.call_state() == 0 && h.b.wire_state() == 0));
    assert_eq!(h.wire_b().unwrap().closes_asked(), 1);
    assert!(h.screen_b().ends_with(NO_CARRIER));
}

#[test]
fn radio_call_during_wire_call_is_busy() {
    let mut h = Harness::new(SimChannel::constant_db(12, 5));
    h.attach_wire_b(FakeWire::new(WireScript::Echo));
    h.type_b("AT+FWD=SRV,example.com,7000\r");
    h.run_ms(20);
    h.type_b("ATDT SRV\r");
    assert!(h.run_until(2_000, |h| h.screen_b().contains(CONNECT_WIFI)));
    assert_eq!(h.b.wire_state(), 2);
    // A radio caller gets BUSY, not a connection.
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(20_000, |h| h.screen_a().contains("\r\nBUSY\r\n")));
    assert!(!h.screen_a().contains("CONNECT"));
    // The wire call is untouched and still works.
    h.send_b_data(b"still here");
    h.run_ms(300);
    assert_eq!(h.wire_received_b(), b"still here");
    assert_eq!(h.b.wire_state(), 2);
    assert!(!h.screen_b().contains("RING"), "B was not rung");
    // After the wire call ends, the same radio dial connects.
    h.run_ms(GUARD_MS);
    h.type_b("+++");
    h.run_ms(GUARD_MS);
    h.type_b("ATH\r");
    h.run_ms(100);
    h.type_a("ATDT AXIS\r");
    assert!(h.run_until(20_000, |h| h.screen_a().contains("CONNECT LORA")));
}

#[test]
fn bridge_telnet_forward_is_clean_for_the_caller() {
    // The published forward is a telnet server (the AXIS board): the radio
    // caller never sees negotiation, and its bytes are IAC escaped on the wire.
    let mut h = bridge_rig(WireScript::Telnet, 23);
    bridge_up(&mut h);
    h.run_ms(3_000);
    assert_eq!(h.received_a(), b"", "no IAC reaches the radio caller");
    assert_eq!(
        h.wire_received_b().len(),
        27,
        "the replies went to the server"
    );
    h.send_a_data(&[b'o', 0xFF, b'k']);
    h.run_ms(4_000);
    // Echoed back through the server's IAC IAC and the core's filter.
    assert_eq!(h.received_a(), [b'o', 0xFF, b'k']);
    let got = h.wire_received_b();
    assert_eq!(&got[27..], [b'o', 0xFF, 0xFF, b'k'], "0xFF doubled out");
    assert_eq!(count(&got, &AXIS_OPENING[..3]), 0);
}
