use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::axrom::Axrom;
use tendy_nes::mapper::bnrom::Bnrom;
use tendy_nes::mapper::camerica::Camerica;
use tendy_nes::mapper::color_dreams::ColorDreams;
use tendy_nes::mapper::cprom::Cprom;
use tendy_nes::mapper::gxrom::Gxrom;
use tendy_nes::mapper::j87::J87;
use tendy_nes::mapper::load;
use tendy_nes::mapper::sunsoft_latch::{Sunsoft1, Sunsoft2};
use tendy_nes::mapper::unrom_variants::{Un1rom, UnromInverted};
use tendy_nes::state::{StateError, StateReader, StateWriter};

/// PRG has `prg_banks` 16 KB banks where every byte holds its bank number; CHR is `chr_kb` KB where
/// every byte holds its 4 KB page number (CHR-RAM when chr_kb is 0: 8 KB of zeros).
fn rom_data(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
    let chr_is_ram = chr_kb == 0;
    let chr = if chr_is_ram {
        vec![0; 8192]
    } else {
        (0..chr_kb * 1024).map(|i| (i / 0x1000) as u8).collect()
    };
    RomData {
        header: Header {
            prg_banks,
            chr_banks: chr_kb / 8,
            mapper: mapper.into(),
            mirroring: Mirroring::Vertical,
            battery: false,
            trainer: false,
            ..Default::default()
        },
        prg: (0..prg_banks * 0x4000)
            .map(|i| (i / 0x4000) as u8)
            .collect(),
        chr,
        chr_is_ram,
        trainer: None,
    }
}

/// `rom_data` with every PRG bank filled with 0xFF instead of its bank number, so a register write on
/// a board with bus conflicts latches exactly the value the test wrote. Each bank's first byte still
/// holds its number, which is what the tests read to tell the banks apart.
fn rom_data_ffff(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
    let mut rom = rom_data(mapper, prg_banks, chr_kb);
    for bank in 0..prg_banks {
        rom.prg[bank * 0x4000] = bank as u8;
        rom.prg[bank * 0x4000 + 1..(bank + 1) * 0x4000].fill(0xFF);
    }
    rom
}

fn roundtrip(a: &dyn Mapper, b: &mut dyn Mapper) {
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut r = StateReader::new(&bytes);
    b.load_state(&mut r).unwrap();
    assert_eq!(r.remaining(), 0, "load must consume the whole state");
}

/// AxROM selects one 32 KB bank: bank n reads `2n` at $8000 and `2n+1` at $C000. The pinned key
/// values (write $05 -> $8000 = 10, $C000 = 11) need the 8 x 32 KB layout, i.e. 256 KB of PRG.
#[test]
fn axrom_switches_32k_bank() {
    let mut m = Axrom::new(rom_data(7, 16, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    m.cpu_write(0x8000, 0x05);
    assert_eq!(m.cpu_read(0x8000), 10);
    assert_eq!(m.cpu_read(0xC000), 11);
}

#[test]
fn axrom_bank_wraps() {
    let mut m = Axrom::new(rom_data(7, 8, 0));
    m.cpu_write(0xFFFF, 0x05);
    // 128 KB = four 32 KB banks: 5 wraps to 1, which reads bank 2 and 3.
    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.cpu_read(0xC000), 3);
}

#[test]
fn axrom_switches_mirroring_half() {
    let mut m = Axrom::new(rom_data(7, 16, 0));
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
    m.cpu_write(0x8000, 0x10);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
    assert_eq!(m.cpu_read(0x8000), 0);
    m.cpu_write(0xB001, 0x05);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
    assert_eq!(m.cpu_read(0x8000), 10);
}

#[test]
fn axrom_chr_ram_is_writable() {
    let mut m = Axrom::new(rom_data(7, 16, 0));
    m.ppu_write(0x0123, 7);
    assert_eq!(m.ppu_read(0x0123), 7);
}

#[test]
fn axrom_chr_rom_is_read_only() {
    let mut m = Axrom::new(rom_data(7, 16, 8));
    m.ppu_write(0x0000, 99);
    assert_eq!(m.ppu_read(0x0000), 0);
}

#[test]
fn axrom_has_no_prg_ram() {
    let mut m = Axrom::new(rom_data(7, 16, 0));
    assert_eq!(m.cpu_read(0x6000), 0);
    m.load_prg_ram(&[0xAA]);
    assert_eq!(m.prg_ram(), &[][..]);
}

#[test]
fn axrom_state_roundtrip() {
    let mut a = Axrom::new(rom_data(7, 16, 0));
    a.cpu_write(0x8000, 0x15);
    a.ppu_write(0x1000, 0x44);
    let mut b = Axrom::new(rom_data(7, 16, 0));
    roundtrip(&a, &mut b);
    // 256 KB: eight 32 KB banks, so bank 5 maps 16 KB banks 10 and 11 unchanged through the state.
    assert_eq!(b.cpu_read(0x8000), 10, "bank 5 survives the roundtrip");
    assert_eq!(b.cpu_read(0xC000), 11);
    assert_eq!(b.mirroring(), Mirroring::SingleScreenUpper);
    assert_eq!(b.ppu_read(0x1000), 0x44);
}

#[test]
fn axrom_truncated_state_is_an_error() {
    let a = Axrom::new(rom_data(7, 16, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Axrom::new(rom_data(7, 16, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

#[test]
fn load_builds_mapper_7() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x70, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16) {
        *byte = ((i - 16) / 16384) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 6);
    assert_eq!(m.cpu_read(0xC000), 7);
}

// GxROM (mapper 66)

/// GxROM: bits 4-5 of the write select the 32 KB PRG bank, bits 0-1 the 8 KB CHR bank. The pinned
/// key values (write $21 -> $8000 = 4, PPU $0000 = 2, $1000 = 3) use 8 x 16 KB PRG and 32 KB CHR.
#[test]
fn gxrom_switches_prg_and_chr_banks() {
    let mut m = Gxrom::new(rom_data(66, 8, 32));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    m.cpu_write(0x8000, 0x21);
    assert_eq!(m.cpu_read(0x8000), 4, "PRG bank 2 maps 16 KB banks 4 and 5");
    assert_eq!(m.cpu_read(0xC000), 5);
    assert_eq!(m.ppu_read(0x0000), 2, "CHR bank 1 maps page 2 at $0000");
    assert_eq!(m.ppu_read(0x1000), 3, "CHR bank 1 maps page 3 at $1000");
}

/// CHR banks wrap when CHR is smaller than four 8 KB banks; a CHR write into a nonselected bank
/// never lands.
#[test]
fn gxrom_chr_bank_wraps() {
    let mut m = Gxrom::new(rom_data(66, 8, 16));
    m.cpu_write(0xFFFF, 0x03);
    // 16 KB of CHR = two 8 KB banks: bank 3 wraps to 1, which reads pages 2 and 3.
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x1800), 3);
}

/// GxROM's mirroring is the header value and register writes never touch it.
#[test]
fn gxrom_mirroring_stays_header_value() {
    let mut m = Gxrom::new(rom_data(66, 8, 32));
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    m.cpu_write(0xB000, 0x21);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn gxrom_chr_ram_is_writable() {
    let mut m = Gxrom::new(rom_data(66, 8, 0));
    m.cpu_write(0x8000, 0x21);
    m.ppu_write(0x1123, 7);
    assert_eq!(m.ppu_read(0x1123), 7);
}

#[test]
fn gxrom_chr_rom_is_read_only() {
    let mut m = Gxrom::new(rom_data(66, 8, 32));
    m.ppu_write(0x0000, 99);
    assert_eq!(m.ppu_read(0x0000), 0);
}

#[test]
fn gxrom_has_no_prg_ram() {
    let mut m = Gxrom::new(rom_data(66, 8, 32));
    assert_eq!(m.cpu_read(0x6000), 0);
    m.load_prg_ram(&[0xAA]);
    assert_eq!(m.prg_ram(), &[][..]);
}

#[test]
fn gxrom_state_roundtrip() {
    let mut a = Gxrom::new(rom_data(66, 8, 0));
    a.cpu_write(0x8000, 0x21);
    a.ppu_write(0x1000, 0x44);
    let mut b = Gxrom::new(rom_data(66, 8, 0));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 4, "bank survives the roundtrip");
    assert_eq!(b.ppu_read(0x1000), 0x44);
    assert_eq!(
        b.mirroring(),
        Mirroring::Vertical,
        "header mirroring survives"
    );
}

#[test]
fn gxrom_truncated_state_is_an_error() {
    let a = Gxrom::new(rom_data(66, 8, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Gxrom::new(rom_data(66, 8, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

// Color Dreams (mapper 11)

/// Color Dreams: bits 0-1 of the write select the 32 KB PRG bank, bits 4-7 the 8 KB CHR bank. The
/// pinned key values (write $32 -> $8000 = 4, PPU $0000 = 6) use 8 x 16 KB PRG and 32 KB CHR.
#[test]
fn color_dreams_switches_prg_and_chr_banks() {
    let mut m = ColorDreams::new(rom_data(11, 8, 32));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    m.cpu_write(0x8000, 0x32);
    assert_eq!(m.cpu_read(0x8000), 4, "PRG bank 2 maps 16 KB banks 4 and 5");
    assert_eq!(m.cpu_read(0xC000), 5);
    assert_eq!(m.ppu_read(0x0000), 6, "CHR bank 3 maps page 6 at $0000");
    assert_eq!(m.ppu_read(0x1000), 7, "CHR bank 3 maps page 7 at $1000");
}

/// CHR banks wrap when CHR is smaller than the selected bank; a CHR write into a nonselected bank
/// never lands.
#[test]
fn color_dreams_chr_bank_wraps() {
    let mut m = ColorDreams::new(rom_data(11, 8, 16));
    m.cpu_write(0xFFFF, 0xF0);
    // 16 KB of CHR = two 8 KB banks: bank 15 wraps to 1, which reads pages 2 and 3.
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x1800), 3);
}

/// Color Dreams' mirroring is the header value and register writes never touch it.
#[test]
fn color_dreams_mirroring_stays_header_value() {
    let mut m = ColorDreams::new(rom_data(11, 8, 32));
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    m.cpu_write(0xB000, 0x32);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn color_dreams_chr_ram_is_writable() {
    let mut m = ColorDreams::new(rom_data(11, 8, 0));
    m.cpu_write(0x8000, 0x32);
    m.ppu_write(0x1123, 7);
    assert_eq!(m.ppu_read(0x1123), 7);
}

#[test]
fn color_dreams_chr_rom_is_read_only() {
    let mut m = ColorDreams::new(rom_data(11, 8, 32));
    m.ppu_write(0x0000, 99);
    assert_eq!(m.ppu_read(0x0000), 0);
}

#[test]
fn color_dreams_has_no_prg_ram() {
    let mut m = ColorDreams::new(rom_data(11, 8, 32));
    assert_eq!(m.cpu_read(0x6000), 0);
    m.load_prg_ram(&[0xAA]);
    assert_eq!(m.prg_ram(), &[][..]);
}

#[test]
fn color_dreams_state_roundtrip() {
    let mut a = ColorDreams::new(rom_data(11, 8, 0));
    a.cpu_write(0x8000, 0x32);
    a.ppu_write(0x1000, 0x44);
    let mut b = ColorDreams::new(rom_data(11, 8, 0));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 4, "bank survives the roundtrip");
    assert_eq!(b.ppu_read(0x1000), 0x44);
    assert_eq!(
        b.mirroring(),
        Mirroring::Vertical,
        "header mirroring survives"
    );
}

#[test]
fn color_dreams_truncated_state_is_an_error() {
    let a = ColorDreams::new(rom_data(11, 8, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = ColorDreams::new(rom_data(11, 8, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

#[test]
fn load_builds_mapper_11() {
    // flags6 high nibble 0x0B, flags7 zero -> mapper 11; 8 PRG banks, 4 CHR banks, vertical
    // mirroring in flags6 bit 0.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 4, 0xB1, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384 + 4 * 8192, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16).take(8 * 16384) {
        *byte = ((i - 16) / 16384) as u8;
    }
    for (i, byte) in v.iter_mut().enumerate().skip(16 + 8 * 16384) {
        *byte = ((i - 16 - 8 * 16384) / 4096) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0x32);
    assert_eq!(m.cpu_read(0x8000), 4);
    assert_eq!(m.cpu_read(0xC000), 5);
    assert_eq!(m.ppu_read(0x0000), 6);
    assert_eq!(m.ppu_read(0x1000), 7);
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "header mirroring passes through"
    );
}

#[test]
fn load_builds_mapper_66() {
    // flags6 high nibble 2, flags7 high nibble 4 -> mapper (0x40 | 0x02) = 66; 8 PRG banks,
    // 4 CHR banks, vertical mirroring in flags6 bit 0.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 4, 0x21, 0x40, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384 + 4 * 8192, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16).take(8 * 16384) {
        *byte = ((i - 16) / 16384) as u8;
    }
    for (i, byte) in v.iter_mut().enumerate().skip(16 + 8 * 16384) {
        *byte = ((i - 16 - 8 * 16384) / 4096) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0x21);
    assert_eq!(m.cpu_read(0x8000), 4);
    assert_eq!(m.cpu_read(0xC000), 5);
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x1000), 3);
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "header mirroring passes through"
    );
}

// BNROM / NINA-001 (mapper 34)

/// The NINA-001 board is chosen by submapper 1 or, with submapper 0, by more than one 8 KB CHR
/// bank; everything else (including submapper 0 with CHR-RAM) is plain BNROM.
fn rom_data_sub(mapper: u8, prg_banks: usize, chr_kb: usize, submapper: u8) -> RomData {
    let mut r = rom_data(mapper, prg_banks, chr_kb);
    r.header.submapper = submapper;
    r
}

#[test]
fn bnrom_switches_32k_bank() {
    let mut m = Bnrom::new(rom_data(34, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    m.cpu_write(0x8000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 6, "PRG bank 3 maps 16 KB banks 6 and 7");
    assert_eq!(m.cpu_read(0xC000), 7);
}

/// All eight bits are the bank; banks wrap modulo the PRG size (four 32 KB banks here).
#[test]
fn bnrom_bank_wraps() {
    let mut m = Bnrom::new(rom_data(34, 8, 0));
    m.cpu_write(0xFFFF, 0x11);
    // 128 KB: bank 17 wraps to 1, which reads 16 KB banks 2 and 3.
    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.cpu_read(0xC000), 3);
}

#[test]
fn bnrom_chr_ram_is_writable() {
    let mut m = Bnrom::new(rom_data(34, 8, 0));
    m.ppu_write(0x1123, 7);
    assert_eq!(m.ppu_read(0x1123), 7);
}

/// BNROM has neither PRG RAM nor CHR ROM registers.
#[test]
fn bnrom_has_no_prg_ram() {
    let mut m = Bnrom::new(rom_data(34, 16, 0));
    assert_eq!(m.cpu_read(0x6000), 0);
    m.cpu_write(0x7FFE, 0x05);
    assert_eq!(
        m.cpu_read(0x7FFE),
        0,
        "the NINA registers do not exist on BNROM"
    );
    m.load_prg_ram(&[0xAA]);
    assert_eq!(m.prg_ram(), &[][..]);
}

#[test]
fn bnrom_state_roundtrip() {
    let mut a = Bnrom::new(rom_data(34, 8, 0));
    a.cpu_write(0x8000, 0x03);
    a.ppu_write(0x1000, 0x44);
    let mut b = Bnrom::new(rom_data(34, 8, 0));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 6, "bank survives the roundtrip");
    assert_eq!(b.ppu_read(0x1000), 0x44);
    assert_eq!(
        b.mirroring(),
        Mirroring::Vertical,
        "header mirroring survives"
    );
}

#[test]
fn bnrom_truncated_state_is_an_error() {
    let a = Bnrom::new(rom_data(34, 8, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Bnrom::new(rom_data(34, 8, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

/// With CHR greater than 8 KB and no submapper, the board is NINA-001: the pinned key values
/// ($7FFE=$05, $7FFF=$02 -> PPU $0000 = 5, $1000 = 2) and the register writes also land in RAM.
#[test]
fn nina_banks_are_split_registers_and_ram() {
    let mut m = Bnrom::new(rom_data(34, 8, 32));
    m.cpu_write(0x7FFE, 0x05);
    m.cpu_write(0x7FFF, 0x02);
    assert_eq!(m.ppu_read(0x0000), 5, "$7FFE picks the 4 KB page at $0000");
    assert_eq!(m.ppu_read(0x1000), 2, "$7FFF picks the 4 KB page at $1000");
    assert_eq!(
        m.cpu_read(0x7FFE),
        5,
        "register writes also store into the RAM"
    );
    assert_eq!(m.cpu_read(0x7FFF), 2);
    m.cpu_write(0x7FFD, 0x01);
    assert_eq!(
        m.cpu_read(0x8000),
        2,
        "$7FFD bit 0 picks the 32 KB PRG bank"
    );
    assert_eq!(m.cpu_read(0xC000), 3);
    assert_eq!(m.cpu_read(0x7FFD), 1);
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "header mirroring is immutable"
    );
}

/// NINA-001 ignores $8000-$FFFF writes: the register file is only $7FFD-$7FFF.
#[test]
fn nina_ignores_prg_region_writes() {
    let mut m = Bnrom::new(rom_data(34, 8, 32));
    m.cpu_write(0x8000, 0x01);
    assert_eq!(m.cpu_read(0x8000), 0, "bank 0 stays mapped");
    assert_eq!(m.cpu_read(0x7FFD), 0, "the write never landed in the RAM");
}

/// Both CHR pages wrap modulo the CHR size; with 16 KB of CHR (four 4 KB pages),
/// $7FFE=$05 wraps to page 1 and $7FFF=$02 stays page 2.
#[test]
fn nina_chr_banks_wrap() {
    let mut m = Bnrom::new(rom_data(34, 8, 16));
    m.cpu_write(0x7FFE, 0x05);
    m.cpu_write(0x7FFF, 0x02);
    assert_eq!(m.ppu_read(0x0000), 1);
    assert_eq!(m.ppu_read(0x0800), 1);
    assert_eq!(m.ppu_read(0x1000), 2);
    assert_eq!(m.ppu_read(0x1800), 2);
}

/// Submapper 1 forces NINA even with CHR-RAM; the two 4 KB pages are written independently.
#[test]
fn nina_chr_ram_is_writable_per_page() {
    let mut m = Bnrom::new(rom_data_sub(34, 8, 0, 1));
    m.cpu_write(0x7FFE, 0x00);
    m.cpu_write(0x7FFF, 0x03);
    m.ppu_write(0x0000, 7);
    m.ppu_write(0x1000, 9);
    assert_eq!(m.ppu_read(0x0000), 7);
    assert_eq!(m.ppu_read(0x1000), 9);
}

/// With CHR greater than 8 KB and no submapper, the board is NINA-001; submapper 2 overrides
/// that and selects plain BNROM, whose only register is the $8000-$FFFF PRG bank.
#[test]
fn submapper_2_is_plain_bnrom() {
    let mut m = Bnrom::new(rom_data_sub(34, 8, 32, 2));
    assert_eq!(m.cpu_read(0x6000), 0);
    m.cpu_write(0x8000, 0x03);
    assert_eq!(
        m.cpu_read(0x8000),
        6,
        "BNROM toggles the 32 KB bank from $8000"
    );
    m.cpu_write(0x7FFE, 0x05);
    assert_eq!(m.ppu_read(0x0000), 0, "$7FFD-$7FFF are RAM-less on BNROM");
}

/// NINA's 8 KB PRG RAM at $6000 round-trips and is reachable through prg_ram()/load_prg_ram().
#[test]
fn nina_prg_ram_roundtrip() {
    let mut a = Bnrom::new(rom_data(34, 8, 32));
    a.cpu_write(0x6123, 0x5A);
    a.cpu_write(0x7FFD, 0x01);
    let mut b = Bnrom::new(rom_data(34, 8, 32));
    b.load_prg_ram(a.prg_ram());
    assert_eq!(b.cpu_read(0x6123), 0x5A);
}

#[test]
fn nina_state_roundtrip() {
    let mut a = Bnrom::new(rom_data(34, 8, 32));
    a.cpu_write(0x7FFD, 0x01);
    a.cpu_write(0x7FFE, 0x05);
    a.cpu_write(0x7FFF, 0x02);
    a.cpu_write(0x6000, 0x5A);
    let mut b = Bnrom::new(rom_data(34, 8, 32));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 2, "PRG bank survives the roundtrip");
    assert_eq!(b.ppu_read(0x0000), 5, "CHR page 0 register survives");
    assert_eq!(b.ppu_read(0x1000), 2, "CHR page 1 register survives");
    assert_eq!(b.cpu_read(0x6000), 0x5A, "PRG RAM survives");
}

#[test]
fn nina_truncated_state_is_an_error() {
    let a = Bnrom::new(rom_data(34, 8, 32));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Bnrom::new(rom_data(34, 8, 32));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

#[test]
fn load_builds_mapper_34() {
    // flags6 = 0x21 (mapper low nibble 2, vertical mirroring), flags7 = 0x20 (mapper high
    // nibble 2) -> mapper 34; 8 PRG banks and 4 CHR banks (>1) select the NINA board.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 4, 0x21, 0x20, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384 + 4 * 8192, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16).take(8 * 16384) {
        *byte = ((i - 16) / 16384) as u8;
    }
    for (i, byte) in v.iter_mut().enumerate().skip(16 + 8 * 16384) {
        *byte = ((i - 16 - 8 * 16384) / 4096) as u8;
    }
    let mut m = load(&v).unwrap();
    assert_eq!(
        m.cpu_read(0x6000),
        0,
        "the NINA board mounts 8 KB of PRG RAM"
    );
    m.cpu_write(0x7FFE, 0x05);
    assert_eq!(m.ppu_read(0x0000), 5);
    assert_eq!(m.cpu_read(0x7FFE), 5);
}

#[test]
fn load_builds_plain_bnrom_34() {
    // 8 PRG banks and no CHR banks -> plain BNROM.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x21, 0x20, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16) {
        *byte = ((i - 16) / 16384) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 6);
    assert_eq!(m.cpu_read(0xC000), 7);
    assert_eq!(m.cpu_read(0x6000), 0, "BNROM has no PRG RAM");
}

// Camerica (mapper 71)

/// The pinned key values: $C000=$03 maps 16 KB prg bank 3 at $8000 and the last bank (7) stays at
/// $C000. prg 8 banks means 8 x 16 KB.
#[test]
fn camerica_switches_16k_bank() {
    let mut m = Camerica::new(rom_data(71, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xBFFF), 0);
    assert_eq!(
        m.cpu_read(0xC000),
        7,
        "the last 16 KB bank is fixed at $C000"
    );
    assert_eq!(m.cpu_read(0xFFFF), 7);
    m.cpu_write(0xC000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xBFFF), 3);
    assert_eq!(m.cpu_read(0xC000), 7);
}

/// The register responds to the whole $C000-$FFFF range, and the bank wraps modulo the PRG size.
#[test]
fn camerica_bank_wraps() {
    let mut m = Camerica::new(rom_data(71, 8, 0));
    m.cpu_write(0xE000, 0x09);
    // 8 x 16 KB: bank 9 wraps to 1.
    assert_eq!(m.cpu_read(0x8000), 1);
    // $BFFF is not part of the register window: the bank stays 1.
    m.cpu_write(0xBFFF, 0x05);
    assert_eq!(m.cpu_read(0x8000), 1);
}

/// CHR is RAM: writes land in the 8 KB window. prg 8 banks, chr 0 KB.
#[test]
fn camerica_chr_ram_is_writable() {
    let mut m = Camerica::new(rom_data(71, 8, 0));
    m.ppu_write(0x1234, 42);
    assert_eq!(m.ppu_read(0x1234), 42);
    assert_eq!(m.ppu_read(0x0000), 0, "read-only when CHR is ROM");
}

/// Submapper 1 reads bit 4 of $8000-$9FFF writes: $10 selects single-screen Upper, $00 Lower.
#[test]
fn camerica_submapper1_drives_mirroring() {
    let mut m = Camerica::new(rom_data_sub(71, 8, 0, 1));
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "starts at the header value"
    );
    m.cpu_write(0x9000, 0x10);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
    m.cpu_write(0x8100, 0x00);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
    m.cpu_write(0x9000, 0x03);
    assert_eq!(
        m.mirroring(),
        Mirroring::SingleScreenLower,
        "only bit 4 matters"
    );
    // $A000-$BFFF is outside the mirroring register window.
    m.cpu_write(0xB000, 0x10);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
}

/// On submapper 0 the header mirroring is immutable even for $10 writes.
#[test]
fn camerica_submapper0_keeps_header_mirroring() {
    let mut m = Camerica::new(rom_data(71, 8, 0));
    m.cpu_write(0x9000, 0x10);
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "header mirroring survives"
    );
    m.cpu_write(0x8000, 0x00);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn camerica_state_roundtrip() {
    let mut a = Camerica::new(rom_data_sub(71, 8, 0, 1));
    a.cpu_write(0xC000, 0x03);
    a.cpu_write(0x9000, 0x10);
    a.ppu_write(0x0400, 0x5A);
    let mut b = Camerica::new(rom_data_sub(71, 8, 0, 1));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 3, "bank survives the roundtrip");
    assert_eq!(b.cpu_read(0xC000), 7, "the fixed last bank survives");
    assert_eq!(
        b.mirroring(),
        Mirroring::SingleScreenUpper,
        "mirror half survives"
    );
    assert_eq!(b.ppu_read(0x0400), 0x5A, "CHR-RAM survives");
}

/// Submapper 0 writes no mirroring byte into the state, so a fresh mapper keeps its header value.
#[test]
fn camerica_submapper0_state_roundtrip() {
    let mut a = Camerica::new(rom_data(71, 8, 0));
    a.cpu_write(0xC000, 0x02);
    a.ppu_write(0x0100, 0x33);
    let mut b = Camerica::new(rom_data(71, 8, 0));
    roundtrip(&a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 2);
    assert_eq!(b.ppu_read(0x0100), 0x33);
    assert_eq!(b.mirroring(), Mirroring::Vertical);
}

#[test]
fn camerica_truncated_state_is_an_error() {
    let a = Camerica::new(rom_data(71, 8, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Camerica::new(rom_data(71, 8, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}

/// Mapper 71 is armed in `mapper::load`: an NES 2.0 header with 8 PRG banks, CHR-RAM (0 CHR banks)
/// and submapper 1 must build the Camerica mapper. flags6 = 0x71 (mapper low nibble 7, vertical),
/// flags7 = 0x48 (mapper bits 4-7 = 0x40 -> 71, NES 2.0 format), byte 8 = 0x10 (bits 4-7
/// submapper 1).
#[test]
fn load_builds_mapper_71() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x71, 0x48, 0x10, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16) {
        *byte = ((i - 16) / 16384) as u8;
    }
    let mut m = load(&v).unwrap();
    assert_eq!(m.cpu_read(0xC000), 7);
    m.cpu_write(0xC000, 0x01);
    assert_eq!(m.cpu_read(0x8000), 1);
    m.cpu_write(0x9000, 0x10);
    assert_eq!(
        m.mirroring(),
        Mirroring::SingleScreenUpper,
        "submapper 1 by NES 2.0 header"
    );
}

// CPROM (mapper 13)

/// CPROM's register picks the page at PPU $1000-$1FFF out of the board's 16 KB of CHR-RAM; the page
/// at $0000-$0FFF is soldered to the first one and never moves.
#[test]
fn cprom_switches_chr_ram_page() {
    let mut m = Cprom::new(rom_data_ffff(13, 2, 0));
    m.ppu_write(0x0010, 0x11);
    m.cpu_write(0xFFF0, 0x01);
    m.ppu_write(0x1000, 0x22);
    assert_eq!(m.ppu_read(0x1000), 0x22);
    m.cpu_write(0xFFF0, 0x02);
    assert_eq!(m.ppu_read(0x1000), 0x00, "page 2 is still empty");
    m.cpu_write(0xFFF0, 0x01);
    assert_eq!(
        m.ppu_read(0x1000),
        0x22,
        "page 1 kept what was written to it"
    );
    assert_eq!(m.ppu_read(0x0010), 0x11, "the $0000 page never moves");
}

/// Four pages of CHR-RAM, so the register holds two bits and ignores the rest of the write.
#[test]
fn cprom_register_holds_two_bits() {
    let mut m = Cprom::new(rom_data_ffff(13, 2, 0));
    m.cpu_write(0x9000, 0x07);
    m.ppu_write(0x1000, 0x33);
    m.cpu_write(0x9000, 0x0F);
    assert_eq!(m.ppu_read(0x1000), 0x33, "0x0F is page 3 again");
    m.cpu_write(0x9000, 0x04);
    assert_eq!(m.ppu_read(0x1000), 0x00, "0x04 is page 0");
}

/// The board has no conflict-prevention circuit: the latch sees the write ANDed with the PRG byte at
/// the address, which in this image is the bank number.
#[test]
fn cprom_bus_conflict_masks_the_register() {
    let mut m = Cprom::new(rom_data(13, 2, 0));
    m.cpu_write(0xFFF0, 0x01); // bank 1 holds 1s: 0x01 & 0x01 = 1
    m.ppu_write(0x1000, 0x44);
    m.cpu_write(0x9000, 0x02); // bank 0 holds zeroes: 0x02 & 0x00 = 0
    assert_eq!(m.ppu_read(0x1000), 0x00, "the conflict kept page 0");
    m.cpu_write(0xFFF0, 0x01);
    assert_eq!(m.ppu_read(0x1000), 0x44);
}

#[test]
fn cprom_chr_ram_is_sixteen_kilobytes() {
    let m = Cprom::new(rom_data(13, 2, 0));
    assert_eq!(m.chr_ram().len(), 0x4000, "two 6264s, so four 4 KB pages");
}

/// A CHR-ROM image on mapper 13 is a mislabelled dump: it reads through the CHR windows unchanged and
/// takes no writes, rather than being re-sized as RAM.
#[test]
fn cprom_chr_rom_dump_is_read_only() {
    let mut m = Cprom::new(rom_data(13, 2, 8));
    assert_eq!(m.chr_ram().len(), 0);
    m.cpu_write(0xFFF0, 0x01);
    assert_eq!(m.ppu_read(0x1000), 1, "4 KB page 1 of the image");
    m.ppu_write(0x1000, 0x99);
    assert_eq!(m.ppu_read(0x1000), 1);
}

#[test]
fn cprom_mirroring_stays_header_value() {
    let mut m = Cprom::new(rom_data(13, 2, 0));
    m.cpu_write(0xFFF0, 0x03);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

/// Mapper 13 is armed in `mapper::load`: flags6 = 0xD1 (mapper bits 0-3 = 0xD, vertical mirroring),
/// flags7 = 0x00 (mapper bits 4-7 = 0), two PRG banks and no CHR banks.
#[test]
fn load_builds_mapper_13() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 2, 0, 0xD1, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 2 * 16384, 0xFF);
    let mut m = load(&v).unwrap();
    m.ppu_write(0x1000, 0x55);
    m.cpu_write(0xFFF0, 0x01);
    assert_eq!(
        m.ppu_read(0x1000),
        0x00,
        "page 1 of the 16 KB CHR-RAM is a new page"
    );
    m.cpu_write(0xFFF0, 0x00);
    assert_eq!(m.ppu_read(0x1000), 0x55);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

// J87 (mapper 87)

/// J87 enters the register with its two low bits crossed: bit 0 is the high bit of the 8 KB CHR bank
/// number. CHR here is 32 KB, so the four banks read 4 KB pages 0/1, 2/3, 4/5 and 6/7.
#[test]
fn j87_switches_chr_bank_with_crossed_bits() {
    let mut m = J87::new(rom_data(87, 2, 32));
    assert_eq!(m.ppu_read(0x0000), 0);
    m.cpu_write(0x6000, 0x01);
    assert_eq!(m.ppu_read(0x0000), 4, "bit 0 selects bank 2");
    assert_eq!(m.ppu_read(0x1000), 5);
    m.cpu_write(0x7FFF, 0x02);
    assert_eq!(m.ppu_read(0x0000), 2, "bit 1 selects bank 1");
}

/// Banks wrap when CHR is smaller than the selected bank number.
#[test]
fn j87_chr_bank_wraps() {
    let mut m = J87::new(rom_data(87, 2, 16));
    m.cpu_write(0x6000, 0x03);
    assert_eq!(
        m.ppu_read(0x0000),
        2,
        "bank 3 is the second of two 8 KB banks"
    );
}

/// The 32 KB of PRG never switches, and $6000-$7FFF is the register rather than RAM.
#[test]
fn j87_prg_is_fixed_and_the_register_is_not_ram() {
    let mut m = J87::new(rom_data(87, 2, 8));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    m.cpu_write(0xA000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 0, "PRG never switches");
    assert!(m.prg_ram().is_empty(), "no PRG RAM on the board");
    assert_eq!(m.cpu_read(0x6000), 0, "$6000 is the register window");
}

#[test]
fn j87_chr_rom_is_read_only_and_chr_ram_takes_writes() {
    let mut rom = J87::new(rom_data(87, 2, 8));
    rom.ppu_write(0x0000, 0x99);
    assert_eq!(rom.ppu_read(0x0000), 0);
    let mut ram = J87::new(rom_data(87, 2, 0));
    ram.ppu_write(0x0000, 0x99);
    assert_eq!(ram.ppu_read(0x0000), 0x99);
}

#[test]
fn j87_mirroring_stays_header_value() {
    let mut m = J87::new(rom_data(87, 2, 8));
    m.cpu_write(0x6000, 0x03);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

/// Mapper 87 is armed in `mapper::load`: flags6 = 0x71 (mapper bits 0-3 = 7, vertical mirroring),
/// flags7 = 0x50 (mapper bits 4-7 = 5), two PRG banks and four CHR banks.
#[test]
fn load_builds_mapper_87() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 2, 4, 0x71, 0x50, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 2 * 16384 + 4 * 8192, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16 + 2 * 16384) {
        *byte = ((i - 16 - 2 * 16384) / 0x1000) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x6000, 0x01);
    assert_eq!(m.ppu_read(0x0000), 4, "bit 0 selects bank 2");
}

// Sunsoft-3R / Sunsoft-2 (mapper 93)

/// Bits 4-6 of a write select the 16 KB bank at $8000-$BFFF; the last bank stays at $C000-$FFFF.
#[test]
fn sunsoft2_switches_prg_bank() {
    let mut m = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 7, "the last bank is fixed");
    m.cpu_write(0xFFF0, 0x30);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xC000), 7);
}

#[test]
fn sunsoft2_prg_bank_wraps() {
    let mut m = Sunsoft2::new(rom_data_ffff(93, 4, 0));
    m.cpu_write(0xFFF0, 0x70);
    assert_eq!(m.cpu_read(0x8000), 3, "bank 7 wraps to the last of four");
}

/// The board has bus conflicts: a write at $8000 in bank 0, whose first byte is 0, latches nothing.
#[test]
fn sunsoft2_bus_conflict_masks_the_register() {
    let mut m = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    m.cpu_write(0x8000, 0x30);
    assert_eq!(m.cpu_read(0x8000), 0, "the conflict kept bank 0");
}

/// Bit 0 of the register enables the 8 KB of CHR-RAM: while it is clear the RAM ignores writes and
/// reads back as this crate's open-bus value, 0.
#[test]
fn sunsoft2_chr_ram_enable_bit() {
    let mut m = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    m.cpu_write(0xFFF0, 0x01);
    m.ppu_write(0x0010, 0x5A);
    assert_eq!(m.ppu_read(0x0010), 0x5A);
    m.cpu_write(0xFFF0, 0x00);
    assert_eq!(m.ppu_read(0x0010), 0x00, "disabled RAM reads open bus");
    m.ppu_write(0x0010, 0x77);
    m.cpu_write(0xFFF0, 0x01);
    assert_eq!(m.ppu_read(0x0010), 0x5A, "the disabled write never landed");
}

#[test]
fn sunsoft2_mirroring_stays_header_value() {
    let mut m = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    m.cpu_write(0xFFF0, 0x31);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

/// Mapper 93 is armed in `mapper::load`: flags6 = 0xD1 (mapper bits 0-3 = 0xD, vertical mirroring),
/// flags7 = 0x50 (mapper bits 4-7 = 5), eight PRG banks and CHR-RAM.
#[test]
fn load_builds_mapper_93() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0xD1, 0x50, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0xFF);
    for bank in 0..8 {
        v[16 + bank * 16384] = bank as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0xFFF0, 0x31);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xC000), 7);
    m.ppu_write(0x0000, 0x24);
    assert_eq!(m.ppu_read(0x0000), 0x24, "bit 0 enabled the CHR-RAM");
}

// UN1ROM (mapper 94)

/// UN1ROM's register is UxROM's shifted by two bits: 0x0C selects bank 3, not bank 12.
#[test]
fn un1rom_switches_prg_bank_from_shifted_register() {
    let mut m = Un1rom::new(rom_data_ffff(94, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 7, "the last bank is fixed");
    m.cpu_write(0xFFF0, 0x0C);
    assert_eq!(m.cpu_read(0x8000), 3);
    m.cpu_write(0xFFF0, 0x03);
    assert_eq!(m.cpu_read(0x8000), 0, "the low two bits select nothing");
}

#[test]
fn un1rom_prg_bank_wraps() {
    let mut m = Un1rom::new(rom_data_ffff(94, 4, 0));
    m.cpu_write(0xFFF0, 0x1C);
    assert_eq!(m.cpu_read(0x8000), 3, "bank 7 wraps to the last of four");
}

/// The board has bus conflicts, so the write ANDs with the PRG byte at the address: the fixed last
/// bank's first byte is 7, which turns 0x1C into 4 and so selects bank 1 where 0x1C alone would have
/// selected bank 7.
#[test]
fn un1rom_bus_conflict_masks_the_register() {
    let mut m = Un1rom::new(rom_data_ffff(94, 8, 0));
    m.cpu_write(0xC001, 0x1C);
    assert_eq!(m.cpu_read(0x8000), 7);
    m.cpu_write(0xC000, 0x1C);
    assert_eq!(m.cpu_read(0x8000), 1);
}

#[test]
fn un1rom_chr_ram_is_writable_and_chr_rom_is_not() {
    let mut ram = Un1rom::new(rom_data(94, 8, 0));
    ram.ppu_write(0x1234, 0x66);
    assert_eq!(ram.ppu_read(0x1234), 0x66);
    let mut rom = Un1rom::new(rom_data(94, 8, 8));
    rom.ppu_write(0x0000, 0x66);
    assert_eq!(rom.ppu_read(0x0000), 0, "CHR-ROM ignores the write");
}

#[test]
fn un1rom_mirroring_stays_header_value() {
    let mut m = Un1rom::new(rom_data(94, 8, 0));
    m.cpu_write(0xA000, 0x0C);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    assert!(m.prg_ram().is_empty(), "no PRG RAM on the board");
}

/// Mapper 94 is armed in `mapper::load`: flags6 = 0xE1 (mapper bits 0-3 = 0xE, vertical mirroring),
/// flags7 = 0x50 (mapper bits 4-7 = 5), eight PRG banks and CHR-RAM.
#[test]
fn load_builds_mapper_94() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0xE1, 0x50, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0xFF);
    for bank in 0..8 {
        v[16 + bank * 16384] = bank as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0xFFF0, 0x0C);
    assert_eq!(m.cpu_read(0x8000), 3);
    m.ppu_write(0x0010, 0x77);
    assert_eq!(m.ppu_read(0x0010), 0x77, "8 KB of CHR-RAM");
}

// UxROM inverted (mapper 180)

/// Mapper 180 fixes the *first* bank at $8000-$BFFF and switches $C000-$FFFF: bits 0-2 of the write
/// select the upper bank.
#[test]
fn unrom_inverted_switches_the_upper_bank() {
    let mut m = UnromInverted::new(rom_data_ffff(180, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0, "the first bank is fixed");
    assert_eq!(m.cpu_read(0xC000), 0);
    m.cpu_write(0xFFF0, 0x05);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 5);
}

#[test]
fn unrom_inverted_bank_wraps() {
    let mut m = UnromInverted::new(rom_data_ffff(180, 4, 0));
    m.cpu_write(0xFFF0, 0x07);
    assert_eq!(m.cpu_read(0xC000), 3, "bank 7 wraps to the last of four");
}

/// The board has bus conflicts, so a write at $8000 in the fixed first bank (first byte 0) latches
/// nothing, while a write at $C000 ANDs with the selected bank's first byte.
#[test]
fn unrom_inverted_bus_conflict_masks_the_register() {
    let mut m = UnromInverted::new(rom_data_ffff(180, 8, 0));
    m.cpu_write(0x8000, 0x05);
    assert_eq!(m.cpu_read(0xC000), 0, "the conflict kept bank 0");
    m.cpu_write(0xC001, 0x03);
    m.cpu_write(0xC000, 0x05);
    assert_eq!(m.cpu_read(0xC000), 1, "0x05 & 3 = 1");
}

#[test]
fn unrom_inverted_chr_ram_is_writable() {
    let mut m = UnromInverted::new(rom_data(180, 8, 0));
    m.ppu_write(0x0123, 0x88);
    assert_eq!(m.ppu_read(0x0123), 0x88);
    assert!(m.prg_ram().is_empty(), "no PRG RAM on the board");
}

#[test]
fn unrom_inverted_mirroring_stays_header_value() {
    let mut m = UnromInverted::new(rom_data(180, 8, 0));
    m.cpu_write(0xFFF0, 0x05);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

/// Mapper 180 is armed in `mapper::load`: flags6 = 0x41 (mapper bits 0-3 = 4, vertical mirroring),
/// flags7 = 0xB0 (mapper bits 4-7 = 0xB), eight PRG banks and CHR-RAM.
#[test]
fn load_builds_mapper_180() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x41, 0xB0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0xFF);
    for bank in 0..8 {
        v[16 + bank * 16384] = bank as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0xFFF0, 0x05);
    assert_eq!(m.cpu_read(0x8000), 0, "the first bank stays fixed");
    assert_eq!(m.cpu_read(0xC000), 5);
}

// Sunsoft-1 (mapper 184)

/// Bits 0-2 of a write select the 4 KB page at PPU $0000-$0FFF, bits 4-5 the one at $1000-$1FFF. CHR
/// here is 32 KB, so both windows point at real, different pages.
#[test]
fn sunsoft1_switches_both_chr_pages() {
    let mut m = Sunsoft1::new(rom_data(184, 2, 32));
    assert_eq!(m.ppu_read(0x0000), 0);
    assert_eq!(m.ppu_read(0x1000), 4, "the upper window starts at page 4");
    m.cpu_write(0x6000, 0x53);
    assert_eq!(m.ppu_read(0x0000), 3);
    assert_eq!(m.ppu_read(0x1000), 5);
}

/// The Sunsoft-1 IC never receives data bit 6 and ties it high, so the upper page is always 4-7; bits
/// 6 and 7 of the write reach nothing at all.
#[test]
fn sunsoft1_upper_page_never_drops_below_four() {
    let mut m = Sunsoft1::new(rom_data(184, 2, 32));
    m.cpu_write(0x6000, 0xC0);
    assert_eq!(m.ppu_read(0x1000), 4);
    m.cpu_write(0x6000, 0xF0);
    assert_eq!(m.ppu_read(0x1000), 7);
    assert_eq!(m.ppu_read(0x0000), 0, "bits 0-2 of 0xF0 are zero");
}

/// The 8 KB CHR-ROM the board shipped with wraps: page 5 is page 1 of two.
#[test]
fn sunsoft1_page_wraps_on_an_eight_kilobyte_chr() {
    let mut m = Sunsoft1::new(rom_data(184, 2, 8));
    m.cpu_write(0x6000, 0x15); // low page 5, high page 4 | 1 = 5
    assert_eq!(m.ppu_read(0x0000), 1);
    assert_eq!(m.ppu_read(0x1000), 1, "page 5 wraps to page 1");
}

#[test]
fn sunsoft1_chr_rom_is_read_only_and_chr_ram_takes_writes() {
    let mut rom = Sunsoft1::new(rom_data(184, 2, 8));
    rom.ppu_write(0x0000, 0x99);
    assert_eq!(rom.ppu_read(0x0000), 0);
    let mut ram = Sunsoft1::new(rom_data(184, 2, 0));
    ram.cpu_write(0x6000, 0x00);
    ram.ppu_write(0x1000, 0x99);
    assert_eq!(ram.ppu_read(0x1000), 0x99);
}

#[test]
fn sunsoft1_mirroring_stays_header_value() {
    let mut m = Sunsoft1::new(rom_data(184, 2, 8));
    m.cpu_write(0x6000, 0x53);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    assert!(m.prg_ram().is_empty(), "no PRG RAM on the board");
}

/// Mapper 184 is armed in `mapper::load`: flags6 = 0x81 (mapper bits 0-3 = 8, vertical mirroring),
/// flags7 = 0xB0 (mapper bits 4-7 = 0xB), two PRG banks and four CHR banks.
#[test]
fn load_builds_mapper_184() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 2, 4, 0x81, 0xB0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 2 * 16384 + 4 * 8192, 0);
    for (i, byte) in v.iter_mut().enumerate().skip(16 + 2 * 16384) {
        *byte = ((i - 16 - 2 * 16384) / 0x1000) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x6000, 0x53);
    assert_eq!(m.ppu_read(0x0000), 3);
    assert_eq!(m.ppu_read(0x1000), 5);
}

// prg_rom_offset: CPU address -> PRG-ROM byte under the current bank registers.

/// iNES image for `mapper` with `prg16` 16 KB PRG banks (each byte holds its bank number) and no CHR.
fn ines(mapper: u8, prg16: usize) -> Vec<u8> {
    let mut v = vec![
        0x4E,
        0x45,
        0x53,
        0x1A,
        prg16 as u8,
        0,
        (mapper & 0x0F) << 4,
        mapper & 0xF0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    for i in 0..prg16 * 0x4000 {
        v.push((i / 0x4000) as u8);
    }
    v
}

const PROBES: [u16; 4] = [0x8000, 0xBFFF, 0xC000, 0xFFFF];

/// Offsets for the four probe addresses, and each must name the byte `cpu_read` returns.
fn offsets(m: &mut Box<dyn Mapper>, prg16: usize) -> [usize; 4] {
    let mut out = [0; 4];
    for (o, &a) in out.iter_mut().zip(PROBES.iter()) {
        *o = m.prg_rom_offset(a).unwrap();
        assert_eq!(
            (*o / 0x4000) as u8 % prg16 as u8,
            m.cpu_read(a),
            "{a:#06x} names the wrong byte"
        );
    }
    assert_eq!(m.prg_rom_offset(0x7FFF), None);
    assert_eq!(m.prg_rom_offset(0x6000), None);
    out
}

#[test]
fn prg_rom_offset_default_is_none() {
    // A board that has not implemented the mapping (mapper 9, MMC2) keeps the None default.
    let m = load(&ines(9, 8)).unwrap();
    assert_eq!(m.prg_rom_offset(0x8000), None);
}

#[test]
fn prg_rom_offset_nrom_and_cnrom() {
    for mapper in [0, 3] {
        let mut m = load(&ines(mapper, 1)).unwrap();
        assert_eq!(offsets(&mut m, 1), [0, 0x3FFF, 0, 0x3FFF], "16 KB mirrors");
        let mut m = load(&ines(mapper, 2)).unwrap();
        m.cpu_write(0x8000, 1);
        assert_eq!(offsets(&mut m, 2), [0, 0x3FFF, 0x4000, 0x7FFF]);
    }
}

#[test]
fn prg_rom_offset_uxrom_and_camerica() {
    let mut m = load(&ines(2, 8)).unwrap();
    assert_eq!(offsets(&mut m, 8), [0, 0x3FFF, 7 * 0x4000, 0x1FFFF]);
    m.cpu_write(0x8000, 3);
    assert_eq!(
        offsets(&mut m, 8),
        [3 * 0x4000, 4 * 0x4000 - 1, 7 * 0x4000, 0x1FFFF]
    );
    let mut m = load(&ines(71, 8)).unwrap();
    m.cpu_write(0xC000, 2);
    assert_eq!(
        offsets(&mut m, 8),
        [2 * 0x4000, 3 * 0x4000 - 1, 7 * 0x4000, 0x1FFFF]
    );
}

#[test]
fn prg_rom_offset_32k_boards() {
    // (mapper, write that selects 32 KB bank 2 of 4 [or 1 for BNROM/GxROM's field])
    for (mapper, val, bank) in [
        (7u8, 0x02u8, 2usize),
        (34, 0x02, 2),
        (66, 0x20, 2),
        (11, 0x02, 2),
    ] {
        let mut m = load(&ines(mapper, 8)).unwrap();
        assert_eq!(
            offsets(&mut m, 8),
            [0, 0x3FFF, 0x4000, 0x7FFF],
            "mapper {mapper} power-on"
        );
        m.cpu_write(0x8000, val);
        let base = bank * 0x8000;
        assert_eq!(
            offsets(&mut m, 8),
            [base, base + 0x3FFF, base + 0x4000, base + 0x7FFF],
            "mapper {mapper}"
        );
    }
}

/// Five serial writes load an MMC1 register; `cpu_clock` between writes dodges the back-to-back rule.
fn mmc1_write(m: &mut Box<dyn Mapper>, addr: u16, val: u8) {
    for bit in 0..5 {
        m.cpu_write(addr, (val >> bit) & 1);
        m.cpu_clock();
        m.cpu_clock();
    }
}

#[test]
fn prg_rom_offset_mmc1_modes() {
    let mut m = load(&ines(1, 8)).unwrap();
    // Power-on mode 3: bank 0 switchable at $8000, last bank fixed at $C000.
    assert_eq!(offsets(&mut m, 8), [0, 0x3FFF, 7 * 0x4000, 0x1FFFF]);
    mmc1_write(&mut m, 0xE000, 3);
    assert_eq!(
        offsets(&mut m, 8),
        [3 * 0x4000, 4 * 0x4000 - 1, 7 * 0x4000, 0x1FFFF]
    );
    // Mode 2: first bank fixed at $8000, switchable at $C000.
    mmc1_write(&mut m, 0x8000, 0x08);
    assert_eq!(offsets(&mut m, 8), [0, 0x3FFF, 3 * 0x4000, 4 * 0x4000 - 1]);
    // Mode 0: 32 KB, the bank's low bit ignored (3 -> pair 2,3).
    mmc1_write(&mut m, 0x8000, 0x00);
    assert_eq!(
        offsets(&mut m, 8),
        [2 * 0x4000, 3 * 0x4000 - 1, 3 * 0x4000, 4 * 0x4000 - 1]
    );
}

/// FNV-1a over every PRG byte a board serves ($8000-$FFFF) for every value written to its
/// bank register, with each reported offset checked against the byte `cpu_read` returns. The
/// pinned digests were captured from the build before `prg_rom_offset` existed, so an
/// equal digest means the ROM image a game sees has not moved.
fn prg_digest(m: &mut dyn Mapper, prg: &[u8], write: u16) -> u64 {
    fn fold(d: &mut u64, b: u8) {
        *d = (*d ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    let mut d = 0xCBF2_9CE4_8422_2325u64;
    for value in 0..=255u8 {
        m.cpu_write(write, value);
        fold(&mut d, value);
        for addr in 0x8000..=0xFFFFu16 {
            let byte = m.cpu_read(addr);
            fold(&mut d, byte);
            let offset = m
                .prg_rom_offset(addr)
                .expect("the whole PRG window has an offset");
            assert_eq!(
                prg[offset], byte,
                "{addr:04X} after write {value:02X} names the wrong byte"
            );
        }
        assert_eq!(m.prg_rom_offset(0x7FFF), None);
        assert_eq!(m.prg_rom_offset(0x6000), None);
    }
    d
}

/// Offsets for the four probe addresses, each checked against the byte `cpu_read` returns in an
/// image whose banks are not uniform (`rom_data_ffff`), where `offsets` cannot be used.
fn prg_offsets(m: &mut dyn Mapper, prg: &[u8]) -> [usize; 4] {
    let mut out = [0; 4];
    for (o, &a) in out.iter_mut().zip(PROBES.iter()) {
        *o = m.prg_rom_offset(a).unwrap();
        assert_eq!(prg[*o], m.cpu_read(a), "{a:#06x} names the wrong byte");
    }
    assert_eq!(m.prg_rom_offset(0x7FFF), None);
    assert_eq!(m.prg_rom_offset(0x6000), None);
    out
}

#[test]
fn prg_rom_offset_six_discrete_latch_boards() {
    // 32 KB of fixed PRG on CPROM, J87 and Sunsoft-1 - two 16 KB banks, no register in the
    // mapping.
    let rom = rom_data_ffff(13, 2, 0);
    let (prg, mut m) = (rom.prg.clone(), Cprom::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 0x4000, 0x7FFF]);
    let rom = rom_data_ffff(87, 2, 8);
    let (prg, mut m) = (rom.prg.clone(), J87::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 0x4000, 0x7FFF]);
    let rom = rom_data_ffff(184, 2, 8);
    let (prg, mut m) = (rom.prg.clone(), Sunsoft1::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 0x4000, 0x7FFF]);
    // Sunsoft-2: bits 4-6 pick the 16 KB bank at $8000, the last bank is fixed at $C000.
    let rom = rom_data_ffff(93, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), Sunsoft2::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 3 * 0x4000, 0xFFFF]);
    m.cpu_write(0x8001, 0x20);
    assert_eq!(
        prg_offsets(&mut m, &prg),
        [2 * 0x4000, 2 * 0x4000 + 0x3FFF, 3 * 0x4000, 0xFFFF]
    );
    // UN1ROM: the same windows, the register shifted two bits.
    let rom = rom_data_ffff(94, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), Un1rom::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 3 * 0x4000, 0xFFFF]);
    m.cpu_write(0x8001, 0x08);
    assert_eq!(
        prg_offsets(&mut m, &prg),
        [2 * 0x4000, 2 * 0x4000 + 0x3FFF, 3 * 0x4000, 0xFFFF]
    );
    // UxROM inverted: the first bank is fixed at $8000, the register picks the one at $C000.
    let rom = rom_data_ffff(180, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), UnromInverted::new(rom));
    assert_eq!(prg_offsets(&mut m, &prg), [0, 0x3FFF, 0, 0x3FFF]);
    m.cpu_write(0x8001, 0x02);
    assert_eq!(
        prg_offsets(&mut m, &prg),
        [0, 0x3FFF, 2 * 0x4000, 2 * 0x4000 + 0x3FFF]
    );
}

#[test]
fn prg_image_of_the_six_discrete_latch_boards_is_unchanged() {
    let rom = rom_data_ffff(13, 2, 0);
    let (prg, mut m) = (rom.prg.clone(), Cprom::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x8001),
        0x332B_9E05_AE73_5525,
        "cprom"
    );
    let rom = rom_data_ffff(87, 2, 8);
    let (prg, mut m) = (rom.prg.clone(), J87::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x6001),
        0x332B_9E05_AE73_5525,
        "j87"
    );
    let rom = rom_data_ffff(184, 2, 8);
    let (prg, mut m) = (rom.prg.clone(), Sunsoft1::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x6001),
        0x332B_9E05_AE73_5525,
        "sunsoft1"
    );
    let rom = rom_data_ffff(93, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), Sunsoft2::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x8001),
        0x9317_087F_7CBC_05E5,
        "sunsoft2"
    );
    let rom = rom_data_ffff(94, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), Un1rom::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x8001),
        0xD227_3727_2A99_4B25,
        "un1rom"
    );
    let rom = rom_data_ffff(180, 4, 0);
    let (prg, mut m) = (rom.prg.clone(), UnromInverted::new(rom));
    assert_eq!(
        prg_digest(&mut m, &prg, 0x8001),
        0x1EE1_4581_221D_9325,
        "unrom inverted"
    );
}
