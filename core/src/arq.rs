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
