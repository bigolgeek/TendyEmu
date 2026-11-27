//! Inertness: the imported core reproduces the source commit's pins (DESIGN §5.6, §8 #7).
//!
//! Each test is one `gb/*` run from `baselines/pins.toml`. It measures every pin the source's probe
//! measured — frame checksums, the audio hash and peak, state length and hash, the bordered
//! picture, the save/load round trip — and compares each with the file. A run whose ROM is not in
//! `TENDY_TEST_ROMS` prints the probe's skip sentence and passes, so a machine without the ROMs
//! still builds green; it does not claim a match it did not measure.

use std::fmt;
use std::path::{Path, PathBuf};

use tendy_gb::system::{SgbScreen, System};
use tendy_gb::{Cartridge, Machine};

/// 48 kHz is what every audio pin is measured at.
const SAMPLE_RATE: u32 = 48_000;

/// The pin file, from this crate.
const PINS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../baselines/pins.toml");

/// FNV-1a 64, the hash every pin uses.
fn fnv1a64(bytes: &[u8]) -> u64 {
    continue_fnv1a64(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continues an FNV-1a 64 over more bytes.
fn continue_fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// FNV-1a 64 over colour words, each as a little-endian `u16`.
fn fnv1a64_words(words: &[u16]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for word in words {
        hash = continue_fnv1a64(hash, &word.to_le_bytes());
    }
    hash
}

/// The audio pin: FNV-1a 64 over drained samples as little-endian `f32`, plus the loudest magnitude.
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
        for sample in samples {
            self.hash = continue_fnv1a64(self.hash, &sample.to_le_bytes());
            let magnitude = sample.abs();
            if magnitude > self.peak {
                self.peak = magnitude;
            }
        }
    }
}

/// The pins one run measured, and the check against the file.
struct Measured {
    run: &'static str,
    pins: Vec<(String, String)>,
}

impl Measured {
    fn new(run: &'static str) -> Measured {
        Measured {
            run,
            pins: Vec::new(),
        }
    }

    fn pin(&mut self, key: impl Into<String>, value: impl fmt::Display) {
        let (key, value) = (key.into(), format!("{value}"));
        println!("[{}] {key} = {value}", self.run);
        self.pins.push((key, value));
    }

    /// Compares every pin with `baselines/pins.toml`: same keys, same values, same count.
    fn check(self) {
        let text = std::fs::read_to_string(PINS).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err} — run the tests from the workspace root",
                Path::new(PINS).display()
            )
        });
        let pins = tendy_core::pin::parse(&text)
            .unwrap_or_else(|err| panic!("{} does not parse: {err}", PINS));
        let expected: Vec<&tendy_core::pin::Pin> =
            pins.iter().filter(|pin| pin.run == self.run).collect();
        assert!(
            !expected.is_empty(),
            "{} holds no pins — the probe measured none",
            self.run
        );
        for pin in &expected {
            let measured = self
                .pins
                .iter()
                .find(|(key, _)| *key == pin.key)
                .unwrap_or_else(|| panic!("{}: measured no {}", self.run, pin.key));
            assert_eq!(
                measured.1, pin.value,
                "{}: {} differs from the pinned source commit",
                self.run, pin.key
            );
        }
        assert_eq!(
            self.pins.len(),
            expected.len(),
            "{}: measured {} pins, the pin file has {}",
            self.run,
            self.pins.len(),
            expected.len()
        );
    }
}

/// `TENDY_TEST_ROMS` when it names a folder; the one skip sentence otherwise.
fn test_roms() -> Option<PathBuf> {
    match std::env::var_os("TENDY_TEST_ROMS") {
        None => {
            println!("# skipped: TENDY_TEST_ROMS is not set (no pins measured)");
            None
        }
        Some(value) => {
            let root = PathBuf::from(value);
            if root.is_dir() {
                Some(root)
            } else {
                println!("# skipped: TENDY_TEST_ROMS is not a folder (no pins measured)");
                None
            }
        }
    }
}

/// A test ROM inside `TENDY_TEST_ROMS`, or the probe's skip line.
fn rom(root: &Path, name: &str) -> Option<PathBuf> {
    let path = root.join(name);
    if path.is_file() {
        Some(path)
    } else {
        println!("# skipped: {name} not in TENDY_TEST_ROMS");
        None
    }
}

/// Reads a file or panics with its path.
fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display()))
}

/// The cartridge at `path` in a DMG-or-CGB machine, as the probe builds it.
fn machine(path: &Path) -> Machine {
    let cart = Cartridge::new(read(path))
        .unwrap_or_else(|why| panic!("{} is not a Game Boy cartridge: {why}", path.display()));
    Machine::new(cart)
}

/// The cartridge at `path` in a Super Game Boy.
fn sgb_machine(path: &Path) -> Machine {
    let cart = Cartridge::new(read(path))
        .unwrap_or_else(|why| panic!("{} is not a Game Boy cartridge: {why}", path.display()));
    Machine::new_sgb(cart)
}

fn drain(machine: &mut dyn System, audio: &mut AudioHash) {
    let mut buffer = [0.0f32; 8192];
    loop {
        let samples = machine.drain_audio(&mut buffer);
        if samples == 0 {
            break;
        }
        audio.feed(&buffer[..samples * 2]);
        if samples < buffer.len() / 2 {
            break;
        }
    }
}

fn run(machine: &mut dyn System, audio: &mut Option<AudioHash>, frames: u64) {
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_mut() {
            drain(machine, audio);
        }
    }
}

fn frame_pin(measured: &mut Measured, machine: &dyn System, at: u64) {
    measured.pin(
        format!("frame@{at}"),
        format!("{:016x}", machine.frame_checksum()),
    );
}

fn state_pins(measured: &mut Measured, machine: &mut dyn System, at: u64) {
    let blob = machine.save_state();
    measured.pin(format!("state@{at}"), format!("{:016x}", fnv1a64(&blob)));
    measured.pin(format!("state_len@{at}"), blob.len());
}

/// Saves at `at`, runs 60 frames, loads, runs 60 again: the two frame checksums must match.
fn roundtrip(measured: &mut Measured, machine: &mut dyn System, at: u64) {
    let blob = machine.save_state();
    run(machine, &mut None, 60);
    let expected = machine.frame_checksum();
    match machine.load_state(&blob) {
        Ok(()) => {
            run(machine, &mut None, 60);
            let value = if machine.frame_checksum() == expected {
                "ok".to_owned()
            } else {
                "mismatch".to_owned()
            };
            measured.pin(format!("roundtrip@{at}"), value);
        }
        Err(why) => measured.pin(format!("roundtrip@{at}"), format!("load refused: {why:?}")),
    }
}

#[test]
fn gb_pins_dmg_acid2() {
    let Some(root) = test_roms() else { return };
    let Some(path) = rom(&root, "dmg-acid2.gb") else {
        return;
    };
    let mut measured = Measured::new("gb/dmg-acid2");
    let mut machine = machine(&path);
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 60);
    frame_pin(&mut measured, &machine, 60);
    run(&mut machine, &mut None, 240);
    frame_pin(&mut measured, &machine, 300);
    run(&mut machine, &mut None, 300);
    frame_pin(&mut measured, &machine, 600);
    roundtrip(&mut measured, &mut machine, 600);
    measured.check();
}

#[test]
fn gb_pins_cpu_instrs() {
    let Some(root) = test_roms() else { return };
    let Some(path) = rom(&root, "cpu_instrs.gb") else {
        return;
    };
    let mut measured = Measured::new("gb/cpu_instrs");
    let mut machine = machine(&path);
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 600);
    frame_pin(&mut measured, &machine, 600);
    run(&mut machine, &mut audio, 2400);
    frame_pin(&mut measured, &machine, 3000);
    let audio = audio.expect("audio was recorded");
    measured.pin("audio", format!("{:016x}", audio.hash));
    measured.pin("audio_peak", format!("{:.6}", audio.peak));
    state_pins(&mut measured, &mut machine, 3000);
    roundtrip(&mut measured, &mut machine, 3000);
    measured.check();
}

#[test]
fn gb_pins_cgb_acid2() {
    let Some(root) = test_roms() else { return };
    let Some(path) = rom(&root, "cgb-acid2.gbc") else {
        return;
    };
    let mut measured = Measured::new("gb/cgb-acid2");
    let mut machine = machine(&path);
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 60);
    frame_pin(&mut measured, &machine, 60);
    run(&mut machine, &mut None, 240);
    frame_pin(&mut measured, &machine, 300);
    run(&mut machine, &mut None, 300);
    frame_pin(&mut measured, &machine, 600);
    state_pins(&mut measured, &mut machine, 600);
    roundtrip(&mut measured, &mut machine, 600);
    measured.check();
}

#[test]
fn gb_pins_sgb() {
    let Some(root) = test_roms() else { return };
    let Some(path) = rom(&root, "dmg-acid2.gb") else {
        return;
    };
    let mut measured = Measured::new("gb/sgb");
    let mut machine = sgb_machine(&path);
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 600);
    // The core's own picture, without the border.
    frame_pin(&mut measured, &machine, 600);
    // The 256x224 bordered picture the front end draws with the border on.
    let mut screen = SgbScreen::new();
    let words = screen
        .compose(&machine)
        .expect("an SGB machine composes a bordered frame");
    measured.pin("frame_border@600", format!("{:016x}", fnv1a64_words(words)));
    state_pins(&mut measured, &mut machine, 600);
    roundtrip(&mut measured, &mut machine, 600);
    measured.check();
}

#[test]
fn gb_pins_sgb_sound() {
    let Some(root) = test_roms() else { return };
    let Some(sgb_cart) = rom(&root, "sgb-cartridge.bin") else {
        return;
    };
    let Some(path) = rom(&root, "cpu_instrs.gb") else {
        return;
    };
    let mut measured = Measured::new("gb/sgb-sound");
    let mut machine = sgb_machine(&path);
    machine.set_sample_rate(SAMPLE_RATE);
    if let Some(why) = machine.set_sgb_sound(read(&sgb_cart)) {
        println!("# skipped: SGB cartridge refused: {why}");
        return;
    }
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 1200);
    let audio = audio.expect("audio was recorded");
    measured.pin("audio@1200", format!("{:016x}", audio.hash));
    measured.pin("audio_peak@1200", format!("{:.6}", audio.peak));
    state_pins(&mut measured, &mut machine, 1200);
    roundtrip(&mut measured, &mut machine, 1200);
    measured.check();
}
