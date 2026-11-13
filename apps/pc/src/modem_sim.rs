//! The modem thread: a real `Modem` on a simulated radio over UDP
//! (`SimNetRadio` with `UdpPort`, the loop of `lfd3k-pcmodem`), the voice
//! app's call `Engine` on voice host protocol session 1 of that modem, and
//! the audio pump (mic queue or `--play` WAV into the engine, the engine's
//! speaker into the speaker queue or the `--record` WAV).
//!
//! The thread runs every millisecond. The window talks to it with [`Cmd`]
//! and hears back through [`Ev`] plus a shared [`Snap`].
//!
//! The modem's saved settings (AT&W) and its directory are kept in the data
//! folder (`modem-settings.bin`, `modem-dir.bin`) as the firmware keeps
//! them in NVS, and loaded at the next start. The hotspot and client
//! passwords in the settings are stored as is, as on the modem.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lfd3k_core::voice::{Floor, TalkMode};
use lfd3k_core::{Config, Modem, Settings, SimNetRadio, Tier, BLOB_LEN, DIR_BLOB_LEN};
use lfd3k_sim::UdpPort;
use lfd3k_voice::{CallStats, ContactView, Engine, Identity, Trust, TrustStore, UiEvent};
use zeroize::Zeroizing;

use crate::audio::{self, Queue};

/// The voice host protocol session the app uses (1 to 3 are the platform's).
const SESSION: u8 = 1;
/// Speaker audio is made in 10 ms pieces.
const CHUNK: usize = 160;
/// Most speaker audio queued ahead of the sound card (latency cap).
const SPK_AHEAD: usize = 3_200;
/// An AtLine with no final reply after this long is given up.
const AT_TIMEOUT_MS: u32 = 20_000;
/// Auto-answer push to talk: listen this long between two plays of the WAV.
const AUTO_LISTEN_MS: u32 = 8_000;

/// What the window asks of the modem thread.
pub enum Cmd {
    Call {
        name: String,
        label: Option<String>,
    },
    Answer,
    Decline,
    Hangup,
    TalkKey(bool),
    AcceptWords,
    TalkAnyway,
    Mute(bool),
    LinkOffer,
    LinkJoin(Zeroizing<String>),
    CancelLink,
    /// One AT line, run through the voice host protocol's AtLine; the
    /// answer comes back as `Ev::At` with the same tag. The line is wiped
    /// once sent.
    At {
        tag: u32,
        line: Zeroizing<String>,
    },
    RefreshDir,
    /// Publish one of our own devices as removed (kept in `removed.json`).
    RemoveMyDevice([u8; 32]),
    RemoveContactDevice(String, [u8; 32]),
    Rename {
        name: String,
        label: String,
    },
    Quit,
}

/// What the modem thread tells the window.
pub enum Ev {
    Ui(UiEvent),
    /// The link code to show (wiped when the window drops it).
    LinkCode(Zeroizing<String>),
    /// An AtLine's whole answer.
    At {
        tag: u32,
        text: String,
        ok: bool,
    },
    /// Something for the status line.
    Note(String),
    /// The thread has stopped (after Quit).
    Stopped,
}

/// The live state the window draws from (copied every few milliseconds).
#[derive(Clone)]
pub struct Snap {
    pub name: String,
    pub label: String,
    pub station: String,
    pub device_key: [u8; 32],
    pub has_person: bool,
    pub stats: CallStats,
    pub floor: Floor,
    pub in_call: bool,
    /// Voice name, device label and station of the far side.
    pub peer: Option<(String, String, String)>,
    pub peer_muted: bool,
    pub contacts: Vec<ContactView>,
    pub removed_mine: Vec<[u8; 32]>,
    pub linking: bool,
    /// The modem's call state (`Modem::call_state`).
    pub call_state: u8,
    /// Seconds of the WAV played and recorded (test helpers).
    pub play_s: f32,
    pub rec_s: f32,
    pub bind: String,
    pub udp_peer: String,
    /// Longest gap between two passes of the modem loop (ms), and passes.
    pub max_gap_ms: u32,
    pub passes: u64,
}

impl Default for Snap {
    fn default() -> Snap {
        Snap {
            name: String::new(),
            label: String::new(),
            station: String::new(),
            device_key: [0; 32],
            has_person: false,
            stats: CallStats::default(),
            floor: Floor::Free,
            in_call: false,
            peer: None,
            peer_muted: false,
            contacts: Vec::new(),
            removed_mine: Vec::new(),
            linking: false,
            call_state: 0,
            play_s: 0.0,
            rec_s: 0.0,
            bind: String::new(),
            udp_peer: String::new(),
            max_gap_ms: 0,
            passes: 0,
        }
    }
}

/// Start-up options for the thread.
pub struct Opts {
    pub bind: String,
    pub peer: SocketAddr,
    pub station: String,
    pub snr_db: i16,
    pub cap_tier: Option<Tier>,
    pub data: PathBuf,
    pub auto_answer: bool,
    pub play: Option<Vec<i16>>,
    pub record: Option<PathBuf>,
    pub mic: Option<Queue>,
    pub spk: Option<Queue>,
}

pub fn identity_path(data: &Path) -> PathBuf {
    data.join("identity.bin")
}

fn removed_path(data: &Path) -> PathBuf {
    data.join("removed.json")
}

fn hex(k: &[u8; 32]) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim().trim_matches('"');
    if s.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, o) in k.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(k)
}

/// Our own devices published as removed: a JSON list of hex device keys.
fn load_removed(data: &Path) -> Vec<[u8; 32]> {
    let t = std::fs::read_to_string(removed_path(data)).unwrap_or_default();
    t.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(unhex)
        .collect()
}

fn save_removed(data: &Path, v: &[[u8; 32]]) -> std::io::Result<()> {
    let items: Vec<String> = v.iter().map(|k| format!("  \"{}\"", hex(k))).collect();
    let t = format!("[\n{}\n]\n", items.join(",\n"));
    let p = removed_path(data);
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, t)?;
    std::fs::rename(&tmp, &p)
}

/// The AtLine in flight and the ones waiting.
struct AtQueue {
    wait: VecDeque<(u32, Zeroizing<String>)>,
    cur: Option<(u32, String, u32)>,
}

/// The push to talk cycle of `--auto-answer --play` on a push to talk tier.
#[derive(Clone, Copy, PartialEq)]
enum AutoPtt {
    Idle,
    /// Key down, waiting for the floor.
    Want,
    /// Floor ours, playing from this WAV position until a full pass.
    Talk(usize),
    /// Key up, listening since this time.
    Listen(u32),
}

struct Worker {
    o: Opts,
    modem: Modem,
    radio: SimNetRadio<UdpPort>,
    engine: Engine,
    vin: VecDeque<u8>,
    at: AtQueue,
    tx: Sender<Ev>,
    snap: Arc<Mutex<Snap>>,
    ctx: eframe::egui::Context,
    removed: Vec<[u8; 32]>,
    play_pos: usize,
    played: usize,
    rec: Vec<i16>,
    spk_made: u64,
    t0: u32,
    auto_ptt: AutoPtt,
    snap_due: u32,
    max_gap: u32,
    passes: u64,
}

/// Run the modem thread until `Cmd::Quit`.
pub fn spawn(
    o: Opts,
    cmd: Receiver<Cmd>,
    tx: Sender<Ev>,
    snap: Arc<Mutex<Snap>>,
    ctx: eframe::egui::Context,
) -> Result<std::thread::JoinHandle<()>, String> {
    let cfg =
        Config::new(&o.station).ok_or("station name: 1 to 16 printable characters, no colon")?;
    let port =
        UdpPort::bind(&o.bind, o.peer).map_err(|e| format!("cannot bind {}: {e}", o.bind))?;
    let id = Identity::load(&identity_path(&o.data))
        .map_err(|e| format!("identity {}: {e}", identity_path(&o.data).display()))?;
    std::thread::Builder::new()
        .name("modem".into())
        .spawn(move || {
            let mut modem = Modem::new(cfg);
            if let Some(s) = std::fs::read(o.data.join("modem-settings.bin"))
                .ok()
                .and_then(|b| Settings::from_blob(&b))
            {
                modem.load_settings(s);
            } else {
                // First start, as the firmware's first boot: the defaults
                // with a generated 12 character hotspot password, saved (a
                // profile with no hotspot password would not load again).
                let pass = new_ap_pass();
                let s = Settings::defaults(modem.settings().ap_ssid(), &pass);
                modem.load_settings(s);
                modem.request_save();
            }
            if let Ok(b) = std::fs::read(o.data.join("modem-dir.bin")) {
                let _ = modem.load_directory_blob(&b);
            }
            let mut radio = SimNetRadio::new(port);
            radio.set_peer_hears(o.snr_db.saturating_mul(4), -78 + o.snr_db);
            let now = radio.port().now_ms();
            // A tier cap for this run only: the line the menu composes for
            // S21, on the host stream before the voice session opens.
            if let Some(t) = o.cap_tier {
                let line = format!("ATS21={}\r", t.index());
                let _ = modem.host_input(line.as_bytes(), &mut radio, now);
                let mut b = [0u8; 256];
                while modem.host_output(&mut b) > 0 {}
            }
            modem.voice_open(SESSION, now);
            let trust_path = TrustStore::path_next_to(&identity_path(&o.data));
            let removed = load_removed(&o.data);
            let mut engine = Engine::new(id, trust_path);
            for d in &removed {
                engine.remove_my_device(*d);
            }
            let mut w = Worker {
                modem,
                radio,
                engine,
                vin: VecDeque::new(),
                at: AtQueue {
                    wait: VecDeque::new(),
                    cur: None,
                },
                tx,
                snap,
                ctx,
                removed,
                play_pos: 0,
                played: 0,
                rec: Vec::new(),
                spk_made: 0,
                t0: now,
                auto_ptt: AutoPtt::Idle,
                snap_due: 0,
                max_gap: 0,
                passes: 0,
                o,
            };
            w.run(cmd);
        })
        .map_err(|e| e.to_string())
}

impl Worker {
    fn send(&self, e: Ev) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }

    fn run(&mut self, cmd: Receiver<Cmd>) {
        let mut buf = [0u8; 1024];
        let mut frame = [0u8; 255];
        let mut quit_at: Option<u32> = None;
        let mut last = self.radio.port().now_ms();
        loop {
            let now = self.radio.port().now_ms();
            self.max_gap = self.max_gap.max(now.wrapping_sub(last));
            self.passes += 1;
            last = now;
            while let Some(n) = self.radio.port().recv(&mut buf) {
                self.radio.on_datagram(buf.get(..n).unwrap_or(&[]));
            }
            while let Some((n, info)) = self.radio.service(&mut frame) {
                self.modem
                    .radio_rx(frame.get(..n).unwrap_or(&[]), info, &mut self.radio, now);
            }
            if quit_at.is_none() {
                while let Ok(c) = cmd.try_recv() {
                    if matches!(c, Cmd::Quit) {
                        self.engine.leave();
                        quit_at = Some(now);
                        break;
                    }
                    self.on_cmd(c, now);
                }
            }
            self.engine.tick(now);
            self.pump_engine(now);
            self.modem.tick(&mut self.radio, now);
            self.platform(now);
            if quit_at.is_none() {
                self.events(now);
                self.audio(now);
                self.at_next(now);
            }
            self.pump_engine(now);
            if now.wrapping_sub(self.snap_due) < u32::MAX / 2 {
                self.snap_due = now.wrapping_add(20);
                self.update_snap(now);
            }
            if quit_at.is_some_and(|q| now.wrapping_sub(q) > 300) {
                self.write_rec();
                self.send(Ev::Stopped);
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Engine bytes to the modem's session, the modem's back to the engine.
    fn pump_engine(&mut self, now: u32) {
        self.vin.extend(self.engine.take_modem_bytes());
        if !self.vin.is_empty() {
            let (a, _) = self.vin.as_slices();
            let chunk: Vec<u8> = a.to_vec();
            let k = self
                .modem
                .voice_input(SESSION, &chunk, &mut self.radio, now)
                .min(chunk.len());
            self.vin.drain(..k);
        }
        let mut b = [0u8; 512];
        loop {
            let n = self.modem.voice_output(SESSION, &mut b);
            if n == 0 {
                break;
            }
            self.engine.on_modem_bytes(b.get(..n).unwrap_or(&[]));
        }
    }

    /// What the firmware would do for the modem: save blobs, refuse Wi-Fi
    /// dials (the PC modem has no Wi-Fi), drop the terminal's output.
    fn platform(&mut self, now: u32) {
        if self.modem.take_save_request() {
            let mut b = [0u8; BLOB_LEN];
            let n = self.modem.saved_settings().to_blob(&mut b).min(BLOB_LEN);
            if let Err(e) = write_file(
                &self.o.data.join("modem-settings.bin"),
                b.get(..n).unwrap_or(&[]),
            ) {
                self.send(Ev::Note(format!("could not save the modem settings: {e}")));
            }
            zeroize::Zeroize::zeroize(&mut b);
        }
        if self.modem.take_dir_save() {
            let mut b = [0u8; DIR_BLOB_LEN];
            let n = self.modem.directory().to_blob(&mut b).min(DIR_BLOB_LEN);
            if let Err(e) = write_file(
                &self.o.data.join("modem-dir.bin"),
                b.get(..n).unwrap_or(&[]),
            ) {
                self.send(Ev::Note(format!("could not save the directory: {e}")));
            }
        }
        if self.modem.take_wire_request().is_some() {
            self.modem.wire_failed(now);
        }
        let _ = self.modem.take_wire_close();
        let _ = self.modem.take_wifi_changed();
        let mut b = [0u8; 256];
        while self.modem.host_output(&mut b) > 0 {}
    }

    fn on_cmd(&mut self, c: Cmd, now: u32) {
        let e = &mut self.engine;
        match c {
            Cmd::Call { name, label } => e.call(&name, label.as_deref()),
            Cmd::Answer => e.answer(),
            Cmd::Decline => e.decline(),
            Cmd::Hangup => {
                e.talk_key(false);
                e.hangup();
            }
            Cmd::TalkKey(d) => e.talk_key(d),
            Cmd::AcceptWords => e.accept_words(),
            Cmd::TalkAnyway => e.talk_anyway(),
            Cmd::Mute(m) => e.set_mute(m),
            Cmd::LinkOffer => {
                // The code comes back as a LinkCode event; this copy goes.
                let _ = Zeroizing::new(e.start_link_offer());
            }
            Cmd::LinkJoin(code) => e.join_link(&code),
            Cmd::CancelLink => e.cancel_link(),
            Cmd::At { tag, line } => self.at.wait.push_back((tag, line)),
            Cmd::RefreshDir => e.refresh_directory(),
            Cmd::RemoveMyDevice(d) => {
                e.remove_my_device(d);
                let me = e.me().0.to_string();
                e.remove_contact_device(&me, d);
                if !self.removed.contains(&d) {
                    self.removed.push(d);
                }
                if let Err(err) = save_removed(&self.o.data, &self.removed) {
                    self.send(Ev::Note(format!(
                        "could not save the removed devices: {err}"
                    )));
                }
            }
            Cmd::RemoveContactDevice(n, d) => e.remove_contact_device(&n, d),
            Cmd::Rename { name, label } => self.rename(&name, &label, now),
            Cmd::Quit => {}
        }
    }

    /// A new name or label: a new cert from our person key, saved, then a
    /// new engine (Leave, then a fresh session that registers the new name).
    fn rename(&mut self, name: &str, label: &str, now: u32) {
        if self.engine.in_call() {
            self.send(Ev::Note("hang up first to change the name".into()));
            return;
        }
        let path = identity_path(&self.o.data);
        let mut id = match Identity::load(&path) {
            Ok(i) => i,
            Err(e) => {
                self.send(Ev::Note(format!("identity: {e}")));
                return;
            }
        };
        if id.rename(name, label).is_none() {
            self.send(Ev::Note(
                "not changed: a name is 1 to 16 printable, no ':'; a label 1 to 8, no ':' or '/'"
                    .into(),
            ));
            return;
        }
        if let Err(e) = id.save(&path) {
            self.send(Ev::Note(format!("could not save the identity: {e}")));
            return;
        }
        self.engine.leave();
        self.pump_engine(now);
        self.modem.voice_close(SESSION, now);
        self.vin.clear();
        self.modem.voice_open(SESSION, now);
        self.engine = Engine::new(id, TrustStore::path_next_to(&path));
        for d in &self.removed {
            self.engine.remove_my_device(*d);
        }
        self.send(Ev::Note(format!("now {name}/{label}")));
    }

    fn events(&mut self, now: u32) {
        while let Some(ev) = self.engine.poll_event() {
            match ev {
                UiEvent::AtReply { text, last } => {
                    if let Some((_, acc, _)) = self.at.cur.as_mut() {
                        acc.push_str(&text);
                    }
                    if last {
                        if let Some((tag, acc, _)) = self.at.cur.take() {
                            let ok = acc.lines().any(|l| l.trim() == "OK");
                            self.send(Ev::At { tag, text: acc, ok });
                        }
                    }
                }
                UiEvent::Error(e)
                    if e.starts_with("modem: message 0x0F") && self.at.cur.is_some() =>
                {
                    if let Some((tag, _, _)) = self.at.cur.take() {
                        self.send(Ev::At {
                            tag,
                            text: e,
                            ok: false,
                        });
                    }
                }
                UiEvent::LinkCode(c) => {
                    let c = Zeroizing::new(c);
                    self.send(Ev::LinkCode(c));
                }
                UiEvent::Linked => {
                    if let Err(e) = self.engine.identity().save(&identity_path(&self.o.data)) {
                        self.send(Ev::Note(format!("could not save the identity: {e}")));
                    }
                    self.send(Ev::Ui(UiEvent::Linked));
                }
                UiEvent::Ringing {
                    name,
                    label,
                    station,
                } => {
                    if self.o.auto_answer {
                        self.engine.answer();
                    }
                    self.send(Ev::Ui(UiEvent::Ringing {
                        name,
                        label,
                        station,
                    }));
                }
                UiEvent::Connected { plan, words, trust } => {
                    if self.o.auto_answer && trust != Trust::Conflict {
                        self.engine.talk_anyway();
                    }
                    self.auto_ptt = AutoPtt::Idle;
                    self.send(Ev::Ui(UiEvent::Connected { plan, words, trust }));
                }
                e @ (UiEvent::Ended
                | UiEvent::Declined
                | UiEvent::NoAnswer
                | UiEvent::AnsweredElsewhere) => {
                    self.auto_ptt = AutoPtt::Idle;
                    self.write_rec();
                    self.send(Ev::Ui(e));
                }
                other => self.send(Ev::Ui(other)),
            }
        }
        let _ = now;
    }

    /// Mic in, speaker out, paced by the clock (16 samples a millisecond).
    fn audio(&mut self, now: u32) {
        let in_call = self.engine.in_call();
        let st = self.engine.stats();
        let slots = in_call && st.slots_ms.is_some();
        let ptt = st.plan.is_some_and(|p| p.mode != TalkMode::Duplex);
        let floor = self.engine.floor();
        let due = u64::from(now.wrapping_sub(self.t0)) * 16;
        // Auto-answer push to talk: take the floor, play the WAV once, let
        // go and listen, again.
        if self.o.auto_answer && self.o.play.is_some() && slots && ptt {
            match self.auto_ptt {
                AutoPtt::Idle if floor == Floor::Free => {
                    self.engine.talk_key(true);
                    self.auto_ptt = AutoPtt::Want;
                }
                AutoPtt::Want if floor == Floor::Mine => self.auto_ptt = AutoPtt::Talk(self.played),
                AutoPtt::Talk(start) => {
                    let len = self.o.play.as_ref().map_or(0, Vec::len);
                    if self.played.saturating_sub(start) >= len {
                        self.engine.talk_key(false);
                        self.auto_ptt = AutoPtt::Listen(now);
                    }
                }
                AutoPtt::Listen(t) if now.wrapping_sub(t) >= AUTO_LISTEN_MS => {
                    self.auto_ptt = AutoPtt::Idle
                }
                _ => {}
            }
        } else if !slots {
            self.auto_ptt = AutoPtt::Idle;
        }
        while self.spk_made + CHUNK as u64 <= due {
            self.spk_made += CHUNK as u64;
            // Mic: the WAV (looped) while we may talk, else the device.
            let sending = slots && (!ptt || floor == Floor::Mine);
            let mic: Vec<i16> = match &self.o.play {
                Some(w) if !w.is_empty() => {
                    if sending {
                        let v: Vec<i16> = (0..CHUNK)
                            .map(|i| w.get((self.play_pos + i) % w.len()).copied().unwrap_or(0))
                            .collect();
                        self.play_pos = (self.play_pos + CHUNK) % w.len();
                        self.played += CHUNK;
                        v
                    } else {
                        vec![0; CHUNK]
                    }
                }
                _ => match &self.o.mic {
                    Some(q) => {
                        let mut q = q.lock().unwrap_or_else(|p| p.into_inner());
                        let n = q.len().min(CHUNK * 4);
                        q.drain(..n).collect()
                    }
                    None => vec![0; CHUNK],
                },
            };
            if in_call {
                self.engine.mic(&mic);
            }
            let mut out = [0i16; CHUNK];
            self.engine.speaker(&mut out);
            if self.o.record.is_some() && in_call {
                self.rec.extend_from_slice(&out);
            }
            if let Some(q) = &self.o.spk {
                let mut q = q.lock().unwrap_or_else(|p| p.into_inner());
                let over = (q.len() + CHUNK).saturating_sub(SPK_AHEAD);
                let over = over.min(q.len());
                q.drain(..over);
                q.extend(out);
                let cap = q.len().saturating_sub(audio::QUEUE_MAX);
                q.drain(..cap);
            }
        }
    }

    /// Send the next AtLine when none is in flight.
    fn at_next(&mut self, now: u32) {
        if let Some((tag, _, t)) = &self.at.cur {
            if now.wrapping_sub(*t) > AT_TIMEOUT_MS {
                let tag = *tag;
                self.at.cur = None;
                self.send(Ev::At {
                    tag,
                    text: "no answer from the modem".into(),
                    ok: false,
                });
            }
            return;
        }
        let Some((tag, line)) = self.at.wait.pop_front() else {
            return;
        };
        if self.engine.at_line(&line) {
            self.at.cur = Some((tag, String::new(), now));
        } else {
            self.send(Ev::At {
                tag,
                text: "line too long".into(),
                ok: false,
            });
        }
        drop(line);
    }

    fn write_rec(&mut self) {
        if let Some(p) = &self.o.record {
            if !self.rec.is_empty() {
                if let Err(e) = audio::write_wav(p, &self.rec) {
                    let _ = self.tx.send(Ev::Note(format!("{}: {e}", p.display())));
                }
            }
        }
    }

    fn update_snap(&mut self, _now: u32) {
        let e = &self.engine;
        let (name, label) = e.me();
        let id = e.identity();
        let s = Snap {
            name: name.to_string(),
            label: label.to_string(),
            station: e.station().to_string(),
            device_key: id.device_pub(),
            has_person: id.person_pub().is_some(),
            stats: e.stats(),
            floor: e.floor(),
            in_call: e.in_call(),
            peer: e.peer(),
            peer_muted: e.peer_muted(),
            contacts: e.contacts(),
            removed_mine: self.removed.clone(),
            linking: e.linking(),
            call_state: self.modem.call_state(),
            play_s: self.played as f32 / 16_000.0,
            rec_s: self.rec.len() as f32 / 16_000.0,
            bind: self.o.bind.clone(),
            udp_peer: self.o.peer.to_string(),
            max_gap_ms: self.max_gap,
            passes: self.passes,
        };
        if let Ok(mut g) = self.snap.lock() {
            *g = s;
        }
    }
}

/// 12 characters from letters and digits (no look-alikes), from the OS
/// random source.
fn new_ap_pass() -> Zeroizing<String> {
    const SET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut r = Zeroizing::new([0u8; 12]);
    // Without a random source the hotspot keeps no password (the profile
    // then is not saved), which is no worse than before.
    if getrandom::fill(r.as_mut()).is_err() {
        return Zeroizing::new(String::new());
    }
    Zeroizing::new(
        r.iter()
            .map(|b| char::from(SET[usize::from(*b) % SET.len()]))
            .collect(),
    )
}

fn write_file(p: &Path, b: &[u8]) -> std::io::Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, b)?;
    std::fs::rename(&tmp, p)
}
