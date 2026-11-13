//! Task 9 tests of the FLOWTRON SCRAMBLER (`voice/src/scrambler.rs`,
//! `identity.rs`, `trust.rs`): the crypto crates against their RFC vectors,
//! the handshake, sealed frames and their replay window, hello checks, the
//! trust verdicts, and the V223.LRA.92 Annex C vectors that are reachable
//! through the public API.
//!
//! Annex C vectors: the certs and public keys (fixed seeds, through an
//! identity file and `Identity::load`), the commit, both fixed hellos (the
//! reference handshake accepts them in both roles) and the words are checked
//! against the reference. The values that need a fixed ephemeral secret
//! (shared secret, session keys, words hash, sealed frames) cannot be fed
//! to the reference (`EphemeralSecret::random`), so they are rebuilt here
//! from the part's formulas with the same RFC primitives and compared with
//! the document.

use std::path::PathBuf;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use lfd3k_voice::scrambler::{MSG_COMMIT, MSG_HELLO, MSG_REVEAL};
use lfd3k_voice::{Handshake, Identity, Role, ScrambleError, Session, Trust, TrustStore};
use sha2::{Digest, Sha256};

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn h32(s: &str) -> [u8; 32] {
    unhex(s).try_into().unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lfd3k-voice-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A full handshake: (caller session, answerer session).
fn handshake(caller: &Identity, answerer: &Identity) -> (Session, Session) {
    let (mut c, commit) = Handshake::start(caller, Role::Caller);
    let (mut a, hello) = Handshake::start(answerer, Role::Answerer);
    assert_eq!(a.on_peer(&commit), Ok(None));
    let reveal = c.on_peer(&hello).unwrap().expect("reveal");
    let sc = c.finish(&hello).expect("caller finish");
    let sa = a.finish(&reveal).expect("answerer finish");
    (sc, sa)
}

/// An identity file with fixed seeds (the `LFD3KID1` layout of
/// `Identity::save`), loaded through `Identity::load`.
fn fixed_identity(
    dir: &str,
    device: [u8; 32],
    person: [u8; 32],
    name: &str,
    label: &str,
) -> Identity {
    let d = SigningKey::from_bytes(&device);
    let p = SigningKey::from_bytes(&person);
    let dpub = d.verifying_key().to_bytes();
    let mut signed = b"LFD3K CERT v1".to_vec();
    signed.push(32);
    signed.extend_from_slice(&dpub);
    signed.push(name.len() as u8);
    signed.extend_from_slice(name.as_bytes());
    signed.push(label.len() as u8);
    signed.extend_from_slice(label.as_bytes());
    let sig = p.sign(&signed).to_bytes();
    let mut f = b"LFD3KID1".to_vec();
    f.extend_from_slice(&device);
    f.push(1);
    f.extend_from_slice(&person);
    f.push(1);
    f.extend_from_slice(&p.verifying_key().to_bytes());
    f.push(name.len() as u8);
    f.extend_from_slice(name.as_bytes());
    f.push(label.len() as u8);
    f.extend_from_slice(label.as_bytes());
    f.extend_from_slice(&sig);
    let path = tmp(dir).join("id.bin");
    std::fs::write(&path, &f).unwrap();
    Identity::load(&path).expect("identity file loads")
}

#[test]
fn rfc_vectors_x25519_ed25519_chacha() {
    // RFC 7748 5.2, the first X25519 vector.
    let k = h32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
    let u = h32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
    assert_eq!(
        x25519_dalek::x25519(k, u),
        h32("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
    );
    // RFC 7748 6.1, Diffie-Hellman.
    let a = h32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = h32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let base = x25519_dalek::X25519_BASEPOINT_BYTES;
    assert_eq!(
        x25519_dalek::x25519(a, base),
        h32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
    );
    assert_eq!(
        x25519_dalek::x25519(b, base),
        h32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
    );
    let shared = h32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    assert_eq!(
        x25519_dalek::x25519(a, x25519_dalek::x25519(b, base)),
        shared
    );
    assert_eq!(
        x25519_dalek::x25519(b, x25519_dalek::x25519(a, base)),
        shared
    );

    // RFC 8032 7.1, tests 1 and 2.
    for (sk, pk, msg, sig) in [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
    ] {
        let key = SigningKey::from_bytes(&h32(sk));
        assert_eq!(key.verifying_key().to_bytes(), h32(pk));
        let s = key.sign(&unhex(msg));
        assert_eq!(s.to_bytes().to_vec(), unhex(sig));
        assert!(key.verifying_key().verify_strict(&unhex(msg), &s).is_ok());
    }

    // RFC 8439 2.8.2, the AEAD vector.
    let key: Vec<u8> = (0x80..=0x9f).collect();
    let nonce: [u8; 12] = unhex("070000004041424344454647").try_into().unwrap();
    let aad = unhex("50515253c0c1c2c3c4c5c6c7");
    let plain = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let want = unhex(
        "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
         3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
         92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
         3ff4def08e4b7a9de576d26586cec64b6116\
         1ae10b594f09e26a7e902ecbd0600691",
    );
    let c = ChaCha20Poly1305::new_from_slice(&key).unwrap();
    let ct = c
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plain,
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(ct, want);
    let back = c
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &ct,
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(back, plain.to_vec());
}

#[test]
fn both_ends_same_words() {
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..8 {
        let (mut c, mut a) = handshake(&rev, &bob);
        assert_eq!(c.words, a.words);
        assert!(lfd3k_voice::words::EVEN.contains(&c.words[0]));
        assert!(lfd3k_voice::words::ODD.contains(&c.words[1]));
        seen.insert(c.words);
        // Each knows the other's device and cert.
        assert_eq!(c.peer_device, bob.device_pub());
        assert_eq!(a.peer_device, rev.device_pub());
        assert_eq!(c.peer_cert.as_ref().unwrap().name, "BOB");
        assert_eq!(a.peer_cert.as_ref().unwrap().label, "PC");
        // Frames open both ways.
        let f = c.seal(b"hello bob");
        assert_eq!(a.open(&f).as_deref(), Some(&b"hello bob"[..]));
        let g = a.seal(b"hi rev");
        assert_eq!(c.open(&g).as_deref(), Some(&b"hi rev"[..]));
        // A frame never opens in its own direction.
        let h = c.seal(b"x");
        assert!(c.open(&h).is_none());
    }
    // Fresh ephemeral keys each call: the words change from call to call.
    assert!(seen.len() > 1, "eight calls gave the same words");
}

#[test]
fn tampered_frame_dropped() {
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (mut c, mut a) = handshake(&rev, &bob);
    let f = c.seal(&[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(f.len(), 8 + 20);
    for i in 0..f.len() {
        let mut t = f.clone();
        t[i] ^= 0x40;
        assert!(a.open(&t).is_none(), "byte {i} flipped still opened");
    }
    // Cut short, too short, empty.
    assert!(a.open(&f[..f.len() - 1]).is_none());
    assert!(a.open(&f[..19]).is_none());
    assert!(a.open(&[]).is_none());
    // The real one still opens after all that.
    assert_eq!(a.open(&f).unwrap(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    // Keys from another call do not open it.
    let (mut c2, mut a2) = handshake(&rev, &bob);
    let g = c2.seal(b"call two");
    assert!(a.open(&g).is_none());
    assert!(a2.open(&g).is_some());
}

#[test]
fn replayed_frame_dropped() {
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (mut c, mut a) = handshake(&rev, &bob);
    let f0 = c.seal(b"zero");
    let f1 = c.seal(b"one");
    let f2 = c.seal(b"two");
    assert!(a.open(&f1).is_some());
    assert!(a.open(&f1).is_none(), "repeat of 1");
    assert!(a.open(&f0).is_some(), "0 late but inside the window");
    assert!(a.open(&f0).is_none(), "repeat of 0");
    assert!(a.open(&f2).is_some());
    assert!(a.open(&f2).is_none());
    assert!(a.open(&f1).is_none());
}

#[test]
fn old_counter_outside_window_dropped() {
    // V223.LRA.92 Annex C.4: highest 100 drops counter 36 and accepts 37 once.
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (mut c, mut a) = handshake(&rev, &bob);
    let frames: Vec<Vec<u8>> = (0..=100u32).map(|i| c.seal(&i.to_le_bytes())).collect();
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(
            u32::from_le_bytes(f[..4].try_into().unwrap()),
            i as u32,
            "counter on the frame"
        );
    }
    assert!(a.open(&frames[100]).is_some());
    assert!(
        a.open(&frames[36]).is_none(),
        "100 - 64: outside the window"
    );
    assert!(a.open(&frames[0]).is_none());
    assert!(
        a.open(&frames[37]).is_some(),
        "the oldest inside the window"
    );
    assert!(a.open(&frames[37]).is_none(), "once");
    assert!(a.open(&frames[99]).is_some());
}

#[test]
fn bad_hello_signature_rejected() {
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (_, hello) = Handshake::start(&bob, Role::Answerer);
    assert_eq!(hello[0], MSG_HELLO);
    let try_hello = |m: &[u8]| {
        let (mut c, _) = Handshake::start(&rev, Role::Caller);
        c.on_peer(m).map(|_| ())
    };
    assert_eq!(try_hello(&hello), Ok(()));
    // The ephemeral key changed in flight: the device signature fails.
    let mut t = hello.clone();
    t[3] ^= 1;
    assert_eq!(try_hello(&t), Err(ScrambleError::BadSignature));
    // The signature itself changed.
    let mut t = hello.clone();
    let n = t.len();
    t[n - 5] ^= 1;
    assert_eq!(try_hello(&t), Err(ScrambleError::BadSignature));
    // Shape: cut short, version, role, wrong type byte.
    assert_eq!(
        try_hello(&hello[..hello.len() - 1]),
        Err(ScrambleError::BadHello)
    );
    let mut t = hello.clone();
    t[1] = 2;
    assert_eq!(try_hello(&t), Err(ScrambleError::BadHello));
    let mut t = hello.clone();
    t[2] = 0; // says caller
    assert_eq!(try_hello(&t), Err(ScrambleError::BadHello));
    let mut t = hello.clone();
    t[0] = MSG_REVEAL;
    assert_eq!(try_hello(&t), Err(ScrambleError::BadHello));

    // A hello correctly signed by its device key that carries someone
    // else's cert: BadCert.
    let dev = SigningKey::from_bytes(&[0x5a; 32]);
    let cert = bob.cert().unwrap();
    let mut body = vec![1u8, 1];
    body.extend_from_slice(
        x25519_dalek::PublicKey::from(&x25519_dalek::EphemeralSecret::random()).as_bytes(),
    );
    body.extend_from_slice(&dev.verifying_key().to_bytes());
    body.push(1);
    body.extend_from_slice(&cert.person_pub);
    body.push(cert.name.len() as u8);
    body.extend_from_slice(cert.name.as_bytes());
    body.push(cert.label.len() as u8);
    body.extend_from_slice(cert.label.as_bytes());
    body.extend_from_slice(&cert.sig);
    let sig = dev.sign(&body).to_bytes();
    body.extend_from_slice(&sig);
    let mut m = vec![MSG_HELLO];
    m.extend_from_slice(&body);
    assert_eq!(try_hello(&m), Err(ScrambleError::BadCert));

    // The answerer side: a reveal that does not match the commit mutes.
    let (mut a, _) = Handshake::start(&bob, Role::Answerer);
    let (_, commit) = Handshake::start(&rev, Role::Caller);
    assert_eq!(commit[0], MSG_COMMIT);
    assert_eq!(a.on_peer(&commit), Ok(None));
    let (mut c2, _) = Handshake::start(&rev, Role::Caller);
    let (_, bob_hello) = Handshake::start(&bob, Role::Answerer);
    let wrong_reveal = c2.on_peer(&bob_hello).unwrap().unwrap();
    assert_eq!(
        a.finish(&wrong_reveal).err(),
        Some(ScrambleError::BadCommit)
    );
}

#[test]
fn squatter_gets_not_checked_no_audio() {
    let dir = tmp("squatter");
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let mut store = TrustStore::load(&dir.join("trust.json")).unwrap();
    let (_, s) = handshake(&rev, &bob);
    store.mark_checked("REV", "PC", &s);
    // Someone else takes the name REV on a new label, with their own person
    // key: not the person Bob checked, so not checked (audio stays held).
    let squat = Identity::generate("REV", "PHONE").unwrap();
    let (_, s) = handshake(&squat, &bob);
    assert_eq!(store.verdict("REV", "PHONE", &s), Trust::NotChecked);
    assert_ne!(store.verdict("REV", "PHONE", &s), Trust::Checked);
    // The directory hint says REV was checked, but the call is by key.
    assert!(store.name_checked("REV"));
    // A name nobody checked is NotChecked too.
    let (_, s) = handshake(&squat, &bob);
    assert_eq!(store.verdict("ZED", "PC", &s), Trust::NotChecked);
}

#[test]
fn changed_device_key_is_conflict() {
    let dir = tmp("conflict");
    let path = dir.join("trust.json");
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let mut store = TrustStore::default();
    let (_, s) = handshake(&rev, &bob);
    store.mark_checked("rev", "pc", &s);
    store.save(&path).unwrap();
    let store = TrustStore::load(&path).unwrap();
    // Same device: still checked after a save and load (names any case).
    let (_, s) = handshake(&rev, &bob);
    assert_eq!(store.verdict("REV", "PC", &s), Trust::Checked);
    // REV/PC now shows a different key (and a different person key).
    let other = Identity::generate("REV", "PC").unwrap();
    let (_, s) = handshake(&other, &bob);
    assert_eq!(store.verdict("REV", "PC", &s), Trust::Conflict);
}

#[test]
fn removed_device_not_checked() {
    let rev_pc = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let mut store = TrustStore::default();
    let (_, s) = handshake(&rev_pc, &bob);
    store.mark_checked("REV", "PC", &s);
    assert_eq!(store.verdict("REV", "PC", &s), Trust::Checked);
    store.remove_device("REV", rev_pc.device_pub());
    let (_, s) = handshake(&rev_pc, &bob);
    // Removed: Conflict, even though its cert is signed by the checked
    // person key, and checking the words again does not bring it back.
    assert_eq!(store.verdict("REV", "PC", &s), Trust::Conflict);
    store.mark_checked("REV", "PC", &s);
    assert_eq!(store.verdict("REV", "PC", &s), Trust::Conflict);
    let v = store.contacts();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].removed, 1);
    assert!(v[0].devices.is_empty());
}

// ---- V223.LRA.92 Annex C.

const CALLER_HELLO: &str = "010038ab664bd86f77d7e66bdd9ae0792913a94fd8b33a1260027e4b46c1f488\
4c67d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c977\
873701a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff93834\
55a4f0035245560250430b4dca034aed4eb90a458f6c4baa34a7b4d2ad8ec181\
b2c741410a20deecdcb50a4b26959dc6411fc5d4dfb065497c31ff9d49b065c1\
346d70b299d38c853d09fdab18c52e8e58d873810efb52d1882cecc3f613d7ed\
c8ae97974866e5f38a198010120c179439e42aa3c0abd77d3468eb1c85c9f4e3\
97637e466bb3da4f8608";
const ANSWERER_HELLO: &str = "0101219e4d800da968d2a5fcb009c784f4746c7138edb9ee4844b739e830b05c\
f42417cb79fb2b4120f2b1ec65e4198d6e08b28e813feb01e4a400839b85e180\
80ce01d759793bbc13a2819a827c76adb6fba8a49aee007f49f2d0992d99b825\
ad2c4803424f420550484f4e459856ac6485a3b4d827e8584f2f01c3e80a481b\
0680201b813fba67f5e5ad6efb269ca5128384a673841b9cb89b014803fcc097\
d81f05cffa631b4def3f2fbe05b80ad6688f5b6313dcf1437bf7de55952270d8\
cbac273c8953964cf4d5bf5c10797d0d69774e835add3b1b86e7ce098df12726\
5e65413227ac85a46589967103";

#[test]
fn v223_lra_92_annex_c_certs_and_keys() {
    let rev = fixed_identity("c1-rev", [0x11; 32], [0x22; 32], "REV", "PC");
    let bob = fixed_identity("c1-bob", [0x33; 32], [0x44; 32], "BOB", "PHONE");
    assert_eq!(
        rev.device_pub(),
        h32("d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737")
    );
    assert_eq!(
        rev.person_pub(),
        Some(h32(
            "a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0"
        ))
    );
    assert_eq!(
        bob.device_pub(),
        h32("17cb79fb2b4120f2b1ec65e4198d6e08b28e813feb01e4a400839b85e18080ce")
    );
    assert_eq!(
        bob.person_pub(),
        Some(h32(
            "d759793bbc13a2819a827c76adb6fba8a49aee007f49f2d0992d99b825ad2c48"
        ))
    );
    assert_eq!(
        x25519_dalek::x25519([0x55; 32], x25519_dalek::X25519_BASEPOINT_BYTES),
        h32("38ab664bd86f77d7e66bdd9ae0792913a94fd8b33a1260027e4b46c1f4884c67")
    );
    assert_eq!(
        x25519_dalek::x25519([0x66; 32], x25519_dalek::X25519_BASEPOINT_BYTES),
        h32("219e4d800da968d2a5fcb009c784f4746c7138edb9ee4844b739e830b05cf424")
    );
    // C.2: the cert message and the cert on the wire (the reference made
    // and checked the cert when it loaded the file).
    let mut msg = b"LFD3K CERT v1".to_vec();
    msg.push(32);
    msg.extend_from_slice(&rev.device_pub());
    msg.extend_from_slice(&[3, b'R', b'E', b'V', 2, b'P', b'C']);
    assert_eq!(
        msg,
        unhex("4c4644334b204345525420763120d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c977873703524556025043")
    );
    for (id, wire) in [
        (&rev, "a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0035245560250430b4dca034aed4eb90a458f6c4baa34a7b4d2ad8ec181b2c741410a20deecdcb50a4b26959dc6411fc5d4dfb065497c31ff9d49b065c1346d70b299d38c853d09"),
        (&bob, "d759793bbc13a2819a827c76adb6fba8a49aee007f49f2d0992d99b825ad2c4803424f420550484f4e459856ac6485a3b4d827e8584f2f01c3e80a481b0680201b813fba67f5e5ad6efb269ca5128384a673841b9cb89b014803fcc097d81f05cffa631b4def3f2fbe05"),
    ] {
        let c = id.cert().unwrap();
        assert!(c.verify());
        let mut w = c.person_pub.to_vec();
        w.push(c.name.len() as u8);
        w.extend_from_slice(c.name.as_bytes());
        w.push(c.label.len() as u8);
        w.extend_from_slice(c.label.as_bytes());
        w.extend_from_slice(&c.sig);
        assert_eq!(w, unhex(wire), "{} cert on the wire", c.name);
    }
}

#[test]
fn v223_lra_92_annex_c_reference_takes_the_fixed_handshake() {
    let rev = fixed_identity("c3-rev", [0x11; 32], [0x22; 32], "REV", "PC");
    let bob = fixed_identity("c3-bob", [0x33; 32], [0x44; 32], "BOB", "PHONE");
    let ch = unhex(CALLER_HELLO);
    let ah = unhex(ANSWERER_HELLO);
    assert_eq!((ch.len(), ah.len()), (234, 237));
    let commit = Sha256::new()
        .chain_update(b"FLOWTRON COMMIT")
        .chain_update(&ch)
        .finalize();
    assert_eq!(
        commit.to_vec(),
        unhex("4fede2a6f4cd6e9e3943db7cc0580be459b5a507bfea33796bf9ab4278c55fa8")
    );
    let mut commit_msg = vec![MSG_COMMIT];
    commit_msg.extend_from_slice(&commit);
    // On the control stream: 2 byte length, then the message.
    let mut on_stream = (commit_msg.len() as u16).to_le_bytes().to_vec();
    on_stream.extend_from_slice(&commit_msg);
    assert_eq!(
        on_stream,
        unhex("2100014fede2a6f4cd6e9e3943db7cc0580be459b5a507bfea33796bf9ab4278c55fa8")
    );

    // A real answerer (Bob's keys, its own ephemeral) takes the fixed
    // commit, then the fixed caller hello as the reveal.
    let (mut a, _) = Handshake::start(&bob, Role::Answerer);
    assert_eq!(a.on_peer(&commit_msg), Ok(None));
    let mut reveal = vec![MSG_REVEAL];
    reveal.extend_from_slice(&ch);
    let s = a
        .finish(&reveal)
        .expect("the reference accepts the fixed caller hello");
    assert_eq!(s.peer_device, rev.device_pub());
    let cert = s.peer_cert.as_ref().unwrap();
    assert_eq!((cert.name.as_str(), cert.label.as_str()), ("REV", "PC"));
    assert_eq!(Some(cert.person_pub), rev.person_pub());

    // A real caller (Rev's keys) takes the fixed answerer hello.
    let (mut c, _) = Handshake::start(&rev, Role::Caller);
    let mut hello = vec![MSG_HELLO];
    hello.extend_from_slice(&ah);
    assert!(c.on_peer(&hello).unwrap().is_some());
    let s = c
        .finish(&[])
        .expect("the reference accepts the fixed answerer hello");
    assert_eq!(s.peer_device, bob.device_pub());
    assert_eq!(s.peer_cert.as_ref().unwrap().name, "BOB");

    // The hellos are exactly what the part's layout gives with these keys.
    for (hello, eph, dev, id) in [
        (&ch, [0x55u8; 32], [0x11u8; 32], &rev),
        (&ah, [0x66; 32], [0x33; 32], &bob),
    ] {
        let c = id.cert().unwrap();
        let mut b = vec![1u8, u8::from(eph[0] == 0x66)];
        b.extend_from_slice(&x25519_dalek::x25519(
            eph,
            x25519_dalek::X25519_BASEPOINT_BYTES,
        ));
        b.extend_from_slice(&id.device_pub());
        b.push(1);
        b.extend_from_slice(&c.person_pub);
        b.push(c.name.len() as u8);
        b.extend_from_slice(c.name.as_bytes());
        b.push(c.label.len() as u8);
        b.extend_from_slice(c.label.as_bytes());
        b.extend_from_slice(&c.sig);
        let sig = SigningKey::from_bytes(&dev).sign(&b).to_bytes();
        b.extend_from_slice(&sig);
        assert_eq!(&b, hello, "{} hello", c.name);
    }
    // The words: W[0] 0xa2 rebirth, W[1] 0xd4 souvenir.
    assert_eq!(
        lfd3k_voice::words::two_words(0xa2, 0xd4),
        ["rebirth", "souvenir"]
    );
}

fn seal(key: &[u8; 32], dir: u8, ctr: u32, plain: &[u8]) -> Vec<u8> {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[8..].copy_from_slice(&ctr.to_le_bytes());
    let c = ChaCha20Poly1305::new_from_slice(key).unwrap();
    let mut out = ctr.to_le_bytes().to_vec();
    out.extend_from_slice(&c.encrypt(&Nonce::from(n), plain).unwrap());
    out
}

#[test]
fn v223_lra_92_annex_c_derived_values() {
    let ch = unhex(CALLER_HELLO);
    let ah = unhex(ANSWERER_HELLO);
    let a_pub = x25519_dalek::x25519([0x66; 32], x25519_dalek::X25519_BASEPOINT_BYTES);
    let shared = x25519_dalek::x25519([0x55; 32], a_pub);
    assert_eq!(
        shared,
        h32("559ed2171b98fa0c0f417b78b5c5a43e971990d7605860038d940284d3245142")
    );
    let mut salt = ch.clone();
    salt.extend_from_slice(&ah);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &shared);
    let mut c2a = [0u8; 32];
    let mut a2c = [0u8; 32];
    hk.expand(b"FLOWTRON SCRAMBLER v1 c2a", &mut c2a).unwrap();
    hk.expand(b"FLOWTRON SCRAMBLER v1 a2c", &mut a2c).unwrap();
    assert_eq!(
        c2a,
        h32("7a17fd90f16e0529ad0806bb6d8b35aad9db13f23899c6610b535fb70598499d")
    );
    assert_eq!(
        a2c,
        h32("8b991beee828883f4abf3d6548f9ffe04aaa0077d03def666766de4d8fcafe63")
    );
    let w = Sha256::new()
        .chain_update(b"FLOWTRON WORDS")
        .chain_update(&ch)
        .chain_update(&ah)
        .finalize();
    assert_eq!(
        w.to_vec(),
        unhex("a2d4b9da744e04a3d0ddb162c1a490a220b0a2a3bfcde79a9b89fb9a28fe98aa")
    );
    assert_eq!(
        lfd3k_voice::words::two_words(w[0], w[1]),
        ["rebirth", "souvenir"]
    );

    // C.4 sealed frames.
    let hello_c = seal(&c2a, 0, 0, lfd3k_voice::control::HELLO_TAG);
    assert_eq!(
        hello_c,
        unhex("0000000018b4575221c800411ce213b4de21ab1aade22edef8644fe5fe88064ed43a924c18")
    );
    let msg = lfd3k_voice::Control::Hello(hello_c.clone()).encode();
    assert_eq!(
        msg,
        unhex("2600200000000018b4575221c800411ce213b4de21ab1aade22edef8644fe5fe88064ed43a924c18")
    );
    assert_eq!(
        seal(&a2c, 1, 0, lfd3k_voice::control::HELLO_TAG),
        unhex("000000003cba648bf609cea03f4cc8cf80150b97ea6c1be59896c932b7d6206b45c27ca087")
    );
    assert_eq!(
        seal(&c2a, 0, 1, &[1, 2, 3, 4, 5, 6]),
        unhex("01000000c372a8cb3d34ab74dd03943674da8f26f4d870413d2c")
    );
    let mute = seal(&a2c, 1, 1, &[1]);
    assert_eq!(mute, unhex("01000000d40351336a813c93cc7f64a7149414e729"));
    assert_eq!(
        lfd3k_voice::Control::Mute(mute).encode(),
        unhex("16002101000000d40351336a813c93cc7f64a7149414e729")
    );
    let removal = lfd3k_voice::control::removal_signed_bytes(&[0x99; 32]);
    assert_eq!(
        removal,
        unhex("4c4644334b2052454d4f56452076319999999999999999999999999999999999999999999999999999999999999999")
    );
    let sig = SigningKey::from_bytes(&[0x22; 32])
        .sign(&removal)
        .to_bytes();
    assert_eq!(
        sig.to_vec(),
        unhex("f22cbfb41b0c6db49b24e1af1a3948140753b27a24205861a283c0ee1c5a54006561566b4d532b689980ad7e2aa45e9af3acb5461a1928263b1c0dc53a299e0e")
    );
    let mut body = vec![0x99u8; 32];
    body.extend_from_slice(&sig);
    assert_eq!(
        seal(&c2a, 0, 2, &body),
        unhex(
            "02000000a14539c12588df4885054cfe8210f3f1a4829357e9432335cd66b7ed\
             5d21169cfcb6687ebb4f766f33959c21c704ebdb9b53b4ced8af77aef66400d3\
             dd90b79a83121253d47dfb7f5d9e1bec1e9d2a2368efe0e2a8626e00b3f4a728\
             820406007236977cfabcf8ed4813e426a9b2d84f"
        )
    );
}

#[test]
fn v223_lra_92_annex_c_reference_session_matches() {
    // The reference handshake itself, run with the document's fixed
    // ephemeral secrets (a test-only seam), gives the document's commit,
    // hellos, words and sealed frames byte for byte.
    let rev = fixed_identity("c4-rev", [0x11; 32], [0x22; 32], "REV", "PC");
    let bob = fixed_identity("c4-bob", [0x33; 32], [0x44; 32], "BOB", "PHONE");
    let (mut c, commit) =
        Handshake::start_with_ephemeral_for_test_vectors(&rev, Role::Caller, [0x55; 32]);
    let (mut a, hello) =
        Handshake::start_with_ephemeral_for_test_vectors(&bob, Role::Answerer, [0x66; 32]);
    assert_eq!(
        commit,
        unhex("014fede2a6f4cd6e9e3943db7cc0580be459b5a507bfea33796bf9ab4278c55fa8")
    );
    assert_eq!(&hello[1..], &unhex(ANSWERER_HELLO)[..]);
    assert_eq!(a.on_peer(&commit), Ok(None));
    let reveal = c.on_peer(&hello).unwrap().unwrap();
    assert_eq!(&reveal[1..], &unhex(CALLER_HELLO)[..]);
    let mut sc = c.finish(&hello).expect("caller session");
    let mut sa = a.finish(&reveal).expect("answerer session");
    assert_eq!(sc.words, ["rebirth", "souvenir"]);
    assert_eq!(sa.words, sc.words);
    let tag = lfd3k_voice::control::HELLO_TAG;
    assert_eq!(
        sc.seal(tag),
        unhex("0000000018b4575221c800411ce213b4de21ab1aade22edef8644fe5fe88064ed43a924c18")
    );
    assert_eq!(
        sa.seal(tag),
        unhex("000000003cba648bf609cea03f4cc8cf80150b97ea6c1be59896c932b7d6206b45c27ca087")
    );
    let v = sc.seal(&[1, 2, 3, 4, 5, 6]);
    assert_eq!(
        v,
        unhex("01000000c372a8cb3d34ab74dd03943674da8f26f4d870413d2c")
    );
    assert_eq!(
        sa.seal(&[1]),
        unhex("01000000d40351336a813c93cc7f64a7149414e729")
    );
    let mut body = vec![0x99u8; 32];
    body.extend_from_slice(
        &SigningKey::from_bytes(&[0x22; 32])
            .sign(&lfd3k_voice::control::removal_signed_bytes(&[0x99; 32]))
            .to_bytes(),
    );
    assert_eq!(
        sc.seal(&body),
        unhex(
            "02000000a14539c12588df4885054cfe8210f3f1a4829357e9432335cd66b7ed\
             5d21169cfcb6687ebb4f766f33959c21c704ebdb9b53b4ced8af77aef66400d3\
             dd90b79a83121253d47dfb7f5d9e1bec1e9d2a2368efe0e2a8626e00b3f4a728\
             820406007236977cfabcf8ed4813e426a9b2d84f"
        )
    );
    // And the answerer opens the caller's vector frames.
    assert_eq!(sa.open(&v), Some(vec![1, 2, 3, 4, 5, 6]));
}

#[test]
fn trust_store_missing_file_is_empty_and_save_is_private() {
    let dir = tmp("trust-io");
    let path = dir.join("trust.json");
    let _ = std::fs::remove_file(&path);
    // No file yet: an empty store, not an error.
    let mut store = TrustStore::load(&path).unwrap();
    assert!(store.contacts().is_empty());
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (_, s) = handshake(&rev, &bob);
    store.mark_checked("REV", "PC", &s);
    // A stale side file is replaced, and none is left behind.
    let mut tmp_side = path.as_os_str().to_owned();
    tmp_side.push(".tmp");
    std::fs::write(&tmp_side, b"stale").unwrap();
    store.save(&path).unwrap();
    assert!(!std::path::Path::new(&tmp_side).exists());
    assert_eq!(TrustStore::load(&path).unwrap().contacts().len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "trust.json is owner only");
    }
}

#[test]
fn trust_store_bad_file_is_kept_and_reported() {
    let dir = tmp("trust-bad");
    let path = dir.join("trust.json");
    let bad = dir.join("trust.json.bad");
    let _ = std::fs::remove_file(&bad);
    std::fs::write(&path, b"{ not json").unwrap();
    let err = TrustStore::load(&path).unwrap_err();
    // Moved aside, contents intact, and the error names where it went.
    assert_eq!(err.kept.as_deref(), Some(bad.as_path()));
    assert!(!path.exists());
    assert_eq!(std::fs::read(&bad).unwrap(), b"{ not json");
    assert!(err.to_string().contains("trust.json.bad"), "{err}");
    // A second bad file does not write over the first one.
    std::fs::write(&path, b"also bad").unwrap();
    let err = TrustStore::load(&path).unwrap_err();
    let kept = err.kept.expect("kept");
    assert_ne!(kept, bad);
    assert_eq!(std::fs::read(&kept).unwrap(), b"also bad");
    assert_eq!(std::fs::read(&bad).unwrap(), b"{ not json");

    // The engine starts with an empty list and says so (UiEvent::Error).
    std::fs::write(&path, b"[1, 2").unwrap();
    let mut e = lfd3k_voice::Engine::new(Identity::generate("REV", "PC").unwrap(), path.clone());
    let mut said = false;
    while let Some(ev) = e.poll_event() {
        if let lfd3k_voice::UiEvent::Error(t) = ev {
            said |= t.contains("checked contacts") && t.contains("kept as");
        }
    }
    assert!(said, "the engine reports the unreadable trust file");
    assert!(e.contacts().is_empty());
    // Three bad files kept, none written over.
    let kept: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with("trust.json.bad")
        })
        .map(|f| std::fs::read(f.path()).unwrap())
        .collect();
    assert_eq!(kept.len(), 3, "three bad files kept");
    assert!(kept.contains(&b"[1, 2".to_vec()));
}

#[test]
fn regression_late_control_message_opens_past_the_voice_window() {
    // A Mute sealed before 100 voice frames comes over ARQ after them: the
    // 64 frame voice window would drop it; the control stream's own
    // in-order check takes it once, and only once.
    let rev = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let (mut tx, mut rx) = handshake(&rev, &bob);
    let one = |p: &[u8]| p.len() == 1;
    let mute = tx.seal(&[1]);
    let voice: Vec<Vec<u8>> = (0..100).map(|i| tx.seal(&[i as u8; 20])).collect();
    let mute2 = tx.seal(&[0]);
    for v in &voice {
        assert!(rx.open(v).is_some());
    }
    // As voice it is too old.
    assert_eq!(rx.open(&mute), None);
    // As control it opens, once.
    assert_eq!(rx.open_control(&mute, one).as_deref(), Some(&[1u8][..]));
    assert_eq!(
        rx.open_control(&mute, one),
        None,
        "a replayed control message"
    );
    // A voice frame inside the window is never taken as control.
    assert_eq!(rx.open_control(voice.last().unwrap(), |_| true), None);
    // The next control message opens; the older one cannot come back.
    assert_eq!(rx.open_control(&mute2, one).as_deref(), Some(&[0u8][..]));
    assert_eq!(rx.open_control(&mute, one), None);
    // And the control message is not a voice frame either.
    assert_eq!(rx.open(&mute2), None);
    // An old voice frame (out of the window, counter above the last control
    // one) fed in as a Mute is not a Mute (20 bytes, not 1): refused, and
    // the control counter does not move on, so the real Mute sealed after
    // it still opens.
    let more: Vec<Vec<u8>> = (0..70).map(|i| tx.seal(&[i as u8; 20])).collect();
    let mute3 = tx.seal(&[1]);
    let old_voice = more.first().unwrap().clone();
    for v in more.iter().skip(1) {
        assert!(rx.open(v).is_some());
    }
    assert!(rx.open(&old_voice).is_none(), "out of the voice window");
    assert_eq!(rx.open_control(&old_voice, one), None);
    assert_eq!(rx.open_control(&mute3, one).as_deref(), Some(&[1u8][..]));
}
