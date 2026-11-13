//! Linking your own devices with a 6 digit code (spec section 7.2).
//!
//! The device that already holds the person key ("Link a device", the
//! [`LinkOffer`]) shows a code from [`new_link_code`]. The new device ("Link
//! to my name", the [`LinkJoin`]) asks for it. The two run SPAKE2 (Ed25519
//! group, RustCrypto `spake2` crate) keyed by the code, with identities
//! `"LFD3K LINK OFFER"` (side A) and `"LFD3K LINK JOIN"` (side B), then check
//! the key both ways with HMAC-SHA256, and only then does the offer send the
//! person private key and the name, sealed with ChaCha20-Poly1305. The joining
//! device keeps its own device key and label, takes the person key and the
//! name, and signs its own device key with the person key (a new
//! [`DeviceCert`](crate::DeviceCert)). A wrong code fails at the joiner's key
//! check on the offer with [`LinkError::WrongCode`], and nothing sealed is
//! sent: a wrong guess learns nothing and costs the guesser one try.
//!
//! # Messages
//!
//! Every link message starts with a type byte, so the engine can route it on
//! the call's control path (Task 8 carries them as control messages inside a
//! voice call), then the version byte ([`VERSION`]). The type bytes do not
//! overlap the scrambler's handshake types (`0x01` to `0x03`).
//!
//! | Type | Name | Sent by | Body after type and version |
//! |------|------|---------|------------------------------|
//! | `0x10` | [`MSG_LINK_REQUEST`] | join | nothing ("I have a code, send your offer") |
//! | `0x11` | [`MSG_LINK_OFFER`] | offer | offer SPAKE2 message (33) |
//! | `0x12` | [`MSG_LINK_JOIN`] | join | join SPAKE2 message (33), join confirm (32) |
//! | `0x13` | [`MSG_LINK_SEALED`] | offer | offer confirm (32), sealed person key and name |
//!
//! # Sequence
//!
//! ```text
//! offer (has the person key)                 join (new device)
//!                                            (j, req) = LinkJoin::start(code)
//!                <========== REQUEST ======  send req
//! (o, off) = LinkOffer::start(id, code)
//! send off  ============ OFFER ===========>   j.step(off) == Ok(Some(join))
//! o.step(join) == Ok(sealed)  <== JOIN ====  send join
//! send sealed ========== SEALED ==========>  j.finish(sealed, &mut id)
//! ```
//!
//! `LinkOffer::step` takes the one JOIN message, so it carries the joiner's
//! SPAKE2 message and its confirm together; that is why the joiner's first
//! message is only a request. The offer may also start before any request
//! arrives (both sides typed or showed the code); the request only tells the
//! offer side the joiner is ready. `LinkJoin::step` given the same OFFER again
//! returns `Ok(None)`; anything else after the first OFFER is
//! [`LinkError::BadMessage`].
//!
//! # Keys
//!
//! The SPAKE2 key (32 bytes) goes through HKDF-SHA256 (salt
//! `"LFD3K LINK v1"`) into three separate 32 byte keys: info
//! `"LFD3K LINK v1 join confirm"`, `"LFD3K LINK v1 offer confirm"` and
//! `"LFD3K LINK v1 seal"`. The transcript is the version byte, the offer
//! SPAKE2 message and the join SPAKE2 message. Join confirm is HMAC-SHA256
//! under the join confirm key of `"JOIN"` and the transcript; offer confirm
//! is HMAC-SHA256 under the offer confirm key of `"OFFER"` and the transcript.
//! Both are the full 32 bytes and are checked in constant time.
//!
//! The sealed part is ChaCha20-Poly1305 under the seal key (used once, so the
//! nonce is 12 zero bytes) with the full 16 byte tag; associated data is the
//! SEALED type and version bytes, then the transcript. The plaintext is the
//! person key seed (32), the name length (1) and the name (at most
//! [`MAX_FIELD`](crate::identity::MAX_FIELD) bytes).
//!
//! # Secrets
//!
//! The SPAKE2 key, the three derived keys, the person key copies and the
//! plaintext are wiped on drop (`zeroize`). The `spake2` crate's own state
//! (its copy of the code and its secret scalar) has no wipe on drop; it is
//! kept for as short a time as possible (dropped as soon as the key is made).
//! No type here has `Debug`, and nothing secret is ever printed.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use zeroize::{Zeroize, Zeroizing};

use crate::identity::{field_ok, Identity, MAX_FIELD};

/// Link message type: the joiner is ready (no body).
pub const MSG_LINK_REQUEST: u8 = 0x10;
/// Link message type: the offer's SPAKE2 message.
pub const MSG_LINK_OFFER: u8 = 0x11;
/// Link message type: the joiner's SPAKE2 message and key confirm.
pub const MSG_LINK_JOIN: u8 = 0x12;
/// Link message type: the offer's key confirm and the sealed person key and name.
pub const MSG_LINK_SEALED: u8 = 0x13;
/// Link protocol version, the second byte of every link message.
pub const VERSION: u8 = 1;

const ID_OFFER: &[u8] = b"LFD3K LINK OFFER";
const ID_JOIN: &[u8] = b"LFD3K LINK JOIN";
const HKDF_SALT: &[u8] = b"LFD3K LINK v1";
const INFO_JOIN_MAC: &[u8] = b"LFD3K LINK v1 join confirm";
const INFO_OFFER_MAC: &[u8] = b"LFD3K LINK v1 offer confirm";
const INFO_SEAL: &[u8] = b"LFD3K LINK v1 seal";
const MAC_JOIN_TAG: &[u8] = b"JOIN";
const MAC_OFFER_TAG: &[u8] = b"OFFER";

/// SPAKE2 message: side byte and a 32 byte Ed25519 point.
const PAKE_LEN: usize = 33;
const MAC_LEN: usize = 32;
const SEED_LEN: usize = 32;
const TAG_LEN: usize = 16;
const OFFER_LEN: usize = 2 + PAKE_LEN;
const JOIN_LEN: usize = 2 + PAKE_LEN + MAC_LEN;
/// Shortest SEALED message: header, confirm, seed, name length, tag.
const SEALED_MIN: usize = 2 + MAC_LEN + SEED_LEN + 1 + TAG_LEN;

type HmacSha256 = Hmac<Sha256>;
type Spake = Spake2<Ed25519Group>;

/// Why linking failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// The other device used a different code (key confirm did not match).
    WrongCode,
    /// A message of the wrong type, version, length or shape, or one that
    /// came at the wrong time.
    BadMessage,
    /// The offering device has no person key (or no name) to give.
    NoPersonKey,
    /// The joining device has no cert, so no label of its own to keep.
    NoLabel,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LinkError::WrongCode => "wrong link code",
            LinkError::BadMessage => "bad link message",
            LinkError::NoPersonKey => "this device has no person key to link with",
            LinkError::NoLabel => "this device has no label to keep",
        })
    }
}

impl std::error::Error for LinkError {}

/// A new link code: 6 decimal digits, uniform, from the OS random source.
pub fn new_link_code() -> String {
    // 4_294_000_000 is the largest multiple of 1_000_000 below 2^32, so
    // drawing below it and taking the remainder is uniform.
    const LIMIT: u32 = 4_294_000_000;
    let mut b = Zeroizing::new([0u8; 4]);
    loop {
        // An OS without a working random source cannot make keys at all.
        getrandom::fill(b.as_mut()).expect("OS random source failed");
        let mut v = u32::from_le_bytes(*b);
        if v < LIMIT {
            let code = format!("{:06}", v % 1_000_000);
            v.zeroize();
            return code;
        }
        v.zeroize();
    }
}

/// The three keys made from the SPAKE2 key. Wiped on drop.
struct Keys {
    join_mac: Zeroizing<[u8; 32]>,
    offer_mac: Zeroizing<[u8; 32]>,
    seal: Zeroizing<[u8; 32]>,
}

impl Keys {
    fn derive(spake_key: &[u8]) -> Result<Keys, LinkError> {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), spake_key);
        let mut k = Keys {
            join_mac: Zeroizing::new([0u8; 32]),
            offer_mac: Zeroizing::new([0u8; 32]),
            seal: Zeroizing::new([0u8; 32]),
        };
        hk.expand(INFO_JOIN_MAC, k.join_mac.as_mut())
            .map_err(|_| LinkError::BadMessage)?;
        hk.expand(INFO_OFFER_MAC, k.offer_mac.as_mut())
            .map_err(|_| LinkError::BadMessage)?;
        hk.expand(INFO_SEAL, k.seal.as_mut())
            .map_err(|_| LinkError::BadMessage)?;
        Ok(k)
    }
}

/// Finish SPAKE2 with the other side's message, wipe the key at once and
/// derive the three keys. A wrong code does not fail here (it gives a
/// different key); only a malformed message does.
fn finish_pake(spake: Spake, peer: &[u8]) -> Result<Keys, LinkError> {
    let key = Zeroizing::new(spake.finish(peer).map_err(|_| LinkError::BadMessage)?);
    Keys::derive(&key)
}

/// Version byte, the offer SPAKE2 message, the join SPAKE2 message.
fn transcript(offer_pake: &[u8], join_pake: &[u8]) -> Vec<u8> {
    let mut t = Vec::with_capacity(1 + 2 * PAKE_LEN);
    t.push(VERSION);
    t.extend_from_slice(offer_pake);
    t.extend_from_slice(join_pake);
    t
}

fn hmac(key: &[u8; 32], tag: &[u8], transcript: &[u8]) -> Result<HmacSha256, LinkError> {
    let mut m =
        <HmacSha256 as hmac::KeyInit>::new_from_slice(key).map_err(|_| LinkError::BadMessage)?;
    m.update(tag);
    m.update(transcript);
    Ok(m)
}

fn mac(key: &[u8; 32], tag: &[u8], transcript: &[u8]) -> Result<[u8; MAC_LEN], LinkError> {
    Ok(hmac(key, tag, transcript)?.finalize().into_bytes().into())
}

/// Constant time check of a confirm; a mismatch means the codes differ.
fn check_mac(key: &[u8; 32], tag: &[u8], transcript: &[u8], got: &[u8]) -> Result<(), LinkError> {
    hmac(key, tag, transcript)?
        .verify_slice(got)
        .map_err(|_| LinkError::WrongCode)
}

fn seal_aad(transcript: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(2 + transcript.len());
    a.push(MSG_LINK_SEALED);
    a.push(VERSION);
    a.extend_from_slice(transcript);
    a
}

/// The body of a link message of type `kind`, after its type and version.
fn body(msg: &[u8], kind: u8) -> Result<&[u8], LinkError> {
    if msg.first() != Some(&kind) || msg.get(1) != Some(&VERSION) {
        return Err(LinkError::BadMessage);
    }
    msg.get(2..).ok_or(LinkError::BadMessage)
}

fn start_spake(code: &str, offer: bool) -> (Spake, Vec<u8>) {
    let pw = Password::new(code.as_bytes());
    let (a, b) = (SpakeId::new(ID_OFFER), SpakeId::new(ID_JOIN));
    if offer {
        Spake::start_a(&pw, &a, &b)
    } else {
        Spake::start_b(&pw, &a, &b)
    }
}

/// The device that has the person key ("Link a device"). No `Debug`.
pub struct LinkOffer {
    spake: Spake,
    /// The SPAKE2 message this side sent (side byte and point).
    pake: Vec<u8>,
    /// The person key seed and the name; `None` when this device has none.
    give: Option<(Zeroizing<[u8; 32]>, String)>,
}

impl LinkOffer {
    /// Start the offer with the code this device shows. Returns the OFFER
    /// message to send. If `id` has no person key or no name, the offer
    /// still starts but [`LinkOffer::step`] fails with
    /// [`LinkError::NoPersonKey`].
    pub fn start(id: &Identity, code: &str) -> (LinkOffer, Vec<u8>) {
        let give = match (id.person_seed(), id.cert()) {
            (Some(seed), Some(c)) if field_ok(&c.name) => Some((seed, c.name.clone())),
            _ => None,
        };
        let (spake, pake) = start_spake(code, true);
        let mut out = Vec::with_capacity(OFFER_LEN);
        out.push(MSG_LINK_OFFER);
        out.push(VERSION);
        out.extend_from_slice(&pake);
        (LinkOffer { spake, pake, give }, out)
    }

    /// Take the joiner's JOIN message. When its confirm checks out, returns
    /// the SEALED message (offer confirm, then the sealed person key and
    /// name) to send. A wrong code gives [`LinkError::WrongCode`] and
    /// nothing to send.
    pub fn step(self, msg: &[u8]) -> Result<Vec<u8>, LinkError> {
        let LinkOffer { spake, pake, give } = self;
        let (seed, name) = give.ok_or(LinkError::NoPersonKey)?;
        if msg.len() != JOIN_LEN {
            return Err(LinkError::BadMessage);
        }
        let b = body(msg, MSG_LINK_JOIN)?;
        let join_pake = b.get(..PAKE_LEN).ok_or(LinkError::BadMessage)?;
        let join_mac = b.get(PAKE_LEN..).ok_or(LinkError::BadMessage)?;
        let keys = finish_pake(spake, join_pake)?;
        let tr = transcript(&pake, join_pake);
        check_mac(&keys.join_mac, MAC_JOIN_TAG, &tr, join_mac)?;

        // Sized exactly, so the buffer never regrows and leaves a copy behind.
        let mut plain = Zeroizing::new(Vec::with_capacity(SEED_LEN + 1 + name.len()));
        plain.extend_from_slice(seed.as_ref());
        plain.push(name.len() as u8);
        plain.extend_from_slice(name.as_bytes());
        let cipher = ChaCha20Poly1305::new_from_slice(keys.seal.as_ref())
            .map_err(|_| LinkError::BadMessage)?;
        let aad = seal_aad(&tr);
        let ct = cipher
            .encrypt(
                &Nonce::from([0u8; 12]),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|_| LinkError::BadMessage)?;
        let offer_mac = mac(&keys.offer_mac, MAC_OFFER_TAG, &tr)?;

        let mut out = Vec::with_capacity(2 + MAC_LEN + ct.len());
        out.push(MSG_LINK_SEALED);
        out.push(VERSION);
        out.extend_from_slice(&offer_mac);
        out.extend_from_slice(&ct);
        Ok(out)
    }
}

/// The new device ("Link to my name"). No `Debug`.
pub struct LinkJoin {
    /// Taken (and dropped) as soon as the OFFER arrives.
    spake: Option<Spake>,
    /// The SPAKE2 message this side sends (side byte and point).
    pake: Vec<u8>,
    /// After the OFFER: the OFFER message, the keys and the transcript.
    done: Option<(Vec<u8>, Keys, Vec<u8>)>,
}

impl LinkJoin {
    /// Start joining with the code the user typed. Returns the REQUEST
    /// message to send.
    pub fn start(code: &str) -> (LinkJoin, Vec<u8>) {
        let (spake, pake) = start_spake(code, false);
        (
            LinkJoin {
                spake: Some(spake),
                pake,
                done: None,
            },
            vec![MSG_LINK_REQUEST, VERSION],
        )
    }

    /// Take the offer's OFFER message; returns `Some(JOIN message)` to send.
    /// The same OFFER again gives `Ok(None)`; any other message after the
    /// first OFFER, or a malformed one, gives [`LinkError::BadMessage`].
    pub fn step(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some((offer, _, _)) = &self.done {
            return if msg == offer.as_slice() {
                Ok(None)
            } else {
                Err(LinkError::BadMessage)
            };
        }
        if msg.len() != OFFER_LEN {
            return Err(LinkError::BadMessage);
        }
        let offer_pake = body(msg, MSG_LINK_OFFER)?;
        let spake = self.spake.take().ok_or(LinkError::BadMessage)?;
        let keys = finish_pake(spake, offer_pake)?;
        let tr = transcript(offer_pake, &self.pake);
        let join_mac = mac(&keys.join_mac, MAC_JOIN_TAG, &tr)?;
        let mut out = Vec::with_capacity(JOIN_LEN);
        out.push(MSG_LINK_JOIN);
        out.push(VERSION);
        out.extend_from_slice(&self.pake);
        out.extend_from_slice(&join_mac);
        self.done = Some((msg.to_vec(), keys, tr));
        Ok(Some(out))
    }

    /// Take the offer's SEALED message: check the offer's confirm, open the
    /// person key and name, and store them in `id` with a new cert for this
    /// device's own key and label. `id` is unchanged on any error.
    pub fn finish(self, sealed: &[u8], id: &mut Identity) -> Result<(), LinkError> {
        let (_, keys, tr) = self.done.ok_or(LinkError::BadMessage)?;
        if id.cert().is_none() {
            return Err(LinkError::NoLabel);
        }
        if sealed.len() < SEALED_MIN || sealed.len() > SEALED_MIN + MAX_FIELD {
            return Err(LinkError::BadMessage);
        }
        let b = body(sealed, MSG_LINK_SEALED)?;
        let offer_mac = b.get(..MAC_LEN).ok_or(LinkError::BadMessage)?;
        let ct = b.get(MAC_LEN..).ok_or(LinkError::BadMessage)?;
        check_mac(&keys.offer_mac, MAC_OFFER_TAG, &tr, offer_mac)?;

        let cipher = ChaCha20Poly1305::new_from_slice(keys.seal.as_ref())
            .map_err(|_| LinkError::BadMessage)?;
        let aad = seal_aad(&tr);
        let plain = Zeroizing::new(
            cipher
                .decrypt(&Nonce::from([0u8; 12]), Payload { msg: ct, aad: &aad })
                .map_err(|_| LinkError::BadMessage)?,
        );
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(plain.get(..SEED_LEN).ok_or(LinkError::BadMessage)?);
        let n = usize::from(*plain.get(SEED_LEN).ok_or(LinkError::BadMessage)?);
        let name = plain.get(SEED_LEN + 1..).ok_or(LinkError::BadMessage)?;
        if name.len() != n || n > MAX_FIELD {
            return Err(LinkError::BadMessage);
        }
        let name = std::str::from_utf8(name).map_err(|_| LinkError::BadMessage)?;
        id.adopt_person(&seed, name).ok_or(LinkError::BadMessage)
    }
}
