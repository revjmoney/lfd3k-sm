//! The voice scheduler (spec 8.3): fixed slots per tier once both ends are
//! ready for voice, in place of the data burst and turn timing.
//!
//! **Before the first voice.** From CONNECT the call runs on today's data
//! timing (bursts, turns, ARQ), so the app's scrambler handshake (opaque
//! bytes through `push_data` and `read_data`) moves at full data speed.
//! When the app says it is ready (`Link::set_voice_ready`), its later
//! control bytes go into ARQ chunks of at most the tier's listener chunk
//! (`SlotLayout::chunk`, `LISTEN_CHUNK` or less). Once no chunk over
//! `LISTEN_CHUNK` is left unacked, the answerer sends a Listener frame with
//! `VF_READY` (ending its turn) about every 2 RTOs. The caller is the clock
//! master: when it is ready too, has heard that flag, has no large chunk
//! left and holds the turn, it starts slot 0 at once. Every frame it sends
//! from then on carries `VF_SLOTS`; the answerer starts slots when it hears
//! one, and sets its slot clock from it.
//!
//! **Slots.** Slot `n` starts at the caller's `t(n)` and lasts the plan's
//! `frame_ms`. Sub-slot A is the caller's, at the start; sub-slot B is the
//! answerer's, `SLOT_GUARD_MS` after the end of the caller's frame (as
//! heard), or at a fixed offset (the length of sub-slot A) when it may run
//! on its own clock. Every slot frame carries the slot number (header seq)
//! and the sender's lateness against `t(n)` in 4 ms units, so the answerer
//! puts `t(n)` exactly where the caller has it. Nobody transmits before
//! `SLOT_GUARD_MS` after the end of the last frame heard or sent (the
//! "hold"), so a slot that ran long (SF12) pushes the next one back a
//! little and the slack of the following slots catches up.
//!
//! - **Duplex:** both ends send every slot: a Data frame (their queued
//!   sealed voice) or, with nothing queued (silence), a Listener frame.
//! - **Push to talk:** the floor holder sends every slot (Data, or Listener
//!   while the app has no frame). The other end sends a Listener frame on
//!   listener slots, every `listen_every` slots (`LISTEN_EVERY_MS`, scaled
//!   up to every slot when `frame_ms` is longer, and stretched where one
//!   slot cannot hold voice and a Listener frame: SF12 listens every
//!   second slot, with chunks of 24 bytes). With the floor free
//!   both send Listener frames every slot.
//!   Where voice and a listener sub-slot fit in one slot with 20 ms to
//!   spare ("room": SF9, SF11), the non-holder may also send a queued
//!   link frame (TierSet) in any slot.
//! - **Replies:** a TierAck or a Keepalive reply goes out at once (after
//!   the hold), never waiting for a sub-slot: the end that asked listens.
//!
//! **Voice buffers.** `Link::voice_push` holds up to `VOICE_QUEUE_DUPLEX`
//! (2) sealed frames in duplex and `VOICE_QUEUE` (6) in push to talk: our
//! send time moves within the slot (a listener sub-slot first, a slot that
//! ran long), and one frame of slack keeps the app's steady frame clock
//! from losing a frame each time it does; in push to talk the app also
//! sends, at once, the words it kept while waiting for the floor and the
//! short last frame at key up, and the holder keeps the floor until its
//! queue is empty. Each slot
//! sends the oldest. `Link::voice_pop` hands out the last frame heard, once.
//!
//! **Floor (push to talk).** The caller arbitrates. Every frame carries
//! `VF_KEY` (the sender's talk key is down, or the holder's voice frames
//! still wait to go out); the caller's frames carry the
//! owner (none, caller, answerer) in bits 4 and 5. At each slot boundary
//! the caller decides: a free floor goes to the first key down (its own
//! first, then the answerer's); a holder keeps it until its key is up and
//! its last queued frames are sent (nothing said before the key went up is
//! dropped); a
//! key down while the other holds waits. A change is announced in the next
//! slot. The answerer reads the floor from the caller's frames.
//!
//! **Control bytes.** Every slot frame carries the ARQ ack (header ack and
//! a SACK byte). ARQ data rides in Listener frames, one chunk of at most
//! `LISTEN_CHUNK` bytes each (24 at SF12). A side whose control bytes have waited
//! `CTRL_WAIT_MS` since its last Listener frame sends a Listener frame in
//! place of one voice frame, so control flows during continuous talk.
//!
//! **Timers.** Slot frames count as heard frames and SNR samples. While
//! slots run, the link sends no PINGs, and its step-down and resync clocks
//! run on `quiet_ms` (the summed length of the consecutive slots in which
//! the peer was due to send and was not heard; slots in which it was not
//! due add nothing) and `missed` (how many) in place of wall-clock silence.
//! In push to talk a wall clock cap rides on top (`WallCaps`, from no frame
//! of any kind heard; L = listen period): PINGs from 3 L (one per L, the
//! peer answers at once), a step down from 5 L with two PINGs unanswered,
//! a resync from 7 L. These never come sooner, whatever S22 says: a short
//! S22 must not turn the ordinary loss of a listener frame or two into a
//! PING storm or a step down. The due-slot counter may act first:
//!
//! | Tier | PING from | step down from | resync from |
//! |---|---|---|---|
//! | SF9, SF10 | 3.6 s | 6.0 s | 8.4 s |
//! | SF11 | 3.0 s | 5.0 s | 7.0 s |
//! | SF12 | 12.0 s | (slowest) | 28.0 s |
//!
//! S22 ends a call only from a resync. On SF9 to SF11 it counts from the
//! resync, so the resync always comes first. On SF12 (already the resync
//! tier) it counts from the last frame heard: with S22 over 7 L (28 s) the
//! resync comes first; with less (the default 20 s is less) no resync can
//! come in time, and the call ends with NO CARRIER at 7 L of silence, 28 s,
//! later than S22 but never on ordinary loss.
//!
//! Duplex tiers have no cap: every slot is due both ways, so the due-slot
//! count is wall time (resync at 8 RTOs: SF5 1.5 s, SF6 2.0 s, SF7 2.8 s,
//! SF8 4.3 s). Measured in task 6 with the earlier S22-capped ladder (fade
//! under the floor 10 s into a talk spell): resync after 7.2 s at SF9,
//! 8.0 s at SF10, 6.0 s at SF11, 10.0 s at SF12; 300 s at steady SNR with
//! 10 % loss: no step down at SF9 or SF12. The SF9 to SF11 rows above are
//! unchanged by the floor; SF12 now waits the full 7 L (to be measured
//! again in task 9).
//! A Listener frame carries an ARQ chunk only up to the tier's chunk size,
//! except in a voice sub-slot or with the floor free (a longer chunk queued
//! on a faster tier gets through there and never overruns a listener
//! sub-slot). A tier change takes effect for the plan and the slot
//! layout at the next slot boundary (the link reports it with `Retrain`).
//!
//! Bodies (after the sub-type byte; header seq = slot number, header ack =
//! the ARQ ack):
//!
//! - Data (5): flags, late (4 ms units), SACK, sealed voice packet.
//! - Listener (6): flags, late, SACK, SNR (i16 dB x4 little endian), then
//!   with `VF_CHUNK` the ARQ sequence number and the chunk.
//!
//! The 3 body bytes before the voice packet come out of the 249 bytes
//! `plan.rs` allows: a sealed packet of up to `VOICE_MAX` (246) bytes fits;
//! the largest plan packet is 220 (SF12).
//!
//! Slot layout per tier (`slot_layout`, from `Tier::airtime_us`; guard
//! 20 ms after each frame; voice = longest Data frame plus guard, listener =
//! longest Listener frame with the tier's chunk plus guard; free is the
//! share of a slot (duplex) or of a listen period (push to talk) that no
//! sub-slot uses; talk slot free is a push to talk slot without a listener):
//!
//! | Tier | Mode | frame_ms | voice ms | listener ms | chunk | listen every | free | talk slot free |
//! |---|---|---|---|---|---|---|---|---|
//! | SF5 | Duplex | 120 | 46 | 28 | 32 | every slot | 23 % | |
//! | SF6 | Duplex | 200 | 73 | 34 | 32 | every slot | 27 % | |
//! | SF7 | Duplex | 200 | 59 | 44 | 32 | every slot | 41 % | |
//! | SF8 | Duplex | 200 | 74 | 62 | 32 | every slot | 26 % | |
//! | SF9 | PTT | 400 | 144 | 92 | 32 | 3 slots | 56 % | 64 % |
//! | SF10 | PTT | 400 | 246 | 154 | 32 | 3 slots | 25 % | 38 % |
//! | SF11 | PTT | 1000 | 595 | 267 | 32 | every slot | 13 % | 40 % |
//! | SF12 | PTT delayed | 2000 | 1784 | 432 | 20 | 2 slots | 0 % | 10 % |
//!
//! Every duplex tier passes `duplex_fits` (20 % of the slot free with the
//! same 20 ms guard). SF12 cannot hold a listener every slot: 2 s of 700C
//! takes 1.76 s on air as a Data frame, so it listens every second slot.
//! The 20 ms guard must be measured on hardware in the firmware part.
//!
//! Measured on a two modem simulation (no loss, 15 dB SNR, the tier capped
//! with S21, 400 bytes each way through ARQ before both apps say ready;
//! first audio is the first voice frame heard after CONNECT, in push to
//! talk with the caller's key down as soon as the slots run; both modems
//! ticked every 10 ms once the slots run):
//!
//! | Tier | CONNECT to slots | CONNECT to first audio |
//! |---|---|---|
//! | SF5 | 131 ms | 187 ms |
//! | SF6 | 219 ms | 301 ms |
//! | SF7 | 371 ms | 443 ms |
//! | SF8 | 651 ms | 735 ms |
//! | SF9 | 1165 ms | 1658 ms |
//! | SF10 | 2159 ms | 2716 ms |
//! | SF11 | 3945 ms | 5402 ms |
//! | SF12 | 7311 ms | 10834 ms |

use super::frames::VSub;
use super::plan::{talk_plan, TalkMode};
use crate::frame::{self, FrameType, Header, HEADER_LEN, MAX_PAYLOAD};
use crate::link::air_ms;
use crate::tier::Tier;

/// The listener (non-holder) gets a sub-slot at least this often.
pub const LISTEN_EVERY_MS: u32 = 1500;
/// Silence after every frame before the next may start (shared with
/// `duplex_fits`): covers the 10 ms tick and the radio's turnaround. Slots
/// need no carrier sense, so this is shorter than the data turn gap. To be
/// measured on hardware in the firmware part.
pub use super::plan::SLOT_GUARD_MS;
/// Most ARQ bytes in one Listener frame.
pub const LISTEN_CHUNK: usize = 32;
/// Control bytes waiting this long since our last Listener frame take the
/// place of one voice frame.
pub const CTRL_WAIT_MS: u32 = 1000;
use super::plan::DATA_HEAD;
/// Listener body bytes before the chunk: flags, late, SACK, SNR.
const LIS_HEAD: usize = 5;
/// Longest sealed voice packet a Data frame carries.
pub const VOICE_MAX: usize = MAX_PAYLOAD - 1 - DATA_HEAD;
/// Voice frames `Link::voice_push` holds in push to talk: one slot of
/// jitter (our send time moves within the slot when a listener sub-slot
/// comes first) on top of the frame for the next slot, the app's words
/// kept while it waited for the floor (up to three frames, sent at once
/// when the floor comes) and the short last frame at key up.
pub const VOICE_QUEUE: usize = 6;
/// Duplex uses only this many of them (a deeper queue there would only
/// add delay).
pub const VOICE_QUEUE_DUPLEX: usize = 2;
/// Spare time a slot needs beyond voice and listener sub-slots before a
/// non-holder may use it outside its listener slots ("room"): two 10 ms
/// ticks of jitter, since the holder then runs on its own clock.
const ROOM_MARGIN_MS: u32 = 20;
/// Most slots in one listen period.
const MAX_LISTEN_EVERY: u8 = 8;

/// Body flags.
pub(crate) const VF_READY: u8 = 0x01;
pub(crate) const VF_SLOTS: u8 = 0x02;
pub(crate) const VF_KEY: u8 = 0x04;
pub(crate) const VF_CHUNK: u8 = 0x08;
const VF_OWNER_SHIFT: u8 = 4;

/// Who holds the push to talk floor, from this end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Floor {
    Mine,
    Theirs,
    Free,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    None,
    Caller,
    Answerer,
}

impl Owner {
    fn bits(self) -> u8 {
        match self {
            Owner::None => 0,
            Owner::Caller => 1,
            Owner::Answerer => 2,
        }
    }

    fn from_flags(f: u8) -> Owner {
        match (f >> VF_OWNER_SHIFT) & 3 {
            1 => Owner::Caller,
            2 => Owner::Answerer,
            _ => Owner::None,
        }
    }
}

/// Sub-slot times of a tier's talk plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotLayout {
    pub mode: TalkMode,
    pub frame_ms: u32,
    /// Longest Data frame on air plus the guard.
    pub voice_ms: u32,
    /// Longest Listener frame (with a `chunk` byte chunk) on air plus the
    /// guard.
    pub listen_ms: u32,
    /// Push to talk: the non-holder's Listener slot comes every this many
    /// slots (1 in duplex).
    pub listen_every: u8,
    /// Most ARQ bytes per Listener frame on this tier (`LISTEN_CHUNK`, less
    /// where that buys a shorter listen period: SF12).
    pub chunk: u8,
    /// Voice and a listener sub-slot fit in one slot with 20 ms to spare.
    pub room: bool,
}

impl SlotLayout {
    /// Percent of a slot (duplex) or listen period (push to talk) left free.
    pub fn free_pct(&self) -> u32 {
        let k = u32::from(self.listen_every.max(1));
        let period = self.frame_ms.saturating_mul(k).max(1);
        let used = match self.mode {
            TalkMode::Duplex => self.voice_ms.saturating_mul(2),
            _ => self
                .voice_ms
                .saturating_mul(k)
                .saturating_add(self.listen_ms),
        };
        period.saturating_sub(used).saturating_mul(100) / period
    }
}

/// Smallest chunk worth carrying in a Listener frame.
const MIN_CHUNK: usize = 8;

/// Listener frame airtime plus guard with a chunk of `c` bytes.
fn listen_ms(t: Tier, c: usize) -> u32 {
    air_ms(t, HEADER_LEN + 1 + LIS_HEAD + 1 + c).saturating_add(SLOT_GUARD_MS)
}

/// The slot layout for a tier. Push to talk: the listen period is the
/// smallest number of slots, from `LISTEN_EVERY_MS / frame_ms` (at least
/// one) up, whose spare time holds a Listener frame with the largest chunk
/// (down to 8 bytes) that fits.
pub fn slot_layout(t: Tier) -> SlotLayout {
    let p = talk_plan(t);
    let frame_ms = u32::from(p.frame_ms);
    let data_len = HEADER_LEN + 1 + DATA_HEAD + usize::from(p.max_sealed_bytes);
    let full = listen_ms(t, LISTEN_CHUNK);
    let voice_ms = air_ms(t, data_len).saturating_add(SLOT_GUARD_MS).max(full);
    let (mut k, mut chunk) = (1u8, LISTEN_CHUNK);
    if p.mode != TalkMode::Duplex {
        k = u8::try_from((LISTEN_EVERY_MS / frame_ms.max(1)).max(1))
            .unwrap_or(1)
            .min(MAX_LISTEN_EVERY);
        let fits = |k: u8, c: usize| {
            let k = u32::from(k);
            voice_ms.saturating_mul(k).saturating_add(listen_ms(t, c)) <= frame_ms.saturating_mul(k)
        };
        loop {
            if let Some(c) = (MIN_CHUNK..=LISTEN_CHUNK).rev().find(|&c| fits(k, c)) {
                chunk = c;
                break;
            }
            if k >= MAX_LISTEN_EVERY {
                break;
            }
            k += 1;
        }
    }
    let lis = listen_ms(t, chunk);
    SlotLayout {
        mode: p.mode,
        frame_ms,
        voice_ms,
        listen_ms: lis,
        listen_every: k,
        chunk: chunk as u8,
        room: voice_ms.saturating_add(lis).saturating_add(ROOM_MARGIN_MS) <= frame_ms,
    }
}

/// `a` is at or after `b` (wrapping u32 milliseconds).
fn reached(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < u32::MAX / 2
}

/// Wall clock silence ladder while push to talk slots run (no frame of any
/// kind heard, in ms): from `ping` a Keepalive PING every `every` (one
/// listen period; the peer answers at once), from `step` with two PINGs
/// unanswered a step down a tier, from `resync` a resync. `L` is the listen
/// period (`listen_every` x `frame_ms`): ping 3 L, step 5 L, resync 7 L,
/// never less whatever S22 is (see the module notes for a short S22).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct WallCaps {
    pub ping: u32,
    pub step: u32,
    pub resync: u32,
    pub every: u32,
}

/// What our turn in a slot may carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Turn {
    /// Our sub-slot: a Data or Listener frame (or a queued link frame).
    Due,
    /// Only a queued link frame (room in the slot, not our sub-slot).
    Extra,
}

/// A parsed Data or Listener body.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Heard<'a> {
    pub flags: u8,
    pub late: u8,
    pub sack: u8,
    /// Listener: the SNR the peer hears us at.
    pub snr: Option<i16>,
    /// Data: the sealed voice packet.
    pub voice: &'a [u8],
    /// Listener: an ARQ chunk.
    pub chunk: Option<(u8, &'a [u8])>,
}

pub(crate) fn parse(sub: VSub, body: &[u8]) -> Option<Heard<'_>> {
    match sub {
        VSub::Data => {
            let [flags, late, sack, voice @ ..] = body else {
                return None;
            };
            Some(Heard {
                flags: *flags,
                late: *late,
                sack: *sack,
                snr: None,
                voice,
                chunk: None,
            })
        }
        VSub::Listener => {
            let [flags, late, sack, a, b, rest @ ..] = body else {
                return None;
            };
            let chunk = if flags & VF_CHUNK != 0 {
                let (&seq, d) = rest.split_first()?;
                Some((seq, d))
            } else if rest.is_empty() {
                None
            } else {
                return None;
            };
            Some(Heard {
                flags: *flags,
                late: *late,
                sack: *sack,
                snr: Some(i16::from_le_bytes([*a, *b])),
                voice: &[],
                chunk,
            })
        }
        _ => None,
    }
}

/// Layout before the slots start (replaced at slot 0).
const INITIAL_LAYOUT: SlotLayout = SlotLayout {
    mode: TalkMode::PttDelayed,
    frame_ms: 2000,
    voice_ms: 0,
    listen_ms: 0,
    listen_every: 1,
    chunk: LISTEN_CHUNK as u8,
    room: false,
};

/// Slot clock, floor, the two-frame voice send queue and the one-frame
/// receive buffer of a voice call.
pub struct VoiceSched {
    /// The app said VoiceReady.
    pub(crate) ready_mine: bool,
    /// Caller: the answerer's `VF_READY` was heard.
    pub(crate) ready_peer: bool,
    /// Voice slots run (from here on until the call ends).
    pub(crate) slots: bool,
    /// Answerer: when the last ready frame was queued.
    pub(crate) ready_at: Option<u32>,
    caller: bool,
    /// Answerer: the slot clock is set from a caller frame.
    synced: bool,
    tier: Tier,
    lay: SlotLayout,
    slot_start: u32,
    n: u8,
    /// We sent in this slot.
    sent: bool,
    /// We heard the peer in this slot.
    heard: bool,
    /// Nothing goes out before this.
    hold: u32,
    owner: Owner,
    /// Caller: the owner changed at this slot's start; say so.
    announce: bool,
    key: bool,
    /// Caller: the answerer's talk key, as last heard.
    peer_key: bool,
    /// Silence: the length of each consecutive slot in which the peer was due
    /// and not heard.
    pub(crate) quiet_ms: u32,
    /// How many such slots.
    pub(crate) missed: u8,
    /// When we last sent a Listener frame.
    pub(crate) last_lis: u32,
    /// Voice frames waiting to go out (a ring of `VOICE_QUEUE`).
    tx: [[u8; VOICE_MAX]; VOICE_QUEUE],
    tx_len: [u8; VOICE_QUEUE],
    tx_head: usize,
    tx_count: usize,
    rx: [u8; VOICE_MAX],
    rx_len: u8,
}

impl Default for VoiceSched {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceSched {
    pub const fn new() -> Self {
        VoiceSched {
            ready_mine: false,
            ready_peer: false,
            slots: false,
            ready_at: None,
            caller: false,
            synced: false,
            tier: Tier::ROBUST,
            lay: INITIAL_LAYOUT,
            slot_start: 0,
            n: 0,
            sent: false,
            heard: false,
            hold: 0,
            owner: Owner::None,
            announce: false,
            key: false,
            peer_key: false,
            quiet_ms: 0,
            missed: 0,
            last_lis: 0,
            tx: [[0; VOICE_MAX]; VOICE_QUEUE],
            tx_len: [0; VOICE_QUEUE],
            tx_head: 0,
            tx_count: 0,
            rx: [0; VOICE_MAX],
            rx_len: 0,
        }
    }

    /// Back to the state of `new` in place (no large temporary on the
    /// stack; the voice buffers are only marked empty).
    pub(crate) fn reset(&mut self) {
        self.ready_mine = false;
        self.ready_peer = false;
        self.slots = false;
        self.ready_at = None;
        self.caller = false;
        self.synced = false;
        self.sent = false;
        self.heard = false;
        self.owner = Owner::None;
        self.announce = false;
        self.key = false;
        self.peer_key = false;
        self.quiet_ms = 0;
        self.missed = 0;
        self.tx_head = 0;
        self.tx_count = 0;
        self.rx_len = 0;
        self.tier = Tier::ROBUST;
        self.lay = INITIAL_LAYOUT;
        self.slot_start = 0;
        self.n = 0;
        self.hold = 0;
        self.last_lis = 0;
    }

    /// An ARQ chunk of `len` bytes, longer than this tier's listener chunk
    /// (queued on a faster tier), may go in our sub-slot now: it is a voice
    /// sub-slot (duplex, or we hold the floor), or the floor is free (both
    /// send listeners, with time to spare), or the listen period is several
    /// slots and the longer Listener frame runs into the next slot by no
    /// more than that slot's spare time (SF12: 82 ms of 216), which the
    /// holder's hold and the next slots absorb.
    pub(crate) fn big_chunk_ok(&self, len: usize) -> bool {
        if self.may_talk() || self.owner == Owner::None {
            return true;
        }
        let over = listen_ms(self.tier, len).saturating_sub(self.lay.listen_ms);
        let spare = self
            .lay
            .frame_ms
            .saturating_sub(self.lay.voice_ms)
            .saturating_sub(SLOT_GUARD_MS);
        self.lay.listen_every >= 2 && over <= spare
    }

    /// Push to talk with the slots running: the wall clock silence ladder
    /// (`WallCaps`). Its steps are whole listen periods and do not move with
    /// S22 (a short S22 must not make ordinary loss look like a fade); with
    /// S22 longer than 7 L the resync comes before S22 can end the call.
    /// None in duplex, where every slot is due both ways and the due-slot
    /// count already is wall time.
    pub(crate) fn wall_caps(&self) -> Option<WallCaps> {
        if !self.slots || !self.synced || self.duplex() {
            return None;
        }
        let l = self
            .lay
            .frame_ms
            .saturating_mul(u32::from(self.lay.listen_every.max(1)));
        Some(WallCaps {
            ping: l.saturating_mul(3),
            step: l.saturating_mul(5),
            resync: l.saturating_mul(7),
            every: l,
        })
    }

    /// The slot clock runs here (always on the caller; on the answerer
    /// once a caller frame set it).
    pub(crate) fn synced(&self) -> bool {
        self.synced
    }

    fn duplex(&self) -> bool {
        self.lay.mode == TalkMode::Duplex
    }

    fn listen_slot(&self) -> bool {
        self.n.is_multiple_of(self.lay.listen_every.max(1))
    }

    /// The caller sends a regular frame in this slot.
    fn caller_due(&self) -> bool {
        self.duplex()
            || self.announce
            || match self.owner {
                Owner::None | Owner::Caller => true,
                Owner::Answerer => self.listen_slot(),
            }
    }

    /// The answerer sends a regular frame in this slot.
    fn answerer_due(&self) -> bool {
        self.duplex()
            || match self.owner {
                Owner::None | Owner::Answerer => true,
                Owner::Caller => self.listen_slot(),
            }
    }

    fn me_due(&self) -> bool {
        if self.caller {
            self.caller_due()
        } else {
            self.answerer_due()
        }
    }

    fn peer_due(&self) -> bool {
        if self.caller {
            self.answerer_due()
        } else {
            self.caller_due()
        }
    }

    fn me_owner(&self) -> Owner {
        if self.caller {
            Owner::Caller
        } else {
            Owner::Answerer
        }
    }

    /// Our voice frames may go out: duplex, or we hold the floor.
    pub(crate) fn may_talk(&self) -> bool {
        self.duplex() || self.owner == self.me_owner()
    }

    /// Who holds the floor, from this end. Free in duplex and before the
    /// voice slots start.
    pub fn floor(&self) -> Floor {
        if !self.slots || self.duplex() {
            return Floor::Free;
        }
        match self.owner {
            Owner::None => Floor::Free,
            o if o == self.me_owner() => Floor::Mine,
            _ => Floor::Theirs,
        }
    }

    pub(crate) fn set_key(&mut self, down: bool) {
        self.key = down;
    }

    /// Our push to talk key (the app's last TalkKey).
    pub(crate) fn key(&self) -> bool {
        self.key
    }

    /// Queue one sealed voice frame; false if the queue is full
    /// (`VOICE_QUEUE_DUPLEX` in duplex, `VOICE_QUEUE` in push to talk) or it
    /// is empty or too long.
    pub(crate) fn push_voice(&mut self, sealed: &[u8]) -> bool {
        let cap = if self.duplex() {
            VOICE_QUEUE_DUPLEX
        } else {
            VOICE_QUEUE
        };
        if self.tx_count >= cap || sealed.is_empty() || sealed.len() > VOICE_MAX {
            return false;
        }
        let i = (self.tx_head + self.tx_count) % VOICE_QUEUE;
        if let (Some(d), Some(l)) = (
            self.tx.get_mut(i).and_then(|b| b.get_mut(..sealed.len())),
            self.tx_len.get_mut(i),
        ) {
            d.copy_from_slice(sealed);
            *l = sealed.len() as u8;
            self.tx_count += 1;
            return true;
        }
        false
    }

    /// Length of the oldest waiting voice frame (0 when none).
    fn head_len(&self) -> u8 {
        if self.tx_count == 0 {
            return 0;
        }
        self.tx_len.get(self.tx_head).copied().unwrap_or(0)
    }

    /// The last voice frame heard (each one once).
    pub(crate) fn pop_voice(&mut self, out: &mut [u8]) -> usize {
        let n = usize::from(self.rx_len);
        match (out.get_mut(..n), self.rx.get(..n)) {
            (Some(d), Some(s)) if n > 0 => {
                d.copy_from_slice(s);
                self.rx_len = 0;
                n
            }
            _ => 0,
        }
    }

    pub(crate) fn store_voice(&mut self, v: &[u8]) {
        if v.is_empty() {
            return;
        }
        if let Some(d) = self.rx.get_mut(..v.len()) {
            d.copy_from_slice(v);
            self.rx_len = v.len() as u8;
        }
    }

    /// The oldest waiting voice frame fits the plan of the current slot.
    pub(crate) fn voice_waiting(&self, t: Tier) -> bool {
        let n = self.head_len();
        n > 0 && n <= talk_plan(t).max_sealed_bytes
    }

    /// Any voice frame waits.
    pub(crate) fn voice_queued(&self) -> bool {
        self.tx_count > 0
    }

    /// Drop the oldest waiting voice frame.
    pub(crate) fn drop_voice(&mut self) {
        if self.tx_count > 0 {
            self.tx_head = (self.tx_head + 1) % VOICE_QUEUE;
            self.tx_count -= 1;
        }
    }

    /// Drop every waiting voice frame.
    pub(crate) fn clear_voice(&mut self) {
        self.tx_count = 0;
    }

    /// Caller: start the voice slots now (slot 0).
    pub(crate) fn start_caller(&mut self, now: u32, t: Tier) {
        self.caller = true;
        self.slots = true;
        self.synced = true;
        self.tier = t;
        self.lay = slot_layout(t);
        self.slot_start = now;
        self.n = 0;
        self.hold = now;
        self.owner = Owner::None;
        self.begin_slot(t);
        self.reset_quiet();
    }

    /// Answerer: a caller frame said the slots run.
    pub(crate) fn start_answerer(&mut self, now: u32, t: Tier) {
        self.caller = false;
        self.slots = true;
        self.synced = false;
        self.tier = t;
        self.lay = slot_layout(t);
        self.hold = now;
        self.owner = Owner::None;
        self.reset_quiet();
    }

    /// Back from a resync: the caller starts a new slot now; the answerer
    /// waits for a caller frame before sending in a slot again.
    pub(crate) fn restart(&mut self, now: u32, t: Tier) {
        self.reset_quiet();
        self.hold = now;
        if self.caller {
            self.slot_start = now;
            self.n = self.n.wrapping_add(1);
            self.begin_slot(t);
        } else {
            self.synced = false;
        }
    }

    fn reset_quiet(&mut self) {
        self.quiet_ms = 0;
        self.missed = 0;
    }

    /// A slot starts: plan change, floor decision (caller).
    fn begin_slot(&mut self, t: Tier) {
        self.sent = false;
        self.heard = false;
        self.announce = false;
        if t != self.tier {
            self.tier = t;
            self.lay = slot_layout(t);
        }
        if self.caller && !self.duplex() {
            // A holder keeps the floor while its key is down or its voice
            // frames still wait to go out (the last words, queued when the
            // key went up, are sent, not dropped; a key down at the other
            // end waits); a free floor goes to our key first.
            let next = match self.owner {
                Owner::Caller if self.key || self.voice_queued() => Owner::Caller,
                Owner::Answerer if self.peer_key => Owner::Answerer,
                _ if self.key => Owner::Caller,
                _ if self.peer_key => Owner::Answerer,
                _ => Owner::None,
            };
            if next != self.owner {
                self.owner = next;
                self.announce = true;
            }
        }
    }

    /// A slot ends: if the peer was due and not heard, its length counts
    /// as silence (only due slots count; a listen period's other slots do
    /// not).
    fn end_slot(&mut self) {
        if self.peer_due() && !self.heard {
            self.quiet_ms = self.quiet_ms.saturating_add(self.lay.frame_ms);
            self.missed = self.missed.saturating_add(1);
        }
    }

    /// Move the slot clock up to `now`; `t` is the link's tier.
    pub(crate) fn advance(&mut self, now: u32, t: Tier) {
        if !self.slots || !self.synced {
            return;
        }
        let ms = self.lay.frame_ms.max(1);
        if now.wrapping_sub(self.slot_start) >= ms.saturating_mul(8)
            && reached(now, self.slot_start)
        {
            // Far behind (the tick stalled): start again from here.
            if self.caller {
                self.slot_start = now;
                self.n = self.n.wrapping_add(1);
                self.begin_slot(t);
            } else {
                self.synced = false;
            }
            return;
        }
        let mut i = 0;
        while i < 8
            && reached(now, self.slot_start)
            && now.wrapping_sub(self.slot_start) >= self.lay.frame_ms.max(1)
        {
            let end = self.slot_start.wrapping_add(self.lay.frame_ms);
            self.end_slot();
            self.slot_start = end;
            self.n = self.n.wrapping_add(1);
            self.begin_slot(t);
            i += 1;
        }
    }

    /// Answerer: a caller slot frame of `len` bytes ended now. Sets the
    /// slot clock and the floor from it.
    pub(crate) fn sync(&mut self, now: u32, t: Tier, len: usize, seq: u8, h: &Heard<'_>) {
        if self.caller {
            return;
        }
        let air = air_ms(t, len);
        let start = now
            .wrapping_sub(air)
            .wrapping_sub(u32::from(h.late).saturating_mul(4));
        if !self.synced || seq != self.n || t != self.tier {
            self.n = seq;
            self.sent = false;
            self.heard = false;
            if t != self.tier {
                self.tier = t;
                self.lay = slot_layout(t);
            }
        }
        self.slot_start = start;
        self.synced = true;
        self.owner = Owner::from_flags(h.flags);
    }

    /// Caller: the answerer's talk key from its frame.
    pub(crate) fn peer_flags(&mut self, flags: u8) {
        if self.caller {
            self.peer_key = flags & VF_KEY != 0;
        }
    }

    /// The peer was heard (any frame, ending now).
    pub(crate) fn on_heard(&mut self, now: u32) {
        self.heard = true;
        self.hold = now.wrapping_add(SLOT_GUARD_MS);
        self.quiet_ms = 0;
        self.missed = 0;
    }

    /// We may transmit now as far as the hold goes.
    pub(crate) fn hold_ok(&self, now: u32) -> bool {
        reached(now, self.hold)
    }

    /// Is it our turn in this slot? `q` is true when a link frame waits.
    pub(crate) fn turn(&self, now: u32, q: bool) -> Option<Turn> {
        if !self.slots || !self.synced || self.sent || !self.hold_ok(now) {
            return None;
        }
        if !reached(now, self.slot_start) {
            return None;
        }
        let into = now.wrapping_sub(self.slot_start);
        if into >= self.lay.frame_ms {
            return None;
        }
        let due = self.me_due();
        let extra = !due && q && self.lay.room;
        if !due && !extra {
            return None;
        }
        let kind = if due { Turn::Due } else { Turn::Extra };
        if self.caller {
            // Not holding while the answerer does: its voice follows our
            // sub-slot on its own clock, so only an on-time start is safe.
            if !self.duplex() && self.owner == Owner::Answerer && into > SLOT_GUARD_MS {
                return None;
            }
            return Some(kind);
        }
        let caller_may = self.caller_due() || self.lay.room;
        if !caller_may || self.heard {
            return Some(kind);
        }
        let free_run = self.duplex() || (self.owner == Owner::Answerer && self.key);
        if !free_run {
            return None;
        }
        let a_len = if self.duplex() || self.owner == Owner::Caller {
            self.lay.voice_ms
        } else {
            self.lay.listen_ms
        };
        (into >= a_len).then_some(kind)
    }

    /// We sent a frame of `air` ms now (in our turn or as a reply).
    pub(crate) fn on_sent(&mut self, now: u32, air: u32, listener: bool) {
        self.sent = true;
        if listener {
            self.last_lis = now;
        }
        if self.caller {
            self.announce = false;
        }
        let mut hold = air.saturating_add(SLOT_GUARD_MS);
        if self.caller && self.answerer_due() {
            // The answerer's sub-slot follows ours: keep it clear.
            let b = if self.duplex() || self.owner == Owner::Answerer {
                self.lay.voice_ms
            } else {
                self.lay.listen_ms
            };
            hold = hold.saturating_add(b);
        }
        self.hold = now.wrapping_add(hold);
    }

    /// Our talk key as the peer should see it: down, or up with our voice
    /// frames still waiting while we hold the floor (the holder keeps the
    /// floor until its last words are out).
    fn key_flag(&self) -> bool {
        self.key || (self.voice_queued() && self.owner == self.me_owner())
    }

    /// Our body flags.
    fn flags(&self, chunk: bool) -> u8 {
        let mut f = 0;
        if self.ready_mine {
            f |= VF_READY;
        }
        if self.slots {
            f |= VF_SLOTS;
        }
        if self.key_flag() {
            f |= VF_KEY;
        }
        if chunk {
            f |= VF_CHUNK;
        }
        if self.caller {
            f |= self.owner.bits() << VF_OWNER_SHIFT;
        }
        f
    }

    fn late(&self, now: u32) -> u8 {
        if !self.slots || !reached(now, self.slot_start) {
            return 0;
        }
        u8::try_from(now.wrapping_sub(self.slot_start) / 4).unwrap_or(u8::MAX)
    }

    fn header(&self, call_id: u8, ack: u8) -> Header {
        Header {
            ftype: FrameType::Voice,
            flags: 0,
            call_id,
            seq: if self.slots { self.n } else { 0 },
            ack,
        }
    }

    /// A Data frame with the waiting voice packet (taken). None when none
    /// waits or `out` is too small.
    pub(crate) fn build_data(
        &mut self,
        call_id: u8,
        (ack, sack): (u8, u8),
        now: u32,
        out: &mut [u8],
    ) -> Option<usize> {
        let n = usize::from(self.head_len());
        if n == 0 {
            return None;
        }
        let mut p = [0u8; MAX_PAYLOAD];
        let head = [VSub::Data as u8, self.flags(false), self.late(now), sack];
        p.get_mut(..head.len())?.copy_from_slice(&head);
        p.get_mut(head.len()..head.len() + n)?
            .copy_from_slice(self.tx.get(self.tx_head)?.get(..n)?);
        let len = frame::encode(&self.header(call_id, ack), p.get(..head.len() + n)?, out).ok()?;
        self.drop_voice();
        Some(len)
    }

    /// A Listener frame: our SNR, the ack, and an ARQ chunk if given.
    pub(crate) fn build_listener(
        &self,
        call_id: u8,
        (ack, sack): (u8, u8),
        snr: i16,
        chunk: Option<(u8, &[u8])>,
        now: u32,
        out: &mut [u8],
    ) -> Option<usize> {
        let mut p = [0u8; MAX_PAYLOAD];
        let s = snr.to_le_bytes();
        let head = [
            VSub::Listener as u8,
            self.flags(chunk.is_some()),
            self.late(now),
            sack,
            s[0],
            s[1],
        ];
        p.get_mut(..head.len())?.copy_from_slice(&head);
        let mut len = head.len();
        if let Some((seq, d)) = chunk {
            *p.get_mut(len)? = seq;
            len += 1;
            p.get_mut(len..len + d.len())?.copy_from_slice(d);
            len += d.len();
        }
        frame::encode(&self.header(call_id, ack), p.get(..len)?, out).ok()
    }
}
