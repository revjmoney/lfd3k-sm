//! Task 9 tests of the voice crate's audio side: the per-tier talk plan
//! (`lfd3k_core::voice::plan`, as the apps use it), every codec on real
//! speech, concealment and the jitter buffer.
//!
//! The talk plan values are the controller's rulings, which replaced the
//! plan text's table: SF5 Opus 10 kbps in 120 ms frames (the plan said Opus
//! 12 kbps, 100 ms), SF6 Opus 8 kbps duplex (the plan's first rule fell back
//! to Codec2 3200), SF8 Codec2 1300 duplex (the plan said push to talk) and
//! push to talk from SF9 down, all with the 20 ms slot guard
//! (`SLOT_GUARD_MS`) in `duplex_fits`.

use std::path::PathBuf;

use lfd3k_core::voice::{
    duplex_fits, encoded_bytes, talk_plan, CodecId, TalkMode, SEAL_OVERHEAD, SLOT_GUARD_MS,
};
use lfd3k_core::Tier;
use lfd3k_voice::{new_codec, Jitter};

/// The speech clip lfd3k-voicesim uses (LibriVox, public domain, 16 kHz mono).
fn speech() -> Vec<i16> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sim/testdata/speech-16k.wav");
    let b = std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    assert_eq!(&b[..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    let mut at = 12;
    let mut data: Option<&[u8]> = None;
    let mut fmt_ok = false;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &b[at + 8..(at + 8 + len).min(b.len())];
        if id == b"fmt " {
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            assert_eq!((ch, rate, bits), (1, 16_000, 16), "speech clip format");
            fmt_ok = true;
        }
        if id == b"data" {
            data = Some(body);
        }
        at += 8 + len + (len & 1);
    }
    assert!(fmt_ok);
    data.expect("data chunk")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| i16::from_le_bytes(*s))
        .collect()
}

#[test]
fn talk_plan_matches_spec_table() {
    // (tier, codec, mode, frame_ms, max_sealed): spec 8.2 with the rulings.
    let want = [
        (Tier::Sf5, CodecId::Opus10k, TalkMode::Duplex, 120, 170),
        (Tier::Sf6, CodecId::Opus8k, TalkMode::Duplex, 200, 220),
        (Tier::Sf7, CodecId::C2_2400, TalkMode::Duplex, 200, 80),
        (Tier::Sf8, CodecId::C2_1300, TalkMode::Duplex, 200, 55),
        (Tier::Sf9, CodecId::C2_1200, TalkMode::Ptt, 400, 80),
        (Tier::Sf10, CodecId::C2_1200, TalkMode::Ptt, 400, 80),
        (Tier::Sf11, CodecId::C2_700C, TalkMode::Ptt, 1000, 120),
        (
            Tier::Sf12,
            CodecId::C2_700C,
            TalkMode::PttDelayed,
            2000,
            220,
        ),
    ];
    assert_eq!(SLOT_GUARD_MS, 20, "ruling: SLOT_GUARD_MS 20");
    for (t, codec, mode, ms, sealed) in want {
        let p = talk_plan(t);
        assert_eq!(
            (p.codec, p.mode, p.frame_ms, p.max_sealed_bytes),
            (codec, mode, ms, sealed),
            "SF{}",
            t.sf()
        );
        assert_eq!(
            usize::from(p.max_sealed_bytes),
            encoded_bytes(p.codec, p.frame_ms) + SEAL_OVERHEAD,
            "SF{} sealed size",
            t.sf()
        );
        // A duplex plan passes the free time rule; every tier is checked.
        let fits = duplex_fits(t, usize::from(p.max_sealed_bytes), p.frame_ms);
        if p.mode == TalkMode::Duplex {
            assert!(fits, "SF{} duplex must fit", t.sf());
        }
    }
    // The two tiers the spec leaves to "if it fits", printed for the report.
    let sf6 = duplex_fits(
        Tier::Sf6,
        encoded_bytes(CodecId::Opus8k, 200) + SEAL_OVERHEAD,
        200,
    );
    let sf8 = duplex_fits(
        Tier::Sf8,
        encoded_bytes(CodecId::C2_1300, 200) + SEAL_OVERHEAD,
        200,
    );
    println!("SF6 Opus 8k 200 ms duplex_fits = {sf6}; SF8 Codec2 1300 200 ms duplex_fits = {sf8}");
    assert!(sf6 && sf8);
    assert_eq!(
        talk_plan(Tier::Sf6).mode,
        if sf6 { TalkMode::Duplex } else { TalkMode::Ptt }
    );
    assert_eq!(
        talk_plan(Tier::Sf8).mode,
        if sf8 { TalkMode::Duplex } else { TalkMode::Ptt }
    );
    // The choices the plan skipped really do not fit: SF5 Opus 12k in 100 ms
    // frames, and SF9 is push to talk by the ruling even though 400 ms
    // Codec2 1200 frames would pass the free time rule.
    assert!(!duplex_fits(
        Tier::Sf5,
        encoded_bytes(CodecId::Opus12k, 100) + SEAL_OVERHEAD,
        100
    ));
    assert!(duplex_fits(
        Tier::Sf9,
        encoded_bytes(CodecId::C2_1200, 400) + SEAL_OVERHEAD,
        400
    ));
}

#[test]
fn every_plan_fits_one_frame() {
    for t in Tier::LADDER {
        let p = talk_plan(t);
        assert!(
            p.max_sealed_bytes as usize <= 249,
            "SF{}: {}",
            t.sf(),
            p.max_sealed_bytes
        );
        assert!(
            p.max_sealed_bytes as usize <= lfd3k_core::voice::VOICE_MAX,
            "SF{}",
            t.sf()
        );
        // Every codec gives exactly the plan's encoded size per frame.
        let mut c = new_codec(p.codec, p.frame_ms);
        assert_eq!(c.id(), p.codec);
        assert_eq!(c.frame_samples(), 16 * usize::from(p.frame_ms));
        let mut out = Vec::new();
        c.encode(&vec![1000; c.frame_samples()], &mut out);
        assert_eq!(
            out.len() + SEAL_OVERHEAD,
            usize::from(p.max_sealed_bytes),
            "SF{}",
            t.sf()
        );
    }
}

/// RMS of each 10 ms block.
fn envelope(x: &[f64]) -> Vec<f64> {
    x.chunks(160)
        .map(|c| (c.iter().map(|v| v * v).sum::<f64>() / c.len() as f64).sqrt())
        .collect()
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().sum::<f64>() / n as f64;
    let mb = b.iter().sum::<f64>() / n as f64;
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma) * (x - ma);
        bb += (y - mb) * (y - mb);
    }
    ab / (aa * bb).sqrt()
}

/// (SNR in dB at the best lag and gain, envelope correlation) of `out`
/// against `input`. The codec delay is found first, sample exact, by cross
/// correlation over 0 to 60 ms.
fn quality(input: &[i16], out: &[i16]) -> (f64, f64) {
    let x: Vec<f64> = input.iter().map(|&s| f64::from(s)).collect();
    let y: Vec<f64> = out.iter().map(|&s| f64::from(s)).collect();
    let n = x.len().min(y.len()) - 1000;
    let mut best = (0usize, f64::MIN);
    for lag in 0..960 {
        let c: f64 = x[..n]
            .iter()
            .zip(&y[lag..lag + n])
            .map(|(a, b)| a * b)
            .sum();
        if c > best.1 {
            best = (lag, c);
        }
    }
    let y = &y[best.0..best.0 + n];
    let x = &x[..n];
    let xy: f64 = x.iter().zip(y).map(|(a, b)| a * b).sum();
    let yy: f64 = y.iter().map(|b| b * b).sum();
    let g = if yy > 0.0 { xy / yy } else { 0.0 };
    let sig: f64 = x.iter().map(|a| a * a).sum();
    let err: f64 = x
        .iter()
        .zip(y)
        .map(|(a, b)| (a - g * b) * (a - g * b))
        .sum();
    let snr = 10.0 * (sig / err.max(1e-9)).log10();
    (snr, pearson(&envelope(x), &envelope(y)))
}

#[test]
fn codec_roundtrip_each_id() {
    // Floors measured on the LibriVox clip (first 4 s, Windows clang-cl
    // build): Opus 12k 8.70 dB / 0.995, 10k 7.94 / 0.994, 8k 7.19 / 0.990;
    // Codec2 3200 0.64 / 0.976, 2400 0.59 / 0.968, 1300 0.54 / 0.949,
    // 1200 0.43 / 0.937, 700C 0.30 / 0.904. Pinned a little under, so a
    // compiler's rounding cannot fail them but a broken codec does. Codec2 is a
    // vocoder: it keeps the voice, not the waveform, so its waveform SNR is
    // near 0 dB and only the loudness envelope says much; Opus keeps both.
    // (codec, frame_ms, SNR floor dB, envelope correlation floor)
    let floors = [
        (CodecId::Opus12k, 100, 7.0, 0.98),
        (CodecId::Opus10k, 120, 6.5, 0.98),
        (CodecId::Opus8k, 200, 5.5, 0.97),
        (CodecId::C2_3200, 200, 0.1, 0.93),
        (CodecId::C2_2400, 200, 0.1, 0.92),
        (CodecId::C2_1300, 200, 0.1, 0.90),
        (CodecId::C2_1200, 400, 0.1, 0.88),
        (CodecId::C2_700C, 1000, 0.05, 0.85),
    ];
    let wav = speech();
    let clip = &wav[..64_000];
    for (id, ms, snr_floor, env_floor) in floors {
        let mut enc = new_codec(id, ms);
        let mut dec = new_codec(id, ms);
        let fs = enc.frame_samples();
        let mut out = Vec::new();
        for f in clip.chunks(fs) {
            let mut bytes = Vec::new();
            enc.encode(f, &mut bytes);
            assert_eq!(bytes.len(), encoded_bytes(id, ms), "{id:?} encoded size");
            dec.decode(&bytes, &mut out);
        }
        // Flush the codec delay with silence.
        for _ in 0..2 {
            let mut bytes = Vec::new();
            enc.encode(&vec![0; fs], &mut bytes);
            dec.decode(&bytes, &mut out);
        }
        assert!(
            out.len() >= clip.len() + 1000,
            "{id:?} gave {} samples",
            out.len()
        );
        let (snr, env) = quality(clip, &out);
        println!("{id:?} {ms} ms: SNR {snr:.2} dB, envelope correlation {env:.3}");
        assert!(snr >= snr_floor, "{id:?} SNR {snr:.2} dB under {snr_floor}");
        assert!(
            env >= env_floor,
            "{id:?} envelope {env:.3} under {env_floor}"
        );
    }
}

#[test]
fn conceal_never_panics() {
    let ids = [
        (CodecId::Opus12k, 100),
        (CodecId::Opus10k, 120),
        (CodecId::Opus8k, 200),
        (CodecId::C2_3200, 200),
        (CodecId::C2_2400, 200),
        (CodecId::C2_1300, 200),
        (CodecId::C2_1200, 400),
        (CodecId::C2_700C, 1000),
        (CodecId::C2_700C, 2000),
    ];
    let mut seed = 0x1234_5678u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for (id, ms) in ids {
        let mut enc = new_codec(id, ms);
        let mut dec = new_codec(id, ms);
        let fs = dec.frame_samples();
        let good_len = encoded_bytes(id, ms);
        let mut out = Vec::new();
        // Concealing before any frame, many times in a row.
        for _ in 0..10 {
            dec.conceal(&mut out);
        }
        assert_eq!(out.len(), 10 * fs);
        for i in 0..40u32 {
            let before = out.len();
            match rnd() % 5 {
                0 => dec.conceal(&mut out),
                1 => {
                    // Garbage of the right length.
                    let b: Vec<u8> = (0..good_len).map(|_| rnd() as u8).collect();
                    dec.decode(&b, &mut out);
                }
                2 => {
                    // The wrong length (a lost frame).
                    let n = (rnd() as usize) % 300;
                    let b: Vec<u8> = (0..n).map(|_| rnd() as u8).collect();
                    dec.decode(&b, &mut out);
                }
                _ => {
                    let pcm: Vec<i16> = (0..fs)
                        .map(|k| ((k as u32 * 7 + i) % 2000) as i16 - 1000)
                        .collect();
                    let mut b = Vec::new();
                    enc.encode(&pcm, &mut b);
                    dec.decode(&b, &mut out);
                }
            }
            assert_eq!(
                out.len() - before,
                fs,
                "{id:?} {ms} ms: one frame of samples per call"
            );
        }
        // Short and empty input to the encoder is padded.
        let mut b = Vec::new();
        enc.encode(&[], &mut b);
        assert_eq!(b.len(), good_len);
        b.clear();
        enc.encode(&vec![5; fs * 2], &mut b);
        assert_eq!(b.len(), good_len, "{id:?}: long input is cut to one frame");
    }
}

#[test]
fn jitter_reorders_and_conceals() {
    let f = |k: i16| vec![k; 4];
    // Depth 2: nothing plays until two frames are in.
    let mut j = Jitter::new(2);
    assert!(j.pop().is_none());
    j.push(11, f(11));
    assert!(j.pop().is_none(), "not primed with one frame");
    j.push(10, f(10)); // arrives late, out of order
    assert_eq!(j.pop(), Some(f(10)));
    assert_eq!(j.pop(), Some(f(11)));
    // 12 lost, 14 and 13 arrive out of order: 12 is concealed (None while
    // frames wait), then 13 and 14 play in counter order.
    j.push(14, f(14));
    j.push(13, f(13));
    assert_eq!(j.pop(), None, "12 is missing: conceal");
    assert!(j.started() && j.buffered() > 0, "a gap, not an underrun");
    assert_eq!(j.pop(), Some(f(13)));
    // 12 turning up after its turn, and a repeat of 13, are dropped.
    j.push(12, f(12));
    j.push(13, f(13));
    assert_eq!(j.buffered(), 1);
    assert_eq!(j.pop(), Some(f(14)));
    // Dry: an underrun stops playout until it is primed again.
    assert_eq!(j.pop(), None);
    assert!(!j.started());
    assert_eq!(j.buffered(), 0);
    j.push(15, f(15));
    assert_eq!(j.pop(), None, "one frame does not prime a depth of 2");
    j.push(16, f(16));
    assert_eq!(j.pop(), Some(f(15)));
    assert_eq!(j.pop(), Some(f(16)));
    // A sender far ahead restarts the buffer at its frame.
    let mut j = Jitter::new(1);
    j.push(0, f(0));
    assert_eq!(j.pop(), Some(f(0)));
    j.push(1_000, f(100));
    assert_eq!(j.pop(), Some(f(100)), "a jump far ahead starts over there");
}
