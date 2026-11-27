//! Connection sounds: a LoRa-like chirp for each frame while a call is set
//! up, mixed into the speaker at a modest level. The shape matches the PC
//! app (`apps/pc/src/monitor.rs`). Nothing here allocates after `start`.

use lfd3k_voice::AirFrame;

const RATE: f32 = 16_000.0;
const F0: f32 = 600.0;
const F1: f32 = 2_400.0;
const CAP_MS: u32 = 1_200;
const MIN_MS: u32 = 40;
const LEVEL: f32 = 0.10;
const ATTACK: u32 = 64;
const RELEASE: u32 = 96;
const VOICES: usize = 4;

#[derive(Clone, Copy)]
struct Voice {
    pos: u32,
    len: u32,
    sweep: u32,
    pre: u32,
    rx: bool,
    phase: f32,
    seed: u32,
    gain: f32,
}

pub struct Chirps {
    v: [Option<Voice>; VOICES],
}

impl Default for Chirps {
    fn default() -> Self {
        Chirps { v: [None; VOICES] }
    }
}

fn sweep_ms(sf: u8) -> f32 {
    15.0 * 1.5f32.powi(i32::from(sf.clamp(5, 12)) - 5)
}

impl Chirps {
    pub fn start(&mut self, a: &AirFrame, gain: f32) {
        let ms = u32::from(a.air_ms).clamp(MIN_MS, CAP_MS);
        let len = ms * 16;
        let sweep = ((sweep_ms(a.sf) * RATE / 1000.0) as u32).max(16);
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

    pub fn stop(&mut self) {
        for v in self.v.iter_mut().flatten() {
            v.len = v.len.min(v.pos + RELEASE);
        }
    }

    pub fn playing(&self) -> bool {
        self.v.iter().any(Option::is_some)
    }

    /// Add the chirps into `out` (16 kHz mono). A chirp never clips what is
    /// already there (call audio, the ring tone).
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
