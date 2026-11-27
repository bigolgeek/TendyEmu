//! Bus memory map: RAM mirroring, PRG mapping and open bus.

use tendy_nes::bus::SysBus;
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::load;

/// A minimal NROM image: 1 PRG bank, 1 CHR bank; the first PRG byte is `first`.
fn tiny_rom(first: u8) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16] = first;
    v
}

#[test]
fn ram_is_mirrored() {
    let mut bus = SysBus::new(load(&tiny_rom(0xEA)).unwrap());

    bus.write(0x0001, 0x42);

    assert_eq!(bus.read(0x0801), 0x42);
    assert_eq!(bus.read(0x1001), 0x42);
    assert_eq!(bus.read(0x1801), 0x42);
    assert_eq!(bus.ram[1], 0x42);
}

#[test]
fn cartridge_is_mapped() {
    let mut bus = SysBus::new(load(&tiny_rom(0xEA)).unwrap());

    assert_eq!(bus.read(0x8000), 0xEA);
    assert_eq!(bus.read(0xC000), 0xEA);
}

#[test]
fn prg_ram_through_bus() {
    let mut bus = SysBus::new(load(&tiny_rom(0xEA)).unwrap());

    bus.write(0x6000, 7);

    assert_eq!(bus.read(0x6000), 7);
}

#[test]
fn unmapped_reads_return_open_bus() {
    let mut bus = SysBus::new(load(&tiny_rom(0xEA)).unwrap());

    assert_eq!(bus.read(0x8000), 0xEA);
    assert_eq!(bus.read(0x4018), 0xEA);
}
