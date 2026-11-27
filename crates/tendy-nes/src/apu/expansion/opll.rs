//! Konami VRC7 audio: six channels of 2-operator FM, a subset of the Yamaha YM2413 OPLL with its
//! own fixed patch ROM.
//!
//! Register access is a two-step port pair (the board routes `$9010`/`$9030` here):
//!
//! ```text
//! $9010  VVVV VVVV   internal register select (+ $30 for the address line, i.e. A0 of the OPLL)
//! $9030  VVVV VVVV   data for the selected register
//! ```
//!
//! Internal registers: `$00-$07` the custom patch for instrument 0, `$10-$15` channel frequency low
//! bytes, `$20-$25` sustain/trigger/octave/frequency-high, `$30-$35` instrument and volume, `$0F`
//! the test register. `$0E` (OPLL rhythm mode) exists but the VRC7 has no rhythm DAC, so its five
//! rhythm channels are audible only as ordinary FM channels — the six FM channels are all this
//! core produces.
//!
//! The chip runs from its own 3.58 MHz ceramic oscillator, updating all channels every 72
//! internal cycles, i.e. one channel update per 49716 Hz. [`Opll::clock`] is called once per CPU
//! cycle and generates an internal sample every 36 CPU cycles (the NTSC ratio; the VRC7 is a
//! Famicom part, so there is no PAL case to scale for).
//!
//! ## Fidelity
//!
//! The FM arithmetic is a straightforward phase-modulation model: the modulator's linear output
//! shifts the carrier's phase, feedback shifts the modulator's, and both operators use the patch's
//! multiplier. Amplitudes are computed in the linear domain from an attenuation in dB, which is
//! more precise than the chip's 1/256-log2 fixed point. The *envelope* rates are the documented
//! shape (0.125 dB per step, one step per `2^(15-rate)` internal samples, attack two rate steps
//! faster) rather than the chip's exact counter, so decay and release times are close but not
//! sample-identical to hardware. Register semantics, key-on/key-off edges, instrument selection,
//! volume, tremolo/vibrato and the test register are modelled from the NESdev VRC7 audio page and
//! pinned by the unit tests.

use crate::state::{StateError, StateReader, StateWriter};

/// Peak of six fully-driven channels against the 2A03 mixer.
pub const OPLL_AUDIO_SCALE: f32 = 0.45;

/// Internal sample period in CPU cycles (1789773 / 49716).
const CPU_CYCLES_PER_SAMPLE: f32 = 36.0;

/// Internal sample rate, for the envelope and LFO rates.
const SAMPLE_RATE: f32 = 49_716.0;

/// The VRC7's fixed patch ROM, dumped in debug mode (NESdev VRC7 audio, "Internal patch set").
/// Instrument 0 is the custom patch in registers `$00-$07`; the rest are hardwired.
const FIXED_PATCHES: [[u8; 8]; 16] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 0: custom
    [0x03, 0x21, 0x05, 0x06, 0xE8, 0x81, 0x42, 0x27], // 1: Buzzy Bell
    [0x13, 0x41, 0x14, 0x0D, 0xD8, 0xF6, 0x23, 0x12], // 2: Guitar
    [0x11, 0x11, 0x08, 0x08, 0xFA, 0xB2, 0x20, 0x12], // 3: Wurly
    [0x31, 0x61, 0x0C, 0x07, 0xA8, 0x64, 0x61, 0x27], // 4: Flute
    [0x32, 0x21, 0x1E, 0x06, 0xE1, 0x76, 0x01, 0x28], // 5: Clarinet
    [0x02, 0x01, 0x06, 0x00, 0xA3, 0xE2, 0xF4, 0xF4], // 6: Synth
    [0x21, 0x61, 0x1D, 0x07, 0x82, 0x81, 0x11, 0x07], // 7: Trumpet
    [0x23, 0x21, 0x22, 0x17, 0xA2, 0x72, 0x01, 0x17], // 8: Organ
    [0x35, 0x11, 0x25, 0x00, 0x40, 0x73, 0x72, 0x01], // 9: Bells
    [0xB5, 0x01, 0x0F, 0x0F, 0xA8, 0xA5, 0x51, 0x02], // A: Vibes
    [0x17, 0xC1, 0x24, 0x07, 0xF8, 0xF8, 0x22, 0x12], // B: Vibraphone
    [0x71, 0x23, 0x11, 0x06, 0x65, 0x74, 0x18, 0x16], // C: Tutti
    [0x01, 0x02, 0xD3, 0x05, 0xC9, 0x95, 0x03, 0x02], // D: Fretless
    [0x61, 0x63, 0x0C, 0x00, 0x94, 0xC0, 0x33, 0xF6], // E: Synth Bass
    [0x21, 0x72, 0x0D, 0x00, 0xC1, 0xD5, 0x56, 0x06], // F: Sweep
];

/// Frequency multiplier per patch M nibble.
const MULTIPLIER: [f32; 16] = [
    0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 10.0, 12.0, 12.0, 15.0, 15.0,
];

/// Modulator feedback depth per patch F nibble, as a fraction of a full cycle of phase.
const FEEDBACK: [f32; 8] = [
    0.0,
    1.0 / 32.0,
    1.0 / 16.0,
    1.0 / 8.0,
    1.0 / 4.0,
    1.0 / 2.0,
    1.0,
    2.0,
];

/// Modulation depth of a full-scale modulator output, in carrier cycles. The YM2413's maximum
/// modulation index is around 4π, which this lands on.
const MODULATION_DEPTH: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug, Default)]
struct Operator {
    phase: f32,
    /// Attenuation in dB: 0 is full volume, 96 is silent.
    att: f32,
    stage: Stage,
    /// Previous output, for the modulator's feedback path.
    prev: f32,
    // Patch parameters, recomputed when a register changes.
    mult: f32,
    ksl_db: f32,
    total_level_db: f32,
    attack_db: f32,
    decay_db: f32,
    release_db: f32,
    sustain_db: f32,
    hold_sustain: bool,
    half_wave: bool,
    tremolo: bool,
    vibrato: bool,
    /// Modulator feedback depth, in cycles of its own phase.
    feedback: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    fnum: u16,
    octave: u8,
    instrument: u8,
    volume: u8,
    key_on: bool,
    sustain: bool,
    /// Phase increment per internal sample before the operator multiplier, `freq / SAMPLE_RATE`.
    /// Derived from `fnum` and `octave`: set by `refresh_channel` and on state load, never
    /// stored in the save state.
    base_inc: f32,
    modulator: Operator,
    carrier: Operator,
}

impl Channel {
    /// f = 49716 * fnum / 2^(19 - octave), in Hz.
    fn freq(&self) -> f32 {
        49_716.0 * f32::from(self.fnum) / 2f32.powi(19 - i32::from(self.octave))
    }
}

/// VRC7 (YM2413 subset) expansion audio.
#[derive(Clone, Debug)]
pub struct Opll {
    /// The custom instrument 0 patch (registers `$00-$07`).
    patch: [u8; 8],
    channels: [Channel; 6],
    /// Register selected by the last `$9010` write.
    select: u8,
    /// Fractional internal-sample accumulator, in CPU cycles.
    sample_acc: f32,
    tremolo_phase: f32,
    vibrato_phase: f32,
    /// `$0F` test register, low four bits.
    test: u8,
    level: f32,
}

impl Default for Opll {
    fn default() -> Self {
        Opll {
            patch: [0; 8],
            channels: [Channel::default(); 6],
            select: 0,
            sample_acc: 0.0,
            tremolo_phase: 0.0,
            vibrato_phase: 0.0,
            test: 0,
            level: 0.0,
        }
    }
}

impl Opll {
    pub fn new() -> Self {
        let mut opll = Self::default();
        for ch in 0..6 {
            opll.refresh_channel(ch);
        }
        opll
    }

    /// `$9010`: select the internal register for the next `$9030` write.
    pub fn write_select(&mut self, val: u8) {
        self.select = val;
    }

    /// `$9030`: write the selected internal register.
    pub fn write_data(&mut self, val: u8) {
        let reg = self.select as usize & 0x3F;
        match reg {
            0x00..=0x07 => {
                self.patch[reg] = val;
                // Instrument 0 of every channel using it changes at once.
                for ch in 0..6 {
                    if self.channels[ch].instrument == 0 {
                        self.refresh_channel(ch);
                    }
                }
            }
            0x0F => self.test = val & 0x0F,
            0x10..=0x15 => {
                let ch = reg & 0x07;
                let ch = if ch < 6 { ch } else { return };
                self.channels[ch].fnum = (self.channels[ch].fnum & 0x100) | u16::from(val);
                self.refresh_channel(ch);
            }
            0x20..=0x25 => {
                let ch = reg & 0x07;
                if ch >= 6 {
                    return;
                }
                let trigger = val & 0x10 != 0;
                self.channels[ch].sustain = val & 0x20 != 0;
                self.channels[ch].octave = (val >> 1) & 0x07;
                self.channels[ch].fnum =
                    (self.channels[ch].fnum & 0x0FF) | (u16::from(val & 1) << 8);
                let was_on = self.channels[ch].key_on;
                self.channels[ch].key_on = trigger;
                if trigger && !was_on {
                    self.start_note(ch);
                } else if !trigger && was_on {
                    self.release_note(ch);
                }
                self.refresh_channel(ch);
            }
            0x30..=0x35 => {
                let ch = reg & 0x07;
                if ch >= 6 {
                    return;
                }
                self.channels[ch].instrument = val >> 4;
                self.channels[ch].volume = val & 0x0F;
                self.refresh_channel(ch);
            }
            _ => {}
        }
    }

    /// `$E000` bit 7: silence the chip and clear its registers (the vibrato LFO keeps its phase).
    pub fn reset(&mut self) {
        self.patch = [0; 8];
        self.select = 0;
        self.test = 0;
        self.tremolo_phase = 0.0;
        for ch in &mut self.channels {
            *ch = Channel::default();
        }
        for ch in 0..6 {
            self.refresh_channel(ch);
        }
        self.level = 0.0;
    }

    /// A key-on restarts both phases from zero and begins the attack from silence.
    fn start_note(&mut self, ch: usize) {
        let c = &mut self.channels[ch];
        c.modulator.phase = 0.0;
        c.carrier.phase = 0.0;
        c.modulator.prev = 0.0;
        c.modulator.att = 96.0;
        c.carrier.att = 96.0;
        c.modulator.stage = Stage::Attack;
        c.carrier.stage = Stage::Attack;
        // An attack rate of 0 never rises: the channel stays silent (the chip's "halt" for attack).
        if c.modulator.attack_db == 0.0 {
            c.modulator.stage = Stage::Off;
        }
        if c.carrier.attack_db == 0.0 {
            c.carrier.stage = Stage::Off;
        }
    }

    /// A key-off starts the release: the carrier always releases, the modulator only when its
    /// patch sustain bit is clear (a sustaining modulator keeps running under the carrier).
    fn release_note(&mut self, ch: usize) {
        let c = &mut self.channels[ch];
        c.carrier.stage = Stage::Release;
        if !c.modulator.hold_sustain {
            c.modulator.stage = Stage::Release;
        }
    }

    /// Recompute a channel's operator parameters from its instrument patch and the channel state.
    fn refresh_channel(&mut self, ch: usize) {
        let instrument = self.channels[ch].instrument as usize & 0x0F;
        let patch = if instrument == 0 {
            self.patch
        } else {
            FIXED_PATCHES[instrument]
        };
        // Below A4 (440 Hz) key scaling does nothing.
        let freq = self.channels[ch].freq();
        let ksl_note = if freq > 440.0 {
            libm::log2f(freq / 440.0)
        } else {
            0.0
        };

        let c = &mut self.channels[ch];
        c.base_inc = freq / SAMPLE_RATE;
        c.modulator.mult = MULTIPLIER[usize::from(patch[0] & 0x0F)];
        c.carrier.mult = MULTIPLIER[usize::from(patch[1] & 0x0F)];
        c.modulator.tremolo = patch[0] & 0x80 != 0;
        c.carrier.tremolo = patch[1] & 0x80 != 0;
        c.modulator.vibrato = patch[0] & 0x40 != 0;
        c.carrier.vibrato = patch[1] & 0x40 != 0;
        c.modulator.hold_sustain = patch[0] & 0x20 != 0;
        c.carrier.hold_sustain = patch[1] & 0x20 != 0;
        c.modulator.ksl_db = key_scale_db(patch[2] >> 6) * ksl_note;
        c.carrier.ksl_db = key_scale_db(patch[3] >> 6) * ksl_note;
        c.modulator.total_level_db = 0.75 * f32::from(patch[2] & 0x3F);
        c.carrier.total_level_db = 0.0;
        c.modulator.feedback = FEEDBACK[usize::from(patch[3] & 0x07)];
        c.modulator.half_wave = patch[3] & 0x08 != 0;
        c.carrier.half_wave = patch[3] & 0x10 != 0;
        c.modulator.attack_db = rate_db_per_sample(patch[4] >> 4, true);
        c.carrier.attack_db = rate_db_per_sample(patch[5] >> 4, true);
        c.modulator.decay_db = rate_db_per_sample(patch[4] & 0x0F, false);
        c.carrier.decay_db = rate_db_per_sample(patch[5] & 0x0F, false);
        c.modulator.sustain_db = 3.0 * f32::from(patch[6] >> 4);
        c.carrier.sustain_db = 3.0 * f32::from(patch[7] >> 4);
        c.modulator.release_db = rate_db_per_sample(patch[6] & 0x0F, false);
        c.carrier.release_db = rate_db_per_sample(patch[7] & 0x0F, false);
    }

    /// Per-operator attenuation from the envelope, key scaling, total level and channel volume,
    /// in dB.
    fn operator_db(op: &Operator, volume: u8, tremolo_db: f32) -> f32 {
        let mut att = op.att;
        att += op.total_level_db + op.ksl_db;
        att += 3.0 * f32::from(volume);
        if op.tremolo {
            att += tremolo_db;
        }
        att.clamp(0.0, 96.0)
    }

    /// One internal sample at 49716 Hz: envelopes, LFOs, phases, and the six-channel sum.
    fn generate(&mut self) {
        // Tremolo: 3.7 Hz, 4.8 dB deep. Vibrato: 6.4 Hz, ±14 cents. Test-register bit 3 runs both
        // fast (64x tremolo, 1024x vibrato) for test ROMs; bit 1 holds both LFOs at zero.
        let test = self.test;
        let (trem_rate, vib_rate) = if test & 0x08 != 0 {
            (3.7 * 64.0, 6.4 * 1024.0)
        } else {
            (3.7, 6.4)
        };
        self.tremolo_phase = (self.tremolo_phase + trem_rate / SAMPLE_RATE).fract();
        self.vibrato_phase = (self.vibrato_phase + vib_rate / SAMPLE_RATE).fract();
        let tremolo_db = if test & 0x02 != 0 {
            0.0
        } else {
            4.8 * (0.5 - 0.5 * libm::cosf(core::f32::consts::TAU * self.tremolo_phase))
        };
        let vibrato_ratio = if test & 0x02 != 0 {
            1.0
        } else {
            libm::powf(
                2.0,
                14.0 * libm::sinf(core::f32::consts::TAU * self.vibrato_phase) / 1200.0,
            )
        };

        let mut sum = 0.0;
        for c in &mut self.channels {
            advance_envelope(&mut c.modulator, c.sustain);
            advance_envelope(&mut c.carrier, c.sustain);

            // A keyed-off channel whose release has finished is silent; skip the arithmetic.
            if c.modulator.stage == Stage::Off && c.carrier.stage == Stage::Off && !c.key_on {
                c.modulator.prev = 0.0;
                continue;
            }

            let mod_inc = c.base_inc
                * c.modulator.mult
                * if c.modulator.vibrato {
                    vibrato_ratio
                } else {
                    1.0
                };
            let car_inc = c.base_inc
                * c.carrier.mult
                * if c.carrier.vibrato {
                    vibrato_ratio
                } else {
                    1.0
                };

            // Test-register bit 0 forces both envelopes to full volume.
            let (mod_amp, car_amp) = if test & 0x01 != 0 {
                (1.0, 1.0)
            } else {
                (
                    db_to_amp(Self::operator_db(&c.modulator, c.volume, tremolo_db)),
                    db_to_amp(Self::operator_db(&c.carrier, c.volume, tremolo_db)),
                )
            };

            // Feedback shifts the modulator's own phase by its last output; the modulator then
            // shifts the carrier's. Bit 2 of the test register holds (and resets) both phases.
            if test & 0x04 != 0 {
                c.modulator.phase = 0.0;
                c.carrier.phase = 0.0;
            }
            let feedback = c.modulator.prev * c.modulator.feedback;
            let mod_out = sine(c.modulator.phase + feedback, c.modulator.half_wave) * mod_amp;
            if test & 0x04 == 0 {
                c.modulator.phase = (c.modulator.phase + mod_inc).fract();
            }
            c.modulator.prev = mod_out;

            let car_out = sine(
                c.carrier.phase + mod_out * MODULATION_DEPTH,
                c.carrier.half_wave,
            ) * car_amp;
            if test & 0x04 == 0 {
                c.carrier.phase = (c.carrier.phase + car_inc).fract();
            }
            sum += car_out;
        }
        self.level = sum / 6.0 * OPLL_AUDIO_SCALE;
    }

    /// The channel's carrier envelope attenuation, for tests and debug views.
    pub fn attenuation(&self, ch: usize) -> f32 {
        self.channels[ch].carrier.att
    }

    /// The channel's key state, for tests and debug views.
    pub fn key_on(&self, ch: usize) -> bool {
        self.channels[ch].key_on
    }

    /// The patch bytes for instrument `n`.
    pub fn patch(&self, n: usize) -> [u8; 8] {
        if n == 0 {
            self.patch
        } else {
            FIXED_PATCHES[n & 0x0F]
        }
    }
}

impl Opll {
    /// One CPU cycle: advance the 49716 Hz internal sample timer.
    pub fn clock(&mut self) {
        self.sample_acc += 1.0;
        if self.sample_acc >= CPU_CYCLES_PER_SAMPLE {
            self.sample_acc -= CPU_CYCLES_PER_SAMPLE;
            self.generate();
        }
    }

    /// The chip's current contribution, in the 2A03 mixer's scale.
    pub fn output(&self) -> f32 {
        self.level
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.patch);
        w.u8(self.select);
        w.f32(self.sample_acc);
        w.f32(self.tremolo_phase);
        w.f32(self.vibrato_phase);
        w.u8(self.test);
        // `level` stays. It is rebuilt only on an internal sample, so recomputing it on load
        // would need the up-to-36-cycles-stale window to be modelled; the patch-derived operator
        // parameters below it do not have that problem and are rebuilt by `refresh_channel`.
        w.f32(self.level);
        for c in &self.channels {
            w.u16(c.fnum);
            w.u8(c.octave);
            w.u8(c.instrument);
            w.u8(c.volume);
            w.bool(c.key_on);
            w.bool(c.sustain);
            for op in [&c.modulator, &c.carrier] {
                w.f32(op.phase);
                w.f32(op.att);
                w.u8(match op.stage {
                    Stage::Off => 0,
                    Stage::Attack => 1,
                    Stage::Decay => 2,
                    Stage::Sustain => 3,
                    Stage::Release => 4,
                });
                w.f32(op.prev);
            }
        }
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.patch)?;
        self.select = r.u8()?;
        self.sample_acc = r.f32()?;
        self.tremolo_phase = r.f32()?;
        self.vibrato_phase = r.f32()?;
        self.test = r.u8()?;
        self.level = r.f32()?;
        for c in &mut self.channels {
            c.fnum = r.u16()?;
            c.octave = r.u8()?;
            c.base_inc = c.freq() / SAMPLE_RATE;
            c.instrument = r.u8()?;
            c.volume = r.u8()?;
            c.key_on = r.bool()?;
            c.sustain = r.bool()?;
            for op in [&mut c.modulator, &mut c.carrier] {
                op.phase = r.f32()?;
                op.att = r.f32()?;
                op.stage = match r.u8()? {
                    0 => Stage::Off,
                    1 => Stage::Attack,
                    2 => Stage::Decay,
                    3 => Stage::Sustain,
                    4 => Stage::Release,
                    _ => return Err(StateError::BadValue),
                };
                op.prev = r.f32()?;
            }
        }
        // the operator parameters derived from the patch and the channel registers are not in
        // the file; rebuild them exactly as the register writes that produced them would.
        for ch in 0..6 {
            self.refresh_channel(ch);
        }
        Ok(())
    }
}

/// Key scale level in dB per octave for the patch's KSL field.
fn key_scale_db(ksl: u8) -> f32 {
    match ksl & 0x03 {
        0 => 0.0,
        1 => 1.5,
        2 => 3.0,
        _ => 6.0,
    }
}

/// Envelope speed: dB of attenuation per internal sample for a 4-bit rate. One step is 0.125 dB
/// every `2^(15 - rate)` samples; rate 0 means "halted" and yields 0.0. The attack runs two rate
/// steps faster, as on the chip.
fn rate_db_per_sample(rate: u8, attack: bool) -> f32 {
    if rate == 0 {
        return 0.0;
    }
    let rate = if attack { (rate + 2).min(15) } else { rate };
    0.125 / f32::powi(2.0, 15 - i32::from(rate))
}

fn advance_envelope(op: &mut Operator, channel_sustain: bool) {
    match op.stage {
        Stage::Attack => {
            op.att -= op.attack_db;
            if op.att <= 0.0 {
                op.att = 0.0;
                op.stage = Stage::Decay;
            }
        }
        Stage::Decay => {
            op.att += op.decay_db;
            if op.att >= op.sustain_db {
                op.att = op.sustain_db;
                op.stage = Stage::Sustain;
            }
        }
        Stage::Sustain => {
            // The patch sustain bit holds the level; without it the envelope keeps decaying.
            if !op.hold_sustain {
                op.att += op.decay_db;
                if op.att >= 96.0 {
                    op.att = 96.0;
                    op.stage = Stage::Off;
                }
            }
        }
        Stage::Release => {
            let rate = if channel_sustain && op.release_db > 0.0 {
                rate_db_per_sample(5, false)
            } else {
                op.release_db
            };
            op.att += rate;
            if op.att >= 96.0 {
                op.att = 96.0;
                op.stage = Stage::Off;
            }
        }
        Stage::Off => {}
    }
}

/// Attenuation in dB to a linear amplitude, with the chip's 96 dB floor treated as silence.
fn db_to_amp(att_db: f32) -> f32 {
    if att_db >= 96.0 {
        0.0
    } else {
        libm::powf(10.0, -att_db / 20.0)
    }
}

/// One cycle of a sine, or the half-wave rectified version (patch waveform bit).
fn sine(phase: f32, half_wave: bool) -> f32 {
    let s = libm::sinf(core::f32::consts::TAU * phase.fract());
    if half_wave && s < 0.0 { 0.0 } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Select and write one internal register.
    fn write(opll: &mut Opll, reg: u8, val: u8) {
        opll.write_select(reg);
        opll.write_data(val);
    }

    /// Start a note on `ch` with the given instrument, octave, fnum and volume. The instrument and
    /// frequency are programmed before the trigger bit, as a driver would.
    fn note(opll: &mut Opll, ch: u8, instrument: u8, octave: u8, fnum: u16, volume: u8) {
        write(opll, 0x30 + ch, (instrument << 4) | volume);
        write(opll, 0x10 + ch, fnum as u8);
        write(
            opll,
            0x20 + ch,
            0x10 | (octave << 1) | ((fnum >> 8) as u8 & 1),
        );
    }

    /// Install a custom instrument 0 with a fast attack (rate 15) and the given decay, sustain and
    /// release nibbles; `flags` is the T/V/S/K + multiplier byte for both operators.
    fn fast_patch(opll: &mut Opll, decay: u8, sustain: u8, release: u8, flags: u8) {
        write(opll, 0x00, flags);
        write(opll, 0x01, flags);
        write(opll, 0x02, 0x00);
        write(opll, 0x03, 0x00);
        write(opll, 0x04, 0xF0 | decay);
        write(opll, 0x05, 0xF0 | decay);
        write(opll, 0x06, (sustain << 4) | release);
        write(opll, 0x07, (sustain << 4) | release);
    }

    #[test]
    fn fixed_patch_table_matches_the_dumped_rom() {
        let opll = Opll::new();
        assert_eq!(
            opll.patch(1),
            [0x03, 0x21, 0x05, 0x06, 0xE8, 0x81, 0x42, 0x27]
        );
        assert_eq!(
            opll.patch(0x0F),
            [0x21, 0x72, 0x0D, 0x00, 0xC1, 0xD5, 0x56, 0x06]
        );
        assert_eq!(
            opll.patch(0x0E),
            [0x61, 0x63, 0x0C, 0x00, 0x94, 0xC0, 0x33, 0xF6]
        );
    }

    #[test]
    fn custom_patch_registers_drive_instrument_zero() {
        let mut opll = Opll::new();
        write(&mut opll, 0x00, 0x01); // modulator multiplier 1
        write(&mut opll, 0x04, 0xF0); // modulator attack 15
        assert_eq!(opll.patch(0)[0], 0x01);
        note(&mut opll, 0, 0, 4, 0x1AB, 0);
        assert!((opll.channels[0].modulator.mult - 1.0).abs() < 1e-6);
        assert!(opll.channels[0].modulator.attack_db > 0.0);
    }

    #[test]
    fn trigger_edge_starts_and_releases_a_note() {
        let mut opll = Opll::new();
        note(&mut opll, 0, 1, 4, 0x180, 0);
        assert!(opll.key_on(0));
        assert_eq!(opll.channels[0].carrier.stage, Stage::Attack);

        // Writing the same trigger bit again is not a new key-on.
        write(&mut opll, 0x20, 0x10 | (4 << 1));
        assert!(opll.key_on(0));
        assert_eq!(opll.channels[0].carrier.stage, Stage::Attack);

        // Clearing it starts the release.
        write(&mut opll, 0x20, 4 << 1);
        assert!(!opll.key_on(0));
        assert_eq!(opll.channels[0].carrier.stage, Stage::Release);
    }

    #[test]
    fn attack_brings_the_carrier_up_and_decay_falls_to_sustain() {
        let mut opll = Opll::new();
        // Decay rate 8, sustain nibble 2 (6 dB).
        fast_patch(&mut opll, 8, 2, 15, 0x01);
        note(&mut opll, 0, 0, 4, 0x180, 0);
        assert_eq!(opll.channels[0].carrier.att, 96.0);
        let mut saw_sustain = false;
        for _ in 0..20_000 {
            opll.generate();
            if opll.channels[0].carrier.stage == Stage::Sustain {
                saw_sustain = true;
                break;
            }
        }
        assert!(saw_sustain, "the envelope reaches its sustain stage");
        assert!((opll.channels[0].carrier.att - 6.0).abs() < 0.01);
    }

    #[test]
    fn release_reaches_silence_and_the_channel_stops() {
        let mut opll = Opll::new();
        fast_patch(&mut opll, 8, 2, 15, 0x01);
        note(&mut opll, 0, 0, 4, 0x180, 0);
        for _ in 0..5000 {
            opll.generate();
        }
        write(&mut opll, 0x20, 4 << 1); // key off
        for _ in 0..2000 {
            opll.generate();
            if opll.channels[0].carrier.stage == Stage::Off {
                break;
            }
        }
        assert_eq!(opll.channels[0].carrier.stage, Stage::Off);
        assert_eq!(opll.output(), 0.0, "a released channel contributes nothing");
    }

    #[test]
    fn volume_register_attenuates_the_channel() {
        let peak = |volume: u8| {
            let mut opll = Opll::new();
            fast_patch(&mut opll, 0, 0, 0, 0x01);
            note(&mut opll, 0, 0, 4, 0x1C0, volume);
            let mut peak: f32 = 0.0;
            for _ in 0..5000 {
                opll.generate();
                peak = peak.max(opll.output().abs());
            }
            peak
        };
        let loud = peak(0);
        let quiet = peak(15);
        assert!(loud > 0.0);
        assert!(
            quiet < loud / 4.0,
            "15 steps of 3 dB is 45 dB down: {loud} vs {quiet}"
        );
    }

    #[test]
    fn output_is_bounded_and_audible() {
        let mut opll = Opll::new();
        fast_patch(&mut opll, 0, 0, 0, 0x01);
        for ch in 0..6 {
            note(&mut opll, ch, 0, 4, 0x180 + u16::from(ch) * 0x20, 0);
        }
        let mut peak: f32 = 0.0;
        for _ in 0..20_000 {
            opll.generate();
            peak = peak.max(opll.output().abs());
        }
        assert!(
            peak > OPLL_AUDIO_SCALE * 0.5,
            "six notes are audible: {peak}"
        );
        assert!(
            peak <= OPLL_AUDIO_SCALE + 1e-3,
            "and stay inside the scale: {peak}"
        );
    }

    #[test]
    fn test_register_envelope_override_plays_at_full_volume() {
        let mut opll = Opll::new();
        fast_patch(&mut opll, 8, 2, 15, 0x01);
        note(&mut opll, 0, 0, 4, 0x180, 0);
        write(&mut opll, 0x0F, 0x01); // envelopes forced to full volume
        assert_eq!(
            opll.channels[0].carrier.att, 96.0,
            "the envelope itself still runs"
        );
        let mut peak: f32 = 0.0;
        for _ in 0..500 {
            opll.generate();
            peak = peak.max(opll.output().abs());
        }
        // One full-volume channel of six: 0.45 / 6 = 0.075.
        assert!(peak > 0.06, "the override ignores the attenuation: {peak}");
    }

    #[test]
    fn test_register_phase_hold_silences_the_chip() {
        let mut opll = Opll::new();
        fast_patch(&mut opll, 8, 2, 15, 0x01);
        note(&mut opll, 0, 0, 4, 0x180, 0);
        write(&mut opll, 0x0F, 0x04); // hold both phases at zero
        let mut phase = opll.channels[0].carrier.phase;
        for _ in 0..500 {
            opll.generate();
            assert_eq!(opll.channels[0].carrier.phase, phase, "phase held");
            phase = opll.channels[0].carrier.phase;
        }
        assert_eq!(opll.output(), 0.0, "a held phase sits on sine(0)");
    }

    #[test]
    fn audio_reset_clears_the_registers() {
        let mut opll = Opll::new();
        note(&mut opll, 2, 5, 3, 0x111, 7);
        for _ in 0..100 {
            opll.clock();
        }
        opll.reset();
        assert_eq!(opll.patch(0), [0; 8]);
        assert!(!opll.key_on(2));
        assert_eq!(opll.output(), 0.0);
    }

    #[test]
    fn internal_sample_runs_once_every_36_cpu_cycles() {
        let mut opll = Opll::new();
        note(&mut opll, 0, 1, 4, 0x180, 0);
        let mut samples = 0;
        let mut last_phase = opll.channels[0].carrier.phase;
        for _ in 0..36 {
            opll.clock();
            if opll.channels[0].carrier.phase != last_phase {
                samples += 1;
                last_phase = opll.channels[0].carrier.phase;
            }
        }
        assert_eq!(samples, 1);
    }

    #[test]
    fn instrument_patch_flags_drive_tremolo_and_vibrato() {
        let mut opll = Opll::new();
        fast_patch(&mut opll, 8, 2, 15, 0xC1); // T + V + multiplier 1
        note(&mut opll, 0, 0, 4, 0x180, 0);
        assert!(opll.channels[0].carrier.tremolo);
        assert!(opll.channels[0].carrier.vibrato);
        assert!(opll.channels[0].modulator.tremolo);

        // Instrument 4 (Flute) has vibrato on the carrier and tremolo only on the modulator.
        write(&mut opll, 0x30, 0x40);
        assert!(opll.channels[0].carrier.vibrato);
        assert!(!opll.channels[0].carrier.tremolo);
        assert!(!opll.channels[0].modulator.vibrato);
    }

    #[test]
    fn state_round_trips() {
        let mut a = Opll::new();
        note(&mut a, 0, 1, 4, 0x180, 2);
        note(&mut a, 3, 0x0A, 5, 0x1C0, 4);
        write(&mut a, 0x00, 0x31);
        for _ in 0..5000 {
            a.clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Opll::new();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        for _ in 0..5000 {
            a.clock();
            b.clock();
            assert_eq!(a.output(), b.output());
        }
    }

    /// The patch-derived operator parameters are not in the file. A load rebuilds every one
    /// of them from the saved registers, bit for bit, before the chip is clocked again.
    #[test]
    fn load_rebuilds_the_derived_operator_parameters() {
        let mut a = Opll::new();
        for (reg, val) in [
            (0x00u8, 0x35u8),
            (0x01, 0x71),
            (0x02, 0x25),
            (0x03, 0x17),
            (0x04, 0xF8),
            (0x05, 0xE1),
            (0x06, 0x42),
            (0x07, 0x27),
        ] {
            write(&mut a, reg, val);
        }
        for ch in 0..6u8 {
            // Instrument 0 is the custom patch; the others come from the fixed ROM, and the
            // octaves put some channels above the 440 Hz key-scaling corner.
            note(&mut a, ch, ch, 4 + ch % 4, 0x0C0 + u16::from(ch) * 0x35, ch);
        }
        for _ in 0..5000 {
            a.clock();
        }

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Opll::new();
        b.load_state(&mut StateReader::new(&bytes)).unwrap();

        for ch in 0..6 {
            let (x, y) = (&a.channels[ch], &b.channels[ch]);
            for (op_a, op_b) in [(&x.modulator, &y.modulator), (&x.carrier, &y.carrier)] {
                assert_eq!(op_a.mult.to_bits(), op_b.mult.to_bits(), "ch {ch} mult");
                assert_eq!(op_a.ksl_db.to_bits(), op_b.ksl_db.to_bits(), "ch {ch} ksl");
                assert_eq!(
                    op_a.total_level_db.to_bits(),
                    op_b.total_level_db.to_bits(),
                    "ch {ch} total level"
                );
                assert_eq!(
                    op_a.attack_db.to_bits(),
                    op_b.attack_db.to_bits(),
                    "ch {ch} attack"
                );
                assert_eq!(
                    op_a.decay_db.to_bits(),
                    op_b.decay_db.to_bits(),
                    "ch {ch} decay"
                );
                assert_eq!(
                    op_a.release_db.to_bits(),
                    op_b.release_db.to_bits(),
                    "ch {ch} release"
                );
                assert_eq!(
                    op_a.sustain_db.to_bits(),
                    op_b.sustain_db.to_bits(),
                    "ch {ch} sustain"
                );
                assert_eq!(
                    op_a.feedback.to_bits(),
                    op_b.feedback.to_bits(),
                    "ch {ch} feedback"
                );
                assert_eq!(
                    (
                        op_a.hold_sustain,
                        op_a.half_wave,
                        op_a.tremolo,
                        op_a.vibrato
                    ),
                    (
                        op_b.hold_sustain,
                        op_b.half_wave,
                        op_b.tremolo,
                        op_b.vibrato
                    ),
                    "ch {ch} patch flags"
                );
            }
        }
        // And the sound from here on is bit-identical.
        for cycle in 0..5000 {
            a.clock();
            b.clock();
            assert_eq!(a.output().to_bits(), b.output().to_bits(), "cycle {cycle}");
        }
    }

    /// 64-bit FNV-1a over `bytes`, folded into `h`.
    fn fnv(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn next(seed: &mut u64) -> u32 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*seed >> 32) as u32
    }

    /// What the channel frequency feeds, after one internal sample: both operators' phases, plus
    /// the envelopes and the LFO phases. The vibrato LFO's `sin`/`powf` feed the vibrato operators'
    /// phases for good, so every transcendental here goes through the pure-Rust `libm` crate: the
    /// system libm's last bit differs on Apple Silicon (this pin was red on macOS until then).
    /// The output level is left out; it is not what this ratchet pins. A vibrato operator's phase
    /// is folded only while the LFOs are held.
    fn fold_sample(h: &mut u64, opll: &Opll) {
        fnv(h, &opll.tremolo_phase.to_bits().to_le_bytes());
        fnv(h, &opll.vibrato_phase.to_bits().to_le_bytes());
        let lfo_held = opll.test & 0x02 != 0;
        for c in &opll.channels {
            // the stored increment is the one `generate` computed inline before, bit for bit.
            let inline =
                49_716.0 * f32::from(c.fnum) / 2f32.powi(19 - i32::from(c.octave)) / SAMPLE_RATE;
            assert_eq!(
                c.base_inc.to_bits(),
                inline.to_bits(),
                "fnum {:#x} octave {}",
                c.fnum,
                c.octave
            );
            for op in [&c.modulator, &c.carrier] {
                fnv(h, &op.att.to_bits().to_le_bytes());
                fnv(h, &[op.stage as u8]);
                if !op.vibrato || lfo_held {
                    fnv(h, &op.phase.to_bits().to_le_bytes());
                }
            }
        }
    }

    /// Ratchet for the per-channel frequency increment: each of the 16 instruments on five
    /// channels (the sixth plays the custom patch throughout) through frequency and octave writes,
    /// an instrument change mid-note and back, random values in every custom-patch byte, the test
    /// register, sustain and key-off, a save state loaded into a fresh chip, re-triggers and the
    /// audio reset, folding [`fold_sample`] after every internal sample. Pinned on main before the
    /// increment was precomputed.
    #[test]
    fn phase_and_envelope_streams_are_pinned() {
        let mut got = [0u64; 16];
        for (inst, digest) in got.iter_mut().enumerate() {
            let inst = inst as u8;
            let mut h = 0xcbf2_9ce4_8422_2325_u64;
            let mut seed = 0x0418 + u64::from(inst);
            let mut opll = Opll::new();
            for phase in 0..13 {
                let s = &mut seed;
                match phase {
                    0 => {
                        for reg in 0x00..0x08 {
                            write(&mut opll, reg, next(s) as u8);
                        }
                        for ch in 0..6u8 {
                            let i = if ch == 5 { 0 } else { inst };
                            note(
                                &mut opll,
                                ch,
                                i,
                                (ch + inst) % 8,
                                next(s) as u16 & 0x1FF,
                                next(s) as u8 & 0x0F,
                            );
                        }
                    }
                    1 => (0..6).for_each(|ch| write(&mut opll, 0x10 + ch, next(s) as u8)),
                    // Trigger held: a new octave, fnum bit 8 and sustain without a key edge.
                    2 => (0..6)
                        .for_each(|ch| write(&mut opll, 0x20 + ch, 0x10 | (next(s) as u8 & 0x2F))),
                    3 => (0..5).for_each(|ch| {
                        write(
                            &mut opll,
                            0x30 + ch,
                            (((inst + 1 + ch) & 0x0F) << 4) | (next(s) as u8 & 0x0F),
                        );
                    }),
                    4 => (0..5).for_each(|ch| {
                        write(&mut opll, 0x30 + ch, (inst << 4) | (next(s) as u8 & 0x0F))
                    }),
                    5 => (0x00..0x08).for_each(|reg| write(&mut opll, reg, next(s) as u8)),
                    6 => write(&mut opll, 0x0F, 0x02),
                    7 => write(&mut opll, 0x0F, 0x0B),
                    8 => write(&mut opll, 0x0F, 0x04),
                    9 => {
                        write(&mut opll, 0x0F, 0x00);
                        for ch in (0..6).step_by(2) {
                            write(&mut opll, 0x20 + ch, next(s) as u8 & 0x2F);
                        }
                    }
                    10 => {
                        let mut w = StateWriter::new();
                        opll.save_state(&mut w);
                        let bytes = w.finish();
                        opll = Opll::new();
                        opll.load_state(&mut StateReader::new(&bytes)).unwrap();
                    }
                    11 => {
                        for ch in 0..6u8 {
                            let i = if ch == 5 { 0 } else { inst };
                            note(
                                &mut opll,
                                ch,
                                i,
                                next(s) as u8 % 8,
                                next(s) as u16 & 0x1FF,
                                next(s) as u8 & 0x0F,
                            );
                        }
                    }
                    _ => {
                        opll.reset();
                        for ch in 0..3u8 {
                            note(&mut opll, ch, inst, ch + 3, next(s) as u16 & 0x1FF, 0);
                        }
                    }
                }
                for _ in 0..1500 {
                    for _ in 0..36 {
                        opll.clock();
                    }
                    fold_sample(&mut h, &opll);
                }
            }
            *digest = h;
        }
        let want = [
            0x6ee7_1a4d_7dec_f2b9,
            0xe59e_e4b5_2e61_2b7f,
            0x43a0_d99c_79ec_eeb4,
            0x1d74_cc09_ee12_e6b2,
            0x0fc1_9673_b4ed_fe13,
            0xb996_cb10_e16a_ebfc,
            0xbdb0_7aa9_4ac7_438e,
            0xf487_1c83_3713_542c,
            0xd1c2_dadf_7f3d_6e74,
            0x9883_29b7_328a_a776,
            0x7018_7166_8b13_bc68,
            0xd47a_7336_f164_22d2,
            0x8576_a6a3_6dcd_23e7,
            0x014e_10a8_ac36_87f0,
            0x8725_fb38_835c_0b77,
            0xd78e_bd19_2e6f_d168,
        ];
        assert_eq!(got, want, "{got:#018x?}");
    }
}
