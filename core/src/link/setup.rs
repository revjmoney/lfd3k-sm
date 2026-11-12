//! Call setup: dial, ring, answer, training handshake. Voice call setup
//! (dial, ring, answer, decline by person name) is wired here; its frames
//! and rules are in `voice::call`.

use super::*;
use crate::voice::call::{VoiceReq, ANSWER_SENDS};
use crate::voice::{Name16, VSub, VOICE_REGS};

impl Link {
    /// Caller's training probe: Keepalive PING on the trained tier.
    pub(super) fn send_probe(&mut self) {
        let p = self.ka_payload();
        let t = self.tier;
        self.enq(t, FrameType::Keepalive, FLAG_PING, &p, true, false);
    }

    /// Start a call. False if busy or a name is invalid. The CallReq offers
    /// compression (capability byte) when S31 is 1.
    pub fn dial(&mut self, me: &str, callee: &str, sregs: &SRegs, now: u32) -> bool {
        if self.state != State::Idle {
            return false;
        }
        let mut p = [0u8; QBUF];
        let Ok(mut plen) = frame::call_req_payload(me, callee, &mut p) else {
            return false;
        };
        let lz = lz_wanted(sregs);
        if lz {
            // Two 16 byte names plus the byte still fit a QBUF frame.
            let Some(c) = p.get_mut(plen) else {
                return false;
            };
            *c = frame::CAP_LZ;
            plen += 1;
        }
        let call_id = (now as u8) | 1;
        let h = Header {
            ftype: FrameType::CallReq,
            flags: 0,
            call_id,
            seq: 0,
            ack: 0,
        };
        let Ok(n) = frame::encode(&h, p.get(..plen).unwrap_or(&[]), &mut self.callreq) else {
            return false;
        };
        self.callreq_len = n as u8;
        self.call_id = call_id;
        self.reset_call(Role::Caller, now);
        self.lz_mine = lz;
        self.state = State::Dialing;
        self.send_callreq(now);
        true
    }

    pub(super) fn send_callreq(&mut self, now: u32) {
        let mut b = [0u8; QBUF];
        let n = self.callreq_len as usize;
        if let (Some(d), Some(s)) = (b.get_mut(..n), self.callreq.get(..n)) {
            d.copy_from_slice(s);
        }
        self.enq_raw(Tier::ROBUST, b.get(..n).unwrap_or(&[]), false);
        self.last_periodic = now;
        self.emit(Event::Ringing);
    }

    /// Answer a ringing call. False if nothing is ringing (a voice call
    /// rings the voice app: only `answer_voice` answers it).
    pub fn answer(&mut self, sregs: &SRegs, now: u32) -> bool {
        if self.state != State::Ringing || self.voice.on {
            return false;
        }
        let call_id = self.call_id;
        let snr = self.my_snr;
        let peer_lz = self.lz_peer;
        self.reset_call(Role::Answerer, now);
        self.call_id = call_id;
        self.my_snr = snr;
        self.lz_peer = peer_lz;
        self.lz_mine = lz_wanted(sregs);
        self.own_cap = cap_tier(sregs);
        self.cap = self.own_cap;
        self.state = State::AwaitTierSet;
        self.send_train();
        true
    }

    pub(super) fn send_train(&mut self) {
        let s = self.my_snr.to_le_bytes();
        // CallAck and Train carry the agreed answer: CAP_LZ only when we
        // offer LZ and the caller did (so it is a new build that takes the
        // extra byte). Without it they are the old forms (empty, 3 bytes).
        let agreed = self.lz_mine && self.lz_peer;
        let p = [self.cap.index(), s[0], s[1], frame::CAP_LZ];
        let (caps, train): (&[u8], &[u8]) = if agreed {
            (&[frame::CAP_LZ], &p)
        } else {
            (&[], p.get(..3).unwrap_or(&[]))
        };
        self.enq(Tier::ROBUST, FrameType::CallAck, 0, caps, false, false);
        self.enq(Tier::ROBUST, FrameType::Train, 0, train, false, false);
    }

    /// Hang up. True if a call (or call attempt) was up.
    pub fn hangup(&mut self, _now: u32) -> bool {
        match self.state {
            State::Idle => false,
            // A voice ring stays silent (CallReject would mean BUSY).
            State::Ringing if self.voice.on => {
                self.go_idle();
                false
            }
            // A voice answerer not yet bound by the caller's TierSet: its
            // Hangup (no token) could end the call another device won.
            State::AwaitTierSet if self.voice.on && !self.got_tierset => {
                self.q_retain(|f| f.urgent);
                self.go_idle();
                true
            }
            State::Ringing => {
                self.enq(Tier::ROBUST, FrameType::CallReject, 0, &[], false, true);
                self.go_idle();
                false
            }
            _ => {
                let t = self.listen_tier();
                self.q_len = 0;
                self.enq(t, FrameType::Hangup, 0, &[], false, true);
                self.enq(t, FrameType::Hangup, 0, &[], false, true);
                self.go_idle();
                true
            }
        }
    }

    pub(super) fn start_tierset(&mut self, t: Tier, now: u32) {
        self.train = t;
        self.state = State::AwaitTierAck;
        self.start = now;
        self.last_periodic = now;
        self.need_listen = true;
        let (p, n) = self.voice.tierset(t.index());
        self.enq(
            Tier::ROBUST,
            FrameType::TierSet,
            0,
            p.get(..n).unwrap_or(&[]),
            false,
            false,
        );
    }

    pub(super) fn connect(&mut self, now: u32, snr: i16) {
        self.state = State::Connected;
        self.last_rx = now;
        self.last_tx = now;
        self.last_change = now;
        self.ewma_x32 = i32::from(snr) * 8;
        self.good_since = None;
        self.low_since = None;
        self.ping_out = 0;
        self.silence_done = false;
        self.need_listen = true;
        self.turn = true;
        self.burst = 0;
        self.lz = self.lz_mine && self.lz_peer;
        self.emit(Event::Connect(self.tier));
    }

    pub(super) fn on_call_req(
        &mut self,
        p: &[u8],
        call_id: u8,
        info: RxInfo,
        me: &str,
        sregs: &SRegs,
        now: u32,
    ) {
        let Ok((_, callee, caps)) = frame::parse_call_req_caps(p) else {
            return;
        };
        if !callee.eq_ignore_ascii_case(me) || call_id == 0 {
            return;
        }
        let peer_lz = caps & frame::CAP_LZ != 0;
        if self.state == State::Idle && !self.engaged {
            self.call_id = call_id;
            self.voice.on = false;
            self.lz_peer = peer_lz;
            self.my_snr = info.snr_x4;
            self.state = State::Ringing;
            self.start = now;
            self.last_req = now;
            self.last_periodic = now;
            self.rings = 0;
            self.ring(sregs, now);
        } else if call_id == self.call_id {
            // Same id as our voice call: not this CallReq's call.
            if self.voice.on {
                return;
            }
            self.last_req = now;
            self.my_snr = info.snr_x4;
            if !self.is_connected() {
                self.lz_peer = peer_lz;
            }
            if self.state == State::AwaitTierSet && !self.got_tierset {
                self.send_train();
                self.last_periodic = now;
            }
        } else {
            let own = self.call_id;
            self.call_id = call_id;
            self.enq(Tier::ROBUST, FrameType::CallReject, 0, &[], false, false);
            self.call_id = own;
        }
    }

    pub(super) fn ring(&mut self, sregs: &SRegs, now: u32) {
        self.rings = self.rings.saturating_add(1);
        self.last_periodic = now;
        if self.voice.on {
            // Every ringing registration; S0 never answers a voice call.
            for i in 0..VOICE_REGS {
                if self.voice.ring_mask & (1 << i) != 0 {
                    self.emit(Event::VoiceRing(i as u8));
                }
            }
            return;
        }
        self.emit(Event::Ring);
        let s0 = reg(sregs, 0, 0);
        if s0 > 0 && u32::from(self.rings) >= s0 {
            self.answer(sregs, now);
        }
    }
}

/// Voice call setup (spec 5.4; frames and rules in `voice::call`).
impl Link {
    /// Place a voice call from our registration `me`/`me_label` on station
    /// `me_station` to the voice name `callee` (one device when
    /// `callee_label` is given). False if busy or a name is invalid. The
    /// request repeats every `PERIOD_MS` until answered or S7 runs out.
    #[allow(clippy::too_many_arguments)]
    pub fn dial_voice(
        &mut self,
        me_station: &str,
        me: &str,
        me_label: &str,
        callee: &str,
        callee_label: Option<&str>,
        sregs: &SRegs,
        now: u32,
    ) -> bool {
        if self.state != State::Idle {
            return false;
        }
        let peer_label = match callee_label {
            Some(l) => Name16::label(l),
            None => Some(Name16::EMPTY),
        };
        let (Some(peer_name), Some(peer_label)) = (Name16::new(callee), peer_label) else {
            return false;
        };
        let lz = lz_wanted(sregs);
        let call_id = (now as u8) | 1;
        let r = VoiceReq {
            caps: if lz { frame::CAP_LZ } else { 0 },
            station: me_station,
            name: me,
            label: me_label,
            callee,
            callee_label: callee_label.unwrap_or(""),
        };
        let mut b = [0u8; QBUF];
        let Some(n) = vcall::encode_req(call_id, &r, &mut b) else {
            return false;
        };
        self.callreq = b;
        self.callreq_len = n as u8;
        self.call_id = call_id;
        self.reset_call(Role::Caller, now);
        self.voice.start(VoiceCallInfo {
            peer_name,
            peer_label,
            ..VoiceCallInfo::EMPTY
        });
        self.voice.want_label = peer_label;
        self.lz_mine = lz;
        self.state = State::Dialing;
        self.send_callreq(now);
        true
    }

    /// Which of our registrations placed the current voice call.
    pub(crate) fn set_voice_reg(&mut self, reg: u8) {
        self.voice.info.reg = reg;
    }

    /// Answer a ringing voice call with our answer `token` (drawn by the
    /// Modem, never zero). Sends the Answer and the voice Train. False if no
    /// voice call rings.
    pub fn answer_voice(&mut self, token: u32, sregs: &SRegs, now: u32) -> bool {
        if self.state != State::Ringing || !self.voice.on {
            return false;
        }
        let call_id = self.call_id;
        let snr = self.my_snr;
        let peer_lz = self.lz_peer;
        let v = self.voice;
        self.reset_call(Role::Answerer, now);
        self.call_id = call_id;
        self.my_snr = snr;
        self.lz_peer = peer_lz;
        self.voice = v;
        self.voice.info.token = token;
        self.voice.sends = 0;
        self.lz_mine = lz_wanted(sregs);
        self.own_cap = cap_tier(sregs);
        self.cap = self.own_cap;
        self.state = State::AwaitTierSet;
        self.send_voice_answer(now);
        true
    }

    /// Answer and voice Train (today's Train form plus the token).
    fn send_voice_answer(&mut self, now: u32) {
        self.voice.sends = self.voice.sends.saturating_add(1);
        self.voice.sent_at = now;
        self.voice.next_at = self.voice.resend_after(now, PERIOD_MS);
        self.last_periodic = now;
        let mut b = [0u8; QBUF];
        let (st, lb) = (self.voice.me_station, self.voice.me_label);
        let token = self.voice.info.token;
        if let Ok(n) = vcall::encode_answer(self.call_id, token, st.as_str(), lb.as_str(), &mut b) {
            self.enq_raw(Tier::ROBUST, b.get(..n).unwrap_or(&[]), false);
        }
        let s = self.my_snr.to_le_bytes();
        let base = [self.cap.index(), s[0], s[1], frame::CAP_LZ];
        let blen = if self.lz_mine && self.lz_peer { 4 } else { 3 };
        let mut t = [0u8; 8];
        let n = vcall::train_payload(base.get(..blen).unwrap_or(&[]), token, &mut t);
        let train = t.get(..n).unwrap_or(&[]);
        self.enq(Tier::ROBUST, FrameType::Train, 0, train, false, false);
    }

    /// Decline a ringing voice call: Decline twice, then idle. Its repeats
    /// do not ring again.
    pub fn decline_voice(&mut self, now: u32) {
        if self.state != State::Ringing || !self.voice.on {
            return;
        }
        let mut b = [0u8; HEADER_LEN + 1];
        if let Ok(n) = vcall::encode_decline(self.call_id, &mut b) {
            let f = b.get(..n).unwrap_or(&[]);
            self.enq_raw(Tier::ROBUST, f, true);
            self.enq_raw(Tier::ROBUST, f, true);
        }
        self.voice.declined = Some((self.call_id, now));
        self.go_idle();
    }

    /// The far end of the voice call ringing, being set up or up; None
    /// otherwise (also during a data call).
    pub fn voice(&self) -> Option<&VoiceCallInfo> {
        (self.voice.on && self.state != State::Idle).then_some(&self.voice.info)
    }

    /// The current (or last) call is a voice call.
    pub(crate) fn voice_last(&self) -> bool {
        self.voice.on
    }

    /// A voice CallReq (call id `call_id`) that rings the registrations in
    /// `mask` (the Modem matched them and applied the privacy rule).
    /// `peer` is the caller, `me_station` and `me_label` our station and
    /// the label of `peer.reg`. An idle link rings; the repeats of the call
    /// ringing or answered here refresh it; anything else is ignored, so a
    /// busy device stays silent.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_voice_req(
        &mut self,
        call_id: u8,
        caps: u8,
        peer: VoiceCallInfo,
        mask: u8,
        me_station: Name16,
        me_label: Name16,
        info: RxInfo,
        sregs: &SRegs,
        now: u32,
    ) {
        if call_id == 0 || mask == 0 {
            return;
        }
        if let Some((id, at)) = self.voice.declined {
            if id == call_id && since(now, at) < 2 * PERIOD_MS + 1000 {
                self.voice.declined = Some((id, now));
                return;
            }
        }
        let peer_lz = caps & frame::CAP_LZ != 0;
        if self.state == State::Idle && !self.engaged {
            self.call_id = call_id;
            self.lz_peer = peer_lz;
            self.my_snr = info.snr_x4;
            self.voice.start(peer);
            self.voice.ring_mask = mask;
            self.voice.me_station = me_station;
            self.voice.me_label = me_label;
            // Not the caller (the Modem reads the role on Declined).
            self.role = Role::Answerer;
            self.state = State::Ringing;
            self.start = now;
            self.last_req = now;
            self.last_periodic = now;
            self.rings = 0;
            self.ring(sregs, now);
        } else if call_id == self.call_id
            && self.voice.on
            && (self.state == State::Ringing || self.role == Role::Answerer)
        {
            self.last_req = now;
            self.my_snr = info.snr_x4;
            if !self.is_connected() {
                self.lz_peer = peer_lz;
            }
            // The caller has not heard our answer: send it again soon once
            // the last one has gone out, within the send limit.
            if self.state == State::AwaitTierSet
                && !self.got_tierset
                && self.q_len == 0
                && self.voice.sends < ANSWER_SENDS
            {
                let soon = self.voice.resend_soon(now);
                if vcall::reached(self.voice.next_at, soon) {
                    self.voice.next_at = soon;
                }
            }
        }
    }

    /// A voice Answer or Decline for call id `call_id`.
    pub(crate) fn on_voice_frame(&mut self, sub: VSub, call_id: u8, body: &[u8], now: u32) {
        if self.state == State::Idle || !self.voice.on || call_id != self.call_id {
            return;
        }
        match sub {
            VSub::Answer => {
                let Some((token, station, label)) = vcall::parse_answer(body) else {
                    return;
                };
                let st = Name16::new(station).unwrap_or(Name16::EMPTY);
                let lb = Name16::label(label).unwrap_or(Name16::EMPTY);
                if self.state == State::Ringing {
                    // Another device picked up.
                    self.emit(Event::AnsweredElsewhere);
                    self.go_idle();
                } else if self.state == State::Dialing && !self.voice.bound {
                    // The first answer wins; later ones get no reply.
                    self.voice.bound = true;
                    self.voice.bound_at = now;
                    self.voice.info.token = token;
                    self.voice.info.peer_station = st;
                    self.voice.info.peer_label = lb;
                    self.state = State::AwaitTrain;
                } else if self.role == Role::Caller
                    && self.voice.bound
                    && token == self.voice.info.token
                    && self.voice.info.peer_station.is_empty()
                {
                    // Bound by its Train: its Answer still names it.
                    self.voice.info.peer_station = st;
                    self.voice.info.peer_label = lb;
                }
            }
            VSub::Decline => {
                if !body.is_empty() {
                    return;
                }
                if self.state == State::Ringing {
                    self.voice.declined = Some((self.call_id, now));
                    self.emit(Event::Declined);
                    self.go_idle();
                } else if self.state == State::Dialing && !self.voice.bound {
                    self.fail(Event::Declined, false);
                }
            }
            _ => {}
        }
    }

    /// A frame of our voice call (call id matched, link not idle) before
    /// the usual handling: binds the caller to a token and screens tokens.
    /// True when it was handled here and the usual handling must not run.
    pub(super) fn voice_frame_screen(&mut self, ft: FrameType, p: &[u8]) -> bool {
        match self.state {
            // Someone else's TierSet: the call was answered elsewhere.
            State::Ringing => {
                if ft == FrameType::TierSet {
                    self.emit(Event::AnsweredElsewhere);
                    self.go_idle();
                    return true;
                }
                false
            }
            State::Dialing | State::AwaitTrain => match ft {
                // Not voice call frames: CallReject means BUSY on data only.
                FrameType::CallAck | FrameType::CallReject => true,
                FrameType::Train => {
                    let Some((_, token)) = vcall::split_train(p) else {
                        return true;
                    };
                    if self.voice.bound {
                        return token != self.voice.info.token;
                    }
                    // Its Answer was lost: the first Train binds.
                    self.voice.bound = true;
                    self.voice.info.token = token;
                    false
                }
                _ => false,
            },
            // A late pickup that never got its TierSet: the call's Hangup
            // means another device had it. Stop, silently.
            State::AwaitTierSet
                if self.role == Role::Answerer && !self.got_tierset && ft == FrameType::Hangup =>
            {
                self.q_retain(|f| f.urgent);
                self.emit(Event::AnsweredElsewhere);
                self.go_idle();
                true
            }
            State::AwaitTierSet | State::Probe
                if self.role == Role::Answerer && ft == FrameType::TierSet =>
            {
                if vcall::tierset_token(p) == Some(self.voice.info.token) {
                    return false;
                }
                if !self.got_tierset {
                    // The caller bound to another device: stop, silently.
                    self.q_retain(|f| f.urgent);
                    self.emit(Event::AnsweredElsewhere);
                    self.go_idle();
                }
                true
            }
            _ => false,
        }
    }

    /// AwaitTierSet on a voice answerer: the answer goes out at most
    /// `ANSWER_SENDS` times (again after 3 to 4 s, or soon after the caller
    /// asks again); with no TierSet carrying our token
    /// `2 * PERIOD_MS` after the last one, another device has the call.
    pub(super) fn voice_await_tick(&mut self, s7: u32, now: u32) {
        if self.got_tierset {
            // Back from a failed probe: as for a data call.
            if since(now, self.start) >= s7 {
                self.fail(Event::NoCarrier, true);
            }
            return;
        }
        if self.voice.sends < ANSWER_SENDS {
            if vcall::reached(now, self.voice.next_at) {
                self.send_voice_answer(now);
            }
        } else if since(now, self.voice.sent_at) >= 2 * PERIOD_MS || since(now, self.start) >= s7 {
            self.q_retain(|f| f.urgent);
            self.emit(Event::AnsweredElsewhere);
            self.go_idle();
        }
    }

    /// Caller bound by an Answer whose Train never came (the answering
    /// device sends it at most twice): unbind and ask again, so the
    /// devices (that one included, once it gives up) can ring and pick up.
    pub(super) fn voice_unbind(&mut self, now: u32) {
        self.voice.bound = false;
        self.voice.info.token = 0;
        self.voice.info.peer_station = Name16::EMPTY;
        self.voice.info.peer_label = self.voice.want_label;
        self.state = State::Dialing;
        self.send_callreq(now);
    }
}
