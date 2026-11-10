//! Wire frame format. Header is 5 bytes: byte0 = AIR_ID, byte1 = type << 4 | flags,
//! byte2 = call_id, byte3 = seq, byte4 = ack. A frame whose byte0 is not
//! AIR_ID is somebody else's traffic and decodes as `Foreign`.
//!
//! Types 1 to 11 are call frames; 12 to 14 (Beacon, ScanReq, ScanResp) are
//! discovery frames with call id 0 and payloads laid out in `discovery.rs`.
//! Older builds drop 12 to 14 as `BadType`.

use crate::tier::Tier;

/// First byte of every frame: 0xD = LFD3K-SM, 1 = air protocol version 1.
pub const AIR_ID: u8 = 0xD1;
pub const HEADER_LEN: usize = 5;
pub const MAX_FRAME: usize = 255;
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
pub const NAME_MAX: usize = 16;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameType {
    CallReq = 1,
    CallAck = 2,
    CallReject = 3,
    Train = 4,
    TrainAck = 5,
    Data = 6,
    Ack = 7,
    TierSet = 8,
    TierAck = 9,
    Hangup = 10,
    Keepalive = 11,
    /// Discovery: "here I am" (see `discovery.rs`). Call id 0.
    Beacon = 12,
    /// Discovery: "who is out there?". Call id 0.
    ScanReq = 13,
    /// Discovery: the answer to a ScanReq. Call id 0.
    ScanResp = 14,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<FrameType> {
        Some(match v {
            1 => FrameType::CallReq,
            2 => FrameType::CallAck,
            3 => FrameType::CallReject,
            4 => FrameType::Train,
            5 => FrameType::TrainAck,
            6 => FrameType::Data,
            7 => FrameType::Ack,
            8 => FrameType::TierSet,
            9 => FrameType::TierAck,
            10 => FrameType::Hangup,
            11 => FrameType::Keepalive,
            12 => FrameType::Beacon,
            13 => FrameType::ScanReq,
            14 => FrameType::ScanResp,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    pub ftype: FrameType,
    pub flags: u8,
    pub call_id: u8,
    pub seq: u8,
    pub ack: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameError {
    TooShort,
    BadType,
    TooLong,
    BadPayload,
    /// First byte is not `AIR_ID`: not our traffic.
    Foreign,
}

pub fn encode(header: &Header, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::TooLong);
    }
    let total = HEADER_LEN + payload.len();
    let dst = out.get_mut(..total).ok_or(FrameError::TooShort)?;
    let (head, body) = dst.split_at_mut(HEADER_LEN);
    if let [id, b0, b1, b2, b3] = head {
        *id = AIR_ID;
        *b0 = ((header.ftype as u8) << 4) | (header.flags & 0x0F);
        *b1 = header.call_id;
        *b2 = header.seq;
        *b3 = header.ack;
    }
    body.copy_from_slice(payload);
    Ok(total)
}

pub fn decode(buf: &[u8]) -> Result<Frame<'_>, FrameError> {
    if buf.len() > MAX_FRAME {
        return Err(FrameError::TooLong);
    }
    let (b0, b1, b2, b3, payload) = match buf {
        [] => return Err(FrameError::TooShort),
        [id, ..] if *id != AIR_ID => return Err(FrameError::Foreign),
        [_, b0, b1, b2, b3, rest @ ..] => (*b0, *b1, *b2, *b3, rest),
        _ => return Err(FrameError::TooShort),
    };
    let ftype = FrameType::from_u8(b0 >> 4).ok_or(FrameError::BadType)?;
    let shape_ok = match ftype {
        FrameType::CallReq => parse_call_req(payload).is_ok(),
        FrameType::Train | FrameType::TrainAck => {
            matches!(payload, [t, _, _] if Tier::from_index(*t).is_some())
        }
        FrameType::TierSet | FrameType::TierAck => {
            matches!(payload, [t] if Tier::from_index(*t).is_some())
        }
        FrameType::Ack => payload.len() == 1,
        FrameType::Beacon => crate::discovery::parse_beacon(payload).is_some(),
        FrameType::ScanReq => crate::discovery::parse_scan_req(payload).is_some(),
        FrameType::ScanResp => crate::discovery::parse_scan_resp(payload).is_some(),
        _ => true,
    };
    if !shape_ok {
        return Err(FrameError::BadPayload);
    }
    Ok(Frame {
        header: Header {
            ftype,
            flags: b0 & 0x0F,
            call_id: b1,
            seq: b2,
            ack: b3,
        },
        payload,
    })
}

/// A station name: 1 to 16 printable ASCII characters.
pub(crate) fn valid_name(n: &str) -> bool {
    (1..=NAME_MAX).contains(&n.len()) && n.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

pub fn call_req_payload(caller: &str, callee: &str, out: &mut [u8]) -> Result<usize, FrameError> {
    if !valid_name(caller) || !valid_name(callee) {
        return Err(FrameError::BadPayload);
    }
    let total = 2 + caller.len() + callee.len();
    let dst = out.get_mut(..total).ok_or(FrameError::TooShort)?;
    let (l1, rest) = dst.split_at_mut(1);
    let (n1, rest) = rest.split_at_mut(caller.len());
    let (l2, n2) = rest.split_at_mut(1);
    // Lengths are 1..=NAME_MAX, so the casts are lossless.
    l1.copy_from_slice(&[caller.len() as u8]);
    n1.copy_from_slice(caller.as_bytes());
    l2.copy_from_slice(&[callee.len() as u8]);
    n2.copy_from_slice(callee.as_bytes());
    Ok(total)
}

pub fn parse_call_req(p: &[u8]) -> Result<(&str, &str), FrameError> {
    let (&l1, rest) = p.split_first().ok_or(FrameError::BadPayload)?;
    let (n1, rest) = rest
        .split_at_checked(l1 as usize)
        .ok_or(FrameError::BadPayload)?;
    let (&l2, rest) = rest.split_first().ok_or(FrameError::BadPayload)?;
    if rest.len() != l2 as usize {
        return Err(FrameError::BadPayload);
    }
    let a = core::str::from_utf8(n1).map_err(|_| FrameError::BadPayload)?;
    let b = core::str::from_utf8(rest).map_err(|_| FrameError::BadPayload)?;
    if !valid_name(a) || !valid_name(b) {
        return Err(FrameError::BadPayload);
    }
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [FrameType; 11] = [
        FrameType::CallReq,
        FrameType::CallAck,
        FrameType::CallReject,
        FrameType::Train,
        FrameType::TrainAck,
        FrameType::Data,
        FrameType::Ack,
        FrameType::TierSet,
        FrameType::TierAck,
        FrameType::Hangup,
        FrameType::Keepalive,
    ];

    /// A payload of the right shape for each type.
    fn payload_for(t: FrameType, scratch: &mut [u8; MAX_PAYLOAD]) -> usize {
        match t {
            FrameType::CallReq => call_req_payload("CALLER", "AXIS", scratch).unwrap(),
            FrameType::Train | FrameType::TrainAck => {
                scratch[..3].copy_from_slice(&[7, 0xF0, 0xFF]);
                3
            }
            FrameType::TierSet | FrameType::TierAck => {
                scratch[0] = 3;
                1
            }
            FrameType::Ack => {
                scratch[0] = 0b1010_0101;
                1
            }
            FrameType::Data => {
                for (i, b) in scratch.iter_mut().enumerate() {
                    *b = (i * 7) as u8;
                }
                MAX_PAYLOAD
            }
            _ => {
                scratch[..2].copy_from_slice(&[0x34, 0x12]);
                2
            }
        }
    }

    #[test]
    fn round_trip_every_type() {
        assert_eq!(MAX_PAYLOAD, 250);
        for (i, t) in ALL.iter().enumerate() {
            assert_eq!(*t as u8, i as u8 + 1);
            assert_eq!(FrameType::from_u8(*t as u8), Some(*t));
            let mut p = [0u8; MAX_PAYLOAD];
            let plen = payload_for(*t, &mut p);
            let h = Header {
                ftype: *t,
                flags: 0x0F & (i as u8),
                call_id: 0xA5,
                seq: 200 + i as u8,
                ack: 255 - i as u8,
            };
            let mut out = [0u8; MAX_FRAME];
            let n = encode(&h, &p[..plen], &mut out).unwrap();
            assert_eq!(n, HEADER_LEN + plen);
            assert_eq!(out[0], AIR_ID);
            assert_eq!(out[1] >> 4, *t as u8);
            let f = decode(&out[..n]).unwrap();
            assert_eq!(f.header, h);
            assert_eq!(f.payload, &p[..plen]);
        }
        // Flags above 4 bits never leak into the type nibble.
        let h = Header {
            ftype: FrameType::Data,
            flags: 0xFF,
            call_id: 1,
            seq: 2,
            ack: 3,
        };
        let mut out = [0u8; 8];
        let n = encode(&h, &[], &mut out).unwrap();
        let f = decode(&out[..n]).unwrap();
        assert_eq!(f.header.ftype, FrameType::Data);
        assert_eq!(f.header.flags, 0x0F);
        // Oversized payload and too small an output buffer are errors.
        assert_eq!(
            encode(&h, &[0; MAX_PAYLOAD + 1], &mut [0; 300]),
            Err(FrameError::TooLong)
        );
        assert_eq!(
            encode(&h, &[0; 10], &mut [0; 13]),
            Err(FrameError::TooShort)
        );
    }

    #[test]
    fn rejects_short_and_bad_type() {
        assert_eq!(decode(&[]), Err(FrameError::TooShort));
        assert_eq!(decode(&[0xD1, 0x60]), Err(FrameError::TooShort));
        assert_eq!(decode(&[0xD1, 0x60, 1, 2]), Err(FrameError::TooShort));
        assert_eq!(decode(&[0xD1, 0x00, 1, 2, 3]), Err(FrameError::BadType));
        assert_eq!(decode(&[0xD1, 0xF0, 1, 2, 3]), Err(FrameError::BadType));
        // Type 12 is Beacon since phase 4: a bare header is a bad payload.
        assert_eq!(decode(&[0xD1, 0xC0, 1, 2, 3]), Err(FrameError::BadPayload));
        assert_eq!(decode(&[0x60; MAX_FRAME + 1]), Err(FrameError::TooLong));
        assert_eq!(FrameType::from_u8(0), None);
        assert_eq!(FrameType::from_u8(12), Some(FrameType::Beacon));
        assert_eq!(FrameType::from_u8(15), None);
        // Wrong payload shapes for the types that have one.
        assert_eq!(decode(&[0xD1, 0x80, 1, 2, 3]), Err(FrameError::BadPayload));
        assert_eq!(
            decode(&[0xD1, 0x80, 1, 2, 3, 8]),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            decode(&[0xD1, 0x90, 1, 2, 3, 0, 0]),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            decode(&[0xD1, 0x40, 1, 2, 3, 9, 0, 0]),
            Err(FrameError::BadPayload)
        );
        assert_eq!(decode(&[0xD1, 0x70, 1, 2, 3]), Err(FrameError::BadPayload));
        assert_eq!(
            decode(&[0xD1, 0x10, 1, 2, 3, 0]),
            Err(FrameError::BadPayload)
        );
        // A bare header is a valid Data, Hangup or Keepalive frame.
        assert!(decode(&[0xD1, 0x60, 1, 2, 3]).is_ok());
        assert!(decode(&[0xD1, 0xA0, 1, 2, 3]).is_ok());
    }

    #[test]
    fn call_req_names() {
        let mut p = [0u8; MAX_PAYLOAD];
        let n = call_req_payload("CALLER", "AXIS", &mut p).unwrap();
        assert_eq!(&p[..n], b"\x06CALLER\x04AXIS");
        assert_eq!(parse_call_req(&p[..n]), Ok(("CALLER", "AXIS")));

        let sixteen = "ABCDEFGHIJKLMNOP";
        let n = call_req_payload(sixteen, "a b~!", &mut p).unwrap();
        assert_eq!(parse_call_req(&p[..n]), Ok((sixteen, "a b~!")));

        let seventeen = "ABCDEFGHIJKLMNOPQ";
        assert_eq!(
            call_req_payload(seventeen, "AXIS", &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("AXIS", seventeen, &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("", "AXIS", &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("AX\tIS", "B", &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("A", "B\x7F", &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("A", "\u{e9}", &mut p),
            Err(FrameError::BadPayload)
        );
        assert_eq!(
            call_req_payload("A", "B", &mut [0u8; 3]),
            Err(FrameError::TooShort)
        );

        // Hand-built payloads: 17 chars, non-printable, bad lengths, bad UTF-8.
        let mut long = vec![17u8];
        long.extend_from_slice(seventeen.as_bytes());
        long.extend_from_slice(b"\x01B");
        assert!(parse_call_req(&long).is_err());
        assert!(parse_call_req(b"\x02A\x00\x01B").is_err());
        assert!(parse_call_req(b"\x01A\x01\n").is_err());
        assert!(parse_call_req(b"\x01A\x02B").is_err());
        assert!(parse_call_req(b"\x01A\x01BC").is_err());
        assert!(parse_call_req(b"\x05A").is_err());
        assert!(parse_call_req(b"\x00\x01B").is_err());
        assert!(parse_call_req(b"\x01\xFF\x01B").is_err());
        assert!(parse_call_req(b"").is_err());
        // The same payloads inside a CallReq frame are rejected by decode.
        let mut f = vec![0xD1, 0x10, 1, 0, 0];
        f.extend_from_slice(&long);
        assert_eq!(decode(&f), Err(FrameError::BadPayload));
    }

    #[test]
    fn foreign_air_id_rejected() {
        // 0x94 and 0x00 are somebody else's traffic, not ours.
        for id in [0x94u8, 0x00, 0xD0, 0xD2, 0xFF] {
            let f = [id, 0x60, 1, 2, 3];
            assert_eq!(decode(&f), Err(FrameError::Foreign), "id {id:#04x}");
            // The id decides before the length is looked at.
            assert_eq!(decode(&[id]), Err(FrameError::Foreign));
        }
        // Ours still decodes; an empty buffer is TooShort, not Foreign.
        assert!(decode(&[AIR_ID, 0x60, 1, 2, 3]).is_ok());
        assert_eq!(decode(&[]), Err(FrameError::TooShort));
        assert_eq!(decode(&[AIR_ID, 0x60]), Err(FrameError::TooShort));
    }
}
