//! Shared voice engine for the LFD3K-SM apps (PC and Android): codecs, the
//! jitter buffer and the FLOWTRON SCRAMBLER (identity keys, the call
//! handshake, sealed voice frames, the two words, the trust store and linking
//! your own devices with a code). The per-tier choices live in
//! `lfd3k_core::voice::plan`.

pub mod codec;
pub mod identity;
pub mod jitter;
pub mod link;
pub mod scrambler;
pub mod trust;
pub mod words;

pub use codec::{new_codec, VoiceCodec, SAMPLE_RATE};
pub use identity::{DeviceCert, Identity};
pub use jitter::Jitter;
pub use link::{new_link_code, LinkError, LinkJoin, LinkOffer};
pub use scrambler::{Handshake, Role, ScrambleError, Session};
pub use trust::{Trust, TrustStore};
