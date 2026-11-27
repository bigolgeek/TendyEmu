//! The inertness pins for the imported SNES core (DESIGN §5.6): every `snes/*` run in
//! `baselines/pins.toml`, taken from the source at `bf3c60b`, must come out byte for byte the same.
//!
//! What the pins do **not** cover, and why they are asserted anyway:
//!
//! - **The DSP hash is idle output.** No ROM in the public `snes-test-roms` suite plays music, so
//!   `audio` and `audio_peak` record the DSP's silence (peak 0.000031). The hash says the APU's
//!   idle path is unchanged, not that the DSP voices are.
//! - **The state blob is a 38-byte scaffold (#69).** It holds the version and three frame
//!   counters, no device state. It is pinned so that a later real serialisation, or an accidental
//!   layout change now, shows up as a diff.
//! - **One ROM is one viewpoint.** `snes/test-rom` drives INIDISP only; the pins say nothing about
//!   DMA/HDMA timing, sprite priority, Mode 7, SRAM or any coprocessor's emulation. The moved test
//!   files (`dma`, `layers`, `sprites`, `map`, …) are that coverage; the pins guard the whole
//!   machine's output, not each unit.
//!
//! A run whose ROM is not in `TENDY_TEST_ROMS` prints the skip sentence and passes, as §5.6 says;
//! a run with the ROM present is checked exactly.

use std::path::PathBuf;

use tendy_core::fnv1a64;
use tendy_core::pin::{Pin, parse};
use tendy_snes::System as _;
use tendy_snes::cart::Cartridge;
use tendy_snes::machine::Machine;

const TEST_ROM: &str = "snes-test-roms/inidisp_hammer_0f.sfc";
const SAMPLE_RATE: u32 = 48_000;

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

/// The test ROM's bytes, or the §5.6 skip line.
fn test_rom() -> Option<Vec<u8>> {
    let Some(root) = std::env::var_os("TENDY_TEST_ROMS") else {
        println!("skipped: TENDY_TEST_ROMS is not set");
        return None;
    };
    let root = PathBuf::from(root);
    if !root.is_dir() {
        println!("skipped: TENDY_TEST_ROMS is not a folder");
        return None;
    }
    let path = root.join(TEST_ROM);
    if !path.is_file() {
        println!("skipped: {TEST_ROM} not in TENDY_TEST_ROMS");
        return None;
    }
    Some(std::fs::read(&path).unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display())))
}

/// FNV-1a 64 over drained samples as little-endian `f32`, in order; the loudest magnitude beside it.
#[derive(Default)]
struct AudioHash {
    hash: u64,
    peak: f32,
}

impl AudioHash {
    fn new() -> AudioHash {
        AudioHash {
            hash: 0xcbf2_9ce4_8422_2325,
            peak: 0.0,
        }
    }

    fn feed(&mut self, samples: &[f32]) {
        for &sample in samples {
            let bytes = sample.to_le_bytes();
            for byte in bytes {
                self.hash ^= u64::from(byte);
                self.hash = self.hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            let magnitude = sample.abs();
            if magnitude > self.peak {
                self.peak = magnitude;
            }
        }
    }
}

/// Drains everything the machine queued, the way the pin probe did: 4096 stereo frames a call.
fn drain(machine: &mut Machine, audio: &mut AudioHash) {
    let mut buffer = [0.0f32; 8192];
    loop {
        let frames = machine.drain_audio(&mut buffer);
        if frames == 0 {
            break;
        }
        audio.feed(&buffer[..frames * 2]);
        if frames < buffer.len() / 2 {
            break;
        }
    }
}

fn advance(machine: &mut Machine, frames: u64, mut audio: Option<&mut AudioHash>) {
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_deref_mut() {
            drain(machine, audio);
        }
    }
}

fn hex(hash: u64) -> String {
    format!("{hash:016x}")
}

fn machine(bytes: Vec<u8>) -> Machine {
    let cart = Cartridge::new(bytes)
        .unwrap_or_else(|why| panic!("the pin image is not a cartridge this core accepts: {why}"));
    Machine::new(cart)
}

/// Every `snes/*` run in the file is one of the two this test checks; a new run cannot slip by
/// unasserted.
#[test]
fn snes_pins_cover_every_run() {
    let pins = all_pins();
    let mut runs: Vec<&str> = pins
        .iter()
        .filter(|pin| pin.run.starts_with("snes/"))
        .map(|pin| pin.run.as_str())
        .collect();
    runs.sort_unstable();
    runs.dedup();
    assert_eq!(runs, ["snes/refusals", "snes/test-rom"]);
}

/// `snes/test-rom`: the frame checksums, the DSP hash and the 38-byte state blob at frame 3000.
#[test]
fn snes_pins_test_rom() {
    let Some(bytes) = test_rom() else { return };
    let pins = all_pins();
    let mut machine = machine(bytes);
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();

    advance(&mut machine, 60, Some(&mut audio));
    assert_eq!(
        hex(machine.frame_checksum()),
        pinned(&pins, "snes/test-rom", "frame@60")
    );
    advance(&mut machine, 540, Some(&mut audio));
    assert_eq!(
        hex(machine.frame_checksum()),
        pinned(&pins, "snes/test-rom", "frame@600")
    );
    advance(&mut machine, 2400, Some(&mut audio));
    assert_eq!(
        hex(machine.frame_checksum()),
        pinned(&pins, "snes/test-rom", "frame@3000")
    );
    assert_eq!(
        hex(audio.hash),
        pinned(&pins, "snes/test-rom", "audio"),
        "the DSP's idle output changed"
    );
    assert_eq!(
        format!("{:.6}", audio.peak),
        pinned(&pins, "snes/test-rom", "audio_peak")
    );

    let blob = machine.save_state();
    assert_eq!(
        hex(fnv1a64(&blob)),
        pinned(&pins, "snes/test-rom", "state@3000")
    );
    assert_eq!(
        blob.len().to_string(),
        pinned(&pins, "snes/test-rom", "state_len@3000"),
        "the state blob is the 38-byte scaffold (#69)"
    );
}

/// `snes/refusals`: the exact refusal sentence, or `ok`, for every `$FFD6` value.
#[test]
fn snes_pins_refusals() {
    let pins = all_pins();
    for byte in 0u8..=255 {
        let value = match Cartridge::new(image(byte)) {
            Ok(_) => "ok".to_owned(),
            Err(refusal) => refusal.to_string(),
        };
        assert_eq!(
            value,
            pinned(&pins, "snes/refusals", &format!("ffd6_{byte:02x}")),
            "$FFD6 = ${byte:02X} changed its answer"
        );
    }
}

/// `roundtrip@3000`: loading the saved state and running 60 frames gives the checksum the run would
/// have reached without it.
#[test]
fn snes_pins_roundtrip() {
    let Some(bytes) = test_rom() else { return };
    let pins = all_pins();
    let mut machine = machine(bytes);
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut machine, 3000, Some(&mut audio));

    let blob = machine.save_state();
    advance(&mut machine, 60, None);
    let expected = machine.frame_checksum();
    machine
        .load_state(&blob)
        .expect("the pin's own state loads");
    advance(&mut machine, 60, None);
    let value = if machine.frame_checksum() == expected {
        "ok"
    } else {
        "mismatch"
    };
    assert_eq!(
        value,
        pinned(&pins, "snes/test-rom", "roundtrip@3000"),
        "the save/load roundtrip no longer reproduces the run"
    );
}

/// DESIGN C7: the boundary frame is 256x224 RGBA8888, and that is frozen.
#[test]
fn snes_frame_is_256x224() {
    use tendy_core::{Frame, Machine as _};
    use tendy_snes::SnesMachine;

    let mut wrapper = SnesMachine::new(image(0)).expect("the synthetic LoROM loads");
    wrapper.run_frame();
    let frame: Frame<'_> = wrapper.frame();
    assert_eq!((frame.width, frame.height), (256, 224));
    assert_eq!(frame.rgba.len(), 256 * 224 * 4);
    assert!(frame.is_complete());
    assert_eq!(
        frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] != 0xFF)
            .count(),
        0,
        "every pixel is opaque"
    );
}

/// A 32 KiB LoROM with a valid header, the `$FFD6` byte set to `chipset` — the pin probe's image,
/// copied byte for byte so the refusal run compares like for like.
fn image(chipset: u8) -> Vec<u8> {
    const AT: usize = 0x7FC0;
    let mut rom = vec![0xEAu8; 0x8000];
    rom[AT..AT + 0x15].fill(b' ');
    rom[AT..AT + 9].copy_from_slice(b"TENDY-PIN");
    rom[AT + 0x15] = 0x30; // LoROM, fast
    rom[AT + 0x16] = chipset;
    rom[AT + 0x17] = 0x05; // 32 KiB
    rom[AT + 0x18] = 0x00;
    rom[AT + 0x19] = 0x01; // USA, NTSC
    rom[AT - 1] = 0; // $FFBF subclass byte
    for vector in [0x2A, 0x2E, 0x3C] {
        rom[AT + vector..AT + vector + 2].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    rom[AT + 0x1C..AT + 0x20].fill(0);
    let sum = rom
        .iter()
        .fold(0u16, |sum, &byte| sum.wrapping_add(u16::from(byte)));
    rom[AT + 0x1E..AT + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[AT + 0x1C..AT + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom
}
