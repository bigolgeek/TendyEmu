use tendy_nes::controller::{BUTTON_A, BUTTON_DOWN, BUTTON_START};
use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;

use crate::common::loop_rom;
use crate::common::rom_file;

#[test]
fn pad1_shifts_out_through_4016() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    nes.bus.pads[0].buttons = BUTTON_A | BUTTON_DOWN;
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);

    let bits: Vec<u8> = (0..8).map(|_| nes.bus.read(0x4016) & 1).collect();
    assert_eq!(bits, vec![1, 0, 0, 0, 0, 1, 0, 0]);
}

#[test]
fn reads_carry_open_bus_bit_6() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    nes.bus.pads[0].buttons = 0;
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    nes.bus.open_bus = 0x40; // last value on the bus has bit 6 set

    assert_eq!(nes.bus.read(0x4016), 0x40);
}

#[test]
fn pad2_is_on_4017() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    nes.bus.pads[1].buttons = BUTTON_A;
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);

    assert_eq!(nes.bus.read(0x4017) & 1, 1);
    assert_eq!(nes.bus.read(0x4016) & 1, 0);
}

#[test]
fn nestest_start_runs_all_tests() {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    for _ in 0..30 {
        nes.run_frame();
    }
    nes.bus.pads[0].buttons = BUTTON_START;
    for _ in 0..5 {
        nes.run_frame();
    }
    nes.bus.pads[0].buttons = 0;
    for _ in 0..120 {
        nes.run_frame();
    }
    // Reviewed snapshot: every menu line reads "OK" after "Run all tests".
    assert_eq!(fnv1a64(&nes.bus.ppu.framebuffer), 0x996c_39af_8842_43c1);
}

/// NROM image whose program is `LDA $40xx` then `JMP $8000`, with the reset vector pointing at
/// it: every frame polls the joypad port at `port` ($4016 or $4017), so no frame of it can be
/// a lag frame.
fn pad_poll_rom(port: u8) -> Vec<u8> {
    let mut v = loop_rom(0, 0);
    v[16..22].copy_from_slice(&[0xAD, port, 0x40, 0x4C, 0x00, 0x80]);
    v
}

#[test]
fn frame_with_no_pad_read_is_lag() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    nes.bus.frame_pads_polled = false;
    nes.run_frame();
    assert!(!nes.bus.frame_pads_polled);
}

#[test]
fn frame_with_pad_read_is_not_lag() {
    let mut nes = Nes::new(&pad_poll_rom(0x16)).unwrap();
    nes.bus.frame_pads_polled = false;
    nes.run_frame();
    assert!(nes.bus.frame_pads_polled);
}

/// REVIEW-237 required change: a $4017-only pad poll must keep the frame out of lag when its
/// flag-set is removed — this is the test that dies on mutant (a) (deleting the $4017 arm).
#[test]
fn frame_with_4017_read_is_not_lag() {
    let mut nes = Nes::new(&pad_poll_rom(0x17)).unwrap();
    nes.bus.frame_pads_polled = false;
    nes.run_frame();
    assert!(nes.bus.frame_pads_polled);
}

#[test]
fn unpolled_read_is_not_a_poll() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    nes.bus.frame_pads_polled = false;
    nes.bus.read_unpolled(0x4016);
    assert!(!nes.bus.frame_pads_polled);
}
