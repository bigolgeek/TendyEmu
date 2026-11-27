//! Per-channel mixer gain: one host-side multiplier per mixer input.
//!
//! The 2A03's mixer is nonlinear — the two pulses are summed and passed through one soft
//! transform, and the triangle, noise and DMC levels through a second — so a channel cannot be
//! scaled after that sum: the transform would spread one channel's change across the others. A
//! gain therefore multiplies the channel's own DAC level *before* the sums, which is also what
//! makes a gain of `0.0` remove exactly one channel: its term is exactly zero, so the transform
//! sees the input the console would see with that channel producing nothing.
//!
//! These are frontend settings, not emulated state: a save state does not store them and a movie
//! does not record them, exactly like the output sample rate. Every channel defaults to `1.0`,
//! and a product with exactly `1.0` is exact, so [`Apu::mix`](super::Apu) gives the pre-fix
//! stream byte for byte at the default — the audio digests in `tests/tasvideos_golden.txt` pin it.
//!
//! A gain is any finite `f32`: `0.0` silences the channel, `1.0` is the console's own level, and
//! above `1.0` asks the nonlinear mixer for more than the channel's DAC can give, which compresses
//! (the transforms saturate) rather than clipping like a hard limiter would.

/// One mixer input, in the order the mixer adds them and the order the flags and config keys
/// list them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Pulse1,
    Pulse2,
    Triangle,
    Noise,
    Dmc,
    /// The cartridge's expansion audio (`Apu::expansion_pulse`), which joins the pulse sum in
    /// pulse units. One gain covers every board's chip (FDS 2C33, MMC5, VRC6/7, N163, 5B),
    /// because they all arrive on that one mixer input.
    Expansion,
}

impl Channel {
    /// Every channel, in mixer order — the order `--gain` errors and the config keys list them.
    pub const ALL: [Channel; 6] = [
        Channel::Pulse1,
        Channel::Pulse2,
        Channel::Triangle,
        Channel::Noise,
        Channel::Dmc,
        Channel::Expansion,
    ];

    /// The channel `name` spells (`pulse1`, `pulse2`, `triangle`, `noise`, `dmc`, `expansion`).
    ///
    /// Names are matched exactly, like the quirk names `--quirks` takes: a wrong-case or
    /// abbreviated name is an error rather than a silent default.
    pub fn from_name(name: &str) -> Option<Channel> {
        match name {
            "pulse1" => Some(Channel::Pulse1),
            "pulse2" => Some(Channel::Pulse2),
            "triangle" => Some(Channel::Triangle),
            "noise" => Some(Channel::Noise),
            "dmc" => Some(Channel::Dmc),
            "expansion" => Some(Channel::Expansion),
            _ => None,
        }
    }

    /// The name [`Channel::from_name`] accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Pulse1 => "pulse1",
            Channel::Pulse2 => "pulse2",
            Channel::Triangle => "triangle",
            Channel::Noise => "noise",
            Channel::Dmc => "dmc",
            Channel::Expansion => "expansion",
        }
    }

    /// The channel names, as the error messages list them.
    pub fn names() -> String {
        Channel::ALL.map(Channel::as_str).join(", ")
    }
}

/// The per-channel gains: one multiplier per mixer input, `1.0` everywhere by default.
#[derive(Clone, Copy, Debug)]
pub struct ChannelGain {
    pub pulse1: f32,
    pub pulse2: f32,
    pub triangle: f32,
    pub noise: f32,
    pub dmc: f32,
    pub expansion: f32,
}

/// Equality compares the gains' bit patterns rather than their values: that keeps the type [`Eq`]
/// (the `config.toml` settings struct that holds it derives `Eq`, and every parse path refuses a
/// non-finite value anyway) and makes a `-0.0` gain a different setting from a `0.0` one, so the
/// file round trip is exact by bits, not by value.
impl PartialEq for ChannelGain {
    fn eq(&self, other: &ChannelGain) -> bool {
        Channel::ALL
            .iter()
            .all(|&channel| self.get(channel).to_bits() == other.get(channel).to_bits())
    }
}

impl Eq for ChannelGain {}

impl Default for ChannelGain {
    fn default() -> ChannelGain {
        ChannelGain {
            pulse1: 1.0,
            pulse2: 1.0,
            triangle: 1.0,
            noise: 1.0,
            dmc: 1.0,
            expansion: 1.0,
        }
    }
}

impl ChannelGain {
    /// The gain on `channel`.
    pub fn get(self, channel: Channel) -> f32 {
        match channel {
            Channel::Pulse1 => self.pulse1,
            Channel::Pulse2 => self.pulse2,
            Channel::Triangle => self.triangle,
            Channel::Noise => self.noise,
            Channel::Dmc => self.dmc,
            Channel::Expansion => self.expansion,
        }
    }

    /// Set `channel`'s gain. Values come from [`ChannelGain::parse_value`] on both frontend
    /// paths, which is where a non-finite number is refused.
    pub fn set(&mut self, channel: Channel, gain: f32) {
        match channel {
            Channel::Pulse1 => self.pulse1 = gain,
            Channel::Pulse2 => self.pulse2 = gain,
            Channel::Triangle => self.triangle = gain,
            Channel::Noise => self.noise = gain,
            Channel::Dmc => self.dmc = gain,
            Channel::Expansion => self.expansion = gain,
        }
    }

    /// True when every channel sits at its default `1.0`.
    pub fn is_unity(self) -> bool {
        Channel::ALL.iter().all(|&channel| self.get(channel) == 1.0)
    }

    /// One gain value: a decimal float, finite.
    ///
    /// Shared by the `--gain` list and the `gain_<channel>` config keys so the two frontends
    /// accept and refuse exactly the same strings (and a `NaN` cannot reach [`Apu::mix`](super::Apu)).
    pub fn parse_value(text: &str) -> Result<f32, String> {
        let text = text.trim();
        match text.parse::<f32>() {
            Ok(value) if value.is_finite() => Ok(value),
            Ok(_) => Err(format!("{text:?} is not a finite number")),
            Err(_) => Err(format!("{text:?} is not a number")),
        }
    }

    /// Parse one `channel=value` pair, e.g. `triangle=0.5`.
    ///
    /// The error is bare — no flag or file name — so `--gain` and `config.toml` can each say where
    /// the bad pair came from; every message names the offending text, the channel table or both.
    pub fn parse_pair(text: &str) -> Result<(Channel, f32), String> {
        parse_pair_with(text, ChannelGain::parse_value)
    }

    /// Apply a comma-separated list of `channel=value` pairs, e.g. `pulse1=1.0,triangle=0.5`.
    ///
    /// Entries are applied left to right, so a repeated channel keeps its last value, the way
    /// every other repeated option in this emulator behaves. A blank entry is an error (there is
    /// no such channel), never a skip: a stray comma must not leave a gain at its default.
    pub fn apply_list(&mut self, list: &str) -> Result<(), String> {
        for entry in list.split(',') {
            let (channel, gain) = ChannelGain::parse_pair(entry)?;
            self.set(channel, gain);
        }
        Ok(())
    }
}

/// One mixer input's stereo position (#76b): `-1.0` hard left, `0.0` centre, `1.0` hard right.
///
/// The side weights follow a linear *balance* law rather than a constant-power pan law:
///
/// ```text
/// left  = 1 - max(pan, 0)
/// right = 1 + min(pan, 0)
/// ```
///
/// so a centred channel keeps its whole level on both sides (a balance control does not dip the
/// middle by 3 dB the way a constant-power pan would), `0.5` is half weight on the left and full on
/// the right, and a hard-panned channel contributes exactly `0.0` to the far side — a zero term
/// before the nonlinear sums, so it cannot leak through either transform. That is the same
/// before-the-sums rule [`ChannelGain`] follows;
/// [`Apu::mix_stereo`](super::Apu::mix_stereo) applies the weight to the channel's own DAC level.
///
/// These are frontend settings like [`ChannelGain`]: a save state does not store them and a movie
/// does not record them. Every channel defaults to `0.0`, and [`ChannelPan::is_centred`] lets
/// [`Apu::clock`](super::Apu::clock) keep the mono path — one mixer, one resynthesizer, one filter
/// chain, `samples` byte for byte as before and `samples_right` empty.
#[derive(Clone, Copy, Debug)]
pub struct ChannelPan {
    pub pulse1: f32,
    pub pulse2: f32,
    pub triangle: f32,
    pub noise: f32,
    pub dmc: f32,
    pub expansion: f32,
}

/// Bit-pattern equality, like [`ChannelGain`]: it keeps the type [`Eq`] for the settings struct
/// that holds it, and a `-0.0` pan stays a different setting on disk from a `0.0` one even though
/// the two apply the same weights.
impl PartialEq for ChannelPan {
    fn eq(&self, other: &ChannelPan) -> bool {
        Channel::ALL
            .iter()
            .all(|&channel| self.get(channel).to_bits() == other.get(channel).to_bits())
    }
}

impl Eq for ChannelPan {}

impl Default for ChannelPan {
    fn default() -> ChannelPan {
        ChannelPan {
            pulse1: 0.0,
            pulse2: 0.0,
            triangle: 0.0,
            noise: 0.0,
            dmc: 0.0,
            expansion: 0.0,
        }
    }
}

impl ChannelPan {
    /// The pan on `channel`.
    pub fn get(self, channel: Channel) -> f32 {
        match channel {
            Channel::Pulse1 => self.pulse1,
            Channel::Pulse2 => self.pulse2,
            Channel::Triangle => self.triangle,
            Channel::Noise => self.noise,
            Channel::Dmc => self.dmc,
            Channel::Expansion => self.expansion,
        }
    }

    /// Set `channel`'s pan. Values come from [`ChannelPan::parse_value`] on both frontend paths,
    /// which is where a value outside `-1.0..=1.0` is refused.
    pub fn set(&mut self, channel: Channel, pan: f32) {
        match channel {
            Channel::Pulse1 => self.pulse1 = pan,
            Channel::Pulse2 => self.pulse2 = pan,
            Channel::Triangle => self.triangle = pan,
            Channel::Noise => self.noise = pan,
            Channel::Dmc => self.dmc = pan,
            Channel::Expansion => self.expansion = pan,
        }
    }

    /// True when every channel sits at centre. `-0.0` counts as centre: it applies the same
    /// weights. [`Apu::clock`](super::Apu::clock) uses this to keep the mono sample path bit for
    /// bit the pre-#76b one.
    pub fn is_centred(self) -> bool {
        Channel::ALL.iter().all(|&channel| self.get(channel) == 0.0)
    }

    /// The `(left, right)` weights for `channel`, from the linear balance law in the type doc.
    pub fn weights(self, channel: Channel) -> (f32, f32) {
        let pan = self.get(channel);
        (1.0 - pan.max(0.0), 1.0 + pan.min(0.0))
    }

    /// One pan value: a decimal float in `-1.0..=1.0`.
    ///
    /// Shared by the `--pan` list and the `pan_<channel>` config keys so the two frontends accept
    /// and refuse exactly the same strings. An out-of-range value is an error, never a clamp: a
    /// typo must not quietly become hard left or hard right.
    pub fn parse_value(text: &str) -> Result<f32, String> {
        let text = text.trim();
        match text.parse::<f32>() {
            Ok(value) if !value.is_finite() => Err(format!("{text:?} is not a finite number")),
            Ok(value) if !(-1.0..=1.0).contains(&value) => {
                Err(format!("{text:?} is outside -1.0..=1.0"))
            }
            Ok(value) => Ok(value),
            Err(_) => Err(format!("{text:?} is not a number")),
        }
    }

    /// Parse one `channel=value` pair, e.g. `pulse1=-1.0`. The error is bare, as in
    /// [`ChannelGain::parse_pair`].
    pub fn parse_pair(text: &str) -> Result<(Channel, f32), String> {
        parse_pair_with(text, ChannelPan::parse_value)
    }

    /// Apply a comma-separated list of `channel=value` pairs, e.g. `pulse1=-1,pulse2=0.5`.
    ///
    /// Entries are applied left to right, so a repeated channel keeps its last value; a blank entry
    /// is an error rather than a skip, exactly like [`ChannelGain::apply_list`].
    pub fn apply_list(&mut self, list: &str) -> Result<(), String> {
        for entry in list.split(',') {
            let (channel, pan) = ChannelPan::parse_pair(entry)?;
            self.set(channel, pan);
        }
        Ok(())
    }
}

/// Split `channel=value` and hand the value to `parse_value`: the shape both [`ChannelGain`] and
/// [`ChannelPan`] accept, so the two lists share one error grammar.
fn parse_pair_with(
    text: &str,
    parse_value: fn(&str) -> Result<f32, String>,
) -> Result<(Channel, f32), String> {
    let entry = text.trim();
    let Some((name, value)) = entry.split_once('=') else {
        return Err(format!("{entry:?} is not channel=value"));
    };
    let Some(channel) = Channel::from_name(name.trim()) else {
        return Err(format!(
            "unknown channel {:?} (channels: {})",
            name.trim(),
            Channel::names()
        ));
    };
    Ok((channel, parse_value(value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apu::Apu;

    /// Long enough for about 4900 output samples at 44.1 kHz: the filter chain's first few hundred
    /// samples and any envelope step fall inside it.
    const CYCLES: usize = 200_000;

    /// Every mixer input, in mixer order.
    const ALL: [Channel; 6] = Channel::ALL;

    /// `$4015`'s enable bits: the four length-counter channels. The DMC is not enabled there — a
    /// bare `Apu` has no bus to feed its DMA, so it is driven through `$4011` instead — and the
    /// expansion input has no register at all.
    fn enable_bit(channel: Channel) -> u8 {
        match channel {
            Channel::Pulse1 => 0x01,
            Channel::Pulse2 => 0x02,
            Channel::Triangle => 0x04,
            Channel::Noise => 0x08,
            Channel::Dmc | Channel::Expansion => 0x00,
        }
    }

    /// The registers that make one channel sound on its own: constant volume, an audible period,
    /// the length loaded. `$4015` is written once by [`sounding`] — every write replaces the whole
    /// enable byte, so one write per channel would leave only the last channel enabled.
    fn start(a: &mut Apu, channel: Channel) {
        match channel {
            Channel::Pulse1 => {
                a.write_register(0x4000, 0xBF);
                a.write_register(0x4002, 253);
                a.write_register(0x4003, 0x08);
            }
            Channel::Pulse2 => {
                a.write_register(0x4004, 0xBF);
                a.write_register(0x4006, 253);
                a.write_register(0x4007, 0x08);
            }
            Channel::Triangle => {
                a.write_register(0x4008, 0x85);
                a.write_register(0x400A, 0x40);
                a.write_register(0x400B, 0x08);
            }
            Channel::Noise => {
                a.write_register(0x400C, 0x3F);
                a.write_register(0x400E, 0x04);
                a.write_register(0x400F, 0x08);
            }
            // $4011 loads the 7-bit level directly: a constant DAC output without a DMA fetch.
            Channel::Dmc => a.dmc.write(1, 0x7F, false),
            Channel::Expansion => a.expansion_pulse = 4000.0,
        }
    }

    /// An APU with `channels` sounding and every other mixer input silent.
    fn sounding(channels: &[Channel]) -> Apu {
        let mut a = Apu::new();
        let enable = channels
            .iter()
            .fold(0, |bits, &channel| bits | enable_bit(channel));
        a.write_register(0x4015, enable);
        for &channel in channels {
            start(&mut a, channel);
        }
        a
    }

    /// Clock [`CYCLES`] cycles and return the drained samples as raw `f32` bits, so the comparisons
    /// below are exact: no float `==` (which would call `-0.0` and `0.0` the same) is involved.
    fn run(mut a: Apu) -> Vec<u32> {
        for _ in 0..CYCLES {
            a.clock();
        }
        a.samples.iter().map(|s| s.to_bits()).collect()
    }

    /// Exact stream comparison that names the first differing sample instead of dumping thousands of
    /// `u32`s into the failure message.
    fn assert_same_stream(got: &[u32], want: &[u32], what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: different sample counts");
        if let Some(index) = got.iter().zip(want).position(|(a, b)| a != b) {
            panic!(
                "{what}: sample {index} differs: {:08x} != {:08x}",
                got[index], want[index]
            );
        }
    }

    #[test]
    fn every_channel_defaults_to_unity() {
        let gain = ChannelGain::default();
        assert_eq!(ALL.len(), 6, "one gain per mixer input");
        for channel in ALL {
            assert_eq!(gain.get(channel), 1.0, "{channel:?}");
        }
        assert!(gain.is_unity());
        assert_eq!(
            Channel::names(),
            "pulse1, pulse2, triangle, noise, dmc, expansion"
        );
    }

    #[test]
    fn channel_names_round_trip_and_match_exactly() {
        for channel in ALL {
            assert_eq!(Channel::from_name(channel.as_str()), Some(channel));
        }
        for name in ["", "pulse", "pulse_1", "Pulse1", "triangle ", "tnd", "fds"] {
            assert_eq!(Channel::from_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_list_sets_only_the_channels_it_names() {
        let mut gain = ChannelGain::default();
        gain.apply_list("pulse1=0.5, triangle=0").unwrap();
        assert_eq!(gain.pulse1, 0.5);
        assert_eq!(gain.triangle, 0.0);
        assert_eq!(gain.pulse2, 1.0, "an unnamed channel keeps its value");
        assert!(!gain.is_unity());
        // A later entry for the same channel wins, like every other repeated option here.
        gain.apply_list("pulse1=0.25").unwrap();
        assert_eq!(gain.pulse1, 0.25);
        // Whitespace around the name, the `=` and the value is optional, as in config.toml.
        gain.apply_list("noise = 1.5 , dmc=2,expansion= 0.125")
            .unwrap();
        assert_eq!((gain.noise, gain.dmc, gain.expansion), (1.5, 2.0, 0.125));
        // A whole-number value is a float, not a rejected integer.
        gain.apply_list("pulse2=1").unwrap();
        assert_eq!(gain.pulse2, 1.0);
        gain.apply_list("pulse1=1,pulse2=1,triangle=1,noise=1,dmc=1,expansion=1")
            .unwrap();
        assert!(gain.is_unity(), "every channel is back at 1.0");
    }

    #[test]
    fn a_bad_pair_names_what_is_wrong() {
        assert_eq!(
            ChannelGain::parse_pair(""),
            Err("\"\" is not channel=value".to_string())
        );
        assert_eq!(
            ChannelGain::parse_pair("pulse1"),
            Err("\"pulse1\" is not channel=value".to_string())
        );
        assert_eq!(
            ChannelGain::parse_pair("=0.5"),
            Err(format!(
                "unknown channel \"\" (channels: {})",
                Channel::names()
            ))
        );
        assert_eq!(
            ChannelGain::parse_pair("pulse3=1.0"),
            Err(format!(
                "unknown channel \"pulse3\" (channels: {})",
                Channel::names()
            ))
        );
        assert_eq!(
            ChannelGain::parse_pair("pulse1=loud"),
            Err("\"loud\" is not a number".to_string())
        );
        // `f32::from_str` accepts these spellings; the mixer does not.
        for text in ["NaN", "nan", "inf", "-inf", "infinity"] {
            assert_eq!(
                ChannelGain::parse_pair(&format!("pulse1={text}")),
                Err(format!("\"{text}\" is not a finite number")),
                "{text}"
            );
        }
        // A blank list entry is an unknown channel, never a silent skip.
        assert!(ChannelGain::default().apply_list("triangle=0.5,").is_err());
    }

    /// Gain `0.0` must silence *exactly* the channel it names: with every other input muted, the
    /// channel's own stream is audible at `1.0` and every sample of it is the bit pattern for +0.0
    /// at `0.0` — the AC-coupled stage has no charge to release because nothing ever reached it.
    ///
    /// Every other gain is muted because the triangle's sequencer free-runs and its DAC holds a
    /// level even with no register written (`Triangle::output`), so a pulse-only console still has
    /// a triangle term in its mix; only its own gain can take that away.
    #[test]
    fn gain_zero_silences_each_channel_on_its_own() {
        for channel in ALL {
            let mut only = sounding(&[channel]);
            for other in ALL {
                if other != channel {
                    only.gain.set(other, 0.0);
                }
            }

            let loud = run(only.clone());
            assert!(
                loud.iter().any(|&bits| f32::from_bits(bits) != 0.0),
                "{channel:?} should be audible at gain 1.0"
            );

            let mut muted = only;
            muted.gain.set(channel, 0.0);
            let silent = run(muted);
            assert_eq!(
                silent.len(),
                loud.len(),
                "{channel:?}: the run must be the same length"
            );
            assert!(
                silent.iter().all(|&bits| bits == 0),
                "{channel:?} at gain 0 must be exactly +0.0 everywhere, not just quiet"
            );
        }
    }

    /// Every gain at `0.0` is digital silence from the mixer onwards, the triangle's held DAC level
    /// included: `mix` returns +0.0 and a filter chain that is never fed emits +0.0 samples.
    #[test]
    fn zero_gains_everywhere_are_exact_silence() {
        let mut a = sounding(&ALL);
        for channel in ALL {
            a.gain.set(channel, 0.0);
        }
        assert_eq!(
            a.mix().to_bits(),
            0,
            "a silent mixer is +0.0, not a tiny residue"
        );
        assert!(run(a).iter().all(|&bits| bits == 0));
    }

    /// Zeroing one channel of a mix must give the same stream as a console on which that channel
    /// never produced anything — the gain is applied before the nonlinear sums, so it cannot leak
    /// into another channel's contribution.
    ///
    /// The triangle is not in the list: its DAC cannot be made to produce nothing (see
    /// [`gain_zero_silences_each_channel_on_its_own`]), so there is no console state to compare
    /// against. Its own gain still silences it, which the tests above cover.
    #[test]
    fn a_zeroed_channel_leaves_the_other_channels_bit_identical() {
        let all = [
            Channel::Pulse1,
            Channel::Pulse2,
            Channel::Noise,
            Channel::Dmc,
            Channel::Expansion,
        ];
        for channel in all {
            let others: Vec<Channel> = all.iter().copied().filter(|&c| c != channel).collect();
            let control = run(sounding(&others));

            let mut muted = sounding(&all);
            muted.gain.set(channel, 0.0);
            assert_same_stream(
                &run(muted),
                &control,
                &format!("{channel:?} at gain 0 against the console without it"),
            );

            // The comparison only proves something if the channel is heard at 1.0.
            assert!(
                run(sounding(&all)) != control,
                "{channel:?} at gain 1.0 must be heard"
            );
        }
    }

    /// An explicit `1.0` everywhere is the default path: the whole sample stream is identical bit
    /// for bit to the stream no gain touches (the golden `:audio` rows pin the same thing across
    /// whole ROMs).
    #[test]
    fn explicit_unity_gains_leave_the_stream_byte_identical() {
        let default = run(sounding(&ALL));
        assert!(default.iter().any(|&bits| f32::from_bits(bits) != 0.0));

        let mut explicit = sounding(&ALL);
        explicit
            .gain
            .apply_list("pulse1=1.0,pulse2=1,triangle=1.0,noise=1.0,dmc=1.0,expansion=1.0")
            .unwrap();
        assert!(explicit.gain.is_unity());
        assert_same_stream(&run(explicit), &default, "explicit 1.0 gains");
    }

    /// Every gain that is not unity has to move the mix, or the multipliers are not wired in.
    #[test]
    fn a_gain_off_unity_moves_the_stream() {
        let base = run(sounding(&ALL));
        for channel in ALL {
            let mut quiet = sounding(&ALL);
            quiet.gain.set(channel, 0.5);
            assert!(run(quiet) != base, "{channel:?} at 0.5 must change the mix");
        }
    }

    #[test]
    fn every_channel_defaults_to_centre() {
        let pan = ChannelPan::default();
        for channel in ALL {
            assert_eq!(pan.get(channel), 0.0, "{channel:?}");
        }
        assert!(pan.is_centred());
        assert_eq!(ChannelPan::default(), ChannelPan::default());
    }

    #[test]
    fn a_hard_pan_weights_one_side_only_and_centre_keeps_both() {
        let pan = ChannelPan {
            pulse1: -1.0,
            pulse2: 1.0,
            triangle: 0.5,
            ..ChannelPan::default()
        };
        assert_eq!(pan.weights(Channel::Pulse1), (1.0, 0.0), "hard left");
        assert_eq!(pan.weights(Channel::Pulse2), (0.0, 1.0), "hard right");
        assert_eq!(
            pan.weights(Channel::Triangle),
            (0.5, 1.0),
            "half left, full right"
        );
        assert_eq!(
            pan.weights(Channel::Noise),
            (1.0, 1.0),
            "centre is full on both sides"
        );
        assert_eq!(pan.weights(Channel::Dmc), (1.0, 1.0));
        assert!(!pan.is_centred());
        // A pan a hair off centre already takes the stereo path.
        assert!(
            !ChannelPan {
                dmc: 0.001,
                ..ChannelPan::default()
            }
            .is_centred()
        );
        // `-0.0` is centre by value, even though it is a different setting on disk.
        let negative_zero = ChannelPan {
            pulse2: -0.0,
            ..ChannelPan::default()
        };
        assert!(negative_zero.is_centred());
        assert_eq!(negative_zero.weights(Channel::Pulse2), (1.0, 1.0));
        assert_ne!(negative_zero, ChannelPan::default());
    }

    #[test]
    fn a_pan_list_sets_only_the_channels_it_names() {
        let mut pan = ChannelPan::default();
        pan.apply_list("pulse1=-1, triangle=0.25").unwrap();
        assert_eq!(pan.pulse1, -1.0);
        assert_eq!(pan.triangle, 0.25);
        assert_eq!(pan.pulse2, 0.0, "an unnamed channel stays centred");
        assert!(!pan.is_centred());
        // Later entries win, whitespace is optional, and a bare whole number is a float.
        pan.apply_list("pulse1=0.5,pulse1=-0.5,noise = 1 , dmc=1,expansion= -1")
            .unwrap();
        assert_eq!(
            (pan.pulse1, pan.noise, pan.dmc, pan.expansion),
            (-0.5, 1.0, 1.0, -1.0)
        );
        pan.apply_list("pulse1=0,pulse2=0,triangle=0,noise=0,dmc=0,expansion=0")
            .unwrap();
        assert!(pan.is_centred(), "every channel is back at centre");
    }

    #[test]
    fn a_bad_pan_names_what_is_wrong() {
        assert_eq!(
            ChannelPan::parse_pair(""),
            Err("\"\" is not channel=value".to_string())
        );
        assert_eq!(
            ChannelPan::parse_pair("pulse1"),
            Err("\"pulse1\" is not channel=value".to_string())
        );
        assert_eq!(
            ChannelPan::parse_pair("pulse3=-1"),
            Err(format!(
                "unknown channel \"pulse3\" (channels: {})",
                Channel::names()
            ))
        );
        assert_eq!(
            ChannelPan::parse_pair("pulse1=left"),
            Err("\"left\" is not a number".to_string())
        );
        for text in ["NaN", "nan", "inf", "-inf"] {
            assert_eq!(
                ChannelPan::parse_pair(&format!("pulse1={text}")),
                Err(format!("\"{text}\" is not a finite number")),
                "{text}"
            );
        }
        // Outside the range is refused, not clamped: 2.0 would otherwise become hard right.
        for text in ["1.0001", "-1.5", "2", "100"] {
            assert_eq!(
                ChannelPan::parse_pair(&format!("pulse1={text}")),
                Err(format!("\"{text}\" is outside -1.0..=1.0")),
                "{text}"
            );
        }
        // The two ends themselves are fine, and so is a blank list entry failing loudly.
        assert_eq!(
            ChannelPan::parse_pair("pulse1=-1"),
            Ok((Channel::Pulse1, -1.0))
        );
        assert_eq!(
            ChannelPan::parse_pair("pulse1=1"),
            Ok((Channel::Pulse1, 1.0))
        );
        assert!(ChannelPan::default().apply_list("triangle=0.5,").is_err());
    }
}
