//! The C functions the NDK UI calls. Pointers are checked here; the
//! session itself stays safe Rust.
//!
//! Layout of every `#[repr(C)]` struct is repeated in
//! `apps/android/app/src/main/cpp/lfd3k_abi.h`. Field sizes are multiples
//! of 4 so the two compilers agree. `lfd3k_status_bytes` lets the C++ side
//! refuse a mismatch.

use std::ffi::CStr;
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;

use crate::session::{ContactSnap, DirSnap, Session, Start, Transport, View};

const NAME: usize = 24;
const LABEL: usize = 16;
const HINT: usize = 16;
const LINE: usize = 160;
const SHORT: usize = 64;
const LINK: usize = 96;
const PLAN: usize = 96;
const CODE: usize = 8;
const DIR_N: usize = 48;
const CONTACT_N: usize = 16;
const DEV_N: usize = 4;
const SCOPE_N: usize = 160;
const AIR_N: usize = 24;
const SCAN_N: usize = 8;
const SCAN_W: usize = 96;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CDir {
    name: [u8; NAME],
    label: [u8; LABEL],
    station: [u8; NAME],
    hint: [u8; HINT],
    sf: i32,
    pinned: i32,
    trust: i32,
    voice: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CContact {
    name: [u8; NAME],
    label: [[u8; LABEL]; DEV_N],
    key: [[u8; 32]; DEV_N],
    n_dev: i32,
    person: i32,
    removed: i32,
    _pad: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CScope {
    dbm: i8,
    tx: u8,
    _pad: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CAir {
    rx: i32,
    sf: i32,
    start_ms: i32,
    air_ms: i32,
}

#[repr(C)]
pub struct CStatus {
    name: [u8; NAME],
    label: [u8; LABEL],
    station: [u8; NAME],
    link: [u8; LINK],
    bind_addr: [u8; SHORT],
    peer_addr: [u8; SHORT],
    banner: [u8; LINE],
    note: [u8; LINE],
    words0: [u8; 24],
    words1: [u8; 24],
    plan: [u8; PLAN],
    who: [u8; SHORT],
    peer_name: [u8; NAME],
    peer_label: [u8; LABEL],
    peer_station: [u8; NAME],
    trust_line: [u8; SHORT],
    link_code: [u8; CODE],
    link_note: [u8; LINE],
    device_key: [u8; 32],
    banner_warn: i32,
    has_person: i32,
    in_call: i32,
    ringing: i32,
    ring_name: [u8; NAME],
    ring_label: [u8; LABEL],
    ring_station: [u8; NAME],
    audio_ok: i32,
    talk_anyway: i32,
    conflict: i32,
    muted: i32,
    ptt: i32,
    floor: i32,
    call_state: i32,
    sf: i32,
    locked: i32,
    sounds: i32,
    linking: i32,
    cap_active: i32,
    unsaved: i32,
    pw_dirty: i32,
    lost: i32,
    ready: i32,
    connecting: i32,
    scanning: i32,
    rssi: i32,
    scope_now: i32,
    dir_count: i32,
    contact_count: i32,
    scope_count: i32,
    air_count: i32,
    scan_count: i32,
    dir: [CDir; DIR_N],
    contacts: [CContact; CONTACT_N],
    scope: [CScope; SCOPE_N],
    air: [CAir; AIR_N],
    scan: [[u8; SCAN_W]; SCAN_N],
}

fn put(dst: &mut [u8], s: &str) {
    dst.fill(0);
    let max = dst.len().saturating_sub(1);
    let mut n = s.len().min(max);
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    if let (Some(d), Some(src)) = (dst.get_mut(..n), s.as_bytes().get(..n)) {
        d.copy_from_slice(src);
    }
}

fn fill_dir(d: &DirSnap) -> CDir {
    let mut c = CDir {
        name: [0; NAME],
        label: [0; LABEL],
        station: [0; NAME],
        hint: [0; HINT],
        sf: d.sf,
        pinned: i32::from(d.pinned),
        trust: d.trust,
        voice: i32::from(d.voice),
    };
    put(&mut c.name, &d.name);
    put(&mut c.label, &d.label);
    put(&mut c.station, &d.station);
    put(&mut c.hint, &d.hint);
    c
}

fn fill_contact(c: &ContactSnap) -> CContact {
    let mut out = CContact {
        name: [0; NAME],
        label: [[0; LABEL]; DEV_N],
        key: [[0; 32]; DEV_N],
        n_dev: 0,
        person: i32::from(c.person),
        removed: c.removed as i32,
        _pad: 0,
    };
    put(&mut out.name, &c.name);
    let n = c.devices.len().min(DEV_N);
    out.n_dev = n as i32;
    for (i, (label, key)) in c.devices.iter().take(n).enumerate() {
        if let Some(slot) = out.label.get_mut(i) {
            put(slot, label);
        }
        if let Some(slot) = out.key.get_mut(i) {
            *slot = *key;
        }
    }
    out
}

fn fill(v: &View) -> CStatus {
    let mut s = CStatus {
        name: [0; NAME],
        label: [0; LABEL],
        station: [0; NAME],
        link: [0; LINK],
        bind_addr: [0; SHORT],
        peer_addr: [0; SHORT],
        banner: [0; LINE],
        note: [0; LINE],
        words0: [0; 24],
        words1: [0; 24],
        plan: [0; PLAN],
        who: [0; SHORT],
        peer_name: [0; NAME],
        peer_label: [0; LABEL],
        peer_station: [0; NAME],
        trust_line: [0; SHORT],
        link_code: [0; CODE],
        link_note: [0; LINE],
        device_key: v.device_key,
        banner_warn: i32::from(v.banner_warn),
        has_person: i32::from(v.has_person),
        in_call: i32::from(v.in_call),
        ringing: i32::from(v.ringing),
        ring_name: [0; NAME],
        ring_label: [0; LABEL],
        ring_station: [0; NAME],
        audio_ok: i32::from(v.audio_ok),
        talk_anyway: i32::from(v.talk_anyway),
        conflict: i32::from(v.conflict),
        muted: i32::from(v.muted),
        ptt: i32::from(v.ptt),
        floor: v.floor,
        call_state: v.call_state,
        sf: v.sf,
        locked: i32::from(v.locked),
        sounds: i32::from(v.sounds),
        linking: i32::from(v.linking),
        cap_active: i32::from(v.cap_active),
        unsaved: i32::from(v.unsaved),
        pw_dirty: i32::from(v.pw_dirty),
        lost: i32::from(v.lost),
        ready: i32::from(v.ready),
        connecting: i32::from(v.connecting),
        scanning: i32::from(v.scanning),
        rssi: v.rssi,
        scope_now: v.scope_now as i32,
        dir_count: v.dir.len().min(DIR_N) as i32,
        contact_count: v.contacts.len().min(CONTACT_N) as i32,
        scope_count: v.scope.len().min(SCOPE_N) as i32,
        air_count: v.air.len().min(AIR_N) as i32,
        scan_count: v.scan.len().min(SCAN_N) as i32,
        dir: std::array::from_fn(|_| CDir {
            name: [0; NAME],
            label: [0; LABEL],
            station: [0; NAME],
            hint: [0; HINT],
            sf: 0,
            pinned: 0,
            trust: 0,
            voice: 0,
        }),
        contacts: std::array::from_fn(|_| CContact {
            name: [0; NAME],
            label: [[0; LABEL]; DEV_N],
            key: [[0; 32]; DEV_N],
            n_dev: 0,
            person: 0,
            removed: 0,
            _pad: 0,
        }),
        scope: [CScope {
            dbm: 0,
            tx: 0,
            _pad: [0; 2],
        }; SCOPE_N],
        air: [CAir {
            rx: 0,
            sf: 0,
            start_ms: 0,
            air_ms: 0,
        }; AIR_N],
        scan: [[0; SCAN_W]; SCAN_N],
    };
    put(&mut s.name, &v.name);
    put(&mut s.label, &v.label);
    put(&mut s.station, &v.station);
    put(&mut s.link, &v.link);
    put(&mut s.bind_addr, &v.bind);
    put(&mut s.peer_addr, &v.peer);
    put(&mut s.banner, v.banner_now());
    put(&mut s.note, &v.note);
    put(
        &mut s.words0,
        v.words.first().map(String::as_str).unwrap_or(""),
    );
    put(
        &mut s.words1,
        v.words.get(1).map(String::as_str).unwrap_or(""),
    );
    put(&mut s.plan, &v.plan);
    put(&mut s.who, &v.who);
    put(&mut s.peer_name, &v.peer_name);
    put(&mut s.peer_label, &v.peer_label);
    put(&mut s.peer_station, &v.peer_station);
    put(&mut s.trust_line, &v.trust_line);
    put(&mut s.link_code, &v.link_code);
    put(&mut s.link_note, &v.link_note);
    put(&mut s.ring_name, &v.ring_name);
    put(&mut s.ring_label, &v.ring_label);
    put(&mut s.ring_station, &v.ring_station);
    for (i, d) in v.dir.iter().take(DIR_N).enumerate() {
        if let Some(slot) = s.dir.get_mut(i) {
            *slot = fill_dir(d);
        }
    }
    for (i, c) in v.contacts.iter().take(CONTACT_N).enumerate() {
        if let Some(slot) = s.contacts.get_mut(i) {
            *slot = fill_contact(c);
        }
    }
    for (i, (dbm, tx)) in v.scope.iter().take(SCOPE_N).enumerate() {
        if let Some(slot) = s.scope.get_mut(i) {
            slot.dbm = *dbm;
            slot.tx = u8::from(*tx);
        }
    }
    for (i, a) in v.air.iter().take(AIR_N).enumerate() {
        if let Some(slot) = s.air.get_mut(i) {
            *slot = CAir {
                rx: i32::from(a.rx),
                sf: i32::from(a.sf),
                start_ms: a.start_ms as i32,
                air_ms: i32::from(a.air_ms),
            };
        }
    }
    for (i, line) in v.scan.iter().take(SCAN_N).enumerate() {
        if let Some(slot) = s.scan.get_mut(i) {
            put(slot, line);
        }
    }
    s
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn write_err(err: *mut c_char, err_len: usize, msg: &str) {
    if err.is_null() || err_len == 0 {
        return;
    }
    let mut buf = vec![0u8; err_len];
    put(&mut buf, msg);
    unsafe {
        std::ptr::copy_nonoverlapping(buf.as_ptr(), err.cast(), err_len);
    }
}

fn session<'a>(p: *mut Session) -> Option<&'a Session> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { &*p })
    }
}

/// Bytes of `Lfd3kStatus`. The UI checks this against `sizeof`.
#[no_mangle]
pub extern "C" fn lfd3k_status_bytes() -> usize {
    std::mem::size_of::<CStatus>()
}

#[no_mangle]
pub extern "C" fn lfd3k_start(
    transport: c_int,
    name: *const c_char,
    label: *const c_char,
    data_dir: *const c_char,
    bind_addr: *const c_char,
    peer_addr: *const c_char,
    station: *const c_char,
    tcp_host: *const c_char,
    tcp_port: c_int,
    find_station: *const c_char,
    usb_label: *const c_char,
    snr_db: c_int,
    cap_sf: c_int,
    sounds: c_int,
    auto_answer: c_int,
    err: *mut c_char,
    err_len: usize,
) -> *mut Session {
    let name = cstr(name);
    let label = cstr(label);
    let data = cstr(data_dir);
    if data.is_empty() {
        write_err(err, err_len, "no data folder");
        return std::ptr::null_mut();
    }
    let bind = {
        let b = cstr(bind_addr);
        if b.is_empty() {
            "0.0.0.0:4211".to_string()
        } else {
            b
        }
    };
    let peer_s = {
        let p = cstr(peer_addr);
        if p.is_empty() {
            "127.0.0.1:4210".to_string()
        } else {
            p
        }
    };
    let peer: SocketAddr = match peer_s.parse() {
        Ok(a) => a,
        Err(_) => {
            write_err(err, err_len, "peer address wants host:port");
            return std::ptr::null_mut();
        }
    };
    let station = {
        let st = cstr(station);
        if !st.is_empty() {
            st
        } else {
            let port = bind.rsplit_once(':').map(|(_, p)| p).unwrap_or("4211");
            format!("PHONEMODEM-{port}")
        }
    };
    let find_st = cstr(find_station);
    let transport = match transport {
        1 => Transport::Tcp {
            host: {
                let h = cstr(tcp_host);
                if h.is_empty() {
                    write_err(err, err_len, "TCP needs a host");
                    return std::ptr::null_mut();
                }
                h
            },
            port: if tcp_port <= 0 {
                link_port()
            } else {
                tcp_port as u16
            },
        },
        2 => Transport::Find {
            station: if find_st.is_empty() {
                None
            } else {
                Some(find_st)
            },
        },
        3 => Transport::Usb {
            label: cstr(usb_label),
        },
        _ => Transport::Sim,
    };
    let cap_sf = match cap_sf {
        5..=12 => Some(cap_sf as u8),
        _ => None,
    };
    let sounds = match sounds {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    let start = Start {
        transport,
        name: if name.is_empty() { "REV".into() } else { name },
        label: if label.is_empty() {
            "PHONE".into()
        } else {
            label
        },
        data: PathBuf::from(data),
        bind,
        peer,
        station,
        snr_db: (snr_db as i16).clamp(-40, 40),
        cap_sf,
        sounds,
        auto_answer: auto_answer != 0,
    };
    match Session::start(start) {
        Ok(s) => Box::into_raw(Box::new(s)),
        Err(e) => {
            write_err(err, err_len, &e);
            std::ptr::null_mut()
        }
    }
}

fn link_port() -> u16 {
    crate::link::VOICE_PORT
}

#[no_mangle]
pub extern "C" fn lfd3k_shutdown(p: *mut Session) {
    if !p.is_null() {
        unsafe { (*p).shutdown() };
    }
}

#[no_mangle]
pub extern "C" fn lfd3k_stop(p: *mut Session) {
    if !p.is_null() {
        unsafe {
            (*p).shutdown();
            drop(Box::from_raw(p));
        }
    }
}

#[no_mangle]
pub extern "C" fn lfd3k_status(p: *mut Session, out: *mut CStatus) {
    let (Some(s), true) = (session(p), !out.is_null()) else {
        return;
    };
    let v = s.view();
    unsafe { out.write(fill(&v)) };
}

#[no_mangle]
pub extern "C" fn lfd3k_text(p: *mut Session, which: c_int, buf: *mut c_char, n: usize) -> c_int {
    let Some(s) = session(p) else {
        return 0;
    };
    if buf.is_null() || n == 0 {
        return 0;
    }
    let text = s.text(which);
    let mut tmp = vec![0u8; n];
    put(&mut tmp, &text);
    unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), buf.cast(), n) };
    text.len().min(n.saturating_sub(1)) as c_int
}

#[no_mangle]
pub extern "C" fn lfd3k_mic(p: *mut Session, pcm: *const i16, n: usize) {
    let Some(s) = session(p) else {
        return;
    };
    if pcm.is_null() || n == 0 {
        return;
    }
    let samples = unsafe { std::slice::from_raw_parts(pcm, n) };
    s.push_mic(samples);
}

#[no_mangle]
pub extern "C" fn lfd3k_speaker(p: *mut Session, pcm: *mut i16, n: usize) -> usize {
    let Some(s) = session(p) else {
        return 0;
    };
    if pcm.is_null() || n == 0 {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(pcm, n) };
    s.pull_spk(out)
}

#[no_mangle]
pub extern "C" fn lfd3k_usb_rx(p: *mut Session, data: *const u8, n: usize) {
    let Some(s) = session(p) else {
        return;
    };
    if data.is_null() || n == 0 {
        return;
    }
    let b = unsafe { std::slice::from_raw_parts(data, n) };
    s.push_usb_rx(b);
}

#[no_mangle]
pub extern "C" fn lfd3k_usb_tx(p: *mut Session, data: *mut u8, n: usize) -> usize {
    let Some(s) = session(p) else {
        return 0;
    };
    if data.is_null() || n == 0 {
        return 0;
    }
    let b = unsafe { std::slice::from_raw_parts_mut(data, n) };
    s.pop_usb_tx(b)
}

#[no_mangle]
pub extern "C" fn lfd3k_usb_tx_unget(p: *mut Session, data: *const u8, n: usize) {
    let Some(s) = session(p) else {
        return;
    };
    if data.is_null() || n == 0 {
        return;
    }
    let b = unsafe { std::slice::from_raw_parts(data, n) };
    s.unget_usb_tx(b);
}

#[no_mangle]
pub extern "C" fn lfd3k_call(p: *mut Session, name: *const c_char, label: *const c_char) {
    let Some(s) = session(p) else {
        return;
    };
    let name = cstr(name);
    let label = cstr(label);
    let label = if label.is_empty() { None } else { Some(label) };
    s.call(&name, label.as_deref());
}

#[no_mangle]
pub extern "C" fn lfd3k_answer(p: *mut Session) {
    if let Some(s) = session(p) {
        s.answer();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_decline(p: *mut Session) {
    if let Some(s) = session(p) {
        s.decline();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_hangup(p: *mut Session) {
    if let Some(s) = session(p) {
        s.hangup();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_talk(p: *mut Session, down: c_int) {
    if let Some(s) = session(p) {
        s.talk(down != 0);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_words(p: *mut Session) {
    if let Some(s) = session(p) {
        s.words();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_anyway(p: *mut Session) {
    if let Some(s) = session(p) {
        s.anyway();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_mute(p: *mut Session, on: c_int) {
    if let Some(s) = session(p) {
        s.mute(on != 0);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_link_offer(p: *mut Session) {
    if let Some(s) = session(p) {
        s.link_offer();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_link_join(p: *mut Session, code: *const c_char) {
    if let Some(s) = session(p) {
        let c = cstr(code);
        s.link_join(&c);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_cancel_link(p: *mut Session) {
    if let Some(s) = session(p) {
        s.cancel_link();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_set(p: *mut Session, line: *const c_char) {
    if let Some(s) = session(p) {
        let line = cstr(line);
        s.set_line(&line);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_save(p: *mut Session) {
    if let Some(s) = session(p) {
        s.save();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_discard(p: *mut Session) {
    if let Some(s) = session(p) {
        s.discard();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_scan(p: *mut Session) {
    if let Some(s) = session(p) {
        s.scan();
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_rename(p: *mut Session, name: *const c_char, label: *const c_char) {
    if let Some(s) = session(p) {
        let name = cstr(name);
        let label = cstr(label);
        s.rename(&name, &label);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_sounds(p: *mut Session, on: c_int) {
    if let Some(s) = session(p) {
        s.sounds(on != 0);
    }
}
#[no_mangle]
pub extern "C" fn lfd3k_remove_mine(p: *mut Session, key: *const u8) {
    let Some(s) = session(p) else {
        return;
    };
    if key.is_null() {
        return;
    }
    let mut k = [0u8; 32];
    unsafe { std::ptr::copy_nonoverlapping(key, k.as_mut_ptr(), 32) };
    s.remove_mine(k);
}
#[no_mangle]
pub extern "C" fn lfd3k_remove_contact(p: *mut Session, name: *const c_char, key: *const u8) {
    let Some(s) = session(p) else {
        return;
    };
    if key.is_null() {
        return;
    }
    let mut k = [0u8; 32];
    unsafe { std::ptr::copy_nonoverlapping(key, k.as_mut_ptr(), 32) };
    let name = cstr(name);
    s.remove_contact(&name, k);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_has_no_surprise_padding() {
        assert_eq!(std::mem::align_of::<CStatus>(), 4);
        assert_eq!(std::mem::size_of::<CDir>() % 4, 0);
        assert_eq!(std::mem::size_of::<CContact>() % 4, 0);
        assert_eq!(std::mem::size_of::<CScope>(), 4);
        assert_eq!(std::mem::size_of::<CAir>(), 16);
        // Printed once so the C header can assert the same size.
        let n = std::mem::size_of::<CStatus>();
        assert_eq!(n, 11436);
        assert_eq!(std::mem::size_of::<CDir>(), 96);
        assert_eq!(std::mem::size_of::<CContact>(), 232);
    }
}
