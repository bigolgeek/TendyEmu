//! Read-only snapshots of the APU for debug views (data layer only, no UI).
//!
//! Every view is an owned struct derived from an `&Apu`: no mutation and no side effects. Derived
//! fields (period indexes, sweep targets, mute state) are computed from the same fields
//! [`super::state`] serializes, so a save/load round trip cannot desynchronize a viewer. The one
//! exception is the region flag and the PAL indexes derived from it: the region comes from the
//! cartridge (`set_region`), not the save state, and is fixed before any state load. That
//! consistency, and its exception, are asserted by tests below.

use super::Apu;
use super::dmc::{DmaKind, Dmc, rates};
use super::noise::{Noise, periods};
use super::pulse::Pulse;
use super::triangle::Triangle;
use super::units::{Envelope, LengthCounter};

/// Index of `period` in `table`, or `None` when it is not an entry (e.g. an unwritten period).
fn table_index(table: &[u16], period: u16) -> Option<u8> {
    table.iter().position(|&p| p == period).map(|i| i as u8)
}

fn view_envelope(e: &Envelope) -> EnvelopeView {
    EnvelopeView {
        looping: e.looping,
        constant: e.constant,
        constant_volume: e.volume,
        divider: e.divider,
        decay: e.decay,
        start: e.start,
    }
}

fn view_length(l: &LengthCounter) -> LengthView {
    LengthView {
        value: l.value,
        halt: l.halt,
        enabled: l.enabled,
    }
}

/// Envelope state shared by pulse and noise (`$400x` bits 0-4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvelopeView {
    pub looping: bool,
    pub constant: bool,
    /// Constant volume, or the decay period.
    pub constant_volume: u8,
    pub divider: u8,
    pub decay: u8,
    /// Reload flag: set by `$4003`/`$400F`, cleared by the next quarter-frame clock.
    pub start: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LengthView {
    pub value: u8,
    pub halt: bool,
    pub enabled: bool,
}

/// Sweep unit of a pulse channel, plus its derived target period.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepView {
    pub enabled: bool,
    /// Sweep divider period as written to `$4001` bits 4-6.
    pub period: u8,
    pub negate: bool,
    pub shift: u8,
    pub divider: u8,
    pub reload: bool,
    /// The period the sweep would move to on its next update (sat rules included).
    pub target: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PulseView {
    pub duty: u8,
    /// Position in the current duty sequence (the timer has stepped this many times mod 8 since the last `$4003` write).
    pub duty_pos: u8,
    pub timer_period: u16,
    /// Cycles remaining on the current timer period (every second CPU cycle).
    pub timer: u16,
    /// Current output level 0..=15 after mute rules.
    pub output: u8,
    pub muted: bool,
    pub envelope: EnvelopeView,
    pub length: LengthView,
    pub sweep: SweepView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriangleView {
    /// Control flag: halts the length counter and keeps the linear counter reloading.
    pub control: bool,
    pub linear_reload_value: u8,
    pub linear_counter: u8,
    pub linear_reload: bool,
    pub timer_period: u16,
    pub timer: u16,
    /// Position in the 32-step sequence (ungated output level; the timer only steps it when both counters are non-zero).
    pub seq_pos: u8,
    pub length: LengthView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoiseView {
    /// Mode flag: feedback from bit 6 (short mode) instead of bit 1.
    pub mode: bool,
    /// Position of `timer_period` in the region's period table.
    pub period_index: Option<u8>,
    pub timer_period: u16,
    pub timer: u16,
    /// The 15-bit LFSR state.
    pub shift: u16,
    /// Current output level 0..=15 after mute rules.
    pub output: u8,
    pub envelope: EnvelopeView,
    pub length: LengthView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmcView {
    /// Position of `timer_period` in the region's rate table.
    pub rate_index: Option<u8>,
    pub timer_period: u16,
    pub timer: u16,
    /// The 7-bit delta output level.
    pub output: u8,
    /// `$4012`-derived start address of the sample (`$C000 + 64*n`).
    pub sample_addr: u16,
    pub sample_len: u16,
    pub current_addr: u16,
    pub bytes_remaining: u16,
    pub buffer: Option<u8>,
    pub shift: u8,
    pub bits_remaining: u8,
    /// True while the output unit is silent because the sample buffer is empty (the core sets it
    /// on an empty refill and clears it when a byte is consumed from the buffer into the shift
    /// register, thereby re-enabling the output unit).
    pub silence: bool,
    pub looping: bool,
    pub irq_enabled: bool,
    pub irq: bool,
    /// DMA request currently latched, mirroring what the save state stores.
    pub dma_req: Option<DmaKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameCounterView {
    pub five_step: bool,
    pub irq_inhibit: bool,
    /// Frame IRQ flag as visible to the next `$4015` read.
    pub frame_irq: bool,
    /// CPU cycles into the frame sequence.
    pub frame_cycle: u32,
    /// CPU cycles until a pending `$4017` reset lands (0 = none).
    pub reset_delay: u8,
}

/// The instantaneous inputs of `Apu::mix()`. The core does not track per-frame aggregates; it mixes
/// these levels whenever a sample fires, so the snapshot is the mixer's current inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixerView {
    pub pulse1: u8,
    pub pulse2: u8,
    /// Ungated waveform level; the sequence gate is internal to the timer.
    pub triangle: u8,
    pub noise: u8,
    pub dmc: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApuSnapshot {
    /// CPU cycles since power-on (the frame counter, samples and periodic IRQs key off this).
    pub cycle: u64,
    /// Region flag (the PAL tables the noise/DMC indexes were derived from).
    pub pal: bool,
    pub pulse1: PulseView,
    pub pulse2: PulseView,
    pub triangle: TriangleView,
    pub noise: NoiseView,
    pub dmc: DmcView,
    pub frame_counter: FrameCounterView,
    pub mixer_inputs: MixerView,
}

impl PulseView {
    /// Uses the write-side mute/target rules, `Pulse::muted`/`Pulse::sweep_target`, which depend
    /// on the channel's ones'-complement mode, so pulse 1 and 2 need no special case here.
    fn capture(p: &Pulse) -> Self {
        PulseView {
            duty: p.duty,
            duty_pos: p.duty_pos,
            timer_period: p.timer_period,
            timer: p.timer,
            output: p.output(),
            muted: p.muted(),
            envelope: view_envelope(&p.envelope),
            length: view_length(&p.length),
            sweep: SweepView {
                enabled: p.sweep_enabled,
                period: p.sweep_period,
                negate: p.sweep_negate,
                shift: p.sweep_shift,
                divider: p.sweep_divider,
                reload: p.sweep_reload,
                target: p.sweep_target(),
            },
        }
    }
}

impl TriangleView {
    fn capture(t: &Triangle) -> Self {
        TriangleView {
            control: t.control,
            linear_reload_value: t.linear_reload_value,
            linear_counter: t.linear_counter,
            linear_reload: t.linear_reload,
            timer_period: t.timer_period,
            timer: t.timer,
            seq_pos: t.seq_pos,
            length: view_length(&t.length),
        }
    }
}

impl NoiseView {
    fn capture(n: &Noise) -> Self {
        NoiseView {
            mode: n.mode,
            period_index: table_index(periods(n.pal), n.timer_period),
            timer_period: n.timer_period,
            timer: n.timer,
            shift: n.shift,
            output: n.output(),
            envelope: view_envelope(&n.envelope),
            length: view_length(&n.length),
        }
    }
}

impl DmcView {
    fn capture(d: &Dmc) -> Self {
        DmcView {
            rate_index: table_index(rates(d.pal), d.timer_period),
            timer_period: d.timer_period,
            timer: d.timer,
            output: d.output,
            sample_addr: d.sample_addr,
            sample_len: d.sample_len,
            current_addr: d.current_addr,
            bytes_remaining: d.bytes_remaining,
            buffer: d.buffer,
            shift: d.shift,
            bits_remaining: d.bits_remaining,
            silence: d.silence,
            looping: d.looping,
            irq_enabled: d.irq_enabled,
            irq: d.irq,
            dma_req: d.dma_req,
        }
    }
}

impl FrameCounterView {
    fn capture(a: &Apu) -> Self {
        FrameCounterView {
            five_step: a.five_step,
            irq_inhibit: a.irq_inhibit,
            frame_irq: a.frame_irq,
            frame_cycle: a.frame_cycle,
            reset_delay: a.reset_delay,
        }
    }
}

impl MixerView {
    fn capture(a: &Apu) -> Self {
        MixerView {
            pulse1: a.pulse1.output(),
            pulse2: a.pulse2.output(),
            triangle: a.triangle.output(a.options.silence_ultrasonic_triangle),
            noise: a.noise.output(),
            dmc: a.dmc.output,
        }
    }
}

/// Snapshot of pulse 1.
pub fn pulse1(apu: &Apu) -> PulseView {
    PulseView::capture(&apu.pulse1)
}

/// Snapshot of pulse 2.
pub fn pulse2(apu: &Apu) -> PulseView {
    PulseView::capture(&apu.pulse2)
}

/// Snapshot of the triangle channel.
pub fn triangle(apu: &Apu) -> TriangleView {
    TriangleView::capture(&apu.triangle)
}

/// Snapshot of the noise channel.
pub fn noise(apu: &Apu) -> NoiseView {
    NoiseView::capture(&apu.noise)
}

/// Snapshot of the DMC.
pub fn dmc(apu: &Apu) -> DmcView {
    DmcView::capture(&apu.dmc)
}

/// Snapshot of the frame counter and its IRQ status.
pub fn frame_counter(apu: &Apu) -> FrameCounterView {
    FrameCounterView::capture(apu)
}

/// The instantaneous mixer inputs.
pub fn mixer_inputs(apu: &Apu) -> MixerView {
    MixerView::capture(apu)
}

/// Full snapshot of the APU state a viewer needs.
pub fn snapshot(apu: &Apu) -> ApuSnapshot {
    ApuSnapshot {
        cycle: apu.cycle,
        pal: apu.pal,
        pulse1: pulse1(apu),
        pulse2: pulse2(apu),
        triangle: triangle(apu),
        noise: noise(apu),
        dmc: dmc(apu),
        frame_counter: frame_counter(apu),
        mixer_inputs: mixer_inputs(apu),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Region;
    use crate::state::{StateReader, StateWriter};

    fn pulse1_setup() -> Apu {
        let mut a = Apu::new();
        // Pulse 1: duty 2, halted length, constant volume 15, period $010, length 254.
        a.write_register(0x4015, 0x01);
        a.write_register(0x4000, 0xBF);
        a.write_register(0x4001, 0x83); // sweep enabled, period 0, shift 3
        a.write_register(0x4002, 0x10);
        a.write_register(0x4003, 0x08);
        a
    }

    #[test]
    fn pulse_snapshots_from_the_register_writes() {
        let p = pulse1(&pulse1_setup());
        assert_eq!(p.duty, 2);
        assert_eq!(p.duty_pos, 0);
        assert_eq!(p.timer_period, 0x010);
        assert_eq!(p.timer, 0);
        assert_eq!(p.length.value, 254);
        assert!(p.length.halt);
        assert!(p.length.enabled);
        let e = p.envelope;
        assert!(e.looping);
        assert!(e.constant);
        assert_eq!(e.constant_volume, 15);
        let s = p.sweep;
        assert_eq!(
            (s.enabled, s.period, s.negate, s.shift),
            (true, 0, false, 3)
        );
        assert_eq!(s.target, 0x010 + (0x010 >> 3));
        assert!(s.reload);
        assert!(!p.muted);
        // Duty step 0 of duty 2 is silent.
        assert_eq!(p.output, 0);
    }

    #[test]
    fn pulse2_takes_the_two_s_complement_negate_rule() {
        let mut a = Apu::new();
        a.write_register(0x4015, 0x03); // both lengths enabled
        // Both pulses: period $100, duty 2, halted length, sweep enabled + negate, shift 1.
        a.write_register(0x4000, 0xBF);
        a.write_register(0x4001, 0x89);
        a.write_register(0x4002, 0x00);
        a.write_register(0x4003, 0x81); // period high bits 1 ($100), length index 16 -> 16
        a.write_register(0x4004, 0xBF);
        a.write_register(0x4005, 0x89);
        a.write_register(0x4006, 0x00);
        a.write_register(0x4007, 0x81);
        // Pulse 1 with ones' complement subtracts change + 1: $100 - ($80 + 1) saturates at $7F.
        assert_eq!(pulse1(&a).sweep.target, 0x7F);
        // Pulse 2 subtracts change once: $100 - $80 = $80.
        let p = pulse2(&a);
        assert_eq!(p.sweep.target, 0x80);
        assert_eq!(p.timer_period, 0x100);
        assert_eq!(p.length.value, 12);
        assert!(p.length.halt);
        assert!(p.sweep.enabled);
        assert!(p.sweep.negate);
    }

    #[test]
    fn pulse_timer_and_duty_step_through_clocking() {
        let mut a = pulse1_setup();
        a.write_register(0x4002, 0x08); // period 8: not muted

        for _ in 0..2 {
            a.clock();
        }

        let p = pulse1(&a);
        // The first even CPU cycle reloads the period and advances the duty sequence.
        assert_eq!(p.timer, 8);
        assert_eq!(p.duty_pos, 1);
        assert_eq!(p.output, 15);
        assert_eq!(mixer_inputs(&a).pulse1, 15);
    }

    #[test]
    fn pulse_below_8_is_flagged_muted() {
        let p = pulse1(&pulse1_setup());
        assert!(!p.muted);
        let mut a = pulse1_setup();
        a.write_register(0x4002, 0x07);
        assert!(pulse1(&a).muted);
    }

    #[test]
    fn pulse_length_stops_clocking_when_halted() {
        let mut a = pulse1_setup();
        // 14915 cycles: past the first half-frame clock at frame cycle 14913, so the length
        // counter has actually had a chance to clock (exactly once; the next half frame is at
        // 29829). Halted length counter: it must not have decremented.
        let cycles = 14_915;
        for _ in 0..cycles {
            a.clock();
        }
        assert_eq!(pulse1(&a).length.value, 254);
    }

    #[test]
    fn pulse_length_decrements_at_the_half_frame_when_not_halted() {
        let mut a = pulse1_setup();
        a.write_register(0x4000, 0x9F); // same setup without the halt flag
        let cycles = 14_915;
        for _ in 0..cycles {
            a.clock();
        }
        // Exactly one half-frame clock in this window dropped the counted-down length by one.
        assert_eq!(pulse1(&a).length.value, 253);
    }

    #[test]
    fn triangle_snapshots_from_the_register_writes() {
        let mut a = Apu::new();
        a.write_register(0x4015, 0x04);
        a.write_register(0x4008, 0x85); // control, linear reload value 5
        a.write_register(0x400A, 0x02);
        a.write_register(0x400B, 0x08); // length index 1 -> 254, linear reload flag set
        let t = triangle(&a);
        assert!(t.control);
        assert_eq!(t.linear_reload_value, 5);
        assert_eq!(t.linear_counter, 0);
        assert!(t.linear_reload);
        assert_eq!(t.timer_period, 2);
        assert_eq!(t.timer, 0);
        assert_eq!(t.seq_pos, 0);
        assert_eq!(t.length.value, 254);
        assert!(t.length.halt);
    }

    #[test]
    fn triangle_linear_counter_loads_on_the_next_quarter_frame() {
        let mut a = Apu::new();
        a.write_register(0x4015, 0x04);
        a.write_register(0x4008, 0x05); // no control: one reload only
        a.write_register(0x400A, 0x02);
        a.write_register(0x400B, 0x08);

        for _ in 0..7458 {
            a.clock();
        }

        let t = triangle(&a);
        assert_eq!(t.linear_counter, 5);
        assert!(!t.linear_reload);
    }

    #[test]
    fn noise_snapshots_mode_and_period_index() {
        let mut a = Apu::new();
        a.write_register(0x4015, 0x08);
        a.write_register(0x400C, 0x3F); // halt, constant volume 15
        a.write_register(0x400E, 0x84); // short mode, period index 4
        a.write_register(0x400F, 0x08); // length index 1 -> 254, envelope start
        let n = noise(&a);
        assert!(n.mode);
        assert_eq!(n.period_index, Some(4));
        assert_eq!(n.timer_period, 64);
        assert_eq!(n.length.value, 254);
        assert!(n.length.halt);
        assert!(n.envelope.constant);
        assert_eq!(n.envelope.constant_volume, 15);
        // Power-on LFSR is 1; one clock shifts it to bit 14.
        for _ in 0..3 {
            a.clock();
        }
        assert_eq!(noise(&a).shift, 0x4000);
    }

    #[test]
    fn noise_period_index_reads_the_pal_table_when_the_region_is_pal() {
        let mut a = Apu::new();
        a.set_region(Region::Pal);
        a.write_register(0x400E, 0x03);
        let n = noise(&a);
        assert_eq!(n.period_index, Some(3));
        assert_eq!(n.timer_period, 30);
    }

    #[test]
    fn dmc_snapshots_the_sample_registers_and_dma_status() {
        let mut a = Apu::new();
        a.write_register(0x4010, 0x05); // rate index 5, IRQ disabled
        a.write_register(0x4012, 0x01); // sample address $C040
        a.write_register(0x4013, 0x01); // sample length 17
        a.write_register(0x4015, 0x10); // enable: load DMA 2 cycles later
        for _ in 0..3 {
            a.clock();
        }
        let d = dmc(&a);
        assert_eq!(d.rate_index, Some(5));
        assert_eq!(d.timer_period, 254);
        assert_eq!(d.sample_addr, 0xC040);
        assert_eq!(d.sample_len, 17);
        assert_eq!(d.current_addr, 0xC040);
        assert_eq!(d.bytes_remaining, 17);
        assert_eq!(d.buffer, None);
        assert_eq!(d.dma_req, Some(DmaKind::Load));
        assert!(!d.irq);
        assert!(d.silence);
        assert_eq!(d.output, 0);
        // The first timer expiry consumed one bit.
        assert_eq!(d.bits_remaining, 7);
    }

    #[test]
    fn dmc_rate_index_reads_the_pal_table() {
        let mut a = Apu::new();
        a.set_region(Region::Pal);
        a.write_register(0x4010, 0x03);
        let d = dmc(&a);
        assert_eq!(d.rate_index, Some(3));
        assert_eq!(d.timer_period, 298);
    }

    #[test]
    fn dmc_flags_and_direct_load() {
        let mut a = Apu::new();
        a.write_register(0x4010, 0xC0); // looping, IRQ enable
        a.write_register(0x4011, 0x40);
        a.write_register(0x4013, 0x00); // one-byte sample
        a.write_register(0x4015, 0x10);
        // Bypassing the bus, so the DMC never fetches; the IRQ flag is only set after the byte is
        // consumed — instead assert the direct load of the output level and the enabled flag.
        let d = dmc(&a);
        assert!(d.irq_enabled);
        assert!(d.looping);
        assert_eq!(d.output, 64);
    }

    #[test]
    fn frame_counter_sets_the_irq_flag_at_the_end_of_the_sequence() {
        let mut a = Apu::new();
        for _ in 0..29831 {
            a.clock();
        }
        let fc = frame_counter(&a);
        assert!(fc.frame_irq);
        assert!(!fc.irq_inhibit);
        assert!(!fc.five_step);
        // The sequence wrapped at step 29830.
        assert_eq!(fc.frame_cycle, 1);
    }

    #[test]
    fn write_4017_selects_five_step_and_can_inhibit_the_frame_irq() {
        let mut a = Apu::new();
        a.write_register(0x4017, 0xC0); // 5-step, IRQ inhibit
        for _ in 0..30_000 {
            a.clock();
        }
        let fc = frame_counter(&a);
        assert!(fc.five_step);
        assert!(fc.irq_inhibit);
        // 5-step mode never raises the frame IRQ in the first place; the inhibit is set here too.
        assert!(!fc.frame_irq);
    }

    #[test]
    fn counts_elapsed_cpu_cycles() {
        let mut a = Apu::new();
        for _ in 0..7 {
            a.clock();
        }
        assert_eq!(snapshot(&a).cycle, 7);
    }

    #[test]
    fn mixer_inputs_are_the_inst_channel_levels() {
        let mut a = pulse1_setup();
        a.write_register(0x4002, 0x08);
        for _ in 0..2 {
            a.clock();
        }
        let m = mixer_inputs(&a);
        assert_eq!(m.pulse1, 15);
        assert_eq!(m.pulse2, 0);
        // The triangle level is the ungated waveform value (15 at seq_pos 0).
        assert_eq!(m.triangle, 15);
        assert_eq!(m.noise, 0);
        assert_eq!(m.dmc, 0);
    }

    /// The snapshot's derived fields are computed from fields `Apu::save_state`/`load_state`
    /// serialize, so a save/load round trip reproduces it exactly. The region flag is the
    /// documented exception (it comes from the cartridge, not the state), covered by
    /// `snapshot_round_trips_in_pal`.
    #[test]
    fn snapshot_round_trips_through_save_state() {
        let mut a = pulse1_setup();
        a.write_register(0x4001, 0x87);
        a.write_register(0x4004, 0xFF);
        a.write_register(0x4004, 0x81); // pulse 2 sweep enabled
        a.write_register(0x4006, 0xAB);
        a.write_register(0x4007, 0x68);
        a.write_register(0x4008, 0x05);
        a.write_register(0x400A, 0x34);
        a.write_register(0x400B, 0x18);
        a.write_register(0x400C, 0x1F);
        a.write_register(0x400E, 0x84);
        a.write_register(0x400F, 0x18);
        a.write_register(0x4010, 0x8F); // rate index 15, IRQ enable
        a.write_register(0x4011, 0x2A);
        a.write_register(0x4012, 0x55);
        a.write_register(0x4013, 0x77);
        a.write_register(0x4017, 0x80); // 5-step mode
        for _ in 0..2000 {
            a.clock();
        }

        let before = snapshot(&a);
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let data = w.finish();

        let mut b = Apu::new();
        b.load_state(&mut StateReader::new(&data)).unwrap();

        assert_eq!(snapshot(&b), before);
    }

    /// Same round trip in PAL, where the derived indexes come from the PAL tables. The region is
    /// not serialized by `save_state`, so the load target must already be a PAL `Apu` — the
    /// machine's region is fixed before any state load.
    #[test]
    fn snapshot_round_trips_in_pal() {
        let mut a = Apu::new();
        a.set_region(Region::Pal);
        a.write_register(0x4015, 0x18);
        // Either channel's PAL-table lookup: noise period index 3, DMC rate index 3.
        a.write_register(0x400E, 0x03);
        a.write_register(0x4010, 0x03);
        for _ in 0..2000 {
            a.clock();
        }

        let before = snapshot(&a);
        assert!(before.pal);

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let data = w.finish();

        let mut b = Apu::new();
        b.set_region(Region::Pal);
        b.load_state(&mut StateReader::new(&data)).unwrap();

        assert_eq!(snapshot(&b), before);
        assert_eq!(noise(&a).period_index, Some(3));
        assert_eq!(dmc(&a).rate_index, Some(3));
    }
}
