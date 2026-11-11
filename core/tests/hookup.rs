//! LFD3K-LINK (telnet option 90) on one modem with a Wi-Fi telnet call: the
//! option is offered, but nothing is reported unless the host answers
//! `IAC DO 90`, and a report never holds back, delays or alters the caller's
//! own bytes. The bridged radio call (retrain reports, the rate limit over
//! time) is in `sim/tests/hookup.rs`; the payload text in `telnet.rs`.

use lfd3k_core::telnet::{DO, DONT, IAC, OPT_LINK, SB, SE, WILL, WONT};
use lfd3k_core::{Config, Modem, Radio, Tier};

struct NullRadio;

impl Radio for NullRadio {
    fn send(&mut self, _tier: Tier, _frame: &[u8]) {}
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

/// What the AXIS telnet server sends on connect.
const OPENING: [u8; 18] = [
    255, 251, 1, 255, 251, 3, 255, 253, 3, 255, 251, 0, 255, 253, 0, 255, 253, 31,
];
const DO90: [u8; 3] = [IAC, DO, OPT_LINK];
const DONT90: [u8; 3] = [IAC, DONT, OPT_LINK];
const WILL90: [u8; 3] = [IAC, WILL, OPT_LINK];
const WONT90: [u8; 3] = [IAC, WONT, OPT_LINK];

struct Rig {
    m: Modem,
    r: NullRadio,
    now: u32,
    /// Every byte the modem wrote to the socket.
    wire: Vec<u8>,
}

impl Rig {
    /// A Wi-Fi call to a forward on `port` (23: telnet, else raw), online.
    fn up(port: u16) -> Rig {
        let mut s = Rig {
            m: Modem::new(Config::new("AXISMODEM-4F1C").unwrap()),
            r: NullRadio,
            now: 1_000,
            wire: Vec::new(),
        };
        s.cmd(&format!("AT+FWD=SRV,example.com,{port}\r"));
        s.cmd("ATDT SRV\r");
        let req = s.m.take_wire_request().expect("a connect request");
        assert_eq!((req.host(), req.port()), ("example.com", port));
        s.m.wire_connected(s.now);
        s.run(50, 4096);
        assert!(s.m.in_data_mode());
        s.wire.clear();
        s
    }

    fn cmd(&mut self, line: &str) {
        assert_eq!(
            self.m.host_input(line.as_bytes(), &mut self.r, self.now),
            line.len()
        );
        self.run(20, 4096);
    }

    fn sink(&mut self) {
        let mut sink = [0u8; 256];
        while self.m.host_output(&mut sink) > 0 {}
    }

    /// Time passes in 10 ms steps; each step the socket is drained once
    /// with a buffer of `buf` bytes.
    fn run(&mut self, ms: u32, buf: usize) {
        let mut out = vec![0u8; buf];
        for _ in 0..ms / 10 {
            self.now += 10;
            self.m.tick(&mut self.r, self.now);
            let n = self.m.wire_output(&mut out);
            self.wire.extend_from_slice(&out[..n]);
            self.sink();
        }
    }

    /// One socket read with a buffer of exactly `buf` bytes, no time passing.
    fn pull(&mut self, buf: usize) -> Vec<u8> {
        let mut out = vec![0u8; buf];
        let n = self.m.wire_output(&mut out);
        out.truncate(n);
        self.wire.extend_from_slice(&out);
        out
    }

    /// The server sends these bytes (all of them must be taken).
    fn server(&mut self, bytes: &[u8]) {
        let mut off = 0;
        while off < bytes.len() {
            let n = self.m.wire_input(&bytes[off..], self.now);
            assert!(n > 0, "the modem stopped taking server bytes");
            off += n;
            self.run(10, 4096);
        }
    }

    /// The caller types these bytes, in data mode, without the socket
    /// being read.
    fn host_queue(&mut self, bytes: &[u8]) {
        assert_eq!(self.m.host_input(bytes, &mut self.r, self.now), bytes.len());
        self.now += 10;
        self.m.tick(&mut self.r, self.now);
        self.sink();
    }
}

/// Splits the socket stream into the `IAC SB 90 ... IAC SE` frames (payload
/// text) and everything else, in order.
fn split_reports(wire: &[u8]) -> (Vec<String>, Vec<u8>) {
    let (mut reports, mut rest) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < wire.len() {
        if wire[i..].starts_with(&[IAC, SB, OPT_LINK]) {
            let body = &wire[i + 3..];
            let end = body
                .windows(2)
                .position(|w| w == [IAC, SE])
                .expect("a report ends with IAC SE");
            reports.push(String::from_utf8(body[..end].to_vec()).unwrap());
            i += 3 + end + 2;
        } else {
            rest.push(wire[i]);
            i += 1;
        }
    }
    (reports, rest)
}

fn count(hay: &[u8], pat: &[u8]) -> usize {
    hay.windows(pat.len()).filter(|w| *w == pat).count()
}

/// A telnet call whose server has opened and answered DO 90; nothing is
/// drained since (the report is due), the wire log is empty.
fn linked() -> Rig {
    let mut s = Rig::up(23);
    s.server(&OPENING);
    s.run(50, 4096);
    s.wire.clear();
    s.m.wire_input(&DO90, s.now);
    s
}

#[test]
fn link_option_only_after_do() {
    // The server negotiates, the modem offers WILL 90, the server never
    // answers: nothing is reported, however long the call lasts.
    let mut s = Rig::up(23);
    s.server(&OPENING);
    s.run(30_000, 4096);
    assert_eq!(count(&s.wire, &WILL90), 1, "offered once: {:?}", s.wire);
    assert!(!s.m.link_on());
    assert!(split_reports(&s.wire).0.is_empty(), "no DO 90, no report");
    // The open offer does not touch the caller's bytes.
    s.wire.clear();
    s.host_queue(b"hello");
    s.run(500, 4096);
    assert_eq!(s.wire, b"hello");

    // DONT 90 as the answer: never a report, and a later DO 90 is refused.
    let mut s = Rig::up(23);
    s.server(&OPENING);
    s.run(100, 4096);
    s.server(&DONT90);
    s.run(30_000, 4096);
    assert!(!s.m.link_on());
    assert!(split_reports(&s.wire).0.is_empty(), "DONT 90: never");
    s.wire.clear();
    s.server(&DO90);
    s.run(30_000, 4096);
    assert!(!s.m.link_on(), "once refused it stays off");
    assert_eq!(count(&s.wire, &WONT90), 1);
    assert!(split_reports(&s.wire).0.is_empty());

    // DO 90 turns it on: a well-formed report at once; DONT 90 later turns
    // it off for good (WONT 90 back, no more reports).
    let mut s = Rig::up(23);
    s.server(&OPENING);
    s.run(100, 4096);
    assert!(split_reports(&s.wire).0.is_empty(), "the offer alone: none");
    s.wire.clear();
    s.server(&DO90);
    s.run(200, 4096);
    assert!(s.m.link_on());
    let (reports, rest) = split_reports(&s.wire);
    assert_eq!(reports.len(), 1, "one report at connect");
    assert!(
        reports[0].starts_with("v=1 modem=AXISMODEM-4F1C kind=wifi caller=- tier=WIFI"),
        "{:?}",
        reports[0]
    );
    assert!(rest.is_empty(), "an answer to our own offer needs no reply");
    s.server(&DONT90);
    s.run(1_000, 4096);
    assert!(!s.m.link_on());
    assert_eq!(count(&s.wire, &WONT90), 1);
    s.wire.clear();
    s.run(60_000, 4096);
    assert!(s.wire.is_empty(), "nothing after DONT 90: {:?}", s.wire);

    // A raw (R) forward is never offered anything and never reports.
    let mut s = Rig::up(7000);
    s.server(&OPENING);
    s.server(&DO90);
    s.run(30_000, 4096);
    assert!(!s.m.link_on());
    assert!(s.wire.is_empty(), "raw forward got {:?}", s.wire);
}

#[test]
fn link_stats_never_block_caller_data() {
    // Caller bytes, 0xFF among them (sent doubled on a telnet call).
    let data: Vec<u8> = (0..400u32)
        .map(|i| match i % 37 {
            0 => 0xFF,
            k => b'a' + (k as u8 % 26),
        })
        .collect();
    let double = |d: &[u8]| -> Vec<u8> {
        d.iter()
            .flat_map(|&b| if b == IAC { vec![IAC, IAC] } else { vec![b] })
            .collect()
    };

    // How long a report is on this call.
    let mut s = linked();
    s.run(100, 4096);
    let (reports, _) = split_reports(&s.wire);
    assert_eq!(reports.len(), 1);
    let frame_len = reports[0].len() + 5;

    // Output buffer nearly full: exactly the caller's bytes, or one byte
    // short of a report plus the caller's bytes counted twice (each byte may
    // double, so the core reserves 2 bytes per byte waiting): the report waits, the
    // caller data goes out whole and in order.
    let short = &data[..40];
    let wf = double(short);
    let need = frame_len + 2 * short.len();
    for room in [wf.len(), need - 1] {
        let mut s = linked();
        s.host_queue(short);
        let got = s.pull(room);
        assert_eq!(got, wf, "buffer of {room}: caller bytes only");
    }
    // With room for both, the report goes first, whole, and the data follows
    // unchanged. A report that waited goes out later, when there is room.
    let mut s = linked();
    s.host_queue(short);
    let got = s.pull(need - 1);
    assert_eq!(got, wf);
    s.host_queue(short);
    let got = s.pull(need);
    let (reports, rest) = split_reports(&got);
    assert_eq!(reports.len(), 1, "the waiting report went out: {got:?}");
    assert_eq!(rest, wf, "and the data behind it is unchanged");

    // A long stream through buffers of every size, report or not: the
    // caller's bytes arrive complete, in order and unchanged, and every
    // report is whole.
    for buf in [1usize, 2, 7, 64, 90, 4096] {
        let mut s = linked();
        s.host_queue(&data);
        s.run(30_000, buf);
        let (reports, rest) = split_reports(&s.wire);
        assert_eq!(rest, double(&data), "buffer {buf}");
        assert!(
            reports.len() <= 4,
            "buffer {buf}: {} reports",
            reports.len()
        );
        assert!(reports.iter().all(|r| r.starts_with("v=1 modem=")));
    }
}
