//! `--trace <dir>` (a test helper for the testing video): what the built-in
//! modem shows and says, logged on the wall clock (Unix milliseconds) so
//! two copies of the app line up.
//!
//! - `status.csv`: every 250 ms the fields of the firmware's
//!   `lfd3k_status_t` (as `lfd3k_status` fills them) plus the last 64 scope
//!   samples (the panel's channel trace), for `tools/panel_replay.c`.
//! - `host.log`: the AT lines the app sent to the modem (`>`), the modem's
//!   replies (`<`), and the result line the core's own formatter
//!   (`format_result`) gives for each call state change (`!`).
//! - `marks.csv`: where the `--record` WAV starts a run of samples and
//!   where a `play` segment of the `--play` WAV starts.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use lfd3k_core::{format_result, Modem, ResultCode};

/// Scope samples kept for the panel (the firmware's `RF_TRACE_N`).
const TRACE_N: usize = 64;
/// One status line this often.
const STATUS_MS: u32 = 250;
/// The panel's HEARD window (15 minutes, as the firmware's).
const HEARD_WINDOW_MS: u32 = 900_000;

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub struct Trace {
    status: File,
    host: File,
    marks: File,
    unix0: u64,
    now0: u32,
    status_due: u32,
    last_state: u8,
    last_tier: Option<u8>,
    ended_logged: bool,
    dbm: [i8; TRACE_N],
    tx: [u8; TRACE_N],
    head: usize,
    count: usize,
}

impl Trace {
    pub fn open(dir: &Path, now: u32) -> std::io::Result<Trace> {
        std::fs::create_dir_all(dir)?;
        let mut status = File::create(dir.join("status.csv"))?;
        writeln!(
            status,
            "unix_ms,lights,state,tier,rssi,snr_x4,radio_ok,wire,simnet,scanning,scan_hits,heard,trace_dbm,trace_tx"
        )?;
        let mut marks = File::create(dir.join("marks.csv"))?;
        writeln!(marks, "what,unix_ms,a,b")?;
        Ok(Trace {
            status,
            host: File::create(dir.join("host.log"))?,
            marks,
            unix0: unix_ms(),
            now0: now,
            status_due: now,
            last_state: 0,
            last_tier: None,
            ended_logged: false,
            dbm: [0; TRACE_N],
            tx: [0; TRACE_N],
            head: 0,
            count: 0,
        })
    }

    /// The wall clock time of the modem's `now`.
    pub fn unix(&self, now: u32) -> u64 {
        self.unix0 + u64::from(now.wrapping_sub(self.now0))
    }

    pub fn scope(&mut self, dbm: i16, tx: bool) {
        if let (Some(d), Some(t)) = (self.dbm.get_mut(self.head), self.tx.get_mut(self.head)) {
            *d = dbm.clamp(-128, 0) as i8;
            *t = u8::from(tx);
        }
        self.head = (self.head + 1) % TRACE_N;
        self.count = (self.count + 1).min(TRACE_N);
    }

    pub fn host(&mut self, now: u32, dir: char, text: &str) {
        let t = self.unix(now);
        for l in text.split('\n') {
            let l = l.trim_end_matches('\r');
            // A hotspot or client password never goes into the log.
            let masked;
            let l = if (l.starts_with("WAP ") || l.starts_with("WSTA ")) && l.split(' ').count() > 3
            {
                masked = l.split(' ').take(3).collect::<Vec<_>>().join(" ") + " ********";
                masked.as_str()
            } else {
                l
            };
            if !l.is_empty() {
                let _ = writeln!(self.host, "{t} {dir} {l}");
            }
        }
    }

    fn result(&mut self, now: u32, code: &ResultCode) {
        let mut b = [0u8; 64];
        let n = format_result(code, true, &mut b);
        let s = String::from_utf8_lossy(b.get(..n).unwrap_or(&[])).to_string();
        self.host(now, '!', &s);
    }

    /// DECLINED and NO ANSWER come from the engine (the call ends without a
    /// connect); the state change to idle then adds no NO CARRIER.
    pub fn ended(&mut self, now: u32, code: &ResultCode) {
        self.result(now, code);
        self.ended_logged = true;
    }

    pub fn mark(&mut self, what: &str, unix: u64, a: usize, b: usize) {
        let _ = writeln!(self.marks, "{what},{unix},{a},{b}");
    }

    pub fn tick(&mut self, m: &Modem, now: u32) {
        let st = m.call_state();
        let tier = m.tier().map(|t| t.index());
        let connected = matches!(st, 4 | 5);
        let was = matches!(self.last_state, 4 | 5);
        if st == 2 && self.last_state != 2 {
            self.result(now, &ResultCode::Ring);
        }
        if connected && !was {
            if let Some(t) = m.tier() {
                self.result(now, &ResultCode::ConnectVoice(t));
            }
        } else if connected && tier != self.last_tier {
            if let Some(t) = m.tier() {
                self.result(now, &ResultCode::Retrain(t));
            }
        }
        if st == 0 && self.last_state != 0 {
            if !self.ended_logged {
                self.result(now, &ResultCode::NoCarrier);
            }
            self.ended_logged = false;
        }
        if st != 0 && self.last_state == 0 {
            self.ended_logged = false;
        }
        self.last_state = st;
        self.last_tier = tier;
        if now.wrapping_sub(self.status_due) >= u32::MAX / 2 {
            return;
        }
        self.status_due = now.wrapping_add(STATUS_MS);
        let l = m.lights(now);
        let bits = [l.oh, l.cd, l.sd, l.rd, l.hs, l.ec, l.dc, l.aa];
        let lights = bits
            .iter()
            .enumerate()
            .fold(0u8, |a, (i, &on)| a | (u8::from(on) << i));
        let (rssi, snr) = m.last_rx().map_or((0, 0), |i| (i.rssi_dbm, i.snr_x4));
        let start = (self.head + TRACE_N - self.count) % TRACE_N;
        let mut dbm = String::new();
        let mut tx = String::new();
        for i in 0..self.count {
            let k = (start + i) % TRACE_N;
            dbm.push_str(&format!(
                "{:02X}",
                self.dbm.get(k).copied().unwrap_or(0) as u8
            ));
            tx.push(if self.tx.get(k).copied().unwrap_or(0) != 0 {
                '1'
            } else {
                '0'
            });
        }
        let _ = writeln!(
            self.status,
            "{},{lights},{st},{},{rssi},{snr},1,{},1,{},{},{},{dbm},{tx}",
            self.unix(now),
            tier.map_or(-1, i16::from),
            m.wire_state(),
            u8::from(m.scanning()),
            m.scan_hits(),
            m.directory().heard_count(now, HEARD_WINDOW_MS)
        );
    }
}
