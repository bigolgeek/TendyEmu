//! G5, the Game Boy Advance's sound: the two DMA-fed sample channels, the four PSG voices, the
//! mixer and the frame seam.
//!
//! Everything here is built from GBATEK (*GBA Sound Controller*, *GBA Sound Channel A and B - DMA
//! Sound*, *SOUNDCNT_H*, *SOUNDBIAS*, *GBA DMA Transfers*) and from cartridges synthesized in code,
//! so no test needs a ROM file. The commercial-cartridge measurement is a headless probe, not a
//! unit test, because a ROM is never committed.

use tendy_gb::system::System;
use tendy_gba::bus::{CYCLES_PER_FRAME, EWRAM_BASE, IO_BASE};
use tendy_gba::cart::Cartridge;
use tendy_gba::sound::{self, FIFO_A, FIFO_B, FIFO_BYTES, SOUNDCNT_H, SOUNDCNT_L, SOUNDCNT_X};
use tendy_gba::{Gba, dma, timer};

/// A cartridge whose code starts at `$080000C0`, the usual entry: the header says so with a `b`.
fn synth_cart(code: &[u32]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-TEST\0");
    rom[0xAC..0xB0].copy_from_slice(b"TST1");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    for (index, word) in code.iter().enumerate() {
        let at = 0xC0 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// `b .`
const SPIN: u32 = 0xEAFF_FFFE;

/// A machine running `code` from the cartridge's entry point.
fn machine(code: &[u32]) -> Gba {
    Gba::new(Cartridge::new(synth_cart(code)).expect("a synthesized cartridge"))
}

/// A machine that does nothing but spin, which is enough to keep the frame clock moving.
fn idle() -> Gba {
    machine(&[SPIN])
}

fn set16(gba: &mut Gba, offset: usize, value: u16) {
    gba.bus.write16(IO_BASE + offset as u32, value);
}

fn set32(gba: &mut Gba, offset: usize, value: u32) {
    gba.bus.write32(IO_BASE + offset as u32, value);
}

/// Program one DMA channel the way a sound driver does.
fn program_dma(gba: &mut Gba, index: usize, sad: u32, dad: u32, control: u16) {
    let base = dma::BASE + index * dma::STRIDE;
    set32(gba, base, sad);
    set32(gba, base + dma::DAD, dad);
    set16(gba, base + dma::COUNT, 0);
    set16(gba, base + dma::CONTROL, control);
}

/// Start timer 0 at a period of `period` master cycles: prescaler /1, reload `0x10000 - period`.
fn start_sample_timer(gba: &mut Gba, period: u32) {
    set16(gba, timer::TIMER_BASE, (0x1_0000 - period) as u16);
    set16(gba, timer::TIMER_BASE + 2, timer::START | timer::IRQ);
}

/// A sample ramp in EWRAM at `$02000100`: `count` bytes of `-128..`, so every byte is distinct.
fn sample_ram(count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| (index as u8).wrapping_mul(3))
        .collect()
}

fn write_samples(gba: &mut Gba, at: usize, bytes: &[u8]) {
    gba.bus.ewram[at..at + bytes.len()].copy_from_slice(bytes);
}

/// FIFO A fed by DMA1 in its sound-FIFO mode, channel A on both sides at 100 %.
fn fifo_a_through_dma(gba: &mut Gba, sad: u32) {
    // Enable, Special timing, 32-bit units, repeat, destination fixed: the control word GBATEK's
    // *Sound DMA* chapter describes.
    program_dma(
        gba,
        1,
        sad,
        IO_BASE + FIFO_A as u32,
        dma::ENABLE | (3 << 12) | dma::WIDTH32 | dma::REPEAT | (0b10 << 5),
    );
    set16(gba, SOUNDCNT_X, sound::MASTER_ENABLE);
    set16(gba, SOUNDCNT_H, sound::FIFO_A_VOLUME);
    gba.bus.sound.write8(SOUNDCNT_H + 1, 0x03); // A to left and right
}

#[test]
fn a_drained_fifo_asks_dma1_for_sixteen_bytes_and_the_transfer_fills_it() {
    let mut gba = idle();
    let source = sample_ram(32);
    write_samples(&mut gba, 0x100, &source);
    fifo_a_through_dma(&mut gba, EWRAM_BASE + 0x100);
    // Nothing has played yet, so nothing has been asked for: GBATEK's procedure checks the level
    // after a timer overflow moves a byte to the sound circuit, and no overflow has happened.
    assert!(gba.bus.sound.fifo[0].data.is_empty());
    start_sample_timer(&mut gba, 512);
    gba.bus.tick(512);
    let fifo = &gba.bus.sound.fifo[0];
    assert_eq!(fifo.data.len(), 16, "one FIFO transfer is 16 bytes");
    assert_eq!(
        fifo.data.iter().copied().collect::<Vec<_>>(),
        source[..16].iter().map(|b| *b as i8).collect::<Vec<_>>()
    );
    assert_eq!(
        gba.bus.dma.channel(1).cursor_sad,
        EWRAM_BASE + 0x110,
        "the source walked four 32-bit units"
    );
    assert_eq!(
        gba.bus.dma.channel(1).dad,
        IO_BASE + FIFO_A as u32,
        "the destination register never moves"
    );
}

#[test]
fn the_timer_moves_one_sample_per_overflow_and_requests_the_next_block() {
    let mut gba = idle();
    let source = sample_ram(32);
    write_samples(&mut gba, 0x100, &source);
    fifo_a_through_dma(&mut gba, EWRAM_BASE + 0x100);
    start_sample_timer(&mut gba, 512);
    // The first overflow asks for the first block: one sample (from an empty FIFO, the held zero)
    gba.bus.tick(512);
    assert_eq!(gba.bus.sound.fifo[0].data.len(), 16);
    // The second overflow plays the block's first byte, and the pop that leaves the FIFO at the
    // refill level has already asked for the next block.
    gba.bus.tick(512);
    assert_eq!(gba.bus.sound.fifo[0].current, source[0] as i8);
    assert_eq!(gba.bus.sound.fifo[0].data.len(), 31);
    // A whole block later the FIFO asks again and the source walks on.
    gba.bus.tick(512 * 16);
    assert!(
        gba.bus.dma.channel(1).cursor_sad >= EWRAM_BASE + 0x120,
        "the second block came from the next sixteen bytes: {:08x}",
        gba.bus.dma.channel(1).cursor_sad
    );
    assert!(!gba.bus.sound.fifo[0].data.is_empty());
}

#[test]
fn a_special_timing_channel_that_is_not_pointed_at_a_fifo_does_not_run() {
    let mut gba = idle();
    write_samples(&mut gba, 0x100, &sample_ram(32));
    // Special timing, but the destination is a work-RAM word rather than FIFO_A.
    program_dma(
        &mut gba,
        2,
        EWRAM_BASE + 0x100,
        EWRAM_BASE + 0x200,
        dma::ENABLE | (3 << 12) | dma::WIDTH32 | dma::REPEAT,
    );
    gba.bus.tick(4096);
    assert_eq!(
        gba.bus.ewram[0x200..0x210],
        [0; 16],
        "nothing may run it: the FIFO request is the only start"
    );
    assert!(
        gba.bus.dma.channel(2).enabled(),
        "still armed, waiting for a FIFO"
    );
}

#[test]
fn the_two_fifos_keep_their_own_dma_channels_and_timers() {
    let mut gba = idle();
    write_samples(&mut gba, 0x100, &sample_ram(32));
    write_samples(&mut gba, 0x180, &[0x40; 32]);
    // A on DMA1/timer 0, B on DMA2/timer 1, at different sample rates.
    fifo_a_through_dma(&mut gba, EWRAM_BASE + 0x100);
    program_dma(
        &mut gba,
        2,
        EWRAM_BASE + 0x180,
        IO_BASE + FIFO_B as u32,
        dma::ENABLE | (3 << 12) | dma::WIDTH32 | dma::REPEAT | (0b10 << 5),
    );
    set16(
        &mut gba,
        SOUNDCNT_H,
        sound::FIFO_A_VOLUME | sound::FIFO_B_VOLUME,
    );
    gba.bus.sound.write8(SOUNDCNT_H + 1, 0x33); // both channels to both sides
    gba.bus.sound.write8(SOUNDCNT_H + 1, 0x73); // and B's sample timer is timer 1 (bit 14)
    start_sample_timer(&mut gba, 512);
    // Timer 1 at half A's rate.
    set16(&mut gba, timer::TIMER_BASE + 4, (0x1_0000 - 1024u32) as u16);
    set16(
        &mut gba,
        timer::TIMER_BASE + 6,
        timer::START | timer::IRQ | (1 << 6),
    );
    gba.bus.tick(2048);
    assert_eq!(
        gba.bus.sound.fifo[0].current,
        sample_ram(32)[2] as i8,
        "A played a byte per 512-cycle overflow, after the first one asked for the block"
    );
    assert_eq!(
        gba.bus.sound.fifo[1].current, 0x40,
        "B played at half the rate, after its own first overflow asked for its block"
    );
}

#[test]
fn a_sounding_fifo_reaches_the_front_ends_drain_as_audible_samples() {
    let mut gba = idle();
    gba.set_sample_rate(44_100);
    // Four KiB of one sample value: two frames pop about 1 100 bytes, so the stream has to be
    // longer than the source the other tests use or the DMA walks into unwritten EWRAM.
    write_samples(&mut gba, 0x100, &[0x40; 4096]);
    fifo_a_through_dma(&mut gba, EWRAM_BASE + 0x100);
    start_sample_timer(&mut gba, 512);
    let mut audio = vec![0.0f32; 4096];
    let mut total = 0usize;
    for _ in 0..2 {
        gba.run_frame();
        total += gba.drain_audio(&mut audio);
    }
    let want = 2.0 * f64::from(CYCLES_PER_FRAME) * 44_100.0 / f64::from(tendy_gba::CLOCK_HZ);
    assert!(
        (total as f64 - want).abs() <= 2.0,
        "{total} frames drained, {want} expected"
    );
    assert!(
        audio[..total * 2].iter().any(|s| (*s - 0.5).abs() < 0.01),
        "the FIFO's 0x40 samples reached the drain at half amplitude"
    );
    assert!(
        audio[..total * 2].iter().all(|s| s.abs() <= 1.0),
        "and the mix clipped rather than wrapping"
    );
}

#[test]
fn the_fifo_holds_its_last_sample_when_the_dma_stops_refilling() {
    let mut gba = idle();
    let source = sample_ram(32);
    write_samples(&mut gba, 0x100, &source);
    // Manually load four bytes and enable the master switch; no DMA at all.
    set16(&mut gba, SOUNDCNT_X, sound::MASTER_ENABLE);
    set16(&mut gba, SOUNDCNT_H, sound::FIFO_A_VOLUME);
    gba.bus.sound.write8(SOUNDCNT_H + 1, 0x03);
    for byte in &source[..4] {
        gba.bus.sound.write8(FIFO_A, *byte);
    }
    start_sample_timer(&mut gba, 512);
    gba.bus.tick(512 * 8);
    assert_eq!(
        gba.bus.sound.fifo[0].current, source[3] as i8,
        "the fourth byte is still what the DAC holds eight overflows later"
    );
    assert!(gba.bus.sound.fifo[0].data.is_empty());
    let mut audio = vec![0.0f32; 64];
    gba.drain_audio(&mut audio);
    assert!(
        !audio.iter().all(|s| *s == 0.0),
        "and it is still audible rather than clicking to silence"
    );
}

#[test]
fn a_square_voice_sounds_through_the_register_map_and_reports_itself_on() {
    let mut gba = idle();
    gba.set_sample_rate(32_768);
    set16(&mut gba, SOUNDCNT_X, sound::MASTER_ENABLE);
    set16(&mut gba, SOUNDCNT_H, 2); // PSG voices at 100 %
    set16(&mut gba, SOUNDCNT_L, 0xF7F7); // every voice on both sides, master volume 7
    set16(&mut gba, 0x60, 0x0000); // sweep time 0: disabled
    set16(&mut gba, 0x62, 0xC080); // duty 50 %, volume 12
    set16(&mut gba, 0x64, 0x8780); // period $780 (1024 Hz), trigger
    assert_eq!(
        gba.bus.read16(IO_BASE + SOUNDCNT_X as u32) & 0x0F,
        1,
        "SOUNDCNT_X bit 0 reports channel 1 sounding"
    );
    gba.run_frame();
    let mut audio = vec![0.0f32; 4096];
    let frames = gba.drain_audio(&mut audio);
    assert!(frames > 0);
    let peak = audio[..frames * 2]
        .iter()
        .fold(0.0f32, |peak, s| peak.max(s.abs()));
    let high = audio[..frames * 2]
        .iter()
        .fold(f32::MIN, |high, s| high.max(*s));
    let low = audio[..frames * 2]
        .iter()
        .fold(f32::MAX, |low, s| low.min(*s));
    // The PSG swing is the Color's unipolar 0..15 DAC input less its 7.5 mid-point: a volume-12
    // square reaches (12-7.5)/7.5 * 0.25 = +0.15 and (0-7.5)/7.5 * 0.25 = -0.25. The analog
    // high-pass that removes that DC offset is not modelled, exactly as the Game Boy's APU does not
    // model it.
    assert!(
        (high - 0.15).abs() < 0.02,
        "the high half is a quarter range at 12/15: {high}"
    );
    assert!(
        (low + 0.25).abs() < 0.02,
        "and the low half swings to the mid-point below zero: {low}"
    );
    assert!(peak <= 1.0, "the mix never wraps: {peak}");
    // Clearing the master enable resets the PSG registers, as GBATEK says.
    set16(&mut gba, SOUNDCNT_X, 0);
    assert_eq!(gba.bus.read16(IO_BASE + 0x62), 0);
    assert_eq!(
        gba.bus.read16(IO_BASE + SOUNDCNT_X as u32) & 0x0F,
        0,
        "and the channel flags follow the reset"
    );
}

/// A triangle in the FIFO's signed 8-bit range: one LSB per sample, so any larger jump between two
/// output samples is a seam artefact rather than the source's own slope.
fn triangle(count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| {
            // Start at zero, so the first sample played out of an empty FIFO does not step onto a
            // full-scale ramp value.
            let phase = (index + 128) % 512;
            let value = if phase < 256 {
                phase as i16 - 128
            } else {
                127 - (phase as i16 - 256)
            };
            value as i8 as u8
        })
        .collect()
}

#[test]
fn a_ramp_crosses_frame_seams_with_no_step_larger_than_its_own() {
    let mut gba = idle();
    gba.set_sample_rate(44_100);
    write_samples(&mut gba, 0x100, &triangle(4096));
    fifo_a_through_dma(&mut gba, EWRAM_BASE + 0x100);
    start_sample_timer(&mut gba, 512);
    let mut audio = vec![0.0f32; 4096];
    let mut stream: Vec<f32> = Vec::new();
    let mut seams = Vec::new();
    for _ in 0..5 {
        gba.run_frame();
        let frames = gba.drain_audio(&mut audio);
        let before = stream.len();
        stream.extend_from_slice(&audio[..frames * 2]);
        seams.push(before.max(2));
    }
    assert!(!stream.is_empty());
    let step = stream
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
    // One signed byte is 1/128 of full scale; the 9-bit output grid is 1/256.
    assert!(
        step <= 0.01,
        "the largest step between two samples is {step}, larger than the ramp's own 1/128"
    );
    // And a seam's step is no wider than the stream's own: the resampler continues.
    for seam in seams {
        if seam + 1 < stream.len() {
            let jump = (stream[seam] - stream[seam - 1]).abs();
            assert!(jump <= 0.01, "the seam at sample {seam} jumps by {jump}");
        }
    }
    assert!(
        stream.iter().any(|s| *s > 0.5),
        "the ramp reached full scale, so the bound was measured on real audio"
    );
}

#[test]
fn the_sound_registers_answer_their_addresses() {
    let mut gba = idle();
    // SOUNDBIAS's post-boot value.
    assert_eq!(gba.bus.read16(IO_BASE + sound::SOUNDBIAS as u32), 0x0200);
    // The FIFOs are write-only.
    gba.bus.sound.write8(FIFO_A, 0x7F);
    assert_eq!(gba.bus.read8(IO_BASE + FIFO_A as u32), 0);
    // SOUNDCNT_X's low bits are live status: nothing is sounding yet.
    assert_eq!(gba.bus.read8(IO_BASE + SOUNDCNT_X as u32) & 0x0F, 0);
    assert_eq!(
        gba.bus.sound.fifo[0].data.len(),
        1,
        "{FIFO_BYTES}-byte FIFO accepted one byte"
    );
}
