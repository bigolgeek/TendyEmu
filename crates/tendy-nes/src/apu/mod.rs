//! The 2A03 audio processing unit: five channels, frame counter, mixer and sample output.

pub mod dmc;
pub mod expansion;
pub mod filter;
pub mod gain;
pub mod noise;
pub mod pulse;
pub mod state;
pub mod synth;
pub mod triangle;
pub mod units;
pub mod viewer;

use crate::cart::Region;
use crate::{CPU_HZ, CPU_HZ_DENDY, CPU_HZ_PAL};
use dmc::Dmc;
use filter::FilterChain;
use gain::{Channel, ChannelGain, ChannelPan};
use noise::Noise;
use pulse::Pulse;
use synth::BandLimitedSynth;
use triangle::Triangle;

/// Default output sample rate in Hz.
pub const SAMPLE_RATE: f64 = 44_100.0;

/// CPU cycles the frame counter is already into its sequence when the machine powers on.
///
/// On hardware the console's virtual `$4017 = $00` write happens 9-12 CPU clocks before the first
/// instruction, so the first frame-counter step lands earlier than a counter starting at 0 predicts.
/// Mesen2 (`ApuFrameCounter::Reset`) gets the same effect by arming a 3-cycle delayed `$00` write
/// once the reset sequence has run. 3 is the value real hardware reports most often:
/// `apu_reset/4017_timing` measures a power-on `$4017` delay of 9 with this lead, and
/// `blargg_apu_2005.07.30/09.reset_timing` accepts a lead in 2..=6.
///
/// Applied by `Apu::power_on` from `Nes::build` and by `Apu::soft_reset` from `Nes::reset`, never by
/// `Apu::new`: the APU unit tests count frame-counter clocks from 0 (e.g. `apu_frame_counter`: length expiry at
/// 29829).
pub const POWER_ON_FRAME_LEAD: u32 = 3;

/// Host-side audio options: switches that change what is *heard* without touching what the
/// console emulates.
///
/// They sit next to `sample_rate` and `gain`, not inside a channel: like those two they are
/// frontend configuration rather than emulated state, so a save state neither stores nor restores
/// them and a movie does not record them. Every field defaults to `false` — the console's own
/// behaviour — so a run that sets none of them produces the pre-fix stream bit for bit (the
/// `:audio` rows in `tests/tasvideos_golden.txt` pin that).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioOptions {
    /// Drop the triangle's ultrasonic output. While its timer period is below 2 the
    /// sequencer steps faster than the DAC can follow (a 1.79 MHz square at period 0), which no
    /// speaker reproduces and which point-sampling at the output rate folds back into the audible
    /// band. On, the channel reports the last level it played — what the DAC does when its input
    /// stops moving — until a timer period of 2 or more is written again. The sequencer keeps
    /// running either way, so the option moves what leaves the mixer and nothing about the
    /// channel's state.
    pub silence_ultrasonic_triangle: bool,
    /// Soften the click a `$4011` direct load makes. A write that moves the DMC level by
    /// more than [`dmc::DMC_POP_THRESHOLD`] keeps half of the move instead of landing on the
    /// value outright — Mesen's `ReduceDmcPopping` rule — which halves the step a game's
    /// mid-sample DAC patch puts into the mixer. The softened level is the channel's level, so it
    /// is part of a save state; the option itself is not, and off is the console's own write.
    pub reduce_dmc_pop: bool,
}

/// The 2A03 APU: five channels, the frame counter, the mixer and resampling to `sample_rate`.
#[derive(Clone, Debug)]
pub struct Apu {
    pub pulse1: Pulse,
    pub pulse2: Pulse,
    pub triangle: Triangle,
    pub noise: Noise,
    pub dmc: Dmc,
    /// Frame counter: 5-step mode, IRQ inhibit, IRQ flag, CPU cycles into the sequence.
    pub five_step: bool,
    pub irq_inhibit: bool,
    pub frame_irq: bool,
    /// $4015 read happened; the flag clears on the next "get" (odd) APU cycle.
    frame_irq_clear_pending: bool,
    pub frame_cycle: u32,
    /// CPU cycles until a $4017 write resets the frame counter (0 = none pending).
    pub reset_delay: u8,
    /// CPU cycles since power-on.
    pub cycle: u64,
    /// PAL (2A07) rate tables and frame-counter periods.
    pub pal: bool,
    /// CPU clock this APU runs at; drives the sample-rate conversion.
    pub cpu_hz: f64,
    pub sample_rate: f64,
    sample_acc: f64,
    /// `sample_rate / cpu_hz`, the part of an output sample one CPU cycle is worth, kept instead of
    /// divided on every cycle. Both operands are public fields a frontend or test may assign
    /// directly, so `step_key` holds the bits of the pair it was derived from and [`Apu::clock`]
    /// re-derives it before the add whenever either differs: the same quotient, bit for bit.
    sample_step: f64,
    step_key: (u64, u64),
    /// The console's analog output section: 2 high-passes (90 Hz, 440 Hz) then a 14 kHz low-pass.
    pub filter: FilterChain,
    /// The right side's analog chain (#76b): the same corners as [`Apu::filter`], its own running
    /// state. Not serialized, for the same reason as [`Apu::synth_right`].
    pub filter_right: FilterChain,
    /// Band-limited resynthesis between the mixer and the analog chain: the DAC's stepwise output
    /// is spread over a windowed-sinc kernel instead of being point-sampled, so content above the
    /// output Nyquist is filtered out rather than folded into the audible band (see [`synth`]).
    pub synth: BandLimitedSynth,
    /// The right side's resynthesizer (#76b): [`Apu::synth`]'s twin, fed the right-hand mixer level
    /// at the same sub-sample phases. Nothing about it is serialized: the state carries the console,
    /// and the pan that gives this chain its input is a frontend setting, so a load resets it.
    pub synth_right: BandLimitedSynth,
    /// Output samples in -1.0..=1.0, appended at `sample_rate`; the frontend drains them. This is
    /// the left side of the pair below.
    pub samples: Vec<f32>,
    /// The right side of the stereo pair (#76b), appended at the same rate as [`Apu::samples`].
    /// Empty while every pan is centred: a mono run costs nothing extra and its `samples` stream is
    /// the pre-#76b one bit for bit. A drain buffer like `samples`, so a save state does not store
    /// it either (see [`state`]).
    pub samples_right: Vec<f32>,
    /// Expansion-audio level from the cartridge (MMC5's two pulses + PCM, or the same scale of
    /// level from another cartridge chip: VRC6, VRC7, N163, FME-7/5B). Refreshed by the bus on the
    /// cycles `Apu::clock` runs the mixer, which is the only place it is read; added to the
    /// output after both nonlinear curves, through the pulse curve on its own (see [`Apu::mix`]).
    /// Zero for every board without expansion audio.
    pub expansion_pulse: f32,
    /// Per-channel mixer gain: one host-side multiplier per mixer input, `1.0` everywhere by
    /// default. Each one scales its channel's DAC level inside [`Apu::mix`] *before* the nonlinear
    /// sums, so `0.0` silences exactly that channel and `1.0` leaves the stream untouched. Like
    /// `sample_rate`, this is frontend configuration and not emulated state: a save state neither
    /// stores nor restores it.
    pub gain: ChannelGain,
    /// Per-channel stereo position (#76b): `-1.0` hard left … `1.0` hard right, `0.0` everywhere by
    /// default. Like `gain` it is frontend configuration and not emulated state — a save state
    /// neither stores nor restores it — and with every channel centred the APU runs the mono path,
    /// so `samples` is byte for byte the pre-#76b stream.
    pub pan: ChannelPan,
    /// The host-side audio options: like `gain`, frontend configuration and never emulated
    /// state. Both off — the default — is the console's own behaviour.
    pub options: AudioOptions,
}

impl Default for Apu {
    fn default() -> Self {
        Apu::new()
    }
}

impl Apu {
    /// Inverse of the nonlinear mixer's pulse transform: the pulse-unit expansion-audio level whose
    /// solo contribution to the mix equals `amplitude`. Keeps every cartridge chip on the same
    /// pulse-units scale as MMC5's channels (see [`crate::cart::Mapper::expansion_audio`]) without
    /// a per-board rewrite: vrc6/vrc7/n163/fme7 emit in the 2A03 mixer's scale, this converts.
    /// Exact for both signs: [`Apu::mix`] curves the magnitude and restores the sign.
    /// Amplitudes at or beyond the transform's ceiling (0.9588) saturate.
    #[must_use]
    pub fn amplitude_to_pulse_units(amplitude: f32) -> f32 {
        let a = amplitude.abs();
        let sign = amplitude.signum();
        if a < f32::EPSILON {
            return 0.0;
        }
        let denom = 95.88 / a - 100.0;
        if denom <= 0.0 {
            return sign * f32::MAX;
        }
        sign * (8128.0 / denom)
    }

    pub fn new() -> Self {
        Apu {
            pulse1: Pulse::new(true),
            pulse2: Pulse::new(false),
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc::default(),
            five_step: false,
            irq_inhibit: false,
            frame_irq: false,
            frame_irq_clear_pending: false,
            frame_cycle: 0,
            reset_delay: 0,
            cycle: 0,
            pal: false,
            cpu_hz: CPU_HZ,
            sample_rate: SAMPLE_RATE,
            sample_acc: 0.0,
            sample_step: SAMPLE_RATE / CPU_HZ,
            step_key: (SAMPLE_RATE.to_bits(), CPU_HZ.to_bits()),
            filter: FilterChain::new(SAMPLE_RATE),
            filter_right: FilterChain::new(SAMPLE_RATE),
            synth: BandLimitedSynth::new(),
            synth_right: BandLimitedSynth::new(),
            samples: Vec::new(),
            samples_right: Vec::new(),
            expansion_pulse: 0.0,
            gain: ChannelGain::default(),
            pan: ChannelPan::default(),
            options: AudioOptions::default(),
        }
    }

    /// Power-on: the frame counter is already `POWER_ON_FRAME_LEAD` cycles into its sequence.
    ///
    /// Kept out of `Apu::new` on purpose: a bare 2A03 unit in the APU unit tests counts frame-counter
    /// clocks from 0 (e.g. `apu_frame_counter`: length expiry at 29829). The reset button re-arms the counter by a
    /// separate path (`Apu::soft_reset`, RC-1) — this is power-on only. `Nes::build` calls it once,
    /// before `Cpu::reset`, so the two reset-vector reads are the first frame-counter clocks the console
    /// runs: the same point in the sequence that hardware's virtual power-on `$4017` write leaves it at.
    pub fn power_on(&mut self) {
        self.frame_cycle = POWER_ON_FRAME_LEAD;
    }

    /// Region timing for the APU: PAL swaps in the 2A07 noise/DMC tables and the slower frame counter,
    /// and every region resamples from its own CPU clock. Dendy keeps the NTSC tables (its APU is a
    /// 2A03-style chip clocked at master/15) but runs at its own CPU clock.
    pub fn set_region(&mut self, region: Region) {
        self.pal = region == Region::Pal;
        self.cpu_hz = match region {
            Region::Ntsc => CPU_HZ,
            Region::Pal => CPU_HZ_PAL,
            Region::Dendy => CPU_HZ_DENDY,
        };
        self.noise.pal = self.pal;
        self.dmc.pal = self.pal;
        // Power-on periods come from the region's tables (no register write has happened yet).
        self.noise.timer_period = noise::periods(self.pal)[0];
        self.dmc.timer_period = dmc::rates(self.pal)[0];
    }

    /// The reset button, APU side: restart the frame counter from its power-on phase with both IRQ
    /// flags and the inhibit latch clear, keeping the `$4017` step mode ("at reset, `$4017` mode is
    /// unchanged, but IRQ inhibit flag is sometimes cleared") as Mesen's `ApuFrameCounter::Reset(softReset)`
    /// does. The five-step "immediate quarter+half" clock is power-on only: `Nes::reset` writes
    /// `$4015 = 0` before this, so the lengths are already zero and only the envelopes would see it.
    pub fn soft_reset(&mut self) {
        self.frame_irq = false;
        self.frame_irq_clear_pending = false;
        self.irq_inhibit = false;
        self.reset_delay = 0;
        self.frame_cycle = POWER_ON_FRAME_LEAD;
        // The reset button re-couples the output stages: no charge from the old session survives
        // into the new one's first samples.
        self.filter.reset();
        self.synth.reset();
        self.filter_right.reset();
        self.synth_right.reset();
    }

    /// Set the output sample rate, re-deriving the analog filter chain's corners for it.
    ///
    /// `sample_rate` stays a plain field because frontends and tests assign it directly; `clock`
    /// re-syncs the chain whenever the two disagree, and this is the path that has the
    /// coefficients right before the first sample.
    pub fn set_sample_rate(&mut self, rate: f64) {
        self.sample_rate = rate;
        self.filter.set_rate(rate);
        self.filter_right.set_rate(rate);
    }

    /// CPU write to $4000-$4013, $4015 or $4017.
    pub fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            0x4000..=0x4003 => self.pulse1.write(addr, val),
            0x4004..=0x4007 => self.pulse2.write(addr, val),
            0x4008..=0x400B => self.triangle.write(addr, val),
            0x400C..=0x400F => self.noise.write(addr, val),
            0x4010..=0x4013 => self.dmc.write(addr, val, self.options.reduce_dmc_pop),
            0x4015 => {
                self.pulse1.length.set_enabled(val & 0x01 != 0);
                self.pulse2.length.set_enabled(val & 0x02 != 0);
                self.triangle.length.set_enabled(val & 0x04 != 0);
                self.noise.length.set_enabled(val & 0x08 != 0);
                self.dmc
                    .set_enabled(val & 0x10 != 0, !self.cycle.is_multiple_of(2));
            }
            0x4017 => {
                self.five_step = val & 0x80 != 0;
                self.irq_inhibit = val & 0x40 != 0;
                if self.irq_inhibit {
                    self.frame_irq = false;
                }
                self.reset_delay = if self.cycle.is_multiple_of(2) { 3 } else { 4 };
            }
            _ => {}
        }
    }

    /// CPU read of $4015: length counter status, DMC active, frame and DMC IRQ flags. Clears the frame IRQ.
    pub fn read_status(&mut self) -> u8 {
        let mut v = 0;
        if self.pulse1.length.active() {
            v |= 0x01;
        }
        if self.pulse2.length.active() {
            v |= 0x02;
        }
        if self.triangle.length.active() {
            v |= 0x04;
        }
        if self.noise.length.active() {
            v |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            v |= 0x10;
        }
        if self.frame_irq {
            v |= 0x40;
        }
        if self.dmc.irq {
            v |= 0x80;
        }
        // The read arms the clear rather than doing it: `clock` clears the flag on the next odd
        // (APU "get") cycle, so a read that lands on the cycle the flag was set still sees it set.
        self.frame_irq_clear_pending = true;
        v
    }

    /// True while the APU asserts the CPU IRQ line.
    pub fn irq(&self) -> bool {
        (self.frame_irq && !self.irq_inhibit) || self.dmc.irq
    }

    fn quarter_frame(&mut self) {
        self.pulse1.envelope.clock();
        self.pulse2.envelope.clock();
        self.noise.envelope.clock();
        self.triangle.clock_quarter();
    }

    fn half_frame(&mut self) {
        self.pulse1.clock_half();
        self.pulse2.clock_half();
        self.triangle.clock_half();
        self.noise.length.clock();
    }

    /// The shared prologue of both frame sequences: runs the $4017 reset cycle and bumps the counter.
    /// Returns true when the reset cycle consumed this clock (no event match).
    #[inline]
    fn advance_frame_counter(&mut self) -> bool {
        if self.reset_delay > 0 {
            self.reset_delay -= 1;
            if self.reset_delay == 0 {
                self.frame_cycle = 0;
                if self.five_step {
                    self.quarter_frame();
                    self.half_frame();
                }
                // The reset cycle ends at frame_cycle == 0: no increment, no event match.
                return true;
            }
        }
        self.frame_cycle += 1;
        false
    }

    /// The NTSC (2A03) frame sequence: 240/120/60 Hz, with the frame IRQ in the last step.
    fn clock_frame_counter(&mut self) {
        if self.advance_frame_counter() {
            return;
        }
        match (self.frame_cycle, self.five_step) {
            (7457, _) | (22371, _) => self.quarter_frame(),
            (14913, _) => {
                self.quarter_frame();
                self.half_frame();
            }
            (29828, false) => self.set_frame_irq(false),
            (29829, false) => {
                self.quarter_frame();
                self.half_frame();
                self.set_frame_irq(false);
            }
            (29830, false) => {
                self.set_frame_irq(true);
                self.frame_cycle = 0;
            }
            (37281, true) => {
                self.quarter_frame();
                self.half_frame();
            }
            (37282, true) => self.frame_cycle = 0,
            _ => {}
        }
    }

    /// The PAL (2A07) frame sequence: the NTSC steps 11.46% slower, 200/100/50 Hz instead of
    /// 240/120/60 Hz. The GET/PUT pairs keep the same three-cycle frame-IRQ window as NTSC.
    ///
    /// Kept out of line: inlined into `clock_frame_counter` it doubled that per-cycle function's code
    /// and cost the NTSC path a few percent of the whole frame.
    #[inline(never)]
    fn clock_frame_counter_pal(&mut self) {
        if self.advance_frame_counter() {
            return;
        }
        match (self.frame_cycle, self.five_step) {
            (8313, _) | (24939, _) => self.quarter_frame(),
            (16627, _) => {
                self.quarter_frame();
                self.half_frame();
            }
            (33252, false) => self.set_frame_irq(false),
            (33253, false) => {
                self.quarter_frame();
                self.half_frame();
                self.set_frame_irq(false);
            }
            (33254, false) => {
                self.set_frame_irq(true);
                self.frame_cycle = 0;
            }
            (41565, true) => {
                self.quarter_frame();
                self.half_frame();
            }
            (41566, true) => self.frame_cycle = 0,
            _ => {}
        }
    }

    /// Frame IRQ status flag. For the first two cycles it is set even when inhibited (the CPU IRQ line
    /// still honors the inhibit); on the third (`last`) it follows the inhibit bit.
    fn set_frame_irq(&mut self, last: bool) {
        self.frame_irq = !(last && self.irq_inhibit);
    }

    /// Nonlinear mixer output (0.0..=~1.0) from the five channel levels, plus whatever the
    /// cartridge's expansion audio adds at this instant. Expansion is *not* part of the 2A03's pulse
    /// sum: the cartridge chip drives its own DAC into the console's summing node, where the 2A03's
    /// output and the cartridge's audio pin meet through resistors (NESdev, MMC5 audio, "Hardware":
    /// the 2A03 output and Cart.45 both reach the node through 15k/10k resistors), so it is one
    /// more term added after both curves — with the pulse curve applied to it alone, because that
    /// is the DAC its level is expressed against ("the sound output of the square channels are
    /// equivalent in volume to the corresponding APU channels"). A board that asks for an amplitude
    /// converts to that scale with [`Apu::amplitude_to_pulse_units`], and the round trip now holds
    /// whatever the native channels are doing; before this it did not, and chips ducked under loud
    /// music. It passes through the analog filter chain like the native channels (see [`Apu::clock`]).
    ///
    /// The triangle's term goes through [`Triangle::output`](triangle::Triangle::output), which
    /// reports the held level while `silence_ultrasonic_triangle` is on and its timer is
    /// ultrasonic.
    ///
    /// Each channel's level is multiplied by its [`ChannelGain`] *before* the sums: both
    /// transforms are nonlinear, so scaling after them would spread one channel's change over the
    /// others, and a gain of 0 would not silence anything. There is no separate unity path: a
    /// product with exactly 1.0 is exact and the pulse levels are small whole numbers that add
    /// exactly, so at the default gains every term and sum is the pre-fix expression's bit for bit,
    /// which is what keeps the pinned `:audio` sample digests byte-identical.
    ///
    /// Both nonlinear terms are applied to a magnitude and the sign put back — one for the pulse
    /// sum, one for the triangle/noise/DMC one: neither curve is symmetric, so evaluating it on a
    /// negative level would distort that half of the waveform, and the tnd curve divides by the sum,
    /// so it runs away near zero. For a positive sum both expressions are the pre-fix ones, bit for
    /// bit.
    pub fn mix(&self) -> f32 {
        self.mix_with(None)
    }

    /// The two sides of the stereo mix (#76b) as `(left, right)`.
    ///
    /// Each side is its own nonlinear mix of the channel levels times gain times pan weight: the
    /// weight multiplies the channel's own DAC level *before* the pulse and the triangle/noise/DMC
    /// sums, exactly like [`ChannelGain`], so a channel panned hard away from a side contributes
    /// exactly `0.0` to it and cannot leak across a transform. The pan weights themselves are the
    /// linear balance law in [`ChannelPan`]: a centred channel is at full level on both sides, a
    /// `0.5` pan is half weight on the left and full on the right.
    ///
    /// With every pan centred this returns [`Apu::mix`] on both sides, bit for bit, but
    /// [`Apu::clock`] still takes the mono path in that case — it does not pay for a second chain.
    pub fn mix_stereo(&self) -> (f32, f32) {
        (self.mix_with(Some(true)), self.mix_with(Some(false)))
    }

    /// The mixer proper: `side` is `None` for the mono mix, `Some(left)` for one stereo side.
    ///
    /// `mix` and `mix_stereo` used to carry two copies of these summing rules, which is how a change
    /// to one could miss the other. The only difference between the paths is a channel's
    /// weight, so that is all [`Apu::channel_gain`] decides.
    fn mix_with(&self, side: Option<bool>) -> f32 {
        let p = f32::from(self.pulse1.output()) * self.channel_gain(Channel::Pulse1, side)
            + f32::from(self.pulse2.output()) * self.channel_gain(Channel::Pulse2, side);
        let expansion = self.expansion_pulse * self.channel_gain(Channel::Expansion, side);
        let t = f32::from(
            self.triangle
                .output(self.options.silence_ultrasonic_triangle),
        ) / 8227.0;
        let n = f32::from(self.noise.output()) / 12241.0;
        let d = f32::from(self.dmc.output) / 22638.0;
        let sum = t * self.channel_gain(Channel::Triangle, side)
            + n * self.channel_gain(Channel::Noise, side)
            + d * self.channel_gain(Channel::Dmc, side);
        Self::pulse_term(p) + Self::tnd_term(sum) + Self::pulse_term(expansion)
    }

    /// The pulse channels' curve at `p`, evaluated on `|p|` with `p`'s sign put back: the
    /// native pulse sum's term first, then the cartridge's expansion level as a term of its own.
    fn pulse_term(p: f32) -> f32 {
        if p == 0.0 {
            0.0
        } else {
            (95.88 / (8128.0 / p.abs() + 100.0)).copysign(p)
        }
    }

    /// What `channel` contributes at for `side`: its [`ChannelGain`], times its pan weight on a
    /// stereo side. `None` is the mono mix, where the pan does not apply.
    fn channel_gain(&self, channel: Channel, side: Option<bool>) -> f32 {
        let gain = self.gain.get(channel);
        match side {
            None => gain,
            Some(left) => {
                let (l, r) = self.pan.weights(channel);
                gain * if left { l } else { r }
            }
        }
    }

    /// The triangle/noise/DMC curve at `sum`, evaluated on `|sum|` with `sum`'s sign put back,
    /// exactly as the pulse curve does.
    ///
    /// For `sum > 0` this is the pre-fix expression, bit for bit, and a zero sum stays `0.0`.
    fn tnd_term(sum: f32) -> f32 {
        if sum == 0.0 {
            0.0
        } else {
            (159.79 / (1.0 / sum.abs() + 100.0)).copysign(sum)
        }
    }

    /// True when the next [`Apu::clock`] will run the mixer — the only place `expansion_pulse` is
    /// read — so the bus can fetch the cartridge's level only on those cycles.
    ///
    /// `clock` adds one sample step to `sample_acc` and then evaluates the mixer while
    /// [`BandLimitedSynth::wants_sub`] says a sub-sample is due. This asks that same question of the
    /// same sum, so the answer is exact rather than an estimate: at 44.1 kHz it is true on 176,400
    /// of the 1,789,773 cycles in an emulated second (one in ten), and the other nine cycles' worth
    /// of virtual calls and conversions were thrown away before.
    #[inline]
    pub fn expansion_due(&self) -> bool {
        self.synth
            .wants_sub(self.sample_acc + self.sample_rate / self.cpu_hz)
    }

    /// Advance one CPU cycle: frame counter, channel timers (pulses every second cycle), and
    /// one output sample whenever a sample period has elapsed, through the analog filter chain.
    pub fn clock(&mut self) {
        self.cycle += 1;
        if self.frame_irq_clear_pending && !self.cycle.is_multiple_of(2) {
            self.frame_irq = false;
            self.frame_irq_clear_pending = false;
        }
        // The frame counter runs against the halt/load state from before this cycle's register write
        // (blargg 10/11); `cur_halt` is per-call scratch, not state. On the cycles with no such write
        // (nearly all of them) the swap is a no-op and is skipped.
        let written = self.pulse1.length.written()
            || self.pulse2.length.written()
            || self.triangle.length.written()
            || self.noise.length.written();
        let cur_halt = if written {
            [
                self.pulse1.length.begin_clock(),
                self.pulse2.length.begin_clock(),
                self.triangle.length.begin_clock(),
                self.noise.length.begin_clock(),
            ]
        } else {
            [false; 4]
        };
        // The region is fixed for the life of the machine, so the branch is perfectly predicted, but the
        // NTSC counter below stays a separate function: with the PAL match inlined into it, its code
        // doubled and the per-cycle cost went up by a few percent.
        if self.pal {
            self.clock_frame_counter_pal();
        } else {
            self.clock_frame_counter();
        }
        // A register write on this cycle lands after this APU clock (src/bus.rs), so a halt or load
        // written on this cycle must not be seen by the half-frame above (blargg 10/11).
        if written {
            self.pulse1.length.end_clock(cur_halt[0]);
            self.pulse2.length.end_clock(cur_halt[1]);
            self.triangle.length.end_clock(cur_halt[2]);
            self.noise.length.end_clock(cur_halt[3]);
        }
        self.triangle
            .clock_timer(self.options.silence_ultrasonic_triangle);
        self.noise.clock_timer();
        self.dmc.clock_timer();
        if self.cycle.is_multiple_of(2) {
            self.pulse1.clock_timer();
            self.pulse2.clock_timer();
        }
        let step_key = (self.sample_rate.to_bits(), self.cpu_hz.to_bits());
        if step_key != self.step_key {
            self.step_key = step_key;
            self.sample_step = self.sample_rate / self.cpu_hz;
        }
        self.sample_acc += self.sample_step;
        // The DAC's stepwise level is evaluated `OVERSAMPLE` times per output sample and spread over
        // the band-limiting kernel, so content above the output Nyquist is filtered out instead of
        // folding back into the audible band (see [`synth`]). The sub-samples are the only extra
        // mixer work: the output tick itself is unchanged, so sample counts do not move.
        // #76b: with every pan at centre this is the mono path, unchanged — one mixer, one
        // resynthesizer, one filter chain, so `samples` stays the pre-#76b stream bit for bit and
        // `samples_right` stays empty. Any pan off centre drives a second chain from the same
        // sub-sample phases, so both sides emit one sample per output tick and stay aligned.
        let stereo = !self.pan.is_centred();
        while self.synth.wants_sub(self.sample_acc) {
            let q = self.synth.take_sub();
            if stereo {
                let (left, right) = self.mix_stereo();
                self.synth.feed(left, q);
                self.synth_right.feed(right, q);
            } else {
                let x = self.mix();
                self.synth.feed(x, q);
            }
        }
        if self.sample_acc >= 1.0 {
            self.sample_acc -= 1.0;
            // The DAC's output leaves the console through 2 high-passes and a low-pass; the corners
            // follow the output rate, so a rate the frontend wrote directly is picked up here. The
            // analog chain runs at the output rate, after the band-limited decimation: its
            // coefficients are derived from that rate.
            if self.filter.rate() != self.sample_rate {
                self.filter.set_rate(self.sample_rate);
            }
            let x = self.synth.next_sample();
            self.samples.push(self.filter.process(x));
            if stereo {
                if self.filter_right.rate() != self.sample_rate {
                    self.filter_right.set_rate(self.sample_rate);
                }
                let x = self.synth_right.next_sample();
                self.samples_right.push(self.filter_right.process(x));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{StateReader, StateWriter};

    /// 64-bit FNV-1a over everything folded in, so one number pins a whole stream bit for bit.
    struct Digest(u64);

    impl Digest {
        fn new() -> Self {
            Digest(0xcbf2_9ce4_8422_2325)
        }

        fn bytes(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0 ^= u64::from(b);
                self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }

    /// The registers the stimulus writes: every channel register, the DMC's direct load and the
    /// frame counter. `$4015` is written once up front, so the DMC never asks for a DMA the bare
    /// APU cannot serve.
    const STIMULUS_REGS: [u16; 15] = [
        0x4000, 0x4001, 0x4002, 0x4003, 0x4004, 0x4005, 0x4006, 0x4007, 0x4008, 0x400A, 0x400B,
        0x400C, 0x400E, 0x400F, 0x4011,
    ];

    /// `cycles` CPU cycles of a fixed pseudo-random register stream and expansion level, folding
    /// `expansion_due` and `sample_acc` on every cycle and then every drained sample (left, right)
    /// into `d`. Returns the loudest drained sample.
    fn drive(apu: &mut Apu, cycles: u32, seed: &mut u64, d: &mut Digest) -> f32 {
        for c in 0..cycles {
            if c % 997 == 0 {
                *seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let r = (*seed >> 32) as u32;
                let reg = STIMULUS_REGS[r as usize % STIMULUS_REGS.len()];
                // One write in sixteen goes to the frame counter instead.
                let reg = if (r >> 16) & 0xF == 0 { 0x4017 } else { reg };
                apu.write_register(reg, (r >> 8) as u8);
                // Expansion level in pulse units, both signs, and exactly zero a quarter of the time.
                apu.expansion_pulse = match (r >> 24) & 3 {
                    0 => 0.0,
                    _ => ((r >> 26) as f32 - 31.5) * 0.75,
                };
            }
            d.bytes(&[u8::from(apu.expansion_due())]);
            apu.clock();
            d.bytes(&apu.sample_acc.to_bits().to_le_bytes());
        }
        let mut peak = 0.0f32;
        for s in apu.samples.drain(..).chain(apu.samples_right.drain(..)) {
            d.bytes(&s.to_bits().to_le_bytes());
            peak = peak.max(s.abs());
        }
        peak
    }

    /// A scenario's setup, run on the APU before each phase with the phase number.
    type Hook = fn(&mut Apu, usize);

    /// Eight phases of 50,000 cycles with `hook` run before each, then the final save state.
    fn stream_digest(hook: Hook) -> u64 {
        let mut apu = Apu::new();
        let mut seed = 0x2A03_u64;
        let mut d = Digest::new();
        apu.write_register(0x4015, 0x0F);
        for phase in 0..8 {
            hook(&mut apu, phase);
            let peak = drive(&mut apu, 50_000, &mut seed, &mut d);
            assert!(
                peak > 0.05,
                "phase {phase} is near-silent ({peak}): the stimulus must be audible"
            );
        }
        let mut w = StateWriter::new();
        apu.save_state(&mut w);
        d.bytes(&w.finish());
        d.0
    }

    fn some_gains(apu: &mut Apu) {
        apu.gain = ChannelGain {
            pulse1: 0.5,
            pulse2: 1.25,
            triangle: 0.0,
            noise: 2.0,
            dmc: 0.75,
            expansion: 0.3,
        };
    }

    /// Sample-identity ratchet for the mixer, the resampler step and the analog chain: a fixed
    /// stimulus through every path `clock` has — default and non-unity gains, mono and stereo, the
    /// three regions, rates written through `set_sample_rate` and directly into the public fields,
    /// soft reset, a state round trip and both host options. The digests were taken on main before
    /// those cleanups and re-taken once, when expansion audio left the pulse sum for a term of its
    /// own -- the stimulus feeds a non-zero expansion level on three cycles in four, so every
    /// scenario moved. A refactor of this code must leave every one of them alone. The 2A03-only
    /// stream is pinned separately, unchanged by that fix, by
    /// `tests/expansion_audio.rs`'s `a_2a03_only_sample_stream_is_unchanged`.
    #[test]
    fn output_stream_digests_are_pinned() {
        let scenarios: [(&str, Hook, u64); 7] = [
            ("ntsc default", |_, _| {}, 0x4e19_9709_bd6d_9d20),
            (
                "gains, then back to unity",
                |apu, phase| match phase {
                    0 => some_gains(apu),
                    4 => apu.gain = ChannelGain::default(),
                    _ => {}
                },
                0x96ed_42c3_0653_c943,
            ),
            (
                "stereo, then centred",
                |apu, phase| match phase {
                    0 => {
                        some_gains(apu);
                        apu.pan = ChannelPan {
                            pulse1: -1.0,
                            pulse2: 0.5,
                            triangle: 0.25,
                            noise: -0.6,
                            dmc: 1.0,
                            expansion: -0.3,
                        };
                    }
                    5 => apu.pan = ChannelPan::default(),
                    _ => {}
                },
                0x5bd1_07df_aedb_d717,
            ),
            (
                "pal, ultrasonic triangle silenced",
                |apu, phase| match phase {
                    0 => apu.set_region(Region::Pal),
                    2 => apu.options.silence_ultrasonic_triangle = true,
                    _ => {}
                },
                0xb9c1_4c4a_fc3c_49fc,
            ),
            (
                "dendy at 48 kHz",
                |apu, phase| {
                    if phase == 0 {
                        apu.set_region(Region::Dendy);
                        apu.set_sample_rate(48_000.0);
                    }
                },
                0x65e8_466d_21a9_0669,
            ),
            (
                "rates and clocks changed mid-run",
                |apu, phase| match phase {
                    1 => apu.sample_rate = 96_000.0,
                    2 => apu.set_sample_rate(22_050.0),
                    3 => apu.cpu_hz = 1_700_000.0,
                    4 => apu.set_region(Region::Pal),
                    5 => apu.sample_rate = 31_250.5,
                    6 => {
                        apu.set_region(Region::Ntsc);
                        apu.set_sample_rate(SAMPLE_RATE);
                    }
                    _ => {}
                },
                0xf4e6_deca_c1c2_58b6,
            ),
            (
                "dmc pop softened, soft reset, state round trip",
                |apu, phase| match phase {
                    0 => apu.options.reduce_dmc_pop = true,
                    3 => apu.soft_reset(),
                    5 => {
                        let mut w = StateWriter::new();
                        apu.save_state(&mut w);
                        let bytes = w.finish();
                        apu.load_state(&mut StateReader::new(&bytes)).unwrap();
                    }
                    _ => {}
                },
                0x4298_c34d_4ccf_a3f2,
            ),
        ];
        let got: Vec<(&str, u64)> = scenarios
            .iter()
            .map(|&(name, hook, _)| (name, stream_digest(hook)))
            .collect();
        for (&(name, _, want), &(_, digest)) in scenarios.iter().zip(&got) {
            assert_eq!(
                digest, want,
                "{name}: {digest:#018x}, pinned {want:#018x} (all: {got:x?})"
            );
        }
    }

    /// Ratchet for the length-counter snapshot around the frame counter: the four counters'
    /// values, the frame IRQ flag and the IRQ line after every cycle, every `$4015` status read,
    /// and the save-state bytes (then a load of them) every 100,000 cycles, in both regions and
    /// both sequencer modes. Writes land at random and on every cycle within one of a half-frame
    /// step or a five-step `$4017` reset, the cycles where the snapshot decides what the clock
    /// sees. Pinned on main before the snapshot was made conditional.
    #[test]
    fn length_counter_and_frame_irq_trace_is_pinned() {
        const REGS: [u16; 9] = [
            0x4000, 0x4003, 0x4004, 0x4007, 0x4008, 0x400B, 0x400C, 0x400F, 0x4015,
        ];
        let mut d = Digest::new();
        for (region, half_frames) in [
            (Region::Ntsc, [14913, 29829, 37281]),
            (Region::Pal, [16627, 33253, 41565]),
        ] {
            let mut apu = Apu::new();
            apu.set_region(region);
            apu.write_register(0x4015, 0x0F);
            let mut seed = 0x0414_u64;
            let mut coincident = 0;
            for c in 0..400_000u32 {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let r = (seed >> 32) as u32;
                if c % 50_000 == 25_000 {
                    // Alternate the sequencer mode; either inhibit.
                    apu.write_register(0x4017, (((c / 50_000) as u8 & 1) << 7) | (r as u8 & 0x40));
                }
                let next = apu.frame_cycle + 1;
                let half = if apu.reset_delay == 1 {
                    apu.five_step
                } else {
                    half_frames.contains(&next)
                };
                let near =
                    half_frames.iter().any(|&s| s.abs_diff(next) <= 1) || apu.reset_delay == 1;
                if near || r & 7 == 0 {
                    let reg = REGS[(r >> 8) as usize % REGS.len()];
                    // $4015 keeps the DMC off: the bare APU cannot serve its DMA.
                    let val = if reg == 0x4015 {
                        (r >> 16) as u8 & 0x0F
                    } else {
                        (r >> 16) as u8
                    };
                    apu.write_register(reg, val);
                }
                if r & 0x3F == 1 {
                    d.bytes(&[apu.read_status()]);
                }
                let lengths = [
                    &apu.pulse1.length,
                    &apu.pulse2.length,
                    &apu.triangle.length,
                    &apu.noise.length,
                ];
                if half
                    && lengths
                        .iter()
                        .any(|l| l.pending.is_some() || l.halt != l.snap_halt)
                {
                    coincident += 1;
                }
                apu.clock();
                d.bytes(&[
                    apu.pulse1.length.value,
                    apu.pulse2.length.value,
                    apu.triangle.length.value,
                    apu.noise.length.value,
                    u8::from(apu.frame_irq),
                    u8::from(apu.irq()),
                ]);
                if c % 100_000 == 99_999 {
                    let mut w = StateWriter::new();
                    apu.save_state(&mut w);
                    let bytes = w.finish();
                    d.bytes(&bytes);
                    apu.load_state(&mut StateReader::new(&bytes)).unwrap();
                }
            }
            assert!(
                coincident >= 10,
                "{region:?}: only {coincident} writes met a half-frame clock"
            );
        }
        assert_eq!(d.0, 0x8077_96b9_db50_65f8, "trace digest {:#018x}", d.0);
    }

    /// The triangle/noise/DMC curve is evaluated on `|sum|` with the sign put back, so a
    /// negative gain distorts one half of a waveform no more than the positive half. Before the
    /// fix the expression divided by the negative sum, which runs away where `1/sum` meets -100.
    #[test]
    fn the_tnd_term_is_sign_symmetric() {
        for sum in [
            0.001f32, 0.01, 0.011, 0.0125, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 10.0,
        ] {
            let positive = Apu::tnd_term(sum);
            let negative = Apu::tnd_term(-sum);
            assert_eq!(
                negative.to_bits(),
                (-positive).to_bits(),
                "the tnd term is not symmetric at {sum}"
            );
            assert!(
                negative.is_finite() && negative.abs() <= 1.6,
                "sum -{sum} -> {negative}"
            );
        }
        // The old asymmetry, in full: `1.0 / -0.01 f32` rounds to exactly -100, so the denominator
        // is zero and the term is +infinity; either side of it the magnitude is several times the
        // fixed one. The curve is monotonic in `sum`, so this is a shape change, not a scale.
        assert!((159.79 / (1.0 / -0.01f32 + 100.0)).is_infinite());
        for sum in [0.011f32, 0.0125, 0.05, 0.1, 0.25, 0.5, 1.0] {
            let old = 159.79 / (1.0 / -sum + 100.0);
            let new = Apu::tnd_term(-sum);
            // The old curve came out *positive* for a negative sum: the wrong half of the waveform
            // was lifted instead of mirrored.
            assert!(old > 0.0 && new < 0.0, "sum -{sum}: old {old}, new {new}");
            if sum <= 0.0125 {
                assert!(old > 5.0 * new.abs(), "sum -{sum}: old {old}, new {new}");
            }
        }
        let new = Apu::tnd_term(-0.01);
        assert!(
            (new + 0.798_95).abs() < 0.001,
            "the fixed value at -0.01 is {new}"
        );
        assert_eq!(Apu::tnd_term(0.0).to_bits(), 0.0f32.to_bits());
        assert_eq!(Apu::tnd_term(-0.0).to_bits(), 0.0f32.to_bits());
    }

    /// For a positive sum the term is the pre-fix expression, bit for bit — positive gains are
    /// untouched, which is what keeps the pinned sample digests byte-identical.
    #[test]
    fn the_tnd_term_is_unchanged_for_positive_sums() {
        for sum in [0.0f32, 0.001, 0.01, 0.1, 0.25, 0.5, 1.0, 2.0, 10.0, 100.0] {
            let before = if sum == 0.0 {
                0.0
            } else {
                159.79 / (1.0 / sum + 100.0)
            };
            assert_eq!(Apu::tnd_term(sum).to_bits(), before.to_bits(), "sum {sum}");
        }
    }
}
