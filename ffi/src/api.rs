//! The C ABI the firmware's modem task calls.
//!
//! One static holder owns the `Modem` and the `Sx1262<HalBus>`. Every entry
//! point except `lfd3k_init` and `lfd3k_core_version` returns early (with
//! zero, false or -1) until `lfd3k_init` has run.
//!
//! The `Modem` is large (over 11 KB), so it is never built on the caller's
//! stack: the static starts as the constant `Modem::blank()` and
//! `lfd3k_init` resets it in place (`Modem::reset` assigns field by field).

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};

use lfd3k_core::modem::WIFI_STATUS_MAX;
use lfd3k_core::settings::{valid_pass, BLOB_LEN, HOST_MAX, PASS_MAX, SSID_MAX};
use lfd3k_core::{Config, Modem, Settings};
use sx1262::{Error, Sx1262};

use crate::hal::HalBus;
use crate::panic::NOTE_LEN;

/// `lfd3k_init` result for a name that is not 1 to 16 printable ASCII
/// characters (not NUL terminated within 17 bytes counts too).
const ERR_NAME: c_int = -4;
/// Frames drained per `lfd3k_poll` at most. A bound so one poll cannot run
/// long if the radio keeps reporting frames; the firmware polls again on the
/// next DIO1 edge or 10 ms tick, so nothing is lost by stopping at 8.
const RX_PER_POLL: usize = 8;

/// Samples in the channel trace ring (matches `RF_TRACE_N` in the firmware).
const TRACE_N: usize = 64;
/// Milliseconds between trace samples.
const TRACE_MS: u32 = 50;

/// Fixed ring of channel samples for the idle scope, oldest overwritten.
struct Trace {
    dbm: [i8; TRACE_N],
    tx: [u8; TRACE_N],
    /// Next slot to write.
    head: usize,
    count: usize,
    /// `now_ms` of the last sample; `None` until the first.
    last_ms: Option<u32>,
}

impl Trace {
    const fn new() -> Self {
        Trace {
            dbm: [0; TRACE_N],
            tx: [0; TRACE_N],
            head: 0,
            count: 0,
            last_ms: None,
        }
    }

    fn push(&mut self, dbm: i8, tx: u8) {
        if let Some(s) = self.dbm.get_mut(self.head) {
            *s = dbm;
        }
        if let Some(s) = self.tx.get_mut(self.head) {
            *s = tx;
        }
        self.head = (self.head + 1) % TRACE_N;
        self.count = (self.count + 1).min(TRACE_N);
    }

    /// Take one sample if 50 ms have passed since the last one.
    fn sample(&mut self, radio: &mut Sx1262<HalBus>, now_ms: u32) {
        if let Some(last) = self.last_ms {
            if now_ms.wrapping_sub(last) < TRACE_MS {
                return;
            }
        }
        if radio.transmitting() {
            self.last_ms = Some(now_ms);
            self.push(0, 1);
        } else if let Some(r) = radio.rssi_inst() {
            self.last_ms = Some(now_ms);
            self.push(r.clamp(-128, 0) as i8, 0);
        }
    }
}

struct Device {
    modem: Modem,
    radio: Sx1262<HalBus>,
    /// Last S30 value given to the radio.
    power: i8,
    trace: Trace,
}

/// The static slot. `dev` always holds a valid (blank) device; `live` says
/// whether `lfd3k_init` has configured it.
struct Slot {
    live: bool,
    dev: Device,
}

/// The one global. Not live until `lfd3k_init` has run.
struct Holder(UnsafeCell<Slot>);

// SAFETY: the C ABI contract says every entry point below is called only
// from the one modem task (never from an ISR or another task), so the cell
// is never accessed from two threads. The firmware's DIO1 ISR only notifies
// that task. Nothing here hands out a reference that outlives one call.
unsafe impl Sync for Holder {}

static HOLDER: Holder = Holder(UnsafeCell::new(Slot {
    live: false,
    dev: Device {
        modem: Modem::blank(),
        radio: Sx1262::new(HalBus),
        power: 0,
        trace: Trace::new(),
    },
}));

/// Give the radio the current S30 power if it changed (ATS30=, ATZ).
fn sync_power(d: &mut Device) {
    let p = d.modem.tx_power_dbm();
    if p != d.power {
        d.power = p;
        // A failure marks the driver failed; AT&V and status show it.
        let _ = d.radio.set_power(p);
    }
}

/// Run `f` on the device, or return `None` before init.
fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
    // SAFETY: single-task contract (see `Holder`). The `&mut` lives only for
    // this call, and `f` cannot call back into an entry point, so it is the
    // only live reference to the cell contents.
    let slot = unsafe { &mut *HOLDER.0.get() };
    if !slot.live {
        return None;
    }
    Some(f(&mut slot.dev))
}

/// Copies the core version string (no NUL terminator) into `out`, truncated
/// to `len` bytes, and returns the number of bytes written. A null `out`
/// writes nothing and returns 0.
///
/// # Safety
/// `out` must be null or valid for writes of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_core_version(out: *mut u8, len: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let v = lfd3k_core::VERSION.as_bytes();
    let n = v.len().min(len);
    // SAFETY: the caller guarantees `out` is valid for `len` bytes and
    // `n <= len`; `v` is a distinct, live static string.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n
}

fn error_code(e: Error) -> i8 {
    match e {
        Error::BusyTimeout => -1,
        Error::NoChip => -2,
        Error::Config => -3,
    }
}

/// Reads a NUL terminated station name of at most 16 bytes.
///
/// # Safety
/// `p` must be null or point to a NUL terminated string, or to at least 17
/// readable bytes.
unsafe fn read_name(p: *const c_char, buf: &mut [u8; 17]) -> Option<&str> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    while n < buf.len() {
        // SAFETY: the caller guarantees the bytes up to the NUL (or 17 of
        // them) are readable; we stop at the first NUL.
        let b = unsafe { *p.add(n) } as u8;
        if b == 0 {
            return core::str::from_utf8(buf.get(..n)?).ok();
        }
        *buf.get_mut(n)? = b;
        n += 1;
    }
    None
}

/// Creates the modem (station `name`, default S registers), shows the
/// previous boot's crash note on ATI, and brings up the radio at S30 power.
/// Replaces any earlier state. Returns 0, or a negative code: -1 radio busy
/// timeout, -2 no chip, -3 radio config, -4 bad name (nothing is created).
/// A radio failure still leaves a working modem (AT commands run, AT&V shows
/// `RADIO FAIL <code> ERR 0x<errors>`).
///
/// # Safety
/// `name` must be null or a NUL terminated string (or 17 readable bytes).
/// `last_panic` must be null or valid for reads of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_init(
    name: *const c_char,
    last_panic: *const u8,
    len: usize,
) -> c_int {
    // SAFETY: single-task contract (see `Holder`); this is the only live
    // reference to the slot for the whole function (nothing below calls back
    // into an entry point).
    let slot = unsafe { &mut *HOLDER.0.get() };
    // A failed re-init leaves no stale state: not live until configured.
    slot.live = false;
    let mut nbuf = [0u8; 17];
    // SAFETY: forwarded from this function's contract.
    let Some(name) = (unsafe { read_name(name, &mut nbuf) }) else {
        return ERR_NAME;
    };
    let Some(cfg) = Config::new(name) else {
        return ERR_NAME;
    };
    // Reset the device in the static slot, in place: only one field at a
    // time is ever a temporary on this stack, never a whole `Modem`.
    let dev = &mut slot.dev;
    dev.modem.reset(cfg);
    dev.radio = Sx1262::new(HalBus);
    dev.power = 0;
    dev.trace = Trace::new();
    slot.live = true;
    if !last_panic.is_null() && len > 0 {
        // SAFETY: the caller guarantees `last_panic` is valid for `len`
        // bytes; the length is capped, and the slice is only read here.
        let note = unsafe { core::slice::from_raw_parts(last_panic, len.min(NOTE_LEN)) };
        dev.modem.set_last_panic(note);
    }
    let power = dev.modem.tx_power_dbm();
    dev.power = power;
    match dev.radio.init(power) {
        Ok(()) => {
            dev.modem.set_radio_diag(0, 0);
            0
        }
        Err(e) => {
            let errs = dev.radio.device_errors().unwrap_or(0);
            let c = error_code(e);
            dev.modem.set_radio_diag(c, errs);
            c_int::from(c)
        }
    }
}

/// Service the radio (drain received frames into the modem), apply a changed
/// S30, then tick the modem. Call on every DIO1 edge and every 10 ms.
#[no_mangle]
pub extern "C" fn lfd3k_poll(now_ms: u32) {
    with_device(|d| {
        sync_power(d);
        let mut buf = [0u8; 255];
        for _ in 0..RX_PER_POLL {
            let Some((n, info)) = d.radio.service(&mut buf) else {
                break;
            };
            let frame = buf.get(..n).unwrap_or(&[]);
            d.modem.radio_rx(frame, info, &mut d.radio, now_ms);
        }
        d.modem.tick(&mut d.radio, now_ms);
        d.trace.sample(&mut d.radio, now_ms);
    });
}

/// Copies the channel trace, oldest first, into `dbm` and `tx` (each `cap`
/// entries) and returns the count (at most 64 and `cap`). Either pointer
/// may be null to skip that array. Returns 0 before init.
///
/// # Safety
/// Each non-null pointer must be valid for writes of `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_rf_trace(dbm: *mut i8, tx: *mut u8, cap: usize) -> usize {
    with_device(|d| {
        let t = &d.trace;
        let n = t.count.min(cap);
        // Oldest of the `count` entries sits at head - count (mod N); skip
        // the oldest ones if the caller's buffer is smaller.
        let start = (t.head + TRACE_N - n) % TRACE_N;
        for i in 0..n {
            let k = (start + i) % TRACE_N;
            if !dbm.is_null() {
                // SAFETY: i < n <= cap, and the caller guarantees `cap` bytes.
                unsafe { dbm.add(i).write(t.dbm.get(k).copied().unwrap_or(0)) };
            }
            if !tx.is_null() {
                // SAFETY: as above.
                unsafe { tx.add(i).write(t.tx.get(k).copied().unwrap_or(0)) };
            }
        }
        n
    })
    .unwrap_or(0)
}

/// Host bytes into the modem. Returns the bytes taken (all of them in
/// command mode, possibly fewer in data mode); 0 before init.
///
/// # Safety
/// `buf` must be null or valid for reads of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_host_in(buf: *const u8, len: usize, now_ms: u32) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `buf` is valid for `len` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(buf, len) };
    with_device(|d| {
        // Feed one line at a time and sync S30 after each, so ATS30=n then
        // ATD in one batch sends the dial at the new power.
        // In data mode the bytes are call data: hand them over in one piece
        // so CRLF-heavy screens fill whole ARQ chunks. The +++ guard time
        // means commands after an escape arrive in a later batch.
        sync_power(d);
        if d.modem.in_data_mode() {
            return d.modem.host_input(bytes, &mut d.radio, now_ms);
        }
        let mut used = 0;
        while used < bytes.len() {
            let rest = bytes.get(used..).unwrap_or(&[]);
            let end = rest
                .iter()
                .position(|&c| c == b'\r')
                .map_or(rest.len(), |i| i + 1);
            let seg = rest.get(..end).unwrap_or(&[]);
            let n = d.modem.host_input(seg, &mut d.radio, now_ms);
            used += n;
            sync_power(d);
            if n < seg.len() {
                break;
            }
        }
        used
    })
    .unwrap_or(0)
}

/// Bytes for the host (echo, result codes, call data). Returns the bytes
/// written to `buf`; 0 before init.
///
/// # Safety
/// `buf` must be null or valid for writes of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_host_out(buf: *mut u8, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `buf` is valid for `len` bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    with_device(|d| d.modem.host_output(out)).unwrap_or(0)
}

/// True once after `AT+FB` was entered; the firmware then writes the frame
/// buffer dump and `OK`.
#[no_mangle]
pub extern "C" fn lfd3k_take_fb_request() -> bool {
    with_device(|d| d.modem.take_fb_request()).unwrap_or(false)
}

/// Status for the lights and the OLED. Mirrored in `lfd3k.h`.
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct lfd3k_status_t {
    /// Bit 0 OH, 1 CD, 2 SD, 3 RD, 4 HS, 5 EC, 6 DC (never set: the modem
    /// has no compression), 7 AA.
    pub lights: u8,
    /// 0 idle, 1 dialing, 2 ringing, 3 training, 4 online, 5 command-online.
    pub state: u8,
    /// -1 when none, else the tier index (0 = SF12 .. 7 = SF5).
    pub tier: i8,
    /// RSSI of the last received frame, dBm; 0 when none.
    pub rssi: i16,
    /// SNR of the last received frame, quarter dB; 0 when none.
    pub snr_x4: i16,
    /// 1 when the radio is usable, 0 after a radio failure (or before init).
    pub radio_ok: u8,
    /// Wi-Fi call: 0 none, 1 dialing, 2 online, 3 bridging a radio call.
    pub wire: u8,
}

/// Fills `out`. Before init it is all zeros with `tier` -1.
///
/// # Safety
/// `out` must be null or valid for a write of one `lfd3k_status_t`.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_status(now_ms: u32, out: *mut lfd3k_status_t) {
    if out.is_null() {
        return;
    }
    let st = with_device(|d| {
        let l = d.modem.lights(now_ms);
        let bits = [l.oh, l.cd, l.sd, l.rd, l.hs, l.ec, false, l.aa];
        let lights = bits
            .iter()
            .enumerate()
            .fold(0u8, |a, (i, &on)| a | (u8::from(on) << i));
        let (rssi, snr_x4) = d.modem.last_rx().map_or((0, 0), |i| (i.rssi_dbm, i.snr_x4));
        lfd3k_status_t {
            lights,
            state: d.modem.call_state(),
            tier: d.modem.tier().map_or(-1, |t| t.index() as i8),
            rssi,
            snr_x4,
            radio_ok: u8::from(!d.radio.failed()),
            wire: d.modem.wire_state(),
        }
    })
    .unwrap_or(lfd3k_status_t {
        tier: -1,
        ..Default::default()
    });
    // SAFETY: the caller guarantees `out` is valid for one write.
    unsafe { out.write(st) };
}

/// Hotspot and client network settings for the Wi-Fi platform code. Mirrored
/// in `lfd3k.h`. All strings are NUL terminated.
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct lfd3k_wifi_cfg_t {
    pub ap_on: u8,
    /// 1 when a client network is set.
    pub sta_set: u8,
    pub ap_ssid: [u8; SSID_MAX + 1],
    pub ap_pass: [u8; PASS_MAX + 1],
    pub sta_ssid: [u8; SSID_MAX + 1],
    pub sta_pass: [u8; PASS_MAX + 1],
}

/// Copies `s` into `dst` NUL terminated (cut to fit), zero filling the rest.
fn put_cstr(dst: &mut [u8], s: &str) {
    dst.fill(0);
    let n = s.len().min(dst.len().saturating_sub(1));
    if let (Some(d), Some(b)) = (dst.get_mut(..n), s.as_bytes().get(..n)) {
        d.copy_from_slice(b);
    }
}

/// Reads a C string of at most `cap` bytes (stopping at the NUL) into `buf`
/// and returns its length.
///
/// # Safety
/// `p` must be non-null and point to a NUL terminated string or to at least
/// `cap` readable bytes.
unsafe fn read_cstr(p: *const c_char, buf: &mut [u8], cap: usize) -> usize {
    let cap = cap.min(buf.len());
    let mut n = 0;
    while n < cap {
        // SAFETY: readable up to the NUL or `cap` bytes (caller's contract).
        let b = unsafe { *p.add(n) } as u8;
        if b == 0 {
            break;
        }
        if let Some(d) = buf.get_mut(n) {
            *d = b;
        }
        n += 1;
    }
    n
}

/// Loads a saved settings blob (from NVS). Returns 0 when accepted (applies
/// S registers, E and V now, and flags a Wi-Fi change), -1 when the blob is
/// rejected or the modem is not initialised (defaults remain; the platform
/// then calls `lfd3k_default_settings` and saves).
///
/// # Safety
/// `blob` must be null or valid for reads of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_load_settings(blob: *const u8, len: usize) -> c_int {
    if blob.is_null() {
        return -1;
    }
    // SAFETY: the caller guarantees `blob` is valid for `len` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(blob, len) };
    with_device(|d| match Settings::from_blob(bytes) {
        Some(s) => {
            d.modem.load_settings(s);
            0
        }
        None => -1,
    })
    .unwrap_or(-1)
}

/// True once after `AT&W` (or a hotspot join) asked for a save: writes the
/// settings blob to `blob` and its length to `len`. When `cap` is under
/// `BLOB_LEN` (1024) it returns false and the request is kept.
///
/// # Safety
/// `blob` must be null or valid for writes of `cap` bytes; `len` must be null
/// or valid for one `size_t` write.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_take_save(blob: *mut u8, cap: usize, len: *mut usize) -> bool {
    if blob.is_null() || len.is_null() || cap < BLOB_LEN {
        return false;
    }
    with_device(|d| {
        if !d.modem.take_save_request() {
            return false;
        }
        let mut tmp = [0u8; BLOB_LEN];
        let n = d.modem.settings().to_blob(&mut tmp).min(BLOB_LEN);
        // SAFETY: `cap >= BLOB_LEN >= n`, so `blob` is valid for `n` bytes;
        // `len` is valid for one write; `tmp` is a distinct local.
        unsafe {
            core::ptr::copy_nonoverlapping(tmp.as_ptr(), blob, n);
            len.write(n);
        }
        true
    })
    .unwrap_or(false)
}

/// First boot (no or rejected blob): factory settings with the hotspot named
/// after the modem and `ap_pass` (8 to 63 printable characters, no comma) as
/// its password; asks for a save (see `lfd3k_take_save`). A null or invalid
/// password changes nothing.
///
/// # Safety
/// `ap_pass` must be null or NUL terminated (or 64 readable bytes).
#[no_mangle]
pub unsafe extern "C" fn lfd3k_default_settings(ap_pass: *const c_char) {
    if ap_pass.is_null() {
        return;
    }
    let mut buf = [0u8; PASS_MAX + 1];
    // SAFETY: forwarded from this function's contract.
    let n = unsafe { read_cstr(ap_pass, &mut buf, PASS_MAX + 1) };
    if n > PASS_MAX {
        return;
    }
    let Some(pass) = buf.get(..n).and_then(|b| core::str::from_utf8(b).ok()) else {
        return;
    };
    if !valid_pass(pass) {
        return;
    }
    with_device(|d| {
        let s = Settings::defaults(d.modem.settings().ap_ssid(), pass);
        d.modem.load_settings(s);
        d.modem.request_save();
    });
}

/// Current hotspot and client settings. Before init, all zero.
///
/// # Safety
/// `out` must be null or valid for a write of one `lfd3k_wifi_cfg_t`.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_get_wifi_config(out: *mut lfd3k_wifi_cfg_t) {
    if out.is_null() {
        return;
    }
    let mut cfg = lfd3k_wifi_cfg_t {
        ap_on: 0,
        sta_set: 0,
        ap_ssid: [0; SSID_MAX + 1],
        ap_pass: [0; PASS_MAX + 1],
        sta_ssid: [0; SSID_MAX + 1],
        sta_pass: [0; PASS_MAX + 1],
    };
    with_device(|d| {
        let s = d.modem.settings();
        cfg.ap_on = u8::from(s.ap_on);
        cfg.sta_set = u8::from(!s.sta_ssid().is_empty());
        put_cstr(&mut cfg.ap_ssid, s.ap_ssid());
        put_cstr(&mut cfg.ap_pass, s.ap_pass());
        put_cstr(&mut cfg.sta_ssid, s.sta_ssid());
        put_cstr(&mut cfg.sta_pass, s.sta_pass());
    });
    // SAFETY: the caller guarantees `out` is valid for one write.
    unsafe { out.write(cfg) };
}

/// True once after AT+WAP, AT+WSTA, ATZ or a settings load changed the Wi-Fi
/// settings: the platform re-reads `lfd3k_get_wifi_config` and re-applies.
#[no_mangle]
pub extern "C" fn lfd3k_take_wifi_changed() -> bool {
    with_device(|d| d.modem.take_wifi_changed()).unwrap_or(false)
}

/// The platform's live Wi-Fi status line for `AT+WIFI?` (up to 96 bytes are
/// kept; non-printable bytes become `?`). Null is ignored.
///
/// # Safety
/// `line` must be null or NUL terminated (or 96 readable bytes).
#[no_mangle]
pub unsafe extern "C" fn lfd3k_set_wifi_status(line: *const c_char) {
    if line.is_null() {
        return;
    }
    let mut buf = [0u8; WIFI_STATUS_MAX];
    // SAFETY: forwarded from this function's contract.
    let n = unsafe { read_cstr(line, &mut buf, WIFI_STATUS_MAX) };
    for b in buf.iter_mut().take(n) {
        if !(0x20..0x7F).contains(b) {
            *b = b'?';
        }
    }
    let Some(text) = buf.get(..n).and_then(|b| core::str::from_utf8(b).ok()) else {
        return;
    };
    with_device(|d| d.modem.set_wifi_status(text));
}

/// True while the generated hotspot password may be shown on the OLED.
#[no_mangle]
pub extern "C" fn lfd3k_show_ap_pass() -> bool {
    with_device(|d| d.modem.show_ap_pass()).unwrap_or(false)
}

/// A device joined the hotspot: stop showing the password (asks for a save).
#[no_mangle]
pub extern "C" fn lfd3k_note_ap_client_joined() {
    with_device(|d| d.modem.note_ap_client_joined());
}

/// The modem task's stack headroom in bytes (high-water mark); AT&V prints
/// `STACK FREE <n>`.
#[no_mangle]
pub extern "C" fn lfd3k_set_stack_free(bytes: u32) {
    with_device(|d| d.modem.set_stack_free(bytes));
}

/// True once when a Wi-Fi call wants a TCP connection: writes the NUL
/// terminated host to `host` and the port to `port`. `cap` must be at least
/// 64 (the longest host plus NUL); with less it returns false and the request
/// is kept. Poll `lfd3k_take_wire_close` first.
///
/// # Safety
/// `host` must be null or valid for writes of `cap` bytes; `port` null or
/// valid for one `uint16_t` write.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_take_wire_request(
    host: *mut c_char,
    cap: usize,
    port: *mut u16,
) -> bool {
    if host.is_null() || port.is_null() || cap < HOST_MAX + 1 {
        return false;
    }
    with_device(|d| {
        let Some(req) = d.modem.take_wire_request() else {
            return false;
        };
        let h = req.host().as_bytes();
        let n = h.len().min(HOST_MAX);
        // SAFETY: `cap >= HOST_MAX + 1 > n`, so `host` is valid for `n + 1`
        // bytes; `port` is valid for one write; `h` is a distinct borrow.
        unsafe {
            core::ptr::copy_nonoverlapping(h.as_ptr(), host.cast::<u8>(), n);
            host.add(n).write(0);
            port.write(req.port());
        }
        true
    })
    .unwrap_or(false)
}

/// The TCP connection changed: `ev` 1 connected, 2 failed (refused, timeout),
/// 3 closed by the far end. Other values are ignored.
#[no_mangle]
pub extern "C" fn lfd3k_wire_event(ev: c_int, now_ms: u32) {
    with_device(|d| match ev {
        1 => d.modem.wire_connected(now_ms),
        2 => d.modem.wire_failed(now_ms),
        3 => d.modem.wire_closed(now_ms),
        _ => {}
    });
}

/// Bytes received from the TCP connection; returns the bytes taken (the
/// platform offers the rest again later). 0 before init.
///
/// # Safety
/// `buf` must be null or valid for reads of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_wire_in(buf: *const u8, len: usize, now_ms: u32) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `buf` is valid for `len` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(buf, len) };
    with_device(|d| d.modem.wire_input(bytes, now_ms)).unwrap_or(0)
}

/// Bytes to send on the TCP connection (telnet encoded); returns the bytes
/// written to `buf`. 0 before init.
///
/// # Safety
/// `buf` must be null or valid for writes of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_wire_out(buf: *mut u8, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `buf` is valid for `len` bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    with_device(|d| d.modem.wire_output(out)).unwrap_or(0)
}

/// True once when the core wants the TCP connection closed (drain
/// `lfd3k_wire_out` first, then close).
#[no_mangle]
pub extern "C" fn lfd3k_take_wire_close() -> bool {
    with_device(|d| d.modem.take_wire_close()).unwrap_or(false)
}
