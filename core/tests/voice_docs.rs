//! Pins the voice constants that the documents state, so a change to one
//! without the other fails here.

use lfd3k_core::voice::host::{AIR_RING, SCOPE_BATCH, SCOPE_MAX, SCOPE_RING};
use lfd3k_core::voice::sched::{VOICE_QUEUE, VOICE_QUEUE_DUPLEX};

const SPEC: &str = include_str!("../../docs/standards/V223.LRA.90.md");
const FEATURES: &str = include_str!("../../docs/FEATURES.md");

/// Voice frames a modem holds: V223.LRA.90 clause 11.3, clause 12.4 (Table 7,
/// VoiceOut), Annex A ("Voice frames held"), and FEATURES.md's 0x08 VoiceOut
/// row all say 2 in duplex and 6 in push to talk.
#[test]
fn voice_queue_depths_match_the_documents() {
    assert_eq!(VOICE_QUEUE_DUPLEX, 2);
    assert_eq!(VOICE_QUEUE, 6);
    let held = format!("{VOICE_QUEUE_DUPLEX} in duplex, {VOICE_QUEUE} in push to talk");
    // Clause 11.3.
    assert!(SPEC.contains(&format!(
        "up to **{VOICE_QUEUE_DUPLEX}** sealed voice frames from its app in duplex and up to **{VOICE_QUEUE}** in push to talk"
    )));
    // Clause 12.4, VoiceOut.
    assert!(SPEC.contains(&format!("({VOICE_QUEUE_DUPLEX} held in duplex, {VOICE_QUEUE} in push to talk; more are refused)")));
    // Annex A.
    assert!(SPEC.contains(&format!("| Voice frames held | {held} |")));
    // FEATURES.md, 0x08 VoiceOut.
    assert!(FEATURES.contains(&format!("(the modem holds {held}; one more is refused)")));
}

/// Scope and AirEvent: V223.LRA.90 clause 12.3, clause 12.5 (Tables 11 and
/// 12) and Annex A.
#[test]
fn scope_and_air_event_constants_match_the_documents() {
    assert_eq!(SCOPE_RING, 16);
    assert_eq!(SCOPE_MAX, 8);
    assert_eq!(SCOPE_BATCH, 5);
    assert_eq!(AIR_RING, 8);
    assert!(SPEC.contains("AirEvents (the modem keeps the newest 8)"));
    assert!(SPEC.contains("| Scope samples kept, per Scope frame | 16, at most 8 |"));
    assert!(SPEC.contains("| Scope frame every | 5 samples (250 ms) |"));
    assert!(SPEC.contains("| AirEvents kept | 8 |"));
    assert!(SPEC.contains("| 0x90 | Scope | see Table 11 |"));
    assert!(SPEC.contains("| 0x91 | AirEvent | see Table 12 |"));
}
