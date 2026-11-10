//! Speed tier ladder. All tiers are LoRa at BW 500 kHz, CR 4/5.
//!
//! At a fixed 500 kHz bandwidth the measured SNR does not depend on SF, so
//! the core measures once on SF12 and picks the fastest tier from the floors.

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Tier {
    Sf12,
    Sf11,
    Sf10,
    Sf9,
    Sf8,
    Sf7,
    Sf6,
    Sf5,
}

impl Tier {
    /// Slowest to fastest.
    pub const LADDER: [Tier; 8] = [
        Tier::Sf12,
        Tier::Sf11,
        Tier::Sf10,
        Tier::Sf9,
        Tier::Sf8,
        Tier::Sf7,
        Tier::Sf6,
        Tier::Sf5,
    ];

    pub const ROBUST: Tier = Tier::Sf12;

    pub fn sf(self) -> u8 {
        match self {
            Tier::Sf12 => 12,
            Tier::Sf11 => 11,
            Tier::Sf10 => 10,
            Tier::Sf9 => 9,
            Tier::Sf8 => 8,
            Tier::Sf7 => 7,
            Tier::Sf6 => 6,
            Tier::Sf5 => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Sf12 => "LORA SF12 BW500",
            Tier::Sf11 => "LORA SF11 BW500",
            Tier::Sf10 => "LORA SF10 BW500",
            Tier::Sf9 => "LORA SF9 BW500",
            Tier::Sf8 => "LORA SF8 BW500",
            Tier::Sf7 => "LORA SF7 BW500",
            Tier::Sf6 => "LORA SF6 BW500",
            Tier::Sf5 => "LORA SF5 BW500",
        }
    }

    /// Air rate in bits per second.
    pub fn air_bps(self) -> u32 {
        match self {
            Tier::Sf12 => 1172,
            Tier::Sf11 => 2148,
            Tier::Sf10 => 3906,
            Tier::Sf9 => 7031,
            Tier::Sf8 => 12500,
            Tier::Sf7 => 21875,
            Tier::Sf6 => 37500,
            Tier::Sf5 => 62500,
        }
    }

    /// Demodulation SNR floor in dB times 4 (SX1262 datasheet limits).
    pub fn snr_floor_x4(self) -> i16 {
        match self {
            Tier::Sf5 => -10,  // -2.5 dB
            Tier::Sf6 => -20,  // -5 dB
            Tier::Sf7 => -30,  // -7.5 dB
            Tier::Sf8 => -40,  // -10 dB
            Tier::Sf9 => -50,  // -12.5 dB
            Tier::Sf10 => -60, // -15 dB
            Tier::Sf11 => -70, // -17.5 dB
            Tier::Sf12 => -80, // -20 dB
        }
    }

    /// Programmed preamble length in symbols: 12 at SF5 and SF6, else 8.
    pub fn preamble_symbols(self) -> u16 {
        if self.sf() <= 6 {
            12
        } else {
            8
        }
    }

    /// Time on air in microseconds for a PHY payload of `payload_len` bytes.
    ///
    /// Formula used: SX1261/2 datasheet "LoRa time-on-air" form, BW 500 kHz,
    /// CR 4/5, explicit header, CRC on, no low-data-rate optimize.
    /// SF7..SF12: preamble = (8 + 4.25) symbols, payload numerator
    /// 8*PL + 16 - 4*SF + 28. SF5/SF6: preamble = (12 + 6.25) symbols,
    /// payload numerator 8*PL + 16 - 4*SF + 20.
    /// Integer math, saturating, never panics.
    pub fn airtime_us(self, payload_len: usize) -> u32 {
        let sf = self.sf() as i64;
        let small = sf <= 6;
        // Symbol time in microseconds: 2^SF / 500 kHz = 2^SF * 2 us.
        let tsym_us = (1i64 << sf) * 2;
        // Preamble length in quarter symbols.
        let pre_q: i64 = 4 * i64::from(self.preamble_symbols()) + if small { 25 } else { 17 };
        let pl = payload_len.min(1 << 20) as i64;
        let k = if small { 20 } else { 28 };
        let num = 8 * pl - 4 * sf + k + 16;
        let den = 4 * sf;
        let blocks = if num > 0 { (num + den - 1) / den } else { 0 };
        let payload_syms = 8 + blocks * 5;
        let total_us = tsym_us * pre_q / 4 + payload_syms * tsym_us;
        u32::try_from(total_us).unwrap_or(u32::MAX)
    }

    pub fn slower(self) -> Option<Tier> {
        Tier::from_index(self.index().wrapping_sub(1))
    }

    pub fn faster(self) -> Option<Tier> {
        Tier::from_index(self.index().wrapping_add(1))
    }

    /// 0 = Sf12 (slowest) .. 7 = Sf5 (fastest).
    pub fn index(self) -> u8 {
        match self {
            Tier::Sf12 => 0,
            Tier::Sf11 => 1,
            Tier::Sf10 => 2,
            Tier::Sf9 => 3,
            Tier::Sf8 => 4,
            Tier::Sf7 => 5,
            Tier::Sf6 => 6,
            Tier::Sf5 => 7,
        }
    }

    pub fn from_index(i: u8) -> Option<Tier> {
        Tier::LADDER.get(i as usize).copied()
    }
}

/// Fastest tier whose floor plus margin fits under the measured SNR, else Sf12.
pub fn best_tier(snr_x4: i16, margin_x4: i16) -> Tier {
    let mut best = Tier::Sf12;
    for t in Tier::LADDER {
        if t.snr_floor_x4().saturating_add(margin_x4) <= snr_x4 {
            best = t;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_rates() {
        let want: [(Tier, &str, u32); 8] = [
            (Tier::Sf12, "LORA SF12 BW500", 1172),
            (Tier::Sf11, "LORA SF11 BW500", 2148),
            (Tier::Sf10, "LORA SF10 BW500", 3906),
            (Tier::Sf9, "LORA SF9 BW500", 7031),
            (Tier::Sf8, "LORA SF8 BW500", 12500),
            (Tier::Sf7, "LORA SF7 BW500", 21875),
            (Tier::Sf6, "LORA SF6 BW500", 37500),
            (Tier::Sf5, "LORA SF5 BW500", 62500),
        ];
        for (t, name, bps) in want {
            assert_eq!(t.name(), name);
            assert_eq!(t.air_bps(), bps);
        }
        // LADDER is slowest to fastest, the same order as the table.
        let ladder: Vec<Tier> = want.iter().map(|w| w.0).collect();
        assert_eq!(Tier::LADDER.to_vec(), ladder);
        for w in Tier::LADDER.windows(2) {
            assert!(w[0].air_bps() < w[1].air_bps());
        }
        for t in Tier::LADDER {
            assert_eq!(Tier::from_index(t.index()), Some(t));
        }
        assert_eq!(Tier::from_index(8), None);
        assert_eq!(Tier::from_index(255), None);
        assert_eq!(Tier::Sf12.slower(), None);
        assert_eq!(Tier::Sf5.faster(), None);
        assert_eq!(Tier::Sf12.faster(), Some(Tier::Sf11));
        assert_eq!(Tier::Sf5.slower(), Some(Tier::Sf6));
    }

    /// Semtech LoRa time on air (AN1200.13 / SX1262 datasheet), in us.
    fn semtech_us(sf: u32, pl: u32) -> f64 {
        let bw = 500_000.0;
        let tsym = f64::from(1u32 << sf) / bw;
        let n_pre = 8.0;
        let pre_extra = if sf <= 6 { 6.25 } else { 4.25 };
        let pre = (n_pre + pre_extra) * tsym;
        // Explicit header (20 bits), CRC on (16 bits), CR 4/5, no low data
        // rate optimize; SF7 and up add the extra 8.
        let extra = if sf <= 6 { 0.0 } else { 8.0 };
        let num = 8.0 * f64::from(pl) + 16.0 - 4.0 * f64::from(sf) + 20.0 + extra;
        let den = 4.0 * f64::from(sf);
        let blocks = (num / den).ceil().max(0.0);
        let payload = 8.0 + blocks * 5.0;
        (pre + payload * tsym) * 1e6
    }

    #[test]
    fn airtime_matches_reference() {
        let reference = semtech_us(7, 255);
        let got = f64::from(Tier::Sf7.airtime_us(255));
        assert!(
            (got - reference).abs() <= reference * 0.02,
            "SF7 255 bytes: got {got} us, reference {reference} us"
        );
        for t in Tier::LADDER {
            let r = semtech_us(u32::from(t.sf()), 255);
            let g = f64::from(t.airtime_us(255));
            assert!(
                (g - r).abs() <= r * 0.02,
                "{t:?}: got {g} us, reference {r} us"
            );
        }
        // About 99.9 ms by hand: (12.25 + 8 + 74 * 5) symbols of 256 us.
        assert!((99_000.0..101_000.0).contains(&got));
        for pl in [1usize, 6, 40, 204, 255] {
            for w in Tier::LADDER.windows(2) {
                // LADDER is slowest first, so airtime must shrink along it.
                assert!(
                    w[0].airtime_us(pl) > w[1].airtime_us(pl),
                    "{:?} vs {:?} at {pl} bytes",
                    w[0],
                    w[1]
                );
            }
        }
        // Saturates instead of panicking on silly lengths.
        let _ = Tier::Sf12.airtime_us(usize::MAX);
    }

    #[test]
    fn best_tier_picks_fastest_with_margin() {
        assert_eq!(best_tier(40, 20), Tier::Sf5);
        assert_eq!(best_tier(-24, 20), Tier::Sf9);
        assert_eq!(best_tier(-48, 20), Tier::Sf11);
        assert_eq!(best_tier(-160, 20), Tier::Sf12);
        assert_eq!(best_tier(i16::MIN, i16::MAX), Tier::Sf12);
        assert_eq!(best_tier(i16::MAX, 0), Tier::Sf5);
    }
}
