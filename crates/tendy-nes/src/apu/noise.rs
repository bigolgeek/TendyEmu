use super::units::{Envelope, LengthCounter};

/// Noise timer periods in CPU cycles (NTSC), indexed by the low 4 bits of $400E.
pub const NOISE_PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

/// PAL (2A07) noise timer periods: the same notes at 1.662607/1.789773 of the NTSC clock.
pub const NOISE_PERIODS_PAL: [u16; 16] = [
    4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778,
];

/// The period table a region's noise channel uses.
pub fn periods(pal: bool) -> &'static [u16; 16] {
    if pal {
        &NOISE_PERIODS_PAL
    } else {
        &NOISE_PERIODS
    }
}

/// The noise channel: a 15-bit linear-feedback shift register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Noise {
    pub envelope: Envelope,
    pub length: LengthCounter,
    /// Mode flag: feedback from bit 6 instead of bit 1 (short, metallic sequence).
    pub mode: bool,
    pub timer_period: u16,
    pub timer: u16,
    pub shift: u16,
    /// PAL: index `NOISE_PERIODS_PAL` instead of the NTSC table.
    pub pal: bool,
}

impl Default for Noise {
    fn default() -> Self {
        Noise {
            envelope: Envelope::default(),
            length: LengthCounter::default(),
            mode: false,
            timer_period: NOISE_PERIODS[0],
            timer: 0,
            shift: 1,
            pal: false,
        }
    }
}

impl Noise {
    /// Register write; `reg` is 0..=3 ($400C, $400D unused, $400E, $400F).
    pub fn write(&mut self, reg: u16, val: u8) {
        match reg & 3 {
            0 => {
                self.length.halt = val & 0x20 != 0;
                self.envelope.write(val);
            }
            1 => {}
            2 => {
                self.mode = val & 0x80 != 0;
                self.timer_period = periods(self.pal)[usize::from(val & 0x0F)];
            }
            _ => {
                self.length.load(val >> 3);
                self.envelope.start = true;
            }
        }
    }

    /// Clocked every CPU cycle (the period table is in CPU cycles).
    pub fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period - 1;
            let other = if self.mode { 6 } else { 1 };
            let feedback = (self.shift ^ (self.shift >> other)) & 1;
            self.shift = (self.shift >> 1) | (feedback << 14);
        } else {
            self.timer -= 1;
        }
    }

    pub fn output(&self) -> u8 {
        if !self.length.active() || self.shift & 1 != 0 {
            0
        } else {
            self.envelope.output()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_shift_1() {
        let n = Noise::default();
        assert_eq!(n.shift, 1);
        assert_eq!(n.timer_period, 4);
    }

    #[test]
    fn period_table() {
        let mut n = Noise::default();

        n.write(2, 0x0F);
        assert_eq!(n.timer_period, 4068);

        n.write(2, 0x03);
        assert_eq!(n.timer_period, 32);
    }

    #[test]
    fn long_mode_feeds_back_bit_1() {
        let mut n = Noise::default();

        n.write(2, 0x00);
        n.shift = 0x41;
        n.clock_timer();

        assert_eq!(n.shift, 0x4020);
    }

    #[test]
    fn short_mode_feeds_back_bit_6() {
        let mut n = Noise::default();

        n.write(2, 0x80);
        n.shift = 0x41;
        n.clock_timer();

        assert_eq!(n.shift, 0x20);
    }

    #[test]
    fn shifts_every_period() {
        let mut n = Noise::default();
        let mut observed = Vec::new();

        for _ in 0..5 {
            for _ in 0..4 {
                n.clock_timer();
            }
            observed.push(n.shift);
        }

        assert_eq!(observed, vec![0x4000, 0x2000, 0x1000, 0x800, 0x400]);
    }

    #[test]
    fn silent_while_bit_0_set() {
        let mut n = Noise::default();

        n.length.set_enabled(true);
        n.write(0, 0x3A);
        n.write(3, 0x08);

        assert_eq!(n.output(), 0);

        n.clock_timer();
        assert_eq!(n.output(), 10);
    }

    #[test]
    fn silent_without_length() {
        let mut n = Noise::default();

        n.write(0, 0x3A);
        n.write(3, 0x08);
        n.clock_timer();

        assert_eq!(n.output(), 0);
    }
}
