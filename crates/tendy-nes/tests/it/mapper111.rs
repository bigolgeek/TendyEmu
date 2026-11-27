//! The GTROM (mapper 111) board — the register at $5000-$5FFF / $7000-$7FFF, the
//! flash-backed 32 KB PRG window, CHR-RAM and nametable-RAM paging. Every expectation comes from
//! nesdev "GTROM".

use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::flash::{MANUFACTURER_ID, Part, SECTOR_SIZE};
use tendy_nes::mapper::gtrom::Gtrom;

/// A 512 KB image whose every byte holds its own 32 KB page number, so a read tells the test which
/// page the register selected. CHR-RAM, as every GTROM image has.
fn rom() -> RomData {
    RomData {
        header: Header {
            prg_banks: 32,
            chr_banks: 0,
            mapper: 111,
            ..Default::default()
        },
        prg: (0..512 * 1024).map(|i| (i / 0x8000) as u8).collect(),
        chr: vec![0; 8192],
        chr_is_ram: true,
        trainer: None,
    }
}

fn ciram() -> Vec<u8> {
    vec![0; 0x1000]
}

/// The flash part's command addresses are chip addresses: $5555 is CPU $D555 and $2AAA is CPU
/// $AAAA in whatever page is selected (nesdev "GTROM" § Self-flashing).
fn unlock(m: &mut Gtrom) {
    m.cpu_write(0xD555, 0xAA);
    m.cpu_write(0xAAAA, 0x55);
}

#[test]
fn the_register_selects_the_prg_chr_and_nametable_pages() {
    let mut m = Gtrom::new(rom());
    assert_eq!(m.cpu_read(0x8000), 0, "power-on selects page 0");
    assert_eq!(m.cpu_read(0xFFFF), 0);

    // Bits 0-3: one 32 KB page covers the whole $8000-$FFFF window.
    m.cpu_write(0x5000, 0x0A);
    assert_eq!(m.cpu_read(0x8000), 10);
    assert_eq!(m.cpu_read(0xC000), 10);
    assert_eq!(m.cpu_read(0xFFFF), 10);
    // $7000-$7FFF is the same register (nesdev "GTROM" § Bank select).
    m.cpu_write(0x7000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 3);
    // $6000-$6FFF is not: A12 is low there, and the board has no PRG RAM at all.
    m.cpu_write(0x6000, 0x0F);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0x6000), 0);

    // Bit 4: CHR RAM is two 8 KB pages (nesdev "GTROM" § CHR RAM).
    m.cpu_write(0x5000, 0x00);
    m.ppu_write(0x0000, 0x11);
    m.ppu_write(0x1000, 0x22);
    m.cpu_write(0x5000, 0x10);
    assert_eq!(
        (m.ppu_read(0x0000), m.ppu_read(0x1000)),
        (0, 0),
        "the other page starts empty"
    );
    m.ppu_write(0x0000, 0x33);
    m.cpu_write(0x5000, 0x00);
    assert_eq!((m.ppu_read(0x0000), m.ppu_read(0x1000)), (0x11, 0x22));

    // Bit 5: the nametables are two 8 KB pages (nesdev "GTROM" § VRAM).
    let mut ciram = ciram();
    m.nt_write(0x2000, 1, &mut ciram);
    m.cpu_write(0x5000, 0x20);
    assert_eq!(m.nt_read(0x2000, &ciram), Some(0));
    m.nt_write(0x2000, 2, &mut ciram);
    m.cpu_write(0x5000, 0x00);
    assert_eq!(m.nt_read(0x2000, &ciram), Some(1));
}

#[test]
fn four_screens_and_the_bonus_ram_are_distinct_bytes() {
    let mut m = Gtrom::new(rom());
    let mut ciram = ciram();
    assert_eq!(
        m.mirroring(),
        Mirroring::FourScreen,
        "the arrangement is fixed, not switchable"
    );

    let tables = [0x2000u16, 0x2400, 0x2800, 0x2C00];
    for (i, &addr) in tables.iter().enumerate() {
        assert!(m.nt_write(addr, i as u8, &mut ciram));
    }
    for (i, &addr) in tables.iter().enumerate() {
        assert_eq!(
            m.nt_read(addr, &ciram),
            Some(i as u8),
            "table {i} at {addr:#06X}"
        );
        assert_eq!(
            m.nt_peek(addr, &ciram),
            Some(i as u8),
            "the viewer sees the same byte"
        );
    }

    // $3000-$3EFF is real RAM here, not a mirror of $2000-$2EFF (nesdev "GTROM" § Bonus RAM).
    m.nt_write(0x3005, 0x42, &mut ciram);
    assert_eq!(m.nt_read(0x2005, &ciram), Some(0));
    assert_eq!(m.nt_read(0x3005, &ciram), Some(0x42));
    assert_eq!(m.nt_peek(0x3005, &ciram), Some(0x42));

    // The bonus RAM is per nametable page, like the rest of the page.
    m.cpu_write(0x5000, 0x20);
    assert_eq!(m.nt_read(0x3005, &ciram), Some(0));
}

#[test]
fn software_id_reads_the_part_through_the_prg_window() {
    let mut m = Gtrom::new(rom());
    unlock(&mut m);
    m.cpu_write(0xD555, 0x90);
    // The ID is read at chip addresses 0 and 1 (DS20005022F Table 3-1), which page 0 puts at
    // CPU $8000/$8001.
    assert_eq!(m.cpu_read(0x8000), MANUFACTURER_ID);
    assert_eq!(
        m.cpu_read(0x8001),
        Part::Sf040.device_id(),
        "the 512 KB part GTROM solders down"
    );
    assert_eq!(
        m.cpu_read(0x8002),
        0,
        "the array still reads through elsewhere"
    );

    // A single write of $F0 leaves ID mode (DS20005022F Table 4-2 note 6).
    m.cpu_write(0x8002, 0xF0);
    assert_eq!(m.cpu_read(0x8000), 0);
}

#[test]
fn a_program_cycle_through_the_board_dirties_one_sector() {
    let mut m = Gtrom::new(rom());
    // Chip $12345: page 2's byte $2345, which CPU $A345 reads with page 2 selected.
    let chip = 0x1_2345u32;
    m.cpu_write(0x5000, (chip / 0x8000) as u8);
    let cpu = 0x8000 + (chip & 0x7FFF) as u16;

    // Sector erase (Table 4-2): AA/55/80 then AA/55/30 at the sector's address.
    unlock(&mut m);
    m.cpu_write(0xD555, 0x80);
    unlock(&mut m);
    m.cpu_write(cpu, 0x30);
    assert_eq!(m.cpu_read(cpu), 0xFF, "the sector comes back erased");

    // Byte program: AA/55/A0 then the address and the data.
    unlock(&mut m);
    m.cpu_write(0xD555, 0xA0);
    m.cpu_write(cpu, 0x5A);

    assert_eq!(
        m.cpu_read(cpu),
        0x5A,
        "the programmed byte reads back through the window"
    );
    assert_eq!(
        m.flash().image()[chip as usize],
        0x5A,
        "...and it is the byte on the chip"
    );
    assert_eq!(
        m.flash().dirty_sectors().collect::<Vec<_>>(),
        vec![chip as usize / SECTOR_SIZE],
        "the one sector the game touched is the one waiting to be written back"
    );
}

#[test]
fn a_new_board_pads_a_short_image_to_the_part() {
    // A homebrew image smaller than the 512 KB part leaves the rest of the chip erased, so a read of
    // a page past the image is $FF rather than a panic or a wrapped copy of the image.
    let mut rom = rom();
    rom.prg.truncate(64 * 1024);
    let mut m = Gtrom::new(rom);
    assert_eq!(m.cpu_read(0x8000), 0);
    m.cpu_write(0x5000, 0x0F);
    assert_eq!(m.cpu_read(0x8000), 0xFF);
}
