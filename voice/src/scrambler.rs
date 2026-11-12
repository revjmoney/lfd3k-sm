//! The FLOWTRON SCRAMBLER (spec section 7): the call handshake, the two word
//! check and sealed voice frames. Standard parts only: X25519, Ed25519,
//! HKDF-SHA256, SHA-256 and ChaCha20-Poly1305 from the RustCrypto and dalek
//! crates. No homemade cipher.
//!
//! # Handshake messages
//!
//! Every handshake message starts with a message type byte, so the engine can
//! tell them apart on the call's control path:
//!
//! | Type | Name | Sent by | Body |
//! |------|------|---------|------|
//! | `0x01` | [`MSG_COMMIT`] | caller | `SHA-256("FLOWTRON COMMIT" \|\| caller hello)` (32 bytes) |
//! | `0x02` | [`MSG_HELLO`] | answerer | answerer hello |
//! | `0x03` | [`MSG_REVEAL`] | caller | caller hello |
//!
//! A **hello** is: version (1, [`VERSION`]), role (1: 0 caller, 1 answerer),
//! ephemeral X25519 public key (32), device Ed25519 public key (32), cert flag
//! (1: 0 none, 1 cert follows), the optional [`DeviceCert`] (person key 32,
//! name length 1, name, label length 1, label, signature 64; its device key is
//! the hello's), then an Ed25519 signature (64) by the device key over all of
//! the hello before it. The type byte is framing only: it is not part of the
//! hello, so it is outside the signature, the commit, the HKDF salt and the
//! words hash.
//!
//! # Call sequence (commit, then reveal)
//!
//! ```text
//! caller                                     answerer
//! (hs, commit) = start(id, Caller)           (hs, hello) = start(id, Answerer)
//! send commit  =========== COMMIT ========>   on_peer(commit) == Ok(None)
//!                                            only now: send hello
//! on_peer(hello) == Ok(Some(reveal)) <== HELLO
//! send reveal  =========== REVEAL ========>   finish(reveal) -> Session
//! finish(hello) -> Session
//! ```
//!
//! The answerer must not put its hello on the air until `on_peer(commit)` has
//! returned `Ok`: if someone in the middle saw the answerer's hello before
//! committing, they could grind caller hellos until the 16 bits of words match
//! on both sides, which is exactly what the commit stops. The caller's
//! `finish` takes the same answerer hello it gave to `on_peer` (or an empty
//! slice, meaning that one); a different hello gives `BadHello`. A reveal that
//! does not match the commit gives `BadCommit`: the call stays muted.
//!
//! # Keys and frames
//!
//! `finish` checks the peer's signature and cert, does X25519 with the
//! ephemeral keys and derives two 32 byte keys with HKDF-SHA256 (salt: caller
//! hello then answerer hello; info `"FLOWTRON SCRAMBLER v1 c2a"` for caller
//! to answerer and `"FLOWTRON SCRAMBLER v1 a2c"` for answerer to caller).
//!
//! A sealed frame is a 4 byte little endian counter, then the ChaCha20-Poly1305
//! ciphertext and its full 16 byte tag. Nonce: direction byte (0 caller to
//! answerer, 1 answerer to caller), 7 zero bytes, the 4 counter bytes. `open`
//! drops a frame that fails to open, a counter at or below `highest - 64`, and
//! a repeat inside the 64 frame window. The ephemeral secret, the shared
//! secret and the session keys are wiped on drop.
//!
//! # The two words
//!
//! The first two bytes of `SHA-256("FLOWTRON WORDS" || caller hello ||
//! answerer hello)` pick word 1 from the PGP even list and word 2 from the
//! odd list ([`crate::words`]).

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signature, VerifyingKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey};
use zeroize::Zeroizing;

use crate::identity::{DeviceCert, Identity};
use crate::words;

/// Handshake message type: the caller's commit.
pub const MSG_COMMIT: u8 = 0x01;
/// Handshake message type: the answerer's hello.
pub const MSG_HELLO: u8 = 0x02;
/// Handshake message type: the caller's hello (the reveal).
pub const MSG_REVEAL: u8 = 0x03;
/// Hello version byte.
pub const VERSION: u8 = 1;

const ROLE_CALLER: u8 = 0;
const ROLE_ANSWERER: u8 = 1;
const DIR_C2A: u8 = 0;
const DIR_A2C: u8 = 1;
const INFO_C2A: &[u8] = b"FLOWTRON SCRAMBLER v1 c2a";
const INFO_A2C: &[u8] = b"FLOWTRON SCRAMBLER v1 a2c";
const COMMIT_TAG: &[u8] = b"FLOWTRON COMMIT";
const WORDS_TAG: &[u8] = b"FLOWTRON WORDS";
/// Counter plus tag: the bytes a sealed frame adds to the voice payload.
pub const SEAL_OVERHEAD: usize = 4 + 16;
const WINDOW: u32 = 64;

/// Which end of the call this side is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Caller,
    Answerer,
}

/// Why a handshake failed. Any of these means no session: the call stays muted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrambleError {
    /// Malformed, wrong version or role, out of order, or a weak key.
    BadHello,
    /// The device key's signature over the hello does not check.
    BadSignature,
    /// The cert does not check (wrong device key or bad person signature).
    BadCert,
    /// The caller's reveal does not match its commit, or no commit came first.
    BadCommit,
}

impl std::fmt::Display for ScrambleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ScrambleError::BadHello => "bad hello",
            ScrambleError::BadSignature => "bad hello signature",
            ScrambleError::BadCert => "bad device cert",
            ScrambleError::BadCommit => "reveal does not match commit",
        })
    }
}

impl std::error::Error for ScrambleError {}

/// A checked peer hello.
struct PeerHello {
    body: Vec<u8>,
    eph: [u8; 32],
    device: [u8; 32],
    cert: Option<DeviceCert>,
}

/// One side of the call handshake. Holds the per call X25519 secret, which is
/// wiped on drop. No `Debug` on purpose.
pub struct Handshake {
    role: Role,
    eph: EphemeralSecret,
    hello: Vec<u8>,
    /// Caller: the answerer hello seen by `on_peer`.
    peer: Option<PeerHello>,
    /// Answerer: the caller's commit.
    commit: Option<[u8; 32]>,
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn framed(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(1 + body.len());
    m.push(kind);
    m.extend_from_slice(body);
    m
}

impl Handshake {
    /// Start a handshake. The caller gets its commit message (send it first);
    /// the answerer gets its hello message (send it only after the commit has
    /// arrived and `on_peer` took it).
    pub fn start(id: &Identity, role: Role) -> (Handshake, Vec<u8>) {
        let eph = EphemeralSecret::random();
        let eph_pub = PublicKey::from(&eph);
        let mut hello = Vec::with_capacity(256);
        hello.push(VERSION);
        hello.push(match role {
            Role::Caller => ROLE_CALLER,
            Role::Answerer => ROLE_ANSWERER,
        });
        hello.extend_from_slice(eph_pub.as_bytes());
        hello.extend_from_slice(&id.device_pub());
        match id.cert() {
            Some(c) => {
                hello.push(1);
                c.encode_into(&mut hello);
            }
            None => hello.push(0),
        }
        let sig = id.sign_device(&hello);
        hello.extend_from_slice(&sig);
        let out = match role {
            Role::Caller => framed(MSG_COMMIT, &sha256(&[COMMIT_TAG, &hello])),
            Role::Answerer => framed(MSG_HELLO, &hello),
        };
        (Handshake { role, eph, hello, peer: None, commit: None }, out)
    }

    /// The middle step. Caller: give it the answerer's hello; it is checked
    /// and the reveal (the caller's own hello) comes back to send. Answerer:
    /// give it the caller's commit; it is stored and `None` comes back.
    pub fn on_peer(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, ScrambleError> {
        match self.role {
            Role::Caller => {
                if self.peer.is_some() || msg.first() != Some(&MSG_HELLO) {
                    return Err(ScrambleError::BadHello);
                }
                let body = msg.get(1..).ok_or(ScrambleError::BadHello)?;
                self.peer = Some(parse_hello(body, ROLE_ANSWERER)?);
                Ok(Some(framed(MSG_REVEAL, &self.hello)))
            }
            Role::Answerer => {
                if self.commit.is_some() || msg.first() != Some(&MSG_COMMIT) || msg.len() != 33 {
                    return Err(ScrambleError::BadHello);
                }
                let c: [u8; 32] = msg.get(1..33).and_then(|s| s.try_into().ok()).ok_or(ScrambleError::BadHello)?;
                self.commit = Some(c);
                Ok(None)
            }
        }
    }

    /// The last step. Caller: the answerer hello already given to `on_peer`
    /// (or an empty slice). Answerer: the caller's reveal, checked against the
    /// commit. Both sides end with the same words and matching keys.
    pub fn finish(self, peer_hello: &[u8]) -> Result<Session, ScrambleError> {
        let Handshake { role, eph, hello, peer, commit } = self;
        let peer = match role {
            Role::Caller => {
                let p = peer.ok_or(ScrambleError::BadHello)?;
                if !peer_hello.is_empty()
                    && (peer_hello.first() != Some(&MSG_HELLO) || peer_hello.get(1..) != Some(p.body.as_slice()))
                {
                    return Err(ScrambleError::BadHello);
                }
                p
            }
            Role::Answerer => {
                let c = commit.ok_or(ScrambleError::BadCommit)?;
                if peer_hello.first() != Some(&MSG_REVEAL) {
                    return Err(ScrambleError::BadHello);
                }
                let body = peer_hello.get(1..).ok_or(ScrambleError::BadHello)?;
                if sha256(&[COMMIT_TAG, body]) != c {
                    return Err(ScrambleError::BadCommit);
                }
                parse_hello(body, ROLE_CALLER)?
            }
        };
        let (caller_hello, answerer_hello) = match role {
            Role::Caller => (hello.as_slice(), peer.body.as_slice()),
            Role::Answerer => (peer.body.as_slice(), hello.as_slice()),
        };

        let shared = eph.diffie_hellman(&PublicKey::from(peer.eph));
        if !shared.was_contributory() {
            return Err(ScrambleError::BadHello);
        }
        let mut salt = Vec::with_capacity(caller_hello.len() + answerer_hello.len());
        salt.extend_from_slice(caller_hello);
        salt.extend_from_slice(answerer_hello);
        let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
        let mut k_c2a = Zeroizing::new([0u8; 32]);
        let mut k_a2c = Zeroizing::new([0u8; 32]);
        hk.expand(INFO_C2A, k_c2a.as_mut()).map_err(|_| ScrambleError::BadHello)?;
        hk.expand(INFO_A2C, k_a2c.as_mut()).map_err(|_| ScrambleError::BadHello)?;

        let w = sha256(&[WORDS_TAG, caller_hello, answerer_hello]);
        let words = words::two_words(w[0], w[1]);

        let (send_key, send_dir, recv_key, recv_dir) = match role {
            Role::Caller => (&k_c2a, DIR_C2A, &k_a2c, DIR_A2C),
            Role::Answerer => (&k_a2c, DIR_A2C, &k_c2a, DIR_C2A),
        };
        Ok(Session {
            words,
            peer_device: peer.device,
            peer_cert: peer.cert,
            send: ChaCha20Poly1305::new_from_slice(send_key.as_slice()).map_err(|_| ScrambleError::BadHello)?,
            send_dir,
            send_ctr: 0,
            send_done: false,
            recv: ChaCha20Poly1305::new_from_slice(recv_key.as_slice()).map_err(|_| ScrambleError::BadHello)?,
            recv_dir,
            highest: None,
            window: 0,
        })
    }
}

/// Check a peer hello body (no type byte): shape, version, role, the device
/// key's signature, then the cert.
fn parse_hello(b: &[u8], want_role: u8) -> Result<PeerHello, ScrambleError> {
    let bad = ScrambleError::BadHello;
    if b.first() != Some(&VERSION) || b.get(1) != Some(&want_role) {
        return Err(bad);
    }
    let eph: [u8; 32] = b.get(2..34).and_then(|s| s.try_into().ok()).ok_or(bad)?;
    let device: [u8; 32] = b.get(34..66).and_then(|s| s.try_into().ok()).ok_or(bad)?;
    let mut at = 67;
    let cert = match b.get(66) {
        Some(0) => None,
        Some(1) => {
            let (c, used) = DeviceCert::decode(b.get(at..).ok_or(bad)?, device).ok_or(bad)?;
            at += used;
            Some(c)
        }
        _ => return Err(bad),
    };
    if b.len() != at + 64 {
        return Err(bad);
    }
    let signed = b.get(..at).ok_or(bad)?;
    let sig: [u8; 64] = b.get(at..).and_then(|s| s.try_into().ok()).ok_or(bad)?;
    let key = VerifyingKey::from_bytes(&device).map_err(|_| bad)?;
    key.verify_strict(signed, &Signature::from_bytes(&sig))
        .map_err(|_| ScrambleError::BadSignature)?;
    if let Some(c) = &cert {
        if c.device_pub != device || !c.verify() {
            return Err(ScrambleError::BadCert);
        }
    }
    Ok(PeerHello { body: b.to_vec(), eph, device, cert })
}

/// A running scrambled call: the words to show, who the peer is, and the two
/// direction keys (wiped on drop). No `Debug` on purpose.
pub struct Session {
    /// The two words both sides show (word 1 even list, word 2 odd list).
    pub words: [&'static str; 2],
    /// The peer's device public key.
    pub peer_device: [u8; 32],
    /// The peer's cert, already checked, if it sent one.
    pub peer_cert: Option<DeviceCert>,
    send: ChaCha20Poly1305,
    send_dir: u8,
    send_ctr: u32,
    send_done: bool,
    recv: ChaCha20Poly1305,
    recv_dir: u8,
    highest: Option<u32>,
    window: u64,
}

fn nonce(dir: u8, ctr: u32) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[8..].copy_from_slice(&ctr.to_le_bytes());
    Nonce::from(n)
}

impl Session {
    /// Seal one voice frame: counter (4, little endian), ciphertext, tag (16).
    /// Returns an empty vector once the 2^32 frame counter is spent (a nonce
    /// is never reused), which the engine treats as the end of the call.
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        if self.send_done {
            return Vec::new();
        }
        let ctr = self.send_ctr;
        let Ok(ct) = self.send.encrypt(&nonce(self.send_dir, ctr), plain) else {
            return Vec::new();
        };
        match ctr.checked_add(1) {
            Some(n) => self.send_ctr = n,
            None => self.send_done = true,
        }
        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&ctr.to_le_bytes());
        out.extend_from_slice(&ct);
        out
    }

    /// Open one sealed frame. `None` for a frame that fails to open, an old
    /// counter (at or below `highest - 64`) or a repeat in the window.
    pub fn open(&mut self, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < SEAL_OVERHEAD {
            return None;
        }
        let ctr = u32::from_le_bytes(sealed.get(..4)?.try_into().ok()?);
        if let Some(h) = self.highest {
            if ctr <= h {
                let back = h - ctr;
                if back >= WINDOW || self.window & (1u64 << back) != 0 {
                    return None;
                }
            }
        }
        let plain = self.recv.decrypt(&nonce(self.recv_dir, ctr), sealed.get(4..)?).ok()?;
        match self.highest {
            None => {
                self.highest = Some(ctr);
                self.window = 1;
            }
            Some(h) if ctr > h => {
                let shift = ctr - h;
                self.window = if shift >= WINDOW { 0 } else { self.window << shift };
                self.window |= 1;
                self.highest = Some(ctr);
            }
            Some(h) => self.window |= 1u64 << (h - ctr),
        }
        Some(plain)
    }
}
