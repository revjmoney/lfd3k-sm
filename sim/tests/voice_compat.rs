//! Task 9 old build compatibility: the core of commit 6364009 (the last
//! build without voice, public on GitHub; the `lfd3k-core-old` dev
//! dependency) next to this build's core on one channel.
//!
//! The two cores are separate crates with their own `Modem`, `Radio` and
//! `Tier`, so they cannot share `Multi`: this file has its own small
//! channel. Every frame is heard by every other station listening on its
//! spreading factor and not sending when it starts, once its airtime is
//! over, at 10 dB SNR, nothing lost. Stations tick every 10 ms and when
//! their own frame ends. Tiers are matched by spreading factor.
//!
//! What a build without voice must do with voice traffic (spec 5.2, plan
//! global constraints): a voice call request is a type 15 frame, which it
//! drops as an unknown frame type, so a voice call to a name that is also
//! its station name never rings it (with S0=1 too) and it sends nothing
//! back; the voice flag on a ScanReq is ignored; beacons, scans and data
//! calls work both ways.

use lfd3k_core::voice::host::{cobs_decode, cobs_encode, msg};
use lfd3k_core_old as old;

#[derive(Default)]
struct Rad {
    now_us: u64,
    listen_sf: Option<u8>,
    tx_until_us: u64,
    out: Vec<(u8, Vec<u8>)>,
}

impl Rad {
    fn on_send(&mut self, sf: u8, airtime_us: u32, frame: &[u8]) {
        self.tx_until_us = self.now_us + u64::from(airtime_us);
        self.out.push((sf, frame.to_vec()));
    }
}

struct NewRad(Rad);
struct OldRad(Rad);

impl lfd3k_core::Radio for NewRad {
    fn send(&mut self, tier: lfd3k_core::Tier, frame: &[u8]) {
        self.0
            .on_send(tier.sf(), tier.airtime_us(frame.len()), frame);
    }
    fn listen(&mut self, tier: lfd3k_core::Tier) {
        self.0.listen_sf = Some(tier.sf());
    }
    fn busy(&self) -> bool {
        self.0.now_us < self.0.tx_until_us
    }
}

impl old::Radio for OldRad {
    fn send(&mut self, tier: old::Tier, frame: &[u8]) {
        self.0
            .on_send(tier.sf(), tier.airtime_us(frame.len()), frame);
    }
    fn listen(&mut self, tier: old::Tier) {
        self.0.listen_sf = Some(tier.sf());
    }
    fn busy(&self) -> bool {
        self.0.now_us < self.0.tx_until_us
    }
}

enum St {
    New(Box<lfd3k_core::Modem>, NewRad),
    Old(Box<old::Modem>, OldRad),
}

impl St {
    fn new(name: &str) -> St {
        St::New(
            Box::new(lfd3k_core::Modem::new(
                lfd3k_core::Config::new(name).unwrap(),
            )),
            NewRad(Rad::default()),
        )
    }

    fn old(name: &str, s0: u16) -> St {
        let mut c = old::Config::new(name).unwrap();
        c.sregs.set(0, s0);
        St::Old(Box::new(old::Modem::new(c)), OldRad(Rad::default()))
    }

    fn rad(&mut self) -> &mut Rad {
        match self {
            St::New(_, r) => &mut r.0,
            St::Old(_, r) => &mut r.0,
        }
    }

    fn tick(&mut self, now: u32) {
        match self {
            St::New(m, r) => m.tick(r, now),
            St::Old(m, r) => m.tick(r, now),
        }
    }

    fn rx(&mut self, sf: u8, frame: &[u8], now: u32) {
        match self {
            St::New(m, r) => {
                let tier = lfd3k_core::Tier::LADDER
                    .into_iter()
                    .find(|t| t.sf() == sf)
                    .unwrap();
                let info = lfd3k_core::RxInfo {
                    tier,
                    rssi_dbm: -80,
                    snr_x4: 40,
                };
                m.radio_rx(frame, info, r, now)
            }
            St::Old(m, r) => {
                let tier = old::Tier::LADDER
                    .into_iter()
                    .find(|t| t.sf() == sf)
                    .unwrap();
                let info = old::RxInfo {
                    tier,
                    rssi_dbm: -80,
                    snr_x4: 40,
                };
                m.radio_rx(frame, info, r, now)
            }
        }
    }

    fn host_in(&mut self, b: &[u8], now: u32) -> usize {
        match self {
            St::New(m, r) => m.host_input(b, r, now),
            St::Old(m, r) => m.host_input(b, r, now),
        }
    }

    fn host_out(&mut self, buf: &mut [u8]) -> usize {
        match self {
            St::New(m, _) => m.host_output(buf),
            St::Old(m, _) => m.host_output(buf),
        }
    }
}

struct Flight {
    end_us: u64,
    from: usize,
    sf: u8,
    bytes: Vec<u8>,
    to: Vec<usize>,
}

struct Net {
    st: Vec<St>,
    now: u32,
    air: Vec<Flight>,
    host: Vec<Vec<u8>>,
    /// Every frame sent: (start ms, station, sf, bytes).
    log: Vec<(u32, usize, u8, Vec<u8>)>,
    /// Pending bytes for the new station's voice session 1, and what came back.
    vin: Vec<u8>,
    vout: Vec<u8>,
}

impl Net {
    fn new(st: Vec<St>) -> Net {
        let n = st.len();
        let mut net = Net {
            st,
            now: 0,
            air: Vec::new(),
            host: vec![Vec::new(); n],
            log: Vec::new(),
            vin: Vec::new(),
            vout: Vec::new(),
        };
        net.run(10);
        net
    }

    fn after(&mut self, i: usize) {
        let now = self.now;
        let mut buf = [0u8; 512];
        loop {
            let n = self.st[i].host_out(&mut buf);
            if n == 0 {
                break;
            }
            self.host[i].extend_from_slice(&buf[..n]);
        }
        let start = u64::from(now) * 1000;
        for (sf, bytes) in std::mem::take(&mut self.st[i].rad().out) {
            self.log.push((now, i, sf, bytes.clone()));
            let tier = lfd3k_core::Tier::LADDER
                .into_iter()
                .find(|t| t.sf() == sf)
                .unwrap();
            let end_us = start + u64::from(tier.airtime_us(bytes.len()));
            let to = (0..self.st.len())
                .filter(|&k| {
                    k != i && {
                        let r = self.st[k].rad();
                        r.listen_sf == Some(sf) && r.tx_until_us <= start
                    }
                })
                .collect();
            self.air.push(Flight {
                end_us,
                from: i,
                sf,
                bytes,
                to,
            });
        }
    }

    fn step(&mut self) {
        let prev = u64::from(self.now) * 1000;
        self.now += 1;
        let now_us = u64::from(self.now) * 1000;
        let now = self.now;
        for i in 0..self.st.len() {
            self.st[i].rad().now_us = now_us;
        }
        for i in 0..self.st.len() {
            let end = self.st[i].rad().tx_until_us;
            if end > prev && end <= now_us {
                self.st[i].tick(now);
                self.after(i);
            }
        }
        let (due, rest): (Vec<Flight>, Vec<Flight>) = std::mem::take(&mut self.air)
            .into_iter()
            .partition(|f| f.end_us <= now_us);
        self.air = rest;
        for f in due {
            for &k in &f.to {
                if k != f.from && self.st[k].rad().listen_sf == Some(f.sf) {
                    self.st[k].rx(f.sf, &f.bytes, now);
                    self.after(k);
                }
            }
        }
        if now.is_multiple_of(10) {
            for i in 0..self.st.len() {
                self.st[i].tick(now);
                self.after(i);
            }
        }
        // The new station (index 0) voice session 1: input and output.
        if let St::New(m, r) = &mut self.st[0] {
            if !self.vin.is_empty() {
                let took = m.voice_input(1, &self.vin, r, now);
                self.vin.drain(..took.min(self.vin.len()));
            }
            let mut buf = [0u8; 512];
            loop {
                let n = m.voice_output(1, &mut buf);
                if n == 0 {
                    break;
                }
                self.vout.extend_from_slice(&buf[..n]);
            }
            if now.is_multiple_of(1000) && m.voice_regs().iter().flatten().count() > 0 {
                let mut w = [0u8; 8];
                let n = cobs_encode(&[msg::HEARTBEAT], &mut w).unwrap();
                self.vin.extend_from_slice(&w[..n]);
            }
        }
        self.after(0);
    }

    fn run(&mut self, ms: u32) {
        for _ in 0..ms {
            self.step();
        }
    }

    fn run_until(&mut self, max: u32, mut f: impl FnMut(&mut Net) -> bool) -> bool {
        for _ in 0..max {
            if f(self) {
                return true;
            }
            self.step();
        }
        f(self)
    }

    fn type_in(&mut self, i: usize, s: &str) {
        let now = self.now;
        self.st[i].host_in(s.as_bytes(), now);
        self.after(i);
    }

    fn text(&self, i: usize) -> String {
        String::from_utf8_lossy(&self.host[i]).into_owned()
    }

    /// A decoded VHP frame to the new station's session 1 (opened first).
    fn vhp(&mut self, f: &[u8]) {
        if let St::New(m, _) = &mut self.st[0] {
            let now = self.now;
            m.voice_open(1, now);
        }
        let mut w = [0u8; 300];
        let n = cobs_encode(f, &mut w).unwrap();
        self.vin.extend_from_slice(&w[..n]);
        self.step();
    }

    /// The new station's VHP frames so far, decoded.
    fn vhp_frames(&self) -> Vec<Vec<u8>> {
        self.vout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| {
                let mut f = [0u8; 300];
                cobs_decode(p, &mut f).map(|n| f[..n].to_vec())
            })
            .collect()
    }

    /// Frame types (and type 15 sub-types) station `i` sent from `t` on.
    fn sent(&self, i: usize, t: u32) -> Vec<(u8, u8)> {
        self.log
            .iter()
            .filter(|e| e.1 == i && e.0 >= t)
            .map(|e| {
                let ty = e.3.get(1).map_or(0, |b| b >> 4);
                (
                    ty,
                    if ty == 15 {
                        e.3.get(5).copied().unwrap_or(0xFF)
                    } else {
                        0xFF
                    },
                )
            })
            .collect()
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

#[test]
fn old_build_ignores_voice_call_to_its_name() {
    for s0 in [0, 1] {
        // ALPHA (this build, voice app ANN/PC) dials voice REV; the old
        // station is named REV.
        let mut net = Net::new(vec![St::new("ALPHA"), St::old("REV", s0)]);
        net.vhp(&[msg::HELLO, 1]);
        net.vhp(&strs(msg::REGISTER, &["ANN", "PC"]));
        net.run(1_000);
        let t0 = net.now;
        net.vhp(&strs(msg::CALL, &["REV", ""]));
        net.run(40_000);
        let req = net
            .sent(0, t0)
            .iter()
            .filter(|&&(t, s)| t == 15 && s == 2)
            .count();
        assert!(
            req >= 5,
            "S0={s0}: ALPHA kept asking ({req} voice requests)"
        );
        let back: Vec<(u8, u8)> = net
            .sent(1, t0)
            .into_iter()
            .filter(|&(t, _)| t != 12)
            .collect();
        assert!(back.is_empty(), "S0={s0}: the old build sent {back:?}");
        let text = net.text(1);
        assert!(
            !text.contains("RING") && !text.contains("CONNECT"),
            "S0={s0}: {text:?}"
        );
        let f = net.vhp_frames();
        assert!(
            f.iter().any(|f| f[0] == msg::NO_ANSWER),
            "S0={s0}: ALPHA's app hears NoAnswer"
        );
        assert!(f.iter().all(|f| f[0] != msg::RING));
        // The old modem is fine afterwards: it still answers AT.
        net.host[1].clear();
        net.type_in(1, "AT\r");
        assert!(net.text(1).contains("OK"));
    }
}

#[test]
fn old_and_new_discover_each_other() {
    let mut net = Net::new(vec![St::new("ALPHA"), St::old("OLDIE", 0)]);
    // A voice name on the new station: its scans carry the voice flag and
    // it sends voice beacons too.
    net.vhp(&[msg::HELLO, 1]);
    net.vhp(&strs(msg::REGISTER, &["ANN", "PC"]));
    // Both see each other's beacons within the beacon interval (300 s
    // plus up to 60 s, the first a full interval after start).
    net.run(400_000);
    let vb = net
        .sent(0, 0)
        .iter()
        .filter(|&&(t, s)| t == 15 && s == 0)
        .count();
    let nb = net.sent(0, 0).iter().filter(|&&(t, _)| t == 12).count();
    let ob = net.sent(1, 0).iter().filter(|&&(t, _)| t == 12).count();
    assert!(
        vb >= 1 && nb >= 1 && ob >= 1,
        "beacons: new {nb} voice {vb}, old {ob}"
    );
    net.host[0].clear();
    net.host[1].clear();
    net.type_in(0, "AT+DIR?\r");
    net.type_in(1, "AT+DIR?\r");
    net.run(2_000);
    assert!(
        net.text(0).contains("DIR OLDIE"),
        "new lists old: {:?}",
        net.text(0)
    );
    assert!(
        net.text(1).contains("DIR ALPHA"),
        "old lists new: {:?}",
        net.text(1)
    );
    assert!(
        !net.text(1).contains("ANN"),
        "the old build knows nothing of voice names"
    );
    // Scans both ways. The new station's ScanReq carries the voice flag,
    // which the old build ignores: it answers with its normal ScanResp.
    for (i, other) in [(0usize, "OLDIE"), (1, "ALPHA")] {
        net.host[i].clear();
        net.run(11_000);
        net.type_in(i, "AT+SCAN\r");
        let ok = net.run_until(20_000, |n| n.text(i).contains("OK"));
        assert!(ok, "station {i} scan finished: {:?}", net.text(i));
        assert!(
            net.text(i).contains(&format!("SCAN {other}")),
            "station {i}: {:?}",
            net.text(i)
        );
    }
    // The old build answered the voice-flagged scan with a plain ScanResp.
    assert!(net.sent(1, 0).iter().any(|&(t, _)| t == 14));
    // The new build answered the old scan (no voice flag) the plain way too.
    assert!(net.sent(0, 0).iter().any(|&(t, _)| t == 14));
    assert!(
        !net.sent(0, 0).iter().any(|&(t, s)| t == 15 && s == 1),
        "no voice scan answer to an old scan"
    );
}

#[test]
fn old_new_data_call_works() {
    // Both ways: new dials old, then old dials new (S0=1 on the far end).
    for new_dials in [true, false] {
        let mut net = Net::new(vec![St::new("ALPHA"), St::old("OLDIE", 1)]);
        if !new_dials {
            net.type_in(0, "ATS0=1\r");
        }
        let (a, b, name) = if new_dials {
            (0, 1, "OLDIE")
        } else {
            (1, 0, "ALPHA")
        };
        net.type_in(a, &format!("ATDT {name}\r"));
        let up = net.run_until(30_000, |n| {
            n.text(a).contains("CONNECT") && n.text(b).contains("CONNECT")
        });
        assert!(up, "connected: {:?} / {:?}", net.text(a), net.text(b));
        assert!(net.text(a).contains("/ARQ") && !net.text(a).contains("VOICE"));
        net.host[a].clear();
        net.host[b].clear();
        net.type_in(a, "hello from the caller\r\n");
        net.type_in(b, "hello from the answerer\r\n");
        net.run(5_000);
        assert!(
            net.text(b).contains("hello from the caller"),
            "{:?}",
            net.text(b)
        );
        assert!(
            net.text(a).contains("hello from the answerer"),
            "{:?}",
            net.text(a)
        );
        // No type 15 on the air during the data call.
        assert!(net
            .log
            .iter()
            .all(|e| e.3.get(1).map(|b| b >> 4) != Some(15)));
    }
}
