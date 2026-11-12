//! Data phase: ARQ glue, frame queue pump, turn taking and keepalive jitter.
//! Voice calls: the wiring of the voice scheduler (`voice::sched`) into the
//! pump, the received voice frames and the voice API.

use super::*;
use crate::lz;
use crate::voice::frames::VSub;
use crate::voice::plan::{talk_plan, TalkPlan};
use crate::voice::sched::{self, Floor, Turn, LISTEN_CHUNK, VF_READY, VF_SLOTS};

impl Link {
    pub(super) fn on_connected(
        &mut self,
        f: &frame::Frame<'_>,
        info: RxInfo,
        sregs: &SRegs,
        now: u32,
    ) {
        let h = f.header;
        let peer = match (h.ftype, f.payload) {
            (FrameType::Keepalive, [a, b]) => Some(i16::from_le_bytes([*a, *b])),
            _ => None,
        };
        self.heard(peer, info, sregs, now);
        if self.vs.slots {
            // Slots, not turns.
        } else if h.flags & FLAG_MORE != 0 {
            self.turn = false;
            self.turn_start = now;
            self.turn_ms =
                air_ms(self.tier, HEADER_LEN + CHUNK).saturating_add(turn_gap_ms(self.tier));
        } else {
            self.turn = true;
            self.burst = 0;
        }
        let tier_arg = f.payload.first().and_then(|&i| Tier::from_index(i));
        self.on_connected_frame(f, tier_arg, info, now);
    }

    /// Something of the call was heard: the link is alive, an SNR sample,
    /// and the way back from a resync. `peer` is the SNR the peer hears us
    /// at, when the frame says.
    pub(super) fn heard(&mut self, peer: Option<i16>, info: RxInfo, sregs: &SRegs, now: u32) {
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
            if self.vs.slots {
                self.vs.restart(now, Tier::ROBUST);
            }
            if self.role == Role::Caller {
                let peer = peer.unwrap_or(info.snr_x4);
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
        if self.vs.slots {
            self.vs.on_heard(now);
        }
    }

    fn on_connected_frame(
        &mut self,
        f: &frame::Frame<'_>,
        tier_arg: Option<Tier>,
        info: RxInfo,
        now: u32,
    ) {
        let h = f.header;
        match h.ftype {
            FrameType::Data => {
                let mut plain = [0u8; CHUNK];
                let data = if h.flags & frame::FLAG_LZ != 0 {
                    match lz::decompress(f.payload, &mut plain) {
                        Ok(n) => plain.get(..n).unwrap_or(&[]),
                        // Treated as lost: no ack, so the sender resends.
                        Err(_) => return,
                    }
                } else {
                    f.payload
                };
                self.lz_count(data.len(), f.payload.len());
                self.rx.on_data(h.seq, data);
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
        if !urgent && self.state == State::Connected && self.vs.slots {
            self.voice_pump(radio, now);
            return;
        }
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
            if let Some(b0) = buf.get_mut(1) {
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
            // The staged chunk stays plain; compressing it here each time
            // it is sent gives the same bytes on every resend.
            let raw = self.staged_buf.get(..len as usize)?;
            let mut packed = [0u8; CHUNK];
            let z = if self.lz {
                lz::compress(raw, &mut packed)
            } else {
                None
            };
            let (flags, p) = match z {
                Some(m) => (frame::FLAG_LZ, packed.get(..m)?),
                None => (0, raw),
            };
            let (raw_len, air_len) = (raw.len(), p.len());
            let n = mk(FrameType::Data, flags, seq, 0, p, out, self.call_id)?;
            self.lz_count(raw_len, air_len);
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

/// Voice calls: the voice scheduler's wiring (`voice::sched`).
impl Link {
    /// The app is ready for voice (its scrambler handshake and trust
    /// decision are done). From now on its control bytes go out in small
    /// chunks; the voice slots start once both ends are ready.
    pub fn set_voice_ready(&mut self) {
        if self.voice.on && self.is_connected() {
            self.vs.ready_mine = true;
        }
    }

    /// The voice slots run.
    pub fn voice_slots(&self) -> bool {
        self.voice.on && self.is_connected() && self.vs.slots
    }

    /// Queue one sealed voice frame for the next slot we may talk in;
    /// false if `VOICE_QUEUE` (2) frames are already waiting (or no voice
    /// call is up, or it is empty or longer than `VOICE_MAX`). In push to
    /// talk, frames queued while the floor is not ours are dropped at our
    /// next sub-slot.
    pub fn voice_push(&mut self, sealed: &[u8]) -> bool {
        self.voice.on && self.is_connected() && self.vs.push_voice(sealed)
    }

    /// The last sealed voice frame heard, once; 0 when none (or `out` is
    /// too small).
    pub fn voice_pop(&mut self, out: &mut [u8]) -> usize {
        self.vs.pop_voice(out)
    }

    /// The push to talk key went down or up.
    pub fn set_talk_key(&mut self, down: bool) {
        self.vs.set_key(down);
    }

    /// The talk plan of the current tier while a voice call is up (it
    /// takes effect on the air at the next slot boundary).
    pub fn talk_plan(&self) -> Option<TalkPlan> {
        (self.voice.on && self.is_connected()).then(|| talk_plan(self.tier))
    }

    /// Who holds the push to talk floor (Free in duplex and before the
    /// voice slots start).
    pub fn floor(&self) -> Floor {
        if self.voice.on && self.is_connected() {
            self.vs.floor()
        } else {
            Floor::Free
        }
    }

    /// Bytes queued with `push_data`: in small chunks once the app is
    /// ready for voice, so they fit a Listener frame (the tier's chunk
    /// size at push time).
    pub(super) fn arq_push(&mut self, data: &[u8]) -> usize {
        if self.voice.on && (self.vs.ready_mine || self.vs.slots) {
            let c = usize::from(sched::slot_layout(self.tier).chunk);
            self.tx.push_max(data, c)
        } else {
            self.tx.push(data)
        }
    }

    /// A voice Data or Listener frame of our call (from the Modem, which
    /// keeps every type 15 frame away from `on_frame`).
    pub(crate) fn on_voice_air(
        &mut self,
        sub: VSub,
        h: Header,
        body: &[u8],
        info: RxInfo,
        sregs: &SRegs,
        now: u32,
    ) {
        if !self.voice.on
            || h.call_id != self.call_id
            || !matches!(self.state, State::Connected | State::Resync)
        {
            return;
        }
        let Some(p) = sched::parse(sub, body) else {
            return;
        };
        self.heard(p.snr, info, sregs, now);
        self.tx.on_ack(h.ack, p.sack);
        if let Some((seq, d)) = p.chunk {
            self.rx.on_data(seq, d);
            self.ack_owed = true;
            self.data_rx = Some(now);
        }
        if !self.vs.slots {
            // Data timing: a voice frame never sets MORE, so it ends the
            // sender's turn.
            self.turn = true;
            self.burst = 0;
            if p.flags & VF_READY != 0 && self.role == Role::Caller {
                self.vs.ready_peer = true;
            }
            if p.flags & VF_SLOTS != 0 && self.role == Role::Answerer && self.vs.ready_mine {
                self.vs.start_answerer(now, self.tier);
            }
        }
        if self.vs.slots && self.state == State::Connected {
            if self.role == Role::Answerer && p.flags & VF_SLOTS != 0 {
                let len = HEADER_LEN + 1 + body.len();
                self.vs.sync(now, info.tier, len, h.seq, &p);
                self.vs.on_heard(now);
            }
            self.vs.peer_flags(p.flags);
        }
        if sub == VSub::Data {
            self.vs.store_voice(p.voice);
        }
    }

    /// Before the slots (data timing): the answerer says it is ready, the
    /// caller starts the slots once both are. Called from the connected
    /// timers.
    pub(super) fn voice_ready_tick(&mut self, now: u32) {
        if self.vs.slots || !self.vs.ready_mine || self.tx.longest() > LISTEN_CHUNK {
            return;
        }
        match self.role {
            Role::Caller => {
                // Start on our turn, so slot 0 never lands on the
                // answerer's burst.
                if self.vs.ready_peer && self.turn {
                    self.vs.start_caller(now, self.tier);
                }
            }
            Role::Answerer => {
                let every = rto_ms(self.tier).saturating_mul(2);
                let due = self.vs.ready_at.is_none_or(|t| since(now, t) >= every);
                if due && self.q_len < QLEN {
                    let mut b = [0u8; MAX_FRAME];
                    let ack = self.rx.ack_state();
                    let snr = self.last_snr;
                    let id = self.call_id;
                    if let Some(n) = self.vs.build_listener(id, ack, snr, None, now, &mut b) {
                        let t = self.tier;
                        self.enq_raw(t, b.get(..n).unwrap_or(&[]), false);
                        // It ends our turn (no MORE on a voice frame).
                        let last = (self.q_head + self.q_len + QLEN - 1) % QLEN;
                        if let Some(f) = self.q.get_mut(last) {
                            f.end_turn = true;
                        }
                        self.vs.ready_at = Some(now);
                    }
                }
            }
        }
    }

    /// The queued frame at the head is a TierAck (a reply: it goes out at
    /// once).
    fn q_head_is_reply(&self) -> bool {
        self.q_len > 0
            && self.q.get(self.q_head).is_some_and(|f| {
                f.buf
                    .get(1)
                    .is_some_and(|b| b >> 4 == FrameType::TierAck as u8)
            })
    }

    /// Pump while the voice slots run: replies at once, everything else in
    /// our turn of the slot.
    fn voice_pump<R: Radio>(&mut self, radio: &mut R, now: u32) {
        self.vs.advance(now, self.tier);
        let mut buf = [0u8; MAX_FRAME];
        let mut listener = false;
        let reply = self.ka_reply || self.q_head_is_reply();
        let sent = if reply && self.vs.hold_ok(now) {
            if self.q_head_is_reply() {
                self.next_queued(&mut buf)
            } else {
                self.ka_reply = false;
                let h = Header {
                    ftype: FrameType::Keepalive,
                    flags: 0,
                    call_id: self.call_id,
                    seq: 0,
                    ack: 0,
                };
                let p = self.ka_payload();
                frame::encode(&h, &p, &mut buf).ok().map(|n| (n, self.tier))
            }
        } else {
            match self.vs.turn(now, self.q_len > 0) {
                Some(_) if self.q_len > 0 => self.next_queued(&mut buf),
                Some(Turn::Due) => self
                    .slot_frame(&mut buf, now, &mut listener)
                    .map(|n| (n, self.tier)),
                _ => None,
            }
        };
        let Some((len, tier)) = sent else {
            self.relisten(radio);
            return;
        };
        radio.send(tier, buf.get(..len).unwrap_or(&[]));
        self.need_listen = true;
        self.last_tx = now;
        self.vs.on_sent(now, air_ms(tier, len), listener);
    }

    /// Pop the queued frame at the head into `out`: (len, tier).
    fn next_queued(&mut self, out: &mut [u8]) -> Option<(usize, Tier)> {
        let f = self.q.get(self.q_head).copied()?;
        self.q_head = (self.q_head + 1) % QLEN;
        self.q_len -= 1;
        let n = f.len as usize;
        out.get_mut(..n)?.copy_from_slice(f.buf.get(..n)?);
        Some((n, f.tier))
    }

    /// Our slot frame: the waiting voice packet in a Data frame when we may
    /// talk, else a Listener frame with an ARQ chunk if one is due. Control
    /// bytes kept waiting `CTRL_WAIT_MS` take one voice frame's place.
    /// `listener` comes back true for a Listener frame.
    fn slot_frame(&mut self, out: &mut [u8], now: u32, listener: &mut bool) -> Option<usize> {
        let t = self.tier;
        let ack = self.rx.ack_state();
        self.ack_owed = false;
        self.ka_reply = false;
        let starve = !self.tx.idle() && since(now, self.vs.last_lis) >= sched::CTRL_WAIT_MS;
        if self.vs.may_talk() && self.vs.voice_waiting(t) && !starve {
            *listener = false;
            return self.vs.build_data(self.call_id, ack, now, out);
        }
        if !self.vs.may_talk() {
            // Not ours to send: a stale frame is never sent later.
            self.vs.clear_voice();
        } else if self.vs.voice_queued() {
            // Replaced by control bytes, or too long for this plan.
            self.vs.drop_voice();
        }
        *listener = true;
        let snr = self.last_snr;
        let call_id = self.call_id;
        let rto = rto_ms(t);
        let chunk = self.tx.next_to_send(now, rto);
        let carried = chunk.is_some();
        let n = self.vs.build_listener(call_id, ack, snr, chunk, now, out);
        if n.is_some() && carried {
            self.data_tx = Some(now);
        }
        n
    }
}
