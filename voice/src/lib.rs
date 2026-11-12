//! Shared voice engine for the LFD3K-SM apps (PC and Android): codecs and
//! the jitter buffer. The per-tier choices live in `lfd3k_core::voice::plan`.

pub mod codec;
pub mod jitter;

pub use codec::{new_codec, VoiceCodec, SAMPLE_RATE};
pub use jitter::Jitter;
