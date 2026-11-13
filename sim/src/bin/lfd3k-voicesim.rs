//! Two voice apps on two simulated modems: one plays a WAV as its mic, the
//! other records what it hears, on each tier asked for.
//!
//! `cargo run --release -p lfd3k-sim --bin lfd3k-voicesim -- --tier all --in sim/testdata/speech-16k.wav --out target/voicesim`
//!
//! - `--tier <SF5..SF12|all>`: the tier (both modems capped there with S21;
//!   the SNR is high enough that training picks the cap). Default `all`.
//! - `--snr <dB>`: the channel SNR (default 10). Loss follows the simulator's
//!   table from the margin over the tier's floor.
//! - `--in <wav>`: what the caller says (16 bit PCM; mixed to mono and
//!   resampled to 16 kHz if needed). Default `sim/testdata/speech-16k.wav`.
//! - `--out <dir>`: where `<tier>.wav` (what the answerer heard) goes.
//!   Default `target/voicesim`.
//! - `--seed <n>`: the channel's loss draws (default 1).
//!
//! Each run: modem ALPHA with app ANN/PC calls BEN, whose app BEN/PC runs on
//! modem BRAVO and answers. Both apps are new to each other (NOT CHECKED)
//! and press TALK ANYWAY. Once the voice slots run (push to talk: once ANN
//! holds the floor, with the key down for the whole WAV) ANN's mic plays the
//! WAV in 10 ms pieces in step with simulated time; BEN's speaker is
//! recorded from that moment until the WAV's length plus a tail of four
//! frames and 3 s.
//!
//! One line per tier: the tier CallState showed, codec, mode, voice frames
//! ANN sent, frames BEN opened, frames lost (sent minus opened: lost on the
//! air, or refused by a full modem queue), frames BEN concealed, the
//! mouth-to-ear delay (the lag, in 10 ms steps, at which the loudness
//! envelope of the recording best matches the WAV's, with that match as a
//! correlation from -1 to 1) and CONNECT to first audio (ANN's CallState
//! connected to the first decoded frame BEN played).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lfd3k_core::voice::{Floor, TalkMode};
use lfd3k_core::{Config, Tier};
use lfd3k_sim::{Multi, SimChannel};
use lfd3k_voice::{codec_name, mode_name, Engine, Identity, Trust, UiEvent};

const SESSION: u8 = 1;
const STEP_MS: u32 = 10;
const CHUNK: usize = 160;
/// Most time from the start to the voice slots running.
const SETUP_LIMIT_MS: u32 = 120_000;

struct Args {
    tiers: Vec<Tier>,
    snr_db: i16,
    input: PathBuf,
    out: PathBuf,
    seed: u64,
}

fn usage() -> String {
    "usage: lfd3k-voicesim [--tier <SF5..SF12|all>] [--snr <dB>] [--in <wav>] [--out <dir>] [--seed <n>]"
        .to_string()
}

fn parse_tier(s: &str) -> Result<Vec<Tier>, String> {
    if s.eq_ignore_ascii_case("all") {
        let mut v = Tier::LADDER.to_vec();
        v.reverse();
        return Ok(v);
    }
    let n = s.trim_start_matches(['S', 's']).trim_start_matches(['F', 'f']);
    let sf: u8 = n.parse().map_err(|_| format!("--tier: {s} is not SF5 to SF12 or all"))?;
    lfd3k_voice::tier_from_sf(sf)
        .map(|t| vec![t])
        .ok_or_else(|| format!("--tier: {s} is not SF5 to SF12 or all"))
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tiers: parse_tier("all")?,
        snr_db: 10,
        input: PathBuf::from("sim/testdata/speech-16k.wav"),
        out: PathBuf::from("target/voicesim"),
        seed: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| format!("{k} needs a value\n{}", usage()));
        match k.as_str() {
            "--tier" => a.tiers = parse_tier(&v()?)?,
            "--snr" => a.snr_db = v()?.parse().map_err(|e| format!("--snr: {e}"))?,
            "--in" => a.input = PathBuf::from(v()?),
            "--out" => a.out = PathBuf::from(v()?),
            "--seed" => a.seed = v()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "-h" | "--help" => return Err(usage()),
            _ => return Err(format!("unknown option {k}\n{}", usage())),
        }
    }
    Ok(a)
}

// ---- WAV files.

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// 16 bit PCM WAV as 16 kHz mono.
fn read_wav(path: &Path) -> Result<Vec<i16>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bad = || format!("{}: not a 16 bit PCM WAV", path.display());
    if b.get(..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(bad());
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while let (Some(id), Some(len)) = (b.get(at..at + 4), le32(&b, at + 4)) {
        let len = len as usize;
        let body = b.get(at + 8..(at + 8 + len).min(b.len())).unwrap_or(&[]);
        match id {
            b"fmt " => {
                let tag = le16(body, 0).ok_or_else(bad)?;
                let ch = le16(body, 2).ok_or_else(bad)?;
                let rate = le32(body, 4).ok_or_else(bad)?;
                let bits = le16(body, 14).ok_or_else(bad)?;
                if (tag != 1 && tag != 0xFFFE) || bits != 16 || ch == 0 {
                    return Err(bad());
                }
                fmt = Some((ch, rate, bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let ((ch, rate, _), data) = (fmt.ok_or_else(bad)?, data.ok_or_else(bad)?);
    let ch = usize::from(ch);
    let mono: Vec<i16> = data
        .chunks_exact(2 * ch)
        .map(|f| {
            let sum: i32 = f
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| i32::from(i16::from_le_bytes(*s)))
                .sum();
            (sum / ch as i32) as i16
        })
        .collect();
    if rate == 16_000 || mono.is_empty() {
        return Ok(mono);
    }
    // Linear resampling to 16 kHz.
    let n = (mono.len() as u64 * 16_000 / u64::from(rate)) as usize;
    let step = f64::from(rate) / 16_000.0;
    Ok((0..n)
        .map(|i| {
            let x = i as f64 * step;
            let k = x as usize;
            let f = x - k as f64;
            let a = f64::from(mono.get(k).copied().unwrap_or(0));
            let b = f64::from(mono.get(k + 1).copied().unwrap_or(0));
            (a + (b - a) * f).round() as i16
        })
        .collect())
}

fn write_wav(path: &Path, pcm: &[i16]) -> std::io::Result<()> {
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

// ---- Delay.

/// RMS of each 10 ms block.
fn envelope(pcm: &[i16]) -> Vec<f64> {
    pcm.chunks(CHUNK)
        .map(|c| {
            let e: f64 = c.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (e / c.len().max(1) as f64).sqrt()
        })
        .collect()
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().sum::<f64>() / n as f64;
    let mb = b.iter().sum::<f64>() / n as f64;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa <= 0.0 || sbb <= 0.0 {
        0.0
    } else {
        sab / (saa * sbb).sqrt()
    }
}

/// The first 10 ms block louder than a tenth of the loudest.
fn onset(env: &[f64]) -> Option<usize> {
    let max = env.iter().copied().fold(0.0, f64::max);
    (max > 0.0).then(|| env.iter().position(|&e| e > max / 10.0)).flatten()
}

/// (lag in ms, match) where the recording's envelope best follows the
/// input's. The search starts from the lag between the two onsets (the
/// first loud block of each) and looks `window_ms` either side of it, so a
/// voice that repeats a sound cannot pull the match a word off.
fn delay(input: &[i16], rec: &[i16], window_ms: u32) -> (u32, f64) {
    let ei = envelope(input);
    let eo = envelope(rec);
    let mut best = (0u32, f64::MIN);
    let max_lag = eo.len().saturating_sub(ei.len() / 2);
    let w = (window_ms / STEP_MS) as usize;
    let (lo, hi) = match (onset(&ei), onset(&eo)) {
        (Some(a), Some(b)) => {
            let c = b.saturating_sub(a);
            (c.saturating_sub(w), (c + w + 1).min(max_lag))
        }
        _ => (0, max_lag),
    };
    for lag in lo..hi {
        let c = pearson(&ei, eo.get(lag..).unwrap_or(&[]));
        if c > best.1 {
            best = (lag as u32 * STEP_MS, c);
        }
    }
    best
}

// ---- One tier.

struct Row {
    sf: u8,
    codec: &'static str,
    mode: &'static str,
    sent: u32,
    heard: u32,
    concealed: u32,
    delay_ms: u32,
    matched: f64,
    first_audio_ms: Option<u32>,
    slots_ms: Option<u32>,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Start,
    Calling,
    WaitFloor(u32),
    Playing(u32),
}

/// The call's voice frame length (ms), 0 before a plan.
fn stats_plan_ms(e: &Engine) -> u32 {
    e.stats().plan.map_or(0, |p| u32::from(p.frame_ms))
}

fn config(name: &str, tier: Tier) -> Result<Config, String> {
    let mut c = Config::new(name).ok_or_else(|| format!("bad station name {name}"))?;
    let idx = Tier::LADDER.iter().position(|&t| t == tier).unwrap_or(0);
    c.sregs.set(21, idx as u16);
    Ok(c)
}

fn run_tier(tier: Tier, a: &Args, wav: &[i16]) -> Result<Row, String> {
    let chan = SimChannel::constant_db(a.snr_db, a.seed);
    let mut multi = Multi::with_configs(chan, vec![config("ALPHA", tier)?, config("BRAVO", tier)?]);
    let ann = Identity::generate("ANN", "PC").ok_or("identity ANN")?;
    let ben = Identity::generate("BEN", "PC").ok_or("identity BEN")?;
    let tag = format!("SF{}", tier.sf());
    let mut eng = [
        Engine::new(ann, a.out.join(format!("{tag}-ann-trust.json"))),
        Engine::new(ben, a.out.join(format!("{tag}-ben-trust.json"))),
    ];
    let mut phase = Phase::Start;
    let mut rec: Vec<i16> = Vec::new();
    let mut pos = 0usize;
    let mut tail = 0usize;
    let mut ended = false;
    let zeros = [0i16; CHUNK];
    loop {
        for (k, e) in eng.iter_mut().enumerate() {
            let b = e.take_modem_bytes();
            if !b.is_empty() {
                multi.voice_send(k, SESSION, &b);
            }
        }
        multi.run_ms(STEP_MS);
        let now = multi.now_ms;
        for (k, e) in eng.iter_mut().enumerate() {
            let b = multi.voice_recv(k, SESSION);
            e.on_modem_bytes(&b);
            e.tick(now);
            while let Some(ev) = e.poll_event() {
                match ev {
                    UiEvent::Ringing { .. } if k == 1 => e.answer(),
                    UiEvent::Connected { trust, .. } if trust != Trust::Conflict => e.talk_anyway(),
                    UiEvent::Error(s) => eprintln!("  {tag} {}: {s}", ["ANN", "BEN"][k]),
                    UiEvent::Ended | UiEvent::Declined | UiEvent::NoAnswer | UiEvent::AnsweredElsewhere => {
                        ended = true;
                    }
                    _ => {}
                }
            }
        }
        if ended && !matches!(phase, Phase::Start) {
            return Err(format!("{tag}: the call ended at {now} ms"));
        }
        match phase {
            Phase::Start if now >= 1_000 => {
                eng[0].call("BEN", None);
                phase = Phase::Calling;
            }
            Phase::Calling => {
                let st = eng[0].stats();
                if st.slots_ms.is_some() {
                    let plan = st.plan.ok_or("no talk plan")?;
                    tail = (4 * usize::from(plan.frame_ms) + 3_000) * 16;
                    if plan.mode == TalkMode::Duplex {
                        phase = Phase::Playing(now);
                    } else {
                        eng[0].talk_key(true);
                        phase = Phase::WaitFloor(now);
                    }
                } else if now > SETUP_LIMIT_MS {
                    return Err(format!("{tag}: no voice slots after {now} ms"));
                }
            }
            Phase::WaitFloor(t) => {
                if eng[0].floor() == Floor::Mine {
                    phase = Phase::Playing(now);
                } else if now.wrapping_sub(t) > 30_000 {
                    return Err(format!("{tag}: no floor after 30 s"));
                }
            }
            _ => {}
        }
        let playing = matches!(phase, Phase::Playing(_));
        let mic: Vec<i16> = if playing {
            let mut v: Vec<i16> = wav.get(pos..(pos + CHUNK).min(wav.len())).unwrap_or(&[]).to_vec();
            v.resize(CHUNK, 0);
            pos += CHUNK;
            v
        } else {
            zeros.to_vec()
        };
        eng[0].mic(&mic);
        eng[1].mic(&zeros);
        let mut junk = [0i16; CHUNK];
        eng[0].speaker(&mut junk);
        let mut heard = [0i16; CHUNK];
        eng[1].speaker(&mut heard);
        if playing {
            rec.extend_from_slice(&heard);
            if rec.len() >= wav.len() + tail {
                break;
            }
        }
    }
    // ANN's mic stops; the frames still queued or on the air arrive before
    // the counts are read.
    let drain_ms = stats_plan_ms(&eng[0]) * 3 + 1_000;
    for _ in 0..drain_ms / STEP_MS {
        for (k, e) in eng.iter_mut().enumerate() {
            let b = e.take_modem_bytes();
            if !b.is_empty() {
                multi.voice_send(k, SESSION, &b);
            }
        }
        multi.run_ms(STEP_MS);
        for (k, e) in eng.iter_mut().enumerate() {
            let b = multi.voice_recv(k, SESSION);
            e.on_modem_bytes(&b);
            e.tick(multi.now_ms);
            while e.poll_event().is_some() {}
            let mut junk = [0i16; CHUNK];
            e.speaker(&mut junk);
        }
    }
    let (sa, sb) = (eng[0].stats(), eng[1].stats());
    eng[0].talk_key(false);
    eng[0].hangup();
    for _ in 0..200 {
        for (k, e) in eng.iter_mut().enumerate() {
            let b = e.take_modem_bytes();
            if !b.is_empty() {
                multi.voice_send(k, SESSION, &b);
            }
        }
        multi.run_ms(STEP_MS);
        for (k, e) in eng.iter_mut().enumerate() {
            let b = multi.voice_recv(k, SESSION);
            e.on_modem_bytes(&b);
            e.tick(multi.now_ms);
            while e.poll_event().is_some() {}
        }
    }
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let path = a.out.join(format!("{tag}.wav"));
    write_wav(&path, &rec).map_err(|e| format!("{}: {e}", path.display()))?;
    let plan = sa.plan.ok_or("no talk plan")?;
    let (delay_ms, matched) = delay(wav, &rec, 300 + u32::from(plan.frame_ms));
    Ok(Row {
        sf: sa.sf,
        codec: codec_name(plan.codec),
        mode: mode_name(plan.mode),
        sent: sa.frames_sent,
        heard: sb.frames_heard,
        concealed: sb.frames_concealed,
        delay_ms,
        matched,
        first_audio_ms: sb.first_audio_ms.zip(sa.connected_ms).map(|(f, c)| f.wrapping_sub(c)),
        slots_ms: sa.slots_ms.zip(sa.connected_ms).map(|(s, c)| s.wrapping_sub(c)),
    })
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let wav = match read_wav(&a.input) {
        Ok(w) if !w.is_empty() => w,
        Ok(_) => {
            eprintln!("{}: no audio", a.input.display());
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&a.out) {
        eprintln!("{}: {e}", a.out.display());
        return ExitCode::from(2);
    }
    println!(
        "lfd3k-voicesim: {} ({:.1} s), SNR {} dB, seed {}, out {}",
        a.input.display(),
        wav.len() as f64 / 16_000.0,
        a.snr_db,
        a.seed,
        a.out.display()
    );
    println!("tier  codec        mode         sent  heard  lost  concealed  mouth-to-ear  match  CONNECT-to-slots  CONNECT-to-first-audio");
    let mut failed = false;
    for &t in &a.tiers {
        match run_tier(t, &a, &wav) {
            Ok(r) => {
                let ms = |v: Option<u32>| v.map_or("none".to_string(), |v| format!("{v} ms"));
                println!(
                    "SF{:<3} {:<12} {:<12} {:>4}  {:>5}  {:>4}  {:>9}  {:>9} ms  {:>5.2}  {:>16}  {:>22}",
                    r.sf,
                    r.codec,
                    r.mode,
                    r.sent,
                    r.heard,
                    r.sent.saturating_sub(r.heard),
                    r.concealed,
                    r.delay_ms,
                    r.matched,
                    ms(r.slots_ms),
                    ms(r.first_audio_ms)
                );
            }
            Err(e) => {
                println!("SF{:<3} FAILED: {e}", t.sf());
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
