use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::mmc1::Mmc1;

/// MMC1 cartridge with `prg_banks` 16 KB PRG banks where every byte holds its bank number, and 8 KB CHR-RAM.
fn board(prg_banks: usize) -> Mmc1 {
    Mmc1::new(RomData {
        header: Header {
            prg_banks,
            chr_banks: 0,
            mapper: 1,
            mirroring: Mirroring::Vertical,
            battery: false,
            trainer: false,
            ..Default::default()
        },
        prg: (0..prg_banks * 0x4000)
            .map(|i| (i / 0x4000) as u8)
            .collect(),
        chr: vec![0; 8192],
        chr_is_ram: true,
        trainer: None,
    })
}

/// Load a 5-bit value into an MMC1 register through the serial port (LSB first).
fn mmc1_write(m: &mut Mmc1, addr: u16, val: u8) {
    for i in 0..5 {
        m.cpu_write(addr, (val >> i) & 1);
    }
}

#[test]
fn power_on_fixes_last_bank_of_lower_half() {
    let mut m = board(32);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 15);
}

#[test]
fn chr0_bit_4_selects_upper_half() {
    let mut m = board(32);
    mmc1_write(&mut m, 0xA000, 0x10);
    assert_eq!(m.cpu_read(0x8000), 16);
    assert_eq!(m.cpu_read(0xC000), 31);
    mmc1_write(&mut m, 0xE000, 3);
    assert_eq!(m.cpu_read(0x8000), 19);
}

#[test]
fn switchable_bank_stays_in_lower_half() {
    let mut m = board(32);
    mmc1_write(&mut m, 0xE000, 3);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xC000), 15);
}

#[test]
fn upper_half_in_32k_mode() {
    let mut m = board(32);
    mmc1_write(&mut m, 0x8000, 0x00);
    mmc1_write(&mut m, 0xA000, 0x10);
    mmc1_write(&mut m, 0xE000, 4);
    assert_eq!(m.cpu_read(0x8000), 20);
    assert_eq!(m.cpu_read(0xC000), 21);
}

#[test]
fn small_boards_ignore_bit_4() {
    let mut m = board(8);
    mmc1_write(&mut m, 0xA000, 0x10);
    assert_eq!(m.cpu_read(0xC000), 7);
    assert_eq!(m.cpu_read(0x8000), 0);
}
