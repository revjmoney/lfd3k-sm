//! Task 9 tests of frame type 15 and its sub-types (`core/src/voice/frames.rs`
//! and `call.rs`), plus the deterministic test vectors of the V223.LRA
//! drafts in `docs/standards/`: V223.LRA.90 Annex C (type 15 frames, frames
//! to drop, VHP COBS framing), V223.LRA.LINK Annex C (call and discovery
//! frames, frames to drop) and V223.LRA.CMPRS Annex B (LZ decoding and the
//! reference encoder's output).
//!
//! Not reachable here: the Data and Listener body builders and parser of
//! the voice scheduler are crate private (`voice::sched`), so the C.1 Data
//! and Listener vectors are checked as frames (`decode_voice` gives the
//! sub-type, header and body), and the C.2 "Listener without CHUNK with a
//! trailing byte" and "Decline with a body" rows (refused by the scheduler
//! and the call setup, not by the frame decoder) are covered by the on-air
//! tests in `sim/tests`. The `voice_decode_never_panics` and
//! `vhp_cobs_roundtrip_and_garbage` property tests are in
//! `sim/tests/voice_props.rs`, where proptest is (core has no dev
//! dependencies).

use lfd3k_core::discovery::{
    beacon_payload, scan_req_payload, scan_resp_payload, Beacon, ScanReq, ScanResp,
};
use lfd3k_core::frame::{
    self, call_req_payload, FrameError, FrameType, Header, CAP_LZ, FLAG_VOICE, MAX_PAYLOAD,
};
use lfd3k_core::lz::{compress, decompress, LzError};
use lfd3k_core::voice::call::{
    encode_answer, encode_decline, encode_req, parse_answer, parse_req, split_train, tierset_token,
    train_payload, VoiceReq,
};
use lfd3k_core::voice::frames::{
    parse_voice_beacon, parse_voice_scan_resp, voice_beacon_body, voice_scan_resp_body,
};
use lfd3k_core::voice::host::{cobs_decode, cobs_encode};
use lfd3k_core::voice::{decode_voice, encode_voice, Name16, VSub, VoiceReg, VOICE_REGS};

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn regs(items: &[(&str, &str)]) -> [Option<VoiceReg>; VOICE_REGS] {
    let mut r = [None; VOICE_REGS];
    for (i, (n, l)) in items.iter().enumerate() {
        r[i] = Some(VoiceReg {
            name: Name16::new(n).unwrap(),
            label: Name16::label(l).unwrap(),
            session: i as u8,
            seen_ms: 0,
        });
    }
    r
}

fn frame(ftype: FrameType, flags: u8, call_id: u8, seq: u8, ack: u8, payload: &[u8]) -> Vec<u8> {
    let h = Header {
        ftype,
        flags,
        call_id,
        seq,
        ack,
    };
    let mut out = [0u8; 255];
    let n = frame::encode(&h, payload, &mut out).unwrap();
    out[..n].to_vec()
}

#[test]
fn voice_subtypes_roundtrip() {
    for v in 0..=255u8 {
        match VSub::from_u8(v) {
            Some(s) => {
                assert!(v <= 6, "sub-type {v} known");
                assert_eq!(s as u8, v);
            }
            None => assert!(v >= 7, "sub-type {v} must be known"),
        }
    }
    let subs = [
        VSub::Beacon,
        VSub::ScanResp,
        VSub::CallReq,
        VSub::Answer,
        VSub::Decline,
        VSub::Data,
        VSub::Listener,
    ];
    for s in subs {
        for body in [&[][..], &[1, 2, 3][..], &[0xAB; MAX_PAYLOAD - 1][..]] {
            let mut out = [0u8; 255];
            let n = encode_voice(s, 0x2B, 7, body, &mut out).unwrap();
            assert_eq!(n, 5 + 1 + body.len());
            let (got, h, p) = decode_voice(&out[..n]).expect("decodes");
            assert_eq!(
                (got, h.ftype, h.call_id, h.seq, h.flags, h.ack),
                (s, FrameType::Voice, 0x2B, 7, 0, 0)
            );
            assert_eq!(p, body);
        }
        // The sub-type byte takes one byte of the payload.
        let mut out = [0u8; 300];
        assert_eq!(
            encode_voice(s, 1, 0, &[0; MAX_PAYLOAD], &mut out),
            Err(FrameError::TooLong)
        );
    }
    // Other frame types are not voice.
    let f = frame(FrameType::Hangup, 0, 0x2B, 0, 0, &[]);
    assert!(decode_voice(&f).is_none());

    // Discovery bodies.
    let r = regs(&[
        ("REV", "PC"),
        ("BOB", "PHONE"),
        ("SIXTEENCHARSXXXX", "EIGHTCHR"),
        ("Z", "Q"),
    ]);
    let mut b = [0u8; 255];
    let n = voice_beacon_body("AXISMODEM-16FC", &r, &mut b).unwrap();
    let vb = parse_voice_beacon(&b[..n]).unwrap();
    assert_eq!(vb.station, "AXISMODEM-16FC");
    let items: Vec<_> = vb.items.collect();
    assert_eq!(
        items,
        vec![
            ("REV", "PC"),
            ("BOB", "PHONE"),
            ("SIXTEENCHARSXXXX", "EIGHTCHR"),
            ("Z", "Q")
        ]
    );
    assert!(parse_voice_beacon(&b[..n - 1]).is_none(), "cut short");
    let mut long = b[..n].to_vec();
    long.push(0);
    assert!(parse_voice_beacon(&long).is_none(), "trailing byte");
    assert!(voice_beacon_body("", &r, &mut b).is_none());
    let sr = ScanResp {
        name: "AXIS",
        services: 1,
        snr_x4: -22,
        scan_id: 0x5A,
    };
    let n = voice_scan_resp_body(&sr, &regs(&[]), &mut b).unwrap();
    let (got, items) = parse_voice_scan_resp(&b[..n]).unwrap();
    assert_eq!(got, sr);
    assert_eq!(items.count(), 0);

    // Call setup bodies.
    let req = VoiceReq {
        caps: CAP_LZ,
        station: "AXIS",
        name: "REV",
        label: "PC",
        callee: "BOB",
        callee_label: "",
    };
    let n = encode_req(9, &req, &mut b).unwrap();
    let (s, h, p) = decode_voice(&b[..n]).unwrap();
    assert_eq!((s, h.call_id), (VSub::CallReq, 9));
    assert_eq!(parse_req(p), Some(req));
    let bad = VoiceReq {
        callee_label: "A/B",
        ..req
    };
    assert!(parse_req(&p[..p.len() - 1]).is_none());
    let mut scratch = [0u8; 255];
    assert!(encode_req(9, &bad, &mut scratch).is_none());
    let bad = VoiceReq { name: "A:B", ..req };
    assert!(encode_req(9, &bad, &mut scratch).is_none());
    let n = encode_answer(9, 0xDEAD_BEEF, "BRAVO", "PHONE", &mut b).unwrap();
    let (s, _, p) = decode_voice(&b[..n]).unwrap();
    assert_eq!(s, VSub::Answer);
    assert_eq!(parse_answer(p), Some((0xDEAD_BEEF, "BRAVO", "PHONE")));
    assert!(encode_answer(9, 1, "BRAVO", "", &mut b).is_err());
    let n = encode_decline(9, &mut b).unwrap();
    let (s, _, p) = decode_voice(&b[..n]).unwrap();
    assert_eq!((s, p.len()), (VSub::Decline, 0));
    // Train and TierSet with the answer token.
    let mut t = [0u8; 8];
    for base in [&[7u8, 13, 0][..], &[7, 13, 0, 1][..]] {
        let n = train_payload(base, 0x1234_5678, &mut t);
        assert_eq!(split_train(&t[..n]), Some((base, 0x1234_5678)));
    }
    assert_eq!(
        tierset_token(&[5, 0x78, 0x56, 0x34, 0x12]),
        Some(0x1234_5678)
    );
    assert_eq!(tierset_token(&[5]), None);
}

#[test]
fn v223_lra_90_annex_c1_type15_frames() {
    let mut out = [0u8; 255];
    let req = VoiceReq {
        caps: CAP_LZ,
        station: "AXISMODEM-E4B0",
        name: "REV",
        label: "PC",
        callee: "BOB",
        callee_label: "",
    };
    let n = encode_req(0x2B, &req, &mut out).unwrap();
    assert_eq!(
        out[..n].to_vec(),
        hex("D1 F0 2B 00 00 02 01 0E 41 58 49 53 4D 4F 44 45 4D 2D 45 34 42 30 03 52 45 56 02 50 43 03 42 4F 42 00")
    );
    let n = encode_req(
        0x2B,
        &VoiceReq {
            callee_label: "PHONE",
            ..req
        },
        &mut out,
    )
    .unwrap();
    assert_eq!(
        out[..n].to_vec(),
        hex("D1 F0 2B 00 00 02 01 0E 41 58 49 53 4D 4F 44 45 4D 2D 45 34 42 30 03 52 45 56 02 50 43 03 42 4F 42 05 50 48 4F 4E 45")
    );
    let n = encode_answer(0x2B, 0x1234_5678, "AXISMODEM-16FC", "PHONE", &mut out).unwrap();
    assert_eq!(
        out[..n].to_vec(),
        hex("D1 F0 2B 00 00 03 78 56 34 12 0E 41 58 49 53 4D 4F 44 45 4D 2D 31 36 46 43 05 50 48 4F 4E 45")
    );
    let n = encode_decline(0x2B, &mut out).unwrap();
    assert_eq!(out[..n].to_vec(), hex("D1 F0 2B 00 00 04"));
    let mut t = [0u8; 8];
    let n = train_payload(&[7, 13, 0, CAP_LZ], 0x1234_5678, &mut t);
    let train = frame(FrameType::Train, 0, 0x2B, 0, 0, &t[..n]);
    assert_eq!(train, hex("D1 40 2B 00 00 07 0D 00 01 78 56 34 12"));
    assert!(
        frame::decode(&train).is_ok(),
        "the voice Train passes the frame decoder"
    );
    let ts = frame(
        FrameType::TierSet,
        0,
        0x2B,
        0,
        0,
        &[5, 0x78, 0x56, 0x34, 0x12],
    );
    assert_eq!(ts, hex("D1 80 2B 00 00 05 78 56 34 12"));
    assert_eq!(
        tierset_token(frame::decode(&ts).unwrap().payload),
        Some(0x1234_5678)
    );
    let r = regs(&[("REV", "PC"), ("BOB", "PHONE")]);
    let mut body = [0u8; 255];
    let n = voice_beacon_body("AXISMODEM-16FC", &r, &mut body).unwrap();
    let n = encode_voice(VSub::Beacon, 0, 0, &body[..n], &mut out).unwrap();
    assert_eq!(
        out[..n].to_vec(),
        hex("D1 F0 00 00 00 00 0E 41 58 49 53 4D 4F 44 45 4D 2D 31 36 46 43 02 03 52 45 56 02 50 43 03 42 4F 42 05 50 48 4F 4E 45")
    );
    let sr = ScanResp {
        name: "AXIS",
        services: 0x01,
        snr_x4: -22,
        scan_id: 0x5A,
    };
    let n = voice_scan_resp_body(&sr, &r, &mut body).unwrap();
    let n = encode_voice(VSub::ScanResp, 0, 0, &body[..n], &mut out).unwrap();
    assert_eq!(
        out[..n].to_vec(),
        hex("D1 F0 00 00 00 01 04 41 58 49 53 01 EA FF 5A 02 03 52 45 56 02 50 43 03 42 4F 42 05 50 48 4F 4E 45")
    );
    // Data and Listener: as frames (their body codec is crate private).
    let data = hex("D1 F0 2B 09 04 05 17 02 00 AA BB CC DD");
    let (s, h, p) = decode_voice(&data).unwrap();
    assert_eq!((s, h.call_id, h.seq, h.ack), (VSub::Data, 0x2B, 9, 4));
    assert_eq!(p, &hex("17 02 00 AA BB CC DD")[..]);
    let lis = hex("D1 F0 2B 09 02 06 0B 05 00 0D 00 03 48 49");
    let (s, h, p) = decode_voice(&lis).unwrap();
    assert_eq!((s, h.seq, h.ack), (VSub::Listener, 9, 2));
    assert_eq!(p, &hex("0B 05 00 0D 00 03 48 49")[..]);
}

#[test]
fn v223_lra_90_annex_c2_frames_to_drop() {
    // Sub-type 7 (reserved): the frame decoder drops it, as an old build
    // drops every type 15 frame.
    assert_eq!(
        frame::decode(&hex("D1 F0 2B 00 00 07")).err(),
        Some(FrameError::BadType)
    );
    assert!(decode_voice(&hex("D1 F0 2B 00 00 07")).is_none());
    // A voice beacon with a count over 4.
    let b = hex("D1 F0 00 00 00 00 04 41 58 49 53 05 03 52 45 56 02 50 43");
    let (s, _, p) = decode_voice(&b).unwrap();
    assert_eq!(s, VSub::Beacon);
    assert!(parse_voice_beacon(p).is_none());
    // Decline with a body: the frame is type 15, but a Decline is exactly
    // the sub-type byte (checked where it is acted on, `sim/tests`).
    let decline = hex("D1 F0 2B 00 00 04 00");
    let (s, _, p) = decode_voice(&decline).unwrap();
    assert_eq!((s, p), (VSub::Decline, &[0u8][..]));
}

#[test]
fn v223_lra_90_annex_c3_vhp_cobs() {
    let rows: [(&str, &str); 8] = [
        ("01 01", "03 01 01 00"),
        ("0B", "02 0B 00"),
        ("0E", "02 0E 00"),
        ("02 03 52 45 56 02 50 43", "09 02 03 52 45 56 02 50 43 00"),
        ("04 03 42 4F 42 00", "06 04 03 42 4F 42 01 00"),
        ("81 01 04 41 58 49 53", "08 81 01 04 41 58 49 53 00"),
        ("08 00 00 11", "02 08 01 02 11 00"),
        (
            "83 04 03 07 04 01 C8 00 50 0D 00 BA FF 04 41 58 49 53 03 42 4F 42 05 50 48 4F 4E 45",
            "08 83 04 03 07 04 01 C8 03 50 0D 12 BA FF 04 41 58 49 53 03 42 4F 42 05 50 48 4F 4E 45 00",
        ),
    ];
    for (dec, wire) in rows {
        let (d, w) = (hex(dec), hex(wire));
        let mut out = [0u8; 300];
        let n = cobs_encode(&d, &mut out).unwrap();
        assert_eq!(out[..n].to_vec(), w, "{dec}");
        // Decoding takes the bytes before the zero delimiter.
        let mut back = [0u8; 300];
        let m = cobs_decode(&w[..w.len() - 1], &mut back).unwrap();
        assert_eq!(back[..m].to_vec(), d, "{wire}");
    }
    // 254 bytes 01..FE: a full block, then an empty final block.
    let d: Vec<u8> = (1..=254u8).collect();
    let mut out = [0u8; 300];
    let n = cobs_encode(&d, &mut out).unwrap();
    assert_eq!(n, 257);
    assert_eq!((out[0], out[255], out[256]), (0xFF, 0x01, 0x00));
    assert_eq!(&out[1..255], &d[..]);
    // 255 bytes 09, 01..FE.
    let mut d = vec![9u8];
    d.extend(1..=254u8);
    let n = cobs_encode(&d, &mut out).unwrap();
    assert_eq!(n, 258);
    assert_eq!(
        (out[0], out[1], out[254], out[255], out[256], out[257]),
        (0xFF, 0x09, 0xFD, 0x02, 0xFE, 0x00)
    );
    let mut back = [0u8; 300];
    let m = cobs_decode(&out[..n - 1], &mut back).unwrap();
    assert_eq!(back[..m].to_vec(), d);
}

#[test]
fn v223_lra_link_annex_c_frames() {
    let mut p = [0u8; 64];
    let n = call_req_payload("CALLER", "AXIS", &mut p).unwrap();
    let req = frame(FrameType::CallReq, 0, 0x2B, 0, 0, &p[..n]);
    assert_eq!(
        req,
        hex("D1 10 2B 00 00 06 43 41 4C 4C 45 52 04 41 58 49 53")
    );
    let mut lz = p[..n].to_vec();
    lz.push(CAP_LZ);
    assert_eq!(
        frame(FrameType::CallReq, 0, 0x2B, 0, 0, &lz),
        hex("D1 10 2B 00 00 06 43 41 4C 4C 45 52 04 41 58 49 53 01")
    );
    let rows: [(FrameType, u8, u8, u8, &str, &str); 12] = [
        (FrameType::CallAck, 0, 0, 0, "01", "D1 20 2B 00 00 01"),
        (FrameType::CallAck, 0, 0, 0, "", "D1 20 2B 00 00"),
        (FrameType::CallReject, 0, 0, 0, "", "D1 30 2B 00 00"),
        (
            FrameType::Train,
            0,
            0,
            0,
            "07 0D 00 01",
            "D1 40 2B 00 00 07 0D 00 01",
        ),
        (
            FrameType::Train,
            0,
            0,
            0,
            "07 EA FF",
            "D1 40 2B 00 00 07 EA FF",
        ),
        (FrameType::TierSet, 0, 0, 0, "05", "D1 80 2B 00 00 05"),
        (FrameType::TierAck, 0, 0, 0, "05", "D1 90 2B 00 00 05"),
        (
            FrameType::Keepalive,
            2,
            0,
            0,
            "0D 00",
            "D1 B2 2B 00 00 0D 00",
        ),
        (
            FrameType::Keepalive,
            0,
            0,
            0,
            "0D 00",
            "D1 B0 2B 00 00 0D 00",
        ),
        (
            FrameType::Data,
            1,
            3,
            0,
            "48 45 4C 4C 4F",
            "D1 61 2B 03 00 48 45 4C 4C 4F",
        ),
        (FrameType::Ack, 0, 0, 4, "05", "D1 70 2B 00 04 05"),
        (FrameType::Hangup, 0, 0, 0, "", "D1 A0 2B 00 00"),
    ];
    for (t, flags, seq, ack, payload, want) in rows {
        let f = frame(t, flags, 0x2B, seq, ack, &hex(payload));
        assert_eq!(f, hex(want), "{t:?}");
        let d = frame::decode(&f).unwrap_or_else(|e| panic!("{t:?}: {e:?}"));
        assert_eq!(
            (d.header.ftype, d.header.flags, d.header.seq, d.header.ack),
            (t, flags, seq, ack)
        );
        assert_eq!(d.payload, &hex(payload)[..]);
    }
    // C.3 discovery.
    let mut b = [0u8; 64];
    let n = beacon_payload(
        &Beacon {
            name: "AXISMODEM-16FC",
            services: 0x0E,
            heard: 3,
            time_quality: 0,
        },
        &mut b,
    )
    .unwrap();
    assert_eq!(
        frame(FrameType::Beacon, 0, 0, 0, 0, &b[..n]),
        hex("D1 C0 00 00 00 0E 41 58 49 53 4D 4F 44 45 4D 2D 31 36 46 43 0E 03 00")
    );
    let n = scan_req_payload(
        &ScanReq {
            name: "CALLER",
            scan_id: 0x5A,
        },
        &mut b,
    )
    .unwrap();
    let sreq = frame(FrameType::ScanReq, FLAG_VOICE, 0, 0, 0, &b[..n]);
    assert_eq!(sreq, hex("D1 D8 00 00 00 06 43 41 4C 4C 45 52 5A"));
    assert_eq!(frame::decode(&sreq).unwrap().header.flags, FLAG_VOICE);
    let n = scan_resp_payload(
        &ScanResp {
            name: "AXIS",
            services: 1,
            snr_x4: -22,
            scan_id: 0x5A,
        },
        &mut b,
    )
    .unwrap();
    assert_eq!(
        frame(FrameType::ScanResp, 0, 0, 0, 0, &b[..n]),
        hex("D1 E0 00 00 00 04 41 58 49 53 01 EA FF 5A")
    );
}

#[test]
fn v223_lra_link_annex_c4_frames_to_drop() {
    let rows: [(&str, FrameError); 10] = [
        ("7E 10 2B 00 00", FrameError::Foreign),
        ("D1 10 2B 00", FrameError::TooShort),
        ("D1 00 2B 00 00", FrameError::BadType),
        ("D1 F0 2B 00 00", FrameError::BadType),
        ("D1 40 2B 00 00 07 0D", FrameError::BadPayload),
        ("D1 40 2B 00 00 08 0D 00", FrameError::BadPayload),
        ("D1 40 2B 00 00 07 0D 00 11", FrameError::BadPayload),
        (
            "D1 10 2B 00 00 06 43 41 4C 4C 45 52 04 41 58 49 53 01 00",
            FrameError::BadPayload,
        ),
        ("D1 70 2B 00 04", FrameError::BadPayload),
        (
            "D1 C0 00 00 00 04 41 58 49 53 01 00",
            FrameError::BadPayload,
        ),
    ];
    for (bytes, why) in rows {
        assert_eq!(frame::decode(&hex(bytes)).err(), Some(why), "{bytes}");
    }
}

#[test]
fn v223_lra_cmprs_annex_b_vectors() {
    let mut out = [0u8; 200];
    let mut d = |s: &str| decompress(&hex(s), &mut out).map(|n| out[..n].to_vec());
    let mut v1 = vec![0xC9u8];
    v1.extend([0xCD; 40]);
    v1.extend([0xBB, 0x0D, 0x0A]);
    assert_eq!(d("80 B4 FC 00 84 00 02 BB 0D 0A"), Ok(v1.clone()));
    assert_eq!(d("00 41 FC 00 D8 00"), Ok(vec![0x41; 60]));
    let ansi = b"\x1b[0m\x1b[2J\x1b[H\x1b[1;37;44m Welcome to AXIS \x1b[0m\r\n\r\n".to_vec();
    assert_eq!(ansi.len(), 46);
    assert_eq!(
        d("AA 36 96 20 A4 20 04 41 58 49 53 20 84 25 85 39"),
        Ok(ansi.clone())
    );
    assert_eq!(d("82 36"), Ok(vec![0x1B, 0x5B, 0x30]));
    assert_eq!(d("82 37"), Err(LzError::BadDistance));
    assert_eq!(d("03 41 42"), Err(LzError::Truncated));
    assert_eq!(d("80"), Err(LzError::Truncated));
    assert_eq!(d("00 41 83 FF"), Err(LzError::BadDistance));
    assert_eq!(
        d("00 41 FC 00 FC 00 FC 00 FC 00 FC 00 FC 00 FC"),
        Err(LzError::Overflow)
    );
    assert_eq!(d(""), Ok(vec![]));
    // B.2: the reference encoder's output.
    let mut c = [0u8; 200];
    for (input, want) in [
        (&ansi, "AA 36 96 20 A4 20 04 41 58 49 53 20 84 25 85 39"),
        (&v1, "80 B4 FC 00 84 00 02 BB 0D 0A"),
        (&vec![0x41; 60], "00 41 FC 00 D8 00"),
    ] {
        let n = compress(input, &mut c).expect("compresses");
        assert_eq!(c[..n].to_vec(), hex(want));
    }
    assert_eq!(
        compress(b"The quick brown fox jumps over the lazy dog.", &mut c),
        None
    );
}
