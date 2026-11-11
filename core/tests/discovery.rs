//! Phase 4 tests of discovery on one modem: beacons, scans, scan answers,
//! the directory commands, the privacy modes and the directory blob. The
//! modem sits on a radio that records every frame it sends, and every frame
//! it hears is built here and handed to `radio_rx`, so each rule is seen from
//! outside: what went on the air, when, and what the host was told.
//!
//! Time is simulated and every run is the same run (the modem seeds its
//! random numbers from its name and the first tick).

use lfd3k_core::discovery::{
    beacon_payload, parse_beacon, parse_scan_req, parse_scan_resp, scan_req_payload,
    scan_resp_payload, Beacon, ScanReq, ScanResp, BEACON_EVERY_MS, BEACON_JITTER_MS, DISC_GAP_MS,
    SCAN_WINDOW_MS, SLOTS, SLOT_MS, SVC_ACCEPTS, SVC_FORWARD, SVC_HOTSPOT,
};
use lfd3k_core::frame::{call_req_payload, decode, encode, FrameType, Header};
use lfd3k_core::settings::BeaconMode;
use lfd3k_core::{Config, Directory, Modem, Radio, RxInfo, Tier, DIR_BLOB_LEN, DIR_MAX};

/// Records every frame with the time it was sent; `busy` is set by the test.
struct Rec {
    now: u32,
    busy: bool,
    sent: Vec<(u32, Tier, Vec<u8>)>,
}

impl Radio for Rec {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        self.sent.push((self.now, tier, frame.to_vec()));
    }
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        self.busy
    }
}

struct Rig {
    m: Modem,
    r: Rec,
    now: u32,
    /// Everything the host has been given so far.
    out: Vec<u8>,
}

fn info(snr_x4: i16) -> RxInfo {
    RxInfo {
        tier: Tier::Sf12,
        rssi_dbm: -100,
        snr_x4,
    }
}

fn frame(ft: FrameType, call_id: u8, payload: &[u8]) -> Vec<u8> {
    let h = Header {
        ftype: ft,
        flags: 0,
        call_id,
        seq: 0,
        ack: 0,
    };
    let mut b = [0u8; 255];
    let n = encode(&h, payload, &mut b).expect("encode");
    b[..n].to_vec()
}

fn beacon_frame(name: &str, services: u8) -> Vec<u8> {
    let mut p = [0u8; 32];
    let n = beacon_payload(
        &Beacon {
            name,
            services,
            heard: 0,
            time_quality: 0,
        },
        &mut p,
    )
    .expect("beacon");
    frame(FrameType::Beacon, 0, &p[..n])
}

fn scan_req_frame(name: &str, scan_id: u8) -> Vec<u8> {
    let mut p = [0u8; 32];
    let n = scan_req_payload(&ScanReq { name, scan_id }, &mut p).expect("scan req");
    frame(FrameType::ScanReq, 0, &p[..n])
}

fn scan_resp_frame(name: &str, services: u8, snr_x4: i16, scan_id: u8) -> Vec<u8> {
    let mut p = [0u8; 32];
    let n = scan_resp_payload(
        &ScanResp {
            name,
            services,
            snr_x4,
            scan_id,
        },
        &mut p,
    )
    .expect("scan resp");
    frame(FrameType::ScanResp, 0, &p[..n])
}

fn call_req_frame(caller: &str, callee: &str, call_id: u8) -> Vec<u8> {
    let mut p = [0u8; 64];
    let n = call_req_payload(caller, callee, &mut p).expect("call req");
    frame(FrameType::CallReq, call_id, &p[..n])
}

impl Rig {
    /// A modem named `name`, echo off, one tick done.
    fn new(name: &str) -> Rig {
        let mut rig = Rig {
            m: Modem::new(Config::new(name).expect("name")),
            r: Rec {
                now: 0,
                busy: false,
                sent: Vec::new(),
            },
            now: 0,
            out: Vec::new(),
        };
        rig.tick();
        rig.line("ATE0");
        rig.out.clear();
        rig
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 256];
        loop {
            let n = self.m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            self.out.extend_from_slice(&buf[..n]);
        }
    }

    fn tick(&mut self) {
        self.now += 1;
        self.r.now = self.now;
        self.m.tick(&mut self.r, self.now);
        self.drain();
    }

    /// `ms` of ticks, one per millisecond.
    fn run(&mut self, ms: u32) {
        for _ in 0..ms {
            self.tick();
        }
    }

    /// One AT line (CR added); returns what the host got meanwhile.
    fn line(&mut self, s: &str) -> String {
        let mark = self.out.len();
        let mut text = s.as_bytes().to_vec();
        text.push(b'\r');
        assert_eq!(self.m.host_input(&text, &mut self.r, self.now), text.len());
        self.drain();
        String::from_utf8_lossy(&self.out[mark..]).into_owned()
    }

    fn rx(&mut self, f: &[u8], snr_x4: i16) {
        self.m.radio_rx(f, info(snr_x4), &mut self.r, self.now);
        self.drain();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }

    /// Discovery frames sent so far: (time, type, payload).
    fn disc(&self) -> Vec<(u32, FrameType, Vec<u8>)> {
        self.r
            .sent
            .iter()
            .filter_map(|(t, _, b)| {
                let f = decode(b).ok()?;
                matches!(
                    f.header.ftype,
                    FrameType::Beacon | FrameType::ScanReq | FrameType::ScanResp
                )
                .then(|| (*t, f.header.ftype, f.payload.to_vec()))
            })
            .collect()
    }

    fn of_type(&self, ft: FrameType) -> Vec<(u32, Vec<u8>)> {
        self.disc()
            .into_iter()
            .filter(|(_, t, _)| *t == ft)
            .map(|(t, _, p)| (t, p))
            .collect()
    }

    /// Run until a ScanReq of ours is on the air (AT+SCAN already typed);
    /// returns its scan id.
    fn scan_id(&mut self) -> u8 {
        for _ in 0..20_000 {
            if let Some((_, p)) = self.of_type(FrameType::ScanReq).first() {
                return parse_scan_req(p).expect("scan req payload").scan_id;
            }
            self.tick();
        }
        panic!("no ScanReq sent");
    }

    fn dir_names(&self) -> Vec<String> {
        self.m
            .directory()
            .iter()
            .map(|e| e.name().to_string())
            .collect()
    }
}

#[test]
fn beacon_interval_and_jitter() {
    let mut rig = Rig::new("BEACONER");
    rig.m.load_settings({
        let mut s = lfd3k_core::Settings::defaults("BEACONER", "hotspotpw234");
        s.ap_on = false;
        s
    });
    rig.line("ATS0=1");
    rig.line("AT+FWD=BBS,h.example,23");
    rig.line("AT+PUB=BBS");
    rig.out.clear();
    // 40 beacons' worth of quiet air.
    let mut step = 0;
    while rig.of_type(FrameType::Beacon).len() < 12 && step < 4_000_000 {
        rig.tick();
        step += 1;
    }
    let beacons = rig.of_type(FrameType::Beacon);
    assert!(beacons.len() >= 12, "only {} beacons", beacons.len());
    // Only beacons: no scan frames, nothing else.
    assert_eq!(rig.disc().len(), rig.of_type(FrameType::Beacon).len());
    // The first one a full interval after start, then every 300 to 360 s.
    let first = beacons[0].0;
    assert!(
        (BEACON_EVERY_MS..=BEACON_EVERY_MS + BEACON_JITTER_MS + 100).contains(&first),
        "first beacon at {first} ms"
    );
    let mut gaps = Vec::new();
    for w in beacons.windows(2) {
        let g = w[1].0 - w[0].0;
        assert!(
            (BEACON_EVERY_MS..=BEACON_EVERY_MS + BEACON_JITTER_MS + 100).contains(&g),
            "gap {g} ms"
        );
        gaps.push(g);
    }
    gaps.sort_unstable();
    gaps.dedup();
    assert!(gaps.len() >= 4, "jitter should vary the gaps: {gaps:?}");
    // The air: SF12 on the calling channel, call id 0, the name, the
    // services we offer (accepts calls, forward, no hotspot) and our count.
    for (_, tier, b) in rig.r.sent.iter() {
        assert_eq!(*tier, Tier::Sf12);
        let f = decode(b).expect("frame");
        assert_eq!(f.header.call_id, 0);
        assert_eq!((f.header.flags, f.header.seq, f.header.ack), (0, 0, 0));
        let bc = parse_beacon(f.payload).expect("beacon payload");
        assert_eq!(bc.name, "BEACONER");
        assert_eq!(bc.services, SVC_ACCEPTS | SVC_FORWARD);
        assert_eq!(bc.heard, 0);
    }
    // 25 bytes is about 0.37 s on the air: short enough for 1 percent duty.
    let n = rig.r.sent[0].2.len();
    assert!(Tier::Sf12.airtime_us(n) < 500_000, "{n} bytes");
}

#[test]
fn hotspot_bit_follows_the_hotspot() {
    let mut rig = Rig::new("HOTSPOTTER");
    // The first-boot profile has the hotspot on.
    rig.m
        .load_settings(lfd3k_core::Settings::defaults("HOTSPOTTER", "hotspotpw234"));
    rig.rx(&scan_req_frame("SCANNER", 7), 40);
    rig.run(3_000);
    let r = rig.of_type(FrameType::ScanResp);
    assert_eq!(r.len(), 1);
    let resp = parse_scan_resp(&r[0].1).expect("resp");
    assert_eq!(resp.services & SVC_HOTSPOT, SVC_HOTSPOT);
    assert_eq!(resp.services & SVC_ACCEPTS, 0, "S0 is 0");
    assert_eq!(resp.services & !(SVC_HOTSPOT), 0);
}

#[test]
fn quiet_mode_answers_scan_no_beacon() {
    let mut rig = Rig::new("QUIETONE");
    assert_eq!(rig.line("AT+BEACON=QUIET"), "\r\nOK\r\n");
    assert_eq!(rig.line("AT+BEACON?"), "\r\nBEACON QUIET\r\n\r\nOK\r\n");
    // Far past any beacon time: nothing goes out.
    rig.run(800_000);
    assert!(rig.disc().is_empty(), "QUIET must not beacon");
    // A scan is still answered, once, in a slot within 2.5 s.
    let at = rig.now;
    rig.rx(&scan_req_frame("SCANNER", 0x5A), 36);
    rig.run(3_000);
    let r = rig.of_type(FrameType::ScanResp);
    assert_eq!(r.len(), 1, "one answer");
    assert!(
        r[0].0 - at <= u32::from(SLOTS) * SLOT_MS + 20,
        "{}",
        r[0].0 - at
    );
    let resp = parse_scan_resp(&r[0].1).expect("resp");
    assert_eq!(resp.name, "QUIETONE");
    assert_eq!(resp.scan_id, 0x5A, "the scan id is echoed");
    assert_eq!(resp.snr_x4, 36, "the SNR the ScanReq was heard at");
    // Still no beacon afterwards.
    rig.run(800_000);
    assert!(rig.of_type(FrameType::Beacon).is_empty());
    // The scanner is in the directory now.
    assert_eq!(rig.dir_names(), ["SCANNER"]);
}

#[test]
fn scan_collects_answers_and_prints() {
    let mut rig = Rig::new("SCANNER");
    assert_eq!(rig.line("AT+SCAN"), "", "no result until the window ends");
    assert!(rig.m.scanning());
    let id = rig.scan_id();
    let sent_at = rig.of_type(FrameType::ScanReq)[0].0;
    // Scan requests carry our name and the scan id.
    let reqs = rig.of_type(FrameType::ScanReq);
    let req = parse_scan_req(&reqs[0].1).expect("req");
    assert_eq!(req.name, "SCANNER");
    // Three answers at different strengths, one with the wrong scan id.
    rig.run(300);
    rig.rx(&scan_resp_frame("ALPHA", SVC_ACCEPTS, 32, id), 40);
    rig.run(400);
    rig.rx(&scan_resp_frame("BRAVO", 0, -20, id), 8);
    rig.run(400);
    rig.rx(&scan_resp_frame("STRAY", 0, 40, id.wrapping_add(1)), 40);
    rig.rx(&scan_resp_frame("CHARLIE", SVC_FORWARD, 60, id), 60);
    assert_eq!(rig.m.scan_hits(), 3);
    // The window runs 4 s from the ScanReq.
    let left = SCAN_WINDOW_MS - (rig.now - sent_at);
    rig.run(left + 10);
    assert!(!rig.m.scanning());
    let t = rig.text();
    // The worse of the two directions decides the tier: ALPHA min(10 dB, 8 dB).
    assert!(t.contains("\r\nSCAN ALPHA SNR 8.0 dB LORA SF"), "{t:?}");
    assert!(
        t.contains("\r\nSCAN BRAVO SNR -5.0 dB LORA SF8 BW500\r\n"),
        "{t:?}"
    );
    assert!(t.contains("\r\nSCAN CHARLIE SNR 15.0 dB LORA SF"), "{t:?}");
    assert!(
        !t.contains("STRAY"),
        "an answer to another scan is not counted: {t:?}"
    );
    assert!(t.ends_with("\r\nOK\r\n"), "{t:?}");
    // Order of arrival.
    let (a, b, c) = (
        t.find("ALPHA").unwrap(),
        t.find("BRAVO").unwrap(),
        t.find("CHARLIE").unwrap(),
    );
    assert!(a < b && b < c);
    // All four went into the directory (the stray one too: it was heard).
    let mut names = rig.dir_names();
    names.sort();
    assert_eq!(names, ["ALPHA", "BRAVO", "CHARLIE", "STRAY"]);
    // A late answer is heard but not printed or counted.
    let before = rig.text().len();
    rig.rx(&scan_resp_frame("LATE", 0, 40, id), 40);
    assert_eq!(rig.text().len(), before);
    assert!(rig.dir_names().contains(&"LATE".to_string()));
}

#[test]
fn scan_with_v0_prints_the_count() {
    let mut rig = Rig::new("SCANNER");
    rig.line("ATV0");
    rig.out.clear();
    rig.line("AT+SCAN");
    let id = rig.scan_id();
    rig.rx(&scan_resp_frame("ALPHA", 0, 20, id), 20);
    rig.rx(&scan_resp_frame("BRAVO", 0, 20, id), 20);
    rig.run(SCAN_WINDOW_MS + 50);
    assert_eq!(rig.text(), "\r\n2\r\n0\r", "the count, then the numeric OK");
}

#[test]
fn scan_answer_backoff_slots() {
    // The answer slot is random, 0 to 10 times 250 ms after the ScanReq,
    // different for different stations.
    let mut slots = Vec::new();
    for k in 0..30 {
        let name = format!("STATION{k:02}");
        let mut rig = Rig::new(&name);
        let at = rig.now;
        rig.rx(&scan_req_frame("SCANNER", 1), 40);
        rig.run(3_000);
        let r = rig.of_type(FrameType::ScanResp);
        assert_eq!(r.len(), 1, "{name}");
        let d = r[0].0 - at;
        assert!(
            d <= u32::from(SLOTS - 1) * SLOT_MS + 20,
            "{name} answered after {d} ms"
        );
        slots.push(d / SLOT_MS);
    }
    slots.sort_unstable();
    slots.dedup();
    assert!(slots.len() >= 5, "slots should differ: {slots:?}");

    // Listen before talk: while the channel is busy the answer waits for a
    // later slot, and never goes out while the radio is busy.
    let mut late = 0;
    for k in 0..30 {
        let name = format!("STATION{k:02}");
        let mut rig = Rig::new(&name);
        let at = rig.now;
        rig.r.busy = true;
        rig.rx(&scan_req_frame("SCANNER", 1), 40);
        rig.run(1_000);
        assert!(
            rig.of_type(FrameType::ScanResp).is_empty(),
            "{name} talked on a busy channel"
        );
        rig.r.busy = false;
        rig.run(3_000);
        let r = rig.of_type(FrameType::ScanResp);
        // The slots that came and went while busy are skipped, one per
        // try; with only 11 slots, some stations run out and stay quiet.
        if let Some((t, _)) = r.first() {
            assert!(
                *t - at >= 1_000,
                "{name} answered while the channel was busy"
            );
            late += 1;
        }
        assert!(r.len() <= 1);
    }
    assert!(late >= 1, "some stations should find a later slot");

    // A channel busy for the whole window: no answer at all, and no retry.
    let mut rig = Rig::new("STATION00");
    rig.r.busy = true;
    rig.rx(&scan_req_frame("SCANNER", 1), 40);
    rig.run(6_000);
    rig.r.busy = false;
    rig.run(30_000);
    assert!(rig.of_type(FrameType::ScanResp).is_empty());
}

#[test]
fn scan_answer_air_manners_one_per_10s() {
    // A busy air: a ScanReq from a different station every 700 ms, and a
    // beacon timer that runs the whole time. No two discovery frames of ours
    // may be less than 10 s apart.
    let mut rig = Rig::new("POLITE");
    for k in 0..1_500u32 {
        let name = format!("SCANNER{k:03}");
        rig.rx(&scan_req_frame(&name, k as u8), 40);
        rig.run(700);
    }
    let sent = rig.disc();
    assert!(sent.len() >= 10, "{} frames", sent.len());
    for w in sent.windows(2) {
        assert!(
            w[1].0 - w[0].0 >= DISC_GAP_MS,
            "{:?} at {} then {:?} at {}",
            w[0].1,
            w[0].0,
            w[1].1,
            w[1].0
        );
    }
    // And our own scan: typed right after an answer, it waits for the gap.
    let mut rig = Rig::new("POLITE");
    rig.rx(&scan_req_frame("SCANNER", 3), 40);
    rig.run(3_000);
    assert_eq!(rig.of_type(FrameType::ScanResp).len(), 1);
    let ans = rig.of_type(FrameType::ScanResp)[0].0;
    rig.line("AT+SCAN");
    rig.run(20_000);
    let req = rig.of_type(FrameType::ScanReq);
    assert_eq!(req.len(), 1);
    assert!(
        req[0].0 - ans >= DISC_GAP_MS,
        "{} after the answer",
        req[0].0 - ans
    );
}

#[test]
fn hidden_mode_silent_to_strangers() {
    let mut rig = Rig::new("GHOST");
    assert_eq!(rig.line("AT+BEACON=HIDDEN"), "\r\nOK\r\n");
    rig.line("ATS0=1");
    rig.out.clear();
    // A scan, a call, and answers to somebody else's scan: all met with
    // silence. No CallReject either.
    rig.rx(&scan_req_frame("STRANGER", 9), 40);
    rig.run(3_000);
    rig.rx(&call_req_frame("STRANGER", "GHOST", 0x31), 40);
    rig.run(5_000);
    rig.rx(&scan_resp_frame("OTHER", 0, 40, 9), 40);
    rig.run(400_000);
    assert!(rig.r.sent.is_empty(), "HIDDEN sent {:?}", rig.r.sent);
    assert!(!rig.text().contains("RING"), "{:?}", rig.text());
    assert_eq!(rig.m.last_caller(), "");
    assert_eq!(rig.m.call_state(), 0);
    // A scan of our own would give our name away.
    assert_eq!(rig.line("AT+SCAN"), "\r\nERROR\r\n");
    assert!(!rig.m.scanning());
    // The strangers never reach the directory through a scan or an answer.
    assert!(rig.dir_names().is_empty(), "{:?}", rig.dir_names());
}

#[test]
fn hidden_mode_stranger_beacon_then_call_is_silent() {
    let mut rig = Rig::new("GHOST");
    rig.line("AT+BEACON=HIDDEN");
    rig.line("ATS0=1");
    rig.out.clear();
    // The stranger announces itself, then calls. Hearing a name never opens
    // a HIDDEN modem: only a pin does.
    rig.rx(&beacon_frame("STRANGER", SVC_ACCEPTS), 40);
    rig.run(1_000);
    rig.rx(&call_req_frame("STRANGER", "GHOST", 0x31), 40);
    rig.run(1_000);
    rig.rx(&call_req_frame("STRANGER", "GHOST", 0x31), 40);
    rig.run(600_000);
    assert!(rig.r.sent.is_empty(), "no frame at all: {:?}", rig.r.sent);
    assert!(!rig.text().contains("RING"));
    assert_eq!(rig.m.last_caller(), "");
    assert_eq!(rig.m.call_state(), 0);
    assert_eq!(rig.line("ATA"), "\r\nNO CARRIER\r\n");
    assert!(rig.r.sent.is_empty());
}

#[test]
fn hidden_pinned_caller_rings() {
    let mut rig = Rig::new("GHOST");
    rig.line("AT+BEACON=HIDDEN");
    assert_eq!(rig.line("AT+DIR=+FRIEND"), "\r\nOK\r\n");
    rig.out.clear();
    // The friend calls: RING, and the call can be answered.
    rig.rx(&call_req_frame("FRIEND", "GHOST", 0x31), 40);
    rig.run(100);
    assert!(rig.text().contains("RING"), "{:?}", rig.text());
    assert_eq!(rig.m.call_state(), 2);
    assert_eq!(rig.m.last_caller(), "FRIEND");
    assert_eq!(rig.line("ATA"), "");
    rig.run(300);
    let types: Vec<FrameType> = rig
        .r
        .sent
        .iter()
        .filter_map(|(_, _, b)| decode(b).ok().map(|f| f.header.ftype))
        .collect();
    assert!(types.contains(&FrameType::CallAck), "{types:?}");
    assert!(types.contains(&FrameType::Train), "{types:?}");
    // The same name in another case is the same pin; a stranger still gets
    // nothing even while the friend's call is up.
    let sent = rig.r.sent.len();
    rig.rx(&call_req_frame("STRANGER", "GHOST", 0x41), 40);
    rig.run(1_000);
    assert!(
        rig.r.sent[sent..].iter().all(|(_, _, b)| {
            decode(b)
                .is_ok_and(|f| f.header.call_id != 0x41 || f.header.ftype != FrameType::CallReject)
        }),
        "a stranger got a reject"
    );
}

#[test]
fn hidden_pin_is_case_blind_and_removing_it_closes_the_door() {
    let mut rig = Rig::new("GHOST");
    rig.line("AT+BEACON=HIDDEN");
    rig.line("AT+DIR=+Friend");
    rig.rx(&call_req_frame("FRIEND", "GHOST", 0x31), 40);
    rig.run(100);
    assert_eq!(rig.m.call_state(), 2);
    rig.line("ATH");
    rig.run(100);
    assert_eq!(rig.m.call_state(), 0);
    rig.line("AT+DIR=-FRIEND");
    rig.out.clear();
    let sent = rig.r.sent.len();
    rig.rx(&call_req_frame("FRIEND", "GHOST", 0x33), 40);
    rig.run(2_000);
    assert_eq!(rig.m.call_state(), 0);
    assert!(!rig.text().contains("RING"));
    // Only the ATH's own frame may be there from before; nothing new now.
    assert_eq!(rig.r.sent.len(), sent);
}

#[test]
fn own_name_never_in_directory() {
    let mut rig = Rig::new("SELF");
    // Our own beacon, scan, answer and call echoed back (two modems with one
    // name, or a SimNet loopback), in every spelling.
    for name in ["SELF", "self", "Self"] {
        rig.rx(&beacon_frame(name, SVC_ACCEPTS), 40);
        rig.rx(&scan_req_frame(name, 1), 40);
        rig.rx(&scan_resp_frame(name, 0, 40, 1), 40);
    }
    rig.run(5_000);
    assert!(rig.dir_names().is_empty(), "{:?}", rig.dir_names());
    assert_eq!(rig.line("AT+DIR?"), "\r\nOK\r\n");
    // A ScanReq from ourselves is not answered.
    assert!(rig.of_type(FrameType::ScanResp).is_empty());
    // We cannot pin ourselves.
    assert_eq!(rig.line("AT+DIR=+SELF"), "\r\nERROR\r\n");
    assert_eq!(rig.line("AT+DIR=+self"), "\r\nERROR\r\n");
    // Another name is fine, and renaming onto it drops it from the list.
    assert_eq!(rig.line("AT+DIR=+OTHER"), "\r\nOK\r\n");
    assert_eq!(rig.line("AT+NAME=OTHER"), "\r\nOK\r\n");
    assert!(rig.dir_names().is_empty(), "{:?}", rig.dir_names());
    // A call from our own name is the same echo: it never lands in the list.
    rig.rx(&call_req_frame("OTHER", "OTHER", 0x31), 40);
    assert!(rig.dir_names().is_empty(), "{:?}", rig.dir_names());
    rig.line("ATH");
    // Our own scan answers of the same name are not counted.
    rig.line("AT+SCAN");
    let id = rig.scan_id();
    rig.rx(&scan_resp_frame("OTHER", 0, 40, id), 40);
    assert_eq!(rig.m.scan_hits(), 0);
}

#[test]
fn scan_answers_fill_and_evict() {
    let mut rig = Rig::new("SCANNER");
    // Three pinned names first.
    for p in ["PIN1", "PIN2", "PIN3"] {
        assert_eq!(rig.line(&format!("AT+DIR=+{p}")), "\r\nOK\r\n");
    }
    rig.out.clear();
    rig.line("AT+SCAN");
    let id = rig.scan_id();
    // 40 stations answer within the window, in a flood: the host is not
    // reading the whole time.
    for k in 0..40 {
        // Straight into the modem: the host does not read during the flood.
        let f = scan_resp_frame(&format!("ST{k:02}"), SVC_ACCEPTS, 40, id);
        rig.m.radio_rx(&f, info(40), &mut rig.r, rig.now);
        rig.m.radio_rx(&[0xD1], info(0), &mut rig.r, rig.now);
        rig.now += 20;
        rig.r.now = rig.now;
        rig.m.tick(&mut rig.r, rig.now);
    }
    assert_eq!(rig.m.scan_hits(), 40);
    // The directory is full and still 32 entries: the 3 pins, and the 29
    // newest answers. The oldest answers were replaced.
    assert_eq!(rig.m.directory().len(), DIR_MAX);
    let names = rig.dir_names();
    for p in ["PIN1", "PIN2", "PIN3"] {
        assert!(names.contains(&p.to_string()), "{p} evicted: {names:?}");
    }
    for k in 0..11 {
        assert!(
            !names.contains(&format!("ST{k:02}")),
            "ST{k:02} should be gone"
        );
    }
    for k in 11..40 {
        assert!(names.contains(&format!("ST{k:02}")), "ST{k:02} missing");
    }
    // The scan ends, the host reads, and the modem is not stuck.
    rig.run(SCAN_WINDOW_MS + 100);
    assert!(!rig.m.scanning());
    let t = rig.text();
    // Every line is whole; the ring holds 1 KB, so late lines may be left out
    // but none is cut.
    for l in t.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(
            l == "OK" || (l.starts_with("SCAN ST") && l.ends_with(" BW500")),
            "torn line {l:?}"
        );
    }
    assert!(t.contains("SCAN ST00 SNR"), "{t:?}");
    assert!(
        t.ends_with("\r\nOK\r\n"),
        "the scan still ends with OK: {t:?}"
    );
    assert_eq!(rig.line("AT"), "\r\nOK\r\n");
    // A fresh scan works at once, and the list stays at 32.
    rig.run(DISC_GAP_MS);
    assert_eq!(rig.line("AT+SCAN"), "");
    let id2 = rig.scan_id_after(1);
    rig.rx(&scan_resp_frame("NEWEST", 0, 40, id2), 40);
    assert_eq!(rig.m.directory().len(), DIR_MAX);
    assert!(rig.dir_names().contains(&"NEWEST".to_string()));
}

impl Rig {
    /// Scan id of the `n`th ScanReq sent (run until it is on the air).
    fn scan_id_after(&mut self, n: usize) -> u8 {
        for _ in 0..30_000 {
            if let Some((_, p)) = self.of_type(FrameType::ScanReq).get(n) {
                return parse_scan_req(p).expect("payload").scan_id;
            }
            self.tick();
        }
        panic!("ScanReq {n} not sent");
    }
}

#[test]
fn pinned_entries_never_evicted() {
    let mut d = Directory::new();
    for k in 0..10 {
        assert!(d.add_pinned(&format!("PIN{k}")));
    }
    for k in 0..500u32 {
        d.heard(&format!("HEARD{k}"), 1, info(30), k * 10);
    }
    assert_eq!(d.len(), DIR_MAX);
    for k in 0..10 {
        let e = d.get(&format!("pin{k}")).expect("pin kept");
        assert!(e.pinned);
    }
    // The oldest unpinned is replaced first.
    assert!(d.get("HEARD499").is_some());
    assert!(d.get("HEARD478").is_some());
    assert!(d.get("HEARD477").is_none());
    // A heard name refreshes its place: it outlives newer ones.
    d.heard("HEARD478", 1, info(30), 9_000);
    for k in 500..520u32 {
        d.heard(&format!("HEARD{k}"), 1, info(30), k * 10);
    }
    assert!(d.get("HEARD478").is_some());
    assert!(d.get("HEARD479").is_none());
    // Every slot pinned: nothing new gets in, nothing is lost, no panic.
    let mut full = Directory::new();
    for k in 0..DIR_MAX {
        assert!(full.add_pinned(&format!("P{k}")));
    }
    assert!(!full.add_pinned("ONEMORE"));
    full.heard("HEARD", 1, info(30), 5);
    assert_eq!(full.len(), DIR_MAX);
    assert!(full.get("HEARD").is_none());
    assert!(full.iter().all(|e| e.pinned));
    // A pinned entry that is removed on purpose does go.
    assert!(full.remove("P3"));
    assert!(full.add_pinned("ONEMORE"));
    // Unpinned-only clearing leaves the pins.
    d.clear_unpinned();
    assert_eq!(d.len(), 10);
}

#[test]
fn dir_pin_remove_clear() {
    let mut rig = Rig::new("DIRTEST");
    // Three stations heard by beacon.
    rig.rx(&beacon_frame("ALPHA", SVC_ACCEPTS), 40);
    rig.m
        .radio_rx(&beacon_frame("BRAVO", 0), info(-20), &mut rig.r, 500);
    rig.rx(&beacon_frame("CHARLIE", SVC_FORWARD | SVC_HOTSPOT), 8);
    rig.run(2_000);
    let t = rig.line("AT+DIR?");
    assert!(t.starts_with("\r\nDIR ALPHA LORA SF"), "{t:?}");
    assert!(t.contains("\r\nDIR BRAVO LORA SF8 BW500 "), "{t:?}");
    assert!(t.contains("\r\nDIR CHARLIE LORA SF"), "{t:?}");
    assert!(t.ends_with("\r\nOK\r\n"), "{t:?}");
    assert!(!t.contains(" PIN"), "{t:?}");
    // Pin one, in any case; an unknown name is added by hand.
    assert_eq!(rig.line("AT+DIR=+bravo"), "\r\nOK\r\n");
    assert_eq!(rig.line("AT+DIR=+DELTA"), "\r\nOK\r\n");
    let t = rig.line("AT+DIR?");
    assert!(
        t.contains("\r\nDIR BRAVO LORA SF8 BW500 1 PIN\r\n"),
        "{t:?}"
    );
    // A name typed by hand and not heard shows NONE and a dash.
    assert!(t.contains("\r\nDIR DELTA NONE - PIN\r\n"), "{t:?}");
    // Bad names and unknown removals.
    assert_eq!(rig.line("AT+DIR=+"), "\r\nERROR\r\n");
    assert_eq!(rig.line("AT+DIR=+A:B"), "\r\nERROR\r\n");
    assert_eq!(rig.line("AT+DIR=+ABCDEFGHIJKLMNOPQ"), "\r\nERROR\r\n");
    assert_eq!(rig.line("AT+DIR=-NOBODY"), "\r\nERROR\r\n");
    // Remove one.
    assert_eq!(rig.line("AT+DIR=-ALPHA"), "\r\nOK\r\n");
    assert!(!rig.line("AT+DIR?").contains("ALPHA"));
    // AT+DIR=0 clears the unpinned entries only.
    assert_eq!(rig.line("AT+DIR=0"), "\r\nOK\r\n");
    let mut names = rig.dir_names();
    names.sort();
    assert_eq!(names, ["BRAVO", "DELTA"]);
    // A pin removed on purpose goes, too.
    assert_eq!(rig.line("AT+DIR=-BRAVO"), "\r\nOK\r\n");
    assert_eq!(rig.dir_names(), ["DELTA"]);
    // Every slot pinned: the 33rd is refused.
    for k in 0..31 {
        assert_eq!(rig.line(&format!("AT+DIR=+P{k:02}")), "\r\nOK\r\n");
    }
    assert_eq!(rig.line("AT+DIR=+ONEMORE"), "\r\nERROR\r\n");
    // A listing is data-mode safe: the commands work from the first line on
    // after a scan answer arrives meanwhile (no state left behind).
    assert_eq!(rig.line("AT"), "\r\nOK\r\n");
}

/// CRC-16/CCITT-FALSE, written out so a test can forge a blob that is wrong
/// in one field only and still carries a good checksum.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn fix_crc(blob: &mut [u8]) {
    let n = blob.len() - 2;
    let c = crc16(&blob[..n]).to_le_bytes();
    blob[n..].copy_from_slice(&c);
}

fn blob_of(d: &Directory) -> Vec<u8> {
    let mut b = [0u8; DIR_BLOB_LEN];
    let n = d.to_blob(&mut b);
    b[..n].to_vec()
}

fn sample_dir() -> Directory {
    let mut d = Directory::new();
    d.add_pinned("ROTATOCAM");
    d.add_pinned("AXIS");
    for (i, n) in ["ALPHA", "BRAVO", "CHARLIE"].iter().enumerate() {
        d.heard(
            n,
            (i as u8) % 8,
            info(10 * (i as i16) - 20),
            1_000 * (i as u32 + 1),
        );
    }
    d
}

#[test]
fn dir_blob_roundtrip_and_reject() {
    let d = sample_dir();
    let good = blob_of(&d);
    // 6 header + 5 records of 20 + 2 CRC.
    assert_eq!(good.len(), 6 + 5 * 20 + 2);
    assert_eq!(&good[..5], b"LFD1\x01");
    let back = Directory::from_blob(&good).expect("loads");
    assert_eq!(back.len(), 5);
    for e in d.iter() {
        let g = back.get(e.name()).expect("name kept");
        assert_eq!(
            (g.pinned, g.services, g.best_tier),
            (e.pinned, e.services, e.best_tier),
            "{}",
            e.name()
        );
        // Not stored: loaded entries count as not heard since boot.
        assert!(!g.heard_since_boot());
    }
    // Most recent first, so a reload keeps the same order of recency.
    assert_eq!(blob_of(&back), good);
    // The empty directory round-trips too.
    let empty = blob_of(&Directory::new());
    assert_eq!(empty.len(), 8);
    assert_eq!(Directory::from_blob(&empty).expect("empty").len(), 0);
    // Only the 8 most recent unpinned entries are stored, every pin is.
    let mut big = Directory::new();
    for k in 0..4 {
        big.add_pinned(&format!("PIN{k}"));
    }
    for k in 0..20u32 {
        big.heard(&format!("H{k:02}"), 1, info(30), k);
    }
    let b = blob_of(&big);
    let back = Directory::from_blob(&b).expect("loads");
    assert_eq!(back.len(), 12);
    assert!(back.get("H19").is_some() && back.get("H12").is_some());
    assert!(back.get("H11").is_none());
    assert!((0..4).all(|k| back.get(&format!("PIN{k}")).is_some()));

    // Wrong lengths.
    assert!(Directory::from_blob(&[]).is_none());
    assert!(Directory::from_blob(&good[..good.len() - 1]).is_none());
    assert!(Directory::from_blob(&good[..6]).is_none());
    let mut long = good.clone();
    long.push(0);
    assert!(Directory::from_blob(&long).is_none());
    assert!(Directory::from_blob(&[0u8; DIR_BLOB_LEN]).is_none());
    assert!(
        Directory::from_blob(&[0xFF; DIR_BLOB_LEN]).is_none(),
        "erased flash"
    );
    // Every single bit flip is caught by the CRC (or a field check).
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            assert!(Directory::from_blob(&b).is_none(), "byte {i} bit {bit}");
        }
    }
    // Forged blobs with a good CRC but one wrong field.
    let forge = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = good.clone();
        f(&mut b);
        fix_crc(&mut b);
        Directory::from_blob(&b)
    };
    assert!(forge(&|b| b[0] = b'X').is_none(), "magic");
    assert!(forge(&|b| b[4] = 2).is_none(), "version");
    assert!(forge(&|b| b[5] = 33).is_none(), "count over 32");
    assert!(forge(&|b| b[6] = 0).is_none(), "name length 0");
    assert!(forge(&|b| b[6] = 17).is_none(), "name length 17");
    assert!(forge(&|b| b[7] = b':').is_none(), "colon in a name");
    assert!(
        forge(&|b| b[7] = 0x07).is_none(),
        "control character in a name"
    );
    assert!(
        forge(&|b| b[6 + 19] = 0xEE).is_none(),
        "trailing name byte not zero"
    );
    assert!(
        forge(&|b| b[6 + 17] = 0x80).is_none(),
        "unknown services bits"
    );
    assert!(forge(&|b| b[6 + 18] = 8).is_none(), "tier index 8");
    assert!(forge(&|b| b[6 + 19] = 2).is_none(), "pinned byte 2");
    // Duplicate names, in another case.
    assert!(
        forge(&|b| {
            let (a, c) = (6 + 1, 6 + 20 + 1);
            let first: Vec<u8> = b[a..a + 16].to_vec();
            b[c..c + 16].copy_from_slice(&first);
            b[6 + 20] = b[6];
        })
        .is_none(),
        "duplicate"
    );
    assert!(
        forge(&|b| {
            let (a, c) = (6 + 1, 6 + 20 + 1);
            let first: Vec<u8> = b[a..a + 16].to_vec();
            b[c..c + 16].copy_from_slice(&first);
            b[6 + 20] = b[6];
            b[c].make_ascii_lowercase();
        })
        .is_none(),
        "duplicate in another case"
    );
    // More than 8 unpinned records.
    let mut many = vec![0u8; 6 + 9 * 20 + 2];
    many[..5].copy_from_slice(b"LFD1\x01");
    many[5] = 9;
    for k in 0..9 {
        let r = 6 + k * 20;
        many[r] = 3;
        many[r + 1..r + 4].copy_from_slice(format!("N{k}X").as_bytes());
        many[r + 18] = 0xFF;
    }
    fix_crc(&mut many);
    assert!(Directory::from_blob(&many).is_none(), "9 unpinned");
    many[5] = 8;
    many.drain(6 + 8 * 20..6 + 9 * 20);
    fix_crc(&mut many);
    assert!(Directory::from_blob(&many).is_some(), "8 unpinned is fine");
}

#[test]
fn directory_loaded_in_place_is_empty_after_a_reject() {
    let mut d = sample_dir();
    let good = blob_of(&d);
    assert!(d.load_blob(&good));
    assert_eq!(d.len(), 5);
    let mut bad = good.clone();
    bad[10] ^= 0x55;
    // The old contents are gone, nothing half loaded stays.
    assert!(!d.load_blob(&bad));
    assert!(d.is_empty());
    assert!(d.load_blob(&good));
    assert_eq!(d.len(), 5);
    // Through the modem: our own name never stays, and a bad blob is refused.
    let mut m = Modem::new(Config::new("ALPHA").expect("name"));
    assert!(m.load_directory_blob(&good));
    assert!(m.directory().get("ALPHA").is_none());
    assert_eq!(m.directory().len(), 4);
    assert!(!m.load_directory_blob(&bad));
    assert!(m.directory().is_empty());
}

#[test]
fn atdt_uses_directory_best_tier() {
    // Training still decides the speed: a directory that claims SF5 for a
    // station on a weak link, or SF12 for a strong one, changes nothing about
    // the call. Here only the one modem's side: the CallReq goes out at SF12
    // whatever the directory says, and a pinned or unknown name dials alike.
    for pin in [None, Some("+FARAWAY")] {
        let mut rig = Rig::new("DIALER");
        if let Some(p) = pin {
            assert_eq!(rig.line(&format!("AT+DIR={p}")), "\r\nOK\r\n");
        }
        // Make FARAWAY look like a very strong neighbour.
        rig.rx(&beacon_frame("FARAWAY", SVC_ACCEPTS), 120);
        assert_eq!(
            rig.m.directory().get("FARAWAY").and_then(|e| e.best_tier),
            Some(Tier::Sf5)
        );
        rig.out.clear();
        let t = rig.line("ATDT FARAWAY");
        assert!(t.contains("DIALING FARAWAY"), "{t:?}");
        rig.run(500);
        let reqs: Vec<_> = rig
            .r
            .sent
            .iter()
            .filter_map(|(_, tier, b)| {
                let f = decode(b).ok()?;
                (f.header.ftype == FrameType::CallReq).then_some(*tier)
            })
            .collect();
        assert!(!reqs.is_empty(), "no CallReq");
        assert!(reqs.iter().all(|t| *t == Tier::Sf12), "{reqs:?}");
        assert_eq!(rig.m.call_state(), 1);
        // No answer: NO ANSWER after S7, and the directory entry is intact.
        rig.run(40_000);
        assert!(rig.text().contains("NO ANSWER"), "{:?}", rig.text());
        assert!(rig.m.directory().get("FARAWAY").is_some());
        assert_eq!(rig.line("AT"), "\r\nOK\r\n");
    }
}

#[test]
fn beacon_hear_updates_directory_with_services_and_tier() {
    let mut rig = Rig::new("LISTENER");
    rig.rx(&beacon_frame("NEARBY", SVC_ACCEPTS | SVC_HOTSPOT), 30);
    let e = rig.m.directory().get("nearby").expect("entry");
    assert_eq!(e.services, SVC_ACCEPTS | SVC_HOTSPOT);
    assert_eq!(e.last_snr_x4, 30);
    assert_eq!(e.last_rssi, -100);
    let first = e.best_tier;
    assert!(first.is_some());
    assert!(e.hits == 1 && !e.pinned);
    // S20 is the margin: a bigger margin picks a slower tier.
    assert_eq!(rig.line("ATS20=60"), "\r\nOK\r\n");
    rig.rx(&beacon_frame("NEARBY", SVC_ACCEPTS), 30);
    let slower = rig.m.directory().get("NEARBY").and_then(|e| e.best_tier);
    assert!(slower < first, "{slower:?} not slower than {first:?}");
    // A foreign frame, a short one and a bad payload are all ignored.
    let before = rig.m.directory().len();
    let mut bad = beacon_frame("BADONE", 0);
    bad.truncate(bad.len() - 1);
    rig.rx(&bad, 40);
    rig.rx(&[0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9], 40);
    rig.rx(
        &frame(FrameType::Beacon, 0, &[3, b'A', b':', b'B', 0, 0, 0]),
        40,
    );
    assert_eq!(rig.m.directory().len(), before);
    // The mode names.
    assert_eq!(BeaconMode::parse("hidden"), Some(BeaconMode::Hidden));
    assert_eq!(rig.m.beacon_mode(), BeaconMode::On);
    let _ = BEACON_EVERY_MS;
}
