//! A cartridge image whose program ROM is missing or too small for its board must fail at
//! load with a clean error. Before the fix these images loaded and the first `$8000` read panicked
//! inside the board's bank arithmetic (divide by zero, or an index into an empty image).

use tendy_nes::mapper::load;

/// An iNES 1.0 image for `mapper`: `prg_banks` banks of PRG-ROM and one bank of CHR-ROM.
fn rom(prg_banks: u8, mapper: u16) -> Vec<u8> {
    let mut v = vec![
        0x4E,
        0x45,
        0x53,
        0x1A,
        prg_banks,
        1,
        ((mapper & 0x0F) as u8) << 4,
        (mapper & 0xF0) as u8,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    v.resize(16 + usize::from(prg_banks) * 16384 + 8192, 0);
    v
}

#[test]
fn a_rom_with_no_program_data_is_a_load_error() {
    // Each of these divides by the PRG length on its first read, so an empty image used to panic
    // with "attempt to calculate the remainder with a divisor of zero".
    for mapper in [0, 1, 2, 3, 4, 7] {
        assert!(
            load(&rom(0, mapper)).is_err(),
            "mapper {mapper} must reject an image with no PRG"
        );
    }
}

#[test]
fn a_32k_window_board_rejects_a_16k_image() {
    // AxROM, Color Dreams, BNROM and GxROM count their banks as `prg.len() / 0x8000`, which is
    // zero for a single 16 KB bank: the first read used to panic on the divide.
    for mapper in [7, 11, 34, 66] {
        assert!(
            load(&rom(1, mapper)).is_err(),
            "mapper {mapper} must reject a 16 KB PRG"
        );
        assert!(
            load(&rom(2, mapper)).is_ok(),
            "mapper {mapper} still takes a 32 KB PRG"
        );
    }
}
