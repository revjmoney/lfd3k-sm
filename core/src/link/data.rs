//! Data phase: ARQ glue, frame queue pump, turn taking and keepalive jitter.

use super::*;

impl Link {
    pub(super) fn on_connected(
        &mut self,
        f: &frame::Frame<'_>,
        info: RxInfo,
        sregs: &SRegs,
        now: u32,
    ) {
        let h = f.header;
        self.last_rx = now;
        self.last_snr = info.snr_x4;
        self.silence_done = false;
        self.ping_out = 0;
        if self.state == State::Resync {
            self.state = State::Connected;
            self.ewma_x32 = i32::from(info.snr_x4) * 8;
            self.last_change = now;
            self.good_since = None;
            self.need_listen = true;
            if self.resync_prev != Tier::ROBUST {
                self.emit(Event::Retrain(Tier::ROBUST));
            }
            let prev = self.resync_prev;
            self.resync_prev = Tier::ROBUST;
            if self.role == Role::Caller {
                let peer = match (h.ftype, f.payload) {
                    (FrameType::Keepalive, [a, b]) => i16::from_le_bytes([*a, *b]),
                    _ => info.snr_x4,
                };
                // Never jump straight back to the tier that just died.
                let below = prev.slower().unwrap_or(Tier::ROBUST);
                let t = best_tier(info.snr_x4.min(peer), reg(sregs, 20, 20) as i16)
                    .min(self.cap)
                    .min(below);
                if t > Tier::ROBUST {
                    self.request(t, now);
                }
            }
        } else {
            let e = self.ewma_x32;
            self.ewma_x32 = e
                .saturating_add(i32::from(info.snr_x4))
                .saturating_sub(e / 8);
        }
        if h.flags & FLAG_MORE != 0 {
            self.turn = false;
            self.turn_start = now;
            self.turn_ms =
                air_ms(self.tier, HEADER_LEN + CHUNK).saturating_add(turn_gap_ms(self.tier));
        } else {
            self.turn = true;
            self.burst = 0;
        }
        let tier_arg = f.payload.first().and_then(|&i| Tier::from_index(i));
        match h.ftype {
            FrameType::Data => {
                self.rx.on_data(h.seq, f.payload);
                // Ack every Data frame, whatever on_data said.
                self.ack_owed = true;
                self.data_rx = Some(now);
            }
            FrameType::Ack => {
                if let [sack] = f.payload {
                    self.tx.on_ack(h.ack, *sack);
                }
            }
            FrameType::Keepalive => {
                if h.flags & FLAG_PING != 0 {
                    self.ka_reply = true;
                }
            }
            FrameType::TierSet => {
                let Some(t) = tier_arg else { return };
                if self.pending.is_some() && self.role == Role::Caller {
                    return; // caller's own request wins
                }
                self.clear_pending();
                // Above our cap: ack our cap instead, and move there.
                let t = t.min(self.own_cap);
                self.enq(info.tier, FrameType::TierAck, 0, &[t.index()], true, false);
                self.switch(t, now);
            }
            FrameType::TierAck => {
                if let (Some(a), Some(p)) = (tier_arg, self.pending) {
                    if a <= p {
                        self.clear_pending();
                        if a < p {
                            self.cap = self.cap.min(a); // peer's cap
                        }
                        self.switch(a, now);
                    }
                }
            }
            _ => {}
        }
    }

    /// Send at most one frame if the radio is free, else re-listen.
    pub fn pump<R: Radio>(&mut self, radio: &mut R, now: u32) {
        if radio.busy() {
            return;
        }
        let urgent = self.q_len > 0 && self.q.get(self.q_head).is_some_and(|f| f.urgent);
        let turns = self.state == State::Connected;
        if turns && !urgent && !self.turn {
            if since(now, self.turn_start) >= self.turn_ms {
                self.turn = true;
                self.burst = 0;
            } else {
                self.relisten(radio);
                return;
            }
        }
        let mut buf = [0u8; MAX_FRAME];
        let Some((len, tier, end_turn, is_data)) = self.next_frame(&mut buf, now) else {
            self.relisten(radio);
            return;
        };
        let more = turns && !end_turn && self.burst + 1 < BURST_MAX && self.has_more(now);
        if more {
            if let Some(b0) = buf.get_mut(0) {
                *b0 |= FLAG_MORE;
            }
        }
        radio.send(tier, buf.get(..len).unwrap_or(&[]));
        self.need_listen = true;
        self.last_tx = now;
        if is_data {
            self.data_tx = Some(now);
        }
        if more {
            self.burst += 1;
        } else {
            self.turn = !turns;
            self.burst = 0;
            self.turn_start = now;
            let y = if self.role == Role::Answerer {
                TURN_YIELD_MS
            } else {
                0
            };
            self.turn_ms = air_ms(tier, len).saturating_add(turn_gap_ms(tier) + y);
        }
    }

    pub(super) fn relisten<R: Radio>(&mut self, radio: &mut R) {
        let want = self.listen_tier();
        if self.need_listen || self.listening != Some(want) {
            radio.listen(want);
            self.listening = Some(want);
            self.need_listen = false;
        }
    }

    pub(super) fn has_more(&mut self, now: u32) -> bool {
        self.q_len > 0 || self.ack_owed || self.stage(now)
    }

    /// Make sure a data frame is staged; true if one is.
    pub(super) fn stage(&mut self, now: u32) -> bool {
        if self.state != State::Connected {
            return false;
        }
        if self.staged.is_some() {
            return true;
        }
        let rto = rto_ms(self.tier);
        if let Some((seq, d)) = self.tx.next_to_send(now, rto) {
            let n = d.len().min(CHUNK);
            if let (Some(dst), Some(src)) = (self.staged_buf.get_mut(..n), d.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.staged = Some((seq, n as u8));
        }
        self.staged.is_some()
    }

    /// Build the next frame: (len, tier, end_turn, is_data).
    pub(super) fn next_frame(
        &mut self,
        out: &mut [u8],
        now: u32,
    ) -> Option<(usize, Tier, bool, bool)> {
        if self.q_len > 0 {
            let f = self.q.get(self.q_head).copied()?;
            self.q_head = (self.q_head + 1) % QLEN;
            self.q_len -= 1;
            let n = f.len as usize;
            out.get_mut(..n)?.copy_from_slice(f.buf.get(..n)?);
            return Some((n, f.tier, f.end_turn, false));
        }
        if self.state != State::Connected {
            return None;
        }
        let t = self.tier;
        let mk = |ft, flags, seq, ack, p: &[u8], out: &mut [u8], cid| {
            let h = Header {
                ftype: ft,
                flags,
                call_id: cid,
                seq,
                ack,
            };
            frame::encode(&h, p, out).ok()
        };
        if self.ack_owed {
            self.ack_owed = false;
            self.ka_reply = false;
            let (a, sack) = self.rx.ack_state();
            let n = mk(FrameType::Ack, 0, 0, a, &[sack], out, self.call_id)?;
            return Some((n, t, false, false));
        }
        // Probe the peer: first PING after 2 RTOs (answerer 2.5) of hearing
        // nothing, then again after about 1 RTO (jittered) while unanswered.
        let rto = rto_ms(t);
        let idle = rto.saturating_mul(2).saturating_add(self.role_off(rto));
        let probe = if self.ping_out == 0 {
            since(now, self.last_rx) >= idle
        } else {
            since(now, self.ping_last) >= self.ping_wait
        };
        if probe {
            self.ping_out = self.ping_out.saturating_add(1);
            self.ping_last = now;
            self.ping_wait = rto.saturating_add(self.jitter(rto / 4));
            let n = mk(
                FrameType::Keepalive,
                FLAG_PING,
                0,
                0,
                &self.ka_payload(),
                out,
                self.call_id,
            )?;
            return Some((n, t, true, false));
        }
        if self.stage(now) {
            let (seq, len) = self.staged.take()?;
            self.ka_reply = false;
            let n = mk(
                FrameType::Data,
                0,
                seq,
                0,
                self.staged_buf.get(..len as usize)?,
                out,
                self.call_id,
            )?;
            return Some((n, t, false, true));
        }
        if self.ka_reply {
            self.ka_reply = false;
            let n = mk(
                FrameType::Keepalive,
                0,
                0,
                0,
                &self.ka_payload(),
                out,
                self.call_id,
            )?;
            return Some((n, t, true, false));
        }
        None
    }
}
