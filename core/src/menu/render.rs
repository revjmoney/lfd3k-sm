//! ANSI writer for the menu: one 80x24 screen, 16 colors, CP437 boxes.
//!
//! Nothing here holds the screen. A row is composed on the stack from the
//! live state (`Row`, at most `ROW_BUF` bytes: a cursor move, then the
//! cells with a color change only where the color changes), hashed, and
//! compared with the hash of what the terminal already shows on that row
//! (`Render::hashes`). Only rows whose hash differs are sent, so after the
//! first paint a key press or the once a second refresh redraws only the
//! lines that changed.
//!
//! Backpressure: a row goes into the host output whole or not at all, and
//! only while `OUT_RESERVE` bytes stay free behind it. When the output is
//! full the writer stops at that row; the row stays dirty and is composed
//! again (from the state at that time) on the next pump. So a slow
//! terminal only slows the redraw, the modem task never waits, and the
//! reserve always has room for the leave sequence and the result codes.
//!
//! Row 24 is written up to column 79 only, then erased to the end with the
//! status color (`ESC[K`), so no terminal ever scrolls from a write into
//! the bottom right cell.

/// Screen size.
pub const ROWS: u8 = 24;
pub const COLS: u8 = 80;
/// Longest row in bytes: cursor move, 80 cells and up to 25 color changes.
pub const ROW_BUF: usize = 400;
/// Every row is dirty.
const ALL: u32 = (1u32 << ROWS) - 1;
/// Room kept at the end of a row: one color change and a cell, then the
/// erase or the attribute reset that ends a row.
const TAIL: usize = 32;

/// Sent once when the menu opens: reset colors, clear, home, hide cursor.
pub const PROLOGUE: &[u8] = b"\x1b[0m\x1b[2J\x1b[H\x1b[?25l";
/// Sent when the menu closes: reset colors, clear, home, show cursor.
pub const LEAVE: &[u8] = b"\x1b[0m\x1b[2J\x1b[H\x1b[?25h";

/// A color pair: foreground 0 to 15 in the low nibble (8 to 15 bright),
/// background 0 to 7 in bits 4 to 6. ANSI color order: black, red, green,
/// yellow, blue, magenta, cyan, white.
pub const fn attr(fg: u8, bg: u8) -> u8 {
    (fg & 15) | ((bg & 7) << 4)
}

/// One row being composed.
pub struct Row {
    buf: [u8; ROW_BUF],
    n: usize,
    /// Next column to write (0 based).
    col: u8,
    /// Columns at and past this one are not drawn (clipping).
    pub limit: u8,
    /// Color last sent in this row, 0xFF before the first.
    cur: u8,
    /// The buffer is nearly full: the rest of the row is clipped (a row
    /// only ever ends on a cell boundary, never inside an escape).
    full: bool,
}

impl Row {
    /// Row `r` (0 based): starts with the cursor move to its first column.
    pub fn new(r: u8) -> Row {
        let mut row = Row {
            buf: [0; ROW_BUF],
            n: 0,
            col: 0,
            limit: COLS,
            cur: 0xFF,
            full: false,
        };
        row.raw(b"\x1b[");
        row.dec(u32::from(r) + 1);
        row.raw(b";1H");
        row
    }

    fn raw(&mut self, s: &[u8]) {
        for &b in s {
            if let Some(d) = self.buf.get_mut(self.n) {
                *d = b;
                self.n += 1;
            }
        }
    }

    fn dec(&mut self, v: u32) {
        let mut tmp = [0u8; 10];
        let mut i = tmp.len();
        let mut v = v;
        loop {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'0' + (v % 10) as u8;
            }
            v /= 10;
            if v == 0 || i == 0 {
                break;
            }
        }
        let s = tmp;
        self.raw(s.get(i..).unwrap_or(&[]));
    }

    fn set(&mut self, a: u8) {
        if a == self.cur {
            return;
        }
        self.cur = a;
        let fg = a & 15;
        self.raw(b"\x1b[0;");
        if fg >= 8 {
            self.raw(b"1;");
        }
        self.raw(&[
            b'3',
            b'0' + (fg & 7),
            b';',
            b'4',
            b'0' + ((a >> 4) & 7),
            b'm',
        ]);
    }

    /// The next column to write.
    pub fn col(&self) -> u8 {
        self.col
    }

    /// One cell (any byte 0x20 and up; CP437 above 0x7F). Control bytes
    /// become a space, cells past `limit` are dropped.
    pub fn cell(&mut self, a: u8, c: u8) -> &mut Self {
        if self.full || self.n + TAIL > ROW_BUF {
            // Clip here; every loop over `limit` then ends.
            self.full = true;
            self.limit = self.col;
        }
        if self.col >= self.limit {
            return self;
        }
        self.set(a);
        self.raw(&[if c < 0x20 || c == 0x7F { b' ' } else { c }]);
        self.col += 1;
        self
    }

    pub fn text(&mut self, a: u8, s: &[u8]) -> &mut Self {
        for &c in s {
            self.cell(a, c);
        }
        self
    }

    pub fn str(&mut self, a: u8, s: &str) -> &mut Self {
        self.text(a, s.as_bytes())
    }

    /// `n` copies of `c`.
    pub fn rep(&mut self, a: u8, c: u8, n: u8) -> &mut Self {
        for _ in 0..n {
            self.cell(a, c);
        }
        self
    }

    /// Spaces up to (not including) column `to`.
    pub fn pad_to(&mut self, a: u8, to: u8) -> &mut Self {
        while self.col < to.min(self.limit) {
            self.cell(a, b' ');
        }
        self
    }

    /// Decimal number.
    pub fn num(&mut self, a: u8, v: u32) -> &mut Self {
        let mut tmp = [0u8; 10];
        let mut i = tmp.len();
        let mut v = v;
        loop {
            i -= 1;
            if let Some(d) = tmp.get_mut(i) {
                *d = b'0' + (v % 10) as u8;
            }
            v /= 10;
            if v == 0 || i == 0 {
                break;
            }
        }
        let s = tmp;
        self.text(a, s.get(i..).unwrap_or(&[]))
    }

    /// Signed decimal number.
    pub fn int(&mut self, a: u8, v: i32) -> &mut Self {
        if v < 0 {
            self.cell(a, b'-');
        }
        self.num(a, v.unsigned_abs())
    }

    /// `s` centred between columns `from` and `to`, padded with `a`.
    pub fn center(&mut self, a: u8, s: &[u8], from: u8, to: u8) -> &mut Self {
        let w = usize::from(to.saturating_sub(from));
        let lead = w.saturating_sub(s.len()) / 2;
        self.pad_to(a, from.saturating_add(lead as u8));
        self.text(a, s);
        self.pad_to(a, to)
    }

    /// Pad the row to the right edge. The last row stops at column 79 and
    /// erases the rest with its color, so the bottom right cell is never
    /// written.
    pub fn finish(&mut self, a: u8, last: bool) {
        self.limit = COLS;
        if self.full {
            // Clipped: leave the terminal with plain attributes.
            self.raw(b"\x1b[0m");
            return;
        }
        if last {
            self.pad_to(a, COLS - 1);
            self.set(a);
            self.raw(b"\x1b[K");
        } else {
            self.pad_to(a, COLS);
        }
    }

    pub fn bytes(&self) -> &[u8] {
        self.buf.get(..self.n).unwrap_or(&[])
    }

    /// FNV-1a of the row's bytes.
    pub fn hash(&self) -> u32 {
        let mut h: u32 = 0x811C_9DC5;
        for &b in self.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }
}

/// What the terminal shows, as one hash per row, and what is left to send.
pub struct Render {
    hashes: [u32; ROWS as usize],
    /// Rows whose hash is known (sent completely since the clear).
    valid: u32,
    /// Rows to compose and compare again.
    dirty: u32,
    /// The clear screen still has to go out.
    pub prologue: bool,
}

impl Render {
    pub const fn new() -> Render {
        Render {
            hashes: [0; ROWS as usize],
            valid: 0,
            dirty: 0,
            prologue: false,
        }
    }

    /// A fresh screen: clear it, then paint every row.
    pub fn start(&mut self) {
        self.valid = 0;
        self.dirty = ALL;
        self.prologue = true;
    }

    /// Compare every row again (after a key, an event, or once a second).
    pub fn refresh(&mut self) {
        self.dirty = ALL;
    }

    /// The first row that needs comparing, if any.
    pub fn next_dirty(&self) -> Option<u8> {
        if self.dirty == 0 {
            None
        } else {
            Some(self.dirty.trailing_zeros() as u8)
        }
    }

    /// Row `r` composed to hash `h` already shows on the terminal.
    pub fn shows(&self, r: u8, h: u32) -> bool {
        let bit = 1u32 << (r % ROWS);
        self.valid & bit != 0 && self.hashes.get(usize::from(r)) == Some(&h)
    }

    /// Row `r` with hash `h` is on the terminal now.
    pub fn done(&mut self, r: u8, h: u32) {
        let bit = 1u32 << (r % ROWS);
        if let Some(slot) = self.hashes.get_mut(usize::from(r)) {
            *slot = h;
        }
        self.valid |= bit;
        self.dirty &= !bit;
    }
}

impl Default for Render {
    fn default() -> Self {
        Self::new()
    }
}
