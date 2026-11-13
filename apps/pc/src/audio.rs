//! Sound in and out with cpal at the device's own rate and channels,
//! converted to and from 16 kHz mono (the voice engine's rate) with a
//! linear resampler. Two queues of 16 kHz samples sit between the sound
//! callbacks and the modem thread: the mic queue (filled by the input
//! callback, emptied by the modem thread) and the speaker queue (filled by
//! the modem thread, emptied by the output callback, silence when empty).
//!
//! cpal is only built on Windows (no ALSA headers needed elsewhere); on
//! other systems `open` gives no devices and the app still runs (WAV play
//! and record work, nothing is heard).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// 16 kHz mono samples shared with a sound callback.
pub type Queue = Arc<Mutex<VecDeque<i16>>>;

/// Most samples a queue holds (half a second); older ones are dropped.
pub const QUEUE_MAX: usize = 8_000;

pub fn new_queue() -> Queue {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Push samples, dropping the oldest past [`QUEUE_MAX`].
pub fn push(q: &Queue, s: impl IntoIterator<Item = i16>) {
    if let Ok(mut q) = q.lock() {
        q.extend(s);
        let over = q.len().saturating_sub(QUEUE_MAX);
        q.drain(..over);
    }
}

/// The open sound devices. Keep it alive while sound should run (the
/// streams stop when it is dropped).
pub struct Audio {
    /// Mic samples (None when no input device is used).
    pub mic: Option<Queue>,
    /// Speaker samples (None when no output device is used).
    pub spk: Option<Queue>,
    /// What was opened (or why not), for the About screen.
    pub note: String,
    #[cfg(windows)]
    _streams: Vec<cpal::Stream>,
}

/// Linear resampler from any rate to any rate, one sample at a time out.
struct Down {
    /// Input samples per output sample.
    ratio: f64,
    /// Next output position, relative to the sample before the current
    /// input chunk (-1.0 is `last`).
    t: f64,
    last: f32,
}

impl Down {
    fn new(in_rate: u32, out_rate: u32) -> Down {
        Down {
            ratio: f64::from(in_rate) / f64::from(out_rate),
            t: 0.0,
            last: 0.0,
        }
    }

    fn run(&mut self, x: &[f32], out: &mut Vec<i16>) {
        let n = x.len() as f64;
        if x.is_empty() {
            return;
        }
        let at = |i: isize| -> f32 {
            if i < 0 {
                self.last
            } else {
                x.get(i as usize).copied().unwrap_or(0.0)
            }
        };
        while self.t <= n - 1.0 {
            let i = self.t.floor();
            let f = (self.t - i) as f32;
            let a = at(i as isize);
            let b = at(i as isize + 1);
            let v = a + (b - a) * f;
            out.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            self.t += self.ratio;
        }
        self.t -= n;
        self.last = x.last().copied().unwrap_or(0.0);
    }
}

/// 16 kHz queue to the device rate: pulls samples as it needs them.
#[cfg_attr(not(windows), allow(dead_code))]
struct Up {
    /// Queue samples per device sample.
    step: f64,
    phase: f64,
    a: f32,
    b: f32,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Up {
    fn new(dev_rate: u32) -> Up {
        Up {
            step: 16_000.0 / f64::from(dev_rate.max(1)),
            phase: 0.0,
            a: 0.0,
            b: 0.0,
        }
    }

    fn next(&mut self, q: &mut VecDeque<i16>) -> f32 {
        while self.phase >= 1.0 {
            self.a = self.b;
            self.b = q.pop_front().map_or(0.0, |s| f32::from(s) / 32768.0);
            self.phase -= 1.0;
        }
        let v = self.a + (self.b - self.a) * self.phase as f32;
        self.phase += self.step;
        v
    }
}

#[cfg(windows)]
pub fn open(want_mic: bool, want_spk: bool) -> Audio {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;

    let host = cpal::default_host();
    let mut streams = Vec::new();
    let mut notes = Vec::new();
    let mut mic = None;
    let mut spk = None;

    if want_mic {
        let r = (|| -> Result<(cpal::Stream, String), String> {
            let dev = host.default_input_device().ok_or("no input device")?;
            let sup = dev.default_input_config().map_err(|e| e.to_string())?;
            let cfg = sup.config();
            let ch = usize::from(cfg.channels.max(1));
            let q = new_queue();
            let q2 = q.clone();
            let mut down = Down::new(cfg.sample_rate, 16_000);
            let mut mono: Vec<f32> = Vec::new();
            let mut out: Vec<i16> = Vec::new();
            let err = |e: cpal::Error| eprintln!("[voice-pc] input: {e}");
            let stream = match sup.sample_format() {
                SampleFormat::F32 => dev
                    .build_input_stream::<f32, _, _>(
                        cfg,
                        move |d: &[f32], _| {
                            mono.clear();
                            mono.extend(d.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
                            out.clear();
                            down.run(&mono, &mut out);
                            push(&q2, out.iter().copied());
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                SampleFormat::I16 => dev
                    .build_input_stream::<i16, _, _>(
                        cfg,
                        move |d: &[i16], _| {
                            mono.clear();
                            mono.extend(d.chunks(ch).map(|f| {
                                f.iter().map(|&s| f32::from(s) / 32768.0).sum::<f32>() / ch as f32
                            }));
                            out.clear();
                            down.run(&mono, &mut out);
                            push(&q2, out.iter().copied());
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                f => return Err(format!("input sample format {f:?} not handled")),
            };
            stream.play().map_err(|e| e.to_string())?;
            mic = Some(q);
            Ok((stream, format!("mic {} Hz x{}", cfg.sample_rate, ch)))
        })();
        match r {
            Ok((s, n)) => {
                streams.push(s);
                notes.push(n);
            }
            Err(e) => notes.push(format!("mic: {e}")),
        }
    }

    if want_spk {
        let r = (|| -> Result<(cpal::Stream, String), String> {
            let dev = host.default_output_device().ok_or("no output device")?;
            let sup = dev.default_output_config().map_err(|e| e.to_string())?;
            let cfg = sup.config();
            let ch = usize::from(cfg.channels.max(1));
            let q = new_queue();
            let q2 = q.clone();
            let mut up = Up::new(cfg.sample_rate);
            let err = |e: cpal::Error| eprintln!("[voice-pc] output: {e}");
            let stream = match sup.sample_format() {
                SampleFormat::F32 => dev
                    .build_output_stream::<f32, _, _>(
                        cfg,
                        move |d: &mut [f32], _| {
                            let Ok(mut q) = q2.lock() else {
                                d.fill(0.0);
                                return;
                            };
                            for f in d.chunks_mut(ch) {
                                let v = up.next(&mut q);
                                f.fill(v);
                            }
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                SampleFormat::I16 => dev
                    .build_output_stream::<i16, _, _>(
                        cfg,
                        move |d: &mut [i16], _| {
                            let Ok(mut q) = q2.lock() else {
                                d.fill(0);
                                return;
                            };
                            for f in d.chunks_mut(ch) {
                                let v = up.next(&mut q);
                                f.fill((v.clamp(-1.0, 1.0) * 32767.0) as i16);
                            }
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                f => return Err(format!("output sample format {f:?} not handled")),
            };
            stream.play().map_err(|e| e.to_string())?;
            spk = Some(q);
            Ok((stream, format!("speaker {} Hz x{}", cfg.sample_rate, ch)))
        })();
        match r {
            Ok((s, n)) => {
                streams.push(s);
                notes.push(n);
            }
            Err(e) => notes.push(format!("speaker: {e}")),
        }
    }

    Audio {
        mic,
        spk,
        note: notes.join(", "),
        _streams: streams,
    }
}

#[cfg(not(windows))]
pub fn open(_want_mic: bool, _want_spk: bool) -> Audio {
    Audio {
        mic: None,
        spk: None,
        note: "no sound devices on this system (Windows only)".into(),
    }
}

/// 16 bit PCM WAV as 16 kHz mono (mixed down and resampled as needed).
pub fn read_wav(path: &std::path::Path) -> Result<Vec<i16>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bad = || format!("{}: not a 16 bit PCM WAV", path.display());
    let le16 = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
    };
    let le32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    if b.get(..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(bad());
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u32)> = None;
    let mut data: Option<(usize, usize)> = None;
    while let (Some(id), Some(len)) = (b.get(at..at + 4), le32(at + 4)) {
        let len = len as usize;
        match id {
            b"fmt " => {
                let tag = le16(at + 8).ok_or_else(bad)?;
                let ch = le16(at + 10).ok_or_else(bad)?;
                let rate = le32(at + 12).ok_or_else(bad)?;
                let bits = le16(at + 22).ok_or_else(bad)?;
                if (tag != 1 && tag != 0xFFFE) || bits != 16 || ch == 0 {
                    return Err(bad());
                }
                fmt = Some((ch, rate));
            }
            b"data" => data = Some((at + 8, (at + 8 + len).min(b.len()))),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let ((ch, rate), (s, e)) = (fmt.ok_or_else(bad)?, data.ok_or_else(bad)?);
    let ch = usize::from(ch);
    let mono: Vec<f32> = b
        .get(s..e)
        .unwrap_or(&[])
        .chunks_exact(2 * ch)
        .map(|f| {
            f.as_chunks::<2>()
                .0
                .iter()
                .map(|p| f32::from(i16::from_le_bytes(*p)) / 32768.0)
                .sum::<f32>()
                / ch as f32
        })
        .collect();
    let mut out = Vec::with_capacity(mono.len());
    let mut d = Down::new(rate, 16_000);
    d.run(&mono, &mut out);
    Ok(out)
}

/// Write 16 kHz mono 16 bit PCM.
pub fn write_wav(path: &std::path::Path, pcm: &[i16]) -> std::io::Result<()> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + pcm.len() * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}
