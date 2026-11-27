//! Mapper 30 (UNROM 512) through the board, not the chip: nesdev "UNROM 512" prints every flash
//! cycle as a register write plus a write in the `$8000-$BFFF` window, and these tests drive
//! exactly those two addresses.

use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::flash;
use tendy_nes::mapper::unrom512::Unrom512;

/// A 16 KB-bank PRG where every byte holds its bank's number, so a read says which bank the
/// window shows; 8 KB of CHR-RAM, which the board grows to its 32 KB.
fn rom(banks: usize, battery: bool, mirroring: Mirroring) -> RomData {
    RomData {
        header: Header {
            prg_banks: banks,
            chr_banks: 0,
            mapper: 30,
            mirroring,
            battery,
            trainer: false,
            ..Default::default()
        },
        prg: (0..banks * 0x4000).map(|i| (i / 0x4000) as u8).collect(),
        chr: vec![0; 8192],
        chr_is_ram: true,
        trainer: None,
    }
}

/// [`rom`] with every byte after a bank's first set to `$FF`, so a register write at such an
/// address latches its value whole (the documented way round a bus conflict).
fn rom_ffff(banks: usize, battery: bool, mirroring: Mirroring) -> RomData {
    let mut r = rom(banks, battery, mirroring);
    for bank in 0..banks {
        r.prg[bank * 0x4000] = bank as u8;
        r.prg[bank * 0x4000 + 1..(bank + 1) * 0x4000].fill(0xFF);
    }
    r
}

/// One cycle of nesdev's "Flash ROM Save" list: the register write that puts `bank` in the window,
/// then the flash write at `addr`.
fn cycle(m: &mut Unrom512, bank: u8, addr: u16, val: u8) {
    m.cpu_write(0xC000, bank);
    m.cpu_write(addr, val);
}

/// AC1 (docs/spec-features-hardware.md): a software-ID entry sequence through the board's
/// `$8000` window answers the manufacturer ID.
#[test]
fn software_id_through_the_window_answers_the_manufacturer() {
    // 256 KB PRG, so the part is the SST39SF020A and its own ID is $B6.
    let mut m = Unrom512::new(rom(16, true, Mirroring::Horizontal));

    // nesdev: `$C000:$01, $9555:$AA`, `$C000:$00, $AAAA:$55`, `$C000:$01, $9555:$90`.
    cycle(&mut m, 0x01, 0x9555, 0xAA);
    cycle(&mut m, 0x00, 0xAAAA, 0x55);
    cycle(&mut m, 0x01, 0x9555, 0x90);

    // Table 3-1 note 5: the ID is read with AMS-A1 = 0, so chip addresses 0 and 1, which in bank 0
    // are $8000 and $8001.
    m.cpu_write(0xC000, 0x00);
    assert_eq!(m.cpu_read(0x8000), flash::MANUFACTURER_ID);
    assert_eq!(m.cpu_read(0x8001), 0xB6);

    // `$C000:$01, $9555:$F0` leaves ID mode and the array answers again.
    cycle(&mut m, 0x01, 0x9555, 0xF0);
    m.cpu_write(0xC000, 0x00);
    assert_eq!(m.cpu_read(0x8000), 0);
}

/// AC2: erase a sector, program a byte through the board's own sequence, read it back, and see the
/// sector in `Flash::dirty_sectors`.
#[test]
fn a_programmed_byte_reads_back_and_dirties_its_sector() {
    let mut m = Unrom512::new(rom(16, true, Mirroring::Horizontal));

    // nesdev's six-cycle "Erase 4KB Flash Sector", the last cycle addressing sector 1 of the chip
    // ($1234 in bank 0 is $9234 on the bus).
    cycle(&mut m, 0x01, 0x9555, 0xAA);
    cycle(&mut m, 0x00, 0xAAAA, 0x55);
    cycle(&mut m, 0x01, 0x9555, 0x80);
    cycle(&mut m, 0x01, 0x9555, 0xAA);
    cycle(&mut m, 0x00, 0xAAAA, 0x55);
    cycle(&mut m, 0x00, 0x9234, 0x30);
    m.cpu_write(0xC000, 0x00);
    assert_eq!(m.cpu_read(0x9234), flash::ERASED);
    assert_eq!(
        m.flash().unwrap().dirty_sectors().collect::<Vec<_>>(),
        vec![1]
    );

    // nesdev's four-cycle "Write a byte": $AA, $55, $A0, then the bank and the data.
    cycle(&mut m, 0x01, 0x9555, 0xAA);
    cycle(&mut m, 0x00, 0xAAAA, 0x55);
    cycle(&mut m, 0x01, 0x9555, 0xA0);
    cycle(&mut m, 0x00, 0x9234, 0x5A);

    m.cpu_write(0xC000, 0x00);
    assert_eq!(m.cpu_read(0x9234), 0x5A);
    assert_eq!(
        m.flash().unwrap().read(0x1234),
        0x5A,
        "the same byte at its chip address"
    );
    assert_eq!(
        m.flash().unwrap().dirty_sectors().collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn the_latch_selects_the_prg_bank_and_the_fixed_window_is_the_last_bank() {
    let mut m = Unrom512::new(rom(16, true, Mirroring::Horizontal));
    assert_eq!(m.cpu_read(0x8000), 0, "bank 0 at power-on");
    assert_eq!(m.cpu_read(0xC000), 15, "the last bank is fixed at $C000");

    m.cpu_write(0xC000, 0x05); // register bits 0-4: PRG bank 5
    assert_eq!(m.cpu_read(0x8000), 5);
    assert_eq!(m.cpu_read(0xBFFF), 5);
    assert_eq!(m.cpu_read(0xC000), 15);
}

#[test]
fn the_latch_selects_the_chr_ram_bank() {
    let mut m = Unrom512::new(rom(16, true, Mirroring::Horizontal));
    assert_eq!(
        m.chr_ram().len(),
        32 * 1024,
        "the board has 32 KB of CHR-RAM"
    );

    m.ppu_write(0x0010, 0x11);
    m.cpu_write(0xC000, 0x20); // register bits 5-6: CHR bank 1
    m.ppu_write(0x0010, 0x22);
    m.cpu_write(0xC000, 0x00);
    assert_eq!(m.ppu_read(0x0010), 0x11, "each bank keeps its own byte");
    m.cpu_write(0xC000, 0x60); // bits 5-6 = 3
    assert_eq!(m.ppu_read(0x0010), 0x00);
}

/// The header's one-screen arrangement is steered by register bit 7 (nesdev:
/// "0: Lower bank (CIRAM A10=0), 1: Upper bank (CIRAM A10=1)").
#[test]
fn register_bit_7_swaps_the_one_screen_bank() {
    let mut m = Unrom512::new(rom(16, true, Mirroring::FourScreen));
    assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
    m.cpu_write(0xC000, 0x80);
    assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
}

#[test]
fn a_hard_wired_header_ignores_register_bit_7() {
    for mirroring in [Mirroring::Horizontal, Mirroring::Vertical] {
        let mut m = Unrom512::new(rom(16, true, mirroring));
        m.cpu_write(0xC000, 0x80);
        assert_eq!(m.mirroring(), mirroring);
    }
}

/// nesdev "Bus conflict": without the battery bit the board has the conflict-prevention circuit
/// absent, so the register takes the value ANDed with the PRG byte the bus drives.
#[test]
fn a_non_flashable_board_conflicts_with_the_prg_bus() {
    let mut r = rom_ffff(16, false, Mirroring::Horizontal);
    // Bank 0's byte at $C100's window offset ($0100) differs from the last bank's $FF there, so
    // the latch below says which of the two the write conflicted with.
    r.prg[0x0100] = 0x00;
    let mut m = Unrom512::new(r);
    assert!(
        m.flash().is_none(),
        "the battery bit is what makes the PRG a flash chip"
    );

    // Bank 0's first byte is 0, so a write of $FF there latches 0 and the window stays on bank 0.
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 0);

    // At an $FF byte in the fixed bank the register takes the value whole: the partner for a
    // write at $C000-$FFFF is the last bank's byte, never the window's $00 at $0100.
    m.cpu_write(0xC100, 0x07);
    assert_eq!(m.cpu_read(0x8000), 7);
}

/// Submapper 3 is the one board whose register bit 7 picks horizontal or vertical instead of a
/// one-screen half (nesdev: "CIRAM A10=PPU A11" against "CIRAM A10=PPU A10").
#[test]
fn submapper_3_takes_horizontal_or_vertical_from_register_bit_7() {
    let mut r = rom(16, true, Mirroring::Vertical);
    r.header.nes2 = true;
    r.header.submapper = 3;
    let mut m = Unrom512::new(r);

    // The header's arrangement is ignored, as nesdev says of submapper 3.
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
    m.cpu_write(0xC000, 0x80);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}
