use tendy_nes::apu::Apu;
use tendy_nes::apu::gain::{ChannelGain, ChannelPan};
use tendy_nes::nes::Nes;

use crate::common::rom_file;
use crate::common::run_blargg;

/// One second of APU output after a setup function.
fn one_second(setup: fn(&mut Apu)) -> Vec<f32> {
    let mut a = Apu::new();
    setup(&mut a);
    for _ in 0..1_789_773 {
        a.clock();
    }
    a.samples
}

#[test]
fn mixer_matches_formula() {
    let mut a = Apu::new();
    assert!((a.mix() - 0.246_412).abs() < 1e-5);
    a.dmc.write(1, 0x7F, false);
    assert!((a.mix() - 0.681_32).abs() < 1e-4);
}

#[test]
fn outputs_44100_samples_per_second() {
    let s = one_second(|_| {});
    assert!((44_098..=44_100).contains(&s.len()));
}

/// The output cadence belongs to the sample accumulator alone: the band-limited resynthesizer between
/// the mixer and the analog chain must not add, drop or shift samples. 120 frames of a fixed ROM
/// produce this many output samples, the count main produced before the resynthesizer existed.
#[test]
fn a_frame_produces_the_golden_number_of_samples() {
    let mut nes = Nes::new(&rom_file("01.basics.nes")).unwrap();
    let mut total = 0;
    for _ in 0..120 {
        nes.run_frame();
        total += std::mem::take(&mut nes.bus.apu.samples).len();
    }
    assert_eq!(total, 87_996);
}

#[test]
fn pulse_tone_is_440_hz() {
    // Pulse 1, constant volume 15, duty 50%, timer period 253: 1789773 / (16 * 254) = 440.4 Hz.
    let s = one_second(|a| {
        a.write_register(0x4015, 0x01);
        a.write_register(0x4000, 0xBF);
        a.write_register(0x4002, 253);
        a.write_register(0x4003, 0x08);
    });
    let s = &s[4410..];
    let rising = s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    let hz = rising as f64 / (s.len() as f64 / 44_100.0);
    assert!((hz - 440.4).abs() < 2.0, "{hz} Hz");
}

#[test]
fn high_pass_removes_dc() {
    let s = one_second(|_| {});
    assert!(s[s.len() - 1].abs() < 1e-3);
}

/// A constant DMC level is a DC input to the mixer: the analog chain's high-passes must take it to
/// zero, which is what stops a stuck level from stepping the speaker cone.
#[test]
fn a_constant_dmc_level_is_blocked_at_the_output() {
    let mut a = Apu::new();
    a.dmc.write(1, 0x7F, false);
    assert!(
        a.mix() > 0.5,
        "the DC source should be loud before filtering"
    );
    for _ in 0..1_789_773 {
        a.clock();
    }
    let last = *a.samples.last().unwrap();
    assert!(last.abs() < 1e-3, "DC survived the chain: {last}");
}

/// RMS of the settled half of one second of the 440 Hz pulse tone at output rate `rate`.
fn tone_rms(rate: f64) -> f32 {
    let mut a = Apu::new();
    a.set_sample_rate(rate);
    a.write_register(0x4015, 0x01);
    a.write_register(0x4000, 0xBF);
    a.write_register(0x4002, 253);
    a.write_register(0x4003, 0x08);
    for _ in 0..1_789_773 {
        a.clock();
    }
    let tail = &a.samples[a.samples.len() / 2..];
    (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt()
}

/// The analog chain's corners come from the output rate, so the level of the same tone does not
/// change when the frontend opens a device at a different rate.
#[test]
fn output_level_is_sample_rate_independent() {
    let base = tone_rms(44_100.0);
    assert!(base > 0.01, "the tone should be audible: {base}");
    for rate in [22_050.0, 48_000.0, 96_000.0] {
        let got = tone_rms(rate);
        assert!(
            (got - base).abs() / base < 0.02,
            "{rate} Hz output: {got} vs {base}"
        );
    }
}

/// A per-channel gain multiplies that channel's DAC level before the nonlinear mixer, so an
/// explicit `1.0` is exactly the console's own mix (bit for bit) and `0.0` removes exactly the one
/// channel — the stream matches a console whose pulse 1 never played at all.
#[test]
fn per_channel_gain_zero_silences_and_unity_is_byte_identical() {
    /// One fifth of a second of the 440 Hz pulse-1 tone, with or without pulse 1 started.
    fn tone(gain: ChannelGain, pulse: bool) -> Vec<f32> {
        let mut a = Apu::new();
        a.gain = gain;
        if pulse {
            a.write_register(0x4015, 0x01);
            a.write_register(0x4000, 0xBF);
            a.write_register(0x4002, 253);
            a.write_register(0x4003, 0x08);
        }
        for _ in 0..200_000 {
            a.clock();
        }
        a.samples
    }

    let bits = |samples: &[f32]| samples.iter().map(|s| s.to_bits()).collect::<Vec<u32>>();
    let default = tone(ChannelGain::default(), true);
    assert!(
        default.iter().any(|s| *s != 0.0),
        "the 440 Hz pulse must be audible"
    );

    let mut explicit = ChannelGain::default();
    explicit
        .apply_list("pulse1=1.0,pulse2=1,triangle=1,noise=1,dmc=1,expansion=1")
        .unwrap();
    assert!(explicit.is_unity());
    assert!(
        bits(&tone(explicit, true)) == bits(&default),
        "an explicit 1.0 must not move a bit"
    );

    // The triangle's sequencer free-runs even with no register written, so the "no pulse" console
    // still has its DAC level in the mix — and the two streams must match exactly, baseline and all.
    let no_pulse = ChannelGain {
        pulse1: 0.0,
        ..ChannelGain::default()
    };
    assert!(
        bits(&tone(no_pulse, true)) == bits(&tone(ChannelGain::default(), false)),
        "pulse 1 at gain 0 must be exactly a console whose pulse 1 never played"
    );
}

#[test]
fn blargg_dmc_basics() {
    let (status, text) = run_blargg("7-dmc_basics.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_dmc_rates() {
    let (status, text) = run_blargg("8-dmc_rates.nes", 600);
    assert_eq!(status, 0, "{text}");
}

// ---------------------------------------------------------------- Stereo mix and per-channel pan

/// One fifth of a second of APU output with `pan` in force, as `(left, right)`.
fn panned(pan: ChannelPan, setup: fn(&mut Apu)) -> (Vec<f32>, Vec<f32>) {
    let mut a = Apu::new();
    a.pan = pan;
    setup(&mut a);
    for _ in 0..200_000 {
        a.clock();
    }
    (a.samples, a.samples_right)
}

/// Pulse 1 alone: constant volume 15, 50 % duty, timer 253 (440 Hz), length loaded.
fn pulse1_tone(a: &mut Apu) {
    a.write_register(0x4015, 0x01);
    a.write_register(0x4000, 0xBF);
    a.write_register(0x4002, 253);
    a.write_register(0x4003, 0x08);
}

/// Both pulses, at different pitches, so each one is separately identifiable in a mix. Periods 253
/// and 123 are 440 Hz and ~900 Hz; only the timer's low byte is written, as `$4006` takes.
fn two_pulse_tones(a: &mut Apu) {
    a.write_register(0x4015, 0x03);
    a.write_register(0x4000, 0xBF);
    a.write_register(0x4002, 253);
    a.write_register(0x4003, 0x08);
    a.write_register(0x4004, 0xBF);
    a.write_register(0x4006, 123);
    a.write_register(0x4007, 0x08);
}

/// The console with `setup` run and its pulse 1 at gain 0: the reference stream for "no pulse-1
/// energy", right through its own analog chain so the comparison is exact sample by sample.
fn without_pulse1(setup: fn(&mut Apu)) -> Vec<f32> {
    let mut a = Apu::new();
    a.gain.pulse1 = 0.0;
    setup(&mut a);
    for _ in 0..200_000 {
        a.clock();
    }
    a.samples
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

/// With every pan at centre nothing changes. The left stream is the mono one bit for bit and
/// the right side is never even produced, so a frontend with no stereo wiring hears today's APU.
#[test]
fn centred_pans_keep_the_mono_stream_bit_identical() {
    let (left, right) = panned(ChannelPan::default(), pulse1_tone);
    assert!(
        left.iter().any(|s| *s != 0.0),
        "the 440 Hz pulse must be audible"
    );
    assert!(right.is_empty(), "all-centre must not fill the right side");

    // An explicit all-zero list is the same setting (a `-0.0` included: it applies the same
    // weights), and it must not move a bit either.
    let mut explicit = ChannelPan::default();
    explicit
        .apply_list("pulse1=0,pulse2=-0.0,triangle=0,noise=0,dmc=0,expansion=0")
        .unwrap();
    assert!(explicit.is_centred());
    let (left_explicit, right_explicit) = panned(explicit, pulse1_tone);
    assert_eq!(
        bits(&left_explicit),
        bits(&left),
        "an explicit centre must not move a bit"
    );
    assert!(right_explicit.is_empty());
}

/// Pulse 1 hard left. The near side is the console's own mix — every weight on it is exactly
/// `1.0` — and the far side carries no pulse-1 energy at all: it is bit for bit the console whose
/// pulse 1 never played, which is what applying the weight before the nonlinear sum buys.
#[test]
fn a_hard_left_pulse1_is_on_the_left_and_absent_from_the_right() {
    let hard_left = ChannelPan {
        pulse1: -1.0,
        ..ChannelPan::default()
    };
    let (left, right) = panned(hard_left, pulse1_tone);
    assert_eq!(
        left.len(),
        right.len(),
        "one sample per side per output tick"
    );

    let mono = panned(ChannelPan::default(), pulse1_tone).0;
    assert_eq!(
        left.len(),
        mono.len(),
        "panning must not change the sample cadence"
    );
    assert_eq!(
        bits(&left),
        bits(&mono),
        "hard left must be today's stream on the left"
    );

    let reference = without_pulse1(pulse1_tone);
    assert_eq!(
        bits(&right),
        bits(&reference),
        "pulse 1 leaked into the right side"
    );
    assert!(
        right.iter().any(|s| *s != 0.0),
        "the right side is still a mix (the triangle's held level), not silence"
    );
}

/// The mirror case. Hard right must reproduce the mono stream on the right *bit for bit*,
/// which is what proves the right-hand resynthesizer and filter chain are a faithful second copy
/// and not merely "something audible".
#[test]
fn a_hard_right_pulse1_is_on_the_right_and_absent_from_the_left() {
    let hard_right = ChannelPan {
        pulse1: 1.0,
        ..ChannelPan::default()
    };
    let (left, right) = panned(hard_right, pulse1_tone);

    let mono = panned(ChannelPan::default(), pulse1_tone).0;
    assert_eq!(
        bits(&right),
        bits(&mono),
        "the right chain must reproduce the mono stream"
    );
    assert_eq!(
        bits(&left),
        bits(&without_pulse1(pulse1_tone)),
        "pulse 1 leaked into the left side"
    );
}

/// Each side mixes only the channels panned to it: pulse 1 hard left and pulse 2 hard right give two
/// sides that are each exactly a console with the other pulse removed. Scaling the *summed* pulses
/// (after the nonlinear transform) could not do that — the transform would spread one channel over
/// both sides — so this is the test that pins "weights before the sums".
#[test]
fn each_side_holds_only_the_channels_panned_to_it() {
    let split = ChannelPan {
        pulse1: -1.0,
        pulse2: 1.0,
        ..ChannelPan::default()
    };
    let (left, right) = panned(split, two_pulse_tones);

    let mut only_pulse1 = Apu::new();
    only_pulse1.gain.pulse2 = 0.0;
    two_pulse_tones(&mut only_pulse1);
    for _ in 0..200_000 {
        only_pulse1.clock();
    }

    let mut only_pulse2 = Apu::new();
    only_pulse2.gain.pulse1 = 0.0;
    two_pulse_tones(&mut only_pulse2);
    for _ in 0..200_000 {
        only_pulse2.clock();
    }

    assert_eq!(
        bits(&left),
        bits(&only_pulse1.samples),
        "the left side must hold pulse 1 alone"
    );
    assert_eq!(
        bits(&right),
        bits(&only_pulse2.samples),
        "the right side must hold pulse 2 alone"
    );
    // The two sides are different streams, so the comparisons above are not a tautology.
    assert_ne!(bits(&left), bits(&right));
}

/// Gain and pan compose into one weight per side: `gain × pan`. A half-gain pulse 1 panned hard
/// left is the half-gain mono stream on the left and the zero-gain console on the right.
#[test]
fn gain_and_pan_multiply_the_channel_level() {
    fn run(gain: f32, pan: ChannelPan) -> Apu {
        let mut a = Apu::new();
        a.gain.pulse1 = gain;
        a.pan = pan;
        pulse1_tone(&mut a);
        for _ in 0..200_000 {
            a.clock();
        }
        a
    }

    let split = run(
        0.5,
        ChannelPan {
            pulse1: -1.0,
            ..ChannelPan::default()
        },
    );
    let half = run(0.5, ChannelPan::default());
    let none = run(0.0, ChannelPan::default());

    assert_eq!(
        bits(&split.samples),
        bits(&half.samples),
        "gain 0.5 at full left weight"
    );
    assert_eq!(
        bits(&split.samples_right),
        bits(&none.samples),
        "gain 0.5 at zero right weight"
    );
}
