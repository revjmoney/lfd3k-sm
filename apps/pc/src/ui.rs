//! The window: the rotatocam.com theme (the site's `css/site.css`
//! tokens: graphite surfaces, 1 px hairlines, no shadows, 5 px radius on
//! buttons only, `#7aa2f7` accent, `--warn` amber only for warnings),
//! Archivo and IBM Plex Mono, the Simian logo, and every screen laid out
//! in fixed rects so it fits 1280 x 720 with no scrolling (lists page).
//!
//! Screens: CALLS (the directory with CALL or PTT hints, call by name, the
//! call panel, the ring screen), DIRECTORY (dial, pin, delete, scan) and
//! SETTINGS (tabs Radio, Privacy, Wi-Fi, Forwards, Voice and keys, About).
//! Every modem setting goes out as one AT line, composed as the AT+MENU
//! composes it, through the voice host protocol's AtLine.
//!
//! `--script <file> --shots <dir>` drives the window from a file (a test
//! helper, see `docs/voice-pc.md`): each action goes through the same code
//! as the button it stands for.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{
    self, epaint, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id,
    Key, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, TextureHandle, Ui, UiBuilder,
    Vec2,
};
use lfd3k_core::voice::{Floor, TalkMode, TalkPlan};
use lfd3k_core::Tier;
use lfd3k_voice::{codec_name, mode_name, tier_from_sf, DirLine, Trust, UiEvent};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::Audio;
use crate::modem_sim::{Cmd, Ev, Snap};
use crate::monitor::ScopeView;
use crate::Args;

// ---- Theme (site.css tokens).

const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const BG_FOOT: Color32 = Color32::from_rgb(0x0b, 0x0c, 0x0e);
pub(crate) const SURFACE: Color32 = Color32::from_rgb(0x16, 0x18, 0x1b);
const SURFACE2: Color32 = Color32::from_rgb(0x14, 0x17, 0x1c);
const ZEBRA: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub(crate) const RULE: Color32 = Color32::from_rgb(0x24, 0x27, 0x2c);
pub(crate) const RULE_WARM: Color32 = Color32::from_rgb(0x2e, 0x34, 0x40);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xea, 0xed);
pub(crate) const TEXT2: Color32 = Color32::from_rgb(0xb6, 0xbb, 0xc3);
pub(crate) const TEXT3: Color32 = Color32::from_rgb(0xa8, 0xad, 0xb5);
pub(crate) const MUTED: Color32 = Color32::from_rgb(0x8a, 0x90, 0x99);
const DIM: Color32 = Color32::from_rgb(0x6b, 0x70, 0x79);
pub(crate) const FAINT: Color32 = Color32::from_rgb(0x4c, 0x50, 0x58);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xf7);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0x9d, 0xb8, 0xff);
const WARN: Color32 = Color32::from_rgb(0xd9, 0xa4, 0x41);
const OK: Color32 = Color32::from_rgb(0x8b, 0xbf, 0x5a);

/// Rev's tagline (About and the README voice line).
const TAGLINE_1: &str = "Your own encrypted radio (cell) phone network!!";
const TAGLINE_2: &str = "Call anyone with a modem. No carrier. No tower. No one listening.";
/// NOTICE.md, additional term 2.
const ATTRIBUTION: &str = "Based on LFD3K-SM by Rev. J. Money / jmscnc.com";
const PRODUCT: &str = "LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM";

/// Height of the bottom band (modem state, attribution, signal scope) on
/// every screen. With the Save bar and a banner the screens above it get
/// 720 - 64 - 44 - 38 - 12 - 132 = 430 points.
const MON_H: f32 = 132.0;

/// S30 at or below this is the safe default range (FEATURES.md: default 2).
const S30_SAFE: i32 = 2;

static LOGO_PNG: &[u8] = include_bytes!("../../../docs/img/simian-logo.png");

fn sans(s: f32) -> FontId {
    FontId::new(s, FontFamily::Proportional)
}
fn heavy(s: f32) -> FontId {
    FontId::new(s, FontFamily::Name("heavy".into()))
}
pub(crate) fn mono(s: f32) -> FontId {
    FontId::new(s, FontFamily::Monospace)
}
pub(crate) fn monob(s: f32) -> FontId {
    FontId::new(s, FontFamily::Name("monob".into()))
}

/// Archivo (variable: regular and heavy instances) and IBM Plex Mono.
/// egui's own fonts are not built in.
fn fonts() -> FontDefinitions {
    use epaint::text::{FontTweak, VariationCoords};
    let arch: &'static [u8] = include_bytes!("../assets/fonts/Archivo-Variable.ttf");
    let plex: &'static [u8] = include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf");
    let plexb: &'static [u8] = include_bytes!("../assets/fonts/IBMPlexMono-SemiBold.ttf");
    let w = |wght: f32| FontTweak {
        coords: VariationCoords::new([(b"wght", wght)]),
        ..FontTweak::default()
    };
    let mut d = FontDefinitions::empty();
    d.font_data.insert(
        "archivo".into(),
        Arc::new(FontData::from_static(arch).tweak(w(450.0))),
    );
    d.font_data.insert(
        "archivo-heavy".into(),
        Arc::new(FontData::from_static(arch).tweak(w(820.0))),
    );
    d.font_data
        .insert("plex".into(), Arc::new(FontData::from_static(plex)));
    d.font_data.insert(
        "plex-semibold".into(),
        Arc::new(FontData::from_static(plexb)),
    );
    d.families.insert(
        FontFamily::Proportional,
        vec!["archivo".into(), "plex".into()],
    );
    d.families
        .insert(FontFamily::Monospace, vec!["plex".into(), "archivo".into()]);
    d.families.insert(
        FontFamily::Name("heavy".into()),
        vec!["archivo-heavy".into(), "plex-semibold".into()],
    );
    d.families.insert(
        FontFamily::Name("monob".into()),
        vec!["plex-semibold".into(), "archivo-heavy".into()],
    );
    d
}

fn decode_logo() -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(LOGO_PNG));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let px = buf.get(..info.buffer_size())?;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((w, h, rgba))
}

/// The window icon from the Simian logo.
pub fn logo_icon() -> Option<egui::IconData> {
    let (w, h, rgba) = decode_logo()?;
    Some(egui::IconData {
        rgba,
        width: w,
        height: h,
    })
}

/// Fonts and the theme.
pub fn setup(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.all_styles_mut(|s| {
        use egui::TextStyle;
        s.text_styles = [
            (TextStyle::Small, sans(12.0)),
            (TextStyle::Body, sans(15.0)),
            (TextStyle::Button, heavy(15.0)),
            (TextStyle::Heading, heavy(22.0)),
            (TextStyle::Monospace, mono(14.0)),
        ]
        .into();
        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = BG_FOOT;
        v.faint_bg_color = ZEBRA;
        v.window_stroke = Stroke::new(1.0, RULE);
        v.window_shadow = epaint::Shadow::NONE;
        v.popup_shadow = epaint::Shadow::NONE;
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARN;
        v.selection.bg_fill = Color32::from_rgba_unmultiplied(0x7a, 0xa2, 0xf7, 90);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        for (w, fill, stroke, fg) in [
            (&mut v.widgets.noninteractive, SURFACE, RULE, TEXT2),
            (&mut v.widgets.inactive, SURFACE2, RULE, TEXT),
            (&mut v.widgets.hovered, SURFACE, ACCENT_HOVER, TEXT),
            (&mut v.widgets.active, SURFACE, ACCENT, TEXT),
            (&mut v.widgets.open, SURFACE, ACCENT, TEXT),
        ] {
            // Square everything here; buttons get their 5 px in `button`.
            w.corner_radius = CornerRadius::ZERO;
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = Stroke::new(1.0, stroke);
            w.fg_stroke = Stroke::new(1.0, fg);
            w.expansion = 0.0;
        }
        s.spacing.button_padding = Vec2::new(10.0, 4.0);
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
    });
}

// ---- Drawing helpers (every rect placed by hand: nothing scrolls).

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Accent fill.
    Primary,
    /// Surface with a hairline.
    Plain,
    /// Amber outline: only where the action is a warning.
    Warn,
    /// Tab or toggle that is on.
    On,
}

fn button_font(r: Rect) -> FontId {
    if r.height() >= 70.0 {
        heavy(28.0)
    } else if r.height() >= 46.0 {
        heavy(19.0)
    } else {
        heavy(14.0)
    }
}

/// A button exactly filling `r` (the only widgets with a radius: 5 px).
fn button(ui: &mut Ui, r: Rect, text: &str, k: Kind, enabled: bool) -> egui::Response {
    let (fill, fg, stroke) = match k {
        Kind::Primary => (ACCENT, BG, Stroke::new(1.0, ACCENT)),
        Kind::Plain => (SURFACE2, TEXT, Stroke::new(1.0, RULE_WARM)),
        Kind::Warn => (SURFACE2, WARN, Stroke::new(1.0, WARN)),
        Kind::On => (SURFACE, ACCENT, Stroke::new(1.0, ACCENT)),
    };
    let fg = if enabled { fg } else { FAINT };
    let fill = if enabled { fill } else { SURFACE2 };
    let b = egui::Button::new(RichText::new(text).font(button_font(r)).color(fg))
        .wrap_mode(egui::TextWrapMode::Truncate)
        .fill(fill)
        .stroke(if enabled {
            stroke
        } else {
            Stroke::new(1.0, RULE)
        })
        .corner_radius(5)
        .min_size(r.size());
    ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
        ui.add_enabled(enabled, b)
    })
    .inner
}

pub(crate) fn text(ui: &Ui, pos: Pos2, a: Align2, s: &str, f: FontId, c: Color32) -> Rect {
    ui.painter().text(pos, a, s, f, c)
}

/// Text cut to fit `w` (no wrap, no overflow).
fn text_fit(ui: &Ui, pos: Pos2, a: Align2, s: &str, f: FontId, c: Color32, w: f32) {
    let mut t = s.to_string();
    loop {
        let g = ui.painter().layout_no_wrap(t.clone(), f.clone(), c);
        if g.size().x <= w || t.is_empty() {
            break;
        }
        t.pop();
        while t.ends_with(' ') {
            t.pop();
        }
        if t.len() > 1 {
            t.pop();
            t.push('.');
        }
    }
    ui.painter().text(pos, a, t, f, c);
}

/// A scan answer (`SCAN <station> SNR <x> dB <tier> [VOICE <names>]`) as
/// two lines: the station and its SNR, then the tier and voice names.
fn scan_two_lines(l: &str) -> (String, String) {
    let l = l.strip_prefix("SCAN ").unwrap_or(l);
    match l.find(" dB ") {
        Some(i) => (
            l.get(..i + 3).unwrap_or(l).to_string(),
            l.get(i + 4..).unwrap_or("").to_string(),
        ),
        None => (l.to_string(), String::new()),
    }
}

/// Wrapped text inside `r`; lines past the bottom are not drawn.
fn text_wrap(ui: &Ui, r: Rect, s: &str, f: FontId, c: Color32) {
    let g = ui.painter().layout(s.to_string(), f, c, r.width());
    if g.size().y <= r.height() + 1.0 {
        ui.painter().galley(r.min, g, c);
    } else {
        ui.painter().with_clip_rect(r).galley(r.min, g, c);
    }
}

/// A square panel: surface plus a 1 px hairline.
fn panel(ui: &Ui, r: Rect) {
    ui.painter().rect_filled(r, CornerRadius::ZERO, SURFACE);
    ui.painter().rect_stroke(
        r,
        CornerRadius::ZERO,
        Stroke::new(1.0, RULE),
        StrokeKind::Inside,
    );
}

fn kicker(ui: &Ui, pos: Pos2, s: &str) {
    text(ui, pos, Align2::LEFT_TOP, s, mono(11.5), ACCENT);
}

fn hline(ui: &Ui, y: f32, x0: f32, x1: f32) {
    ui.painter()
        .line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], Stroke::new(1.0, RULE));
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
}

/// A one line text field in `r`.
fn field(
    ui: &mut Ui,
    r: Rect,
    s: &mut String,
    id: &str,
    password: bool,
    hint: &str,
    limit: usize,
) -> egui::Response {
    let te = TextEdit::singleline(s)
        .id(Id::unique(id))
        .password(password)
        .hint_text(RichText::new(hint).color(FAINT))
        .font(mono(15.0))
        .char_limit(limit)
        .margin(Vec2::new(8.0, 7.0))
        .align(Align2::LEFT_CENTER)
        .desired_width(r.width() - 16.0)
        .min_size(r.size());
    ui.put(r, te)
}

/// Forget a text field's undo history (after a password went out).
fn forget_field(ctx: &egui::Context, id: &str) {
    if let Some(mut st) = egui::text_edit::TextEditState::load(ctx, Id::unique(id)) {
        st.clear_undoer();
        st.store(ctx, Id::unique(id));
    }
}

/// `LORA SF7 BW500` -> `SF7 BW500 21875`.
fn tier_line(t: Tier) -> String {
    format!("{} {}", t.name().trim_start_matches("LORA "), t.air_bps())
}

fn plan_line(sf: u8, plan: Option<TalkPlan>) -> String {
    match (tier_from_sf(sf), plan) {
        (Some(t), Some(p)) => format!(
            "{}   {}   {}",
            tier_line(t),
            codec_name(p.codec),
            mode_name(p.mode)
        ),
        (Some(t), None) => tier_line(t),
        _ => "TRAINING".into(),
    }
}

// ---- Window state.

#[derive(Clone, Copy, PartialEq, Debug)]
enum Screen {
    Calls,
    Dir,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Radio,
    Privacy,
    Wifi,
    Forwards,
    Voice,
    About,
}

const TABS: [(Tab, &str); 6] = [
    (Tab::Radio, "RADIO"),
    (Tab::Privacy, "PRIVACY"),
    (Tab::Wifi, "WI-FI"),
    (Tab::Forwards, "FORWARDS"),
    (Tab::Voice, "VOICE AND KEYS"),
    (Tab::About, "ABOUT"),
];

/// AtLine tags: which reply is which.
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

/// The modem as the AT readouts show it.
#[derive(Default)]
struct ModemView {
    atv: String,
    ati: String,
    fwd: String,
    wap: String,
    wsta: String,
    wifi: String,
    scan: Vec<String>,
    scanning: bool,
}

impl ModemView {
    fn sreg(&self, n: u8) -> Option<i32> {
        let want = format!("S{n:02}=");
        self.atv
            .split_whitespace()
            .find_map(|t| t.strip_prefix(&want).and_then(|v| v.parse().ok()))
    }
    fn line(&self, start: &str) -> Option<&str> {
        self.atv
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with(start))
    }
    fn name(&self) -> String {
        self.atv
            .lines()
            .next()
            .and_then(|l| l.split_once(" NAME "))
            .map_or(String::new(), |(_, n)| n.trim().to_string())
    }
    fn beacon(&self) -> String {
        self.line("BEACON ").map_or(String::new(), |l| {
            l.trim_start_matches("BEACON ").to_string()
        })
    }
    /// WIFI AP <ON|OFF> <ssid>  STA <ssid|OFF>
    fn wifi_ap(&self) -> (bool, String, String) {
        let Some(l) = self.line("WIFI AP ") else {
            return (false, String::new(), String::new());
        };
        let rest = l.trim_start_matches("WIFI AP ");
        let (ap, sta) = rest.split_once("  STA ").unwrap_or((rest, ""));
        let (onoff, ssid) = ap.split_once(' ').unwrap_or((ap, ""));
        (onoff == "ON", ssid.to_string(), sta.to_string())
    }
    fn published(&self) -> String {
        self.line("PUB ")
            .map_or(String::new(), |l| l.trim_start_matches("PUB ").to_string())
    }
    /// Forwards from AT+FWD?: name, host, port, mode.
    fn forwards(&self) -> Vec<(String, String, String, String)> {
        self.fwd
            .lines()
            .filter_map(|l| {
                let l = l.trim().strip_prefix("FWD ")?;
                let mut p = l.split_whitespace();
                let name = p.next()?.to_string();
                let hp = p.next()?;
                let mode = p.next().unwrap_or("").to_string();
                let (h, port) = hp.rsplit_once(':')?;
                Some((name, h.to_string(), port.to_string(), mode))
            })
            .collect()
    }
    /// What Save and Discard compare: the settings lines of AT&V (not the
    /// counters that change by themselves), the forwards and AT+WAP?.
    fn fingerprint(&self) -> String {
        let keep = ["E", "S00", "TX POWER", "WIFI AP", "PUB", "FWD", "BEACON"];
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
}

/// The call as the window shows it.
#[derive(Default)]
struct CallView {
    /// Who we called or who called us (the name dialed or rung).
    who: String,
    placed: bool,
    words: Option<[&'static str; 2]>,
    trust: Option<Trust>,
    /// Checked, WORDS MATCH or TALK ANYWAY: audio flows.
    audio_ok: bool,
    talk_anyway: bool,
    plan: Option<TalkPlan>,
    switched: Option<String>,
}

struct Banner {
    text: String,
    warn: bool,
    at: Instant,
}

/// One script step.
struct Script {
    lines: VecDeque<String>,
    wait_until: Option<Instant>,
    until: Option<(String, Instant)>,
    shots: PathBuf,
    shot_pending: Arc<AtomicUsize>,
    t0: Instant,
    /// The go mark of `sync` (Unix ms), for `at_t`.
    go: Option<u64>,
}

pub struct App {
    tx: Sender<Cmd>,
    rx: Receiver<Ev>,
    snap_src: Arc<Mutex<Snap>>,
    s: Snap,
    args: Args,
    audio: Audio,
    logo: Option<TextureHandle>,
    screen: Screen,
    tab: Tab,
    dir: Vec<DirLine>,
    dir_page: usize,
    calls_page: usize,
    sel: Option<String>,
    dial_name: String,
    dial_label: String,
    ringing: Option<(String, String, String)>,
    call: CallView,
    muted: bool,
    talk_sent: bool,
    script_talk: bool,
    banner: Option<Banner>,
    note: String,
    link_code: Option<Zeroizing<String>>,
    join_code: Zeroizing<String>,
    link_note: String,
    m: ModemView,
    baseline: Option<String>,
    rebase: bool,
    pw_dirty: bool,
    at_open: usize,
    wifi_poll: Option<Instant>,
    // Settings fields.
    st_name: String,
    s_edit: [String; 33],
    ap_ssid: String,
    ap_pass: Zeroizing<String>,
    sta_ssid: String,
    sta_pass: Zeroizing<String>,
    fwd_name: String,
    fwd_host: String,
    fwd_port: String,
    fwd_mode_t: bool,
    pin_name: String,
    pin_voice: bool,
    my_name: String,
    my_label: String,
    contacts_page: usize,
    scan_page: usize,
    shown_me: (String, String),
    pins_page: usize,
    script: Option<Script>,
    stopping: bool,
    stopped: bool,
    fake_dir: bool,
    /// The modem signal scope on every screen.
    scope: ScopeView,
    /// Connection sounds on (Settings, About).
    sounds: bool,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        args: Args,
        audio: Audio,
        tx: Sender<Cmd>,
        rx: Receiver<Ev>,
        snap_src: Arc<Mutex<Snap>>,
    ) -> App {
        let logo = decode_logo().map(|(w, h, rgba)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            ctx.load_texture("simian-logo", img, egui::TextureOptions::LINEAR)
        });
        let script = args
            .script
            .as_ref()
            .and_then(|p| match std::fs::read_to_string(p) {
                Ok(t) => Some(Script {
                    lines: t
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(String::from)
                        .collect(),
                    wait_until: None,
                    until: None,
                    shots: args.shots.clone().unwrap_or_else(|| PathBuf::from("shots")),
                    shot_pending: Arc::new(AtomicUsize::new(0)),
                    t0: Instant::now(),
                    go: None,
                }),
                Err(e) => {
                    eprintln!("{}: {e}", p.display());
                    None
                }
            });
        if let Some(sc) = &script {
            let _ = std::fs::create_dir_all(&sc.shots);
        }
        let mut a = App {
            tx,
            rx,
            snap_src,
            s: Snap::default(),
            my_name: args.name.clone(),
            my_label: args.label.clone(),
            args,
            audio,
            logo,
            screen: Screen::Calls,
            tab: Tab::Radio,
            dir: Vec::new(),
            dir_page: 0,
            calls_page: 0,
            sel: None,
            dial_name: String::new(),
            dial_label: String::new(),
            ringing: None,
            call: CallView::default(),
            muted: false,
            talk_sent: false,
            script_talk: false,
            banner: None,
            note: String::new(),
            link_code: None,
            join_code: Zeroizing::new(String::new()),
            link_note: String::new(),
            m: ModemView::default(),
            baseline: None,
            rebase: true,
            pw_dirty: false,
            at_open: 0,
            wifi_poll: None,
            st_name: String::new(),
            s_edit: std::array::from_fn(|_| String::new()),
            ap_ssid: String::new(),
            ap_pass: Zeroizing::new(String::new()),
            sta_ssid: String::new(),
            sta_pass: Zeroizing::new(String::new()),
            fwd_name: String::new(),
            fwd_host: String::new(),
            fwd_port: "23".into(),
            fwd_mode_t: true,
            pin_name: String::new(),
            pin_voice: true,
            contacts_page: 0,
            scan_page: 0,
            shown_me: (String::new(), String::new()),
            pins_page: 0,
            script,
            stopping: false,
            stopped: false,
            fake_dir: false,
            scope: ScopeView::default(),
            sounds: false,
        };
        a.sounds = crate::modem_sim::load_sounds(&a.args.data);
        a.at(T_ATI, "ATI");
        a.refresh_settings();
        a
    }

    fn cmd(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    fn at(&mut self, tag: u32, line: &str) {
        self.at_open += 1;
        self.cmd(Cmd::At {
            tag,
            line: Zeroizing::new(line.to_string()),
        });
    }

    /// A settings change: one AT line, then the readouts again.
    fn set(&mut self, line: &str) {
        self.at(T_SET, line);
        self.refresh_settings();
    }

    fn refresh_settings(&mut self) {
        self.at(T_ATV, "AT&V");
        self.at(T_FWD, "AT+FWD?");
        self.at(T_WAP, "AT+WAP?");
        self.at(T_WSTA, "AT+WSTA?");
    }

    fn unsaved(&self) -> bool {
        self.pw_dirty
            || self
                .baseline
                .as_ref()
                .is_some_and(|b| *b != self.m.fingerprint())
    }

    fn banner(&mut self, t: impl Into<String>, warn: bool) {
        self.banner = Some(Banner {
            text: t.into(),
            warn,
            at: Instant::now(),
        });
    }

    // ---- Events from the modem thread.

    fn poll(&mut self) {
        if let Ok(g) = self.snap_src.lock() {
            self.s = g.clone();
        }
        // The name and label fields follow the identity (first start, a
        // rename, or a link that brought a new name).
        let me = (self.s.name.clone(), self.s.label.clone());
        if !me.0.is_empty() && me != self.shown_me {
            self.my_name = me.0.clone();
            self.my_label = me.1.clone();
            self.shown_me = me;
        }
        loop {
            let ev = match self.rx.try_recv() {
                Ok(ev) => ev,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The modem thread is gone (it stopped or failed): close too.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.stopped = true;
                    break;
                }
            };
            match ev {
                Ev::Ui(e) => self.on_ui(e),
                Ev::LinkCode(c) => {
                    self.link_code = Some(c);
                    self.link_note =
                        "Type this code on your other device (Link to my name), then call it."
                            .into();
                }
                Ev::At { tag, text, ok } => self.on_at(tag, text, ok),
                Ev::Note(n) => self.note = n,
                Ev::LinkEnded => {
                    if !link_code_stays(true, Some(LinkSig::Ended), self.s.linking) {
                        self.link_code = None;
                    }
                }
                Ev::Stopped => self.stopped = true,
            }
        }
        if !link_code_stays(self.link_code.is_some(), None, self.s.linking) {
            self.link_code = None;
        }
        if self
            .banner
            .as_ref()
            .is_some_and(|b| b.at.elapsed().as_secs() >= 8)
        {
            self.banner = None;
        }
    }

    fn on_ui(&mut self, e: UiEvent) {
        match e {
            UiEvent::Ringing {
                name,
                label,
                station,
            } => {
                if self.ringing.is_none() && !self.s.in_call {
                    self.ringing = Some((name, label, station));
                    // A call must be seen whatever screen is open.
                    self.screen = Screen::Calls;
                }
            }
            UiEvent::Connected { plan, words, trust } => {
                self.ringing = None;
                self.call.words = Some(words);
                self.call.trust = Some(trust);
                self.call.plan = Some(plan);
                self.call.audio_ok =
                    trust == Trust::Checked || self.args.auto_answer && trust != Trust::Conflict;
                self.call.talk_anyway = self.args.auto_answer && trust == Trust::NotChecked;
            }
            UiEvent::PlanChanged(p) => {
                let was = self.call.plan.map(|p| p.mode);
                self.call.plan = Some(p);
                if was != Some(p.mode) {
                    self.call.switched = Some(if p.mode == TalkMode::Duplex {
                        "SWITCHED TO CALL".into()
                    } else {
                        "SWITCHED TO PUSH-TO-TALK".into()
                    });
                }
            }
            UiEvent::Floor(_) => {}
            UiEvent::AnsweredElsewhere => self.end_call("ANSWERED ELSEWHERE", false),
            UiEvent::Declined => self.end_call("DECLINED", false),
            UiEvent::NoAnswer => self.end_call("NO ANSWER", false),
            UiEvent::Ended => self.end_call("CALL ENDED", false),
            UiEvent::Directory(d) => {
                if !self.fake_dir {
                    self.dir = d;
                }
            }
            UiEvent::LinkCode(_) => {}
            UiEvent::Linked => {
                if !link_code_stays(true, Some(LinkSig::Linked), self.s.linking) {
                    self.link_code = None;
                }
                self.link_note = "Linked. This device now carries your name.".into();
                self.banner("LINKED", false);
            }
            UiEvent::Error(t) => {
                let warn = t.starts_with("NOT THE");
                if t.starts_with("link failed")
                    && !link_code_stays(true, Some(LinkSig::Failed), self.s.linking)
                {
                    self.link_code = None;
                    self.link_note = t.clone();
                }
                self.banner(t, warn);
            }
            UiEvent::AtReply { .. } => {}
            UiEvent::Scope {
                t_ms,
                step_ms,
                samples,
            } => self.scope.on_scope(t_ms, step_ms, &samples),
            UiEvent::Air(a) => self.scope.on_air(a),
        }
    }

    fn end_call(&mut self, why: &str, warn: bool) {
        let had = self.ringing.is_some()
            || self.s.in_call
            || self.call.words.is_some()
            || !self.call.who.is_empty();
        self.ringing = None;
        self.call = CallView::default();
        self.muted = false;
        self.talk_sent = false;
        if had {
            self.banner(why, warn);
        }
        self.cmd(Cmd::RefreshDir);
    }

    fn on_at(&mut self, tag: u32, text: String, ok: bool) {
        self.at_open = self.at_open.saturating_sub(1);
        let clean = text.replace("\r\n", "\n").replace('\r', "\n");
        let body: String = clean
            .lines()
            .filter(|l| !l.trim().is_empty() && l.trim() != "OK")
            .collect::<Vec<_>>()
            .join("\n");
        match tag {
            T_ATV if ok => self.m.atv = body,
            T_FWD if ok => self.m.fwd = body,
            T_WAP if ok => self.m.wap = body,
            T_WSTA if ok => {
                self.m.wsta = body;
                // The last readout of a refresh.
                if self.rebase {
                    self.rebase = false;
                    self.baseline = Some(self.m.fingerprint());
                    self.pw_dirty = false;
                }
                if self.st_name.is_empty() {
                    self.st_name = self.m.name();
                }
            }
            T_ATI if ok => self.m.ati = body,
            T_WIFI => self.m.wifi = body,
            T_SCAN => {
                self.m.scanning = false;
                self.m.scan = body
                    .lines()
                    .filter(|l| l.starts_with("SCAN "))
                    .map(String::from)
                    .collect();
                if !ok {
                    self.note = format!("scan: {}", body.lines().last().unwrap_or("ERROR"));
                } else {
                    self.note = format!("scan: {} answered", self.m.scan.len());
                }
                self.cmd(Cmd::RefreshDir);
            }
            T_DIR => {
                self.note = if ok {
                    "directory changed".into()
                } else {
                    format!("directory: {body}")
                };
                self.cmd(Cmd::RefreshDir);
            }
            T_SET => {
                // A pin or a station name changes the directory too.
                self.cmd(Cmd::RefreshDir);
                if !ok {
                    let last = body.lines().last().unwrap_or("ERROR").to_string();
                    self.note = format!("refused: {last}");
                    self.banner(format!("THE MODEM SAID {last}"), false);
                }
            }
            T_SAVE => {
                // Only a save that took makes the readouts that follow
                // (queued right after it) the new saved copy.
                self.rebase = ok;
                self.note = if ok {
                    "saved (AT&W)".into()
                } else {
                    format!("save: {body}")
                };
            }
            T_DISCARD => {
                self.rebase = ok;
                self.note = if ok {
                    "back to the saved settings (ATZ)".into()
                } else {
                    format!("discard: {body}")
                };
                self.st_name.clear();
            }
            _ => {
                if !ok {
                    self.note = body.lines().last().unwrap_or("ERROR").to_string();
                }
            }
        }
    }

    // ---- Actions (buttons and the script use these).

    fn do_call(&mut self, name: &str, label: Option<&str>) {
        let name = name.trim().to_string();
        if name.is_empty() || self.s.in_call {
            return;
        }
        self.call = CallView {
            who: name.clone(),
            placed: true,
            ..CallView::default()
        };
        if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
            self.call.who = format!("{name}/{}", l.trim());
        }
        self.cmd(Cmd::Call {
            name,
            label: label
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
        });
    }

    fn do_answer(&mut self) {
        if let Some((n, l, _)) = self.ringing.take() {
            self.call = CallView {
                who: format!("{n}/{l}"),
                placed: false,
                ..CallView::default()
            };
            self.cmd(Cmd::Answer);
        }
    }

    fn do_decline(&mut self) {
        if self.ringing.take().is_some() {
            self.cmd(Cmd::Decline);
            self.banner("DECLINED", false);
        }
    }

    fn do_hangup(&mut self) {
        self.cmd(Cmd::Hangup);
        self.talk_sent = false;
    }

    fn do_words_match(&mut self) {
        if self.call.trust == Some(Trust::NotChecked) {
            self.cmd(Cmd::AcceptWords);
            self.call.trust = Some(Trust::Checked);
            self.call.audio_ok = true;
        }
    }

    fn do_talk_anyway(&mut self) {
        if self.call.trust == Some(Trust::NotChecked) {
            self.cmd(Cmd::TalkAnyway);
            self.call.audio_ok = true;
            self.call.talk_anyway = true;
        }
    }

    fn do_save(&mut self) {
        self.at(T_SAVE, "AT&W");
        self.refresh_settings();
    }

    fn do_discard(&mut self) {
        if self.s.in_call {
            self.banner("HANG UP FIRST: DISCARD IS ATZ, WHICH ENDS A CALL", false);
            return;
        }
        self.at(T_DISCARD, "ATZ");
        self.refresh_settings();
    }

    fn ptt(&self) -> bool {
        self.s.in_call
            && self.call.audio_ok
            && self
                .s
                .stats
                .plan
                .or(self.call.plan)
                .is_some_and(|p| p.mode != TalkMode::Duplex)
    }

    /// The TALK key, level based: the button held, the space bar down (no
    /// text field focused, window focused), or a script.
    fn talk_level(&mut self, ctx: &egui::Context, button_down: bool) {
        let space = ctx.input(|i| i.focused && i.key_down(Key::Space))
            && ctx.memory(|m| m.focused().is_none());
        let want = self.ptt() && (button_down || space || self.script_talk);
        if want != self.talk_sent {
            self.talk_sent = want;
            self.cmd(Cmd::TalkKey(want));
        }
    }

    // ---- The frame.

    fn header(&mut self, ui: &mut Ui, r: Rect) {
        ui.painter().rect_filled(r, CornerRadius::ZERO, SURFACE);
        hline(ui, r.bottom() - 0.5, r.left(), r.right());
        if let Some(t) = &self.logo {
            let lr = rect(r.left() + 16.0, r.top() + 8.0, 48.0, 48.0);
            ui.painter().image(
                t.id(),
                lr,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let x = r.left() + 76.0;
        text(
            ui,
            Pos2::new(x, r.top() + 12.0),
            Align2::LEFT_TOP,
            PRODUCT,
            heavy(17.0),
            TEXT,
        );
        let who = format!(
            "VOICE  {}/{}   VIA {}   {} TO {}",
            self.s.name, self.s.label, self.s.station, self.s.bind, self.s.udp_peer
        );
        text(
            ui,
            Pos2::new(x, r.top() + 38.0),
            Align2::LEFT_TOP,
            &who,
            mono(11.5),
            ACCENT,
        );
        let tabs = [
            (Screen::Calls, "CALLS"),
            (Screen::Dir, "DIRECTORY"),
            (Screen::Settings, "SETTINGS"),
        ];
        let mut bx = r.right() - 16.0 - 3.0 * 132.0 - 2.0 * 8.0;
        for (sc, name) in tabs {
            let b = rect(bx, r.top() + 14.0, 132.0, 36.0);
            let k = if self.screen == sc {
                Kind::On
            } else {
                Kind::Plain
            };
            if button(ui, b, name, k, true).clicked() {
                self.screen = sc;
            }
            bx += 140.0;
        }
    }

    fn save_bar(&mut self, ui: &mut Ui, r: Rect) {
        ui.painter().rect_filled(r, CornerRadius::ZERO, SURFACE2);
        hline(ui, r.bottom() - 0.5, r.left(), r.right());
        let why = if self.pw_dirty {
            "  (a password was set)"
        } else {
            ""
        };
        text(
            ui,
            Pos2::new(r.left() + 24.0, r.center().y),
            Align2::LEFT_CENTER,
            &format!("SETTINGS CHANGED, NOT SAVED{why}. They are live now; SAVE keeps them after a restart (AT&W)."),
            sans(14.0),
            TEXT2,
        );
        let save = rect(
            r.right() - 24.0 - 120.0,
            r.top() + 5.0,
            120.0,
            r.height() - 10.0,
        );
        let disc = rect(save.left() - 8.0 - 120.0, save.top(), 120.0, save.height());
        if button(ui, save, "SAVE", Kind::Primary, true).clicked() {
            self.do_save();
        }
        if button(ui, disc, "DISCARD", Kind::Plain, !self.s.in_call).clicked() {
            self.do_discard();
        }
    }

    fn banner_row(&mut self, ui: &mut Ui, r: Rect) {
        let Some(b) = &self.banner else {
            return;
        };
        let c = if b.warn { WARN } else { ACCENT };
        ui.painter().rect_filled(r, CornerRadius::ZERO, SURFACE2);
        ui.painter().rect_filled(
            rect(r.left(), r.top(), 4.0, r.height()),
            CornerRadius::ZERO,
            c,
        );
        let t = b.text.clone();
        text_fit(
            ui,
            Pos2::new(r.left() + 20.0, r.center().y),
            Align2::LEFT_CENTER,
            &t,
            heavy(15.0),
            c,
            r.width() - 120.0,
        );
        let x = rect(r.right() - 90.0, r.top() + 4.0, 80.0, r.height() - 8.0);
        if button(ui, x, "OK", Kind::Plain, true).clicked() {
            self.banner = None;
        }
    }

    /// The bottom band on every screen: the modem's state, the attribution
    /// and the signal scope.
    fn monitor(&mut self, ui: &mut Ui, r: Rect) {
        ui.painter().rect_filled(r, CornerRadius::ZERO, BG_FOOT);
        hline(ui, r.top() + 0.5, r.left(), r.right());
        let st = match self.s.call_state {
            0 => "IDLE",
            1 => "DIALING",
            2 => "RINGING",
            3 => "TRAINING",
            4 => "IN CALL",
            5 => "IN CALL",
            _ => "",
        };
        let ty = r.top() + 15.0;
        text(
            ui,
            Pos2::new(r.left() + 16.0, ty),
            Align2::LEFT_CENTER,
            "MODEM SIGNAL",
            monob(11.5),
            TEXT2,
        );
        let level = self
            .scope
            .level()
            .map_or("NO SIGNAL DATA".to_string(), |d| format!("{d} DBM"));
        let left = format!(
            "{}  {}   {}   CONNECTION SOUNDS {}   {}",
            self.s.station,
            st,
            level,
            if self.sounds { "ON" } else { "OFF" },
            self.note
        );
        text_fit(
            ui,
            Pos2::new(r.left() + 128.0, ty),
            Align2::LEFT_CENTER,
            &left,
            mono(11.5),
            MUTED,
            760.0,
        );
        text(
            ui,
            Pos2::new(r.right() - 16.0, ty),
            Align2::RIGHT_CENTER,
            ATTRIBUTION,
            mono(11.5),
            DIM,
        );
        let plot = Rect::from_min_max(
            Pos2::new(r.left() + 16.0, r.top() + 30.0),
            Pos2::new(r.right() - 16.0, r.bottom() - 10.0),
        );
        self.scope.draw(ui, plot);
    }

    // ---- CALLS.

    fn calls(&mut self, ui: &mut Ui, r: Rect) {
        let left = rect(r.left(), r.top(), 560.0, r.height());
        let right = Rect::from_min_max(Pos2::new(left.right() + 20.0, r.top()), r.max);
        self.people(ui, left);
        if let Some((n, l, st)) = self.ringing.clone() {
            self.ring_panel(ui, right, &n, &l, &st);
        } else {
            self.call_panel(ui, right);
        }
    }

    /// People first (voice devices), then stations; fixed rows, paged.
    fn people(&mut self, ui: &mut Ui, r: Rect) {
        panel(ui, r);
        kicker(
            ui,
            Pos2::new(r.left() + 20.0, r.top() + 16.0),
            "PEOPLE AND STATIONS NEARBY",
        );
        let rows = 6usize;
        let row_h = 44.0;
        let pages = self.dir.len().div_ceil(rows).max(1);
        self.calls_page = self.calls_page.min(pages - 1);
        let top = r.top() + 40.0;
        for i in 0..rows {
            let y = top + i as f32 * row_h;
            let rr = rect(r.left() + 1.0, y, r.width() - 2.0, row_h);
            if i % 2 == 1 {
                ui.painter().rect_filled(rr, CornerRadius::ZERO, ZEBRA);
            }
            let Some(d) = self.dir.get(self.calls_page * rows + i).cloned() else {
                continue;
            };
            let voice = !d.label.is_empty() || d.station != d.name;
            let who = if voice && !d.label.is_empty() {
                format!("{}/{}", d.name, d.label)
            } else {
                d.name.clone()
            };
            let selected = self.sel.as_deref() == Some(who.as_str());
            if selected {
                ui.painter().rect_filled(
                    rect(rr.left(), rr.top(), 3.0, rr.height()),
                    CornerRadius::ZERO,
                    ACCENT,
                );
            }
            let resp = ui.allocate_rect(
                rect(rr.left(), rr.top(), rr.width() - 110.0, rr.height()),
                Sense::click(),
            );
            if resp.clicked() && voice {
                self.sel = Some(who.clone());
                self.dial_name = d.name.clone();
                self.dial_label = d.label.clone();
            }
            text_fit(
                ui,
                Pos2::new(rr.left() + 18.0, y + 12.0),
                Align2::LEFT_CENTER,
                &who,
                heavy(16.0),
                if voice { TEXT } else { TEXT3 },
                220.0,
            );
            let sub = if voice {
                format!(
                    "{}VIA {}",
                    if d.trust == Trust::Checked {
                        "CHECKED  "
                    } else {
                        ""
                    },
                    if d.station.is_empty() {
                        "-"
                    } else {
                        &d.station
                    }
                )
            } else {
                "STATION (DATA CALLS FROM A TERMINAL)".into()
            };
            text_fit(
                ui,
                Pos2::new(rr.left() + 18.0, y + 31.0),
                Align2::LEFT_CENTER,
                &sub,
                mono(11.0),
                DIM,
                260.0,
            );
            let tier = d.best_tier.map_or("NO TIER".to_string(), |t| {
                t.name().trim_start_matches("LORA ").to_string()
            });
            text(
                ui,
                Pos2::new(rr.left() + 300.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                &tier,
                mono(12.5),
                TEXT2,
            );
            if voice {
                let hint = if d.mode_hint == TalkMode::Duplex {
                    "CALL"
                } else {
                    "PTT"
                };
                text(
                    ui,
                    Pos2::new(rr.left() + 400.0, y + row_h / 2.0),
                    Align2::LEFT_CENTER,
                    hint,
                    monob(13.0),
                    ACCENT,
                );
                if d.pinned {
                    text(
                        ui,
                        Pos2::new(rr.left() + 400.0, y + 34.0),
                        Align2::LEFT_CENTER,
                        "PIN",
                        mono(10.0),
                        DIM,
                    );
                }
                let b = rect(rr.right() - 100.0, y + 7.0, 88.0, row_h - 14.0);
                let can = !self.s.in_call && self.ringing.is_none() && !d.label.is_empty();
                if button(ui, b, "CALL", Kind::Primary, can).clicked() {
                    let (n, l) = (d.name.clone(), d.label.clone());
                    self.do_call(&n, Some(&l));
                }
            }
        }
        let py = top + rows as f32 * row_h + 10.0;
        hline(ui, py - 5.0, r.left() + 1.0, r.right() - 1.0);
        let shown = if self.dir.is_empty() {
            "NOBODY HEARD YET: WAIT A MINUTE, OR SCAN".to_string()
        } else {
            format!(
                "PAGE {} OF {}   {} ENTRIES",
                self.calls_page + 1,
                pages,
                self.dir.len()
            )
        };
        text(
            ui,
            Pos2::new(r.left() + 20.0, py + 17.0),
            Align2::LEFT_CENTER,
            &shown,
            mono(11.5),
            MUTED,
        );
        if button(
            ui,
            rect(r.right() - 196.0, py, 88.0, 34.0),
            "PREV",
            Kind::Plain,
            self.calls_page > 0,
        )
        .clicked()
        {
            self.calls_page -= 1;
        }
        if button(
            ui,
            rect(r.right() - 100.0, py, 88.0, 34.0),
            "NEXT",
            Kind::Plain,
            self.calls_page + 1 < pages,
        )
        .clicked()
        {
            self.calls_page += 1;
        }
        // Call by name.
        let cy = py + 46.0;
        hline(ui, cy - 6.0, r.left() + 1.0, r.right() - 1.0);
        kicker(
            ui,
            Pos2::new(r.left() + 20.0, cy + 4.0),
            "CALL BY NAME (ANY DEVICE, OR NAME / LABEL FOR ONE)",
        );
        let fy = cy + 24.0;
        field(
            ui,
            rect(r.left() + 20.0, fy, 230.0, 40.0),
            &mut self.dial_name,
            "dial_name",
            false,
            "NAME",
            16,
        );
        field(
            ui,
            rect(r.left() + 258.0, fy, 120.0, 40.0),
            &mut self.dial_label,
            "dial_label",
            false,
            "LABEL",
            8,
        );
        let can = !self.s.in_call && self.ringing.is_none() && !self.dial_name.trim().is_empty();
        if button(
            ui,
            rect(r.left() + 390.0, fy, r.width() - 410.0, 40.0),
            "CALL",
            Kind::Primary,
            can,
        )
        .clicked()
        {
            let (n, l) = (self.dial_name.clone(), self.dial_label.clone());
            self.do_call(&n, Some(&l));
        }
    }

    fn ring_panel(&mut self, ui: &mut Ui, r: Rect, n: &str, l: &str, st: &str) {
        panel(ui, r);
        ui.painter().rect_filled(
            rect(r.left(), r.top(), r.width(), 4.0),
            CornerRadius::ZERO,
            ACCENT,
        );
        kicker(
            ui,
            Pos2::new(r.left() + 28.0, r.top() + 28.0),
            "INCOMING CALL",
        );
        let who = if l.is_empty() {
            n.to_string()
        } else {
            format!("{n}/{l}")
        };
        text_fit(
            ui,
            Pos2::new(r.left() + 28.0, r.top() + 56.0),
            Align2::LEFT_TOP,
            &who,
            heavy(56.0),
            TEXT,
            r.width() - 56.0,
        );
        text(
            ui,
            Pos2::new(r.left() + 28.0, r.top() + 132.0),
            Align2::LEFT_TOP,
            &format!("THROUGH THE MODEM {st}"),
            mono(14.0),
            TEXT2,
        );
        text_wrap(
            ui,
            rect(r.left() + 28.0, r.top() + 166.0, r.width() - 56.0, 60.0),
            "A name is only a hint: once you answer, the FLOWTRON SCRAMBLER checks this device's key and shows two words to compare.",
            sans(14.0),
            MUTED,
        );
        let bw = (r.width() - 56.0 - 16.0) / 2.0;
        let by = r.top() + 250.0;
        if button(
            ui,
            rect(r.left() + 28.0, by, bw, 110.0),
            "ANSWER",
            Kind::Primary,
            true,
        )
        .clicked()
        {
            self.do_answer();
        }
        if button(
            ui,
            rect(r.left() + 44.0 + bw, by, bw, 110.0),
            "DECLINE",
            Kind::Plain,
            true,
        )
        .clicked()
        {
            self.do_decline();
        }
    }

    fn call_panel(&mut self, ui: &mut Ui, r: Rect) {
        panel(ui, r);
        let x = r.left() + 28.0;
        let w = r.width() - 56.0;
        let st = self.s.stats;
        let plan = st.plan.or(self.call.plan);
        let connected = self.s.in_call && st.connected_ms.is_some();
        let peer = self.s.peer.clone();
        let who = match &peer {
            Some((n, l, _)) if !l.is_empty() => format!("{n}/{l}"),
            Some((n, _, _)) => n.clone(),
            None => self.call.who.clone(),
        };
        let trust = self.call.trust;
        let conflict = trust == Some(Trust::Conflict);
        let (kick, title, tcol) = if !self.s.in_call {
            ("READY", "NO CALL".to_string(), TEXT)
        } else if !connected {
            if self.call.placed {
                ("CALLING", who.clone(), TEXT)
            } else {
                ("ANSWERING", who.clone(), TEXT)
            }
        } else if conflict {
            (
                "WARNING",
                format!(
                    "NOT THE {} YOU CHECKED",
                    peer.as_ref()
                        .map_or(who.clone(), |p| p.0.clone())
                        .to_ascii_uppercase()
                ),
                WARN,
            )
        } else if trust == Some(Trust::NotChecked) && !self.call.talk_anyway {
            ("NOT CHECKED", format!("{who} (NOT CHECKED)"), WARN)
        } else if trust.is_none() {
            ("SCRAMBLER", format!("{who}  STARTING THE SCRAMBLER"), TEXT)
        } else {
            ("IN CALL", who.clone(), TEXT)
        };
        kicker(ui, Pos2::new(x, r.top() + 22.0), kick);
        text_fit(
            ui,
            Pos2::new(x, r.top() + 42.0),
            Align2::LEFT_TOP,
            &title,
            heavy(34.0),
            tcol,
            w,
        );
        // The tier, codec and mode line.
        // (SF12 reads `PTT DELAYED` from the mode name.)
        let line = if self.s.in_call {
            plan_line(st.sf, plan)
        } else {
            "NO LINK".into()
        };
        text_fit(
            ui,
            Pos2::new(x, r.top() + 92.0),
            Align2::LEFT_TOP,
            &line,
            monob(17.0),
            if self.s.in_call { ACCENT } else { DIM },
            w,
        );
        let mut sub = String::new();
        if self.s.in_call {
            sub = format!(
                "VOICE {}   SENT {}   HEARD {}   GAPS FILLED {}",
                if st.slots_ms.is_some() {
                    "ON"
                } else if connected {
                    "HELD"
                } else {
                    "-"
                },
                st.frames_sent,
                st.frames_heard,
                st.frames_concealed,
            );
            if self.s.peer_muted {
                sub.push_str("   THEY MUTED");
            }
            if let Some(s) = &self.call.switched {
                sub.push_str("   ");
                sub.push_str(s);
            }
        }
        text_fit(
            ui,
            Pos2::new(x, r.top() + 120.0),
            Align2::LEFT_TOP,
            &sub,
            mono(12.0),
            MUTED,
            w,
        );
        // The two words.
        let wy = r.top() + 144.0;
        let wr = rect(x, wy, w, 108.0);
        ui.painter().rect_filled(wr, CornerRadius::ZERO, SURFACE2);
        ui.painter().rect_stroke(
            wr,
            CornerRadius::ZERO,
            Stroke::new(
                1.0,
                if conflict || trust == Some(Trust::NotChecked) && !self.call.audio_ok {
                    WARN
                } else {
                    RULE
                },
            ),
            StrokeKind::Inside,
        );
        match self.call.words.filter(|_| self.s.in_call) {
            Some(words) => {
                kicker(
                    ui,
                    Pos2::new(x + 18.0, wy + 14.0),
                    "THE TWO WORDS: BOTH SCREENS MUST SHOW THE SAME",
                );
                text(
                    ui,
                    Pos2::new(x + 18.0, wy + 36.0),
                    Align2::LEFT_TOP,
                    &format!(
                        "{}  {}",
                        words[0].to_ascii_uppercase(),
                        words[1].to_ascii_uppercase()
                    ),
                    monob(36.0),
                    TEXT,
                );
                let note = match trust {
                    Some(Trust::Checked) if !self.call.talk_anyway => {
                        "CHECKED: this device's key is one you checked before."
                    }
                    Some(Trust::NotChecked) if !self.call.audio_ok => {
                        "Audio is held both ways until you compare the words."
                    }
                    Some(Trust::NotChecked) => {
                        "TALK ANYWAY: this call only. The device stays NOT CHECKED."
                    }
                    Some(Trust::Conflict) => {
                        "A device you checked shows a different key, or was removed. Muted."
                    }
                    _ => "",
                };
                text(
                    ui,
                    Pos2::new(x + 18.0, wy + 82.0),
                    Align2::LEFT_TOP,
                    note,
                    sans(13.5),
                    if conflict { WARN } else { TEXT3 },
                );
                if trust == Some(Trust::NotChecked) && !self.call.audio_ok {
                    let bw = 170.0;
                    if button(
                        ui,
                        rect(wr.right() - 18.0 - bw, wy + 12.0, bw, 40.0),
                        "WORDS MATCH",
                        Kind::Primary,
                        true,
                    )
                    .clicked()
                    {
                        self.do_words_match();
                    }
                    if button(
                        ui,
                        rect(wr.right() - 18.0 - bw, wy + 58.0, bw, 40.0),
                        "TALK ANYWAY",
                        Kind::Warn,
                        true,
                    )
                    .clicked()
                    {
                        self.do_talk_anyway();
                    }
                }
            }
            None => {
                kicker(ui, Pos2::new(x + 18.0, wy + 14.0), "FLOWTRON SCRAMBLER");
                text_wrap(
                    ui,
                    rect(x + 18.0, wy + 36.0, w - 36.0, 66.0),
                    "Every call is encrypted end to end. Two words appear here once the scrambler is up: read them to each other. Matching words mean nobody is in the middle.",
                    sans(14.0),
                    TEXT3,
                );
            }
        }
        // The big buttons.
        let by = wy + 120.0;
        let bh = 62.0;
        if self.s.in_call {
            let mute_w = 150.0;
            if button(
                ui,
                rect(x, by, w - mute_w - 12.0, bh),
                "HANG UP",
                Kind::Plain,
                true,
            )
            .clicked()
            {
                self.do_hangup();
            }
            let mk = if self.muted { Kind::On } else { Kind::Plain };
            if button(
                ui,
                rect(x + w - mute_w, by, mute_w, bh),
                if self.muted { "MUTED" } else { "MUTE" },
                mk,
                connected,
            )
            .clicked()
            {
                self.muted = !self.muted;
                self.cmd(Cmd::Mute(self.muted));
            }
        } else {
            let target = if self.dial_name.trim().is_empty() {
                "CALL".to_string()
            } else if self.dial_label.trim().is_empty() {
                format!("CALL {}", self.dial_name.trim())
            } else {
                format!("CALL {}/{}", self.dial_name.trim(), self.dial_label.trim())
            };
            let can = !self.dial_name.trim().is_empty();
            if button(ui, rect(x, by, w, bh), &target, Kind::Primary, can).clicked() {
                let (n, l) = (self.dial_name.clone(), self.dial_label.clone());
                self.do_call(&n, Some(&l));
            }
        }
        // TALK, only on a push to talk tier.
        let ty = by + bh + 12.0;
        let mut held = false;
        if self.ptt() {
            let floor = self.s.floor;
            let (label, k) = match floor {
                Floor::Mine => ("TALKING  (RELEASE TO LISTEN)", Kind::On),
                Floor::Theirs => ("THEY ARE TALKING", Kind::Plain),
                Floor::Free if self.talk_sent => ("WAITING FOR THE FLOOR", Kind::Plain),
                Floor::Free => ("HOLD TO TALK  (OR SPACE)", Kind::Primary),
            };
            let resp = button(ui, rect(x, ty, w, 84.0), label, k, true);
            let resp = resp.interact(Sense::drag());
            held = resp.is_pointer_button_down_on();
        } else if self.s.in_call
            && plan.is_some_and(|p| p.mode == TalkMode::Duplex)
            && self.call.audio_ok
        {
            text_wrap(
                ui,
                rect(x, ty + 8.0, w, 80.0),
                "CALL mode: both talk at once, like a phone. Use a headset: the PC app has no echo canceller yet.",
                sans(14.0),
                TEXT3,
            );
        }
        self.talk_level(ui.ctx(), held);
    }

    // ---- DIRECTORY.

    fn directory(&mut self, ui: &mut Ui, r: Rect) {
        let left = rect(r.left(), r.top(), 840.0, r.height());
        let right = Rect::from_min_max(Pos2::new(left.right() + 20.0, r.top()), r.max);
        panel(ui, left);
        kicker(
            ui,
            Pos2::new(left.left() + 20.0, left.top() + 16.0),
            "DIRECTORY: VOICE DEVICES FIRST, THEN STATIONS",
        );
        let cols = [
            (20.0, "NAME"),
            (200.0, "DEVICE"),
            (290.0, "HEARD VIA"),
            (440.0, "BEST TIER"),
            (590.0, ""),
        ];
        for (cx, h) in cols {
            text(
                ui,
                Pos2::new(left.left() + cx, left.top() + 44.0),
                Align2::LEFT_TOP,
                h,
                mono(11.0),
                DIM,
            );
        }
        let rows = 6usize;
        let row_h = 44.0;
        let top = left.top() + 64.0;
        let pages = self.dir.len().div_ceil(rows).max(1);
        self.dir_page = self.dir_page.min(pages - 1);
        for i in 0..rows {
            let y = top + i as f32 * row_h;
            let rr = rect(left.left() + 1.0, y, left.width() - 2.0, row_h);
            if i % 2 == 1 {
                ui.painter().rect_filled(rr, CornerRadius::ZERO, ZEBRA);
            }
            let Some(d) = self.dir.get(self.dir_page * rows + i).cloned() else {
                continue;
            };
            let voice = !d.label.is_empty() || d.station != d.name;
            let cy = y + row_h / 2.0;
            text_fit(
                ui,
                Pos2::new(left.left() + 20.0, cy),
                Align2::LEFT_CENTER,
                &d.name,
                heavy(15.0),
                if voice { TEXT } else { TEXT3 },
                170.0,
            );
            let dev = if voice {
                if d.label.is_empty() {
                    "-".into()
                } else {
                    d.label.clone()
                }
            } else {
                "STATION".to_string()
            };
            text(
                ui,
                Pos2::new(left.left() + 200.0, cy),
                Align2::LEFT_CENTER,
                &dev,
                mono(12.5),
                TEXT2,
            );
            let via = if voice {
                if d.station.is_empty() {
                    "-".to_string()
                } else {
                    d.station.clone()
                }
            } else {
                "-".into()
            };
            text_fit(
                ui,
                Pos2::new(left.left() + 290.0, cy),
                Align2::LEFT_CENTER,
                &via,
                mono(12.5),
                TEXT2,
                140.0,
            );
            let tier = d.best_tier.map_or("NONE".to_string(), |t| {
                t.name().trim_start_matches("LORA ").to_string()
            });
            let hint = if voice {
                if d.mode_hint == TalkMode::Duplex {
                    " CALL"
                } else {
                    " PTT"
                }
            } else {
                ""
            };
            text(
                ui,
                Pos2::new(left.left() + 440.0, cy),
                Align2::LEFT_CENTER,
                &format!("{tier}{hint}"),
                mono(12.5),
                TEXT2,
            );
            // Actions: CALL (voice), PIN or UNPIN, DELETE (stations).
            let bx = left.left() + 590.0;
            let bh = row_h - 12.0;
            if voice {
                let can = !self.s.in_call && self.ringing.is_none() && !d.label.is_empty();
                if button(ui, rect(bx, y + 6.0, 64.0, bh), "CALL", Kind::Primary, can).clicked() {
                    let (n, l) = (d.name.clone(), d.label.clone());
                    self.do_call(&n, Some(&l));
                    self.screen = Screen::Calls;
                }
            } else {
                text(
                    ui,
                    Pos2::new(bx + 32.0, cy),
                    Align2::CENTER_CENTER,
                    "DATA",
                    mono(10.5),
                    FAINT,
                );
            }
            let (pl, line) = match (voice, d.pinned) {
                (true, false) => ("PIN", format!("AT+DIR=+V:{}", d.name)),
                (true, true) => ("UNPIN", format!("AT+DIR=-V:{}", d.name)),
                (false, false) => ("PIN", format!("AT+DIR=+{}", d.name)),
                (false, true) => ("PINNED", String::new()),
            };
            if button(
                ui,
                rect(bx + 72.0, y + 6.0, 84.0, bh),
                pl,
                if d.pinned { Kind::On } else { Kind::Plain },
                !line.is_empty(),
            )
            .clicked()
            {
                self.at(T_DIR, &line);
            }
            if !voice
                && button(
                    ui,
                    rect(bx + 164.0, y + 6.0, 80.0, bh),
                    "DELETE",
                    Kind::Plain,
                    true,
                )
                .clicked()
            {
                let l = format!("AT+DIR=-{}", d.name);
                self.at(T_DIR, &l);
            }
        }
        let py = top + rows as f32 * row_h + 12.0;
        hline(ui, py - 6.0, left.left() + 1.0, left.right() - 1.0);
        text(
            ui,
            Pos2::new(left.left() + 20.0, py + 17.0),
            Align2::LEFT_CENTER,
            &format!(
                "PAGE {} OF {}   {} ENTRIES",
                self.dir_page + 1,
                pages,
                self.dir.len()
            ),
            mono(11.5),
            MUTED,
        );
        if button(
            ui,
            rect(left.right() - 320.0, py, 88.0, 34.0),
            "PREV",
            Kind::Plain,
            self.dir_page > 0,
        )
        .clicked()
        {
            self.dir_page -= 1;
        }
        if button(
            ui,
            rect(left.right() - 224.0, py, 88.0, 34.0),
            "NEXT",
            Kind::Plain,
            self.dir_page + 1 < pages,
        )
        .clicked()
        {
            self.dir_page += 1;
        }
        if button(
            ui,
            rect(left.right() - 128.0, py, 108.0, 34.0),
            "REFRESH",
            Kind::Plain,
            true,
        )
        .clicked()
        {
            self.cmd(Cmd::RefreshDir);
        }
        text_wrap(
            ui,
            rect(left.left() + 20.0, py + 44.0, left.width() - 40.0, 40.0),
            "A voice pin covers every device of that name (AT+DIR=+V:NAME) and lets it ring a HIDDEN modem. DELETE removes a station (AT+DIR=-NAME). Voice devices are unpinned, not deleted: they come back when heard.",
            sans(13.0),
            MUTED,
        );

        // Scan and pin.
        panel(ui, right);
        let x = right.left() + 20.0;
        let w = right.width() - 40.0;
        kicker(
            ui,
            Pos2::new(x, right.top() + 16.0),
            "SCAN (AT+SCAN, 5 S OF ANSWERS)",
        );
        let hidden = self.m.beacon() == "HIDDEN";
        let label = if self.m.scanning {
            "SCANNING"
        } else {
            "SCAN NOW"
        };
        if button(
            ui,
            rect(x, right.top() + 40.0, w, 46.0),
            label,
            Kind::Primary,
            !self.m.scanning && !self.s.in_call && !hidden,
        )
        .clicked()
        {
            self.m.scanning = true;
            self.m.scan.clear();
            self.at(T_SCAN, "AT+SCAN");
        }
        if hidden {
            text(
                ui,
                Pos2::new(x, right.top() + 94.0),
                Align2::LEFT_TOP,
                "HIDDEN mode sends no scans.",
                sans(13.0),
                MUTED,
            );
        }
        // Scan answers, 2 to a page, each on two lines (station and SNR,
        // then the tier and the voice names) so a long one is not cut.
        let per = 2usize;
        let pages = self.m.scan.len().div_ceil(per).max(1);
        self.scan_page = self.scan_page.min(pages - 1);
        if pages > 1 && !hidden {
            let label = format!("{} OF {}", self.scan_page + 1, pages);
            text(
                ui,
                Pos2::new(x, right.top() + 106.0),
                Align2::LEFT_CENTER,
                &label,
                mono(11.0),
                MUTED,
            );
            if button(
                ui,
                rect(right.right() - 196.0, right.top() + 92.0, 84.0, 28.0),
                "PREV",
                Kind::Plain,
                self.scan_page > 0,
            )
            .clicked()
            {
                self.scan_page -= 1;
            }
            if button(
                ui,
                rect(right.right() - 104.0, right.top() + 92.0, 84.0, 28.0),
                "NEXT",
                Kind::Plain,
                self.scan_page + 1 < pages,
            )
            .clicked()
            {
                self.scan_page += 1;
            }
        }
        for (i, l) in self
            .m
            .scan
            .iter()
            .skip(self.scan_page * per)
            .take(per)
            .enumerate()
        {
            let (a, b) = scan_two_lines(l);
            let y = right.top() + 130.0 + i as f32 * 44.0;
            text_fit(
                ui,
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                &a,
                mono(11.5),
                TEXT2,
                w,
            );
            text_fit(
                ui,
                Pos2::new(x + 16.0, y + 19.0),
                Align2::LEFT_TOP,
                &b,
                mono(11.5),
                MUTED,
                w - 16.0,
            );
        }
        if self.m.scan.is_empty() && !self.m.scanning {
            text(
                ui,
                Pos2::new(x, right.top() + 130.0),
                Align2::LEFT_TOP,
                "No scan yet.",
                sans(13.0),
                DIM,
            );
        }
        let py = right.top() + 218.0;
        hline(ui, py, right.left() + 1.0, right.right() - 1.0);
        kicker(ui, Pos2::new(x, py + 14.0), "PIN A NAME NOT HEARD YET");
        field(
            ui,
            rect(x, py + 34.0, w, 40.0),
            &mut self.pin_name,
            "pin_name",
            false,
            "NAME",
            16,
        );
        let half = (w - 8.0) / 2.0;
        if button(
            ui,
            rect(x, py + 80.0, half, 32.0),
            "VOICE NAME",
            if self.pin_voice {
                Kind::On
            } else {
                Kind::Plain
            },
            true,
        )
        .clicked()
        {
            self.pin_voice = true;
        }
        if button(
            ui,
            rect(x + half + 8.0, py + 80.0, half, 32.0),
            "STATION",
            if self.pin_voice {
                Kind::Plain
            } else {
                Kind::On
            },
            true,
        )
        .clicked()
        {
            self.pin_voice = false;
        }
        let n = self.pin_name.trim().to_string();
        if button(
            ui,
            rect(x, py + 120.0, w, 36.0),
            "PIN",
            Kind::Primary,
            !n.is_empty(),
        )
        .clicked()
        {
            let l = if self.pin_voice {
                format!("AT+DIR=+V:{n}")
            } else {
                format!("AT+DIR=+{n}")
            };
            self.at(T_DIR, &l);
            self.pin_name.clear();
        }
        if button(
            ui,
            rect(x, right.bottom() - 40.0, w, 34.0),
            "CLEAR UNPINNED (AT+DIR=0)",
            Kind::Plain,
            true,
        )
        .clicked()
        {
            self.at(T_DIR, "AT+DIR=0");
        }
    }

    // ---- SETTINGS.

    fn settings(&mut self, ui: &mut Ui, r: Rect) {
        let nav = rect(r.left(), r.top(), 210.0, r.height());
        panel(ui, nav);
        kicker(
            ui,
            Pos2::new(nav.left() + 18.0, nav.top() + 16.0),
            "SETTINGS",
        );
        for (i, (t, name)) in TABS.iter().enumerate() {
            let b = rect(
                nav.left() + 12.0,
                nav.top() + 40.0 + i as f32 * 44.0,
                nav.width() - 24.0,
                38.0,
            );
            if button(
                ui,
                b,
                name,
                if self.tab == *t {
                    Kind::On
                } else {
                    Kind::Plain
                },
                true,
            )
            .clicked()
            {
                self.tab = *t;
            }
        }
        text_wrap(
            ui,
            rect(nav.left() + 18.0, nav.bottom() - 104.0, nav.width() - 36.0, 96.0),
            "Every change is one AT line, the same the AT+MENU sends, and is live at once. SAVE (the bar on top) keeps it after a restart.",
            sans(12.5),
            DIM,
        );
        let body = Rect::from_min_max(Pos2::new(nav.right() + 20.0, r.top()), r.max);
        panel(ui, body);
        let inner = body.shrink2(Vec2::new(24.0, 14.0));
        match self.tab {
            Tab::Radio => self.tab_radio(ui, inner),
            Tab::Privacy => self.tab_privacy(ui, inner),
            Tab::Wifi => self.tab_wifi(ui, inner),
            Tab::Forwards => self.tab_forwards(ui, inner),
            Tab::Voice => self.tab_voice(ui, inner),
            Tab::About => self.tab_about(ui, inner),
        }
    }

    /// One register row: name, note, value and - / + (and a typed value).
    #[allow(clippy::too_many_arguments)]
    fn sreg_row(
        &mut self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        w: f32,
        reg: u8,
        title: &str,
        note: &str,
        step: i32,
        show: &dyn Fn(i32) -> String,
        warn: bool,
    ) {
        let v = self.m.sreg(reg);
        text(
            ui,
            Pos2::new(x, y + 2.0),
            Align2::LEFT_TOP,
            title,
            heavy(15.0),
            TEXT,
        );
        text_fit(
            ui,
            Pos2::new(x, y + 21.0),
            Align2::LEFT_TOP,
            note,
            sans(12.5),
            if warn { WARN } else { MUTED },
            470.0,
        );
        let vx = x + 490.0;
        let vs = v.map_or("-".into(), show);
        text(
            ui,
            Pos2::new(vx, y + 18.0),
            Align2::LEFT_CENTER,
            &vs,
            monob(15.0),
            if warn { WARN } else { ACCENT },
        );
        let bx = x + w - 3.0 * 52.0 - 120.0;
        let Some(v) = v else {
            return;
        };
        // On/off registers toggle as the menu does (`ATSn=1` or `=0`).
        if reg == 31 || reg == 32 {
            let (label, to) = if v == 0 {
                ("TURN ON", 1)
            } else {
                ("TURN OFF", 0)
            };
            if button(ui, rect(bx, y + 3.0, 252.0, 32.0), label, Kind::Plain, true).clicked() {
                self.set(&format!("ATS{reg}={to}"));
            }
            return;
        }
        if button(ui, rect(bx, y + 3.0, 44.0, 32.0), "-", Kind::Plain, true).clicked() {
            self.set(&format!("ATS{reg}={}", v - step));
        }
        if button(
            ui,
            rect(bx + 52.0, y + 3.0, 44.0, 32.0),
            "+",
            Kind::Plain,
            true,
        )
        .clicked()
        {
            self.set(&format!("ATS{reg}={}", v + step));
        }
        let idx = usize::from(reg).min(32);
        let mut s = std::mem::take(&mut self.s_edit[idx]);
        let resp = field(
            ui,
            rect(bx + 104.0, y + 3.0, 70.0, 32.0),
            &mut s,
            &format!("sreg{reg}"),
            false,
            "",
            4,
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if button(
            ui,
            rect(bx + 182.0, y + 3.0, 70.0, 32.0),
            "SET",
            Kind::Plain,
            !s.trim().is_empty(),
        )
        .clicked()
            || enter && !s.trim().is_empty()
        {
            self.set(&format!("ATS{reg}={}", s.trim()));
            s.clear();
        }
        self.s_edit[idx] = s;
    }

    fn tab_radio(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        kicker(ui, Pos2::new(x, r.top()), "RADIO");
        // Station name (AT+NAME), as the menu's Settings screen.
        let y0 = r.top() + 22.0;
        text(
            ui,
            Pos2::new(x, y0 + 2.0),
            Align2::LEFT_TOP,
            "Station name",
            heavy(15.0),
            TEXT,
        );
        text(
            ui,
            Pos2::new(x, y0 + 21.0),
            Align2::LEFT_TOP,
            "The modem's name on the air (AT+NAME). Empty: the firmware's name.",
            sans(12.5),
            MUTED,
        );
        let mut n = std::mem::take(&mut self.st_name);
        field(
            ui,
            rect(x + 490.0, y0 + 3.0, 220.0, 32.0),
            &mut n,
            "st_name",
            false,
            "NAME",
            16,
        );
        if button(
            ui,
            rect(x + w - 120.0, y0 + 3.0, 120.0, 32.0),
            "SET NAME",
            Kind::Plain,
            !self.s.in_call,
        )
        .clicked()
        {
            let l = format!("AT+NAME={}", n.trim());
            self.set(&l);
        }
        self.st_name = n;
        let rows: [(u8, &str, &str, i32); 8] = [
            (30, "TX power (S30)", "", 1),
            (
                21,
                "Tier cap (S21)",
                "The fastest tier this modem uses (7 = SF5, 0 = SF12).",
                1,
            ),
            (
                0,
                "Auto-answer rings (S0)",
                "Data calls only: a voice call is always picked up by a person.",
                1,
            ),
            (
                31,
                "Compression (S31)",
                "LZ on the air for data calls when both ends have 1.",
                1,
            ),
            (
                32,
                "Boot to menu (S32)",
                "1 opens the setup menu on the first host byte after boot.",
                1,
            ),
            (
                20,
                "Training margin (S20)",
                "dB x4 kept over a tier's floor when training (20 = 5 dB).",
                4,
            ),
            (
                7,
                "Carrier wait (S7)",
                "Seconds a call may ring or dial before NO ANSWER.",
                5,
            ),
            (
                22,
                "Resync timeout (S22)",
                "Seconds of silence before NO CARRIER.",
                1,
            ),
        ];
        let tx_line = self.m.line("TX POWER").unwrap_or("").to_string();
        for (i, (reg, title, note, step)) in rows.into_iter().enumerate() {
            let y = y0 + 46.0 + i as f32 * 40.0;
            hline(ui, y - 4.0, x, x + w);
            let v = self.m.sreg(reg);
            let warn = reg == 30 && v.is_some_and(|v| v > S30_SAFE);
            let note_s = if reg == 30 {
                if warn {
                    format!(
                        "ABOVE THE SAFE DEFAULT ({S30_SAFE}). {}",
                        tx_line.trim_start_matches("TX POWER ")
                    )
                } else {
                    format!(
                        "dBm at the chip, -9 to 22. {}",
                        tx_line.trim_start_matches("TX POWER ")
                    )
                }
            } else {
                note.to_string()
            };
            let show: Box<dyn Fn(i32) -> String> = match reg {
                30 => Box::new(|v| format!("{v} dBm")),
                21 => Box::new(|v| {
                    Tier::from_index(v as u8).map_or(v.to_string(), |t| {
                        format!(
                            "{v} {}",
                            t.name()
                                .trim_start_matches("LORA ")
                                .split(' ')
                                .next()
                                .unwrap_or("")
                        )
                    })
                }),
                31 | 32 => Box::new(|v| {
                    if v == 0 {
                        "0 OFF".into()
                    } else {
                        "1 ON".into()
                    }
                }),
                0 => Box::new(|v| {
                    if v == 0 {
                        "0 OFF".into()
                    } else {
                        v.to_string()
                    }
                }),
                20 => Box::new(|v| format!("{v} ({:.1} dB)", v as f32 / 4.0)),
                _ => Box::new(|v| format!("{v} s")),
            };
            self.sreg_row(ui, x, y, w, reg, title, &note_s, step, &*show, warn);
        }
    }

    fn tab_privacy(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        kicker(ui, Pos2::new(x, r.top()), "PRIVACY MODE (AT+BEACON)");
        let cur = self.m.beacon();
        let modes = [
            ("ON", "Beacons and voice beacons; answers scans. Your voice names show up nearby."),
            ("QUIET", "No beacons of either kind; still answers scans and voice scans."),
            ("HIDDEN", "Sends nothing but answers to pinned callers. Only pinned voice names can ring you."),
        ];
        for (i, (m, note)) in modes.iter().enumerate() {
            let y = r.top() + 22.0 + i as f32 * 44.0;
            let k = if cur == *m { Kind::On } else { Kind::Plain };
            if button(ui, rect(x, y, 150.0, 38.0), m, k, true).clicked() && cur != *m {
                self.set(&format!("AT+BEACON={m}"));
            }
            text_fit(
                ui,
                Pos2::new(x + 170.0, y + 19.0),
                Align2::LEFT_CENTER,
                note,
                sans(14.0),
                TEXT3,
                w - 170.0,
            );
        }
        let y = r.top() + 164.0;
        hline(ui, y, x, x + w);
        kicker(
            ui,
            Pos2::new(x, y + 14.0),
            "PINNED NAMES (KEPT, AND THE ONLY ONES THAT RING A HIDDEN MODEM)",
        );
        let pins: Vec<DirLine> = {
            let mut seen: Vec<String> = Vec::new();
            self.dir
                .iter()
                .filter(|d| d.pinned)
                .filter(|d| {
                    let voice = !d.label.is_empty() || d.station != d.name;
                    let key = format!(
                        "{}{}",
                        if voice { "V:" } else { "" },
                        d.name.to_ascii_uppercase()
                    );
                    if seen.contains(&key) {
                        false
                    } else {
                        seen.push(key);
                        true
                    }
                })
                .cloned()
                .collect()
        };
        let rows = 4usize;
        let pages = pins.len().div_ceil(rows).max(1);
        self.pins_page = self.pins_page.min(pages - 1);
        for i in 0..rows {
            let ry = y + 36.0 + i as f32 * 40.0;
            if i % 2 == 1 {
                ui.painter()
                    .rect_filled(rect(x, ry, w * 0.6, 40.0), CornerRadius::ZERO, ZEBRA);
            }
            let Some(d) = pins.get(self.pins_page * rows + i) else {
                continue;
            };
            let voice = !d.label.is_empty() || d.station != d.name;
            text(
                ui,
                Pos2::new(x + 12.0, ry + 20.0),
                Align2::LEFT_CENTER,
                &d.name,
                heavy(15.0),
                TEXT,
            );
            text(
                ui,
                Pos2::new(x + 240.0, ry + 20.0),
                Align2::LEFT_CENTER,
                if voice { "VOICE NAME" } else { "STATION" },
                mono(12.0),
                TEXT2,
            );
            let line = if voice {
                format!("AT+DIR=-V:{}", d.name)
            } else {
                format!("AT+DIR=-{}", d.name)
            };
            if button(
                ui,
                rect(x + w * 0.6 - 110.0, ry + 4.0, 100.0, 32.0),
                if voice { "UNPIN" } else { "DELETE" },
                Kind::Plain,
                true,
            )
            .clicked()
            {
                self.at(T_DIR, &line);
            }
        }
        if pins.is_empty() {
            text(
                ui,
                Pos2::new(x + 12.0, y + 56.0),
                Align2::LEFT_CENTER,
                "Nothing pinned.",
                sans(14.0),
                DIM,
            );
        }
        let py = y + 36.0 + rows as f32 * 40.0 + 6.0;
        if button(
            ui,
            rect(x, py, 88.0, 32.0),
            "PREV",
            Kind::Plain,
            self.pins_page > 0,
        )
        .clicked()
        {
            self.pins_page -= 1;
        }
        if button(
            ui,
            rect(x + 96.0, py, 88.0, 32.0),
            "NEXT",
            Kind::Plain,
            self.pins_page + 1 < pages,
        )
        .clicked()
        {
            self.pins_page += 1;
        }
        // Add a pin.
        let ax = x + w * 0.6 + 24.0;
        let aw = w * 0.4 - 24.0;
        kicker(ui, Pos2::new(ax, y + 46.0), "PIN A NAME");
        field(
            ui,
            rect(ax, y + 70.0, aw, 40.0),
            &mut self.pin_name,
            "pin_name2",
            false,
            "NAME",
            16,
        );
        let half = (aw - 8.0) / 2.0;
        if button(
            ui,
            rect(ax, y + 118.0, half, 34.0),
            "VOICE NAME",
            if self.pin_voice {
                Kind::On
            } else {
                Kind::Plain
            },
            true,
        )
        .clicked()
        {
            self.pin_voice = true;
        }
        if button(
            ui,
            rect(ax + half + 8.0, y + 118.0, half, 34.0),
            "STATION",
            if self.pin_voice {
                Kind::Plain
            } else {
                Kind::On
            },
            true,
        )
        .clicked()
        {
            self.pin_voice = false;
        }
        let n = self.pin_name.trim().to_string();
        if button(
            ui,
            rect(ax, y + 160.0, aw, 40.0),
            "PIN",
            Kind::Primary,
            !n.is_empty(),
        )
        .clicked()
        {
            let l = if self.pin_voice {
                format!("AT+DIR=+V:{n}")
            } else {
                format!("AT+DIR=+{n}")
            };
            self.at(T_DIR, &l);
            self.pin_name.clear();
        }
    }

    fn tab_wifi(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        let (ap_on, ap_ssid, sta) = self.m.wifi_ap();
        kicker(ui, Pos2::new(x, r.top()), "HOTSPOT (AT+WAP)");
        let y = r.top() + 22.0;
        text(
            ui,
            Pos2::new(x, y + 18.0),
            Align2::LEFT_CENTER,
            &format!("{}   {}", if ap_on { "ON" } else { "OFF" }, ap_ssid),
            monob(16.0),
            if ap_on { OK } else { MUTED },
        );
        if button(
            ui,
            rect(x + w - 180.0, y, 180.0, 36.0),
            if ap_on { "TURN OFF" } else { "TURN ON" },
            Kind::Plain,
            true,
        )
        .clicked()
        {
            self.set(if ap_on { "AT+WAP=0" } else { "AT+WAP=1" });
        }
        let y = y + 46.0;
        text(
            ui,
            Pos2::new(x, y - 2.0),
            Align2::LEFT_TOP,
            "Name and password (both are sent together; the password is never shown)",
            sans(13.0),
            MUTED,
        );
        if self.ap_ssid.is_empty() && !ap_ssid.is_empty() {
            self.ap_ssid = ap_ssid.clone();
        }
        field(
            ui,
            rect(x, y + 16.0, 300.0, 36.0),
            &mut self.ap_ssid,
            "ap_ssid",
            false,
            "HOTSPOT NAME",
            32,
        );
        let mut pw = std::mem::take(&mut self.ap_pass);
        field(
            ui,
            rect(x + 312.0, y + 16.0, 300.0, 36.0),
            &mut pw,
            "ap_pass",
            true,
            "PASSWORD, 8 TO 63",
            63,
        );
        let ok = valid_ssid(&self.ap_ssid) && valid_pass(&pw);
        if button(
            ui,
            rect(x + w - 180.0, y + 16.0, 180.0, 36.0),
            "SET HOTSPOT",
            Kind::Plain,
            ok,
        )
        .clicked()
        {
            let line = Zeroizing::new(format!("AT+WAP={},{}", self.ap_ssid, pw.as_str()));
            self.at(T_SET, &line);
            self.refresh_settings();
            pw.zeroize();
            forget_field(ui.ctx(), "ap_pass");
            self.pw_dirty = true;
        }
        self.ap_pass = pw;
        text(
            ui,
            Pos2::new(x, y + 58.0),
            Align2::LEFT_TOP,
            &self.m.wap.clone(),
            mono(12.0),
            TEXT2,
        );

        let y = y + 84.0;
        hline(ui, y, x, x + w);
        kicker(ui, Pos2::new(x, y + 12.0), "CLIENT NETWORK (AT+WSTA)");
        text(
            ui,
            Pos2::new(x, y + 32.0),
            Align2::LEFT_TOP,
            &format!(
                "NOW: {}",
                if sta.is_empty() || sta == "OFF" {
                    "NONE".to_string()
                } else {
                    sta.clone()
                }
            ),
            monob(15.0),
            TEXT2,
        );
        field(
            ui,
            rect(x, y + 52.0, 300.0, 36.0),
            &mut self.sta_ssid,
            "sta_ssid",
            false,
            "NETWORK NAME",
            32,
        );
        let mut pw = std::mem::take(&mut self.sta_pass);
        field(
            ui,
            rect(x + 312.0, y + 52.0, 300.0, 36.0),
            &mut pw,
            "sta_pass",
            true,
            "PASSWORD, 8 TO 63",
            63,
        );
        let ok = valid_ssid(&self.sta_ssid) && valid_pass(&pw);
        if button(
            ui,
            rect(x + w - 180.0, y + 52.0, 180.0, 36.0),
            "JOIN",
            Kind::Plain,
            ok,
        )
        .clicked()
        {
            let line = Zeroizing::new(format!("AT+WSTA={},{}", self.sta_ssid, pw.as_str()));
            self.at(T_SET, &line);
            self.refresh_settings();
            pw.zeroize();
            forget_field(ui.ctx(), "sta_pass");
            self.pw_dirty = true;
        }
        self.sta_pass = pw;
        let has_sta = !(sta.is_empty() || sta == "OFF");
        if button(
            ui,
            rect(x + w - 180.0, y + 94.0, 180.0, 32.0),
            "FORGET",
            Kind::Plain,
            has_sta,
        )
        .clicked()
        {
            self.set("AT+WSTA=0");
        }
        text(
            ui,
            Pos2::new(x, y + 98.0),
            Align2::LEFT_TOP,
            &self.m.wsta.clone(),
            mono(12.0),
            TEXT2,
        );

        let y = y + 136.0;
        hline(ui, y, x, x + w);
        kicker(
            ui,
            Pos2::new(x, y + 12.0),
            "LIVE STATUS (AT+WIFI?, EVERY 2 S)",
        );
        let live = if self.m.wifi.is_empty() {
            "-".to_string()
        } else {
            self.m.wifi.clone()
        };
        text_fit(
            ui,
            Pos2::new(x, y + 32.0),
            Align2::LEFT_TOP,
            &live,
            mono(14.0),
            TEXT,
            w,
        );
        text_wrap(
            ui,
            rect(x, y + 56.0, w, 40.0),
            "THE BUILT-IN SIMULATED MODEM HAS NO WI-FI: these settings do nothing in this app, and no Wi-Fi password is kept on this PC. Names 1 to 32 printable characters, passwords 8 to 63, no comma.",
            sans(12.5),
            DIM,
        );
        // Poll the live line while this tab shows.
        if self.at_open == 0
            && self
                .wifi_poll
                .is_none_or(|t| t.elapsed().as_millis() >= 2000)
        {
            self.wifi_poll = Some(Instant::now());
            self.at(T_WIFI, "AT+WIFI?");
        }
    }

    fn tab_forwards(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        kicker(ui, Pos2::new(x, r.top()), "FORWARDS (AT+FWD): ATDT <NAME> DIALS THEM OVER WI-FI. PUBLISH BRIDGES RADIO CALLERS TO ONE");
        let fwds = self.m.forwards();
        let published = self.m.published();
        let cols = [
            (0.0, "SLOT"),
            (60.0, "NAME"),
            (250.0, "HOST"),
            (520.0, "PORT"),
            (600.0, "MODE"),
        ];
        for (cx, h) in cols {
            text(
                ui,
                Pos2::new(x + cx + 8.0, r.top() + 22.0),
                Align2::LEFT_TOP,
                h,
                mono(11.0),
                DIM,
            );
        }
        let row_h = 32.0;
        for i in 0..8usize {
            let y = r.top() + 40.0 + i as f32 * row_h;
            if i % 2 == 1 {
                ui.painter()
                    .rect_filled(rect(x, y, w, row_h), CornerRadius::ZERO, ZEBRA);
            }
            text(
                ui,
                Pos2::new(x + 8.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                &format!("{}", i + 1),
                mono(12.0),
                DIM,
            );
            let Some((n, h, p, m)) = fwds.get(i).cloned() else {
                text(
                    ui,
                    Pos2::new(x + 68.0, y + row_h / 2.0),
                    Align2::LEFT_CENTER,
                    "EMPTY",
                    mono(12.0),
                    FAINT,
                );
                continue;
            };
            let pubd = published.eq_ignore_ascii_case(&n);
            text_fit(
                ui,
                Pos2::new(x + 68.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                &n,
                heavy(14.0),
                TEXT,
                180.0,
            );
            text_fit(
                ui,
                Pos2::new(x + 258.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                &h,
                mono(12.5),
                TEXT2,
                250.0,
            );
            text(
                ui,
                Pos2::new(x + 528.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                &p,
                mono(12.5),
                TEXT2,
            );
            text(
                ui,
                Pos2::new(x + 608.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                if m == "T" { "TELNET" } else { "RAW" },
                mono(12.5),
                TEXT2,
            );
            let bx = x + w - 3.0 * 96.0;
            if button(
                ui,
                rect(bx, y + 3.0, 88.0, row_h - 6.0),
                "EDIT",
                Kind::Plain,
                true,
            )
            .clicked()
            {
                self.fwd_name = n.clone();
                self.fwd_host = h.clone();
                self.fwd_port = p.clone();
                self.fwd_mode_t = m == "T";
            }
            if button(
                ui,
                rect(bx + 96.0, y + 3.0, 88.0, row_h - 6.0),
                if pubd { "PUBLISHED" } else { "PUBLISH" },
                if pubd { Kind::On } else { Kind::Plain },
                true,
            )
            .clicked()
            {
                let l = if pubd {
                    "AT+PUB=0".to_string()
                } else {
                    format!("AT+PUB={n}")
                };
                self.set(&l);
            }
            if button(
                ui,
                rect(bx + 192.0, y + 3.0, 88.0, row_h - 6.0),
                "DELETE",
                Kind::Plain,
                true,
            )
            .clicked()
            {
                self.set(&format!("AT+FWD={n}"));
            }
        }
        let y = r.top() + 40.0 + 8.0 * row_h + 14.0;
        hline(ui, y - 6.0, x, x + w);
        kicker(
            ui,
            Pos2::new(x, y + 6.0),
            "ADD OR EDIT (A NAME THAT EXISTS IS REPLACED)",
        );
        let fy = y + 28.0;
        field(
            ui,
            rect(x, fy, 190.0, 36.0),
            &mut self.fwd_name,
            "fwd_name",
            false,
            "NAME",
            16,
        );
        field(
            ui,
            rect(x + 198.0, fy, 300.0, 36.0),
            &mut self.fwd_host,
            "fwd_host",
            false,
            "HOST",
            63,
        );
        field(
            ui,
            rect(x + 506.0, fy, 90.0, 36.0),
            &mut self.fwd_port,
            "fwd_port",
            false,
            "PORT",
            5,
        );
        if button(
            ui,
            rect(x + 604.0, fy, 90.0, 36.0),
            "TELNET",
            if self.fwd_mode_t {
                Kind::On
            } else {
                Kind::Plain
            },
            true,
        )
        .clicked()
        {
            self.fwd_mode_t = true;
        }
        if button(
            ui,
            rect(x + 700.0, fy, 70.0, 36.0),
            "RAW",
            if self.fwd_mode_t {
                Kind::Plain
            } else {
                Kind::On
            },
            true,
        )
        .clicked()
        {
            self.fwd_mode_t = false;
        }
        let full = fwds.len() >= 8
            && !fwds
                .iter()
                .any(|f| f.0.eq_ignore_ascii_case(self.fwd_name.trim()));
        let ok = valid_fwd_name(self.fwd_name.trim())
            && valid_host(self.fwd_host.trim())
            && self.fwd_port.trim().parse::<u16>().is_ok_and(|p| p > 0)
            && !full;
        if button(
            ui,
            rect(x + w - 150.0, fy, 150.0, 36.0),
            "SAVE FORWARD",
            Kind::Primary,
            ok,
        )
        .clicked()
        {
            let l = format!(
                "AT+FWD={},{},{},{}",
                self.fwd_name.trim(),
                self.fwd_host.trim(),
                self.fwd_port.trim(),
                if self.fwd_mode_t { "T" } else { "R" }
            );
            self.set(&l);
            self.fwd_name.clear();
            self.fwd_host.clear();
            self.fwd_port = "23".into();
        }
        let msg = if full {
            "All 8 forwards are in use: delete one first.".to_string()
        } else {
            format!(
                "Published: {}. Name 1 to 16 without ':' or ','; host 1 to 63 of A-Z a-z 0-9 . -",
                if published.is_empty() {
                    "NONE"
                } else {
                    &published
                }
            )
        };
        text(
            ui,
            Pos2::new(x, fy + 44.0),
            Align2::LEFT_TOP,
            &msg,
            sans(12.5),
            MUTED,
        );
    }

    fn tab_voice(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        let half = (w - 30.0) / 2.0;
        kicker(
            ui,
            Pos2::new(x, r.top()),
            "MY VOICE NAME AND THIS DEVICE'S LABEL",
        );
        let y = r.top() + 24.0;
        field(
            ui,
            rect(x, y, 200.0, 40.0),
            &mut self.my_name,
            "my_name",
            false,
            "NAME",
            16,
        );
        field(
            ui,
            rect(x + 208.0, y, 110.0, 40.0),
            &mut self.my_label,
            "my_label",
            false,
            "LABEL",
            8,
        );
        let changed = !self.my_name.trim().eq_ignore_ascii_case(&self.s.name)
            || self.my_label.trim() != self.s.label;
        if button(
            ui,
            rect(x + 326.0, y, half - 326.0, 40.0),
            "CHANGE",
            Kind::Plain,
            changed && !self.s.in_call,
        )
        .clicked()
        {
            let (n, l) = (
                self.my_name.trim().to_string(),
                self.my_label.trim().to_string(),
            );
            self.cmd(Cmd::Rename { name: n, label: l });
        }
        let key = self.s.device_key;
        let fp: String = key
            .iter()
            .take(8)
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|c| c.concat())
            .collect::<Vec<_>>()
            .join(" ");
        text(
            ui,
            Pos2::new(x, y + 50.0),
            Align2::LEFT_TOP,
            &format!("NOW {}/{}   DEVICE KEY {fp}", self.s.name, self.s.label),
            mono(12.0),
            TEXT2,
        );
        text(
            ui,
            Pos2::new(x, y + 70.0),
            Align2::LEFT_TOP,
            if self.s.has_person {
                "This device holds your person key (it can link others)."
            } else {
                "No person key on this device."
            },
            sans(12.5),
            MUTED,
        );

        // Linking (right column).
        let lx = x + half + 30.0;
        kicker(ui, Pos2::new(lx, r.top()), "LINK YOUR DEVICES (SPEC 7.2)");
        if let Some(code) = &self.link_code {
            // "123 456", built at its final size so it never reallocates.
            let mut shown = Zeroizing::new(String::with_capacity(7));
            for (i, ch) in code.chars().enumerate() {
                if i == 3 {
                    shown.push(' ');
                }
                shown.push(ch);
            }
            text(
                ui,
                Pos2::new(lx, y - 2.0),
                Align2::LEFT_TOP,
                &shown,
                monob(40.0),
                TEXT,
            );
            if button(
                ui,
                rect(lx + half - 120.0, y, 120.0, 40.0),
                "CANCEL",
                Kind::Plain,
                true,
            )
            .clicked()
            {
                self.cmd(Cmd::CancelLink);
                if !link_code_stays(true, Some(LinkSig::Cancelled), self.s.linking) {
                    self.link_code = None;
                }
                self.link_note = "Cancelled. The code is gone.".into();
            }
        } else {
            let bw = (half - 8.0) / 2.0;
            if button(
                ui,
                rect(lx, y, bw, 40.0),
                "LINK A DEVICE",
                Kind::Primary,
                self.s.has_person,
            )
            .clicked()
            {
                self.cmd(Cmd::LinkOffer);
            }
            let mut code = std::mem::take(&mut self.join_code);
            field(
                ui,
                rect(lx + bw + 8.0, y, bw - 88.0, 40.0),
                &mut code,
                "join_code",
                false,
                "CODE",
                7,
            );
            let ok = code.chars().filter(char::is_ascii_digit).count() == 6;
            if button(
                ui,
                rect(lx + half - 80.0, y, 80.0, 40.0),
                "JOIN",
                Kind::Plain,
                ok,
            )
            .clicked()
            {
                let c: Zeroizing<String> =
                    Zeroizing::new(code.chars().filter(char::is_ascii_digit).collect());
                self.cmd(Cmd::LinkJoin(c));
                code.zeroize();
                forget_field(ui.ctx(), "join_code");
                self.link_note =
                    "Now call your other device by name: the link runs inside that call.".into();
            }
            self.join_code = code;
        }
        let note = if self.link_note.is_empty() {
            "LINK A DEVICE shows a code here. On the new device (LINK TO MY NAME) type that code in CODE, press JOIN, then call this device."
        } else {
            &self.link_note.clone()
        };
        text_wrap(ui, rect(lx, y + 50.0, half, 44.0), note, sans(12.5), MUTED);

        // Checked contacts: one row per checked device, paged.
        let y = y + 108.0;
        hline(ui, y, x, x + w);
        kicker(
            ui,
            Pos2::new(x, y + 12.0),
            "CHECKED CONTACTS: EACH DEVICE WHOSE WORDS YOU CHECKED",
        );
        let me = self.s.name.clone();
        let mut rows: Vec<(String, String, [u8; 32], bool, bool)> = Vec::new();
        for c in &self.s.contacts {
            let mine = c.name.eq_ignore_ascii_case(&me);
            for (l, k) in &c.devices {
                rows.push((c.name.clone(), l.clone(), *k, c.person, mine));
            }
            if c.devices.is_empty() && c.person {
                rows.push((c.name.clone(), "(person key)".into(), [0; 32], true, mine));
            }
        }
        let per = 5usize;
        let pages = rows.len().div_ceil(per).max(1);
        self.contacts_page = self.contacts_page.min(pages - 1);
        let rh = 36.0;
        for i in 0..per {
            let ry = y + 34.0 + i as f32 * rh;
            if i % 2 == 1 {
                ui.painter()
                    .rect_filled(rect(x, ry, w, rh), CornerRadius::ZERO, ZEBRA);
            }
            let Some((n, l, k, person, mine)) = rows.get(self.contacts_page * per + i).cloned()
            else {
                continue;
            };
            text(
                ui,
                Pos2::new(x + 10.0, ry + rh / 2.0),
                Align2::LEFT_CENTER,
                &format!("{n}/{l}"),
                heavy(15.0),
                TEXT,
            );
            let kf: String = k.iter().take(6).map(|b| format!("{b:02X}")).collect();
            let tag = format!(
                "KEY {kf}{}{}",
                if person { "   PERSON KEY CHECKED" } else { "" },
                if mine { "   ONE OF YOURS" } else { "" }
            );
            text(
                ui,
                Pos2::new(x + 260.0, ry + rh / 2.0),
                Align2::LEFT_CENTER,
                &tag,
                mono(12.0),
                TEXT2,
            );
            if k != [0; 32] {
                let label = if mine { "REMOVE DEVICE" } else { "UNCHECK" };
                if button(
                    ui,
                    rect(x + w - 160.0, ry + 4.0, 160.0, rh - 8.0),
                    label,
                    Kind::Plain,
                    true,
                )
                .clicked()
                {
                    if mine {
                        self.cmd(Cmd::RemoveMyDevice(k));
                    } else {
                        self.cmd(Cmd::RemoveContactDevice(n.clone(), k));
                    }
                }
            }
        }
        if rows.is_empty() {
            text(
                ui,
                Pos2::new(x + 10.0, y + 54.0),
                Align2::LEFT_CENTER,
                "Nobody checked yet: press WORDS MATCH in a call.",
                sans(14.0),
                DIM,
            );
        }
        let py = y + 34.0 + per as f32 * rh + 6.0;
        if button(
            ui,
            rect(x, py, 88.0, 32.0),
            "PREV",
            Kind::Plain,
            self.contacts_page > 0,
        )
        .clicked()
        {
            self.contacts_page -= 1;
        }
        if button(
            ui,
            rect(x + 96.0, py, 88.0, 32.0),
            "NEXT",
            Kind::Plain,
            self.contacts_page + 1 < pages,
        )
        .clicked()
        {
            self.contacts_page += 1;
        }
        let rm = self.s.removed_mine.len();
        text_wrap(
            ui,
            rect(x + 200.0, py, w - 200.0, 44.0),
            &format!(
                "REMOVE DEVICE (a lost device of yours) is told to each contact at your next call with them, signed by your person key, and kept in removed.json ({rm} so far). UNCHECK forgets a contact's device here."
            ),
            sans(12.0),
            DIM,
        );
    }

    fn tab_about(&mut self, ui: &mut Ui, r: Rect) {
        let (x, w) = (r.left(), r.width());
        if let Some(t) = &self.logo {
            let lr = rect(x, r.top(), 100.0, 100.0);
            ui.painter().image(
                t.id(),
                lr,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let tx = x + 120.0;
        let tw = w - 120.0;
        kicker(ui, Pos2::new(tx, r.top()), "ABOUT");
        text_fit(
            ui,
            Pos2::new(tx, r.top() + 14.0),
            Align2::LEFT_TOP,
            PRODUCT,
            heavy(22.0),
            TEXT,
            tw,
        );
        text_fit(
            ui,
            Pos2::new(tx, r.top() + 42.0),
            Align2::LEFT_TOP,
            TAGLINE_1,
            heavy(18.0),
            ACCENT,
            tw,
        );
        text_fit(
            ui,
            Pos2::new(tx, r.top() + 64.0),
            Align2::LEFT_TOP,
            TAGLINE_2,
            sans(16.0),
            TEXT2,
            tw,
        );
        let rev = text(
            ui,
            Pos2::new(tx, r.top() + 86.0),
            Align2::LEFT_TOP,
            "Rev. J. Money",
            heavy(16.0),
            TEXT,
        );
        text(
            ui,
            Pos2::new(rev.right() + 16.0, rev.center().y),
            Align2::LEFT_CENTER,
            ATTRIBUTION,
            mono(12.5),
            TEXT2,
        );
        let y = r.top() + 112.0;
        hline(ui, y, x, x + w);
        let col = (w - 24.0) / 2.0;
        kicker(ui, Pos2::new(x, y + 10.0), "ATI");
        text_wrap(
            ui,
            rect(x, y + 28.0, col, 50.0),
            &self.m.ati.clone(),
            mono(12.0),
            TEXT2,
        );
        kicker(ui, Pos2::new(x, y + 80.0), "THIS APP");
        let app = format!(
            "lfd3k-voice-pc {}, GPL-3.0-or-later (LICENSE, NOTICE.md, THIRD_PARTY.md)\nSound: {}\nData: {}\nFonts: Archivo and IBM Plex Mono (SIL Open Font License 1.1)",
            env!("CARGO_PKG_VERSION"),
            if self.audio.note.is_empty() { "none (WAV test helpers)" } else { &self.audio.note },
            self.args.data.display()
        );
        text_wrap(ui, rect(x, y + 98.0, col, 106.0), &app, mono(11.5), TEXT2);
        // Connection sounds: live chirps from the modem's frames while a
        // call is set up (and at NO CARRIER).
        let on = self.sounds;
        if button(
            ui,
            rect(x, y + 208.0, 250.0, 30.0),
            if on {
                "CONNECTION SOUNDS ON"
            } else {
                "CONNECTION SOUNDS OFF"
            },
            if on { Kind::On } else { Kind::Plain },
            true,
        )
        .clicked()
        {
            self.set_sounds(!on);
        }
        text_fit(
            ui,
            Pos2::new(x + 262.0, y + 223.0),
            Align2::LEFT_CENTER,
            "Chirps while a call is set up",
            sans(12.5),
            MUTED,
            col - 262.0,
        );
        text_wrap(
            ui,
            rect(x, y + 246.0, col, 40.0),
            "No ham mode yet, so the FLOWTRON SCRAMBLER is always on. Use a headset for CALL mode: there is no echo canceller in this version.",
            sans(12.5),
            MUTED,
        );
        let ax = x + col + 24.0;
        kicker(ui, Pos2::new(ax, y + 10.0), "AT&V");
        text_wrap(
            ui,
            rect(ax, y + 28.0, col, r.bottom() - y - 30.0),
            &self.m.atv.clone(),
            mono(11.5),
            TEXT2,
        );
    }

    /// Connection sounds on or off (the modem thread saves it).
    fn set_sounds(&mut self, on: bool) {
        self.sounds = on;
        self.cmd(Cmd::Sounds(on));
    }

    // ---- The script (test helper).
    //
    // One command per line, `#` comments:
    // `wait <ms>`; `until <cond> [<ms>]` (ringing, connected, slots,
    // floor_mine, floor_theirs, floor_free, idle, at_idle, unsaved, saved,
    // heard, dir, dir:<NAME>, sent:<n>, heard:<n>); `shot <name>` (PNG of
    // the window into --shots); `screen calls|dir|settings`; `tab radio|
    // privacy|wifi|forwards|voice|about`; `call NAME[/LABEL]`; `answer`;
    // `decline`; `hangup`; `words_match`; `talk_anyway`; `talk down|up`;
    // `mute on|off`; `set <AT line>` (a settings change); `at <AT line>`;
    // `save`; `discard`; `scan`; `dir`; `link_offer`; `fill <field>
    // <text>`; `fake ringing|conflict|answered_elsewhere|declined|link_code|
    // full_dir|real_dir` (screen states only, nothing is sent); `sounds
    // on|off` (connection sounds); `print`
    // (state to stdout); `size <w> <h>` (window size in points);
    // `link_code_file <path>` / `link_join_file <path>` (hand a link code to
    // another copy's script; conditions link_code, linked, not_linking;
    // link_code_file writes the code, a secret, in plain text to a file, so
    // it runs only in a debug build or with --script-unsafe);
    // `sync <file> [<ms>]` (two copies meet at one wall clock go mark: the
    // first to get here writes now + ms, default 3000, to the file, the
    // other reads it; both wait for it); `at_t <ms>` (wait until the go
    // mark + ms); `play <from_ms> <to_ms>` (play that part of the --play
    // WAV from now, once; the WAV stops looping); `quit`.

    fn run_script(&mut self, ctx: &egui::Context) {
        let Some(sc) = self.script.as_mut() else {
            return;
        };
        if sc.shot_pending.load(Ordering::SeqCst) > 0 {
            ctx.request_repaint();
            return;
        }
        if let Some(t) = sc.wait_until {
            if Instant::now() < t {
                ctx.request_repaint_after(std::time::Duration::from_millis(20));
                return;
            }
            sc.wait_until = None;
        }
        if let Some((cond, deadline)) = sc.until.clone() {
            if self.cond(&cond) {
                if let Some(sc) = self.script.as_mut() {
                    println!("[{:7.1}] ok until {cond}", sc.t0.elapsed().as_secs_f32());
                    sc.until = None;
                }
            } else if Instant::now() > deadline {
                println!("[script] until {cond}: TIMED OUT");
                if let Some(sc) = self.script.as_mut() {
                    sc.until = None;
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(20));
                return;
            }
        }
        let Some(sc) = self.script.as_mut() else {
            return;
        };
        let Some(line) = sc.lines.pop_front() else {
            println!("[script] done");
            self.script = None;
            return;
        };
        let t = sc.t0.elapsed().as_secs_f32();
        println!("[{t:7.1}] {}", mask_line(&line));
        let (c, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let rest = rest.trim().to_string();
        match c {
            "wait" => {
                let ms: u64 = rest.parse().unwrap_or(0);
                if let Some(sc) = self.script.as_mut() {
                    sc.wait_until = Some(Instant::now() + std::time::Duration::from_millis(ms));
                }
            }
            "until" => {
                let (cond, ms) = rest.rsplit_once(' ').unwrap_or((rest.as_str(), "30000"));
                let ms: u64 = ms.parse().unwrap_or(30_000);
                let cond = cond.to_string();
                if let Some(sc) = self.script.as_mut() {
                    sc.until = Some((cond, Instant::now() + std::time::Duration::from_millis(ms)));
                }
            }
            "shot" => {
                if let Some(sc) = self.script.as_mut() {
                    let path = sc.shots.join(format!("{rest}.png"));
                    let pending = sc.shot_pending.clone();
                    pending.fetch_add(1, Ordering::SeqCst);
                    let c2 = ctx.clone();
                    ctx.request_screenshot(move |img| {
                        let r = save_png(&path, &img);
                        println!(
                            "[script] shot {} {}x{} {}",
                            path.display(),
                            img.size[0],
                            img.size[1],
                            r.err().unwrap_or_default()
                        );
                        pending.fetch_sub(1, Ordering::SeqCst);
                        c2.request_repaint();
                    });
                }
            }
            "screen" => {
                self.screen = match rest.as_str() {
                    "dir" => Screen::Dir,
                    "settings" => Screen::Settings,
                    _ => Screen::Calls,
                }
            }
            "tab" => {
                self.screen = Screen::Settings;
                self.tab = match rest.as_str() {
                    "privacy" => Tab::Privacy,
                    "wifi" => Tab::Wifi,
                    "forwards" => Tab::Forwards,
                    "voice" => Tab::Voice,
                    "about" => Tab::About,
                    _ => Tab::Radio,
                }
            }
            "call" => {
                let (n, l) = rest.split_once('/').unwrap_or((rest.as_str(), ""));
                self.dial_name = n.to_string();
                self.dial_label = l.to_string();
                let (n, l) = (n.to_string(), l.to_string());
                self.do_call(&n, Some(&l));
            }
            "answer" => self.do_answer(),
            "decline" => self.do_decline(),
            "hangup" => self.do_hangup(),
            "words_match" => self.do_words_match(),
            "talk_anyway" => self.do_talk_anyway(),
            "talk" => self.script_talk = rest == "down",
            "mute" => {
                self.muted = rest == "on";
                self.cmd(Cmd::Mute(self.muted));
            }
            "set" => self.set(&rest),
            "at" => self.at(0, &rest),
            "save" => self.do_save(),
            "discard" => self.do_discard(),
            "scan" => {
                self.m.scanning = true;
                self.at(T_SCAN, "AT+SCAN");
            }
            "dir" => self.cmd(Cmd::RefreshDir),
            "link_offer" => self.cmd(Cmd::LinkOffer),
            // Two copies in one test: hand the code shown here to the
            // other copy's script through a file (it deletes the file).
            "link_code_file" if !(cfg!(debug_assertions) || self.args.script_unsafe) => {
                println!("[script] link_code_file needs a debug build or --script-unsafe");
            }
            "link_code_file" => {
                if let Some(c) = &self.link_code {
                    if let Err(e) = std::fs::write(&rest, c.as_bytes()) {
                        println!("[script] {rest}: {e}");
                    }
                } else {
                    println!("[script] no link code shown");
                }
            }
            "link_join_file" => match std::fs::read_to_string(&rest) {
                Ok(c) => {
                    let _ = std::fs::remove_file(&rest);
                    let c = Zeroizing::new(c);
                    self.cmd(Cmd::LinkJoin(Zeroizing::new(c.trim().to_string())));
                    self.link_note = "Joining: call your other device by name.".into();
                }
                Err(e) => println!("[script] {rest}: {e}"),
            },
            "fill" => {
                let (f, v) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                match f {
                    "ap_pass" => *self.ap_pass = v.to_string(),
                    "sta_ssid" => self.sta_ssid = v.to_string(),
                    "sta_pass" => *self.sta_pass = v.to_string(),
                    "fwd_name" => self.fwd_name = v.to_string(),
                    "fwd_host" => self.fwd_host = v.to_string(),
                    "pin_name" => self.pin_name = v.to_string(),
                    _ => println!("[script] no field {f}"),
                }
            }
            "fake" => self.fake(&rest),
            "sounds" => self.set_sounds(rest == "on"),
            "print" => self.print_state(),
            "size" => {
                let (w, h) = rest.split_once(' ').unwrap_or(("1280", "720"));
                let v = Vec2::new(w.parse().unwrap_or(1280.0), h.parse().unwrap_or(720.0));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(v));
            }
            "sync" => {
                let (f, ms) = rest.split_once(' ').unwrap_or((rest.as_str(), "3000"));
                let go = sync_mark(f, ms.parse().unwrap_or(3000));
                println!("[script] go {go}");
                if let Some(sc) = self.script.as_mut() {
                    sc.go = Some(go);
                    sc.wait_until = Some(instant_at(go));
                }
            }
            "at_t" => {
                if let Some(sc) = self.script.as_mut() {
                    let ms: u64 = rest.parse().unwrap_or(0);
                    if let Some(go) = sc.go {
                        sc.wait_until = Some(instant_at(go + ms));
                    }
                }
            }
            "play" => {
                let (a, b) = rest.split_once(' ').unwrap_or((rest.as_str(), "0"));
                self.cmd(Cmd::PlaySeg {
                    from_ms: a.parse().unwrap_or(0),
                    to_ms: b.parse().unwrap_or(0),
                });
            }
            "quit" => {
                self.stopping = true;
                self.cmd(Cmd::Quit);
            }
            _ => println!("[script] unknown: {line}"),
        }
    }

    fn cond(&self, c: &str) -> bool {
        let st = self.s.stats;
        match c {
            "ringing" => self.ringing.is_some(),
            "connected" => self.call.words.is_some(),
            "slots" => self.s.in_call && st.slots_ms.is_some(),
            "floor_mine" => self.s.floor == Floor::Mine,
            "floor_theirs" => self.s.floor == Floor::Theirs,
            "floor_free" => self.s.floor == Floor::Free,
            "idle" => !self.s.in_call && self.ringing.is_none(),
            "at_idle" => self.at_open == 0,
            "unsaved" => self.unsaved(),
            "saved" => self.baseline.is_some() && !self.unsaved(),
            "heard" => st.frames_heard > 0,
            "dir" => !self.dir.is_empty(),
            "link_code" => self.link_code.is_some(),
            "linked" => self.link_note.starts_with("Linked"),
            "not_linking" => !self.s.linking,
            _ => {
                if let Some(n) = c.strip_prefix("dir:") {
                    self.dir.iter().any(|d| d.name.eq_ignore_ascii_case(n))
                } else if let Some(n) = c.strip_prefix("sent:") {
                    st.frames_sent >= n.parse().unwrap_or(u32::MAX)
                } else if let Some(n) = c.strip_prefix("heard:") {
                    st.frames_heard >= n.parse().unwrap_or(u32::MAX)
                } else {
                    false
                }
            }
        }
    }

    fn print_state(&self) {
        let st = self.s.stats;
        println!(
            "[state] loop passes {} max gap {} ms; me {}/{} station {} in_call {} peer {:?} sf {} plan {} words {:?} trust {:?} audio_ok {} floor {:?} sent {} heard {} dropped {} concealed {} rec {:.1}s play {:.1}s unsaved {}",
            self.s.passes,
            self.s.max_gap_ms,
            self.s.name,
            self.s.label,
            self.s.station,
            self.s.in_call,
            self.s.peer,
            st.sf,
            plan_line(st.sf, st.plan),
            self.call.words,
            self.call.trust,
            self.call.audio_ok,
            self.s.floor,
            st.frames_sent,
            st.frames_heard,
            st.frames_dropped,
            st.frames_concealed,
            self.s.rec_s,
            self.s.play_s,
            self.unsaved()
        );
        println!(
            "[state] ring tone {:.1} s, connection sounds {} ({:.1} s made), linking {}, link code shown {}",
            self.s.ring_s,
            if self.sounds { "on" } else { "off" },
            self.s.chirp_s,
            self.s.linking,
            self.link_code.is_some()
        );
        println!(
            "[state] S30={:?} S21={:?} beacon {} name {}",
            self.m.sreg(30),
            self.m.sreg(21),
            self.m.beacon(),
            self.m.name()
        );
        for d in &self.dir {
            println!(
                "[state] dir {} /{} via {} {:?} pin {}",
                d.name, d.label, d.station, d.best_tier, d.pinned
            );
        }
    }

    /// Show states two copies cannot easily reach, for screenshots only
    /// (the script's `fake`; nothing is sent).
    fn fake(&mut self, what: &str) {
        match what {
            "ringing" => {
                self.ringing = Some(("ANN".into(), "PHONE".into(), "AXISMODEM-E4B0".into()))
            }
            "conflict" => {
                self.call.trust = Some(Trust::Conflict);
                self.call.audio_ok = false;
                self.call.words = Some(["WRENCH", "TANGO"]);
            }
            "answered_elsewhere" => self.banner("ANSWERED ELSEWHERE", false),
            "declined" => self.banner("DECLINED", false),
            "link_code" => {
                self.link_code = Some(Zeroizing::new("042917".into()));
                self.link_note =
                    "Type this code on your other device (Link to my name), then call it.".into();
            }
            "full_dir" => {
                self.fake_dir = true;
                let names = [
                    "ANN", "BEN", "CARLA", "DMITRI", "EVE", "FRANKIE", "GUS", "HAL", "IVY", "JUNE",
                    "KARL",
                ];
                self.dir = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| DirLine {
                        name: (*n).into(),
                        label: if i < 8 {
                            ["PC", "PHONE"][i % 2].into()
                        } else {
                            String::new()
                        },
                        station: if i < 8 {
                            format!("AXISMODEM-{:04X}", 0x16FC + i * 7)
                        } else {
                            (*n).into()
                        },
                        best_tier: Tier::from_index((7 - i % 8) as u8),
                        mode_hint: lfd3k_core::voice::talk_plan(
                            Tier::from_index((7 - i % 8) as u8).unwrap_or(Tier::Sf12),
                        )
                        .mode,
                        trust: if i % 3 == 0 {
                            Trust::Checked
                        } else {
                            Trust::NotChecked
                        },
                        pinned: i % 4 == 1,
                    })
                    .collect();
            }
            "real_dir" => {
                self.fake_dir = false;
                self.cmd(Cmd::RefreshDir);
            }
            _ => println!("[script] no fake {what}"),
        }
    }
}

/// The script's `sync`: the go mark in the file, or now + `ms` written to
/// it by the first copy to get there.
fn sync_mark(file: &str, ms: u64) -> u64 {
    if let Some(go) = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| t.trim().parse().ok())
    {
        return go;
    }
    let go = crate::trace::unix_ms() + ms;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = write!(f, "{go}");
            go
        }
        // The other copy made it just now.
        Err(_) => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::read_to_string(file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(go)
        }
    }
}

/// The Instant of a wall clock time (Unix ms); now when it has passed.
fn instant_at(unix: u64) -> Instant {
    let now = crate::trace::unix_ms();
    Instant::now() + std::time::Duration::from_millis(unix.saturating_sub(now))
}

/// A script line as it may be printed: a password after `fill *_pass`,
/// and after the comma of `AT+WAP=` or `AT+WSTA=`, shows as `********`.
pub(crate) fn mask_line(line: &str) -> String {
    let mut words = line.splitn(3, ' ');
    if let (Some("fill"), Some(f)) = (words.next(), words.next()) {
        if f.ends_with("_pass") {
            return format!("fill {f} ********");
        }
    }
    let up = line.to_ascii_uppercase();
    for key in ["AT+WAP=", "AT+WSTA="] {
        if let Some(at) = up.find(key) {
            if let Some(c) = line.get(at..).and_then(|r| r.find(',')) {
                return format!("{},********", line.get(..at + c).unwrap_or(""));
            }
        }
    }
    line.to_string()
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(f),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let data: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
    w.write_image_data(&data).map_err(|e| e.to_string())
}

fn valid_ssid(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b',')
}

fn valid_pass(s: &str) -> bool {
    (8..=63).contains(&s.len()) && s.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b',')
}

fn valid_fwd_name(s: &str) -> bool {
    (1..=16).contains(&s.len())
        && s != "0"
        && s.bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b':' && b != b',')
}

fn valid_host(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        if self.stopped {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Closing the window: Leave the modem session first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.stopped {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.stopping {
                self.stopping = true;
                self.cmd(Cmd::Quit);
            }
        }
        let full = ui.max_rect();
        ui.painter().rect_filled(full, CornerRadius::ZERO, BG);
        let mut y = full.top();
        self.header(ui, rect(full.left(), y, full.width(), 64.0));
        y += 64.0;
        if self.unsaved() {
            self.save_bar(ui, rect(full.left(), y, full.width(), 44.0));
            y += 44.0;
        }
        self.banner_row(ui, rect(full.left(), y, full.width(), 38.0));
        y += 38.0;
        let mon = rect(full.left(), full.bottom() - MON_H, full.width(), MON_H);
        self.monitor(ui, mon);
        let content = Rect::from_min_max(
            Pos2::new(full.left() + 20.0, y),
            Pos2::new(full.right() - 20.0, mon.top() - 12.0),
        );
        match self.screen {
            Screen::Calls => self.calls(ui, content),
            Screen::Dir => {
                self.talk_level(&ctx, false);
                self.directory(ui, content)
            }
            Screen::Settings => {
                self.talk_level(&ctx, false);
                self.settings(ui, content)
            }
        }
        self.run_script(&ctx);
        // Live numbers and the moving scope: redraw 20 times a second.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// Why a shown link code may go (see `link_code_stays`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LinkSig {
    /// The modem thread says the engine stopped linking (`Ev::LinkEnded`).
    Ended,
    Linked,
    Failed,
    /// The user pressed CANCEL.
    Cancelled,
}

/// Whether a link code (shown or not, `shown`) is still shown after `sig`
/// (None: a pass of the window with no link event). Only an explicit end
/// clears it. `snapshot_linking` (the modem thread's last snapshot) is
/// deliberately not used: it lags the events, so a code that just came
/// would vanish while the snapshot still says "not linking" (the bug fixed
/// in 496dc47).
fn link_code_stays(shown: bool, sig: Option<LinkSig>, snapshot_linking: bool) -> bool {
    let _ = snapshot_linking;
    match sig {
        Some(LinkSig::Ended | LinkSig::Linked | LinkSig::Failed | LinkSig::Cancelled) => false,
        None => shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_link_code_stays_until_an_explicit_end() {
        // 496dc47: the code stays on screen whatever the (lagging) snapshot
        // says, until LinkEnded, Linked, a link failure or CANCEL.
        for snap in [false, true] {
            assert!(
                link_code_stays(true, None, snap),
                "a pass with snapshot linking {snap}"
            );
            for sig in [
                LinkSig::Ended,
                LinkSig::Linked,
                LinkSig::Failed,
                LinkSig::Cancelled,
            ] {
                assert!(!link_code_stays(true, Some(sig), snap), "{sig:?}");
            }
        }
        assert!(
            !link_code_stays(false, None, true),
            "nothing shown stays nothing"
        );
        // The code just came (Ev::LinkCode) and the snapshot still says not
        // linking: many passes later it is still shown.
        let mut shown = true;
        for _ in 0..100 {
            shown = link_code_stays(shown, None, false);
        }
        assert!(shown);
    }
}
