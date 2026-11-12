//! Identity keys of the FLOWTRON SCRAMBLER (spec sections 7.1 and 7.2).
//!
//! Every app install has one long term **device key** (Ed25519). The first
//! device a person sets up also makes a **person key** (Ed25519) and signs its
//! own device key with it, giving a [`DeviceCert`]. A device linked later
//! (Task 3) holds only a cert made by the person key of another device.
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
/// printable ASCII (spec 5.1); longer input is cut at a char boundary.
pub const MAX_FIELD: usize = 64;

const FILE_MAGIC: &[u8; 8] = b"LFD3KID1";

/// A device key signed by a person key: `sig` is the person key's Ed25519
/// signature over `device_pub || name || label`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCert {
    pub person_pub: [u8; 32],
    pub device_pub: [u8; 32],
    pub name: String,
    pub label: String,
    pub sig: [u8; 64],
}

impl DeviceCert {
    /// The bytes the person key signs: `device_pub || name || label`.
    fn signed_bytes(device_pub: &[u8; 32], name: &str, label: &str) -> Vec<u8> {
        let mut m = Vec::with_capacity(32 + name.len() + label.len());
        m.extend_from_slice(device_pub);
        m.extend_from_slice(name.as_bytes());
        m.extend_from_slice(label.as_bytes());
        m
    }

    /// Make a cert for `device_pub`, signed by `person`.
    pub(crate) fn sign(person: &SigningKey, device_pub: [u8; 32], name: &str, label: &str) -> Self {
        let name = cut(name);
        let label = cut(label);
        let sig = person.sign(&Self::signed_bytes(&device_pub, &name, &label));
        DeviceCert {
            person_pub: person.verifying_key().to_bytes(),
            device_pub,
            name,
            label,
            sig: sig.to_bytes(),
        }
    }

    /// True when the person key really signed this device key, name and label.
    pub fn verify(&self) -> bool {
        let Ok(person) = VerifyingKey::from_bytes(&self.person_pub) else {
            return false;
        };
        let sig = Signature::from_bytes(&self.sig);
        person
            .verify_strict(&Self::signed_bytes(&self.device_pub, &self.name, &self.label), &sig)
            .is_ok()
    }

    /// Wire form, without the device key (the carrier already holds it):
    /// person_pub (32), name length (1), name, label length (1), label, sig (64).
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
        Some((DeviceCert { person_pub, device_pub, name, label, sig }, at))
    }
}

fn cut(s: &str) -> String {
    let mut end = s.len().min(MAX_FIELD);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("").to_string()
}

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

fn random_key() -> SigningKey {
    let mut seed = Zeroizing::new([0u8; 32]);
    // An OS without a working random source cannot make keys at all.
    getrandom::fill(seed.as_mut()).expect("OS random source failed");
    SigningKey::from_bytes(&seed)
}

/// This app install's keys: the device key, an optional person key and an
/// optional cert for the device key. Private keys are wiped on drop.
pub struct Identity {
    device: SigningKey,
    person: Option<SigningKey>,
    cert: Option<DeviceCert>,
}

impl Identity {
    /// A first device: makes a device key and a person key, and signs the
    /// device key with the person key (`device_pub || name || label`).
    pub fn generate(name: &str, label: &str) -> Self {
        let device = random_key();
        let person = random_key();
        let cert = DeviceCert::sign(&person, device.verifying_key().to_bytes(), name, label);
        Identity { device, person: Some(person), cert: Some(cert) }
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

    /// Sign with the device key.
    pub(crate) fn sign_device(&self, msg: &[u8]) -> [u8; 64] {
        self.device.sign(msg).to_bytes()
    }

    /// Write the identity file (private keys). Written to a side file first,
    /// then moved over `path`. Owner only on Unix.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Sized for the largest file up front, so the buffer never regrows
        // and leaves a copy of the seeds behind unwiped.
        let mut buf = Zeroizing::new(Vec::with_capacity(8 + 32 + 33 + 1 + 32 + 2 * (1 + MAX_FIELD) + 64));
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
        Self::decode(&buf).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an LFD3K identity file"))
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
                let (c, used) = DeviceCert::decode(b.get(at + 1..)?, device.verifying_key().to_bytes())?;
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
        Some(Identity { device, person, cert })
    }
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}
