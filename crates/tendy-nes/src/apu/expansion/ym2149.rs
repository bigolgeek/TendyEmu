//! Sunsoft 5B audio: a Yamaha YM2149F SSG with its SEL pin low, driven by the CPU clock.
//!
//! Three square-wave tone channels, one shared noise generator and one shared envelope
//! generator, selected through a two-step register port:
//!
//! ```text
//! $C000-$DFFF  DDDD RRRR   high nibble nonzero disables the data port, low nibble selects reg
//! $E000-$FFFF  VVVV VVVV   value for the selected internal register
//! ```
//!
//! Registers $00-$05 are the three 12-bit tone periods, $06 the 5-bit noise period, $07 the
//! tone/noise enables, $08-$0A volume (bit 4 = envelope), $0B-$0C the 16-bit envelope period and
//! $0D the envelope shape.
//!
//! Timing per the NESdev Sunsoft 5B audio page: the tone counters step every 16 CPU cycles (so a
//! tone period of `t` is `CPU / (32 * t)`), noise shifts every 32 cycles, and the envelope steps
//! every `16 * t` cycles. Volume is the chip's logarithmic 1.5 dB-per-step DAC, so this module
//! converts the 5-bit level to a linear amplitude instead of summing raw register values.

use crate::state::{StateError, StateReader, StateWriter};

/// Peak of the three channels against the 2A03 mixer. The 5B is the loudest expansion audio board
/// ("very loud compared to other audio expansion carts" on the wiki), so it sits higher than the
/// VRC6's 0.40 but still leaves the native channels dominant.
pub const YM2149_AUDIO_SCALE: f32 = 0.50;

/// The chip's logarithmic DAC: 1.5 dB per step, full scale at 31.
fn dac_level(level: u8) -> f32 {
    if level < 2 {
        // The wiki: envelope levels 0 and 1 are both equivalent to volume 0.
        return 0.0;
    }
    let db = -1.5 * f32::from(31 - level.min(31));
    10f32.powf(db / 20.0)
}

/// `dac_level` for the 32 levels, computed once: `analog_sum` runs every CPU cycle.
fn dac_table() -> &'static [f32; 32] {
    static TABLE: std::sync::OnceLock<[f32; 32]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|level| dac_level(level as u8)))
}

#[derive(Clone, Debug)]
pub struct Ym2149 {
    regs: [u8; 16],
    /// Low nibble of the last `$C000` write.
    latch: u8,
    /// The high nibble of the last `$C000` write disables the data port while nonzero.
    write_enabled: bool,
    /// Per-tone-channel 12-bit period counters and their square-wave level.
    tone_counter: [u16; 3],
    tone_high: [bool; 3],
    noise_counter: u16,
    /// 17-bit LFSR; bit 0 ^ bit 3 feeds bit 16 (the x^17 + x^14 + 1 polynomial, shifted right).
    noise_lfsr: u32,
    env_counter: u16,
    env_step: u8,
    env_level: u8,
    env_up: bool,
    env_done: bool,
    /// CPU cycles into the 16-cycle tone/noise prescaler, and the /32 noise halving.
    div16: u8,
    div32: bool,
    level: f32,
    /// `level` is stale: a register write or a prescaler tick moved an input of `analog_sum`.
    level_dirty: bool,
}

impl Default for Ym2149 {
    fn default() -> Self {
        Ym2149 {
            regs: [0; 16],
            latch: 0,
            write_enabled: false,
            tone_counter: [0; 3],
            tone_high: [false; 3],
            noise_counter: 0,
            noise_lfsr: 0,
            env_counter: 0,
            env_step: 0,
            env_level: 0,
            env_up: false,
            env_done: false,
            div16: 0,
            div32: false,
            level: 0.0,
            level_dirty: true,
        }
    }
}

impl Ym2149 {
    pub fn new() -> Self {
        Ym2149 {
            write_enabled: true,
            noise_lfsr: 1,
            env_level: 31,
            ..Ym2149::default()
        }
    }

    /// Write to the register-select port, `$C000-$DFFF`.
    pub fn write_select(&mut self, val: u8) {
        self.write_enabled = val & 0xF0 == 0;
        if self.write_enabled {
            self.latch = val & 0x0F;
        }
    }

    /// Write to the data port, `$E000-$FFFF`.
    pub fn write_data(&mut self, val: u8) {
        if !self.write_enabled {
            return;
        }
        let reg = usize::from(self.latch);
        self.regs[reg] = val;
        // the volume/enable registers are inputs of `analog_sum`.
        self.level_dirty = true;
        if reg == 0x0D {
            // Writing the shape resets the envelope: back to step 0 in the attack direction and,
            // with the shape's attack bit clear, at full volume.
            let attack = val & 0x04 != 0;
            self.env_step = 0;
            self.env_up = attack;
            self.env_level = if attack { 0 } else { 31 };
            self.env_done = false;
        }
    }

    fn tone_period(&self, i: usize) -> u16 {
        u16::from(self.regs[i * 2]) | (u16::from(self.regs[i * 2 + 1] & 0x0F) << 8)
    }

    fn noise_period(&self) -> u16 {
        u16::from(self.regs[0x06] & 0x1F)
    }

    fn env_period(&self) -> u16 {
        u16::from(self.regs[0x0B]) | (u16::from(self.regs[0x0C]) << 8)
    }

    /// One 16-cycle prescaler tick: tone counters, the envelope, and every second tick the noise.
    fn tick16(&mut self) {
        for i in 0..3 {
            let period = self.tone_period(i);
            // Count, then compare: a period of `t` flips every `t` ticks, and 0 acts like 1.
            self.tone_counter[i] = self.tone_counter[i].wrapping_add(1);
            if self.tone_counter[i] >= period {
                self.tone_counter[i] = 0;
                self.tone_high[i] = !self.tone_high[i];
            }
        }

        self.div32 = !self.div32;
        if self.div32 {
            let period = self.noise_period();
            self.noise_counter = self.noise_counter.wrapping_add(1);
            if self.noise_counter >= period {
                self.noise_counter = 0;
                // Shifting right, the taps sit at the output end (bits 0 and 3); feeding bit 16
                // from bits 16 and 13 locks the register at zero.
                let bit = (self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1;
                self.noise_lfsr = (self.noise_lfsr >> 1) | (bit << 16);
            }
        }

        let period = self.env_period();
        self.env_counter = self.env_counter.wrapping_add(1);
        if self.env_counter >= period {
            self.env_counter = 0;
            self.clock_envelope();
        }
        // tone/noise/envelope all move here, so the cached level is stale from now on.
        self.level_dirty = true;
    }

    fn clock_envelope(&mut self) {
        if self.env_done {
            return;
        }
        self.env_step += 1;
        if self.env_step >= 32 {
            self.env_step = 0;
            let shape = self.regs[0x0D] & 0x0F;
            let cont = shape & 0x08 != 0;
            let alternate = shape & 0x02 != 0;
            let hold = shape & 0x01 != 0;
            if !cont {
                // One ramp, then silence: the alternate/hold bits have no effect.
                self.env_done = true;
                self.env_level = 0;
                return;
            }
            if hold {
                self.env_done = true;
                let end = if self.env_up { 31 } else { 0 };
                // Alternate + hold flips the level once more at the end of the ramp.
                self.env_level = if alternate { 31 - end } else { end };
                return;
            }
            if alternate {
                self.env_up = !self.env_up;
            }
        }
        self.env_level = if self.env_up {
            self.env_step
        } else {
            31 - self.env_step
        };
    }

    /// A channel's 5-bit volume: the envelope's level if bit 4 selects it, else the 4-bit volume
    /// scaled up (the register cannot control the DAC's least significant bit).
    fn channel_volume(&self, i: usize) -> u8 {
        let reg = self.regs[0x08 + i];
        if reg & 0x10 != 0 {
            self.env_level
        } else {
            (reg & 0x0F) * 2 + 1
        }
    }

    /// Sum of the three channels through the log DAC, before normalisation.
    fn analog_sum(&self) -> f32 {
        let enables = self.regs[0x07];
        let mut sum = 0.0;
        for i in 0..3 {
            let tone_disabled = enables & (1 << i) != 0;
            let noise_disabled = enables & (1 << (i + 3)) != 0;
            let tone = if tone_disabled {
                true
            } else {
                self.tone_high[i]
            };
            let noise = if noise_disabled {
                true
            } else {
                self.noise_lfsr & 1 != 0
            };
            if tone && noise {
                sum += dac_table()[usize::from(self.channel_volume(i).min(31))];
            }
        }
        sum / 3.0 * YM2149_AUDIO_SCALE
    }

    /// The selected internal register, for tests and debug views.
    pub fn register(&self, reg: usize) -> u8 {
        self.regs.get(reg).copied().unwrap_or(0)
    }
}

impl Ym2149 {
    /// One CPU cycle: the 16-cycle prescaler and the rebuilt level.
    pub fn clock(&mut self) {
        self.div16 += 1;
        if self.div16 >= 16 {
            self.div16 = 0;
            self.tick16();
        }
        // every input of `analog_sum` moves only in `write_data` or `tick16`, so the level is
        // rebuilt at most once per 16 cycles instead of on every cycle.
        if self.level_dirty {
            self.level = self.analog_sum();
            self.level_dirty = false;
        }
    }

    /// The chip's current contribution, in the 2A03 mixer's scale.
    pub fn output(&self) -> f32 {
        self.level
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.regs);
        w.u8(self.latch);
        w.bool(self.write_enabled);
        for i in 0..3 {
            w.u16(self.tone_counter[i]);
            w.bool(self.tone_high[i]);
        }
        w.u16(self.noise_counter);
        w.u32(self.noise_lfsr);
        w.u16(self.env_counter);
        w.u8(self.env_step);
        w.u8(self.env_level);
        w.bool(self.env_up);
        w.bool(self.env_done);
        w.u8(self.div16);
        w.bool(self.div32);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.regs)?;
        self.latch = r.u8()?;
        // `latch` selects one of the 16 registers in `write_data`.
        if self.latch > 15 {
            return Err(StateError::BadValue);
        }
        self.write_enabled = r.bool()?;
        for i in 0..3 {
            self.tone_counter[i] = r.u16()?;
            self.tone_high[i] = r.bool()?;
        }
        self.noise_counter = r.u16()?;
        self.noise_lfsr = r.u32()?;
        // a zero register shifts in zeros forever, so a state that carries zero would stay
        // silent. Nothing that can be reached from `new`/`clock` is zero, so this is only a repair.
        if self.noise_lfsr == 0 {
            self.noise_lfsr = 1;
        }
        self.env_counter = r.u16()?;
        self.env_step = r.u8()?;
        // 0..32 is the reachable range; `clock_envelope` adds one to it, so a larger value is a
        // damaged file. `env_level` needs no guard: `analog_sum` clamps it to the DAC's 32 steps.
        if self.env_step > 31 {
            return Err(StateError::BadValue);
        }
        self.env_level = r.u8()?;
        self.env_up = r.bool()?;
        self.env_done = r.bool()?;
        self.div16 = r.u8()?;
        // 0..16 is the reachable range and the next `clock` adds one to it.
        if self.div16 >= 16 {
            return Err(StateError::BadValue);
        }
        self.div32 = r.bool()?;
        // `level` is `analog_sum()` of the state just read; recompute it rather than storing
        // it. `clock` rebuilds it whenever an input moved, so this is the value that was saved.
        // the flag itself is not in the file; the rebuilt level makes it clean again.
        self.level = self.analog_sum();
        self.level_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(chip: &mut Ym2149, reg: u8, val: u8) {
        chip.write_select(reg);
        chip.write_data(val);
    }

    #[test]
    fn register_port_needs_an_enabled_select() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x08, 0x0F);
        assert_eq!(chip.register(0x08), 0x0F);
        // High nibble set: the low nibble no longer selects, and the data port is closed.
        chip.write_select(0x18);
        chip.write_data(0x00);
        assert_eq!(
            chip.register(0x08),
            0x0F,
            "writes are ignored while disabled"
        );
        // ...and the latch stays where it was, so $18 did not select register 8.
        chip.write_select(0x00);
        chip.write_data(0x0A);
        assert_eq!(chip.register(0x00), 0x0A);
        assert_eq!(chip.register(0x08), 0x0F);
    }

    #[test]
    fn tone_period_flips_every_period_ticks() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x07, 0x3F); // all tones and noise off -> constant output
        write(&mut chip, 0x08, 0x0F); // channel A at full volume
        write(&mut chip, 0x00, 4); // period 4 -> a flip every 4 prescaler ticks
        let mut flips = 0;
        let mut last = chip.tone_high[0];
        for _ in 0..(16 * 4 * 10) {
            chip.clock();
            if chip.tone_high[0] != last {
                flips += 1;
                last = chip.tone_high[0];
            }
        }
        assert_eq!(flips, 10);
    }

    #[test]
    fn noise_and_envelope_step_every_period_ticks() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x06, 3); // noise period 3 -> a shift every 3 * 32 cycles
        write(&mut chip, 0x0B, 3); // envelope period 3 -> a step every 3 * 16 cycles
        write(&mut chip, 0x0D, 0x0C); // rising sawtooth, so env_step counts the steps
        let mut shifts = 0;
        let mut last = chip.noise_lfsr;
        for _ in 0..(32 * 3 * 10) {
            chip.clock();
            if chip.noise_lfsr != last {
                shifts += 1;
                last = chip.noise_lfsr;
            }
        }
        assert_eq!(shifts, 10);
        assert_eq!(chip.env_step, 20);
    }

    #[test]
    fn period_zero_acts_like_period_one() {
        let mut chip = Ym2149::new(); // every period register is 0
        let last = chip.tone_high[0];
        for _ in 0..16 {
            chip.clock();
        }
        assert_ne!(chip.tone_high[0], last, "a flip on every prescaler tick");
    }

    #[test]
    fn dac_table_matches_the_formula_for_all_levels() {
        for level in 0..32u8 {
            let formula = if level < 2 {
                0.0
            } else {
                10f32.powf(-1.5 * f32::from(31 - level) / 20.0)
            };
            assert_eq!(
                dac_table()[usize::from(level)].to_bits(),
                formula.to_bits(),
                "level {level}"
            );
        }
    }

    /// The table was filled from `dac_level`, so indexing it is bit-identical to calling the
    /// function — including the < 2 levels that return a literal zero.
    #[test]
    fn the_dac_table_is_bit_identical_to_dac_level() {
        for level in 0..32u8 {
            assert_eq!(
                dac_table()[usize::from(level)].to_bits(),
                dac_level(level).to_bits(),
                "level {level}"
            );
        }
    }

    /// The register script for the stream test: it moves every input of `analog_sum` — tone
    /// periods, the noise period, the enables, both volume fields (direct and envelope) and the
    /// envelope shape — so a cycle where the level is rebuilt and a cycle where it is not are both
    /// exercised, in runs of both kinds.
    fn scripted_write(chip: &mut Ym2149, cycle: usize) {
        match cycle {
            0 => {
                write(chip, 0x00, 2); // tone A: a flip every 2 prescaler ticks
                write(chip, 0x02, 5);
                write(chip, 0x04, 1);
                write(chip, 0x06, 1); // noise: a shift every 2 * 32 cycles
                write(chip, 0x07, 0x18); // tones on; noise on channel C only, so A and B follow
                // their tone counters instead of the slow LFSR gate
                write(chip, 0x08, 0x0F); // channel A: a direct 4-bit volume
                write(chip, 0x09, 0x08);
                write(chip, 0x0A, 0x1E); // channel C: the envelope (bit 4)
            }
            40 => {
                write(chip, 0x0B, 1); // envelope: a step every 16 cycles
                write(chip, 0x0C, 0);
                write(chip, 0x0D, 0x0E); // continue, attack, alternate
            }
            400 => write(chip, 0x07, 0x3F), // all tones and noise off: a constant level
            600 => write(chip, 0x07, 0x18),
            700 => write(chip, 0x08, 0x00),  // channel A silent
            900 => write(chip, 0x08, 0x1E),  // ...then driven by the envelope as well
            1000 => write(chip, 0x0D, 0x00), // continue clear: ramp down, then silence
            1300 => write(chip, 0x07, 0x00), // noise on for all three channels
            1500 => write(chip, 0x00, 0x20),
            _ => {}
        }
    }

    const STREAM_CYCLES: usize = 2048;

    /// Runs the script over `from..to`, asserting after every cycle that the cached `level` is the
    /// value the pre-fix code rebuilt on that cycle (`level = analog_sum()` unconditionally).
    fn run_script(chip: &mut Ym2149, from: usize, to: usize, stream: &mut Vec<u32>) {
        for cycle in from..to {
            scripted_write(chip, cycle);
            chip.clock();
            let level = chip.output().to_bits();
            assert_eq!(
                level,
                chip.analog_sum().to_bits(),
                "cycle {cycle}: the cached level is not the rebuilt sum"
            );
            stream.push(level);
        }
    }

    /// `clock` now skips `analog_sum` on cycles where nothing moved. The per-cycle check
    /// against a fresh `analog_sum()` is the old code's value, so this is the sample-identical
    /// proof: every cycle of the stream must match bit for bit, before and after a save/load.
    #[test]
    fn the_cached_level_matches_a_per_cycle_rebuild() {
        let mut a = Ym2149::new();
        let mut stream = Vec::new();
        run_script(&mut a, 0, 1024, &mut stream);

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Ym2149::new();
        b.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            b.output().to_bits(),
            a.output().to_bits(),
            "the loaded level is the saved one"
        );

        // `b` was never clocked through the first 1024 cycles; from the load onwards its stream must
        // be `a`'s, bit for bit.
        let mut tail_a = Vec::new();
        let mut tail_b = Vec::new();
        run_script(&mut a, 1024, STREAM_CYCLES, &mut tail_a);
        run_script(&mut b, 1024, STREAM_CYCLES, &mut tail_b);
        assert_eq!(tail_a, tail_b, "the loaded chip left the stream");
        stream.extend(tail_a);

        // The workload has to move the level, or this proves nothing.
        let distinct: std::collections::HashSet<u32> = stream.iter().copied().collect();
        let transitions = stream.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(
            distinct.len() > 12,
            "the stream must vary: {} distinct levels",
            distinct.len()
        );
        assert!(
            transitions > 60,
            "the level must move often: {transitions} transitions"
        );
    }

    /// `level` is a cache, so `clock` must rebuild it only on cycles where an input moved —
    /// a register write or the 16-cycle prescaler tick. Rebuilding every cycle is the cost this
    /// removes, so a poisoned cache must survive a cycle that moves nothing.
    #[test]
    fn clock_only_rebuilds_the_level_when_an_input_moved() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x07, 0x00);
        write(&mut chip, 0x08, 0x0F);
        chip.clock(); // div16 = 1: rebuilds the level the writes dirtied
        chip.level = -1.0; // only a rebuild can overwrite this
        for cycle in 1..15 {
            // div16 = 2..=15: no prescaler tick, nothing moved.
            chip.clock();
            assert_eq!(chip.level, -1.0, "cycle {cycle} rebuilt an unchanged level");
        }
        // div16 = 16 runs `tick16`, so this same clock must rebuild the level.
        chip.clock();
        assert_eq!(
            chip.output().to_bits(),
            chip.analog_sum().to_bits(),
            "the tick's rebuild"
        );
    }

    #[test]
    fn disabled_tone_and_noise_output_a_constant() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x07, 0x3F);
        write(&mut chip, 0x08, 0x0F);
        write(&mut chip, 0x09, 0x0F);
        write(&mut chip, 0x0A, 0x0F);
        chip.clock();
        let expected = chip.analog_sum();
        for _ in 0..1000 {
            chip.clock();
            assert!((chip.analog_sum() - expected).abs() < 1e-6);
        }
        assert!(
            expected > 0.4,
            "three full-volume channels are loud: {expected}"
        );
    }

    #[test]
    fn volume_zero_is_silent() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x07, 0x3F);
        write(&mut chip, 0x08, 0x00);
        write(&mut chip, 0x09, 0x00);
        write(&mut chip, 0x0A, 0x00);
        chip.clock();
        assert_eq!(chip.output(), 0.0);
    }

    /// The exact taps are pinned by a golden vector — bit 0 ^ bit 3 into bit 16, starting
    /// from the `new` seed of 1. Moving a tap changes every value after the shift it lands on.
    #[test]
    fn the_noise_lfsr_matches_a_golden_vector() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x06, 0); // fastest noise
        let mut values = vec![chip.noise_lfsr];
        while values.len() < 22 {
            chip.clock();
            if chip.noise_lfsr != *values.last().expect("a value") {
                values.push(chip.noise_lfsr);
            }
        }
        assert_eq!(
            values,
            [
                0x00001, 0x10000, 0x08000, 0x04000, 0x02000, 0x01000, 0x00800, 0x00400, 0x00200,
                0x00100, 0x00080, 0x00040, 0x00020, 0x00010, 0x00008, 0x10004, 0x08002, 0x04001,
                0x12000, 0x09000, 0x04800, 0x02400,
            ],
            "the LFSR taps moved"
        );
    }

    /// A state written by the pre-fix code can carry `noise_lfsr = 0`, which the shift
    /// keeps at zero forever. A load resets it, so an old state gets its noise back.
    #[test]
    fn a_loaded_lfsr_of_zero_is_reset() {
        let mut a = Ym2149::new();
        write(&mut a, 0x06, 0);
        a.noise_lfsr = 0;
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let mut b = Ym2149::new();
        b.load_state(&mut StateReader::new(&w.finish())).unwrap();
        assert_eq!(b.noise_lfsr, 1, "a zero LFSR is repaired on load");
        let mut seen = std::collections::HashSet::new();
        for _ in 0..(32 * 100) {
            b.clock();
            seen.insert(b.noise_lfsr);
        }
        assert!(
            seen.len() > 20,
            "distinct values after the repair: {}",
            seen.len()
        );
        assert_ne!(b.noise_lfsr, 0);
    }

    #[test]
    fn noise_shifts_the_seventeen_bit_lfsr() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x06, 0); // fastest noise
        write(&mut chip, 0x07, 0x07); // tones off, noise on for all channels
        write(&mut chip, 0x08, 0x0F);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..(32 * 1000) {
            chip.clock();
            seen.insert(chip.noise_lfsr);
        }
        assert_ne!(chip.noise_lfsr, 0, "the register must not lock at zero");
        assert!(seen.len() > 900, "distinct values {}", seen.len());
        assert!(chip.noise_lfsr < (1 << 17), "17-bit register");
    }

    #[test]
    fn envelope_shape_zero_ramps_down_then_holds_silence() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x0B, 0); // fastest envelope period
        write(&mut chip, 0x0C, 0);
        write(&mut chip, 0x0D, 0x00); // continue=0, attack=0 -> decay then silence
        assert_eq!(
            chip.env_level, 31,
            "an attack-clear shape starts at full volume"
        );
        let mut levels = Vec::new();
        for _ in 0..(32 * 2) {
            for _ in 0..16 {
                chip.clock();
            }
            levels.push(chip.env_level);
        }
        assert_eq!(levels[0], 30, "the first envelope tick drops one step");
        assert!(levels[20] < levels[0]);
        assert_eq!(levels[31], 0);
        assert!(chip.env_done);
    }

    #[test]
    fn envelope_alternating_shape_oscillates() {
        let mut chip = Ym2149::new();
        write(&mut chip, 0x0B, 0);
        write(&mut chip, 0x0C, 0);
        write(&mut chip, 0x0D, 0x0E); // continue, attack, alternate, no hold
        for _ in 0..(32 * 32) {
            for _ in 0..16 {
                chip.clock();
            }
        }
        assert!(!chip.env_done, "a continuing shape never finishes");
        let a = chip.env_level;
        for _ in 0..(32 * 16) {
            chip.clock();
        }
        assert_ne!(a, chip.env_level, "the ramp reverses direction");
    }

    #[test]
    fn state_round_trips() {
        let mut a = Ym2149::new();
        for (reg, val) in [
            (0x00u8, 0x12u8),
            (0x01, 0x03),
            (0x06, 0x11),
            (0x07, 0x38),
            (0x08, 0x1F),
            (0x0D, 0x0A),
        ] {
            write(&mut a, reg, val);
        }
        for _ in 0..1234 {
            a.clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Ym2149::new();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        for _ in 0..500 {
            a.clock();
            b.clock();
            assert_eq!(a.output(), b.output());
        }
    }

    /// `latch` selects one of the 16 registers on a data-port write, and `div16`/`env_step`
    /// are added to on every clock. A state carrying any of them out of range used to load cleanly
    /// and then either index past `regs` or overflow the counter.
    #[test]
    fn out_of_range_state_fields_are_a_bad_value() {
        fn rejects(mutate: fn(&mut Ym2149), name: &str) {
            let mut chip = Ym2149::new();
            mutate(&mut chip);
            let mut w = StateWriter::new();
            chip.save_state(&mut w);
            let bytes = w.finish();
            assert_eq!(
                Ym2149::new().load_state(&mut StateReader::new(&bytes)),
                Err(StateError::BadValue),
                "{name} must be rejected on load"
            );
        }
        rejects(|c| c.latch = 16, "latch 16");
        rejects(|c| c.div16 = 16, "div16 16");
        rejects(|c| c.env_step = 32, "env_step 32");
    }

    /// `level` is derived and no longer saved; a load recomputes it from the loaded
    /// registers, which is also what the next `clock` would produce.
    #[test]
    fn a_loaded_level_is_rebuilt_from_the_loaded_state() {
        let mut a = Ym2149::new();
        write(&mut a, 0x07, 0x3F); // tones and noise off: a constant level
        write(&mut a, 0x08, 0x0F);
        write(&mut a, 0x09, 0x0F);
        write(&mut a, 0x0A, 0x0F);
        for _ in 0..500 {
            a.clock();
        }
        let stale = a.output();
        write(&mut a, 0x08, 0x00); // channel A silent; `a.level` is stale until the next clock
        assert_eq!(a.output().to_bits(), stale.to_bits());

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Ym2149::new();
        b.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            b.output().to_bits(),
            b.analog_sum().to_bits(),
            "the loaded level is the loaded sum"
        );
        assert_ne!(
            b.output().to_bits(),
            stale.to_bits(),
            "the stale level must not come back"
        );
        a.clock();
        b.clock();
        assert_eq!(a.output().to_bits(), b.output().to_bits());
    }
}
