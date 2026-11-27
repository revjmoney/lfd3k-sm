//! Burst parity. One Reed-Solomon parity symbol per byte column.
//!
//! When a call has agreed `CAP_FEC` and a selective ack has shown a hole,
//! the sender XORs the plain chunks of a group and appends one parity
//! frame. The receiver rebuilds the one missing chunk from that parity
//! and the chunks it already holds, so the sender does not resend it.
//! A classic group is four chunks, so the extra frame is one in five
//! (25 percent) while parity is on. A bulk burst of eight holds seven
//! chunks and the parity frame. A clean link sends none.
//!
//! The code is a shortened systematic Reed-Solomon code over GF(256)
//! with one parity symbol. In characteristic 2 the parity symbol is the
//! field sum of the data symbols, which is XOR. One missing symbol is
//! the XOR of the parity and the symbols that arrived. Two missing
//! symbols cannot be rebuilt; ARQ resends those.
//!
//! Wire payload of a parity frame (a Data frame with `FLAG_PARITY`):
//! `n` (1 byte, 1 to 7), `n` sequence numbers, the XOR of the chunk
//! lengths (1 byte), then the XOR of the chunk bytes. A shorter chunk
//! is padded with zero bytes up to the longest chunk in the group, and
//! the XOR of the bytes is that long. `CHUNK` is 200, so the longest
//! payload is 1 + 7 + 1 + 200 = 209 bytes, inside the 250 byte limit.
//!
//! # Cost on the target MCU
//!
//! The target is the ESP32-S3 on the Heltec V3 and V4 (240 MHz, 512 KB
//! SRAM). `Group` is the only extra buffer. It lives inside `Link`,
//! which is inside `Modem`, with no heap and no second copy of the
//! chunks: the receiver reads the siblings back out of `ArqRx`.
//! Building or applying one parity frame is at most 4 * 200 byte XORs.
//! That is a few microseconds, under one SF5 symbol (64 us). ROM is the
//! code itself; there are no lookup tables.

use crate::arq::CHUNK;

/// Most data chunks one parity frame can cover. A classic burst uses 4
/// of these. A bulk burst of 8 uses 7, so the parity frame is the last
/// of the eight.
pub const GROUP: usize = 7;
/// New chunks with no hole before parity turns off (when the SNR is fine).
pub const CLEAN_FRAMES: u8 = 16;
/// Third sends of a chunk, while parity is on, before the tier may step down.
/// A bulk window keeps more chunks in flight, so a few third sends at a
/// 10 percent loss are not yet "parity cannot keep this tier".
pub const FAIL_LIMIT: u8 = 8;

/// Running XOR of one parity group. Fixed storage, no heap.
#[derive(Clone, Copy)]
pub struct Group {
    seq: [u8; GROUP],
    len: [u8; GROUP],
    n: u8,
    xor_len: u8,
    /// Longest chunk folded so far. The parity bytes are this long.
    span: u8,
    xor: [u8; CHUNK],
}

impl Group {
    pub const fn new() -> Self {
        Group {
            seq: [0; GROUP],
            len: [0; GROUP],
            n: 0,
            xor_len: 0,
            span: 0,
            xor: [0; CHUNK],
        }
    }

    pub fn count(&self) -> usize {
        self.n as usize
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Fold one plain chunk. False if the group is full or `data` is
    /// longer than `CHUNK`.
    pub fn fold(&mut self, seq: u8, data: &[u8]) -> bool {
        if data.len() > CHUNK || self.n as usize >= GROUP {
            return false;
        }
        let i = self.n as usize;
        if let Some(s) = self.seq.get_mut(i) {
            *s = seq;
        }
        if let Some(l) = self.len.get_mut(i) {
            *l = data.len() as u8;
        }
        self.xor_len ^= data.len() as u8;
        for (a, b) in self.xor.iter_mut().zip(data.iter()) {
            *a ^= *b;
        }
        if data.len() > self.span as usize {
            self.span = data.len() as u8;
        }
        self.n = self.n.saturating_add(1);
        true
    }

    /// Encode the parity payload. None if the group is empty or `out`
    /// is short.
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        let n = self.n as usize;
        if n == 0 || n > GROUP {
            return None;
        }
        let span = self.span as usize;
        let total = 1 + n + 1 + span;
        let dst = out.get_mut(..total)?;
        let (head, rest) = dst.split_at_mut(1);
        head[0] = self.n;
        let (seqs, rest) = rest.split_at_mut(n);
        seqs.copy_from_slice(self.seq.get(..n)?);
        let (lenb, bytes) = rest.split_at_mut(1);
        lenb[0] = self.xor_len;
        bytes.copy_from_slice(self.xor.get(..span)?);
        Some(total)
    }
}

impl Default for Group {
    fn default() -> Self {
        Self::new()
    }
}

/// Rebuild the one missing chunk of a parity payload.
///
/// `get` returns the plain bytes already held for a sequence number, or
/// None when that chunk is absent. The result is `(seq, len)` and the
/// rebuilt bytes in `out`. None when the payload is malformed, when
/// every chunk is already held, or when more than one is missing.
pub fn recover<'a>(
    payload: &[u8],
    mut get: impl FnMut(u8) -> Option<&'a [u8]>,
    out: &mut [u8],
) -> Option<(u8, usize)> {
    let (&n_b, rest) = payload.split_first()?;
    let n = n_b as usize;
    if n == 0 || n > GROUP {
        return None;
    }
    let (seqs, rest) = rest.split_at_checked(n)?;
    let (&len_xor, parity) = rest.split_first()?;
    if parity.len() > CHUNK {
        return None;
    }
    let mut missing: Option<usize> = None;
    let mut len_acc = len_xor;
    let mut acc = [0u8; CHUNK];
    acc.get_mut(..parity.len())?.copy_from_slice(parity);
    for (i, &seq) in seqs.iter().enumerate() {
        match get(seq) {
            Some(data) => {
                if data.len() > CHUNK || data.len() > parity.len() {
                    return None;
                }
                len_acc ^= data.len() as u8;
                for (a, b) in acc.iter_mut().zip(data.iter()) {
                    *a ^= *b;
                }
            }
            None => {
                if missing.is_some() {
                    return None;
                }
                missing = Some(i);
            }
        }
    }
    let i = missing?;
    let seq = *seqs.get(i)?;
    let len = len_acc as usize;
    if len > parity.len() {
        return None;
    }
    let src = acc.get(..len)?;
    out.get_mut(..len)?.copy_from_slice(src);
    Some((seq, len))
}

// The group is the RAM cost of parity: one column, not four chunks.
const _: () = assert!(core::mem::size_of::<Group>() <= 224);

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(len: usize, salt: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i * 31 + salt * 17) % 251) as u8)
            .collect()
    }

    #[test]
    fn rebuilds_each_missing_chunk() {
        let chunks = [
            pattern(200, 1),
            pattern(50, 2),
            pattern(1, 3),
            pattern(200, 4),
        ];
        let mut g = Group::new();
        for (i, c) in chunks.iter().enumerate() {
            assert!(g.fold(10 + i as u8, c));
        }
        let mut payload = [0u8; 256];
        let n = g.encode(&mut payload).unwrap();
        assert!(n <= 209, "payload {n}");
        for hole in 0..4 {
            let mut out = [0u8; CHUNK];
            let (seq, len) = recover(
                &payload[..n],
                |s| {
                    let i = (s - 10) as usize;
                    if i == hole {
                        None
                    } else {
                        chunks.get(i).map(|c| c.as_slice())
                    }
                },
                &mut out,
            )
            .unwrap();
            assert_eq!(seq, 10 + hole as u8);
            assert_eq!(&out[..len], chunks[hole].as_slice());
        }
    }

    #[test]
    fn refuses_zero_or_two_holes() {
        let mut g = Group::new();
        g.fold(1, &[9, 8, 7]);
        g.fold(2, &[1, 2, 3, 4]);
        let mut payload = [0u8; 64];
        let n = g.encode(&mut payload).unwrap();
        let mut out = [0u8; CHUNK];
        assert!(recover(
            &payload[..n],
            |s| match s {
                1 => Some(&[9, 8, 7][..]),
                2 => Some(&[1, 2, 3, 4][..]),
                _ => None,
            },
            &mut out,
        )
        .is_none());
        assert!(recover(&payload[..n], |_| None, &mut out).is_none());
        assert!(recover(&[], |_| None, &mut out).is_none());
        assert!(recover(&[0], |_| None, &mut out).is_none());
        assert!(recover(&[5, 1, 2, 3, 4], |_| None, &mut out).is_none());
    }

    #[test]
    fn bulk_group_of_seven_rebuilds_one_hole() {
        let chunks: Vec<Vec<u8>> = (0..GROUP).map(|i| pattern(20 + i, i + 1)).collect();
        let mut g = Group::new();
        for (i, c) in chunks.iter().enumerate() {
            assert!(g.fold(i as u8, c));
        }
        assert!(!g.fold(9, &[1]), "an eighth chunk must not fit");
        let mut payload = [0u8; 256];
        let n = g.encode(&mut payload).unwrap();
        assert!(n <= 209, "payload {n}");
        let hole = 3usize;
        let mut out = [0u8; CHUNK];
        let (seq, len) = recover(
            &payload[..n],
            |s| {
                let i = s as usize;
                if i == hole {
                    None
                } else {
                    chunks.get(i).map(|c| c.as_slice())
                }
            },
            &mut out,
        )
        .unwrap();
        assert_eq!(seq, hole as u8);
        assert_eq!(&out[..len], chunks[hole].as_slice());
    }

    #[test]
    fn one_chunk_group_is_a_copy() {
        let mut g = Group::new();
        assert!(g.fold(7, b"abc"));
        let mut payload = [0u8; 16];
        let n = g.encode(&mut payload).unwrap();
        let mut out = [0u8; CHUNK];
        let (seq, len) = recover(&payload[..n], |_| None, &mut out).unwrap();
        assert_eq!((seq, len), (7, 3));
        assert_eq!(&out[..len], b"abc");
    }

    #[test]
    fn ram_is_one_column() {
        let n = core::mem::size_of::<Group>();
        assert_eq!(n, 217, "Group is {n} bytes; docs/fec-sim.md quotes this");
        // Four saved chunks would be 800 bytes. The running XOR is not.
        assert!(n < 4 * CHUNK);
    }
}
