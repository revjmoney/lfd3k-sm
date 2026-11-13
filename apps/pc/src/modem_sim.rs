//! The modem thread: a real `Modem` on a simulated radio over UDP
//! (`SimNetRadio` with `UdpPort`, the loop of `lfd3k-pcmodem`), the voice
//! app's call `Engine` on voice host protocol session 1 of that modem, and
//! the audio pump (mic queue or `--play` WAV into the engine, the engine's
//! speaker into the speaker queue or the `--record` WAV).
//!
//! The thread runs every millisecond. The window talks to it with [`Cmd`]
//! and hears back through [`Ev`] plus a shared [`Snap`].
//!
//! The modem monitor: every 50 ms the thread gives the modem one channel
//! sample from the simulated radio (`SimNetRadio::rx_rssi`, the RSSI of a
//! frame being received or the -110 dBm floor, and 0 dBm flagged as
//! transmit while sending, as the firmware's trace does), so the modem's
//! Scope frames reach the engine like a real modem's would. AirEvents from
//! the engine start the connection sounds ([`crate::monitor::Chirps`]) only
//! while a call is set up (dialing, ringing, training or a resync) and
//! for 2 s after a call or attempt ends or a Hangup goes by (NO CARRIER);
//! while a call is up only a retrain's own frames chirp, ducked; never
//! voice frames, never while idle. A chirp never clips the call audio. They are mixed into the speaker here, not in
//! the sound card's callback. The ON/OFF choice is kept in
//! `app-prefs.txt` in the data folder (default ON).
//!
//! The modem's saved settings (AT&W) and its directory are kept in the data
//! folder (`modem-settings.bin`, `modem-dir.bin`) as the firmware keeps
//! them in NVS, and loaded at the next start. This modem has no Wi-Fi, so
//! no Wi-Fi password is ever written: the copy on disk carries a fixed
//! placeholder hotspot password and no client network. Files here are
//! written owner only on Unix (the user profile's ACL on Windows).

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
use crate::monitor::Chirps;

/// The voice host protocol session the app uses (1 to 3 are the platform's).
const SESSION: u8 = 1;
/// Speaker audio is made in 10 ms pieces.
const CHUNK: usize = 160;
/// Most speaker audio queued ahead of the sound card (latency cap).
const SPK_AHEAD: usize = 3_200;
/// An AtLine with no final reply after this long is given up.
const AT_TIMEOUT_MS: u32 = 20_000;
/// Keep ringing this long after the last Ring (the modem repeats it every 3 s).
const RING_HOLD_MS: u32 = 7_000;
/// Auto-answer push to talk: listen this long between two plays of the WAV.
const AUTO_LISTEN_MS: u32 = 8_000;
/// One channel sample for the modem's scope this often.
const SCOPE_MS: u32 = 50;
/// Connection sounds go on this long after a call or attempt ends (the
/// Hangup frames: NO CARRIER); a retrain's Keepalive probes sound this long
/// after its last TierSet or TierAck.
const SOUND_AFTER_MS: u32 = 2_000;
/// Frame types of a retrain (TierSet, TierAck).
const RETRAIN_TYPES: [u8; 2] = [8, 9];
/// Keepalive (a retrain's probes, while connected only after a retrain
/// frame) and the frames that end a call (CallReject, Hangup).
const KEEPALIVE: u8 = 11;
const END_TYPES: [u8; 2] = [3, 10];
/// While a call is connected a retrain's chirps play at this share of the
/// usual level, under the call audio.
const DUCK: f32 = 0.25;
/// CallState `state` while the link resyncs.
const STATE_RESYNC: u8 = 5;

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
    /// Connection sounds on or off (saved in `app-prefs.txt`).
    Sounds(bool),
    Quit,
}

/// What the modem thread tells the window.
pub enum Ev {
    Ui(UiEvent),
    /// The link code to show (wiped when the window drops it).
    LinkCode(Zeroizing<String>),
    /// The link offer or join is over (used, failed, timed out or
    /// cancelled): drop the code shown.
    LinkEnded,
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
    /// Seconds of ring tone made (test helpers print it).
    pub ring_s: f32,
    /// Seconds of connection sound made (test helpers print it).
    pub chirp_s: f32,
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
            ring_s: 0.0,
            chirp_s: 0.0,
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
    /// Connection sounds on.
    pub sounds: bool,
}

pub fn identity_path(data: &Path) -> PathBuf {
    data.join("identity.bin")
}

fn prefs_path(data: &Path) -> PathBuf {
    data.join("app-prefs.txt")
}

/// Connection sounds on (the default) unless `app-prefs.txt` says
/// `connection_sounds=off`.
pub fn load_sounds(data: &Path) -> bool {
    let t = std::fs::read_to_string(prefs_path(data)).unwrap_or_default();
    !t.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("connection_sounds=off"))
}

fn save_sounds(data: &Path, on: bool) -> std::io::Result<()> {
    let t = format!("connection_sounds={}\n", if on { "on" } else { "off" });
    write_file(&prefs_path(data), t.as_bytes())
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
    write_file(&removed_path(data), t.as_bytes())
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
    /// The engine was linking at the last pass.
    was_linking: bool,
    /// A call rings us (no auto-answer): ring until this time (a Ring
    /// comes every 3 s while it rings).
    ring_until: Option<u32>,
    /// Ring tone samples made (the cadence clock).
    ring_pos: u64,
    ring_total: u64,
    max_gap: u32,
    passes: u64,
    /// When the next scope sample is due.
    scope_due: u32,
    chirps: Chirps,
    /// The modem's call state at the last pass, and connection sounds
    /// allowed until this time whatever the state (after an end or a
    /// retrain frame).
    last_state: u8,
    sound_until: Option<u32>,
    /// While connected: a retrain frame went by; its probes sound until then.
    retrain_until: Option<u32>,
    /// The `--record` WAV keeps recording until then after a call ends
    /// (its NO CARRIER chirps), then is written.
    rec_tail: Option<u32>,
    /// Seconds of connection sound made (test helpers print it).
    chirp_total: u64,
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
                // First start: the defaults, saved. A profile needs a
                // hotspot password to load again; this modem has no Wi-Fi,
                // so it gets the fixed placeholder.
                let s = Settings::defaults(modem.settings().ap_ssid(), AP_PLACEHOLDER);
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
                was_linking: false,
                ring_until: None,
                ring_pos: 0,
                ring_total: 0,
                max_gap: 0,
                passes: 0,
                scope_due: now,
                chirps: Chirps::default(),
                last_state: 0,
                sound_until: None,
                retrain_until: None,
                rec_tail: None,
                chirp_total: 0,
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
            self.scope_sample(now);
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

    /// Every 50 ms: the simulated channel's RSSI (0 dBm and flagged while
    /// we transmit) into the modem's scope.
    fn scope_sample(&mut self, now: u32) {
        if now.wrapping_sub(self.scope_due) >= u32::MAX / 2 {
            return;
        }
        // Keep the 50 ms cadence; start again after a long stall.
        self.scope_due = self.scope_due.wrapping_add(SCOPE_MS);
        if now.wrapping_sub(self.scope_due) < u32::MAX / 2 {
            self.scope_due = now.wrapping_add(SCOPE_MS);
        }
        let tx = self.radio.transmitting();
        let rssi = if tx { 0 } else { self.radio.rx_rssi() };
        self.modem.scope_sample(rssi, tx, now);
    }

    /// A call is being set up: the modem dialing, ringing or training, or
    /// the link resyncing.
    fn setting_up(&self) -> bool {
        matches!(self.modem.call_state(), 1..=3)
            || self.engine.in_call() && self.engine.stats().state == STATE_RESYNC
    }

    /// Chirps may go on playing: on, and a call set up, or the end window
    /// (NO CARRIER) or a retrain window open.
    fn sounding(&self, now: u32) -> bool {
        self.o.sounds
            && (self.setting_up()
                || before(self.sound_until, now)
                || before(self.retrain_until, now))
    }

    /// Whether a frame chirps now, and ducked (under a live call). While
    /// a call is connected only a retrain's own frames (TierSet, TierAck,
    /// and Keepalive probes right after one) and the end frames do, never
    /// voice, Data or Listener frames.
    fn chirp_for(&mut self, a: &lfd3k_voice::AirFrame, st: u8, now: u32) -> Option<bool> {
        if !self.o.sounds {
            return None;
        }
        let up = self.setting_up();
        chirp_rule(
            a.ftype,
            st,
            up,
            &mut self.sound_until,
            &mut self.retrain_until,
            now,
        )
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
            let n = scrubbed(self.modem.saved_settings())
                .to_blob(&mut b)
                .min(BLOB_LEN);
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
            Cmd::Answer => {
                self.ring_until = None;
                e.answer();
            }
            Cmd::Decline => {
                self.ring_until = None;
                e.decline();
            }
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
            Cmd::Sounds(on) => {
                self.o.sounds = on;
                if !on {
                    self.chirps.stop();
                }
                if let Err(err) = save_sounds(&self.o.data, on) {
                    self.send(Ev::Note(format!("could not save the preferences: {err}")));
                }
            }
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
        // A call or attempt that just ended: its Hangup frames sound (NO
        // CARRIER). One that just connected: the setup sounds stop.
        let st = self.modem.call_state();
        if self.last_state != 0 && st == 0 {
            self.sound_until = Some(now.wrapping_add(SOUND_AFTER_MS));
        }
        self.last_state = st;
        if !before(self.sound_until, now) {
            self.sound_until = None;
        }
        if !before(self.retrain_until, now) {
            self.retrain_until = None;
        }
        if !self.sounding(now) {
            self.chirps.stop();
        }
        if self.rec_tail.is_some_and(|t| !before(Some(t), now)) && !self.engine.in_call() {
            self.rec_tail = None;
            self.write_rec();
        }
        while let Some(ev) = self.engine.poll_event() {
            match ev {
                UiEvent::Air(a) => {
                    if let Some(duck) = self.chirp_for(&a, st, now) {
                        self.chirps.start(&a, if duck { DUCK } else { 1.0 });
                    }
                    self.send(Ev::Ui(UiEvent::Air(a)));
                }
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
                    } else if !self.engine.in_call() {
                        self.ring_until = Some(now.wrapping_add(RING_HOLD_MS));
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
                    self.ring_until = None;
                    self.send(Ev::Ui(UiEvent::Connected { plan, words, trust }));
                }
                e @ (UiEvent::Ended
                | UiEvent::Declined
                | UiEvent::NoAnswer
                | UiEvent::AnsweredElsewhere) => {
                    self.auto_ptt = AutoPtt::Idle;
                    self.ring_until = None;
                    // Written once the NO CARRIER chirps are in.
                    self.rec_tail = Some(now.wrapping_add(SOUND_AFTER_MS + 300));
                    self.send(Ev::Ui(e));
                }
                other => self.send(Ev::Ui(other)),
            }
        }
        if self
            .ring_until
            .is_some_and(|t| now.wrapping_sub(t) < u32::MAX / 2)
        {
            self.ring_until = None;
        }
        let linking = self.engine.linking();
        if self.was_linking && !linking {
            self.send(Ev::LinkEnded);
        }
        self.was_linking = linking;
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
            if self.ring_until.is_some() && !in_call {
                ring_tone(&mut out, self.ring_pos);
                self.ring_pos += CHUNK as u64;
                self.ring_total += CHUNK as u64;
            } else {
                self.ring_pos = 0;
            }
            if self.chirps.playing() {
                self.chirps.mix(&mut out);
                self.chirp_total += CHUNK as u64;
            }
            if self.o.record.is_some() && (in_call || self.rec_tail.is_some()) {
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
            ring_s: self.ring_total as f32 / 16_000.0,
            chirp_s: self.chirp_total as f32 / 16_000.0,
        };
        if let Ok(mut g) = self.snap.lock() {
            *g = s;
        }
    }
}

/// `chirp_for` without the app: whether a frame of header type `ftype`
/// chirps (`Some(ducked)`) given the engine's call state `st` and whether a
/// call is being set up; it opens the end window (`sound_until`) on a
/// Hangup or CallReject of our own call and the retrain window
/// (`retrain_until`) on a TierSet or TierAck while connected.
fn chirp_rule(
    ftype: u8,
    st: u8,
    setting_up: bool,
    sound_until: &mut Option<u32>,
    retrain_until: &mut Option<u32>,
    now: u32,
) -> Option<bool> {
    if END_TYPES.contains(&ftype) {
        // A Hangup or CallReject sounds only while this app has a call
        // or an attempt (dialing, ringing, training, connected, resync)
        // or the end window it opened itself. Other stations' hangups
        // heard while idle stay silent.
        let ours = st >= 4 || setting_up || before(*sound_until, now);
        if !ours {
            return None;
        }
        *sound_until = Some(now.wrapping_add(SOUND_AFTER_MS));
        return Some(false);
    }
    let connected = st >= 4 && !setting_up;
    if connected {
        if RETRAIN_TYPES.contains(&ftype) {
            *retrain_until = Some(now.wrapping_add(SOUND_AFTER_MS));
            return Some(true);
        }
        let probe = ftype == KEEPALIVE && before(*retrain_until, now);
        return probe.then_some(true);
    }
    (setting_up || before(*sound_until, now)).then_some(false)
}

/// The hotspot password written to disk in place of any real one (this
/// modem has no Wi-Fi; a saved profile needs 8 to 63 characters there).
const AP_PLACEHOLDER: &str = "NO-WIFI-ON-THIS-MODEM";

/// The saved profile as it goes to disk: the placeholder hotspot password
/// and no client network.
fn scrubbed(s: &Settings) -> Settings {
    let mut c = s.clone();
    let ssid = c.ap_ssid().to_string();
    let _ = c.set_ap(&ssid, AP_PLACEHOLDER);
    c.clear_sta();
    c
}

/// A US style ring (440 Hz plus 480 Hz, 2 s on, 4 s off) into `out`,
/// starting at sample `pos` of the cadence.
fn ring_tone(out: &mut [i16], pos: u64) {
    for (i, s) in out.iter_mut().enumerate() {
        let n = pos + i as u64;
        let in_cycle = n % (6 * 16_000);
        if in_cycle >= 2 * 16_000 {
            *s = 0;
            continue;
        }
        let t = n as f64 / 16_000.0;
        let v = (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            + (2.0 * std::f64::consts::PI * 480.0 * t).sin();
        *s = (v * 0.12 * 32767.0) as i16;
    }
}

/// Write `b` to `p` through a side file, owner only on Unix.
fn write_file(p: &Path, b: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = open_private(&tmp)?;
        f.write_all(b)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, p)
}

#[cfg(unix)]
fn open_private(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)
}

#[cfg(not(unix))]
fn open_private(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(p)
}

/// `until` is set and not reached yet at `now`.
fn before(until: Option<u32>, now: u32) -> bool {
    until.is_some_and(|t| t.wrapping_sub(now) < u32::MAX / 2 && t != now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_scrubbed_profile_carries_no_wifi_password() {
        // 496dc47: modem-settings.bin never holds a real Wi-Fi password: the
        // hotspot password is the placeholder and there is no client network.
        let mut s = Settings::defaults("REV-PC", "defaultpass1");
        assert!(s.set_ap("SHOPNET", "hunter2hunter2"));
        assert!(s.set_sta("HOMEWIFI", "correct horse battery"));
        let c = scrubbed(&s);
        assert_eq!(c.ap_ssid(), "SHOPNET", "the hotspot name stays");
        assert_eq!(c.ap_pass(), AP_PLACEHOLDER);
        assert_eq!(c.sta_ssid(), "");
        assert_eq!(c.sta_pass(), "");
        let mut b = [0u8; BLOB_LEN];
        let n = c.to_blob(&mut b).min(BLOB_LEN);
        let blob = &b[..n];
        for secret in [
            &b"hunter2hunter2"[..],
            b"correct horse battery",
            b"HOMEWIFI",
        ] {
            assert!(
                !blob.windows(secret.len()).any(|w| w == secret),
                "a secret reached the blob"
            );
        }
        // The original is untouched (the modem keeps its own profile).
        assert_eq!(s.ap_pass(), "hunter2hunter2");
    }

    /// Run the rule for `ftype` at `now` with fresh or given windows.
    fn rule(
        ftype: u8,
        st: u8,
        up: bool,
        w: &mut (Option<u32>, Option<u32>),
        now: u32,
    ) -> Option<bool> {
        chirp_rule(ftype, st, up, &mut w.0, &mut w.1, now)
    }

    #[test]
    fn regression_connected_chirps_only_retrain_and_own_end_frames() {
        // 303a9f6: while connected (state 4 or 5, not setting up) voice,
        // Data, Ack and Listener frames (type 15 voice and Listener, 6
        // Data, 7 Ack, 11 Keepalive outside a retrain) never chirp; TierSet
        // and TierAck chirp ducked and open a 2 s window in which Keepalive
        // probes chirp; Hangup and CallReject chirp only during our own call.
        let mut w = (None, None);
        for t in [15, 6, 7, 11, 1, 2, 4, 5, 12, 13, 14] {
            assert_eq!(
                rule(t, 4, false, &mut w, 1_000),
                None,
                "type {t} while connected"
            );
        }
        assert_eq!(
            rule(8, 4, false, &mut w, 1_000),
            Some(true),
            "TierSet, ducked"
        );
        assert_eq!(
            rule(11, 4, false, &mut w, 1_500),
            Some(true),
            "a probe in the retrain window"
        );
        assert_eq!(
            rule(15, 4, false, &mut w, 1_500),
            None,
            "voice never, even then"
        );
        assert_eq!(rule(6, 4, false, &mut w, 1_500), None);
        assert_eq!(rule(11, 4, false, &mut w, 3_100), None, "window over");
        assert_eq!(rule(9, 4, false, &mut w, 4_000), Some(true), "TierAck");
        // End frames: ours while connected, not when idle with no window.
        assert_eq!(rule(10, 4, false, &mut w, 5_000), Some(false));
        let mut idle = (None, None);
        assert_eq!(
            rule(10, 0, false, &mut idle, 5_000),
            None,
            "another station's Hangup"
        );
        assert_eq!(rule(3, 0, false, &mut idle, 5_000), None);
        assert_eq!(
            rule(10, 0, true, &mut idle, 5_000),
            Some(false),
            "while dialing"
        );
        // Setting up: every frame chirps, at full level.
        let mut s = (None, None);
        for t in [1, 4, 8, 15] {
            assert_eq!(
                rule(t, 1, true, &mut s, 100),
                Some(false),
                "type {t} while setting up"
            );
        }
        // The end window: the frames after our Hangup chirp for 2 s, then stop.
        let mut e = (None, None);
        assert_eq!(rule(10, 4, false, &mut e, 10_000), Some(false));
        assert_eq!(rule(12, 0, false, &mut e, 11_000), Some(false));
        assert_eq!(rule(12, 0, false, &mut e, 12_100), None);
    }

    #[test]
    fn regression_chirp_mix_never_clips_call_audio() {
        // 303a9f6: a chirp mixed under call audio never takes a sample past
        // full scale (and never wraps it around).
        use crate::monitor::Chirps;
        for &(base, gain) in &[
            (32_767i16, 1.0f32),
            (-32_768, 1.0),
            (32_000, 1.0),
            (-31_000, 0.25),
            (0, 1.0),
        ] {
            let mut ch = Chirps::default();
            let a = lfd3k_voice::AirFrame {
                rx: false,
                sf: 7,
                ftype: 8,
                sub: 0xFF,
                len: 6,
                start_ms: 1,
                air_ms: 300,
                rssi_dbm: None,
                snr_x4: None,
            };
            ch.start(&a, gain);
            let mut buf = vec![base; 16 * 400];
            ch.mix(&mut buf);
            // Loud call audio stays on its side of zero: no wrap around.
            if base > 0 {
                assert!(buf.iter().all(|&s| s >= 0), "base {base}: wrapped");
            } else if base < 0 {
                assert!(buf.iter().all(|&s| s <= 0), "base {base}: wrapped");
            }
            if base == 0 {
                assert!(buf.iter().any(|&s| s != 0), "the chirp plays over silence");
                assert!(
                    buf.iter().all(|&s| s.unsigned_abs() < 32_000),
                    "a chirp alone stays under full scale"
                );
            }
            if base == i16::MAX {
                assert!(buf.iter().all(|&s| s > 0));
            }
        }
    }
}
