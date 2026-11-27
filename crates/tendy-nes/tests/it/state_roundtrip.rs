//! Save-state completeness guard: state saved from a running machine must reload into a fresh
//! object and compare equal, including the `Debug` rendering (so a field missing from
//! `save_state` shows up as a diff).

use tendy_nes::apu::Apu;
use tendy_nes::controller::Controller;
use tendy_nes::cpu::Cpu;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::Ppu;
use tendy_nes::state::{StateError, StateReader, StateWriter};

use crate::common::rom_file;

/// Warm `frames` frames of nestest (it exercises the joypad ports every frame).
fn warm_nestest(frames: usize) -> Nes {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    for _ in 0..frames {
        nes.run_frame();
    }
    nes
}

#[test]
fn cpu_state_round_trips_from_a_running_machine() {
    let nes = warm_nestest(30);
    let cpu = nes.cpu.clone();

    let mut w = StateWriter::new();
    cpu.save_state(&mut w);
    let data = w.finish();

    let mut loaded = Cpu::new();
    loaded.load_state(&mut StateReader::new(&data)).unwrap();

    assert_eq!(loaded, cpu);
    assert_eq!(format!("{loaded:?}"), format!("{cpu:?}"));
    assert_eq!(StateReader::new(&data).remaining(), data.len());
}

#[test]
fn joypad_state_round_trips_from_a_running_machine() {
    let nes = warm_nestest(30);
    let pad = nes.bus.pads[0].clone();

    let mut w = StateWriter::new();
    pad.save_state(&mut w);
    let data = w.finish();

    let mut loaded = Controller::default();
    loaded.load_state(&mut StateReader::new(&data)).unwrap();

    assert_eq!(loaded, pad);
    assert_eq!(format!("{loaded:?}"), format!("{pad:?}"));
}

#[test]
fn joypad_shift_register_is_part_of_the_state() {
    // Strobe low, then read two bits: the shift register is mid-shift and must be restored.
    let mut pad = Controller::default();
    pad.buttons = 0b1010_0101;
    pad.write(1);
    pad.write(0);
    pad.read();
    pad.read();

    let mut w = StateWriter::new();
    pad.save_state(&mut w);
    let data = w.finish();

    let mut loaded = Controller::default();
    loaded.load_state(&mut StateReader::new(&data)).unwrap();
    assert_eq!(loaded, pad);
    assert_eq!(loaded.read(), pad.read());
}

#[test]
fn truncated_state_is_an_error() {
    let mut cpu = Cpu::new();
    assert_eq!(
        cpu.load_state(&mut StateReader::new(&[])),
        Err(StateError::Truncated)
    );

    let mut pad = Controller::default();
    assert_eq!(
        pad.load_state(&mut StateReader::new(&[])),
        Err(StateError::Truncated)
    );

    // A state cut short of its last field must also fail rather than silently default.
    let mut w = StateWriter::new();
    Cpu::new().save_state(&mut w);
    let data = w.finish();
    assert_eq!(
        Cpu::new().load_state(&mut StateReader::new(&data[..data.len() - 1])),
        Err(StateError::Truncated)
    );
}

/// The APU after `frames` frames of `rom`, with the output drain buffer emptied (it is not state).
fn warm_apu(rom: &str, frames: usize) -> Apu {
    let mut nes = Nes::new(&rom_file(rom)).unwrap();
    for _ in 0..frames {
        nes.run_frame();
    }
    let mut apu = nes.bus.apu.clone();
    apu.samples.clear();
    apu
}

fn assert_apu_round_trips(apu: &Apu) {
    // Guard against a vacuous comparison: the warm machine must differ from a fresh APU.
    assert_ne!(format!("{apu:?}"), format!("{:?}", Apu::new()));

    let mut w = StateWriter::new();
    apu.save_state(&mut w);
    let data = w.finish();

    let mut loaded = Apu::new();
    loaded.sample_rate = apu.sample_rate;
    loaded.load_state(&mut StateReader::new(&data)).unwrap();

    assert_eq!(StateReader::new(&data).remaining(), data.len());
    assert_eq!(format!("{loaded:?}"), format!("{apu:?}"));
}

#[test]
fn apu_state_round_trips_after_dmc_playback() {
    assert_apu_round_trips(&warm_apu("7-dmc_basics.nes", 120));
}

#[test]
fn apu_state_round_trips_with_a_frame_irq_pending() {
    assert_apu_round_trips(&warm_apu("4-irq_and_dma.nes", 120));
}

fn roundtrip_ppu(rom: &str, frames: usize, steps: usize) {
    let data = rom_file(rom);
    let mut nes = Nes::new(&data).unwrap();
    for _ in 0..frames {
        nes.run_frame();
    }
    for _ in 0..steps {
        nes.step();
    }
    let mut w = StateWriter::new();
    nes.bus.ppu.save_state(&mut w);
    let bytes = w.finish();
    let mut fresh = Ppu::new();
    let mut r = StateReader::new(&bytes);
    fresh.load_state(&mut r).unwrap();
    assert_eq!(r.remaining(), 0, "unread bytes");
    assert_eq!(format!("{:?}", nes.bus.ppu), format!("{fresh:?}"), "{rom}");
    // Saving the loaded copy yields identical bytes.
    let mut w2 = StateWriter::new();
    fresh.save_state(&mut w2);
    assert_eq!(bytes, w2.finish());
}

#[test]
fn ppu_roundtrip_accuracycoin_mid_frame() {
    roundtrip_ppu("AccuracyCoin.nes", 200, 1234);
}

#[test]
fn ppu_roundtrip_oam_stress() {
    roundtrip_ppu("oam_stress.nes", 300, 0);
}

#[test]
fn ppu_load_truncated() {
    let data = rom_file("oam_stress.nes");
    let mut nes = Nes::new(&data).unwrap();
    nes.run_frame();
    let mut w = StateWriter::new();
    nes.bus.ppu.save_state(&mut w);
    let bytes = w.finish();
    let mut p = Ppu::new();
    assert_eq!(
        p.load_state(&mut StateReader::new(&[])),
        Err(StateError::Truncated)
    );
    let cut = &bytes[..bytes.len() - 1];
    assert!(p.load_state(&mut StateReader::new(cut)).is_err());
}

/// The OAM-decay row stamps are emulated state: they are the last field the PPU
/// writes, they are saved whatever the quirks (spec §4), and they round-trip exactly.
#[test]
fn oam_decay_stamps_round_trip() {
    let mut ppu = Ppu::new();
    assert_eq!(ppu.oam_stamp, [0; 32], "power-on stamps are zero");
    for (i, stamp) in ppu.oam_stamp.iter_mut().enumerate() {
        *stamp = (i as u64) * 0x0101_0101_0101 + 7;
    }
    let mut w = StateWriter::new();
    ppu.save_state(&mut w);
    let bytes = w.finish();

    let mut fresh = Ppu::new();
    let mut r = StateReader::new(&bytes);
    fresh.load_state(&mut r).unwrap();
    assert_eq!(
        r.remaining(),
        0,
        "unread bytes: the stamps are the last field"
    );
    assert_eq!(fresh.oam_stamp, ppu.oam_stamp);

    // And the layout does not depend on the quirk: the same PPU with it on writes the same length.
    let mut decayed = Ppu::new();
    decayed.quirks.oam_decay = true;
    let mut w2 = StateWriter::new();
    decayed.save_state(&mut w2);
    assert_eq!(w2.finish().len(), bytes.len());
}
