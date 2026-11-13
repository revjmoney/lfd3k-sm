//! Task 9 tests of the voice host protocol on the modem
//! (`core/src/voice/host.rs`, `docs/FEATURES.md` "Voice host protocol"):
//! registrations, sessions, AT+VOICE, ATDT and ATDV next to voice names,
//! AtLine, and the regression tests for the Task 7 and Task 8c review
//! findings that a single modem (or two on a perfect channel) can show.
//!
//! The harness (`Net`) puts modems on a perfect channel: every frame is
//! heard by every other modem listening on its tier once its airtime is
//! over, at 15 dB SNR, nothing is lost. Time moves in 1 ms steps; each
//! modem ticks every 10 ms and when its own frame ends. Unlike the sim's
//! `Multi`, a session's output is read only when a test asks for it, so a
//! test controls what is pending at once.
//!
//! Rulings that changed values from the plan text, as asserted here: VHP
//! 0x8F RingEnded, 0x90 Scope, 0x91 AirEvent; AtLine refuses AT+FB too;
//! ATH and ATZ through AtLine are refused while a data or Wi-Fi call is up.

use lfd3k_core::voice::host::{call_state, cobs_decode, cobs_encode, err, msg, SCOPE_MAX};
use lfd3k_core::voice::{VOICE_GRACE_MS, VOICE_HEARTBEAT_MS};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};

// ---- Harness.

struct Rad {
    now_us: u64,
    listen: Option<Tier>,
    tx_until_us: u64,
    outbox: Vec<(Tier, Vec<u8>)>,
}

impl Radio for Rad {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        self.tx_until_us = self.now_us + u64::from(tier.airtime_us(frame.len()));
        self.outbox.push((tier, frame.to_vec()));
    }
    fn listen(&mut self, tier: Tier) {
        self.listen = Some(tier);
    }
    fn busy(&self) -> bool {
        self.now_us < self.tx_until_us
    }
}

struct Flight {
    end_us: u64,
    from: usize,
    tier: Tier,
    bytes: Vec<u8>,
    to: Vec<usize>,
}

struct Net {
    m: Vec<Modem>,
    r: Vec<Rad>,
    now: u32,
    air: Vec<Flight>,
    /// Host stream output per modem (terminal text, or session 0 frames).
    host: Vec<Vec<u8>>,
    /// Bytes offered to a session and not taken yet, per modem and session.
    pending: Vec<[Vec<u8>; 4]>,
    /// Frames each modem put on the air (header type, sub-type).
    sent: Vec<Vec<(u8, u8)>>,
    /// Sessions that send a Heartbeat every second (the apps are alive).
    keep: Vec<(usize, u8)>,
}

fn cfg(name: &str, cap: Option<Tier>) -> Config {
    let mut c = Config::new(name).unwrap();
    if let Some(t) = cap {
        let i = Tier::LADDER.iter().position(|&x| x == t).unwrap();
        c.sregs.set(21, i as u16);
    }
    c
}

impl Net {
    fn new(cfgs: Vec<Config>) -> Net {
        let n = cfgs.len();
        let mut net = Net {
            m: cfgs.into_iter().map(Modem::new).collect(),
            r: (0..n)
                .map(|_| Rad {
                    now_us: 0,
                    listen: None,
                    tx_until_us: 0,
                    outbox: Vec::new(),
                })
                .collect(),
            now: 0,
            air: Vec::new(),
            host: vec![Vec::new(); n],
            pending: (0..n).map(|_| Default::default()).collect(),
            sent: vec![Vec::new(); n],
            keep: Vec::new(),
        };
        net.run(10);
        net
    }

    fn one(name: &str) -> Net {
        Net::new(vec![cfg(name, None)])
    }

    /// Run `f` on modem `i`, then send what it queued and collect its host output.
    fn with<T>(&mut self, i: usize, f: impl FnOnce(&mut Modem, &mut Rad, u32) -> T) -> T {
        let now = self.now;
        self.r[i].now_us = u64::from(now) * 1000;
        let out = f(&mut self.m[i], &mut self.r[i], now);
        let mut buf = [0u8; 512];
        loop {
            let n = self.m[i].host_output(&mut buf);
            if n == 0 {
                break;
            }
            self.host[i].extend_from_slice(&buf[..n]);
        }
        for (tier, bytes) in std::mem::take(&mut self.r[i].outbox) {
            let start = u64::from(now) * 1000;
            let end = start + u64::from(tier.airtime_us(bytes.len()));
            let ftype = bytes.get(1).map_or(0, |b| b >> 4);
            let sub = if ftype == 15 {
                bytes.get(5).copied().unwrap_or(0xFF)
            } else {
                0xFF
            };
            self.sent[i].push((ftype, sub));
            let to = (0..self.m.len())
                .filter(|&k| {
                    k != i && self.r[k].listen == Some(tier) && self.r[k].tx_until_us <= start
                })
                .collect();
            self.air.push(Flight {
                end_us: end,
                from: i,
                tier,
                bytes,
                to,
            });
        }
        out
    }

    fn step(&mut self) {
        let prev = u64::from(self.now) * 1000;
        self.now += 1;
        let now_us = u64::from(self.now) * 1000;
        for i in 0..self.m.len() {
            let end = self.r[i].tx_until_us;
            if end > prev && end <= now_us {
                self.with(i, |m, r, now| m.tick(r, now));
            }
        }
        let (due, rest): (Vec<Flight>, Vec<Flight>) = std::mem::take(&mut self.air)
            .into_iter()
            .partition(|f| f.end_us <= now_us);
        self.air = rest;
        for f in due {
            for &k in &f.to {
                if self.r[k].listen == Some(f.tier) && k != f.from {
                    let info = RxInfo {
                        tier: f.tier,
                        rssi_dbm: -80,
                        snr_x4: 60,
                    };
                    let b = f.bytes.clone();
                    self.with(k, |m, r, now| m.radio_rx(&b, info, r, now));
                }
            }
        }
        if self.now.is_multiple_of(10) {
            for i in 0..self.m.len() {
                self.with(i, |m, r, now| m.tick(r, now));
            }
        }
        if self.now.is_multiple_of(1000) {
            for (i, s) in self.keep.clone() {
                self.pending[i][usize::from(s)].extend_from_slice(&[2, msg::HEARTBEAT, 0]);
            }
        }
        for i in 0..self.m.len() {
            for s in 1..4 {
                if !self.pending[i][s].is_empty() {
                    let p = std::mem::take(&mut self.pending[i][s]);
                    let took = self.with(i, |m, r, now| m.voice_input(s as u8, &p, r, now));
                    self.pending[i][s] = p[took.min(p.len())..].to_vec();
                }
            }
        }
    }

    fn run(&mut self, ms: u32) {
        for _ in 0..ms {
            self.step();
        }
    }

    /// Run until `cond` or `max` ms; true when it held.
    fn run_until(&mut self, max: u32, mut cond: impl FnMut(&mut Net) -> bool) -> bool {
        for _ in 0..max {
            if cond(self) {
                return true;
            }
            self.step();
        }
        cond(self)
    }

    /// Terminal text into modem `i`'s host stream.
    fn type_in(&mut self, i: usize, s: &str) -> usize {
        let b = s.as_bytes().to_vec();
        self.with(i, |m, r, now| m.host_input(&b, r, now))
    }

    /// Host stream output since the last call, as text.
    fn host_text(&mut self, i: usize) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.host[i])).into_owned()
    }

    fn open(&mut self, i: usize, s: u8) {
        let now = self.now;
        assert!(self.m[i].voice_open(s, now), "open session {s}");
    }

    /// One decoded VHP frame to session `s` of modem `i` (0: the host stream).
    fn send(&mut self, i: usize, s: u8, frame: &[u8]) {
        let mut w = [0u8; 300];
        let n = cobs_encode(frame, &mut w).unwrap();
        if s == 0 {
            let b = w[..n].to_vec();
            let took = self.with(i, |m, r, now| m.host_input(&b, r, now));
            assert_eq!(took, n, "host stream took the frame");
        } else {
            self.pending[i][usize::from(s)].extend_from_slice(&w[..n]);
            self.step();
        }
    }

    /// Every whole frame session `s` of modem `i` has for the app (0: the
    /// host stream), decoded, in order.
    fn read(&mut self, i: usize, s: u8) -> Vec<Vec<u8>> {
        let raw = if s == 0 {
            std::mem::take(&mut self.host[i])
        } else {
            let mut v = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = self.m[i].voice_output(s, &mut buf);
                if n == 0 {
                    break;
                }
                v.extend_from_slice(&buf[..n]);
            }
            v
        };
        let mut out = Vec::new();
        for part in raw.split(|&b| b == 0) {
            if part.is_empty() {
                continue;
            }
            let mut f = [0u8; 300];
            if let Some(n) = cobs_decode(part, &mut f) {
                out.push(f[..n].to_vec());
            }
        }
        out
    }

    /// Hello and Register; the session then sends a Heartbeat every
    /// second (sessions 1 to 3).
    fn hello_register(&mut self, i: usize, s: u8, name: &str, label: &str) {
        self.send(i, s, &[msg::HELLO, 1]);
        self.send(i, s, &register(name, label));
        if s > 0 {
            self.keep.push((i, s));
        }
    }

    /// Session `s` of modem `i` stops sending Heartbeats.
    fn quiet(&mut self, i: usize, s: u8) {
        self.keep.retain(|&k| k != (i, s));
    }
}

fn strs(t: u8, a: &[&str]) -> Vec<u8> {
    let mut v = vec![t];
    for s in a {
        v.push(s.len() as u8);
        v.extend_from_slice(s.as_bytes());
    }
    v
}

fn register(name: &str, label: &str) -> Vec<u8> {
    strs(msg::REGISTER, &[name, label])
}

fn call(name: &str, label: &str) -> Vec<u8> {
    strs(msg::CALL, &[name, label])
}

fn types(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.iter().map(|f| f[0]).collect()
}

fn errors(frames: &[Vec<u8>]) -> Vec<(u8, u8)> {
    frames
        .iter()
        .filter(|f| f[0] == msg::ERROR)
        .map(|f| (f[1], f[2]))
        .collect()
}

fn last_state(frames: &[Vec<u8>]) -> Option<Vec<u8>> {
    frames
        .iter()
        .rev()
        .find(|f| f[0] == msg::CALL_STATE)
        .cloned()
}

/// Run an AtLine on session `s` and collect its AtReply text until final.
fn at_line(net: &mut Net, i: usize, s: u8, line: &str) -> Result<String, (u8, u8)> {
    let mut f = vec![msg::AT_LINE];
    f.extend_from_slice(line.as_bytes());
    net.send(i, s, &f);
    let mut text = String::new();
    for _ in 0..800 {
        for fr in net.read(i, s) {
            if fr[0] == msg::ERROR {
                return Err((fr[1], fr[2]));
            }
            if fr[0] == msg::AT_REPLY {
                text.push_str(&String::from_utf8_lossy(&fr[2..]));
                if fr[1] & 1 != 0 {
                    return Ok(text);
                }
            }
        }
        net.run(10);
    }
    panic!("no final AtReply for {line}: {text:?}");
}

// ---- Tests.

#[test]
fn register_four_fifth_refused() {
    let mut net = Net::one("ALPHA");
    // Session 0 is the host stream after AT+VOICE.
    net.type_in(0, "AT+VOICE\r");
    assert!(net.host_text(0).contains("OK"));
    net.type_in(0, "\0");
    net.hello_register(0, 0, "ZERO", "PC");
    let f = net.read(0, 0);
    assert_eq!(types(&f), vec![msg::HELLO_ACK], "session 0 frames: {f:?}");
    for s in 1..=3u8 {
        net.open(0, s);
        net.hello_register(0, s, &format!("NAME{s}"), "PC");
        let f = net.read(0, s);
        assert_eq!(f[0][0], msg::HELLO_ACK);
        assert!(errors(&f).is_empty(), "session {s}: {f:?}");
    }
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 4);
    // There is no fifth session, and no fifth registration.
    let now = net.now;
    assert!(!net.m[0].voice_open(4, now));
    assert!(!net.m[0].voice_open(1, now), "already open");
    assert!(!net.m[0].voice_register(4, "FIFTH", "PC", now));
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 4);
    // A second Register on a session replaces its first.
    net.send(0, 2, &register("RENAMED", "PHONE"));
    assert!(errors(&net.read(0, 2)).is_empty());
    let names: Vec<String> = net.m[0]
        .voice_regs()
        .iter()
        .flatten()
        .map(|r| r.name.as_str().to_string())
        .collect();
    assert_eq!(names.len(), 4);
    assert!(names.contains(&"RENAMED".to_string()) && !names.contains(&"NAME2".to_string()));
    // Bad names and labels are refused (Error 4), and bad payloads (2).
    for (n, l) in [
        ("", "PC"),
        ("A:B", "PC"),
        ("REV", ""),
        ("REV", "A/B"),
        ("SEVENTEENCHARSXXX", "PC"),
    ] {
        net.send(0, 3, &register(n, l));
        assert_eq!(
            errors(&net.read(0, 3)),
            vec![(msg::REGISTER, err::REFUSED)],
            "{n:?} {l:?}"
        );
    }
    net.send(0, 3, &[msg::REGISTER, 9, b'X']);
    assert_eq!(
        errors(&net.read(0, 3)),
        vec![(msg::REGISTER, err::BAD_PAYLOAD)]
    );
    net.send(0, 3, &[0x7F]);
    assert_eq!(errors(&net.read(0, 3)), vec![(0x7F, err::UNKNOWN)]);
    // Unregister drops it at once.
    net.send(0, 3, &[msg::UNREGISTER]);
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 3);
}

#[test]
fn registration_grace_30s() {
    let mut net = Net::one("ALPHA");
    net.open(0, 1);
    net.hello_register(0, 1, "REV", "PC");
    net.open(0, 2);
    net.hello_register(0, 2, "BOB", "PC");
    // Session 1 goes quiet; session 2 keeps talking.
    net.quiet(0, 1);
    let t0 = net.now;
    net.run(VOICE_HEARTBEAT_MS + 1_000);
    // Session 1 closed by the heartbeat rule, its registration still lives.
    assert!(net.read(0, 1).is_empty(), "closed: nothing more for it");
    let regs: Vec<String> = net.m[0]
        .voice_regs()
        .iter()
        .flatten()
        .map(|r| r.name.as_str().to_string())
        .collect();
    assert!(
        regs.contains(&"REV".to_string()),
        "grace keeps REV: {regs:?}"
    );
    // Until 30 s after its last frame.
    let until = t0 + VOICE_GRACE_MS - 2_000 - net.now;
    net.run(until);
    assert!(
        net.m[0]
            .voice_regs()
            .iter()
            .flatten()
            .any(|r| r.name.is("REV")),
        "still there at {} ms",
        net.now - t0
    );
    net.run(4_000);
    assert!(
        !net.m[0]
            .voice_regs()
            .iter()
            .flatten()
            .any(|r| r.name.is("REV")),
        "gone after 30 s"
    );
    assert!(net.m[0]
        .voice_regs()
        .iter()
        .flatten()
        .any(|r| r.name.is("BOB")));
    // Leave drops a registration at once, no grace.
    net.quiet(0, 2);
    net.send(0, 2, &[msg::LEAVE]);
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 0);
    // A closed session (the socket went) keeps its registration 30 s.
    net.open(0, 3);
    net.hello_register(0, 3, "ANN", "PC");
    net.quiet(0, 3);
    let now = net.now;
    net.m[0].voice_close(3, now);
    net.run(VOICE_GRACE_MS - 1_000);
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 1);
    net.run(2_000);
    assert_eq!(net.m[0].voice_regs().iter().flatten().count(), 0);
}

/// Two modems: REV's app on ALPHA session 1, BOB's on BRAVO session 1.
/// BOB answers; returns once both see CallState connected.
fn connected_pair(cap: Option<Tier>) -> Net {
    let mut net = Net::new(vec![cfg("ALPHA", cap), cfg("BRAVO", cap)]);
    net.open(0, 1);
    net.hello_register(0, 1, "REV", "PC");
    net.open(1, 1);
    net.hello_register(1, 1, "BOB", "PHONE");
    net.read(0, 1);
    net.read(1, 1);
    net.send(0, 1, &call("BOB", ""));
    let rang = net.run_until(20_000, |n| {
        let f = n.read(1, 1);
        f.iter().any(|f| f[0] == msg::RING)
    });
    assert!(rang, "BOB rang");
    net.send(1, 1, &[msg::ANSWER]);
    let mut a_up = false;
    let mut b_up = false;
    let up = net.run_until(60_000, |n| {
        for (k, flag) in [(0usize, &mut a_up), (1, &mut b_up)] {
            if let Some(s) = last_state(&n.read(k, 1)) {
                *flag = s[1] == call_state::CONNECTED;
            }
        }
        a_up && b_up
    });
    assert!(up, "the call connects");
    net
}

/// Both apps say VoiceReady; run until the slots run (CallState flags bit 0).
fn voice_ready(net: &mut Net) {
    net.send(0, 1, &[msg::VOICE_READY]);
    net.send(1, 1, &[msg::VOICE_READY]);
    let ok = net.run_until(30_000, |n| {
        n.m[0].call_state() != 0 && last_state(&n.read(0, 1)).is_some_and(|s| s[2] & 1 != 0)
    });
    assert!(ok, "voice slots run");
}

#[test]
fn session_timeout_hangs_up() {
    let mut net = connected_pair(None);
    assert!(net.m[0].voice_call().is_some());
    // REV's app goes silent; BOB's keeps its heartbeat.
    net.quiet(0, 1);
    let t0 = net.now;
    let ended = net.run_until(VOICE_HEARTBEAT_MS + 10_000, |n| {
        n.read(1, 1).iter().any(|f| f[0] == msg::ENDED)
    });
    assert!(ended, "BOB hears Ended after REV's session timed out");
    let took = net.now - t0;
    // Counted from REV's last Heartbeat, up to a second before it went quiet.
    assert!(
        took >= VOICE_HEARTBEAT_MS - 1_000 && took < VOICE_HEARTBEAT_MS + 5_000,
        "after {took} ms"
    );
    assert!(net.m[0].voice_call().is_none() && net.m[1].voice_call().is_none());
    // The registration outlives the session (grace).
    assert!(net.m[0]
        .voice_regs()
        .iter()
        .flatten()
        .any(|r| r.name.is("REV")));
}

#[test]
fn at_voice_enters_and_leaves_framed_mode() {
    let mut net = Net::one("ALPHA");
    net.type_in(0, "ATE0\r");
    net.host_text(0);
    net.type_in(0, "AT+VOICE\r\n");
    assert_eq!(net.host_text(0), "\r\nOK\r\n");
    // Framed now: text is not AT any more, nothing is echoed.
    net.type_in(0, "\0");
    net.send(0, 0, &[msg::HELLO, 1]);
    let f = net.read(0, 0);
    assert_eq!(
        f,
        vec![vec![msg::HELLO_ACK, 1, 5, b'A', b'L', b'P', b'H', b'A']]
    );
    net.type_in(0, "AT\r");
    net.run(20);
    assert!(
        !net.host_text(0).contains("OK"),
        "AT text is not a command in framed mode"
    );
    // A second AT+VOICE from an AtLine is refused (it would take the link).
    net.type_in(0, "\0");
    let mut fr = vec![msg::AT_LINE];
    fr.extend_from_slice(b"AT+VOICE");
    net.send(0, 0, &fr);
    assert_eq!(
        errors(&net.read(0, 0)),
        vec![(msg::AT_LINE, err::AT_REFUSED)]
    );
    // Leave: back to AT commands, no NO CARRIER (no call).
    net.send(0, 0, &[msg::LEAVE]);
    net.run(20);
    net.host_text(0);
    net.type_in(0, "AT\r");
    assert_eq!(net.host_text(0), "\r\nOK\r\n");
    // And the 5 s rule ends framed mode too.
    net.type_in(0, "at+voice\r");
    assert!(net.host_text(0).contains("OK"));
    net.run(VOICE_HEARTBEAT_MS + 100);
    net.host_text(0);
    net.type_in(0, "AT\r");
    assert_eq!(net.host_text(0), "\r\nOK\r\n");
}

#[test]
fn atdt_unchanged_with_voice_names() {
    // A voice app registered REV on ALPHA; the terminal dials ATDT REV:
    // that is a data call to a station named REV, the same CallReq as a
    // build without voice sends (type 1, never type 15).
    let mut rev = cfg("REV", None);
    rev.sregs.set(0, 1);
    let mut net = Net::new(vec![cfg("ALPHA", None), rev]);
    net.open(0, 1);
    net.hello_register(0, 1, "REV", "PC");
    net.type_in(0, "ATDT REV\r");
    let ok = net.run_until(20_000, |n| n.m[0].is_connected());
    assert!(ok, "the data call to station REV connects");
    let text = net.host_text(0);
    assert!(
        text.contains("CONNECT LORA") && text.contains("/ARQ"),
        "{text:?}"
    );
    assert!(!text.contains("VOICE"));
    assert!(
        net.sent[0].iter().all(|&(t, s)| !(t == 15 && s == 2)),
        "no voice CallReq from the data dialer: {:?}",
        net.sent[0]
    );
    assert!(net.sent[0].iter().any(|&(t, _)| t == 1), "a normal CallReq");
    // The voice session saw nothing of the data call.
    let f = net.read(0, 1);
    assert!(
        f.iter()
            .all(|f| f[0] != msg::CALL_STATE && f[0] != msg::RING),
        "{f:?}"
    );
}

#[test]
fn atdv_error_without_session() {
    let mut net = Net::one("ALPHA");
    net.type_in(0, "ATDV BOB\r");
    assert!(net.host_text(0).contains("ERROR"), "no registration: ERROR");
    net.type_in(0, "ATD VBOB\r");
    assert!(net.host_text(0).contains("ERROR"), "ATD V... is ATDV");
    assert!(net.sent[0].iter().all(|&(t, _)| t != 15));
    // With a voice app registered it dials (a type 15 CallReq goes out).
    net.open(0, 1);
    net.hello_register(0, 1, "REV", "PC");
    net.type_in(0, "ATDV BOB/PHONE\r");
    net.run(3_000);
    let text = net.host_text(0);
    assert!(text.contains("DIALING BOB/PHONE"), "{text:?}");
    assert!(
        net.sent[0].iter().any(|&(t, s)| t == 15 && s == 2),
        "voice CallReq sent: {:?}",
        net.sent[0]
    );
    // The first registered session owns it.
    let st = last_state(&net.read(0, 1)).expect("CallState to the owner");
    assert_eq!(st[1], call_state::DIALING);
    net.type_in(0, "ATH\r");
    net.run(100);
}

#[test]
fn vhp_at_line_sets_and_reads_back() {
    let mut net = Net::one("ALPHA");
    net.open(0, 1);
    net.send(0, 1, &[msg::HELLO, 1]);
    net.read(0, 1);
    assert!(at_line(&mut net, 0, 1, "ATS30=10").unwrap().contains("OK"));
    assert!(at_line(&mut net, 0, 1, "ATS30?").unwrap().contains("010"));
    assert!(at_line(&mut net, 0, 1, "AT+BEACON=QUIET")
        .unwrap()
        .contains("OK"));
    assert!(at_line(&mut net, 0, 1, "AT+BEACON?")
        .unwrap()
        .contains("BEACON QUIET"));
    // The hotspot password goes in and never comes back out.
    let secret = "hunter2hunter2";
    assert!(at_line(&mut net, 0, 1, &format!("AT+WAP=SHOPNET,{secret}"))
        .unwrap()
        .contains("OK"));
    let wap = at_line(&mut net, 0, 1, "AT+WAP?").unwrap();
    assert!(
        wap.contains("SHOPNET") && wap.contains("********"),
        "{wap:?}"
    );
    assert!(!wap.contains(secret));
    let all = at_line(&mut net, 0, 1, "AT&V").unwrap();
    assert!(
        !all.contains(secret),
        "AT&V through AtLine never shows the password"
    );
    assert!(at_line(&mut net, 0, 1, "AT+FWD=BBS,bbs.example.com,23,T")
        .unwrap()
        .contains("OK"));
    assert!(at_line(&mut net, 0, 1, "AT+FWD?")
        .unwrap()
        .contains("FWD BBS bbs.example.com:23 T"));
    // Saved, changed, then ATZ brings back what was saved.
    assert!(at_line(&mut net, 0, 1, "AT&W").unwrap().contains("OK"));
    assert!(
        net.m[0].take_save_request(),
        "AT&W asks the platform to save"
    );
    assert!(at_line(&mut net, 0, 1, "ATS30=3").unwrap().contains("OK"));
    assert!(at_line(&mut net, 0, 1, "AT+BEACON=ON")
        .unwrap()
        .contains("OK"));
    assert!(at_line(&mut net, 0, 1, "ATZ").unwrap().contains("OK"));
    assert!(at_line(&mut net, 0, 1, "ATS30?").unwrap().contains("010"));
    assert!(at_line(&mut net, 0, 1, "AT+BEACON?")
        .unwrap()
        .contains("BEACON QUIET"));
    assert!(at_line(&mut net, 0, 1, "AT+FWD?")
        .unwrap()
        .contains("FWD BBS"));
    // Nothing of it reached the terminal.
    assert!(!net.host_text(0).contains("010"));
    // A bad command answers ERROR in the reply, not a VHP Error.
    assert!(at_line(&mut net, 0, 1, "ATS30=99")
        .unwrap()
        .contains("ERROR"));
    // One AtLine at a time for the whole modem: a scan holds the line.
    net.open(0, 2);
    let mut f = vec![msg::AT_LINE];
    f.extend_from_slice(b"AT+SCAN");
    net.send(0, 1, &f);
    let mut f2 = vec![msg::AT_LINE];
    f2.extend_from_slice(b"ATI");
    net.send(0, 2, &f2);
    assert_eq!(errors(&net.read(0, 2)), vec![(msg::AT_LINE, err::BUSY)]);
}

#[test]
fn vhp_at_line_refuses_link_takers() {
    let mut net = Net::one("ALPHA");
    net.open(0, 1);
    for line in [
        "AT+MODE=MODEM",
        "AT+MODE=RADAR",
        "AT+SIMNET=10.0.0.2",
        "AT+SIMNET=0",
        "AT+MENU",
        "AT+VOICE",
        "ATD BOB",
        "ATDT BOB",
        "ATDP BOB",
        "ATDV BOB",
        "ATA",
        "ATO",
        "AT+FB",
    ] {
        assert_eq!(
            at_line(&mut net, 0, 1, line),
            Err((msg::AT_LINE, err::AT_REFUSED)),
            "{line}"
        );
    }
    assert!(
        net.sent[0].is_empty() || net.sent[0].iter().all(|&(t, _)| t == 12),
        "nothing dialed"
    );
    // The read-only queries are allowed.
    assert!(at_line(&mut net, 0, 1, "AT+SIMNET?")
        .unwrap()
        .contains("SIMNET OFF"));
    assert!(at_line(&mut net, 0, 1, "AT+MODE?").is_ok());
    assert!(at_line(&mut net, 0, 1, "ATI").unwrap().contains("OK"));
}

#[test]
fn regression_atline_ath_atz_refused_during_data_or_wifi_call() {
    // Task 7 review I2: an app must never end the terminal's call.
    let mut net = Net::one("ALPHA");
    net.open(0, 1);
    // A radio data call (dialing counts).
    net.type_in(0, "ATS7=30\r");
    net.type_in(0, "ATDT NOBODY\r");
    net.run(100);
    for line in ["ATH", "ATZ", "ATH0"] {
        assert_eq!(
            at_line(&mut net, 0, 1, line),
            Err((msg::AT_LINE, err::AT_REFUSED)),
            "{line} while dialing"
        );
    }
    assert!(net.host_text(0).contains("DIALING"));
    net.type_in(0, "\r");
    net.run(500);
    net.type_in(0, "ATH\r");
    net.run(100);
    // A Wi-Fi (TCP) call.
    net.type_in(0, "AT+FWD=SRV,example.com,23\r");
    net.type_in(0, "ATDT SRV\r");
    assert!(net.m[0].take_wire_request().is_some());
    let now = net.now;
    net.m[0].wire_connected(now);
    net.run(50);
    assert!(net.host_text(0).contains("CONNECT WIFI TCP"));
    for line in ["ATH", "ATZ"] {
        assert_eq!(
            at_line(&mut net, 0, 1, line),
            Err((msg::AT_LINE, err::AT_REFUSED)),
            "{line} on Wi-Fi"
        );
    }
    assert!(net.m[0].wire_active(), "the Wi-Fi call is still up");
    // Idle again: allowed.
    let now = net.now;
    net.m[0].wire_closed(now);
    net.run(200);
    assert!(at_line(&mut net, 0, 1, "ATH").unwrap().contains("OK"));
    assert!(at_line(&mut net, 0, 1, "ATZ").unwrap().contains("OK"));
}

#[test]
fn regression_non_owner_sessions_get_idle_callstate_and_free_floor() {
    // Task 7 review M1: CallState and Floor describe the call only to its
    // owner. REV owns a push to talk call on ALPHA session 1 and holds the
    // floor; the call ends; before session 1 reads, session 2 places the
    // next call. Session 1 then hears Ended, CallState idle and Floor free,
    // never the new call's state.
    let mut net = connected_pair(Some(Tier::Sf9));
    voice_ready(&mut net);
    net.send(0, 1, &[msg::TALK_KEY, 1]);
    let mine = net.run_until(20_000, |n| {
        n.read(0, 1).iter().any(|f| f[0] == msg::FLOOR && f[1] == 1)
    });
    assert!(mine, "REV holds the floor");
    net.open(0, 2);
    net.hello_register(0, 2, "ANN", "TAB");
    net.read(0, 2);
    // BOB hangs up; ALPHA's session 1 is not read from here on.
    net.send(1, 1, &[msg::HANGUP]);
    // (The Hangup may cross REV's own frame; then ALPHA ends at S22.)
    let idle = net.run_until(120_000, |n| n.m[0].voice_call().is_none());
    assert!(idle, "ALPHA's call ends");
    net.send(0, 2, &call("BOB", ""));
    net.run(200);
    assert!(
        net.m[0].voice_call().is_some(),
        "session 2's call is placed"
    );
    let f = net.read(0, 1);
    let t = types(&f);
    assert!(t.contains(&msg::ENDED), "{t:x?}");
    let st = last_state(&f).expect("a CallState after the end");
    assert_eq!(
        st[1],
        call_state::IDLE,
        "idle, not session 2's call: {st:x?}"
    );
    assert_eq!(st[2], 0, "no flags");
    let fl: Vec<u8> = f
        .iter()
        .filter(|f| f[0] == msg::FLOOR)
        .map(|f| f[1])
        .collect();
    assert_eq!(fl.last(), Some(&0), "Floor free: {fl:?}");
    // Session 2 owns its call and hears it.
    let st2 = last_state(&net.read(0, 2)).expect("owner's CallState");
    assert_eq!(st2[1], call_state::DIALING);
}

/// Scope frames: (t_ms, n, tx_mask, samples).
fn scopes(frames: &[Vec<u8>]) -> Vec<(u32, u8, u8, Vec<i8>)> {
    frames
        .iter()
        .filter(|f| f[0] == msg::SCOPE)
        .map(|f| {
            let t = u32::from_le_bytes(f[1..5].try_into().unwrap());
            let n = f[6];
            (
                t,
                n,
                f[7],
                f[8..8 + usize::from(n)].iter().map(|&b| b as i8).collect(),
            )
        })
        .collect()
}

#[test]
fn regression_scope_and_airevent_cursors() {
    // Task 8c: every session reads the scope and the air events at its own
    // pace; one that lags loses the oldest, a new one starts at the next,
    // and the rings wrap.
    let mut net = Net::one("ALPHA");
    net.open(0, 1);
    fn feed(net: &mut Net, k: i16) {
        net.run(50);
        let now = net.now;
        net.m[0].scope_sample(-100 + k, k % 7 == 0, now);
    }
    for k in 0..5 {
        feed(&mut net, k);
    }
    let s = scopes(&net.read(0, 1));
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].3, vec![-100, -99, -98, -97, -96]);
    assert_eq!(s[0].2, 0b0000_0001, "sample 0 taken while sending");
    // Session 1 stops reading for 40 samples (the 16 sample ring wraps
    // twice): it then gets only the newest SCOPE_MAX.
    for k in 5..45 {
        feed(&mut net, k);
    }
    let s = scopes(&net.read(0, 1));
    assert_eq!(s.len(), 1, "one Scope, not a backlog");
    assert_eq!(s[0].1 as usize, SCOPE_MAX);
    assert_eq!(
        s[0].3,
        (37..45).map(|k| (-100 + k) as i8).collect::<Vec<_>>()
    );
    // A session opened now starts with the next samples only.
    net.open(0, 2);
    for k in 45..50 {
        feed(&mut net, k);
    }
    let s2 = scopes(&net.read(0, 2));
    assert_eq!(s2.len(), 1);
    assert_eq!(
        s2[0].3,
        (45..50).map(|k| (-100 + k) as i8).collect::<Vec<_>>()
    );
    let s1 = scopes(&net.read(0, 1));
    assert_eq!(
        s1[0].3,
        (45..50).map(|k| (-100 + k) as i8).collect::<Vec<_>>()
    );
    assert_eq!(s1[0].0, net.now, "t_ms is the last sample's time");
    // AirEvents: twelve beacons and scans go out; a session that did not
    // read gets the newest 8, a new session none of the old ones.
    // ALPHA hears BRAVO's twelve scans (a ScanReq each, every 15 s).
    let mut net = Net::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)]);
    net.open(0, 1);
    net.keep.push((0, 1));
    net.read(0, 1);
    for _ in 0..12 {
        net.type_in(1, "AT+SCAN\r");
        net.run(15_000);
    }
    let heard = |frames: Vec<Vec<u8>>| -> Vec<Vec<u8>> {
        frames
            .into_iter()
            .filter(|f| f[0] == msg::AIR_EVENT && f[1] & 1 == 1)
            .collect()
    };
    // Every frame sent or heard is one: BRAVO's ScanReqs (heard) and
    // ALPHA's answers (sent), 24 in all; session 1 did not read, so it gets
    // the newest 8, oldest first.
    let air1: Vec<Vec<u8>> = net
        .read(0, 1)
        .into_iter()
        .filter(|f| f[0] == msg::AIR_EVENT)
        .collect();
    let reqs = net.sent[1].iter().filter(|&&(t, _)| t == 13).count();
    let resps = net.sent[0].iter().filter(|&&(t, _)| t == 14).count();
    assert_eq!((reqs, resps), (12, 12));
    assert_eq!(air1.len(), 8, "the newest 8 AirEvents");
    for f in &air1 {
        assert_eq!(f.len(), 16);
        assert_eq!(f[2], 12, "on SF12");
        assert_eq!(f[4], 0xFF, "no sub-type");
        let (rssi, snr) = (
            i16::from_le_bytes([f[12], f[13]]),
            i16::from_le_bytes([f[14], f[15]]),
        );
        if f[1] & 1 == 1 {
            assert_eq!((f[3], rssi, snr), (13, -80, 60), "a ScanReq heard: {f:x?}");
        } else {
            assert_eq!(
                (f[3], rssi, snr),
                (14, i16::MIN, i16::MIN),
                "a ScanResp sent: {f:x?}"
            );
        }
    }
    assert_eq!(heard(air1.clone()).len(), 4);
    let starts: Vec<u32> = air1
        .iter()
        .map(|f| u32::from_le_bytes(f[6..10].try_into().unwrap()))
        .collect();
    assert!(
        starts.windows(2).all(|w| w[0] < w[1]),
        "oldest first: {starts:?}"
    );
    // The newest 8 are the last 4 scans (15 s apart) and their answers.
    assert!(starts[7] - starts[0] >= 3 * 14_000, "{starts:?}");
    assert!(net.now - starts[0] <= 4 * 15_000 + 1_000, "{starts:?}");
    net.open(0, 3);
    net.keep.push((0, 3));
    assert!(
        heard(net.read(0, 3)).is_empty(),
        "a new session gets none of the old ones"
    );
    net.type_in(1, "AT+SCAN\r");
    net.run(5_200);
    assert_eq!(heard(net.read(0, 3)).len(), 1, "the new scan only");
    assert_eq!(heard(net.read(0, 1)).len(), 1);
}

#[test]
fn regression_scope_and_airevent_never_delay_voicein() {
    // Task 8c: Scope and AirEvent come last in the output order, so a
    // VoiceIn waiting at the same time always goes out first.
    let mut net = connected_pair(Some(Tier::Sf5));
    voice_ready(&mut net);
    let mut checked = 0;
    for _ in 0..200u32 {
        // REV talks (duplex), BOB's modem samples the channel, and BOB's
        // app reads only every 150 ms, so everything piles up at once.
        let mut v = vec![msg::VOICE_OUT];
        v.extend_from_slice(&[0x55; 60]);
        net.send(0, 1, &v);
        for _ in 0..3 {
            net.run(50);
            let now = net.now;
            net.m[1].scope_sample(-90, false, now);
        }
        let f = net.read(1, 1);
        let first_voice = f.iter().position(|f| f[0] == msg::VOICE_IN);
        let first_low = f
            .iter()
            .position(|f| f[0] == msg::SCOPE || f[0] == msg::AIR_EVENT);
        if let (Some(v), Some(l)) = (first_voice, first_low) {
            assert!(v < l, "VoiceIn behind Scope or AirEvent: {:x?}", types(&f));
            checked += 1;
        }
    }
    assert!(
        checked > 50,
        "VoiceIn and scope traffic met {checked} times"
    );
}
