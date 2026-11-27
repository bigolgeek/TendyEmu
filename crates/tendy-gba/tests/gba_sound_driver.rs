//! The BIOS sound driver (`$1A`-`$1F`, `$28`/`$29`, `$20`-`$24`), tested on synthesized work areas and
//! synthesized waveforms — the one place a mixer's output can be predicted on paper.
//!
//! GBATEK, *BIOS Sound Functions*, documents the structures and the mode word but not the mixing
//! loop, so these tests are the specification of the loop this machine derived: a channel's envelope
//! steps once per `Main` call, its phase advances by `fr / rate` source samples per output sample,
//! and a sample is `data * level/255 * volume/255` truncated. Every expected number below is
//! arithmetic on the constants in the test, not a value read back from an earlier run.
//!
//! The layout is X-Men's: `vchn[0]` at `+$50`, a
//! channel record every `$40` bytes, `fr` at `+$20`, `wp` at `+$24`, `pcmbuf` at `+$250`.

use tendy_gba::Gba;
use tendy_gba::bios::{self, Bios};
use tendy_gba::cart::Cartridge;
use tendy_gba::sound_driver::{CHANNEL_STRIDE, CHUNK, IDENT, PCMBUF, VCHN};

/// A work area in EWRAM, well away from the cartridge and from the wave data below it.
const WORK: u32 = 0x0200_1000;
/// Where the synthesized `WaveData` sits: past the area the driver owns (`pcmbuf` ends at `+$D50`).
const WAVE: u32 = WORK + 0x1000;

/// A cartridge whose entry point is the usual branch; the tests call the BIOS directly.
fn machine() -> Gba {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"GBA-SND\0");
    rom[0xAC..0xB0].copy_from_slice(b"TBS2");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    Gba::new(Cartridge::new(rom).expect("a synthesized cartridge"))
}

fn call(gba: &mut Gba, number: u32, args: [u32; 4]) -> [u32; 4] {
    gba.cpu.r[0] = args[0];
    gba.cpu.r[1] = args[1];
    gba.cpu.r[2] = args[2];
    gba.cpu.r[3] = args[3];
    bios::call(&mut gba.cpu, &mut gba.bus, number);
    [gba.cpu.r[0], gba.cpu.r[1], gba.cpu.r[2], gba.cpu.r[3]]
}

fn put(gba: &mut Gba, addr: u32, bytes: &[u8]) {
    for (index, &byte) in bytes.iter().enumerate() {
        gba.bus.write8(addr + index as u32, byte);
    }
}

fn get(gba: &mut Gba, addr: u32, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| gba.bus.read8(addr + index as u32))
        .collect()
}

fn word(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

/// One `SndCh`, in the field order and at the offsets the caller was measured writing.
#[derive(Clone, Copy)]
struct Channel {
    sf: u8,
    lv: u8,
    rv: u8,
    at: u8,
    de: u8,
    su: u8,
    re: u8,
    fr: u32,
    wave: u32,
}

impl Channel {
    fn sounding(wave: u32, fr: u32) -> Channel {
        Channel {
            sf: 0x80,
            lv: 0xFF,
            rv: 0xFF,
            at: 0xFF,
            de: 0,
            su: 0xFF,
            re: 0,
            fr,
            wave,
        }
    }
}

fn write_channel(gba: &mut Gba, index: u32, channel: Channel) {
    let at = WORK + VCHN + index * CHANNEL_STRIDE;
    put(
        gba,
        at,
        &[
            channel.sf, 0, channel.rv, channel.lv, channel.at, channel.de, channel.su, channel.re,
        ],
    );
    put(gba, at + 0x20, &word(channel.fr));
    put(gba, at + 0x24, &word(channel.wave));
}

/// A `WaveData`: `type`, `stat`, `freq`, `loop`, `size`, then the signed samples.
fn write_wave(gba: &mut Gba, addr: u32, stat: u16, loop_at: u32, size: u32, data: &[i8]) {
    let mut bytes = Vec::with_capacity(16 + data.len());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&stat.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&loop_at.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend(data.iter().map(|&sample| sample as u8));
    put(gba, addr, &bytes);
}

/// A machine with an initialised driver at [`WORK`], mode `word`, ready to mix.
fn driver(word: u32) -> Gba {
    let mut gba = machine();
    call(&mut gba, 0x1A, [WORK, 0, 0, 0]);
    call(&mut gba, 0x1B, [word, 0, 0, 0]);
    gba
}

/// The left half-buffer the next `Vsync` will play, after `Main` filled it.
fn left(work: u32, half: u32) -> u32 {
    work + PCMBUF + half * CHUNK as u32
}

/// The right half-buffer.
fn right(work: u32, half: u32) -> u32 {
    work + PCMBUF + (2 + half) * CHUNK as u32
}

/// One frame: `Main` mixes into the back half and `Vsync` makes it the front one, which is the order
/// X-Men calls them in.
fn frame(gba: &mut Gba) {
    call(gba, 0x1C, [0, 0, 0, 0]);
    call(gba, 0x1D, [0, 0, 0, 0]);
}

/// The 10512 Hz mode: index 3 of GBATEK's rate list, every other field at its documented default.
const MODE_10512: u32 = 3 << 16;

#[test]
fn the_mode_word_decodes_with_the_documented_defaults_standing_in_for_zero_fields() {
    // X-Men's own word: rate 3 and nothing else. GBATEK's ranges make a zero field meaningless, so
    // each one falls back to the documented default: 8 channels, master 15, DAC field 9 (8 bits).
    let mut gba = driver(0x0003_0000);
    assert_eq!(gba.bus.read32(WORK + 0x0C), 10_512, "rate 3 is 10 512 Hz");
    assert_eq!(gba.bus.read16(WORK + 0x06), 8, "8 channels by default");
    assert_eq!(
        gba.bus.read8(WORK + 0x18),
        15,
        "master volume 15 by default"
    );
    assert_eq!(gba.bus.read8(WORK + 0x19), 9, "DAC field 9 by default");
    // DAC field 9 is 8 bits, which is `SOUNDBIAS` sampling cycle 1 (65.536 kHz).
    assert_eq!(gba.bus.read16(0x0400_0088) & 0xC000, 0x4000);

    // A word that sets every field: reverb 0x40 with bit 7 on, 6 channels, master 12, rate 7
    // (21024 Hz) and DAC field 10 (7 bits, sampling cycle 2).
    call(&mut gba, 0x1B, [0x00A7_C6C0, 0, 0, 0]);
    assert_eq!(gba.bus.read32(WORK + 0x0C), 21_024, "rate 7 is 21 024 Hz");
    assert_eq!(gba.bus.read16(WORK + 0x06), 6);
    assert_eq!(gba.bus.read8(WORK + 0x18), 12);
    assert_eq!(gba.bus.read8(WORK + 0x19), 10);
    assert_eq!(gba.bus.read8(WORK + 0x05), 0x40, "the reverb field is kept");
    assert_eq!(gba.bus.read16(0x0400_0088) & 0xC000, 0x8000);
}

#[test]
fn a_square_wave_plays_the_sample_under_the_phase_pointer() {
    let mut gba = driver(MODE_10512);
    // Eight samples of a square wave, looped from zero, and a channel whose `fr` equals the playback
    // rate — one source sample per output sample, so the nth output value is data[n % 8].
    write_wave(
        &mut gba,
        WAVE,
        0x4000,
        0,
        8,
        &[100, 100, 100, 100, -100, -100, -100, -100],
    );
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    assert_eq!(
        left(WORK, 1),
        gba.bus.dma.channel(1).sad,
        "Vsync plays the half Main filled"
    );
    let samples = get(&mut gba, left(WORK, 1), 12);
    let expected: Vec<u8> = [
        100i8, 100, 100, 100, -100, -100, -100, -100, 100, 100, 100, 100,
    ]
    .iter()
    .map(|&sample| sample as u8)
    .collect();
    assert_eq!(
        samples, expected,
        "the square's own bytes, one per output sample"
    );
    // The right side is the same stream at the same volumes.
    assert_eq!(get(&mut gba, right(WORK, 1), 12), expected);
}

#[test]
fn the_master_volume_scales_the_mix_and_five_is_not_the_identity() {
    // Master 5, in the mode word's 4-bit field (bits 12-15); rate 3 as everywhere else. Every other
    // test in this file runs at the default master 15, where `left * master / 15` is the identity,
    // so a master volume that is ignored, inverted or mis-scaled passes all of them — this is the
    // one test that can fail it.
    let mut gba = driver((5 << 12) | MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 8, &[100; 8]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    // Hand-computed, not observed: 100 · 255/255 · 255/255 = 100, then 100 · 5/15 = 500/15 = 33
    // in integer division. A master that did nothing would leave 100 here; one applied twice would
    // leave 11 (100 · 5/15 applied again: 33 · 5/15).
    let expected = vec![33u8; 8];
    assert_eq!(
        get(&mut gba, left(WORK, 1), 8),
        expected,
        "computed 100 x 5/15 = 33, not a sample read back from a run"
    );
    assert_eq!(get(&mut gba, right(WORK, 1), 8), expected);
}

#[test]
fn a_phase_step_of_one_and_a_half_walks_the_wave_at_one_and_a_half_samples() {
    let mut gba = driver(MODE_10512);
    write_wave(
        &mut gba,
        WAVE,
        0x4000,
        0,
        8,
        &[0, 10, 20, 30, 40, 50, 60, 70],
    );
    // fr = 1.5 x the playback rate: the phase walks 0, 1.5, 3, 4.5, 6, 7.5 — which wraps to 1 —
    // then 2.5, so the source samples read are 0, 1, 3, 4, 6, 7, 1, 2.
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 15_768));
    frame(&mut gba);
    let samples = get(&mut gba, left(WORK, 1), 8);
    assert_eq!(samples, vec![0u8, 10, 30, 40, 60, 70, 10, 20]);
}

#[test]
fn the_envelope_attacks_by_its_own_value_each_frame() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    let mut channel = Channel::sounding(WAVE, 10_512);
    channel.at = 51;
    write_channel(&mut gba, 0, channel);
    // One sample a frame is enough: the first byte of each half-buffer is the first mixed sample.
    let mut levels = Vec::new();
    let mut half = 1;
    for _ in 0..4 {
        call(&mut gba, 0x1C, [0, 0, 0, 0]);
        call(&mut gba, 0x1D, [0, 0, 0, 0]);
        levels.push(gba.bus.read8(left(WORK, half)));
        half ^= 1;
    }
    // 100 * level / 255 with levels 51, 102, 153, 204: 20, 40, 60, 80 — exactly.
    assert_eq!(levels, vec![20u8, 40, 60, 80]);
}

#[test]
fn the_decay_multiplies_by_its_value_until_the_sustain_is_reached() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[127]);
    let mut channel = Channel::sounding(WAVE, 10_512);
    channel.at = 255;
    channel.de = 128;
    channel.su = 32;
    write_channel(&mut gba, 0, channel);
    let mut samples = Vec::new();
    let mut half = 1;
    for _ in 0..6 {
        frame(&mut gba);
        samples.push(gba.bus.read8(left(WORK, half)));
        half ^= 1;
    }
    // Levels 255, 127, 63, 32, 32, 32 (31 is below the sustain, so it is pinned to 32), and
    // 127 * level / 255 truncated: 127, 63, 31, 15, 15, 15.
    assert_eq!(samples, vec![127u8, 63, 31, 15, 15, 15]);
}

#[test]
fn a_released_channel_decays_by_its_release_value_and_stops_at_zero() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[127]);
    let mut channel = Channel::sounding(WAVE, 10_512);
    channel.re = 128;
    write_channel(&mut gba, 0, channel);
    frame(&mut gba); // the attack takes the level to 255
    // Key off: "logical OR 40h for a release-attached off".
    let sf = WORK + VCHN;
    gba.bus.write8(sf, 0xC0);
    let mut samples = Vec::new();
    let mut half = 0;
    for _ in 0..10 {
        frame(&mut gba);
        samples.push(gba.bus.read8(left(WORK, half)) as i8);
        half ^= 1;
    }
    // 255 halved eight times is 0; the channel is then "completely stopped" and silent.
    assert_eq!(samples[0], 63, "128/256 of 127 is 63");
    assert_eq!(samples[1], 31);
    assert_eq!(samples[6], 0, "the release reached zero");
    assert_eq!(
        gba.bus.read8(sf),
        0,
        "a released channel stops when it reaches zero"
    );
}

#[test]
fn a_wave_that_does_not_loop_stops_at_its_size() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x0000, 0, 2, &[10, 20]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    let samples = get(&mut gba, left(WORK, 1), 4);
    assert_eq!(samples, vec![10u8, 20, 0, 0], "two samples, then silence");
    assert_eq!(gba.bus.read8(WORK + VCHN), 0, "and the channel is stopped");
}

#[test]
fn a_looped_wave_wraps_at_its_loop_pointer_not_at_zero() {
    let mut gba = driver(MODE_10512);
    // Eight samples, a forward loop from sample 4: 0..7 then 4..7 for ever.
    write_wave(
        &mut gba,
        WAVE,
        0x4000,
        4,
        8,
        &[0, 10, 20, 30, 40, 50, 60, 70],
    );
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    let samples = get(&mut gba, left(WORK, 1), 12);
    assert_eq!(
        samples,
        vec![0u8, 10, 20, 30, 40, 50, 60, 70, 40, 50, 60, 70]
    );
}

#[test]
fn the_two_channel_volumes_reach_their_own_sides() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    let mut channel = Channel::sounding(WAVE, 10_512);
    channel.lv = 0; // right only
    write_channel(&mut gba, 0, channel);
    frame(&mut gba);
    assert_eq!(
        get(&mut gba, left(WORK, 1), 4),
        vec![0u8; 4],
        "lv 0 is silence on the left"
    );
    assert_eq!(
        get(&mut gba, right(WORK, 1), 4),
        vec![100u8; 4],
        "and the right plays"
    );
}

#[test]
fn the_mode_words_channel_count_caps_how_many_voices_are_mixed() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    write_channel(&mut gba, 1, Channel::sounding(WAVE, 10_512));
    // One channel in the mode word: the second channel's record is ignored.
    call(&mut gba, 0x1B, [(1 << 8) | (3 << 16), 0, 0, 0]);
    frame(&mut gba);
    assert_eq!(
        get(&mut gba, left(WORK, 1), 2),
        vec![100u8, 100],
        "one voice, not two"
    );
}

#[test]
fn vsync_hands_the_dma_the_half_main_just_filled_and_the_other_half_next_frame() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    frame(&mut gba);
    let front = gba.bus.read8(WORK + 0x1A);
    assert_eq!(front, 0, "two Vsyncs return to the first half");
    let dma1 = gba.bus.dma.channel(1);
    assert_eq!(dma1.sad, left(WORK, 0));
    assert_eq!(dma1.dad, 0x0400_00A0, "FIFO A");
    assert_eq!(
        dma1.control, 0xB640,
        "enable, special timing, 32-bit, repeat, fixed"
    );
    let dma2 = gba.bus.dma.channel(2);
    assert_eq!(dma2.sad, right(WORK, 0));
    assert_eq!(dma2.dad, 0x0400_00A4, "FIFO B");
    // And the bytes at the pointer are the mix: the channel's single sample at full level.
    assert_eq!(get(&mut gba, dma1.sad, 2), vec![100u8, 100]);
}

#[test]
fn a_frame_is_the_rates_own_number_of_samples() {
    let mut gba = driver(MODE_10512);
    // 10 512 / 59.7275 = 175.98, so a frame carries 175 or 176 samples and the fraction is carried
    // in the accumulator. Sixty frames owe 10 512 * 60 / 59.7275 = 10 559 samples.
    let mut total = 0;
    for _ in 0..60 {
        call(&mut gba, 0x1C, [0, 0, 0, 0]);
        let samples = gba.bus.read32(WORK + 0x1C);
        assert!((175..=176).contains(&samples), "one frame owes {samples}");
        total += samples;
    }
    assert_eq!(total, 10_559, "sixty frames of 10 512 Hz");
}

#[test]
fn an_uninitialised_driver_answers_and_touches_nothing() {
    let mut gba = machine();
    let before = get(&mut gba, WORK, 0x20);
    for number in [0x1C, 0x1D, 0x1E, 0x28, 0x29] {
        call(&mut gba, number, [WORK, 0, 0, 0]);
    }
    assert_eq!(get(&mut gba, WORK, 0x20), before);
    assert_eq!(
        gba.bus.bios.unimplemented, None,
        "every sound entry is answered"
    );
    assert_eq!(gba.bus.dma.channel(1).control, 0, "and no DMA was armed");
}

#[test]
fn init_clears_the_area_and_marks_it() {
    let mut gba = machine();
    put(&mut gba, WORK, &[0xAB; 0x20]);
    call(&mut gba, 0x1A, [WORK, 0, 0, 0]);
    assert_eq!(
        gba.bus.read32(WORK),
        IDENT,
        "ident says the area is the driver's"
    );
    assert_eq!(
        get(&mut gba, WORK + 0x20, 8),
        vec![0u8; 8],
        "the rest of the header is clear"
    );
    // A work area outside work RAM is refused rather than written to.
    let mut gba = machine();
    call(&mut gba, 0x1A, [0x0400_0000, 0, 0, 0]);
    assert_eq!(gba.bus.bios.sound_work, None, "I/O is not a work area");
}

#[test]
fn a_clear_stops_the_channels_the_dma_and_the_sample_clock() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    frame(&mut gba);
    call(&mut gba, 0x1E, [0, 0, 0, 0]);
    assert_eq!(
        gba.bus.read8(WORK + VCHN),
        0,
        "the channel record is cleared"
    );
    assert!(
        !gba.bus.dma.channel(1).enabled(),
        "and the FIFO channel is disarmed"
    );
    assert_eq!(
        gba.bus.read16(0x0400_0106) & 0x80,
        0,
        "the sample clock is stopped"
    );
}

#[test]
fn the_vsync_pair_pauses_and_resumes_the_dma() {
    let mut gba = driver(MODE_10512);
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[100]);
    write_channel(&mut gba, 0, Channel::sounding(WAVE, 10_512));
    frame(&mut gba);
    call(&mut gba, 0x28, [0, 0, 0, 0]);
    assert!(
        !gba.bus.dma.channel(1).enabled(),
        "VsyncOff stops the sound DMA"
    );
    assert!(!gba.bus.dma.channel(2).enabled());
    frame(&mut gba);
    assert!(
        !gba.bus.dma.channel(1).enabled(),
        "and a Vsync while paused does not re-arm it"
    );
    call(&mut gba, 0x29, [0, 0, 0, 0]);
    frame(&mut gba);
    assert!(
        gba.bus.dma.channel(1).enabled(),
        "VsyncOn lets the next Vsync re-arm it"
    );
}

#[test]
fn every_sound_number_is_answered_and_the_jump_list_still_refuses() {
    let mut gba = driver(MODE_10512);
    for number in (0x1A..=0x24).chain([0x28, 0x29]) {
        call(&mut gba, number, [WORK, 0, 0, 0]);
        assert_eq!(
            gba.bus.bios.unimplemented, None,
            "{number:#04x} is answered"
        );
    }
    // $2A is SoundGetJumpList — 36 more routines GBATEK gives the shape of and none of the names
    // for — and $2B is not a call. No cartridge in the library asks for $2A (0 of 866 over
    // 1 200 frames), so it stays refused rather than answered with invented addresses.
    for number in [0x2A, 0x2B] {
        call(&mut gba, number, [0, 0, 0, 0]);
        assert_eq!(
            gba.bus.bios.unimplemented,
            Some(0x2A),
            "{number:#04x} still refuses"
        );
    }
}

#[test]
fn midi_key_2_freq_returns_the_scale_the_caller_uses() {
    let mut gba = machine();
    // The wave X-Men plays: 44 100 Hz at key 60, so `freq` is 44 100 * 2^((180-60)/12) = 45 158 400.
    write_wave(&mut gba, WAVE, 0x4000, 0, 1, &[0]);
    put(&mut gba, WAVE + 4, &word(44_100 * 1024));
    let key_36 = call(&mut gba, 0x1F, [WAVE, 36, 0, 0])[0];
    assert_eq!(key_36, 11_025, "45 158 400 / 2^12");
    let key_48 = call(&mut gba, 0x1F, [WAVE, 48, 0, 0])[0];
    assert_eq!(key_48, 22_050, "45 158 400 / 2^11");
    // Half a semitone up is 2^(1/24) of the value: 11 025 * 1.0293 = 11 348.
    let fine = call(&mut gba, 0x1F, [WAVE, 36, 128, 0])[0];
    assert!(
        (11_346..=11_350).contains(&fine),
        "fp 128 is half a semitone: {fine}"
    );
}

#[test]
fn the_state_carries_the_work_area_pointer() {
    let mut bios = Bios::new();
    bios.sound_work = Some(WORK);
    let bytes = bios.encode();
    assert_eq!(bytes.len(), Bios::STATE_LEN);
    let mut restored = Bios::new();
    restored.decode(&bytes);
    assert_eq!(restored.sound_work, Some(WORK));
    assert_eq!(Bios::new().sound_work, None);
}
