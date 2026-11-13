//! The modem monitor: the signal scope drawn on every screen and the live
//! connection sounds.
//!
//! Both come from the modem itself over the voice host protocol, with no
//! SDR: Scope frames carry the channel samples the modem takes every 50 ms
//! (the RSSI, flagged when taken while transmitting) and AirEvents describe
//! each frame it sends or hears (`docs/FEATURES.md`, "Voice host
//! protocol").
//!
//! [`ScopeView`] keeps the last few seconds and draws them: the trace on a
//! -130 to -30 dBm axis, transmit samples in the accent color, and every
//! frame as a shaded span with its tier (SF5 to SF12).
//!
//! [`Chirps`] makes a LoRa-like up-chirp for each frame while a call is set
//! up: each sweep lasts the frame's symbol time scaled (x1.5 per SF in
//! place of x2, so SF12 is a slow 256 ms sweep and SF5 a 15 ms flutter)
//! over 600 to 2400 Hz, for the frame's real airtime (40 ms to 1.2 s). The
//! preamble part is plain up-chirps; the rest starts each sweep at a
//! pseudo-random point, as data symbols do. Frames heard carry a second
//! harmonic (a brighter timbre) and are a little softer than frames sent.
//! The modem thread decides when they play and mixes them into the speaker
//! at a modest level; nothing here allocates.

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui::{Align2, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui};
use lfd3k_voice::AirFrame;

use crate::ui::{
    mono, monob, text, ACCENT, FAINT, MUTED, RULE, RULE_WARM, SURFACE, TEXT2, TEXT3,
};

/// Seconds of signal the scope shows.
const SHOW_MS: u32 = 10_000;
/// Samples and frames older than this are dropped.
const KEEP_MS: u32 = 12_000;
/// Samples further apart than this are not joined by the trace.
const GAP_MS: u32 = 160;
/// No Scope frame for this long: NO MODEM.
const STALE_MS: u128 = 1_500;
/// The dBm axis.
const DBM_TOP: f32 = -30.0;
const DBM_BOTTOM: f32 = -130.0;

/// The newest samples and frames, in the modem's clock.
#[derive(Default)]
pub struct ScopeView {
    /// (time, dBm, taken while transmitting), oldest first.
    samples: VecDeque<(u32, i8, bool)>,
    frames: VecDeque<AirFrame>,
    /// The modem time of the newest sample and when it came.
    last: Option<(u32, Instant)>,
}

impl ScopeView {
    /// A Scope frame: `samples` oldest first, the last at `t_ms`.
    pub fn on_scope(&mut self, t_ms: u32, step_ms: u8, samples: &[(i8, bool)]) {
        let step = u32::from(step_ms.max(1));
        let n = samples.len() as u32;
        for (k, &(d, tx)) in samples.iter().enumerate() {
            let back = (n - 1 - k as u32) * step;
            self.samples.push_back((t_ms.wrapping_sub(back), d, tx));
        }
        self.last = Some((t_ms, Instant::now()));
        while self
            .samples
            .front()
            .is_some_and(|s| t_ms.wrapping_sub(s.0) > KEEP_MS)
        {
            self.samples.pop_front();
        }
        while self
            .frames
            .front()
            .is_some_and(|f| t_ms.wrapping_sub(f.start_ms) > KEEP_MS)
        {
            self.frames.pop_front();
        }
    }

    /// An AirEvent.
    pub fn on_air(&mut self, a: AirFrame) {
        self.frames.push_back(a);
        if self.frames.len() > 256 {
            self.frames.pop_front();
        }
    }

    /// The newest sample's dBm (None: no modem).
    pub fn level(&self) -> Option<i8> {
        if !self.live() {
            return None;
        }
        self.samples.back().map(|s| s.1)
    }

    fn live(&self) -> bool {
        self.last
            .is_some_and(|(_, at)| at.elapsed().as_millis() < STALE_MS)
    }

    /// Draw into `r`: the plot with its dBm labels on the left.
    pub fn draw(&self, ui: &Ui, r: Rect) {
        let p = ui.painter();
        let gutter = 44.0;
        let plot = Rect::from_min_max(Pos2::new(r.left() + gutter, r.top()), r.max);
        p.rect_filled(plot, CornerRadius::ZERO, SURFACE);
        let y_of = |dbm: f32| {
            let f = ((DBM_TOP - dbm) / (DBM_TOP - DBM_BOTTOM)).clamp(0.0, 1.0);
            plot.top() + f * plot.height()
        };
        for dbm in [-40.0f32, -60.0, -80.0, -100.0, -120.0] {
            let y = y_of(dbm).round() + 0.5;
            p.hline(plot.x_range(), y, Stroke::new(1.0, RULE));
            text(
                ui,
                Pos2::new(plot.left() - 8.0, y),
                Align2::RIGHT_CENTER,
                &format!("{dbm:.0}"),
                mono(10.0),
                FAINT,
            );
        }
        // Where "now" is: the newest sample, moved on by the time since it
        // came (at most 0.4 s, so a stalled modem stops the trace).
        let Some((t_last, at)) = self.last else {
            self.no_modem(ui, plot);
            return;
        };
        let ahead = at.elapsed().as_millis().min(400) as u32;
        let now = t_last.wrapping_add(ahead);
        let w = plot.width();
        let x_of = |t: u32| -> Option<f32> {
            let back = now.wrapping_sub(t) as i32;
            (-400..=SHOW_MS as i32 + 400)
                .contains(&back)
                .then(|| plot.right() - back as f32 / SHOW_MS as f32 * w)
        };
        // One faint line a second.
        let sec = now % 1000;
        for k in 0..=(SHOW_MS / 1000) {
            if let Some(x) = x_of(now.wrapping_sub(sec + k * 1000)) {
                if x > plot.left() {
                    p.vline(x.round() + 0.5, plot.y_range(), Stroke::new(1.0, RULE));
                }
            }
        }
        let clip = p.with_clip_rect(plot);
        // Frames as shaded spans with their tier (a label only where the
        // last one leaves room).
        let mut label_end = f32::MIN;
        for f in &self.frames {
            let (Some(x0), Some(x1)) = (
                x_of(f.start_ms),
                x_of(f.start_ms.wrapping_add(u32::from(f.air_ms.max(1)))),
            ) else {
                continue;
            };
            let span = Rect::from_min_max(
                Pos2::new(x0, plot.top()),
                Pos2::new(x1.max(x0 + 2.0), plot.bottom()),
            );
            let (fill, edge, label) = if f.rx {
                (
                    Color32::from_rgba_unmultiplied(0xb6, 0xbb, 0xc3, 22),
                    RULE_WARM,
                    TEXT3,
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(0x7a, 0xa2, 0xf7, 30),
                    ACCENT,
                    ACCENT,
                )
            };
            clip.rect_filled(span, CornerRadius::ZERO, fill);
            clip.vline(span.left().round() + 0.5, plot.y_range(), Stroke::new(1.0, edge));
            if span.left() >= label_end {
                let lr = clip.text(
                    Pos2::new(span.left() + 3.0, plot.top() + 2.0),
                    Align2::LEFT_TOP,
                    format!("SF{}", f.sf),
                    mono(10.0),
                    label,
                );
                label_end = label_end.max(lr.right() + 6.0);
            }
        }
        // The trace: a line between neighbouring samples; transmit samples
        // in the accent color.
        // The line breaks where samples are missing (a gap of more than
        // three sample steps, or time going back).
        let mut prev: Option<(Pos2, bool)> = None;
        let mut prev_t: Option<u32> = None;
        for &(t, d, tx) in &self.samples {
            let gap = prev_t.is_some_and(|p| {
                let dt = t.wrapping_sub(p);
                dt == 0 || dt > GAP_MS
            });
            prev_t = Some(t);
            if gap {
                prev = None;
            }
            let Some(x) = x_of(t) else {
                prev = None;
                continue;
            };
            let pt = Pos2::new(x, y_of(f32::from(d)));
            if let Some((q, qtx)) = prev {
                let c = if tx || qtx { ACCENT } else { TEXT2 };
                clip.line_segment([q, pt], Stroke::new(1.5, c));
            }
            if tx {
                clip.circle_filled(pt, 2.0, ACCENT);
            }
            prev = Some((pt, tx));
        }
        p.rect_stroke(plot, CornerRadius::ZERO, Stroke::new(1.0, RULE), StrokeKind::Inside);
        if !self.live() {
            self.no_modem(ui, plot);
        }
    }

    fn no_modem(&self, ui: &Ui, plot: Rect) {
        let p = ui.painter();
        p.rect_filled(
            plot,
            CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(0x0e, 0x0f, 0x11, 170),
        );
        p.rect_stroke(plot, CornerRadius::ZERO, Stroke::new(1.0, RULE), StrokeKind::Inside);
        text(
            ui,
            plot.center(),
            Align2::CENTER_CENTER,
            "NO MODEM",
            monob(16.0),
            MUTED,
        );
    }
}

/// The audio rate (the voice engine's).
const RATE: f32 = 16_000.0;
/// The sweep band.
const F0: f32 = 600.0;
const F1: f32 = 2_400.0;
/// Longest chirp played for one frame, and the shortest (an SF5 frame of
/// a few ms would not be heard).
const CAP_MS: u32 = 1_200;
const MIN_MS: u32 = 40;
/// Level of a frame sent (heard frames are a little softer).
const LEVEL: f32 = 0.10;
/// Fade in and out (samples).
const ATTACK: u32 = 64;
const RELEASE: u32 = 96;
/// Chirps at once.
const VOICES: usize = 4;

#[derive(Clone, Copy)]
struct Voice {
    pos: u32,
    len: u32,
    /// Samples per sweep, and the preamble's length in samples.
    sweep: u32,
    pre: u32,
    rx: bool,
    phase: f32,
    seed: u32,
    /// Share of the usual level (below 1 under a live call).
    gain: f32,
}

/// The connection sound synth (see the module docs).
pub struct Chirps {
    v: [Option<Voice>; VOICES],
}

impl Default for Chirps {
    fn default() -> Self {
        Chirps { v: [None; VOICES] }
    }
}

/// Sweep length for a spreading factor: 15 ms at SF5, x1.5 per SF.
fn sweep_ms(sf: u8) -> f32 {
    15.0 * 1.5f32.powi(i32::from(sf.clamp(5, 12)) - 5)
}

impl Chirps {
    /// Start the chirp of one frame at gain times the usual level
    /// (dropped when four already play).
    pub fn start(&mut self, a: &AirFrame, gain: f32) {
        let ms = u32::from(a.air_ms).clamp(MIN_MS, CAP_MS);
        let len = ms * 16;
        let sweep = ((sweep_ms(a.sf) * RATE / 1000.0) as u32).max(16);
        // The real preamble's share of the frame: 12.25 symbols (16.25 at
        // SF5 and SF6) of 2^SF / 500 kHz.
        let sf = u32::from(a.sf.clamp(5, 12));
        let syms_x4: u32 = if sf <= 6 { 73 } else { 49 };
        let pre_us = syms_x4 * (1 << sf) * 2 / 4;
        let air_us = u32::from(a.air_ms.max(1)) * 1000;
        let pre = (u64::from(len) * u64::from(pre_us.min(air_us)) / u64::from(air_us)) as u32;
        let seed = a
            .start_ms
            .wrapping_mul(2_654_435_761)
            .wrapping_add((u32::from(a.len) << 8) | u32::from(a.ftype))
            | 1;
        if let Some(slot) = self.v.iter_mut().find(|s| s.is_none()) {
            *slot = Some(Voice {
                pos: 0,
                len,
                sweep,
                pre,
                rx: a.rx,
                phase: 0.0,
                seed,
                gain: gain.clamp(0.0, 1.0),
            });
        }
    }

    /// Fade out whatever plays (a call connected).
    pub fn stop(&mut self) {
        for v in self.v.iter_mut().flatten() {
            v.len = v.len.min(v.pos + RELEASE);
        }
    }

    pub fn playing(&self) -> bool {
        self.v.iter().any(Option::is_some)
    }

    /// Add the chirps into `out` (16 kHz mono).
    pub fn mix(&mut self, out: &mut [i16]) {
        for slot in self.v.iter_mut() {
            let Some(v) = slot else {
                continue;
            };
            for s in out.iter_mut() {
                if v.pos >= v.len {
                    break;
                }
                let k = v.pos / v.sweep;
                let u = (v.pos % v.sweep) as f32 / v.sweep as f32;
                // Data symbols start their sweep at a pseudo-random point.
                let off = if v.pos < v.pre {
                    0.0
                } else {
                    let mut h = v.seed ^ k.wrapping_mul(0x9E37_79B9);
                    h ^= h >> 15;
                    h = h.wrapping_mul(0x2C1B_3C6D);
                    h ^= h >> 12;
                    (h % 1024) as f32 / 1024.0
                };
                let f = F0 + (F1 - F0) * (u + off).fract();
                v.phase += std::f32::consts::TAU * f / RATE;
                if v.phase > std::f32::consts::TAU {
                    v.phase -= std::f32::consts::TAU;
                }
                let x = if v.rx {
                    0.75 * v.phase.sin() + 0.3 * (2.0 * v.phase).sin()
                } else {
                    v.phase.sin()
                };
                let env = (v.pos as f32 / ATTACK as f32)
                    .min((v.len - v.pos) as f32 / RELEASE as f32)
                    .min(1.0);
                let level = if v.rx { LEVEL * 0.8 } else { LEVEL } * v.gain;
                let add = (x * env * level * 32767.0) as i32;
                // Never past full scale: what is already there (call audio,
                // the ring tone) is never clipped by a chirp.
                let cur = i32::from(*s);
                let room_up = 32767 - cur.max(0);
                let room_down = -32768 - cur.min(0);
                *s = (cur + add.clamp(room_down, room_up)) as i16;
                v.pos += 1;
            }
            if v.pos >= v.len {
                *slot = None;
            }
        }
    }
}
