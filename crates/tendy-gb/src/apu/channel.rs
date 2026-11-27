//! The four sound channels' state and their per-step units.
//!
//! Every timer here counts in dots (T-cycles), the finest unit the APU sees. `Pulse::period` and
//! `Wave::period` hold the raw 11-bit period value from `NRx3`/`NRx4` (`Audio_Registers`: "the
//! pulse channels' period dividers are clocked at 1048576 Hz", the wave channel's at 2097152 Hz);
//! `Noise::period` mirrors the dot count derived from `NR43`.
//!
//! The one fidelity bound: `timer` is a `u16`, so a reload longer than 65 535 dots is clamped. That
//! only reaches noise clock shifts of 10 and up (below about 64 Hz) and the bottom octave of the
//! wave channel, where the pitch is at most one step flat.

use crate::state::{StateError, StateReader, StateWriter};

/// Duty waveforms, `NRx1` bits 7-6: 12.5%, 25%, 50%, 75% (`Audio_Registers`, `NR11`). A `1` is the
/// high half of the pulse, so the count of ones per eight sets the duty.
const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

/// Step a dot countdown by `dots`; returns how many times it wrapped.
fn count_down(timer: &mut u16, reload: u16, dots: u32) -> u32 {
    let reload = reload.max(1);
    if *timer == 0 {
        *timer = reload;
    }
    let mut remaining = dots;
    let mut wraps = 0;
    loop {
        let left = *timer as u32;
        if remaining < left {
            *timer = (left - remaining) as u16;
            return wraps;
        }
        remaining -= left;
        wraps += 1;
        *timer = reload;
        if remaining == 0 {
            return wraps;
        }
    }
}

/// The length timer, clocked at 256 Hz (`Audio`: "a channel's length timer ticks up at 256 Hz …
/// when it reaches 64 the channel is turned off"); stored inverted, counting down to 0.
fn length_tick(length: &mut u16, length_enable: bool, enabled: &mut bool) {
    if !length_enable || *length == 0 {
        return;
    }
    *length -= 1;
    if *length == 0 {
        *enabled = false;
    }
}

/// The volume envelope, clocked at 64 Hz (`Audio`: "every 1–7 of those ticks, the volume will be
/// increased or decreased"). A period of 0 disables it.
fn envelope_tick(vol: &mut u8, up: bool, period: u8, timer: &mut u8) {
    if period == 0 {
        return;
    }
    if *timer > 0 {
        *timer -= 1;
    }
    if *timer != 0 {
        return;
    }
    *timer = period;
    if up {
        if *vol < 15 {
            *vol += 1;
        }
    } else if *vol > 0 {
        *vol -= 1;
    }
}

/// Square channel 1 (with sweep) and channel 2.
#[derive(Clone, Debug, Default)]
pub struct Pulse {
    pub enabled: bool,
    pub dac: bool,
    pub duty: u8,
    pub duty_pos: u8,
    pub period: u16,
    pub timer: u16,
    pub length: u16,
    pub length_enable: bool,
    pub vol: u8,
    pub env_dir_up: bool,
    pub env_period: u8,
    pub env_timer: u8,
    pub sweep_period: u8,
    pub sweep_dir_down: bool,
    pub sweep_shift: u8,
    pub sweep_timer: u8,
    pub sweep_enabled: bool,
    pub sweep_shadow: u16,
}

impl Pulse {
    /// Dots per duty step: the divider is clocked once per four dots and reloads from
    /// `2048 - period` (`Audio_Registers`, `NR13`), so one step is `4 * (2048 - period)` dots.
    fn reload_dots(&self) -> u16 {
        4 * (0x800 - (self.period & 0x7FF))
    }

    /// Advance the duty divider by `dots`.
    pub fn step(&mut self, dots: u32) {
        let reload = self.reload_dots();
        let wraps = count_down(&mut self.timer, reload, dots);
        self.duty_pos = ((self.duty_pos as u32 + wraps) & 7) as u8;
    }

    /// Digital output, `$0` or the current volume (`Audio_details`, "Pulse channels").
    pub fn level(&self) -> u8 {
        if !self.enabled || !self.dac {
            return 0;
        }
        if DUTY[(self.duty & 3) as usize][(self.duty_pos & 7) as usize] == 1 {
            self.vol & 0x0F
        } else {
            0
        }
    }

    pub fn length_clock(&mut self) {
        length_tick(&mut self.length, self.length_enable, &mut self.enabled);
    }

    pub fn envelope_clock(&mut self) {
        envelope_tick(
            &mut self.vol,
            self.env_dir_up,
            self.env_period,
            &mut self.env_timer,
        );
    }

    /// The period the sweep produces from `shadow` (`Audio_details`, "Frequency calculation"):
    /// `shadow ± shadow / 2^step`.
    fn sweep_calc(&self, shadow: u16) -> u16 {
        let delta = shadow >> (self.sweep_shift & 7);
        if self.sweep_dir_down {
            shadow.wrapping_sub(delta)
        } else {
            shadow.wrapping_add(delta)
        }
    }

    /// Clocked at 128 Hz by the frame sequencer (`Audio_details`, "Pulse channel with sweep").
    pub fn sweep_clock(&mut self) {
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer != 0 {
            return;
        }
        self.sweep_timer = if self.sweep_period == 0 {
            8
        } else {
            self.sweep_period
        };
        if !self.sweep_enabled || self.sweep_period == 0 {
            return;
        }
        let next = self.sweep_calc(self.sweep_shadow);
        if next > 0x7FF {
            self.enabled = false;
            return;
        }
        if self.sweep_shift == 0 {
            return;
        }
        self.sweep_shadow = next;
        self.period = next;
        if self.sweep_calc(next) > 0x7FF {
            self.enabled = false;
        }
    }

    /// `NRx4` bit 7. `sweep` is true for CH1 only.
    pub fn trigger(&mut self, initial_vol: u8, sweep: bool) {
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = self.reload_dots();
        self.env_timer = self.env_period;
        self.vol = initial_vol;
        self.enabled = self.dac;
        if sweep {
            self.sweep_shadow = self.period;
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            if self.sweep_shift != 0 && self.sweep_calc(self.period) > 0x7FF {
                self.enabled = false;
            }
        }
    }

    /// The state row, field order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dac);
        w.u8(self.duty);
        w.u8(self.duty_pos);
        w.u16(self.period);
        w.u16(self.timer);
        w.u16(self.length);
        w.bool(self.length_enable);
        w.u8(self.vol);
        w.bool(self.env_dir_up);
        w.u8(self.env_period);
        w.u8(self.env_timer);
        w.u8(self.sweep_period);
        w.bool(self.sweep_dir_down);
        w.u8(self.sweep_shift);
        w.u8(self.sweep_timer);
        w.bool(self.sweep_enabled);
        w.u16(self.sweep_shadow);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dac = r.bool()?;
        self.duty = r.u8()?;
        self.duty_pos = r.u8()?;
        self.period = r.u16()?;
        self.timer = r.u16()?;
        self.length = r.u16()?;
        self.length_enable = r.bool()?;
        self.vol = r.u8()?;
        self.env_dir_up = r.bool()?;
        self.env_period = r.u8()?;
        self.env_timer = r.u8()?;
        self.sweep_period = r.u8()?;
        self.sweep_dir_down = r.bool()?;
        self.sweep_shift = r.u8()?;
        self.sweep_timer = r.u8()?;
        self.sweep_enabled = r.bool()?;
        self.sweep_shadow = r.u16()?;
        Ok(())
    }
}

/// Wave channel 3.
#[derive(Clone, Debug, Default)]
pub struct Wave {
    pub enabled: bool,
    pub dac: bool,
    pub period: u16,
    pub timer: u16,
    pub length: u16,
    pub length_enable: bool,
    /// 0..3: mute, 100%, 50%, 25%.
    pub volume_code: u8,
    /// 0..31 sample position.
    pub pos: u8,
}

impl Wave {
    /// Dots per sample: the divider is clocked once per two dots and reloads from
    /// `2048 - period` (`Audio_Registers`, `NR33`), so one sample is `2 * (2048 - period)` dots.
    fn reload_dots(&self) -> u16 {
        2 * (0x800 - (self.period & 0x7FF))
    }

    /// Advance the sample divider by `dots`.
    pub fn step(&mut self, dots: u32) {
        let reload = self.reload_dots();
        let wraps = count_down(&mut self.timer, reload, dots);
        self.pos = ((self.pos as u32 + wraps) & 31) as u8;
    }

    /// Digital output: the nibble at `pos`, shifted right by the output level
    /// (`Audio_Registers`, `NR32`: 50% is a right shift by one, 25% by two).
    pub fn level(&self, wave: &[u8; 16]) -> u8 {
        if !self.enabled || !self.dac || self.volume_code == 0 {
            return 0;
        }
        let byte = wave[(self.pos & 31) as usize / 2];
        let nibble = if self.pos & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        };
        nibble >> (self.volume_code - 1)
    }

    /// The middle of the shifted output range, so a flat waveform mixes to zero.
    pub fn center(&self) -> f32 {
        if self.volume_code == 0 {
            0.0
        } else {
            (15 >> (self.volume_code - 1)) as f32 / 2.0
        }
    }

    pub fn length_clock(&mut self) {
        length_tick(&mut self.length, self.length_enable, &mut self.enabled);
    }

    /// `NRx4` bit 7 (`Audio_Registers`, `NR34`): the sample index restarts, the divider reloads.
    pub fn trigger(&mut self) {
        if self.length == 0 {
            self.length = 256;
        }
        self.timer = self.reload_dots();
        self.pos = 0;
        self.enabled = self.dac;
    }

    /// The state row, field order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dac);
        w.u16(self.period);
        w.u16(self.timer);
        w.u16(self.length);
        w.bool(self.length_enable);
        w.u8(self.volume_code);
        w.u8(self.pos);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dac = r.bool()?;
        self.period = r.u16()?;
        self.timer = r.u16()?;
        self.length = r.u16()?;
        self.length_enable = r.bool()?;
        self.volume_code = r.u8()?;
        self.pos = r.u8()?;
        Ok(())
    }
}

/// Noise channel 4.
#[derive(Clone, Debug, Default)]
pub struct Noise {
    pub enabled: bool,
    pub dac: bool,
    /// Dots per LFSR clock, derived from `NR43`; mirrors `reload_dots`.
    pub period: u16,
    pub timer: u16,
    pub length: u16,
    pub length_enable: bool,
    pub vol: u8,
    pub env_dir_up: bool,
    pub env_period: u8,
    pub env_timer: u8,
    pub clock_shift: u8,
    pub width_mode: bool,
    pub divisor_code: u8,
    pub lfsr: u16,
}

impl Noise {
    /// Dots per LFSR clock: the clock runs at `262144 / (divider * 2^shift)` Hz, and a divider
    /// code of 0 means 0.5 (`Audio_Registers`, `NR43`). Shifts of 14 and 15 stop the clock.
    fn reload_dots(&self) -> u16 {
        if self.clock_shift >= 14 {
            return u16::MAX;
        }
        let base: u32 = if self.divisor_code == 0 {
            8
        } else {
            16 * self.divisor_code as u32
        };
        (base << self.clock_shift).min(u16::MAX as u32) as u16
    }

    /// Take `NR43`: clock shift, LFSR width and clock divider (`Audio_Registers`, `NR43`).
    pub fn set_divider(&mut self, value: u8) {
        self.clock_shift = value >> 4;
        self.width_mode = value & 0x08 != 0;
        self.divisor_code = value & 0x07;
        self.period = self.reload_dots();
    }

    /// Advance the LFSR clock by `dots`.
    pub fn step(&mut self, dots: u32) {
        if self.clock_shift >= 14 {
            return;
        }
        let reload = self.reload_dots();
        let wraps = count_down(&mut self.timer, reload, dots);
        for _ in 0..wraps {
            self.clock_lfsr();
        }
    }

    /// One LFSR clock (`Audio_details`, "Noise channel"): XOR bits 0 and 1 into bit 15, copy that
    /// bit into bit 7 in 7-bit mode, shift right; bit 0 picks between 0 and the volume.
    fn clock_lfsr(&mut self) {
        let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
        self.lfsr = (self.lfsr & 0x7FFF) | (bit << 15);
        if self.width_mode {
            self.lfsr = (self.lfsr & !0x80) | (bit << 7);
        }
        self.lfsr >>= 1;
    }

    pub fn level(&self) -> u8 {
        if !self.enabled || !self.dac {
            return 0;
        }
        if self.lfsr & 1 != 0 {
            self.vol & 0x0F
        } else {
            0
        }
    }

    pub fn length_clock(&mut self) {
        length_tick(&mut self.length, self.length_enable, &mut self.enabled);
    }

    pub fn envelope_clock(&mut self) {
        envelope_tick(
            &mut self.vol,
            self.env_dir_up,
            self.env_period,
            &mut self.env_timer,
        );
    }

    /// `NRx4` bit 7 (`Audio_Registers`, `NR44`): length reset, envelope reset, LFSR reset.
    pub fn trigger(&mut self, initial_vol: u8) {
        if self.length == 0 {
            self.length = 64;
        }
        self.period = self.reload_dots();
        self.timer = self.period;
        self.env_timer = self.env_period;
        self.vol = initial_vol;
        self.lfsr = 0x7FFF;
        self.enabled = self.dac;
    }

    /// The state row, field order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dac);
        w.u16(self.period);
        w.u16(self.timer);
        w.u16(self.length);
        w.bool(self.length_enable);
        w.u8(self.vol);
        w.bool(self.env_dir_up);
        w.u8(self.env_period);
        w.u8(self.env_timer);
        w.u8(self.clock_shift);
        w.bool(self.width_mode);
        w.u8(self.divisor_code);
        w.u16(self.lfsr);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dac = r.bool()?;
        self.period = r.u16()?;
        self.timer = r.u16()?;
        self.length = r.u16()?;
        self.length_enable = r.bool()?;
        self.vol = r.u8()?;
        self.env_dir_up = r.bool()?;
        self.env_period = r.u8()?;
        self.env_timer = r.u8()?;
        self.clock_shift = r.u8()?;
        self.width_mode = r.bool()?;
        self.divisor_code = r.u8()?;
        self.lfsr = r.u16()?;
        Ok(())
    }
}
