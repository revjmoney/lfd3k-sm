//! Retrain: tier step up and down from the SNR average, resync after loss.

use super::*;

impl Link {
    /// Extra wait the answerer adds before starting anything the caller
    /// may start too (retrains, probes), so the two never collide in step.
    pub(super) fn role_off(&self, rto: u32) -> u32 {
        if self.role == Role::Answerer {
            rto / 2
        } else {
            0
        }
    }

    /// A TierSet waits in the queue.
    fn q_has_tierset(&self) -> bool {
        (0..self.q_len).any(|i| self.q.get((self.q_head + i) % QLEN).is_some_and(is_tierset))
    }

    /// Drop the pending retrain and any of its TierSets still queued.
    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
        self.q_retain(|f| !is_tierset(f));
    }

    /// Retrain request inside a call: TierSet on the current tier.
    pub(super) fn request(&mut self, t: Tier, now: u32) {
        self.clear_pending();
        self.pending = Some(t);
        self.pend_last = now;
        self.pend_tries = 1;
        let rto = rto_ms(self.tier);
        self.pend_wait = rto.saturating_add(self.jitter(rto / 2));
        let cur = self.tier;
        self.enq(cur, FrameType::TierSet, 0, &[t.index()], true, false);
    }

    pub(super) fn switch(&mut self, t: Tier, now: u32) {
        self.last_change = now;
        self.good_since = None;
        self.low_since = None;
        self.need_listen = true;
        if t != self.tier {
            self.tier = t;
            self.emit(Event::Retrain(t));
        }
    }

    pub(super) fn connected_timers(&mut self, sregs: &SRegs, now: u32) {
        if self.voice.on && !self.vs.slots {
            self.voice_ready_tick(now);
        }
        let rto = rto_ms(self.tier);
        // Voice slots: only slots in which the peer was due to send count
        // as silence, and missed ones stand for unanswered pings (an
        // answerer still waiting for the caller's clock counts time).
        let (silent, pings) = if self.vs.slots && self.vs.synced() {
            (self.vs.quiet_ms, self.vs.missed)
        } else {
            (since(now, self.last_rx), self.ping_out)
        };
        if silent >= rto.saturating_mul(8) {
            self.resync_prev = self.tier;
            self.tier = Tier::ROBUST;
            self.state = State::Resync;
            // Already on SF12: the S22 count runs from the last frame heard.
            self.start = if self.resync_prev == Tier::ROBUST {
                self.last_rx
            } else {
                now
            };
            self.last_periodic = now.wrapping_sub(u32::MAX / 2);
            self.clear_pending();
            self.turn = true;
            self.need_listen = true;
            return;
        }
        let off = self.role_off(rto);
        if let Some(p) = self.pending {
            if since(now, self.pend_last) >= self.pend_wait {
                if self.pend_tries >= 3 {
                    self.clear_pending(); // resync recovers if the peer moved
                } else if self.vs.slots && self.q_has_tierset() {
                    // Still waiting for our sub-slot: no second copy.
                    self.pend_last = now;
                } else {
                    self.pend_tries += 1;
                    self.pend_last = now;
                    self.pend_wait = rto.saturating_add(self.jitter(rto / 2));
                    let cur = self.tier;
                    self.enq(cur, FrameType::TierSet, 0, &[p.index()], true, false);
                }
            }
            return;
        }
        // Real loss: 4 RTOs silent and at least two probes unanswered (one
        // lost keepalive reply is not enough).
        if silent >= rto.saturating_mul(4).saturating_add(off) && pings >= 2 && !self.silence_done {
            self.silence_done = true;
            if let Some(s) = self.tier.slower() {
                self.request(s, now);
            }
            return;
        }
        if since(now, self.last_change) < rto.saturating_mul(2) {
            return;
        }
        let s20 = reg(sregs, 20, 20) as i32;
        let ewma = self.ewma_x32 / 8;
        let floor = i32::from(self.tier.snr_floor_x4());
        if let Some(s) = self.tier.slower() {
            if ewma < floor + s20 - 8 {
                let l = *self.low_since.get_or_insert(now);
                if since(now, l) >= off {
                    self.low_since = None;
                    self.request(s, now);
                }
                return;
            }
        }
        self.low_since = None;
        let up = self.tier.faster().filter(|f| *f <= self.cap);
        match up {
            Some(f) if ewma > i32::from(f.snr_floor_x4()) + s20 + 12 => {
                let g = *self.good_since.get_or_insert(now);
                if since(now, g) >= UP_HOLD_MS.saturating_add(off) {
                    self.good_since = None;
                    self.request(f, now);
                }
            }
            _ => self.good_since = None,
        }
    }
}
