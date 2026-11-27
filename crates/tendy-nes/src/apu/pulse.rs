use super::units::{Envelope, LengthCounter};

const DUTY: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

/// A pulse (square) channel. Pulse 1 negates its sweep with ones' complement, pulse 2 with two's complement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pulse {
    pub ones_complement: bool,
    pub duty: u8,
    pub duty_pos: u8,
    pub timer_period: u16,
    pub timer: u16,
    pub envelope: Envelope,
    pub length: LengthCounter,
    pub sweep_enabled: bool,
    pub sweep_period: u8,
    pub sweep_negate: bool,
    pub sweep_shift: u8,
    pub sweep_divider: u8,
    pub sweep_reload: bool,
}

impl Pulse {
    pub fn new(ones_complement: bool) -> Pulse {
        Pulse {
            ones_complement,
            ..Default::default()
        }
    }

    /// Register write; `reg` is 0..=3 ($4000-$4003 or $4004-$4007).
    pub fn write(&mut self, reg: u16, val: u8) {
        match reg & 3 {
            0 => {
                self.duty = val >> 6;
                self.length.halt = val & 0x20 != 0;
                self.envelope.write(val);
            }
            1 => {
                self.sweep_enabled = val & 0x80 != 0;
                self.sweep_period = (val >> 4) & 0x07;
                self.sweep_negate = val & 0x08 != 0;
                self.sweep_shift = val & 0x07;
                self.sweep_reload = true;
            }
            2 => self.timer_period = (self.timer_period & 0x0700) | u16::from(val),
            _ => {
                self.timer_period = (self.timer_period & 0x00FF) | (u16::from(val & 0x07) << 8);
                self.length.load(val >> 3);
                self.duty_pos = 0;
                self.envelope.start = true;
            }
        }
    }

    /// Clocked every APU cycle (every second CPU cycle).
    pub fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            self.duty_pos = (self.duty_pos + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }

    pub(super) fn sweep_target(&self) -> u16 {
        let change = self.timer_period >> self.sweep_shift;
        if self.sweep_negate {
            let sub = if self.ones_complement {
                change + 1
            } else {
                change
            };
            self.timer_period.saturating_sub(sub)
        } else {
            self.timer_period + change
        }
    }

    pub(super) fn muted(&self) -> bool {
        self.timer_period < 8 || self.sweep_target() > 0x7FF
    }

    /// Half-frame clock: length counter and sweep unit.
    pub fn clock_half(&mut self) {
        self.length.clock();
        if self.sweep_divider == 0 && self.sweep_enabled && self.sweep_shift > 0 && !self.muted() {
            self.timer_period = self.sweep_target();
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    /// Current output level 0..=15.
    pub fn output(&self) -> u8 {
        if !self.length.active()
            || self.muted()
            || DUTY[usize::from(self.duty)][usize::from(self.duty_pos)] == 0
        {
            0
        } else {
            self.envelope.output()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pulse with its length counter enabled, constant volume 15, length halted, the given duty and timer period.
    fn playing(ones_complement: bool, duty: u8, period: u16) -> Pulse {
        let mut p = Pulse::new(ones_complement);
        p.length.set_enabled(true);
        p.write(0, (duty << 6) | 0x3F);
        p.write(2, (period & 0xFF) as u8);
        p.write(3, 0x08 | (period >> 8) as u8);
        p
    }

    #[test]
    fn duty_2_sequence() {
        let mut p = playing(true, 2, 8);
        let mut outputs = Vec::new();

        for _ in 0..8 {
            outputs.push(p.output());
            for _ in 0..9 {
                p.clock_timer();
            }
        }

        assert_eq!(outputs, vec![0, 15, 15, 15, 15, 0, 0, 0]);
    }

    #[test]
    fn write_3_loads_length() {
        let p = playing(true, 2, 8);
        assert_eq!(p.length.value, 254);
    }

    #[test]
    fn period_below_8_is_muted() {
        let mut p = playing(true, 2, 7);

        for _ in 0..9 {
            p.clock_timer();
        }

        assert_eq!(p.output(), 0);
    }

    #[test]
    fn sweep_up_adds_shifted_period() {
        let mut p = playing(true, 2, 0x100);
        p.write(1, 0x81);
        p.clock_half();

        assert_eq!(p.timer_period, 0x180);
    }

    #[test]
    fn pulse1_sweep_negates_ones_complement() {
        let mut p = playing(true, 2, 0x100);
        p.write(1, 0x89);
        p.clock_half();

        assert_eq!(p.timer_period, 0x7F);
    }

    #[test]
    fn pulse2_sweep_negates_twos_complement() {
        let mut p = playing(false, 2, 0x100);
        p.write(1, 0x89);
        p.clock_half();

        assert_eq!(p.timer_period, 0x80);
    }

    #[test]
    fn sweep_overflow_mutes_without_changing_period() {
        let mut p = playing(false, 2, 0x7FF);
        p.write(1, 0x01);

        for _ in 0..9 {
            p.clock_timer();
        }

        assert_eq!(p.output(), 0);
        assert_eq!(p.timer_period, 0x7FF);
    }

    #[test]
    fn silent_when_length_is_zero() {
        let mut p = Pulse::new(true);
        p.write(0, 0xBF);
        p.write(2, 0x08);
        p.write(3, 0x08);

        for _ in 0..9 {
            p.clock_timer();
        }

        assert_eq!(p.output(), 0);
    }
}
