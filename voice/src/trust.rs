//! The trust store: who the user has checked (spec section 7.1).
//!
//! A contact is a name (case-insensitive) plus a set of checked device keys
//! (each under the device label it was checked with), an optional checked
//! person key, and the device keys removed from it. Verdict, in this order:
//!
//! 1. the device key was removed: [`Trust::Conflict`];
//! 2. the device key is checked: [`Trust::Checked`];
//! 3. the peer's cert is signed by the contact's checked person key:
//!    [`Trust::Checked`] (a linked device, spec 7.2);
//! 4. a checked device of that name with the same label now shows a
//!    different key: [`Trust::Conflict`];
//! 5. anything else: [`Trust::NotChecked`].
//!
//! A removal is sticky: checking the words again does not bring a removed
//! key back, because a lost device still holds its key and still shows the
//! right words.
//!
//! Stored as JSON (public keys only, as hex) next to the identity file; see
//! [`TrustStore::path_next_to`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scrambler::Session;

/// The verdict for the device on the other end of a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Show the name, play audio.
    Checked,
    /// Show `NAME/LABEL (NOT CHECKED)` with the two words; hold audio until
    /// "words match" or "talk anyway".
    NotChecked,
    /// `NOT THE NAME YOU CHECKED`: muted, loud warning.
    Conflict,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Device {
    label: String,
    key: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Contact {
    name: String,
    #[serde(default)]
    devices: Vec<Device>,
    #[serde(default)]
    person: Option<String>,
    #[serde(default)]
    removed: Vec<String>,
}

/// The user's checked contacts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    contacts: Vec<Contact>,
}

fn hex(k: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in k {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, o) in k.iter_mut().enumerate() {
        let hi = char::from(*b.get(2 * i)?).to_digit(16)?;
        let lo = char::from(*b.get(2 * i + 1)?).to_digit(16)?;
        *o = (hi * 16 + lo) as u8;
    }
    Some(k)
}

/// Why [`TrustStore::load`] gave no store.
#[derive(Debug)]
pub struct TrustLoadError {
    /// What went wrong reading or parsing the file.
    pub error: io::Error,
    /// Where the unreadable file was moved (it is kept, never deleted).
    /// `None`: the file is still at its path (it could not be read, or not
    /// moved), so it must not be saved over.
    pub kept: Option<PathBuf>,
}

impl std::fmt::Display for TrustLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kept {
            Some(p) => write!(
                f,
                "the checked contacts file could not be read ({}); it was kept as {} and the list starts empty",
                self.error,
                p.display()
            ),
            None => write!(
                f,
                "the checked contacts file could not be read ({}); it was left as it is and nothing is saved over it",
                self.error
            ),
        }
    }
}

impl std::error::Error for TrustLoadError {}

/// One contact as an app lists it ([`TrustStore::contacts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactView {
    pub name: String,
    /// Checked devices: label and device key.
    pub devices: Vec<(String, [u8; 32])>,
    /// A person key is checked (linked devices count as checked).
    pub person: bool,
    /// Devices removed from this contact.
    pub removed: usize,
}

impl TrustStore {
    /// The trust file that goes with an identity file (same folder).
    pub fn path_next_to(identity_path: &Path) -> PathBuf {
        identity_path.with_file_name("trust.json")
    }

    /// Read the store. No file yet gives an empty store (everyone NOT
    /// CHECKED). A file that is not a trust store is moved aside to
    /// `trust.json.bad` (or `trust.json.bad.<unix seconds>`, with `.1`,
    /// `.2` ... added when that is taken too), so it is kept for the user and never written over, and the
    /// error says where it went. Any other read error is returned with the
    /// file left where it is ([`TrustLoadError::kept`] is `None`); do not
    /// save over it.
    pub fn load(path: &Path) -> Result<Self, TrustLoadError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(TrustLoadError { error, kept: None }),
        };
        match serde_json::from_slice(&bytes) {
            Ok(s) => Ok(s),
            Err(e) => {
                let error = io::Error::new(io::ErrorKind::InvalidData, e);
                // The first free name: trust.json.bad, then .bad.<secs>,
                // then .bad.<secs>.1, .2 ... (a rename would replace one).
                let mut bad = path.as_os_str().to_owned();
                bad.push(".bad");
                let mut bad = PathBuf::from(bad);
                if bad.exists() {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let mut b = bad.clone().into_os_string();
                    b.push(format!(".{secs}"));
                    let stamped = PathBuf::from(b);
                    bad = stamped.clone();
                    let mut k = 1u32;
                    while bad.exists() && k < 1000 {
                        let mut b = stamped.clone().into_os_string();
                        b.push(format!(".{k}"));
                        bad = PathBuf::from(b);
                        k += 1;
                    }
                }
                match fs::rename(path, &bad) {
                    Ok(()) => Err(TrustLoadError {
                        error,
                        kept: Some(bad),
                    }),
                    // Could not move it: leave it, and say so (kept None).
                    Err(_) => Err(TrustLoadError { error, kept: None }),
                }
            }
        }
    }

    /// Write the store: a private side file (owner only on Unix), synced to
    /// the disk, then moved over `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        // A stale side file could carry looser permissions; start fresh.
        let _ = fs::remove_file(&tmp);
        {
            let mut f = crate::identity::open_private(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    fn find(&self, name: &str) -> Option<&Contact> {
        self.contacts
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn find_or_add(&mut self, name: &str) -> &mut Contact {
        let i = match self
            .contacts
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
        {
            Some(i) => i,
            None => {
                self.contacts.push(Contact {
                    name: name.to_string(),
                    ..Contact::default()
                });
                self.contacts.len() - 1
            }
        };
        &mut self.contacts[i]
    }

    /// True when the user has checked at least one device (or the person
    /// key) of `name`: the directory shows such a name as checked. Only a
    /// hint; the verdict on a call comes from [`TrustStore::verdict`].
    pub fn name_checked(&self, name: &str) -> bool {
        self.find(name)
            .is_some_and(|c| !c.devices.is_empty() || c.person.is_some())
    }

    /// The verdict for `name`/`label` on a finished handshake.
    pub fn verdict(&self, name: &str, label: &str, s: &Session) -> Trust {
        let Some(c) = self.find(name) else {
            return Trust::NotChecked;
        };
        let dev = hex(&s.peer_device);
        if c.removed.contains(&dev) {
            return Trust::Conflict;
        }
        if c.devices.iter().any(|d| d.key == dev) {
            return Trust::Checked;
        }
        if let (Some(person), Some(cert)) = (&c.person, &s.peer_cert) {
            // The cert was checked by the handshake (signature and device key).
            if *person == hex(&cert.person_pub) && cert.device_pub == s.peer_device {
                return Trust::Checked;
            }
        }
        if c.devices
            .iter()
            .any(|d| d.label.eq_ignore_ascii_case(label) && d.key != dev)
        {
            return Trust::Conflict;
        }
        Trust::NotChecked
    }

    /// The user said the words match: store this device key under `label`
    /// (replacing that label's old key) and, if the peer sent a cert, its
    /// person key. A removed key stays removed.
    pub fn mark_checked(&mut self, name: &str, label: &str, s: &Session) {
        let dev = hex(&s.peer_device);
        let c = self.find_or_add(name);
        if c.removed.contains(&dev) {
            return;
        }
        c.devices
            .retain(|d| !d.label.eq_ignore_ascii_case(label) && d.key != dev);
        c.devices.push(Device {
            label: label.to_string(),
            key: dev,
        });
        if let Some(cert) = &s.peer_cert {
            c.person = Some(hex(&cert.person_pub));
        }
    }

    /// Every contact, for an app's list: name, checked devices (label and
    /// device key), whether a person key is checked, and how many devices
    /// were removed.
    pub fn contacts(&self) -> Vec<ContactView> {
        self.contacts
            .iter()
            .map(|c| ContactView {
                name: c.name.clone(),
                devices: c
                    .devices
                    .iter()
                    .filter_map(|d| Some((d.label.clone(), unhex(&d.key)?)))
                    .collect(),
                person: c.person.is_some(),
                removed: c.removed.len(),
            })
            .collect()
    }

    /// Remove a device from a contact: it is no longer checked, and from now
    /// on it gives [`Trust::Conflict`], even with a valid person cert.
    pub fn remove_device(&mut self, name: &str, device_pub: [u8; 32]) {
        let dev = hex(&device_pub);
        let c = self.find_or_add(name);
        c.devices.retain(|d| d.key != dev);
        if !c.removed.contains(&dev) {
            c.removed.push(dev);
        }
    }
}
