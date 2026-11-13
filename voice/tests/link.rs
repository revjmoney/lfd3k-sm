//! Task 9 tests of linking your own devices with a code
//! (`voice/src/link.rs`), the identity rules it leans on, and the trust a
//! linked device gets, plus the V223.LRA.92 Annex C.5 linking vectors that
//! do not need SPAKE2's random scalars (the SPAKE2 messages themselves need
//! x and y, which the crate draws from the OS; those are taken from the
//! document and everything built on them is checked).
//!
//! Regression tests for review findings (Task 3 and Task 8 carry-ins):
//! `regression_wrong_link_code_reveals_nothing` and
//! `regression_identity_generate_refuses_bad_names`. The engine side ("a
//! link code request before the peer is live is ignored") needs a call and
//! lives in `sim/tests/voice_calls.rs`.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use lfd3k_voice::link::{MSG_LINK_JOIN, MSG_LINK_OFFER, MSG_LINK_REQUEST, MSG_LINK_SEALED};
use lfd3k_voice::{
    new_link_code, Handshake, Identity, LinkError, LinkJoin, LinkOffer, Role, Session, Trust,
    TrustStore,
};
use sha2::{Digest, Sha256};

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn handshake(caller: &Identity, answerer: &Identity) -> (Session, Session) {
    let (mut c, commit) = Handshake::start(caller, Role::Caller);
    let (mut a, hello) = Handshake::start(answerer, Role::Answerer);
    assert_eq!(a.on_peer(&commit), Ok(None));
    let reveal = c.on_peer(&hello).unwrap().expect("reveal");
    let sc = c.finish(&hello).expect("caller finish");
    let sa = a.finish(&reveal).expect("answerer finish");
    (sc, sa)
}

/// Run a whole link: `offer_id` shows `offer_code`, `join_id` types
/// `join_code`. Returns the offer's result (the SEALED message or why not).
fn link(
    offer_id: &Identity,
    offer_code: &str,
    join_id: &mut Identity,
    join_code: &str,
) -> Result<(), LinkError> {
    let (mut j, req) = LinkJoin::start(join_code);
    assert_eq!(req, vec![MSG_LINK_REQUEST, 1]);
    let (o, off) = LinkOffer::start(offer_id, offer_code);
    assert_eq!((off[0], off[1], off.len()), (MSG_LINK_OFFER, 1, 35));
    let join = j.step(&off)?.expect("join message");
    assert_eq!((join[0], join[1], join.len()), (MSG_LINK_JOIN, 1, 67));
    // The same OFFER again is harmless; anything else now is not.
    assert_eq!(j.step(&off), Ok(None));
    assert_eq!(j.step(&join), Err(LinkError::BadMessage));
    let sealed = o.step(&join)?;
    assert_eq!((sealed[0], sealed[1]), (MSG_LINK_SEALED, 1));
    j.finish(&sealed, join_id)
}

#[test]
fn link_with_the_right_code_adopts_the_person_key() {
    let rev_pc = Identity::generate("REV", "PC").unwrap();
    let mut phone = Identity::generate("NEWPHONE", "PHONE").unwrap();
    let own_device = phone.device_pub();
    let code = new_link_code();
    assert_eq!(link(&rev_pc, &code, &mut phone, &code), Ok(()));
    let c = phone.cert().unwrap();
    assert_eq!(
        (c.name.as_str(), c.label.as_str()),
        ("REV", "PHONE"),
        "takes the name, keeps its label"
    );
    assert_eq!(phone.device_pub(), own_device, "keeps its own device key");
    assert_eq!(phone.person_pub(), rev_pc.person_pub());
    assert_eq!(c.person_pub, rev_pc.person_pub().unwrap());
    assert!(c.verify());
    // Saved and loaded, the linked identity is the same.
    let dir = std::env::temp_dir().join(format!("lfd3k-link-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("phone.id");
    phone.save(&p).unwrap();
    let back = Identity::load(&p).unwrap();
    assert_eq!(back.cert(), phone.cert());
    assert_eq!(back.person_pub(), phone.person_pub());
}

#[test]
fn linked_device_trusted_by_existing_contact() {
    let rev_pc = Identity::generate("REV", "PC").unwrap();
    let bob = Identity::generate("BOB", "PHONE").unwrap();
    let mut store = TrustStore::default();
    // Bob checks Rev's PC once (the words matched): its person key is stored.
    let (_, s) = handshake(&rev_pc, &bob);
    store.mark_checked("REV", "PC", &s);
    assert!(store.contacts()[0].person);
    // Rev links a phone; Bob has never seen that device key.
    let mut phone = Identity::generate("TEMP", "PHONE").unwrap();
    let code = new_link_code();
    link(&rev_pc, &code, &mut phone, &code).unwrap();
    let (_, s) = handshake(&phone, &bob);
    assert_eq!(store.verdict("REV", "PHONE", &s), Trust::Checked);
    // An unlinked phone of the same label and name is not.
    let stranger = Identity::generate("REV", "PHONE").unwrap();
    let (_, s) = handshake(&stranger, &bob);
    assert_eq!(store.verdict("REV", "PHONE", &s), Trust::NotChecked);
    // A cert by Rev's person key does not make the phone checked under
    // another name (the cert names REV).
    let (_, s) = handshake(&phone, &bob);
    assert_eq!(store.verdict("ZED", "PHONE", &s), Trust::NotChecked);
}

#[test]
fn wrong_link_code_fails_nothing_sealed() {
    let rev_pc = Identity::generate("REV", "PC").unwrap();
    let mut phone = Identity::generate("TEMP", "PHONE").unwrap();
    let before = phone.cert().cloned();
    let (mut j, _) = LinkJoin::start("123456");
    let (o, off) = LinkOffer::start(&rev_pc, "123457");
    let join = j.step(&off).unwrap().unwrap();
    // The offer checks the joiner's confirm and sends nothing.
    assert_eq!(o.step(&join).err(), Some(LinkError::WrongCode));
    // The joiner never gets a SEALED; a made up one does not change it.
    let mut fake = vec![MSG_LINK_SEALED, 1];
    fake.extend_from_slice(&[0u8; 32 + 32 + 1 + 3 + 16]);
    assert!(j.finish(&fake, &mut phone).is_err());
    assert_eq!(phone.cert().cloned(), before);
    assert_eq!(phone.person_pub(), before.as_ref().map(|c| c.person_pub));
}

#[test]
fn regression_wrong_link_code_reveals_nothing() {
    // A wrong guess gets nothing about the person key: the offer's only
    // message before the confirm is its SPAKE2 message, which does not
    // depend on the person key, and every wrong code fails the same way.
    let rev_pc = Identity::generate("REV", "PC").unwrap();
    let person = rev_pc.person_pub().unwrap();
    for guess in ["000000", "999999", "123455", "654321"] {
        let (o, off) = LinkOffer::start(&rev_pc, "123456");
        assert!(!off.windows(8).any(|w| person.windows(8).any(|p| p == w)));
        let (mut j, _) = LinkJoin::start(guess);
        let join = j.step(&off).unwrap().unwrap();
        assert_eq!(
            o.step(&join).err(),
            Some(LinkError::WrongCode),
            "guess {guess}"
        );
    }
    // A SEALED from a run with the right code does not open for a joiner
    // that used a wrong code (it never had that key).
    let mut good = Identity::generate("TEMP", "PC").unwrap();
    let (mut j_ok, _) = LinkJoin::start("222222");
    let (o_ok, off_ok) = LinkOffer::start(&rev_pc, "222222");
    let join_ok = j_ok.step(&off_ok).unwrap().unwrap();
    let sealed = o_ok.step(&join_ok).unwrap();
    let mut wrong = Identity::generate("TEMP", "PHONE").unwrap();
    let (mut j_bad, _) = LinkJoin::start("222223");
    j_bad.step(&off_ok).unwrap();
    assert_eq!(j_bad.finish(&sealed, &mut wrong), Err(LinkError::WrongCode));
    assert_eq!(wrong.cert().unwrap().name, "TEMP");
    assert_ne!(wrong.person_pub(), Some(person));
    // The right joiner still links with it.
    j_ok.finish(&sealed, &mut good).unwrap();
    assert_eq!(good.person_pub(), Some(person));
    // An offer from a device without a person key gives nothing either.
    let (mut j, _) = LinkJoin::start("333333");
    let lonely = {
        // A linked phone holds the person key; an identity file without one
        // is the case here: load a saved device with the person flag 0.
        let dir = std::env::temp_dir().join(format!("lfd3k-link-np-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("np.id");
        let mut f = b"LFD3KID1".to_vec();
        f.extend_from_slice(&[0x42; 32]);
        f.push(0);
        f.push(0);
        std::fs::write(&p, &f).unwrap();
        Identity::load(&p).unwrap()
    };
    assert!(lonely.person_pub().is_none());
    let (o, off) = LinkOffer::start(&lonely, "333333");
    let join = j.step(&off).unwrap().unwrap();
    assert_eq!(o.step(&join).err(), Some(LinkError::NoPersonKey));
}

#[test]
fn regression_identity_generate_refuses_bad_names() {
    for (name, label) in [
        ("", "PC"),
        ("SEVENTEENCHARSXXX", "PC"),
        ("A:B", "PC"),
        ("TAB\tX", "PC"),
        ("CAF\u{e9}", "PC"),
        ("REV", ""),
        ("REV", "NINECHARS"),
        ("REV", "A/B"),
        ("REV", "A:B"),
        ("REV", "P C\u{7f}"),
    ] {
        assert!(
            Identity::generate(name, label).is_none(),
            "{name:?} {label:?}"
        );
        assert!(!lfd3k_voice::valid_voice_name(name) || !lfd3k_core::voice::valid_label(label));
    }
    for (name, label) in [
        ("R", "P"),
        ("SIXTEENCHARSXXXX", "EIGHTCHR"),
        ("REV J", "MY PC"),
    ] {
        let id = Identity::generate(name, label).unwrap_or_else(|| panic!("{name:?} {label:?}"));
        assert_eq!(id.cert().unwrap().name, name);
    }
    // Rename follows the same rules and changes nothing when it refuses.
    let mut id = Identity::generate("REV", "PC").unwrap();
    assert!(id.rename("A:B", "PC").is_none());
    assert!(id.rename("REV", "A/B").is_none());
    assert_eq!(id.cert().unwrap().name, "REV");
    assert!(id.rename("REVJ", "SHOP").is_some());
    assert_eq!(
        (
            id.cert().unwrap().name.as_str(),
            id.cert().unwrap().label.as_str()
        ),
        ("REVJ", "SHOP")
    );
    assert!(id.cert().unwrap().verify());
}

#[test]
fn link_codes_are_six_digits() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..50 {
        let c = new_link_code();
        assert_eq!(c.len(), 6);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
        seen.insert(c);
    }
    assert!(seen.len() > 40, "codes repeat too often");
    // A joiner given a malformed OFFER fails cleanly.
    let (mut j, _) = LinkJoin::start("123456");
    assert_eq!(
        j.step(&[MSG_LINK_OFFER, 1, 0x41]),
        Err(LinkError::BadMessage)
    );
    assert_eq!(j.step(&[MSG_LINK_OFFER, 2]), Err(LinkError::BadMessage));
}

#[test]
fn v223_lra_92_annex_c5_linking_vectors() {
    let password = b"123456";
    assert_eq!(password.to_vec(), unhex("313233343536"));
    // B.2: OKM = HKDF-SHA256(empty salt, password, "SPAKE2 pw", 48).
    let mut okm = [0u8; 48];
    Hkdf::<Sha256>::new(Some(&[]), password)
        .expand(b"SPAKE2 pw", &mut okm)
        .unwrap();
    assert_eq!(
        okm.to_vec(),
        unhex("96a5d1cbe67aa2840b010592e97aaec909156e76acd0e763ccb169d9ff14dcc6126ebe6d58b6b897385d6b873625d971")
    );
    let offer_pake = unhex("41476b50c887a90d25187c1648180949c9ae2a577095be652aa84a03746589a8f1");
    let join_pake = unhex("42df7e02929c57e1fc80ba50640920c2c239e5db7181cecca2f6b28fd85340a455");
    let enc_k = unhex("2bde6219a726c891fe938238085983c89da6b0958a353620d5fa966493565dad");
    // B.4: the SPAKE2 key.
    let sha = |b: &[u8]| Sha256::digest(b).to_vec();
    let key = Sha256::new()
        .chain_update(sha(password))
        .chain_update(sha(b"LFD3K LINK OFFER"))
        .chain_update(sha(b"LFD3K LINK JOIN"))
        .chain_update(&offer_pake[1..])
        .chain_update(&join_pake[1..])
        .chain_update(&enc_k)
        .finalize()
        .to_vec();
    assert_eq!(
        key,
        unhex("fa0a70c585b8c1f56d981896bc0fffcf31a904a653721124bf6408c5e8f121a4")
    );
    // 15.4: the three keys.
    let hk = Hkdf::<Sha256>::new(Some(b"LFD3K LINK v1"), &key);
    let mut join_mac = [0u8; 32];
    let mut offer_mac = [0u8; 32];
    let mut seal = [0u8; 32];
    hk.expand(b"LFD3K LINK v1 join confirm", &mut join_mac)
        .unwrap();
    hk.expand(b"LFD3K LINK v1 offer confirm", &mut offer_mac)
        .unwrap();
    hk.expand(b"LFD3K LINK v1 seal", &mut seal).unwrap();
    assert_eq!(
        join_mac.to_vec(),
        unhex("1c6e2948964e0a82e2cdd0950652bc0eb7bf76f3003014bf34efa77d43c47e60")
    );
    assert_eq!(
        offer_mac.to_vec(),
        unhex("39c9df21834c672dffecce4fac51608f01422233fb584bcfd8d962408d47eab7")
    );
    assert_eq!(
        seal.to_vec(),
        unhex("ae29ee2028381804c8325b9e7fe1e2b2fc05d6660011d23735072e2749092a95")
    );
    let mut tr = vec![1u8];
    tr.extend_from_slice(&offer_pake);
    tr.extend_from_slice(&join_pake);
    assert_eq!(
        tr,
        unhex("0141476b50c887a90d25187c1648180949c9ae2a577095be652aa84a03746589a8f142df7e02929c57e1fc80ba50640920c2c239e5db7181cecca2f6b28fd85340a455")
    );
    let confirm = |k: &[u8; 32], tag: &[u8]| {
        let mut m = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(k).unwrap();
        m.update(tag);
        m.update(&tr);
        m.finalize().into_bytes().to_vec()
    };
    let join_confirm = confirm(&join_mac, b"JOIN");
    let offer_confirm = confirm(&offer_mac, b"OFFER");
    // The document's "Join confirm" and "Offer confirm" rows read "key" (an
    // editing slip); the values are inside the JOIN and SEALED vectors.
    let mut join = vec![MSG_LINK_JOIN, 1];
    join.extend_from_slice(&join_pake);
    join.extend_from_slice(&join_confirm);
    assert_eq!(
        join,
        unhex("120142df7e02929c57e1fc80ba50640920c2c239e5db7181cecca2f6b28fd85340a455fe5c7baf631d3fdb3dc49a8de02519be71dcbb8c732c369903b68fcacee68785")
    );
    let plain = unhex("222222222222222222222222222222222222222222222222222222222222222203524556");
    let mut aad = vec![MSG_LINK_SEALED, 1];
    aad.extend_from_slice(&tr);
    let ct = ChaCha20Poly1305::new_from_slice(&seal)
        .unwrap()
        .encrypt(
            &Nonce::from([0u8; 12]),
            Payload {
                msg: &plain,
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(
        ct,
        unhex("af1115d95a806a766890205f9caf29e154b4a6254f48242abaf4c7be6da96f00c1ca3d3b07a03b32afabc096ed7e0f29c5a62ab3")
    );
    let mut sealed = vec![MSG_LINK_SEALED, 1];
    sealed.extend_from_slice(&offer_confirm);
    sealed.extend_from_slice(&ct);
    assert_eq!(
        sealed,
        unhex(
            "13011a344f5e3076f718a9020422e39044e1bd23788c904c76ddbc88a1d55bd6\
             6c7baf1115d95a806a766890205f9caf29e154b4a6254f48242abaf4c7be6da9\
             6f00c1ca3d3b07a03b32afabc096ed7e0f29c5a62ab3"
        )
    );
    let mut off = vec![MSG_LINK_OFFER, 1];
    off.extend_from_slice(&offer_pake);
    assert_eq!(
        off,
        unhex("110141476b50c887a90d25187c1648180949c9ae2a577095be652aa84a03746589a8f1")
    );
}

/// Decoded VHP frames in `wire` (COBS frames, each ending in a zero).
fn vhp_frames(wire: &[u8]) -> Vec<Vec<u8>> {
    wire.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            let mut f = [0u8; 300];
            lfd3k_core::voice::host::cobs_decode(p, &mut f).map(|n| f[..n].to_vec())
        })
        .collect()
}

fn vhp(frame: &[u8]) -> Vec<u8> {
    let mut w = [0u8; 300];
    let n = lfd3k_core::voice::host::cobs_encode(frame, &mut w).unwrap();
    w[..n].to_vec()
}

#[test]
fn regression_link_request_before_peer_live_is_ignored() {
    // Task 8a review: the device showing a link code answers a link
    // request only from a live peer (its sealed Hello opened). A request
    // that arrives earlier in the call (here: right after CallState
    // connected, before any handshake) gets no offer, and the code is not
    // spent on it. The engine runs against a scripted modem.
    use lfd3k_core::voice::host::msg;
    let dir = std::env::temp_dir().join(format!("lfd3k-link-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut e = lfd3k_voice::Engine::new(
        Identity::generate("REV", "PC").unwrap(),
        dir.join("trust.json"),
    );
    let code = e.start_link_offer();
    assert_eq!(code.len(), 6);
    e.on_modem_bytes(&vhp(&[msg::HELLO_ACK, 1, 5, b'A', b'L', b'P', b'H', b'A']));
    e.call("REVJ", None);
    // CallState: connected, placed here, SF5, Opus 10k, duplex, 120 ms,
    // max 170, no SNR or RSSI yet, peer ALPHA2 / REVJ / PHONE.
    let mut cs = vec![
        msg::CALL_STATE,
        4,
        2,
        5,
        8,
        1,
        120,
        0,
        170,
        0x00,
        0x80,
        0x00,
        0x80,
    ];
    for s in ["ALPHA2", "REVJ", "PHONE"] {
        cs.push(s.len() as u8);
        cs.extend_from_slice(s.as_bytes());
    }
    e.on_modem_bytes(&vhp(&cs));
    e.tick(1_000);
    let _ = e.take_modem_bytes();
    // The far app's link request arrives as control bytes: length 2,
    // type 0x10, version 1.
    let mut ci = vec![msg::CONTROL_IN];
    ci.extend_from_slice(&lfd3k_voice::Control::LinkMsg(vec![MSG_LINK_REQUEST, 1]).encode());
    e.on_modem_bytes(&vhp(&ci));
    e.tick(1_100);
    let out = vhp_frames(&e.take_modem_bytes());
    let mut rd = lfd3k_voice::ControlReader::new();
    for f in out.iter().filter(|f| f[0] == msg::CONTROL_OUT) {
        rd.push(&f[1..]);
    }
    let mut offers = 0;
    while let Some(m) = rd.next_msg() {
        if matches!(&m, lfd3k_voice::Control::LinkMsg(b) if b.first() == Some(&MSG_LINK_OFFER)) {
            offers += 1;
        }
    }
    assert_eq!(offers, 0, "no link offer before the peer is live: {out:x?}");
    assert!(e.linking(), "the code is not spent");
    let mut code_shown = false;
    while let Some(ev) = e.poll_event() {
        assert!(!matches!(ev, lfd3k_voice::UiEvent::Linked));
        if let lfd3k_voice::UiEvent::LinkCode(c) = ev {
            code_shown = c == code;
        }
    }
    assert!(code_shown);
}
