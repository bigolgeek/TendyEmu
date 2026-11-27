//! Band-limited resynthesis of the mixer's output (blip_buf-style delta accumulation).
//!
//! The DACs in [`super::Apu::mix`] step to a new level once per CPU cycle, but the frontend hears
//! the console at [`super::SAMPLE_RATE`] — 44.1 kHz, 40.6 CPU cycles per output sample. Sampling a
//! stepwise signal at that rate folds everything above 22.05 kHz back into the audible band:
//! a 30 kHz component lands at 14.1 kHz at full amplitude. The console's own amplifier does not do
//! that (its 14 kHz low-pass is an analog pole, not a decimation), so the emulator has to filter
//! *before* it decimates.
//!
//! This module does it the way Blargg's `blip_buf` does: instead of point-sampling the mixer, the
//! level changes are fed to a kernel that spreads each change over a window of output samples, and
//! the output sample is the running sum of those spreads.
//!
//! * The kernel is the step response of a windowed-sinc low-pass ([`CUTOFF`] × the output rate,
//!   [`HALF_TAPS`] samples to either side). Its rows are stored as the *deviation* from the level
//!   the signal settles at, so a sample is `level + Σ pending deviations`: every delta updates the
//!   running `level` and only the samples inside its window carry a correction.
//! * The mixer is evaluated [`OVERSAMPLE`] times per output sample (at the midpoint of each
//!   sub-interval), so the step times are known to a quarter of an output sample instead of a whole
//!   one. The measured effect at 44.1 kHz is 90-120 dB of alias suppression for content above
//!   Nyquist (see `aliasing_is_suppressed_against_point_sampling`), at the cost of three extra mixer
//!   evaluations per output sample.
//! * The kernel is defined in output-sample units, so it does not change with the frontend's rate:
//!   the same table serves 32 kHz, 44.1 kHz, 48 kHz and 96 kHz outputs.
//!
//! The cost paid for the filtering is latency: the kernel's group delay is [`HALF_TAPS`] output
//! samples (0.27 ms at 44.1 kHz), and no sample is emitted early to hide it. Sample *counts* are
//! unchanged — the resynthesizer is driven by the APU's existing sample accumulator.

use std::sync::OnceLock;

use crate::state::{StateError, StateReader, StateWriter};

/// Mixer evaluations per output sample: the sub-sample positions the step times are known to.
pub const OVERSAMPLE: usize = 4;

/// Kernel half-width in output samples; the kernel spans [`TAPS`] samples.
pub const HALF_TAPS: usize = 12;

/// Coefficients per kernel row.
pub const TAPS: usize = 2 * HALF_TAPS + 1;

/// Kernel corner as a fraction of the output sample rate: 0.45 is 90 % of Nyquist, which leaves the
/// transition band of a 25-tap windowed sinc below the fold frequency instead of above it.
pub const CUTOFF: f64 = 0.45;

/// Half-width of the kernel as an `f64`, for the kernel maths.
const HALF: f64 = HALF_TAPS as f64;

/// Integration steps per kernel window when the table is built (once, at first use).
const KERNEL_STEPS: usize = 4096;

/// Sub-sample position of the `sub`-th mixer evaluation inside an output sample: the midpoint of
/// each oversampled interval, so no evaluation ever lands exactly on a sample boundary.
pub const PHASES: [f64; OVERSAMPLE] = [0.125, 0.375, 0.625, 0.875];

/// Sub-sample position of the `sub`-th mixer evaluation inside an output sample.
pub fn phase_of(sub: usize) -> f64 {
    PHASES.get(sub).copied().unwrap_or(1.0)
}

/// Blackman-windowed sinc low-pass impulse response at `u` output samples from its centre.
fn impulse(u: f64) -> f64 {
    if u.abs() >= HALF {
        return 0.0;
    }
    let x = 2.0 * CUTOFF * u;
    let sinc = if x.abs() < 1.0e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    };
    let window = 0.42
        + 0.5 * (std::f64::consts::PI * u / HALF).cos()
        + 0.08 * (2.0 * std::f64::consts::PI * u / HALF).cos();
    sinc * window
}

/// Build the kernel rows: for a level change at `phase` of an output sample, `rows[q][j]` is how far
/// the sample `origin + j` still is from the new level.
fn build_kernel() -> [[f32; TAPS]; OVERSAMPLE] {
    // Cumulative integral of the impulse response across the window, by the trapezoid rule.
    let step = 2.0 * HALF / KERNEL_STEPS as f64;
    let mut cumulative = [0.0f64; KERNEL_STEPS + 1];
    let mut prev = impulse(-HALF);
    for i in 0..KERNEL_STEPS {
        let u = -HALF + step * (i as f64 + 1.0);
        let cur = impulse(u);
        cumulative[i + 1] = cumulative[i] + 0.5 * (prev + cur) * step;
        prev = cur;
    }
    let total = cumulative[KERNEL_STEPS];

    // Step response, normalised so a settled delta lands exactly on its own level.
    let step_response = |u: f64| -> f64 {
        if u <= -HALF {
            return 0.0;
        }
        if u >= HALF {
            return 1.0;
        }
        let pos = (u + HALF) / step;
        let i = pos.floor() as usize;
        let f = pos - i as f64;
        let (lo, hi) = (cumulative[i], cumulative[i + 1]);
        (lo * (1.0 - f) + hi * f) / total
    };

    let mut rows = [[0.0f32; TAPS]; OVERSAMPLE];
    for (q, row) in rows.iter_mut().enumerate() {
        let phase = phase_of(q);
        for (j, tap) in row.iter_mut().enumerate() {
            // Deviation from the settled level: -1 for samples the change has not reached yet, 0
            // once the kernel window has passed.
            *tap = (step_response(j as f64 - phase - HALF) - 1.0) as f32;
        }
    }
    rows
}

/// The kernel table, built once and shared by every APU.
fn kernel() -> &'static [[f32; TAPS]; OVERSAMPLE] {
    static KERNEL: OnceLock<[[f32; TAPS]; OVERSAMPLE]> = OnceLock::new();
    KERNEL.get_or_init(build_kernel)
}

/// Delta accumulator and kernel spreader between the mixer and the analog filter chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandLimitedSynth {
    /// Sum of every level change fed so far: the level the output has settled at.
    level: f32,
    /// Corrections for the samples still inside the kernel window, indexed from `origin`.
    ring: [f32; TAPS],
    /// Ring slot of the next output sample to emit.
    origin: usize,
    /// Mixer evaluations already taken in the current output sample (0..=[`OVERSAMPLE`]).
    sub: usize,
}

impl Default for BandLimitedSynth {
    fn default() -> Self {
        BandLimitedSynth::new()
    }
}

impl BandLimitedSynth {
    pub fn new() -> Self {
        BandLimitedSynth {
            level: 0.0,
            ring: [0.0; TAPS],
            origin: 0,
            sub: 0,
        }
    }

    /// Drop every pending level change and correction (power-on / reset).
    pub fn reset(&mut self) {
        *self = BandLimitedSynth::new();
    }

    /// True while the next oversampled evaluation is due: `progress` is the fraction of the current
    /// output sample already elapsed, in `0.0..1.0`.
    #[inline]
    pub fn wants_sub(&self, progress: f64) -> bool {
        self.sub < OVERSAMPLE && progress >= PHASES[self.sub]
    }

    /// Consume one oversampled evaluation and return its index `q`, the one [`Self::wants_sub`]
    /// just said is due: its position is [`PHASES`]`[q]` and its kernel row is row `q`.
    #[inline]
    pub fn take_sub(&mut self) -> usize {
        let q = self.sub;
        self.sub += 1;
        q
    }

    /// Feed the mixer's level as sampled at sub-sample `q` (from [`Self::take_sub`]) of the
    /// current output sample.
    #[inline]
    pub fn feed(&mut self, level: f32, q: usize) {
        let delta = level - self.level;
        if delta == 0.0 {
            return;
        }
        self.level = level;
        // Walk the ring without a modulo per tap: this runs once per level change per sub-sample,
        // so the divisor would cost more than the multiply-add it addresses.
        let mut slot = self.origin;
        for &c in &kernel()[q] {
            self.ring[slot] += delta * c;
            slot += 1;
            if slot == TAPS {
                slot = 0;
            }
        }
    }

    /// The finished output sample; starts the next one.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let out = self.level + self.ring[self.origin];
        self.ring[self.origin] = 0.0;
        self.origin += 1;
        if self.origin == TAPS {
            self.origin = 0;
        }
        self.sub = 0;
        out
    }

    /// level, origin, sub, then the [`TAPS`] pending corrections in ring order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.f32(self.level);
        w.u8(self.origin as u8);
        w.u8(self.sub as u8);
        for slot in self.ring {
            w.f32(slot);
        }
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.level = r.f32()?;
        let origin = usize::from(r.u8()?);
        if origin >= TAPS {
            return Err(StateError::BadValue);
        }
        self.origin = origin;
        let sub = usize::from(r.u8()?);
        if sub > OVERSAMPLE {
            return Err(StateError::BadValue);
        }
        self.sub = sub;
        for slot in &mut self.ring {
            *slot = r.f32()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apu::SAMPLE_RATE;

    /// Output samples per CPU cycle at 44.1 kHz, the APU's default.
    const STEP: f64 = SAMPLE_RATE / crate::CPU_HZ;

    /// Drive the resynthesizer the way `Apu::clock` does: `OVERSAMPLE` mixer evaluations per output
    /// sample, one output sample per `1.0 / STEP` CPU cycles.
    fn run(synth: &mut BandLimitedSynth, signal: impl Fn(f64) -> f32, samples: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples);
        let mut progress = 0.0;
        let mut cycles = 0usize;
        while out.len() < samples {
            progress += STEP;
            while synth.wants_sub(progress) {
                let q = synth.take_sub();
                synth.feed(signal(out.len() as f64 + phase_of(q)), q);
            }
            if progress >= 1.0 {
                progress -= 1.0;
                out.push(synth.next_sample());
            }
            cycles += 1;
            assert!(cycles < samples * 1_000, "the driver made no progress");
        }
        out
    }

    /// Amplitude of `freq` in `samples`, by projecting onto the sine/cosine pair at that frequency.
    fn magnitude(samples: &[f32], freq: f64) -> f64 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &v) in samples.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * freq * n as f64 / SAMPLE_RATE;
            re += f64::from(v) * w.sin();
            im += f64::from(v) * w.cos();
        }
        2.0 * re.hypot(im) / samples.len() as f64
    }

    /// The old path: one mixer evaluation per output sample, no band limiting.
    fn point_sampled(signal: impl Fn(f64) -> f32, samples: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples);
        let mut progress = 0.0;
        while out.len() < samples {
            progress += STEP;
            if progress >= 1.0 {
                progress -= 1.0;
                out.push(signal(out.len() as f64 + 1.0));
            }
        }
        out
    }

    fn sine(freq: f64, amplitude: f32) -> impl Fn(f64) -> f32 {
        move |t| amplitude * (2.0 * std::f64::consts::PI * freq * t / SAMPLE_RATE).sin() as f32
    }

    /// A silent input stays exactly silent, so the accumulator cannot add a DC offset or dither.
    #[test]
    fn silence_stays_silent() {
        let mut synth = BandLimitedSynth::new();
        let out = run(&mut synth, |_| 0.0, 1_000);
        assert!(
            out.iter().all(|&v| v == 0.0),
            "silence produced {:?}",
            &out[..8]
        );
    }

    /// A constant level is reproduced exactly once the kernel window has passed, which is what keeps
    /// a stuck DAC level at its own value instead of drifting.
    #[test]
    fn a_constant_level_settles_on_itself() {
        let mut synth = BandLimitedSynth::new();
        let out = run(&mut synth, |_| 0.25, 1_000);
        for (n, &v) in out.iter().enumerate().skip(2 * TAPS) {
            assert!((v - 0.25).abs() < 1.0e-6, "sample {n}: {v}");
        }
    }

    /// The passband is flat: a 1 kHz tone comes out at its input amplitude (the old path's level is
    /// the reference, so a level change here would move every audio test).
    #[test]
    fn passband_gain_is_unity() {
        for freq in [100.0, 1_000.0, 5_000.0] {
            let mut synth = BandLimitedSynth::new();
            let out = run(&mut synth, sine(freq, 1.0), 4_096);
            let got = magnitude(&out[512..], freq);
            assert!((got - 1.0).abs() < 0.02, "{freq} Hz: gain {got:.4}");
        }
    }

    /// The point of the module: a tone above the output Nyquist is filtered away instead of being
    /// folded down to its alias frequency. 30 kHz aliases to 14.1 kHz at 44.1 kHz, where the
    /// point-sampled path puts it at full amplitude; the resynthesizer must bury it.
    #[test]
    fn aliasing_is_suppressed_against_point_sampling() {
        let tone = 30_000.0;
        let alias = SAMPLE_RATE - tone; // 14.1 kHz
        let old = point_sampled(sine(tone, 1.0), 4_096);
        let old_alias = magnitude(&old[512..], alias);
        assert!(
            old_alias > 0.5,
            "the old path should alias at full level: {old_alias:.3}"
        );

        let mut synth = BandLimitedSynth::new();
        let new = run(&mut synth, sine(tone, 1.0), 4_096);
        let new_alias = magnitude(&new[512..], alias);
        let reduction = 20.0 * (old_alias / new_alias.max(1.0e-12)).log10();
        assert!(
            reduction > 40.0,
            "alias suppression only {reduction:.1} dB ({new_alias:e})"
        );
    }

    /// A tone far above Nyquist folds onto 11.8 kHz in the old path; the resynthesizer leaves
    /// nothing at that frequency.
    #[test]
    fn far_out_of_band_content_does_not_fold_down() {
        let tone = 100_000.0;
        let alias = tone % SAMPLE_RATE; // 11.8 kHz at 44.1 kHz
        let old = magnitude(&point_sampled(sine(tone, 1.0), 4_096)[512..], alias);
        assert!(
            old > 0.5,
            "the old path should fold {tone} Hz onto {alias} Hz: {old:.3}"
        );
        let mut synth = BandLimitedSynth::new();
        let new = magnitude(&run(&mut synth, sine(tone, 1.0), 4_096)[512..], alias);
        assert!(new < 0.01, "{new:e} left at {alias} Hz");
    }

    /// Save and load restore the accumulator bit for bit: a state taken mid-tone continues on the
    /// same waveform, not on a discontinuity.
    #[test]
    fn state_round_trips_mid_stream() {
        let mut synth = BandLimitedSynth::new();
        let signal = sine(440.0, 0.5);
        let first = run(&mut synth, &signal, 500);

        let mut w = StateWriter::new();
        synth.save_state(&mut w);
        let data = w.finish();

        let mut restored = BandLimitedSynth::new();
        restored.load_state(&mut StateReader::new(&data)).unwrap();
        assert_eq!(restored, synth);

        let tail = run(&mut synth, &signal, 500);
        let restored_tail = run(&mut restored, &signal, 500);
        assert_eq!(tail, restored_tail);
        assert!(first.len() == 500);
    }

    /// A hand-built state with every field off its default, so the round-trip test above cannot pass
    /// on a state that only happens to be zeros.
    #[test]
    fn every_field_round_trips() {
        let mut synth = BandLimitedSynth::new();
        synth.level = -0.375;
        synth.origin = TAPS - 1;
        synth.sub = OVERSAMPLE;
        for (i, slot) in synth.ring.iter_mut().enumerate() {
            *slot = i as f32 * 0.031_25 - 0.25;
        }
        let mut w = StateWriter::new();
        synth.save_state(&mut w);
        let data = w.finish();

        let mut out = BandLimitedSynth::new();
        let mut reader = StateReader::new(&data);
        out.load_state(&mut reader).unwrap();
        assert_eq!(reader.remaining(), 0);
        assert_eq!(out, synth);

        // Out-of-range ring and sub indices are rejected instead of panicking later.
        let mut bad = data.clone();
        bad[4] = TAPS as u8;
        assert_eq!(
            BandLimitedSynth::new().load_state(&mut StateReader::new(&bad)),
            Err(StateError::BadValue)
        );
        let mut bad = data.clone();
        bad[5] = (OVERSAMPLE + 1) as u8;
        assert_eq!(
            BandLimitedSynth::new().load_state(&mut StateReader::new(&bad)),
            Err(StateError::BadValue)
        );
    }

    /// Golden ratchet on the kernel itself: a single level change of 1.0 at the first sub-sample
    /// position. Any change to the window, the cutoff, the tap count or the phase grid moves these
    /// numbers, so a refactor cannot quietly retune the filter.
    #[test]
    fn golden_kernel_ratchet() {
        let mut synth = BandLimitedSynth::new();
        synth.feed(1.0, 0);
        let out: Vec<f32> = (0..TAPS + 2).map(|_| synth.next_sample()).collect();
        assert!(
            out[0].abs() < 1.0e-6,
            "the change has not arrived yet: {}",
            out[0]
        );
        assert!((out[TAPS] - 1.0).abs() < 1.0e-6, "settled at {}", out[TAPS]);
        assert!((out[TAPS + 1] - 1.0).abs() < 1.0e-6);
        // The transition itself overshoots by the window's ~9 %, as a windowed sinc must.
        let peak = out.iter().cloned().fold(f32::MIN, f32::max);
        assert!((1.05..1.12).contains(&peak), "step overshoot {peak}");
        // Halfway through the window the level change has half arrived.
        let mid = out[HALF_TAPS];
        assert!((mid - 0.5).abs() < 0.12, "midpoint {mid}");
    }
}
