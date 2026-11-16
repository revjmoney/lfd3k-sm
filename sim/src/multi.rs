//! N modems on one simulated channel, each with voice host protocol
//! sessions (the real VHP: `Modem::voice_open`, `voice_input`,
//! `voice_output`), for voice calls between voice apps in the simulator.
//!
//! The channel is the two modem harness's (`SimChannel`, `SimRadio`, the
//! loss table, the preamble and collision rules in the crate docs) with
//! every frame heard by every other radio: each receiver gets its own
//! early checks (sending, not listening on that tier, preamble not
//! detected), its own collisions and its own loss draw at the frame's end.
//! One SNR profile holds for every pair. Loss draws come from one seeded
//! RNG in a fixed order (by end time, then lowest radio index), so a run is
//! the same for the same seed and inputs.
//!
//! Time moves in 1 ms steps as in `Harness`: TX done ticks, deliveries,
//! every 10 ms a tick of every modem (index order), then the voice
//! sessions: input bytes a modem did not take yet are offered again, and
//! everything `voice_output` has is collected (every step: the modem keeps
//! only the last VoiceIn frame, so it must be read often). Host output
//! (the terminal) is kept (the newest `HOST_KEEP` bytes) for `host_text`,
//! and `host_send` types into a modem's terminal.
//!
//! For tests: `set_snr_x4` changes the channel SNR from now on, `air_log`
//! lists every frame put on the air (start time, sender, tier, bytes) and
//! `collisions` counts receptions spoiled by an overlapping frame.
//!
//! Sessions 1 to 3 only (session 0 is the host stream after `AT+VOICE`).
//! A session opens on the first `voice_send` to it.

use std::collections::VecDeque;

use lfd3k_core::voice::VHP_SESSIONS;
use lfd3k_core::{Config, Modem, RxInfo, Tier};

use crate::{loss_x1000, preamble_us, Outcome, Rng, Rx, SimChannel, SimRadio};

/// Host output kept per modem for `host_text` (the newest bytes).
const HOST_KEEP: usize = 64 * 1024;

/// One frame put on the air: start (ms), sender, tier, bytes.
pub type AirLogEntry = (u32, usize, Tier, Vec<u8>);

/// Per modem: pending input and collected output for each session.
struct Sessions {
    open: [bool; VHP_SESSIONS],
    input: [VecDeque<u8>; VHP_SESSIONS],
    output: [Vec<u8>; VHP_SESSIONS],
}

impl Sessions {
    fn new() -> Sessions {
        Sessions {
            open: [false; VHP_SESSIONS],
            input: std::array::from_fn(|_| VecDeque::new()),
            output: std::array::from_fn(|_| Vec::new()),
        }
    }
}

/// N modems on one channel (see the module docs).
pub struct Multi {
    pub modems: Vec<Modem>,
    /// Simulated time. Read only: advance it with `run_ms`.
    pub now_ms: u32,
    radios: Vec<SimRadio>,
    chan: SimChannel,
    rng: Rng,
    sent: Vec<usize>,
    sessions: Vec<Sessions>,
    host_in: Vec<VecDeque<u8>>,
    host_out: Vec<Vec<u8>>,
    air_log: Vec<AirLogEntry>,
    collisions: usize,
}

impl Multi {
    /// One modem per station name, default settings.
    ///
    /// # Panics
    /// On a station name `Config::new` refuses (a harness should fail loudly).
    pub fn new(chan: SimChannel, names: &[&str]) -> Multi {
        let cfgs = names
            .iter()
            .map(|n| Config::new(n).unwrap_or_else(|| panic!("bad station name {n}")))
            .collect();
        Multi::with_configs(chan, cfgs)
    }

    /// One modem per config (for S registers set before the start, such as
    /// an S21 tier cap).
    pub fn with_configs(chan: SimChannel, cfgs: Vec<Config>) -> Multi {
        let n = cfgs.len();
        Multi {
            modems: cfgs.into_iter().map(Modem::new).collect(),
            now_ms: 0,
            radios: (0..n).map(|_| SimRadio::new()).collect(),
            rng: Rng::new(chan.seed),
            chan,
            sent: vec![0; n],
            sessions: (0..n).map(|_| Sessions::new()).collect(),
            host_in: (0..n).map(|_| VecDeque::new()).collect(),
            host_out: vec![Vec::new(); n],
            air_log: Vec::new(),
            collisions: 0,
        }
    }

    /// The channel SNR (dB x4) from now on, for every pair.
    pub fn set_snr_x4(&mut self, snr_x4: i16) {
        self.chan.profile = crate::SnrProfile::Constant(snr_x4);
    }

    /// Bytes typed into modem `m`'s terminal (host stream). What the modem
    /// does not take at once is offered again every step.
    pub fn host_send(&mut self, m: usize, bytes: &[u8]) {
        if let Some(q) = self.host_in.get_mut(m) {
            q.extend(bytes.iter().copied());
        }
        self.host_service(m);
    }

    /// Modem `m`'s terminal output since the last call (the newest
    /// `HOST_KEEP` bytes), as text.
    pub fn host_text(&mut self, m: usize) -> String {
        let b = self
            .host_out
            .get_mut(m)
            .map(std::mem::take)
            .unwrap_or_default();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Every frame put on the air so far, in start order.
    pub fn air_log(&self) -> &[AirLogEntry] {
        &self.air_log
    }

    /// Receptions spoiled because another frame overlapped them at that
    /// receiver (counted once per receiver and frame).
    pub fn collisions(&self) -> usize {
        self.collisions
    }

    /// Advance `ms` milliseconds.
    pub fn run_ms(&mut self, ms: u32) {
        for _ in 0..ms {
            self.step();
        }
    }

    /// App bytes for modem `m`'s session `session` (1 to 3; opened on first
    /// use). What the modem does not take at once is offered again every
    /// step, in order.
    pub fn voice_send(&mut self, m: usize, session: u8, bytes: &[u8]) {
        let s = usize::from(session);
        if s == 0 || s >= VHP_SESSIONS || m >= self.modems.len() {
            return;
        }
        let now = self.now_ms;
        let Some(ss) = self.sessions.get_mut(m) else {
            return;
        };
        if !ss.open[s] {
            if let Some(md) = self.modems.get_mut(m) {
                md.voice_open(session, now);
                // The simulated apps are local, like the USB cable: full
                // control (the firmware's TCP sessions never get it).
                md.voice_grant_settings(session);
            }
            ss.open[s] = true;
        }
        ss.input[s].extend(bytes.iter().copied());
        self.service(m);
    }

    /// Everything modem `m` sent its app on `session` since the last call.
    pub fn voice_recv(&mut self, m: usize, session: u8) -> Vec<u8> {
        self.service(m);
        let s = usize::from(session);
        self.sessions
            .get_mut(m)
            .and_then(|ss| ss.output.get_mut(s))
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Close modem `m`'s session (the app's socket closed).
    pub fn voice_close(&mut self, m: usize, session: u8) {
        let s = usize::from(session);
        let now = self.now_ms;
        if let (Some(md), Some(ss)) = (self.modems.get_mut(m), self.sessions.get_mut(m)) {
            if ss.open.get(s).copied().unwrap_or(false) {
                md.voice_close(session, now);
                ss.open[s] = false;
                ss.input[s].clear();
            }
        }
    }

    /// Frames modem `m` has put on the air.
    pub fn frames_sent_by(&self, m: usize) -> usize {
        self.sent.get(m).copied().unwrap_or(0)
    }

    /// Modem `m`'s radio.
    pub fn radio(&self, m: usize) -> Option<&SimRadio> {
        self.radios.get(m)
    }

    // Internals.

    fn step(&mut self) {
        let prev_us = u64::from(self.now_ms) * 1000;
        self.now_ms = self.now_ms.wrapping_add(1);
        let now_us = u64::from(self.now_ms) * 1000;
        for r in self.radios.iter_mut() {
            r.now_us = now_us;
        }
        for i in 0..self.radios.len() {
            let end = self.radios[i].tx_end_us;
            if end > prev_us && end <= now_us {
                self.with(i, |m, r, now| m.tick(r, now));
            }
        }
        self.deliver_due(now_us);
        if self.now_ms.is_multiple_of(10) {
            for i in 0..self.modems.len() {
                self.with(i, |m, r, now| m.tick(r, now));
            }
        }
        for i in 0..self.modems.len() {
            self.host_service(i);
            self.service(i);
        }
    }

    /// Offer typed bytes to modem `i`'s terminal.
    fn host_service(&mut self, i: usize) {
        let pending: Vec<u8> = match self.host_in.get(i) {
            Some(q) if !q.is_empty() => q.iter().copied().collect(),
            _ => return,
        };
        let mut took = 0;
        self.with(i, |m, r, now| took = m.host_input(&pending, r, now));
        if let Some(q) = self.host_in.get_mut(i) {
            q.drain(..took.min(pending.len()));
        }
    }

    /// Offer pending input to modem `i`'s open sessions and collect output.
    fn service(&mut self, i: usize) {
        for s in 1..VHP_SESSIONS {
            let pending: Vec<u8> = match self.sessions.get(i) {
                Some(ss) if ss.open[s] && !ss.input[s].is_empty() => {
                    ss.input[s].iter().copied().collect()
                }
                _ => Vec::new(),
            };
            if !pending.is_empty() {
                let mut took = 0;
                self.with(i, |m, r, now| {
                    took = m.voice_input(s as u8, &pending, r, now)
                });
                if let Some(ss) = self.sessions.get_mut(i) {
                    ss.input[s].drain(..took.min(pending.len()));
                }
            }
            let Some(md) = self.modems.get_mut(i) else {
                return;
            };
            let Some(ss) = self.sessions.get_mut(i) else {
                return;
            };
            if !ss.open[s] {
                continue;
            }
            let mut buf = [0u8; 512];
            loop {
                let n = md.voice_output(s as u8, &mut buf);
                if n == 0 {
                    break;
                }
                ss.output[s].extend_from_slice(buf.get(..n).unwrap_or(&[]));
            }
        }
    }

    /// Run `f` on modem `i` and its radio, then put sent frames on the air
    /// and drop the host output.
    fn with(&mut self, i: usize, f: impl FnOnce(&mut Modem, &mut SimRadio, u32)) {
        let now = self.now_ms;
        let (Some(m), Some(r)) = (self.modems.get_mut(i), self.radios.get_mut(i)) else {
            return;
        };
        r.now_us = u64::from(now) * 1000;
        f(m, r, now);
        let out = std::mem::take(&mut r.outbox);
        let mut buf = [0u8; 256];
        loop {
            let n = m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            if let Some(h) = self.host_out.get_mut(i) {
                h.extend_from_slice(buf.get(..n).unwrap_or(&[]));
                if h.len() > HOST_KEEP {
                    let cut = h.len() - HOST_KEEP;
                    h.drain(..cut);
                }
            }
        }
        for (tier, bytes) in out {
            self.start_frame(i, tier, bytes);
        }
    }

    fn rx_info(&self, tier: Tier) -> RxInfo {
        let snr_x4 = self.chan.profile.at(self.now_ms);
        RxInfo {
            tier,
            rssi_dbm: (-120i16).saturating_add(snr_x4 / 4),
            snr_x4,
        }
    }

    fn deliver_due(&mut self, now_us: u64) {
        loop {
            let mut next: Option<(u64, usize, usize)> = None;
            for (side, r) in self.radios.iter().enumerate() {
                for (k, rx) in r.rx.iter().enumerate() {
                    if rx.end_us <= now_us && next.is_none_or(|n| rx.end_us < n.0) {
                        next = Some((rx.end_us, side, k));
                    }
                }
            }
            let Some((_, side, k)) = next else { return };
            let rx = self.radios[side].rx.remove(k);
            let mut outcome = rx.outcome;
            if rx.alive && outcome == Outcome::Pending {
                let snr = self.chan.profile.at(self.now_ms);
                let margin = snr.saturating_sub(rx.tier.snr_floor_x4());
                let lost = self.rng.chance(loss_x1000(margin));
                outcome = if rx.hooked {
                    Outcome::Dropped
                } else if lost {
                    Outcome::Lost
                } else {
                    Outcome::Delivered
                };
            }
            if outcome == Outcome::Delivered {
                let info = self.rx_info(rx.tier);
                self.with(side, |m, r, now| m.radio_rx(&rx.bytes, info, r, now));
            }
        }
    }

    /// A frame from radio `from` goes on the air: every other radio may hear it.
    fn start_frame(&mut self, from: usize, tier: Tier, bytes: Vec<u8>) {
        let now = self.now_ms;
        let now_us = u64::from(now) * 1000;
        let end_us = now_us + u64::from(tier.airtime_us(bytes.len()));
        if let Some(n) = self.sent.get_mut(from) {
            *n += 1;
        }
        self.air_log.push((now, from, tier, bytes.clone()));
        let hooked = self.chan.drop.is_some_and(|f| f(now, &bytes));
        let snr = self.chan.profile.at(now);
        let margin = snr.saturating_sub(tier.snr_floor_x4());
        for (to, r) in self.radios.iter_mut().enumerate() {
            if to == from {
                continue;
            }
            r.now_us = now_us;
            if r.transmitting() || r.listen != Some(tier) || margin < -8 {
                continue;
            }
            let mut collided = false;
            for other in r.rx.iter_mut().filter(|x| x.alive && x.end_us > now_us) {
                collided = true;
                if other.outcome == Outcome::Pending {
                    other.outcome = Outcome::Collided;
                    self.collisions += 1;
                }
            }
            if collided {
                self.collisions += 1;
            }
            r.rx.push(Rx {
                trace: 0,
                tier,
                bytes: bytes.clone(),
                pre_end_us: now_us + preamble_us(tier),
                end_us,
                alive: true,
                hooked,
                outcome: if collided {
                    Outcome::Collided
                } else {
                    Outcome::Pending
                },
            });
        }
    }
}
