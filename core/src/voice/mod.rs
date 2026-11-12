//! Voice calls: the per-tier talk plan. Frame types and the scheduler arrive
//! in later parts; this module is the single source of truth for which codec,
//! talk mode and frame length each speed tier uses.

pub mod plan;

pub use plan::{
    duplex_fits, encoded_bytes, talk_plan, CodecId, TalkMode, TalkPlan, SEAL_OVERHEAD,
};
