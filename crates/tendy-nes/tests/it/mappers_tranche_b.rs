//! Bank switching on a second group of discrete mappers (38, 78, 79, 140, 152, 185). Every decode here
//! comes from the board's nesdev wiki page, quoted in the test that exercises it; nothing is taken
//! from another emulator's source.

use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::cnrom_security::CnromSecurity;
use tendy_nes::mapper::cnrom_unrom::{CnromUnrom, Kind as CuKind};
use tendy_nes::mapper::gnrom_moved::{GnromMoved, Kind as GnromKind};
use tendy_nes::mapper::nina::Nina;

/// PRG has `prg_banks` 16 KB banks where every byte holds its bank number; CHR is `chr_kb` KB where
/// every byte holds its 4 KB page number. So a 32 KB PRG bank `b` starts at 16 KB bank `2b`, and an
/// 8 KB CHR bank `b` is 4 KB pages `2b` and `2b + 1`.
fn rom_data(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
    RomData {
        header: Header {
            prg_banks,
            chr_banks: chr_kb / 8,
            mapper: mapper.into(),
            mirroring: Mirroring::Vertical,
            ..Default::default()
        },
        prg: (0..prg_banks * 0x4000)
            .map(|i| (i / 0x4000) as u8)
            .collect(),
        chr: (0..chr_kb * 1024).map(|i| (i / 0x1000) as u8).collect(),
        chr_is_ram: false,
        trainer: None,
    }
}

/// `rom_data` with 0xFF in every PRG byte after its bank number, so a board whose writes conflict
/// with the PRG bus latches exactly the value the test writes (the bank markers stay readable).
fn rom_data_ffff(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
    let mut rom = rom_data(mapper, prg_banks, chr_kb);
    for bank in 0..prg_banks {
        rom.prg[bank * 0x4000] = bank as u8;
        rom.prg[bank * 0x4000 + 1..(bank + 1) * 0x4000].fill(0xFF);
    }
    rom
}

/// Mapper 38 (Bit Corp. UNL-PCI556). nesdev "INES Mapper 038": the latch is at `$7000-$7FFF`, bits
/// 0-1 select the 32 KB PRG bank and bits 4-5 the 8 KB CHR bank; no bus conflicts.
#[test]
fn bit_corp_38_latches_from_7000() {
    let mut m = GnromMoved::new(rom_data(38, 8, 32), GnromKind::BitCorp);

    m.cpu_write(0x7000, 0x31); // PRG bank 1, CHR bank 3

    assert_eq!(
        m.cpu_read(0x8000),
        2,
        "32 KB PRG bank 1 starts at 16 KB bank 2"
    );
    assert_eq!(m.ppu_read(0x0000), 6, "8 KB CHR bank 3 is 4 KB pages 6-7");
    assert_eq!(m.ppu_read(0x1000), 7);

    // A write outside the latch's port changes nothing.
    m.cpu_write(0x8000, 0x00);
    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.ppu_read(0x0000), 6);
}

/// Mapper 140 (Jaleco JF-11/JF-14). nesdev "INES Mapper 140": GNROM with the port moved down to
/// `$6000-$7FFF`, bits 4-5 the 32 KB PRG bank and bits 0-3 the 8 KB CHR bank.
#[test]
fn jaleco_jf_140_latches_from_6000() {
    let mut m = GnromMoved::new(rom_data(140, 8, 32), GnromKind::JalecoJf);

    m.cpu_write(0x6000, 0x23); // PRG bank 2, CHR bank 3

    assert_eq!(
        m.cpu_read(0x8000),
        4,
        "32 KB PRG bank 2 starts at 16 KB bank 4"
    );
    assert_eq!(m.ppu_read(0x0000), 6, "8 KB CHR bank 3 is 4 KB pages 6-7");

    // $8000 is not the latch's port on this board.
    m.cpu_write(0x8000, 0x00);
    assert_eq!(m.cpu_read(0x8000), 4);
    assert_eq!(m.ppu_read(0x0000), 6);
}

/// Mapper 79 (NINA-03/NINA-06). nesdev "NINA-003-006": the latch answers the decode
/// `010x xxx1 xxxx xxxx` (`addr & 0xE100 == 0x4100`), bits 0-2 select the 8 KB CHR bank and bit 3 the
/// 32 KB PRG bank.
#[test]
fn nina_79_latches_from_4100_5fff() {
    let mut m = Nina::new(rom_data(79, 4, 64));

    m.cpu_write(0x4100, 0x0D); // CHR bank 5, PRG bank 1

    assert_eq!(
        m.cpu_read(0x8000),
        2,
        "32 KB PRG bank 1 starts at 16 KB bank 2"
    );
    assert_eq!(
        m.ppu_read(0x0000),
        10,
        "8 KB CHR bank 5 is 4 KB pages 10-11"
    );

    // $5F00 is the same register (A9-A13 are part of the decode).
    m.cpu_write(0x5F00, 0x01);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.ppu_read(0x0000), 2);

    // $4200 misses A8 of the decode, which the register requires to be set.
    m.cpu_write(0x4200, 0x07);
    assert_eq!(m.ppu_read(0x0000), 2);
}

/// Mapper 78 (Holy Diver, Uchuusen - Cosmo Carrier). nesdev "INES Mapper 078": bits 0-2 select the
/// 16 KB PRG bank at `$8000`, bit 3 the mirroring, bits 4-7 the 8 KB CHR bank, the last 16 KB stays
/// fixed, and the board is subject to bus conflicts. Cosmo Carrier's wiring (NES 2.0 submapper 1,
/// the submapper-0 default nesdev records) puts the bit straight on CIRAM A10.
#[test]
fn cosmo_carrier_78_switches_prg_chr_and_one_screen_mirroring() {
    let mut m = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, false);

    // $8001 holds 0xFF in this image, so the bus conflict leaves the written value intact.
    m.cpu_write(0x8001, 0x2A); // PRG bank 2, one-screen upper, CHR bank 2

    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.cpu_read(0xC000), 7, "the last 16 KB bank is fixed");
    assert_eq!(m.ppu_read(0x0000), 4, "8 KB CHR bank 2 is 4 KB pages 4-5");
    assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);

    m.cpu_write(0x8001, 0x11); // PRG bank 1, one-screen lower, CHR bank 1
    assert_eq!(m.cpu_read(0x8000), 1);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
}

/// Mapper 78 on Holy Diver (NES 2.0 submapper 3): the same bit drives a 2:1 mux of PPU A10/A11, so
/// nesdev's "Holy Diver: 0 = H, 1 = V" replaces Cosmo Carrier's one-screen pair.
#[test]
fn holy_diver_78_submapper_3_mirroring_is_horizontal_or_vertical() {
    let mut m = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, true);

    m.cpu_write(0x8001, 0x08);
    assert_eq!(m.mirroring(), Mirroring::Vertical);

    m.cpu_write(0x8001, 0x00);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
}

/// Mapper 152 (Arkanoid II, Gegege no Kitarou 2). nesdev "INES Mapper 152": bit 7 the mirroring
/// (0 = one-screen A, 1 = one-screen B), bits 4-6 the 16 KB PRG bank at `$8000`, bits 0-3 the 8 KB
/// CHR bank, last PRG bank fixed, bus conflicts.
#[test]
fn arkanoid_2_152_switches_prg_chr_and_one_screen_mirroring() {
    let mut m = CnromUnrom::new(rom_data_ffff(152, 8, 128), CuKind::M152, false);

    m.cpu_write(0x8001, 0xA5); // PRG bank 2, one-screen upper, CHR bank 5

    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.cpu_read(0xC000), 7, "the last 16 KB bank is fixed");
    assert_eq!(
        m.ppu_read(0x0000),
        10,
        "8 KB CHR bank 5 is 4 KB pages 10-11"
    );
    assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);

    m.cpu_write(0x8001, 0x2A); // PRG bank 2, one-screen lower, CHR bank 10
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
    assert_eq!(m.ppu_read(0x0000), 20);
}

/// Both CNROM+UNROM boards are "subject to bus conflicts" (nesdev 078 and 152): the latch takes the
/// written value ANDed with the PRG byte at that address.
#[test]
fn cnrom_unrom_writes_conflict_with_prg() {
    // 78: bits 0-2 hold the PRG bank, so the latched $FF AND $06 selects PRG bank 6.
    let mut rom = rom_data(78, 8, 128);
    rom.prg[1] = 0x06; // the byte at $8001 is ANDed into the write
    let mut m = CnromUnrom::new(rom, CuKind::M78, false);
    m.cpu_write(0x8001, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 6, "latched $FF AND $06");

    // 152: the same latched value holds PRG bank 0 and CHR bank 6.
    let mut rom = rom_data(152, 8, 128);
    rom.prg[1] = 0x06;
    let mut m = CnromUnrom::new(rom, CuKind::M152, false);
    m.cpu_write(0x8001, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(
        m.ppu_read(0x0000),
        12,
        "8 KB CHR bank 6 is 4 KB pages 12-13"
    );
}

/// Mapper 185 (CNROM with the security diodes). nesdev "CNROM": the 8 KB CHR-ROM's A13/A14 pins are
/// chip selects, so a latched value whose bits 0-1 are not the board's chip-select value leaves the
/// ROM tri-stated and pattern reads return open bus (0 in this crate); NES 2.0 submapper 4+N names
/// that value. PRG is 32 KB fixed and the board always has bus conflicts.
#[test]
fn cnrom_security_185_disables_chr_rom_for_the_wrong_chip_select() {
    let mut rom = rom_data_ffff(185, 2, 8);
    rom.header.submapper = 4; // chip-select value 0
    rom.chr[0] = 0x5A;
    let mut m = CnromSecurity::new(rom);

    m.cpu_write(0x8001, 0x00);
    assert_eq!(m.ppu_read(0x0000), 0x5A, "chip selects 0: CHR-ROM enabled");
    assert_eq!(m.cpu_read(0xC000), 1, "PRG stays 32 KB fixed");

    m.cpu_write(0x8001, 0x02);
    assert_eq!(m.ppu_read(0x0000), 0, "chip selects 2: CHR-ROM tri-stated");

    // Submapper 5 names chip-select value 1.
    let mut rom = rom_data_ffff(185, 2, 8);
    rom.header.submapper = 5;
    rom.chr[0] = 0x5A;
    let mut m = CnromSecurity::new(rom);

    m.cpu_write(0x8001, 0x01);
    assert_eq!(m.ppu_read(0x0000), 0x5A);
    m.cpu_write(0x8001, 0x00);
    assert_eq!(m.ppu_read(0x0000), 0);
}

/// The chip-select bits arrive through the same bus-conflict AND as a mapper 3 write: the value the
/// board latches is the write ANDed with the PRG byte at that address, not the write itself.
#[test]
fn cnrom_security_185_writes_conflict_with_prg() {
    let mut rom = rom_data(185, 2, 8);
    rom.header.submapper = 4; // chip-select value 0: the write's $FF AND $06 latches 2, which disables
    rom.prg[0x10] = 0x06;
    rom.chr[0] = 0x5A;
    let mut m = CnromSecurity::new(rom);

    m.cpu_write(0x8010, 0xFF);

    assert_eq!(m.ppu_read(0x0000), 0);
}

/// An iNES 1.0 header (submapper 0) names no chip-select value, and nesdev's substitute — disable
/// CHR-ROM for the first two reads from $2007 after a reset — needs a read's originating CPU address,
/// which `Mapper::ppu_read` is not passed. CHR-ROM therefore stays enabled.
#[test]
fn cnrom_security_185_submapper_0_keeps_chr_rom_enabled() {
    let mut rom = rom_data_ffff(185, 2, 8);
    rom.chr[0] = 0x5A;
    let mut m = CnromSecurity::new(rom);

    m.cpu_write(0x8001, 0x02);
    assert_eq!(m.ppu_read(0x0000), 0x5A);
}
