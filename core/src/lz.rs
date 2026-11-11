//! LZ compression for the payload of one Data frame (`FLAG_LZ`).
//!
//! Every frame is compressed on its own: LZ77 over a virtual buffer that is
//! the fixed `PRESET` dictionary followed by the frame's own bytes. There is
//! no history shared between frames, so ARQ can resend, reorder or drop any
//! frame without the two ends ever disagreeing about the dictionary.
//!
//! # Stream format
//!
//! A compressed payload is a sequence of tokens, read until the payload
//! ends. The first byte of a token says which kind it is:
//!
//! | header byte | token |
//! |-------------|-------|
//! | `0LLL LLLL` | literal run: `L + 1` bytes (1 to 128) follow and are copied as is |
//! | `1MMM MMDD` `dddd dddd` | match: length `M + 3` (3 to 34), distance `(DD << 8 \| dddd dddd) + 1` (1 to 1024) |
//!
//! A match copies `length` bytes, one at a time, starting `distance` bytes
//! back from the current end of the virtual buffer (`PRESET` then the bytes
//! produced so far). The source may lie in `PRESET`, in the output, or run
//! from one into the other, and may overlap the bytes being written (a
//! distance of 1 repeats the last byte). The distance can never reach in
//! front of `PRESET`: at most `PRESET.len()` plus the bytes produced.
//!
//! The compressor is greedy (longest match at each position, earliest found
//! wins a tie) and deterministic: the same input always gives the same
//! bytes, so a resent chunk goes out identical to the first try. It returns
//! `None` unless the result is at least `MIN_GAIN` (8) bytes shorter than
//! the input; the sender then sends the chunk plain, without `FLAG_LZ`.
//!
//! The decompressor never panics and never reads or writes out of bounds on
//! any input: a truncated token, a distance before the start of `PRESET`, or
//! output longer than `out` is an `LzError`.
//!
//! # Preset dictionary
//!
//! 567 bytes of what BBS screens are made of: ANSI SGR sequences
//! (reset, bold and normal for the 8 foreground colors, the 8 backgrounds),
//! clear screen, home and cursor moves, CRLF, runs of spaces, CP437 box
//! drawing lines and corners (single and double), CP437 block characters
//! in runs, the same blocks and lines in UTF-8, and a few prompt words.
//! It is part of the air format: changing one byte of it breaks LZ calls
//! with older builds, so it never changes without a new capability bit.

use crate::arq::CHUNK;

/// Least saving, in bytes, for a frame to go out compressed.
pub const MIN_GAIN: usize = 8;
/// Shortest match.
pub const MIN_MATCH: usize = 3;
/// Longest match (5 bits of length).
pub const MAX_MATCH: usize = 34;
/// Longest literal run (7 bits of count).
pub const MAX_LITERALS: usize = 128;
/// Farthest match distance (10 bits).
pub const MAX_DIST: usize = 1024;
/// Longest input `compress` takes: one ARQ chunk.
pub const MAX_INPUT: usize = CHUNK;

/// Why a compressed payload was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LzError {
    /// The payload ends inside a token.
    Truncated,
    /// A match reaches in front of the preset dictionary.
    BadDistance,
    /// The output would not fit in `out`.
    Overflow,
}

/// The fixed preset dictionary (see the module notes).
pub const PRESET: &[u8] = b"\
\x1b[0m\x1b[2J\x1b[H\x1b[0;1;37;40m\x1b[0;37;40m\x1b[1;37;44m\x1b[K\x1b[1;1H\x1b[24;1H\
\x1b[s\x1b[u\x1b[?25l\x1b[?25h\x1b[A\x1b[B\x1b[C\x1b[D\x1b[2;1H\x1b[10;20H\
\x1b[1;30m\x1b[1;31m\x1b[1;32m\x1b[1;33m\x1b[1;34m\x1b[1;35m\x1b[1;36m\x1b[1;37m\
\x1b[0;30m\x1b[0;31m\x1b[0;32m\x1b[0;33m\x1b[0;34m\x1b[0;35m\x1b[0;36m\x1b[0;37m\
\x1b[40m\x1b[41m\x1b[42m\x1b[43m\x1b[44m\x1b[45m\x1b[46m\x1b[47m\
\x1b[30m\x1b[31m\x1b[32m\x1b[33m\x1b[34m\x1b[35m\x1b[36m\x1b[37m\
\r\n\r\n                                  \
\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\xC4\
\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\xCD\
\xDA\xC4\xC4\xBF\xC0\xC4\xC4\xD9\xC3\xC4\xB4\xC2\xC4\xC1\xC5\xB3  \xB3\
\xC9\xCD\xCD\xBB\xC8\xCD\xCD\xBC\xCC\xCD\xB9\xCB\xCD\xCA\xCE\xBA  \xBA\
\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\xDB\
\xB0\xB0\xB0\xB0\xB0\xB0\xB0\xB0\xB1\xB1\xB1\xB1\xB1\xB1\xB1\xB1\
\xB2\xB2\xB2\xB2\xB2\xB2\xB2\xB2\xDC\xDC\xDC\xDC\xDC\xDC\xDC\xDC\
\xDF\xDF\xDF\xDF\xDF\xDF\xDF\xDF\
\xDB\xDB\xB2\xB2\xB1\xB1\xB0\xB0\xDF\xDC\
\xE2\x96\x88\xE2\x96\x88\xE2\x96\x93\xE2\x96\x93\xE2\x96\x92\xE2\x96\x92\xE2\x96\x91\xE2\x96\x91\
\xE2\x95\x90\xE2\x95\x90\xE2\x95\x91\xE2\x94\x80\xE2\x94\x80\xE2\x94\x82\
Press any key (Y/N)? Enter your password: Welcome to \
";

/// Length of `PRESET`.
const P: usize = PRESET.len();

// Every distance into PRESET plus a full chunk fits the 10 bit field, and
// every virtual position fits a u16 below the NIL marker.
const _: () = assert!(P + MAX_INPUT <= MAX_DIST);
const _: () = assert!(P + MAX_INPUT < NIL as usize);

const NIL: u16 = u16::MAX;
const BUCKETS: usize = 256;
/// Candidates looked at per position (bounds the time per frame).
const MAX_CHAIN: usize = 64;

/// Bucket of the 3 bytes starting at a position.
const fn hash(a: u8, b: u8, c: u8) -> usize {
    let x = (a as u32) | ((b as u32) << 8) | ((c as u32) << 16);
    (x.wrapping_mul(0x9E37_79B1) >> 24) as usize
}

/// Hash chains over PRESET, built at compile time: the newest position of
/// each bucket, and for each position the previous one in its bucket.
const fn preset_chains() -> ([u16; BUCKETS], [u16; P]) {
    let mut head = [NIL; BUCKETS];
    let mut prev = [NIL; P];
    let mut i = 0;
    while i + 2 < P {
        let h = hash(PRESET[i], PRESET[i + 1], PRESET[i + 2]);
        prev[i] = head[h];
        head[h] = i as u16;
        i += 1;
    }
    (head, prev)
}

const CHAINS: ([u16; BUCKETS], [u16; P]) = preset_chains();
const PRESET_HEAD: [u16; BUCKETS] = CHAINS.0;
const PRESET_PREV: [u16; P] = CHAINS.1;

/// Byte `v` of the virtual buffer PRESET followed by `input`.
fn at(input: &[u8], v: usize) -> Option<u8> {
    if v < P {
        PRESET.get(v).copied()
    } else {
        input.get(v - P).copied()
    }
}

/// Compressor state: the hash chains, PRESET's from the constants and the
/// input's in `prev`.
struct Chains {
    head: [u16; BUCKETS],
    prev: [u16; MAX_INPUT],
}

impl Chains {
    fn prev_of(&self, v: usize) -> u16 {
        if v < P {
            PRESET_PREV.get(v).copied().unwrap_or(NIL)
        } else {
            self.prev.get(v - P).copied().unwrap_or(NIL)
        }
    }

    /// Add input position `i` (needs 3 bytes from there).
    fn insert(&mut self, input: &[u8], i: usize) {
        let (Some(a), Some(b), Some(c)) = (input.get(i), input.get(i + 1), input.get(i + 2)) else {
            return;
        };
        let h = hash(*a, *b, *c);
        if let (Some(hd), Some(pv)) = (self.head.get_mut(h), self.prev.get_mut(i)) {
            *pv = *hd;
            *hd = (P + i) as u16;
        }
    }

    /// Longest match for input position `i`: (length, distance), length 0
    /// when there is none of at least MIN_MATCH.
    fn longest(&self, input: &[u8], i: usize) -> (usize, usize) {
        let n = input.len();
        if i + MIN_MATCH > n {
            return (0, 0);
        }
        let v = P + i;
        let (Some(a), Some(b), Some(c)) = (input.get(i), input.get(i + 1), input.get(i + 2)) else {
            return (0, 0);
        };
        let max = MAX_MATCH.min(n - i);
        let mut cand = self.head.get(hash(*a, *b, *c)).copied().unwrap_or(NIL);
        let (mut best, mut dist) = (0, 0);
        let mut tries = 0;
        while cand != NIL && tries < MAX_CHAIN {
            tries += 1;
            let c = usize::from(cand);
            if c >= v || v - c > MAX_DIST {
                break;
            }
            let mut l = 0;
            // `v + l` is always in the input (l < max <= n - i).
            while l < max {
                let x = at(input, c + l);
                if x.is_none() || x != input.get(i + l).copied() {
                    break;
                }
                l += 1;
            }
            if l > best {
                best = l;
                dist = v - c;
                if l == max {
                    break;
                }
            }
            cand = self.prev_of(c);
        }
        if best >= MIN_MATCH {
            (best, dist)
        } else {
            (0, 0)
        }
    }
}

/// Bounded output writer: any write past `limit` fails.
struct Out<'a> {
    out: &'a mut [u8],
    pos: usize,
    limit: usize,
}

impl Out<'_> {
    fn byte(&mut self, b: u8) -> Option<()> {
        if self.pos >= self.limit {
            return None;
        }
        *self.out.get_mut(self.pos)? = b;
        self.pos += 1;
        Some(())
    }

    fn literals(&mut self, mut lit: &[u8]) -> Option<()> {
        while !lit.is_empty() {
            let k = lit.len().min(MAX_LITERALS);
            let (run, rest) = lit.split_at_checked(k)?;
            self.byte((k - 1) as u8)?;
            for &b in run {
                self.byte(b)?;
            }
            lit = rest;
        }
        Some(())
    }

    fn matched(&mut self, len: usize, dist: usize) -> Option<()> {
        let m = (len - MIN_MATCH) as u8;
        let d = dist - 1;
        self.byte(0x80 | (m << 2) | ((d >> 8) as u8 & 0x03))?;
        self.byte((d & 0xFF) as u8)
    }
}

/// Compress `input` (at most `MAX_INPUT` bytes) into `out`. `None` when it
/// does not save at least `MIN_GAIN` bytes, when the input is too long, or
/// when `out` is too small for the result.
pub fn compress(input: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = input.len();
    if n > MAX_INPUT || n <= MIN_GAIN {
        return None;
    }
    let limit = (n - MIN_GAIN).min(out.len());
    let mut w = Out { out, pos: 0, limit };
    let mut ch = Chains {
        head: PRESET_HEAD,
        prev: [NIL; MAX_INPUT],
    };
    let (mut i, mut lit) = (0, 0);
    while i < n {
        let (len, dist) = ch.longest(input, i);
        if len >= MIN_MATCH {
            w.literals(input.get(lit..i)?)?;
            w.matched(len, dist)?;
            for k in i..i + len {
                ch.insert(input, k);
            }
            i += len;
            lit = i;
        } else {
            ch.insert(input, i);
            i += 1;
        }
    }
    w.literals(input.get(lit..n)?)?;
    Some(w.pos)
}

/// Decompress one payload into `out`; the number of bytes produced.
pub fn decompress(input: &[u8], out: &mut [u8]) -> Result<usize, LzError> {
    let (mut ip, mut op) = (0, 0);
    while let Some(&h) = input.get(ip) {
        ip += 1;
        if h & 0x80 == 0 {
            let k = usize::from(h) + 1;
            let src = input.get(ip..ip + k).ok_or(LzError::Truncated)?;
            let dst = out.get_mut(op..op + k).ok_or(LzError::Overflow)?;
            dst.copy_from_slice(src);
            ip += k;
            op += k;
        } else {
            let lo = *input.get(ip).ok_or(LzError::Truncated)?;
            ip += 1;
            let len = usize::from((h >> 2) & 0x1F) + MIN_MATCH;
            let dist = ((usize::from(h & 0x03) << 8) | usize::from(lo)) + 1;
            let v = P + op;
            if dist > v {
                return Err(LzError::BadDistance);
            }
            if op + len > out.len() {
                return Err(LzError::Overflow);
            }
            // Byte by byte: the source may overlap the bytes being written.
            for s in (v - dist..).take(len) {
                let b = if s < P {
                    PRESET.get(s).copied()
                } else {
                    out.get(s - P).copied()
                };
                let b = b.ok_or(LzError::BadDistance)?;
                *out.get_mut(op).ok_or(LzError::Overflow)? = b;
                op += 1;
            }
        }
    }
    Ok(op)
}
