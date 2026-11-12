//! Voice calls: the per-tier talk plan (`plan`), frame type 15 and its
//! sub-types (`frames`), voice names and labels (`names`), registrations
//! (`regs`), the voice directory (`dir`) and voice call setup by person
//! name (`call`). The scheduler arrives in a later part.

pub mod call;
pub mod dir;
pub mod frames;
pub mod names;
pub mod plan;
pub mod regs;

pub use call::VoiceCallInfo;
pub use dir::{VoiceDir, VoiceEntry, VOICE_DIR_MAX};
pub use frames::{decode_voice, encode_voice, VSub, VoiceItems, VOICE_DISC_FRAME, VOICE_DISC_MAX};
pub use names::{valid_label, Name16, LABEL_MAX};
pub use plan::{duplex_fits, encoded_bytes, talk_plan, CodecId, TalkMode, TalkPlan, SEAL_OVERHEAD};
pub use regs::{VoiceReg, VOICE_GRACE_MS, VOICE_REGS};
