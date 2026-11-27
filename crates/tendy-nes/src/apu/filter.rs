//! The console's analog output filter chain: two one-pole high-passes and one one-pole low-pass.
//!
//! The 2A03's DAC output never reaches a speaker directly — it leaves the console through an
//! analog network: a DC-blocking output capacitor, the mixer stage's AC coupling and the
//! amplifier's high-frequency roll-off. Emulators that model it (RustyNES' analog filter chain,
//! Nestopia UE's "analog filter" option) use the same three corners:
//!
//! * high-pass at [`HIGH_PASS_1_HZ`] (90 Hz) — the output coupling capacitor,
//! * high-pass at [`HIGH_PASS_2_HZ`] (440 Hz) — the mixer stage's coupling,
//! * low-pass at [`LOW_PASS_HZ`] (14 kHz) — the amplifier's bandwidth.
//!
//! Each stage is a one-pole section, so the chain is 6 dB/octave outside its passband. The
//! coefficients come from the bilinear transform with prewarping:
//!
//! ```text
//! k  = tan(pi * fc / fs)
//! b0 = k / (1 + k)
//! a1 = (k - 1) / (1 + k)
//! ```
//!
//! which puts the -3 dB point exactly at `fc` at *any* output sample rate, unlike the plain
//! `RC/(RC + dt)` coefficient whose corner creeps as `fc` approaches Nyquist. That is what makes
//! the chain sample-rate independent: the same 90 Hz / 440 Hz / 14 kHz corners hold whether the
//! frontend asks for 44.1 kHz, 48 kHz or 96 kHz.

/// Output-section DC block corner, in Hz.
pub const HIGH_PASS_1_HZ: f64 = 90.0;

/// Mixer-stage coupling corner, in Hz.
pub const HIGH_PASS_2_HZ: f64 = 440.0;

/// Amplifier bandwidth corner, in Hz.
pub const LOW_PASS_HZ: f64 = 14_000.0;

/// Highest fraction of the sample rate a corner is allowed to sit at. `tan` diverges as a corner
/// approaches Nyquist, so at a low output rate the low-pass corner is pulled down to
/// `MAX_CORNER_FRACTION * sample_rate` and the section keeps its one-pole shape instead of
/// degenerating into a division by zero.
const MAX_CORNER_FRACTION: f64 = 0.45;

/// Bilinear prewarp term `k = tan(pi * fc / fs)`, with the corner clamped below Nyquist.
fn prewarp(cutoff_hz: f64, sample_rate: f64) -> f32 {
    let fc = cutoff_hz.min(sample_rate * MAX_CORNER_FRACTION).max(1.0e-3);
    (std::f64::consts::PI * fc / sample_rate).tan() as f32
}

/// A usable output rate: anything non-finite or non-positive falls back to the APU's default
/// (44.1 kHz).
fn sane_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        super::SAMPLE_RATE
    }
}

/// One-pole RC section: `y[n] = b0 * (x[n] + in_sign * x[n-1]) - a1 * y[n-1]`.
///
/// High-pass and low-pass differ only in `b0` and in the sign of the input tap (`-1.0` high-pass,
/// `+1.0` low-pass). A product with exactly `+-1.0` is exact, so `x + -1.0 * p` is `x - p` bit for
/// bit: one type, the same samples as the two it replaced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnePole {
    b0: f32,
    a1: f32,
    prev_in: f32,
    prev_out: f32,
    in_sign: f32,
}

impl OnePole {
    /// A high-pass with its corner at `cutoff_hz` for an output rate of `sample_rate`.
    pub fn high_pass(cutoff_hz: f64, sample_rate: f64) -> Self {
        Self::new(-1.0, cutoff_hz, sample_rate)
    }

    /// A low-pass with its corner at `cutoff_hz` for an output rate of `sample_rate`.
    pub fn low_pass(cutoff_hz: f64, sample_rate: f64) -> Self {
        Self::new(1.0, cutoff_hz, sample_rate)
    }

    fn new(in_sign: f32, cutoff_hz: f64, sample_rate: f64) -> Self {
        let mut section = OnePole {
            b0: 0.0,
            a1: 0.0,
            prev_in: 0.0,
            prev_out: 0.0,
            in_sign,
        };
        section.set_cutoff(cutoff_hz, sample_rate);
        section
    }

    /// Re-derive the coefficients, keeping the running state (a rate change must not click).
    pub fn set_cutoff(&mut self, cutoff_hz: f64, sample_rate: f64) {
        let k = prewarp(cutoff_hz, sane_rate(sample_rate));
        // High-pass `H(z) = 1/(1+k) * (1 - z^-1) / (1 + (k-1)/(k+1) z^-1)`: unity at Nyquist, a
        // zero at DC. Low-pass `H(z) = k/(1+k) * (1 + z^-1) / (1 + (k-1)/(k+1) z^-1)`: unity at
        // DC, a zero at Nyquist.
        let num = if self.in_sign < 0.0 { 1.0 } else { k };
        self.b0 = num / (1.0 + k);
        self.a1 = (k - 1.0) / (1.0 + k);
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * (x + self.in_sign * self.prev_in) - self.a1 * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        y
    }

    /// Drop the running state (power-on / reset); the coefficients stay.
    pub fn reset(&mut self) {
        self.prev_in = 0.0;
        self.prev_out = 0.0;
    }

    /// `(prev_in, prev_out)` — the input and output history this stage keeps.
    pub fn taps(&self) -> (f32, f32) {
        (self.prev_in, self.prev_out)
    }

    pub fn set_taps(&mut self, prev_in: f32, prev_out: f32) {
        self.prev_in = prev_in;
        self.prev_out = prev_out;
    }
}

/// The three analog sections in signal order: high-pass 90 Hz, high-pass 440 Hz, low-pass 14 kHz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterChain {
    /// Output coupling high-pass ([`HIGH_PASS_1_HZ`]).
    pub hp1: OnePole,
    /// Mixer-stage high-pass ([`HIGH_PASS_2_HZ`]).
    pub hp2: OnePole,
    /// Amplifier low-pass ([`LOW_PASS_HZ`]).
    pub lp: OnePole,
    /// The output rate the coefficients were derived for.
    rate: f64,
}

impl FilterChain {
    /// A chain with every corner derived for `sample_rate`.
    pub fn new(sample_rate: f64) -> Self {
        let rate = sane_rate(sample_rate);
        FilterChain {
            hp1: OnePole::high_pass(HIGH_PASS_1_HZ, rate),
            hp2: OnePole::high_pass(HIGH_PASS_2_HZ, rate),
            lp: OnePole::low_pass(LOW_PASS_HZ, rate),
            rate,
        }
    }

    /// Re-derive every corner for `sample_rate`; a no-op when the rate has not changed, so the
    /// APU can call this on the sample tick and follow a frontend that writes `sample_rate`.
    pub fn set_rate(&mut self, sample_rate: f64) {
        let rate = sane_rate(sample_rate);
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        self.hp1.set_cutoff(HIGH_PASS_1_HZ, rate);
        self.hp2.set_cutoff(HIGH_PASS_2_HZ, rate);
        self.lp.set_cutoff(LOW_PASS_HZ, rate);
    }

    /// The output rate the coefficients are currently derived for.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// One sample through the whole chain: DC block, mixer coupling, amplifier roll-off.
    pub fn process(&mut self, x: f32) -> f32 {
        self.lp.process(self.hp2.process(self.hp1.process(x)))
    }

    /// Drop every section's running state, keeping the corner coefficients.
    pub fn reset(&mut self) {
        self.hp1.reset();
        self.hp2.reset();
        self.lp.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apu::Apu;

    const FS: f64 = 44_100.0;

    /// -3 dB, the gain a one-pole section has at its corner.
    const THREE_DB: f64 = std::f64::consts::FRAC_1_SQRT_2;

    /// Steady-state gain of a linear stage at `freq`, measured by projecting the settled output
    /// onto the input sine/cosine pair (exact for a sinusoid, unlike a peak-to-peak estimate).
    fn response(mut stage: impl FnMut(f32) -> f32, freq: f64, rate: f64) -> f64 {
        let n = (rate * 0.5) as usize;
        let skip = n / 2;
        let (mut re, mut im, mut count) = (0.0f64, 0.0f64, 0usize);
        for i in 0..n {
            let w = 2.0 * std::f64::consts::PI * freq * (i as f64 / rate);
            let y = f64::from(stage(w.sin() as f32));
            if i >= skip {
                re += y * w.sin();
                im += y * w.cos();
                count += 1;
            }
        }
        2.0 * (re * re + im * im).sqrt() / count as f64
    }

    fn assert_gain(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{what}: gain {actual:.4}, want {expected:.4}"
        );
    }

    /// Closed-form magnitude of the documented bilinear section at `freq`, computed in f64: the
    /// independent reference the measured gains are checked against.
    fn bilinear_gain(kind: char, freq: f64, corner: f64, rate: f64) -> f64 {
        let k = (std::f64::consts::PI * corner.min(rate * MAX_CORNER_FRACTION) / rate).tan();
        let b0 = if kind == 'h' {
            1.0 / (1.0 + k)
        } else {
            k / (1.0 + k)
        };
        let a1 = (k - 1.0) / (1.0 + k);
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let (sin, cos) = w.sin_cos();
        // Numerator `1 - z^-1` (high-pass) or `1 + z^-1` (low-pass) over `1 + a1 * z^-1`.
        let (num_re, num_im) = if kind == 'h' {
            (1.0 - cos, sin)
        } else {
            (1.0 + cos, -sin)
        };
        let (den_re, den_im) = (1.0 + a1 * cos, -a1 * sin);
        b0 * num_re.hypot(num_im) / den_re.hypot(den_im)
    }

    /// Each section is -3 dB at its own corner: the property that defines the corner.
    #[test]
    fn every_section_is_three_db_down_at_its_corner() {
        let mut hp1 = OnePole::high_pass(HIGH_PASS_1_HZ, FS);
        assert_gain(
            response(|x| hp1.process(x), HIGH_PASS_1_HZ, FS),
            THREE_DB,
            "hp1 @ 90 Hz",
        );

        let mut hp2 = OnePole::high_pass(HIGH_PASS_2_HZ, FS);
        assert_gain(
            response(|x| hp2.process(x), HIGH_PASS_2_HZ, FS),
            THREE_DB,
            "hp2 @ 440 Hz",
        );

        let mut lp = OnePole::low_pass(LOW_PASS_HZ, FS);
        assert_gain(
            response(|x| lp.process(x), LOW_PASS_HZ, FS),
            THREE_DB,
            "lp @ 14 kHz",
        );
    }

    /// One octave out is -3 dB more (0.4472) for a 6 dB/octave high-pass, and a decade out is
    /// within 0.5 dB of unity on the passband side. The low-pass keeps its passband and rolls the
    /// stopband off hard (its bilinear zero at Nyquist steepens it near the top of the band).
    #[test]
    fn every_section_rolls_off_six_db_per_octave() {
        let mut hp1 = OnePole::high_pass(HIGH_PASS_1_HZ, FS);
        assert_gain(
            response(|x| hp1.process(x), HIGH_PASS_1_HZ / 2.0, FS),
            0.4472,
            "hp1 @ 45 Hz",
        );
        assert_gain(
            response(|x| hp1.process(x), HIGH_PASS_1_HZ * 10.0, FS),
            0.9950,
            "hp1 @ 900 Hz",
        );

        let mut hp2 = OnePole::high_pass(HIGH_PASS_2_HZ, FS);
        assert_gain(
            response(|x| hp2.process(x), HIGH_PASS_2_HZ / 2.0, FS),
            0.4472,
            "hp2 @ 220 Hz",
        );
        assert_gain(
            response(|x| hp2.process(x), HIGH_PASS_2_HZ * 10.0, FS),
            0.9950,
            "hp2 @ 4.4 kHz",
        );

        let mut lp = OnePole::low_pass(LOW_PASS_HZ, FS);
        let passband = response(|x| lp.process(x), LOW_PASS_HZ / 4.0, FS);
        assert!(passband > 0.95, "lp @ 3.5 kHz: {passband:.4}");
        let stopband = response(|x| lp.process(x), 20_000.0, FS);
        assert!(stopband < 0.35, "lp @ 20 kHz: {stopband:.4}");
    }

    /// The measured response tracks the closed-form bilinear response of the documented
    /// difference equation across the band — an independent f64 reference, not the implementation
    /// reading itself back.
    #[test]
    fn measured_response_matches_the_bilinear_reference() {
        for freq in [110.0, 440.0, 1_000.0, 4_000.0] {
            let mut hp = OnePole::high_pass(HIGH_PASS_2_HZ, FS);
            let got = response(|x| hp.process(x), freq, FS);
            assert_gain(
                got,
                bilinear_gain('h', freq, HIGH_PASS_2_HZ, FS),
                &format!("hp2 @ {freq} Hz"),
            );
        }

        for freq in [1_000.0, 7_000.0, 14_000.0, 20_000.0] {
            let mut lp = OnePole::low_pass(LOW_PASS_HZ, FS);
            let got = response(|x| lp.process(x), freq, FS);
            assert_gain(
                got,
                bilinear_gain('l', freq, LOW_PASS_HZ, FS),
                &format!("lp @ {freq} Hz"),
            );
        }
    }

    /// The corners follow the sample rate: -3 dB at 90/440/14000 Hz at any rate where the
    /// low-pass corner still fits under Nyquist.
    #[test]
    fn corners_hold_across_output_sample_rates() {
        for rate in [32_000.0, 44_100.0, 48_000.0, 96_000.0] {
            let mut chain = FilterChain::new(rate);
            assert_gain(
                response(|x| chain.hp1.process(x), HIGH_PASS_1_HZ, rate),
                THREE_DB,
                &format!("hp1 @ {rate}"),
            );
            assert_gain(
                response(|x| chain.hp2.process(x), HIGH_PASS_2_HZ, rate),
                THREE_DB,
                &format!("hp2 @ {rate}"),
            );
            assert_gain(
                response(|x| chain.lp.process(x), LOW_PASS_HZ, rate),
                THREE_DB,
                &format!("lp @ {rate}"),
            );
        }
    }

    /// The passband gain is the same at every supported rate, which is what "sample-rate
    /// independent" means for the audio path: changing the frontend's output device must not
    /// change the tone.
    #[test]
    fn passband_gain_is_sample_rate_independent() {
        let gain_at = |rate: f64| {
            let mut chain = FilterChain::new(rate);
            response(|x| chain.process(x), 1_000.0, rate)
        };
        let base = gain_at(44_100.0);
        for rate in [8_000.0, 22_050.0, 32_000.0, 48_000.0, 96_000.0] {
            let got = gain_at(rate);
            assert!(
                (got - base).abs() < 0.01,
                "{rate} Hz output: {got:.4} vs {base:.4}"
            );
        }
    }

    /// A low-pass corner above Nyquist is clamped, not allowed to blow up: an 8 kHz output still
    /// filters sensibly and never produces a NaN or an unstable section.
    #[test]
    fn low_rates_clamp_the_low_pass_below_nyquist() {
        let rate = 8_000.0;
        let clamped = rate * MAX_CORNER_FRACTION;
        let mut lp = OnePole::low_pass(LOW_PASS_HZ, rate);
        assert_gain(
            response(|x| lp.process(x), clamped, rate),
            THREE_DB,
            "lp @ clamped corner",
        );
        assert_gain(response(|x| lp.process(x), 100.0, rate), 1.0, "lp @ 100 Hz");

        let mut chain = FilterChain::new(rate);
        for i in 0..1000 {
            let y = chain.process(if i % 2 == 0 { 1.0 } else { -1.0 });
            assert!(y.is_finite(), "sample {i} is {y}");
        }
    }

    /// The chain blocks DC (both high-passes) and its passband gain is the product of its three
    /// sections — the sections compose in series, in the documented order.
    #[test]
    fn chain_blocks_dc_and_composes_its_sections() {
        let mut chain = FilterChain::new(FS);
        let mut y = 0.0;
        for _ in 0..(FS as usize / 2) {
            y = chain.process(1.0);
        }
        assert!(y.abs() < 1e-3, "DC feed settled at {y}");

        for freq in [500.0, 1_000.0, 3_000.0] {
            let mut chain = FilterChain::new(FS);
            let mut hp1 = OnePole::high_pass(HIGH_PASS_1_HZ, FS);
            let mut hp2 = OnePole::high_pass(HIGH_PASS_2_HZ, FS);
            let mut lp = OnePole::low_pass(LOW_PASS_HZ, FS);
            let whole = response(|x| chain.process(x), freq, FS);
            let parts = response(|x| hp1.process(x), freq, FS)
                * response(|x| hp2.process(x), freq, FS)
                * response(|x| lp.process(x), freq, FS);
            assert!(
                (whole - parts).abs() < 1e-3,
                "{freq} Hz: chain {whole:.5} vs sections {parts:.5}"
            );
        }
    }

    /// A zero input produces exactly zero output, which is what keeps silence silent.
    #[test]
    fn silence_stays_silent() {
        let mut chain = FilterChain::new(FS);
        for _ in 0..1024 {
            assert_eq!(chain.process(0.0), 0.0);
        }
    }

    /// Golden ratchet: the first samples of a unit impulse through the chain at 44.1 kHz. Any
    /// change to a corner, a coefficient or the section order moves these numbers, so a refactor
    /// cannot quietly retune the hardware model.
    #[test]
    fn golden_impulse_response_ratchet() {
        let mut chain = FilterChain::new(FS);
        let out: Vec<f64> = (0..8)
            .map(|i| f64::from(chain.process(if i == 0 { 1.0 } else { 0.0 })))
            .collect();
        let golden: [f64; 8] = [
            0.585_357_4,
            0.416_359_5,
            -0.172_973_0,
            -0.040_910_8,
            -0.064_388_9,
            -0.054_702_9,
            -0.052_436_8,
            -0.048_841_0,
        ];
        for (i, (got, want)) in out.iter().zip(golden.iter()).enumerate() {
            assert!(
                (got - want).abs() < 1e-6,
                "sample {i}: {got} want {want} (all: {out:?})"
            );
        }
    }

    /// Sample-identity ratchet: a fixed pseudo-random input through each section on its own
    /// and through the whole chain, at a clamped low rate and three common ones, with a retune and
    /// a tap write mid-stream and a reset at the end, folded bit for bit into one FNV-1a digest.
    /// Taken on main before the two section types were merged; a refactor must not move it.
    #[test]
    fn section_streams_are_bit_identical() {
        let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fold = |y: f32| {
            for b in y.to_bits().to_le_bytes() {
                digest ^= u64::from(b);
                digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        let mut seed = 0x0411_u64;
        for rate in [8_000.0, 44_100.0, 48_000.0, 96_000.0] {
            let mut chain = FilterChain::new(rate);
            for i in 0..4096 {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let x = (seed >> 40) as f32 / (1 << 23) as f32 - 1.0;
                fold(chain.hp1.process(x));
                fold(chain.hp2.process(x));
                fold(chain.lp.process(x));
                fold(chain.process(x));
                if i == 2048 {
                    chain.set_rate(rate * 1.5);
                    chain.lp.set_taps(0.25, -0.125);
                }
            }
            chain.reset();
            fold(chain.process(1.0));
        }
        assert_eq!(
            digest, 0xb83c_eae2_16cf_a393,
            "section stream digest {digest:#018x}"
        );
    }

    /// Resetting drops the running state: no DC offset or ringing survives a reset mid-tone.
    #[test]
    fn reset_clears_every_section() {
        let mut chain = FilterChain::new(FS);
        for i in 0..1000 {
            chain.process(if i % 2 == 0 { 0.5 } else { -0.5 });
        }
        chain.reset();
        assert_eq!(chain.hp1.taps(), (0.0, 0.0));
        assert_eq!(chain.hp2.taps(), (0.0, 0.0));
        assert_eq!(chain.lp.taps(), (0.0, 0.0));
        assert_eq!(chain.process(0.0), 0.0);
    }

    /// A rate change keeps the coefficients valid (the new corner is the one in force) and keeps
    /// the running state, so switching output device mid-session does not click.
    #[test]
    fn set_rate_retunes_the_chain() {
        let mut chain = FilterChain::new(FS);
        chain.set_rate(96_000.0);
        assert_eq!(chain.rate(), 96_000.0);
        assert_gain(
            response(|x| chain.hp1.process(x), HIGH_PASS_1_HZ, 96_000.0),
            THREE_DB,
            "hp1 after retune",
        );

        let before = chain.hp1.taps();
        chain.set_rate(96_000.0);
        assert_eq!(
            chain.hp1.taps(),
            before,
            "a same-rate set must not reset the state"
        );

        // Nonsense rates fall back to the default instead of poisoning the coefficients.
        chain.set_rate(f64::NAN);
        assert_eq!(chain.rate(), super::super::SAMPLE_RATE);
        assert!(chain.process(0.5).is_finite());
    }

    /// The reset button clears the chain's history, so a soft reset cannot leave a DC transient
    /// in the first samples of the new session.
    #[test]
    fn soft_reset_clears_the_filter_chain() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4000, 0xBF);
        apu.write_register(0x4002, 253);
        apu.write_register(0x4003, 0x08);
        for _ in 0..10_000 {
            apu.clock();
        }
        assert_ne!(
            apu.filter.hp2.taps(),
            (0.0, 0.0),
            "the tone should have charged the chain"
        );

        apu.soft_reset();
        assert_eq!(apu.filter.hp1.taps(), (0.0, 0.0));
        assert_eq!(apu.filter.hp2.taps(), (0.0, 0.0));
        assert_eq!(apu.filter.lp.taps(), (0.0, 0.0));
    }

    /// The APU follows a frontend that sets `sample_rate` as a plain field, not only through
    /// `set_sample_rate`: the chain retunes on the next sample tick.
    #[test]
    fn apu_follows_a_direct_sample_rate_write() {
        let mut apu = Apu::new();
        apu.sample_rate = 48_000.0;
        while apu.samples.is_empty() {
            apu.clock();
        }
        assert_eq!(apu.filter.rate(), 48_000.0);
    }
}
