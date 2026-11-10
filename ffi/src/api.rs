//! The C ABI the firmware's modem task calls.
//!
//! One static holder owns the `Modem` and the `Sx1262<HalBus>`. Every entry
//! point except `lfd3k_init` and `lfd3k_core_version` returns early (with
//! zero or nothing) until `lfd3k_init` has run.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};

use lfd3k_core::{Config, Modem};
use sx1262::{Error, Sx1262};

use crate::hal::HalBus;
use crate::panic::NOTE_LEN;

/// `lfd3k_init` result for a name that is not 1 to 16 printable ASCII
/// characters (not NUL terminated within 17 bytes counts too).
const ERR_NAME: c_int = -4;
/// Frames drained per `lfd3k_poll` at most.
const RX_PER_POLL: usize = 8;

struct Device {
    modem: Modem,
    radio: Sx1262<HalBus>,
    /// Last S30 value given to the radio.
    power: i8,
}

/// The one global. `Option` so it exists before `lfd3k_init`.
struct Holder(UnsafeCell<Option<Device>>);

// SAFETY: the C ABI contract says every entry point below is called only
// from the one modem task (never from an ISR or another task), so the cell
// is never accessed from two threads. The firmware's DIO1 ISR only notifies
// that task. Nothing here hands out a reference that outlives one call.
unsafe impl Sync for Holder {}

static HOLDER: Holder = Holder(UnsafeCell::new(None));

/// Run `f` on the device, or return `None` before init.
fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
    // SAFETY: single-task contract (see `Holder`). The `&mut` lives only for
    // this call, and `f` cannot call back into an entry point, so it is the
    // only live reference to the cell contents.
    let slot = unsafe { &mut *HOLDER.0.get() };
    slot.as_mut().map(f)
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
    let mut nbuf = [0u8; 17];
    // SAFETY: forwarded from this function's contract.
    let Some(name) = (unsafe { read_name(name, &mut nbuf) }) else {
        return ERR_NAME;
    };
    let Some(cfg) = Config::new(name) else {
        return ERR_NAME;
    };
    let mut modem = Modem::new(cfg);
    if !last_panic.is_null() && len > 0 {
        // SAFETY: the caller guarantees `last_panic` is valid for `len`
        // bytes; the length is capped, and the slice is only read here.
        let note = unsafe { core::slice::from_raw_parts(last_panic, len.min(NOTE_LEN)) };
        modem.set_last_panic(note);
    }
    let power = modem.tx_power_dbm();
    let mut radio = Sx1262::new(HalBus);
    let code = match radio.init(power) {
        Ok(()) => {
            modem.set_radio_diag(0, 0);
            0
        }
        Err(e) => {
            let errs = radio.device_errors().unwrap_or(0);
            let c = error_code(e);
            modem.set_radio_diag(c, errs);
            c_int::from(c)
        }
    };
    // SAFETY: single-task contract (see `Holder`); no other reference to
    // the cell contents is live.
    unsafe { *HOLDER.0.get() = Some(Device { modem, radio, power }) };
    code
}

/// Service the radio (drain received frames into the modem), apply a changed
/// S30, then tick the modem. Call on every DIO1 edge and every 10 ms.
#[no_mangle]
pub extern "C" fn lfd3k_poll(now_ms: u32) {
    with_device(|d| {
        let p = d.modem.tx_power_dbm();
        if p != d.power {
            d.power = p;
            // A failure marks the driver failed; AT&V and status show it.
            let _ = d.radio.set_power(p);
        }
        let mut buf = [0u8; 255];
        for _ in 0..RX_PER_POLL {
            let Some((n, info)) = d.radio.service(&mut buf) else {
                break;
            };
            let frame = buf.get(..n).unwrap_or(&[]);
            d.modem.radio_rx(frame, info, &mut d.radio, now_ms);
        }
        d.modem.tick(&mut d.radio, now_ms);
    });
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
    with_device(|d| d.modem.host_input(bytes, &mut d.radio, now_ms)).unwrap_or(0)
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
        let (rssi, snr_x4) = d
            .modem
            .last_rx()
            .map_or((0, 0), |i| (i.rssi_dbm, i.snr_x4));
        lfd3k_status_t {
            lights,
            state: d.modem.call_state(),
            tier: d.modem.tier().map_or(-1, |t| t.index() as i8),
            rssi,
            snr_x4,
            radio_ok: u8::from(!d.radio.failed()),
        }
    })
    .unwrap_or(lfd3k_status_t {
        tier: -1,
        ..Default::default()
    });
    // SAFETY: the caller guarantees `out` is valid for one write.
    unsafe { out.write(st) };
}
