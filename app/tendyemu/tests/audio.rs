//! Sound, end to end and headless (DESIGN §8; issue #19).
//!
//! The three tests here drive the same [`Mixer`] the window's stream drains: one per core, with a
//! simulated card that asks for fixed blocks on its own clock. The console images are built in
//! code, so no test needs a ROM library. What no headless test can do is hear the result or hold
//! the cpal stream (a stream is not `Send`); `switch_keeps_stream_open` therefore proves the
//! property the stream depends on — the queue and mixer outlive the machine, and only the
//! converter is rebuilt for a new rate.

use std::f64::consts::PI;

use tendy_core::{Machine, SystemKind};
use tendyemu::audio::{Converter, Mixer, SampleRing, volume_gain};

/// The rate every test card runs at.
const DEVICE: u32 = 48_000;
/// One callback's block, in stereo frames: about 10 ms, the period WASAPI and ALSA ask for.
const BLOCK: usize = 480;

// --------------------------------------------------------------------------------- the images

/// A 32 KiB Game Boy cartridge with a valid header and a `ret`-loop that never touches the APU.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"AUDIOGB ");
    rom[0x147] = 0x00;
    rom[0x148] = 0x00;
    rom[0x149] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A 512-byte Game Boy Advance cartridge: the header a cartridge is, and a `b` to itself at the
/// entry point, so the CPU stays put.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-AUDIO");
    rom[0xAC..0xB0].copy_from_slice(b"TST1");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A minimal NROM image: 16 KiB PRG and 8 KiB CHR, `JMP $8000` at the reset vector, so the CPU
/// spins on the spot and the APU is silent but still clocked.
fn nes_rom() -> Vec<u8> {
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.resize(16 + 16_384 + 8_192, 0);
    rom[16] = 0x4C;
    rom[17] = 0x00;
    rom[18] = 0x80;
    rom[16 + 0x3FFC] = 0x00;
    rom[16 + 0x3FFD] = 0x80;
    rom
}

/// A 32 KiB LoROM image with a valid header, the vectors pointing at `$8000`, where the CPU runs
/// `jmp $8000` forever.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0xEAu8; 0x8000];
    rom[0..3].copy_from_slice(&[0x4C, 0x00, 0x80]);
    let at = 0x7FC0;
    rom[at..at + 0x15].fill(b' ');
    rom[at..at + 8].copy_from_slice(b"SNES-AUD");
    rom[at + 0x15] = 0x20; // LoROM, slow
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x08; // 256 KiB, the smallest a header may claim
    rom[at + 0x18] = 0x00;
    rom[at + 0x19] = 0x01; // USA, NTSC
    rom[at - 1] = 0;
    for vector in [0x2A, 0x2E, 0x3C] {
        rom[at + vector..at + vector + 2].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    rom[at + 0x1C..at + 0x20].fill(0);
    let mut sum = 0u32;
    for &byte in &rom {
        sum = sum.wrapping_add(u32::from(byte));
    }
    let sum = (sum & 0xFFFF) as u16;
    rom[at + 0x1E..at + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[at + 0x1C..at + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom
}

/// The four cores, built from the images above. A refusal is a test failure: these are the
/// smallest images each constructor accepts, and they are ours.
fn machines() -> Vec<(&'static str, Box<dyn Machine>)> {
    let gb = tendy_gb::GbMachine::new(gb_rom(), tendy_gb::GbOptions::default())
        .expect("the Game Boy accepts a 32 KiB cart");
    let gba = tendy_gba::GbaMachine::new(gba_rom(), None).expect("the Advance accepts a header");
    let nes = tendy_nes::NesMachine::new(
        nes_rom(),
        tendy_nes::NesImage::Cartridge,
        None,
        tendy_nes::console::ConsoleConfig::default(),
    )
    .expect("the NES accepts an NROM image");
    let snes = tendy_snes::SnesMachine::new(snes_rom()).expect("the SNES accepts a LoROM image");
    vec![
        ("gb", Box::new(gb)),
        ("gba", Box::new(gba)),
        ("nes", Box::new(nes)),
        ("snes", Box::new(snes)),
    ]
}

// --------------------------------------------------------------------------------- the tests

/// Each core's own rate reaches the card at the card's rate: the queue grows at 48 kHz whatever
/// the machine produces, for all four systems.
#[test]
fn each_core_rate_converted_to_device() {
    for (name, mut machine) in machines() {
        // The rate each core keeps when nothing tells it otherwise: what the mixer has to bridge.
        let core_hz = machine.sample_rate();
        assert!(core_hz > 0, "{name}: no rate");

        let mut mixer = Mixer::new(DEVICE);
        let frames = machine.frame_rate().round() as usize;
        let mut pushed = 0;
        for _ in 0..frames {
            machine.run_frame();
            pushed += mixer.push(machine.as_mut(), 1.0);
        }
        // The mixer follows what the core really produces, measured over the first frames.
        let source = mixer.source_hz();
        assert!(
            source.abs_diff(core_hz) < core_hz / 100,
            "{name}: the converter is at {source} Hz, the core at {core_hz} Hz"
        );
        assert_eq!(mixer.device_hz(), DEVICE);
        assert_eq!(
            mixer.ring().len(),
            pushed.min(tendyemu::audio::RING_CAPACITY),
            "{name}: the queue holds what the mixer converted, up to its capacity"
        );
        let expected = f64::from(DEVICE) * frames as f64 / machine.frame_rate();
        assert!(
            (pushed as f64 - expected).abs() < expected / 20.0,
            "{name}: {pushed} frames queued for one emulated second, about {expected}"
        );
    }
}

/// A card asking for 10 ms blocks every 10 ms is never left without frames, for all four
/// systems, once the queue has its prefill.
#[test]
fn no_underrun_at_normal_speed_each_system() {
    for (name, mut machine) in machines() {
        machine.set_sample_rate(DEVICE);
        let mut mixer = Mixer::new(DEVICE);
        let mut block = vec![0.0f32; BLOCK * 2];
        let mut due = 0.0f64;
        let mut underruns = 0;
        // The window's own rule: the card starts once the queue holds the prefill.
        let mut playing = false;
        for _ in 0..120 {
            machine.run_frame();
            mixer.push(machine.as_mut(), 1.0);
            if mixer.ring().len() >= tendyemu::audio::PREFILL_FRAMES {
                playing = true;
                break;
            }
        }
        assert!(playing, "{name}: the prefill never filled");

        // Five seconds of playback at 1x: not one callback may come up short.
        for _ in 0..(5 * 60) {
            machine.run_frame();
            mixer.push(machine.as_mut(), 1.0);
            due += f64::from(DEVICE) / machine.frame_rate();
            while due >= BLOCK as f64 {
                due -= BLOCK as f64;
                let got = mixer.ring().pop_or_zero(&mut block) / 2;
                if got < BLOCK {
                    underruns += 1;
                }
            }
        }
        assert_eq!(underruns, 0, "{name}: {underruns} starved callbacks");
    }
}

/// A card whose crystal runs 0.1 % fast does not drain the queue: the fill controller stretches
/// the source until the two clocks agree, which is the difference between a working emulator and
/// one that clicks after ten minutes.
#[test]
fn a_card_a_fraction_fast_never_runs_the_queue_dry() {
    let card_hz = f64::from(DEVICE) * 1.001;
    let (_, mut machine) = machines().remove(0);
    machine.set_sample_rate(DEVICE);
    let mut mixer = Mixer::new(DEVICE);
    while mixer.ring().len() < tendyemu::audio::PREFILL_FRAMES {
        machine.run_frame();
        mixer.push(machine.as_mut(), 1.0);
    }

    let mut block = vec![0.0f32; BLOCK * 2];
    let mut due = 0.0f64;
    let mut underruns = 0;
    let mut produced = 0usize;
    for _ in 0..(60 * 60) {
        machine.run_frame();
        produced += mixer.push(machine.as_mut(), 1.0);
        due += card_hz / machine.frame_rate();
        while due >= BLOCK as f64 {
            due -= BLOCK as f64;
            if mixer.ring().pop_or_zero(&mut block) / 2 < BLOCK {
                underruns += 1;
            }
        }
    }
    assert_eq!(underruns, 0, "{underruns} starved callbacks in a minute");
    let trim = mixer.trim();
    let fill = mixer.ring().len();
    assert!(
        (tendyemu::audio::PREFILL_FRAMES / 2..=tendyemu::audio::RING_CAPACITY).contains(&fill),
        "the queue settled at {fill} frames (trim {trim}, {produced} produced)"
    );
}

/// A system switch replaces the machine, not the sound: the frames queued before the switch are
/// still played after it, and only the converter is rebuilt when the new core runs at another
/// rate. The stream itself is module state that never hears about `App::open` (§6.4).
#[test]
fn switch_keeps_stream_open() {
    let mut mixer = Mixer::new(DEVICE);
    let (_, mut gb) = machines().remove(0);
    gb.set_sample_rate(DEVICE);
    for _ in 0..5 {
        gb.run_frame();
        mixer.push(gb.as_mut(), 1.0);
    }
    let queued = mixer.ring().len();
    assert!(queued >= BLOCK, "the first core queued {queued} frames");
    assert_eq!(mixer.retunes(), 0);

    // The switch: the shell drops the Game Boy and runs the NES at its own 32 kHz.
    let (_, mut nes) = machines().remove(2);
    nes.set_sample_rate(32_000);
    nes.run_frame();
    mixer.push(nes.as_mut(), 1.0);
    assert_eq!(
        mixer.source_hz(),
        32_000,
        "the converter follows the new core"
    );
    assert_eq!(mixer.retunes(), 1, "one rebuild, on the rate change");
    assert!(
        mixer.ring().len() > queued,
        "the queue kept the old frames and the new ones"
    );

    // And the new core's frames keep coming out at the card's rate.
    let mut block = vec![0.0f32; BLOCK * 2];
    let got = mixer.ring().pop_or_zero(&mut block) / 2;
    assert_eq!(got, BLOCK, "the stream plays straight through the switch");
}

/// A tone through every core rate keeps its pitch and its level at the card's rate.
#[test]
fn a_tone_survives_every_core_rate() {
    for from in [32_000, 32_768, 44_100, 48_000] {
        let mut converter = Converter::new(from, DEVICE);
        let frames = 2 * from as usize;
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let value = (2.0 * PI * 1_000.0 * n as f64 / f64::from(from)).sin() as f32;
            input.push(value);
            input.push(value);
        }
        let mut out = Vec::new();
        converter.convert(&input, &mut out);
        let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|pair| pair[0]).collect();
        let crossings = left
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let hz = crossings as f64 * f64::from(DEVICE) / left.len() as f64;
        assert!((hz - 1_000.0).abs() < 10.0, "{from} Hz: {hz} Hz out");
        let peak = left.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.05, "{from} Hz: peak {peak}");
    }
}

/// The ring is the boundary between the producer and the card, and it is a stereo queue: a fresh
/// ring plays silence, never a stale chunk.
#[test]
fn a_starved_ring_plays_silence() {
    let ring = SampleRing::new();
    let mut block = [1.0f32; 8];
    assert_eq!(ring.pop_or_zero(&mut block), 0);
    assert_eq!(block, [0.0; 8]);
    ring.push_stereo(&[0.25, -0.25]);
    assert_eq!(ring.len(), 1);
    assert_eq!(ring.pop_or_zero(&mut block), 2);
    assert_eq!(block[..2], [0.25, -0.25]);
    assert_eq!(block[2..], [0.0; 6]);
}

/// The volume the mixer scales by is the `[app]` slider's square, and mute is silence.
#[test]
fn the_volume_is_a_gain_the_mixer_applies() {
    assert_eq!(volume_gain(100), 1.0);
    assert!(volume_gain(50) < 0.3 && volume_gain(50) > 0.2);
    assert_eq!(volume_gain(0), 0.0);
}

/// A system's own name still comes from the core the mixer was fed (a smoke check that these
/// images really are the four systems, not that any of them made a sound).
#[test]
fn the_images_are_the_four_systems() {
    let kinds: Vec<SystemKind> = machines()
        .iter()
        .map(|(_, machine)| machine.kind())
        .collect();
    assert_eq!(
        kinds,
        vec![
            SystemKind::Gb,
            SystemKind::Gba,
            SystemKind::Nes,
            SystemKind::Snes
        ]
    );
}
