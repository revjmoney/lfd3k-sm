//! The phone's modem thread: the call `Engine` on a simulated modem
//! (SimNet over UDP, the same loop as the PC app) or on a real modem
//! (TCP, mDNS find, or the USB byte pipe).
//!
//! The thread owns the engine. The UI sends commands and reads a
//! snapshot. Mic and speaker samples move through two queues the audio
//! callback fills and drains. The engine still sees 16 kHz mono, paced
//! by this thread's clock (160 samples every 10 ms), the way the PC app
//! paces it.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lfd3k_core::voice::{Floor, TalkMode, TalkPlan};
use lfd3k_core::{
    Config as ModemConfig, Modem, Settings, SimNetRadio, Tier, BLOB_LEN, DIR_BLOB_LEN,
};
use lfd3k_voice::{
    codec_name, mode_name, tier_from_sf, AirFrame, DirLine, Engine, Identity, IdentityError, Trust,
    TrustStore, UiEvent,
};
use zeroize::Zeroizing;

use crate::chirp::Chirps;
use crate::link::{RealLink, UsbPipe};
use crate::udp::UdpPort;

const SESSION: u8 = 1;
const CHUNK: usize = 160;
const SPK_AHEAD: usize = 3_200;
const MIC_MAX: usize = 8_000;
const AT_TIMEOUT_MS: u32 = 20_000;
const RING_HOLD_MS: u32 = 7_000;
const SCOPE_MS: u32 = 50;
const SOUND_AFTER_MS: u32 = 2_000;
const RETRAIN_TYPES: [u8; 2] = [8, 9];
const KEEPALIVE: u8 = 11;
const END_TYPES: [u8; 2] = [3, 10];
const DUCK: f32 = 0.25;
const STATE_RESYNC: u8 = 5;
const RESTORE_MS: u32 = 2_000;
const RECV_PASSES: usize = 64;
const SILENT_SCOPE_MS: u32 = 3_000;
const SILENT_MS: u32 = 20_000;
const TAG_CAP_READ: u32 = 0xFFFF_0001;
const TAG_CAP_SET: u32 = 0xFFFF_0002;
const TAG_CAP_RESTORE: u32 = 0xFFFF_0003;
const TAG_CAP_UNSET: u32 = 0xFFFF_0004;
const T_ATV: u32 = 1;
const T_FWD: u32 = 2;
const T_WAP: u32 = 3;
const T_WSTA: u32 = 4;
const T_ATI: u32 = 5;
const T_WIFI: u32 = 6;
const T_SCAN: u32 = 7;
const T_DIR: u32 = 8;
const T_SET: u32 = 10;
const T_SAVE: u32 = 11;
const T_DISCARD: u32 = 12;
const AP_PLACEHOLDER: &str = "NO-WIFI-ON-THIS-MODEM";
const DIR_CAP: usize = 48;
const CONTACT_CAP: usize = 16;
const SCOPE_CAP: usize = 160;
const AIR_CAP: usize = 24;
const SCAN_CAP: usize = 8;

/// Which modem the connect screen picked.
#[derive(Clone, Debug)]
pub enum Transport {
    Sim,
    Tcp { host: String, port: u16 },
    Find { station: Option<String> },
    Usb { label: String },
}

#[derive(Clone, Debug)]
pub struct Start {
    pub transport: Transport,
    pub name: String,
    pub label: String,
    pub data: PathBuf,
    pub bind: String,
    pub peer: SocketAddr,
    pub station: String,
    pub snr_db: i16,
    /// Spreading factor 5 to 12, or none.
    pub cap_sf: Option<u8>,
    /// None loads `app-prefs.txt` (on when the file does not say off).
    pub sounds: Option<bool>,
    pub auto_answer: bool,
}

#[derive(Clone)]
pub struct DirSnap {
    pub name: String,
    pub label: String,
    pub station: String,
    pub hint: String,
    pub sf: i32,
    pub pinned: bool,
    pub trust: i32,
    pub voice: bool,
}

#[derive(Clone)]
pub struct ContactSnap {
    pub name: String,
    pub devices: Vec<(String, [u8; 32])>,
    pub person: bool,
    pub removed: usize,
}

#[derive(Clone, Copy)]
pub struct AirSnap {
    pub rx: bool,
    pub sf: u8,
    pub start_ms: u32,
    pub air_ms: u16,
}

#[derive(Clone)]
pub struct View {
    pub name: String,
    pub label: String,
    pub station: String,
    pub link: String,
    pub bind: String,
    pub peer: String,
    pub banner: String,
    pub banner_warn: bool,
    pub banner_at: Option<Instant>,
    pub note: String,
    pub words: [String; 2],
    pub plan: String,
    pub who: String,
    pub peer_name: String,
    pub peer_label: String,
    pub peer_station: String,
    pub trust_line: String,
    pub link_code: String,
    pub link_note: String,
    pub device_key: [u8; 32],
    pub has_person: bool,
    pub in_call: bool,
    pub ringing: bool,
    pub ring_name: String,
    pub ring_label: String,
    pub ring_station: String,
    pub audio_ok: bool,
    pub talk_anyway: bool,
    pub conflict: bool,
    pub muted: bool,
    pub ptt: bool,
    pub floor: i32,
    pub call_state: i32,
    pub sf: i32,
    pub locked: bool,
    pub sounds: bool,
    pub linking: bool,
    pub cap_active: bool,
    pub unsaved: bool,
    pub pw_dirty: bool,
    pub lost: bool,
    pub ready: bool,
    pub connecting: bool,
    pub scanning: bool,
    pub rssi: i32,
    pub scope_now: u32,
    pub dir: Vec<DirSnap>,
    pub contacts: Vec<ContactSnap>,
    pub scope: Vec<(i8, bool)>,
    pub air: Vec<AirSnap>,
    pub scan: Vec<String>,
    pub atv: String,
    pub ati: String,
    pub fwd: String,
    pub wap: String,
    pub wsta: String,
    pub wifi: String,
}

impl View {
    fn connecting(s: &Start) -> View {
        View {
            name: s.name.clone(),
            label: s.label.clone(),
            station: s.station.clone(),
            link: "CONNECTING".into(),
            bind: s.bind.clone(),
            peer: s.peer.to_string(),
            banner: String::new(),
            banner_warn: false,
            banner_at: None,
            note: String::new(),
            words: [String::new(), String::new()],
            plan: String::new(),
            who: String::new(),
            peer_name: String::new(),
            peer_label: String::new(),
            peer_station: String::new(),
            trust_line: String::new(),
            link_code: String::new(),
            link_note: String::new(),
            device_key: [0; 32],
            has_person: false,
            in_call: false,
            ringing: false,
            ring_name: String::new(),
            ring_label: String::new(),
            ring_station: String::new(),
            audio_ok: false,
            talk_anyway: false,
            conflict: false,
            muted: false,
            ptt: false,
            floor: 0,
            call_state: 0,
            sf: 0,
            locked: false,
            sounds: true,
            linking: false,
            cap_active: false,
            unsaved: false,
            pw_dirty: false,
            lost: false,
            ready: false,
            connecting: true,
            scanning: false,
            rssi: -999,
            scope_now: 0,
            dir: Vec::new(),
            contacts: Vec::new(),
            scope: Vec::new(),
            air: Vec::new(),
            scan: Vec::new(),
            atv: String::new(),
            ati: String::new(),
            fwd: String::new(),
            wap: String::new(),
            wsta: String::new(),
            wifi: String::new(),
        }
    }

    pub fn banner_now(&self) -> &str {
        match self.banner_at {
            Some(t) if t.elapsed().as_secs() < 8 => self.banner.as_str(),
            _ => "",
        }
    }
}

enum Cmd {
    Call { name: String, label: Option<String> },
    Answer,
    Decline,
    Hangup,
    Talk(bool),
    Words,
    Anyway,
    Mute(bool),
    LinkOffer,
    LinkJoin(String),
    CancelLink,
    Set(String),
    Save,
    Discard,
    Scan,
    Rename { name: String, label: String },
    Sounds(bool),
    RemoveMine([u8; 32]),
    RemoveContact { name: String, key: [u8; 32] },
    Quit,
}

struct AudioQ {
    mic: Mutex<VecDeque<i16>>,
    spk: Mutex<VecDeque<i16>>,
}

impl AudioQ {
    fn new() -> Arc<AudioQ> {
        Arc::new(AudioQ {
            mic: Mutex::new(VecDeque::new()),
            spk: Mutex::new(VecDeque::new()),
        })
    }

    fn push_mic(&self, pcm: &[i16]) {
        let mut q = self.mic.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(pcm);
        if q.len() > MIC_MAX {
            let n = q.len() - MIC_MAX;
            q.drain(..n);
        }
    }

    fn pull_spk(&self, out: &mut [i16]) -> usize {
        let mut q = self.spk.lock().unwrap_or_else(|e| e.into_inner());
        let n = out.len().min(q.len());
        for (i, s) in q.drain(..n).enumerate() {
            if let Some(d) = out.get_mut(i) {
                *d = s;
            }
        }
        n
    }
}

enum Backend {
    Sim {
        modem: Box<Modem>,
        radio: Box<SimNetRadio<UdpPort>>,
    },
    Real(RealLink),
    Gone,
}

struct AtQueue {
    wait: VecDeque<(u32, Zeroizing<String>)>,
    cur: Option<(u32, String, u32)>,
}

pub struct Session {
    cmd: Sender<Cmd>,
    view: Arc<Mutex<View>>,
    audio: Arc<AudioQ>,
    usb: Arc<UsbPipe>,
    join: Option<std::thread::JoinHandle<()>>,
    stopped: bool,
}

impl Session {
    pub fn start(s: Start) -> Result<Session, String> {
        std::fs::create_dir_all(&s.data).map_err(|e| format!("data folder: {e}"))?;
        let id_path = identity_path(&s.data);
        if !id_path.exists() {
            let id = Identity::generate(&s.name, &s.label).map_err(|e| match e {
                IdentityError::BadNameOrLabel => e.to_string(),
                IdentityError::Random => e.to_string(),
            })?;
            id.save(&id_path).map_err(|e| format!("identity: {e}"))?;
        } else {
            Identity::load(&id_path).map_err(|e| format!("identity: {e}"))?;
        }
        if matches!(s.transport, Transport::Sim) && ModemConfig::new(&s.station).is_none() {
            return Err("station name: 1 to 16 printable characters, no colon".into());
        }
        let view = Arc::new(Mutex::new(View::connecting(&s)));
        let audio = AudioQ::new();
        let usb = UsbPipe::new();
        let (tx, rx) = mpsc::channel();
        let view_t = Arc::clone(&view);
        let audio_t = Arc::clone(&audio);
        let usb_t = Arc::clone(&usb);
        let join = std::thread::Builder::new()
            .name("modem".into())
            .spawn(move || thread_main(s, rx, view_t, audio_t, usb_t))
            .map_err(|e| e.to_string())?;
        Ok(Session {
            cmd: tx,
            view,
            audio,
            usb,
            join: Some(join),
            stopped: false,
        })
    }

    /// End the modem thread. Safe to call twice. The USB pump must still be
    /// running through this call so the Leave frame can go out; free the
    /// session only after the pump has stopped.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.usb.cancel();
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }

    pub fn view(&self) -> View {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn text(&self, which: i32) -> String {
        let v = self.view.lock().unwrap_or_else(|e| e.into_inner());
        match which {
            0 => v.atv.clone(),
            1 => v.ati.clone(),
            2 => v.fwd.clone(),
            3 => v.wap.clone(),
            4 => v.wsta.clone(),
            5 => v.wifi.clone(),
            _ => String::new(),
        }
    }

    pub fn push_mic(&self, pcm: &[i16]) {
        self.audio.push_mic(pcm);
    }

    pub fn pull_spk(&self, out: &mut [i16]) -> usize {
        self.audio.pull_spk(out)
    }

    pub fn push_usb_rx(&self, b: &[u8]) {
        self.usb.push_rx(b);
    }

    pub fn pop_usb_tx(&self, buf: &mut [u8]) -> usize {
        self.usb.pop_tx(buf)
    }

    pub fn unget_usb_tx(&self, b: &[u8]) {
        self.usb.unget_tx(b);
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    pub fn call(&self, name: &str, label: Option<&str>) {
        self.send(Cmd::Call {
            name: name.to_string(),
            label: label.map(str::to_string).filter(|s| !s.is_empty()),
        });
    }
    pub fn answer(&self) {
        self.send(Cmd::Answer);
    }
    pub fn decline(&self) {
        self.send(Cmd::Decline);
    }
    pub fn hangup(&self) {
        self.send(Cmd::Hangup);
    }
    pub fn talk(&self, down: bool) {
        self.send(Cmd::Talk(down));
    }
    pub fn words(&self) {
        self.send(Cmd::Words);
    }
    pub fn anyway(&self) {
        self.send(Cmd::Anyway);
    }
    pub fn mute(&self, on: bool) {
        self.send(Cmd::Mute(on));
    }
    pub fn link_offer(&self) {
        self.send(Cmd::LinkOffer);
    }
    pub fn link_join(&self, code: &str) {
        self.send(Cmd::LinkJoin(code.to_string()));
    }
    pub fn cancel_link(&self) {
        self.send(Cmd::CancelLink);
    }
    pub fn set_line(&self, line: &str) {
        self.send(Cmd::Set(line.to_string()));
    }
    pub fn save(&self) {
        self.send(Cmd::Save);
    }
    pub fn discard(&self) {
        self.send(Cmd::Discard);
    }
    pub fn scan(&self) {
        self.send(Cmd::Scan);
    }
    pub fn rename(&self, name: &str, label: &str) {
        self.send(Cmd::Rename {
            name: name.to_string(),
            label: label.to_string(),
        });
    }
    pub fn sounds(&self, on: bool) {
        self.send(Cmd::Sounds(on));
    }
    pub fn remove_mine(&self, key: [u8; 32]) {
        self.send(Cmd::RemoveMine(key));
    }
    pub fn remove_contact(&self, name: &str, key: [u8; 32]) {
        self.send(Cmd::RemoveContact {
            name: name.to_string(),
            key,
        });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn identity_path(data: &Path) -> PathBuf {
    data.join("identity.bin")
}

fn thread_main(
    s: Start,
    cmd: Receiver<Cmd>,
    view: Arc<Mutex<View>>,
    audio: Arc<AudioQ>,
    usb: Arc<UsbPipe>,
) {
    let sounds = s.sounds.unwrap_or_else(|| load_sounds(&s.data));
    let cap = s.cap_sf.and_then(tier_from_sf);
    let opened = open_backend(&s, &usb);
    let (be, link_desc, locked, bind_shown) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            let mut v = view.lock().unwrap_or_else(|err| err.into_inner());
            v.connecting = false;
            v.ready = true;
            v.lost = true;
            v.link = "MODEM LOST".into();
            v.note = e.clone();
            v.banner = e;
            v.banner_warn = true;
            v.banner_at = Some(Instant::now());
            v.sounds = sounds;
            drop(v);
            // Stay up so Quit is observed and the UI can read the error.
            idle_until_quit(cmd);
            return;
        }
    };
    let id = match Identity::load(&identity_path(&s.data)) {
        Ok(id) => id,
        Err(e) => {
            let mut v = view.lock().unwrap_or_else(|err| err.into_inner());
            v.connecting = false;
            v.ready = true;
            v.lost = true;
            v.link = "MODEM LOST".into();
            v.note = format!("identity: {e}");
            return;
        }
    };
    let mut w = Worker::new(
        s, be, id, view, audio, sounds, cap, link_desc, locked, bind_shown,
    );
    w.prime();
    w.run(cmd);
}

fn idle_until_quit(cmd: Receiver<Cmd>) {
    loop {
        match cmd.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            _ => {}
        }
    }
}

fn open_backend(s: &Start, usb: &Arc<UsbPipe>) -> Result<(Backend, String, bool, String), String> {
    match &s.transport {
        Transport::Sim => {
            let cfg = ModemConfig::new(&s.station)
                .ok_or("station name: 1 to 16 printable characters, no colon")?;
            let port = UdpPort::bind(&s.bind, s.peer)
                .map_err(|e| format!("cannot bind {}: {e}", s.bind))?;
            let bind_shown = port
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| s.bind.clone());
            let mut modem = Modem::new(cfg);
            if let Some(saved) = std::fs::read(s.data.join("modem-settings.bin"))
                .ok()
                .and_then(|b| Settings::from_blob(&b))
            {
                modem.load_settings(saved);
            } else {
                let settings = Settings::defaults(modem.settings().ap_ssid(), AP_PLACEHOLDER);
                modem.load_settings(settings);
                modem.request_save();
            }
            if let Ok(b) = std::fs::read(s.data.join("modem-dir.bin")) {
                let _ = modem.load_directory_blob(&b);
            }
            let mut radio = SimNetRadio::new(port);
            radio.set_peer_hears(s.snr_db.saturating_mul(4), -78 + s.snr_db);
            let now = radio.port().now_ms();
            if let Some(t) = s.cap_sf.and_then(tier_from_sf) {
                let line = format!("ATS21={}\r", t.index());
                let _ = modem.host_input(line.as_bytes(), &mut radio, now);
                let mut b = [0u8; 256];
                while modem.host_output(&mut b) > 0 {}
            }
            modem.voice_open(SESSION, now);
            modem.voice_grant_settings(SESSION);
            let be = Backend::Sim {
                modem: Box::new(modem),
                radio: Box::new(radio),
            };
            Ok((be, String::new(), false, bind_shown))
        }
        Transport::Tcp { host, port } => {
            let link = RealLink::open_tcp(host, *port)?;
            let desc = link.describe();
            Ok((Backend::Real(link), desc, true, s.bind.clone()))
        }
        Transport::Find { station } => {
            let link = RealLink::open_find(station.as_deref())?;
            let desc = link.describe();
            Ok((Backend::Real(link), desc, true, s.bind.clone()))
        }
        Transport::Usb { label } => {
            let link = RealLink::open_usb(Arc::clone(usb), label.clone())?;
            let desc = link.describe();
            Ok((Backend::Real(link), desc, false, s.bind.clone()))
        }
    }
}

struct Worker {
    s: Start,
    be: Backend,
    clock: Instant,
    link_desc: String,
    bind_shown: String,
    cap: Option<Tier>,
    cap_old: Option<u16>,
    cap_end_tag: Option<(u32, bool)>,
    locked: bool,
    save_armed: bool,
    restore_sent: Option<u32>,
    rx_at: u32,
    scope_seen: bool,
    engine: Engine,
    vin: VecDeque<u8>,
    at: AtQueue,
    view: Arc<Mutex<View>>,
    audio: Arc<AudioQ>,
    removed: Vec<[u8; 32]>,
    sounds: bool,
    snap_due: u32,
    was_linking: bool,
    ring_until: Option<u32>,
    ring_pos: u64,
    scope_due: u32,
    chirps: Chirps,
    last_state: u8,
    sound_until: Option<u32>,
    retrain_until: Option<u32>,
    spk_made: u64,
    t0: u32,
    scope: VecDeque<(i8, bool)>,
    scope_now: u32,
    air: VecDeque<AirSnap>,
    dir: Vec<DirSnap>,
    scan: Vec<String>,
    scanning: bool,
    atv: String,
    ati: String,
    fwd: String,
    wap: String,
    wsta: String,
    wifi: String,
    baseline: Option<String>,
    rebase: bool,
    pw_dirty: bool,
    banner: String,
    banner_warn: bool,
    banner_at: Option<Instant>,
    note: String,
    words: [String; 2],
    plan_s: String,
    plan: Option<TalkPlan>,
    who: String,
    trust_line: String,
    audio_ok: bool,
    talk_anyway: bool,
    conflict: bool,
    muted: bool,
    link_code: String,
    link_note: String,
    lost: bool,
    wifi_due: u32,
    ring_on: bool,
    ring_name: String,
    ring_label: String,
    ring_station: String,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        s: Start,
        be: Backend,
        id: Identity,
        view: Arc<Mutex<View>>,
        audio: Arc<AudioQ>,
        sounds: bool,
        cap: Option<Tier>,
        link_desc: String,
        locked: bool,
        bind_shown: String,
    ) -> Worker {
        let trust_path = TrustStore::path_next_to(&identity_path(&s.data));
        let removed = load_removed(&s.data);
        let mut engine = Engine::new(id, trust_path);
        for d in &removed {
            engine.remove_my_device(*d);
        }
        Worker {
            s,
            be,
            clock: Instant::now(),
            link_desc,
            bind_shown,
            cap,
            cap_old: None,
            cap_end_tag: None,
            locked,
            save_armed: false,
            restore_sent: None,
            rx_at: 0,
            scope_seen: false,
            engine,
            vin: VecDeque::new(),
            at: AtQueue {
                wait: VecDeque::new(),
                cur: None,
            },
            view,
            audio,
            removed,
            sounds,
            snap_due: 0,
            was_linking: false,
            ring_until: None,
            ring_pos: 0,
            scope_due: 0,
            chirps: Chirps::default(),
            last_state: 0,
            sound_until: None,
            retrain_until: None,
            spk_made: 0,
            t0: 0,
            scope: VecDeque::new(),
            scope_now: 0,
            air: VecDeque::new(),
            dir: Vec::new(),
            scan: Vec::new(),
            scanning: false,
            atv: String::new(),
            ati: String::new(),
            fwd: String::new(),
            wap: String::new(),
            wsta: String::new(),
            wifi: String::new(),
            baseline: None,
            rebase: true,
            pw_dirty: false,
            banner: String::new(),
            banner_warn: false,
            banner_at: None,
            note: String::new(),
            words: [String::new(), String::new()],
            plan_s: String::new(),
            plan: None,
            who: String::new(),
            trust_line: String::new(),
            audio_ok: false,
            talk_anyway: false,
            conflict: false,
            muted: false,
            link_code: String::new(),
            link_note: String::new(),
            lost: false,
            wifi_due: 2_000,
            ring_on: false,
            ring_name: String::new(),
            ring_label: String::new(),
            ring_station: String::new(),
        }
    }

    fn prime(&mut self) {
        if self.cap.is_some() && self.locked {
            self.note("tier cap: settings are locked over Wi-Fi, so the cap is not set");
        } else if self.cap.is_some() && matches!(self.be, Backend::Real(_)) {
            self.at
                .wait
                .push_back((TAG_CAP_READ, Zeroizing::new("AT&V".into())));
        }
        self.queue_refresh();
        self.at
            .wait
            .push_back((T_ATI, Zeroizing::new("ATI".into())));
    }

    fn queue_refresh(&mut self) {
        for (tag, line) in [
            (T_ATV, "AT&V"),
            (T_FWD, "AT+FWD?"),
            (T_WAP, "AT+WAP?"),
            (T_WSTA, "AT+WSTA?"),
        ] {
            self.at
                .wait
                .push_back((tag, Zeroizing::new(line.to_string())));
        }
    }

    fn now(&self) -> u32 {
        match &self.be {
            Backend::Sim { radio, .. } => radio.port().now_ms(),
            _ => self.clock.elapsed().as_millis() as u32,
        }
    }

    fn call_state(&self) -> u8 {
        match &self.be {
            Backend::Sim { modem, .. } => modem.call_state(),
            _ => self.engine.stats().state,
        }
    }

    fn note(&mut self, t: impl Into<String>) {
        self.note = t.into();
    }

    fn banner(&mut self, t: impl Into<String>, warn: bool) {
        self.banner = t.into();
        self.banner_warn = warn;
        self.banner_at = Some(Instant::now());
    }

    fn run(&mut self, cmd: Receiver<Cmd>) {
        let mut buf = [0u8; 1024];
        let mut frame = [0u8; 255];
        let mut quit_at: Option<u32> = None;
        let mut restoring: Option<u32> = None;
        self.t0 = self.now();
        self.rx_at = self.t0;
        self.scope_due = self.t0.wrapping_add(SCOPE_MS);
        self.engine.refresh_directory();
        loop {
            let now = self.now();
            if let Backend::Sim { modem, radio } = &mut self.be {
                while let Some(n) = radio.port().recv(&mut buf) {
                    radio.on_datagram(buf.get(..n).unwrap_or(&[]));
                }
                while let Some((n, info)) = radio.service(&mut frame) {
                    modem.radio_rx(frame.get(..n).unwrap_or(&[]), info, &mut **radio, now);
                }
            }
            self.scope_sample(now);
            if quit_at.is_none() && restoring.is_none() {
                while let Ok(c) = cmd.try_recv() {
                    if matches!(c, Cmd::Quit) {
                        if self.start_quit(now) {
                            restoring = Some(now);
                        } else {
                            quit_at = Some(now);
                        }
                        break;
                    }
                    self.on_cmd(c, now);
                }
            }
            let restore_late = self
                .restore_sent
                .is_some_and(|t| now.wrapping_sub(t) > RESTORE_MS);
            let quit_late =
                restoring.is_some_and(|r| now.wrapping_sub(r) > AT_TIMEOUT_MS + RESTORE_MS);
            if restoring.is_some() && (self.cap_old.is_none() || restore_late || quit_late) {
                restoring = None;
                self.close_link();
                quit_at = Some(now);
            }
            self.engine.tick(now);
            self.pump(now);
            if let Backend::Sim { modem, radio } = &mut self.be {
                modem.tick(&mut **radio, now);
            }
            self.platform();
            if quit_at.is_none() {
                self.events(now);
                self.audio_pump(now);
                self.at_next(now);
                self.wifi_poll(now);
            }
            self.pump(now);
            if now.wrapping_sub(self.snap_due) < u32::MAX / 2 {
                self.snap_due = now.wrapping_add(20);
                self.publish();
            }
            if quit_at.is_some_and(|q| now.wrapping_sub(q) > 300) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn start_quit(&mut self, now: u32) -> bool {
        match &self.be {
            Backend::Sim { .. } => {
                self.engine.leave();
                false
            }
            Backend::Real(_) => match self.cap_old {
                Some(v) => {
                    self.at.wait.clear();
                    self.at
                        .wait
                        .push_back((TAG_CAP_RESTORE, Zeroizing::new(format!("ATS21={v}"))));
                    self.at_next(now);
                    true
                }
                None => {
                    self.close_link();
                    false
                }
            },
            Backend::Gone => false,
        }
    }

    fn close_link(&mut self) {
        self.pump(self.now());
        if let Backend::Real(l) = std::mem::replace(&mut self.be, Backend::Gone) {
            l.close();
        }
    }

    fn lost(&mut self, why: String) {
        self.be = Backend::Gone;
        self.cap_old = None;
        self.link_desc = "MODEM LOST".into();
        self.lost = true;
        let path = identity_path(&self.s.data);
        if let Ok(id) = Identity::load(&path) {
            self.engine = Engine::new(id, TrustStore::path_next_to(&path));
            for d in &self.removed {
                self.engine.remove_my_device(*d);
            }
        }
        let _ = self.engine.take_modem_bytes();
        let cur = self.at.cur.take().map(|(tag, _, _)| tag);
        let waiting: Vec<u32> = self.at.wait.drain(..).map(|(tag, _)| tag).collect();
        for tag in cur.into_iter().chain(waiting) {
            if tag < TAG_CAP_READ {
                self.at_done(tag, "MODEM LOST".into(), false);
            }
        }
        self.ring_until = None;
        self.chirps.stop();
        self.banner(why, true);
    }

    fn scope_sample(&mut self, now: u32) {
        let Backend::Sim { modem, radio } = &mut self.be else {
            return;
        };
        if now.wrapping_sub(self.scope_due) >= u32::MAX / 2 {
            return;
        }
        self.scope_due = self.scope_due.wrapping_add(SCOPE_MS);
        if now.wrapping_sub(self.scope_due) < u32::MAX / 2 {
            self.scope_due = now.wrapping_add(SCOPE_MS);
        }
        let tx = radio.transmitting();
        let rssi = if tx { 0 } else { radio.rx_rssi() };
        modem.scope_sample(rssi, tx, now);
    }

    fn setting_up(&self) -> bool {
        matches!(self.call_state(), 1..=3)
            || self.engine.in_call() && self.engine.stats().state == STATE_RESYNC
    }

    fn sounding(&self, now: u32) -> bool {
        self.sounds
            && (self.setting_up()
                || before(self.sound_until, now)
                || before(self.retrain_until, now))
    }

    fn chirp_for(&mut self, a: &AirFrame, st: u8, now: u32) -> Option<bool> {
        if !self.sounds {
            return None;
        }
        chirp_rule(
            a.ftype,
            st,
            self.setting_up(),
            &mut self.sound_until,
            &mut self.retrain_until,
            now,
        )
    }

    fn pump(&mut self, now: u32) {
        let mut err = None;
        match &mut self.be {
            Backend::Sim { modem, radio } => {
                self.vin.extend(self.engine.take_modem_bytes());
                if !self.vin.is_empty() {
                    let (a, _) = self.vin.as_slices();
                    let chunk: Vec<u8> = a.to_vec();
                    let k = modem
                        .voice_input(SESSION, &chunk, &mut **radio, now)
                        .min(chunk.len());
                    self.vin.drain(..k);
                }
                let mut b = [0u8; 512];
                loop {
                    let n = modem.voice_output(SESSION, &mut b);
                    if n == 0 {
                        break;
                    }
                    self.engine.on_modem_bytes(b.get(..n).unwrap_or(&[]));
                }
            }
            Backend::Real(link) => {
                let out = self.engine.take_modem_bytes();
                if !out.is_empty() {
                    if let Err(e) = link.send(&out) {
                        err = Some(e);
                    }
                }
                let mut b = [0u8; 512];
                for _ in 0..RECV_PASSES {
                    if err.is_some() {
                        break;
                    }
                    match link.recv(&mut b) {
                        Ok(0) => break,
                        Ok(n) => {
                            self.rx_at = now;
                            self.engine.on_modem_bytes(b.get(..n).unwrap_or(&[]));
                        }
                        Err(e) => err = Some(e),
                    }
                }
                let window = if self.scope_seen {
                    SILENT_SCOPE_MS
                } else {
                    SILENT_MS
                };
                let silent = now.wrapping_sub(self.rx_at);
                if err.is_none() && silent < u32::MAX / 2 && silent > window {
                    err = Some(format!("no frames from the modem for {} s", window / 1000));
                }
            }
            Backend::Gone => {
                let _ = self.engine.take_modem_bytes();
            }
        }
        if let Some(e) = err {
            self.lost(e);
        }
    }

    fn platform(&mut self) {
        let mut notes = Vec::new();
        let now = self.now();
        let Backend::Sim { modem, .. } = &mut self.be else {
            return;
        };
        if modem.take_save_request() {
            let mut b = [0u8; BLOB_LEN];
            let n = scrubbed(modem.saved_settings())
                .to_blob(&mut b)
                .min(BLOB_LEN);
            if let Err(e) = write_file(
                &self.s.data.join("modem-settings.bin"),
                b.get(..n).unwrap_or(&[]),
            ) {
                notes.push(format!("could not save the modem settings: {e}"));
            }
            zeroize::Zeroize::zeroize(&mut b);
        }
        if modem.take_dir_save() {
            let mut b = [0u8; DIR_BLOB_LEN];
            let n = modem.directory().to_blob(&mut b).min(DIR_BLOB_LEN);
            if let Err(e) = write_file(
                &self.s.data.join("modem-dir.bin"),
                b.get(..n).unwrap_or(&[]),
            ) {
                notes.push(format!("could not save the directory: {e}"));
            }
        }
        if modem.take_wire_request().is_some() {
            modem.wire_failed(now);
        }
        let _ = modem.take_wire_close();
        let _ = modem.take_wifi_changed();
        let mut b = [0u8; 256];
        while modem.host_output(&mut b) > 0 {}
        for n in notes {
            self.note(n);
        }
    }

    fn on_cmd(&mut self, c: Cmd, now: u32) {
        match c {
            Cmd::Call { name, label } => {
                self.who = match &label {
                    Some(l) => format!("{name}/{l}"),
                    None => name.clone(),
                };
                self.engine.call(&name, label.as_deref());
            }
            Cmd::Answer => {
                self.clear_ring();
                self.engine.answer();
            }
            Cmd::Decline => {
                self.clear_ring();
                self.engine.decline();
            }
            Cmd::Hangup => {
                self.engine.talk_key(false);
                self.engine.hangup();
            }
            Cmd::Talk(d) => self.engine.talk_key(d),
            Cmd::Words => {
                self.engine.accept_words();
                self.audio_ok = !self.conflict;
            }
            Cmd::Anyway => {
                self.engine.talk_anyway();
                self.talk_anyway = true;
                self.audio_ok = !self.conflict;
            }
            Cmd::Mute(m) => {
                self.muted = m;
                self.engine.set_mute(m);
            }
            Cmd::LinkOffer => {
                let _ = self.engine.start_link_offer();
            }
            Cmd::LinkJoin(code) => self.engine.join_link(&code),
            Cmd::CancelLink => self.engine.cancel_link(),
            Cmd::Set(line) => self.enqueue_set(line),
            Cmd::Save => {
                self.at
                    .wait
                    .push_back((T_SAVE, Zeroizing::new("AT&W".into())));
                self.queue_refresh();
            }
            Cmd::Discard => {
                if self.engine.in_call() {
                    self.banner("HANG UP FIRST: DISCARD IS ATZ, WHICH ENDS A CALL", false);
                } else {
                    self.at
                        .wait
                        .push_back((T_DISCARD, Zeroizing::new("ATZ".into())));
                    self.queue_refresh();
                }
            }
            Cmd::Scan => {
                if !line_allowed(self.locked, "AT+SCAN") {
                    self.banner(locked_text(), true);
                } else {
                    self.scanning = true;
                    self.at
                        .wait
                        .push_back((T_SCAN, Zeroizing::new("AT+SCAN".into())));
                }
            }
            Cmd::Rename { name, label } => self.rename(&name, &label, now),
            Cmd::Sounds(on) => {
                self.sounds = on;
                if !on {
                    self.chirps.stop();
                }
                if let Err(err) = save_sounds(&self.s.data, on) {
                    self.note(format!("could not save the preferences: {err}"));
                }
            }
            Cmd::RemoveMine(d) => {
                self.engine.remove_my_device(d);
                let me = self.engine.me().0.to_string();
                self.engine.remove_contact_device(&me, d);
                if !self.removed.contains(&d) {
                    self.removed.push(d);
                }
                if let Err(err) = save_removed(&self.s.data, &self.removed) {
                    self.note(format!("could not save the removed devices: {err}"));
                }
            }
            Cmd::RemoveContact { name, key } => self.engine.remove_contact_device(&name, key),
            Cmd::Quit => {}
        }
    }

    fn enqueue_set(&mut self, line: String) {
        if !line_allowed(self.locked, &line) {
            self.banner(locked_text(), true);
            return;
        }
        if password_set(&line) {
            self.pw_dirty = true;
        }
        self.at.wait.push_back((T_SET, Zeroizing::new(line)));
        self.queue_refresh();
    }

    fn rename(&mut self, name: &str, label: &str, now: u32) {
        if self.engine.in_call() {
            self.note("hang up first to change the name");
            return;
        }
        let path = identity_path(&self.s.data);
        let mut id = match Identity::load(&path) {
            Ok(i) => i,
            Err(e) => {
                self.note(format!("identity: {e}"));
                return;
            }
        };
        if id.rename(name, label).is_none() {
            self.note(
                "not changed: a name is 1 to 16 printable, no ':'; a label 1 to 8, no ':' or '/'",
            );
            return;
        }
        if let Err(e) = id.save(&path) {
            self.note(format!("could not save the identity: {e}"));
            return;
        }
        if matches!(self.be, Backend::Sim { .. }) {
            self.engine.leave();
            self.pump(now);
            if let Backend::Sim { modem, .. } = &mut self.be {
                modem.voice_close(SESSION, now);
                self.vin.clear();
                modem.voice_open(SESSION, now);
                modem.voice_grant_settings(SESSION);
            }
        } else {
            self.pump(now);
        }
        self.engine = Engine::new(id, TrustStore::path_next_to(&path));
        for d in &self.removed {
            self.engine.remove_my_device(*d);
        }
        self.note(format!("now {name}/{label}"));
    }

    fn events(&mut self, now: u32) {
        let st = self.call_state();
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
        while let Some(ev) = self.engine.poll_event() {
            match ev {
                UiEvent::Scope {
                    t_ms,
                    step_ms: _,
                    samples,
                } => {
                    self.scope_seen = true;
                    self.scope_now = t_ms;
                    for (dbm, tx) in samples {
                        self.scope.push_back((dbm, tx));
                    }
                    while self.scope.len() > SCOPE_CAP {
                        self.scope.pop_front();
                    }
                }
                UiEvent::Air(a) => {
                    if let Some(duck) = self.chirp_for(&a, st, now) {
                        self.chirps.start(&a, if duck { DUCK } else { 1.0 });
                    }
                    self.air.push_back(AirSnap {
                        rx: a.rx,
                        sf: a.sf,
                        start_ms: a.start_ms,
                        air_ms: a.air_ms,
                    });
                    while self.air.len() > AIR_CAP {
                        self.air.pop_front();
                    }
                }
                UiEvent::AtReply { text, last } => {
                    if let Some((_, acc, _)) = self.at.cur.as_mut() {
                        acc.push_str(&text);
                    }
                    if last {
                        if let Some((tag, acc, _)) = self.at.cur.take() {
                            let ok = acc.lines().any(|l| l.trim() == "OK");
                            self.at_done(tag, acc, ok);
                        }
                    }
                }
                UiEvent::Error(e)
                    if e.starts_with("modem: message 0x0F") && self.at.cur.is_some() =>
                {
                    if let Some((tag, _, _)) = self.at.cur.take() {
                        self.at_done(tag, e, false);
                    }
                }
                UiEvent::LinkCode(c) => {
                    self.link_code = c;
                    self.link_note.clear();
                }
                UiEvent::Linked => {
                    if let Err(e) = self.engine.identity().save(&identity_path(&self.s.data)) {
                        self.note(format!("could not save the identity: {e}"));
                    }
                    self.link_code.clear();
                    self.link_note = "Linked. This device now carries your name.".into();
                    self.banner("LINKED", false);
                }
                UiEvent::Ringing {
                    name,
                    label,
                    station,
                } => {
                    if self.s.auto_answer {
                        self.engine.answer();
                    } else if !self.engine.in_call() {
                        self.ring_until = Some(now.wrapping_add(RING_HOLD_MS));
                        // Stash on the view fields via note locals. publish reads engine
                        // plus these strings, kept below.
                        self.who = format!("{name}/{label}");
                        // Reuse trust_line's neighbors: store ring in dedicated fields
                        // by writing them through a side channel. They live on Worker.
                        self.store_ring(&name, &label, &station);
                    }
                }
                UiEvent::Connected { plan, words, trust } => {
                    self.ring_until = None;
                    self.clear_ring();
                    self.words = [words[0].to_string(), words[1].to_string()];
                    self.plan = Some(plan);
                    self.plan_s = plan_line(self.engine.stats().sf, Some(plan));
                    self.conflict = trust == Trust::Conflict;
                    self.talk_anyway = false;
                    self.audio_ok = trust == Trust::Checked;
                    self.trust_line = trust_line(trust);
                    if self.s.auto_answer && trust != Trust::Conflict {
                        self.engine.talk_anyway();
                        self.talk_anyway = trust == Trust::NotChecked;
                        self.audio_ok = true;
                    }
                }
                UiEvent::PlanChanged(p) => {
                    let was = self.plan.map(|p| p.mode);
                    self.plan = Some(p);
                    self.plan_s = plan_line(self.engine.stats().sf, Some(p));
                    if was != Some(p.mode) {
                        self.banner(
                            if p.mode == TalkMode::Duplex {
                                "SWITCHED TO CALL"
                            } else {
                                "SWITCHED TO PUSH TO TALK"
                            },
                            false,
                        );
                    }
                }
                UiEvent::Floor(_) => {}
                UiEvent::AnsweredElsewhere => self.end_call("ANSWERED ELSEWHERE"),
                UiEvent::Declined => self.end_call("DECLINED"),
                UiEvent::NoAnswer => self.end_call("NO ANSWER"),
                UiEvent::Ended => self.end_call("CALL ENDED"),
                UiEvent::Directory(d) => {
                    self.dir = d.into_iter().take(DIR_CAP).map(dir_snap).collect();
                }
                UiEvent::Error(t) => {
                    let warn = t.starts_with("NOT THE");
                    if t.starts_with("link failed") {
                        self.link_code.clear();
                        self.link_note = t.clone();
                    }
                    self.banner(t, warn);
                }
            }
        }
        if self
            .ring_until
            .is_some_and(|t| now.wrapping_sub(t) < u32::MAX / 2)
        {
            self.ring_until = None;
            self.clear_ring();
        }
        let linking = self.engine.linking();
        if self.was_linking && !linking {
            self.link_code.clear();
        }
        self.was_linking = linking;
    }

    fn store_ring(&mut self, name: &str, label: &str, station: &str) {
        self.ring_name = name.to_string();
        self.ring_label = label.to_string();
        self.ring_station = station.to_string();
        self.ring_on = true;
    }

    fn clear_ring(&mut self) {
        self.ring_until = None;
        self.ring_on = false;
        self.ring_name.clear();
        self.ring_label.clear();
        self.ring_station.clear();
    }

    fn end_call(&mut self, why: &str) {
        let had = self.ring_on
            || self.engine.in_call()
            || !self.words[0].is_empty()
            || !self.who.is_empty();
        self.clear_ring();
        self.words = [String::new(), String::new()];
        self.plan = None;
        self.plan_s.clear();
        self.who.clear();
        self.trust_line.clear();
        self.audio_ok = false;
        self.talk_anyway = false;
        self.conflict = false;
        self.muted = false;
        if had {
            self.banner(why, false);
        }
        self.engine.refresh_directory();
    }

    fn audio_pump(&mut self, now: u32) {
        let in_call = self.engine.in_call();
        let due = u64::from(now.wrapping_sub(self.t0)) * 16;
        let behind = due.saturating_sub(self.spk_made);
        if behind > 16_000 {
            self.spk_made = due.saturating_sub(1_600);
        }
        let mut made = 0;
        while self.spk_made + CHUNK as u64 <= due && made < 4 {
            self.spk_made += CHUNK as u64;
            made += 1;
            let mic: Vec<i16> = {
                let mut q = self.audio.mic.lock().unwrap_or_else(|e| e.into_inner());
                let n = q.len().min(CHUNK * 4);
                q.drain(..n).collect()
            };
            if in_call && !mic.is_empty() {
                self.engine.mic(&mic);
            }
            let mut out = [0i16; CHUNK];
            self.engine.speaker(&mut out);
            if self.ring_until.is_some() && !in_call {
                ring_tone(&mut out, self.ring_pos);
                self.ring_pos += CHUNK as u64;
            } else {
                self.ring_pos = 0;
            }
            if self.chirps.playing() {
                self.chirps.mix(&mut out);
            }
            let mut q = self.audio.spk.lock().unwrap_or_else(|e| e.into_inner());
            let over = (q.len() + CHUNK).saturating_sub(SPK_AHEAD);
            let over = over.min(q.len());
            q.drain(..over);
            q.extend(out);
        }
    }

    fn at_next(&mut self, now: u32) {
        if let Some((tag, _, t)) = &self.at.cur {
            if now.wrapping_sub(*t) > AT_TIMEOUT_MS {
                let tag = *tag;
                self.at.cur = None;
                self.at_done(tag, "no answer from the modem".into(), false);
            }
            return;
        }
        let cap = match (self.cap_old, self.cap) {
            (Some(old), Some(t)) => Some((old, t.index())),
            _ => None,
        };
        let Some((tag, line, ends)) = take_next(&mut self.at.wait, cap, &mut self.save_armed)
        else {
            return;
        };
        if let Some(note) = ends {
            self.cap_end_tag = Some((tag, note));
        }
        if matches!(self.be, Backend::Gone) {
            self.at_done(tag, "no modem (MODEM LOST)".into(), false);
        } else if self.engine.at_line(&line) {
            self.at.cur = Some((tag, String::new(), now));
            if tag == TAG_CAP_RESTORE {
                self.restore_sent = Some(now);
            }
        } else {
            self.at_done(tag, "line too long".into(), false);
        }
    }

    fn at_done(&mut self, tag: u32, text: String, ok: bool) {
        match tag {
            TAG_CAP_READ => {
                let old = if ok { parse_s21(&text) } else { None };
                self.cap_old = old;
                match (old, self.cap) {
                    (None, _) => self.note("tier cap: could not read S21, so the cap is not set"),
                    (Some(_), Some(t)) => {
                        let line = format!("ATS21={}", t.index());
                        self.at.wait.push_front((TAG_CAP_SET, Zeroizing::new(line)));
                    }
                    (Some(_), None) => {}
                }
            }
            TAG_CAP_UNSET => {
                if !ok {
                    self.save_armed = false;
                    if let Some(t) = take_save_behind(&mut self.at.wait) {
                        self.at_done(
                            t,
                            format!(
                                "not saved: S21 could not be put back before AT&W ({})",
                                text.trim()
                            ),
                            false,
                        );
                    }
                }
            }
            TAG_CAP_SET => {
                if !ok {
                    self.cap_old = None;
                    self.note(format!("tier cap: the modem refused S21: {}", text.trim()));
                }
            }
            TAG_CAP_RESTORE => {
                self.cap_old = None;
                if !ok {
                    self.note(format!("could not put S21 back: {}", text.trim()));
                }
            }
            _ => {
                if let Some((t, note)) = self.cap_end_tag {
                    if t == tag {
                        self.cap_end_tag = None;
                        if ok && self.cap_old.take().is_some() && note {
                            self.note(
                                "tier cap ended: S21 was set in Settings and stays after you leave",
                            );
                        }
                    }
                }
                self.take_reply(tag, text, ok);
            }
        }
    }

    fn take_reply(&mut self, tag: u32, text: String, ok: bool) {
        let body = body_of(&text);
        match tag {
            T_ATV if ok => self.atv = body,
            T_FWD if ok => self.fwd = body,
            T_WAP if ok => self.wap = body,
            T_WSTA if ok => {
                self.wsta = body;
                if self.rebase {
                    self.rebase = false;
                    self.baseline = Some(self.fingerprint());
                    self.pw_dirty = false;
                }
            }
            T_ATI if ok => self.ati = body,
            T_WIFI => self.wifi = body,
            T_SCAN => {
                self.scanning = false;
                self.scan = body
                    .lines()
                    .filter(|l| l.starts_with("SCAN "))
                    .take(SCAN_CAP)
                    .map(str::to_string)
                    .collect();
                self.note(if ok {
                    format!("scan: {} answered", self.scan.len())
                } else {
                    format!("scan: {}", body.lines().last().unwrap_or("ERROR"))
                });
                self.engine.refresh_directory();
            }
            T_DIR => {
                self.note(if ok {
                    "directory changed".into()
                } else {
                    format!("directory: {body}")
                });
                self.engine.refresh_directory();
            }
            T_SET => {
                self.engine.refresh_directory();
                if !ok {
                    let last = body.lines().last().unwrap_or("ERROR").to_string();
                    self.note(format!("refused: {last}"));
                    self.banner(format!("THE MODEM SAID {last}"), false);
                }
            }
            T_SAVE => {
                self.rebase = ok;
                self.note(if ok {
                    "saved (AT&W)".into()
                } else {
                    format!("save: {body}")
                });
            }
            T_DISCARD => {
                self.rebase = ok;
                self.note(if ok {
                    "back to the saved settings (ATZ)".into()
                } else {
                    format!("discard: {body}")
                });
            }
            _ => {}
        }
    }

    fn wifi_poll(&mut self, now: u32) {
        if now.wrapping_sub(self.wifi_due) >= u32::MAX / 2 {
            return;
        }
        self.wifi_due = now.wrapping_add(2_000);
        let busy = self.at.cur.is_some() || self.at.wait.iter().any(|(t, _)| *t == T_WIFI);
        if !busy && !matches!(self.be, Backend::Gone) {
            self.at
                .wait
                .push_back((T_WIFI, Zeroizing::new("AT+WIFI?".into())));
        }
    }

    fn fingerprint(&self) -> String {
        let keep = ["E", "S00", "TX POWER", "WIFI AP", "PUB ", "FWD", "BEACON"];
        let mut s: String = self
            .atv
            .lines()
            .map(str::trim)
            .filter(|l| keep.iter().any(|k| l.starts_with(k)))
            .collect::<Vec<_>>()
            .join("\n");
        s.push('\n');
        s.push_str(self.fwd.trim());
        s.push('\n');
        s.push_str(self.wap.trim());
        s
    }

    fn unsaved(&self) -> bool {
        if self.locked {
            return false;
        }
        let cap = self.cap_old.is_some();
        self.pw_dirty
            || self.baseline.as_ref().is_some_and(|b| {
                let now = self.fingerprint();
                if cap {
                    without_s21(b) != without_s21(&now)
                } else {
                    *b != now
                }
            })
    }

    fn publish(&mut self) {
        let (name, label) = self.engine.me();
        let id = self.engine.identity();
        let stats = self.engine.stats();
        let peer = self.engine.peer();
        let contacts: Vec<ContactSnap> = self
            .engine
            .contacts()
            .into_iter()
            .take(CONTACT_CAP)
            .map(|c| ContactSnap {
                name: c.name,
                devices: c.devices,
                person: c.person,
                removed: c.removed,
            })
            .collect();
        let rssi = self
            .scope
            .back()
            .map(|(d, _)| i32::from(*d))
            .unwrap_or(-999);
        let ptt = self.plan.is_some_and(|p| p.mode != TalkMode::Duplex);
        let floor = match self.engine.floor() {
            Floor::Free => 0,
            Floor::Mine => 1,
            Floor::Theirs => 2,
        };
        let v = View {
            name: name.to_string(),
            label: label.to_string(),
            station: {
                let st = self.engine.station();
                if st.is_empty() {
                    self.s.station.clone()
                } else {
                    st.to_string()
                }
            },
            link: self.link_desc.clone(),
            bind: self.bind_shown.clone(),
            peer: self.s.peer.to_string(),
            banner: self.banner.clone(),
            banner_warn: self.banner_warn,
            banner_at: self.banner_at,
            note: self.note.clone(),
            words: self.words.clone(),
            plan: if self.plan_s.is_empty() {
                plan_line(stats.sf, self.plan)
            } else {
                self.plan_s.clone()
            },
            who: self.who.clone(),
            peer_name: peer.as_ref().map(|p| p.0.clone()).unwrap_or_default(),
            peer_label: peer.as_ref().map(|p| p.1.clone()).unwrap_or_default(),
            peer_station: peer.as_ref().map(|p| p.2.clone()).unwrap_or_default(),
            trust_line: self.trust_line.clone(),
            link_code: self.link_code.clone(),
            link_note: self.link_note.clone(),
            device_key: id.device_pub(),
            has_person: id.person_pub().is_some(),
            in_call: self.engine.in_call(),
            ringing: self.ring_on,
            ring_name: self.ring_name.clone(),
            ring_label: self.ring_label.clone(),
            ring_station: self.ring_station.clone(),
            audio_ok: self.audio_ok,
            talk_anyway: self.talk_anyway,
            conflict: self.conflict,
            muted: self.muted,
            ptt,
            floor,
            call_state: i32::from(self.call_state()),
            sf: i32::from(stats.sf),
            locked: self.locked,
            sounds: self.sounds,
            linking: self.engine.linking(),
            cap_active: self.cap_old.is_some(),
            unsaved: self.unsaved(),
            pw_dirty: self.pw_dirty,
            lost: self.lost,
            ready: true,
            connecting: false,
            scanning: self.scanning,
            rssi,
            scope_now: self.scope_now,
            dir: self.dir.clone(),
            contacts,
            scope: self.scope.iter().copied().collect(),
            air: self.air.iter().copied().collect(),
            scan: self.scan.clone(),
            atv: self.atv.clone(),
            ati: self.ati.clone(),
            fwd: self.fwd.clone(),
            wap: self.wap.clone(),
            wsta: self.wsta.clone(),
            wifi: self.wifi.clone(),
        };
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }
}

fn dir_snap(d: DirLine) -> DirSnap {
    let voice = !d.label.is_empty() || d.station != d.name;
    DirSnap {
        name: d.name,
        label: d.label,
        station: d.station,
        hint: mode_name(d.mode_hint).to_string(),
        sf: d.best_tier.map(|t| i32::from(t.sf())).unwrap_or(0),
        pinned: d.pinned,
        trust: match d.trust {
            Trust::NotChecked => 0,
            Trust::Checked => 1,
            Trust::Conflict => 2,
        },
        voice,
    }
}

fn trust_line(t: Trust) -> String {
    match t {
        Trust::Checked => "CHECKED".into(),
        Trust::NotChecked => "NOT CHECKED".into(),
        Trust::Conflict => "NOT THE NAME YOU CHECKED".into(),
    }
}

fn plan_line(sf: u8, plan: Option<TalkPlan>) -> String {
    match (tier_from_sf(sf), plan) {
        (Some(t), Some(p)) => format!(
            "{} {}   {}   {}",
            t.name().trim_start_matches("LORA "),
            t.air_bps(),
            codec_name(p.codec),
            mode_name(p.mode)
        ),
        (Some(t), None) => format!("{} {}", t.name().trim_start_matches("LORA "), t.air_bps()),
        _ => {
            if sf == 0 {
                "TRAINING".into()
            } else {
                format!("SF{sf}")
            }
        }
    }
}

fn body_of(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|l| !l.trim().is_empty() && l.trim() != "OK")
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_allowed(locked: bool, line: &str) -> bool {
    if !locked {
        return true;
    }
    match lfd3k_core::parse_line(line.trim()) {
        Ok(cmd) => lfd3k_core::voice::host::at_read_only(&cmd),
        Err(_) => false,
    }
}

fn locked_text() -> &'static str {
    "Settings are locked over Wi-Fi. Plug the modem in by USB to change them."
}

fn without_s21(fp: &str) -> String {
    fp.lines()
        .map(|l| {
            if l.trim_start().starts_with("S00") {
                l.split(' ')
                    .map(|w| if w.starts_with("S21=") { "S21=" } else { w })
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn password_set(line: &str) -> bool {
    let u = line.trim().to_ascii_uppercase();
    (u.starts_with("AT+WAP=") || u.starts_with("AT+WSTA=")) && line.contains(',')
}

fn is_save_line(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("AT&W")
}

fn is_reset_line(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("ATZ")
}

fn is_s21_set_line(line: &str) -> bool {
    matches!(
        lfd3k_core::parse_line(line.trim()),
        Ok(lfd3k_core::AtCmd::SetReg(21, _))
    )
}

fn take_next(
    wait: &mut VecDeque<(u32, Zeroizing<String>)>,
    cap: Option<(u16, u8)>,
    armed: &mut bool,
) -> Option<(u32, Zeroizing<String>, Option<bool>)> {
    let (tag, line) = wait.pop_front()?;
    let window = tag < TAG_CAP_READ;
    if window && is_save_line(&line) && core::mem::take(armed) {
        return Some((tag, line, None));
    }
    match cap {
        Some((old, cap)) if window && is_save_line(&line) => {
            wait.push_front((TAG_CAP_SET, Zeroizing::new(format!("ATS21={cap}"))));
            wait.push_front((tag, line));
            *armed = true;
            Some((TAG_CAP_UNSET, Zeroizing::new(format!("ATS21={old}")), None))
        }
        Some(_) if window && is_reset_line(&line) => Some((tag, line, Some(false))),
        Some(_) if window && is_s21_set_line(&line) => Some((tag, line, Some(true))),
        _ => Some((tag, line, None)),
    }
}

fn take_save_behind(wait: &mut VecDeque<(u32, Zeroizing<String>)>) -> Option<u32> {
    match wait.front() {
        Some(&(t, _)) if t < TAG_CAP_READ => {
            wait.pop_front();
            Some(t)
        }
        _ => None,
    }
}

fn parse_s21(text: &str) -> Option<u16> {
    let at = text.find("S21=")?;
    let digits: String = text
        .get(at + 4..)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn chirp_rule(
    ftype: u8,
    st: u8,
    setting_up: bool,
    sound_until: &mut Option<u32>,
    retrain_until: &mut Option<u32>,
    now: u32,
) -> Option<bool> {
    if END_TYPES.contains(&ftype) {
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

fn before(until: Option<u32>, now: u32) -> bool {
    until.is_some_and(|t| t.wrapping_sub(now) < u32::MAX / 2 && t != now)
}

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

fn scrubbed(s: &Settings) -> Settings {
    let mut c = s.clone();
    let ssid = c.ap_ssid().to_string();
    let _ = c.set_ap(&ssid, AP_PLACEHOLDER);
    c.clear_sta();
    c
}

fn load_sounds(data: &Path) -> bool {
    let t = std::fs::read_to_string(data.join("app-prefs.txt")).unwrap_or_default();
    !t.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("connection_sounds=off"))
}

fn save_sounds(data: &Path, on: bool) -> std::io::Result<()> {
    let t = format!("connection_sounds={}\n", if on { "on" } else { "off" });
    write_file(&data.join("app-prefs.txt"), t.as_bytes())
}

fn hex_key(k: &[u8; 32]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in k {
        s.push(char::from(D[usize::from(b >> 4)]));
        s.push(char::from(D[usize::from(b & 15)]));
    }
    s
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

fn load_removed(data: &Path) -> Vec<[u8; 32]> {
    let t = std::fs::read_to_string(data.join("removed.json")).unwrap_or_default();
    t.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(unhex)
        .collect()
}

fn save_removed(data: &Path, v: &[[u8; 32]]) -> std::io::Result<()> {
    let items: Vec<String> = v.iter().map(|k| format!("  \"{}\"", hex_key(k))).collect();
    let t = format!("[\n{}\n]\n", items.join(",\n"));
    write_file(&data.join("removed.json"), t.as_bytes())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_save_wraps_s21() {
        let mut w = VecDeque::new();
        w.push_back((T_SAVE, Zeroizing::new("AT&W".into())));
        let mut armed = false;
        let (tag, line, _) = take_next(&mut w, Some((7, 3)), &mut armed).unwrap();
        assert_eq!(tag, TAG_CAP_UNSET);
        assert_eq!(line.as_str(), "ATS21=7");
        assert!(armed);
        let (tag, line, _) = take_next(&mut w, Some((7, 3)), &mut armed).unwrap();
        assert_eq!(tag, T_SAVE);
        assert_eq!(line.as_str(), "AT&W");
    }

    #[test]
    fn locked_allows_reads_and_scan_only() {
        assert!(line_allowed(true, "AT&V"));
        assert!(line_allowed(true, "AT+SCAN"));
        assert!(line_allowed(true, "AT+FWD?"));
        assert!(!line_allowed(true, "ATS30=5"));
        assert!(!line_allowed(true, "AT&W"));
        assert!(!line_allowed(true, "AT+DIR=+REV"));
        assert!(line_allowed(false, "ATS30=5"));
    }

    #[test]
    fn sim_session_answers_ati() {
        let dir = std::env::temp_dir().join(format!(
            "lfd3k-android-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = Session::start(Start {
            transport: Transport::Sim,
            name: "REV".into(),
            label: "PH".into(),
            data: dir.clone(),
            bind: "127.0.0.1:0".into(),
            peer: "127.0.0.1:9".parse().unwrap(),
            station: "PHONEMODEM".into(),
            snr_db: 8,
            cap_sf: None,
            sounds: Some(false),
            auto_answer: false,
        })
        .expect("start");
        let mut ati = String::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(50));
            ati = s.text(1);
            if !ati.is_empty() {
                break;
            }
            let v = s.view();
            if v.lost {
                panic!("modem lost: {}", v.note);
            }
        }
        assert!(
            ati.contains("PHONEMODEM") || ati.contains("LFD3K") || !ati.is_empty(),
            "ATI was {ati:?}"
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
