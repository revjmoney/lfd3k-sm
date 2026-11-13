//! Shared harness for the voice tests in `sim/tests` (Task 9): voice apps
//! (the real call engine, `lfd3k_voice::Engine`) and raw voice host
//! protocol clients on modems of one simulated channel (`lfd3k_sim::Multi`).
//!
//! Time moves in 10 ms steps, like an app's audio loop: each step every
//! app's bytes go to its modem session, the channel runs 10 ms, then every
//! app takes its modem's bytes, ticks, feeds 10 ms of mic audio (a tone or
//! silence) and pulls 10 ms of speaker audio, and its UI events are logged
//! with the time. Raw clients log every decoded VHP frame they get and
//! send a Heartbeat every second. Everything is seeded: the same test does
//! the same thing every run, on any machine and under any load (no clocks,
//! threads or sockets).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lfd3k_core::voice::host::{cobs_decode, cobs_encode, msg};
use lfd3k_core::{Config, Tier};
use lfd3k_sim::{Multi, SimChannel};
use lfd3k_voice::{Engine, Identity, UiEvent};

pub const STEP_MS: u32 = 10;
const CHUNK: usize = 160;

/// A modem config: station `name`, tier cap `cap` (S21), no training
/// margin (S20 = 0) so a call trains to the cap whenever the SNR clears
/// that tier's floor (as `lfd3k-voicesim` does).
pub fn cfg(name: &str, cap: Option<Tier>) -> Config {
    let mut c = Config::new(name).unwrap();
    if let Some(t) = cap {
        let i = Tier::LADDER.iter().position(|&x| x == t).unwrap();
        c.sregs.set(21, i as u16);
    }
    c.sregs.set(20, 0);
    c
}

/// A fresh folder for one engine's trust store.
pub fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "lfd3k-voice-sim-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mic {
    Silent,
    /// A 400 Hz tone, about -12 dBFS.
    Tone,
}

pub struct App {
    pub m: usize,
    pub s: u8,
    pub eng: Engine,
    pub log: Vec<(u32, UiEvent)>,
    /// Answer as soon as it rings.
    pub auto_answer: bool,
    /// Press TALK ANYWAY as soon as the call is live (words not checked).
    pub auto_talk: bool,
    pub mic: Mic,
    /// Speaker samples that were not silence.
    pub heard_samples: u64,
    phase: u32,
}

pub struct Raw {
    pub m: usize,
    pub s: u8,
    rx: Vec<u8>,
    /// Every frame from the modem: (time, decoded frame).
    pub frames: Vec<(u32, Vec<u8>)>,
    /// Send a Heartbeat every second.
    pub alive: bool,
}

/// Rewrites the bytes an app sends its modem (app index, bytes).
pub type Tamper = Box<dyn FnMut(usize, Vec<u8>) -> Vec<u8>>;

pub struct World {
    pub net: Multi,
    pub apps: Vec<App>,
    pub raws: Vec<Raw>,
    pub tamper: Option<Tamper>,
}

impl World {
    pub fn new(cfgs: Vec<Config>, snr_db: i16, seed: u64) -> World {
        World::with_channel(cfgs, SimChannel::constant_db(snr_db, seed))
    }

    pub fn with_channel(cfgs: Vec<Config>, chan: SimChannel) -> World {
        World {
            net: Multi::with_configs(chan, cfgs),
            apps: Vec::new(),
            raws: Vec::new(),
            tamper: None,
        }
    }

    /// A voice app with a new identity on modem `m`, session `s`; it
    /// answers and talks anyway by itself. Returns its index.
    pub fn app(&mut self, m: usize, s: u8, name: &str, label: &str) -> usize {
        let id = Identity::generate(name, label).unwrap();
        self.app_with(m, s, id)
    }

    pub fn app_with(&mut self, m: usize, s: u8, id: Identity) -> usize {
        let eng = Engine::new(id, temp_dir().join("trust.json"));
        self.apps.push(App {
            m,
            s,
            eng,
            log: Vec::new(),
            auto_answer: true,
            auto_talk: true,
            mic: Mic::Silent,
            heard_samples: 0,
            phase: 0,
        });
        self.apps.len() - 1
    }

    /// A raw VHP client on modem `m`, session `s`: Hello, then the frames
    /// the test sends. Returns its index.
    pub fn raw(&mut self, m: usize, s: u8) -> usize {
        self.raws.push(Raw {
            m,
            s,
            rx: Vec::new(),
            frames: Vec::new(),
            alive: true,
        });
        let i = self.raws.len() - 1;
        self.net.voice_send(m, s, &[0]);
        self.raw_send(i, &[msg::HELLO, 1]);
        i
    }

    /// One decoded frame from raw client `i` to its modem.
    pub fn raw_send(&mut self, i: usize, frame: &[u8]) {
        let mut w = [0u8; 300];
        let n = cobs_encode(frame, &mut w).unwrap();
        let (m, s) = (self.raws[i].m, self.raws[i].s);
        self.net.voice_send(m, s, &w[..n]);
    }

    pub fn now(&self) -> u32 {
        self.net.now_ms
    }

    pub fn step(&mut self) {
        for k in 0..self.apps.len() {
            let mut b = self.apps[k].eng.take_modem_bytes();
            if let Some(t) = self.tamper.as_mut() {
                b = t(k, b);
            }
            if !b.is_empty() {
                let (m, s) = (self.apps[k].m, self.apps[k].s);
                self.net.voice_send(m, s, &b);
            }
        }
        if self.now().is_multiple_of(1000) {
            for i in 0..self.raws.len() {
                if self.raws[i].alive {
                    self.raw_send(i, &[msg::HEARTBEAT]);
                }
            }
        }
        self.net.run_ms(STEP_MS);
        let now = self.now();
        for a in self.apps.iter_mut() {
            let b = self.net.voice_recv(a.m, a.s);
            a.eng.on_modem_bytes(&b);
            a.eng.tick(now);
            let mut mic = [0i16; CHUNK];
            if a.mic == Mic::Tone {
                for v in mic.iter_mut() {
                    let t = a.phase as f64 / 16_000.0;
                    *v = (8_000.0 * (2.0 * std::f64::consts::PI * 400.0 * t).sin()) as i16;
                    a.phase = a.phase.wrapping_add(1);
                }
            }
            a.eng.mic(&mic);
            let mut spk = [0i16; CHUNK];
            a.eng.speaker(&mut spk);
            a.heard_samples += spk.iter().filter(|&&v| v != 0).count() as u64;
            while let Some(ev) = a.eng.poll_event() {
                match &ev {
                    UiEvent::Ringing { .. } if a.auto_answer => a.eng.answer(),
                    UiEvent::Connected { trust, .. }
                        if a.auto_talk && *trust != lfd3k_voice::Trust::Conflict =>
                    {
                        a.eng.talk_anyway()
                    }
                    _ => {}
                }
                if !matches!(ev, UiEvent::Scope { .. } | UiEvent::Air(_)) || a.log.len() < 200_000 {
                    a.log.push((now, ev));
                }
            }
        }
        for r in self.raws.iter_mut() {
            let b = self.net.voice_recv(r.m, r.s);
            for &x in &b {
                if x != 0 {
                    r.rx.push(x);
                    continue;
                }
                let mut f = [0u8; 300];
                if let Some(n) = cobs_decode(&r.rx, &mut f).filter(|&n| n > 0) {
                    r.frames.push((now, f[..n].to_vec()));
                }
                r.rx.clear();
            }
        }
    }

    pub fn run(&mut self, ms: u32) {
        for _ in 0..ms.div_ceil(STEP_MS) {
            self.step();
        }
    }

    /// Run until `cond` holds or `max_ms` passed; true when it held.
    pub fn run_until(&mut self, max_ms: u32, mut cond: impl FnMut(&World) -> bool) -> bool {
        let end = self.now() + max_ms;
        while self.now() < end {
            if cond(self) {
                return true;
            }
            self.step();
        }
        cond(self)
    }

    /// The first time app `a` logged an event matching `f`.
    pub fn when(&self, a: usize, f: impl Fn(&UiEvent) -> bool) -> Option<u32> {
        self.apps[a].log.iter().find(|(_, e)| f(e)).map(|(t, _)| *t)
    }

    pub fn has(&self, a: usize, f: impl Fn(&UiEvent) -> bool) -> bool {
        self.when(a, f).is_some()
    }

    /// Frames modem `m` put on the air: (start ms, type, sub-type or 0xFF, bytes).
    pub fn sent(&self, m: usize) -> Vec<(u32, u8, u8, Vec<u8>)> {
        self.net
            .air_log()
            .iter()
            .filter(|e| e.1 == m)
            .map(|(t, _, _, b)| {
                let ty = b.get(1).map_or(0, |x| x >> 4);
                let sub = if ty == 15 {
                    b.get(5).copied().unwrap_or(0xFF)
                } else {
                    0xFF
                };
                (*t, ty, sub, b.clone())
            })
            .collect()
    }

    /// Raw client `i`'s frames of type `t`: (time, payload after the type).
    pub fn raw_frames(&self, i: usize, t: u8) -> Vec<(u32, Vec<u8>)> {
        self.raws[i]
            .frames
            .iter()
            .filter(|(_, f)| f.first() == Some(&t))
            .map(|(at, f)| (*at, f[1..].to_vec()))
            .collect()
    }
}

pub fn is_ring(e: &UiEvent) -> bool {
    matches!(e, UiEvent::Ringing { .. })
}

pub fn is_connected(e: &UiEvent) -> bool {
    matches!(e, UiEvent::Connected { .. })
}

/// A VHP frame: the type, then each string as a length byte and its bytes.
pub fn strs(t: u8, a: &[&str]) -> Vec<u8> {
    let mut v = vec![t];
    for s in a {
        v.push(s.len() as u8);
        v.extend_from_slice(s.as_bytes());
    }
    v
}
