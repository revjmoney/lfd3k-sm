//! Host tests of the voice session entry points of the C ABI
//! (`lfd3k_voice_open`, `_in`, `_out`, `_close`, `_alive`) against the fake
//! HAL. The static device is a process global, so every test takes
//! `serial()` first: it holds a lock for the whole test and resets the fake
//! HAL. Each test then runs `lfd3k_init` itself (which resets the device in
//! place), so no test sees another one's state. This file is its own test
//! binary, so it shares no global with the other FFI tests.

use std::ffi::CString;
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use lfd3k_core::voice::host::{cobs_decode, cobs_encode, msg, VHP_FRAME_MAX, VHP_VERSION};
use lfd3k_ffi::*;

static LOCK: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fake_hal::reset_all();
    g
}

fn init(name: &str) {
    let c = CString::new(name).unwrap();
    assert_eq!(unsafe { lfd3k_init(c.as_ptr(), ptr::null(), 0) }, 0);
}

/// A frame as the app sends it: COBS and the zero delimiter.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut b = [0u8; 300];
    let n = cobs_encode(body, &mut b).unwrap();
    b[..n].to_vec()
}

fn hello() -> Vec<u8> {
    framed(&[msg::HELLO, VHP_VERSION])
}

fn voice_in(session: u8, bytes: &[u8], now: u32) -> usize {
    unsafe { lfd3k_voice_in(session, bytes.as_ptr(), bytes.len(), now) }
}

/// Everything the session has for the app now.
fn voice_out(session: u8) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 300];
    loop {
        let n = unsafe { lfd3k_voice_out(session, buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            return all;
        }
        all.extend_from_slice(&buf[..n]);
    }
}

/// Split output into decoded frames (type byte first).
fn frames(out: &[u8]) -> Vec<Vec<u8>> {
    out.split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| {
            let mut d = [0u8; VHP_FRAME_MAX];
            let n = cobs_decode(f, &mut d).expect("a valid COBS frame");
            d[..n].to_vec()
        })
        .collect()
}

fn count_type(fr: &[Vec<u8>], t: u8) -> usize {
    fr.iter().filter(|f| f.first() == Some(&t)).count()
}

#[test]
fn ffi_voice_hello_roundtrip() {
    let _g = serial();
    init("REVTEST");
    assert!(lfd3k_voice_open(1, 0));
    assert_eq!(voice_in(1, &hello(), 10), hello().len());
    lfd3k_poll(20);
    let fr = frames(&voice_out(1));
    let ack = fr
        .iter()
        .find(|f| f.first() == Some(&msg::HELLO_ACK))
        .expect("a HelloAck");
    assert_eq!(ack[1], VHP_VERSION);
    let l = usize::from(ack[2]);
    assert_eq!(&ack[3..3 + l], b"REVTEST");
    assert_eq!(ack.len(), 3 + l);
}

#[test]
fn ffi_voice_session_zero_refused() {
    let _g = serial();
    init("REVTEST");
    assert!(!lfd3k_voice_open(0, 0));
    assert!(!lfd3k_voice_open(4, 0));
    assert!(!lfd3k_voice_alive(0));
    // A good number opens once, and a second open is refused.
    assert!(lfd3k_voice_open(1, 0));
    assert!(!lfd3k_voice_open(1, 0));
}

#[test]
fn ffi_voice_out_before_init_is_zero() {
    let _g = serial();
    // Not initialised: a failed init (bad name) leaves the device not live.
    assert_eq!(unsafe { lfd3k_init(ptr::null(), ptr::null(), 0) }, -4);
    assert!(!lfd3k_voice_open(1, 0));
    assert!(!lfd3k_voice_alive(1));
    let mut buf = [0u8; 64];
    assert_eq!(
        unsafe { lfd3k_voice_out(1, buf.as_mut_ptr(), buf.len()) },
        0
    );
    assert_eq!(voice_in(1, &hello(), 0), 0);
    lfd3k_voice_close(1, 0);
}

#[test]
fn ffi_voice_in_split_frames() {
    let _g = serial();
    init("REVTEST");
    assert!(lfd3k_voice_open(1, 0));
    for (i, b) in hello().iter().enumerate() {
        assert_eq!(voice_in(1, std::slice::from_ref(b), 10 + i as u32), 1);
    }
    lfd3k_poll(30);
    let fr = frames(&voice_out(1));
    assert_eq!(count_type(&fr, msg::HELLO_ACK), 1);
    // And nothing more for it later.
    lfd3k_poll(40);
    assert_eq!(count_type(&frames(&voice_out(1)), msg::HELLO_ACK), 0);
}

#[test]
fn ffi_voice_in_garbage_survives() {
    let _g = serial();
    init("REVTEST");
    assert!(lfd3k_voice_open(1, 0));
    let mut x: u32 = 0x1234_5678;
    let junk: Vec<u8> = (0..10_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect();
    // Offer it all, the way the firmware does: again for what was not taken.
    let mut off = 0;
    let mut now = 10;
    let mut stuck = 0;
    while off < junk.len() && stuck < 2_000 {
        let n = voice_in(1, &junk[off..], now);
        off += n;
        if n == 0 {
            stuck += 1;
            // Drain output so the session can go on.
            let _ = voice_out(1);
            lfd3k_poll(now);
        }
        now += 1;
    }
    assert_eq!(off, junk.len(), "all garbage bytes were taken");
    let _ = voice_out(1);
    // A zero ends whatever frame the junk left half read.
    assert!(lfd3k_voice_alive(1));
    voice_in(1, &[0], now);
    voice_in(1, &hello(), now + 1);
    lfd3k_poll(now + 2);
    let fr = frames(&voice_out(1));
    assert!(
        count_type(&fr, msg::HELLO_ACK) >= 1,
        "a Hello is still answered"
    );
}

#[test]
fn ffi_session_timeout_reports_closed() {
    let _g = serial();
    init("REVTEST");
    assert!(lfd3k_voice_open(1, 0));
    assert!(lfd3k_voice_alive(1));
    let mut t = 0;
    while t < 4_990 {
        t += 10;
        lfd3k_poll(t);
    }
    assert!(lfd3k_voice_alive(1), "still open just under 5 s");
    lfd3k_poll(5_001);
    assert!(!lfd3k_voice_alive(1), "closed after 5 s of silence");
}

#[test]
fn ffi_scope_frames_flow() {
    let _g = serial();
    init("REVTEST");
    assert!(lfd3k_voice_open(1, 0));
    // Four samples: no Scope yet.
    for k in 0..4 {
        lfd3k_poll(k * 50);
    }
    assert_eq!(count_type(&frames(&voice_out(1)), msg::SCOPE), 0);
    // The fifth sample, 50 ms after the fourth, makes the frame due.
    lfd3k_poll(200);
    let fr = frames(&voice_out(1));
    assert_eq!(count_type(&fr, msg::SCOPE), 1, "one Scope after 5 samples");
}
