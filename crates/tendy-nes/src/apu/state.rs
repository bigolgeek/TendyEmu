//! Save-state serialisation for the APU (fixed field order; every field in declaration order).
//!
//! `sample_rate`, `gain`, `pan` and the audio options are frontend settings, the triangle's
//! held ultrasonic level is the analog output rather than emulated state, and `samples` and
//! `samples_right` are drain buffers, so none of them is stored; the buffers are cleared on load
//! instead.
//!
//! #76b's right-hand side is not stored either, on the `sample_rate` precedent: `samples_right`,
//! `filter_right` and `synth_right` exist only to serve a non-centred
//! [`ChannelPan`](super::gain::ChannelPan), and the pan itself is the frontend's. The state's audio
//! is the left stream, so a load resets the right-hand chain rather than let it replay audio from
//! before the load, and `STATE_VERSION` does not move.

use super::Apu;
use super::dmc::{DmaKind, Dmc};
use super::noise::Noise;
use super::pulse::Pulse;
use super::triangle::Triangle;
use super::units::{Envelope, LengthCounter};
use crate::state::{StateError, StateReader, StateWriter};

/// Tag byte for `Option<DmaKind>`: 0 none, 1 load, 2 reload, 3 abort.
fn write_dma_kind(w: &mut StateWriter, v: Option<DmaKind>) {
    w.u8(match v {
        None => 0,
        Some(DmaKind::Load) => 1,
        Some(DmaKind::Reload) => 2,
        Some(DmaKind::Abort) => 3,
    });
}

fn read_dma_kind(r: &mut StateReader) -> Result<Option<DmaKind>, StateError> {
    Ok(match r.u8()? {
        0 => None,
        1 => Some(DmaKind::Load),
        2 => Some(DmaKind::Reload),
        3 => Some(DmaKind::Abort),
        _ => return Err(StateError::BadValue),
    })
}

impl Envelope {
    /// start, looping, constant, volume, divider, decay.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.start);
        w.bool(self.looping);
        w.bool(self.constant);
        w.u8(self.volume);
        w.u8(self.divider);
        w.u8(self.decay);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.start = r.bool()?;
        self.looping = r.bool()?;
        self.constant = r.bool()?;
        self.volume = r.u8()?;
        self.divider = r.u8()?;
        self.decay = r.u8()?;
        Ok(())
    }
}

impl LengthCounter {
    /// enabled, halt, value, snap_halt, a has-pending tag and, when set, the pending pair (new, prev).
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.bool(self.halt);
        w.u8(self.value);
        w.bool(self.snap_halt);
        w.bool(self.pending.is_some());
        if let Some((new, prev)) = self.pending {
            w.u8(new);
            w.u8(prev);
        }
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.halt = r.bool()?;
        self.value = r.u8()?;
        self.snap_halt = r.bool()?;
        self.pending = if r.bool()? {
            Some((r.u8()?, r.u8()?))
        } else {
            None
        };
        Ok(())
    }
}

impl Pulse {
    /// ones_complement, duty, duty_pos, timer_period, timer, envelope, length, sweep_enabled,
    /// sweep_period, sweep_negate, sweep_shift, sweep_divider, sweep_reload.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ones_complement);
        w.u8(self.duty);
        w.u8(self.duty_pos);
        w.u16(self.timer_period);
        w.u16(self.timer);
        self.envelope.save_state(w);
        self.length.save_state(w);
        w.bool(self.sweep_enabled);
        w.u8(self.sweep_period);
        w.bool(self.sweep_negate);
        w.u8(self.sweep_shift);
        w.u8(self.sweep_divider);
        w.bool(self.sweep_reload);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.ones_complement = r.bool()?;
        self.duty = r.u8()?;
        self.duty_pos = r.u8()?;
        // both index `DUTY`, so an out-of-range file must fail here rather than panic there.
        if self.duty > 3 || self.duty_pos > 7 {
            return Err(StateError::BadValue);
        }
        self.timer_period = r.u16()?;
        self.timer = r.u16()?;
        self.envelope.load_state(r)?;
        self.length.load_state(r)?;
        self.sweep_enabled = r.bool()?;
        self.sweep_period = r.u8()?;
        self.sweep_negate = r.bool()?;
        self.sweep_shift = r.u8()?;
        self.sweep_divider = r.u8()?;
        self.sweep_reload = r.bool()?;
        Ok(())
    }
}

impl Triangle {
    /// timer_period, timer, seq_pos, length, control, linear_reload_value, linear_counter, linear_reload.
    ///
    /// `held` (the ultrasonic hold) is not stored: it is the level the analog output is
    /// sitting at, which the option recomputes from the sequencer on the next audible period.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.timer_period);
        w.u16(self.timer);
        w.u8(self.seq_pos);
        self.length.save_state(w);
        w.bool(self.control);
        w.u8(self.linear_reload_value);
        w.u8(self.linear_counter);
        w.bool(self.linear_reload);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.timer_period = r.u16()?;
        self.timer = r.u16()?;
        self.seq_pos = r.u8()?;
        // `seq_pos` indexes `SEQUENCE`.
        if self.seq_pos > 31 {
            return Err(StateError::BadValue);
        }
        self.length.load_state(r)?;
        self.control = r.bool()?;
        self.linear_reload_value = r.u8()?;
        self.linear_counter = r.u8()?;
        self.linear_reload = r.bool()?;
        Ok(())
    }
}

impl Noise {
    /// envelope, length, mode, timer_period, timer, shift.
    pub fn save_state(&self, w: &mut StateWriter) {
        self.envelope.save_state(w);
        self.length.save_state(w);
        w.bool(self.mode);
        w.u16(self.timer_period);
        w.u16(self.timer);
        w.u16(self.shift);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.envelope.load_state(r)?;
        self.length.load_state(r)?;
        self.mode = r.bool()?;
        self.timer_period = r.u16()?;
        // `clock_timer` reloads the timer as `timer_period - 1`, so zero can only come from
        // a damaged file and would underflow.
        if self.timer_period == 0 {
            return Err(StateError::BadValue);
        }
        self.timer = r.u16()?;
        self.shift = r.u16()?;
        Ok(())
    }
}

impl Dmc {
    /// irq_enabled, looping, timer_period, timer, output, sample_addr, sample_len, current_addr,
    /// bytes_remaining, buffer, shift, bits_remaining, silence, irq, load_delay, just_reloaded,
    /// dma_req, abort_countdown.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.irq_enabled);
        w.bool(self.looping);
        w.u16(self.timer_period);
        w.u16(self.timer);
        w.u8(self.output);
        w.u16(self.sample_addr);
        w.u16(self.sample_len);
        w.u16(self.current_addr);
        w.u16(self.bytes_remaining);
        w.opt_u8(self.buffer);
        w.u8(self.shift);
        w.u8(self.bits_remaining);
        w.bool(self.silence);
        w.bool(self.irq);
        w.u8(self.load_delay);
        w.bool(self.just_reloaded);
        write_dma_kind(w, self.dma_req);
        w.opt_u8(self.abort_countdown);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.irq_enabled = r.bool()?;
        self.looping = r.bool()?;
        self.timer_period = r.u16()?;
        // `clock_timer` reloads the timer as `timer_period - 1`.
        if self.timer_period == 0 {
            return Err(StateError::BadValue);
        }
        self.timer = r.u16()?;
        self.output = r.u8()?;
        self.sample_addr = r.u16()?;
        self.sample_len = r.u16()?;
        self.current_addr = r.u16()?;
        self.bytes_remaining = r.u16()?;
        self.buffer = r.opt_u8()?;
        self.shift = r.u8()?;
        self.bits_remaining = r.u8()?;
        // the output unit starts at 8 and reloads with the literal 8 when it reaches zero, so
        // a stored zero can only come from a damaged file — and `bits_remaining - 1` would underflow.
        if self.bits_remaining == 0 {
            return Err(StateError::BadValue);
        }
        self.silence = r.bool()?;
        self.irq = r.bool()?;
        self.load_delay = r.u8()?;
        self.just_reloaded = r.bool()?;
        self.dma_req = read_dma_kind(r)?;
        self.abort_countdown = r.opt_u8()?;
        Ok(())
    }
}

impl Apu {
    /// pulse1, pulse2, triangle, noise, dmc, five_step, irq_inhibit, frame_irq,
    /// frame_irq_clear_pending, frame_cycle, reset_delay, cycle, sample_acc, the analog filter
    /// chain's six tap words (hp1 in/out, hp2 in/out, lp in/out), then the band-limited
    /// resynthesizer's running level, ring position, oversampled phase and pending corrections.
    /// `sample_rate` (a frontend setting, which the filter coefficients follow), `gain` (the
    /// frontend's per-channel mix), `pan` and the right-hand mixer/filter/resynthesizer it
    /// drives, `options` (the host-side audio options), and the `samples` /
    /// `samples_right` drain buffers are not stored.
    pub fn save_state(&self, w: &mut StateWriter) {
        self.pulse1.save_state(w);
        self.pulse2.save_state(w);
        self.triangle.save_state(w);
        self.noise.save_state(w);
        self.dmc.save_state(w);
        w.bool(self.five_step);
        w.bool(self.irq_inhibit);
        w.bool(self.frame_irq);
        w.bool(self.frame_irq_clear_pending);
        w.u32(self.frame_cycle);
        w.u8(self.reset_delay);
        w.u64(self.cycle);
        w.f64(self.sample_acc);
        let (hp1_in, hp1_out) = self.filter.hp1.taps();
        let (hp2_in, hp2_out) = self.filter.hp2.taps();
        let (lp_in, lp_out) = self.filter.lp.taps();
        w.f32(hp1_in);
        w.f32(hp1_out);
        w.f32(hp2_in);
        w.f32(hp2_out);
        w.f32(lp_in);
        w.f32(lp_out);
        self.synth.save_state(w);
    }

    /// Mirror of `save_state`; clears the pending output samples.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.pulse1.load_state(r)?;
        self.pulse2.load_state(r)?;
        self.triangle.load_state(r)?;
        self.noise.load_state(r)?;
        self.dmc.load_state(r)?;
        self.five_step = r.bool()?;
        self.irq_inhibit = r.bool()?;
        self.frame_irq = r.bool()?;
        self.frame_irq_clear_pending = r.bool()?;
        self.frame_cycle = r.u32()?;
        self.reset_delay = r.u8()?;
        self.cycle = r.u64()?;
        self.sample_acc = r.f64()?;
        self.filter.hp1.set_taps(r.f32()?, r.f32()?);
        self.filter.hp2.set_taps(r.f32()?, r.f32()?);
        self.filter.lp.set_taps(r.f32()?, r.f32()?);
        self.synth.load_state(r)?;
        // The coefficients are derived from the frontend's rate, not stored: re-derive them so the
        // loaded taps are interpreted at the rate this machine is actually running.
        self.filter.set_rate(self.sample_rate);
        self.samples.clear();
        // #76b: nothing of the right-hand side is in the state — the pan is the frontend's setting —
        // so its buffers go and its chain restarts from silence instead of replaying the pre-load
        // timeline into the right output.
        self.samples_right.clear();
        self.filter_right.reset();
        self.synth_right.reset();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dma_kind_tag_round_trips() {
        for kind in [
            None,
            Some(DmaKind::Load),
            Some(DmaKind::Reload),
            Some(DmaKind::Abort),
        ] {
            let mut w = StateWriter::new();
            write_dma_kind(&mut w, kind);
            let data = w.finish();
            assert_eq!(read_dma_kind(&mut StateReader::new(&data)), Ok(kind));
        }
    }

    #[test]
    fn unknown_dma_kind_tag_is_a_bad_value() {
        assert_eq!(
            read_dma_kind(&mut StateReader::new(&[4])),
            Err(StateError::BadValue)
        );
    }

    /// The fields the read guard rejects. Every one of these is either a table index or a
    /// counter that the channel decrements, so before the guard a state carrying it loaded cleanly
    /// and then panicked on the next cycle (`DUTY[4][..]`, `SEQUENCE[32]`, `timer_period - 1`, …).
    /// Each is set to the smallest value the emulator itself can never store.
    #[test]
    fn out_of_range_channel_fields_are_a_bad_value() {
        fn rejects(mutate: fn(&mut Apu), name: &str) {
            let mut apu = Apu::new();
            mutate(&mut apu);
            let mut w = StateWriter::new();
            apu.save_state(&mut w);
            let data = w.finish();
            assert_eq!(
                Apu::new().load_state(&mut StateReader::new(&data)),
                Err(StateError::BadValue),
                "{name} must be rejected on load"
            );
        }
        rejects(|a| a.pulse1.duty = 4, "pulse duty 4");
        rejects(|a| a.pulse1.duty_pos = 8, "pulse duty_pos 8");
        rejects(|a| a.triangle.seq_pos = 32, "triangle seq_pos 32");
        rejects(|a| a.noise.timer_period = 0, "noise timer period 0");
        rejects(|a| a.dmc.timer_period = 0, "DMC timer period 0");
        rejects(|a| a.dmc.bits_remaining = 0, "DMC bits remaining 0");
    }

    /// The same guard must not touch a state every channel wrote itself: the boundary values are
    /// still good, and they survive a round trip.
    #[test]
    fn the_top_of_every_range_still_loads() {
        let mut apu = Apu::new();
        apu.pulse1.duty = 3;
        apu.pulse1.duty_pos = 7;
        apu.pulse2.duty = 3;
        apu.pulse2.duty_pos = 7;
        apu.triangle.seq_pos = 31;
        let mut w = StateWriter::new();
        apu.save_state(&mut w);
        let data = w.finish();
        assert!(Apu::new().load_state(&mut StateReader::new(&data)).is_ok());
    }

    #[test]
    fn truncated_state_is_an_error() {
        let mut apu = Apu::new();
        assert_eq!(
            apu.load_state(&mut StateReader::new(&[])),
            Err(StateError::Truncated)
        );
    }

    #[test]
    fn samples_are_cleared_on_load() {
        let mut apu = Apu::new();
        apu.samples.push(0.5);
        let mut w = StateWriter::new();
        apu.save_state(&mut w);
        let data = w.finish();

        let mut out = Apu::new();
        out.samples.push(-0.5);
        out.load_state(&mut StateReader::new(&data)).unwrap();
        assert!(out.samples.is_empty());
    }

    /// Every field, including the ones a frame boundary leaves at their default (`dma_req`,
    /// `abort_countdown`, `buffer`, `just_reloaded`, the frame-IRQ latches).
    #[test]
    fn all_fields_round_trip_from_a_hand_built_apu() {
        let mut apu = Apu::new();
        apu.pulse1.duty = 2;
        apu.pulse1.duty_pos = 6;
        apu.pulse1.timer_period = 0x1AB;
        apu.pulse1.timer = 0x0CD;
        apu.pulse1.envelope.start = true;
        apu.pulse1.envelope.divider = 9;
        apu.pulse1.envelope.decay = 3;
        apu.pulse1.length.value = 42;
        apu.pulse1.sweep_enabled = true;
        apu.pulse1.sweep_period = 5;
        apu.pulse1.sweep_negate = true;
        apu.pulse1.sweep_shift = 3;
        apu.pulse1.sweep_divider = 1;
        apu.pulse1.sweep_reload = true;
        apu.pulse2.sweep_reload = true;
        apu.triangle.timer_period = 0x2BC;
        apu.triangle.timer = 0x0DE;
        apu.triangle.seq_pos = 17;
        apu.triangle.length.value = 13;
        apu.triangle.control = true;
        apu.triangle.linear_reload_value = 0x40;
        apu.triangle.linear_counter = 0x20;
        apu.triangle.linear_reload = true;
        apu.noise.mode = true;
        apu.noise.timer_period = 202;
        apu.noise.timer = 7;
        apu.noise.shift = 0x4321;
        apu.dmc.irq_enabled = true;
        apu.dmc.looping = true;
        apu.dmc.timer_period = 428;
        apu.dmc.timer = 6;
        apu.dmc.output = 77;
        apu.dmc.sample_addr = 0xC040;
        apu.dmc.sample_len = 33;
        apu.dmc.current_addr = 0xC051;
        apu.dmc.bytes_remaining = 2;
        apu.dmc.buffer = Some(0x5A);
        apu.dmc.shift = 0b1010_0101;
        apu.dmc.bits_remaining = 3;
        apu.dmc.silence = false;
        apu.dmc.irq = true;
        apu.dmc.load_delay = 3;
        apu.dmc.just_reloaded = true;
        apu.dmc.dma_req = Some(DmaKind::Abort);
        apu.dmc.abort_countdown = Some(2);
        apu.five_step = true;
        apu.irq_inhibit = true;
        apu.frame_irq = true;
        apu.frame_irq_clear_pending = true;
        apu.frame_cycle = 29_830;
        apu.reset_delay = 4;
        apu.cycle = 0x0123_4567_89AB;
        apu.sample_acc = 0.25;
        apu.filter.hp1.set_taps(-0.5, 0.125);
        apu.filter.hp2.set_taps(0.25, -0.75);
        apu.filter.lp.set_taps(0.5, 0.0625);
        // Put the resynthesizer mid-window: a level change waiting on some of its ring slots.
        apu.synth.feed(0.5, 0);
        assert_eq!(apu.synth.take_sub(), 0);
        apu.synth.next_sample();

        let mut w = StateWriter::new();
        apu.save_state(&mut w);
        let data = w.finish();

        let mut out = Apu::new();
        out.sample_rate = apu.sample_rate;
        out.samples.push(-0.25);
        out.load_state(&mut StateReader::new(&data)).unwrap();

        assert_eq!(StateReader::new(&data).remaining(), data.len());
        assert_eq!(out.dmc.dma_req, Some(DmaKind::Abort));
        assert_eq!(out.dmc.abort_countdown, Some(2));
        assert_eq!(out.dmc.buffer, Some(0x5A));
        assert!(out.samples.is_empty());
        assert_eq!(format!("{out:?}"), format!("{apu:?}"));
    }
}
