//! Selective repeat ARQ with 200 byte chunks.
//!
//! The classic window is 8 chunks. After `CAP_BULK` is agreed the link
//! raises the limit to 16 for that call (`WINDOW_BULK`). Storage is always
//! the larger window: fixed arrays, no heap, so a call that stays at 8
//! simply leaves the extra slots unused.
//!
//! Sequence numbers are u8 modulo 256 and compared with wrapping math.
//!
//! RAM, per modem, versus a build that stored only 8 chunks: two payload
//! buffers of `(WINDOW_BULK - WINDOW) * CHUNK` bytes (3200), plus 8 extra
//! `TxSlot`s. Each slot stays 8 bytes: the sent, acked, and closed flags
//! share one byte, and `sends` (how many times the chunk went out, which
//! the parity tier-hold uses) is the next byte. The extra slots are 64
//! bytes. Eight extra receive length and presence bytes are 24,
//! and one `usize` limit sits on each of `ArqTx` and `ArqRx`. `Link` also
//! holds one parity `Group` (217 bytes) and a `last_sends` byte on `ArqTx`.
//! The 3200 bytes of payload buffers are the same on a 32 bit device; the
//! two limit words are 4 bytes each there instead of 8.

pub const WINDOW: usize = 8;
/// Window used on a call that agreed `frame::CAP_BULK`.
pub const WINDOW_BULK: usize = 16;
/// Slots actually stored. `limit` is `WINDOW` or `WINDOW_BULK`.
const SLOTS: usize = WINDOW_BULK;
pub const CHUNK: usize = 200;

const _: () = assert!(WINDOW_BULK == SLOTS && WINDOW <= SLOTS && WINDOW_BULK <= 16);

/// Sent, individually acknowledged, or sealed against further appends.
const TX_SENT: u8 = 1;
const TX_ACKED: u8 = 2;
const TX_CLOSED: u8 = 4;

#[derive(Clone, Copy)]
struct TxSlot {
    len: u16,
    /// `TX_SENT`, `TX_ACKED`, `TX_CLOSED`. One byte keeps the slot at 8 bytes.
    flags: u8,
    /// Times this chunk has been handed to the sender. 0 until the first.
    sends: u8,
    last_ms: u32,
}

const _: () = assert!(core::mem::size_of::<TxSlot>() == 8);

impl TxSlot {
    fn sent(self) -> bool {
        self.flags & TX_SENT != 0
    }
    fn acked(self) -> bool {
        self.flags & TX_ACKED != 0
    }
    fn closed(self) -> bool {
        self.flags & TX_CLOSED != 0
    }
    fn set(&mut self, bit: u8) {
        self.flags |= bit;
    }
}

const EMPTY_TX: TxSlot = TxSlot {
    len: 0,
    flags: 0,
    sends: 0,
    last_ms: 0,
};

pub struct ArqTx {
    buf: [[u8; CHUNK]; SLOTS],
    slots: [TxSlot; SLOTS],
    /// Ring index of the oldest unacked slot.
    head: usize,
    /// Slots in use.
    count: usize,
    /// Sequence number of the head slot.
    base: u8,
    /// Outstanding chunks allowed: `WINDOW` or `WINDOW_BULK`.
    limit: usize,
    /// `sends` of the chunk `next_to_send` handed out most recently.
    last_sends: u8,
}

impl Default for ArqTx {
    fn default() -> Self {
        Self::new()
    }
}

impl ArqTx {
    pub const fn new() -> Self {
        ArqTx {
            buf: [[0; CHUNK]; SLOTS],
            slots: [EMPTY_TX; SLOTS],
            head: 0,
            count: 0,
            base: 0,
            limit: WINDOW,
            last_sends: 0,
        }
    }

    /// `WINDOW` or `WINDOW_BULK`. Other values clamp. Call only while idle.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.clamp(1, SLOTS);
    }

    fn ring(&self, i: usize) -> usize {
        (self.head + i) % SLOTS
    }

    /// Chunks queued or awaiting a cumulative acknowledgement.
    pub fn outstanding(&self) -> usize {
        self.count
    }

    /// Bytes `push` can still accept: whole free slots, plus the unused tail
    /// of the newest slot if it has not been sent yet.
    pub fn free(&self) -> usize {
        let mut f = self.limit.saturating_sub(self.count) * CHUNK;
        if self.count > 0 {
            if let Some(s) = self.slots.get(self.ring(self.count - 1)) {
                if !s.sent() {
                    f += CHUNK - s.len as usize;
                }
            }
        }
        f
    }

    /// Queue bytes; returns how many were accepted.
    pub fn push(&mut self, data: &[u8]) -> usize {
        self.push_max(data, CHUNK)
    }

    /// Queue bytes in chunks of at most `max` bytes (1 to `CHUNK`); returns
    /// how many were accepted. A voice call's listener frames carry small
    /// chunks (`voice::sched`).
    pub fn push_max(&mut self, data: &[u8], max: usize) -> usize {
        let max = max.clamp(1, CHUNK);
        let mut done = 0;
        while done < data.len() {
            // Reuse the newest slot if it is unsent and has room.
            let mut idx = None;
            if self.count > 0 {
                let r = self.ring(self.count - 1);
                if let Some(s) = self.slots.get(r) {
                    if !s.sent() && !s.closed() && (s.len as usize) < max {
                        idx = Some(r);
                    }
                }
            }
            if idx.is_none() {
                if self.count >= self.limit {
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
            let n = max.saturating_sub(at).min(data.len() - done);
            if n == 0 {
                break;
            }
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
        let i = self.pick(now_ms, rto_ms)?;
        let r = self.ring(i);
        let s = self.slots.get_mut(r)?;
        s.set(TX_SENT);
        s.sends = s.sends.saturating_add(1);
        s.last_ms = now_ms;
        self.last_sends = s.sends;
        let len = s.len as usize;
        let seq = self.base.wrapping_add(i as u8);
        let data = self.buf.get(r)?.get(..len)?;
        Some((seq, data))
    }

    /// How many times the chunk from the latest `next_to_send` has been
    /// handed out. 1 is the first send, 2 is the first resend.
    pub fn last_sends(&self) -> u8 {
        self.last_sends
    }

    /// True when a queued chunk has never been sent.
    pub fn has_unsent(&self) -> bool {
        (0..self.count).any(|i| self.slots.get(self.ring(i)).is_some_and(|s| !s.sent()))
    }

    /// Length of the chunk `next_to_send` would hand out now, without
    /// sending it; None when nothing is due.
    pub fn next_len(&self, now_ms: u32, rto_ms: u32) -> Option<usize> {
        let i = self.pick(now_ms, rto_ms)?;
        self.slots.get(self.ring(i)).map(|s| s.len as usize)
    }

    /// Window offset of the chunk due now (see `next_to_send`).
    fn pick(&self, now_ms: u32, rto_ms: u32) -> Option<usize> {
        let mut pick = None;
        for i in 0..self.count {
            if let Some(s) = self.slots.get(self.ring(i)) {
                if !s.sent() {
                    pick = Some(i);
                    break;
                }
            }
        }
        if pick.is_none() {
            for i in 0..self.count {
                if let Some(s) = self.slots.get(self.ring(i)) {
                    if !s.acked() && now_ms.wrapping_sub(s.last_ms) >= rto_ms {
                        pick = Some(i);
                        break;
                    }
                }
            }
        }
        pick
    }

    /// `ack` is the receiver's next expected seq; sack bit i means frame
    /// `ack + 1 + i` was received. The classic bitmap is 8 bits (bit 7
    /// clear). A bulk call uses 16 bits (bit 15 clear). Invalid or stale
    /// acks are ignored.
    pub fn on_ack(&mut self, ack: u8, sack: u16) {
        let adv = ack.wrapping_sub(self.base) as usize;
        if adv > self.count {
            return;
        }
        // Only frames that were actually sent can be acknowledged.
        for i in 0..adv {
            if !self.slots.get(self.ring(i)).is_some_and(|s| s.sent()) {
                return;
            }
        }
        self.head = self.ring(adv);
        self.count -= adv;
        self.base = ack;
        let bits = self.limit.saturating_sub(1).min(15);
        for bit in 0..bits {
            if sack & (1 << bit) == 0 {
                continue;
            }
            let off = 1 + bit;
            if off < self.count && off < self.limit {
                let r = self.ring(off);
                if let Some(s) = self.slots.get_mut(r) {
                    if s.sent() {
                        s.set(TX_ACKED);
                    }
                }
            }
        }
    }

    /// Length of the longest chunk queued or awaiting acknowledgement.
    pub fn longest(&self) -> usize {
        (0..self.count)
            .filter_map(|i| self.slots.get(self.ring(i)))
            .map(|s| s.len as usize)
            .max()
            .unwrap_or(0)
    }

    /// True when nothing is queued or awaiting acknowledgement.
    pub fn idle(&self) -> bool {
        self.count == 0
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Sequence number of the oldest unacked chunk.
    pub fn base(&self) -> u8 {
        self.base
    }

    /// Sequence number the next new chunk will get.
    pub fn next_seq(&self) -> u8 {
        self.base.wrapping_add(self.count as u8)
    }

    /// The newest unsent chunk will not take more bytes. A later `push`
    /// starts a new chunk, so a dictionary reset is not appended to bytes
    /// that were compressed with the old one.
    pub fn seal(&mut self) {
        if self.count == 0 {
            return;
        }
        let r = self.ring(self.count - 1);
        if let Some(s) = self.slots.get_mut(r) {
            if !s.sent() && s.len > 0 {
                s.set(TX_CLOSED);
            }
        }
    }
}

pub struct ArqRx {
    buf: [[u8; CHUNK]; SLOTS],
    len: [u16; SLOTS],
    have: [bool; SLOTS],
    /// Ring index of the oldest undelivered slot.
    head: usize,
    /// Sequence number of the head slot.
    base: u8,
    /// Bytes of the head slot already delivered.
    read_off: usize,
    /// Chunks the window accepts: `WINDOW` or `WINDOW_BULK`.
    limit: usize,
}

impl Default for ArqRx {
    fn default() -> Self {
        Self::new()
    }
}

impl ArqRx {
    pub const fn new() -> Self {
        ArqRx {
            buf: [[0; CHUNK]; SLOTS],
            len: [0; SLOTS],
            have: [false; SLOTS],
            head: 0,
            base: 0,
            read_off: 0,
            limit: WINDOW,
        }
    }

    /// `WINDOW` or `WINDOW_BULK`. Other values clamp. Call only while idle.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.clamp(1, SLOTS);
    }

    fn ring(&self, i: usize) -> usize {
        (self.head + i) % SLOTS
    }

    fn has(&self, off: usize) -> bool {
        self.have.get(self.ring(off)).copied().unwrap_or(false)
    }

    /// Plain bytes held for `seq`, if they are inside the window.
    pub fn get(&self, seq: u8) -> Option<&[u8]> {
        let off = seq.wrapping_sub(self.base) as usize;
        if off >= self.limit {
            return None;
        }
        let r = self.ring(off);
        if !self.have.get(r).copied().unwrap_or(false) {
            return None;
        }
        let len = self.len.get(r).copied().unwrap_or(0) as usize;
        self.buf.get(r).and_then(|b| b.get(..len))
    }

    /// Store a frame. Returns false if it is out of window (old, too far
    /// ahead, or oversized); in-window duplicates are ignored and return true.
    pub fn on_data(&mut self, seq: u8, data: &[u8]) -> bool {
        let off = seq.wrapping_sub(self.base) as usize;
        if off >= self.limit || data.len() > CHUNK {
            return false;
        }
        let r = self.ring(off);
        if self.have.get(r).copied().unwrap_or(true) {
            return true;
        }
        if let (Some(dst), Some(l), Some(h)) = (
            self.buf.get_mut(r),
            self.len.get_mut(r),
            self.have.get_mut(r),
        ) {
            if let Some(d) = dst.get_mut(..data.len()) {
                d.copy_from_slice(data);
            }
            *l = data.len() as u16;
            *h = true;
        }
        true
    }

    /// True when the next in order frame is here (`read` would make progress
    /// or finish a frame it already started).
    pub fn has_ready(&self) -> bool {
        self.has(0)
    }

    /// Sequence number of the frame `read` would take from, if one is here.
    pub fn head_seq(&self) -> Option<u8> {
        if self.has(0) {
            Some(self.base)
        } else {
            None
        }
    }

    /// Drop the in order frame, including a tail already partly read.
    pub fn discard_head(&mut self) {
        if !self.has(0) {
            return;
        }
        let h = self.head;
        if let Some(x) = self.have.get_mut(h) {
            *x = false;
        }
        self.head = (h + 1) % SLOTS;
        self.base = self.base.wrapping_add(1);
        self.read_off = 0;
    }

    /// (cumulative next expected seq, sack bitmap) where bit i means
    /// frame `next + 1 + i` is held. Bit 7 stays clear at the classic
    /// limit. A bulk limit uses bits 0 to 14 and leaves bit 15 clear.
    pub fn ack_state(&self) -> (u8, u16) {
        let mut contig = 0;
        while contig < self.limit && self.has(contig) {
            contig += 1;
        }
        let ack = self.base.wrapping_add(contig as u8);
        let mut sack = 0u16;
        let bits = self.limit.saturating_sub(1).min(15);
        for bit in 0..bits {
            let off = contig + 1 + bit;
            if off < self.limit && self.has(off) {
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
                    self.buf
                        .get(h)
                        .and_then(|b| b.get(self.read_off..self.read_off + n)),
                ) {
                    dst.copy_from_slice(src);
                }
                written += n;
                self.read_off += n;
                break;
            }
            if let (Some(dst), Some(src)) = (
                out.get_mut(written..written + n),
                self.buf
                    .get(h)
                    .and_then(|b| b.get(self.read_off..self.read_off + n)),
            ) {
                dst.copy_from_slice(src);
            }
            written += n;
            if let Some(x) = self.have.get_mut(h) {
                *x = false;
            }
            self.head = (h + 1) % SLOTS;
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

    fn pattern(len: usize, mix: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i * 31 + mix * 17) % 251) as u8)
            .collect()
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

    #[test]
    fn bulk_window_sacks_fifteen_and_retransmits_holes() {
        let mut tx = ArqTx::new();
        let mut rx = ArqRx::new();
        tx.set_limit(WINDOW_BULK);
        rx.set_limit(WINDOW_BULK);
        assert_eq!(tx.free(), WINDOW_BULK * CHUNK);
        // A request past the stored slots clamps.
        tx.set_limit(100);
        assert_eq!(tx.free(), WINDOW_BULK * CHUNK);

        let data = pattern(WINDOW_BULK * CHUNK, 9);
        assert_eq!(tx.push(&data), data.len());
        assert_eq!(tx.push(&[1]), 0);
        let mut frames = Vec::new();
        while let Some((seq, d)) = tx.next_to_send(0, 1000) {
            frames.push((seq, d.to_vec()));
        }
        assert_eq!(frames.len(), WINDOW_BULK);
        let seqs: Vec<u8> = frames.iter().map(|f| f.0).collect();
        assert_eq!(seqs, (0..16).collect::<Vec<u8>>());

        for (seq, d) in &frames {
            if *seq != 0 && *seq != 9 {
                assert!(rx.on_data(*seq, d));
            }
        }
        // Past the bulk window, and an oversized chunk, are refused.
        assert!(!rx.on_data(16, &[1]));
        assert!(!rx.on_data(0, &[0; CHUNK + 1]));
        let (ack, sack) = rx.ack_state();
        assert_eq!(ack, 0);
        let mut want = 0u16;
        for seq in 1..16u16 {
            if seq != 9 {
                want |= 1 << (seq - 1);
            }
        }
        assert_eq!(sack, want);
        assert_eq!(sack & (1 << 15), 0);
        // Nothing is readable until seq 0 arrives.
        assert!(read_all(&mut rx).is_empty());

        tx.on_ack(ack, sack);
        assert!(tx.next_to_send(999, 1000).is_none());
        let resent: Vec<u8> = core::iter::from_fn(|| tx.next_to_send(1000, 1000).map(|f| f.0))
            .take(4)
            .collect();
        assert_eq!(resent, [0, 9]);
        for seq in resent {
            let d = &data[usize::from(seq) * CHUNK..usize::from(seq + 1) * CHUNK];
            assert!(rx.on_data(seq, d));
        }
        assert_eq!(read_all(&mut rx), data);
        let (ack, sack) = rx.ack_state();
        assert_eq!((ack, sack), (16, 0));
        tx.on_ack(ack, sack);
        assert!(tx.idle());

        // The classic limit still refuses what a window of 8 cannot hold,
        // and its bitmap leaves bit 7 clear.
        let mut narrow = ArqRx::new();
        assert!(!narrow.on_data(8, &[1]));
        assert!(narrow.on_data(0, &[7, 7]));
        assert!(narrow.on_data(2, &[9]));
        assert_eq!(narrow.ack_state(), (1, 0b0000_0001));
        tx.reset();
        assert_eq!(tx.free(), WINDOW * CHUNK);
    }

    #[test]
    fn bulk_ram_is_the_extra_slots() {
        let payloads = (WINDOW_BULK - WINDOW) * CHUNK * 2;
        assert_eq!(payloads, 3_200);
        // Flags and `sends` are one byte each, so the slot stays 8 bytes.
        assert_eq!(core::mem::size_of::<TxSlot>(), 8);
        let tx_slots = (WINDOW_BULK - WINDOW) * core::mem::size_of::<TxSlot>();
        assert_eq!(tx_slots, 64);
        eprintln!(
            "RAM modem {} link {} tx {} rx {}",
            core::mem::size_of::<crate::Modem>(),
            core::mem::size_of::<crate::link::Link>(),
            core::mem::size_of::<ArqTx>(),
            core::mem::size_of::<ArqRx>()
        );
    }
}
