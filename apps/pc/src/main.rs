//! `lfd3k-voice-pc`: voice calls by name on the PC, for the LORA FLOWTRON
//! DATAMASTER 3000 SERIAL MODEM - LFD3K-SM.
//!
//! The app runs its own modem (a real `Modem` on SimNet over UDP, like
//! `lfd3k-pcmodem`), so two copies on one PC, or on two PCs, call each
//! other with no radio. See `docs/voice-pc.md`.
//!
//! ```text
//! lfd3k-voice-pc --name REV --label PC --bind 127.0.0.1:4211 --peer 127.0.0.1:4210
//!     [--snr <dB>] [--cap-tier SF9] [--data <dir>] [--station <NAME>]
//!     [--auto-answer] [--play <wav>] [--record <wav>]
//!     [--script <file> --shots <dir>]
//! ```

// No console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod modem_sim;
mod ui;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use lfd3k_core::Tier;
use lfd3k_voice::Identity;

pub struct Args {
    pub name: String,
    pub label: String,
    pub bind: String,
    pub peer: SocketAddr,
    pub snr_db: i16,
    pub cap_tier: Option<Tier>,
    pub data: PathBuf,
    pub station: String,
    pub auto_answer: bool,
    pub play: Option<PathBuf>,
    pub record: Option<PathBuf>,
    pub script: Option<PathBuf>,
    pub shots: Option<PathBuf>,
}

fn usage() -> String {
    "usage: lfd3k-voice-pc --name <NAME> --label <LABEL> [--bind <addr:port>] [--peer <addr:port>] \
     [--snr <dB>] [--cap-tier <SF5..SF12>] [--data <dir>] [--station <NAME>] [--auto-answer] \
     [--play <wav>] [--record <wav>] [--script <file> --shots <dir>]"
        .to_string()
}

fn parse_tier(s: &str) -> Option<Tier> {
    let n = s
        .trim_start_matches(['S', 's'])
        .trim_start_matches(['F', 'f']);
    lfd3k_voice::tier_from_sf(n.parse().ok()?)
}

fn parse_args() -> Result<Args, String> {
    let mut name = "REV".to_string();
    let mut label = "PC".to_string();
    let mut bind = "127.0.0.1:4211".to_string();
    let mut peer = "127.0.0.1:4210".to_string();
    let mut a = Args {
        name: String::new(),
        label: String::new(),
        bind: String::new(),
        peer: SocketAddr::from(([127, 0, 0, 1], 4210)),
        snr_db: 8,
        cap_tier: None,
        data: PathBuf::new(),
        station: String::new(),
        auto_answer: false,
        play: None,
        record: None,
        script: None,
        shots: None,
    };
    let mut data = None;
    let mut station = None;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || {
            it.next()
                .ok_or_else(|| format!("{k} needs a value\n{}", usage()))
        };
        match k.as_str() {
            "--name" => name = v()?,
            "--label" => label = v()?,
            "--bind" => bind = v()?,
            "--peer" => peer = v()?,
            "--snr" => a.snr_db = v()?.parse().map_err(|e| format!("--snr: {e}"))?,
            "--cap-tier" => {
                let t = v()?;
                a.cap_tier = Some(
                    parse_tier(&t).ok_or_else(|| format!("--cap-tier: {t} is not SF5 to SF12"))?,
                );
            }
            "--data" => data = Some(PathBuf::from(v()?)),
            "--station" => station = Some(v()?),
            "--auto-answer" => a.auto_answer = true,
            "--play" => a.play = Some(PathBuf::from(v()?)),
            "--record" => a.record = Some(PathBuf::from(v()?)),
            "--script" => a.script = Some(PathBuf::from(v()?)),
            "--shots" => a.shots = Some(PathBuf::from(v()?)),
            "-h" | "--help" => return Err(usage()),
            _ => return Err(format!("unknown option {k}\n{}", usage())),
        }
    }
    a.peer = peer
        .parse()
        .map_err(|e| format!("--peer {peer}: {e} (want addr:port)"))?;
    let port = bind.rsplit_once(':').map_or("", |(_, p)| p).to_string();
    a.station = station.unwrap_or_else(|| format!("PCMODEM-{port}"));
    a.data = data.unwrap_or_else(|| {
        let base = std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("HOME"))
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        base.join("lfd3k-voice-pc").join(format!("{name}-{label}"))
    });
    a.name = name;
    a.label = label;
    a.bind = bind;
    Ok(a)
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&a.data) {
        eprintln!("{}: {e}", a.data.display());
        return ExitCode::from(2);
    }
    // First run: make this device's identity (a device key and a person
    // key). Later runs keep the saved one (change the name in Settings).
    let idp = modem_sim::identity_path(&a.data);
    if !idp.exists() {
        let Some(id) = Identity::generate(&a.name, &a.label) else {
            eprintln!("--name is 1 to 16 printable characters without ':'; --label 1 to 8 without ':' or '/'");
            return ExitCode::from(2);
        };
        if let Err(e) = id.save(&idp) {
            eprintln!("{}: {e}", idp.display());
            return ExitCode::from(2);
        }
    }
    let play = match &a.play {
        Some(p) => match audio::read_wav(p) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    // --play stands in for the mic and --record for the speaker, so a test
    // copy never feeds the room back into another copy's mic.
    let sound = audio::open(a.play.is_none(), a.record.is_none());
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let snap = Arc::new(Mutex::new(modem_sim::Snap::default()));
    let title = format!(
        "LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM  {}/{}",
        a.name, a.label
    );
    let icon = ui::logo_icon();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([1280.0, 720.0]);
    if let Some(i) = icon {
        viewport = viewport.with_icon(Arc::new(i));
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let r = eframe::run_native(
        "lfd3k-voice-pc",
        opts,
        Box::new(move |cc| {
            ui::setup(&cc.egui_ctx);
            let o = modem_sim::Opts {
                bind: a.bind.clone(),
                peer: a.peer,
                station: a.station.clone(),
                snr_db: a.snr_db,
                cap_tier: a.cap_tier,
                data: a.data.clone(),
                auto_answer: a.auto_answer,
                play,
                record: a.record.clone(),
                mic: sound.mic.clone(),
                spk: sound.spk.clone(),
            };
            modem_sim::spawn(o, cmd_rx, ev_tx, snap.clone(), cc.egui_ctx.clone())?;
            Ok(Box::new(ui::App::new(
                &cc.egui_ctx,
                a,
                sound,
                cmd_tx,
                ev_rx,
                snap,
            )))
        }),
    );
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
