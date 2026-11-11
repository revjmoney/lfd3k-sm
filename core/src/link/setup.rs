//! Call setup: dial, ring, answer, training handshake.

use super::*;

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
        self.enq_raw(Tier::ROBUST, b.get(..n).unwrap_or(&[]));
        self.last_periodic = now;
        self.emit(Event::Ringing);
    }

    /// Answer a ringing call. False if nothing is ringing.
    pub fn answer(&mut self, sregs: &SRegs, now: u32) -> bool {
        if self.state != State::Ringing {
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
        let p = [self.cap.index(), s[0], s[1]];
        // The capability byte only when we offer LZ; empty is the old form.
        let caps: &[u8] = if self.lz_mine { &[frame::CAP_LZ] } else { &[] };
        self.enq(Tier::ROBUST, FrameType::CallAck, 0, caps, false, false);
        self.enq(Tier::ROBUST, FrameType::Train, 0, &p, false, false);
    }

    /// Hang up. True if a call (or call attempt) was up.
    pub fn hangup(&mut self, _now: u32) -> bool {
        match self.state {
            State::Idle => false,
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
        self.enq(
            Tier::ROBUST,
            FrameType::TierSet,
            0,
            &[t.index()],
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
            self.lz_peer = peer_lz;
            self.my_snr = info.snr_x4;
            self.state = State::Ringing;
            self.start = now;
            self.last_req = now;
            self.last_periodic = now;
            self.rings = 0;
            self.ring(sregs, now);
        } else if call_id == self.call_id {
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
        self.emit(Event::Ring);
        let s0 = reg(sregs, 0, 0);
        if s0 > 0 && u32::from(self.rings) >= s0 {
            self.answer(sregs, now);
        }
    }
}
