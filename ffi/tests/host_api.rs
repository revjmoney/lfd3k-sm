//! Host tests of the C ABI against the fake HAL. The fake HAL and the static
//! device are process globals, so every test takes `serial()` first.
//!
//! `Rig` wires the FFI modem to a second core modem ("PEER") by carrying
//! frames between the fake SX1262 and the peer's radio, so a real call can
//! be set up and driven through the C entry points.

use std::collections::VecDeque;
use std::ffi::{c_char, CString};
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use lfd3k_core::arq::{CHUNK, WINDOW};
use lfd3k_core::frame::{decode, FrameType};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_ffi::panic::{format_note, NOTE_LEN};
use lfd3k_ffi::{
    fake_hal, lfd3k_core_version, lfd3k_host_in, lfd3k_host_out, lfd3k_init, lfd3k_poll,
    lfd3k_status, lfd3k_status_t, lfd3k_take_fb_request,
};

static LOCK: Mutex<()> = Mutex::new(());

/// Serialises the tests and starts each from a clean fake HAL.
fn serial() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fake_hal::reset_all();
    g
}

fn init_with(name: &str, note: &[u8]) -> i32 {
    let c = CString::new(name).unwrap();
    let (p, n) = if note.is_empty() {
        (ptr::null(), 0)
    } else {
        (note.as_ptr(), note.len())
    };
    unsafe { lfd3k_init(c.as_ptr(), p, n) }
}

fn init(name: &str) -> i32 {
    init_with(name, &[])
}

fn host_in(bytes: &[u8], now: u32) -> usize {
    unsafe { lfd3k_host_in(bytes.as_ptr(), bytes.len(), now) }
}

fn host_out() -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 300];
    loop {
        let n = unsafe { lfd3k_host_out(buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            return all;
        }
        all.extend_from_slice(&buf[..n]);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn status(now: u32) -> lfd3k_status_t {
    let mut st = lfd3k_status_t::default();
    unsafe { lfd3k_status(now, &mut st) };
    st
}

/// Deterministic bytes with CRs in them and no '+' (so no escape logic).
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = (x >> 24) as u8;
            if b == b'+' {
                b'-'
            } else {
                b
            }
        })
        .collect()
}

struct PeerRadio {
    sent: Vec<(Tier, Vec<u8>)>,
    listening: Tier,
}

impl Radio for PeerRadio {
    fn send(&mut self, tier: Tier, frame: &[u8]) {
        self.sent.push((tier, frame.to_vec()));
    }
    fn listen(&mut self, tier: Tier) {
        self.listening = tier;
    }
    fn busy(&self) -> bool {
        false
    }
}

struct Rig {
    now: u32,
    peer: Modem,
    pr: PeerRadio,
    to_ffi: VecDeque<(Tier, Vec<u8>)>,
    ffi_out: Vec<u8>,
    peer_out: Vec<u8>,
}

impl Rig {
    fn new() -> Rig {
        assert_eq!(init("CALLER"), 0);
        let mut pr = PeerRadio {
            sent: Vec::new(),
            listening: Tier::Sf12,
        };
        let mut peer = Modem::new(Config::new("PEER").unwrap());
        assert_eq!(peer.host_input(b"ATS0=1\r", &mut pr, 0), 7);
        Rig {
            now: 0,
            peer,
            pr,
            to_ffi: VecDeque::new(),
            ffi_out: Vec::new(),
            peer_out: Vec::new(),
        }
    }

    /// One millisecond: frames the FFI radio sent reach the peer, the peer
    /// runs, its frames go to the chip (when it is receiving on that SF),
    /// then the FFI modem polls.
    fn step(&mut self) {
        self.now += 1;
        let tx = fake_hal::with(|h| std::mem::take(&mut h.tx_frames));
        for (sf, frame) in tx {
            let t = self.pr.listening;
            if sf == t.sf() {
                let info = RxInfo {
                    tier: t,
                    rssi_dbm: -60,
                    snr_x4: 48,
                };
                self.peer.radio_rx(&frame, info, &mut self.pr, self.now);
            }
        }
        self.peer.tick(&mut self.pr, self.now);
        let mut buf = [0u8; 300];
        loop {
            let n = self.peer.host_output(&mut buf);
            if n == 0 {
                break;
            }
            self.peer_out.extend_from_slice(&buf[..n]);
        }
        self.to_ffi.extend(self.pr.sent.drain(..));
        let to_ffi = &mut self.to_ffi;
        fake_hal::with(|h| {
            if h.rx_mode && h.irq & 0x0002 == 0 {
                if let Some((t, f)) = to_ffi.pop_front() {
                    if t.sf() == h.sf {
                        h.deliver(&f, 120, 48);
                    }
                }
            }
        });
        lfd3k_poll(self.now);
        let o = host_out();
        self.ffi_out.extend_from_slice(&o);
    }

    fn run_ms(&mut self, ms: u32) {
        for _ in 0..ms {
            self.step();
        }
    }

    fn run_until(&mut self, max_ms: u32, cond: impl Fn(&Rig) -> bool) -> bool {
        for _ in 0..max_ms {
            if cond(self) {
                return true;
            }
            self.step();
        }
        cond(self)
    }

    fn ffi_in(&mut self, bytes: &[u8]) -> usize {
        host_in(bytes, self.now)
    }

    fn connect(&mut self) {
        self.run_ms(5);
        assert_eq!(self.ffi_in(b"ATDT PEER\r"), 10);
        let ok = self.run_until(30_000, |r| status(r.now).state == 4);
        assert!(
            ok,
            "no connect\nFFI: {:?}\nPEER: {:?}",
            text(&self.ffi_out),
            text(&self.peer_out)
        );
    }

    /// Bytes the peer printed after its CONNECT line (call data).
    fn peer_data(&self) -> &[u8] {
        let marker = b"/ARQ\r\n";
        let at = self
            .peer_out
            .windows(marker.len())
            .rposition(|w| w == marker)
            .expect("peer CONNECT line");
        &self.peer_out[at + marker.len()..]
    }

    fn ffi_data(&self) -> &[u8] {
        let marker = b"/ARQ\r\n";
        let at = self
            .ffi_out
            .windows(marker.len())
            .rposition(|w| w == marker)
            .expect("ffi CONNECT line");
        &self.ffi_out[at + marker.len()..]
    }
}

#[test]
fn calls_before_init_are_safe() {
    let _g = serial();
    // A failed init leaves the device empty, the same state as before any
    // init (the holder is cleared first).
    assert_eq!(init(""), -4);
    assert_eq!(
        unsafe { lfd3k_init(ptr::null::<c_char>(), ptr::null(), 0) },
        -4
    );

    lfd3k_poll(5);
    lfd3k_poll(u32::MAX);
    assert_eq!(host_in(b"AT\r", 10), 0);
    assert_eq!(host_in(b"", 10), 0);
    assert_eq!(unsafe { lfd3k_host_in(ptr::null(), 5, 0) }, 0);
    assert_eq!(host_out(), b"");
    assert_eq!(unsafe { lfd3k_host_out(ptr::null_mut(), 8) }, 0);
    assert!(!lfd3k_take_fb_request());

    let mut st = lfd3k_status_t {
        lights: 0xFF,
        state: 9,
        tier: 3,
        rssi: -5,
        snr_x4: 77,
        radio_ok: 1,
    };
    unsafe { lfd3k_status(0, &mut st) };
    assert_eq!(
        st,
        lfd3k_status_t {
            tier: -1,
            ..Default::default()
        }
    );
    unsafe { lfd3k_status(0, ptr::null_mut()) };

    // No SPI traffic, no pin changes.
    fake_hal::with(|h| {
        assert!(h.xfers.is_empty());
        assert_eq!(h.resets, 0);
    });

    // The version call needs no init.
    let mut v = [0u8; 16];
    let n = unsafe { lfd3k_core_version(v.as_mut_ptr(), v.len()) };
    assert_eq!(&v[..n], lfd3k_core::VERSION.as_bytes());
    assert_eq!(unsafe { lfd3k_core_version(ptr::null_mut(), 4) }, 0);

    // Then a good init works, and a bad re-init puts it back to no-ops.
    assert_eq!(init("CALLER"), 0);
    assert_eq!(host_in(b"AT\r", 20), 3);
    assert_eq!(init("seventeen chars 17"), -4);
    assert_eq!(host_in(b"AT\r", 30), 0);
    assert_eq!(host_out(), b"");
}

#[test]
fn panic_note_roundtrip() {
    let _g = serial();
    let mut note = [0u8; NOTE_LEN];
    let n = format_note("core/src/link/mod.rs", 412, &mut note);
    assert_eq!(&note[..n], b"core/src/link/mod.rs:412");

    // The HAL stores what the panic handler gives it; the next boot hands
    // it back to init and ATI shows it.
    fake_hal::panic(&note[..n]);
    let stored = fake_hal::with(|h| h.panics.last().cloned()).unwrap();
    assert_eq!(init_with("CALLER", &stored), 0);
    host_in(b"ATI\r", 0);
    let t = text(&host_out());
    assert!(
        t.contains("\r\nLAST CRASH: core/src/link/mod.rs:412\r\n"),
        "{t:?}"
    );

    // A long path keeps the line number and the tail of the file name.
    let long = format!("{}/deep/module.rs", "d".repeat(100));
    let n = format_note(&long, 7, &mut note);
    assert_eq!(n, NOTE_LEN);
    assert!(note[..n].ends_with(b"/deep/module.rs:7"));
    assert_eq!(init_with("CALLER", &note[..n]), 0);
    host_in(b"ATI\r", 0);
    let t = text(&host_out());
    let line = t
        .split("\r\n")
        .find(|l| l.starts_with("LAST CRASH: "))
        .unwrap();
    assert_eq!(&line.as_bytes()["LAST CRASH: ".len()..], &note[..n]);

    // More bytes than a note can be are cut to 64.
    assert_eq!(init_with("CALLER", &[b'z'; 200]), 0);
    host_in(b"ATI\r", 0);
    let t = text(&host_out());
    let line = t
        .split("\r\n")
        .find(|l| l.starts_with("LAST CRASH: "))
        .unwrap();
    assert_eq!(line.len(), "LAST CRASH: ".len() + NOTE_LEN);

    // No note, no line. Non-printable bytes are shown as '?'.
    assert_eq!(init("CALLER"), 0);
    host_in(b"ATI\r", 0);
    assert!(!text(&host_out()).contains("LAST CRASH"));
    assert_eq!(init_with("CALLER", &[b'a', 0x01, b'b']), 0);
    host_in(b"ATI\r", 0);
    assert!(text(&host_out()).contains("LAST CRASH: a?b\r\n"));
}

#[test]
fn radio_fail_shows_in_atv() {
    let _g = serial();
    // BUSY stuck high: the init times out.
    fake_hal::with(|h| h.busy = true);
    assert_eq!(init("CALLER"), -1);
    // The modem still works and says what happened.
    assert_eq!(host_in(b"AT&V\r", 0), 5);
    let t = text(&host_out());
    assert!(t.contains("RADIO FAIL -1 ERR 0x"), "{t:?}");
    assert!(
        t.contains("TX POWER S30=2 dBm chip, about 14 dBm out"),
        "{t:?}"
    );
    let st = status(0);
    assert_eq!(st.radio_ok, 0);
    host_in(b"AT\r", 1);
    assert!(text(&host_out()).ends_with("\r\nOK\r\n"));

    // No chip on the bus: GetStatus mode 0, device errors shown.
    fake_hal::reset_all();
    fake_hal::with(|h| {
        h.status = 0x00;
        h.dev_errors = 0x0160;
    });
    assert_eq!(init("CALLER"), -2);
    host_in(b"AT&V\r", 0);
    let t = text(&host_out());
    assert!(t.contains("\r\nRADIO FAIL -2 ERR 0x0160\r\n"), "{t:?}");

    // A healthy chip: RADIO OK and radio_ok set.
    fake_hal::reset_all();
    assert_eq!(init("CALLER"), 0);
    host_in(b"AT&V\r", 0);
    let t = text(&host_out());
    assert!(t.contains("\r\nRADIO OK\r\n"), "{t:?}");
    assert_eq!(status(0).radio_ok, 1);
}

#[test]
fn ats30_then_atd_in_one_batch_dials_at_new_power() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    fake_hal::with(|h| h.xfers.clear());
    assert_eq!(host_in(b"ATS30=10\rATDT X\r", 100), 16);

    let (x, frames) = fake_hal::with(|h| (h.xfers.clone(), h.tx_frames.clone()));
    let first_tx = x
        .iter()
        .position(|t| t[0] == 0x83)
        .expect("the dial went on air");
    let last_params = x[..first_tx]
        .iter()
        .rev()
        .find(|t| t[0] == 0x8E)
        .expect("SetTxParams");
    assert_eq!(
        last_params[1], 10,
        "SetTxParams before the first SetTx carries power 10"
    );
    // No SetTxParams with the old power got in between.
    assert!(x[..first_tx]
        .iter()
        .filter(|t| t[0] == 0x8E)
        .all(|t| t[1] == 10));
    // And what went out is the CallReq at SF12.
    let (sf, frame) = frames.first().expect("frame");
    assert_eq!(*sf, 12);
    let f = decode(frame).expect("decodes");
    assert_eq!(f.header.ftype, FrameType::CallReq);
}

#[test]
fn host_bytes_cross_tasks_in_order() {
    let _g = serial();
    // Command mode: the same text in odd chunks, with polls in between,
    // gives the same output as one call.
    let script = b"ATE1\rATS30=7\rAT&V\rATI\rAT+FB\rATS30?\r";
    assert_eq!(init("CALLER"), 0);
    assert_eq!(host_in(script, 0), script.len());
    lfd3k_poll(1);
    let whole = host_out();
    assert!(lfd3k_take_fb_request());

    for sizes in [&[1usize][..], &[3, 1, 7][..], &[64, 2][..]] {
        assert_eq!(init("CALLER"), 0);
        let (mut off, mut i, mut now) = (0, 0, 0u32);
        let mut got = Vec::new();
        while off < script.len() {
            let n = sizes[i % sizes.len()].min(script.len() - off);
            i += 1;
            let took = host_in(&script[off..off + n], now);
            assert_eq!(took, n, "command mode takes everything");
            off += took;
            now += 3;
            lfd3k_poll(now);
            got.extend_from_slice(&host_out());
        }
        assert_eq!(text(&got), text(&whole), "chunks {sizes:?}");
        assert!(lfd3k_take_fb_request());
    }

    // Data mode, both directions at once: partial takes interleaved with
    // polls, everything arrives once and in order.
    let mut rig = Rig::new();
    rig.connect();
    let up = pattern(3_000, 1);
    let down = pattern(2_500, 2);
    let (mut iu, mut id) = (0, 0);
    let sizes = [1usize, 7, 64, 255, 3, 190];
    let mut k = 0;
    let mut spins = 0;
    while iu < up.len() || id < down.len() {
        spins += 1;
        assert!(spins < 200_000, "stuck at {iu}/{id}");
        let n = sizes[k % sizes.len()];
        k += 1;
        let e = (iu + n).min(up.len());
        iu += rig.ffi_in(&up[iu..e]);
        lfd3k_poll(rig.now);
        let e = (id + n).min(down.len());
        let now = rig.now;
        id += rig.peer.host_input(&down[id..e], &mut rig.pr, now);
        rig.step();
    }
    rig.run_ms(8_000);
    assert_eq!(rig.peer_data(), &up[..], "FFI to peer");
    assert_eq!(rig.ffi_data(), &down[..], "peer to FFI");
}

#[test]
fn host_in_partial_take_mid_batch() {
    let _g = serial();
    let mut rig = Rig::new();
    rig.connect();
    let st = status(rig.now);
    assert_eq!(st.tier, 7, "SF5 at 12 dB");

    // 3000 bytes with CRs inside, offered in one call at one instant. The
    // send window holds at most WINDOW * CHUNK bytes (a little less when the
    // first slot is sealed short), so the call takes about that many and
    // reports the rest as not consumed.
    let data = pattern(3_000, 9);
    assert!(data.contains(&b'\r'), "the batch has CRs in it");
    let took = rig.ffi_in(&data);
    assert!(
        took > (WINDOW - 1) * CHUNK && took <= WINDOW * CHUNK,
        "took {took}"
    );
    assert!(took < data.len());
    // Offered again at once, the window is still full: nothing is taken.
    assert_eq!(rig.ffi_in(&data[took..]), 0);

    // Re-offer the untaken tail as the host would, until all of it is in.
    let mut off = took;
    let mut spins = 0;
    while off < data.len() {
        spins += 1;
        assert!(spins < 100_000);
        rig.step();
        off += rig.ffi_in(&data[off..]);
    }
    rig.run_ms(8_000);
    // Once, in order, nothing lost and nothing doubled.
    assert_eq!(rig.peer_data(), &data[..]);
    assert!(text(&rig.ffi_out).contains("CONNECT LORA SF5 BW500 62500/ARQ"));
}

#[test]
fn connected_status_reports_tier_and_last_rx() {
    let _g = serial();
    let mut rig = Rig::new();
    let idle = status(rig.now);
    assert_eq!(
        (idle.state, idle.tier, idle.rssi, idle.radio_ok),
        (0, -1, 0, 1)
    );
    rig.connect();
    let st = status(rig.now);
    assert_eq!(st.state, 4);
    assert_eq!(st.tier, 7);
    assert_eq!(st.rssi, -60);
    assert_eq!(st.snr_x4, 48);
    assert_eq!(st.radio_ok, 1);
    assert_ne!(st.lights & 0b0000_0011, 0, "OH and CD");
    assert_eq!(st.lights & 0b0100_0000, 0, "DC is never lit");
}
