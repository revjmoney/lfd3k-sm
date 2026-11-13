//! Jitter buffer keyed by the voice frame counter.

use std::collections::BTreeMap;

/// Frames this far past the next expected one restart the buffer.
const RESYNC_AHEAD: u32 = 64;

pub struct Jitter {
    depth: usize,
    frames: BTreeMap<u32, Vec<i16>>,
    next: u32,
    last_played: Option<u32>,
    started: bool,
}

impl Jitter {
    /// `depth_frames` frames are collected before playout starts (and again
    /// after an underrun).
    pub fn new(depth_frames: usize) -> Self {
        Jitter {
            depth: depth_frames.max(1),
            frames: BTreeMap::new(),
            next: 0,
            last_played: None,
            started: false,
        }
    }

    pub fn push(&mut self, counter: u32, pcm: Vec<i16>) {
        if self.last_played.is_some_and(|l| counter <= l) {
            return; // already played or concealed
        }
        if self.started && counter.wrapping_sub(self.next) > RESYNC_AHEAD && counter > self.next {
            // The sender jumped far ahead: start over.
            self.frames.clear();
            self.started = false;
        }
        self.frames.insert(counter, pcm);
        // Bound memory if the reader stalls.
        while self.frames.len() > self.depth + RESYNC_AHEAD as usize {
            self.frames.pop_first();
        }
        if !self.started {
            if let Some((&first, _)) = self.frames.first_key_value() {
                self.next = first;
            }
        }
    }

    /// Frames waiting to be played. With `pop` giving `None`, a count above
    /// 0 means a frame was lost or is late (conceal); 0 means the buffer ran
    /// dry or has not started (the talker stopped, or not primed yet).
    pub fn buffered(&self) -> usize {
        self.frames.len()
    }

    /// True while playing out (primed, no underrun since).
    pub fn started(&self) -> bool {
        self.started
    }

    /// Next frame to play, or `None` when the decoder should conceal (not yet
    /// primed, frame lost or late, or nothing buffered).
    pub fn pop(&mut self) -> Option<Vec<i16>> {
        if !self.started {
            if self.frames.len() < self.depth {
                return None;
            }
            self.started = true;
        }
        if self.frames.is_empty() {
            // Underrun: conceal and refill before playing on.
            self.started = false;
            return None;
        }
        let want = self.next;
        self.next = self.next.wrapping_add(1);
        self.last_played = Some(want);
        self.frames.remove(&want)
    }
}
