use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::cnrom::Cnrom;
use tendy_nes::mapper::load;
use tendy_nes::mapper::mmc1::Mmc1;
use tendy_nes::mapper::uxrom::Uxrom;
use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;

use crate::common::rom_file;
use crate::common::run_blargg;

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

/// Load a 5-bit value into an MMC1 register through the serial port (LSB first).
fn mmc1_write(m: &mut Mmc1, addr: u16, val: u8) {
    for i in 0..5 {
        m.cpu_write(addr, (val >> i) & 1);
    }
}

fn ines(flags6: u8) -> Vec<u8> {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 2, 2, flags6, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 2 * 16384 + 2 * 8192, 0);
    v
}

#[test]
fn mmc1_power_on_fixes_last_bank() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 7);
}

#[test]
fn mmc1_switches_16k_at_8000() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut m, 0xE000, 3);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xC000), 7);
}

#[test]
fn mmc1_mode_2_fixes_first_bank() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut m, 0x8000, 0x08);
    mmc1_write(&mut m, 0xE000, 5);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 5);
}

#[test]
fn mmc1_32k_mode_ignores_low_bit() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut m, 0x8000, 0x00);
    mmc1_write(&mut m, 0xE000, 5);
    assert_eq!(m.cpu_read(0x8000), 4);
    assert_eq!(m.cpu_read(0xC000), 5);
}

#[test]
fn mmc1_reset_write_restores_mode_3() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut m, 0x8000, 0x00);
    m.cpu_write(0x8000, 0x80);
    mmc1_write(&mut m, 0xE000, 2);
    assert_eq!(m.cpu_read(0x8000), 2);
    assert_eq!(m.cpu_read(0xC000), 7);
}

#[test]
fn mmc1_mirroring_control() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut m, 0x8000, 0x02);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    mmc1_write(&mut m, 0x8000, 0x03);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
    mmc1_write(&mut m, 0x8000, 0x00);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
}

#[test]
fn mmc1_chr_8k_mode() {
    let mut m = Mmc1::new(rom_data(1, 8, 32));
    mmc1_write(&mut m, 0x8000, 0x0C);
    mmc1_write(&mut m, 0xA000, 2);
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x1000), 3);
}

#[test]
fn mmc1_chr_4k_mode() {
    let mut m = Mmc1::new(rom_data(1, 8, 32));
    mmc1_write(&mut m, 0x8000, 0x1C);
    mmc1_write(&mut m, 0xA000, 1);
    mmc1_write(&mut m, 0xC000, 6);
    assert_eq!(m.ppu_read(0x0000), 1);
    assert_eq!(m.ppu_read(0x1000), 6);
}

#[test]
fn mmc1_prg_ram() {
    let mut m = Mmc1::new(rom_data(1, 8, 0));
    m.cpu_write(0x6000, 0x5A);
    assert_eq!(m.cpu_read(0x6000), 0x5A);
}

#[test]
fn uxrom_switches_low_bank() {
    let mut m = Uxrom::new(rom_data(2, 8, 0));
    m.cpu_write(0x8000, 3);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xC000), 7);
    m.cpu_write(0x8000, 9);
    assert_eq!(m.cpu_read(0x8000), 1);
}

#[test]
fn uxrom_chr_ram_is_writable() {
    let mut m = Uxrom::new(rom_data(2, 8, 0));
    m.ppu_write(0x0123, 7);
    assert_eq!(m.ppu_read(0x0123), 7);
}

#[test]
fn cnrom_switches_chr() {
    let mut m = Cnrom::new(rom_data(3, 2, 32));
    m.cpu_write(0x8000, 2);
    assert_eq!(m.ppu_read(0x0000), 4);
    assert_eq!(m.ppu_read(0x1000), 5);
}

#[test]
fn cnrom_prg_is_fixed() {
    let mut m = Cnrom::new(rom_data(3, 2, 32));
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
}

#[test]
fn cnrom_chr_is_read_only() {
    let mut m = Cnrom::new(rom_data(3, 2, 32));
    m.ppu_write(0x0000, 99);
    assert_eq!(m.ppu_read(0x0000), 0);
}

#[test]
fn load_supports_mappers_0_to_5() {
    assert!(load(&ines(0x00)).is_ok());
    assert!(load(&ines(0x10)).is_ok());
    assert!(load(&ines(0x20)).is_ok());
    assert!(load(&ines(0x30)).is_ok());
    assert!(load(&ines(0x40)).is_ok());
    assert!(load(&ines(0x50)).is_ok());
    assert!(load(&ines(0x60)).is_err());
}

#[test]
fn blargg_all_instrs_on_mmc1() {
    let (status, text) = run_blargg("all_instrs.nes", 4000);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_official_only_on_mmc1() {
    let (status, text) = run_blargg("official_only.nes", 4000);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn mmc1_a12_title_screen() {
    let mut nes = Nes::new(&rom_file("mmc1_a12.nes")).unwrap();
    for _ in 0..300 {
        nes.run_frame();
    }
    // Reviewed snapshot: "MMC1 WRAM DISABLE SCANLINE COUNTER TEST (C) 2010 BREGALAD" over the blue test pattern.
    assert_eq!(fnv1a64(&nes.bus.ppu.framebuffer), 0x91af_e700_ff40_413a);
}
