//! Stream LZ77 above ARQ (`CAP_LZ_STREAM`).
//!
//! Per frame LZ (`lz.rs`) starts from the preset on every frame, so a screen
//! that spans frames cannot reuse what the previous frame already sent.
//! This codec compresses the host byte stream first. ARQ then frames those
//! compressed bytes and resends them unchanged, and the peer decodes only
//! the in order stream `ArqRx` delivers. A lost frame cannot pull the two
//! dictionaries apart, because the resent bytes are the same bytes and the
//! decoder does not move until they arrive in order.
//!
//! # Wire format
//!
//! Tokens are not aligned to ARQ chunks. A token may start in one chunk and
//! finish in the next. The decoder holds a partial token until the rest
//! arrives.
//!
//! * `0x00` to `0x7E`, then `L + 1` bytes (`L` is that first byte): a literal
//!   run of 1 to 127 bytes.
//! * `0x7F`: RESET. History returns to the preset.
//! * `0x80` to `0xFF`, then one byte: a match, the same layout as per frame
//!   LZ. Length `(M + 3)` is 3 to 34. Distance is 1 to 1024.
//!
//! A match copies one byte at a time, so a distance of 1 repeats the last
//! byte. A distance past the bytes produced so far, or past the window, is
//! `LzError::BadDistance`. The decoder does not panic on a bad stream.
//!
//! The history is the preset (567 bytes) followed by plaintext, in a ring of
//! [`WINDOW`] bytes. After the window fills, the oldest bytes fall out,
//! including the preset. RESET, and a fresh call, load the preset again.
//!
//! Chain positions are `u16`. Before the plaintext position would pass
//! 60_000 the encoder emits RESET on its own, so a long call cannot wrap
//! those positions. The peer resets when it reads that token.
//!
//! # RAM
//!
//! Both directions live in `Link` for the life of the modem (no heap, and
//! nothing is allocated when a call starts). The sizes below are the arrays;
//! [`STREAM_RAM`] is `size_of` of both structs, padding included.
//!
//! * Encoder history: 1024 bytes.
//! * Encoder chain heads: 256 of `u16`, 512 bytes.
//! * Encoder chain links: 1024 of `u16`, 2048 bytes.
//! * Encoder pending output: 256 bytes.
//! * Decoder history: 1024 bytes.
//! * Decoder input hold: 160 bytes.
//! * Both structs together, padding included: [`STREAM_RAM`] (5128 bytes
//!   in the host build that measured the ratios below).
//!
//! Measured on test data in `core/tests/lz_stream.rs`, not on a live BBS.
//! Four AXIS style menus, 3608 bytes: per frame LZ 1372 bytes (2.6 to 1),
//! stream LZ 591 bytes (6.1 to 1). Block art, 3960 bytes: per frame LZ
//! 1421 bytes (2.7 to 1), stream LZ 456 bytes (8.6 to 1).
//!
//! # Dictionary reset
//!
//! * Retrain and resync: the sender emits RESET after the bytes already
//!   compressed, then clears its history. The peer clears its history when
//!   it reads that token, so frames already in flight still decode.
//! * Link loss: the sender drops its history at once. The receiver keeps
//!   its history until it has delivered the bytes already in ARQ, then
//!   clears it. The next call starts from the preset again.
//!
//! Each stream Data frame ends with one check byte ([`stream_sum`] of the
//! compressed bytes). The sender still holds the good copy, so a mismatch
//! is not acknowledged and ARQ sends that frame again. The bad copy never
//! reaches the dictionary. If the decoder still reports a bad distance, the
//! receiver asks the peer to reset (Keepalive with `FLAG_LZ_SYNC`) and skips
//! compressed bytes until the Data frame that carries the same flag. The
//! call continues instead of stalling.

use crate::lz::{LzError, PRESET};

/// Sliding history, in bytes. Also the farthest match distance (10 bits).
pub const WINDOW: usize = 1024;
/// Encoder output held until ARQ has room. One token is at most 128 bytes.
const PEND: usize = 256;
/// Compressed bytes the decoder holds across `pull` calls.
const INBUF: usize = 160;
const BUCKETS: usize = 256;
const NIL: u16 = u16::MAX;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 34;
const MAX_LIT: usize = 127;
/// One byte token: put the history back to the preset.
const RESET: u8 = 0x7F;
/// Emit RESET before chain positions get near `u16::MAX` (`NIL`).
const POS_RESET: u32 = 60_000;
/// Candidates tried at one position. Bounds the time per byte.
const MAX_CHAIN: usize = 64;

/// One byte mixed into each stream Data frame, after the compressed bytes.
/// A mismatch means the copy on the air is not the copy the sender still
/// holds, so the frame is not acknowledged and ARQ sends the good one again.
pub fn stream_sum(data: &[u8]) -> u8 {
    let mut s = 0u8;
    for &b in data {
        s = s.wrapping_mul(33).wrapping_add(b);
    }
    s
}
/// Plaintext held across `push` calls. The host feeds one byte at a time
/// (the `+++` escape scan), so a match has to be able to see earlier bytes
/// of the same burst. Full, this is one batch: compress it, keep two bytes
/// so the next byte can still extend a match. `flush_tail` sends the rest.
pub(crate) const HOLD: usize = 64;

const _: () = assert!(PRESET.len() < WINDOW);
const _: () = assert!(PRESET.len() < NIL as usize);
const _: () = assert!(POS_RESET < NIL as u32);
const _: () = assert!(PEND > MAX_LIT);
const _: () = assert!(INBUF > MAX_LIT);
const _: () = assert!(WINDOW == 1024);

const fn hash(a: u8, b: u8, c: u8) -> usize {
    let x = (a as u32) | ((b as u32) << 8) | ((c as u32) << 16);
    (x.wrapping_mul(0x9E37_79B1) >> 24) as usize
}

/// Load the preset into `hist` and return its length. `hist` is cleared first.
const fn load_preset(hist: &mut [u8; WINDOW]) -> u32 {
    let mut i = 0;
    while i < WINDOW {
        hist[i] = 0;
        i += 1;
    }
    let mut i = 0;
    while i < PRESET.len() {
        hist[i] = PRESET[i];
        i += 1;
    }
    PRESET.len() as u32
}

/// Encoder: plaintext in, compressed bytes out, history kept across calls.
pub struct StreamEnc {
    hist: [u8; WINDOW],
    prev: [u16; WINDOW],
    head: [u16; BUCKETS],
    pend: [u8; PEND],
    hold: [u8; HOLD],
    pos: u32,
    /// Committed positions at the end of `hist` not yet inserted in a chain.
    unhashed: u32,
    raw_total: u32,
    comp_total: u32,
    pend_len: u16,
    pend_at: u16,
    /// Literal bytes committed to `hist` and not yet written to `pend`.
    lit_pending: u8,
    hold_len: u8,
    reset_owed: bool,
}

/// Decoder: compressed bytes in, plaintext out.
pub struct StreamDec {
    hist: [u8; WINDOW],
    buf: [u8; INBUF],
    pos: u32,
    blen: u8,
    bat: u8,
    phase: Phase,
    /// Match header, kept while the distance byte is still to come.
    hdr: u8,
    err: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    Head,
    Lit { left: u8 },
    Dist { len: u8 },
    Copy { left: u16, dist: u16 },
}

/// Bytes of `StreamEnc` plus `StreamDec` (padding included).
pub const STREAM_RAM: usize = core::mem::size_of::<StreamEnc>() + core::mem::size_of::<StreamDec>();

const _: () = assert!(STREAM_RAM < 6 * 1024);
const _: () = assert!(STREAM_RAM > 1024 + 512 + 2048 + 256 + 1024);

impl StreamEnc {
    pub const fn new() -> Self {
        let mut s = StreamEnc {
            hist: [0; WINDOW],
            prev: [NIL; WINDOW],
            head: [NIL; BUCKETS],
            pend: [0; PEND],
            hold: [0; HOLD],
            pos: 0,
            unhashed: 0,
            raw_total: 0,
            comp_total: 0,
            pend_len: 0,
            pend_at: 0,
            lit_pending: 0,
            hold_len: 0,
            reset_owed: false,
        };
        s.reseed_const();
        s
    }

    /// History, chains and the pending reset flag back to a fresh call.
    /// Counters of bytes already counted for the ratio are cleared too.
    pub fn reset_call(&mut self) {
        self.pend_len = 0;
        self.pend_at = 0;
        self.reset_owed = false;
        self.raw_total = 0;
        self.comp_total = 0;
        self.hold_len = 0;
        self.reseed_const();
    }

    const fn reseed_const(&mut self) {
        let mut i = 0;
        while i < WINDOW {
            self.prev[i] = NIL;
            i += 1;
        }
        let mut i = 0;
        while i < BUCKETS {
            self.head[i] = NIL;
            i += 1;
        }
        self.pos = load_preset(&mut self.hist);
        self.unhashed = 0;
        self.lit_pending = 0;
        // `hold` is plaintext not yet compressed. A RESET keeps it so those
        // bytes are emitted against the fresh history instead of being
        // dropped. `reset_call` clears `hold_len` before it gets here.
        let mut p = 0;
        while p + 2 < PRESET.len() {
            let h = hash(PRESET[p], PRESET[p + 1], PRESET[p + 2]);
            self.prev[p] = self.head[h];
            self.head[h] = p as u16;
            p += 1;
        }
    }

    /// After the bytes already queued, the next token is RESET.
    pub fn request_reset(&mut self) {
        self.flush_tail();
        self.reset_owed = true;
        self.pump();
    }

    /// Compress plaintext still held back for lookahead, and write its tokens.
    /// The link calls this when it is about to send, not after every byte.
    pub fn flush_tail(&mut self) {
        self.crank(true);
    }

    /// Write any literal run still held, then a queued RESET if it fits.
    pub fn pump(&mut self) {
        let _ = self.emit_lits();
        let _ = self.try_flush_reset();
    }

    pub fn reset_owed(&self) -> bool {
        self.reset_owed
    }

    pub fn raw_total(&self) -> u32 {
        self.raw_total
    }

    pub fn comp_total(&self) -> u32 {
        self.comp_total
    }

    /// Compressed bytes waiting to be handed to ARQ.
    pub fn pending(&self) -> usize {
        usize::from(self.pend_len.saturating_sub(self.pend_at))
    }

    /// Plaintext still in the lookahead hold, not yet turned into tokens.
    pub(crate) fn copy_hold(&self, out: &mut [u8]) -> usize {
        let n = usize::from(self.hold_len).min(out.len());
        if let (Some(dst), Some(src)) = (out.get_mut(..n), self.hold.get(..n)) {
            dst.copy_from_slice(src);
        }
        n
    }

    /// Take `input` into the hold and compress while the hold is full.
    /// Returns how many bytes were taken. A short count means the pending
    /// buffer is full: drain it and call again. Bytes not taken are not in
    /// the hold. Bytes in the hold are not yet all in the history; call
    /// `flush_tail` before reading the compressed tail of a burst.
    pub fn push(&mut self, input: &[u8]) -> usize {
        let mut i = 0;
        while i < input.len() {
            if self.hold_len as usize == HOLD && !self.crank(false) {
                break;
            }
            if self.hold_len as usize == HOLD {
                break;
            }
            if let Some(slot) = self.hold.get_mut(self.hold_len as usize) {
                if let Some(b) = input.get(i) {
                    *slot = *b;
                }
            }
            self.hold_len = self.hold_len.saturating_add(1);
            i += 1;
            if self.hold_len as usize == HOLD {
                let _ = self.crank(false);
            }
        }
        i
    }

    /// Compress the hold. `all` emits every byte. Otherwise the hold must be
    /// full, and two bytes stay so the next push can extend a match.
    fn crank(&mut self, all: bool) -> bool {
        if !all && (self.hold_len as usize) < HOLD {
            return true;
        }
        let n = self.hold_len as usize;
        if n == 0 {
            return true;
        }
        let mut local = [0u8; HOLD];
        if let Some(src) = self.hold.get(..n) {
            if let Some(dst) = local.get_mut(..n) {
                dst.copy_from_slice(src);
            }
        }
        let keep = if all { 0 } else { MIN_MATCH - 1 };
        let limit = n.saturating_sub(keep);
        let took = self.compress_bytes(local.get(..n).unwrap_or(&[]), limit);
        if took == 0 && limit > 0 {
            return false;
        }
        let left = n - took;
        for i in 0..left {
            let b = local.get(took + i).copied().unwrap_or(0);
            if let Some(d) = self.hold.get_mut(i) {
                *d = b;
            }
        }
        self.hold_len = left as u8;
        // True when the hold can take another byte, including when a match
        // consumed the lookahead and the hold is empty.
        all || (self.hold_len as usize) < HOLD
    }

    /// Compress `data[..limit]`, using `data` past `limit` only as lookahead.
    /// Returns how many bytes were consumed.
    fn compress_bytes(&mut self, data: &[u8], limit: usize) -> usize {
        let mut i = 0;
        while i < limit && i < data.len() {
            if self.pos >= POS_RESET {
                self.reset_owed = true;
            }
            if !self.try_flush_reset() {
                break;
            }
            let rest = data.get(i..).unwrap_or(&[]);
            self.hash_lookahead(rest);
            let (mut len, dist) = self.longest(rest);
            if len > rest.len() {
                len = rest.len();
            }
            if len >= MIN_MATCH {
                if !self.emit_lits() || !self.reserve(2) {
                    break;
                }
                let Some(bytes) = rest.get(..len) else {
                    break;
                };
                self.emit_match(len, dist);
                for (k, &b) in bytes.iter().enumerate() {
                    self.commit(b);
                    self.hash_lookahead(rest.get(k + 1..).unwrap_or(&[]));
                }
                i += len;
                continue;
            }
            if !self.can_take_lit() {
                break;
            }
            let Some(b) = rest.first().copied() else {
                break;
            };
            self.commit(b);
            self.lit_pending = self.lit_pending.saturating_add(1);
            i += 1;
        }
        if self.pending_room_for_lits() {
            let _ = self.emit_lits();
        }
        i
    }

    /// Copy queued compressed bytes into `out`.
    pub fn drain(&mut self, out: &mut [u8]) -> usize {
        let have = usize::from(self.pend_len.saturating_sub(self.pend_at));
        let n = have.min(out.len());
        let start = usize::from(self.pend_at);
        if let (Some(dst), Some(src)) = (out.get_mut(..n), self.pend.get(start..start + n)) {
            dst.copy_from_slice(src);
        }
        self.pend_at = self.pend_at.saturating_add(n as u16);
        if self.pend_at >= self.pend_len {
            self.pend_at = 0;
            self.pend_len = 0;
        }
        n
    }

    /// Give `n` bytes back to the front of the pending buffer. Used when
    /// ARQ accepts fewer bytes than `drain` just handed out.
    pub fn rewind(&mut self, n: u16) {
        self.pend_at = self.pend_at.saturating_sub(n);
    }

    fn pending_room_for_lits(&mut self) -> bool {
        let n = usize::from(self.lit_pending);
        n == 0 || self.reserve(1 + n)
    }

    fn can_take_lit(&mut self) -> bool {
        if usize::from(self.lit_pending) == MAX_LIT && !self.emit_lits() {
            return false;
        }
        let n = usize::from(self.lit_pending) + 1;
        if self.reserve(1 + n) {
            return true;
        }
        if self.lit_pending > 0 && self.emit_lits() {
            return self.reserve(2);
        }
        false
    }

    fn try_flush_reset(&mut self) -> bool {
        if !self.reset_owed {
            return true;
        }
        if !self.emit_lits() {
            return false;
        }
        if !self.reserve(1) {
            return false;
        }
        self.append(RESET);
        self.comp_total = self.comp_total.saturating_add(1);
        self.reseed_const();
        self.reset_owed = false;
        true
    }

    fn emit_lits(&mut self) -> bool {
        let n = usize::from(self.lit_pending);
        if n == 0 {
            return true;
        }
        if !self.reserve(1 + n) {
            return false;
        }
        let start = self.pos - u32::from(self.lit_pending);
        self.append((n - 1) as u8);
        for k in 0..n {
            self.append(self.hist_abs(start + k as u32));
        }
        self.raw_total = self.raw_total.saturating_add(n as u32);
        self.comp_total = self.comp_total.saturating_add(1 + n as u32);
        self.lit_pending = 0;
        true
    }

    fn emit_match(&mut self, len: usize, dist: usize) {
        let m = (len - MIN_MATCH) as u8;
        let d = dist - 1;
        let h = 0x80 | (m << 2) | (((d >> 8) as u8) & 0x03);
        self.append(h);
        self.append((d & 0xFF) as u8);
        self.raw_total = self.raw_total.saturating_add(len as u32);
        self.comp_total = self.comp_total.saturating_add(2);
    }

    fn reserve(&mut self, n: usize) -> bool {
        self.make_room();
        self.pend_len as usize + n <= PEND
    }

    fn make_room(&mut self) {
        if self.pend_at == 0 {
            return;
        }
        let n = usize::from(self.pend_len - self.pend_at);
        let at = usize::from(self.pend_at);
        for i in 0..n {
            let b = self.pend.get(at + i).copied().unwrap_or(0);
            if let Some(d) = self.pend.get_mut(i) {
                *d = b;
            }
        }
        self.pend_len = n as u16;
        self.pend_at = 0;
    }

    fn append(&mut self, b: u8) {
        if self.pend_len as usize >= PEND {
            self.make_room();
        }
        if let Some(slot) = self.pend.get_mut(self.pend_len as usize) {
            *slot = b;
            self.pend_len = self.pend_len.saturating_add(1);
        }
    }

    fn hist_abs(&self, abs: u32) -> u8 {
        self.hist.get((abs as usize) % WINDOW).copied().unwrap_or(0)
    }

    fn commit(&mut self, b: u8) {
        let slot = (self.pos as usize) % WINDOW;
        if let Some(d) = self.hist.get_mut(slot) {
            *d = b;
        }
        self.pos = self.pos.saturating_add(1);
        self.unhashed = self.unhashed.saturating_add(1);
    }

    fn byte_from(&self, abs: u32, look: &[u8]) -> Option<u8> {
        if abs < self.pos {
            Some(self.hist_abs(abs))
        } else {
            look.get((abs - self.pos) as usize).copied()
        }
    }

    fn hash_lookahead(&mut self, look: &[u8]) {
        while self.unhashed > 0 {
            let start = self.pos - self.unhashed;
            let Some(b1) = self.byte_from(start + 1, look) else {
                break;
            };
            let Some(b2) = self.byte_from(start + 2, look) else {
                break;
            };
            let b0 = self.hist_abs(start);
            self.insert(start, b0, b1, b2);
            self.unhashed -= 1;
        }
    }

    fn insert(&mut self, p: u32, a: u8, b: u8, c: u8) {
        let h = hash(a, b, c);
        let slot = (p as usize) % WINDOW;
        if let (Some(prev), Some(head)) = (self.prev.get_mut(slot), self.head.get(h).copied()) {
            *prev = head;
        }
        if let Some(head) = self.head.get_mut(h) {
            *head = p as u16;
        }
    }

    fn longest(&self, input: &[u8]) -> (usize, usize) {
        if input.len() < MIN_MATCH {
            return (0, 0);
        }
        let (Some(&a), Some(&b), Some(&c)) = (input.first(), input.get(1), input.get(2)) else {
            return (0, 0);
        };
        let mut cand = self.head.get(hash(a, b, c)).copied().unwrap_or(NIL);
        let abs = self.pos;
        let max_len = MAX_MATCH.min(input.len());
        let (mut best, mut best_d) = (0usize, 0usize);
        let mut tries = 0;
        while cand != NIL && tries < MAX_CHAIN {
            tries += 1;
            let cpos = u32::from(cand);
            if cpos >= abs {
                break;
            }
            let dist = abs - cpos;
            if dist == 0 || dist > WINDOW as u32 {
                break;
            }
            let mut l = 0;
            while l < max_len {
                let src_abs = abs - dist + l as u32;
                let src = if src_abs < self.pos {
                    self.hist_abs(src_abs)
                } else {
                    let Some(x) = input.get((src_abs - abs) as usize) else {
                        break;
                    };
                    *x
                };
                if src != input.get(l).copied().unwrap_or(!src) {
                    break;
                }
                l += 1;
            }
            if l > best {
                best = l;
                best_d = dist as usize;
                if l == max_len {
                    break;
                }
            }
            cand = self
                .prev
                .get((cpos as usize) % WINDOW)
                .copied()
                .unwrap_or(NIL);
        }
        if best >= MIN_MATCH {
            (best, best_d)
        } else {
            (0, 0)
        }
    }
}

impl Default for StreamEnc {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamDec {
    pub const fn new() -> Self {
        let mut s = StreamDec {
            hist: [0; WINDOW],
            buf: [0; INBUF],
            pos: 0,
            blen: 0,
            bat: 0,
            phase: Phase::Head,
            hdr: 0,
            err: false,
        };
        s.pos = load_preset(&mut s.hist);
        s
    }

    /// History back to the preset, and any half read token dropped.
    pub fn reset_call(&mut self) {
        self.apply_reset();
        self.blen = 0;
        self.bat = 0;
        self.err = false;
        self.hdr = 0;
    }

    pub fn errored(&self) -> bool {
        self.err
    }

    /// Free bytes in the input hold.
    pub fn feed_room(&self) -> usize {
        INBUF - usize::from(self.blen.saturating_sub(self.bat))
    }

    /// Take compressed bytes. Returns how many were stored.
    pub fn feed(&mut self, data: &[u8]) -> usize {
        self.compact();
        let n = data.len().min(INBUF - usize::from(self.blen));
        let at = usize::from(self.blen);
        if let (Some(dst), Some(src)) = (self.buf.get_mut(at..at + n), data.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.blen = self.blen.saturating_add(n as u8);
        n
    }

    /// Write plaintext into `out`. `Ok(0)` means more compressed bytes are
    /// needed (or `out` is empty). A bad distance is `Err`, and it sticks
    /// until `reset_call`.
    pub fn pull(&mut self, out: &mut [u8]) -> Result<usize, LzError> {
        if self.err {
            return Err(LzError::BadDistance);
        }
        let mut n = 0;
        while n < out.len() {
            match self.phase {
                Phase::Head => {
                    let Some(h) = self.take() else {
                        break;
                    };
                    if h == RESET {
                        self.apply_reset();
                        continue;
                    }
                    if h < 0x80 {
                        self.phase = Phase::Lit { left: h + 1 };
                    } else {
                        self.hdr = h;
                        self.phase = Phase::Dist {
                            len: ((h >> 2) & 0x1F) + MIN_MATCH as u8,
                        };
                    }
                }
                Phase::Dist { len } => {
                    let Some(lo) = self.take() else {
                        break;
                    };
                    let dist = (u16::from(self.hdr & 0x03) << 8) | u16::from(lo);
                    let dist = dist.saturating_add(1);
                    if u32::from(dist) == 0
                        || u32::from(dist) > self.pos
                        || u32::from(dist) > WINDOW as u32
                    {
                        self.err = true;
                        self.phase = Phase::Head;
                        if n > 0 {
                            return Ok(n);
                        }
                        return Err(LzError::BadDistance);
                    }
                    self.phase = Phase::Copy {
                        left: u16::from(len),
                        dist,
                    };
                }
                Phase::Lit { left } => {
                    if left == 0 {
                        self.phase = Phase::Head;
                        continue;
                    }
                    let Some(b) = self.take() else {
                        break;
                    };
                    self.commit(b);
                    if let Some(d) = out.get_mut(n) {
                        *d = b;
                    }
                    n += 1;
                    let left = left - 1;
                    self.phase = if left == 0 {
                        Phase::Head
                    } else {
                        Phase::Lit { left }
                    };
                }
                Phase::Copy { left, dist } => {
                    if left == 0 {
                        self.phase = Phase::Head;
                        continue;
                    }
                    let b = match self.copy_one(dist) {
                        Ok(b) => b,
                        Err(e) => {
                            if n > 0 {
                                return Ok(n);
                            }
                            return Err(e);
                        }
                    };
                    if let Some(d) = out.get_mut(n) {
                        *d = b;
                    }
                    n += 1;
                    let left = left - 1;
                    self.phase = if left == 0 {
                        Phase::Head
                    } else {
                        Phase::Copy { left, dist }
                    };
                }
            }
        }
        Ok(n)
    }

    fn apply_reset(&mut self) {
        self.pos = load_preset(&mut self.hist);
        self.phase = Phase::Head;
    }

    fn compact(&mut self) {
        if self.bat == 0 {
            return;
        }
        let n = usize::from(self.blen.saturating_sub(self.bat));
        let at = usize::from(self.bat);
        for i in 0..n {
            let b = self.buf.get(at + i).copied().unwrap_or(0);
            if let Some(d) = self.buf.get_mut(i) {
                *d = b;
            }
        }
        self.blen = n as u8;
        self.bat = 0;
    }

    fn take(&mut self) -> Option<u8> {
        if self.bat >= self.blen {
            return None;
        }
        let b = self.buf.get(usize::from(self.bat)).copied()?;
        self.bat = self.bat.saturating_add(1);
        if self.bat >= self.blen {
            self.bat = 0;
            self.blen = 0;
        }
        Some(b)
    }

    fn commit(&mut self, b: u8) {
        let slot = (self.pos as usize) % WINDOW;
        if let Some(d) = self.hist.get_mut(slot) {
            *d = b;
        }
        self.pos = self.pos.saturating_add(1);
    }

    fn copy_one(&mut self, dist: u16) -> Result<u8, LzError> {
        let d = u32::from(dist);
        if d == 0 || d > self.pos || d > WINDOW as u32 {
            self.err = true;
            self.phase = Phase::Head;
            return Err(LzError::BadDistance);
        }
        let b = self
            .hist
            .get(((self.pos - d) as usize) % WINDOW)
            .copied()
            .ok_or(LzError::BadDistance)?;
        self.commit(b);
        Ok(b)
    }
}

impl Default for StreamDec {
    fn default() -> Self {
        Self::new()
    }
}

/// Compress `plain` to completion and decode it back. Test helper.
#[cfg(test)]
fn round_trip(plain: &[u8]) -> Result<Vec<u8>, LzError> {
    let comp = compress_all(plain);
    decode_all(&comp)
}

#[cfg(test)]
fn compress_all(plain: &[u8]) -> Vec<u8> {
    let mut enc = StreamEnc::new();
    let mut out = Vec::new();
    let mut off = 0;
    while off < plain.len() {
        let n = enc.push(&plain[off..]);
        assert!(n > 0, "encoder made no progress at {off}");
        off += n;
        let mut buf = [0u8; 64];
        loop {
            let k = enc.drain(&mut buf);
            if k == 0 {
                break;
            }
            out.extend_from_slice(&buf[..k]);
        }
    }
    enc.flush_tail();
    enc.pump();
    let mut buf = [0u8; 64];
    loop {
        let k = enc.drain(&mut buf);
        if k == 0 {
            break;
        }
        out.extend_from_slice(&buf[..k]);
    }
    assert!(enc.pending() == 0);
    out
}

#[cfg(test)]
fn decode_all(comp: &[u8]) -> Result<Vec<u8>, LzError> {
    let mut dec = StreamDec::new();
    let mut out = Vec::new();
    let mut off = 0;
    let mut guard = 0;
    // Keep pulling after the input is finished: a match can still have
    // bytes to copy from the history.
    loop {
        guard += 1;
        assert!(guard < comp.len() + out.len() + 8, "decoder stuck");
        let mut buf = [0u8; 32];
        match dec.pull(&mut buf)? {
            0 => {
                if off >= comp.len() {
                    break;
                }
                let n = dec.feed(&comp[off..]);
                assert!(n > 0, "no room and no output");
                off += n;
            }
            n => out.extend_from_slice(&buf[..n]),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_runs_and_screens() {
        let runs: [&[u8]; 4] = [b"", b"a", b"ab", b"hello"];
        for s in runs {
            assert_eq!(round_trip(s).unwrap(), s);
        }
        let z = vec![b'Z'; 300];
        assert_eq!(round_trip(&z).unwrap(), z);
        let mut mixed = b"\x1b[0m\x1b[2J\x1b[H".to_vec();
        mixed.extend(std::iter::repeat_n(0xCDu8, 80));
        mixed.extend_from_slice(b"\r\nWelcome to AXIS\r\n");
        mixed.extend(std::iter::repeat_n(0xDBu8, 40));
        assert_eq!(round_trip(&mixed).unwrap(), mixed);
    }

    #[test]
    fn x_cr_run_shrinks() {
        let s: Vec<u8> = b"x\r".iter().copied().cycle().take(400).collect();
        let c = compress_all(&s);
        assert_eq!(decode_all(&c).unwrap(), s);
        assert!(c.len() < 80, "x\\r compressed to {} bytes", c.len());
    }

    #[test]
    fn one_byte_pushes_round_trip() {
        let s = b"\x1b[1;37;44mMAIN MENU\x1b[0m\r\n\r\n";
        let mut enc = StreamEnc::new();
        let mut comp = Vec::new();
        let mut buf = [0u8; 8];
        for &b in s {
            assert_eq!(enc.push(&[b]), 1);
            loop {
                let k = enc.drain(&mut buf);
                if k == 0 {
                    break;
                }
                comp.extend_from_slice(&buf[..k]);
            }
        }
        enc.flush_tail();
        loop {
            let k = enc.drain(&mut buf);
            if k == 0 {
                break;
            }
            comp.extend_from_slice(&buf[..k]);
        }
        assert_eq!(decode_all(&comp).unwrap(), s);
    }

    #[test]
    fn reset_keeps_plaintext_still_in_the_hold() {
        // Fill the pending buffer and stop, so a reset runs while accepted
        // plaintext is still in the lookahead. Those bytes must come back.
        let data: Vec<u8> = (0..600u16)
            .map(|i| (i.wrapping_mul(13).wrapping_add(9)) as u8)
            .collect();
        let mut enc = StreamEnc::new();
        let mut off = 0;
        while off < data.len() {
            let n = enc.push(&data[off..]);
            if n == 0 {
                break;
            }
            off += n;
        }
        assert!(off < data.len(), "pending buffer never filled");
        assert!(enc.hold_len > 0, "nothing left in the lookahead");
        enc.request_reset();
        let mut comp = Vec::new();
        let mut buf = [0u8; 64];
        let drain_all = |enc: &mut StreamEnc, comp: &mut Vec<u8>, buf: &mut [u8]| loop {
            enc.pump();
            let k = enc.drain(buf);
            if k == 0 {
                break;
            }
            comp.extend_from_slice(&buf[..k]);
        };
        drain_all(&mut enc, &mut comp, &mut buf);
        enc.flush_tail();
        drain_all(&mut enc, &mut comp, &mut buf);
        while off < data.len() {
            let n = enc.push(&data[off..]);
            assert!(n > 0, "no progress at {off}");
            off += n;
            drain_all(&mut enc, &mut comp, &mut buf);
        }
        enc.flush_tail();
        enc.pump();
        drain_all(&mut enc, &mut comp, &mut buf);
        assert_eq!(decode_all(&comp).unwrap(), data);
    }

    #[test]
    fn reset_mid_stream_matches_a_fresh_decoder() {
        let a = b"\x1b[2J\x1b[H\x1b[1;37;44mMAIN MENU\r\n".as_slice();
        let mut a = a.to_vec();
        a.extend(std::iter::repeat_n(0xCDu8, 50));
        let b = b"FILE AREAS are different text after the reset, 12345".as_slice();
        let mut enc = StreamEnc::new();
        let mut comp = Vec::new();
        let take = |enc: &mut StreamEnc, src: &[u8], comp: &mut Vec<u8>| {
            let mut off = 0;
            let mut buf = [0u8; 32];
            while off < src.len() {
                let n = enc.push(&src[off..]);
                assert!(n > 0);
                off += n;
                loop {
                    let k = enc.drain(&mut buf);
                    if k == 0 {
                        break;
                    }
                    comp.extend_from_slice(&buf[..k]);
                }
            }
        };
        take(&mut enc, &a, &mut comp);
        enc.request_reset();
        assert!(!enc.reset_owed());
        let mut buf = [0u8; 32];
        loop {
            let k = enc.drain(&mut buf);
            if k == 0 {
                break;
            }
            comp.extend_from_slice(&buf[..k]);
        }
        let at_reset = comp.len();
        take(&mut enc, b, &mut comp);
        enc.flush_tail();
        loop {
            let k = enc.drain(&mut buf);
            if k == 0 {
                break;
            }
            comp.extend_from_slice(&buf[..k]);
        }
        let mut want = a.clone();
        want.extend_from_slice(b);
        assert_eq!(decode_all(&comp).unwrap(), want);
        // The suffix after RESET stands alone: a decoder that did not see
        // `a` still produces `b`.
        let tail = &comp[at_reset..];
        assert_eq!(decode_all(tail).unwrap(), b);
    }

    #[test]
    fn bad_distance_sticks_until_reset() {
        let mut dec = StreamDec::new();
        // Match of length 3 at distance 1024. The history is only the
        // preset (567 bytes), so the distance is past the start.
        assert_eq!(dec.feed(&[0x83, 0xFF]), 2);
        assert_eq!(dec.pull(&mut [0u8; 8]), Err(LzError::BadDistance));
        assert!(dec.errored());
        assert_eq!(dec.pull(&mut [0u8; 8]), Err(LzError::BadDistance));
        dec.reset_call();
        assert_eq!(dec.feed(&[0x00, b'Q']), 2);
        let mut o = [0u8; 4];
        assert_eq!(dec.pull(&mut o), Ok(1));
        assert_eq!(o[0], b'Q');
    }
}
