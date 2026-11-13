//! Identity keys of the FLOWTRON SCRAMBLER (spec sections 7.1 and 7.2).
//!
//! Every app install has one long term **device key** (Ed25519). The first
//! device a person sets up also makes a **person key** (Ed25519) and signs its
//! own device key with it, giving a [`DeviceCert`]. A device linked later
//! ([`crate::link`]) gets that person key and the name over a code protected
//! exchange and signs its own device key with it, keeping its own label.
//!
//! The identity file lives in app storage. It holds private keys, so it is
//! written owner only on Unix (mode 0600; on Windows the user profile ACL
//! applies), its buffers are wiped after use, and keys are never printed.
//! `Identity` deliberately has no `Debug`.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::{Zeroize, Zeroizing};

/// Longest name or label (bytes) carried in a cert. Voice names are 1 to 16
/// printable ASCII (spec 5.1). A longer name or label is refused when a cert
/// is made and makes [`DeviceCert::verify`] fail; it is never cut short.
pub const MAX_FIELD: usize = 64;

const FILE_MAGIC: &[u8; 8] = b"LFD3KID1";
/// Domain tag at the front of every cert's signed bytes.
const CERT_TAG: &[u8] = b"LFD3K CERT v1";

/// A device key signed by a person key: `sig` is the person key's Ed25519
/// signature over `"LFD3K CERT v1"` then device_pub, name and label, each
/// as one length byte and its bytes (see `DeviceCert::signed_bytes`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCert {
    pub person_pub: [u8; 32],
    pub device_pub: [u8; 32],
    pub name: String,
    pub label: String,
    pub sig: [u8; 64],
}

impl DeviceCert {
    /// The bytes the person key signs, the one place both signing and
    /// checking use: the domain tag `"LFD3K CERT v1"`, then each of
    /// device_pub (32), name and label as one length byte and its bytes.
    /// `None` when the name or the label is longer than [`MAX_FIELD`]: such
    /// a cert is never made and never checks out.
    pub(crate) fn signed_bytes(device_pub: &[u8; 32], name: &str, label: &str) -> Option<Vec<u8>> {
        if !field_ok(name) || !field_ok(label) {
            return None;
        }
        let mut m = Vec::with_capacity(CERT_TAG.len() + 3 + 32 + name.len() + label.len());
        m.extend_from_slice(CERT_TAG);
        m.push(32);
        m.extend_from_slice(device_pub);
        push_field(&mut m, name);
        push_field(&mut m, label);
        Some(m)
    }

    /// Make a cert for `device_pub`, signed by `person`. `None` when the name
    /// or the label is longer than [`MAX_FIELD`].
    pub(crate) fn sign(
        person: &SigningKey,
        device_pub: [u8; 32],
        name: &str,
        label: &str,
    ) -> Option<Self> {
        let sig = person.sign(&Self::signed_bytes(&device_pub, name, label)?);
        Some(DeviceCert {
            person_pub: person.verifying_key().to_bytes(),
            device_pub,
            name: name.to_string(),
            label: label.to_string(),
            sig: sig.to_bytes(),
        })
    }

    /// True when the person key really signed this device key, name and
    /// label. False for a name or label longer than [`MAX_FIELD`].
    pub fn verify(&self) -> bool {
        let Some(m) = Self::signed_bytes(&self.device_pub, &self.name, &self.label) else {
            return false;
        };
        let Ok(person) = VerifyingKey::from_bytes(&self.person_pub) else {
            return false;
        };
        let sig = Signature::from_bytes(&self.sig);
        person.verify_strict(&m, &sig).is_ok()
    }

    /// Wire form, without the device key (the carrier already holds it):
    /// person_pub (32), name length (1), name, label length (1), label, sig (64).
    /// Only for certs made by `sign` or read by `decode`, whose fields are
    /// never longer than [`MAX_FIELD`].
    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.person_pub);
        push_field(out, &self.name);
        push_field(out, &self.label);
        out.extend_from_slice(&self.sig);
    }

    /// Read the wire form at the start of `b` for device `device_pub`.
    /// Returns the cert and how many bytes it used. Checks shape only; call
    /// [`DeviceCert::verify`] for the signature.
    pub(crate) fn decode(b: &[u8], device_pub: [u8; 32]) -> Option<(Self, usize)> {
        let person_pub: [u8; 32] = b.get(..32)?.try_into().ok()?;
        let mut at = 32;
        let name = take_field(b, &mut at)?;
        let label = take_field(b, &mut at)?;
        let sig: [u8; 64] = b.get(at..at + 64)?.try_into().ok()?;
        at += 64;
        Some((
            DeviceCert {
                person_pub,
                device_pub,
                name,
                label,
                sig,
            },
            at,
        ))
    }
}

/// A voice name the voice host protocol takes: 1 to 16 printable ASCII
/// without `:` (the station name rule, spec 5.1).
pub fn valid_voice_name(s: &str) -> bool {
    lfd3k_core::voice::Name16::new(s).is_some()
}

/// True when `s` fits a cert field (at most [`MAX_FIELD`] bytes).
pub(crate) fn field_ok(s: &str) -> bool {
    s.len() <= MAX_FIELD
}

/// One length byte and the bytes. Callers check [`field_ok`] first; the
/// clamp only keeps the length byte and the bytes written in step.
fn push_field(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(MAX_FIELD);
    out.push(n as u8);
    out.extend_from_slice(b.get(..n).unwrap_or(&[]));
}

fn take_field(b: &[u8], at: &mut usize) -> Option<String> {
    let n = usize::from(*b.get(*at)?);
    if n > MAX_FIELD {
        return None;
    }
    let s = b.get(*at + 1..*at + 1 + n)?;
    *at += 1 + n;
    String::from_utf8(s.to_vec()).ok()
}

fn random_key() -> Result<SigningKey, IdentityError> {
    let mut seed = Zeroizing::new([0u8; 32]);
    // An OS without a working random source cannot make keys at all.
    getrandom::fill(seed.as_mut()).map_err(|_| IdentityError::Random)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Why [`Identity::generate`] made no identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The voice host protocol would refuse the name or the label.
    BadNameOrLabel,
    /// The OS random source failed: no keys can be made.
    Random,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            IdentityError::BadNameOrLabel => {
                "the name is 1 to 16 printable characters without ':'; the label 1 to 8 without ':' or '/'"
            }
            IdentityError::Random => "the OS random source failed, so no keys can be made",
        })
    }
}

impl std::error::Error for IdentityError {}

/// This app install's keys: the device key, an optional person key and an
/// optional cert for the device key. Private keys are wiped on drop.
pub struct Identity {
    device: SigningKey,
    person: Option<SigningKey>,
    cert: Option<DeviceCert>,
}

impl Identity {
    /// A first device: makes a device key and a person key, and signs the
    /// device key with the person key (see `DeviceCert::signed_bytes`).
    /// [`IdentityError::BadNameOrLabel`] when the voice host protocol would
    /// refuse the name or the label: a name is 1 to 16 printable ASCII
    /// without `:`, a label 1 to 8 printable ASCII without `:` or `/` (spec
    /// 5.1). [`IdentityError::Random`] when the OS gives no random bytes.
    pub fn generate(name: &str, label: &str) -> Result<Self, IdentityError> {
        if !valid_voice_name(name) || !lfd3k_core::voice::valid_label(label) {
            return Err(IdentityError::BadNameOrLabel);
        }
        let device = random_key()?;
        let person = random_key()?;
        let cert = DeviceCert::sign(&person, device.verifying_key().to_bytes(), name, label)
            .ok_or(IdentityError::BadNameOrLabel)?;
        Ok(Identity {
            device,
            person: Some(person),
            cert: Some(cert),
        })
    }

    /// The device public key.
    pub fn device_pub(&self) -> [u8; 32] {
        self.device.verifying_key().to_bytes()
    }

    /// The person public key, when this device holds the person key.
    pub fn person_pub(&self) -> Option<[u8; 32]> {
        self.person.as_ref().map(|p| p.verifying_key().to_bytes())
    }

    /// The cert for this device key, if any.
    pub fn cert(&self) -> Option<&DeviceCert> {
        self.cert.as_ref()
    }

    /// The person private key seed, for linking another device
    /// ([`crate::link`]). Wiped when dropped.
    pub(crate) fn person_seed(&self) -> Option<Zeroizing<[u8; 32]>> {
        self.person.as_ref().map(|p| {
            let mut seed = Zeroizing::new([0u8; 32]);
            seed.copy_from_slice(p.as_bytes());
            seed
        })
    }

    /// Take a person key and name from another of this person's devices
    /// ([`crate::link`]): this device keeps its own device key and the label
    /// of its current cert, and gets a new cert signed by the adopted person
    /// key. Nothing changes unless the whole new cert is made: `None` when
    /// this device has no cert (so no label) or the name is too long.
    pub(crate) fn adopt_person(&mut self, seed: &[u8; 32], name: &str) -> Option<()> {
        let label = self.cert.as_ref()?.label.clone();
        let person = SigningKey::from_bytes(seed);
        let cert = DeviceCert::sign(&person, self.device_pub(), name, &label)?;
        if !cert.verify() {
            return None;
        }
        self.person = Some(person);
        self.cert = Some(cert);
        Some(())
    }

    /// A new voice name and device label for this device: a new cert for
    /// the same device key, signed by this device's own person key (keys
    /// are kept). `None`, and nothing changes, when this device holds no
    /// person key or the protocol would refuse the name or the label.
    pub fn rename(&mut self, name: &str, label: &str) -> Option<()> {
        if !valid_voice_name(name) || !lfd3k_core::voice::valid_label(label) {
            return None;
        }
        let person = self.person.as_ref()?;
        let cert = DeviceCert::sign(person, self.device_pub(), name, label)?;
        if !cert.verify() {
            return None;
        }
        self.cert = Some(cert);
        Some(())
    }

    /// Sign with the device key.
    pub(crate) fn sign_device(&self, msg: &[u8]) -> [u8; 64] {
        self.device.sign(msg).to_bytes()
    }

    /// Sign with the person key (a device removal, [`crate::control`]).
    /// `None` when this device holds no person key.
    pub(crate) fn sign_person(&self, msg: &[u8]) -> Option<[u8; 64]> {
        self.person.as_ref().map(|p| p.sign(msg).to_bytes())
    }

    /// Write the identity file (private keys). Written to a side file first,
    /// then moved over `path`. Owner only on Unix.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Sized for the largest file up front, so the buffer never regrows
        // and leaves a copy of the seeds behind unwiped.
        let mut buf = Zeroizing::new(Vec::with_capacity(
            8 + 32 + 33 + 1 + 32 + 2 * (1 + MAX_FIELD) + 64,
        ));
        buf.extend_from_slice(FILE_MAGIC);
        buf.extend_from_slice(self.device.as_bytes());
        match &self.person {
            Some(p) => {
                buf.push(1);
                buf.extend_from_slice(p.as_bytes());
            }
            None => buf.push(0),
        }
        match &self.cert {
            Some(c) => {
                buf.push(1);
                c.encode_into(&mut buf);
            }
            None => buf.push(0),
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        // A stale side file could carry looser permissions; start fresh.
        let _ = fs::remove_file(&tmp);
        {
            let mut f = open_private(&tmp)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    /// Read an identity file written by [`Identity::save`].
    pub fn load(path: &Path) -> io::Result<Self> {
        let buf = Zeroizing::new(fs::read(path)?);
        Self::decode(&buf)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an LFD3K identity file"))
    }

    fn decode(b: &[u8]) -> Option<Self> {
        if b.get(..8)? != FILE_MAGIC {
            return None;
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(b.get(8..40)?);
        let device = SigningKey::from_bytes(&seed);
        let mut at = 40;
        let person = match *b.get(at)? {
            0 => {
                at += 1;
                None
            }
            1 => {
                seed.copy_from_slice(b.get(at + 1..at + 33)?);
                at += 33;
                Some(SigningKey::from_bytes(&seed))
            }
            _ => return None,
        };
        seed.zeroize();
        let cert = match *b.get(at)? {
            0 => {
                at += 1;
                None
            }
            1 => {
                let (c, used) =
                    DeviceCert::decode(b.get(at + 1..)?, device.verifying_key().to_bytes())?;
                at += 1 + used;
                if !c.verify() {
                    return None;
                }
                Some(c)
            }
            _ => return None,
        };
        if at != b.len() {
            return None;
        }
        Some(Identity {
            device,
            person,
            cert,
        })
    }
}

/// A new file only its owner may read (mode 0600 on Unix; Windows keeps the
/// folder's permissions).
#[cfg(unix)]
pub(crate) fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}
