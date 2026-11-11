//! Phase 5 tests of `AT+MODE` on the multimode board: the parser, the
//! query, and the core's request handling (idle only, refused during a call
//! or a scan, later lines of the same chunk dropped, the result printed
//! through the normal result path, ATV0). The modem sits on a radio that
//! records what it sends, and every frame it hears is built here.

use lfd3k_core::discovery::{beacon_payload, Beacon};
use lfd3k_core::frame::{call_req_payload, encode, FrameType, Header};
use lfd3k_core::{parse_line, AtCmd, AtError, Config, ModeTarget, Modem, Radio, RxInfo, Tier};

struct Rec {
    sent: Vec<Vec<u8>>,
}

impl Radio for Rec {
    fn send(&mut self, _tier: Tier, frame: &[u8]) {
        self.sent.push(frame.to_vec());
    }
    fn listen(&mut self, _tier: Tier) {}
    fn busy(&self) -> bool {
        false
    }
}

struct Rig {
    m: Modem,
    r: Rec,
    now: u32,
    out: Vec<u8>,
}

impl Rig {
    /// A modem with echo off, all four images present (the common case).
    fn new() -> Rig {
        let mut rig = Rig::bare();
        rig.m.set_mode_slots(0x0F);
        rig
    }

    /// A modem on which the firmware has not reported mode slots (the PC, or
    /// the single app layout).
    fn bare() -> Rig {
        let mut rig = Rig {
            m: Modem::new(Config::new("MODETEST").expect("name")),
            r: Rec { sent: Vec::new() },
            now: 0,
            out: Vec::new(),
        };
        rig.tick();
        rig.line("ATE0");
        rig.out.clear();
        rig
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 256];
        loop {
            let n = self.m.host_output(&mut buf);
            if n == 0 {
                break;
            }
            self.out.extend_from_slice(&buf[..n]);
        }
    }

    fn tick(&mut self) {
        self.now += 1;
        self.m.tick(&mut self.r, self.now);
        self.drain();
    }

    fn run(&mut self, ms: u32) {
        for _ in 0..ms {
            self.tick();
        }
    }

    /// Raw bytes into the host port; returns (bytes taken, what came out).
    fn send(&mut self, bytes: &[u8]) -> (usize, String) {
        let mark = self.out.len();
        let n = self.m.host_input(bytes, &mut self.r, self.now);
        self.drain();
        (n, String::from_utf8_lossy(&self.out[mark..]).into_owned())
    }

    /// One AT line (CR added); returns what the host got meanwhile.
    fn line(&mut self, s: &str) -> String {
        let mut text = s.as_bytes().to_vec();
        text.push(b'\r');
        let (n, out) = self.send(&text);
        assert_eq!(n, text.len(), "all of {s:?} is taken");
        out
    }

    /// What the modem printed since the last call.
    fn after(&mut self, f: impl FnOnce(&mut Modem)) -> String {
        let mark = self.out.len();
        f(&mut self.m);
        self.drain();
        String::from_utf8_lossy(&self.out[mark..]).into_owned()
    }
}

const OK: &str = "\r\nOK\r\n";
const ERROR: &str = "\r\nERROR\r\n";

fn info() -> RxInfo {
    RxInfo {
        tier: Tier::Sf12,
        rssi_dbm: -100,
        snr_x4: 20,
    }
}

fn frame(ft: FrameType, call_id: u8, payload: &[u8]) -> Vec<u8> {
    let h = Header {
        ftype: ft,
        flags: 0,
        call_id,
        seq: 0,
        ack: 0,
    };
    let mut b = [0u8; 255];
    let n = encode(&h, payload, &mut b).expect("encode");
    b[..n].to_vec()
}

// ---- the parser ------------------------------------------------------------

#[test]
fn at_mode_parses_every_target_in_any_case() {
    let cases = [
        ("MODEM", ModeTarget::Modem),
        ("RADAR", ModeTarget::Radar),
        ("RNODE", ModeTarget::Rnode),
        ("MESH", ModeTarget::Mesh),
        ("PICKER", ModeTarget::Picker),
    ];
    for (word, target) in cases {
        for form in [
            format!("AT+MODE={word}"),
            format!("at+mode={}", word.to_lowercase()),
            format!("At+Mode = {word} "),
            format!("AT+MODE={word}  "),
            format!("  AT+MODE={word}"),
        ] {
            assert_eq!(parse_line(&form), Ok(AtCmd::Mode(target)), "{form:?}");
        }
    }
    assert_eq!(parse_line("AT+MODE?"), Ok(AtCmd::ModeQuery));
    assert_eq!(parse_line("at+mode?"), Ok(AtCmd::ModeQuery));
    assert_eq!(parse_line("AT+MODE ?"), Ok(AtCmd::ModeQuery));
}

#[test]
fn at_mode_rejects_everything_else() {
    // Known command, bad argument.
    for bad in [
        "AT+MODE=",
        "AT+MODE= ",
        "AT+MODE=0",
        "AT+MODE=1",
        "AT+MODE=LFD3K-SM",
        "AT+MODE=SIMIAN",
        "AT+MODE=MESHTASTIC",
        "AT+MODE=MODEMX",
        "AT+MODE=MO DEM",
        "AT+MODE=RADAR,RNODE",
        "AT+MODE?x",
        "AT+MODE??",
    ] {
        assert_eq!(parse_line(bad), Err(AtError::BadArg), "{bad:?}");
    }
    // No argument form at all is not the command.
    assert_eq!(parse_line("AT+MODE"), Err(AtError::Unknown));
    assert_eq!(parse_line("AT+MODES?"), Err(AtError::Unknown));
    assert_eq!(parse_line("AT+MOD=MESH"), Err(AtError::Unknown));
}

#[test]
fn mode_targets_follow_the_picker_menu_order() {
    // Menu order and C ABI indices: LFD3K-SM, SIMIAN RADAR, RNODE, MESH, then the picker.
    assert_eq!(
        ModeTarget::SLOTS.map(|m| m.index()),
        [0, 1, 2, 3],
        "slot indices"
    );
    assert_eq!(ModeTarget::Picker.index(), 4);
    assert_eq!(
        ModeTarget::SLOTS.map(|m| m.name()),
        ["LFD3K-SM", "SIMIAN RADAR", "RNODE", "MESH"]
    );
    assert_eq!(ModeTarget::Picker.name(), "PICKER");
    assert_eq!(ModeTarget::parse("picker"), Some(ModeTarget::Picker));
    assert_eq!(ModeTarget::parse(""), None);
}

// ---- AT+MODE? ---------------------------------------------------------------

#[test]
fn mode_query_lists_modes_with_marker_and_no_image() {
    let mut rig = Rig::bare();
    rig.m.set_mode_slots(0b0101); // this modem and RNODE only
    assert_eq!(
        rig.line("AT+MODE?"),
        "\r\nMODE LFD3K-SM *\r\n\r\nMODE SIMIAN RADAR NO IMAGE\r\n\r\nMODE RNODE\r\n\r\nMODE MESH NO IMAGE\r\n\r\nOK\r\n"
    );

    // The running modem is always listed as the running one, even when the
    // mask does not carry its bit; bits above the four modes are ignored.
    rig.m.set_mode_slots(0xF0);
    let t = rig.line("AT+MODE?");
    assert!(t.contains("MODE LFD3K-SM *"), "{t:?}");
    assert!(t.contains("MODE SIMIAN RADAR NO IMAGE"), "{t:?}");
    assert!(t.contains("MODE MESH NO IMAGE"), "{t:?}");
    assert!(t.ends_with(OK), "{t:?}");

    rig.m.set_mode_slots(0x0F);
    let t = rig.line("AT+MODE?");
    assert!(!t.contains("NO IMAGE"), "{t:?}");
}

#[test]
fn mode_is_error_without_slots_reported() {
    // The PC build and the single app layout: nothing reports slots.
    let mut rig = Rig::bare();
    assert_eq!(rig.line("AT+MODE?"), ERROR);
    assert_eq!(rig.line("AT+MODE=MESH"), ERROR);
    assert_eq!(rig.line("AT+MODE=PICKER"), ERROR);
    assert!(rig.m.take_mode_request().is_none());
    // The error did not wedge anything.
    assert_eq!(rig.line("AT"), OK);
}

#[test]
fn slots_survive_atz() {
    let mut rig = Rig::new();
    assert_eq!(rig.line("ATZ"), OK);
    assert!(rig.line("AT+MODE?").ends_with(OK));
    rig.line("AT+MODE=RADAR");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Radar));
}

// ---- the request ------------------------------------------------------------

#[test]
fn accepted_request_prints_nothing_until_the_result() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.line("AT+MODE=RADAR"),
        "",
        "no OK before the platform answers"
    );
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Radar));
    assert!(rig.m.take_mode_request().is_none(), "taken once");
    rig.run(50);
    assert_eq!(rig.out, b"", "still nothing while the platform works");

    // OK goes out through the normal result path.
    let t = rig.after(|m| m.mode_result(true));
    assert_eq!(t, OK);
    // A second result without a request is ignored.
    assert_eq!(rig.after(|m| m.mode_result(true)), "");
    assert_eq!(rig.after(|m| m.mode_result(false)), "");
}

#[test]
fn every_target_is_handed_to_the_platform() {
    for target in ModeTarget::SLOTS.into_iter().chain([ModeTarget::Picker]) {
        let mut rig = Rig::new();
        let word = match target {
            ModeTarget::Modem => "MODEM",
            ModeTarget::Radar => "RADAR",
            ModeTarget::Rnode => "RNODE",
            ModeTarget::Mesh => "MESH",
            ModeTarget::Picker => "PICKER",
        };
        assert_eq!(rig.line(&format!("AT+MODE={word}")), "");
        assert_eq!(rig.m.take_mode_request(), Some(target), "{word}");
    }
}

#[test]
fn failed_switch_answers_error_and_modem_goes_on() {
    let mut rig = Rig::new();
    rig.line("AT+MODE=MESH");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Mesh));
    // Ruling P5-3: the platform could not set otadata or save: ERROR, no switch.
    assert_eq!(rig.after(|m| m.mode_result(false)), ERROR);
    // Lines work again and a new request can be made.
    assert_eq!(rig.line("AT"), OK);
    assert_eq!(rig.line("AT+MODE=RNODE"), "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Rnode));
    assert_eq!(rig.after(|m| m.mode_result(true)), OK);
}

#[test]
fn slot_without_image_is_refused_picker_never_is() {
    let mut rig = Rig::bare();
    rig.m.set_mode_slots(0b0001); // only this modem
    assert_eq!(rig.line("AT+MODE=RADAR"), ERROR);
    assert_eq!(rig.line("AT+MODE=RNODE"), ERROR);
    assert_eq!(rig.line("AT+MODE=MESH"), ERROR);
    assert!(rig.m.take_mode_request().is_none());
    // The picker needs no image slot; restarting into the modem itself is fine.
    assert_eq!(rig.line("AT+MODE=PICKER"), "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Picker));
    assert_eq!(rig.after(|m| m.mode_result(true)), OK);
    assert_eq!(rig.line("AT+MODE=MODEM"), "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Modem));
}

#[test]
fn bad_arguments_are_error_and_leave_no_request() {
    let mut rig = Rig::new();
    for bad in ["AT+MODE=", "AT+MODE=NOPE", "AT+MODE=7", "AT+MODE"] {
        assert_eq!(rig.line(bad), ERROR, "{bad}");
    }
    assert!(rig.m.take_mode_request().is_none());
}

// ---- idle only ---------------------------------------------------------------

#[test]
fn refused_while_dialing() {
    let mut rig = Rig::new();
    let t = rig.line("ATDT FARAWAY");
    assert!(t.contains("DIALING FARAWAY") && !t.contains("OK"), "{t:?}");
    assert!(!rig.r.sent.is_empty(), "a CallReq went out");
    assert_eq!(rig.line("AT+MODE=RADAR"), ERROR);
    assert_eq!(rig.line("AT+MODE=PICKER"), ERROR);
    assert!(rig.m.take_mode_request().is_none());
    // Hang up, then it works.
    rig.line("ATH");
    rig.run(5_000);
    assert_eq!(rig.line("AT+MODE=RADAR"), "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Radar));
}

#[test]
fn refused_while_ringing() {
    let mut rig = Rig::new();
    let mut p = [0u8; 64];
    let n = call_req_payload("CALLER", "MODETEST", &mut p).expect("call req");
    let f = frame(FrameType::CallReq, 7, &p[..n]);
    rig.m.radio_rx(&f, info(), &mut rig.r, rig.now);
    rig.drain();
    assert!(
        String::from_utf8_lossy(&rig.out).contains("RING"),
        "{:?}",
        rig.out
    );
    assert_eq!(rig.line("AT+MODE=RADAR"), ERROR);
    assert!(rig.m.take_mode_request().is_none());
}

#[test]
fn refused_while_scanning() {
    let mut rig = Rig::new();
    assert_eq!(rig.line("AT+SCAN"), "");
    assert!(rig.m.scanning());
    assert_eq!(rig.line("AT+MODE=MESH"), ERROR);
    assert_eq!(rig.line("AT+MODE=PICKER"), ERROR);
    assert!(rig.m.take_mode_request().is_none());
    // The scan was not disturbed.
    assert!(rig.m.scanning());
    // After the window the mode switch works.
    rig.run(lfd3k_core::discovery::SCAN_WINDOW_MS + 2_000);
    assert!(!rig.m.scanning());
    rig.out.clear();
    assert_eq!(rig.line("AT+MODE=MESH"), "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Mesh));
}

#[test]
fn second_request_while_one_is_pending_is_refused() {
    let mut rig = Rig::new();
    // Not yet taken by the platform.
    assert_eq!(rig.send(b"AT+MODE=MESH\r").1, "");
    // A new line cannot even be read: dropped like the rest of the chunk.
    assert_eq!(rig.send(b"AT+MODE=RNODE\r").1, "");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Mesh));
    assert!(rig.m.take_mode_request().is_none());
    // Taken, answer outstanding: still dropped.
    assert_eq!(rig.send(b"AT+MODE=RNODE\r").1, "");
    assert!(rig.m.take_mode_request().is_none());
    assert_eq!(rig.after(|m| m.mode_result(true)), OK);
}

// ---- later lines dropped ----------------------------------------------------

#[test]
fn later_lines_in_the_chunk_are_dropped() {
    let mut rig = Rig::new();
    let chunk = b"AT+MODE=MESH\rATZ\rAT&W\rAT+NAME=OTHER\rATDT SOMEONE\rATE1\r";
    let (taken, out) = rig.send(chunk);
    assert_eq!(taken, chunk.len(), "the whole chunk is consumed");
    assert_eq!(out, "", "no echo, no results for the dropped lines");
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Mesh));
    // Nothing behind the request ran.
    assert!(!rig.m.take_save_request(), "AT&W did not run");
    assert!(
        rig.r.sent.is_empty(),
        "ATDT did not run: nothing on the air"
    );
    // And the OK is the only thing the host ever sees for the chunk.
    assert_eq!(rig.after(|m| m.mode_result(true)), OK);
    // After the answer the port works again; ATE1 and AT+NAME never ran.
    assert_eq!(rig.line("AT"), OK);
    let name = rig.line("AT+NAME?");
    assert!(
        name.contains("MODETEST") && !name.contains("OTHER"),
        "{name:?}"
    );
}

#[test]
fn input_while_the_switch_is_pending_is_swallowed_whole() {
    let mut rig = Rig::new();
    rig.line("AT+MODE=RADAR");
    let (taken, out) = rig.send(b"AT&W\rAT\rATH\r");
    assert_eq!(taken, 12);
    assert_eq!(out, "");
    assert!(!rig.m.take_save_request());
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Radar));
    // Still dropped while the platform works on it.
    let (taken, out) = rig.send(b"AT&W\r");
    assert_eq!(taken, 5);
    assert_eq!(out, "");
    assert!(!rig.m.take_save_request());
    assert_eq!(rig.after(|m| m.mode_result(false)), ERROR);
    // After an ERROR the lines run again.
    assert_eq!(rig.line("AT&W"), OK);
    assert!(rig.m.take_save_request());
}

// ---- result codes -----------------------------------------------------------

#[test]
fn numeric_results_with_atv0() {
    let mut rig = Rig::new();
    assert_eq!(rig.line("ATV0"), "0\r");
    rig.line("AT+MODE=RADAR");
    rig.m.take_mode_request();
    assert_eq!(rig.after(|m| m.mode_result(true)), "0\r");
    rig.line("AT+MODE=RADAR");
    rig.m.take_mode_request();
    assert_eq!(rig.after(|m| m.mode_result(false)), "4\r");
    // A refused request prints 4 at once.
    assert_eq!(rig.line("AT+MODE=NOPE"), "4\r");
    rig.m.set_mode_slots(0b0001);
    assert_eq!(rig.line("AT+MODE=MESH"), "4\r");
    // The query ends with 0.
    assert!(rig.line("AT+MODE?").ends_with("0\r"));
    // Back to words: the same path prints OK.
    rig.line("ATV1");
    rig.m.set_mode_slots(0x0F);
    rig.line("AT+MODE=RADAR");
    rig.m.take_mode_request();
    assert_eq!(rig.after(|m| m.mode_result(true)), OK);
}

// ---- the directory goes to flash before the restart -------------------------

#[test]
fn unsaved_directory_is_saved_before_the_restart() {
    let mut rig = Rig::new();
    // Hear a station: the directory changed but its save is rate limited.
    let mut p = [0u8; 32];
    let n = beacon_payload(
        &Beacon {
            name: "NEIGHBOR",
            services: 0,
            heard: 0,
            time_quality: 0,
        },
        &mut p,
    )
    .expect("beacon");
    let f = frame(FrameType::Beacon, 0, &p[..n]);
    rig.m.radio_rx(&f, info(), &mut rig.r, rig.now);
    rig.drain();
    assert_eq!(rig.m.directory().len(), 1);
    assert!(!rig.m.take_dir_save(), "not saved yet");
    assert_eq!(rig.line("AT+MODE=MESH"), "");
    assert!(
        rig.m.take_dir_save(),
        "the pending directory change is written now"
    );
    assert_eq!(rig.m.take_mode_request(), Some(ModeTarget::Mesh));
}

#[test]
fn refused_switch_does_not_force_a_directory_save() {
    let mut rig = Rig::new();
    let mut p = [0u8; 32];
    let n = beacon_payload(
        &Beacon {
            name: "NEIGHBOR",
            services: 0,
            heard: 0,
            time_quality: 0,
        },
        &mut p,
    )
    .expect("beacon");
    let f = frame(FrameType::Beacon, 0, &p[..n]);
    rig.m.radio_rx(&f, info(), &mut rig.r, rig.now);
    rig.drain();
    rig.line("AT+SCAN");
    assert_eq!(rig.line("AT+MODE=MESH"), ERROR);
    assert!(!rig.m.take_dir_save());
}

#[test]
fn atv_shows_the_running_mode_after_the_dir_line() {
    let mut rig = Rig::new();
    let t = rig.line("AT&V");
    let dir = t.find("\r\nDIR 0\r\n").expect("DIR line");
    let mode = t.find("\r\nMODE LFD3K-SM\r\n").expect("MODE line");
    assert!(dir < mode, "MODE follows DIR: {t:?}");
    // The same on a board that never reported slots.
    let mut bare = Rig::bare();
    assert!(bare.line("AT&V").contains("\r\nMODE LFD3K-SM\r\n"));
}
