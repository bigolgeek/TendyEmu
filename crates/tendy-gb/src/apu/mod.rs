//! The APU: `NR10`-`NR52`, wave RAM, the mixer and the resampler.
//!
//! The frame sequencer has no clock of its own: `Bus::tick` raises [`Apu::div_apu_event`] on every
//! DIV bit-4 falling edge (512 Hz) and calls [`Apu::tick`] with the four dots of each M-cycle, so
//! everything here is a function of those two calls.
//!
//! Mixing happens in the digital domain: each sounding channel contributes its `$0`-`$F` DAC input
//! minus that channel's mid-point, so a disabled channel, or a waveform that sits at its mid-point,
//! mixes to exactly zero (Pan Docs, `Audio_details`, "DACs"). The analog high-pass filter is not
//! modelled.

use crate::CLOCK_HZ;
use crate::apu::channel::{Noise, Pulse, Wave};
use crate::sgb::SgbSound;
use crate::sgb::spc::DSP_SAMPLE_HZ;
use crate::state::{StateError, StateReader, StateWriter};

pub mod channel;

/// Post-boot `$FF10-$FF26` (DMG values).
const NR_DEFAULTS: [u8; 0x17] = [
    0x80, 0xBF, 0xF3, 0xFF, 0xBF, 0x00, 0x3F, 0x00, 0xFF, 0xBF, 0x7F, 0xFF, 0x9F, 0xFF, 0xBF, 0x00,
    0xFF, 0x00, 0x00, 0xBF, 0x77, 0xF3, 0xF1,
];

/// Staging buffer: 4 096 f32 = 2 048 stereo frames, allocated once.
const OUT_SAMPLES: usize = 4096;

/// A channel's mid-point: the DAC takes `$0`-`$F`, so half scale is 7.5.
const FULL_SCALE: f32 = 7.5;

/// One mixer unit: four channels at half scale times the eight-step `NR50` gain, so the mix of a
/// fully driven APU lands in `-1.0..=1.0`.
const MIX_SCALE: f32 = 4.0 * FULL_SCALE * 8.0;

/// How much of the SGB sound chip's output reaches the mix: a quarter, −12 dB.
///
/// The four Game Boy channels already mix to full scale, so the SGB's music has to fit beside them.
/// A quarter puts one SGB voice at roughly a single Game Boy channel's weight, leaves 12 dB of
/// headroom before the sum reaches ±1.0 (the clamp is a guard, not the normal case), and is
/// loud enough that the effect is heard rather than found by measurement.
pub const SGB_VOLUME: f32 = 0.25;

/// The sound unit.
#[derive(Clone)]
pub struct Apu {
    /// `$FF10-$FF26` as written, exposed for reads.
    pub nr: [u8; 0x17],
    /// `$FF30-$FF3F`, 32 4-bit samples.
    pub wave: [u8; 16],
    pub ch1: Pulse,
    pub ch2: Pulse,
    pub ch3: Wave,
    pub ch4: Noise,
    /// 0..7 DIV-APU step counter.
    pub div_apu: u8,
    /// The CHANNELS rows: bit 0 CH1 .. bit 3 CH4, 1 = audible.
    pub channel_mask: u8,
    /// Set by the front end from the open device.
    pub sample_rate: u32,
    /// Dots per output sample, fractional.
    pub acc: f64,
    /// Interleaved L,R staging, 4096 f32, allocated once.
    pub out: Vec<f32>,
    pub read_pos: usize,
    pub len: usize,
    /// The SGB's sound chip, when there is one: `None` unless an SGB ROM file is set *and*
    /// the console is an SGB, and then the only thing that changes in this file is that one extra
    /// term in [`Apu::mix`]. `None` leaves every sample byte-identical to before it existed.
    pub sgb: Option<Box<SgbSound>>,
    /// The SGB resampler's position between the last source frame (`sgb_cur`) and the next
    /// (`sgb_next`), as a numerator over the output rate; exact, so 32 kHz maps onto 44.1/48/96 kHz
    /// without drift.
    pub sgb_phase: u32,
    pub sgb_cur: [f32; 2],
    pub sgb_next: [f32; 2],
}

impl Apu {
    /// Post-boot NR values, power on, mask `$0F`.
    pub fn new() -> Apu {
        let mut apu = Apu {
            nr: NR_DEFAULTS,
            wave: [0; 16],
            ch1: Pulse::default(),
            ch2: Pulse::default(),
            ch3: Wave::default(),
            ch4: Noise::default(),
            div_apu: 0,
            channel_mask: 0x0F,
            sample_rate: 44_100,
            acc: 0.0,
            out: vec![0.0; OUT_SAMPLES],
            read_pos: 0,
            len: 0,
            sgb: None,
            sgb_phase: 0,
            sgb_cur: [0.0; 2],
            sgb_next: [0.0; 2],
        };
        // Replay the boot ROM's writes so the channel state matches the post-boot registers
        // Post-boot: NR12 before NR14 leaves CH1 on at `$FF26 = $F1`.
        for (i, value) in NR_DEFAULTS.iter().enumerate() {
            apu.write(0xFF10 + i as u16, *value);
        }
        apu
    }

    /// True while `$FF26` bit 7 is set.
    fn powered(&self) -> bool {
        self.nr[0x16] & 0x80 != 0
    }

    /// `NR10`-`NR52` as the register file reports them: 0 while the APU is off, since power-off
    /// clears every register (`Audio_Registers`, `NR52`).
    fn reg(&self, addr: u16) -> u8 {
        if self.powered() {
            self.nr[(addr - 0xFF10) as usize]
        } else {
            0
        }
    }

    /// Channel timers, mixer and resampler.
    pub fn tick(&mut self, dots: u32) {
        if let Some(sgb) = self.sgb.as_mut() {
            // The SGB chip's clock is derived from these same dots, exactly.
            sgb.tick(dots);
        }
        self.ch1.step(dots);
        self.ch2.step(dots);
        self.ch3.step(dots);
        self.ch4.step(dots);
        let per_sample = self.dots_per_sample();
        self.acc += dots as f64;
        while self.acc >= per_sample {
            self.acc -= per_sample;
            self.push_sample();
        }
    }

    fn dots_per_sample(&self) -> f64 {
        CLOCK_HZ as f64 / self.sample_rate.max(1) as f64
    }

    /// One DIV bit-4 falling edge: the frame sequencer (`Audio_details`, "Frame sequencer").
    pub fn div_apu_event(&mut self) {
        self.div_apu = (self.div_apu + 1) & 7;
        let step = self.div_apu;
        // Length: 256 Hz, every second step.
        if step & 1 == 0 {
            self.ch1.length_clock();
            self.ch2.length_clock();
            self.ch3.length_clock();
            self.ch4.length_clock();
        }
        // Sweep: 128 Hz, steps 2 and 6.
        if step & 3 == 2 {
            self.ch1.sweep_clock();
        }
        // Envelope: 64 Hz, step 7.
        if step == 7 {
            self.ch1.envelope_clock();
            self.ch2.envelope_clock();
            self.ch4.envelope_clock();
        }
    }

    /// Each channel's current DAC input, `$0`-`$F`; `$0` when the channel is off.
    pub fn channel_levels(&self) -> [u8; 4] {
        [
            self.ch1.level(),
            self.ch2.level(),
            self.ch3.level(&self.wave),
            self.ch4.level(),
        ]
    }

    /// Wave RAM is locked while CH3 is active (Pan Docs, `Audio_Registers`, `FF30-FF3F`).
    fn wave_locked(&self) -> bool {
        self.powered() && self.ch3.enabled && self.ch3.dac
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0xFF30..=0xFF3F => {
                if self.wave_locked() {
                    0xFF
                } else {
                    self.wave[(addr - 0xFF30) as usize]
                }
            }
            0xFF10..=0xFF26 => self.read_reg(addr),
            _ => 0xFF,
        }
    }

    /// Unused bits read as 1, write-only registers read `$FF`.
    fn read_reg(&self, addr: u16) -> u8 {
        let value = self.reg(addr);
        match addr {
            0xFF10 => 0x80 | (value & 0x7F),
            0xFF11 | 0xFF16 => 0x3F | (value & 0xC0),
            0xFF12 | 0xFF17 | 0xFF21 => value,
            0xFF13 | 0xFF18 | 0xFF1B | 0xFF1D | 0xFF20 => 0xFF,
            0xFF14 | 0xFF19 | 0xFF1E | 0xFF23 => 0xBF | (value & 0x40),
            0xFF1A => 0x7F | (value & 0x80),
            0xFF1C => 0x9F | (value & 0x60),
            0xFF24 | 0xFF25 => value,
            0xFF26 => 0x70 | (self.nr[0x16] & 0x80) | self.status_bits(),
            _ => 0xFF,
        }
    }

    /// `$FF26` bits 0-3: 1 while a channel is on (`Audio_Registers`, `NR52`).
    fn status_bits(&self) -> u8 {
        let on = [
            self.ch1.enabled && self.ch1.dac,
            self.ch2.enabled && self.ch2.dac,
            self.ch3.enabled && self.ch3.dac,
            self.ch4.enabled && self.ch4.dac,
        ];
        let mut bits = 0;
        for (i, audible) in on.iter().enumerate() {
            if *audible {
                bits |= 1 << i;
            }
        }
        bits
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF30..=0xFF3F => {
                if !self.wave_locked() {
                    self.wave[(addr - 0xFF30) as usize] = value;
                }
            }
            0xFF26 => self.write_nr52(value),
            0xFF10..=0xFF25 => {
                if self.powered() {
                    self.write_reg(addr, value);
                } else {
                    // Power-off clears the registers, but on monochrome models the length timers
                    // stay writable (`Audio_Registers`, `NR52`).
                    self.write_length_while_off(addr, value);
                }
            }
            _ => {}
        }
    }

    fn write_length_while_off(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF11 => self.ch1.length = 64 - (value & 0x3F) as u16,
            0xFF16 => self.ch2.length = 64 - (value & 0x3F) as u16,
            0xFF1B => self.ch3.length = 256 - value as u16,
            0xFF20 => self.ch4.length = 64 - (value & 0x3F) as u16,
            _ => {}
        }
    }

    /// `$FF26` bit 7 (`Audio_Registers`, `NR52`): clearing it powers the unit down and clears
    /// `$FF10-$FF25`; bits 0-6 are read-only.
    fn write_nr52(&mut self, value: u8) {
        let on = value & 0x80 != 0;
        if self.powered() && !on {
            self.nr = [0; 0x17];
            self.ch1 = Pulse::default();
            self.ch2 = Pulse::default();
            self.ch3 = Wave::default();
            self.ch4 = Noise::default();
        }
        self.nr[0x16] = value & 0x80;
    }

    fn write_reg(&mut self, addr: u16, value: u8) {
        self.nr[(addr - 0xFF10) as usize] = value;
        match addr {
            0xFF10 => {
                self.ch1.sweep_period = (value >> 4) & 0x07;
                self.ch1.sweep_dir_down = value & 0x08 != 0;
                self.ch1.sweep_shift = value & 0x07;
            }
            0xFF11 => {
                self.ch1.duty = value >> 6;
                self.ch1.length = 64 - (value & 0x3F) as u16;
            }
            0xFF12 => set_pulse_envelope(&mut self.ch1, value),
            0xFF13 => self.ch1.period = (self.ch1.period & 0x700) | value as u16,
            0xFF14 => {
                self.ch1.length_enable = value & 0x40 != 0;
                self.ch1.period = (self.ch1.period & 0x0FF) | (((value & 0x07) as u16) << 8);
                if value & 0x80 != 0 {
                    let initial_vol = self.nr[0x02] >> 4;
                    self.ch1.trigger(initial_vol, true);
                }
            }
            0xFF16 => {
                self.ch2.duty = value >> 6;
                self.ch2.length = 64 - (value & 0x3F) as u16;
            }
            0xFF17 => set_pulse_envelope(&mut self.ch2, value),
            0xFF18 => self.ch2.period = (self.ch2.period & 0x700) | value as u16,
            0xFF19 => {
                self.ch2.length_enable = value & 0x40 != 0;
                self.ch2.period = (self.ch2.period & 0x0FF) | (((value & 0x07) as u16) << 8);
                if value & 0x80 != 0 {
                    let initial_vol = self.nr[0x07] >> 4;
                    self.ch2.trigger(initial_vol, false);
                }
            }
            0xFF1A => {
                self.ch3.dac = value & 0x80 != 0;
                if !self.ch3.dac {
                    self.ch3.enabled = false;
                }
            }
            0xFF1B => self.ch3.length = 256 - value as u16,
            0xFF1C => self.ch3.volume_code = (value >> 5) & 0x03,
            0xFF1D => self.ch3.period = (self.ch3.period & 0x700) | value as u16,
            0xFF1E => {
                self.ch3.length_enable = value & 0x40 != 0;
                self.ch3.period = (self.ch3.period & 0x0FF) | (((value & 0x07) as u16) << 8);
                if value & 0x80 != 0 {
                    self.ch3.trigger();
                }
            }
            0xFF20 => self.ch4.length = 64 - (value & 0x3F) as u16,
            0xFF21 => {
                self.ch4.vol = value >> 4;
                self.ch4.env_dir_up = value & 0x08 != 0;
                self.ch4.env_period = value & 0x07;
                self.ch4.dac = value & 0xF8 != 0;
                if !self.ch4.dac {
                    self.ch4.enabled = false;
                }
            }
            0xFF22 => self.ch4.set_divider(value),
            0xFF23 => {
                self.ch4.length_enable = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    let initial_vol = self.nr[0x11] >> 4;
                    self.ch4.trigger(initial_vol);
                }
            }
            _ => {}
        }
    }

    /// Mix and stage one stereo frame.
    fn push_sample(&mut self) {
        if self.len + 2 > self.out.len() {
            if self.read_pos == 0 {
                // The front end has not drained: drop rather than grow.
                return;
            }
            self.out.copy_within(self.read_pos..self.len, 0);
            self.len -= self.read_pos;
            self.read_pos = 0;
        }
        let (left, right) = self.mix();
        self.out[self.len] = left;
        self.out[self.len + 1] = right;
        self.len += 2;
    }

    /// `NR51` panning and `NR50` master volume applied to the digital mix, plus the
    /// SGB sound chip's contribution when there is one.
    ///
    /// With no chip the two values are exactly what the four channels mixed to. With one, the chip's
    /// 32 kHz stereo is resampled to this output rate and added at [`SGB_VOLUME`], and the sum is
    /// clamped: two full-scale sources would otherwise wrap, and a wrapped sample is a click, not a
    /// loud note.
    fn mix(&mut self) -> (f32, f32) {
        let pan = self.reg(0xFF25);
        let master = self.reg(0xFF24);
        let left_gain = ((master >> 4) & 0x07) as f32 + 1.0;
        let right_gain = (master & 0x07) as f32 + 1.0;
        let levels = self.channel_levels();
        let centers = [FULL_SCALE, FULL_SCALE, self.ch3.center(), FULL_SCALE];
        let dac_on = [
            self.ch1.enabled && self.ch1.dac,
            self.ch2.enabled && self.ch2.dac,
            self.ch3.enabled && self.ch3.dac,
            self.ch4.enabled && self.ch4.dac,
        ];
        let mut left = 0.0;
        let mut right = 0.0;
        for i in 0..4 {
            if (self.channel_mask >> i) & 1 == 0 || !dac_on[i] {
                continue;
            }
            let offset = levels[i] as f32 - centers[i];
            if (pan >> (4 + i)) & 1 != 0 {
                left += offset;
            }
            if (pan >> i) & 1 != 0 {
                right += offset;
            }
        }
        let mut out = (left * left_gain / MIX_SCALE, right * right_gain / MIX_SCALE);
        if let Some([sgb_left, sgb_right]) = self.sgb_frame() {
            out.0 = (out.0 + sgb_left * SGB_VOLUME).clamp(-1.0, 1.0);
            out.1 = (out.1 + sgb_right * SGB_VOLUME).clamp(-1.0, 1.0);
        }
        out
    }

    /// One output frame of the SGB chip's audio, linearly interpolated from its 32 kHz stream.
    ///
    /// The phase advances by the chip's rate against this output rate as an exact integer
    /// numerator, so a 44 100, 48 000 or 96 000 Hz output is anchored to the same 32 kHz clock with
    /// no accumulated drift. A chip that has run dry holds its last sample — the chip is slower than
    /// every output rate here, so that bridges a single sample rather than filling with silence.
    /// `None` when there is no chip, which is the whole of this file's old behaviour.
    fn sgb_frame(&mut self) -> Option<[f32; 2]> {
        let chip = self.sgb.as_mut()?;
        let frac = self.sgb_phase as f32 / self.sample_rate.max(1) as f32;
        let mut frame = [0.0; 2];
        for channel in 0..2 {
            frame[channel] =
                self.sgb_cur[channel] + (self.sgb_next[channel] - self.sgb_cur[channel]) * frac;
        }
        self.sgb_phase += DSP_SAMPLE_HZ;
        while self.sgb_phase >= self.sample_rate.max(1) {
            self.sgb_phase -= self.sample_rate.max(1);
            self.sgb_cur = self.sgb_next;
            let mut next = [0.0; 2];
            if chip.read_samples(&mut next) == 1 {
                self.sgb_next = next;
            }
        }
        Some(frame)
    }

    /// Interleaved stereo, up to `out.len()/2` frames.
    pub fn read_samples(&mut self, out: &mut [f32]) -> usize {
        let available = (self.len - self.read_pos) / 2;
        let frames = available.min(out.len() / 2);
        let samples = frames * 2;
        out[..samples].copy_from_slice(&self.out[self.read_pos..self.read_pos + samples]);
        self.read_pos += samples;
        if self.read_pos >= self.len {
            self.read_pos = 0;
            self.len = 0;
        }
        frames
    }

    pub fn set_sample_rate(&mut self, hz: u32) {
        self.sample_rate = hz;
    }

    /// 0 = all four silent, `$0F` = all audible.
    pub fn set_channel_mask(&mut self, mask: u8) {
        self.channel_mask = mask & 0x0F;
    }

    /// Registers, wave RAM, the four channels, the frame sequencer and the resampler,
    /// and the SGB sound chip with its ports and clock remainder when there is one.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.nr);
        w.bytes(&self.wave);
        self.ch1.save_state(w);
        self.ch2.save_state(w);
        self.ch3.save_state(w);
        self.ch4.save_state(w);
        w.u8(self.div_apu);
        w.u32(self.sample_rate);
        w.f64(self.acc);
        w.bool(self.sgb.is_some());
        if let Some(sgb) = &self.sgb {
            sgb.save_state(w);
        }
        // The resampler's position between the chip's frames, so a resumed state picks up the audio
        // exactly where it left off rather than fading in from its own window.
        w.u32(self.sgb_phase);
        for value in [
            self.sgb_cur[0],
            self.sgb_cur[1],
            self.sgb_next[0],
            self.sgb_next[1],
        ] {
            w.u32(value.to_bits());
        }
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.nr)?;
        r.bytes_into(&mut self.wave)?;
        self.ch1.load_state(r)?;
        self.ch2.load_state(r)?;
        self.ch3.load_state(r)?;
        self.ch4.load_state(r)?;
        self.div_apu = r.u8()?;
        self.sample_rate = r.u32()?;
        self.acc = r.f64()?;
        self.sgb = if r.bool()? {
            let mut sound = SgbSound::new();
            sound.load_state(r)?;
            Some(Box::new(sound))
        } else {
            None
        };
        self.sgb_phase = r.u32()?;
        for sample in self.sgb_cur.iter_mut() {
            *sample = f32::from_bits(r.u32()?);
        }
        for sample in self.sgb_next.iter_mut() {
            *sample = f32::from_bits(r.u32()?);
        }
        Ok(())
    }
}

/// `NRx2`: initial volume, envelope direction and pace, DAC enable (`Audio_Registers`, `NR12`).
fn set_pulse_envelope(ch: &mut Pulse, value: u8) {
    ch.vol = value >> 4;
    ch.env_dir_up = value & 0x08 != 0;
    ch.env_period = value & 0x07;
    ch.dac = value & 0xF8 != 0;
    if !ch.dac {
        ch.enabled = false;
    }
}

impl Default for Apu {
    fn default() -> Self {
        Apu::new()
    }
}
