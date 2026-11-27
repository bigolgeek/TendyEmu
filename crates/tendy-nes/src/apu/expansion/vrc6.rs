//! Konami VRC6 audio: two pulse channels and one sawtooth, mixed linearly.
//!
//! Registers (mapper 24 addresses; mapper 26 swaps address bits A0/A1, which the board applies
//! before calling in here):
//!
//! ```text
//! $9000  MDDD VVVV   pulse 1: mode, duty, volume
//! $9001  FFFF FFFF   pulse 1 period low
//! $9002  E... FFFF   pulse 1: enable, period high
//! $9003  .... .ABH   global: 256x, 16x, halt
//! $A000  MDDD VVVV   pulse 2 (same layout)
//! $A001 / $A002      pulse 2 period
//! $B000  ..AA AAAA   sawtooth accumulator rate
//! $B001 / $B002      sawtooth period
//! ```
//!
//! Timing per the NESdev VRC6 audio page: the pulse channels divide the CPU clock by
//! `16 * (t + 1)` (16 steps of the duty generator, each `t + 1` CPU cycles), the sawtooth by
//! `14 * (t + 1)`. The `$9003` 16x/256x bits right-shift the 12-bit periods by 4 or 8; halt stops
//! every oscillator in place.
//!
//! Output is the chip's 6-bit linear DAC: the two 4-bit pulse levels plus the top five bits of the
//! sawtooth accumulator, i.e. 0..=61. [`VRC6_AUDIO_SCALE`] maps that onto the 2A03 mixer's scale.

use crate::state::{StateError, StateReader, StateWriter};

/// Peak level of a fully-driven VRC6 against the 2A03 mixer: at maximum volume its pulse channels
/// are roughly a 2A03 pulse pair, and the sawtooth is a third channel on top, so 0.40 is slightly
/// above a 0.26 pulse pair without dominating the native channels.
pub const VRC6_AUDIO_SCALE: f32 = 0.40;

/// Full-scale sum of the chip's 6-bit DAC (two 15-step pulses + 31-step sawtooth).
const DAC_FULL_SCALE: f32 = 61.0;

#[derive(Clone, Debug, Default)]
struct Pulse {
    /// `$x000`: mode (bit 7), duty (bits 4-6), volume (bits 0-3).
    ctrl: u8,
    /// 12-bit period from `$x001`/`$x002`.
    period: u16,
    enabled: bool,
    /// Cycles until the next duty-generator step.
    divider: u16,
    /// Duty step, 15 down to 0, so `step <= duty` gives `duty + 1` sixteenths.
    step: u8,
}

impl Pulse {
    /// One pulse channel's registers: `reg` 0 is `$x000`, 1 is `$x001`, 2 is `$x002`.
    fn write(&mut self, reg: u16, val: u8) {
        match reg {
            0 => self.ctrl = val,
            1 => self.period = (self.period & 0x0F00) | u16::from(val),
            _ => {
                self.period = (self.period & 0x00FF) | (u16::from(val & 0x0F) << 8);
                self.enabled = val & 0x80 != 0;
                if !self.enabled {
                    self.reset_phase();
                }
            }
        }
    }

    fn reset_phase(&mut self) {
        self.step = 15;
        self.divider = 0;
    }

    fn clock(&mut self, shift: u32) -> bool {
        // A disabled channel is halted: its divider and duty step stay where they are, so the
        // phase only restarts when `$x002` is written again (the wiki: "resume from the
        // beginning when E is once again set").
        if !self.enabled {
            return false;
        }
        // A `u16` shifted right by at most 8 always fits in a `u16`.
        let period = self.period >> shift;
        if self.divider == 0 {
            self.divider = period;
            self.step = if self.step == 0 { 15 } else { self.step - 1 };
            // the duty step moved, so the channel's output may have moved with it.
            true
        } else {
            self.divider -= 1;
            false
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        let volume = self.ctrl & 0x0F;
        let duty = (self.ctrl >> 4) & 0x07;
        if self.ctrl & 0x80 != 0 || self.step <= duty {
            volume
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Saw {
    /// `$B000` bits 0-5: accumulator rate.
    rate: u8,
    period: u16,
    enabled: bool,
    divider: u16,
    /// 0..=13, the 14-step period of the accumulator.
    step: u8,
    accum: u8,
}

impl Saw {
    fn reset(&mut self) {
        self.step = 0;
        self.accum = 0;
        self.divider = 0;
    }

    fn clock(&mut self, shift: u32) -> bool {
        if !self.enabled {
            return false;
        }
        // A `u16` shifted right by at most 8 always fits in a `u16`.
        let period = self.period >> shift;
        if self.divider == 0 {
            self.divider = period;
            self.step += 1;
            if self.step >= 14 {
                // The 14th step resets the accumulator instead of adding to it.
                self.step = 0;
                self.accum = 0;
            } else if self.step.is_multiple_of(2) {
                self.accum = self.accum.wrapping_add(self.rate);
            }
            // the accumulator (or the step's phase) moved.
            true
        } else {
            self.divider -= 1;
            false
        }
    }

    fn output(&self) -> u8 {
        if self.enabled { self.accum >> 3 } else { 0 }
    }
}

/// VRC6 expansion audio (mapper 24/26).
#[derive(Clone, Debug, Default)]
pub struct Vrc6Audio {
    pulse1: Pulse,
    pulse2: Pulse,
    saw: Saw,
    /// `$9003`: halt (bit 0), 16x (bit 1), 256x (bit 2).
    freq_ctrl: u8,
    /// Cached level, refreshed by [`Vrc6Audio::clock`] when an input moved and by a register
    /// write, so the board can hand it out cheaply.
    level: f32,
    /// A register write landed: the cached level is stale and must be rebuilt on the next clock.
    level_dirty: bool,
}

impl Vrc6Audio {
    pub fn new() -> Self {
        Self::default()
    }

    /// CPU write to `$9000-$9003`, `$A000-$A002` or `$B000-$B002`. The board has already mapped
    /// the VRC6b (mapper 26) A0/A1 swap, so `addr` is a mapper-24 address.
    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x9000..=0x9002 => self.pulse1.write(addr - 0x9000, val),
            0x9003 => self.freq_ctrl = val & 0x07,
            0xA000..=0xA002 => self.pulse2.write(addr - 0xA000, val),
            0xB000 => self.saw.rate = val & 0x3F,
            0xB001 => self.saw.period = (self.saw.period & 0x0F00) | u16::from(val),
            0xB002 => {
                self.saw.period = (self.saw.period & 0x00FF) | (u16::from(val & 0x0F) << 8);
                self.saw.enabled = val & 0x80 != 0;
                if !self.saw.enabled {
                    self.saw.reset();
                }
            }
            _ => {}
        }
        // Any register write can move what `dac` sums; the level is rebuilt on the next clock, as it
        // always was, so a write still does not show in `output` until then.
        self.level_dirty = true;
    }

    /// The period right-shift the `$9003` bits select: 256x wins over 16x, and halt stops the
    /// oscillators entirely.
    fn shift(&self) -> Option<u32> {
        if self.freq_ctrl & 0x01 != 0 {
            None
        } else if self.freq_ctrl & 0x04 != 0 {
            Some(8)
        } else if self.freq_ctrl & 0x02 != 0 {
            Some(4)
        } else {
            Some(0)
        }
    }

    /// The current DAC sum before normalisation, 0..=61.
    fn dac(&self) -> u16 {
        u16::from(self.pulse1.output())
            + u16::from(self.pulse2.output())
            + u16::from(self.saw.output())
    }

    /// One CPU cycle: both pulses, the sawtooth, then the cached level.
    pub fn clock(&mut self) {
        // the DAC sum only moves when a divider expires or a write landed, so the level is
        // rebuilt then instead of on all 1.79 million cycles a second.
        let mut moved = self.level_dirty;
        if let Some(shift) = self.shift() {
            // `|` and not `||`: every channel has to be clocked whether or not an earlier one moved.
            moved |= self.pulse1.clock(shift);
            moved |= self.pulse2.clock(shift);
            moved |= self.saw.clock(shift);
        }
        if moved {
            self.level = f32::from(self.dac()) / DAC_FULL_SCALE * VRC6_AUDIO_SCALE;
            self.level_dirty = false;
        }
    }
}

impl Vrc6Audio {
    /// The chip's current contribution, in the 2A03 mixer's scale.
    pub fn output(&self) -> f32 {
        self.level
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        for p in [&self.pulse1, &self.pulse2] {
            w.u8(p.ctrl);
            w.u16(p.period);
            w.bool(p.enabled);
            w.u16(p.divider);
            w.u8(p.step);
        }
        w.u8(self.saw.rate);
        w.u16(self.saw.period);
        w.bool(self.saw.enabled);
        w.u16(self.saw.divider);
        w.u8(self.saw.step);
        w.u8(self.saw.accum);
        w.u8(self.freq_ctrl);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        for p in [&mut self.pulse1, &mut self.pulse2] {
            p.ctrl = r.u8()?;
            p.period = r.u16()?;
            p.enabled = r.bool()?;
            p.divider = r.u16()?;
            p.step = r.u8()?;
        }
        self.saw.rate = r.u8()?;
        self.saw.period = r.u16()?;
        self.saw.enabled = r.bool()?;
        self.saw.divider = r.u16()?;
        self.saw.step = r.u8()?;
        self.saw.accum = r.u8()?;
        self.freq_ctrl = r.u8()?;
        // `level` is derived from the channel state, so it is not in the file; rebuild it
        // here, exactly as the next clock would. `level_dirty` was never stored either, so a state
        // saved right after a register write used to come back with that write's stale level.
        self.level = f32::from(self.dac()) / DAC_FULL_SCALE * VRC6_AUDIO_SCALE;
        self.level_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pulse channel whose control register is at `ctrl_addr` (`$9000` or `$A000`), with the
    /// given period and control byte, enabled.
    fn pulse(rate: &mut Vrc6Audio, ctrl_addr: u16, period: u16, ctrl: u8) {
        rate.write(ctrl_addr, ctrl);
        rate.write(ctrl_addr + 1, u8::try_from(period & 0xFF).unwrap());
        rate.write(ctrl_addr + 2, 0x80 | u8::try_from(period >> 8).unwrap());
    }

    #[test]
    fn pulse_period_divides_the_cpu_clock() {
        // period 7 -> a duty step every 8 CPU cycles, so the 16-step duty cycle is 128 cycles.
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 7, 0x7F);
        // Mode bit set: output is the volume regardless of duty, so the level never changes.
        for _ in 0..128 {
            audio.clock();
        }
        assert!((audio.output() - 15.0 / DAC_FULL_SCALE * VRC6_AUDIO_SCALE).abs() < 1e-6);

        // 50% duty (duty 7, mode clear): eight of the sixteen steps are high.
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 7, 0x7F);
        let mut high = 0;
        for _ in 0..128 {
            audio.clock();
            if audio.dac() != 0 {
                high += 1;
            }
        }
        assert_eq!(
            high, 64,
            "duty 7 with period 7 is high for half of the 128-cycle cycle"
        );
    }

    #[test]
    fn pulse_duty_zero_is_one_sixteenth() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 7, 0x0F);
        let mut high = 0;
        for _ in 0..128 {
            audio.clock();
            if audio.dac() != 0 {
                high += 1;
            }
        }
        assert_eq!(high, 8);
    }

    #[test]
    fn disabled_channel_is_silent_and_resets_phase() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 0, 0x7F);
        for _ in 0..10 {
            audio.clock();
        }
        audio.write(0x9002, 0x00);
        audio.clock();
        assert_eq!(audio.dac(), 0);
        assert_eq!(audio.pulse1.step, 15);
        assert_eq!(audio.pulse1.divider, 0);
    }

    #[test]
    fn halt_stops_the_oscillators_in_place() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 2, 0x7F);
        audio.clock();
        let step = audio.pulse1.step;
        audio.write(0x9003, 0x01);
        for _ in 0..100 {
            audio.clock();
        }
        assert_eq!(audio.pulse1.step, step);
        audio.write(0x9003, 0x00);
        for _ in 0..4 {
            audio.clock();
        }
        assert_ne!(audio.pulse1.step, step, "halt clear resumes the oscillator");
    }

    #[test]
    fn sixteen_x_shortens_the_period() {
        let mut long = Vrc6Audio::new();
        pulse(&mut long, 0x9000, 0x0F, 0x7F);
        let mut fast = Vrc6Audio::new();
        pulse(&mut fast, 0x9000, 0x0F, 0x7F);
        fast.write(0x9003, 0x02);
        let mut long_edges = 0;
        let mut fast_edges = 0;
        let mut last_long = long.dac();
        let mut last_fast = fast.dac();
        for _ in 0..(16 * 16) {
            long.clock();
            fast.clock();
            if long.dac() != last_long {
                long_edges += 1;
                last_long = long.dac();
            }
            if fast.dac() != last_fast {
                fast_edges += 1;
                last_fast = fast.dac();
            }
        }
        assert_eq!(
            long_edges, 2,
            "16 steps of 16 cycles: one high and one low transition"
        );
        assert_eq!(
            fast_edges, 32,
            "16x divides the period by 16, so 16 duty cycles in the same time"
        );
    }

    #[test]
    fn sawtooth_ramps_and_resets_after_fourteen_steps() {
        let mut audio = Vrc6Audio::new();
        audio.write(0xB000, 0x08);
        audio.write(0xB001, 0x00); // period 0: a step every CPU cycle
        audio.write(0xB002, 0x80);
        let mut levels = Vec::new();
        for _ in 0..14 {
            audio.clock();
            levels.push(audio.saw.accum);
        }
        // Adds 8 on the even steps 2, 4, 6, 8, 10, 12 and resets on the 14th step.
        assert_eq!(
            levels,
            vec![0, 8, 8, 16, 16, 24, 24, 32, 32, 40, 40, 48, 48, 0]
        );
    }

    #[test]
    fn output_scale_is_bounded() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 0, 0x7F);
        pulse(&mut audio, 0xA000, 0, 0x7F);
        audio.write(0xB000, 0x2A);
        audio.write(0xB002, 0x80);
        let mut peak: f32 = 0.0;
        for _ in 0..200 {
            audio.clock();
            peak = peak.max(audio.output());
        }
        assert!(
            peak <= VRC6_AUDIO_SCALE + 1e-6,
            "peak {peak} stays inside the scale"
        );
        assert!(
            peak > VRC6_AUDIO_SCALE * 0.5,
            "a driven chip is loud: {peak}"
        );
    }

    #[test]
    fn state_round_trips() {
        let mut a = Vrc6Audio::new();
        pulse(&mut a, 0x9000, 0x123, 0x5F);
        audio_clock(&mut a, 37);
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Vrc6Audio::new();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.output(), b.output());
        // Same waveform from here on.
        for _ in 0..100 {
            a.clock();
            b.clock();
            assert_eq!(a.output(), b.output());
        }
    }

    fn audio_clock(a: &mut Vrc6Audio, n: usize) {
        for _ in 0..n {
            a.clock();
        }
    }

    /// `level` is a cache of the DAC sum, so after every clock it must be bit-identical to the
    /// sum the pre-fix code rebuilt unconditionally.
    #[test]
    fn the_cached_level_is_the_recomputed_dac_every_cycle() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 3, 0x3F); // mode clear, duty 3, volume 15
        pulse(&mut audio, 0xA000, 5, 0x5F); // duty 5, volume 15
        audio.write(0xB000, 0x20);
        audio.write(0xB001, 0x03);
        audio.write(0xB002, 0x80);
        for cycle in 0..4000 {
            audio.clock();
            let reference = f32::from(audio.dac()) / DAC_FULL_SCALE * VRC6_AUDIO_SCALE;
            assert_eq!(
                audio.output().to_bits(),
                reference.to_bits(),
                "cycle {cycle}"
            );
        }
    }

    /// A write still lands in the level inside `clock`, not at the write: `$9000`'s volume reaches
    /// `output` on the next cycle, exactly as it did when the sum was rebuilt there.
    #[test]
    fn a_register_write_reaches_the_level_on_the_next_clock() {
        let mut audio = Vrc6Audio::new();
        pulse(&mut audio, 0x9000, 7, 0x8F); // mode bit set: the volume is the level
        audio.clock();
        let playing = audio.output();
        assert!(playing > 0.0);

        audio.write(0x9000, 0x00); // volume 0, mode clear
        assert_eq!(
            audio.output().to_bits(),
            playing.to_bits(),
            "not visible until the clock"
        );
        audio.clock();
        assert_eq!(audio.output(), 0.0);
    }

    /// `level` is derived and no longer saved. A state taken right after a write (the cache
    /// is one clock behind) must load with the level the write implies, not the stale one.
    #[test]
    fn a_loaded_level_is_rebuilt_from_the_loaded_state() {
        let mut a = Vrc6Audio::new();
        pulse(&mut a, 0x9000, 4, 0x8F); // mode set: the level is the volume, 15
        for _ in 0..20 {
            a.clock();
        }
        let stale = a.output();
        a.write(0x9000, 0x87); // volume 7; `a.level` is still the volume-15 value

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Vrc6Audio::new();
        b.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            b.output().to_bits(),
            (f32::from(b.dac()) / DAC_FULL_SCALE * VRC6_AUDIO_SCALE).to_bits(),
            "the loaded level is the sum of the loaded channel state"
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
