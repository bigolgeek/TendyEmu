//! The Vs. System's mapper 99 board and its cabinet inputs.
//!
//! * AC2: bit 2 of a `$4016` write switches the 8 KiB CHR bank and — on the one 40 KiB image
//!   (Vs. Gumshoe) — the 8 KiB PRG window at `$8000-$9FFF` (nesdev "INES Mapper 099" and
//!   "Vs. System"; the Vs. page restricts the PRG half to 40 KiB carts).
//! * AC3: a coin key sets the coin line a game reads at `$4016` bit 5/6 for the frames it is held,
//!   and the DIP switches and service button read on the bits the Vs. page's two tables give.
//! * A machine with no Vs. header keeps reading exactly as it did.

use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;

/// A Vs. mapper-99 image: `prg_kb` of PRG in 8 KiB banks, each starting with its own number, then
/// `chr_kb` of CHR in 8 KiB pages holding their own number. The header is NES 2.0 because 40 KiB is
/// not a whole number of 16 KiB units, and its size is spelled in bytes (2^13 * 5), not in banks.
fn vs_rom(prg_kb: usize, chr_kb: usize) -> Vec<u8> {
    let prg_banks = prg_kb / 8;
    let chr_banks = chr_kb / 8;
    let mut v = vec![0; 16];
    v[0..4].copy_from_slice(b"NES\x1A");
    v[5] = chr_banks as u8;
    v[6] = 0x30; // mapper 99: low nibble 3 straight through from byte 6
    v[7] = 0x69; // NES 2.0, mapper high nibble 6, console type 1 (Vs. System)
    match prg_kb {
        32 => v[4] = 2,                    // two 16 KiB units
        40 => (v[4], v[9]) = (0x36, 0x0F), // exponent-multiplier: 2^13 * 5 bytes, MSB nibble $F
        other => panic!("{other} KiB has no Vs. layout"),
    }
    for bank in 0..prg_banks {
        v.resize(16 + (bank + 1) * 0x2000, 0);
        v[16 + bank * 0x2000] = bank as u8;
    }
    for page in 0..chr_banks {
        v.resize(16 + prg_banks * 0x2000 + (page + 1) * 0x2000, 0);
        v[16 + prg_banks * 0x2000 + page * 0x2000] = page as u8;
    }
    // `JMP $A010` in the fixed 24 KiB, and the reset vector pointing at it, so the program never
    // runs out of a window the bank bit can switch away (and each 8 KiB bank keeps its number at
    // its first byte).
    v[16 + 0x2010] = 0x4C;
    v[16 + 0x2011] = 0x10;
    v[16 + 0x2012] = 0xA0;
    v[16 + 3 * 0x2000 + 0x1FFC] = 0x10;
    v[16 + 3 * 0x2000 + 0x1FFD] = 0xA0;
    v
}

/// AC2: the `$4016` write's bit 2 is the board's bank register.
#[test]
fn a_4016_write_switches_the_chr_bank_and_the_40k_prg_window() {
    let mut nes = Nes::new(&vs_rom(40, 16)).expect("build the Vs. console");
    assert!(
        nes.bus.vs.is_some(),
        "a Vs. header plugs the cabinet inputs in"
    );
    assert_eq!(
        nes.bus.cart_mut().ppu_read(0x0000),
        0,
        "CHR page 0 at power-on"
    );
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x8000),
        0,
        "PRG bank 0 at $8000"
    );

    nes.bus.write(0x4016, 0x04);
    assert_eq!(
        nes.bus.cart_mut().ppu_read(0x0000),
        1,
        "$4016 bit 2 shows CHR page 1"
    );
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x8000),
        4,
        "and the extra 8 KiB PRG bank"
    );
    // The fixed 24 KiB above the window does not move with the bank bit.
    assert_eq!(nes.bus.cart_mut().cpu_read(0xA000), 1);
    assert_eq!(nes.bus.cart_mut().cpu_read(0xE000), 3);

    nes.bus.write(0x4016, 0x00);
    assert_eq!(
        nes.bus.cart_mut().ppu_read(0x0000),
        0,
        "bit 2 clear selects CHR page 0 again"
    );
    assert_eq!(nes.bus.cart_mut().cpu_read(0x8000), 0, "and PRG bank 0");
}

/// A 32 KiB image (every Vs. Super Mario Bros. board) has no second `$8000` bank, so the bit moves
/// the CHR bank alone and the program at `$8000` keeps running (nesdev "Vs. System" ties the PRG
/// half of the bit to 40 KiB images).
#[test]
fn a_32k_image_keeps_its_8000_window_while_the_bit_switches_chr() {
    let mut nes = Nes::new(&vs_rom(32, 16)).expect("build the Vs. console");
    nes.bus.write(0x4016, 0x04);
    assert_eq!(nes.bus.cart_mut().ppu_read(0x0000), 1);
    assert_eq!(nes.bus.cart_mut().cpu_read(0x8000), 0);
}

/// The board has 2 KiB of work RAM (nesdev "INES Mapper 099": "CPU $6000-$7FFF: 2 KiB RAM"), so
/// the four 2 KiB slots of that window mirror each other.
#[test]
fn the_2k_prg_ram_mirrors_across_the_6000_window() {
    let mut nes = Nes::new(&vs_rom(32, 16)).expect("build the Vs. console");
    nes.bus.write(0x6800, 0x5A);
    assert_eq!(
        nes.bus.read(0x6000),
        0x5A,
        "a byte written at $6800 reads back at $6000"
    );
    assert_eq!(nes.bus.read(0x7000), 0x5A, "and at every mirror of it");
}

/// The mapper 99 page's warning: with only 8 KiB of CHR the second socket is empty, so bit 2 high
/// leaves the PPU reading an undriven bus rather than a mirror of the first page.
#[test]
fn a_single_chr_rom_reads_no_tiles_on_the_second_bank() {
    let mut nes = Nes::new(&vs_rom(32, 8)).expect("build the Vs. console");
    nes.bus.write(0x4016, 0x04);
    assert_eq!(nes.bus.cart_mut().ppu_read(0x0000), 0xFF);
    nes.bus.write(0x4016, 0x00);
    assert_eq!(nes.bus.cart_mut().ppu_read(0x0000), 0x00);
}

/// AC3: each coin slot reads on its own `$4016` bit for as long as its key is held, and not on the
/// other slot's bit (nesdev: a game that sees both bits grants two credits).
#[test]
fn a_coin_reads_high_for_the_frames_its_key_is_held() {
    let mut nes = Nes::new(&vs_rom(32, 16)).expect("build the Vs. console");
    for frame in 0..3 {
        nes.bus.set_vs_inputs(0b01, false);
        nes.run_frame();
        assert_eq!(
            nes.bus.read(0x4016) & 0x20,
            0x20,
            "frame {frame}: coin 1 is in"
        );
        assert_eq!(
            nes.bus.read(0x4016) & 0x40,
            0x00,
            "coin 1 does not raise slot 2"
        );
    }
    nes.bus.set_vs_inputs(0, false);
    nes.run_frame();
    assert_eq!(
        nes.bus.read(0x4016) & 0x60,
        0x00,
        "released: both slots read clear"
    );

    nes.bus.set_vs_inputs(0b10, false);
    assert_eq!(nes.bus.read(0x4016) & 0x60, 0x40, "slot 2 has its own bit");
}

/// The other half of the two read tables: DIP switches 1 and 2 and the service button on `$4016`,
/// DIP switches 8 down to 3 on `$4017`, with bit 1 of both ports reading 0.
#[test]
fn the_dip_switches_and_service_button_read_on_their_bits() {
    let mut nes = Nes::new(&vs_rom(32, 16)).expect("build the Vs. console");
    nes.bus.set_vs_dip(0b1010_0101);
    nes.bus.set_vs_inputs(0, true);

    let one = nes.bus.read(0x4016);
    assert_eq!(one & 0x02, 0, "$4016 bit 1 is always 0");
    assert_eq!(one & 0x04, 0x04, "the service button is bit 2");
    assert_eq!(
        one & 0x18,
        0b0_1000,
        "DIP 1 is bit 3 and DIP 2 is bit 4 (DIP 1 high, DIP 2 low)"
    );
    assert_eq!(one & 0x80, 0, "the primary CPU flag reads 0");

    let two = nes.bus.read(0x4017);
    assert_eq!(two & 0x02, 0, "$4017 bit 1 is always 0");
    assert_eq!(
        two & 0xFC,
        0b1010_0100,
        "DIP 8 to 3 are bits 7 to 2, in that order"
    );
}

/// A plain NES has no cabinet, so the upper bits of both ports stay open bus; a Vs. header is what
/// plugs the cabinet in, not any particular mapper.
#[test]
fn only_a_vs_header_plugs_the_cabinet_in() {
    let mut plain = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    plain.resize(16 + 16384 + 8192, 0);
    let mut nes = Nes::new(&plain).expect("build an NROM console");
    assert!(nes.bus.vs.is_none());
    // With no cabinet, those bits are the open bus: whatever the data bus last held comes back.
    nes.bus.open_bus = 0xE0;
    assert_eq!(
        nes.bus.read(0x4016) & 0xE0,
        0xE0,
        "port 1 keeps its open-bus bits"
    );
    nes.bus.open_bus = 0xE0;
    assert_eq!(
        nes.bus.read(0x4017) & 0xE0,
        0xE0,
        "port 2 keeps its open-bus bits"
    );

    let mut vs_header = plain;
    vs_header[7] = 0x01; // iNES 1.0 Vs. Unisystem
    let mut nes = Nes::new(&vs_header).expect("build the Vs. console");
    assert!(nes.bus.vs.is_some());
    assert_eq!(
        nes.bus.read(0x4016) & 0xE0,
        0,
        "the cabinet drives those bits now"
    );
}
