//! RetroAchievements, offline: condition sets run against a synthetic RAM trace and
//! against a real guest through the side-effect-free bus read. Nothing here touches the network.

use tendy_nes::checksum;
use tendy_nes::nes::Nes;
use tendy_nes::ra::{Achievement, nes_bus_address, read_nes};

use crate::common::program_code;

fn read_ram(ram: &[u8], addr: u32) -> u8 {
    ram.get(addr as usize).copied().unwrap_or(0)
}

#[test]
fn a_condition_set_unlocks_on_the_frame_both_conditions_first_hold() {
    // $10 counts frames and $20 bit 0 is a "door open" flag that appears on frame 18.
    let mut trace = Vec::new();
    for frame in 0..40u8 {
        let mut ram = [0u8; 0x800];
        ram[0x10] = frame;
        ram[0x20] = u8::from(frame >= 18);
        trace.push(ram);
    }

    let mut achievement = Achievement::new("0xH0010>=h12_0xM0020=1").unwrap();
    let mut unlocks = Vec::new();
    for (frame, ram) in trace.iter().enumerate() {
        if achievement.step(|addr| read_ram(ram, addr)) {
            unlocks.push(frame);
        }
    }
    assert_eq!(unlocks, vec![18]);

    // The bit alone (frame 0) and the counter alone (frame 39 without the bit) are not enough.
    let mut bit_only = Achievement::new("0xM0020=1").unwrap();
    let mut count = 0;
    for ram in &trace {
        if bit_only.step(|addr| read_ram(ram, addr)) {
            count += 1;
        }
    }
    assert_eq!(count, 1);
}

#[test]
fn the_engine_reads_the_guest_through_the_side_effect_free_bus_peek() {
    // JMP $8000 forever: the guest does not touch RAM, so the test owns $0100.
    let mut nes = Nes::new(&program_code(&[0x4C, 0x00, 0x80])).unwrap();
    nes.bus.ram[0x0100] = 0;

    // RA $0900 is a mirror of $0100. The first true frame only primes the achievement.
    let mut achievement = Achievement::new("0xH0900=h5A").unwrap();
    assert!(!achievement.step(|addr| read_nes(&mut nes.bus, addr)));

    nes.bus.ram[0x0100] = 0x5A;
    assert_eq!(read_nes(&mut nes.bus, 0x0900), 0x5A);
    assert_eq!(read_nes(&mut nes.bus, 0x0100), 0x5A);
    assert!(achievement.step(|addr| read_nes(&mut nes.bus, addr)));

    // PPU/APU registers are not exposed to achievements: a blank read, no side effect.
    assert_eq!(nes_bus_address(0x2002), None);
    assert_eq!(read_nes(&mut nes.bus, 0x2002), 0);
}

#[test]
fn a_synthetic_rom_is_hashed_without_its_ines_header() {
    let rom = program_code(&[0x4C, 0x00, 0x80]);
    assert_eq!(
        tendy_nes::ra::game_hash(&rom),
        checksum::hex(&checksum::md5(&rom[16..]))
    );
    assert_ne!(
        tendy_nes::ra::game_hash(&rom),
        checksum::hex(&checksum::md5(&rom))
    );
}
