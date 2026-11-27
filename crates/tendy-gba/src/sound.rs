//! The Game Boy Advance's sound unit: two DMA-fed sample channels, four PSG voices, the mixer and
//! the resampler into the front end's ring.
//!
//! GBATEK, *GBA Sound Controller*, is the whole of the hardware:
//!
//! - **Channel A and B** are two 8-bit signed sample streams, each behind a 32-byte FIFO at
//!   `$040000A0`/`$040000A4`. The selected timer's overflow moves one byte from the FIFO to the
//!   sound circuit, and when the FIFO holds only 16 bytes left the hardware asks DMA1 or DMA2 for
//!   sixteen more ("If Timer overflows then move 8bit data from FIFO to sound circuit. If FIFO
//!   contains only 4 x 32bits (16 bytes) then request more data per DMA").
//! - **Channels 1-4** are the PSG voices the hardware kept from the Color: two squares (channel 1
//!   with sweep), a wave channel with two banks of 32 four-bit digits, and a noise channel. Their
//!   register layout and shape maths are the Color's, at four times the clock: GBATEK gives the
//!   tone as `131072/(2048-n) Hz`, the wave's digit rate as `2097152/(2048-n) Hz` and the noise as
//!   `524288 / r / 2^(s+1) Hz`, which fixes the master-clock divisors used here.
//! - **The mixer** adds what `SOUNDCNT_L` enables and volumes, what `SOUNDCNT_H` selects for the
//!   direct channels, and `SOUNDBIAS`'s sampling cycle, then clips.
//!
//! **Why this file exists beside `src/apu/`.** The Game Boy's APU is the Color's sound unit and its
//! shape maths is the same mathematics, but the two machines must not share timing state — the rule
//! here is that a shared helper is how DMG accuracy regresses. The duty table,
//! the envelope and the length timer are therefore *re-implemented* here in the GBA's own units
//! (master-clock cycles rather than Game Boy dots), with `src/apu/` read-only beside them.
//!
//! **One ring, two producers.** The output is staged in [`Sound::out`] exactly as the Game Boy's
//! APU stages its own, and [`Sound::read_samples`] hands a frame's worth to whichever `System`
//! adapter asks; `src/audio.rs`'s ring is unchanged and knows nothing about consoles.
//!
//! **Where the state lives.** The sound registers are storage, but several of them change what a
//! read returns (`SOUNDCNT_X`'s channel flags) and several are write-only (the FIFOs), so [`Sound`]
//! owns the whole `$060-$0A7` block the way [`crate::timer::Timers`] owns the timer registers,
//! and carries it in the save state.

use std::collections::VecDeque;

use crate::CLOCK_HZ;
use tendy_gb::state::{StateError, StateReader, StateWriter};

/// First sound register in the I/O page: `SOUND1CNT_L` at `$04000060`.
pub const REG_BASE: usize = 0x060;
/// One past the last sound register (`FIFO_B_H` ends at `$040000A8`).
pub const REG_END: usize = 0x0A8;

/// `SOUNDCNT_L`: PSG master volume and left/right enables (`NR50`, `NR51`).
pub const SOUNDCNT_L: usize = 0x080;
/// `SOUNDCNT_H`: the direct channels' mixing, volume and timer select.
pub const SOUNDCNT_H: usize = 0x082;
/// `SOUNDCNT_X`: the master enable and the four PSG "channel on" flags (`NR52`).
pub const SOUNDCNT_X: usize = 0x084;
/// `SOUNDBIAS`: the PWM bias and the sampling cycle.
pub const SOUNDBIAS: usize = 0x088;
/// `WAVE_RAM0_L`: the first byte of channel 3's wave pattern RAM.
pub const WAVE_RAM: usize = 0x090;
/// `FIFO_A_L`: the first byte of channel A's FIFO.
pub const FIFO_A: usize = 0x0A0;
/// `FIFO_B_L`: the first byte of channel B's FIFO.
pub const FIFO_B: usize = 0x0A4;

/// Bits of `SOUNDCNT_H` (GBATEK, *4000082h - SOUNDCNT_H*).
pub const PSG_VOLUME_MASK: u16 = 0b11;
/// Bit 2: direct sound A at 100 % rather than 50 %.
pub const FIFO_A_VOLUME: u16 = 1 << 2;
/// Bit 3: direct sound B at 100 % rather than 50 %.
pub const FIFO_B_VOLUME: u16 = 1 << 3;
/// Bit 8: direct sound A to the right output.
pub const FIFO_A_RIGHT: u16 = 1 << 8;
/// Bit 9: direct sound A to the left output.
pub const FIFO_A_LEFT: u16 = 1 << 9;
/// Bit 10: direct sound A's timer is timer 1 rather than timer 0.
pub const FIFO_A_TIMER: u16 = 1 << 10;
/// Bit 11: reset direct sound A's FIFO.
pub const FIFO_A_RESET: u16 = 1 << 11;
/// Bit 12: direct sound B to the right output.
pub const FIFO_B_RIGHT: u16 = 1 << 12;
/// Bit 13: direct sound B to the left output.
pub const FIFO_B_LEFT: u16 = 1 << 13;
/// Bit 14: direct sound B's timer is timer 1 rather than timer 0.
pub const FIFO_B_TIMER: u16 = 1 << 14;
/// Bit 15: reset direct sound B's FIFO.
pub const FIFO_B_RESET: u16 = 1 << 15;

/// `SOUNDCNT_X` bit 7: the PSG/FIFO master enable.
pub const MASTER_ENABLE: u16 = 1 << 7;

/// Bytes a FIFO holds ("the capacity of the FIFO is 8 x 32bit (32 bytes)").
pub const FIFO_BYTES: usize = 32;
/// The level at which the hardware asks DMA for sixteen more bytes.
pub const FIFO_REFILL_LEVEL: usize = 16;

/// The default physical output rate: `SOUNDBIAS`'s sampling cycle `0` is 32.768 kHz.
pub const OUTPUT_HZ: u32 = 32_768;

/// Staging buffer: 16 384 f32 = 8 192 stereo frames, about 186 ms at 44.1 kHz. The front end drains
/// one frame at a time; the slack is for a test that runs a handful of frames between reads.
const OUT_SAMPLES: usize = 16_384;

/// The master-clock cycles a timer of `period` accumulates per sample. GBATEK's initialisation
/// writes `0xFFFF - round(2^24 / playback frequency)`, so a timer period of `p` master cycles is
/// `2^24 / p` samples a second — the clock divided by the period.
pub fn sample_rate_from_period(period: u32) -> f64 {
    f64::from(CLOCK_HZ) / f64::from(period.max(1))
}

/// One direct-sound FIFO: the bytes waiting, the sample the DAC is holding, and the cycles towards
/// the next pop.
///
/// GBATEK's playback pseudo-procedure: on every timer overflow one byte leaves the FIFO and reaches
/// the sound circuit. **An empty FIFO holds its last sample** rather than clicking to zero — the
/// hardware keeps outputting the last value shifted out, and the report's underrun test pins it.
#[derive(Clone, Debug, Default)]
pub struct Fifo {
    /// The bytes not yet played, oldest first.
    pub data: VecDeque<i8>,
    /// The sample the DAC is holding. Signed 8-bit, as the FIFO delivers it.
    pub current: i8,
    /// Master cycles accumulated towards the next timer overflow.
    pub sub: u32,
    /// A pop has left the FIFO at or below the refill level since the last DMA ran.
    ///
    /// GBATEK's playback procedure checks the level *after* moving a byte to the sound circuit
    /// ("If Timer overflows then move 8bit data from FIFO to sound circuit. If FIFO contains only 4
    /// x 32bits (16 bytes) then request more data per DMA"), so the request is a latch set by a
    /// pop and consumed by one transfer — not a level that would fire again the moment the 16-byte
    /// block landed at exactly the threshold.
    pub wants: bool,
}

impl Fifo {
    /// A store to one of the FIFO's four write-only bytes. A full FIFO drops the sample, which is
    /// what the hardware's fixed 32-byte depth does.
    pub fn push(&mut self, byte: u8) {
        if self.data.len() < FIFO_BYTES {
            self.data.push_back(byte as i8);
        }
    }

    /// One timer overflow: the oldest byte becomes the DAC's sample, or the DAC keeps holding what
    /// it had when the FIFO is empty. A pop that leaves the FIFO at the refill level raises the
    /// DMA request.
    pub fn pop(&mut self) {
        if let Some(sample) = self.data.pop_front() {
            self.current = sample;
        }
        if self.data.len() <= FIFO_REFILL_LEVEL {
            self.wants = true;
        }
    }

    /// True while a pop is waiting for the DMA block it asked for.
    pub fn wants_refill(&self) -> bool {
        self.wants
    }

    /// Take the request: one transfer answers one pop's worth of request, the way GBATEK's
    /// procedure issues one request per timer overflow.
    pub fn take_refill(&mut self) -> bool {
        let wants = self.wants;
        self.wants = false;
        wants
    }

    /// Forgets every queued byte. `SOUNDCNT_H`'s reset bit does this; the DAC's held sample is
    /// cleared too, or a reset would keep playing the last byte.
    pub fn clear(&mut self) {
        self.data.clear();
        self.current = 0;
        self.sub = 0;
        self.wants = false;
    }
}

/// The eight duty steps, `SOUND1CNT_H`/`SOUND2CNT_L` bits 6-7. The table is the Color's waveform
/// (12.5 %, 25 %, 50 %, 75 %), re-implemented from `src/apu/channel.rs` rather than imported: the
/// two machines keep their own sound units.
const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

/// Length ticks at 256 Hz: GBATEK's "units of (64-n)/256s".
const LENGTH_PERIOD: u32 = CLOCK_HZ / 256;
/// Sweep ticks at 128 Hz: GBATEK's "units of 7.8ms".
const SWEEP_PERIOD: u32 = CLOCK_HZ / 128;
/// Envelope ticks at 64 Hz: GBATEK's "units of n/64s".
const ENVELOPE_PERIOD: u32 = CLOCK_HZ / 64;

/// Step a master-cycle countdown by `cycles`; returns how many times it wrapped.
fn count_down(timer: &mut u32, reload: u32, cycles: u32) -> u32 {
    let reload = reload.max(1);
    if *timer == 0 {
        *timer = reload;
    }
    let mut remaining = cycles;
    let mut wraps = 0;
    loop {
        let left = *timer;
        if remaining < left {
            *timer = left - remaining;
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

/// A tone channel's frequency from its 11-bit period: GBATEK's `131072/(2048-n) Hz`.
pub fn square_hz(period: u16) -> f64 {
    131_072.0 / f64::from(2048 - u32::from(period & 0x7FF))
}

/// The wave channel's digit rate: GBATEK's `2097152/(2048-n) Hz`.
pub fn wave_hz(period: u16) -> f64 {
    2_097_152.0 / f64::from(2048 - u32::from(period & 0x7FF))
}

/// The noise channel's clock: GBATEK's `524288 / r / 2^(s+1) Hz`, with `r` 0.5 when the divider
/// code is zero.
pub fn noise_hz(divisor_code: u8, shift: u8) -> f64 {
    let r = if divisor_code == 0 {
        0.5
    } else {
        f64::from(divisor_code)
    };
    524_288.0 / r / 2f64.powi(i32::from(shift) + 1)
}

/// Square channel 1 (with sweep) and channel 2, in master-clock cycles.
///
/// GBATEK gives the tone as `131072/(2048-n) Hz`; at the 16.777216 MHz master clock that is a duty
/// step every `16 * (2048-n)` cycles, eight steps to the cycle (`16.777216e6 / (16*8*2048)` =
/// 64 Hz at n=0, i.e. `131072/2048`).
#[derive(Clone, Debug, Default)]
pub struct Square {
    pub enabled: bool,
    pub dac: bool,
    pub duty: u8,
    pub duty_pos: u8,
    pub period: u16,
    pub timer: u32,
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

impl Square {
    /// Master cycles per duty step.
    fn reload(&self) -> u32 {
        16 * (0x800 - u32::from(self.period & 0x7FF))
    }

    /// Advance the duty divider.
    pub fn step(&mut self, cycles: u32) {
        let reload = self.reload();
        let wraps = count_down(&mut self.timer, reload, cycles);
        self.duty_pos = ((u32::from(self.duty_pos) + wraps) & 7) as u8;
    }

    /// Digital output: 0 or the current volume (`DUTY` gives the waveform).
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

    /// One 256 Hz length tick: the channel stops when the length expires.
    pub fn length_clock(&mut self) {
        if !self.length_enable || self.length == 0 {
            return;
        }
        self.length -= 1;
        if self.length == 0 {
            self.enabled = false;
        }
    }

    /// One 64 Hz envelope tick.
    pub fn envelope_clock(&mut self) {
        if self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
        }
        if self.env_timer != 0 {
            return;
        }
        self.env_timer = self.env_period;
        if self.env_dir_up {
            if self.vol < 15 {
                self.vol += 1;
            }
        } else if self.vol > 0 {
            self.vol -= 1;
        }
    }

    /// The period the sweep produces from `shadow`: `X +/- X/2^n` (GBATEK, *SOUND1CNT_L*).
    fn sweep_calc(&self, shadow: u16) -> u16 {
        let delta = shadow >> (self.sweep_shift & 7);
        if self.sweep_dir_down {
            shadow.wrapping_sub(delta)
        } else {
            shadow.wrapping_add(delta)
        }
    }

    /// One 128 Hz sweep tick. Sweep is disabled by a time of zero (GBATEK: "Sweep is disabled by
    /// setting Sweep Time to zero").
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

    /// `SOUNDxCNT_X` bit 15. `sweep` is true for channel 1 only.
    pub fn trigger(&mut self, sweep: bool) {
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = self.reload();
        self.env_timer = self.env_period;
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

    /// Walk the state's fields in a fixed order.
    fn encode(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dac);
        w.u8(self.duty);
        w.u8(self.duty_pos);
        w.u16(self.period);
        w.u32(self.timer);
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

    fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dac = r.bool()?;
        self.duty = r.u8()?;
        self.duty_pos = r.u8()?;
        self.period = r.u16()?;
        self.timer = r.u32()?;
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

/// Wave channel 3: two banks of 32 four-bit digits, selectable independently of the one being
/// played (GBATEK: "The currently selected Bank Number (Bit 6) will be played back, while
/// reading/writing to/from wave RAM will address the other (not selected) bank").
#[derive(Clone, Debug, Default)]
pub struct Wave {
    pub enabled: bool,
    /// `SOUND3CNT_L` bit 5: one bank (32 digits) or two (64).
    pub dimension: bool,
    /// `SOUND3CNT_L` bit 6: the bank being played.
    pub bank: u8,
    pub period: u16,
    pub timer: u32,
    pub length: u16,
    pub length_enable: bool,
    /// 0 mute, 1 100 %, 2 50 %, 3 25 %.
    pub volume_code: u8,
    /// `SOUND3CNT_H` bit 15: output at 75 % regardless of `volume_code`.
    pub force75: bool,
    /// Digit index, 0-31 in one-bank mode and 0-63 across both.
    pub pos: u8,
}

impl Wave {
    /// Master cycles per digit: GBATEK's `2097152/(2048-n) Hz` digit rate is `8 * (2048-n)` cycles.
    fn reload(&self) -> u32 {
        8 * (0x800 - u32::from(self.period & 0x7FF))
    }

    /// How many digits one pass plays.
    pub fn digits(&self) -> u8 {
        if self.dimension { 64 } else { 32 }
    }

    /// Advance the digit divider.
    pub fn step(&mut self, cycles: u32) {
        let reload = self.reload();
        let wraps = count_down(&mut self.timer, reload, cycles);
        self.pos = ((u32::from(self.pos) + wraps) % u32::from(self.digits())) as u8;
    }

    /// The wave-RAM byte and nibble the current digit reads: the selected bank first, then the
    /// other bank when both are played.
    fn digit(&self, wave: &[u8; 32]) -> u8 {
        let index = if self.dimension && self.pos >= 32 {
            // After the selected bank, playback continues through the other one.
            let within = usize::from(self.pos - 32);
            let bank = usize::from(1 - (self.bank & 1));
            bank * 16 + within / 2
        } else {
            usize::from(self.bank & 1) * 16 + usize::from(self.pos & 31) / 2
        };
        let byte = wave[index.min(31)];
        if self.pos & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        }
    }

    /// Digital output: the digit shifted by the volume code (GBATEK's 100/50/25 %), or forced to
    /// 75 %.
    pub fn level(&self, wave: &[u8; 32]) -> u8 {
        if !self.enabled {
            return 0;
        }
        if self.volume_code == 0 && !self.force75 {
            return 0;
        }
        let digit = self.digit(wave);
        if self.force75 {
            (u16::from(digit) * 3 / 4) as u8
        } else {
            digit >> (self.volume_code - 1)
        }
    }

    /// The middle of the channel's output range, so a flat waveform mixes to zero.
    pub fn center(&self) -> f32 {
        if self.force75 {
            5.625
        } else if self.volume_code == 0 {
            0.0
        } else {
            (15u32 >> (self.volume_code - 1)) as f32 / 2.0
        }
    }

    pub fn length_clock(&mut self) {
        if !self.length_enable || self.length == 0 {
            return;
        }
        self.length -= 1;
        if self.length == 0 {
            self.enabled = false;
        }
    }

    /// `SOUND3CNT_X` bit 15: the digit index restarts.
    pub fn trigger(&mut self) {
        if self.length == 0 {
            self.length = 256;
        }
        self.timer = self.reload();
        self.pos = 0;
    }

    fn encode(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dimension);
        w.u8(self.bank);
        w.u16(self.period);
        w.u32(self.timer);
        w.u16(self.length);
        w.bool(self.length_enable);
        w.u8(self.volume_code);
        w.bool(self.force75);
        w.u8(self.pos);
    }

    fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dimension = r.bool()?;
        self.bank = r.u8()?;
        self.period = r.u16()?;
        self.timer = r.u32()?;
        self.length = r.u16()?;
        self.length_enable = r.bool()?;
        self.volume_code = r.u8()?;
        self.force75 = r.bool()?;
        self.pos = r.u8()?;
        Ok(())
    }
}

/// Noise channel 4: GBATEK's 15-bit (or 7-bit) polynomial counter.
#[derive(Clone, Debug, Default)]
pub struct Noise {
    pub enabled: bool,
    pub dac: bool,
    pub timer: u32,
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
    /// The level the last shift produced: GBATEK's "IF carry THEN Out=HIGH".
    pub high: bool,
}

impl Noise {
    /// Master cycles per shift: GBATEK's `524288 / r / 2^(s+1) Hz`, with `r` 0.5 at code zero.
    fn reload(&self) -> u32 {
        if self.clock_shift >= 14 {
            return u32::MAX;
        }
        let shift = u32::from(self.clock_shift);
        if self.divisor_code == 0 {
            32 << shift
        } else {
            (64 * u32::from(self.divisor_code)) << shift
        }
    }

    /// Advance the shift clock by `cycles`.
    pub fn step(&mut self, cycles: u32) {
        if self.clock_shift >= 14 {
            return;
        }
        let reload = self.reload();
        let wraps = count_down(&mut self.timer, reload, cycles);
        for _ in 0..wraps {
            self.clock_lfsr();
        }
    }

    /// One polynomial-counter shift (GBATEK: "X=X SHR 1, IF carry THEN Out=HIGH, X=X XOR 60h ELSE
    /// Out=LOW"; the 15-bit mask XORs `6000h` instead).
    fn clock_lfsr(&mut self) {
        self.high = self.lfsr & 1 != 0;
        let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
        self.lfsr = (self.lfsr >> 1) | (bit << 14);
        if self.width_mode {
            self.lfsr = (self.lfsr & !0x40) | (bit << 6);
        }
    }

    pub fn level(&self) -> u8 {
        if !self.enabled || !self.dac {
            return 0;
        }
        if self.high { self.vol & 0x0F } else { 0 }
    }

    pub fn length_clock(&mut self) {
        if !self.length_enable || self.length == 0 {
            return;
        }
        self.length -= 1;
        if self.length == 0 {
            self.enabled = false;
        }
    }

    pub fn envelope_clock(&mut self) {
        if self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
        }
        if self.env_timer != 0 {
            return;
        }
        self.env_timer = self.env_period;
        if self.env_dir_up {
            if self.vol < 15 {
                self.vol += 1;
            }
        } else if self.vol > 0 {
            self.vol -= 1;
        }
    }

    /// `SOUND4CNT_H` bit 15: the counters reload and the shift register restarts at `4000h`
    /// (15-bit) or `40h` (7-bit), per GBATEK's *Noise Random Generator*.
    pub fn trigger(&mut self, initial_vol: u8) {
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = self.reload();
        self.env_timer = self.env_period;
        self.vol = initial_vol;
        self.lfsr = if self.width_mode { 0x0040 } else { 0x4000 };
        self.high = false;
        self.enabled = self.dac;
    }

    fn encode(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.dac);
        w.u32(self.timer);
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
        w.bool(self.high);
    }

    fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.dac = r.bool()?;
        self.timer = r.u32()?;
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
        self.high = r.bool()?;
        Ok(())
    }
}

/// The frame-independent clock dividers for the PSG's three sequencer rates.
#[derive(Clone, Debug, Default)]
pub struct Sequencer {
    pub length: u32,
    pub sweep: u32,
    pub envelope: u32,
}

impl Sequencer {
    fn tick(&mut self, cycles: u32) -> (u32, u32, u32) {
        (
            count_down(&mut self.length, LENGTH_PERIOD, cycles),
            count_down(&mut self.sweep, SWEEP_PERIOD, cycles),
            count_down(&mut self.envelope, ENVELOPE_PERIOD, cycles),
        )
    }
}

/// The sound unit.
#[derive(Clone, Debug)]
pub struct Sound {
    /// `$060-$0A7` as written: the sound registers and, in step one, a byte for every wave-RAM
    /// address. Reads of the derived and write-only registers are answered by [`Sound::read8`].
    pub regs: [u8; REG_END - REG_BASE],
    /// Channel 3's two banks of wave pattern RAM, 32 four-bit digits each.
    pub wave: [u8; 32],
    /// PSG voices 1-4: two squares (the first with sweep), the wave channel and the noise channel.
    pub ch1: Square,
    pub ch2: Square,
    pub ch3: Wave,
    pub ch4: Noise,
    /// The 256/128/64 Hz length, sweep and envelope clocks.
    pub seq: Sequencer,
    /// The two direct-sound FIFOs, A then B.
    pub fifo: [Fifo; 2],
    /// The timer period each FIFO's pops are derived from, `None` while its timer is stopped. A
    /// change restarts that FIFO's accumulator, which is what a start edge does to the hardware.
    pub period: [Option<u32>; 2],
    /// The rate the front end asked for: what `drain_audio` produces.
    pub sample_rate: u32,
    /// Master-clock cycles accumulated towards the next *internal* mix sample.
    mix_acc: f64,
    /// Internal samples not yet handed to the output: the fractional accumulator that resamples the
    /// SOUNDBIAS output rate to the ring's.
    out_acc: f64,
    /// Interleaved L,R staging, [`OUT_SAMPLES`] f32, allocated once.
    pub out: Vec<f32>,
    pub read_pos: usize,
    pub len: usize,
    /// The four CHANNELS rows, as one mask: bit 0 PSG 1 .. bit 3 PSG 4.
    pub channel_mask: u8,
    /// How many timer overflows have moved a byte from each FIFO to its DAC. A diagnostic, not
    /// hardware state: it is what proves a title's direct sound is timer-driven, and it does not
    /// travel in the save state.
    pub pops: [u64; 2],
}

impl Sound {
    /// Post-boot: `SOUNDBIAS` at its published `$0200`, every other register zero (the boot
    /// sequence leaves the sound unit off until a game enables it).
    pub fn new() -> Sound {
        let mut regs = [0u8; REG_END - REG_BASE];
        regs[SOUNDBIAS - REG_BASE] = 0x00;
        regs[SOUNDBIAS - REG_BASE + 1] = 0x02;
        Sound {
            regs,
            wave: [0; 32],
            ch1: Square::default(),
            ch2: Square::default(),
            ch3: Wave::default(),
            ch4: Noise::default(),
            seq: Sequencer::default(),
            fifo: [Fifo::default(), Fifo::default()],
            period: [None, None],
            sample_rate: 44_100,
            mix_acc: 0.0,
            out_acc: 0.0,
            out: vec![0.0; OUT_SAMPLES],
            read_pos: 0,
            len: 0,
            channel_mask: 0x0F,
            pops: [0; 2],
        }
    }

    /// True for an I/O offset that belongs to the sound unit.
    pub fn owns(offset: usize) -> bool {
        (REG_BASE..REG_END).contains(&offset)
    }

    /// One byte of the sound block. The FIFOs are write-only and read as zero; `SOUNDCNT_X`'s
    /// channel flags are the live state, not storage (GBATEK: bits 0-3 are "automatically set when
    /// starting sound output, and automatically cleared when a sound ends").
    pub fn read8(&self, offset: usize) -> u8 {
        match offset {
            FIFO_A..=0x0A7 => 0,
            WAVE_RAM..=0x09F => self.wave[self.wave_index(offset)],
            SOUNDCNT_X => {
                let mut value = self.regs[SOUNDCNT_X - REG_BASE] & 0x80;
                for (bit, on) in self.psg_on().iter().enumerate() {
                    if *on {
                        value |= 1 << bit;
                    }
                }
                value
            }
            _ => self.regs[offset - REG_BASE],
        }
    }

    /// One store to the sound block.
    ///
    /// Most registers are storage this step; the two FIFOs are the exception: a store pushes one
    /// sample byte, in address order, so a 32-bit DMA write delivers its four bytes as the four
    /// samples the hardware plays.
    pub fn write8(&mut self, offset: usize, value: u8) {
        match offset {
            FIFO_A..=0x0A7 => {
                let index = usize::from(offset >= FIFO_B);
                self.fifo[index].push(value);
            }
            x if x == SOUNDCNT_H || x == SOUNDCNT_H + 1 => {
                self.regs[x - REG_BASE] = value;
                // Bits 11 and 15 are the FIFO resets. They are write-to-reset and self-clearing, so
                // a subsequent control write cannot clear a FIFO the game has since refilled.
                let mut control = self.reg_u16(SOUNDCNT_H);
                if control & FIFO_A_RESET != 0 {
                    self.fifo[0].clear();
                    control &= !FIFO_A_RESET;
                }
                if control & FIFO_B_RESET != 0 {
                    self.fifo[1].clear();
                    control &= !FIFO_B_RESET;
                }
                let low = SOUNDCNT_H - REG_BASE;
                self.regs[low..low + 2].copy_from_slice(&control.to_le_bytes());
            }
            WAVE_RAM..=0x09F => {
                let index = self.wave_index(offset);
                self.wave[index] = value;
            }
            SOUNDCNT_X => {
                // Bit 7 is the only writable bit; bits 0-6 are the read-only status flags.
                let was = self.master_enabled();
                self.regs[SOUNDCNT_X - REG_BASE] = value & 0x80;
                // GBATEK: "While Bit 7 is cleared, both PSG and FIFO sounds are disabled, and all
                // PSG registers at 4000060h..4000081h are reset to zero (and must be
                // re-initialized after re-enabling sound)."
                if was && !self.master_enabled() {
                    let end = SOUNDCNT_L - REG_BASE + 2;
                    self.regs[..end].fill(0);
                    self.ch1 = Square::default();
                    self.ch2 = Square::default();
                    self.ch3 = Wave::default();
                    self.ch4 = Noise::default();
                }
            }
            _ => {
                self.regs[offset - REG_BASE] = value;
                self.sync_register(offset);
            }
        }
    }

    /// Bring the channel state in line with the register pair a store just changed.
    ///
    /// Every field is recomputed from the pair's current word, because the two halves of a register
    /// arrive as separate byte stores; the trigger bit is only acted on by the high byte's store, so
    /// writing a frequency's low byte cannot restart the sound.
    fn sync_register(&mut self, offset: usize) {
        let word = |sound: &Sound, low: usize| {
            let at = low - REG_BASE;
            u16::from_le_bytes([sound.regs[at], sound.regs[at + 1]])
        };
        match offset {
            0x60 | 0x61 => {
                let value = word(self, 0x60);
                self.ch1.sweep_shift = (value & 0x07) as u8;
                self.ch1.sweep_dir_down = value & 0x08 != 0;
                self.ch1.sweep_period = ((value >> 4) & 0x07) as u8;
            }
            0x62 | 0x63 => {
                let value = word(self, 0x62);
                set_square_duty(&mut self.ch1, value);
                set_square_envelope(&mut self.ch1, value);
            }
            0x64 | 0x65 => {
                let value = word(self, 0x64);
                self.ch1.period = value & 0x7FF;
                self.ch1.length_enable = value & 0x4000 != 0;
                if offset == 0x65 && value & 0x8000 != 0 {
                    self.ch1.trigger(true);
                }
            }
            0x68 | 0x69 => {
                let value = word(self, 0x68);
                set_square_duty(&mut self.ch2, value);
                set_square_envelope(&mut self.ch2, value);
            }
            0x6C | 0x6D => {
                let value = word(self, 0x6C);
                self.ch2.period = value & 0x7FF;
                self.ch2.length_enable = value & 0x4000 != 0;
                if offset == 0x6D && value & 0x8000 != 0 {
                    self.ch2.trigger(false);
                }
            }
            0x70 | 0x71 => {
                let value = word(self, 0x70);
                self.ch3.dimension = value & 0x20 != 0;
                self.ch3.bank = ((value >> 6) & 1) as u8;
                self.ch3.enabled = value & 0x80 != 0;
            }
            0x72 | 0x73 => {
                let value = word(self, 0x72);
                self.ch3.length = 256 - (value & 0xFF);
                self.ch3.volume_code = ((value >> 13) & 0x03) as u8;
                self.ch3.force75 = value & 0x8000 != 0;
            }
            0x74 | 0x75 => {
                let value = word(self, 0x74);
                self.ch3.period = value & 0x7FF;
                self.ch3.length_enable = value & 0x4000 != 0;
                if offset == 0x75 && value & 0x8000 != 0 {
                    self.ch3.trigger();
                }
            }
            0x78 | 0x79 => {
                let value = word(self, 0x78);
                self.ch4.length = 64 - (value & 0x3F);
                self.ch4.env_period = ((value >> 8) & 0x07) as u8;
                self.ch4.env_dir_up = value & 0x0800 != 0;
                self.ch4.vol = ((value >> 12) & 0x0F) as u8;
                let dac_gate = value & 0xF800 != 0;
                self.ch4.dac = dac_gate;
                if !dac_gate {
                    self.ch4.enabled = false;
                }
            }
            0x7C | 0x7D => {
                let value = word(self, 0x7C);
                self.ch4.divisor_code = (value & 0x07) as u8;
                self.ch4.width_mode = value & 0x08 != 0;
                self.ch4.clock_shift = ((value >> 4) & 0x0F) as u8;
                self.ch4.length_enable = value & 0x4000 != 0;
                if offset == 0x7D && value & 0x8000 != 0 {
                    self.ch4.trigger(self.ch4.vol);
                }
            }
            _ => {}
        }
    }

    /// `SOUNDCNT_H` as a word.
    pub fn control_h(&self) -> u16 {
        self.reg_u16(SOUNDCNT_H)
    }

    /// `SOUNDCNT_X` as a word.
    pub fn control_x(&self) -> u16 {
        self.reg_u16(SOUNDCNT_X)
    }

    fn reg_u16(&self, offset: usize) -> u16 {
        let at = offset - REG_BASE;
        u16::from_le_bytes([self.regs[at], self.regs[at + 1]])
    }

    /// True while `SOUNDCNT_X` bit 7 is set: the master PSG/FIFO enable.
    fn master_enabled(&self) -> bool {
        self.control_x() & MASTER_ENABLE != 0
    }

    /// Which PSG voices are sounding, for `SOUNDCNT_X`'s read-only flags (GBATEK: bits 0-3 are
    /// "automatically set when starting sound output, and ... cleared when a sound ends").
    fn psg_on(&self) -> [bool; 4] {
        [
            self.ch1.enabled && self.ch1.dac,
            self.ch2.enabled && self.ch2.dac,
            self.ch3.enabled,
            self.ch4.enabled && self.ch4.dac,
        ]
    }

    /// The wave-RAM address a CPU access reaches: GBATEK, *SOUND3CNT_L*: "The currently selected
    /// Bank Number (Bit 6) will be played back, while reading/writing to/from wave RAM will address
    /// the other (not selected) bank."
    fn wave_index(&self, offset: usize) -> usize {
        let within = offset - WAVE_RAM;
        let bank = 1 - (self.ch3.bank & 1);
        usize::from(bank) * 16 + within
    }

    /// Which FIFOs have asked for a refill: bit 0 channel A, bit 1 channel B. A peek; the bus clears
    /// each bit as it answers it with [`Fifo::take_refill`].
    pub fn refill_request(&self) -> u8 {
        let mut mask = 0;
        for (index, fifo) in self.fifo.iter().enumerate() {
            if fifo.wants_refill() {
                mask |= 1 << index;
            }
        }
        mask
    }

    /// The rate the hardware's own output stage runs at: 32.768 kHz at `SOUNDBIAS`'s default, and
    /// doubled for each step of its sampling-cycle field (GBATEK: 32.768/65.536/131.072/262.144 kHz
    /// with 9/8/7/6-bit amplitude).
    pub fn internal_hz(&self) -> u32 {
        OUTPUT_HZ << (self.reg_u16(SOUNDBIAS) >> 14)
    }

    /// Master-clock cycles per internal mix sample.
    fn mix_period(&self) -> f64 {
        f64::from(CLOCK_HZ) / f64::from(self.internal_hz().max(1))
    }

    /// Advance the sound unit by `cycles` master cycles.
    ///
    /// `periods` is each FIFO's timer period, `None` while that timer is stopped. A change of period
    /// restarts the FIFO's accumulator, because the hardware's sample clock *is* the timer's
    /// overflow: a timer that is started begins counting from its reload, not from wherever the
    /// sound unit happened to be.
    ///
    /// The loop steps to the next thing that changes the output — the next internal mix sample, or
    /// the next FIFO pop — so a pop and a sample never land in the wrong order. It runs twice in
    /// effect: the mixer produces one sample per `SOUNDBIAS` period (32.768 kHz by default), and
    /// [`Sound::resample`] hands that stream to the ring's rate.
    pub fn tick(&mut self, mut cycles: u32, periods: [Option<u32>; 2]) {
        for (index, period) in periods.iter().enumerate() {
            if *period != self.period[index] {
                self.period[index] = *period;
                self.fifo[index].sub = 0;
            }
        }
        while cycles > 0 {
            let to_sample = (self.mix_period() - self.mix_acc).ceil().max(1.0) as u32;
            let mut step = cycles.min(to_sample);
            for (index, period) in self.period.iter().enumerate() {
                if let Some(period) = *period {
                    let left = period.saturating_sub(self.fifo[index].sub).max(1);
                    step = step.min(left);
                }
            }
            self.advance(step);
            cycles -= step;
            if self.mix_acc >= self.mix_period() {
                self.mix_acc -= self.mix_period();
                let (left, right) = self.mix();
                self.resample(left, right);
            }
        }
    }

    /// Hand one internal mix sample to the output at `sample_rate`.
    ///
    /// This is the same fractional-accumulator idea the Game Boy's APU uses to place its samples on
    /// the master clock: `out_acc` counts internal samples not yet spent, and one output sample
    /// costs `internal_hz / sample_rate` of them. The accumulator is never reset at a frame
    /// boundary, so the seam between two frames' batches is the next output sample, not a restart —
    /// a reset would drop or repeat a fraction of a sample and be audible as a tick at 59.7 Hz.
    fn resample(&mut self, left: f32, right: f32) {
        let step = f64::from(self.internal_hz().max(1)) / f64::from(self.sample_rate.max(1));
        self.out_acc += 1.0;
        while self.out_acc >= step {
            self.out_acc -= step;
            self.push_sample(left, right);
        }
    }

    /// Move every counter forward `cycles`, popping a FIFO byte on each timer overflow it crosses.
    fn advance(&mut self, cycles: u32) {
        self.mix_acc += f64::from(cycles);
        self.ch1.step(cycles);
        self.ch2.step(cycles);
        self.ch3.step(cycles);
        self.ch4.step(cycles);
        let (length, sweep, envelope) = self.seq.tick(cycles);
        for _ in 0..length {
            self.ch1.length_clock();
            self.ch2.length_clock();
            self.ch3.length_clock();
            self.ch4.length_clock();
        }
        for _ in 0..sweep {
            self.ch1.sweep_clock();
        }
        for _ in 0..envelope {
            self.ch1.envelope_clock();
            self.ch2.envelope_clock();
            self.ch4.envelope_clock();
        }
        for (index, period) in self.period.iter().enumerate() {
            let Some(period) = *period else {
                continue;
            };
            let fifo = &mut self.fifo[index];
            fifo.sub += cycles;
            while fifo.sub >= period {
                fifo.sub -= period;
                fifo.pop();
                self.pops[index] += 1;
            }
        }
    }

    /// The mixer: each direct channel's held sample through its volume and panning, clipped.
    ///
    /// GBATEK's *Max Output Levels*: "Each of the two FIFOs can span the FULL output range (+/-200h)
    /// ... The current output levels of all six channels are added together by hardware ... values
    /// that exceed the unsigned 10bit output range of 0..3FFh are clipped". In normalised signed
    /// terms that is a sample of +/-1 plus the PSG's quarters, clamped to +/-1 — a wrapped sample
    /// would be an audible click at full volume.
    fn mix(&self) -> (f32, f32) {
        if !self.master_enabled() {
            return (0.0, 0.0);
        }
        let control = self.control_h();
        let volume = |bit: u16| if control & bit != 0 { 1.0 } else { 0.5 };
        let a = f32::from(self.fifo[0].current) / 128.0 * volume(FIFO_A_VOLUME);
        let b = f32::from(self.fifo[1].current) / 128.0 * volume(FIFO_B_VOLUME);
        let mut left = 0.0;
        let mut right = 0.0;
        if control & FIFO_A_LEFT != 0 {
            left += a;
        }
        if control & FIFO_A_RIGHT != 0 {
            right += a;
        }
        if control & FIFO_B_LEFT != 0 {
            left += b;
        }
        if control & FIFO_B_RIGHT != 0 {
            right += b;
        }
        // The PSG voices. GBATEK's *Max Output Levels*: "Each of the four PSGs can span one QUARTER
        // of the output range (+/-80h)", so a voice at full volume is a quarter of the direct
        // channels' full scale. `SOUNDCNT_H` bits 0-1 select 25/50/100 % (3 is "Prohibited").
        let psg_ratio = match control & PSG_VOLUME_MASK {
            0 => 0.25,
            1 => 0.5,
            2 => 1.0,
            _ => 0.0,
        };
        let levels = [
            self.ch1.level(),
            self.ch2.level(),
            self.ch3.level(&self.wave),
            self.ch4.level(),
        ];
        let centers = [7.5, 7.5, self.ch3.center(), 7.5];
        // A voice whose DAC is off contributes nothing — not its mid-point. So does a tone or noise
        // voice at volume zero: its DAC input is a constant, which the output stage's AC coupling
        // removes, and mixing that constant would put a -7.5 mid-point offset on everything else
        // (F-Zero leaves three voices enabled at volume zero, which is exactly this case).
        let on = [
            self.ch1.enabled && self.ch1.dac && self.ch1.vol != 0,
            self.ch2.enabled && self.ch2.dac && self.ch2.vol != 0,
            self.ch3.enabled && (self.ch3.volume_code != 0 || self.ch3.force75),
            self.ch4.enabled && self.ch4.dac && self.ch4.vol != 0,
        ];
        // `SOUNDCNT_L` is the Color's `NR50` and `NR51` side by side: master volume right in bits
        // 0-2 and left in 4-6, with the same +1 gain the GB APU uses, and an enable per voice per
        // side in bits 8-11 (right) and 12-15 (left).
        let control_l = self.reg_u16(SOUNDCNT_L);
        let right_gain = (f32::from(control_l & 0x07) + 1.0) / 8.0;
        let left_gain = (f32::from((control_l >> 4) & 0x07) + 1.0) / 8.0;
        for (index, level) in levels.iter().enumerate() {
            if !on[index] || (self.channel_mask >> index) & 1 == 0 {
                continue;
            }
            let value = (f32::from(*level) - centers[index]) / 7.5 * 0.25 * psg_ratio;
            if (control_l >> (12 + index)) & 1 != 0 {
                left += value * left_gain;
            }
            if (control_l >> (8 + index)) & 1 != 0 {
                right += value * right_gain;
            }
        }
        // `SOUNDBIAS` bits 14-15 pick the amplitude resolution: 9 bits at the default 32.768 kHz,
        // down to 6 at 262.144 kHz. The hardware's output is that grid, so a 6-bit setting really
        // does sound coarser.
        let bits = 9 - (self.reg_u16(SOUNDBIAS) >> 14);
        let levels = (1u32 << (bits - 1)) as f32;
        let quantise = |sample: f32| (sample.clamp(-1.0, 1.0) * levels).round() / levels;
        (quantise(left), quantise(right))
    }

    /// Stage one stereo sample, dropping it if the front end has not drained since the buffer
    /// filled — the same policy the Game Boy's APU uses.
    fn push_sample(&mut self, left: f32, right: f32) {
        if self.len + 2 > self.out.len() {
            if self.read_pos == 0 {
                return;
            }
            self.out.copy_within(self.read_pos..self.len, 0);
            self.len -= self.read_pos;
            self.read_pos = 0;
        }
        self.out[self.len] = left;
        self.out[self.len + 1] = right;
        self.len += 2;
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
        self.sample_rate = hz.max(1);
    }

    /// The four CHANNELS rows. On the Game Boy Advance they mute PSG voices 1-4; the two
    /// direct channels are not in that row set and stay audible.
    pub fn set_channel_mask(&mut self, mask: u8) {
        self.channel_mask = mask & 0x0F;
    }

    /// The registers, both FIFOs and the resampler's position.
    pub fn encode(&self, w: &mut StateWriter) {
        w.bytes(&self.regs);
        w.bytes(&self.wave);
        self.ch1.encode(w);
        self.ch2.encode(w);
        self.ch3.encode(w);
        self.ch4.encode(w);
        w.u32(self.seq.length);
        w.u32(self.seq.sweep);
        w.u32(self.seq.envelope);
        for fifo in &self.fifo {
            w.u32(fifo.data.len() as u32);
            for sample in &fifo.data {
                w.u8(*sample as u8);
            }
            w.u8(fifo.current as u8);
            w.u32(fifo.sub);
        }
        for period in self.period {
            match period {
                Some(period) => {
                    w.bool(true);
                    w.u32(period);
                }
                None => w.bool(false),
            }
        }
        w.u32(self.sample_rate);
        w.f64(self.mix_acc);
        w.f64(self.out_acc);
        w.u8(self.channel_mask);
    }

    pub fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.regs)?;
        r.bytes_into(&mut self.wave)?;
        self.ch1.decode(r)?;
        self.ch2.decode(r)?;
        self.ch3.decode(r)?;
        self.ch4.decode(r)?;
        self.seq.length = r.u32()?;
        self.seq.sweep = r.u32()?;
        self.seq.envelope = r.u32()?;
        for fifo in &mut self.fifo {
            let len = r.u32()? as usize;
            if len > FIFO_BYTES {
                return Err(StateError::BadValue);
            }
            fifo.data.clear();
            for _ in 0..len {
                fifo.data.push_back(r.u8()? as i8);
            }
            fifo.current = r.u8()? as i8;
            fifo.sub = r.u32()?;
        }
        for period in &mut self.period {
            *period = if r.bool()? { Some(r.u32()?) } else { None };
        }
        self.sample_rate = r.u32()?;
        self.mix_acc = r.f64()?;
        self.out_acc = r.f64()?;
        self.channel_mask = r.u8()?;
        Ok(())
    }
}

impl Default for Sound {
    fn default() -> Self {
        Sound::new()
    }
}

/// `SOUND1CNT_H`/`SOUND2CNT_L` bits 0-7: the duty pattern and the length.
fn set_square_duty(square: &mut Square, value: u16) {
    square.duty = ((value >> 6) & 0x03) as u8;
    square.length = 64 - (value & 0x3F);
}

/// The same register's high byte: envelope step time, direction and initial volume, and the DAC
/// gate (GBATEK: "Initial Volume of envelope (1-15, 0=No Sound)"; a zero high nibble switches the
/// DAC off, which stops the channel).
fn set_square_envelope(square: &mut Square, value: u16) {
    square.env_period = ((value >> 8) & 0x07) as u8;
    square.env_dir_up = value & 0x0800 != 0;
    square.vol = ((value >> 12) & 0x0F) as u8;
    let dac_gate = value & 0xF800 != 0;
    square.dac = dac_gate;
    if !dac_gate {
        square.enabled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Push `bytes` into a FIFO through the register interface, as a DMA write does.
    fn fill(sound: &mut Sound, channel: usize, bytes: &[u8]) {
        let base = if channel == 0 { FIFO_A } else { FIFO_B };
        for (index, byte) in bytes.iter().enumerate() {
            sound.write8(base + index % 4, *byte);
        }
    }

    /// A FIFO of `count` copies of `sample`.
    fn constant(count: usize, sample: u8) -> Vec<u8> {
        vec![sample; count]
    }

    #[test]
    fn a_store_to_a_fifo_is_a_sample_and_the_fourth_store_is_the_fourth_sample() {
        let mut sound = Sound::new();
        fill(&mut sound, 0, &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(
            sound.fifo[0].data.iter().copied().collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        // A 32-bit store is one bus access and four sample bytes, in address order.
        sound.write8(FIFO_A, 0x10);
        sound.write8(FIFO_A + 1, 0x20);
        sound.write8(FIFO_A + 2, 0x30);
        sound.write8(FIFO_A + 3, 0x40);
        assert_eq!(sound.fifo[0].data.len(), 8);
        assert_eq!(
            sound.fifo[0]
                .data
                .iter()
                .copied()
                .skip(4)
                .collect::<Vec<_>>(),
            [0x10, 0x20, 0x30, 0x40]
        );
    }

    #[test]
    fn a_timer_overflow_moves_one_byte_to_the_dac_and_an_empty_fifo_holds_it() {
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        fill(&mut sound, 0, &[0x40, 0x80]);
        // Channel A to both sides at 100 %.
        sound.write8(SOUNDCNT_H, FIFO_A_VOLUME as u8);
        sound.write8(SOUNDCNT_H + 1, 0x03);
        // Two pops at a 512-cycle period: the first two timer overflows play both bytes.
        sound.tick(512, [Some(512), None]);
        assert_eq!(sound.fifo[0].current, 0x40);
        sound.tick(512, [Some(512), None]);
        assert_eq!(sound.fifo[0].current, -128);
        assert!(sound.fifo[0].data.is_empty());
        // A third overflow finds nothing: the DAC keeps the last sample rather than clicking to
        // zero (GBATEK's FIFO chapter: the FIFO's contents are replayed forever).
        sound.tick(512, [Some(512), None]);
        assert_eq!(sound.fifo[0].current, -128);
        assert!(
            sound.out.iter().any(|s| *s < -0.9),
            "the sample reached the mix"
        );
    }

    #[test]
    fn the_refill_request_rises_when_a_pop_leaves_sixteen_bytes() {
        let mut sound = Sound::new();
        fill(&mut sound, 0, &constant(FIFO_BYTES, 0x20));
        assert_eq!(
            sound.refill_request(),
            0,
            "no pop has asked for anything yet"
        );
        // Pop sixteen bytes at a 512-cycle period: the sixteenth leaves it exactly at the level.
        sound.tick(512 * 16, [Some(512), None]);
        assert_eq!(sound.fifo[0].data.len(), FIFO_REFILL_LEVEL);
        assert_eq!(
            sound.refill_request(),
            0b01,
            "channel A asks for sixteen more"
        );
        // One transfer answers one request: the latch is taken, not left high for the next chunk.
        assert!(sound.fifo[0].take_refill());
        assert_eq!(sound.refill_request(), 0);
        // And the DMA's sixteen bytes raise the level again without re-raising the request.
        fill(&mut sound, 0, &constant(FIFO_REFILL_LEVEL, 0x20));
        assert_eq!(sound.fifo[0].data.len(), FIFO_BYTES);
        assert_eq!(sound.refill_request(), 0);
    }

    #[test]
    fn the_sample_rate_is_the_timer_overflow_rate() {
        // GBATEK's initialisation writes `0xFFFF - round(2^24 / playrate)`, so a period of 512
        // master cycles at 16.777216 MHz is 32.768 kHz — the physical output rate.
        assert_eq!(sample_rate_from_period(512), 32_768.0);
        assert_eq!(sample_rate_from_period(1024), 16_384.0);
        assert_eq!(sample_rate_from_period(2048), 8_192.0);
        // And the reload arithmetic a game does: 0x10000 - 512.
        let reload = 0x1_0000 - 512u32;
        assert_eq!(0x1_0000 - reload, 512);
    }

    #[test]
    fn a_stopped_or_restarted_timer_restarts_the_fifos_accumulator() {
        let mut sound = Sound::new();
        fill(&mut sound, 0, &constant(32, 0x30));
        sound.tick(500, [None, None]);
        assert_eq!(sound.fifo[0].data.len(), 32, "a stopped timer pops nothing");
        // The timer starts at 512: the accumulator begins at zero, so the first pop is a full
        // period away rather than the 12 cycles left over from the stopped span.
        sound.tick(400, [Some(512), None]);
        assert_eq!(sound.fifo[0].data.len(), 32);
        sound.tick(112, [Some(512), None]);
        assert_eq!(sound.fifo[0].data.len(), 31);
    }

    #[test]
    fn the_output_rate_is_the_rate_the_front_end_asked_for() {
        let mut sound = Sound::new();
        sound.set_sample_rate(48_000);
        sound.write8(SOUNDCNT_X, 0x80);
        // One frame's worth of master cycles.
        sound.tick(crate::bus::CYCLES_PER_FRAME, [Some(512), None]);
        let want = f64::from(crate::bus::CYCLES_PER_FRAME) * 48_000.0 / f64::from(CLOCK_HZ);
        let got = (sound.len / 2) as f64;
        assert!(
            (got - want).abs() <= 2.0,
            "{got} samples staged, {want} expected"
        );
        assert_eq!(sound.out[sound.len - 2], 0.0, "nothing is playing");
        assert_eq!(sound.out[sound.len - 1], 0.0);
    }

    #[test]
    fn a_master_disabled_or_quiet_register_block_produces_exact_zeroes() {
        let mut sound = Sound::new();
        sound.set_sample_rate(44_100);
        // The post-boot block: master enable clear, so every staged sample is exactly zero.
        sound.tick(crate::bus::CYCLES_PER_FRAME, [None, None]);
        assert!(!sound.out[..sound.len].iter().any(|s| *s != 0.0));
        // Even with a FIFO playing, the master enable gates it.
        fill(&mut sound, 0, &constant(32, 0x7F));
        sound.write8(SOUNDCNT_H, 0x00);
        sound.write8(SOUNDCNT_H + 1, 0x03);
        sound.tick(crate::bus::CYCLES_PER_FRAME, [Some(512), None]);
        assert!(sound.out[..sound.len].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn panning_is_soundcnt_h_bits_eight_to_thirteen() {
        let mut sound = Sound::new();
        sound.set_sample_rate(32_768);
        sound.write8(SOUNDCNT_X, 0x80);
        // A held +1.0 sample (0x7F is +127/128) and a held -1.0 (0x80 is -128), both at 100 %:
        // channel A on the left, channel B on the right.
        sound.fifo[0].current = 0x7F;
        sound.fifo[1].current = -0x80;
        sound.write8(SOUNDCNT_H, (FIFO_A_VOLUME | FIFO_B_VOLUME) as u8);
        sound.write8(SOUNDCNT_H + 1, 0x12); // bit 9 (A left) | bit 12 (B right)
        sound.tick(512, [None, None]);
        let (left, right) = (sound.out[0], sound.out[1]);
        assert!(left > 0.9, "A on the left: {left}");
        assert!(right < -0.9, "B on the right: {right}");
        // Moving A to the right and B to the left swaps them.
        sound.len = 0;
        sound.read_pos = 0;
        sound.write8(SOUNDCNT_H + 1, 0x21); // bit 8 (A right) | bit 13 (B left)
        sound.tick(512, [None, None]);
        let (left, right) = (sound.out[0], sound.out[1]);
        assert!(right > 0.9, "A on the right: {right}");
        assert!(left < -0.9, "B on the left: {left}");
    }

    #[test]
    fn the_mix_clips_rather_than_wrapping() {
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        // Both channels at full volume and full amplitude: +2.0 before the clamp.
        sound.fifo[0].current = 0x7F;
        sound.fifo[1].current = 0x7F;
        sound.write8(SOUNDCNT_H, (FIFO_A_VOLUME | FIFO_B_VOLUME) as u8);
        sound.write8(SOUNDCNT_H + 1, 0b0011_0011);
        sound.tick(512, [None, None]);
        assert_eq!(sound.out[0], 1.0);
        assert_eq!(sound.out[1], 1.0);
    }

    #[test]
    fn a_reset_bit_clears_the_fifo_and_the_held_sample() {
        let mut sound = Sound::new();
        fill(&mut sound, 1, &constant(4, 0x11));
        sound.fifo[1].current = 0x11;
        sound.write8(SOUNDCNT_H, 0x00);
        sound.write8(SOUNDCNT_H + 1, 0x88); // bit 15: reset B
        assert!(sound.fifo[1].data.is_empty());
        assert_eq!(sound.fifo[1].current, 0);
    }

    /// Program square 1 the way a driver does: no sweep, 50 % duty, volume 12, 512 Hz, and
    /// `SOUNDCNT_L` with every voice on both sides at full master volume.
    fn square_one(sound: &mut Sound) {
        sound.write8(SOUNDCNT_X, 0x80);
        sound.write8(SOUNDCNT_H, 0x02); // PSG voices at 100 %
        sound.write8(SOUNDCNT_L, 0xF7); // right volume 7, left volume 7
        sound.write8(SOUNDCNT_L + 1, 0xF7); // all four voices on both sides
        sound.write8(0x60, 0x00); // sweep time 0: the sweep is disabled
        sound.write8(0x61, 0x00);
        sound.write8(0x62, 0x80); // duty 2 (50 %), length $00
        sound.write8(0x63, 0xC0); // envelope period 0, volume 12
        sound.write8(0x64, 0x00);
        sound.write8(0x65, 0x87); // period $700, trigger
    }

    #[test]
    fn a_square_plays_at_gbateks_frequency() {
        assert_eq!(square_hz(1792), 512.0);
        assert_eq!(square_hz(2047), 131072.0 / 1.0);
        let mut sound = Sound::new();
        square_one(&mut sound);
        assert_eq!(sound.ch1.period, 1792);
        assert!(sound.ch1.enabled && sound.ch1.dac);
        // A duty step is 16 * (2048 - 1792) = 4096 master cycles.
        sound.tick(4096, [None, None]);
        assert_eq!(sound.ch1.duty_pos, 1);
        // Eight steps make one cycle of a 512 Hz tone: 16 777 216 / 32 768 = 512.
        sound.tick(4096 * 7, [None, None]);
        assert_eq!(sound.ch1.duty_pos, 0);
    }

    #[test]
    fn the_duty_table_is_the_colors_waveform() {
        let mut square = Square {
            enabled: true,
            dac: true,
            vol: 9,
            duty: 2,
            ..Square::default()
        };
        let highs: Vec<u8> = (0..8)
            .map(|pos| {
                square.duty_pos = pos;
                square.level()
            })
            .collect();
        assert_eq!(highs, [9, 0, 0, 0, 0, 9, 9, 9], "50 % duty, volume 9");
    }

    #[test]
    fn the_wave_channel_plays_the_bank_that_is_not_being_written() {
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        // One-bank mode, bank 0 selected for playback, playback on (bit 7).
        sound.write8(0x70, 0x80);
        // Reads and writes address the *other* bank, so 0x90 lands in bank 1.
        sound.write8(0x90, 0xF0);
        assert_eq!(sound.wave[16], 0xF0, "the store reached bank 1");
        assert_eq!(sound.wave[0], 0, "and left the played bank alone");
        // Flip playback to bank 1: the channel now plays what was written.
        sound.write8(0x70, 0xC0);
        sound.write8(0x73, 0x20); // volume 100 % (bits 13-14)
        sound.write8(0x75, 0x87); // period $700 (8192 Hz digits), trigger
        assert_eq!(wave_hz(1792), 8192.0);
        assert_eq!(sound.ch3.level(&sound.wave), 0x0F, "the first digit is F");
        // One digit is 8 * (2048 - 1792) = 2048 cycles; the second digit is 0.
        sound.tick(2048, [None, None]);
        assert_eq!(sound.ch3.pos, 1);
        assert_eq!(sound.ch3.level(&sound.wave), 0x00);
    }

    #[test]
    fn the_noise_channel_shifts_gbateks_polynomial_counter() {
        assert_eq!(noise_hz(0, 0), 524_288.0);
        assert_eq!(noise_hz(1, 0), 262_144.0, "r=1 is half the r=0.5 clock");
        assert_eq!(noise_hz(0, 1), 262_144.0, "one shift halves it");
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        sound.write8(0x78, 0x00);
        sound.write8(0x79, 0xF0); // volume 15
        sound.write8(0x7C, 0x00); // divisor 0, 15-bit, shift 0
        sound.write8(0x7D, 0x80); // trigger
        assert_eq!(sound.ch4.lfsr, 0x4000, "GBATEK's initial value");
        // One shift at 32 cycles: the output is the old bit 0, which is zero here.
        sound.tick(32, [None, None]);
        assert!(!sound.ch4.high);
        let mut high = sound.ch4.high;
        let mut low = !sound.ch4.high;
        for _ in 0..64 {
            sound.tick(32, [None, None]);
            high |= sound.ch4.high;
            low |= !sound.ch4.high;
        }
        assert!(high && low, "the counter switches between both levels");
        assert_ne!(sound.ch4.lfsr, 0x4000, "and it is no longer the seed");
    }

    #[test]
    fn the_length_timer_stops_a_channel_at_256_hz() {
        let mut sound = Sound::new();
        square_one(&mut sound);
        sound.ch1.length = 2;
        sound.ch1.length_enable = true;
        sound.tick(LENGTH_PERIOD * 2, [None, None]);
        assert!(!sound.ch1.enabled, "two 256 Hz ticks end a two-tick length");
    }

    #[test]
    fn the_envelope_steps_at_sixty_four_hertz() {
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        sound.write8(0x62, 0x00);
        sound.write8(0x63, 0x71); // period 1, decreasing, volume 7
        sound.write8(0x64, 0x00);
        sound.write8(0x65, 0x87);
        assert_eq!(sound.ch1.vol, 7);
        sound.tick(ENVELOPE_PERIOD, [None, None]);
        assert_eq!(sound.ch1.vol, 6, "one 64 Hz step down");
        sound.tick(ENVELOPE_PERIOD, [None, None]);
        assert_eq!(sound.ch1.vol, 5);
    }

    #[test]
    fn soundcnt_l_pans_and_the_master_volume_scales_each_side() {
        let mut sound = Sound::new();
        sound.set_sample_rate(32_768);
        square_one(&mut sound);
        // Channel 1 to the left only: high byte bit 12.
        sound.write8(SOUNDCNT_L + 1, 0x10);
        // One whole 512 Hz tone cycle, so both halves of the 50 % duty are sampled.
        sound.tick(4096 * 8, [None, None]);
        let left: Vec<f32> = sound.out[..sound.len].iter().step_by(2).copied().collect();
        let right: Vec<f32> = sound.out[..sound.len]
            .iter()
            .skip(1)
            .step_by(2)
            .copied()
            .collect();
        assert!(left.iter().any(|s| *s > 0.1), "the left side carries it");
        assert!(
            right.iter().all(|s| *s == 0.0),
            "the right side is exactly silent"
        );
        let swing = |samples: &[f32]| {
            let high = samples.iter().fold(f32::MIN, |peak, s| peak.max(*s));
            let low = samples.iter().fold(f32::MAX, |trough, s| trough.min(*s));
            high - low
        };
        // Master volume 0 leaves the same square at one eighth, the Color's +1 gain.
        let loud = swing(&left);
        sound.len = 0;
        sound.read_pos = 0;
        sound.write8(SOUNDCNT_L, 0x00); // left and right master volume 0
        sound.write8(SOUNDCNT_L + 1, 0x10); // still channel 1 left only
        sound.tick(4096 * 8, [None, None]);
        let left: Vec<f32> = sound.out[..sound.len].iter().step_by(2).copied().collect();
        let quiet = swing(&left);
        assert!(
            (loud - 0.3984).abs() < 0.01,
            "volume 12 at master volume 7 swings 0.15 to -0.25: {loud}"
        );
        assert!(
            (quiet - 0.0469).abs() < 0.01,
            "and one eighth of that at master volume 0: {quiet}"
        );
    }

    #[test]
    fn soundbias_sets_the_amplitude_resolution() {
        let mut sound = Sound::new();
        sound.set_sample_rate(32_768);
        square_one(&mut sound);
        // Sampling cycle 3: 6-bit amplitude at 262.144 kHz.
        sound.write8(SOUNDBIAS, 0x00);
        sound.write8(SOUNDBIAS + 1, 0xC2);
        sound.tick(4096, [None, None]);
        let samples = &sound.out[..sound.len];
        assert!(
            samples
                .iter()
                .all(|s| ((s * 32.0).round() - s * 32.0).abs() < 1e-4),
            "every sample sits on the 1/32 grid"
        );
        let peak = samples.iter().fold(f32::MIN, |peak, s| peak.max(*s));
        assert!(
            (peak - 0.15625).abs() < 0.005,
            "0.15 quantised to six bits is 5/32: {peak}"
        );
    }

    #[test]
    fn a_triggered_square_reaches_the_mix_at_a_quarter_of_full_scale() {
        let mut sound = Sound::new();
        sound.set_sample_rate(32_768);
        square_one(&mut sound);
        sound.write8(SOUNDCNT_H, 0x02); // PSG at 100 %
        sound.tick(4096, [None, None]);
        // Volume 12 of 15, half a duty cycle: (12 - 7.5) / 7.5 * 0.25 = 0.15.
        assert!(
            sound.out[..sound.len]
                .iter()
                .any(|s| (*s - 0.15).abs() < 0.01),
            "the square's high half reached the mix"
        );
    }

    #[test]
    fn a_frame_seam_is_a_continuation_of_the_same_sample_stream() {
        let mut sound = Sound::new();
        sound.set_sample_rate(44_100);
        sound.write8(SOUNDCNT_X, 0x80);
        sound.write8(SOUNDCNT_H, FIFO_A_VOLUME as u8);
        sound.write8(SOUNDCNT_H + 1, 0x03);
        // A held +0.25 direct sample. If the resampler restarted at a frame boundary the seam would
        // drop or repeat a fraction of a sample and the constant would step; it must not.
        sound.fifo[0].current = 0x20;
        let mut frames = Vec::new();
        for _ in 0..3 {
            let before = sound.len;
            sound.tick(crate::bus::CYCLES_PER_FRAME, [None, None]);
            frames.push(sound.out[before..sound.len].to_vec());
        }
        for frame in &frames {
            assert!(!frame.is_empty(), "every frame produces samples");
            assert!(
                frame.iter().all(|s| (*s - 0.25).abs() < 1e-6),
                "a held sample stays exactly one value across the seam"
            );
        }
        // The count is the rate's own. One frame can sit a sample either side of the ideal because
        // the internal 32.768 kHz grid and the ring's rate do not share a period; the *running*
        // total is what must not drift, so that bound is tighter than the per-frame one.
        let counts: Vec<usize> = frames.iter().map(|frame| frame.len() / 2).collect();
        let want = f64::from(crate::bus::CYCLES_PER_FRAME) * 44_100.0 / f64::from(CLOCK_HZ);
        for count in &counts {
            assert!(
                (*count as f64 - want).abs() <= 2.0,
                "{count} samples in a frame, {want} expected"
            );
        }
        let total: usize = counts.iter().sum();
        // Two grids (32.768 kHz internal, the ring's rate) leave the running total below the ideal
        // by less than one internal sample plus one output sample. The offset does not grow with
        // the run: it is the fractional internal sample the frame boundary fell inside.
        assert!(
            (total as f64 - want * 3.0).abs() <= 3.0,
            "{total} samples over three frames, {} expected",
            want * 3.0
        );
    }

    #[test]
    fn the_resampler_carries_the_fraction_across_a_boundary() {
        // Two halves of a frame and one frame must produce the same stream: the accumulator is the
        // state, not the call.
        let mut whole = Sound::new();
        whole.set_sample_rate(44_100);
        whole.write8(SOUNDCNT_X, 0x80);
        whole.fifo[0].current = 0x40;
        whole.write8(SOUNDCNT_H, FIFO_A_VOLUME as u8);
        whole.write8(SOUNDCNT_H + 1, 0x03);
        whole.tick(crate::bus::CYCLES_PER_FRAME, [None, None]);

        let mut split = Sound::new();
        split.set_sample_rate(44_100);
        split.write8(SOUNDCNT_X, 0x80);
        split.fifo[0].current = 0x40;
        split.write8(SOUNDCNT_H, FIFO_A_VOLUME as u8);
        split.write8(SOUNDCNT_H + 1, 0x03);
        split.tick(crate::bus::CYCLES_PER_FRAME / 2, [None, None]);
        split.tick(crate::bus::CYCLES_PER_FRAME / 2, [None, None]);

        assert_eq!(
            split.len / 2,
            whole.len / 2,
            "the same master cycles produce the same number of samples however they are batched"
        );
    }

    #[test]
    fn the_sound_block_round_trips_through_a_state() {
        let mut sound = Sound::new();
        sound.write8(SOUNDCNT_X, 0x80);
        fill(&mut sound, 0, &[1, 2, 3]);
        sound.fifo[0].current = 5;
        sound.fifo[0].sub = 123;
        sound.tick(512, [Some(512), None]);
        let mut w = StateWriter::new();
        sound.encode(&mut w);
        let bytes = w.finish();
        let mut r = StateReader::new(&bytes);
        let mut back = Sound::new();
        back.decode(&mut r).expect("a state this build wrote");
        assert_eq!(back.regs, sound.regs);
        assert_eq!(back.fifo[0].current, sound.fifo[0].current);
        assert_eq!(back.fifo[0].sub, sound.fifo[0].sub);
        assert_eq!(back.period[0], sound.period[0]);
    }
}
