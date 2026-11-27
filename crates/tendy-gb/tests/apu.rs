//! The APU's named tests. Every one drives `Apu` the way a game does — register
//! writes, then `tick`d dots and `div_apu_event`s — because the CPU and timer are stubs here, not the
//! live ones a game would run against.

mod common;

mod apu {
    use tendy_gb::apu::Apu;

    /// An APU with `$FF26` cleared and the power bit set again: every register clear, no channel
    /// running (`Audio_Registers`, `NR52`: power-off clears `$FF10`-`$FF25`).
    fn fresh() -> Apu {
        let mut apu = Apu::new();
        apu.write(0xFF26, 0x00);
        apu.write(0xFF26, 0x80);
        apu
    }

    /// `NR50` both sides at 7, `NR51` all four channels into both outputs.
    fn full_volume(apu: &mut Apu) {
        apu.write(0xFF24, 0x77);
        apu.write(0xFF25, 0xFF);
    }

    /// CH1 sounding: duty 50%, volume 15, no envelope, no length, at `period`.
    fn ch1_sounding(apu: &mut Apu, period: u16) {
        apu.write(0xFF11, 0x80);
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF13, (period & 0xFF) as u8);
        apu.write(0xFF14, 0x80 | ((period >> 8) as u8 & 0x07));
    }

    /// Everything the staging buffer has produced so far.
    fn drain(apu: &mut Apu) -> Vec<f32> {
        let mut buf = vec![0.0f32; 4096];
        let frames = apu.read_samples(&mut buf);
        buf.truncate(frames * 2);
        buf
    }

    /// Run `dots` in M-cycle chunks (as `Bus` does) and collect the stereo samples.
    fn render(apu: &mut Apu, dots: u32) -> Vec<f32> {
        let mut all = Vec::new();
        let mut left = dots;
        while left > 0 {
            let step = left.min(4);
            apu.tick(step);
            left -= step;
            all.extend(drain(apu));
        }
        all
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Peak level of one side of the interleaved buffer: `skip` 0 is left, 1 is right.
    fn side_peak(samples: &[f32], skip: usize) -> f32 {
        samples
            .iter()
            .skip(skip)
            .step_by(2)
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn length_timer_ticks_at_256_hz() {
        let mut apu = fresh();
        // UNVERIFIED: the length counter is clocked every second DIV-APU step (256 Hz of the
        // 512 Hz event), which is what makes 8 events here equal one 15.6 ms "length unit".
        apu.write(0xFF11, 60); // length bits 60 -> 64 - 60 = 4 ticks
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF14, 0x80 | 0x40); // trigger, length enable
        assert!(apu.ch1.enabled);
        assert_eq!(apu.ch1.length, 4);

        apu.div_apu = 0; // step boundary, so the walk below is deterministic
        for _ in 0..7 {
            apu.div_apu_event(); // steps 1..7: even steps 2, 4, 6 tick the length
        }
        assert_eq!(apu.ch1.length, 1);
        assert!(apu.ch1.enabled);

        apu.div_apu_event(); // step 0
        assert_eq!(apu.ch1.length, 0);
        assert!(!apu.ch1.enabled, "length expiry turns the channel off");
        assert_eq!(apu.read(0xFF26) & 0x01, 0, "NR52 bit 0 follows");
    }

    #[test]
    fn length_timer_loads_from_reload_value() {
        let mut apu = fresh();
        apu.write(0xFF11, 0x00); // length bits 0 -> 64
        assert_eq!(apu.ch1.length, 64);
        apu.write(0xFF11, 0x3F); // length bits 63 -> 1
        assert_eq!(apu.ch1.length, 1);

        // A trigger with an expired length reloads the maximum (`Audio_Registers`, `NRx4`).
        apu.ch1.length = 0;
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF14, 0x80);
        assert_eq!(apu.ch1.length, 64);

        // CH3's length is the full 8-bit range, not 6 bits (`Audio_Registers`, `NR31`).
        apu.write(0xFF1B, 0x00);
        assert_eq!(apu.ch3.length, 256);
        apu.write(0xFF1B, 0xFF);
        assert_eq!(apu.ch3.length, 1);
    }

    #[test]
    fn envelope_steps_on_the_64_hz_clock() {
        let mut apu = fresh();
        apu.write(0xFF12, 0x5A); // volume 5, up, pace 2
        apu.write(0xFF14, 0x80);
        assert_eq!(apu.ch1.vol, 5);

        apu.div_apu = 0;
        for _ in 0..8 {
            apu.div_apu_event(); // 8 events = one 64 Hz envelope tick
        }
        assert_eq!(apu.ch1.vol, 5, "pace 2 needs two envelope ticks");
        for _ in 0..8 {
            apu.div_apu_event();
        }
        assert_eq!(apu.ch1.vol, 6);

        // Downwards, pace 1: one envelope tick per 64 Hz.
        let mut apu = fresh();
        apu.write(0xFF12, 0x21); // volume 2, down, pace 1
        apu.write(0xFF14, 0x80);
        apu.div_apu = 0;
        for _ in 0..8 {
            apu.div_apu_event();
        }
        assert_eq!(apu.ch1.vol, 1);
    }

    #[test]
    fn sweep_overflow_disables_the_channel() {
        // 0x700 with shift 1 gives 0xA80 on the trigger check: past 0x7FF, so CH1 dies at once
        // (`Audio_details`, "Pulse channel with sweep").
        let mut apu = fresh();
        apu.write(0xFF10, 0x11); // pace 1, addition, shift 1
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF13, 0x00);
        apu.write(0xFF14, 0x07); // period 0x700, no trigger
        apu.write(0xFF14, 0x87); // trigger
        assert!(!apu.ch1.enabled);
        assert_eq!(apu.read(0xFF26) & 0x01, 0);

        // 0x400 gives 0x600 first: inside range, so the channel survives the trigger.
        let mut apu = fresh();
        apu.write(0xFF10, 0x11);
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF13, 0x00);
        apu.write(0xFF14, 0x84);
        assert!(apu.ch1.enabled);
        assert_eq!(apu.ch1.period, 0x400);
    }

    #[test]
    fn sweep_shift_and_direction() {
        // Shift 2, pace 1, addition: 0x100 + 0x40 = 0x140.
        let mut apu = fresh();
        apu.write(0xFF10, 0x12); // pace 1, addition, shift 2
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF13, 0x00);
        apu.write(0xFF14, 0x81); // period 0x100, trigger
        assert_eq!(apu.ch1.period, 0x100);
        apu.div_apu = 1;
        apu.div_apu_event(); // step 2 clocks the sweep
        assert_eq!(apu.ch1.period, 0x140);
        assert!(apu.ch1.enabled);

        // Same shift with subtraction: 0x140 - 0x50 = 0xF0.
        let mut apu = fresh();
        apu.write(0xFF10, 0x1A); // pace 1, subtraction, shift 2
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF13, 0x40);
        apu.write(0xFF14, 0x81); // period 0x140, trigger
        apu.div_apu = 1;
        apu.div_apu_event();
        assert_eq!(apu.ch1.period, 0xF0);
    }

    #[test]
    fn wave_output_levels() {
        let mut apu = fresh();
        apu.wave[0] = 0xA5; // first sample = upper nibble = $A
        apu.write(0xFF1A, 0x80); // DAC on
        apu.write(0xFF1C, 0x20); // 100%
        apu.write(0xFF1D, 0x00);
        apu.write(0xFF1E, 0x87); // period 0x700, trigger
        assert_eq!(apu.channel_levels()[2], 0x0A);

        apu.write(0xFF1C, 0x40); // 50%
        assert_eq!(apu.channel_levels()[2], 0x05);
        apu.write(0xFF1C, 0x60); // 25%
        assert_eq!(apu.channel_levels()[2], 0x02);
        apu.write(0xFF1C, 0x00); // mute
        assert_eq!(apu.channel_levels()[2], 0x00);
    }

    #[test]
    fn wave_period_divider() {
        let mut apu = fresh();
        apu.write(0xFF1A, 0x80);
        apu.write(0xFF1C, 0x20);
        apu.write(0xFF1D, 0x00);
        apu.write(0xFF1E, 0x07); // period 0x700
        apu.write(0xFF1E, 0x87); // trigger
        assert_eq!(apu.ch3.pos, 0);

        // One sample is 2 * (2048 - 0x700) = 512 dots: the divider is clocked once per two dots.
        apu.tick(511);
        assert_eq!(apu.ch3.pos, 0);
        apu.tick(1);
        assert_eq!(apu.ch3.pos, 1);
        apu.tick(512 * 31);
        assert_eq!(apu.ch3.pos, 0, "32 samples per pass over wave RAM");
    }

    #[test]
    fn noise_lfsr_15_bit() {
        let mut apu = fresh();
        apu.write(0xFF21, 0xF0); // volume 15, DAC on
        apu.write(0xFF22, 0x00); // shift 0, 15-bit, divisor 0 -> 8 dots per clock
        apu.write(0xFF23, 0x80);
        // UNVERIFIED: $7FFF is the DMG's LFSR reset value; Pan Docs only say the LFSR is "reset".
        assert_eq!(apu.ch4.lfsr, 0x7FFF);

        apu.tick(8);
        assert_eq!(apu.ch4.lfsr, 0x3FFF);
        apu.tick(8);
        assert_eq!(apu.ch4.lfsr, 0x1FFF);

        let mut clocks = 2;
        loop {
            apu.tick(8);
            clocks += 1;
            assert_eq!(apu.ch4.lfsr & 0x8000, 0, "15-bit mode never sets bit 15");
            if apu.ch4.lfsr == 0x7FFF {
                break;
            }
            assert!(clocks < 32_768, "15-bit LFSR did not repeat");
        }
        assert_eq!(clocks, 32_767, "the 15-bit LFSR's period is 32 767");
    }

    #[test]
    fn noise_lfsr_7_bit() {
        let mut apu = fresh();
        apu.write(0xFF21, 0xF0);
        apu.write(0xFF22, 0x08); // shift 0, 7-bit, divisor 0
        apu.write(0xFF23, 0x80);
        assert_eq!(apu.ch4.lfsr, 0x7FFF);

        // Bits 0-6 are the shifted 7-bit register; bit 7 is the feedback bit, which the shift
        // moves into bit 6, so the register to compare is `lfsr & 0x7F`.
        let start = apu.ch4.lfsr & 0x7F;
        let mut clocks = 0;
        loop {
            apu.tick(8);
            clocks += 1;
            assert_eq!(apu.ch4.lfsr & 0x8000, 0, "7-bit mode never sets bit 15");
            if apu.ch4.lfsr & 0x7F == start {
                break;
            }
            assert!(clocks < 128, "7-bit LFSR did not repeat");
        }
        assert_eq!(clocks, 127, "the 7-bit LFSR's period is 127");
    }

    #[test]
    fn noise_divisor_zero_is_half() {
        let mut apu = fresh();
        apu.write(0xFF21, 0xF0);
        apu.write(0xFF22, 0x00); // divisor 0 is 0.5 -> 8 dots per clock
        assert_eq!(apu.ch4.period, 8);
        apu.write(0xFF23, 0x80);
        apu.tick(8);
        assert_eq!(apu.ch4.lfsr, 0x3FFF, "one clock in 8 dots");

        apu.write(0xFF22, 0x01); // divisor 1 -> 16 dots per clock
        assert_eq!(apu.ch4.period, 16);
        apu.write(0xFF23, 0x80); // trigger, so the divider starts on the new period
        let start = apu.ch4.lfsr;
        apu.tick(8);
        assert_eq!(apu.ch4.lfsr, start, "16 dots per clock");
        apu.tick(8);
        assert_ne!(apu.ch4.lfsr, start);
    }

    #[test]
    fn nr52_off_clears_registers() {
        let mut apu = fresh();
        apu.write(0xFF12, 0xF3);
        apu.write(0xFF24, 0x77);
        apu.write(0xFF25, 0xF3);
        assert_eq!(apu.read(0xFF12), 0xF3);

        apu.write(0xFF26, 0x00); // power off
        assert_eq!(apu.read(0xFF12), 0x00);
        assert_eq!(apu.read(0xFF24), 0x00);
        assert_eq!(apu.read(0xFF25), 0x00);
        assert_eq!(apu.read(0xFF10), 0x80, "unused bits still read 1");
        assert_eq!(apu.read(0xFF11), 0x3F);
        assert_eq!(apu.read(0xFF14), 0xBF);
        assert_eq!(apu.read(0xFF26), 0x70, "power bit and channels clear");

        apu.write(0xFF12, 0xFF); // dropped while off
        assert_eq!(apu.read(0xFF12), 0x00);
        apu.write(0xFF11, 63); // but the length timers stay writable on monochrome models
        assert_eq!(apu.ch1.length, 1);

        apu.write(0xFF26, 0x80); // power on: only bit 7 is stored in NR52
        assert_eq!(apu.read(0xFF26), 0xF0);
    }

    #[test]
    fn nr52_off_keeps_wave_ram() {
        let mut apu = fresh();
        apu.write(0xFF31, 0x5A);
        apu.write(0xFF26, 0x00);
        assert_eq!(apu.read(0xFF31), 0x5A, "power-off keeps wave RAM");

        apu.write(0xFF31, 0xA5); // and it stays writable while the unit is off
        assert_eq!(apu.read(0xFF31), 0xA5);
    }

    #[test]
    fn nr50_master_volume() {
        let mut apu = fresh();
        full_volume(&mut apu);
        ch1_sounding(&mut apu, 0x400);

        // The first duty step of a 50% pulse is high for 4096 dots, so both windows stay high.
        let loud = peak(&render(&mut apu, 2000));
        apu.write(0xFF24, 0x11); // both sides 1
        let quiet = peak(&render(&mut apu, 2000));
        assert!(loud > 0.2, "full volume reached {loud}");
        assert!((loud / quiet - 4.0).abs() < 0.01, "{loud} vs {quiet}");
    }

    #[test]
    fn nr51_panning() {
        let mut apu = fresh();
        full_volume(&mut apu);
        apu.write(0xFF25, 0x01); // CH1 to the right output only
        ch1_sounding(&mut apu, 0x400);
        let samples = render(&mut apu, 2000);
        assert_eq!(side_peak(&samples, 0), 0.0);
        assert!(side_peak(&samples, 1) > 0.2);

        apu.write(0xFF25, 0x10); // CH1 to the left output only
        let samples = render(&mut apu, 2000);
        assert_eq!(side_peak(&samples, 1), 0.0);
        assert!(side_peak(&samples, 0) > 0.2);
    }

    #[test]
    fn dac_off_silences_channel() {
        let mut apu = fresh();
        full_volume(&mut apu);
        ch1_sounding(&mut apu, 0x400);
        assert!(peak(&render(&mut apu, 1000)) > 0.2);

        apu.write(0xFF12, 0x00); // DAC off: the channel drops out at once
        assert_eq!(apu.read(0xFF26) & 0x01, 0);
        assert_eq!(apu.channel_levels()[0], 0);
        assert_eq!(peak(&render(&mut apu, 1000)), 0.0);
    }

    #[test]
    fn wave_ram_is_locked_while_ch3_plays() {
        let mut apu = fresh();
        apu.write(0xFF30, 0x12);
        assert_eq!(apu.read(0xFF30), 0x12);

        apu.write(0xFF1A, 0x80);
        apu.write(0xFF1C, 0x20);
        apu.write(0xFF1E, 0x87); // trigger: CH3 active
        apu.write(0xFF30, 0xAB); // ignored
        assert_eq!(apu.read(0xFF30), 0xFF);

        apu.write(0xFF1A, 0x00); // DAC off: CH3 inactive again
        assert_eq!(apu.read(0xFF30), 0x12);
        apu.write(0xFF30, 0xAB);
        assert_eq!(apu.read(0xFF30), 0xAB);
    }

    #[test]
    fn frame_sequencer_steps_on_div_apu_events() {
        let mut apu = fresh();
        // UNVERIFIED: step numbering — length on 0/2/4/6, sweep on 2/6, envelope on 7.
        apu.write(0xFF10, 0x11); // sweep pace 1, addition, shift 1
        apu.write(0xFF11, 0xB8); // duty 50%, length 8
        apu.write(0xFF12, 0x51); // volume 5, down, pace 1
        apu.write(0xFF13, 0x00);
        apu.write(0xFF14, 0xC1); // length enable, period 0x100, trigger
        assert_eq!(apu.ch1.length, 8);

        let period0 = apu.ch1.period;
        let vol0 = apu.ch1.vol;

        apu.div_apu = 7;
        apu.div_apu_event(); // step 0: length only
        assert_eq!(apu.div_apu, 0);
        assert_eq!(apu.ch1.length, 7);
        assert_eq!(apu.ch1.period, period0);
        assert_eq!(apu.ch1.vol, vol0);

        apu.div_apu = 1;
        apu.div_apu_event(); // step 2: length and sweep
        assert_eq!(apu.ch1.length, 6);
        assert_eq!(apu.ch1.period, 0x180);
        assert_eq!(apu.ch1.vol, vol0);

        apu.div_apu = 5;
        apu.div_apu_event(); // step 6: length and sweep
        assert_eq!(apu.ch1.length, 5);
        assert_eq!(apu.ch1.period, 0x240);
        assert_eq!(apu.ch1.vol, vol0);

        apu.div_apu = 6;
        apu.div_apu_event(); // step 7: envelope only
        assert_eq!(apu.ch1.length, 5);
        assert_eq!(apu.ch1.period, 0x240);
        assert_eq!(apu.ch1.vol, vol0 - 1);
    }

    #[test]
    fn channel_mask_silences_one_channel() {
        let mut apu = fresh();
        full_volume(&mut apu);
        ch1_sounding(&mut apu, 0x400);
        assert!(peak(&render(&mut apu, 1000)) > 0.2);

        apu.set_channel_mask(0x0E); // CH1 muted
        assert_eq!(peak(&render(&mut apu, 1000)), 0.0);
        assert_eq!(apu.read(0xFF26) & 0x01, 1, "muting is not a channel-off");

        apu.set_channel_mask(0x0F);
        assert!(peak(&render(&mut apu, 1000)) > 0.2);
    }

    #[test]
    fn resampler_produces_the_expected_sample_count() {
        let mut apu = fresh();
        apu.set_sample_rate(48_000);
        let mut buf = vec![0.0f32; 4096];
        let mut frames = 0usize;
        for _ in 0..17_556 {
            apu.tick(4); // one frame of M-cycles
            frames += apu.read_samples(&mut buf);
        }
        // 70 224 dots per frame at 4 194 304 Hz, resampled to 48 kHz.
        let expected = 70_224.0 * 48_000.0 / 4_194_304.0;
        assert!(
            (frames as f64 - expected).abs() <= 1.0,
            "{frames} frames, expected {expected}"
        );
    }

    #[test]
    fn silence_is_zero_when_all_channels_off() {
        let mut apu = fresh();
        full_volume(&mut apu);
        assert_eq!(
            apu.read(0xFF26) & 0x0F,
            0,
            "no channel is on after a power cycle"
        );
        let samples = render(&mut apu, 2000);
        assert!(!samples.is_empty());
        assert!(
            samples.iter().all(|s| *s == 0.0),
            "silence must be exact zero"
        );
    }

    #[test]
    fn machine_drains_audio_after_a_frame() {
        // The wiring check the front end depends on: `Bus::tick` feeds the APU, and one video
        // frame yields one frame's worth of stereo samples at 44.1 kHz.
        let mut machine =
            crate::common::machine(crate::common::synth_rom(&crate::common::entry(&[0x00])));
        machine.set_sample_rate(44_100);
        machine.run_frame();
        let mut buf = [0.0f32; 4096];
        let frames = machine.drain_audio(&mut buf);
        assert!(frames > 700 && frames < 760, "{frames} stereo frames");
    }

    #[test]
    fn machine_drains_audio_at_the_device_rate() {
        // The count must follow `set_sample_rate`, because the front end now passes the
        // machine's own rate (48 000 here) instead of assuming 44 100 — the cause of the muddled
        // sound and the every-callback underrun.
        let frame = crate::common::synth_rom(&crate::common::entry(&[0x00]));
        let at = |rate: u32| {
            let mut machine = crate::common::machine(frame.clone());
            machine.set_sample_rate(rate);
            machine.run_frame();
            let mut buf = [0.0f32; 4096];
            machine.drain_audio(&mut buf)
        };
        let at_44 = at(44_100);
        let at_48 = at(48_000);
        assert!((735..=741).contains(&at_44), "{at_44} frames at 44.1 kHz");
        assert!((800..=806).contains(&at_48), "{at_48} frames at 48 kHz");
    }
}
