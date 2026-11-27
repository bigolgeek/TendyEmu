//! The synthetic sound pin for the SGB host (DESIGN §4.3, §5.6).
//!
//! `[gb/sgb-sound]` is skipped until the user supplies `sgb-cartridge.bin`, so the SGB sound path
//! has no sound baseline at all. This test needs no cartridge: it loads a hand-written SPC700
//! program and its BRR block through [`SgbSound::load_block`] — the direct copy `SOU_TRN` makes —
//! starts the chip at it, and pins one second of the DSP's output.
//!
//! The pins are `[spc/sgb-synthetic]` in `baselines/pins.toml`, measured through `tools/pin`:
//! `tendy-pin spc-sgb --source <gb checkout>`. The image is byte-for-byte the one
//! `crates/tendy-snes/tests/synthetic.rs` uploads through the IPL handshake, so the two hosts run
//! the same program; only the entry point and the clock differ.

use std::path::PathBuf;

use tendy_core::fnv1a64;
use tendy_core::pin::{Pin, parse};
use tendy_gb::sgb::sound::{SGB_CLOCK_HZ, SgbSound};
use tendy_gb::state::StateWriter;

/// Where the image starts in APU RAM.
const SPC_PROGRAM: u16 = 0x0200;
/// Where the image's sample directory page sits; `DIR` is a page number, so this is `$0300`.
const SPC_DIR: u16 = 0x0300;
/// Where the image's one BRR block sits; instrument 0 starts and loops here.
const SPC_BRR: u16 = 0x0310;

/// The run: one second of Game Boy dots in 32 slices, each about 1 000 DSP samples — well inside
/// the chip's 2 048-sample staging buffer, so nothing is dropped.
const SLICES: u32 = 32;
const SLICE_DOTS: u32 = SGB_CLOCK_HZ / SLICES;

/// Every pin in `baselines/pins.toml`.
fn all_pins() -> Vec<Pin> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../baselines/pins.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display()));
    parse(&text).unwrap_or_else(|why| panic!("{}: {why}", path.display()))
}

/// The value recorded for one run and key.
fn pinned(pins: &[Pin], run: &str, key: &str) -> String {
    pins.iter()
        .find(|pin| pin.run == run && pin.key == key)
        .unwrap_or_else(|| panic!("baselines/pins.toml has no [{run}] {key}"))
        .value
        .clone()
}

/// The synthetic SPC700 program and its data, as one image loaded at [`SPC_PROGRAM`]: 26 DSP
/// register writes that give voice 0 a looping BRR square wave and arm echo with a non-zero FIR,
/// then `BRA *`. Identical to `tools/pin/probes/common.rs`'s `spc_image`.
fn spc_image() -> Vec<u8> {
    const WRITES: [(u8, u8); 26] = [
        (0x5D, 0x03), // DIR: directory page $0300
        (0x0C, 0x50), // MVOLL
        (0x1C, 0x50), // MVOLR
        (0x00, 0x50), // V0VOLL
        (0x01, 0x50), // V0VOLR
        (0x02, 0x00), // V0PITCHL
        (0x03, 0x10), // V0PITCHH: $1000, 32 000 Hz
        (0x04, 0x00), // V0SRCN: instrument 0
        (0x05, 0x00), // V0ADSR1: 0 selects GAIN mode
        (0x07, 0x7F), // V0GAIN: direct, maximum
        (0x0F, 0x40), // FIR tap 0 = 64
        (0x1F, 0x20), // FIR tap 1 = 32
        (0x2F, 0x10), // FIR tap 2 = 16
        (0x3F, 0x08), // FIR tap 3 = 8
        (0x4F, 0x00), // FIR tap 4
        (0x5F, 0x00), // FIR tap 5
        (0x6F, 0x00), // FIR tap 6
        (0x7F, 0x00), // FIR tap 7
        (0x2C, 0x30), // EVOLL
        (0x3C, 0x30), // EVOLR
        (0x0D, 0x20), // EFB
        (0x6D, 0x30), // ESA: echo buffer at $3000
        (0x7D, 0x01), // EDL: 512 entries (16 ms)
        (0x4D, 0x01), // EON: voice 0 feeds the echo
        (0x6C, 0x00), // FLG: reset, mute and echo-write-disable all clear
        (0x4C, 0x01), // KON: voice 0 on
    ];
    let len = usize::from(SPC_BRR + 9 - SPC_PROGRAM);
    let mut image = vec![0u8; len];
    let mut at = 0;
    for (reg, value) in WRITES {
        image[at..at + 3].copy_from_slice(&[0x8F, reg, 0xF2]);
        image[at + 3..at + 6].copy_from_slice(&[0x8F, value, 0xF3]);
        at += 6;
    }
    image[at..at + 2].copy_from_slice(&[0x2F, 0xFE]);
    let dir = usize::from(SPC_DIR - SPC_PROGRAM);
    image[dir..dir + 4].copy_from_slice(&[
        SPC_BRR as u8,
        (SPC_BRR >> 8) as u8,
        SPC_BRR as u8,
        (SPC_BRR >> 8) as u8,
    ]);
    let brr = usize::from(SPC_BRR - SPC_PROGRAM);
    // Shift 12, filter 0, end+loop (code 3), then a square of +7 and -7 nibbles.
    image[brr] = (12 << 4) | 3;
    image[brr + 1..brr + 9].copy_from_slice(&[0x77, 0x77, 0x99, 0x99, 0x77, 0x77, 0x99, 0x99]);
    image
}

#[test]
fn sgb_synthetic_audio_and_state_match_the_pin() {
    let pins = all_pins();
    let mut sound = SgbSound::new();
    sound.load_block(SPC_PROGRAM, &spc_image());
    sound.start(SPC_PROGRAM);

    let at = "the synthetic program, one second after the transfer";
    let mut samples: Vec<f32> = Vec::new();
    let mut buffer = [0.0f32; 8192];
    for _ in 0..SLICES {
        sound.tick(SLICE_DOTS);
        loop {
            let frames = sound.read_samples(&mut buffer);
            if frames == 0 {
                break;
            }
            samples.extend_from_slice(&buffer[..frames * 2]);
            if frames < buffer.len() / 2 {
                break;
            }
        }
    }
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));

    assert_eq!(
        format!("{:016x}", fnv1a64(&bytes)),
        pinned(&pins, "spc/sgb-synthetic", "audio"),
        "audio hash, {at}"
    );
    assert_eq!(
        format!("{peak:.6}"),
        pinned(&pins, "spc/sgb-synthetic", "audio_peak"),
        "peak, {at}"
    );
    assert!(
        peak > 0.1,
        "the pin must be real sound, not the DSP's silence (peak {peak:.6})"
    );
    assert_eq!(
        (samples.len() / 2).to_string(),
        pinned(&pins, "spc/sgb-synthetic", "samples"),
        "sample count, {at}"
    );

    // The chip's own state, not `SgbSound`'s: that is what the pin measures.
    let mut writer = StateWriter::new();
    sound.chip().save_state(&mut writer);
    let blob = writer.finish();
    assert_eq!(
        blob.len().to_string(),
        pinned(&pins, "spc/sgb-synthetic", "apu_state_len"),
        "state length, {at}"
    );
    assert_eq!(
        format!("{:016x}", fnv1a64(&blob)),
        pinned(&pins, "spc/sgb-synthetic", "apu_state"),
        "state hash, {at}"
    );
}
