//! The three address maps and the SRAM windows (`tests/map.rs`).

mod common;

use tendy_snes::addr::Addr;
use tendy_snes::cart::{Board, Cartridge};

#[test]
fn lorom_decodes_8000_to_ffff() {
    // §8.3: LoROM wires A0-A14 and leaves A15 unconnected, so the image offset is
    // `((bank & 0x7F) << 15) | (offset & 0x7FFF)` (fullsnes "SNES Cart LoROM Mapping").
    let mask = 0x3F_FFFF;
    let cases: &[(u8, u16, usize)] = &[
        (0x00, 0x8000, 0x00_0000),
        (0x00, 0xFFFF, 0x00_7FFF),
        (0x01, 0x8000, 0x00_8000),
        (0x01, 0xFFFF, 0x00_FFFF),
        (0x7D, 0x8000, 0x3E_8000),
        (0x7F, 0xFFFF, 0x3F_FFFF),
    ];
    for &(bank, offset, expected) in cases {
        assert_eq!(
            Board::LoRom.rom_offset(Addr::new(bank, offset), mask),
            expected,
            "LoROM ${bank:02X}:{offset:04X} decodes from ((bank & $7F) << 15) | (offset & $7FFF)"
        );
    }

    // The dispatch agrees: A15 is not wired, so the same image byte answers in both halves of a
    // bank, and the next bank continues at the next 32 KiB.
    let mut cart = Cartridge::new(common::synth_lorom(0x4_0000, &[0xEA])).unwrap();
    cart.rom[0x1234] = 0x5A;
    cart.rom[0x9234] = 0xA5;
    assert_eq!(cart.read(Addr::new(0x00, 0x9234)), 0x5A);
    assert_eq!(cart.read(Addr::new(0x01, 0x9234)), 0xA5);
}

#[test]
fn lorom_mirrors_banks_40_to_7d() {
    // §8.3: A15 is not connected, so banks $40-$7D see the same 32 KiB window as $00-$3F; on a
    // small image the mask is what folds the extra bank bits away.
    let mut cart = Cartridge::new(common::synth_lorom(0x8000, &[0xEA])).unwrap();
    assert_eq!(cart.rom_mask, 0x7FFF);
    cart.rom[0x1234] = 0x5A;
    for bank in [0x00u8, 0x40, 0x41, 0x70, 0x7D] {
        // $8000-$FFFF is the ROM region in every one of these banks; $9234 -> image $1234.
        assert_eq!(
            cart.read(Addr::new(bank, 0x9234)),
            0x5A,
            "bank ${bank:02X}:9234 mirrors $00:9234 on a 32 KiB image"
        );
    }
}

#[test]
fn hirom_is_linear() {
    // §8.3: HiROM wires A0-A21 straight through and leaves A22/A23 unconnected, so the offset is
    // `addr & $3FFFFF`: banks $40-$7D continue the image and $80-$BF mirror $C0-$FF.
    let mask = 0x3F_FFFF;
    let cases: &[(u8, u16, usize)] = &[
        (0xC0, 0x0000, 0x00_0000),
        (0xC0, 0xFFFF, 0x00_FFFF),
        (0xC1, 0x0000, 0x01_0000),
        (0x40, 0x1234, 0x00_1234),
        (0x80, 0x1234, 0x00_1234),
        (0xFF, 0xFFFF, 0x3F_FFFF),
    ];
    for &(bank, offset, expected) in cases {
        assert_eq!(
            Board::HiRom.rom_offset(Addr::new(bank, offset), mask),
            expected,
            "HiROM ${bank:02X}:{offset:04X} decodes from addr & $3FFFFF"
        );
    }

    let mut cart = Cartridge::new(common::synth_hirom(0x2_0000, &[0xEA])).unwrap();
    cart.rom[0x1_0000] = 0x5A;
    assert_eq!(cart.read(Addr::new(0xC1, 0x0000)), 0x5A);
    assert_eq!(cart.read(Addr::new(0x41, 0x0000)), 0x5A, "$40-$7D continue");
    assert_eq!(cart.read(Addr::new(0x81, 0x0000)), 0x5A, "$80-$BF mirror");
}

#[test]
fn exhirom_header_is_at_40ffc0() {
    // §8.1: an ExHiROM header sits at CPU $40:FFC0, image offset $40FFC0 (fullsnes "SNES Cartridge
    // ROM Header"; SNESdev `ROM header`). §8.6 records this offset as inferred from the pin table.
    let cart =
        Cartridge::new(common::synth_cart(Board::ExHiRom, 0x80_0000, 0, 0, &[0xEA])).unwrap();
    assert_eq!(cart.board, Board::ExHiRom);
    assert_eq!(cart.header.header_offset, 0x40FFC0);
    assert_eq!(cart.header.map_mode, 5);
    assert_eq!(cart.header.title, "SNES-TEST");
}

#[test]
fn exhirom_second_half_starts_at_4mib() {
    // §8.3 (and §8.6's UNVERIFIED row): A23 drives the ROM's A22 inverted, so banks $80-$FF see
    // the first 4 MiB of an 8 MiB image and banks $00-$7F the second.
    let mask = 0x7F_FFFF;
    let cases: &[(u8, u16, usize)] = &[
        (0x00, 0x0000, 0x40_0000),
        (0x7F, 0xFFFF, 0x7F_FFFF),
        (0x80, 0x0000, 0x00_0000),
        (0xFF, 0xFFFF, 0x3F_FFFF),
    ];
    for &(bank, offset, expected) in cases {
        assert_eq!(
            Board::ExHiRom.rom_offset(Addr::new(bank, offset), mask),
            expected,
            "ExHiROM ${bank:02X}:{offset:04X} is base + (addr & $3FFFFF) with the base inverted by A23"
        );
    }

    // And the bytes: a marker at 4 MiB shows at $00:0000, not at $80:0000.
    let mut rom = common::synth_cart(Board::ExHiRom, 0x80_0000, 0, 0, &[0xEA]);
    rom[0x00_0000] = 0xA5;
    rom[0x40_0000] = 0x5A;
    let mut cart = Cartridge::new(rom).unwrap();
    assert_eq!(cart.read(Addr::new(0x00, 0x0000)), 0x5A);
    assert_eq!(cart.read(Addr::new(0x80, 0x0000)), 0xA5);
}

#[test]
fn lorom_sram_window() {
    // §8.3's LoROM SRAM table: banks $70-$7D and $F0-$FF at $0000-$7FFF, index
    // `((bank & 0x0F) << 15 | (offset & $7FFF)) & (ram_len - 1)`. A 64 KiB chip is the size where
    // the bank term does real work: bank $71's 32 KiB chunk lands at index $8000.
    let rom = common::synth_cart(Board::LoRom, 0x8000, 0x02, 6, &[0xEA]);
    let mut cart = Cartridge::new(rom).expect("ROM+RAM+battery loads");
    assert_eq!(cart.ram.len(), 64 * 1024);

    cart.write(Addr::new(0x70, 0x0000), 0x11);
    cart.write(Addr::new(0x71, 0x0000), 0x22);
    assert!(cart.ram_dirty);
    assert_eq!(cart.read(Addr::new(0x70, 0x0000)), 0x11);
    assert_eq!(
        cart.read(Addr::new(0x71, 0x0000)),
        0x22,
        "bank $71 addresses the second 32 KiB chunk"
    );
    assert_eq!(
        cart.read(Addr::new(0xF1, 0x0000)),
        0x22,
        "banks $F0-$FF are the same window, because only bank & $0F survives"
    );
    assert_eq!(
        cart.read(Addr::new(0x7D, 0x7FFF)),
        0x00,
        "bank $7D is index ($0D << 15) | $7FFF = $6FFFF, masked to $FFFF by a 64 KiB chip"
    );
    // Outside the window a write lands nowhere: $70:8000 is the ROM region of that bank.
    let mapped = cart.rom[0x0000];
    cart.write(Addr::new(0x70, 0x8000), 0x33);
    assert_eq!(cart.read(Addr::new(0x70, 0x8000)), mapped);
}

#[test]
fn hirom_sram_window() {
    // §8.3's HiROM SRAM table: banks $20-$3F and $A0-$BF at $6000-$7FFF, index
    // `((bank & 0x1F) << 13 | (offset & $1FFF)) & (ram_len - 1)`.
    let rom = common::synth_cart(Board::HiRom, 0x1_0000, 0x02, 5, &[0xEA]);
    let mut cart = Cartridge::new(rom).expect("ROM+RAM+battery loads");
    assert_eq!(cart.ram.len(), 32 * 1024);

    cart.write(Addr::new(0x20, 0x6000), 0x11);
    cart.write(Addr::new(0x21, 0x6000), 0x22);
    assert_eq!(cart.read(Addr::new(0x20, 0x6000)), 0x11);
    assert_eq!(
        cart.read(Addr::new(0x21, 0x6000)),
        0x22,
        "bank $21 is the next 8 KiB chunk"
    );
    assert_eq!(
        cart.read(Addr::new(0xA1, 0x6000)),
        0x22,
        "banks $A0-$BF mirror $20-$3F"
    );
    // $6000-$5FFF is ROM on this board, not SRAM.
    let mapped = cart.rom[0x6000];
    cart.write(Addr::new(0x20, 0x5FFF), 0x33);
    assert_eq!(cart.read(Addr::new(0x20, 0x5FFF)), mapped);
}

#[test]
fn small_sram_mirrors_inside_the_window() {
    // §8.3: "a 2 KiB cartridge repeats its 2 KiB through the 8 KiB window" — the mask is the
    // mirroring, not a length check.
    let rom = common::synth_cart(Board::LoRom, 0x8000, 0x02, 1, &[0xEA]);
    let mut cart = Cartridge::new(rom).unwrap();
    assert_eq!(cart.ram.len(), 2 * 1024);
    cart.write(Addr::new(0x70, 0x0000), 0x5A);
    for at in [0x0800u16, 0x1000, 0x7800] {
        assert_eq!(
            cart.read(Addr::new(0x70, at)),
            0x5A,
            "$70:{at:04X} mirrors $70:0000 through a 2 KiB chip"
        );
    }
    assert_eq!(cart.read(Addr::new(0x71, 0x0000)), 0x5A, "and bank $71 too");
    // Writing one mirror writes the chip: the byte at $70:0000 changes as well.
    cart.write(Addr::new(0x70, 0x0800), 0x3C);
    assert_eq!(cart.read(Addr::new(0x70, 0x0000)), 0x3C);
}

#[test]
fn a_lorom_board_with_a_chip_answers_the_eight_kilobyte_window() {
    // fullsnes "SNES Memory", §Battery-backed SRAM: "HiROM ---> SRAM at 30h-3Fh,B0h-BFh:6000h-7FFFh
    // ;small 8K SRAM bank(s)", followed by "SRAM is usually also mirrored to both WS1 and WS2 areas.
    // SRAM in bank 30h-3Fh is often also mirrored to 20h-2Fh". A LoROM board with a chip can be wired
    // to the same window: sixteen 8 KiB banks is exactly a 128 KiB chip, which is what Gamars Puzzle
    // declares and what it writes a block to at $31:6000 before reading it back to verify.
    let rom = common::synth_cart(Board::LoRom, 0x10_0000, 0x02, 7, &[0xEA]);
    let mut cart = Cartridge::new(rom).expect("ROM+RAM+battery loads");
    assert_eq!(cart.ram.len(), 128 * 1024);

    cart.write(Addr::new(0x30, 0x6000), 0x10);
    cart.write(Addr::new(0x31, 0x6000), 0x11);
    cart.write(Addr::new(0x3F, 0x6000), 0x3F);
    assert!(cart.ram_dirty);
    assert_eq!(cart.read(Addr::new(0x31, 0x6000)), 0x11);
    assert_eq!(
        cart.read(Addr::new(0x30, 0x6000)),
        0x10,
        "bank $30 is index ($10 << 13) & $1FFFF = 0"
    );
    assert_eq!(
        cart.read(Addr::new(0x3F, 0x6000)),
        0x3F,
        "bank $3F is index ($1F << 13) & $1FFFF = $1E000"
    );
    assert_eq!(
        cart.read(Addr::new(0xB1, 0x6000)),
        0x11,
        "banks $A0-$BF mirror $20-$3F"
    );
    // Inside the bank but outside the window, the address is not the chip's: $31:0000 is the
    // expansion half of a LoROM bank, and a write there is dropped rather than landing in SRAM.
    let before = cart.read(Addr::new(0x31, 0x0000));
    cart.write(Addr::new(0x31, 0x0000), 0x99);
    assert_eq!(
        cart.read(Addr::new(0x31, 0x0000)),
        before,
        "a write below $6000 in the same bank is not the chip's"
    );
}

#[test]
fn a_chip_less_lorom_cartridge_does_not_decode_the_eight_kilobyte_window() {
    // The window above is the chip's. A LoROM image with no SRAM has nothing to put there, and this
    // machine does not model the expansion region, so the address keeps reading the ROM byte the
    // image maps into that bank — and a write is dropped. That is the narrower of the two lies; the
    // alternative would take ROM bytes away from images that read them.
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let mut cart = Cartridge::new(rom).expect("a ROM-only cartridge loads");
    assert!(cart.ram.is_empty());
    let mapped = cart.rom[0x6000];
    assert_eq!(cart.read(Addr::new(0x31, 0x6000)), mapped);
    cart.write(Addr::new(0x31, 0x6000), 0x42);
    assert_eq!(cart.read(Addr::new(0x31, 0x6000)), mapped);
    assert!(!cart.ram_dirty);
}

#[test]
fn no_sram_reads_ff() {
    // §8.3: "A cartridge with no SRAM but with a board that has an SRAM window reads `$FF` and
    // drops writes, which is what an unpopulated socket does."
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let mut cart = Cartridge::new(rom).expect("a ROM-only cartridge loads");
    assert!(cart.ram.is_empty());

    assert_eq!(cart.read(Addr::new(0x70, 0x0000)), 0xFF);
    cart.write(Addr::new(0x70, 0x0000), 0x42);
    assert_eq!(cart.read(Addr::new(0x70, 0x0000)), 0xFF);
    assert!(!cart.ram_dirty);

    // Outside the window the same bank is ROM, so the rule is the window's and not the bank's.
    let mapped = cart.rom[0x0000];
    assert_eq!(cart.read(Addr::new(0x70, 0x8000)), mapped);

    // The HiROM window behaves the same way.
    let mut hi = Cartridge::new(common::synth_hirom(0x1_0000, &[0xEA])).unwrap();
    assert_eq!(hi.read(Addr::new(0x20, 0x6000)), 0xFF);
    hi.write(Addr::new(0x20, 0x6000), 0x42);
    assert_eq!(hi.read(Addr::new(0x20, 0x6000)), 0xFF);
    let mapped = hi.rom[0x6000];
    assert_eq!(hi.read(Addr::new(0x20, 0x5FFF)), mapped);
}

#[test]
fn the_expansion_window_is_open_bus_not_rom() {
    // Row `$4400-$5FFF`: "open bus", source fullsnes "I/O Ports". The window is
    // decided by the *bus*: `Cartridge::read` maps every offset of a LoROM/HiROM bank into the image
    // (`map::rom_offset` ignores the window), so a bus that lets the range fall through to the
    // cartridge answers ROM bytes at an address the console does not assert /CART for. Combatribes,
    // The read its first block count at `$00:5000` and was handed ROM 0x5000 there.
    let rom = common::synth_lorom(0x1_0000, &[0xEA]);
    let cart = Cartridge::new(rom).expect("a ROM-only cartridge loads");
    let mut m = tendy_snes::machine::Machine::new(cart);

    for addr in [
        Addr::new(0x00, 0x5000),
        Addr::new(0x20, 0x4400),
        Addr::new(0xBF, 0x5FFF),
    ] {
        m.bus.mdr = 0x5A;
        assert_eq!(
            m.bus.read(addr),
            0x5A,
            "{:02X}:{:04X} is in the `$4400-$5FFF` row: open bus answers the last value on \
             the data bus (here $5A), not a ROM byte",
            addr.bank(),
            addr.offset()
        );
    }

    // The ROM window of the same bank is unaffected: `$00:8000` is the image's first byte.
    let first = m.bus.cart.rom[0x0000];
    m.bus.mdr = 0x00;
    assert_eq!(
        m.bus.read(Addr::new(0x00, 0x8000)),
        first,
        "$8000-$FFFF in a system-area bank is still the ROM window"
    );
}
