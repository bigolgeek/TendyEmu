//! The 2C33 wavetable sound channel: the Famicom Disk System's expansion audio.
//!
//! The unit is a 64-step wavetable driven by a 20-bit accumulator, with a 32-entry modulation table
//! stepping a 7-bit signed counter that bends the wave pitch. Register layout, unit tick rates and
//! the envelope clock all follow the NESdev wiki's *FDS audio* reference; the mixed output is
//! modelled the way FCEUX and puNES do it: the 6-bit DAC value scaled by the volume gain and master
//! volume, through the channel's own one-pole ~2 kHz low-pass.
//!
//! Registers live at `$4040-$4097`; the board (`crate::mapper::fds`) owns one of these and forwards
//! writes once `$4023` bit 1 has enabled the sound registers. The board then turns the channel's
//! mixer-scale level into pulse units with [`crate::apu::Apu::amplitude_to_pulse_units`], which is
//! the scale `Apu::mix` decodes the level back from: the round trip is exact, so the level the chip
//! asks for is the level added to the output.

/// Steps in the wavetable (`$4040-$407F`).
pub const WAVE_STEPS: usize = 64;
/// Entries in the modulation table (`$4088` ring buffer).
pub const MOD_STEPS: usize = 32;
/// CPU cycles between wave-unit and modulation-unit ticks (both units tick every 16 cycles).
pub const UNIT_TICK_CYCLES: u32 = 16;

/// A full-scale FDS sample is about 2.4x one APU square at volume 15, which the mixer's nonlinear
/// curve renders as 0.1494; 2.4 * 0.1494 = 0.36 is therefore the amplitude [`FdsSound::mix_level`]
/// reports, for the board to convert to pulse units.
pub const MIX_LEVEL: f32 = 0.36;

/// Cutoff of the channel's output filter, in Hz (NESdev: a one-pole low-pass at ~2000 Hz).
pub const LOWPASS_HZ: f32 = 2000.0;

/// Modulator input (0-7) to counter increment: `0 1 2 4` then `reset`, `-4 -2 -1`.
const BIAS: [i8; 8] = [0, 1, 2, 4, 0, -4, -2, -1];
/// Table entry meaning "reset the modulation counter to zero" (input 4).
const MOD_RESET: u8 = 0x10;

/// One volume or modulation envelope.
///
/// `$4080`/`$4084` bit 7 turns the envelope *off* and loads the gain directly from bits 0-5; with
/// bit 7 clear the gain walks one step per tick in the direction of bit 6.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FdsEnvelope {
    /// Bits 0-5 of `$4080`/`$4084`: the envelope step period.
    pub speed: u8,
    /// Current gain, 0-63 (clamped to 32 when applied).
    pub gain: u8,
    /// Envelope on: bit 7 of the control register is clear.
    pub enable: bool,
    /// Bit 6: increase (true) or decrease (false).
    pub up: bool,
    /// CPU cycles until the next tick.
    pub timer: u32,
}

impl FdsEnvelope {
    fn write(&mut self, val: u8) {
        self.speed = val & 0x3F;
        self.enable = val & 0x80 == 0;
        self.up = val & 0x40 != 0;
        if !self.enable {
            // Bit 7 set: the gain is written directly instead of being driven by the envelope.
            self.gain = val & 0x3F;
        }
    }

    /// One CPU cycle: tick the gain when the period expires. `$408A = 0` disables both envelopes.
    fn clock(&mut self, master_speed: u8) {
        if !self.enable || master_speed == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
            if self.timer == 0 {
                self.tick();
                self.timer = self.period(master_speed);
            }
        }
    }

    /// Cycles per envelope tick: `8 * (e + 1) * (m + 1)`.
    pub fn period(&self, master_speed: u8) -> u32 {
        8 * (u32::from(self.speed) + 1) * (u32::from(master_speed) + 1)
    }

    fn tick(&mut self) {
        if self.up {
            if self.gain < 32 {
                self.gain += 1;
            }
        } else if self.gain > 0 {
            self.gain -= 1;
        }
    }
}

/// The 2C33 wavetable channel.
#[derive(Clone, Debug)]
pub struct FdsSound {
    /// Wavetable RAM (`$4040-$407F`), 6-bit samples.
    pub wave: [u8; WAVE_STEPS],
    /// Modulation table, pre-translated to counter increments (`MOD_RESET` for input 4).
    pub mod_table: [u8; MOD_STEPS],

    /// `$4080`: volume envelope.
    pub volume: FdsEnvelope,
    /// `$4084`: modulation envelope.
    pub modulation: FdsEnvelope,
    /// `$4082` and `$4083` low 4 bits: the 12-bit wave pitch.
    pub freq: u16,
    /// `$4086` and `$4087` low 4 bits: the 12-bit modulation pitch.
    pub mod_freq: u16,

    /// `$4083` bit 7: wave unit halted (accumulator held at 0, output is `wave[0]`).
    pub wave_halt: bool,
    /// `$4083` bit 6: volume and modulation envelopes halted.
    pub env_halt: bool,
    /// `$4087` bit 7: modulation unit disabled and its accumulator reset.
    pub mod_disabled: bool,
    /// `$4089` bits 0-1: master volume (0 full, 1 2/3, 2 1/2, 3 2/5).
    pub master_volume: u8,
    /// `$4089` bit 7: wavetable writes enabled, output held at the current level.
    pub wave_write_enable: bool,
    /// `$408A`: master envelope speed. 0 disables both envelopes.
    pub env_speed: u8,

    /// 18-bit modulation accumulator: 5 table-address bits (13-17), ghost bit 12, 12 fraction bits.
    pub mod_acc: u32,
    /// 24-bit wave accumulator: position in bits 18-23, 20-bit pitch addition.
    pub wave_acc: u32,
    /// 7-bit signed modulation counter (`$4085`).
    pub mod_counter: i8,
    /// Cycles into the current 16-cycle unit tick.
    tick_cycles: u32,

    /// Effective volume latch: gain changes take effect when the wave position is 0.
    vol_latched: u8,
    /// Filtered output, 0.0..=1.0.
    lowpass: f32,
    lowpass_coeff: f32,
    /// Output held while `$4089` bit 7 is set.
    held: f32,
    /// The low-pass's input for this sample: DAC * gain * master volume, rebuilt only when one of
    /// those moved. The filter itself still runs every cycle.
    target: f32,
    /// `target` is stale: the wave or the modulation unit ticked, or a register was written.
    target_dirty: bool,
}

impl Default for FdsSound {
    fn default() -> Self {
        Self::new()
    }
}

impl FdsSound {
    pub fn new() -> Self {
        FdsSound {
            wave: [0; WAVE_STEPS],
            mod_table: [0; MOD_STEPS],
            volume: FdsEnvelope::default(),
            modulation: FdsEnvelope::default(),
            freq: 0,
            mod_freq: 0,
            wave_halt: false,
            env_halt: false,
            mod_disabled: false,
            master_volume: 0,
            wave_write_enable: false,
            env_speed: 0,
            mod_acc: 0,
            wave_acc: 0,
            mod_counter: 0,
            tick_cycles: 0,
            vol_latched: 0,
            lowpass: 0.0,
            // The 2C33 runs from the CPU clock; it only sets the low-pass coefficient.
            lowpass_coeff: 1.0
                - (-2.0 * std::f32::consts::PI * LOWPASS_HZ / crate::CPU_HZ as f32).exp(),
            held: 0.0,
            target: 0.0,
            target_dirty: true,
        }
    }

    /// Power-on / reset-button state, as `FDSInit` leaves it in FCEUX: every register clear.
    pub fn reset(&mut self) {
        let coeff = self.lowpass_coeff;
        *self = FdsSound {
            lowpass_coeff: coeff,
            ..FdsSound::new()
        };
    }

    /// CPU write to `$4080-$408A`. The caller has already checked `$4023` bit 1.
    pub fn write(&mut self, addr: u16, val: u8) {
        match addr & 0xFF {
            0x80 => {
                self.volume.write(val);
                self.volume.timer = self.volume.period(self.env_speed);
                if self.volume.gain == 0 {
                    // Muting through a direct gain write is immediate.
                    self.vol_latched = 0;
                }
            }
            0x82 => self.freq = (self.freq & 0x0F00) | u16::from(val),
            0x83 => {
                // Setting bit 7 halts the wave unit and resets its accumulator; clearing it
                // re-enables the unit from wave position 0.
                if val & 0x80 != 0 {
                    self.wave_acc = 0;
                }
                self.freq = (self.freq & 0x00FF) | (u16::from(val & 0x0F) << 8);
                self.wave_halt = val & 0x80 != 0;
                self.env_halt = val & 0x40 != 0;
                if self.env_halt {
                    self.volume.timer = self.volume.period(self.env_speed);
                    self.modulation.timer = self.modulation.period(self.env_speed);
                }
            }
            0x84 => {
                self.modulation.write(val);
                self.modulation.timer = self.modulation.period(self.env_speed);
            }
            0x85 => self.mod_counter = sign7(val & 0x7F),
            0x86 => self.mod_freq = (self.mod_freq & 0x0F00) | u16::from(val),
            0x87 => {
                self.mod_freq = (self.mod_freq & 0x00FF) | (u16::from(val & 0x0F) << 8);
                self.mod_disabled = val & 0x80 != 0;
                if self.mod_disabled {
                    // Force bits 0-12 clear; the table address in bits 13-17 stays.
                    self.mod_acc &= !0x1FFF;
                }
            }
            0x88 => {
                // Only the disabled modulator accepts table writes: replace the entry at the
                // current playback position, then step the position on by one entry.
                if self.mod_disabled {
                    let pos = ((self.mod_acc >> 13) & 0x1F) as usize;
                    let entry = val & 0x07;
                    self.mod_table[pos] = if entry == 4 {
                        MOD_RESET
                    } else {
                        BIAS[entry as usize] as u8
                    };
                    self.mod_acc = (self.mod_acc + 0x2000) & 0x0003_FFFF;
                }
            }
            0x89 => {
                self.master_volume = val & 0x03;
                self.wave_write_enable = val & 0x80 != 0;
            }
            0x8A => {
                self.env_speed = val;
                self.volume.timer = self.volume.period(val);
                self.modulation.timer = self.modulation.period(val);
            }
            _ => {}
        }
        // The cached low-pass input reads the wave position, the volume latch and the master
        // volume, and any of them can move on a write.
        self.target_dirty = true;
    }

    /// Wavetable RAM read (`$4040-$407F`). While writes are disabled the whole range reads back
    /// the sample at the current wave position, as the hardware does.
    pub fn read_wave(&self, addr: u16) -> u8 {
        if self.wave_write_enable {
            self.wave[usize::from(addr & 0x3F)]
        } else {
            self.wave[self.position()]
        }
    }

    /// Wavetable RAM write (`$4040-$407F`), gated by `$4089` bit 7.
    pub fn write_wave(&mut self, addr: u16, val: u8) {
        if self.wave_write_enable {
            self.wave[usize::from(addr & 0x3F)] = val & 0x3F;
            // `dac_value` reads the table.
            self.target_dirty = true;
        }
    }

    /// CPU read of `$4090-$4097`, the unit's monitor registers. Bits 6-7 read back as `01` where
    /// the hardware shows open bus.
    pub fn read(&self, addr: u16) -> u8 {
        match addr & 0xFF {
            0x90 => self.volume.gain | 0x40,
            0x91 => ((self.wave_acc >> 12) & 0xFF) as u8,
            0x92 => self.modulation.gain | 0x40,
            0x93 => ((self.mod_acc >> 5) & 0x7F) as u8,
            0x94 => {
                (((i32::from(self.mod_counter) * i32::from(self.modulation.gain.min(32))) >> 4)
                    & 0xFF) as u8
            }
            0x95 => {
                let pos = ((self.mod_acc >> 13) & 0x1F) as usize;
                match self.mod_table[pos] {
                    MOD_RESET => 0x04,
                    v => v & 0x0F,
                }
            }
            0x96 => (self.dac_value() & 0x3F) | 0x40,
            0x97 => self.mod_counter as u8,
            _ => 0x40,
        }
    }

    /// Current wave position (bits 18-23 of the wave accumulator).
    pub fn position(&self) -> usize {
        ((self.wave_acc >> 18) & 0x3F) as usize
    }

    /// The 6-bit DAC value before the volume gain: the wavetable sample at the current position.
    pub fn dac_value(&self) -> u8 {
        if self.wave_halt {
            self.wave[0]
        } else {
            self.wave[self.position()]
        }
    }

    /// Master volume as a multiplier: `$4089` bits 0-1 select 1, 2/3, 1/2 or 2/5.
    pub fn master_gain(&self) -> f32 {
        match self.master_volume {
            0 => 1.0,
            1 => 2.0 / 3.0,
            2 => 0.5,
            _ => 0.4,
        }
    }

    /// The wave pitch for this tick: `(pitch * temp) & 0xFFFFF`, where `temp` folds in the
    /// modulation counter and gain exactly as the wiki's C excerpt does.
    pub fn wave_pitch(&self) -> u32 {
        let gain = i32::from(self.modulation.gain.min(32));
        let mut temp = i32::from(self.mod_counter) * gain;
        if temp & 0x0F != 0 && temp & 0x800 == 0 {
            temp += 0x20;
        }
        temp += 0x400;
        let temp = ((temp >> 4) & 0xFF) as u32;
        (u32::from(self.freq) * temp) & 0xFFFFF
    }

    /// One CPU cycle: the 16-cycle unit tick (wave and modulation), both envelopes, and the output
    /// low-pass. Called once per CPU cycle by the board.
    pub fn clock(&mut self) {
        self.tick_cycles += 1;
        if self.tick_cycles >= UNIT_TICK_CYCLES {
            self.tick_cycles = 0;
            self.clock_units();
        }
        if !self.env_halt {
            self.volume.clock(self.env_speed);
            self.modulation.clock(self.env_speed);
        }
        self.update_output();
    }

    /// The 16-cycle tick shared by the wave and modulation units.
    fn clock_units(&mut self) {
        // The wave position, the volume latch and the modulation counter all move on this tick.
        self.target_dirty = true;
        if !self.mod_disabled && self.mod_freq != 0 {
            let before = self.mod_acc;
            self.mod_acc = before.wrapping_add(u32::from(self.mod_freq)) & 0x0003_FFFF;
            // A carry out of the 12 fraction bits steps the counter. The ghost bit (12) makes each
            // of the 32 table entries apply twice.
            if self.mod_acc & 0x0FFF < before & 0x0FFF {
                let pos = ((self.mod_acc >> 13) & 0x1F) as usize;
                if self.mod_table[pos] == MOD_RESET {
                    self.mod_counter = 0;
                } else {
                    self.mod_counter =
                        wrap7(i32::from(self.mod_counter) + i32::from(self.mod_table[pos] as i8));
                }
            }
        }
        if self.wave_halt {
            self.vol_latched = self.volume.gain;
        } else {
            self.wave_acc = self.wave_acc.wrapping_add(self.wave_pitch()) & 0x00FF_FFFF;
            if self.position() == 0 {
                // The volume gain is latched by the wave position reaching 0 (its PWM comparator).
                self.vol_latched = self.volume.gain;
            }
        }
    }

    /// DAC * gain * master volume, through the channel's one-pole 2 kHz low-pass.
    fn update_output(&mut self) {
        // the product below only moves on a unit tick or a register write — two divides and a
        // `match` per CPU cycle, to rebuild a number that changes every 16 cycles at most. The
        // one-pole filter after it genuinely needs every cycle, and still gets it.
        if self.target_dirty {
            let gain = self.vol_latched.min(32);
            self.target =
                f32::from(self.dac_value()) / 63.0 * (f32::from(gain) / 32.0) * self.master_gain();
            self.target_dirty = false;
        }
        self.lowpass += (self.target - self.lowpass) * self.lowpass_coeff;
        if self.wave_write_enable {
            // `$4089` bit 7 holds the output at its current level.
            return;
        }
        self.held = self.lowpass;
    }

    /// The channel output in `0.0..=1.0`, already low-passed.
    pub fn output(&self) -> f32 {
        self.held
    }

    /// A save state carries no `target` (it is the cached low-pass input), so the board calls
    /// this after restoring one: the next cycle rebuilds it before the filter runs.
    pub fn state_loaded(&mut self) {
        self.target_dirty = true;
    }

    /// The channel output as a mixer-scale amplitude; the board converts it to pulse units.
    pub fn mix_level(&self) -> f32 {
        self.output() * MIX_LEVEL
    }
}

/// Sign-extend a 7-bit two's-complement value.
fn sign7(v: u8) -> i8 {
    ((i16::from(v) << 9) >> 9) as i8
}

/// Wrap a modulation counter addition into the 7-bit signed range (`63 + 1 = -64`).
fn wrap7(v: i32) -> i8 {
    (((v + 64).rem_euclid(128)) - 64) as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sound unit playing a flat full-scale waveform at pitch `$400`, volume gain 32 and full
    /// master volume: its filtered output settles at 1.0.
    fn unit() -> FdsSound {
        let mut s = FdsSound::new();
        s.write(0x4089, 0x80); // write enable
        s.wave.fill(63);
        s.write(0x4089, 0x00); // master volume full, writes off
        s.write(0x4082, 0x00);
        s.write(0x4083, 0x04); // pitch 0x400, wave unit running
        s.write(0x4080, 0xC0 | 0x20); // envelope off, gain 32
        s.write(0x408A, 0xE8); // the BIOS value
        s
    }

    #[test]
    fn envelope_register_writes_gain_when_bit_seven_is_set() {
        let mut s = FdsSound::new();
        s.write(0x4080, 0x80 | 0x21);
        assert_eq!(s.volume.gain, 0x21);
        assert!(!s.volume.enable, "bit 7 turns the envelope off");
        assert_eq!(s.volume.speed, 0x21);
    }

    #[test]
    fn disabled_envelope_does_not_move_the_gain() {
        let mut s = FdsSound::new();
        s.write(0x4080, 0x80 | 0x10); // gain 16, envelope off
        for _ in 0..10_000 {
            s.clock();
        }
        assert_eq!(s.volume.gain, 16);
    }

    #[test]
    fn envelope_ticks_once_per_documented_period() {
        let mut s = FdsSound::new();
        s.write(0x408A, 0x01); // master speed 1
        s.write(0x4080, 0x80 | 0x10); // gain 16, envelope off
        s.write(0x4080, 0x00); // envelope on, decreasing, speed 0 -> 8*1*2 = 16 cycles
        let period = s.volume.period(1);
        assert_eq!(period, 16);
        s.volume.timer = period;
        for _ in 0..period {
            s.clock();
        }
        assert_eq!(s.volume.gain, 15, "one tick after exactly one period");
        for _ in 0..period {
            s.clock();
        }
        assert_eq!(s.volume.gain, 14);
    }

    #[test]
    fn master_volume_scales_the_output() {
        let mut full = unit();
        let mut quiet = unit();
        quiet.write(0x4089, 0x03); // 2/5
        for _ in 0..20_000 {
            full.clock();
            quiet.clock();
        }
        assert!(
            (full.output() - 1.0).abs() < 0.02,
            "full = {}",
            full.output()
        );
        assert!(
            (quiet.output() - 0.4).abs() < 0.02,
            "quiet = {}",
            quiet.output()
        );
    }

    #[test]
    fn wave_position_advances_at_the_documented_rate() {
        // Pitch $400: NESdev's f = n*p/16/2^12 gives a 27965 Hz table tick, i.e. one position
        // step every 64 CPU cycles (four 16-cycle unit ticks). The accumulator adds $10000 per
        // tick and the position is bits 18-23, so a step needs 2^18/$10000 = 4 ticks.
        let mut s = unit();
        let step = |s: &mut FdsSound| {
            for _ in 0..UNIT_TICK_CYCLES {
                s.clock();
            }
        };
        for _ in 0..3 {
            step(&mut s);
        }
        assert_eq!(s.position(), 0, "three ticks are still short of a step");
        step(&mut s);
        assert_eq!(s.position(), 1, "64 CPU cycles per step");
        for _ in 4..8 {
            step(&mut s);
        }
        assert_eq!(s.position(), 2);
        for _ in 8..(64 * 4) {
            step(&mut s);
        }
        assert_eq!(s.position(), 0, "64 steps wrap back to the start");
    }

    #[test]
    fn halted_wave_outputs_the_first_sample() {
        let mut s = FdsSound::new();
        s.write(0x4089, 0x80);
        s.wave[0] = 12;
        s.wave[1] = 60;
        s.write(0x4089, 0x00);
        s.write(0x4080, 0xC0 | 0x20);
        s.write(0x4083, 0x84); // bit 7: wave unit halted
        for _ in 0..20_000 {
            s.clock();
        }
        assert_eq!(s.dac_value(), 12);
        assert!((s.output() - 12.0 / 63.0).abs() < 0.02, "{}", s.output());
    }

    #[test]
    fn modulation_table_writes_advance_the_playback_position() {
        let mut s = FdsSound::new();
        s.write(0x4087, 0x80); // modulator disabled: table writes accepted, accumulator reset
        for i in 0..MOD_STEPS {
            s.write(0x4088, (i % 8) as u8);
        }
        // 32 writes walk the position back to where it started.
        assert_eq!((s.mod_acc >> 13) & 0x1F, 0);
        assert_eq!(s.mod_table[0], BIAS[0] as u8);
        assert_eq!(s.mod_table[3], BIAS[3] as u8);
        assert_eq!(s.mod_table[4], MOD_RESET);
    }

    #[test]
    fn modulation_counter_steps_while_the_modulator_runs() {
        let mut s = FdsSound::new();
        s.write(0x4087, 0x80);
        s.write(0x4088, 1); // entry 0 = +1 per step
        for _ in 1..MOD_STEPS {
            s.write(0x4088, 0);
        }
        s.write(0x4085, 0);
        s.write(0x4084, 0xC0 | 0x20); // mod gain 32, envelope off
        s.write(0x4086, 0x00);
        s.write(0x4087, 0x01); // mod pitch 0x100, modulator running
        for _ in 0..(UNIT_TICK_CYCLES * 64) {
            s.clock();
        }
        assert!(s.mod_counter > 0, "counter stepped: {}", s.mod_counter);
    }

    #[test]
    fn waveram_is_write_protected_until_bit_seven_of_4089() {
        let mut s = FdsSound::new();
        s.write_wave(0x4045, 0x2A);
        assert_eq!(s.wave[5], 0, "writes are dropped while protected");
        s.write(0x4089, 0x80);
        s.write_wave(0x4045, 0x2A);
        assert_eq!(s.wave[5], 0x2A);
    }

    #[test]
    fn output_is_held_while_wavetable_writes_are_enabled() {
        let mut s = unit();
        for _ in 0..20_000 {
            s.clock();
        }
        let before = s.output();
        s.write(0x4089, 0x80); // hold
        for _ in 0..20_000 {
            s.clock();
        }
        assert_eq!(s.output(), before);
    }

    #[test]
    fn reset_clears_every_register() {
        let mut s = unit();
        s.reset();
        assert_eq!(s.volume.gain, 0);
        assert_eq!(s.freq, 0);
        assert_eq!(s.output(), 0.0);
    }

    /// Only the cached `target` is new — the one-pole still runs every cycle, so the output
    /// must stay bit-identical to the pre-fix expression, recomputed here as the reference.
    #[test]
    fn caching_the_target_leaves_the_filter_identical() {
        let mut s = unit();
        // A ramp table, so the wave position moves the DAC value on every unit tick.
        for (i, w) in s.wave.iter_mut().enumerate() {
            *w = (i as u8) & 0x3F;
        }
        s.write(0x4086, 0x40); // modulation pitch, so the wave pitch and the volume both move
        s.write(0x4087, 0x00);
        let mut lowpass = 0.0f32;
        let mut held = 0.0f32;
        for cycle in 0..5000 {
            s.clock();
            let target = f32::from(s.dac_value()) / 63.0
                * (f32::from(s.vol_latched.min(32)) / 32.0)
                * s.master_gain();
            lowpass += (target - lowpass) * s.lowpass_coeff;
            if !s.wave_write_enable {
                held = lowpass;
            }
            assert_eq!(s.output().to_bits(), held.to_bits(), "cycle {cycle}");
        }
        assert!(held > 0.0, "the reference channel is audible: {held}");
    }

    /// A register write lands in the cached target on the next clock, not before it — the timing
    /// the unconditional recompute had.
    #[test]
    fn a_register_write_shows_up_on_the_next_clock() {
        let mut s = unit();
        for _ in 0..1000 {
            s.clock();
        }
        let before = s.output();
        s.write(0x4089, 0x03); // master volume 2/5
        assert_eq!(s.output(), before, "the level only moves inside clock");
        s.clock();
        assert!(
            s.output() < before,
            "the new master volume is applied: {} vs {before}",
            s.output()
        );
    }

    /// A load carries no `target` — `state_loaded` must leave it stale so the first cycle rebuilds
    /// it, or the filter would run on a zero input for a cycle.
    #[test]
    fn a_loaded_state_rebuilds_the_target_before_filtering() {
        let mut s = unit();
        for _ in 0..1000 {
            s.clock();
        }
        s.state_loaded();
        let lowpass = s.lowpass;
        s.clock();
        let target = f32::from(s.dac_value()) / 63.0
            * (f32::from(s.vol_latched.min(32)) / 32.0)
            * s.master_gain();
        assert!(target > 0.0);
        let expected = lowpass + (target - lowpass) * s.lowpass_coeff;
        assert_eq!(s.output().to_bits(), expected.to_bits());
    }
}
