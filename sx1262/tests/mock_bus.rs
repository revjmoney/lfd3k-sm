//! Driver tests on a mock bus. The mock is a small SX1262: it records every
//! SPI transaction, pin change and reset, keeps registers and the IRQ
//! register, and replies by opcode. The clock is a counter that moves only
//! when the test (or a BUSY poll) moves it, so every run is the same run.

use std::cell::Cell;
use std::collections::BTreeMap;

use lfd3k_core::{Radio, Tier};
use sx1262::cmd;
use sx1262::{
    Bus, Error, Sx1262, BUSY_LIMIT_US, INIT_BUSY_LIMIT_US, POWER_MAX_DBM, POWER_MIN_DBM,
    PREAMBLE_GUARD_EXTRA_SYMBOLS, TX_MARGIN_US,
};

#[derive(Clone, PartialEq, Eq, Debug)]
enum Ev {
    Reset,
    Fem(bool),
    Xfer(Vec<u8>),
}

struct Mock {
    events: Vec<Ev>,
    clock: Cell<u64>,
    /// Microseconds added by every `now_us` call.
    step: u64,
    /// BUSY reads high while the clock is below this.
    busy_until: Cell<u64>,
    regs: BTreeMap<u16, u8>,
    status: u8,
    irq: u16,
    dev_errors: u16,
    payload: Vec<u8>,
    buf_start: u8,
    rssi_pkt: u8,
    snr_pkt: u8,
    /// Value stored in a register before the driver writes it.
    sync_stuck: Option<(u16, u8)>,
}

impl Mock {
    fn new() -> Self {
        let mut regs = BTreeMap::new();
        regs.insert(cmd::REG_SENSITIVITY, 0x0F);
        regs.insert(cmd::REG_TX_CLAMP, 0x01);
        Mock {
            events: Vec::new(),
            clock: Cell::new(0),
            step: 1,
            busy_until: Cell::new(0),
            regs,
            status: 0x20,
            irq: 0,
            dev_errors: 0,
            payload: Vec::new(),
            buf_start: 7,
            rssi_pkt: 0,
            snr_pkt: 0,
            sync_stuck: None,
        }
    }

    fn xfers(&self) -> Vec<Vec<u8>> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Ev::Xfer(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn deliver(&mut self, frame: &[u8], rssi_pkt: u8, snr_pkt: u8, irq: u16) {
        self.payload = frame.to_vec();
        self.rssi_pkt = rssi_pkt;
        self.snr_pkt = snr_pkt;
        self.irq |= irq;
    }
}

impl Bus for Mock {
    fn xfer(&mut self, tx: &[u8], rx: &mut [u8]) {
        assert_eq!(tx.len(), rx.len(), "driver must pass equal lengths");
        self.events.push(Ev::Xfer(tx.to_vec()));
        let b = |i: usize| tx.get(i).copied().unwrap_or(0);
        let mut put = |i: usize, v: u8| {
            if let Some(d) = rx.get_mut(i) {
                *d = v;
            }
        };
        match b(0) {
            cmd::GET_STATUS => put(1, self.status),
            cmd::GET_IRQ_STATUS => {
                put(2, (self.irq >> 8) as u8);
                put(3, self.irq as u8);
            }
            cmd::CLEAR_IRQ_STATUS => self.irq &= !u16::from_be_bytes([b(1), b(2)]),
            cmd::GET_RX_BUFFER_STATUS => {
                put(2, self.payload.len() as u8);
                put(3, self.buf_start);
            }
            cmd::READ_BUFFER => {
                assert_eq!(
                    b(1),
                    self.buf_start,
                    "ReadBuffer must start at the reported offset"
                );
                for (i, &v) in self.payload.iter().enumerate() {
                    put(3 + i, v);
                }
            }
            cmd::GET_PACKET_STATUS => {
                put(2, self.rssi_pkt);
                put(3, self.snr_pkt);
            }
            cmd::GET_DEVICE_ERRORS => {
                put(2, (self.dev_errors >> 8) as u8);
                put(3, self.dev_errors as u8);
            }
            cmd::WRITE_REGISTER => {
                let addr = u16::from_be_bytes([b(1), b(2)]);
                let v = match self.sync_stuck {
                    Some((a, stuck)) if a == addr => stuck,
                    _ => b(3),
                };
                self.regs.insert(addr, v);
            }
            cmd::READ_REGISTER => {
                let addr = u16::from_be_bytes([b(1), b(2)]);
                put(4, self.regs.get(&addr).copied().unwrap_or(0));
            }
            _ => {}
        }
    }

    fn busy(&self) -> bool {
        self.clock.get() < self.busy_until.get()
    }

    fn reset(&mut self) {
        self.events.push(Ev::Reset);
    }

    fn fem(&mut self, tx: bool) {
        self.events.push(Ev::Fem(tx));
    }

    fn now_us(&self) -> u64 {
        let t = self.clock.get();
        self.clock.set(t + self.step);
        t
    }
}

fn radio() -> Sx1262<Mock> {
    Sx1262::new(Mock::new())
}

fn ready() -> Sx1262<Mock> {
    let mut r = radio();
    r.init(2).expect("init");
    r.bus_mut().events.clear();
    r
}

fn opcodes(r: &Sx1262<Mock>) -> Vec<u8> {
    r.bus().xfers().iter().map(|t| t[0]).collect()
}

fn rx_buf() -> [u8; 255] {
    [0u8; 255]
}

#[test]
fn init_sequence_matches_datasheet() {
    let mut r = radio();
    r.init(2).expect("init");
    let want: Vec<Ev> = [
        vec![Ev::Fem(false), Ev::Reset],
        [
            vec![0x80, 0x00],
            // TCXO 1.8 V, 320 steps of 15.625 us = 5 ms.
            vec![0x97, 0x02, 0x00, 0x01, 0x40],
            vec![0x89, 0x7F],
            vec![0x07, 0x00, 0x00],
            vec![0x96, 0x01],
            vec![0x98, 0xE1, 0xE9],
            vec![0x9D, 0x01],
            vec![0x8A, 0x01],
            // 918.5 MHz.
            vec![0x86, 0x39, 0x68, 0x00, 0x00],
            vec![0x95, 0x04, 0x07, 0x00, 0x01],
            vec![0x0D, 0x08, 0xE7, 0x38],
            vec![0x8E, 0x02, 0x02],
            // Private sync word 0x1424.
            vec![0x0D, 0x07, 0x40, 0x14],
            vec![0x0D, 0x07, 0x41, 0x24],
            vec![0x8F, 0x00, 0x00],
            vec![0x93, 0x30],
            vec![0x08, 0x02, 0x77, 0x02, 0x77, 0x00, 0x00, 0x00, 0x00],
            // Errata 15.2: read 0x08D8 (0x01 in the mock), write it |= 0x1E.
            vec![0x1D, 0x08, 0xD8, 0x00, 0x00],
            vec![0x0D, 0x08, 0xD8, 0x1F],
            // Boosted receive gain: RxGain 0x08AC = 0x96.
            vec![0x0D, 0x08, 0xAC, 0x96],
            vec![0x02, 0x03, 0xFF],
            vec![0xC0, 0x00],
            vec![0x1D, 0x07, 0x40, 0x00, 0x00],
            vec![0x1D, 0x07, 0x41, 0x00, 0x00],
        ]
        .into_iter()
        .map(Ev::Xfer)
        .collect(),
    ]
    .concat();
    assert_eq!(r.bus().events, want);
    assert!(!r.failed());
    assert!(!r.busy());
    assert_eq!(r.bus().regs.get(&cmd::REG_SYNC_WORD_MSB), Some(&0x14));
    assert_eq!(r.bus().regs.get(&cmd::REG_SYNC_WORD_LSB), Some(&0x24));
}

#[test]
fn init_no_chip_and_config_errors() {
    // MISO stuck low: chip mode 0.
    let mut r = radio();
    r.bus_mut().status = 0x00;
    assert_eq!(r.init(2), Err(Error::NoChip));
    assert!(r.failed());
    // MISO stuck high: chip mode 7.
    let mut r = radio();
    r.bus_mut().status = 0xFF;
    assert_eq!(r.init(2), Err(Error::NoChip));
    // Sync word does not read back.
    let mut r = radio();
    r.bus_mut().sync_stuck = Some((cmd::REG_SYNC_WORD_LSB, 0x00));
    assert_eq!(r.init(2), Err(Error::Config));
    assert!(r.failed());
    // A later good init clears the failure.
    r.bus_mut().sync_stuck = None;
    r.init(2).expect("init");
    assert!(!r.failed());
}

#[test]
fn tier_change_sets_sf_and_preamble() {
    let mut r = ready();
    r.listen(Tier::Sf5);
    let want: Vec<Vec<u8>> = vec![
        vec![0x80, 0x01],
        // SF5, BW 0x06 (500 kHz), CR 4/5, LDRO off.
        vec![0x8B, 0x05, 0x06, 0x01, 0x00],
        // Errata 15.1: read 0x0889 (0x0F), write with bit 2 cleared.
        vec![0x1D, 0x08, 0x89, 0x00, 0x00],
        vec![0x0D, 0x08, 0x89, 0x0B],
        // Preamble 12 at SF5, explicit header, 255 bytes, CRC on, IQ standard.
        vec![0x8C, 0x00, 0x0C, 0x00, 0xFF, 0x01, 0x00],
        vec![0x02, 0x03, 0xFF],
        vec![0x82, 0xFF, 0xFF, 0xFF],
    ];
    assert_eq!(r.bus().xfers(), want);
    assert_eq!(r.bus().events.first(), Some(&Ev::Fem(false)));

    r.bus_mut().events.clear();
    r.listen(Tier::Sf12);
    let x = r.bus().xfers();
    assert!(
        x.contains(&vec![0x8B, 0x0C, 0x06, 0x01, 0x00]),
        "SF12 modulation"
    );
    assert!(
        x.contains(&vec![0x8C, 0x00, 0x08, 0x00, 0xFF, 0x01, 0x00]),
        "SF12 preamble 8"
    );
    assert!(
        x.contains(&vec![0x0D, 0x08, 0x89, 0x0B]),
        "errata on the tier change"
    );

    // SF6 also gets 12; SF7 gets 8.
    for (t, sf, pre) in [(Tier::Sf6, 6u8, 12u8), (Tier::Sf7, 7, 8)] {
        r.bus_mut().events.clear();
        r.listen(t);
        let x = r.bus().xfers();
        assert!(x.contains(&vec![0x8B, sf, 0x06, 0x01, 0x00]));
        assert!(x.contains(&vec![0x8C, 0x00, pre, 0x00, 0xFF, 0x01, 0x00]));
    }

    // Listening again on the same tier skips the modulation setup.
    r.bus_mut().events.clear();
    r.listen(Tier::Sf7);
    assert!(!opcodes(&r).contains(&cmd::SET_MODULATION_PARAMS));
    assert!(!opcodes(&r).contains(&cmd::SET_PACKET_PARAMS));
    assert!(opcodes(&r).contains(&cmd::SET_RX));
}

#[test]
fn send_sets_fem_and_busy() {
    let mut r = ready();
    let frame = [0xD1, 0x10, 0x01, 0x00, 0x00];
    r.send(Tier::Sf12, &frame);
    assert!(r.busy(), "busy from SetTx");
    let ev = &r.bus().events;
    let fem_on = ev.iter().position(|e| *e == Ev::Fem(true)).expect("fem on");
    let set_tx = ev
        .iter()
        .position(|e| matches!(e, Ev::Xfer(t) if t[0] == cmd::SET_TX))
        .expect("SetTx");
    assert!(fem_on < set_tx, "PA mode before SetTx");
    assert_eq!(
        ev.get(set_tx),
        Some(&Ev::Xfer(vec![0x83, 0x00, 0x00, 0x00]))
    );
    // WriteBuffer at offset 0 carries the frame, and the packet length is 5.
    let mut wb = vec![0x0E, 0x00];
    wb.extend_from_slice(&frame);
    assert!(r.bus().xfers().contains(&wb));
    assert!(r
        .bus()
        .xfers()
        .contains(&vec![0x8C, 0x00, 0x08, 0x00, 0x05, 0x01, 0x00]));
    assert_eq!(r.bus().events.last(), Some(&Ev::Xfer(vec![0x83, 0, 0, 0])));

    // Not done yet: still busy, FEM still on, even after service.
    let mut buf = rx_buf();
    assert!(r.service(&mut buf).is_none());
    assert!(r.busy());
    assert_eq!(
        r.bus().events.iter().rev().find_map(|e| match e {
            Ev::Fem(v) => Some(*v),
            _ => None,
        }),
        Some(true)
    );

    // TxDone: FEM off, not busy, IRQ cleared.
    r.bus_mut().irq |= cmd::IRQ_TX_DONE;
    assert!(r.service(&mut buf).is_none());
    assert!(!r.busy());
    assert_eq!(r.bus().irq, 0);
    assert_eq!(
        r.bus().events.iter().rev().find_map(|e| match e {
            Ev::Fem(v) => Some(*v),
            _ => None,
        }),
        Some(false)
    );
}

#[test]
fn tx_guard_clears_busy_when_txdone_is_lost() {
    let mut r = ready();
    r.send(Tier::Sf12, &[0xD1; 10]);
    assert!(r.busy());
    let air = u64::from(Tier::Sf12.airtime_us(10));
    let now = r.bus().clock.get();
    // Just inside airtime + margin: still busy.
    r.bus().clock.set(now + air);
    let mut buf = rx_buf();
    r.service(&mut buf);
    assert!(r.busy());
    // Past it: forced to standby, FEM off.
    r.bus().clock.set(now + air + TX_MARGIN_US + 1_000);
    r.service(&mut buf);
    assert!(!r.busy());
    assert!(!r.failed());
    assert_eq!(r.bus().events.last(), Some(&Ev::Fem(false)));
}

#[test]
fn rx_frame_reports_snr_and_rssi() {
    let mut r = ready();
    r.listen(Tier::Sf7);
    let frame = [0xD1, 0x60, 1, 2, 3, 9, 8, 7];
    // RssiPkt 0x50 is -40 dBm; SnrPkt 0x1C is +7 dB, snr_x4 28.
    r.bus_mut().deliver(
        &frame,
        0x50,
        0x1C,
        cmd::IRQ_PREAMBLE_DETECTED | cmd::IRQ_HEADER_VALID | cmd::IRQ_RX_DONE,
    );
    let mut buf = rx_buf();
    let (n, info) = r.service(&mut buf).expect("frame");
    assert_eq!(&buf[..n], &frame);
    assert_eq!(info.rssi_dbm, -40);
    assert_eq!(info.snr_x4, 28);
    assert_eq!(info.tier, Tier::Sf7);
    assert!(!r.busy(), "back to listening");
    assert_eq!(r.bus().irq, 0);

    // Negative SNR: 0xF0 is -16 as i8, -4 dB.
    r.bus_mut().deliver(&frame, 0xC8, 0xF0, cmd::IRQ_RX_DONE);
    let (_, info) = r.service(&mut buf).expect("frame");
    assert_eq!(info.rssi_dbm, -100);
    assert_eq!(info.snr_x4, -16);
}

#[test]
fn rx_phases_make_the_radio_busy() {
    let mut r = ready();
    r.listen(Tier::Sf9);
    let mut buf = rx_buf();
    assert!(!r.busy());
    r.bus_mut().irq |= cmd::IRQ_PREAMBLE_DETECTED;
    assert!(r.service(&mut buf).is_none());
    assert!(r.busy(), "preamble seen");
    r.bus_mut().irq |= cmd::IRQ_HEADER_VALID;
    assert!(r.service(&mut buf).is_none());
    assert!(r.busy(), "header valid");
    r.bus_mut()
        .deliver(&[0xD1, 1, 2, 3, 4], 0x60, 0x10, cmd::IRQ_RX_DONE);
    assert!(r.service(&mut buf).is_some());
    assert!(!r.busy());
}

#[test]
fn false_preamble_times_out() {
    let mut r = ready();
    r.listen(Tier::Sf12);
    let mut buf = rx_buf();
    r.bus_mut().irq |= cmd::IRQ_PREAMBLE_DETECTED;
    assert!(r.service(&mut buf).is_none());
    assert!(r.busy());
    // Guard: (8 + 16) symbols of 4096 * 2 us.
    let guard = (8 + PREAMBLE_GUARD_EXTRA_SYMBOLS) * 4096 * 2;
    let t0 = r.bus().clock.get();
    r.bus().clock.set(t0 + guard - 5_000);
    assert!(r.service(&mut buf).is_none());
    assert!(r.busy(), "still inside the guard");

    r.bus_mut().events.clear();
    r.bus().clock.set(t0 + guard + 5_000);
    assert!(r.service(&mut buf).is_none());
    assert!(!r.busy(), "false preamble dropped");
    assert!(!r.failed());
    // Re-armed: standby, clear IRQs, continuous RX.
    let ops = opcodes(&r);
    let tail: Vec<u8> = ops.iter().rev().take(3).rev().copied().collect();
    assert_eq!(tail, [cmd::SET_STANDBY, cmd::CLEAR_IRQ_STATUS, cmd::SET_RX]);
    assert_eq!(r.bus().xfers().last(), Some(&vec![0x82, 0xFF, 0xFF, 0xFF]));
}

#[test]
fn crc_error_frame_dropped() {
    let mut r = ready();
    r.listen(Tier::Sf8);
    let mut buf = rx_buf();
    r.bus_mut().deliver(
        &[0xD1, 9, 9, 9, 9],
        0x50,
        0x10,
        cmd::IRQ_PREAMBLE_DETECTED | cmd::IRQ_HEADER_VALID | cmd::IRQ_RX_DONE | cmd::IRQ_CRC_ERR,
    );
    r.bus_mut().events.clear();
    assert!(
        r.service(&mut buf).is_none(),
        "CRC error frame is not delivered"
    );
    assert!(
        !opcodes(&r).contains(&cmd::READ_BUFFER),
        "payload never read"
    );
    assert!(opcodes(&r).contains(&cmd::SET_RX), "receive restarted");
    assert!(!r.busy());
    assert!(!r.failed());
    // The next good frame still comes through.
    let good = [0xD1, 0x70, 5, 6, 7];
    r.bus_mut().deliver(&good, 0x50, 0x10, cmd::IRQ_RX_DONE);
    let (n, _) = r.service(&mut buf).expect("good frame");
    assert_eq!(&buf[..n], &good);

    // A header error is dropped the same way.
    r.bus_mut()
        .deliver(&good, 0x50, 0x10, cmd::IRQ_HEADER_ERR | cmd::IRQ_RX_DONE);
    assert!(r.service(&mut buf).is_none());
    assert!(!r.busy());
}

#[test]
fn busy_stuck_reports_fail() {
    let mut r = ready();
    r.bus().busy_until.set(u64::MAX);
    r.bus_mut().step = 100;
    r.listen(Tier::Sf7);
    assert!(r.failed());
    assert!(!r.busy());
    assert_eq!(r.bus().events.last(), Some(&Ev::Fem(false)));
    // The wait stopped at the limit, not later.
    let waited = r.bus().clock.get();
    assert!(waited <= BUSY_LIMIT_US + 1_000, "waited {waited} us");

    // Once failed, send and service touch nothing.
    r.bus().busy_until.set(0);
    r.bus_mut().events.clear();
    r.send(Tier::Sf12, &[0xD1, 1, 2, 3, 4]);
    let mut buf = rx_buf();
    assert!(r.service(&mut buf).is_none());
    assert!(r.bus().xfers().is_empty());
    assert!(r.failed());

    // Init with BUSY stuck fails with BusyTimeout after the init limit.
    let mut r = radio();
    r.bus().busy_until.set(u64::MAX);
    r.bus_mut().step = 100;
    assert_eq!(r.init(2), Err(Error::BusyTimeout));
    assert!(r.failed());
    assert!(r.bus().clock.get() <= INIT_BUSY_LIMIT_US + 1_000);
}

#[test]
fn init_uses_50ms_limit_runtime_10ms() {
    // A 30 ms BUSY during init (TCXO start plus calibration) is fine.
    let mut r = radio();
    r.bus_mut().step = 100;
    r.bus().busy_until.set(30_000);
    assert_eq!(r.init(2), Ok(()));
    assert!(!r.failed());
    // A 60 ms BUSY during init is not.
    let mut r = radio();
    r.bus_mut().step = 100;
    r.bus().busy_until.set(60_000);
    assert_eq!(r.init(2), Err(Error::BusyTimeout));

    // At runtime a 5 ms BUSY is fine, a 30 ms BUSY fails.
    let mut r = radio();
    r.bus_mut().step = 100;
    r.init(2).expect("init");
    let now = r.bus().clock.get();
    r.bus().busy_until.set(now + 5_000);
    r.listen(Tier::Sf7);
    assert!(!r.failed(), "5 ms BUSY is within the runtime limit");
    let now = r.bus().clock.get();
    r.bus().busy_until.set(now + 30_000);
    r.listen(Tier::Sf8);
    assert!(r.failed(), "30 ms BUSY is over the runtime limit");
}

#[test]
fn power_clamped() {
    assert_eq!((POWER_MIN_DBM, POWER_MAX_DBM), (-9, 22));
    // Through init.
    let mut r = radio();
    r.init(30).expect("init");
    assert!(r.bus().xfers().contains(&vec![0x8E, 22, 0x02]));
    let mut r = radio();
    r.init(-20).expect("init");
    assert!(r.bus().xfers().contains(&vec![0x8E, 0xF7, 0x02]));
    // Through set_power while idle: applied at once.
    let mut r = ready();
    r.set_power(30).expect("set");
    assert_eq!(r.bus().xfers(), [vec![0x8E, 22, 0x02]]);
    r.bus_mut().events.clear();
    r.set_power(-20).expect("set");
    assert_eq!(r.bus().xfers(), [vec![0x8E, 0xF7, 0x02]]);
    r.bus_mut().events.clear();
    r.set_power(10).expect("set");
    assert_eq!(r.bus().xfers(), [vec![0x8E, 10, 0x02]]);
}

#[test]
fn power_set_while_busy_applies_at_next_send() {
    let mut r = ready();
    r.listen(Tier::Sf7);
    r.bus_mut().events.clear();
    // Listening is not Idle: nothing is written now.
    r.set_power(15).expect("set");
    assert!(r.bus().xfers().is_empty());
    r.send(Tier::Sf7, &[0xD1, 1, 2, 3, 4]);
    let x = r.bus().xfers();
    let p = x
        .iter()
        .position(|t| t == &vec![0x8E, 15, 0x02])
        .expect("SetTxParams");
    let tx = x.iter().position(|t| t[0] == cmd::SET_TX).expect("SetTx");
    assert!(p < tx);
}

#[test]
fn device_errors_reads_opcode_0x17() {
    let mut r = ready();
    r.bus_mut().dev_errors = 0x0160;
    assert_eq!(r.device_errors(), Ok(0x0160));
    assert_eq!(r.bus().xfers(), [vec![0x17, 0x00, 0x00, 0x00]]);
}
