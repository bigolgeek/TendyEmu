//! The synthetic sound pin for the SNES host (issue #6, DESIGN §4.3, §5.6).
//!
//! `[snes/test-rom]`'s audio hash is the DSP's idle output (peak 0.000031), because no public
//! `snes-test-roms` image plays music. It proves the APU's idle path runs, not that the voices and
//! the echo do. This test is the proof the shared crate needs: a hand-written SPC700 program is
//! uploaded through the IPL handshake over the four ports — the sequence a cartridge's boot code
//! runs — and one second of the DSP's output is pinned.
//!
//! The pins are `[spc/snes-synthetic]` in `baselines/pins.toml`, measured through `tools/pin`:
//! `tendy-pin spc-snes --source <snes checkout>`. The program and its expected values are identical
//! to `crates/tendy-gb/tests/synthetic.rs`, which runs the same image through the SGB sound path.

use std::path::PathBuf;

use tendy_core::fnv1a64;
use tendy_core::pin::{Pin, parse};
use tendy_snes::MASTER_CLOCK_HZ;
use tendy_snes::apu::{Apu, Host};
use tendy_snes::state::StateWriter;
/// Where the image starts in APU RAM.
const SPC_PROGRAM: u16 = 0x0200;
/// Where the image's sample directory page sits; `DIR` is a page number, so this is `$0300`.
const SPC_DIR: u16 = 0x0300;
/// Where the image's one BRR block sits; instrument 0 starts and loops here.
const SPC_BRR: u16 = 0x0310;

/// The run: one second of master clocks in 32 slices, each about 640 DSP samples — well inside the
/// APU's 2 048-sample staging buffer, so nothing is dropped.
const SLICES: u32 = 32;
const SLICE_TICKS: u32 = MASTER_CLOCK_HZ / SLICES;

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

/// One command: the destination in `$2142`/`$2143`, `$2141` = 01 (transfer), then the kick.
fn kick(apu: &mut Apu, destination: u16, kick_byte: u8) {
    apu.write_port(2, destination as u8);
    apu.write_port(3, (destination >> 8) as u8);
    apu.write_port(1, 0x01);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
}

/// Polls `$2140` the way an uploader does, handing the SPC clocks while it waits: the boot ROM
/// reads the ports from software, so a write is only seen once the SPC runs.
fn await_echo(apu: &mut Apu, want: u8) {
    for _ in 0..200_000 {
        if apu.read_port(0) == want {
            return;
        }
        apu.tick(64);
    }
    panic!(
        "the boot ROM never echoed ${want:02X}; $2140 holds ${:02X}",
        apu.read_port(0)
    );
}

/// The image into APU RAM through the IPL handshake, then the entry command.
fn upload(apu: &mut Apu) {
    let image = spc_image();
    kick(apu, SPC_PROGRAM, 0xCC);
    for (index, byte) in image.iter().enumerate() {
        apu.write_port(1, *byte);
        apu.write_port(0, index as u8);
        await_echo(apu, index as u8);
    }
    // The entry command: `$2141` = 0, the address in `$2142`/`$2143`, then a kick the transfer
    // loop reads as "past the last index".
    let kick_byte = ((image.len().wrapping_add(1) & 0xFF) | 1) as u8;
    apu.write_port(2, SPC_PROGRAM as u8);
    apu.write_port(3, (SPC_PROGRAM >> 8) as u8);
    apu.write_port(1, 0x00);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
    assert_eq!(
        apu.cpu.pc, SPC_PROGRAM,
        "the entry command must leave the SPC700 at the program"
    );
}

#[test]
fn snes_synthetic_audio_and_state_match_the_pin() {
    let pins = all_pins();
    let mut apu = Apu::new(Host::Snes);
    upload(&mut apu);

    let at = "the synthetic program, one second after the upload";
    let mut samples: Vec<f32> = Vec::new();
    let mut buffer = [0.0f32; 8192];
    for _ in 0..SLICES {
        apu.tick(SLICE_TICKS);
        loop {
            let frames = apu.read_samples(&mut buffer);
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
        pinned(&pins, "spc/snes-synthetic", "audio"),
        "audio hash, {at}"
    );
    assert_eq!(
        format!("{peak:.6}"),
        pinned(&pins, "spc/snes-synthetic", "audio_peak"),
        "peak, {at}"
    );
    assert!(
        peak > 0.1,
        "the pin must be real sound, not the DSP's silence (peak {peak:.6})"
    );
    assert_eq!(
        (samples.len() / 2).to_string(),
        pinned(&pins, "spc/snes-synthetic", "samples"),
        "sample count, {at}"
    );

    let mut writer = StateWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();
    assert_eq!(
        blob.len().to_string(),
        pinned(&pins, "spc/snes-synthetic", "apu_state_len"),
        "state length, {at}"
    );
    assert_eq!(
        format!("{:016x}", fnv1a64(&blob)),
        pinned(&pins, "spc/snes-synthetic", "apu_state"),
        "state hash, {at}"
    );
}
