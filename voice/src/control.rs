//! Control messages between the two voice apps of a call (spec 8.1). They
//! ride the call's reliable stream: the voice host protocol's ControlOut on
//! one side, ControlIn on the other (ARQ, in order, never lost while the call
//! is up). The modem treats them as opaque bytes.
//!
//! # Framing
//!
//! ControlIn is a byte stream: the far modem hands the bytes over in pieces
//! of any size (1 to 255), not in the pieces the sender gave ControlOut. So
//! every message goes on the stream as a 2 byte little endian length, then
//! that many bytes: one message type byte and its body. [`ControlReader`]
//! puts the messages back together. A length of 0 or over [`MAX_MESSAGE`]
//! means the stream is broken (it never happens between two good apps); the
//! reader then stops and the engine reports an error.
//!
//! # Message types
//!
//! The type byte ranges never overlap, so one stream carries them all:
//!
//! | Type | Name | Body |
//! |------|------|------|
//! | `0x01` to `0x03` | scrambler handshake ([`crate::scrambler`]) | commit, answerer hello, reveal |
//! | `0x10` to `0x13` | linking ([`crate::link`]) | request, offer, join, sealed |
//! | `0x20` | [`MSG_HELLO`] | sealed `"FLOWTRON HELLO v1"` (key confirm) |
//! | `0x21` | [`MSG_MUTE`] | sealed 1 byte: 1 muted, 0 not |
//! | `0x22` | [`MSG_REMOVE_DEVICE`] | sealed device key (32) and the person key's signature (64) |
//!
//! Sealed means [`crate::Session::seal`] under the call's keys (counter,
//! ciphertext, full 16 byte tag), the same as a voice frame. Each side sends
//! its sealed Hello as soon as its handshake is finished; the peer counts as
//! live (and the two words and the trust check count) only once that Hello
//! opens. There is no floor message: push to talk floor and voice-ready are
//! the modems' (the voice host protocol's TalkKey and VoiceReady).
//!
//! A device removal is signed by the person key over `"LFD3K REMOVE v1"`
//! then the removed device's public key (32). The receiving app applies it
//! only when it checks against the person key of the peer's cert and the
//! peer is a checked contact.
//!
//! Control bytes are slow once the voice slots run (a listener piece of 24
//! to 32 bytes at a time, 24 bytes every 4 s at SF12), so the engine sends
//! them only when needed: the handshake, Hello and any removals go out
//! before VoiceReady; Mute only on a change; linking only on request.

/// Sealed key confirm: the first sealed message each side sends.
pub const MSG_HELLO: u8 = 0x20;
/// Sealed mute state.
pub const MSG_MUTE: u8 = 0x21;
/// Sealed device removal.
pub const MSG_REMOVE_DEVICE: u8 = 0x22;
/// The plaintext of a sealed Hello.
pub const HELLO_TAG: &[u8] = b"FLOWTRON HELLO v1";
/// Domain tag in front of the removed device key in a removal signature.
pub const REMOVE_TAG: &[u8] = b"LFD3K REMOVE v1";
/// Longest message (type byte and body) on the stream.
pub const MAX_MESSAGE: usize = 1024;

/// One control message.
#[derive(Clone, PartialEq, Eq)]
pub enum Control {
    /// A scrambler handshake message (types 0x01 to 0x03), type byte included.
    Handshake(Vec<u8>),
    /// A sealed key confirm (the sealed blob).
    Hello(Vec<u8>),
    /// A sealed mute state (the sealed blob).
    Mute(Vec<u8>),
    /// A link message (types 0x10 to 0x13), type byte included.
    LinkMsg(Vec<u8>),
    /// A sealed device removal (the sealed blob).
    RemoveDevice(Vec<u8>),
}

impl Control {
    /// Read one message (type byte and body). `None` for an unknown type or
    /// an empty message.
    pub fn parse(m: &[u8]) -> Option<Control> {
        let (&t, body) = m.split_first()?;
        Some(match t {
            0x01..=0x03 => Control::Handshake(m.to_vec()),
            0x10..=0x13 => Control::LinkMsg(m.to_vec()),
            MSG_HELLO => Control::Hello(body.to_vec()),
            MSG_MUTE => Control::Mute(body.to_vec()),
            MSG_REMOVE_DEVICE => Control::RemoveDevice(body.to_vec()),
            _ => return None,
        })
    }

    /// The message as it goes on the stream: length (2, little endian),
    /// type byte, body. Empty for a message over [`MAX_MESSAGE`] (never
    /// built by the engine).
    pub fn encode(&self) -> Vec<u8> {
        let (t, body): (Option<u8>, &[u8]) = match self {
            Control::Handshake(m) | Control::LinkMsg(m) => (None, m),
            Control::Hello(b) => (Some(MSG_HELLO), b),
            Control::Mute(b) => (Some(MSG_MUTE), b),
            Control::RemoveDevice(b) => (Some(MSG_REMOVE_DEVICE), b),
        };
        let len = body.len() + usize::from(t.is_some());
        if len == 0 || len > MAX_MESSAGE {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(2 + len);
        out.extend_from_slice(&(len as u16).to_le_bytes());
        if let Some(t) = t {
            out.push(t);
        }
        out.extend_from_slice(body);
        out
    }
}

/// The removal signature's message: the domain tag, then the device key.
pub fn removal_signed_bytes(device_pub: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(REMOVE_TAG.len() + 32);
    m.extend_from_slice(REMOVE_TAG);
    m.extend_from_slice(device_pub);
    m
}

/// Puts control messages back together from ControlIn pieces.
#[derive(Default)]
pub struct ControlReader {
    buf: Vec<u8>,
    broken: bool,
}

impl ControlReader {
    pub fn new() -> Self {
        ControlReader::default()
    }

    /// Add ControlIn bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        if !self.broken {
            self.buf.extend_from_slice(bytes);
        }
    }

    /// True once a bad length was seen: nothing more comes out.
    pub fn broken(&self) -> bool {
        self.broken
    }

    /// The next whole message, if one is in. Unknown types are skipped.
    pub fn next_msg(&mut self) -> Option<Control> {
        loop {
            if self.broken {
                return None;
            }
            let len = usize::from(u16::from_le_bytes([*self.buf.first()?, *self.buf.get(1)?]));
            if len == 0 || len > MAX_MESSAGE {
                self.broken = true;
                self.buf.clear();
                return None;
            }
            let m = self.buf.get(2..2 + len)?.to_vec();
            self.buf.drain(..2 + len);
            if let Some(c) = Control::parse(&m) {
                return Some(c);
            }
        }
    }
}
