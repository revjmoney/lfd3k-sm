//! Host tests of the C ABI against the fake HAL. The fake HAL and the static
//! device are process globals, so every test takes `serial()` first.
//!
//! `Rig` wires the FFI modem to a second core modem ("PEER") by carrying
//! frames between the fake SX1262 and the peer's radio, so a real call can
//! be set up and driven through the C entry points.

use std::collections::VecDeque;
use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use lfd3k_core::arq::{CHUNK, WINDOW};
use lfd3k_core::frame::{decode, FrameType};
use lfd3k_core::{Config, Modem, Radio, RxInfo, Tier};
use lfd3k_ffi::panic::{format_note, NOTE_LEN};
use lfd3k_ffi::*;

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
        let marker = b"/ARQ/LZ\r\n";
        let at = self
            .peer_out
            .windows(marker.len())
            .rposition(|w| w == marker)
            .expect("peer CONNECT line");
        &self.peer_out[at + marker.len()..]
    }

    fn ffi_data(&self) -> &[u8] {
        let marker = b"/ARQ/LZ\r\n";
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
        wire: 9,
        simnet: 9,
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

#[test]
fn data_mode_batch_fills_a_full_arq_chunk() {
    let _g = serial();
    let mut rig = Rig::new();
    rig.connect();
    // Let any frames from the connect settle, then drop what the chip saw.
    rig.run_ms(500);
    fake_hal::with(|h| h.tx_frames.clear());

    // About 400 bytes of "x\r" pairs in ONE host_in call (pins 160aaf8: in
    // data mode the bytes go in as one piece, so the first data frame is a
    // full chunk instead of one tiny frame per line).
    let data: Vec<u8> = b"x\r".iter().copied().cycle().take(400).collect();
    assert_eq!(rig.ffi_in(&data), 400);

    // Poll (without handing frames to the peer) until a data frame is sent.
    let mut first = None;
    for _ in 0..2_000 {
        rig.now += 1;
        lfd3k_poll(rig.now);
        let tx = fake_hal::with(|h| std::mem::take(&mut h.tx_frames));
        for (_, f) in tx {
            let fr = decode(&f).expect("a valid frame");
            if fr.header.ftype == FrameType::Data {
                // With /LZ the chunk goes out compressed: count host bytes.
                let mut plain = [0u8; CHUNK];
                first = Some(if fr.header.flags & lfd3k_core::frame::FLAG_LZ != 0 {
                    lfd3k_core::lz::decompress(fr.payload, &mut plain).expect("valid LZ")
                } else {
                    fr.payload.len()
                });
                break;
            }
        }
        if first.is_some() {
            break;
        }
    }
    assert_eq!(first, Some(CHUNK), "first data frame is a full ARQ chunk");
}

// ---- Phase 3: Wi-Fi settings, the wire, SimNet and the channel trace. ----

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Byte compare that reports the first difference, not 10 KB of numbers.
fn same(got: &[u8], want: &[u8], what: &str) {
    let at = got.iter().zip(want).position(|(a, b)| a != b);
    assert!(
        at.is_none() && got.len() == want.len(),
        "{what}: got {} bytes, want {}, first difference at {at:?}",
        got.len(),
        want.len()
    );
}

/// `lfd3k_take_wire_request` with a 64 byte host buffer.
fn wire_req() -> Option<(String, u16)> {
    let mut host = [0 as c_char; 64];
    let mut port = 0u16;
    if unsafe { lfd3k_take_wire_request(host.as_mut_ptr(), host.len(), &mut port) } {
        let h = unsafe { CStr::from_ptr(host.as_ptr()) };
        Some((h.to_str().unwrap().to_string(), port))
    } else {
        None
    }
}

fn wire_out_all() -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 100];
    loop {
        let n = unsafe { lfd3k_wire_out(buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            return all;
        }
        all.extend_from_slice(&buf[..n]);
    }
}

fn wire_in(bytes: &[u8], now: u32) -> usize {
    unsafe { lfd3k_wire_in(bytes.as_ptr(), bytes.len(), now) }
}

fn take_save() -> Option<Vec<u8>> {
    let mut blob = [0u8; 1024];
    let mut len = 0usize;
    if unsafe { lfd3k_take_save(blob.as_mut_ptr(), blob.len(), &mut len) } {
        Some(blob[..len].to_vec())
    } else {
        None
    }
}

fn load(blob: &[u8]) -> i32 {
    unsafe { lfd3k_load_settings(blob.as_ptr(), blob.len()) }
}

fn wifi_cfg() -> lfd3k_wifi_cfg_t {
    let mut c = lfd3k_wifi_cfg_t {
        ap_on: 9,
        sta_set: 9,
        ap_ssid: [0xAA; 33],
        ap_pass: [0xAA; 64],
        sta_ssid: [0xAA; 33],
        sta_pass: [0xAA; 64],
    };
    unsafe { lfd3k_get_wifi_config(&mut c) };
    c
}

fn cfg_str(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).expect("NUL terminated");
    String::from_utf8(b[..n].to_vec()).unwrap()
}

/// Types a command line (command mode) and returns the modem's reply.
fn cmd(line: &str, now: u32) -> String {
    let mut v = line.as_bytes().to_vec();
    v.push(b'\r');
    assert_eq!(host_in(&v, now), v.len(), "{line}");
    lfd3k_poll(now);
    text(&host_out())
}

#[test]
fn wire_bytes_cross_in_order() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    assert!(cmd("AT+FWD=S,h.example,7000", 1).ends_with("\r\nOK\r\n"));
    // Dial: nothing to close, one request, taken once; a short buffer is
    // refused and keeps the request.
    assert!(cmd("ATDT S", 2).contains("DIALING S"));
    assert!(!lfd3k_take_wire_close());
    assert_eq!(status(3).wire, 1);
    let mut small = [0 as c_char; 32];
    let mut port = 0u16;
    assert!(!unsafe { lfd3k_take_wire_request(small.as_mut_ptr(), 32, &mut port) });
    assert!(!unsafe { lfd3k_take_wire_request(ptr::null_mut(), 64, &mut port) });
    assert_eq!(wire_req(), Some(("h.example".to_string(), 7000)));
    assert_eq!(wire_req(), None);
    // Connected: CONNECT WIFI TCP, data mode.
    lfd3k_wire_event(1, 10);
    lfd3k_poll(11);
    assert!(text(&host_out()).contains("\r\nCONNECT WIFI TCP\r\n"));
    assert_eq!(status(12).wire, 2);

    // Both directions at once, in odd sized pieces with polls in between:
    // everything arrives once and in order (no '+', so no escape logic).
    let up = pattern(3_000, 21);
    let down = pattern(5_000, 22);
    assert!(up.contains(&b'\r') && down.contains(&b'\r'));
    let (mut iu, mut id) = (0, 0);
    let (mut got_wire, mut got_host) = (Vec::new(), Vec::new());
    let sizes = [1usize, 7, 64, 255, 3, 190, 2048];
    let mut now = 20u32;
    for spin in 0.. {
        assert!(spin < 100_000, "stuck at {iu}/{id}");
        if iu >= up.len()
            && id >= down.len()
            && got_wire.len() == up.len()
            && got_host.len() == down.len()
        {
            break;
        }
        let n = sizes[spin % sizes.len()];
        let e = (iu + n).min(up.len());
        iu += host_in(&up[iu..e], now);
        got_wire.extend_from_slice(&wire_out_all());
        let e = (id + n).min(down.len());
        id += wire_in(&down[id..e], now);
        now += 1;
        lfd3k_poll(now);
        got_host.extend_from_slice(&host_out());
    }
    same(&got_wire, &up, "host to wire");
    same(&got_host, &down, "wire to host");

    // A burst bigger than the ring: only part is taken, the rest is offered
    // again and nothing is lost (the platform keeps what was not taken).
    let burst = pattern(10_000, 23);
    let took = wire_in(&burst, now);
    assert!(took > 0 && took < burst.len(), "took {took}");
    let (mut off, mut got) = (took, host_out());
    let mut spins = 0;
    while off < burst.len() {
        spins += 1;
        assert!(spins < 10_000);
        now += 1;
        lfd3k_poll(now);
        got.extend_from_slice(&host_out());
        off += wire_in(&burst[off..], now);
    }
    // What the core still holds drains over the next polls.
    for _ in 0..100 {
        now += 1;
        lfd3k_poll(now);
        got.extend_from_slice(&host_out());
    }
    same(&got, &burst, "burst");

    // Escape, hang up: the close is asked for once and the status clears.
    now += 1_100;
    lfd3k_poll(now);
    assert_eq!(host_in(b"+++", now), 3);
    now += 1_100;
    lfd3k_poll(now);
    assert!(text(&host_out()).ends_with("\r\nOK\r\n"));
    assert!(cmd("ATH", now).ends_with("\r\nNO CARRIER\r\n"));
    assert!(lfd3k_take_wire_close());
    assert!(!lfd3k_take_wire_close(), "once");
    assert_eq!(status(now).wire, 0);

    // The far end closing (event 3) gives NO CARRIER and no close request.
    assert!(cmd("ATDT S", now + 1).contains("DIALING"));
    assert!(wire_req().is_some());
    lfd3k_wire_event(1, now + 2);
    lfd3k_wire_event(7, now + 2); // unknown events are ignored
    lfd3k_poll(now + 3);
    assert_eq!(status(now + 3).wire, 2);
    host_out();
    lfd3k_wire_event(3, now + 4);
    lfd3k_poll(now + 5);
    assert!(text(&host_out()).ends_with("\r\nNO CARRIER\r\n"));
    assert!(!lfd3k_take_wire_close());
    assert_eq!(status(now + 5).wire, 0);
    // Refused: NO CARRIER at once.
    assert!(cmd("ATDT S", now + 10).contains("DIALING"));
    assert!(wire_req().is_some());
    lfd3k_wire_event(2, now + 11);
    lfd3k_poll(now + 12);
    assert!(text(&host_out()).ends_with("\r\nNO CARRIER\r\n"));

    // Everything is a safe no-op with null pointers.
    assert_eq!(unsafe { lfd3k_wire_in(ptr::null(), 5, 0) }, 0);
    assert_eq!(unsafe { lfd3k_wire_out(ptr::null_mut(), 5) }, 0);
}

#[test]
fn settings_load_save_roundtrip_c_abi() {
    let _g = serial();
    // Not initialised (a failed init leaves the device empty): every settings
    // entry point refuses or does nothing.
    assert_eq!(init(""), -4);
    assert_eq!(load(&[0u8; 899]), -1);
    assert!(take_save().is_none());
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    assert!(take_save().is_none());
    assert!(!lfd3k_take_wifi_changed() && !lfd3k_show_ap_pass());
    lfd3k_note_ap_client_joined();
    lfd3k_set_stack_free(5);
    let c = wifi_cfg();
    assert_eq!((c.ap_on, c.sta_set), (0, 0));
    assert_eq!(cfg_str(&c.ap_ssid), "");
    assert_eq!(cfg_str(&c.ap_pass), "");
    assert_eq!(init("AXISMODEM-1234"), 0);
    cmd("ATE0", 0);
    // First boot: no blob, or a rejected one. The platform loads, gets -1,
    // and asks for defaults with a generated password.
    assert_eq!(load(&[0xFF; 1024]), -1, "erased flash");
    assert_eq!(load(&[]), -1);
    assert_eq!(unsafe { lfd3k_load_settings(ptr::null(), 899) }, -1);
    assert!(take_save().is_none(), "a rejected blob asks for nothing");
    assert!(!lfd3k_take_wifi_changed());
    // Bad passwords change nothing: null, short, long, comma, non-ASCII.
    unsafe { lfd3k_default_settings(ptr::null()) };
    for bad in [
        "short",
        &"p".repeat(64),
        "has,comma-123",
        "caf\u{e9}caf\u{e9}",
    ] {
        unsafe { lfd3k_default_settings(cstr(bad).as_ptr()) };
        assert!(take_save().is_none(), "{bad}");
        assert_eq!(cfg_str(&wifi_cfg().ap_pass), "");
    }
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    let first = take_save().expect("defaults ask for a save");
    assert!(take_save().is_none(), "once");
    assert_eq!(first.len(), 900);
    assert!(lfd3k_take_wifi_changed());
    let c = wifi_cfg();
    assert_eq!((c.ap_on, c.sta_set), (1, 0));
    assert_eq!(cfg_str(&c.ap_ssid), "AXISMODEM-1234");
    assert_eq!(cfg_str(&c.ap_pass), "hotspotpw234");
    assert!(lfd3k_show_ap_pass());

    // Configure over the AT side, save with AT&W.
    assert!(cmd("AT+FWD=AXIS,192.168.1.164,23", 1).ends_with("OK\r\n"));
    assert!(cmd("AT+FWD=echo,10.0.0.9,7000", 1).ends_with("OK\r\n"));
    assert!(cmd("AT+PUB=echo", 1).ends_with("OK\r\n"));
    assert!(cmd("ATS7=45", 1).ends_with("OK\r\n"));
    assert!(cmd("AT+WSTA=HomeNet,Rev-Home-Pass-1", 1).ends_with("OK\r\n"));
    assert!(lfd3k_take_wifi_changed(), "AT+WSTA flags the platform");
    assert!(!lfd3k_take_wifi_changed());
    let c = wifi_cfg();
    assert_eq!(c.sta_set, 1);
    assert_eq!(cfg_str(&c.sta_ssid), "HomeNet");
    assert_eq!(
        cfg_str(&c.sta_pass),
        "Rev-Home-Pass-1",
        "the platform needs it"
    );
    assert!(cmd("AT&W", 2).ends_with("OK\r\n"));
    // A buffer under 1 KB is refused and the request is kept.
    let mut small = [0u8; 900];
    let mut len = 0usize;
    assert!(!unsafe { lfd3k_take_save(small.as_mut_ptr(), small.len(), &mut len) });
    assert!(!unsafe { lfd3k_take_save(ptr::null_mut(), 1024, &mut len) });
    assert!(!unsafe { lfd3k_take_save(small.as_mut_ptr(), 1024, ptr::null_mut()) });
    let blob = take_save().expect("the kept request");
    assert_eq!(blob.len(), 900);
    assert!(!lfd3k_show_ap_pass(), "saved once with a client network");

    // Power cycle: init, load the blob, and it all comes back.
    assert_eq!(init("AXISMODEM-1234"), 0);
    cmd("ATE0", 0);
    assert_eq!(load(&blob), 0);
    assert!(lfd3k_take_wifi_changed(), "a load flags the platform");
    let c = wifi_cfg();
    assert_eq!(cfg_str(&c.ap_pass), "hotspotpw234");
    assert_eq!(cfg_str(&c.sta_ssid), "HomeNet");
    let t = cmd("AT+FWD?", 3);
    assert!(
        t.contains("FWD AXIS 192.168.1.164:23 T") && t.contains("FWD echo 10.0.0.9:7000 R"),
        "{t:?}"
    );
    // (Echo came back on with the blob: the saved profile had E1.)
    assert!(cmd("AT+PUB?", 3).ends_with("\r\nPUB echo\r\n\r\nOK\r\n"));
    assert!(cmd("ATS7?", 3).ends_with("\r\n045\r\n\r\nOK\r\n"));
    let v = cmd("AT&V", 3);
    assert!(
        v.contains("PWSHOW 0") && !v.contains("Rev-Home-Pass-1"),
        "{v:?}"
    );
    // Saving again gives a blob equal to the one loaded.
    cmd("AT&W", 4);
    assert_eq!(take_save().unwrap(), blob);

    // The password is hidden once a device joins (and a save is asked for).
    assert_eq!(init("AXISMODEM-1234"), 0);
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    take_save();
    assert!(lfd3k_show_ap_pass());
    lfd3k_note_ap_client_joined();
    assert!(!lfd3k_show_ap_pass());
    let hidden = take_save().expect("joining asks for a save");
    assert_ne!(hidden, first, "the blob now says hidden");
    assert_eq!(load(&hidden), 0);
    assert!(!lfd3k_show_ap_pass());
}

#[test]
fn heap_min_c_abi() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    assert!(!cmd("AT&V", 1).contains("HEAP MIN"));
    lfd3k_set_heap_min(98_304);
    assert!(cmd("AT&V", 2).contains("\r\nHEAP MIN 98304\r\n"));
}

#[test]
fn wifi_cfg_debug_masks_passwords() {
    let _g = serial();
    assert_eq!(init("AXISMODEM-1234"), 0);
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    take_save();
    cmd("AT+WSTA=HomeNet,Rev-Home-Pass-1", 1);
    let shown = format!("{:?}", wifi_cfg());
    assert!(
        shown.contains("HomeNet") && shown.contains("********"),
        "{shown}"
    );
    assert!(!shown.contains("hotspotpw234") && !shown.contains("Rev-Home-Pass-1"));
}

#[test]
fn hotspot_join_saves_only_flag_c_abi() {
    let _g = serial();
    assert_eq!(init("AXISMODEM-1234"), 0);
    cmd("ATE0", 0);
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    let first = take_save().expect("first boot saves");
    // Unsaved live changes, then a device joins the hotspot.
    assert!(cmd("ATS0=1", 1).ends_with("OK\r\n"));
    assert!(cmd("AT+WSTA=HomeNet,Rev-Home-Pass-1", 1).ends_with("OK\r\n"));
    lfd3k_note_ap_client_joined();
    let blob = take_save().expect("the join asks for a save");
    assert_ne!(blob, first, "the flag changed");
    assert_eq!(blob.len(), first.len());
    // A power cycle with that blob: flag cleared, nothing else leaked in.
    assert_eq!(init("AXISMODEM-1234"), 0);
    cmd("ATE0", 0);
    assert_eq!(load(&blob), 0);
    assert!(!lfd3k_show_ap_pass());
    assert!(cmd("ATS0?", 2).ends_with("\r\n000\r\n\r\nOK\r\n"));
    assert!(cmd("AT+WSTA?", 2).contains("WSTA OFF"));
    // A second join asks for nothing.
    lfd3k_note_ap_client_joined();
    assert!(take_save().is_none());
}

#[test]
fn wifi_status_line_and_stack_free_c_abi() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    assert_eq!(cmd("AT+WIFI?", 1), "\r\nWIFI OFF\r\n\r\nOK\r\n");
    unsafe {
        lfd3k_set_wifi_status(cstr("AP AXISMODEM-1234 0/4  STA 192.168.1.50 -61 dBm").as_ptr())
    };
    assert_eq!(
        cmd("AT+WIFI?", 1),
        "\r\nAP AXISMODEM-1234 0/4  STA 192.168.1.50 -61 dBm\r\n\r\nOK\r\n"
    );
    // 96 bytes are kept, control bytes become '?', a null is ignored.
    unsafe { lfd3k_set_wifi_status(cstr(&"w".repeat(200)).as_ptr()) };
    let t = cmd("AT+WIFI?", 2);
    assert!(
        t.contains(&"w".repeat(96)) && !t.contains(&"w".repeat(97)),
        "{t:?}"
    );
    unsafe { lfd3k_set_wifi_status(CString::new(vec![b'a', 0x01, b'b']).unwrap().as_ptr()) };
    assert_eq!(cmd("AT+WIFI?", 3), "\r\na?b\r\n\r\nOK\r\n");
    unsafe { lfd3k_set_wifi_status(ptr::null()) };
    assert!(cmd("AT+WIFI?", 4).contains("a?b"));
    // STACK FREE shows in AT&V only after the platform reports it.
    assert!(!cmd("AT&V", 5).contains("STACK FREE"));
    lfd3k_set_stack_free(9_216);
    assert!(cmd("AT&V", 6).contains("\r\nSTACK FREE 9216\r\n"));
}

#[test]
fn status_wire_and_simnet_fields() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    let st = status(1);
    assert_eq!((st.wire, st.simnet, st.state), (0, 0, 0));
    cmd("AT+FWD=S,h.example,7000", 1);
    cmd("ATDT S", 2);
    assert_eq!(status(3).wire, 1, "dialing");
    wire_req();
    lfd3k_wire_event(1, 4);
    lfd3k_poll(5);
    let st = status(6);
    assert_eq!(st.wire, 2, "online");
    assert!(st.lights & 0b10 != 0, "CD is on for a Wi-Fi call");
    lfd3k_wire_event(3, 7);
    lfd3k_poll(8);
    assert_eq!(status(9).wire, 0);
    host_out();

    // SimNet on and off shows in the status.
    assert_eq!(cmd("AT+SIMNET=10.1.2.3", 20), "\r\nOK\r\n");
    assert_eq!(status(21).simnet, 1);
    assert_eq!(cmd("AT+SIMNET=0", 22), "\r\nOK\r\n");
    assert_eq!(status(23).simnet, 0);
    // A new init is on the real radio again (SimNet is never remembered).
    cmd("AT+SIMNET=10.1.2.3", 24);
    assert_eq!(init("CALLER"), 0);
    assert_eq!(status(0).simnet, 0);

    // Bridging: a radio call answered here goes to the published forward.
    let mut rig = Rig::new();
    rig.ffi_in(b"ATS0=1\rATE0\rAT+FWD=BBS,h.example,2323\rAT+PUB=BBS\r");
    rig.run_ms(5);
    let now = rig.now;
    assert_eq!(rig.peer.host_input(b"ATDT CALLER\r", &mut rig.pr, now), 12);
    let ok = rig.run_until(30_000, |r| {
        status(r.now).wire == 3 && status(r.now).state != 0
    });
    assert!(
        ok,
        "FFI: {:?}\nPEER: {:?}",
        text(&rig.ffi_out),
        text(&rig.peer_out)
    );
    // No other state shows while the bridge's connection is pending.
    assert_eq!(wire_req(), Some(("h.example".to_string(), 2323)));
    lfd3k_wire_event(1, rig.now);
    rig.run_ms(5);
    assert_eq!(status(rig.now).wire, 3, "bridging");
    assert!(text(&rig.ffi_out).contains("\r\nBRIDGE BBS\r\n"));
    // Server bytes reach the radio caller, the caller's bytes reach the wire.
    let banner = b"WELCOME TO THE BBS\r\n";
    assert_eq!(wire_in(banner, rig.now), banner.len());
    let mut from_peer = Vec::new();
    let t0 = rig.now;
    let mut typed = false;
    while rig.now - t0 < 20_000 {
        rig.run_ms(10);
        from_peer.extend_from_slice(&wire_out_all());
        let seen = rig.peer_out.windows(banner.len()).any(|w| w == banner);
        if seen && !typed {
            let now = rig.now;
            assert_eq!(rig.peer.host_input(b"hello", &mut rig.pr, now), 5);
            typed = true;
        }
        if typed && from_peer.ends_with(b"hello") {
            break;
        }
    }
    assert!(
        typed,
        "the banner crossed the radio: {:?}",
        text(&rig.peer_out)
    );
    assert_eq!(from_peer, b"hello");
    assert_eq!(status(rig.now).wire, 3);
}

/// Pass the SimNet switch on the C ABI and return the peer address taken.
fn take_simnet() -> (i32, [u8; 4]) {
    let mut ip = [0u8; 4];
    let r = unsafe { lfd3k_take_simnet(ip.as_mut_ptr()) };
    (r, ip)
}

fn spi_ops() -> Vec<u8> {
    fake_hal::with(|h| h.xfers.iter().map(|t| t[0]).collect())
}

/// A CallReq from PEER to `callee` as the SimNet datagram a peer would send.
fn call_req_datagram(callee: &str, tier: Tier) -> Vec<u8> {
    let mut p = [0u8; 40];
    let n = lfd3k_core::frame::call_req_payload("PEER", callee, &mut p).unwrap();
    let hdr = lfd3k_core::frame::Header {
        ftype: FrameType::CallReq,
        flags: 0,
        call_id: 9,
        seq: 0,
        ack: 0,
    };
    let mut f = [0u8; 255];
    let m = lfd3k_core::frame::encode(&hdr, &p[..n], &mut f).unwrap();
    let mut d = [0u8; lfd3k_core::simnet::DGRAM_MAX];
    let k = lfd3k_core::simnet::build(tier, 32, -70, &f[..m], &mut d);
    d[..k].to_vec()
}

#[test]
fn simnet_switch_never_touches_spi_tx() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    lfd3k_poll(1);
    // Control: on the SX1262 a dial goes out through SetTx.
    cmd("ATDT PEER", 2);
    assert!(
        fake_hal::with(|h| !h.tx_frames.is_empty()),
        "the chip transmitted"
    );
    assert!(spi_ops().contains(&0x83));
    cmd("ATH", 3);
    cmd("ATH", 4);

    // Switch: the chip goes quiet, the dial becomes a datagram.
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    lfd3k_poll(1);
    assert_eq!(cmd("AT+SIMNET=192.168.1.77", 5), "\r\nOK\r\n");
    assert_eq!(take_simnet(), (1, [192, 168, 1, 77]));
    assert_eq!(take_simnet().0, -1, "taken once");
    fake_hal::with(|h| {
        h.xfers.clear();
        h.tx_frames.clear();
        h.udp_out.clear();
    });
    let t = cmd("ATDT PEER", 10);
    assert!(t.contains("DIALING PEER"), "{t:?}");
    lfd3k_poll(20);
    lfd3k_poll(30);
    let (ops, tx, udp) = fake_hal::with(|h| {
        (
            h.xfers.iter().map(|t| t[0]).collect::<Vec<u8>>(),
            h.tx_frames.len(),
            h.udp_out.clone(),
        )
    });
    assert!(
        !ops.contains(&0x83),
        "SetTx on the SPI bus in SimNet: {ops:?}"
    );
    assert_eq!(tx, 0);
    assert!(ops.is_empty(), "no SPI traffic at all in SimNet: {ops:?}");
    assert_eq!(udp.len(), 1, "one CallReq datagram");
    let (tier, snr, rssi, frame) = lfd3k_core::simnet::parse(&udp[0]).expect("a SimNet datagram");
    assert_eq!(tier, Tier::Sf12, "calls start on the robust tier");
    assert_eq!((snr, rssi), (32, -70));
    let f = decode(frame).unwrap();
    assert_eq!(f.header.ftype, FrameType::CallReq);
    assert_eq!(
        lfd3k_core::frame::parse_call_req(f.payload).unwrap(),
        ("CALLER", "PEER")
    );
    // Hanging up goes out as datagrams too.
    cmd("ATH", 40);
    lfd3k_poll(50);
    assert!(!spi_ops().contains(&0x83));
    assert_eq!(fake_hal::with(|h| h.tx_frames.len()), 0);
}

#[test]
fn simnet_off_returns_to_listening() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    cmd("ATE0", 0);
    lfd3k_poll(1);
    assert!(
        fake_hal::with(|h| h.rx_mode),
        "listening on the chip at boot"
    );
    cmd("AT+SIMNET=10.0.0.2", 2);
    lfd3k_poll(3);
    assert!(
        !fake_hal::with(|h| h.rx_mode),
        "the chip is in standby in SimNet"
    );
    assert_eq!(take_simnet().0, 1);
    // While on, a CallReq datagram for us rings after its airtime (the HAL
    // clock is what the simulated radio reads).
    let d = call_req_datagram("CALLER", Tier::Sf12);
    assert_eq!(unsafe { lfd3k_udp_in(d.as_ptr(), d.len(), 0) }, d.len());
    fake_hal::with(|h| h.now_us += 5_000_000);
    lfd3k_poll(100);
    assert!(text(&host_out()).contains("\r\nRING\r\n"));
    assert_eq!(status(101).simnet, 1);
    cmd("ATH", 110);
    host_out();

    // Off again: the next poll puts the chip back in receive, and the status
    // and AT&V say so.
    fake_hal::with(|h| h.xfers.clear());
    assert_eq!(cmd("AT+SIMNET=0", 120), "\r\nOK\r\n");
    assert_eq!(take_simnet().0, 0, "close the UDP socket");
    assert_eq!(take_simnet().0, -1);
    lfd3k_poll(121);
    assert!(fake_hal::with(|h| h.rx_mode), "listening on the chip again");
    assert!(spi_ops().contains(&0x82), "SetRx");
    assert_eq!(status(122).simnet, 0);
    assert!(!cmd("AT&V", 123).contains("SIMNET"));
    assert_eq!(lfd3k_simnet_due_ms(), u32::MAX);
    // Datagrams are dropped now (no ring), and a dial uses the chip again.
    assert_eq!(unsafe { lfd3k_udp_in(d.as_ptr(), d.len(), 0) }, d.len());
    fake_hal::with(|h| h.now_us += 5_000_000);
    lfd3k_poll(200);
    assert!(!text(&host_out()).contains("RING"));
    fake_hal::with(|h| {
        h.tx_frames.clear();
        h.udp_out.clear();
    });
    cmd("ATDT PEER", 210);
    assert!(fake_hal::with(
        |h| !h.tx_frames.is_empty() && h.udp_out.is_empty()
    ));
    // Null and early calls are harmless.
    assert_eq!(unsafe { lfd3k_udp_in(ptr::null(), 5, 0) }, 0);
    assert_eq!(unsafe { lfd3k_take_simnet(ptr::null_mut()) }, -1);
}

#[test]
fn simnet_not_saved_in_blob() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    unsafe { lfd3k_default_settings(cstr("hotspotpw234").as_ptr()) };
    cmd("ATE0", 0);
    cmd("AT&W", 1);
    let plain = take_save().expect("blob");
    // The same settings with SimNet on save to the very same bytes.
    assert!(cmd("AT+SIMNET=10.9.8.7", 2).ends_with("OK\r\n"));
    cmd("AT&W", 3);
    let with = take_save().expect("blob");
    assert_eq!(with, plain);
    assert!(!with.windows(4).any(|w| w == [10, 9, 8, 7]));
    assert!(!with.windows(7).any(|w| w == b"10.9.8."));
    // After a "reboot" that loads it, SimNet is off and the chip is in use.
    assert_eq!(init("CALLER"), 0);
    assert_eq!(load(&with), 0);
    assert_eq!(cmd("AT+SIMNET?", 4), "\r\nSIMNET OFF\r\n\r\nOK\r\n");
    assert_eq!(take_simnet().0, -1);
    assert_eq!(status(5).simnet, 0);
}

#[test]
fn rf_trace_samples_while_listening() {
    let _g = serial();
    let mut dbm = [0i8; 64];
    let mut tx = [9u8; 64];
    // Not initialised (a failed init leaves the device empty): nothing to read.
    assert_eq!(init(""), -4);
    assert_eq!(
        unsafe { lfd3k_rf_trace(dbm.as_mut_ptr(), tx.as_mut_ptr(), 64) },
        0
    );
    assert_eq!(init("CALLER"), 0);
    fake_hal::with(|h| h.rssi_inst = 200); // -100 dBm
    let trace = |cap: usize| {
        let (mut d, mut t) = (vec![0i8; cap], vec![9u8; cap]);
        let n = unsafe { lfd3k_rf_trace(d.as_mut_ptr(), t.as_mut_ptr(), cap) };
        (d[..n].to_vec(), t[..n].to_vec())
    };
    assert_eq!(trace(64).0.len(), 0, "no sample yet");
    // Polled every 10 ms for a second: one sample per 50 ms.
    for ms in (0..=1_000).step_by(10) {
        lfd3k_poll(ms);
    }
    let (d, t) = trace(64);
    assert!((19..=21).contains(&d.len()), "{} samples", d.len());
    assert!(d.iter().all(|&v| v == -100), "{d:?}");
    assert!(t.iter().all(|&v| v == 0), "not transmitting");
    // The level changes: newest samples show it, oldest first.
    fake_hal::with(|h| h.rssi_inst = 160); // -80 dBm
    for ms in (1_010..=1_500).step_by(10) {
        lfd3k_poll(ms);
    }
    let (d2, _) = trace(64);
    assert_eq!(d2.last(), Some(&-80));
    assert_eq!(d2[0], -100);
    let at = d2.iter().position(|&v| v == -80).unwrap();
    assert!(
        d2[at..].iter().all(|&v| v == -80),
        "ordered in time: {d2:?}"
    );
    // The ring holds 64; a smaller buffer gets the newest ones.
    for ms in (1_510..=10_000).step_by(10) {
        lfd3k_poll(ms);
    }
    let (d3, t3) = trace(64);
    assert_eq!((d3.len(), t3.len()), (64, 64));
    assert!(d3.iter().all(|&v| v == -80));
    let (d4, _) = trace(10);
    assert_eq!(d4.len(), 10);
    // Either pointer may be null; a zero cap gives nothing.
    assert_eq!(
        unsafe { lfd3k_rf_trace(ptr::null_mut(), tx.as_mut_ptr(), 8) },
        8
    );
    assert_eq!(
        unsafe { lfd3k_rf_trace(dbm.as_mut_ptr(), ptr::null_mut(), 8) },
        8
    );
    assert_eq!(
        unsafe { lfd3k_rf_trace(dbm.as_mut_ptr(), tx.as_mut_ptr(), 0) },
        0
    );
    // Values are clamped to the signed byte range (the chip can say -127.5).
    fake_hal::with(|h| h.rssi_inst = 255);
    for ms in (10_010..=10_100).step_by(10) {
        lfd3k_poll(ms);
    }
    assert_eq!(trace(64).0.last(), Some(&-127));
}

#[test]
fn rf_trace_marks_tx() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    fake_hal::with(|h| h.rssi_inst = 200);
    let trace = || {
        let (mut d, mut t) = ([0i8; 64], [9u8; 64]);
        let n = unsafe { lfd3k_rf_trace(d.as_mut_ptr(), t.as_mut_ptr(), 64) };
        (d[..n].to_vec(), t[..n].to_vec())
    };
    for ms in (0..=200).step_by(10) {
        lfd3k_poll(ms);
    }
    let before = trace().0.len();
    assert!(before >= 3);
    // SimNet branch: the simulated radio is transmitting for the frame's
    // airtime on the HAL clock, which the fake advances by hand. (The chip
    // branch is f_trace_marks_tx_on_the_chip.)
    cmd("AT+SIMNET=10.0.0.2", 210);
    lfd3k_poll(215);
    host_out();
    cmd("ATDT PEER", 220);
    fake_hal::with(|h| h.xfers.clear());
    for ms in (230..=400).step_by(10) {
        lfd3k_poll(ms);
    }
    let (d, t) = trace();
    let marks: Vec<usize> = (0..t.len()).filter(|&i| t[i] == 1).collect();
    assert!(!marks.is_empty(), "the dial shows as tx: {t:?}");
    assert!(
        marks.iter().all(|&i| d[i] == 0),
        "tx samples carry no level"
    );
    // Samples before the dial are listening ones (tx 0, -100 dBm).
    assert!(t[..before].iter().all(|&v| v == 0) && d[..before].iter().all(|&v| v == -100));
    // And the marks are contiguous with the transmission, then listening again
    // once its airtime has passed on the HAL clock.
    fake_hal::with(|h| h.now_us += 10_000_000);
    for ms in (410..=700).step_by(10) {
        lfd3k_poll(ms);
    }
    let (_, t2) = trace();
    assert_eq!(t2.last(), Some(&0), "listening again after the airtime");
}

#[test]
fn rf_trace_marks_tx_on_the_chip() {
    let _g = serial();
    assert_eq!(init("CALLER"), 0);
    fake_hal::with(|h| h.rssi_inst = 200); // -100 dBm
    let newest = || {
        let (mut d, mut t) = ([0i8; 1], [9u8; 1]);
        let n = unsafe { lfd3k_rf_trace(d.as_mut_ptr(), t.as_mut_ptr(), 1) };
        assert_eq!(n, 1);
        (d[0], t[0])
    };
    for ms in (0..=200).step_by(10) {
        lfd3k_poll(ms);
    }
    assert_eq!(newest(), (-100, 0), "listening sample with the level");
    // The fake chip raises TxDone inside SetTx, so a real transmission (it
    // lasts the airtime) is made by taking the bit away again: the driver
    // stays in TX until it sees TxDone. Dial without polling first.
    assert_eq!(host_in(b"ATDT PEER\r", 205), 10);
    assert!(
        fake_hal::with(|h| !h.tx_frames.is_empty()),
        "the chip was told to send"
    );
    fake_hal::with(|h| {
        h.irq &= !0x0001;
        h.xfers.clear();
    });
    // 50 ms after the last sample: due. The chip is in TX, so the sample is
    // a tx mark with no level, and it costs no SPI read (GetRssiInst, 0x15).
    lfd3k_poll(250);
    assert_eq!(newest(), (0, 1), "tx sample");
    assert!(
        !spi_ops().contains(&0x15),
        "no RSSI read while transmitting: {:?}",
        spi_ops()
    );
    lfd3k_poll(300);
    assert_eq!(newest(), (0, 1), "still transmitting");
    // TxDone arrives: back to listening, and the next sample has the level.
    fake_hal::with(|h| h.irq |= 0x0001);
    lfd3k_poll(310);
    lfd3k_poll(360);
    assert_eq!(newest(), (-100, 0), "listening again");
    assert!(spi_ops().contains(&0x15));
}
