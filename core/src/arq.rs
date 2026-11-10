//! Selective repeat ARQ with a window of 8 and 200 byte chunks.
//!
//! Sequence numbers are u8 modulo 256 and compared with wrapping math.
//! All storage is fixed arrays, no heap.

pub const WINDOW: usize = 8;
pub const CHUNK: usize = 200;

#[derive(Clone, Copy)]
struct TxSlot {
    len: u16,
    sent: bool,
    acked: bool,
    last_ms: u32,
}

const EMPTY_TX: TxSlot = TxSlot { len: 0, sent: false, acked: false, last_ms: 0 };

pub struct ArqTx {
    buf: [[u8; CHUNK]; WINDOW],
    slots: [TxSlot; WINDOW],
    /// Ring index of the oldest unacked slot.
    head: usize,
    /// Slots in use.
    count: usize,
    /// Sequence number of the head slot.
    base: u8,
}

impl Default for ArqTx {
    fn default() -> Self {
        Self::new()
    }
}

impl ArqTx {
    pub fn new() -> Self {
        ArqTx {
            buf: [[0; CHUNK]; WINDOW],
            slots: [EMPTY_TX; WINDOW],
            head: 0,
            count: 0,
            base: 0,
        }
    }

    fn ring(&self, i: usize) -> usize {
        (self.head + i) % WINDOW
    }

    /// Bytes `push` can still accept: whole free slots, plus the unused tail
    /// of the newest slot if it has not been sent yet.
    pub fn free(&self) -> usize {
        let mut f = (WINDOW - self.count) * CHUNK;
        if self.count > 0 {
            if let Some(s) = self.slots.get(self.ring(self.count - 1)) {
                if !s.sent {
                    f += CHUNK - s.len as usize;
                }
            }
        }
        f
    }

    /// Queue bytes; returns how many were accepted.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let mut done = 0;
        while done < data.len() {
            // Reuse the newest slot if it is unsent and has room.
            let mut idx = None;
            if self.count > 0 {
                let r = self.ring(self.count - 1);
                if let Some(s) = self.slots.get(r) {
                    if !s.sent && (s.len as usize) < CHUNK {
                        idx = Some(r);
                    }
                }
            }
            if idx.is_none() {
                if self.count >= WINDOW {
                    break;
                }
                let r = self.ring(self.count);
                if let Some(s) = self.slots.get_mut(r) {
                    *s = EMPTY_TX;
                }
                self.count += 1;
                idx = Some(r);
            }
            let Some(r) = idx else { break };
            let (Some(s), Some(b)) = (self.slots.get_mut(r), self.buf.get_mut(r)) else {
                break;
            };
            let at = s.len as usize;
            let n = (CHUNK - at).min(data.len() - done);
            if let (Some(dst), Some(src)) = (b.get_mut(at..at + n), data.get(done..done + n)) {
                dst.copy_from_slice(src);
            }
            s.len += n as u16;
            done += n;
        }
        done
    }

    /// Next frame to transmit: the first never-sent chunk, otherwise the
    /// oldest unacked chunk whose last send is at least `rto_ms` ago
    /// (wrapping u32 millisecond math).
    pub fn next_to_send(&mut self, now_ms: u32, rto_ms: u32) -> Option<(u8, &[u8])> {
        let mut pick = None;
        for i in 0..self.count {
            if let Some(s) = self.slots.get(self.ring(i)) {
                if !s.sent {
                    pick = Some(i);
                    break;
                }
            }
        }
        if pick.is_none() {
            for i in 0..self.count {
                if let Some(s) = self.slots.get(self.ring(i)) {
                    if !s.acked && now_ms.wrapping_sub(s.last_ms) >= rto_ms {
                        pick = Some(i);
                        break;
                    }
                }
            }
        }
        let i = pick?;
        let r = self.ring(i);
        let s = self.slots.get_mut(r)?;
        s.sent = true;
        s.last_ms = now_ms;
        let len = s.len as usize;
        let seq = self.base.wrapping_add(i as u8);
        let data = self.buf.get(r)?.get(..len)?;
        Some((seq, data))
    }

    /// `ack` is the receiver's next expected seq; sack bit i means frame
    /// `ack + 1 + i` was received. Invalid or stale acks are ignored.
    pub fn on_ack(&mut self, ack: u8, sack: u8) {
        let adv = ack.wrapping_sub(self.base) as usize;
        if adv > self.count {
            return;
        }
        // Only frames that were actually sent can be acknowledged.
        for i in 0..adv {
            if !self.slots.get(self.ring(i)).is_some_and(|s| s.sent) {
                return;
            }
        }
        self.head = self.ring(adv);
        self.count -= adv;
        self.base = ack;
        for bit in 0..7usize {
            if sack & (1 << bit) == 0 {
                continue;
            }
            let off = 1 + bit;
            if off < self.count {
                let r = self.ring(off);
                if let Some(s) = self.slots.get_mut(r) {
                    if s.sent {
                        s.acked = true;
                    }
                }
            }
        }
    }

    /// True when nothing is queued or awaiting acknowledgement.
    pub fn idle(&self) -> bool {
        self.count == 0
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

pub struct ArqRx {
    buf: [[u8; CHUNK]; WINDOW],
    len: [u16; WINDOW],
    have: [bool; WINDOW],
    /// Ring index of the oldest undelivered slot.
    head: usize,
    /// Sequence number of the head slot.
    base: u8,
    /// Bytes of the head slot already delivered.
    read_off: usize,
}

impl Default for ArqRx {
    fn default() -> Self {
        Self::new()
    }
}

impl ArqRx {
    pub fn new() -> Self {
        ArqRx {
            buf: [[0; CHUNK]; WINDOW],
            len: [0; WINDOW],
            have: [false; WINDOW],
            head: 0,
            base: 0,
            read_off: 0,
        }
    }

    fn ring(&self, i: usize) -> usize {
        (self.head + i) % WINDOW
    }

    fn has(&self, off: usize) -> bool {
        self.have.get(self.ring(off)).copied().unwrap_or(false)
    }

    /// Store a frame. Returns false if it is out of window (old, too far
    /// ahead, or oversized); in-window duplicates are ignored and return true.
    pub fn on_data(&mut self, seq: u8, data: &[u8]) -> bool {
        let off = seq.wrapping_sub(self.base) as usize;
        if off >= WINDOW || data.len() > CHUNK {
            return false;
        }
        let r = self.ring(off);
        if self.have.get(r).copied().unwrap_or(true) {
            return true;
        }
        if let (Some(dst), Some(l), Some(h)) =
            (self.buf.get_mut(r), self.len.get_mut(r), self.have.get_mut(r))
        {
            if let Some(d) = dst.get_mut(..data.len()) {
                d.copy_from_slice(data);
            }
            *l = data.len() as u16;
            *h = true;
        }
        true
    }

    /// (cumulative next expected seq, sack bitmap) where bit i means
    /// frame `next + 1 + i` is held.
    pub fn ack_state(&self) -> (u8, u8) {
        let mut contig = 0;
        while contig < WINDOW && self.has(contig) {
            contig += 1;
        }
        let ack = self.base.wrapping_add(contig as u8);
        let mut sack = 0u8;
        for bit in 0..7usize {
            let off = contig + 1 + bit;
            if off < WINDOW && self.has(off) {
                sack |= 1 << bit;
            }
        }
        (ack, sack)
    }

    /// Deliver contiguous in-order bytes. A frame larger than `out` is
    /// delivered across several calls; the remainder is kept.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let mut written = 0;
        while self.has(0) {
            let h = self.head;
            let total = self.len.get(h).copied().unwrap_or(0) as usize;
            let avail = total.saturating_sub(self.read_off);
            let n = avail.min(out.len() - written);
            if n < avail {
                // Partial: copy what fits and stop.
                if let (Some(dst), Some(src)) = (
                    out.get_mut(written..written + n),
                    self.buf.get(h).and_then(|b| b.get(self.read_off..self.read_off + n)),
                ) {
                    dst.copy_from_slice(src);
                }
                written += n;
                self.read_off += n;
                break;
            }
            if let (Some(dst), Some(src)) = (
                out.get_mut(written..written + n),
                self.buf.get(h).and_then(|b| b.get(self.read_off..self.read_off + n)),
            ) {
                dst.copy_from_slice(src);
            }
            written += n;
            if let Some(x) = self.have.get_mut(h) {
                *x = false;
            }
            self.head = (h + 1) % WINDOW;
            self.base = self.base.wrapping_add(1);
            self.read_off = 0;
        }
        written
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(len: usize, salt: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + salt * 17) % 251) as u8).collect()
    }

    fn read_all(rx: &mut ArqRx) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 77];
        loop {
            let n = rx.read(&mut buf);
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// Queues 8 full chunks and sends them all once at t = 0.
    fn sent_window(data: &[u8]) -> (ArqTx, Vec<(u8, Vec<u8>)>) {
        let mut tx = ArqTx::new();
        assert_eq!(tx.push(data), WINDOW * CHUNK);
        let mut frames = Vec::new();
        while let Some((seq, d)) = tx.next_to_send(0, 1000) {
            frames.push((seq, d.to_vec()));
        }
        (tx, frames)
    }

    #[test]
    fn in_order_delivery_with_loss() {
        let data = pattern(WINDOW * CHUNK, 1);
        let (mut tx, frames) = sent_window(&data);
        assert_eq!(frames.len(), 8);
        let seqs: Vec<u8> = frames.iter().map(|f| f.0).collect();
        assert_eq!(seqs, [0, 1, 2, 3, 4, 5, 6, 7]);

        let mut rx = ArqRx::new();
        for (seq, d) in &frames {
            if *seq != 2 && *seq != 5 {
                assert!(rx.on_data(*seq, d));
            }
        }
        // Only frames 0 and 1 are deliverable; 3, 4, 6, 7 are held.
        assert_eq!(rx.ack_state(), (2, 0b0001_1011));
        let first = read_all(&mut rx);
        assert_eq!(first, &data[..2 * CHUNK]);

        let (ack, sack) = rx.ack_state();
        tx.on_ack(ack, sack);
        let mut resent = Vec::new();
        while let Some((seq, d)) = tx.next_to_send(1000, 1000) {
            resent.push((seq, d.to_vec()));
        }
        for (seq, d) in &resent {
            assert!(rx.on_data(*seq, d));
        }
        let mut got = first;
        got.extend(read_all(&mut rx));
        assert_eq!(got, data);
        let (ack, sack) = rx.ack_state();
        assert_eq!((ack, sack), (8, 0));
        tx.on_ack(ack, sack);
        assert!(tx.idle());
    }

    #[test]
    fn sack_resends_only_missing() {
        let data = pattern(WINDOW * CHUNK, 2);
        let (mut tx, frames) = sent_window(&data);
        let mut rx = ArqRx::new();
        for (seq, d) in &frames {
            if *seq != 2 && *seq != 5 {
                rx.on_data(*seq, d);
            }
        }
        let (ack, sack) = rx.ack_state();
        tx.on_ack(ack, sack);
        // Nothing is resent before the RTO.
        assert!(tx.next_to_send(999, 1000).is_none());
        let a = tx.next_to_send(1000, 1000).map(|(s, d)| (s, d.to_vec()));
        let b = tx.next_to_send(1000, 1000).map(|(s, d)| (s, d.to_vec()));
        assert_eq!(a, Some((2, data[2 * CHUNK..3 * CHUNK].to_vec())));
        assert_eq!(b, Some((5, data[5 * CHUNK..6 * CHUNK].to_vec())));
        assert!(tx.next_to_send(1000, 1000).is_none());
        // Lost again: one more round, still only 2 and 5.
        let again: Vec<u8> =
            core::iter::from_fn(|| tx.next_to_send(2000, 1000).map(|f| f.0)).collect();
        assert_eq!(again, [2, 5]);
        // Stale, duplicate and nonsense acks change nothing.
        tx.on_ack(0, 0xFF);
        tx.on_ack(200, 0);
        let again: Vec<u8> =
            core::iter::from_fn(|| tx.next_to_send(3000, 1000).map(|f| f.0)).collect();
        assert_eq!(again, [2, 5]);
        // Duplicates at the receiver are ignored; out of window is refused.
        assert!(rx.on_data(3, &[9; 5]));
        assert!(!rx.on_data(10, &[1]));
        assert!(!rx.on_data(255, &[1]));
        assert!(!rx.on_data(2, &[0; CHUNK + 1]));
    }

    #[test]
    fn wraps_sequence_numbers() {
        let mut tx = ArqTx::new();
        let mut rx = ArqRx::new();
        let total = 600usize;
        let mut sent = Vec::new();
        let mut got = Vec::new();
        let mut queued = 0usize;
        let mut seqs_seen = Vec::new();
        let mut now = 0u32;
        let mut rounds = 0;
        while got.len() < total * CHUNK {
            rounds += 1;
            assert!(rounds < 10_000, "no progress");
            while queued < total && tx.free() >= CHUNK {
                let chunk = pattern(CHUNK, queued);
                assert_eq!(tx.push(&chunk), CHUNK);
                sent.extend_from_slice(&chunk);
                queued += 1;
            }
            let mut n = 0;
            while let Some((seq, d)) = tx.next_to_send(now, 50) {
                seqs_seen.push(seq);
                // Lose every 7th transmission.
                n += 1;
                if (seqs_seen.len() % 7) != 3 {
                    let d = d.to_vec();
                    rx.on_data(seq, &d);
                }
                if n > WINDOW {
                    break;
                }
            }
            got.extend(read_all(&mut rx));
            let (ack, sack) = rx.ack_state();
            tx.on_ack(ack, sack);
            now = now.wrapping_add(60);
        }
        assert_eq!(got.len(), sent.len());
        assert!(got == sent, "data differs after wrap");
        assert!(tx.idle());
        // 600 chunks means the u8 sequence wrapped twice: seq 0 carried
        // chunks 0, 256 and 512.
        assert!(seqs_seen.iter().filter(|&&s| s == 0).count() >= 3);
        assert!(seqs_seen.contains(&255));
    }

    #[test]
    fn push_respects_free() {
        let mut tx = ArqTx::new();
        assert_eq!(tx.free(), WINDOW * CHUNK);
        assert!(tx.idle());
        assert_eq!(tx.push(&[1; 50]), 50);
        assert_eq!(tx.free(), WINDOW * CHUNK - 50);
        // The unsent newest slot is topped up, not started fresh.
        assert_eq!(tx.push(&[2; 100]), 100);
        assert_eq!(tx.free(), WINDOW * CHUNK - 150);
        // Once sent, a short slot's tail is no longer free.
        let (seq, d) = tx.next_to_send(0, 100).unwrap();
        assert_eq!((seq, d.len()), (0, 150));
        assert_eq!(tx.free(), (WINDOW - 1) * CHUNK);
        let big = vec![3u8; 5000];
        let n = tx.push(&big);
        assert_eq!(n, (WINDOW - 1) * CHUNK);
        assert_eq!(tx.free(), 0);
        assert_eq!(tx.push(&big), 0);
        assert_eq!(tx.push(&[]), 0);
        // Acking frees whole slots again.
        let mut sent = 0;
        while tx.next_to_send(0, 100).is_some() {
            sent += 1;
        }
        assert_eq!(sent, WINDOW - 1);
        assert_eq!(tx.free(), 0);
        tx.on_ack(3, 0);
        assert_eq!(tx.free(), 3 * CHUNK);
        assert_eq!(tx.push(&big), 3 * CHUNK);
        assert_eq!(tx.free(), 0);
        // Acks for frames never sent are refused.
        tx.on_ack(3 + WINDOW as u8, 0);
        assert_eq!(tx.free(), 0);
        tx.reset();
        assert_eq!(tx.free(), WINDOW * CHUNK);
    }
}
