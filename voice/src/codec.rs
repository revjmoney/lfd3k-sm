//! Voice codecs. The engine works on 16 kHz mono i16. Opus runs at 16 kHz;
//! Codec 2 (the official C library, vendored in `voice/codec2`) runs at 8 kHz
//! behind a 2:1 windowed-sinc resampler each way.
//!
//! `encode`, `decode` and `conceal` append to their output vector.

use lfd3k_core::voice::plan::{encoded_bytes, CodecId};
use std::os::raw::{c_int, c_short, c_uchar};

/// Engine audio rate.
pub const SAMPLE_RATE: u32 = 16_000;

pub trait VoiceCodec: Send {
    fn id(&self) -> CodecId;
    /// Samples (16 kHz) in one voice frame.
    fn frame_samples(&self) -> usize;
    /// Encode one frame of `frame_samples()` samples (short input is padded
    /// with silence) and append the encoded bytes to `out`.
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>);
    /// Decode one frame and append `frame_samples()` samples. Input of the
    /// wrong length is treated as a lost frame.
    fn decode(&mut self, bytes: &[u8], pcm: &mut Vec<i16>);
    /// A frame was lost: append `frame_samples()` samples of filler.
    fn conceal(&mut self, pcm: &mut Vec<i16>);
}

/// A codec could not be made (the library refused, or out of memory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecError(pub String);

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

/// A codec for `id` with `frame_ms` voice frames. An error (never a panic)
/// when the library cannot make one; the engine then ends the call.
pub fn new_codec(id: CodecId, frame_ms: u16) -> Result<Box<dyn VoiceCodec>, CodecError> {
    Ok(match id {
        CodecId::Opus12k => Box::new(OpusCodec::new(id, 12_000, frame_ms)?),
        CodecId::Opus10k => Box::new(OpusCodec::new(id, 10_000, frame_ms)?),
        CodecId::Opus8k => Box::new(OpusCodec::new(id, 8_000, frame_ms)?),
        CodecId::C2_3200 => Box::new(Codec2Codec::new(id, C2_MODE_3200, frame_ms)?),
        CodecId::C2_2400 => Box::new(Codec2Codec::new(id, C2_MODE_2400, frame_ms)?),
        CodecId::C2_1300 => Box::new(Codec2Codec::new(id, C2_MODE_1300, frame_ms)?),
        CodecId::C2_1200 => Box::new(Codec2Codec::new(id, C2_MODE_1200, frame_ms)?),
        CodecId::C2_700C => Box::new(Codec2Codec::new(id, C2_MODE_700C, frame_ms)?),
    })
}

fn frame_len(frame_ms: u16) -> usize {
    SAMPLE_RATE as usize * usize::from(frame_ms) / 1000
}

/// Fit `pcm` to exactly `n` samples.
fn fit(pcm: &[i16], n: usize) -> Vec<i16> {
    let mut v = pcm.get(..n.min(pcm.len())).unwrap_or(&[]).to_vec();
    v.resize(n, 0);
    v
}

// ---------------------------------------------------------------- Opus

/// Opus packets are at most 100 ms here, so a longer voice frame is several
/// constant bit rate packets of equal size back to back: 100 ms packets when
/// the frame is a multiple of 100 ms, else 20 ms ones (120 ms = 6 x 20 ms).
const OPUS_PKT_MS: usize = 100;
const OPUS_SHORT_PKT_MS: usize = 20;

struct OpusCodec {
    id: CodecId,
    enc: opus::Encoder,
    dec: opus::Decoder,
    frame_ms: u16,
    pkts: usize,
    pkt_bytes: usize,
}

impl OpusCodec {
    fn new(id: CodecId, bps: i32, frame_ms: u16) -> Result<Self, CodecError> {
        let mut enc =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .map_err(|e| CodecError(format!("opus encoder: {e}")))?;
        let _ = enc.set_bitrate(opus::Bitrate::Bits(bps));
        let _ = enc.set_vbr(false);
        let dec = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)
            .map_err(|e| CodecError(format!("opus decoder: {e}")))?;
        let ms = usize::from(frame_ms);
        let pkt_ms = if ms % OPUS_PKT_MS == 0 {
            OPUS_PKT_MS
        } else {
            OPUS_SHORT_PKT_MS
        };
        let pkts = (ms / pkt_ms).max(1);
        let total = encoded_bytes(id, frame_ms);
        Ok(OpusCodec {
            id,
            enc,
            dec,
            frame_ms,
            pkts,
            pkt_bytes: total / pkts,
        })
    }

    fn pkt_samples(&self) -> usize {
        frame_len(self.frame_ms) / self.pkts
    }
}

impl VoiceCodec for OpusCodec {
    fn id(&self) -> CodecId {
        self.id
    }

    fn frame_samples(&self) -> usize {
        frame_len(self.frame_ms)
    }

    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>) {
        let pcm = fit(pcm, self.frame_samples());
        let ps = self.pkt_samples();
        for chunk in pcm.chunks(ps) {
            let mut buf = vec![0u8; 1275];
            let n = self.enc.encode(chunk, &mut buf).unwrap_or(0);
            // CBR makes every packet exactly pkt_bytes; keep the frame size
            // fixed even if the encoder ever disagrees.
            buf.truncate(n.min(self.pkt_bytes));
            buf.resize(self.pkt_bytes, 0);
            out.extend_from_slice(&buf);
        }
    }

    fn decode(&mut self, bytes: &[u8], pcm: &mut Vec<i16>) {
        if bytes.len() != self.pkt_bytes * self.pkts {
            return self.conceal(pcm);
        }
        let ps = self.pkt_samples();
        for p in bytes.chunks(self.pkt_bytes) {
            let mut buf = vec![0i16; ps];
            if self.dec.decode(p, &mut buf, false).is_err() {
                buf.fill(0);
            }
            pcm.extend_from_slice(&buf);
        }
    }

    fn conceal(&mut self, pcm: &mut Vec<i16>) {
        let ps = self.pkt_samples();
        for _ in 0..self.pkts {
            let mut buf = vec![0i16; ps];
            // An empty packet asks Opus for its own loss concealment.
            if self.dec.decode(&[], &mut buf, false).is_err() {
                buf.fill(0);
            }
            pcm.extend_from_slice(&buf);
        }
    }
}

// -------------------------------------------------------------- Codec 2

const C2_MODE_3200: c_int = 0;
const C2_MODE_2400: c_int = 1;
const C2_MODE_1300: c_int = 4;
const C2_MODE_1200: c_int = 5;
const C2_MODE_700C: c_int = 8;

#[repr(C)]
struct Codec2State {
    _private: [u8; 0],
}

extern "C" {
    fn codec2_create(mode: c_int) -> *mut Codec2State;
    fn codec2_destroy(c2: *mut Codec2State);
    fn codec2_encode(c2: *mut Codec2State, bytes: *mut c_uchar, speech: *mut c_short);
    fn codec2_decode(c2: *mut Codec2State, speech: *mut c_short, bytes: *const c_uchar);
    fn codec2_samples_per_frame(c2: *mut Codec2State) -> c_int;
    fn codec2_bytes_per_frame(c2: *mut Codec2State) -> c_int;
}

/// Safe owner of a Codec 2 state: the only `unsafe` in the crate.
struct C2 {
    p: *mut Codec2State,
    samples: usize,
    bytes: usize,
}

// The state is plain heap data used from one thread at a time.
unsafe impl Send for C2 {}

impl C2 {
    fn new(mode: c_int) -> Result<C2, CodecError> {
        // SAFETY: codec2_create returns a fresh state or null.
        let p = unsafe { codec2_create(mode) };
        if p.is_null() {
            return Err(CodecError(format!("codec2_create failed (mode {mode})")));
        }
        // SAFETY: p is a live state.
        let (s, b) = unsafe { (codec2_samples_per_frame(p), codec2_bytes_per_frame(p)) };
        Ok(C2 {
            p,
            samples: s.max(1) as usize,
            bytes: b.max(1) as usize,
        })
    }

    /// `speech` must hold `self.samples` samples (8 kHz).
    fn encode(&mut self, speech: &mut [i16], out: &mut Vec<u8>) {
        if speech.len() != self.samples {
            return;
        }
        let mut b = vec![0u8; self.bytes];
        // SAFETY: buffers are exactly the sizes the library reports.
        unsafe { codec2_encode(self.p, b.as_mut_ptr(), speech.as_mut_ptr()) };
        out.extend_from_slice(&b);
    }

    fn decode(&mut self, bytes: &[u8]) -> Vec<i16> {
        let mut s = vec![0i16; self.samples];
        if bytes.len() == self.bytes {
            // SAFETY: buffers are exactly the sizes the library reports.
            unsafe { codec2_decode(self.p, s.as_mut_ptr(), bytes.as_ptr()) };
        }
        s
    }
}

impl Drop for C2 {
    fn drop(&mut self) {
        // SAFETY: p came from codec2_create and is destroyed once.
        unsafe { codec2_destroy(self.p) };
    }
}

const TAPS: usize = 31;

/// Windowed-sinc low pass for 16 kHz, cutoff about 3.6 kHz, DC gain 1.
fn lowpass() -> [f32; TAPS] {
    use std::f32::consts::PI;
    let fc = 3600.0 / SAMPLE_RATE as f32;
    let mid = (TAPS / 2) as f32;
    let mut h = [0f32; TAPS];
    let mut sum = 0.0;
    for (i, v) in h.iter_mut().enumerate() {
        let x = i as f32 - mid;
        let sinc = if x == 0.0 {
            2.0 * fc
        } else {
            (2.0 * PI * fc * x).sin() / (PI * x)
        };
        let w = 0.54 - 0.46 * (2.0 * PI * i as f32 / (TAPS - 1) as f32).cos();
        *v = sinc * w;
        sum += *v;
    }
    for v in h.iter_mut() {
        *v /= sum;
    }
    h
}

/// Streaming FIR with history, used for both resampling directions.
struct Fir {
    h: [f32; TAPS],
    hist: [f32; TAPS - 1],
}

impl Fir {
    fn new() -> Self {
        Fir {
            h: lowpass(),
            hist: [0.0; TAPS - 1],
        }
    }

    /// Filter `x` and return the output samples whose index satisfies `keep`.
    fn run(&mut self, x: &[f32], keep: impl Fn(usize) -> bool, gain: f32) -> Vec<f32> {
        let mut buf = Vec::with_capacity(x.len() + TAPS - 1);
        buf.extend_from_slice(&self.hist);
        buf.extend_from_slice(x);
        let mut y = Vec::with_capacity(x.len());
        for i in 0..x.len() {
            if !keep(i) {
                continue;
            }
            let acc: f32 = self
                .h
                .iter()
                .enumerate()
                .map(|(k, hk)| hk * buf.get(i + TAPS - 1 - k).copied().unwrap_or(0.0))
                .sum();
            y.push(acc * gain);
        }
        if let Some(t) = buf.get(buf.len() - (TAPS - 1)..) {
            self.hist.copy_from_slice(t);
        }
        y
    }
}

fn to_i16(v: f32) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

struct Codec2Codec {
    id: CodecId,
    c2: C2,
    frame_ms: u16,
    native: usize,
    down: Fir,
    up: Fir,
    last: Vec<i16>,
    lost: u32,
}

impl Codec2Codec {
    fn new(id: CodecId, mode: c_int, frame_ms: u16) -> Result<Self, CodecError> {
        let c2 = C2::new(mode)?;
        let native = (frame_len(frame_ms) / 2 / c2.samples).max(1);
        Ok(Codec2Codec {
            id,
            c2,
            frame_ms,
            native,
            down: Fir::new(),
            up: Fir::new(),
            last: vec![0; frame_len(frame_ms)],
            lost: 0,
        })
    }
}

impl VoiceCodec for Codec2Codec {
    fn id(&self) -> CodecId {
        self.id
    }

    fn frame_samples(&self) -> usize {
        frame_len(self.frame_ms)
    }

    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>) {
        let pcm = fit(pcm, self.frame_samples());
        let x: Vec<f32> = pcm.iter().map(|&s| f32::from(s)).collect();
        let y = self.down.run(&x, |i| i % 2 == 0, 1.0);
        let mut s8: Vec<i16> = y.into_iter().map(to_i16).collect();
        for chunk in s8.chunks_mut(self.c2.samples) {
            self.c2.encode(chunk, out);
        }
    }

    fn decode(&mut self, bytes: &[u8], pcm: &mut Vec<i16>) {
        if bytes.len() != self.c2.bytes * self.native {
            return self.conceal(pcm);
        }
        let mut x = Vec::with_capacity(self.frame_samples());
        for b in bytes.chunks(self.c2.bytes) {
            for s in self.c2.decode(b) {
                // Zero stuffing; the filter gain of 2 restores the level.
                x.push(f32::from(s));
                x.push(0.0);
            }
        }
        let y = self.up.run(&x, |_| true, 2.0);
        let out: Vec<i16> = y.into_iter().map(to_i16).collect();
        self.last.clone_from(&out);
        self.lost = 0;
        pcm.extend_from_slice(&out);
    }

    fn conceal(&mut self, pcm: &mut Vec<i16>) {
        // Repeat the last frame, 6 dB quieter each time, then silence.
        self.lost = self.lost.saturating_add(1);
        let shift = self.lost.min(16);
        pcm.extend(self.last.iter().map(|&s| s.checked_shr(shift).unwrap_or(0)));
    }
}
