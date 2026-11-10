//! Wire frame format. Header is 4 bytes:
//! byte0 = type << 4 | flags, byte1 = call_id, byte2 = seq, byte3 = ack.

use crate::tier::Tier;

pub const HEADER_LEN: usize = 4;
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
}

pub fn encode(header: &Header, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::TooLong);
    }
    let total = HEADER_LEN + payload.len();
    let dst = out.get_mut(..total).ok_or(FrameError::TooShort)?;
    let (head, body) = dst.split_at_mut(HEADER_LEN);
    if let [b0, b1, b2, b3] = head {
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
        [b0, b1, b2, b3, rest @ ..] => (*b0, *b1, *b2, *b3, rest),
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

fn valid_name(n: &str) -> bool {
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
