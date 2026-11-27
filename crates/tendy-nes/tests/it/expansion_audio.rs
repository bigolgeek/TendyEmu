//! Expansion audio and the new boards, exercised through the real bus rather than the unit-test
//! seams: `Mapper::expansion_audio` reaching `Apu` before the output filter, the `$4020-$5FFF`
//! register window, nametable banking through the PPU, and `mapper::load` for every new mapper.

use tendy_nes::apu::Apu;
use tendy_nes::apu::gain::Channel;
use tendy_nes::bus::SysBus;
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::load;

/// An iNES image whose PRG holds `0xEA` everywhere and whose CHR holds its 1 KB bank number in
/// every byte of that bank.
fn ines(mapper: u16, prg_banks: u8, chr_banks: u8) -> Vec<u8> {
    let flags6 = ((mapper & 0x0F) as u8) << 4;
    let flags7 = (mapper & 0xF0) as u8;
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, flags6, flags7, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + prg_banks as usize * 16384, 0xEA);
    let chr_start = v.len();
    v.resize(chr_start + chr_banks as usize * 8192, 0);
    for i in 0..chr_banks as usize * 8192 {
        v[chr_start + i] = (i / 1024) as u8;
    }
    v
}

fn bus_for(mapper: u16) -> SysBus {
    SysBus::new(load(&ines(mapper, 2, 2)).unwrap())
}

/// Run `n` CPU cycles by reading a RAM address (each read is one cycle).
fn run(bus: &mut SysBus, n: usize) {
    for _ in 0..n {
        bus.read(0x0000);
    }
}

#[test]
fn load_accepts_every_new_board() {
    for mapper in [19u16, 21, 22, 23, 24, 25, 26, 69, 85, 210] {
        assert!(
            load(&ines(mapper, 2, 2)).is_ok(),
            "mapper {mapper} should load"
        );
    }
}

#[test]
fn vrc6_audio_reaches_the_apu_sample_stream() {
    let silent = {
        let mut bus = bus_for(24);
        run(&mut bus, 20_000);
        bus.apu.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    };

    let mut bus = bus_for(24);
    // Pulse 1 at full volume, 50% duty, period 0: a square wave at the sample path's own rate.
    bus.write(0x9000, 0x7F);
    bus.write(0x9001, 0x00);
    bus.write(0x9002, 0x80); // enable, period high nibble 0
    run(&mut bus, 20_000);

    assert!(!bus.apu.samples.is_empty(), "samples were produced");
    let peak = bus.apu.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        peak > silent + 0.01,
        "the VRC6 pulse is audible: {peak} vs {silent}"
    );
    // The native channels are untouched: the extra level can only come from the cartridge.
    assert_eq!(bus.apu.pulse1.output(), 0);
    assert_eq!(bus.apu.pulse2.output(), 0);
}

#[test]
fn a_board_without_audio_has_no_expansion_level() {
    let mut bus = bus_for(24); // the board is capable, but its audio registers are never programmed
    for _ in 0..20_000 {
        bus.read(0x0000);
        assert_eq!(
            bus.apu.expansion_pulse, 0.0,
            "an unprogrammed chip contributes nothing"
        );
    }
}

#[test]
fn expansion_is_mixed_before_the_output_filter() {
    let mut plain = bus_for(24);
    let mut with_expansion = bus_for(24);
    // 0.25 is the target mix amplitude; the hook takes pulse units, so scale it the way the board
    // would (see `Apu::amplitude_to_pulse_units`).
    with_expansion.apu.expansion_pulse = tendy_nes::apu::Apu::amplitude_to_pulse_units(0.25);
    assert!(
        (with_expansion.apu.mix() - plain.apu.mix() - 0.25).abs() < 1e-6,
        "the mixer sums it"
    );

    // A constant level is DC, and the output filter chain is a DC blocker: both machines converge
    // on the same samples once it has settled, which is only true if the expansion term entered
    // before the filter.
    run(&mut plain, 60_000);
    run(&mut with_expansion, 60_000);
    let a = *plain.apu.samples.last().unwrap();
    let b = *with_expansion.apu.samples.last().unwrap();
    assert!(
        (a - b).abs() < 1e-3,
        "the filter removed the constant term: {a} vs {b}"
    );
}

/// The pulse transform as it was before the inverse-curve fix, spelled out here so the fixed
/// curve can be checked against it for positive levels without asking the mixer for the value it
/// is testing.
fn positive_pulse_curve(p: f32) -> f32 {
    if p == 0.0 {
        0.0
    } else {
        95.88 / (8128.0 / p + 100.0)
    }
}

/// `amplitude_to_pulse_units` is meant to be the exact inverse of the mixer curve, so a solo
/// expansion level must come out of `mix` at the amplitude the board asked for. The N163 (centred
/// samples), VRC7 (a sine) and MMC5 (`-(pulses + pcm)`) all ask for negative levels all the time;
/// the old mixer applied the curve to `p` itself and distorted every one of them.
#[test]
fn expansion_levels_round_trip_at_both_signs() {
    for &amplitude in &[
        -0.75f32, -0.4, -0.25, -0.2, -0.1, -0.05, 0.05, 0.1, 0.2, 0.25, 0.4, 0.75,
    ] {
        let mut apu = Apu::new();
        let silent = apu.mix();
        apu.expansion_pulse = Apu::amplitude_to_pulse_units(amplitude);
        let heard = apu.mix() - silent;
        assert!(
            (heard - amplitude).abs() <= 1e-6 * amplitude.abs().max(1.0),
            "{amplitude} came out of the mixer as {heard}"
        );
    }
}

/// The reachable range of `expansion_pulse`, in both units the boards use: mixer-scale boards
/// (MMC5) emit pulse units down to about -45, and `amplitude_to_pulse_units` maps -2.0..=2.0 onto
/// pulse units that saturate at the curve's ceiling. No point in that range may divide by zero,
/// return a non-finite value, or flip the sign of the level the chip asked for.
#[test]
fn no_reachable_expansion_level_is_non_finite_or_sign_flipped() {
    let mut apu = Apu::new();
    let silent = apu.mix();

    // The board-facing scale, -2.0..=2.0 in steps of 0.01.
    for i in -200..=200 {
        let amplitude = i as f32 / 100.0;
        apu.expansion_pulse = Apu::amplitude_to_pulse_units(amplitude);
        let heard = apu.mix() - silent;
        assert!(heard.is_finite(), "amplitude {amplitude} mixed to {heard}");
        assert!(
            heard == 0.0 || heard.signum() == amplitude.signum(),
            "amplitude {amplitude} came out as {heard}"
        );
    }

    // Raw pulse units, including the old curve's pole at -81.28.
    for level in [
        -120.0f32,
        -81.28,
        -81.0,
        -45.0,
        -1.0,
        0.0,
        1.0,
        45.0,
        81.0,
        120.0,
        f32::MIN,
        f32::MAX,
    ] {
        apu.expansion_pulse = level;
        let heard = apu.mix() - silent;
        assert!(heard.is_finite(), "pulse units {level} mixed to {heard}");
        assert!(
            heard == 0.0 || heard.signum() == level.signum(),
            "pulse units {level} came out as {heard}"
        );
    }
}

/// A positive level is decoded exactly as it always was: `abs` and `copysign` are no-ops on a
/// positive number, so with the native pulses silent the mix is bit-identical to the old
/// curve, and every game without expansion audio is untouched. (The level is now a term of its own,
/// so it no longer joins the native pulse sum -- see `a_cartridge_level_is_not_ducked_by_the_native_channels`.)
#[test]
fn positive_expansion_levels_are_bit_identical_to_the_old_curve() {
    for &p in &[
        0.0f32,
        1e-6,
        0.5,
        1.0,
        10.0,
        28.668_173,
        100.0,
        1000.0,
        8128.0,
        1e6,
        f32::MAX,
    ] {
        let mut apu = Apu::new();
        let silent = apu.mix();
        apu.expansion_pulse = p;
        assert_eq!(
            apu.mix().to_bits(),
            (silent + positive_pulse_curve(p)).to_bits(),
            "pulse units {p} moved"
        );
    }
}

/// The stereo path (`mix_side`) runs the same curve, so a panned negative expansion level
/// must not be distorted either.
#[test]
fn panned_negative_expansion_level_round_trips() {
    for &amplitude in &[-0.5f32, -0.25, 0.25, 0.5] {
        let mut apu = Apu::new();
        apu.pan.set(Channel::Expansion, -1.0); // hard left
        let (silent_left, silent_right) = apu.mix_stereo();
        apu.expansion_pulse = Apu::amplitude_to_pulse_units(amplitude);
        let (left, right) = apu.mix_stereo();
        assert!(
            (left - silent_left - amplitude).abs() <= 1e-6,
            "{amplitude} came out of the left mixer as {}",
            left - silent_left
        );
        assert_eq!(
            right.to_bits(),
            silent_right.to_bits(),
            "a hard-left pan must not leak right"
        );
    }
}

#[test]
fn fme7_audio_reaches_the_sample_stream() {
    let mut bus = bus_for(69);
    bus.write(0xC000, 0x07); // YM2149 register 7: tone/noise enables
    bus.write(0xE000, 0x3F); // all three channels hold a constant level
    bus.write(0xC000, 0x08);
    bus.write(0xE000, 0x0F); // channel A at full volume
    run(&mut bus, 20_000);
    let peak = bus.apu.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 1e-4, "the 5B tone is audible: {peak}");
}

#[test]
fn namco163_audio_and_data_port_work_through_the_bus() {
    let mut bus = bus_for(19);
    // $F800 latches the chip-RAM address at the start of the next cycle.
    bus.write(0xF800, 0x00);
    bus.write(0x4800, 0x0F); // wave sample 0 = $F
    bus.write(0xF800, 0x7E);
    bus.write(0x4800, 0x00); // wave address 0
    bus.write(0xF800, 0x7F);
    bus.write(0x4800, 0x0F); // channel 8, volume 15
    // Read the data port back: the last address written is $7F.
    bus.write(0xF800, 0x7F);
    assert_eq!(bus.read(0x4800), 0x0F);

    run(&mut bus, 20_000);
    let peak = bus.apu.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 1e-4, "the N163 wavetable channel is audible: {peak}");
}

/// Swapping an expansion-audio cartridge for one without audio must clear the level the old
/// chip left in the mix. Nothing writes `expansion_pulse` again for the new board — the fetch is
/// skipped when `expansion_audio_name()` is `None` — so without the clear in `set_cart` the N163's
/// last value would stay in the mix for the rest of the session.
#[test]
fn switching_to_a_board_without_expansion_audio_clears_the_mix() {
    let mut bus = bus_for(19);
    program_n163(&mut bus);
    run(&mut bus, 20_000);
    assert_ne!(
        bus.apu.expansion_pulse, 0.0,
        "the N163 left a level in the mix"
    );

    bus.set_cart(load(&ines(0, 2, 2)).unwrap());
    assert_eq!(
        bus.apu.expansion_pulse, 0.0,
        "the old chip's last level stayed in the mix"
    );

    run(&mut bus, 20_000);
    assert_eq!(
        bus.apu.expansion_pulse, 0.0,
        "an NROM board has no expansion audio to write it"
    );
}

/// Swapping one expansion-audio chip for another (N163 -> FME-7) cannot clear the mix the way
/// a swap to a silent board does — the new board's level is not known until the next mixer cycle.
/// The old chip's level therefore stays in the mix for that one cycle and no longer; the N163 is
/// left audible, the unprogrammed FME-7 silent, so the level that replaces it is 0.0.
#[test]
fn swapping_one_audio_chip_for_another_keeps_the_old_level_for_one_mixer_cycle() {
    let mut bus = bus_for(19);
    program_n163(&mut bus);
    run(&mut bus, 20_000);
    let old = bus.apu.expansion_pulse;
    assert_ne!(old, 0.0, "the N163 left a level in the mix");

    bus.set_cart(load(&ines(69, 2, 2)).unwrap());
    assert_eq!(
        bus.apu.expansion_pulse, old,
        "an audio-to-audio swap has not asked the new board yet, so the old level is still mixed"
    );

    let mut stale = 0usize;
    while bus.apu.expansion_pulse == old {
        bus.read(0x0000); // one CPU cycle
        stale += 1;
        assert!(stale < 1_000, "the old chip's level never left the mix");
    }
    assert_eq!(
        bus.apu.expansion_pulse, 0.0,
        "the unprogrammed FME-7 is silent"
    );
    println!("audio-to-audio swap: the old level survived {stale} CPU cycles");
    assert!(
        stale <= 41,
        "the old level survived {stale} CPU cycles, more than one output sample's worth ({})",
        1_789_773 / 44_100
    );
}

#[test]
fn namco163_banks_nametables_through_the_ppu() {
    let mut bus = bus_for(19);
    // Nametable window 0 ($2000-$23FF) shows CHR bank 3.
    bus.write(0xC000, 0x03);
    let (ppu, cart) = bus.ppu_and_cart();
    assert_eq!(ppu.mem_read(cart, 0x2000), 3);
    assert_eq!(ppu.mem_read(cart, 0x2100), 3);
    // Window 1 uses its own register.
    bus.write(0xC800, 0x05);
    let (ppu, cart) = bus.ppu_and_cart();
    assert_eq!(ppu.mem_read(cart, 0x2400), 5);
}

#[test]
fn namco163_banks_nametables_to_ciram_pages() {
    let mut bus = bus_for(19);
    // $E0/$E1 select the two 1 KB CIRAM pages for a window.
    bus.write(0xC000, 0xE0);
    bus.write(0xC800, 0xE1);
    // Write nametable bytes through the PPU's data port.
    for (addr, val) in [(0x2000u16, 0x11u8), (0x2400, 0x22)] {
        bus.write(0x2006, (addr >> 8) as u8);
        bus.write(0x2006, addr as u8);
        bus.write(0x2007, val);
    }
    let (ppu, cart) = bus.ppu_and_cart();
    assert_eq!(ppu.mem_read(cart, 0x2000), 0x11);
    assert_eq!(ppu.mem_read(cart, 0x2400), 0x22);
    // Window 2 has its own register; point it at page A too.
    bus.write(0xD000, 0xE0);
    let (ppu, cart) = bus.ppu_and_cart();
    assert_eq!(
        ppu.mem_read(cart, 0x2800),
        0x11,
        "window 2 shows page A as well"
    );
    assert_eq!(
        ppu.mem_read(cart, 0x2C00),
        0x00,
        "window 3 is still its own page"
    );
}

#[test]
fn vrc7_audio_reaches_the_sample_stream() {
    let mut bus = bus_for(85);
    // Select/write pairs with the chip's settling delays between them.
    let audio_write = |bus: &mut SysBus, reg: u8, val: u8| {
        bus.write(0x9010, reg);
        run(bus, 8);
        bus.write(0x9030, val);
        run(bus, 48);
    };
    // Custom patch: fast attack, no decay or release.
    for (reg, val) in [
        (0x00u8, 0x01u8),
        (0x01, 0x01),
        (0x02, 0x00),
        (0x03, 0x00),
        (0x04, 0xF0),
        (0x05, 0xF0),
        (0x06, 0x00),
        (0x07, 0x00),
        (0x30, 0x00), // instrument 0, volume 0
        (0x10, 0x80), // fnum low
        (0x20, 0x19), // trigger, octave 4, fnum high bit
    ] {
        audio_write(&mut bus, reg, val);
    }
    run(&mut bus, 200_000);
    let peak = bus.apu.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 1e-3, "the OPLL channel is audible: {peak}");
}

#[test]
fn expansion_audio_survives_a_save_state_round_trip() {
    let mut bus = bus_for(24);
    bus.write(0x9000, 0x7F);
    bus.write(0x9002, 0x80);
    run(&mut bus, 5000);

    let mut w = tendy_nes::state::StateWriter::new();
    bus.save_state(&mut w);
    let bytes = w.finish();

    let mut other = bus_for(24);
    let mut r = tendy_nes::state::StateReader::new(&bytes);
    other.load_state(&mut r).unwrap();

    // The sample queue is frontend output, not machine state: drain both and compare what the
    // restored machine produces from here.
    bus.apu.samples.clear();
    other.apu.samples.clear();
    run(&mut bus, 5000);
    run(&mut other, 5000);
    assert_eq!(bus.apu.samples, other.apu.samples);
    assert_eq!(bus.cart().expansion_audio(), other.cart().expansion_audio());
}

#[test]
fn nrom_keeps_open_bus_in_the_register_window() {
    let mut bus = bus_for(0);
    bus.write(0x8000, 0x5A);
    // $4020-$5FFF is unmapped on NROM: the read returns the last value on the bus.
    assert_eq!(bus.read(0x4800), 0x5A);
}

/// Program the N163 to play: a ramp wavetable, one channel, a frequency that moves the phase.
fn program_n163(bus: &mut SysBus) {
    for i in 0..0x40u16 {
        bus.write(0xF800, i as u8);
        bus.write(0x4800, (i as u8).wrapping_mul(7) & 0x3F);
    }
    bus.write(0xF800, 0x7E);
    bus.write(0x4800, 0x00); // channel 8 wave address 0
    bus.write(0xF800, 0x78);
    bus.write(0x4800, 0x40); // channel 8 frequency low
    bus.write(0xF800, 0x79);
    bus.write(0x4800, 0x01);
    bus.write(0xF800, 0x7F);
    bus.write(0x4800, 0x0F); // one enabled channel, volume 15
}

/// The bus now asks the board for its level only on the cycles the mixer runs. Doing what it
/// did before — copy the level in after every cycle — must give the same samples, bit for bit.
///
/// The second machine is the old code: the value it copies in after cycle *n* is the one the mixer
/// reads on cycle *n + 1*, which is exactly what the conditional fetch puts there.
#[test]
fn fetching_expansion_audio_only_on_mix_cycles_is_output_neutral() {
    let mut fast = bus_for(19);
    program_n163(&mut fast);
    let mut every_cycle = bus_for(19);
    program_n163(&mut every_cycle);

    for _ in 0..60_000 {
        fast.read(0x0000);
        every_cycle.read(0x0000);
        every_cycle.apu.expansion_pulse = every_cycle.cart().expansion_audio();
    }

    assert!(!fast.apu.samples.is_empty());
    assert!(
        fast.apu.samples.iter().any(|&s| s != 0.0),
        "the N163 tone reaches the sample stream"
    );
    assert_eq!(fast.apu.samples, every_cycle.apu.samples);
}

/// `expansion_due` reports the mixer's own condition, so an emulated second of CPU clocks
/// costs 176,400 fetches (44,100 output samples, four sub-samples each) instead of 1,789,773.
#[test]
fn the_board_is_asked_for_its_level_once_per_mixer_evaluation() {
    let mut apu = Apu::new();
    let mut due = 0usize;
    for _ in 0..1_789_773 {
        if apu.expansion_due() {
            due += 1;
        }
        apu.clock();
    }
    // 44,100 sample periods in an emulated second, four sub-samples each: the mixer runs 176,400
    // times — once per 10.15 CPU cycles — where the old bus fetched on every one of the 1,789,773.
    // The 44,100th sample is still in the accumulator when the last cycle ends.
    assert_eq!(due, 176_400);
    assert_eq!(apu.samples.len(), 44_099);
    assert!(
        due * 10 < 1_789_773,
        "{due} fetches is fewer than one per ten cycles"
    );
}

/// The cartridge chips sum at the console's audio node, after the 2A03's own DACs: a level a board
/// asks for must arrive at that level whether the native channels are silent or at full volume.
/// While the expansion term was inside the pulse sum, 0.25 asked for with both pulses at 15 came
/// out at 0.12 -- the chip ducked under the native channels, which is the review's finding.
#[test]
fn a_cartridge_level_is_not_ducked_by_the_native_channels() {
    for volume in [0u8, 15] {
        let mut bus = bus_for(24);
        bus.write(0x4015, 0x03); // pulse 1 and pulse 2 enabled
        bus.write(0x4000, 0xB0 | volume); // duty 2, halt, constant volume
        bus.write(0x4002, 0x10);
        bus.write(0x4003, 0x08);
        bus.write(0x4004, 0xB0 | volume);
        bus.write(0x4006, 0x10);
        bus.write(0x4007, 0x08);
        let mut spins = 0;
        while bus.apu.pulse1.output() != volume || bus.apu.pulse2.output() != volume {
            bus.read(0x0000);
            spins += 1;
            assert!(
                spins < 1000,
                "the pulses never reached the level under test"
            );
        }
        let native = bus.apu.mix();
        bus.apu.expansion_pulse = Apu::amplitude_to_pulse_units(0.25);
        let heard = bus.apu.mix() - native;
        assert!(
            (heard - 0.25).abs() < 1e-6,
            "pulses at {volume}: asked for 0.25, heard {heard}"
        );
    }
}

/// A 2A03-only cart must sound exactly as it did: this digest pins the sample stream (both pulses,
/// triangle, noise and a DMC level, through the filter chain) and it is the same number before the
/// expansion term left the pulse sum and after.
#[test]
fn a_2a03_only_sample_stream_is_unchanged() {
    let mut bus = bus_for(0);
    bus.write(0x4015, 0x0F);
    bus.write(0x4000, 0xBF);
    bus.write(0x4002, 0x64);
    bus.write(0x4003, 0x00);
    bus.write(0x4004, 0xBF);
    bus.write(0x4006, 0x8C);
    bus.write(0x4007, 0x01);
    bus.write(0x4008, 0xFF);
    bus.write(0x400A, 0x40);
    bus.write(0x400B, 0x02);
    bus.write(0x400C, 0x3F);
    bus.write(0x400E, 0x04);
    bus.write(0x400F, 0x10);
    bus.write(0x4011, 0x40);
    run(&mut bus, 40_000);
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for sample in &bus.apu.samples {
        digest ^= u64::from(sample.to_bits());
        digest = digest.wrapping_mul(0x0100_0000_01b3);
    }
    assert_eq!(bus.apu.samples.len(), 985, "sample count: {digest:016x}");
    assert_eq!(
        digest, 0xf67e_5e32_dbc7_2e40,
        "the 2A03-only stream changed"
    );
}
