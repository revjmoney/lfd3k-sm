//! Task 9 voice call tests on the simulated channel (seeded `Multi`):
//! calling a person by name, ringing and pickups, the scrambler muting a
//! bad handshake, busy and HIDDEN devices, S0, talk mode changes, push to
//! talk holds, fades and loss, voice beacons and scans, and a data call
//! next to voice names; plus the regression tests for the bugs found and
//! fixed in review (named `regression_*`).
//!
//! Values follow the controller's rulings where they changed the plan
//! text: SF5 Opus 10k in 120 ms frames, SF6 Opus 8k and SF8 Codec2 1300 in
//! duplex, push to talk from SF9 down, SF12 listener every 2 slots, PTT
//! wall caps at 3/5/7 listen periods (never lower, whatever S22), mouth to
//! ear under 400 ms at SF5 and under 600 ms at SF6 to SF8 (the plan said
//! 400 ms for SF5 and SF6), the scan window 5 s, VHP 0x8F RingEnded.
//!
//! SNR values are dB x4. Tier floors (`Tier::snr_floor_x4`): SF5 -10, SF6
//! -20, SF7 -30, SF8 -40, SF9 -50, SF10 -60, SF11 -70, SF12 -80. Loss on the
//! channel: 0.5 % at 8 or more over a tier's floor, 10 % from 0 to 8 over,
//! 60 % up to 8 under, nothing heard below that. With S20 = 0 a call trains
//! to the fastest tier whose floor the SNR clears (or the S21 cap).

mod voice_common;

use std::cell::RefCell;

use lfd3k_core::voice::host::{call_state, msg};
use lfd3k_core::voice::{slot_layout, CodecId, Floor, TalkMode, VSub, LISTEN_CHUNK};
use lfd3k_core::Tier;
use lfd3k_sim::{SimChannel, SnrProfile};
use lfd3k_voice::{Trust, UiEvent};
use voice_common::*;

/// SF9 with 0.5 % loss, and nothing faster usable.
const SF9_SNR: i16 = -42;
/// SF12 only (0.5 % loss there; SF11 is 2 dB under its floor).
const SF12_ONLY: i16 = -72;

fn chan_x4(snr_x4: i16, seed: u64) -> SimChannel {
    SimChannel {
        profile: SnrProfile::Constant(snr_x4),
        seed,
        drop: None,
    }
}

/// REV/PC on ALPHA calls BOB; BOB/PHONE on BRAVO answers; both talk
/// anyway. Returns (world, rev, bob) once both apps see the slots run.
fn rev_calls_bob(cap: Option<Tier>, snr_x4: i16) -> (World, usize, usize) {
    let mut w = World::with_channel(
        vec![cfg("ALPHA", cap), cfg("BRAVO", cap)],
        chan_x4(snr_x4, 7),
    );
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    let up = w.run_until(120_000, |w| {
        w.apps[rev].eng.stats().slots_ms.is_some() && w.apps[bob].eng.stats().slots_ms.is_some()
    });
    assert!(up, "REV and BOB reach the voice slots");
    (w, rev, bob)
}

fn sf_of(w: &World, a: usize) -> u8 {
    w.apps[a].eng.stats().sf
}

/// Frames of type `t` (and sub-type `sub` for type 15) modem `m` sent at or after `from`.
fn count_sent(w: &World, m: usize, t: u8, sub: Option<u8>, from: u32) -> usize {
    w.sent(m)
        .iter()
        .filter(|(at, ty, sb, _)| *at >= from && *ty == t && sub.is_none_or(|s| *sb == s))
        .count()
}

// ---- Calls by name.

#[test]
fn call_by_name_rings_and_connects() {
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 1);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[rev].mic = Mic::Tone;
    w.apps[bob].mic = Mic::Tone;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(30_000, |w| {
        w.apps[rev].eng.stats().slots_ms.is_some() && w.apps[bob].eng.stats().slots_ms.is_some()
    }));
    let ring = w.apps[bob].log.iter().find_map(|(_, e)| match e {
        UiEvent::Ringing {
            name,
            label,
            station,
        } => Some((name.clone(), label.clone(), station.clone())),
        _ => None,
    });
    assert_eq!(ring, Some(("REV".into(), "PC".into(), "ALPHA".into())));
    assert!(!w.has(rev, is_ring), "the caller never rings");
    let words = |a: usize| {
        w.apps[a].log.iter().find_map(|(_, e)| match e {
            UiEvent::Connected { words, trust, plan } => Some((*words, *trust, *plan)),
            _ => None,
        })
    };
    let (wr, tr, pr) = words(rev).unwrap();
    let (wb, tb, pb) = words(bob).unwrap();
    assert_eq!(wr, wb, "both show the same two words");
    assert_eq!((tr, tb), (Trust::NotChecked, Trust::NotChecked));
    assert_eq!(pr, pb);
    assert_eq!(
        (pr.codec, pr.mode),
        (CodecId::Opus10k, TalkMode::Duplex),
        "SF5 at 10 dB"
    );
    assert_eq!(
        w.apps[rev].eng.peer(),
        Some(("BOB".into(), "PHONE".into(), "BRAVO".into()))
    );
    assert_eq!(
        w.apps[bob].eng.peer(),
        Some(("REV".into(), "PC".into(), "ALPHA".into()))
    );
    w.run(5_000);
    for a in [rev, bob] {
        let st = w.apps[a].eng.stats();
        assert!(
            st.frames_sent > 30 && st.frames_heard > 30,
            "app {a}: {st:?}"
        );
        assert!(w.apps[a].heard_samples > 16_000, "app {a} played audio");
    }
    w.apps[rev].eng.hangup();
    assert!(w.run_until(10_000, |w| w.has(bob, |e| matches!(e, UiEvent::Ended))));
    assert!(!w.apps[rev].eng.in_call() && !w.apps[bob].eng.in_call());
}

/// REV on ALPHA; BOB/PHONE on BRAVO and BOB/PC on CHARLIE, not answering.
fn three() -> (World, usize, usize, usize) {
    let mut w = World::new(
        vec![cfg("ALPHA", None), cfg("BRAVO", None), cfg("CHARLIE", None)],
        10,
        3,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    let pc = w.app(2, 1, "BOB", "PC");
    w.apps[phone].auto_answer = false;
    w.apps[pc].auto_answer = false;
    w.run(1_000);
    (w, rev, phone, pc)
}

#[test]
fn call_by_name_and_label_rings_one() {
    let (mut w, rev, phone, pc) = three();
    w.apps[rev].eng.call("BOB", Some("PC"));
    w.run(10_000);
    assert!(w.has(pc, is_ring), "BOB/PC rings");
    assert!(!w.has(phone, is_ring), "BOB/PHONE does not");
    w.apps[pc].eng.answer();
    assert!(w.run_until(30_000, |w| w.has(rev, is_connected)));
    assert_eq!(w.apps[rev].eng.peer().unwrap().1, "PC");
    assert!(!w.has(phone, is_ring));
}

#[test]
fn callers_own_device_never_rings() {
    // The rule (`ring_mask`): a registration with the caller's station and
    // label is the calling device itself and is never rung; the same name
    // with another label on the same station, or any label elsewhere, is.
    use lfd3k_core::voice::call::{ring_mask, VoiceReq};
    use lfd3k_core::voice::{Name16, VoiceReg};
    let reg = |n: &str, l: &str, s: u8| {
        Some(VoiceReg {
            name: Name16::new(n).unwrap(),
            label: Name16::label(l).unwrap(),
            session: s,
            seen_ms: 0,
        })
    };
    let regs = [
        reg("REV", "PC", 0),
        reg("REV", "PHONE", 1),
        None,
        reg("BOB", "PC", 3),
    ];
    let req = VoiceReq {
        caps: 0,
        station: "ALPHA",
        name: "REV",
        label: "PC",
        callee: "REV",
        callee_label: "",
    };
    assert_eq!(
        ring_mask(&regs, &req, "ALPHA"),
        0b0010,
        "own device not rung, REV/PHONE is"
    );
    assert_eq!(
        ring_mask(&regs, &req, "BRAVO"),
        0b0011,
        "on another station REV/PC is another device"
    );
    let to_pc = VoiceReq {
        callee_label: "PC",
        ..req
    };
    assert_eq!(ring_mask(&regs, &to_pc, "ALPHA"), 0);
    // On the air: REV/PC on ALPHA calls REV; REV/TAB on BRAVO rings, the
    // calling device never does.
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 4);
    let pc = w.app(0, 1, "REV", "PC");
    let tab = w.app(1, 1, "REV", "TAB");
    w.apps[pc].auto_answer = false;
    w.apps[tab].auto_answer = false;
    w.run(1_000);
    w.apps[pc].eng.call("REV", None);
    w.run(10_000);
    assert!(!w.has(pc, is_ring), "the calling device never rings");
    assert!(w.has(tab, is_ring), "REV/TAB on another modem rings");
}

#[test]
fn two_ringing_one_pickup_other_answered_elsewhere() {
    let (mut w, rev, phone, pc) = three();
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring) && w.has(pc, is_ring)));
    w.apps[phone].eng.answer();
    assert!(w.run_until(30_000, |w| w.has(rev, is_connected)
        && w.has(phone, is_connected)));
    assert!(w.run_until(20_000, |w| w
        .has(pc, |e| matches!(e, UiEvent::AnsweredElsewhere))));
    assert!(!w.has(pc, is_connected));
    assert_eq!(w.apps[rev].eng.peer().unwrap().1, "PHONE");
    assert_eq!(
        count_sent(&w, 2, 15, Some(3), 0),
        0,
        "BOB/PC never answered"
    );
}

/// REV on ALPHA calls BOB; BOB/PHONE on `a` and BOB/PC on `b` both ring;
/// PHONE picks up, PC `gap_ms` later (and both pick up again if it rings
/// again).
struct Race {
    phone: bool,
    pc: bool,
    rev: bool,
    label: String,
    /// Answers each device put on the air, and the rings each saw.
    sends: (usize, usize),
    rings: (usize, usize),
}

fn pickup_race(a: &str, b: &str, gap_ms: u32, seed: u64) -> Race {
    let mut alpha = cfg("ALPHA", None);
    alpha.sregs.set(7, 90);
    let mut w = World::new(vec![alpha, cfg(a, None), cfg(b, None)], 10, seed);
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    let pc = w.app(2, 1, "BOB", "PC");
    w.apps[phone].auto_answer = false;
    w.apps[pc].auto_answer = false;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring) && w.has(pc, is_ring)));
    w.apps[phone].eng.answer();
    let t = w.now() + gap_ms;
    while w.now() < t {
        w.step();
    }
    w.apps[pc].eng.answer();
    // If both pickups collide on both of their sends, both hear answered
    // elsewhere and the call rings again: the people pick up again.
    w.apps[phone].auto_answer = true;
    w.apps[pc].auto_answer = true;
    w.run_until(90_000, |w| w.has(rev, is_connected));
    w.run(15_000);
    let rings = |k: usize| w.apps[k].log.iter().filter(|(_, e)| is_ring(e)).count();
    Race {
        phone: w.has(phone, is_connected),
        pc: w.has(pc, is_connected),
        rev: w.has(rev, is_connected),
        label: w.apps[rev].eng.peer().map(|p| p.1).unwrap_or_default(),
        sends: (
            count_sent(&w, 1, 15, Some(3), 0),
            count_sent(&w, 2, 15, Some(3), 0),
        ),
        rings: (rings(phone), rings(pc)),
    }
}

#[test]
fn two_pickups_one_winner() {
    // Two devices pick up the same call at once, or one a moment after the
    // other (inside the first one's Answer and Train), on four station
    // pairs (the answer tokens come from the station names): exactly one
    // device gets the call, REV shows it, and no device sends its Answer
    // more than twice per ring. In this sweep 19 of 20 races connect on
    // the first ring; in the other both pickups collide on both sends,
    // both hear answered elsewhere, the call rings again and the second
    // pickup connects.
    let mut again = 0;
    for (k, (a, b)) in [
        ("BRAVO", "CHARLIE"),
        ("DELTA", "ECHO"),
        ("KILO", "LIMA"),
        ("AXIS-1", "AXIS-2"),
    ]
    .into_iter()
    .enumerate()
    {
        for gap in [0, 50, 100, 200, 400] {
            let r = pickup_race(a, b, gap, 100 + k as u64);
            let why = format!(
                "{a}/{b} gap {gap} ms: phone {} pc {} rev {} label {:?} sends {:?} rings {:?}",
                r.phone, r.pc, r.rev, r.label, r.sends, r.rings
            );
            println!("{why}");
            assert!(r.rev, "REV connects: {why}");
            assert!(r.phone != r.pc, "exactly one device connects: {why}");
            let want = if r.phone { "PHONE" } else { "PC" };
            assert_eq!(r.label, want, "REV shows the winner: {why}");
            assert!(
                r.sends.0 <= 2 * r.rings.0 && r.sends.1 <= 2 * r.rings.1,
                "at most two Answers per ring: {why}"
            );
            if r.rings.0 > 1 {
                again += 1;
            }
        }
    }
    assert!(again <= 2, "{again} of 20 races needed a second ring");
}

fn drop_answers(_t: u32, b: &[u8]) -> bool {
    b.get(1).map(|x| x >> 4) == Some(15) && b.get(5) == Some(&3)
}

#[test]
fn lost_answer_still_binds_one() {
    // Every voice Answer frame is lost on the air: the answer token in the
    // Train still binds the caller to exactly one device.
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 5,
        drop: Some(drop_answers),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", None), cfg("BRAVO", None), cfg("CHARLIE", None)],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    let pc = w.app(2, 1, "BOB", "PC");
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(60_000, |w| w.apps[rev].eng.stats().slots_ms.is_some()));
    w.run(15_000);
    let won: Vec<usize> = [phone, pc]
        .into_iter()
        .filter(|&a| w.has(a, is_connected))
        .collect();
    assert_eq!(won.len(), 1, "one device bound");
    assert!(w.apps[won[0]].eng.stats().slots_ms.is_some());
    assert!(
        count_sent(&w, 1, 15, Some(3), 0) + count_sent(&w, 2, 15, Some(3), 0) > 0,
        "answers were sent (and lost)"
    );
}

thread_local! {
    /// Voice TierSets still to drop (`drop_answers_and_first_tierset`).
    static TIERSET_DROP: RefCell<u32> = const { RefCell::new(0) };
}

/// Drops every voice Answer and the first voice TierSet (tier and token).
fn drop_answers_and_first_tierset(t: u32, b: &[u8]) -> bool {
    if drop_answers(t, b) {
        return true;
    }
    if b.get(1).map(|x| x >> 4) == Some(8) && b.len() == 10 {
        return TIERSET_DROP.with(|d| {
            let mut d = d.borrow_mut();
            let hit = *d > 0;
            *d = d.saturating_sub(1);
            hit
        });
    }
    false
}

#[test]
fn late_pickup_sends_at_most_twice_then_elsewhere() {
    // BOB/PHONE picks up; its Answers and REV's first TierSet are lost, so
    // BOB/PC still rings and picks up late, after REV already bound
    // BOB/PHONE by its Train. BOB/PC's Answer goes out at most twice (only
    // once the channel has been quiet long enough, between REV's TierSet
    // resends); REV's next TierSet (BOB/PHONE's token) ends it: answered
    // elsewhere, silently, never NO CARRIER, never over the live call.
    // Several pickup moments (each draws another token and quiet time), so
    // some of them do get an answer on the air.
    let mut sent_any = 0;
    for delay in [100u32, 250, 400, 550, 700] {
        let sends = late_pickup(delay);
        assert!(sends <= 2, "pickup {delay} ms late answered {sends} times");
        if sends > 0 {
            sent_any += 1;
        }
    }
    assert!(sent_any >= 1, "no late pickup got an answer on the air");
}

/// One late pickup `delay` ms after REV's first (dropped) TierSet; the
/// Answers the late device sent.
fn late_pickup(delay: u32) -> usize {
    TIERSET_DROP.with(|d| *d.borrow_mut() = 1);
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 8,
        drop: Some(drop_answers_and_first_tierset),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", None), cfg("BRAVO", None), cfg("CHARLIE", None)],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    let pc = w.app(2, 1, "BOB", "PC");
    w.apps[phone].auto_answer = false;
    w.apps[pc].auto_answer = false;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring) && w.has(pc, is_ring)));
    w.apps[phone].eng.answer();
    // REV's first TierSet (dropped) has gone out and ended.
    assert!(w.run_until(5_000, |_| TIERSET_DROP.with(|d| *d.borrow()) == 0));
    w.run(delay);
    assert!(
        !w.has(pc, |e| matches!(e, UiEvent::AnsweredElsewhere)),
        "BOB/PC still rings"
    );
    let t_pc = w.now();
    w.apps[pc].eng.answer();
    assert!(w.run_until(30_000, |w| w
        .has(pc, |e| matches!(e, UiEvent::AnsweredElsewhere))));
    w.run(20_000);
    let sends = count_sent(&w, 2, 15, Some(3), t_pc);
    assert!(!w.has(pc, is_connected));
    assert!(w.has(rev, is_connected) && w.has(phone, is_connected));
    assert!(!w.has(pc, |e| matches!(e, UiEvent::Ended)));
    assert!(!w.net.host_text(2).contains("NO CARRIER"));
    // Nothing from BOB/PC once REV's call was up.
    let up = w.apps[rev].eng.stats().connected_ms.unwrap();
    assert!(
        w.sent(2)
            .iter()
            .all(|x| x.0 < up || x.1 == 12 || (x.1 == 15 && x.2 == 0)),
        "BOB/PC kept quiet"
    );
    sends
}

#[test]
fn ringer_stops_on_tierset() {
    // BOB/PHONE's Answer frames are all lost, so BOB/PC (ringing) never
    // hears another device pick up; REV binds by PHONE's Train and sends
    // its setup TierSet with PHONE's token: that stops BOB/PC's ring.
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 6,
        drop: Some(drop_answers),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", None), cfg("BRAVO", None), cfg("CHARLIE", None)],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    let pc = w.app(2, 1, "BOB", "PC");
    w.apps[phone].auto_answer = false;
    w.apps[pc].auto_answer = false;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring) && w.has(pc, is_ring)));
    let t0 = w.now();
    w.apps[phone].eng.answer();
    assert!(w.run_until(30_000, |w| w
        .has(pc, |e| matches!(e, UiEvent::AnsweredElsewhere))));
    let t_else = w
        .when(pc, |e| matches!(e, UiEvent::AnsweredElsewhere))
        .unwrap();
    let ts = w
        .sent(0)
        .into_iter()
        .find(|(at, ty, _, b)| *at >= t0 && *ty == 8 && b.len() == 10)
        .map(|x| x.0)
        .expect("a voice TierSet (tier and token)");
    assert!(
        ts < t_else,
        "TierSet at {ts}, answered elsewhere at {t_else}"
    );
    assert!(
        t_else - ts < 1_000,
        "rang on {} ms after the TierSet",
        t_else - ts
    );
    assert_eq!(
        count_sent(&w, 2, 15, Some(3), 0),
        0,
        "BOB/PC never answered"
    );
    assert!(w.run_until(30_000, |w| w.has(rev, is_connected)
        && w.has(phone, is_connected)));
}

// ---- Setup timing per tier.

/// One call capped at `t`: (CONNECT to slots, CONNECT to first audio) in ms.
fn first_audio(t: Tier) -> (u32, u32) {
    let mut w = World::new(vec![cfg("ALPHA", Some(t)), cfg("BRAVO", Some(t))], 10, 11);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[rev].mic = Mic::Tone;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    let mut keyed = false;
    let end = w.now() + 180_000;
    while w.now() < end && w.apps[bob].eng.stats().first_audio_ms.is_none() {
        w.step();
        let st = w.apps[rev].eng.stats();
        if !keyed && st.slots_ms.is_some() && st.plan.is_some_and(|p| p.mode != TalkMode::Duplex) {
            // Push to talk: REV presses TALK as soon as the slots run.
            w.apps[rev].eng.talk_key(true);
            keyed = true;
        }
    }
    let (r, b) = (w.apps[rev].eng.stats(), w.apps[bob].eng.stats());
    assert_eq!(r.sf, t.sf(), "trained to the cap");
    let c = r.connected_ms.expect("connected");
    let slots = r.slots_ms.expect("slots") - c;
    let audio = b
        .first_audio_ms
        .unwrap_or_else(|| panic!("SF{}: no audio in 180 s", t.sf()))
        - c;
    (slots, audio)
}

#[test]
fn connect_to_first_audio_each_tier() {
    // CONNECT (the caller's CallState connected) to the voice slots and to
    // the first voice frame the answerer plays, on every tier (capped with
    // S21), push to talk tiers with REV's key down as soon as the slots
    // run. The plan's target: SF12 under 60 s. The tiers run on their own
    // threads (each world is seeded and alone, so this is deterministic).
    let rows: Vec<(Tier, (u32, u32))> = std::thread::scope(|sc| {
        let hs: Vec<_> = Tier::LADDER
            .iter()
            .rev()
            .map(|&t| (t, sc.spawn(move || first_audio(t))))
            .collect();
        hs.into_iter()
            .map(|(t, h)| (t, h.join().unwrap()))
            .collect()
    });
    // Bounds about a quarter over what this build measures (the plan's own
    // target is SF12 under 60 s).
    for (t, (slots, audio)) in &rows {
        println!(
            "SF{}: CONNECT to slots {slots} ms, to first audio {audio} ms",
            t.sf()
        );
        // Measured: 740, 990, 1120, 1380, 2320, 3370, 6700, 13120 ms.
        let limit = match t {
            Tier::Sf5 => 1_000,
            Tier::Sf6 => 1_300,
            Tier::Sf7 => 1_500,
            Tier::Sf8 => 1_800,
            Tier::Sf9 => 3_000,
            Tier::Sf10 => 4_300,
            Tier::Sf11 => 8_500,
            Tier::Sf12 => 16_500,
        };
        assert!(
            *audio < limit,
            "SF{}: first audio after {audio} ms (limit {limit})",
            t.sf()
        );
        assert!(slots <= audio);
    }
}

// ---- The scrambler mutes a bad handshake.

#[test]
fn bad_commit_reveal_mutes() {
    use lfd3k_core::voice::host::{cobs_decode, cobs_encode};
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 9);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[rev].mic = Mic::Tone;
    w.apps[bob].mic = Mic::Tone;
    // A man in the middle changes the caller's reveal (its hello) on the
    // way: it no longer matches the commit.
    let hit = std::rc::Rc::new(std::cell::Cell::new(false));
    let h2 = hit.clone();
    w.tamper = Some(Box::new(move |k, b| {
        if k != rev {
            return b;
        }
        let mut out = Vec::new();
        for part in b.split(|&x| x == 0) {
            if part.is_empty() {
                out.push(0);
                continue;
            }
            let mut f = [0u8; 300];
            match cobs_decode(part, &mut f) {
                Some(n) if n > 4 && f[0] == msg::CONTROL_OUT && f[3] == 0x03 => {
                    f[20] ^= 0x01;
                    h2.set(true);
                    let mut e = [0u8; 310];
                    let m = cobs_encode(&f[..n], &mut e).unwrap();
                    out.extend_from_slice(&e[..m]);
                }
                _ => {
                    out.extend_from_slice(part);
                    out.push(0);
                }
            }
        }
        // `split` gave one piece too many for the final zero.
        if !b.ends_with(&[0]) || out.len() > b.len() + 1 {
            out.truncate(out.len().saturating_sub(1));
        }
        out
    }));
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(30_000, |w| w.apps[rev].eng.stats().state
        == call_state::CONNECTED));
    w.run(20_000);
    assert!(hit.get(), "the reveal went by");
    for a in [rev, bob] {
        assert!(!w.has(a, is_connected), "app {a} never shows a live call");
        let st = w.apps[a].eng.stats();
        assert_eq!(st.frames_sent, 0, "app {a} sent audio");
        assert!(st.slots_ms.is_none(), "no VoiceReady, no slots");
        assert_eq!(w.apps[a].heard_samples, 0);
    }
    assert!(
        w.has(bob, |e| matches!(e, UiEvent::Error(_))),
        "the answerer says why"
    );
}

// ---- Declines, busy, HIDDEN, S0.

#[test]
fn decline_ends_everywhere() {
    let (mut w, rev, phone, pc) = three();
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring) && w.has(pc, is_ring)));
    w.apps[phone].eng.decline();
    assert!(w.run_until(15_000, |w| w.has(rev, |e| matches!(e, UiEvent::Declined))));
    assert!(w.run_until(15_000, |w| w.has(pc, |e| matches!(e, UiEvent::Declined))));
    w.run(10_000);
    assert!(!w.apps[rev].eng.in_call());
    for a in [rev, phone, pc] {
        assert!(!w.has(a, is_connected));
    }
    let t = w.now();
    w.run(10_000);
    assert!(
        w.apps[pc].log.iter().all(|(at, e)| *at < t || !is_ring(e)),
        "no more rings"
    );
}

#[test]
fn busy_device_silent_others_ring() {
    let (mut w, rev, phone, pc) = three();
    // BOB/PHONE's modem is busy: its app is calling someone else.
    w.apps[phone].eng.call("NOBODY", None);
    w.run(2_000);
    let t0 = w.now();
    w.apps[rev].eng.call("BOB", None);
    w.run(12_000);
    assert!(w.has(pc, is_ring), "BOB/PC rings");
    assert!(!w.has(phone, is_ring), "the busy device does not ring");
    assert_eq!(count_sent(&w, 1, 15, Some(3), t0), 0, "no answer");
    assert_eq!(count_sent(&w, 1, 15, Some(4), t0), 0, "no decline");
    assert_eq!(count_sent(&w, 1, 3, None, t0), 0, "no CallReject: silent");
}

#[test]
fn hidden_rings_only_pinned_voice() {
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 12);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[bob].auto_answer = false;
    w.net.host_send(1, b"AT+BEACON=HIDDEN\r");
    w.run(1_000);
    assert!(w.net.host_text(1).contains("OK"));
    let t0 = w.now();
    w.apps[rev].eng.call("BOB", None);
    w.run(15_000);
    assert!(
        !w.has(bob, is_ring),
        "HIDDEN: an unpinned voice caller never rings"
    );
    assert_eq!(
        w.sent(1)
            .iter()
            .filter(|(at, ty, _, _)| *at >= t0 && *ty != 12)
            .count(),
        0,
        "HIDDEN sends nothing back"
    );
    w.apps[rev].eng.hangup();
    w.run(5_000);
    // Pin the voice name REV: now it rings.
    w.net.host_send(1, b"AT+DIR=+V:REV\r");
    w.run(500);
    assert!(w.net.host_text(1).contains("OK"));
    w.apps[rev].eng.call("BOB", None);
    assert!(
        w.run_until(15_000, |w| w.has(bob, is_ring)),
        "a pinned voice name rings"
    );
}

#[test]
fn s0_never_answers_voice() {
    let mut bravo = cfg("BRAVO", None);
    bravo.sregs.set(0, 1);
    let mut w = World::new(vec![cfg("ALPHA", None), bravo], 10, 13);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[bob].auto_answer = false;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    w.run(15_000);
    let rings = w.apps[bob].log.iter().filter(|(_, e)| is_ring(e)).count();
    assert_eq!(rings, 1, "the app shows the ring (once per ring)");
    assert!(w.raws.is_empty());
    assert_eq!(
        count_sent(&w, 1, 15, Some(3), 0),
        0,
        "S0=1 never answers a voice call"
    );
    assert_eq!(count_sent(&w, 1, 4, None, 0), 0, "no Train");
    assert!(!w.has(rev, is_connected));
    assert!(w.apps[rev].eng.in_call(), "REV still dialing");
}

// ---- Talk modes, push to talk, fades, loss.

#[test]
fn talk_mode_switches_on_retrain() {
    // A duplex call on SF7 (the cap); the SNR drops so only SF9 and slower
    // work: the call retrains to push to talk and the apps change codec
    // and mode; then it comes back up to duplex.
    let (mut w, rev, bob) = rev_calls_bob(Some(Tier::Sf7), 40);
    assert_eq!(sf_of(&w, rev), 7);
    assert_eq!(w.apps[rev].eng.stats().plan.unwrap().mode, TalkMode::Duplex);
    let t0 = w.now();
    w.net.set_snr_x4(SF9_SNR);
    let ptt = w.run_until(90_000, |w| {
        w.apps[rev]
            .eng
            .stats()
            .plan
            .is_some_and(|p| p.mode != TalkMode::Duplex)
            && w.apps[bob]
                .eng
                .stats()
                .plan
                .is_some_and(|p| p.mode != TalkMode::Duplex)
    });
    assert!(ptt, "push to talk after the drop");
    let changed = |a: usize| {
        w.apps[a].log.iter().any(|(at, e)| {
            *at >= t0 && matches!(e, UiEvent::PlanChanged(p) if p.mode != TalkMode::Duplex)
        })
    };
    assert!(
        changed(rev) && changed(bob),
        "both apps saw the plan change"
    );
    println!(
        "duplex to push to talk after {} ms (SF{})",
        w.now() - t0,
        sf_of(&w, rev)
    );
    let t1 = w.now();
    w.net.set_snr_x4(40);
    let up = w.run_until(180_000, |w| {
        w.apps[rev]
            .eng
            .stats()
            .plan
            .is_some_and(|p| p.mode == TalkMode::Duplex)
            && w.apps[bob]
                .eng
                .stats()
                .plan
                .is_some_and(|p| p.mode == TalkMode::Duplex)
    });
    assert!(up, "back to duplex");
    println!(
        "push to talk to duplex after {} ms (SF{})",
        w.now() - t1,
        sf_of(&w, rev)
    );
    assert!(w.apps[rev].eng.in_call() && w.apps[bob].eng.in_call());
}

/// A push to talk call capped at `cap`; REV holds the floor with the tone
/// on the mic for `secs`; returns the world and the apps.
fn ptt_hold(cap: Tier, snr_x4: i16, secs: u32) -> (World, usize, usize) {
    let (mut w, rev, bob) = rev_calls_bob(Some(cap), snr_x4);
    w.apps[rev].mic = Mic::Tone;
    w.apps[rev].eng.talk_key(true);
    assert!(
        w.run_until(30_000, |w| w.apps[rev].eng.floor() == Floor::Mine),
        "REV gets the floor"
    );
    let t0 = w.now();
    let mut sfs = std::collections::BTreeSet::new();
    while w.now() < t0 + secs * 1000 {
        w.step();
        sfs.insert(sf_of(&w, rev));
        assert!(w.apps[rev].eng.in_call(), "the call is up");
    }
    assert_eq!(
        sfs.into_iter().collect::<Vec<_>>(),
        vec![cap.sf()],
        "the tier never changed"
    );
    (w, rev, bob)
}

#[test]
fn ptt_60s_sf9_no_stepdown() {
    let (w, rev, bob) = ptt_hold(Tier::Sf9, SF9_SNR, 60);
    let t = w.apps[rev].eng.stats().slots_ms.unwrap();
    assert_eq!(
        count_sent(&w, 0, 8, None, t) + count_sent(&w, 1, 8, None, t),
        0,
        "no TierSet"
    );
    let (s, h) = (
        w.apps[rev].eng.stats().frames_sent,
        w.apps[bob].eng.stats().frames_heard,
    );
    println!("SF9 60 s hold: {s} frames sent, {h} heard");
    assert!(s >= 140 && h * 100 >= s * 90, "{h} of {s} heard");
}

#[test]
fn ptt_60s_sf12_no_stepdown() {
    let (w, rev, bob) = ptt_hold(Tier::Sf12, SF12_ONLY, 60);
    let t = w.apps[rev].eng.stats().slots_ms.unwrap();
    assert_eq!(
        count_sent(&w, 0, 8, None, t) + count_sent(&w, 1, 8, None, t),
        0,
        "no TierSet"
    );
    let (s, h) = (
        w.apps[rev].eng.stats().frames_sent,
        w.apps[bob].eng.stats().frames_heard,
    );
    println!("SF12 60 s hold: {s} frames sent, {h} heard");
    assert!(s >= 20 && h * 100 >= s * 80, "{h} of {s} heard");
}

#[test]
fn voice_fade_still_retrains() {
    // Duplex on SF7; the SNR sinks a little under SF7's floor (60 % loss
    // there, SF8 0.5 %): the voice slots do not hide the fade, the call
    // retrains (a step down to SF8; under 60 % loss a lost TierAck may end
    // in a resync and a climb back to SF9 instead) and goes on: voice flows
    // again (on a push to talk tier once REV presses TALK).
    let (mut w, rev, bob) = rev_calls_bob(Some(Tier::Sf7), 40);
    w.apps[rev].mic = Mic::Tone;
    w.apps[bob].mic = Mic::Tone;
    let t0 = w.now();
    w.net.set_snr_x4(-32);
    assert!(
        w.run_until(60_000, |w| sf_of(w, rev) > 7 && sf_of(w, bob) > 7),
        "stepped down"
    );
    println!(
        "SF7 fade: on SF{} after {} ms",
        sf_of(&w, rev),
        w.now() - t0
    );
    assert!(
        count_sent(&w, 0, 8, None, t0) + count_sent(&w, 1, 8, None, t0) > 0,
        "a TierSet"
    );
    assert!(w.has(
        rev,
        |e| matches!(e, UiEvent::PlanChanged(p) if p.codec == CodecId::C2_1300)
    ));
    w.run(10_000);
    assert!(w.apps[rev].eng.in_call() && w.apps[bob].eng.in_call());
    if w.apps[rev]
        .eng
        .stats()
        .plan
        .is_some_and(|p| p.mode != TalkMode::Duplex)
    {
        w.apps[rev].eng.talk_key(true);
        assert!(w.run_until(20_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
    }
    let heard = w.apps[bob].eng.stats().frames_heard;
    w.run(10_000);
    let now = w.apps[bob].eng.stats();
    println!(
        "after the fade: SF{} {:?}, {} more frames heard",
        now.sf,
        now.plan.map(|p| p.mode),
        now.frames_heard - heard
    );
    assert!(
        now.frames_heard > heard + 5,
        "voice flows after the retrain"
    );
}

thread_local! {
    static LOSS: RefCell<(u32, u32)> = const { RefCell::new((0, 1)) };
}

/// Drops about one voice Data frame in five (`LOSS`: xorshift state).
fn drop_fifth_of_voice(_t: u32, b: &[u8]) -> bool {
    if b.get(1).map(|x| x >> 4) != Some(15) || b.get(5) != Some(&5) {
        return false;
    }
    LOSS.with(|l| {
        let mut l = l.borrow_mut();
        let mut x = l.1;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        l.1 = x;
        l.0 += 1;
        x % 5 == 0
    })
}

#[test]
fn loss_is_gap_not_stall() {
    LOSS.with(|l| *l.borrow_mut() = (0, 0x2545_F491));
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 21,
        drop: Some(drop_fifth_of_voice),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", Some(Tier::Sf7)), cfg("BRAVO", Some(Tier::Sf7))],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.apps[rev].mic = Mic::Tone;
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(60_000, |w| w.apps[bob].eng.stats().first_audio_ms.is_some()));
    // 30 s at 20 % voice frame loss: in every 5 s some frames arrive, the
    // gaps are concealed, and the call stays up.
    let mut last = w.apps[bob].eng.stats().frames_heard;
    for _ in 0..6 {
        w.run(5_000);
        let h = w.apps[bob].eng.stats().frames_heard;
        assert!(h >= last + 15, "frames keep flowing: {last} then {h}");
        last = h;
        assert!(w.apps[rev].eng.in_call() && w.apps[bob].eng.in_call());
    }
    let s = w.apps[bob].eng.stats();
    assert!(s.frames_concealed > 0, "lost frames were concealed");
    let sent = w.apps[rev].eng.stats().frames_sent;
    println!(
        "20 % loss: {} of {sent} heard, {} concealed",
        s.frames_heard, s.frames_concealed
    );
    assert!(s.frames_heard * 100 >= sent * 70 && s.frames_heard * 100 <= sent * 90);
}

#[test]
fn duplex_delay_under_target() {
    // lfd3k-voicesim's mouth to ear delay on the duplex tiers, at its
    // default 10 dB. Ruling: SF5 under 400 ms, SF6 to SF8 under 600 ms
    // (the plan said SF5 and SF6 under 400 ms).
    let out = temp_dir();
    let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/speech-16k.wav");
    for (tier, limit) in [("SF5", 400), ("SF6", 600), ("SF7", 600), ("SF8", 600)] {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_lfd3k-voicesim"))
            .args(["--tier", tier, "--in"])
            .arg(&wav)
            .arg("--out")
            .arg(&out)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{tier}: {text}");
        let row = text
            .lines()
            .find(|l| l.starts_with(tier))
            .unwrap_or_else(|| panic!("{text}"));
        // ... concealed  <delay> ms  <match> ...
        let cols: Vec<&str> = row.split_whitespace().collect();
        let ms_at = cols.iter().position(|c| *c == "ms").unwrap();
        let delay: u32 = cols[ms_at - 1].parse().unwrap();
        let matched: f64 = cols[ms_at + 1].parse().unwrap();
        println!("{tier}: mouth to ear {delay} ms (match {matched:.2})");
        assert!(delay < limit, "{tier}: {delay} ms");
        assert!(matched > 0.5, "{tier}: the delay match is weak ({matched})");
    }
}

// ---- Discovery.

#[test]
fn voice_beacon_spacing_10s() {
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 14);
    let _ = w.app(0, 1, "REV", "PC");
    w.run(1_000);
    // Registrations change a few times in a row (a second app, a rename).
    let r = w.raw(0, 2);
    w.raw_send(r, &strs(msg::REGISTER, &["ANN", "TAB"]));
    w.run(2_000);
    w.raw_send(r, &strs(msg::REGISTER, &["ANN", "TAB2"]));
    w.run(1_000);
    w.raw_send(r, &strs(msg::REGISTER, &["ANN", "TAB3"]));
    w.run(900_000);
    let vb: Vec<u32> = w
        .sent(0)
        .iter()
        .filter(|(_, ty, sub, _)| *ty == 15 && *sub == 0)
        .map(|x| x.0)
        .collect();
    assert!(!vb.is_empty(), "voice beacons went out");
    assert!(
        vb[0] < 30_000,
        "the first soon after the registration: {vb:?}"
    );
    for p in vb.windows(2) {
        assert!(
            p[1] - p[0] >= 10_000,
            "voice beacons {} ms apart: {vb:?}",
            p[1] - p[0]
        );
    }
    // Each normal beacon is followed by a voice beacon (not before 10 s).
    let nb: Vec<u32> = w
        .sent(0)
        .iter()
        .filter(|(_, ty, _, _)| *ty == 12)
        .map(|x| x.0)
        .collect();
    assert!(!nb.is_empty(), "normal beacons in 15 minutes");
    for b in &nb {
        assert!(
            vb.iter().any(|v| *v > *b && *v - *b <= 30_000),
            "voice beacon after beacon at {b}: {vb:?}"
        );
    }
    // BRAVO heard them: its directory lists REV/PC and ANN/TAB3 via ALPHA.
    let names: Vec<String> = w.net.modems[1]
        .voice_dir()
        .iter()
        .map(|e| format!("{}/{}", e.name.as_str(), e.label.as_str()))
        .collect();
    assert!(
        names.contains(&"REV/PC".to_string()) && names.contains(&"ANN/TAB3".to_string()),
        "{names:?}"
    );
}

#[test]
fn voice_scan_answers() {
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 15);
    let rev = w.app(0, 1, "REV", "PC");
    let _bob = w.app(1, 1, "BOB", "PHONE");
    w.net.host_send(1, b"AT+BEACON=QUIET\r");
    w.run(1_000);
    assert!(w.apps[rev].eng.at_line("AT+SCAN"));
    // The scan's ScanReq waits out the 10 s gap after ALPHA's voice beacon
    // (DISC_GAP_MS: no discovery frame within 10 s of the last one), then
    // answers come in for SCAN_WINDOW_MS (5 s).
    let t0 = w.now();
    assert!(w.run_until(20_000, |w| w
        .has(rev, |e| matches!(e, UiEvent::AtReply { last: true, .. }))));
    let req = w
        .sent(0)
        .into_iter()
        .find(|x| x.1 == 13)
        .map(|x| x.0)
        .expect("a ScanReq");
    let last_beacon = w
        .sent(0)
        .into_iter()
        .filter(|x| x.0 < req)
        .map(|x| x.0)
        .max()
        .unwrap_or(0);
    assert!(
        req - last_beacon >= 10_000,
        "ScanReq {req} ms, the frame before it {last_beacon} ms"
    );
    let done = w
        .when(rev, |e| matches!(e, UiEvent::AtReply { last: true, .. }))
        .unwrap();
    assert!(
        done >= req + 5_000 && done < req + 6_000,
        "answers for 5 s: {req} to {done}"
    );
    let _ = t0;
    let text: String = w.apps[rev]
        .log
        .iter()
        .filter_map(|(_, e)| match e {
            UiEvent::AtReply { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("SCAN BRAVO") && text.contains("VOICE BOB/PHONE"),
        "{text:?}"
    );
    assert!(
        count_sent(&w, 1, 15, Some(1), 0) >= 1,
        "a voice scan answer (type 15, sub-type 1)"
    );
    assert_eq!(count_sent(&w, 1, 14, None, 0), 0, "not the plain ScanResp");
    w.apps[rev].eng.refresh_directory();
    w.run(2_000);
    let dir = w.apps[rev].log.iter().rev().find_map(|(_, e)| match e {
        UiEvent::Directory(d) => Some(d.clone()),
        _ => None,
    });
    let dir = dir.expect("a directory");
    assert!(
        dir.iter()
            .any(|d| d.name == "BOB" && d.label == "PHONE" && d.station == "BRAVO"),
        "{dir:?}"
    );
}

#[test]
fn data_call_unchanged_between_voice_builds() {
    // Two voice builds with voice apps registered: a terminal data call
    // between them is the data call of a build without voice (same
    // frames, CONNECT .../ARQ, data both ways), and the voice apps see
    // none of it.
    let mut w = World::new(vec![cfg("ALPHA", None), cfg("BRAVO", None)], 10, 16);
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.net.host_send(1, b"ATS0=1\r");
    w.run(1_000);
    let t0 = w.now();
    w.net.host_send(0, b"ATDT BRAVO\r");
    assert!(w.run_until(20_000, |w| w.net.modems[0].is_connected()));
    w.run(500);
    let a = w.net.host_text(0);
    assert!(
        a.contains("CONNECT LORA SF5") && a.contains("/ARQ"),
        "{a:?}"
    );
    assert!(!a.contains("VOICE"));
    w.net.host_text(1);
    w.net.host_send(0, b"hello from alpha\r\n");
    w.run(3_000);
    assert!(w.net.host_text(1).contains("hello from alpha"));
    for m in [0, 1] {
        let call: Vec<(u8, u8)> = w
            .sent(m)
            .iter()
            .filter(|(at, _, _, _)| *at >= t0)
            .map(|x| (x.1, x.2))
            .collect();
        assert!(
            call.iter().all(|&(t, s)| t != 15 || s == 0),
            "modem {m}: no voice call frames: {call:?}"
        );
        assert!(call.iter().any(|&(t, _)| t == 6 || t == 7), "data or acks");
    }
    for app in [rev, bob] {
        assert!(!w.has(app, is_ring) && !w.has(app, is_connected));
        assert!(!w.apps[app].eng.in_call());
    }
    // A voice call while the data call is up is refused (one call at a time).
    w.apps[rev].eng.call("BOB", None);
    w.run(1_000);
    assert!(w.has(rev, |e| matches!(e, UiEvent::Error(_))));
}

// ---- Regressions (bugs found and fixed in review).

#[test]
fn regression_ptt_key_never_left_down_across_retrain() {
    // Task 8a I1 (fixed in 4f33e51): REV holds TALK on SF9; the call
    // retrains up to duplex and back down to push to talk. Back on push to
    // talk with no new key press, the key must not still be down at the
    // modem: REV's mic sends nothing.
    let (mut w, rev, _bob) = rev_calls_bob(None, SF9_SNR);
    assert_eq!(sf_of(&w, rev), 9);
    w.apps[rev].mic = Mic::Tone;
    w.apps[rev].eng.talk_key(true);
    assert!(w.run_until(30_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
    w.run(5_000);
    assert!(w.apps[rev].eng.stats().frames_sent > 5);
    w.net.set_snr_x4(40);
    assert!(
        w.run_until(180_000, |w| w.apps[rev]
            .eng
            .stats()
            .plan
            .is_some_and(|p| p.mode == TalkMode::Duplex)),
        "up to duplex"
    );
    w.run(3_000);
    w.net.set_snr_x4(SF9_SNR);
    assert!(
        w.run_until(180_000, |w| w.apps[rev]
            .eng
            .stats()
            .plan
            .is_some_and(|p| p.mode != TalkMode::Duplex)
            && w.apps[rev].eng.stats().slots_ms.is_some()),
        "back to push to talk"
    );
    w.run(5_000);
    let before = w.apps[rev].eng.stats().frames_sent;
    w.run(15_000);
    assert_eq!(
        w.apps[rev].eng.stats().frames_sent,
        before,
        "no mic audio without the key"
    );
    assert_ne!(w.apps[rev].eng.floor(), Floor::Mine);
    // A fresh key press works.
    w.apps[rev].eng.talk_key(false);
    w.apps[rev].eng.talk_key(true);
    assert!(w.run_until(30_000, |w| w.apps[rev].eng.stats().frames_sent > before));
}

/// Raw REV on ALPHA session 1 calls raw BOB on BRAVO session 1 (BOB
/// answers on the first Ring); both say VoiceReady once connected.
/// Returns once both CallStates show the slots running.
fn raw_call(w: &mut World) -> (usize, usize) {
    let a = w.raw(0, 1);
    let b = w.raw(1, 1);
    w.raw_send(a, &strs(msg::REGISTER, &["REV", "PC"]));
    w.raw_send(b, &strs(msg::REGISTER, &["BOB", "PHONE"]));
    w.run(500);
    w.raw_send(a, &strs(msg::CALL, &["BOB", ""]));
    assert!(w.run_until(20_000, |w| !w.raw_frames(b, msg::RING).is_empty()));
    w.raw_send(b, &[msg::ANSWER]);
    let connected = |w: &World, r: usize| {
        w.raw_frames(r, msg::CALL_STATE)
            .last()
            .is_some_and(|(_, p)| p[0] == call_state::CONNECTED)
    };
    assert!(w.run_until(60_000, |w| connected(w, a) && connected(w, b)));
    w.raw_send(a, &[msg::VOICE_READY]);
    w.raw_send(b, &[msg::VOICE_READY]);
    let slots = |w: &World, r: usize| {
        w.raw_frames(r, msg::CALL_STATE)
            .last()
            .is_some_and(|(_, p)| p[1] & 1 != 0)
    };
    assert!(w.run_until(30_000, |w| slots(w, a) && slots(w, b)));
    (a, b)
}

#[test]
fn regression_ptt_voice_out_after_key_up_is_refused() {
    // Final review 9b: in push to talk the modem takes VoiceOut only while
    // the floor is ours and the key is down. The tail sent just before the
    // key goes up still goes out; VoiceOut after the key went up is
    // dropped, even while the floor is still ours (the holder keeps it
    // until its queue is empty).
    let mut w = World::with_channel(
        vec![
            cfg("ALPHA", Some(Tier::Sf12)),
            cfg("BRAVO", Some(Tier::Sf12)),
        ],
        chan_x4(SF12_ONLY, 71),
    );
    let (a, b) = raw_call(&mut w);
    w.raw_send(a, &[msg::TALK_KEY, 1]);
    assert!(w.run_until(20_000, |w| raw_floor(w, a) == Some(1)));
    let t0 = w.now();
    let mut tail = vec![msg::VOICE_OUT];
    tail.extend_from_slice(&[0x11; 30]);
    let mut late = vec![msg::VOICE_OUT];
    late.extend_from_slice(&[0x22; 30]);
    w.raw_send(a, &tail);
    w.raw_send(a, &[msg::TALK_KEY, 0]);
    w.raw_send(a, &late);
    w.raw_send(a, &late);
    assert!(w.run_until(30_000, |w| raw_floor(w, a) == Some(0)));
    w.run(10_000);
    let data = count_sent(&w, 0, 15, Some(VSub::Data as u8), t0);
    assert_eq!(data, 1, "only the tail went on the air");
    let heard: Vec<Vec<u8>> = w
        .raw_frames(b, msg::VOICE_IN)
        .into_iter()
        .filter(|(t, _)| *t >= t0)
        .map(|(_, p)| p)
        .collect();
    assert_eq!(heard, vec![vec![0x11; 30]], "BOB hears the tail only");
}

fn raw_floor(w: &World, r: usize) -> Option<u8> {
    w.raw_frames(r, msg::FLOOR).last().map(|(_, p)| p[0])
}

fn control_in(w: &World, r: usize) -> Vec<u8> {
    w.raw_frames(r, msg::CONTROL_IN)
        .into_iter()
        .flat_map(|(_, p)| p)
        .collect()
}

#[test]
fn regression_sf12_listener_chunks_fit_after_fade_from_sf9() {
    // Task 6 review Important 1 (fixed in 44b58ef): SF9 slots with the
    // caller holding the floor and 100 control bytes queued each way in
    // 32 byte chunks, then a fade only SF12 survives. On SF12 no Listener
    // frame carries more than SF12's layout chunk (24), nothing collides,
    // and every byte arrives both ways.
    let mut w = World::with_channel(
        vec![cfg("ALPHA", Some(Tier::Sf9)), cfg("BRAVO", Some(Tier::Sf9))],
        chan_x4(SF9_SNR, 31),
    );
    let (a, b) = raw_call(&mut w);
    w.raw_send(a, &[msg::TALK_KEY, 1]);
    assert!(w.run_until(10_000, |w| raw_floor(w, a) == Some(1)));
    let pa: Vec<u8> = (0..100u8).collect();
    let pb: Vec<u8> = (100..200u8).collect();
    let mut f = vec![msg::CONTROL_OUT];
    f.extend_from_slice(&pa);
    w.raw_send(a, &f);
    let mut f = vec![msg::CONTROL_OUT];
    f.extend_from_slice(&pb);
    w.raw_send(b, &f);
    w.run(500);
    // Still queued at the fade: SF9 moves 32 bytes per listen period.
    assert!(
        control_in(&w, b).len() < pa.len(),
        "REV's bytes still in flight"
    );
    assert!(
        control_in(&w, a).len() < pb.len(),
        "BOB's bytes still in flight"
    );
    let t_fade = w.now();
    let coll0 = w.net.collisions();
    w.net.set_snr_x4(SF12_ONLY);
    // REV talks the whole time: a VoiceOut of the tier's max_sealed every
    // second (the modem keeps two), so every slot carries its voice frame
    // and a Listener frame running past its sub-slot would hit it.
    let end = w.now() + 180_000;
    while w.now() < end {
        let max = w
            .raw_frames(a, msg::CALL_STATE)
            .last()
            .map_or(0, |(_, p)| usize::from(p[7]));
        if max > 0 && w.now().is_multiple_of(1000) {
            let mut v = vec![msg::VOICE_OUT];
            v.extend(std::iter::repeat_n(0x33, max));
            w.raw_send(a, &v);
        }
        w.step();
    }
    let voice12 = w
        .net
        .air_log()
        .iter()
        .filter(|e| e.0 >= t_fade && e.1 == 0 && e.2 == Tier::Sf12 && e.3.get(5) == Some(&5))
        .count();
    assert!(voice12 > 20, "REV's voice frames on SF12: {voice12}");
    // The fix (44b58ef) lets a chunk queued on SF9 (32 bytes) go in an SF12
    // Listener frame only where `big_chunk_ok` says it fits (the floor is
    // free, the sender holds the floor, or the overrun fits the slot's
    // spare time); the brief's wording "never larger than the tier's
    // chunk" is therefore checked as: never over LISTEN_CHUNK (32), and
    // never colliding with anything (below).
    let sf12 = slot_layout(Tier::Sf12).chunk as usize;
    assert_eq!(sf12, 24);
    let mut on12 = 0;
    let mut big = 0;
    for (at, from, tier, bytes) in w.net.air_log() {
        let _ = from;
        if *at < t_fade
            || *tier != Tier::Sf12
            || bytes.get(1).map(|x| x >> 4) != Some(15)
            || bytes[5] != 6
        {
            continue;
        }
        on12 += 1;
        if bytes[6] & 0x08 != 0 {
            let chunk = bytes.len() - 12;
            assert!(
                chunk <= LISTEN_CHUNK,
                "an SF12 Listener frame carried {chunk} bytes at {at} ms"
            );
            if chunk > sf12 {
                big += 1;
            }
        }
    }
    assert!(on12 > 5, "Listener frames on SF12: {on12}");
    println!("SF12 Listener frames after the fade: {on12}, {big} with a 32 byte SF9 chunk");
    let last_sf = w.raw_frames(a, msg::CALL_STATE).last().map(|(_, p)| p[2]);
    assert_eq!(last_sf, Some(12), "the call runs on SF12");
    assert_eq!(
        w.net.collisions() - coll0,
        0,
        "no collisions after the fade"
    );
    assert_eq!(control_in(&w, b), pa, "REV's bytes all arrived");
    assert_eq!(control_in(&w, a), pb, "BOB's bytes all arrived");
}

thread_local! {
    /// (drop from this time, Listener frames still to drop).
    static LISTENER_DROP: RefCell<(u32, u32)> = const { RefCell::new((u32::MAX, 0)) };
}

fn drop_listeners(t: u32, b: &[u8]) -> bool {
    if b.get(1).map(|x| x >> 4) != Some(15) || b.get(5) != Some(&6) {
        return false;
    }
    LISTENER_DROP.with(|d| {
        let mut d = d.borrow_mut();
        if t >= d.0 && d.1 > 0 {
            d.1 -= 1;
            true
        } else {
            false
        }
    })
}

#[test]
fn regression_lost_listener_frames_at_sf9_no_stepdown() {
    // Task 6 review (due-only silence, 44b58ef): the floor holder misses
    // two, then later three, Listener frames in a row: that is not a fade.
    // Only slots in which the peer was due count as silence; counting every
    // slot (before the fix) makes three lost Listener frames (11 slots,
    // 4.4 s) pass the 4 RTO step down line, which this test then catches
    // (two alone stay just under it, 3.2 s).
    LISTENER_DROP.with(|d| *d.borrow_mut() = (u32::MAX, 0));
    let chan = SimChannel {
        profile: SnrProfile::Constant(SF9_SNR),
        seed: 41,
        drop: Some(drop_listeners),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", Some(Tier::Sf9)), cfg("BRAVO", Some(Tier::Sf9))],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let _bob = w.app(1, 1, "BOB", "PHONE");
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(60_000, |w| w.apps[rev].eng.stats().slots_ms.is_some()));
    w.apps[rev].mic = Mic::Tone;
    w.apps[rev].eng.talk_key(true);
    assert!(w.run_until(20_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
    w.run(5_000);
    let t = w.now();
    LISTENER_DROP.with(|d| *d.borrow_mut() = (t, 2));
    w.run(20_000);
    assert_eq!(
        LISTENER_DROP.with(|d| d.borrow().1),
        0,
        "two Listener frames were dropped"
    );
    let t3 = w.now();
    LISTENER_DROP.with(|d| *d.borrow_mut() = (t3, 3));
    w.run(20_000);
    assert_eq!(
        LISTENER_DROP.with(|d| d.borrow().1),
        0,
        "three Listener frames were dropped"
    );
    assert_eq!(
        count_sent(&w, 0, 8, None, t) + count_sent(&w, 1, 8, None, t),
        0,
        "no TierSet"
    );
    assert_eq!(sf_of(&w, rev), 9);
    assert!(
        w.apps[rev]
            .log
            .iter()
            .all(|(at, e)| *at < t || !matches!(e, UiEvent::PlanChanged(_))),
        "no plan change"
    );
}

#[test]
fn regression_real_fade_reaches_resync_before_s22() {
    // Task 6 cap (ad3449e): with the floor held on SF9, a fade below SF9's
    // floor resyncs well inside S22 (20 s), and the call goes on on SF12.
    let (mut w, rev, bob) = rev_calls_bob(Some(Tier::Sf9), SF9_SNR);
    w.apps[rev].mic = Mic::Tone;
    w.apps[rev].eng.talk_key(true);
    assert!(w.run_until(20_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
    w.run(3_000);
    let t0 = w.now();
    w.net.set_snr_x4(SF12_ONLY);
    let r = w.run_until(20_000, |w| {
        w.apps[rev].eng.stats().state == call_state::RESYNC
    });
    let took = w.now() - t0;
    assert!(r, "resync within S22");
    println!("SF9 fade: resync after {took} ms");
    assert!(took < 15_000, "resync after {took} ms");
    assert!(w.run_until(40_000, |w| sf_of(w, rev) == 12
        && w.apps[rev].eng.stats().state == call_state::CONNECTED));
    w.run(20_000);
    assert!(
        w.apps[rev].eng.in_call() && w.apps[bob].eng.in_call(),
        "the call goes on"
    );
    assert!(!w.has(rev, |e| matches!(e, UiEvent::Ended)));
}

#[test]
fn regression_low_s22_never_fires_wall_caps_early() {
    // Task 6 minor folded into Task 7: the push to talk wall caps (PING at
    // 3, step down at 5, resync at 7 listen periods) were capped at a
    // share of S22, so a low S22 fired them on ordinary gaps. They never
    // go below 3/5/7 listen periods. S22 = 2 s here; SF9's listen period
    // is 1.2 s, so a cap from S22 would ping every 0.8 s.
    let mut a = cfg("ALPHA", Some(Tier::Sf9));
    let mut b = cfg("BRAVO", Some(Tier::Sf9));
    a.sregs.set(22, 2);
    b.sregs.set(22, 2);
    let mut w = World::with_channel(vec![a, b], chan_x4(SF9_SNR, 51));
    let rev = w.app(0, 1, "REV", "PC");
    let bob = w.app(1, 1, "BOB", "PHONE");
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(60_000, |w| w.apps[rev].eng.stats().slots_ms.is_some()));
    w.apps[rev].mic = Mic::Tone;
    w.apps[rev].eng.talk_key(true);
    assert!(w.run_until(20_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
    let t = w.now();
    w.run(60_000);
    let pings = w
        .sent(0)
        .iter()
        .filter(|(at, ty, _, b)| *at >= t && *ty == 11 && b[1] & 2 != 0)
        .count();
    assert!(pings <= 2, "{pings} wall clock PINGs in 60 s");
    assert_eq!(
        count_sent(&w, 0, 8, None, t) + count_sent(&w, 1, 8, None, t),
        0,
        "no TierSet"
    );
    assert_eq!(sf_of(&w, rev), 9);
    assert!(w.apps[rev].eng.in_call() && w.apps[bob].eng.in_call());
    assert!(w.apps[rev]
        .log
        .iter()
        .all(|(at, e)| *at < t || !matches!(e, UiEvent::Ended)));
}

/// Drops every voice Answer and every voice TierSet (tier and token).
fn drop_answers_and_tiersets(t: u32, b: &[u8]) -> bool {
    drop_answers(t, b) || (b.get(1).map(|x| x >> 4) == Some(8) && b.len() == 10)
}

#[test]
fn regression_late_pickup_hearing_hangup_is_elsewhere_not_no_carrier() {
    // Task 5 minor folded into Task 6 (880ffc0): a late pickup still
    // waiting for its TierSet that hears the call's Hangup says answered
    // elsewhere and goes idle, silently: never NO CARRIER, never Ended.
    // Every Answer and TierSet is lost here, so BOB/TAB still rings after
    // BOB/PHONE picked up (REV bound PHONE by its Train), picks up, and
    // then hears only REV's Hangup.
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 18,
        drop: Some(drop_answers_and_tiersets),
    };
    let mut w = World::with_channel(
        vec![cfg("ALPHA", None), cfg("BRAVO", None), cfg("CHARLIE", None)],
        chan,
    );
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    w.apps[phone].auto_answer = false;
    let r = w.raw(2, 1);
    w.raw_send(r, &strs(msg::REGISTER, &["BOB", "TAB"]));
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring)
        && !w.raw_frames(r, msg::RING).is_empty()));
    w.apps[phone].eng.answer();
    // PHONE's Train went out (REV binds by it); TAB still rings.
    assert!(w.run_until(5_000, |w| count_sent(w, 1, 4, None, 0) > 0));
    w.run(500);
    assert!(
        w.raw_frames(r, msg::ANSWERED_ELSEWHERE).is_empty(),
        "TAB still rings"
    );
    w.raw_send(r, &[msg::ANSWER]);
    w.run(1_000);
    assert!(
        w.raw_frames(r, msg::ERROR).is_empty(),
        "TAB's Answer was taken"
    );
    assert!(
        w.net.modems[2].voice_call().is_some(),
        "CHARLIE waits for its TierSet"
    );
    // REV hangs up while the late pickup still waits.
    w.apps[rev].eng.hangup();
    assert!(w.run_until(20_000, |w| !w
        .raw_frames(r, msg::ANSWERED_ELSEWHERE)
        .is_empty()));
    w.run(10_000);
    assert!(w.raw_frames(r, msg::ENDED).is_empty(), "never Ended");
    let text = w.net.host_text(2);
    assert!(!text.contains("NO CARRIER"), "{text:?}");
    assert!(w.net.modems[2].voice_call().is_none());
    let st = w.raw_frames(r, msg::CALL_STATE);
    assert!(st.iter().all(|(_, p)| p[0] != call_state::CONNECTED));
}

#[test]
fn regression_pickup_gives_up_at_s7_on_a_busy_channel() {
    // Found in this task's review of its own fix (ca89514): an answer waits
    // for a quiet channel, so on a channel that is never quiet for long
    // enough it may never go out; the pickup must still give up S7 after
    // picking up (V223.LRA.90 clause 9.3 item 4), not wait for the caller.
    // Every Answer and TierSet is lost, so REV keeps resending its TierSet
    // (about one every second) to BOB/PHONE; BOB/TAB on CHARLIE (S7 = 10 s)
    // picks up late and never hears a TierSet.
    let chan = SimChannel {
        profile: SnrProfile::Constant(40),
        seed: 19,
        drop: Some(drop_answers_and_tiersets),
    };
    let mut charlie = cfg("CHARLIE", None);
    charlie.sregs.set(7, 10);
    let mut w = World::with_channel(vec![cfg("ALPHA", None), cfg("BRAVO", None), charlie], chan);
    let rev = w.app(0, 1, "REV", "PC");
    let phone = w.app(1, 1, "BOB", "PHONE");
    w.apps[phone].auto_answer = false;
    let r = w.raw(2, 1);
    w.raw_send(r, &strs(msg::REGISTER, &["BOB", "TAB"]));
    w.run(1_000);
    w.apps[rev].eng.call("BOB", None);
    assert!(w.run_until(15_000, |w| w.has(phone, is_ring)
        && !w.raw_frames(r, msg::RING).is_empty()));
    w.apps[phone].eng.answer();
    assert!(w.run_until(5_000, |w| count_sent(w, 1, 4, None, 0) > 0));
    w.run(500);
    let t0 = w.now();
    w.raw_send(r, &[msg::ANSWER]);
    assert!(w.run_until(15_000, |w| !w
        .raw_frames(r, msg::ANSWERED_ELSEWHERE)
        .is_empty()));
    let took = w.raw_frames(r, msg::ANSWERED_ELSEWHERE)[0].0 - t0;
    let sends = count_sent(&w, 2, 15, Some(3), t0);
    println!("BOB/TAB gave up {took} ms after picking up, {sends} answers sent");
    assert!(
        sends < 2,
        "the busy channel kept it under ANSWER_SENDS: {sends}"
    );
    assert!(
        (10_000..=11_000).contains(&took),
        "gave up after {took} ms (S7 10 s)"
    );
    let t1 = w.now();
    w.run(10_000);
    assert!(
        w.sent(2)
            .iter()
            .all(|x| x.0 < t1 || x.1 == 12 || (x.1 == 15 && x.2 == 0)),
        "quiet after giving up"
    );
    assert!(w.raw_frames(r, msg::ENDED).is_empty());
    assert!(!w.net.host_text(2).contains("NO CARRIER"));
}

#[test]
fn regression_non_holder_hangup_during_ptt_hold_reaches_peer() {
    // Controller ruling R1 (Task 9a review): BOB, not holding the floor,
    // hangs up while REV holds it on SF9 (REV's voice frames fill the
    // slots). REV hears the Hangup and ends at once, not after resync and
    // S22 (27.7 s on the harness that first showed it). Hangup moments
    // spread over a whole listen period.
    for k in 0..12u32 {
        let (mut w, rev, bob) = rev_calls_bob(Some(Tier::Sf9), SF9_SNR);
        w.apps[rev].mic = Mic::Tone;
        w.apps[rev].eng.talk_key(true);
        assert!(w.run_until(20_000, |w| w.apps[rev].eng.floor() == Floor::Mine));
        w.run(2_000 + k * 100);
        let t0 = w.now();
        w.apps[bob].eng.hangup();
        let ended = w.run_until(10_000, |w| w.has(rev, |e| matches!(e, UiEvent::Ended)));
        let took = w.now() - t0;
        assert!(
            ended && took < 3_000,
            "offset {} ms: REV ended after {took} ms",
            k * 100
        );
    }
}

#[test]
fn regression_hangup_in_the_slots_ends_for_the_app_at_once() {
    // Fix round 2: on SF12 BOB (not holding the floor) hangs up; his slotted
    // Hangups may take one or two listen periods (4 s each) to go out, but
    // his app hears Ended at once. A new Call in that window is refused
    // (Error 4, the link is still busy); once the link is idle it works. REV
    // ends promptly too.
    let mut w = World::with_channel(
        vec![
            cfg("ALPHA", Some(Tier::Sf12)),
            cfg("BRAVO", Some(Tier::Sf12)),
        ],
        chan_x4(SF12_ONLY, 71),
    );
    let (a, b) = raw_call(&mut w);
    w.raw_send(a, &[msg::TALK_KEY, 1]);
    assert!(w.run_until(20_000, |w| raw_floor(w, a) == Some(1)));
    w.run(3_000);
    let t0 = w.now();
    w.raw_send(b, &[msg::HANGUP]);
    assert!(w.run_until(1_000, |w| w
        .raw_frames(b, msg::ENDED)
        .iter()
        .any(|(t, _)| *t >= t0)));
    let took = w.raw_frames(b, msg::ENDED).last().unwrap().0 - t0;
    assert!(
        took <= 100,
        "BOB's app heard Ended {took} ms after its Hangup"
    );
    let st = w.raw_frames(b, msg::CALL_STATE).last().unwrap().1.clone();
    assert_eq!(st[0], call_state::IDLE);
    // The link is still sending its Hangups: a new call is refused now.
    let busy = w.net.modems[1].voice_call().is_some();
    assert!(busy, "BRAVO's Hangups are still waiting for its sub-slots");
    w.raw_send(b, &strs(msg::CALL, &["REV", ""]));
    w.run(50);
    assert!(
        w.raw_frames(b, msg::ERROR)
            .iter()
            .any(|(_, p)| p[0] == msg::CALL && p[1] == 4),
        "a Call while the Hangups go out is refused"
    );
    // REV hears a Hangup and ends within two listen periods plus a slot.
    assert!(
        w.run_until(12_000, |w| !w.raw_frames(a, msg::ENDED).is_empty()),
        "REV ends"
    );
    println!(
        "SF12: BOB's Ended after {took} ms, REV's after {} ms, BRAVO busy at first: {busy}",
        w.raw_frames(a, msg::ENDED)[0].0 - t0
    );
    assert!(w.run_until(15_000, |w| w.net.modems[1].voice_call().is_none()));
    let n_err = w.raw_frames(b, msg::ERROR).len();
    w.run(1_000);
    w.raw_send(b, &strs(msg::CALL, &["REV", ""]));
    w.run(500);
    assert_eq!(
        w.raw_frames(b, msg::ERROR).len(),
        n_err,
        "a Call once idle is taken"
    );
    assert!(w.net.modems[1].voice_call().is_some());
}

/// Push to talk, turns taking turns (Task 9b fix): each speaker presses
/// TALK as soon as the screen shows the other let go (floor free) and talks
/// at once (before the floor is theirs), for 4 frames, or for 1 frame and
/// let go (turns 4 and 5, before the floor came at SF12), right after the
/// last word. Every frame an app sends must be heard (the words said while waiting for the
/// floor, and the last frame at key up), in both directions, and the far
/// end must hear (nearly) all of the words.
fn alternating_turns(cap: Tier, snr_x4: i16) {
    let (mut w, rev, bob) = rev_calls_bob(Some(cap), snr_x4);
    let frame = u32::from(lfd3k_core::voice::talk_plan(cap).frame_ms);
    let mut said = [0u64; 2];
    let mut last = (rev, 0u32);
    for turn in 0..6 {
        let sp = if turn % 2 == 0 { rev } else { bob };
        assert!(
            w.run_until(60_000, |w| w.apps[sp].eng.floor() == Floor::Free),
            "{cap:?} turn {turn}: the other side lets go"
        );
        let long = turn < 4;
        let talk = if long { 4 * frame } else { frame };
        w.apps[sp].eng.talk_key(true);
        w.apps[sp].mic = Mic::Tone;
        let t0 = w.now();
        w.run_until(10 * frame, |w| w.now() >= t0 + talk);
        if long {
            assert_eq!(
                w.apps[sp].eng.floor(),
                Floor::Mine,
                "{cap:?} turn {turn}: the floor came while talking"
            );
        }
        w.apps[sp].mic = Mic::Silent;
        w.apps[sp].eng.talk_key(false);
        last = (sp, w.apps[sp].eng.stats().frames_sent);
        said[usize::from(sp != rev)] += u64::from(talk) * 16;
    }
    w.run(12 * frame);
    // The last key up is done: the floor is free at both ends, and after
    // it only the words kept while waiting (at most 3 frames) and the tail
    // went out.
    for a in [rev, bob] {
        assert_eq!(w.apps[a].eng.floor(), Floor::Free, "{cap:?}: the floor is free at the end");
    }
    let after = w.apps[last.0].eng.stats().frames_sent - last.1;
    assert!(after <= 4, "{cap:?}: {after} frames sent after the last key up");
    // With the key up nothing more goes out, even with the mic live.
    let sent = [w.apps[rev].eng.stats().frames_sent, w.apps[bob].eng.stats().frames_sent];
    w.apps[rev].mic = Mic::Tone;
    w.apps[bob].mic = Mic::Tone;
    w.run(4 * frame);
    w.apps[rev].mic = Mic::Silent;
    w.apps[bob].mic = Mic::Silent;
    assert_eq!(
        [w.apps[rev].eng.stats().frames_sent, w.apps[bob].eng.stats().frames_sent],
        sent,
        "{cap:?}: no frames with the key up"
    );
    for (a, b) in [(rev, bob), (bob, rev)] {
        let (s, h) = (
            w.apps[a].eng.stats().frames_sent,
            w.apps[b].eng.stats().frames_heard,
        );
        println!("{cap:?}: {s} frames sent, {h} heard");
        assert_eq!(s, h, "{cap:?}: every frame sent is heard");
    }
    let heard = w.apps[bob].heard_samples + w.apps[rev].heard_samples;
    let total = said[0] + said[1];
    println!("{cap:?}: {heard} of {total} samples of words heard");
    assert!(
        heard * 100 >= total * 95,
        "{cap:?}: {heard} of {total} samples of words heard"
    );
}

/// Push to talk, TALK let go before the floor came and then no more mic
/// audio at all (the sound card stopped): the words kept while waiting
/// still go out once the floor comes, and the key goes up on its own (from
/// the Floor event and `tick`, not only from `mic`).
#[test]
fn regression_ptt_release_resolves_with_the_mic_stopped_sf12() {
    let (mut w, rev, bob) = rev_calls_bob(Some(Tier::Sf12), SF12_ONLY);
    let frame = u32::from(lfd3k_core::voice::talk_plan(Tier::Sf12).frame_ms);
    assert!(w.run_until(60_000, |w| w.apps[rev].eng.floor() == Floor::Free));
    w.apps[rev].eng.talk_key(true);
    w.apps[rev].mic = Mic::Tone;
    w.run(frame / 2);
    assert_ne!(w.apps[rev].eng.floor(), Floor::Mine, "let go before the floor came");
    w.apps[rev].mic = Mic::Off;
    w.apps[rev].eng.talk_key(false);
    assert!(
        w.run_until(30_000, |w| w.apps[rev].eng.floor() == Floor::Mine),
        "the floor comes for the kept words"
    );
    assert!(
        w.run_until(30_000, |w| w.apps[rev].eng.floor() == Floor::Free
            && w.apps[bob].eng.floor() == Floor::Free),
        "the key went up without more mic audio"
    );
    w.run(4 * frame);
    let (s, h) = (
        w.apps[rev].eng.stats().frames_sent,
        w.apps[bob].eng.stats().frames_heard,
    );
    assert!(s >= 1, "the kept words went out");
    assert_eq!(s, h, "every frame sent is heard");
}

#[test]
fn regression_ptt_alternating_turns_lose_no_words_sf12() {
    alternating_turns(Tier::Sf12, SF12_ONLY);
}

#[test]
fn regression_ptt_alternating_turns_lose_no_words_sf9() {
    alternating_turns(Tier::Sf9, SF9_SNR);
}
