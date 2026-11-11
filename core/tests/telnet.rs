//! Phase 3 tests of the telnet client filter on its own (the bytes it
//! accepts, answers and escapes). The CR rule on a live wire call is tested
//! in `sim/tests/wire.rs`, where the platform side exists.

use lfd3k_core::telnet::{
    Telnet, DO, DONT, IAC, OPT_BINARY, OPT_ECHO, OPT_NAWS, OPT_SGA, SB, SE, WILL, WONT,
};
use lfd3k_core::WireMode;

/// What the AXIS telnet server sends on connect (facts from its telnet.cpp):
/// IAC WILL ECHO, WILL SGA, DO SGA, WILL BINARY, DO BINARY, DO NAWS.
const AXIS_OPENING: [u8; 18] = [
    255, 251, 1, 255, 251, 3, 255, 253, 3, 255, 251, 0, 255, 253, 0, 255, 253, 31,
];

fn telnet() -> Telnet {
    let mut t = Telnet::new();
    t.reset(WireMode::Telnet);
    t
}

/// Feeds every byte (as the modem does: only while `can_take`), returns the
/// clean data and every reply byte, popping replies as they appear.
fn feed(t: &mut Telnet, bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut data, mut replies) = (Vec::new(), Vec::new());
    let mut buf = [0u8; 64];
    for &b in bytes {
        assert!(t.can_take(), "replies were not drained");
        if let Some(d) = t.input(b) {
            data.push(d);
        }
        let n = t.pop_replies(&mut buf);
        replies.extend_from_slice(&buf[..n]);
    }
    (data, replies)
}

#[test]
fn telnet_answers_axis_negotiation() {
    let mut t = telnet();
    let (data, replies) = feed(&mut t, &AXIS_OPENING);
    assert_eq!(data, b"", "negotiation never reaches the user");
    let want: Vec<u8> = [
        // WILL ECHO -> DO ECHO, WILL SGA -> DO SGA, DO SGA -> WILL SGA
        &[IAC, DO, OPT_ECHO][..],
        &[IAC, DO, OPT_SGA],
        &[IAC, WILL, OPT_SGA],
        // WILL BINARY -> DO BINARY, DO BINARY -> WILL BINARY
        &[IAC, DO, OPT_BINARY],
        &[IAC, WILL, OPT_BINARY],
        // DO NAWS -> WILL NAWS, then SB NAWS 0 80 0 25 SE (80 x 25)
        &[IAC, WILL, OPT_NAWS],
        &[IAC, SB, OPT_NAWS, 0, 80, 0, 25, IAC, SE],
    ]
    .concat();
    assert_eq!(replies, want);
    assert_eq!(t.replies_pending(), 0);
    assert!(t.binary(), "BINARY agreed both ways");

    // The banner after the negotiation is clean text, and with BINARY both
    // ways a CR goes out bare (no NUL).
    let (data, replies) = feed(&mut t, b"AXIS BBS\r\nlogin: ");
    assert_eq!(data, b"AXIS BBS\r\nlogin: ");
    assert!(replies.is_empty());
    assert_eq!(t.encode(b'\r', None), (b'\r', None));
    assert_eq!(t.encode(b'\r', Some(0)), (b'\r', None));

    // Asking again for what is already on is not answered again (no loops).
    let (_, replies) = feed(&mut t, &AXIS_OPENING);
    assert!(replies.is_empty(), "{replies:?}");
}

#[test]
fn telnet_refuses_everything_else() {
    let mut t = telnet();
    // WILL 24 (terminal type), DO 24, DO 1 (we do not echo), WILL 5.
    let (data, replies) = feed(
        &mut t,
        &[
            IAC, WILL, 24, IAC, DO, 24, IAC, DO, OPT_ECHO, IAC, WILL, 5, IAC, DO, 39,
        ],
    );
    assert_eq!(data, b"");
    assert_eq!(
        replies,
        [IAC, DONT, 24, IAC, WONT, 24, IAC, WONT, OPT_ECHO, IAC, DONT, 5, IAC, WONT, 39]
    );
    // WONT and DONT for something that was never on: no answer.
    let (_, replies) = feed(&mut t, &[IAC, WONT, 24, IAC, DONT, 24, IAC, DONT, OPT_NAWS]);
    assert!(replies.is_empty());
}

#[test]
fn telnet_iac_iac_and_ff_escape() {
    let mut t = telnet();
    // Incoming IAC IAC is one 0xFF of data.
    let (data, replies) = feed(&mut t, &[b'a', IAC, IAC, b'b', IAC, IAC, IAC, IAC]);
    assert_eq!(data, [b'a', 255, b'b', 255, 255]);
    assert!(replies.is_empty());
    // Outgoing 0xFF is doubled; other bytes are not touched.
    assert_eq!(t.encode(255, None), (255, Some(255)));
    assert_eq!(t.encode(255, Some(b'x')), (255, Some(255)));
    assert_eq!(t.encode(b'a', None), (b'a', None));
    assert_eq!(t.encode(0, None), (0, None));
}

#[test]
fn telnet_subneg_stripped() {
    let mut t = telnet();
    // A terminal type request (SB 24 1 SE), a NOP, a GA, a stray SE: all gone.
    let (data, replies) = feed(
        &mut t,
        &[
            b'h', IAC, SB, 24, 1, IAC, SE, b'i', IAC, 241, IAC, 249, IAC, SE, b'!',
        ],
    );
    assert_eq!(data, b"hi!");
    assert!(replies.is_empty());

    // An IAC IAC inside a subnegotiation is part of it, not data and not the end.
    let (data, _) = feed(&mut t, &[IAC, SB, 24, 0, IAC, IAC, 7, IAC, SE, b'x']);
    assert_eq!(data, b"x");
}

#[test]
fn telnet_long_subneg_never_leaks() {
    let mut t = telnet();
    let mut stream = vec![b'<', IAC, SB, 250];
    // 300 body bytes: letters, an escaped IAC every 50 bytes, then the end.
    for i in 0..300u32 {
        if i % 50 == 49 {
            stream.extend_from_slice(&[IAC, IAC]);
        } else {
            stream.push(b'A' + (i % 26) as u8);
        }
    }
    stream.extend_from_slice(&[IAC, SE, b'>']);
    let (data, replies) = feed(&mut t, &stream);
    assert_eq!(
        data, b"<>",
        "no byte of the subnegotiation reaches the user"
    );
    assert!(replies.is_empty());

    // A subnegotiation that never ends swallows everything, silently.
    let mut t = telnet();
    let mut stream = vec![IAC, SB, 1];
    stream.extend(std::iter::repeat_n(b'z', 5000));
    let (data, replies) = feed(&mut t, &stream);
    assert!(data.is_empty() && replies.is_empty());
}

#[test]
fn raw_mode_passes_ff() {
    let mut t = Telnet::new();
    t.reset(WireMode::Raw);
    assert!(!t.is_telnet());
    // Every byte, IAC included, is plain data both ways; nothing is answered.
    let all: Vec<u8> = (0..=255u8).collect();
    let (data, replies) = feed(&mut t, &all);
    assert_eq!(data, all);
    assert!(replies.is_empty());
    let (data, replies) = feed(&mut t, &AXIS_OPENING);
    assert_eq!(data, AXIS_OPENING);
    assert!(replies.is_empty());
    assert_eq!(t.encode(255, None), (255, None));
    assert_eq!(t.encode(b'\r', Some(b'x')), (b'\r', None));
    assert!(!t.cr_rule());
    assert!(t.can_take());
}

#[test]
fn telnet_cr_rule_follows_our_direction_only() {
    let mut t = telnet();
    // Nothing agreed: CR followed by anything but LF becomes CR NUL.
    assert!(t.cr_rule());
    assert_eq!(t.encode(b'\r', Some(b'x')), (b'\r', Some(0)));
    assert_eq!(t.encode(b'\r', None), (b'\r', Some(0)));
    assert_eq!(t.encode(b'\r', Some(b'\n')), (b'\r', None), "CR LF stays");
    // The server says WILL BINARY (its direction) but never DO BINARY: our
    // direction is still NVT, so the CR rule still holds.
    feed(&mut t, &[IAC, WILL, OPT_BINARY]);
    assert!(t.cr_rule() && !t.binary());
    // DO BINARY: our direction is binary, CR goes bare.
    feed(&mut t, &[IAC, DO, OPT_BINARY]);
    assert!(!t.cr_rule() && t.binary());
    assert_eq!(t.encode(b'\r', Some(b'x')), (b'\r', None));
}

#[test]
fn telnet_incoming_cr_nul_collapsed() {
    // A server that is not binary: CR NUL is the NVT bare CR.
    let mut t = telnet();
    let (data, _) = feed(&mut t, b"OK\r\0x\r\ny\0");
    assert_eq!(data, b"OK\rx\r\ny\0", "only a NUL right after a CR goes");
    // Split across calls: the memory of the CR holds.
    let (a, _) = feed(&mut t, b"p\r");
    let (b, _) = feed(&mut t, b"\0q");
    assert_eq!([a, b].concat(), b"p\rq");
    // Once the server said WILL BINARY, a NUL is data.
    let mut t = telnet();
    feed(&mut t, &[IAC, WILL, OPT_BINARY]);
    let (data, _) = feed(&mut t, b"a\r\0b");
    assert_eq!(data, b"a\r\0b");
    // Raw mode never touches it.
    let mut t = Telnet::new();
    t.reset(WireMode::Raw);
    let (data, _) = feed(&mut t, b"a\r\0b");
    assert_eq!(data, b"a\r\0b");
}

#[test]
fn telnet_reply_queue_never_overflows() {
    // A hostile stream of requests that all get answers: the caller stops
    // feeding when `can_take` is false, so no reply is dropped.
    let mut t = telnet();
    let mut fed = 0;
    let mut answered = 0;
    let mut buf = [0u8; 8];
    let req = [IAC, DO, OPT_NAWS, IAC, WILL, 99];
    // Never drained: it must refuse more input well before 64 bytes pile up.
    'outer: for _ in 0..100 {
        for &b in &req {
            if !t.can_take() {
                break 'outer;
            }
            t.input(b);
            fed += 1;
        }
        // Make NAWS answerable again so every round queues something.
        for &b in &[IAC, DONT, OPT_NAWS] {
            if !t.can_take() {
                break 'outer;
            }
            t.input(b);
            fed += 1;
        }
    }
    assert!(!t.can_take(), "the queue filled and the caller was told");
    assert!(t.replies_pending() <= 64);
    while t.pop_replies(&mut buf) > 0 {
        answered += 1;
    }
    assert!(answered > 0 && fed > 0);
    assert!(t.can_take());
}

// ---- LFD3K-LINK (option 90) report text ----

use lfd3k_core::telnet::{
    link_frame, link_payload, LinkStats, LINK_FRAME_MAX, LINK_PAYLOAD_MAX, OPT_LINK,
};

fn payload_text(s: &LinkStats<'_>) -> String {
    let mut b = [0u8; LINK_PAYLOAD_MAX];
    let n = link_payload(s, &mut b);
    String::from_utf8(b[..n].to_vec()).expect("printable ASCII")
}

fn frame_bytes(s: &LinkStats<'_>) -> Vec<u8> {
    let mut b = [0u8; LINK_FRAME_MAX];
    let n = link_frame(s, &mut b);
    b[..n].to_vec()
}

fn radio_stats() -> LinkStats<'static> {
    LinkStats {
        modem: "AXISMODEM-4F1C",
        radio: true,
        caller: "CALLER",
        sf: Some(9),
        bps: Some(1_760),
        snr_x4: Some(38),
        rssi: Some(-71),
        retrains: Some(2),
        lz_x10: Some(29),
    }
}

#[test]
fn link_payload_format() {
    // Known stats: exact text, key order as documented.
    assert_eq!(
        payload_text(&radio_stats()),
        "v=1 modem=AXISMODEM-4F1C kind=radio caller=CALLER tier=SF9 bps=1760 snr=9.5 rssi=-71 retrains=2 lz=2.9"
    );
    // Negative SNR (quarter dB, rounded to tenths), compression off.
    let s = LinkStats {
        snr_x4: Some(-22),
        lz_x10: Some(0),
        sf: Some(12),
        ..radio_stats()
    };
    assert_eq!(
        payload_text(&s),
        "v=1 modem=AXISMODEM-4F1C kind=radio caller=CALLER tier=SF12 bps=1760 snr=-5.5 rssi=-71 retrains=2 lz=0"
    );
    // A USB Wi-Fi call: no caller, tier WIFI, unknown values left out.
    let w = LinkStats {
        modem: "AXISMODEM-4F1C",
        radio: false,
        caller: "",
        sf: None,
        bps: None,
        snr_x4: None,
        rssi: None,
        retrains: None,
        lz_x10: None,
    };
    assert_eq!(
        payload_text(&w),
        "v=1 modem=AXISMODEM-4F1C kind=wifi caller=- tier=WIFI"
    );
    // Empty names send "-".
    let e = LinkStats {
        modem: "",
        caller: "",
        ..radio_stats()
    };
    assert!(payload_text(&e).starts_with("v=1 modem=- kind=radio caller=- tier=SF9 "));

    // Values clamp to what the server accepts.
    let big = LinkStats {
        snr_x4: Some(i16::MAX),
        rssi: Some(i16::MAX),
        retrains: Some(u32::MAX),
        lz_x10: Some(u32::MAX),
        ..radio_stats()
    };
    let t = payload_text(&big);
    assert!(
        t.contains(" snr=99.9 rssi=50 retrains=999999 lz=99.9"),
        "{t}"
    );
    let low = LinkStats {
        snr_x4: Some(i16::MIN),
        rssi: Some(i16::MIN),
        ..radio_stats()
    };
    let t = payload_text(&low);
    assert!(t.contains(" snr=-99.9 rssi=-200 "), "{t}");

    // Escaping: anything outside letters, digits and "- _ ." becomes "_",
    // so no space, "=" or non-ASCII byte (0xFF included) is ever sent.
    let nasty = LinkStats {
        modem: "A B=C\u{ff}D\r\n",
        caller: "x y\u{1}",
        ..radio_stats()
    };
    let t = payload_text(&nasty);
    assert!(
        t.starts_with("v=1 modem=A_B_C__D__ kind=radio caller=x_y_ tier=SF9 "),
        "{t}"
    );
    assert!(t.bytes().all(|c| (0x20..0x7F).contains(&c)));

    // Length cap: names are cut to 24 bytes, the whole payload to 160, and
    // the frame is IAC SB 90 <payload> IAC SE with no stray 0xFF inside.
    let long_name = "N".repeat(200);
    let long = LinkStats {
        modem: &long_name,
        caller: &long_name,
        ..big
    };
    let t = payload_text(&long);
    assert!(t.len() <= LINK_PAYLOAD_MAX, "{} bytes", t.len());
    assert!(t.contains(&format!("modem={} ", "N".repeat(24))), "{t}");
    for s in [&long, &nasty, &big, &low] {
        let f = frame_bytes(s);
        let p = payload_text(s);
        assert!(f.len() <= LINK_FRAME_MAX);
        assert_eq!(&f[..3], [IAC, SB, OPT_LINK]);
        assert_eq!(&f[f.len() - 2..], [IAC, SE]);
        assert_eq!(&f[3..f.len() - 2], p.as_bytes());
        assert!(!f[3..f.len() - 2].contains(&IAC), "IAC inside the payload");
    }
}
