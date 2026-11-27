//! Cartridge header and refusal messages.
//!
//! Every ROM here is built as bytes in code (`tests/common/mod.rs`), so no test needs a file.

mod common;

use tendy_gb::Cartridge;
use tendy_gb::cart::header::cart_type_name;

/// A synthesized ROM with `banks` x 16 KiB, a chosen type byte and a chosen RAM size code.
fn rom_bytes(banks: usize, cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = common::synth_rom_banked(banks, cart_type, &common::entry(&[0x00]));
    rom[0x0149] = ram_code;
    fix_checksum(&mut rom);
    rom
}

/// The header checksum, recomputed after a test patches a header field.
fn fix_checksum(rom: &mut [u8]) {
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x014D] = x;
}

#[test]
fn header_parse() {
    let rom = rom_bytes(2, 0x03, 0x02);
    let cart = Cartridge::new(rom.clone()).expect("cartridge");

    assert_eq!(cart.header.title, "GB-TEST");
    assert_eq!(cart.header.cart_type, 0x03);
    assert_eq!(cart.header.rom_size_code, 0x00);
    assert_eq!(cart.header.ram_size_code, 0x02);
    assert_eq!(cart.header.cgb_flag, 0x00);
    assert_eq!(cart.header.rom_bytes, 0x8000);
    assert_eq!(cart.header.ram_bytes, 0x2000);
    assert!(!cart.header.has_rtc);
    assert_eq!(cart.header.header_checksum, rom[0x014D]);
    assert!(cart.header.header_ok);
    assert_eq!(cart.mbc_name(), "mbc1");
    assert_eq!(cart.ram().len(), 0x2000);

    // The title stops at the first NUL and drops bytes that are not printable ASCII.
    let mut odd = rom.clone();
    odd[0x0134..0x0144].copy_from_slice(b"AB\x01CD\0\0\0\0\0\0\0\0\0\0\0");
    fix_checksum(&mut odd);
    assert_eq!(Cartridge::new(odd).unwrap().header.title, "ABCD");

    // A type with a clock says so.
    assert!(
        Cartridge::new(rom_bytes(2, 0x0F, 0x00))
            .unwrap()
            .header
            .has_rtc
    );
}

#[test]
fn header_checksum_matches_the_formula() {
    let rom = rom_bytes(4, 0x01, 0x00);
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    assert_eq!(rom[0x014D], x);

    let cart = Cartridge::new(rom.clone()).expect("cartridge");
    assert_eq!(cart.header.header_checksum, x);
    assert!(cart.header.header_ok);

    // One changed title byte breaks the comparison, and nothing else about the parse.
    let stored = rom[0x014D];
    let mut broken = rom;
    broken[0x0134] ^= 0x01;
    let cart = Cartridge::new(broken).expect("cartridge");
    assert!(!cart.header.header_ok);
    assert_eq!(
        cart.header.header_checksum, stored,
        "the byte is not rewritten"
    );
}

#[test]
fn bad_checksum_still_loads() {
    // A bad checksum is a notice, not a refusal.
    let mut rom = rom_bytes(2, 0x00, 0x00);
    rom[0x014D] = rom[0x014D].wrapping_add(1);
    let cart = Cartridge::new(rom.clone()).expect("a bad checksum still loads");
    assert!(!cart.header.header_ok);
    assert_eq!(cart.header.header_checksum, rom[0x014D]);

    // And post-boot F follows the byte, not the verdict (§4.3).
    let machine = tendy_gb::Machine::new(cart);
    assert_eq!(machine.cpu.pc, 0x0100);
    let expected = if rom[0x014D] == 0 { 0x80 } else { 0xB0 };
    assert_eq!(machine.cpu.f.0, expected);
}

#[test]
fn refuses_unknown_type_with_message() {
    let message = Cartridge::new(rom_bytes(2, 0xFC, 0x00))
        .err()
        .expect("refused");
    assert_eq!(
        message,
        "unsupported cartridge type $FC (POCKET CAMERA) - supported: ROM only, MBC1, MBC1M, MBC2, MBC3, MBC30, MBC5, MBC5+rumble, MBC6, MBC7, MMM01, HuC1, HuC3, M161"
    );

    // A byte the Pan Docs table does not list is named `unknown`.
    let unknown = Cartridge::new(rom_bytes(2, 0x2A, 0x00))
        .err()
        .expect("refused");
    assert!(unknown.contains("$2A (unknown)"), "{unknown}");
    assert!(unknown.ends_with("HuC3, M161"), "{unknown}");

    // The boards v5 implements are no longer refused.
    for (byte, ram) in [
        (0x0Bu8, 0x00u8),
        (0x0C, 0x02),
        (0x0D, 0x03),
        (0x13, 0x05),
        (0x1C, 0x03),
        (0x1E, 0x03),
        (0x20, 0x00),
        (0x22, 0x02),
        (0xE1, 0x00),
        (0xFE, 0x02),
    ] {
        let cart = Cartridge::new(rom_bytes(4, byte, ram))
            .unwrap_or_else(|e| panic!("${byte:02X} refused: {e}"));
        assert_ne!(cart.mbc_name(), "unknown", "${byte:02X}");
    }

    // The types v5 still does not implement keep their Pan Docs names, and are still refused.
    assert_eq!(cart_type_name(0xFC), "POCKET CAMERA");
    assert_eq!(cart_type_name(0xFD), "BANDAI TAMA5");
}

#[test]
fn refuses_short_rom() {
    let message = Cartridge::new(vec![0; 0x100]).err().expect("refused");
    assert_eq!(message, "ROM is 256 bytes, need at least 32768");

    let message = Cartridge::new(vec![0; 0x7FFF]).err().expect("refused");
    assert_eq!(message, "ROM is 32767 bytes, need at least 32768");
}

#[test]
fn rom_size_codes() {
    for code in 0u8..=8 {
        let mut rom = rom_bytes(2, 0x01, 0x00);
        rom[0x0148] = code;
        fix_checksum(&mut rom);
        let cart = Cartridge::new(rom).expect("cartridge");
        assert_eq!(
            cart.header.rom_bytes,
            0x8000usize << code,
            "ROM size code ${code:02X}"
        );
        assert_eq!(cart.header.rom_size_code, code);
    }
    for (code, banks) in [(0x52u8, 72usize), (0x53, 80), (0x54, 96)] {
        let mut rom = rom_bytes(2, 0x01, 0x00);
        rom[0x0148] = code;
        fix_checksum(&mut rom);
        let cart = Cartridge::new(rom).expect("cartridge");
        assert_eq!(cart.header.rom_bytes, banks * 0x4000, "code ${code:02X}");
    }
}

#[test]
fn ram_size_codes() {
    // §5.1: $00 none, $01 as 8 KiB (PD ROMs used it), $02 8 KiB, $03 32 KiB, $04 128 KiB, $05 64 KiB.
    for (code, bytes) in [
        (0x00u8, 0usize),
        (0x01, 0x2000),
        (0x02, 0x2000),
        (0x03, 0x8000),
        (0x04, 0x20000),
        (0x05, 0x10000),
    ] {
        let cart = Cartridge::new(rom_bytes(2, 0x1B, code)).expect("cartridge");
        assert_eq!(cart.header.ram_bytes, bytes, "RAM size code ${code:02X}");
        assert_eq!(cart.ram().len(), bytes, "allocated RAM for ${code:02X}");
    }

    // MBC2's cells are in the chip, so the header byte is 0 and 512 bytes exist anyway (§5.4).
    let mbc2 = Cartridge::new(rom_bytes(2, 0x06, 0x00)).expect("cartridge");
    assert_eq!(mbc2.header.ram_bytes, 0);
    assert_eq!(mbc2.ram().len(), 512);

    // A ROM-only cartridge has no mapper RAM unless the header says so.
    assert_eq!(
        Cartridge::new(rom_bytes(2, 0x00, 0x00))
            .unwrap()
            .ram()
            .len(),
        0
    );
}

/// The 48-byte Nintendo logo at `$0104-$0133`.
const LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// A `banks` x 16 KiB image whose every bank carries its own number at `$0200`, so one read says
/// which bank a mapper put in a window.
fn bank_marked(banks: usize, cart_type: u8) -> Vec<u8> {
    let mut rom = common::synth_rom_banked(banks, cart_type, &[]);
    for bank in 0..banks {
        rom[bank * 0x4000 + 0x0200] = bank as u8;
    }
    rom
}

/// A 32 KiB image that looks licensed: the logo is present.
fn licensed_rom(cart_type: u8) -> Vec<u8> {
    let mut rom = rom_bytes(2, cart_type, 0x00);
    rom[0x0104..0x0134].copy_from_slice(&LOGO);
    rom
}

#[test]
fn sachen_4_in_1_switches_both_windows() {
    // The real images carry a decoy header (no logo) and their reset vector at `$0000`.
    let mut rom = bank_marked(16, 0x01);
    rom[0..4].copy_from_slice(&[0xF3, 0xC3, 0x00, 0x02]); // DI; JP $0200
    let mut cart = Cartridge::new(rom).expect("cartridge");

    assert_eq!(cart.mbc_name(), "sachen");
    assert_eq!(
        cart.entry_point(),
        0x0000,
        "the decoy header is not the entry"
    );
    // Before the launcher runs: bank 0 at `$0000`, bank 1 at `$4000`.
    assert_eq!(cart.read(0x0200), 0);
    assert_eq!(cart.read(0x4200), 1);

    // The launcher writes the chosen game's bank to `$0000` (`$08` for `4B-001`'s fourth game) and
    // the board maps that bank at `$0000` with its odd partner at `$4000`: the game's first
    // instruction is `CALL $45F4`, and `$45F4` is real code in bank 9, not menu data in bank 0.
    cart.write(0x0000, 0x08);
    assert_eq!(cart.read(0x0200), 8);
    assert_eq!(cart.read(0x4200), 9, "the partner of the block");

    // `$2000 <- 1..3` picks the block's second, third or fourth bank at `$4000` — the games' own
    // bank API (`LD A,1; LD ($2000),A`, call into `$4000`, restore from `$C00A`).
    cart.write(0x2000, 0x03);
    assert_eq!(cart.read(0x4200), 11);
    assert_eq!(cart.read(0x0200), 8, "the other window is untouched");

    // The launcher brackets its handover with `$2000 <- $FF` and `$2000 <- $00`. Honouring either
    // as a bank puts bank 15 or bank 8 at `$4000` and every one of the five images goes to the
    // blank frame, so neither write moves the window.
    cart.write(0x2000, 0xFF);
    assert_eq!(cart.read(0x4200), 11);
    cart.write(0x2000, 0x00);
    assert_eq!(cart.read(0x4200), 11);

    // `4B-009` puts the bank at `$4000-$5FFF` instead (`$0000` gets `$FC`, which the 16-bank image
    // cannot hold); the games' HRAM switch stub writes `$2000 <- $31` then `$4000 <- <bank>`.
    cart.write(0x0000, 0xFE);
    assert_eq!(
        cart.read(0x0200),
        8,
        "a value the image cannot hold is not a bank"
    );
    cart.write(0x2000, 0x31);
    assert_eq!(cart.read(0x4200), 11, "$31 is not a sub-bank");
    cart.write(0x4000, 0x0C);
    assert_eq!(cart.read(0x0200), 12);
    assert_eq!(cart.read(0x4200), 13);
    cart.write(0x4000, 0xFE);
    assert_eq!(cart.read(0x0200), 12);

    // A sub-bank past the end of the image is not a bank either.
    cart.write(0x0000, 0x0E);
    cart.write(0x2000, 0x03);
    assert_eq!(cart.read(0x0200), 14);
    assert_eq!(cart.read(0x4200), 15, "clamped to the pair, not bank 17");

    // No cartridge RAM on this board.
    assert_eq!(cart.ram().len(), 0);
    assert_eq!(cart.read(0xA000), 0xFF);

    // And the machine starts the CPU at the cartridge's own reset vector.
    assert_eq!(tendy_gb::Machine::new(cart).cpu.pc, 0x0000);
}

#[test]
fn wisdom_tree_switches_the_whole_32_kib_by_address() {
    let mut rom = bank_marked(16, 0x00);
    rom[0..7].copy_from_slice(&[0x01, 0x00, 0x00, 0x02, 0xC3, 0x4B, 0x09]);
    rom[0x0100..0x0103].copy_from_slice(&[0xC3, 0x00, 0x00]); // JP $0000
    rom[0x0104..0x0134].copy_from_slice(&LOGO);
    let mut cart = Cartridge::new(rom).expect("cartridge");

    assert_eq!(cart.mbc_name(), "wisdomtree");
    assert_eq!(cart.entry_point(), 0x0000);

    // The trampoline writes to `$0000`, so the latch reads 0 and the windows are banks 0 and 1 —
    // the value written is ignored (Pan Docs: the board latches A7-A0, not the data bus).
    cart.write(0x0000, 0xA5);
    assert_eq!(cart.read(0x0200), 0);
    assert_eq!(cart.read(0x4200), 1);

    // `$0004` selects the 32 KiB block 4: banks 8 and 9, whatever value is written. Treating the
    // low byte as a 16 KiB bank number (the old `& $FE` / `| $01` rule) selects banks 4 and 5.
    cart.write(0x0004, 0xFF);
    assert_eq!(cart.read(0x0200), 8);
    assert_eq!(cart.read(0x4200), 9);

    // The low byte of the address is the block, so `$0106` is block 6: banks 12 and 13.
    cart.write(0x0106, 0x00);
    assert_eq!(cart.read(0x0200), 12);
    assert_eq!(cart.read(0x4200), 13);

    assert_eq!(tendy_gb::Machine::new(cart).cpu.pc, 0x0000);
}

#[test]
fn pro_action_replay_has_work_ram_without_any_enable() {
    let mut rom = rom_bytes(2, 0xFF, 0x00);
    rom[0x0100] = 0x00; // NOP
    rom[0x0101..0x0104].copy_from_slice(&[0xC3, 0x50, 0x01]); // JP $0150
    rom[0x4100] = 0x42;
    let mut cart = Cartridge::new(rom).expect("cartridge");

    assert_eq!(cart.mbc_name(), "actionreplay");
    assert_eq!(cart.entry_point(), 0x0100, "the licensed entry address");
    // The header says "no RAM" (size code $00) but the cartridge has 8 KiB at `$A000-$BFFF`.
    assert_eq!(cart.header.ram_bytes, 0);
    assert_eq!(cart.ram().len(), 0x2000);

    // The boot code fills `$B000-$B03E` with zero and branches on `$B040`, and never writes an
    // enable byte anywhere: the RAM has to answer by itself.
    cart.write(0xB040, 0x01);
    assert_eq!(cart.read(0xB040), 0x01);
    cart.write(0xA000, 0x5A);
    assert_eq!(cart.read(0xA000), 0x5A);

    // Two 16 KiB banks and no bank switching at all.
    assert_eq!(cart.read(0x4100), 0x42);
    cart.write(0x0000, 0x01);
    cart.write(0x2000, 0x01);
    cart.write(0x4000, 0x01);
    assert_eq!(cart.read(0x4100), 0x42);
    assert_eq!(cart.read(0x0100), 0x00);
}

#[test]
fn unlicensed_detection_does_not_capture_licensed_images() {
    // A 32 KiB ROM-only cartridge with the logo and the usual entry stays ROM only.
    let cart = Cartridge::new(licensed_rom(0x00)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "romonly");
    assert_eq!(cart.entry_point(), 0x0100);

    // The same with MBC1, and a 256 KiB MBC1 image whose entry is `JP $0150`, not `JP $0200`.
    assert_eq!(
        Cartridge::new(licensed_rom(0x01)).unwrap().mbc_name(),
        "mbc1"
    );
    let big = Cartridge::new(rom_bytes(16, 0x01, 0x00)).expect("cartridge");
    assert_eq!(
        big.mbc_name(),
        "mbc1",
        "256 KiB alone is not a Sachen board"
    );

    // A 256 KiB image built like a Sachen board but *with* a logo is not captured either.
    let mut logo_sachen = bank_marked(16, 0x01);
    logo_sachen[0..4].copy_from_slice(&[0xF3, 0xC3, 0x00, 0x02]);
    logo_sachen[0x0104..0x0134].copy_from_slice(&LOGO);
    assert_eq!(
        Cartridge::new(logo_sachen).unwrap().mbc_name(),
        "mbc1",
        "the logo rules the Sachen board out"
    );
}

#[test]
fn rocket_games_ride_mbc1_and_only_space_invasion_gets_ram() {
    // Rocket Games (Datel/InterAct) unlicensed carts: type `$97` (single games) and `$99` (the
    // multicarts and Race Time), a decoy header with no logo, and banking that is MBC1's — the ten
    // write the whole bank number to `$2000-$3FFF` in one go, never splitting it across `$2000` and
    // `$3000` as MBC5 would. So the board rides the MBC1 mapper.
    let mut cart = Cartridge::new(bank_marked(16, 0x97)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc1");
    assert_eq!(
        cart.entry_point(),
        0x0100,
        "the trampoline at $0100 is the entry"
    );

    // MBC1 banking: bank 0 at `$0000-$3FFF`, bank 1 at `$4000-$7FFF`, then one five-bit write
    // selects the bank (the multicarts boot with `LD A,$0E; LD ($3F00),A`).
    assert_eq!(cart.read(0x0200), 0);
    assert_eq!(cart.read(0x4200), 1);
    cart.write(0x3F00, 0x0E);
    assert_eq!(cart.read(0x4200), 0x0E);
    // MBC1's 0 -> 1 translation: a write of 0 puts bank 1, not bank 0, in the banked window.
    cart.write(0x2000, 0x00);
    assert_eq!(cart.read(0x4200), 1);

    // The decoy header RAM size is ignored when the image never writes the enable nibble: no RAM
    // is allocated, so `$A000-$BFFF` reads open bus and writes go nowhere.
    assert_eq!(cart.ram().len(), 0);
    cart.write(0x0000, 0x0A);
    assert_eq!(cart.read(0xA000), 0xFF);

    // `$99` rides the same board.
    assert_eq!(
        Cartridge::new(bank_marked(32, 0x99)).unwrap().mbc_name(),
        "mbc1"
    );

    // A `$97` image that writes MBC1's enable nibble (`LD A,$0A; LD ($1FFF),A` — the Space
    // Invasion carts) gets 8 KiB of gated RAM, enabled and disabled like MBC1.
    let mut si = bank_marked(8, 0x97);
    si[0x150..0x155].copy_from_slice(&[0x3E, 0x0A, 0xEA, 0xFF, 0x1F]);
    let mut si = Cartridge::new(si).expect("cartridge");
    assert_eq!(si.ram().len(), 0x2000);
    assert_eq!(
        si.read(0xA000),
        0xFF,
        "disabled until the nibble is written"
    );
    si.write(0x1FFF, 0x0A);
    si.write(0xA000, 0x5A);
    assert_eq!(si.read(0xA000), 0x5A);
    si.write(0x1FFF, 0x00);
    assert_eq!(
        si.read(0xA000),
        0xFF,
        "disabling the nibble drops the window"
    );
}

// ---------------------------------------------------------------------------------------------
// v5 boards. Every image is `bank_marked`, so a bank number in a marker byte is the
// bank the board put in that window.
// ---------------------------------------------------------------------------------------------

/// `bank_marked` plus a RAM size code, re-checksummed.
fn bank_marked_ram(banks: usize, cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = bank_marked(banks, cart_type);
    rom[0x0149] = ram_code;
    fix_checksum(&mut rom);
    rom
}

#[test]
fn mbc1m_banks_with_four_high_bits() {
    // Pan Docs `MBC1.html`, "MBC1M: 1 MiB Multi-Game Compilation Carts": a 1 MiB image with the
    // Nintendo logo repeated in bank 16 is an MBC1M, and its high bank bits sit at bit 4.
    let mut rom = bank_marked(64, 0x01);
    rom[0x0104..0x0134].copy_from_slice(&LOGO);
    rom[0x4_0104..0x4_0134].copy_from_slice(&LOGO);
    let mut cart = Cartridge::new(rom).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc1m");

    // Power-on: bank 0 at `$0000`, bank 1 at `$4000`.
    assert_eq!(cart.read(0x0200), 0);
    assert_eq!(cart.read(0x4200), 1);

    // `$2000` = 1, `$4000` = 1 is bank 17, not bank 33: the second register's bit 0 is bank bit 4.
    cart.write(0x2000, 1);
    cart.write(0x4000, 1);
    assert_eq!(cart.read(0x4200), 17);
    assert_eq!(cart.read(0x0200), 0, "mode 0 keeps bank 0 at $0000");

    // Mode 1 puts the second register in `$0000`, at the same shift.
    cart.write(0x6000, 1);
    assert_eq!(cart.read(0x0200), 16);

    // The main register's bit 4 is dropped, so 0x11 is bank 1 of the same game.
    cart.write(0x2000, 0x11);
    cart.write(0x6000, 0);
    assert_eq!(cart.read(0x4200), 17);
}

#[test]
fn mbc30_has_eight_bit_bank_and_64_kib_ram() {
    // `MBC3.html`: MBC30 addresses 4 MiB of ROM and 64 KiB of RAM. The header page's footnote is
    // the only way to tell it apart, so a `$13` cartridge with a 64 KiB RAM code is one.
    let mut cart = Cartridge::new(bank_marked_ram(256, 0x13, 0x05)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc30");
    assert_eq!(cart.ram().len(), 0x1_0000);

    // Bank $80 is only reachable with an eight-bit register: MBC3's seven bits would give bank 0.
    cart.write(0x2000, 0x80);
    assert_eq!(cart.read(0x4200), 0x80);
    cart.write(0x2000, 0x85);
    assert_eq!(cart.read(0x4200), 0x85);

    // Four RAM banks, selected by `$4000`'s low two bits.
    cart.write(0x0000, 0x0A);
    for bank in 0..4u8 {
        cart.write(0x4000, bank);
        cart.write(0xA000, bank + 1);
    }
    for bank in 0..4u8 {
        cart.write(0x4000, bank);
        assert_eq!(cart.read(0xA000), bank + 1, "RAM bank {bank}");
    }

    // The board a game does exercise elsewhere is unchanged: a `$13` with 32 KiB of RAM stays MBC3,
    // and so does a 2 MiB one, whose bit 7 is still dropped by the seven-bit register.
    assert_eq!(
        Cartridge::new(bank_marked_ram(128, 0x13, 0x03))
            .unwrap()
            .mbc_name(),
        "mbc3"
    );
    let mut mbc3 = Cartridge::new(bank_marked_ram(128, 0x13, 0x03)).expect("cartridge");
    mbc3.write(0x2000, 0x80);
    assert_eq!(
        mbc3.read(0x4200),
        1,
        "seven bits: bank 0, which MBC3 maps to 1"
    );
}

#[test]
fn m161_takes_one_bank_write_per_run() {
    // M161 (`M161.html`, and the type byte `$E1` this build maps to it): one 32 KiB bank register,
    // written once, in `$0000-$1FFF`. The markers are 16 KiB apart, so a 32 KiB bank n shows marker
    // 2n at `$0200` and 2n + 1 at `$4200`.
    let mut cart = Cartridge::new(bank_marked(8, 0xE1)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "m161");
    assert_eq!(cart.read(0x0200), 0, "bank 0 at power-on");
    assert_eq!(cart.read(0x4200), 1);

    cart.write(0x0000, 3);
    assert_eq!(cart.read(0x0200), 6);
    assert_eq!(cart.read(0x4200), 7);

    // The latch is spent: a second write does nothing.
    cart.write(0x0000, 1);
    assert_eq!(cart.read(0x0200), 6, "the first write is the only one");

    // The register spans the whole `$0000-$7FFF` range, so the first write can be anywhere in it.
    let mut high = Cartridge::new(bank_marked(8, 0xE1)).expect("cartridge");
    high.write(0x4000, 3);
    assert_eq!(high.read(0x0200), 6, "a write at $4000 is a bank write");
    high.write(0x0000, 1);
    assert_eq!(high.read(0x0200), 6, "and it was the only one");
}

#[test]
fn mbc5_rumble_bit_is_not_a_ram_bank() {
    // `MBC5.html`, "Rumble": with a motor on board, the RAM-bank register's bit 3 drives the motor
    // and only bits 0-2 reach the chip.
    let mut cart = Cartridge::new(bank_marked_ram(4, 0x1E, 0x03)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc5r");
    assert_eq!(cart.ram().len(), 0x8000);

    cart.write(0x0000, 0x0A);
    cart.write(0x4000, 0);
    cart.write(0xA000, 0x11);
    cart.write(0x4000, 1);
    cart.write(0xA000, 0x22);

    // RAM bank 9 is RAM bank 1 with the motor on: the byte is bank 0's and bank 1's, and nothing is
    // written through to a ninth bank.
    cart.write(0x4000, 0x08);
    assert_eq!(cart.read(0xA000), 0x11, "the motor bit selects bank 0");
    cart.write(0x4000, 0x09);
    assert_eq!(cart.read(0xA000), 0x22);
    cart.write(0xA000, 0x33);
    cart.write(0x4000, 1);
    assert_eq!(cart.read(0xA000), 0x33, "bank 9 wrote bank 1");

    // ROM banking is the MBC5 eight-bit register, bank 0 included.
    cart.write(0x2000, 2);
    assert_eq!(cart.read(0x4200), 2);
    cart.write(0x2000, 0);
    assert_eq!(cart.read(0x4200), 0);
}

#[test]
fn mbc6_switches_both_windows_and_programs_flash() {
    // `MBC6.html`: 8 KiB banks, bank A at `$4000`, bank B at `$6000`, each ROM or flash, plus the
    // flash command set. The markers are 16 KiB apart, so an 8 KiB bank 2n shows marker n.
    let mut cart = Cartridge::new(bank_marked_ram(64, 0x20, 0x00)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc6");

    // `$0000-$3FFF` is ROM banks 0 and 1, bank A and bank B are 8 KiB windows.
    assert_eq!(cart.read(0x0200), 0);
    cart.write(0x2000, 10);
    assert_eq!(
        cart.read(0x4200),
        5,
        "bank A 10 is the low half of 16 KiB bank 5"
    );
    cart.write(0x3000, 20);
    assert_eq!(cart.read(0x6200), 10);

    // The same windows show the flash when the select register says `$08`, and the chip has to be
    // enabled as well: erased flash reads `$FF` where ROM bank 10 holds a marker.
    cart.write(0x2800, 0x08);
    assert_eq!(cart.read(0x4200), 0xFF, "flash, not enabled");
    cart.write(0x0C00, 0x01);
    assert_eq!(cart.read(0x4200), 0xFF, "erased flash");

    // Program a page: unlock, program mode, then 128 writes into the page. The flash address is the
    // window's bank number as the top bits (`MBC6.html`'s "2:5555"), so `$5555` needs bank A = 2.
    cart.write(0x1000, 0x01);
    flash_command(&mut cart, 0xA0);
    for i in 0..128u16 {
        cart.write(0x4000 + i, 0x5A);
    }
    // $F0 leaves program mode; writing the last byte of the page already committed it.
    cart.write(0x4000, 0xF0);
    assert_eq!(cart.read(0x4000), 0x5A, "programmed byte");
    assert_eq!(cart.read(0x4001), 0x5A);
    assert_eq!(
        cart.read(0x4100),
        0xFF,
        "the next 128-byte page is untouched"
    );

    // Sector 0 is write-protected out of the box (`MBC6.html`), so a chip erase needs the
    // protection lifted first: `$60` then `$40`, each behind the same two-byte unlock. The erase
    // itself is the documented two-step one: `$80`, then `$10`.
    flash_command(&mut cart, 0x60);
    flash_command(&mut cart, 0x40);
    flash_command(&mut cart, 0x80);
    flash_command(&mut cart, 0x10);
    assert_eq!(cart.read(0x4000), 0xFF, "chip erase");

    // Eight 4 KiB RAM banks behind one enable, A and B on either side of `$B000`.
    cart.write(0x0000, 0x0A);
    cart.write(0x0400, 3);
    cart.write(0x0800, 5);
    cart.write(0xA000, 0xA3);
    cart.write(0xB000, 0xB5);
    cart.write(0x0400, 5);
    cart.write(0x0800, 3);
    assert_eq!(cart.read(0xB000), 0xA3, "bank A 3 and bank B 3 swapped");
    assert_eq!(cart.read(0xA000), 0xB5);
}

#[test]
fn mmm01_starts_unmapped_and_maps_when_told() {
    // `MMM01.html`: at power-on the last 32 KiB of the ROM is the whole address space, and the
    // board becomes MBC1-like when `$0000-$1FFF` is written with bit 6 set.
    let mut cart = Cartridge::new(bank_marked_ram(16, 0x0C, 0x02)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mmm01");

    // 256 KiB image: the last 32 KiB starts at 16 KiB bank 14, so `$0200` shows marker 14.
    assert_eq!(cart.read(0x0200), 14);
    assert_eq!(cart.read(0x4200), 15);
    // RAM is not available while unmapped (the page calls this unknown; the conservative reading).
    assert_eq!(cart.read(0xA000), 0xFF);

    // The magic write maps the board.
    cart.write(0x0000, 0x40);
    assert_eq!(cart.read(0x0200), 0, "bank 0 in the fixed window");
    assert_eq!(cart.read(0x4200), 1, "bank 1 in the banked window");

    // MBC1-like banking: the low five bits in `$2000`.
    cart.write(0x2000, 3);
    assert_eq!(cart.read(0x4200), 3);

    // `$6000`'s mask pins the game bits in the fixed window. The page says the extra registers are
    // writable only while unmapped, so the mask is set before the magic write, not after it.
    let mut masked = Cartridge::new(bank_marked_ram(16, 0x0C, 0x02)).expect("cartridge");
    masked.write(0x6000, 0x02);
    masked.write(0x0000, 0x40);
    masked.write(0x2000, 3);
    assert_eq!(
        masked.read(0x0200),
        1,
        "the fixed window keeps only the bits the mask leaves to the game"
    );
    assert_eq!(
        masked.read(0x4200),
        3,
        "the banked window sees the whole register"
    );
}

#[test]
fn huc3_banks_on_bank_zero_and_answers_the_mailbox() {
    // `HuC3.html`: ROM/RAM banking like an MBC5, bank 0 mappable, plus the command mailbox.
    let mut cart = Cartridge::new(bank_marked_ram(8, 0xFE, 0x02)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "huc3");

    // The bank register is seven bits and bank 0 really is bank 0.
    cart.write(0x2000, 5);
    assert_eq!(cart.read(0x4200), 5);
    cart.write(0x2000, 0);
    assert_eq!(cart.read(0x4200), 0, "bank 0 is mappable on this board");

    // RAM: `$0` read-only, `$A` read-write, `$4000` selects the bank.
    cart.write(0x0000, 0x0A);
    cart.write(0x4000, 1);
    cart.write(0xA000, 0x5A);
    assert_eq!(cart.read(0xA000), 0x5A);
    cart.write(0x0000, 0x00);
    assert_eq!(cart.read(0xA000), 0x5A, "read-only mode still reads");
    cart.write(0xA000, 0x11);
    assert_eq!(cart.read(0xA000), 0x5A, "read-only mode drops the write");

    // The mailbox: `$D` reads ready, `$B` takes command 6 / argument 2 (status), `$C` reads back
    // "6" and the result. `HuC3.html` says the games will not start unless the status is `$1`.
    cart.write(0x0000, 0x0D);
    assert_eq!(cart.read(0xA000) & 0x01, 0x01);
    cart.write(0x0000, 0x0B);
    cart.write(0xA000, 0x62);
    cart.write(0x0000, 0x0D);
    cart.write(0xA000, 0xFE);
    cart.write(0x0000, 0x0C);
    assert_eq!(cart.read(0xA000), 0x80 | 0x60 | 0x01);
}

#[test]
fn mbc7_latches_the_tilt_and_shifts_the_eeprom() {
    // `MBC7.html`: one register at a time at `$A000-$BFFF`, a 93LC56 EEPROM at `Ax8x`, and the
    // accelerometer at `Ax2x`/`Ax3x` (X) and `Ax4x`/`Ax5x` (Y).
    let mut cart = Cartridge::new(bank_marked_ram(8, 0x22, 0x02)).expect("cartridge");
    assert_eq!(cart.mbc_name(), "mbc7");
    assert_eq!(cart.ram().len(), 0, "the board has no battery RAM");

    // ROM banking is MBC5's, bank 0 included.
    cart.write(0x2000, 3);
    assert_eq!(cart.read(0x4200), 3);

    // Both enables are needed: `$0A` to `$0000` and `$40` to `$4000`.
    assert_eq!(cart.read(0xA020), 0xFF, "registers disabled");
    cart.write(0x0000, 0x0A);
    cart.write(0x4000, 0x40);
    assert_eq!(
        cart.read(0xA020),
        0x00,
        "X low of $8000: the sample starts erased"
    );
    assert_eq!(cart.read(0xA030), 0x80);
    assert_eq!(cart.read(0xA060), 0x00);
    assert_eq!(cart.read(0xA070), 0xFF);

    // Tilt X right (positive) and latch: `$8000` + `$70`.
    cart.set_tilt(1, 0);
    cart.write(0xA010, 0xAA);
    assert_eq!(cart.read(0xA020), 0x40);
    assert_eq!(cart.read(0xA030), 0x82);

    // A second latch without an erase is ignored, and Y stayed at rest.
    cart.set_tilt(-1, 0);
    cart.write(0xA010, 0xAA);
    assert_eq!(cart.read(0xA020), 0x40);

    // Erase, then latch the new tilt: `$8000` - `$70`.
    cart.write(0xA000, 0x55);
    assert_eq!(cart.read(0xA030), 0x80);
    cart.write(0xA010, 0xAA);
    assert_eq!(cart.read(0xA020), 0x60);
    assert_eq!(cart.read(0xA030), 0x81);
    assert_eq!(cart.read(0xA040), 0xD0, "Y low of $81D0");
    assert_eq!(cart.read(0xA050), 0x81, "Y high");

    // The 93LC56: EWEN, WRITE word 0 = $1234, then READ it back, MSB first.
    eeprom_cmd(&mut cart, 0x260);
    eeprom_cmd(&mut cart, 0x280);
    for bit in (0..16).rev() {
        eeprom_bit(&mut cart, (0x1234u16 >> bit) as u8 & 1);
    }
    eeprom_cmd(&mut cart, 0x300);
    let mut word = 0u16;
    for _ in 0..16 {
        word = (word << 1) | (cart.read(0xA080) & 1) as u16;
        eeprom_bit(&mut cart, 0);
    }
    assert_eq!(word, 0x1234, "the word comes back");

    // Without EWEN a write is dropped: EWDS, then program word 1 and read $FFFF back.
    eeprom_cmd(&mut cart, 0x200);
    eeprom_cmd(&mut cart, 0x281);
    for bit in (0..16).rev() {
        eeprom_bit(&mut cart, (0x5678u16 >> bit) as u8 & 1);
    }
    eeprom_cmd(&mut cart, 0x301);
    let mut word = 0u16;
    for _ in 0..16 {
        word = (word << 1) | (cart.read(0xA080) & 1) as u16;
        eeprom_bit(&mut cart, 0);
    }
    assert_eq!(word, 0xFFFF, "write disabled");
}

/// A three-write MBC6 flash command: `$AA` at `2:5555`, `$55` at `1:4AAA`, the command at `2:5555`,
/// in the `MBC6.html` address notation. Leaves bank A on 2.
fn flash_command(cart: &mut Cartridge, command: u8) {
    cart.write(0x2000, 2);
    cart.write(0x5555, 0xAA);
    cart.write(0x2000, 1);
    cart.write(0x4AAA, 0x55);
    cart.write(0x2000, 2);
    cart.write(0x5555, command);
}

/// One 93LC56 bit: CS high with CLK low, then CLK high, which is the rising edge that shifts it.
fn eeprom_bit(cart: &mut Cartridge, bit: u8) {
    let pins = 0x80 | ((bit & 1) << 1);
    cart.write(0xA080, pins);
    cart.write(0xA080, pins | 0x40);
}

/// A 93LC56 instruction: CS low, CS high, then ten bits MSB first.
fn eeprom_cmd(cart: &mut Cartridge, instruction: u16) {
    cart.write(0xA080, 0x00);
    cart.write(0xA080, 0x80);
    for bit in (0..10).rev() {
        eeprom_bit(cart, (instruction >> bit) as u8 & 1);
    }
}
