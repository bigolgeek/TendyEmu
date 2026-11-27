//! Inertness: the imported core reproduces the source commit's pins (DESIGN §5.6, §8 #8).
//!
//! `gba_pins_minimal` measures every frame, audio and state pin of the `gba/minimal` run in
//! `baselines/pins.toml`; `gba_pins_roundtrip` measures the run's save/load round trip. Together
//! they compare every pin of every `gba/*` run. The cartridge is built in code by this crate's
//! tests, so there is no ROM to find and no skip path.

mod common;

use std::fmt;
use std::path::Path;

use tendy_core::pin::{self, Pin};
use tendy_gb::System;
use tendy_gba::Gba;
use tendy_gba::cart::Cartridge;

/// 48 kHz is what every audio pin is measured at.
const SAMPLE_RATE: u32 = 48_000;

/// The one run the source's probe measured.
const RUN: &str = "gba/minimal";

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

/// The pins one test measured.
struct Measured {
    pins: Vec<(String, String)>,
}

impl Measured {
    fn new() -> Measured {
        Measured { pins: Vec::new() }
    }

    fn pin(&mut self, key: impl Into<String>, value: impl fmt::Display) {
        let (key, value) = (key.into(), format!("{value}"));
        println!("[{RUN}] {key} = {value}");
        self.pins.push((key, value));
    }

    /// Compares every measured pin with `baselines/pins.toml`, and checks that the pins selected
    /// by `only` are exactly the ones measured — no pin silently skipped, none invented.
    fn check(&self, only: fn(&str) -> bool) {
        let expected: Vec<Pin> = expected_pins()
            .into_iter()
            .filter(|p| only(&p.key))
            .collect();
        assert!(
            !expected.is_empty(),
            "no {RUN} pins selected — the filter is wrong"
        );
        for (key, value) in &self.pins {
            let want = expected
                .iter()
                .find(|pin| &pin.key == key)
                .unwrap_or_else(|| panic!("{RUN}: measured {key}, which is not a pinned key"));
            assert_eq!(
                value, &want.value,
                "{RUN}: {key} differs from the pinned source commit"
            );
        }
        assert_eq!(
            self.pins.len(),
            expected.len(),
            "{RUN}: measured {} pins, the selection has {}",
            self.pins.len(),
            expected.len()
        );
    }
}

/// The pins in `baselines/pins.toml` for [`RUN`].
fn expected_pins() -> Vec<Pin> {
    let text = std::fs::read_to_string(PINS).unwrap_or_else(|err| {
        panic!(
            "cannot read {}: {err} — run the tests from the workspace root",
            Path::new(PINS).display()
        )
    });
    let pins = pin::parse(&text).unwrap_or_else(|err| panic!("{PINS} does not parse: {err}"));
    let selected: Vec<Pin> = pins.into_iter().filter(|pin| pin.run == RUN).collect();
    assert!(
        !selected.is_empty(),
        "{} holds no {RUN} pins — the probe measured none",
        PINS
    );
    selected
}

/// The `gba/minimal` cartridge, as the source's probe built it.
fn machine() -> Gba {
    let cart = Cartridge::new(common::mode3_image())
        .expect("the pin image is a Game Boy Advance cartridge");
    Gba::new(cart)
}

fn drain(machine: &mut Gba, audio: &mut AudioHash) {
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

fn run(machine: &mut Gba, audio: &mut Option<AudioHash>, frames: u64) {
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_mut() {
            drain(machine, audio);
        }
    }
}

/// Saves at `at`, runs 60 frames, loads, runs 60 again: the two frame checksums must match.
fn roundtrip(measured: &mut Measured, machine: &mut Gba, at: u64) {
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

/// Every pin but the round trip.
fn not_roundtrip(key: &str) -> bool {
    !key.starts_with("roundtrip@")
}

/// The round trip.
fn roundtrip_only(key: &str) -> bool {
    key.starts_with("roundtrip@")
}

#[test]
fn gba_pins_minimal() {
    let mut measured = Measured::new();
    let mut machine = machine();
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 60);
    measured.pin("frame@60", format!("{:016x}", machine.frame_checksum()));
    run(&mut machine, &mut audio, 540);
    measured.pin("frame@600", format!("{:016x}", machine.frame_checksum()));
    let audio = audio.expect("audio was recorded");
    measured.pin("audio", format!("{:016x}", audio.hash));
    measured.pin("audio_peak", format!("{:.6}", audio.peak));
    let blob = machine.save_state();
    measured.pin("state@600", format!("{:016x}", fnv1a64(&blob)));
    measured.pin("state_len@600", blob.len());
    measured.check(not_roundtrip);
}

#[test]
fn gba_pins_roundtrip() {
    let mut measured = Measured::new();
    let mut machine = machine();
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 600);
    roundtrip(&mut measured, &mut machine, 600);
    measured.check(roundtrip_only);
}
