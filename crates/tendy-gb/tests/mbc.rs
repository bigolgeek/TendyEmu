//! The mappers' bank arithmetic: MBC1 incl. multicart, MBC2, MBC3+RTC, MBC5,
//! HuC1 and ROM only. Every ROM is built as bytes in code.

mod common;

use tendy_gb::Cartridge;

/// The byte a marker sets at the start of `bank`.
fn bank_byte(bank: usize) -> u8 {
    0xA0u8.wrapping_add(bank as u8)
}

/// A ROM with a marker byte at offset 0 of every 16 KiB bank.
fn marked_rom(banks: usize, cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = rom_bytes(banks, cart_type, ram_code);
    for bank in 0..banks {
        rom[bank * 0x4000] = bank_byte(bank);
    }
    rom
}

fn rom_bytes(banks: usize, cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = common::synth_rom_banked(banks, cart_type, &common::entry(&[0x00]));
    rom[0x0149] = ram_code;
    fix_checksum(&mut rom);
    rom
}

fn fix_checksum(rom: &mut [u8]) {
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x014D] = x;
}

fn cart(banks: usize, cart_type: u8, ram_code: u8) -> Cartridge {
    Cartridge::new(marked_rom(banks, cart_type, ram_code)).expect("cartridge")
}

#[test]
fn mbc1_bank_zero_maps_to_one() {
    let mut c = cart(4, 0x01, 0x00);
    assert_eq!(c.read(0x0000), bank_byte(0));
    // The register starts at 0, which the mapper reads as bank 1 (MBC1.html).
    assert_eq!(c.read(0x4000), bank_byte(1));

    c.write(0x2000, 0x02);
    assert_eq!(c.read(0x4000), bank_byte(2));

    c.write(0x2000, 0x00);
    assert_eq!(c.read(0x4000), bank_byte(1));

    // The low bank stays put in mode 0.
    assert_eq!(c.read(0x0000), bank_byte(0));
}

#[test]
fn mbc1_256k_can_select_bank_zero_via_bit4() {
    // 16 banks: bit 4 of the 5-bit register reaches bank 16, which wraps to bank 0 on a 256 KiB
    // cartridge - the one way bank 0 is reachable at $4000-$7FFF (MBC1.html).
    let mut c = cart(16, 0x01, 0x00);
    c.write(0x2000, 0x10);
    assert_eq!(c.read(0x4000), bank_byte(0));

    c.write(0x2000, 0x11);
    assert_eq!(c.read(0x4000), bank_byte(1));

    c.write(0x2000, 0x1F);
    assert_eq!(c.read(0x4000), bank_byte(15));
}

#[test]
fn mbc1_ram_enable_nibble() {
    let mut c = cart(4, 0x03, 0x02);
    assert_eq!(c.ram().len(), 0x2000);

    // Disabled: reads $FF and writes are dropped.
    assert_eq!(c.read(0xA000), 0xFF);
    c.write(0xA000, 0x42);
    assert_eq!(c.read(0xA000), 0xFF);

    c.write(0x0000, 0x0A);
    assert_eq!(c.read(0xA000), 0x00);
    c.write(0xA000, 0x42);
    assert_eq!(c.read(0xA000), 0x42);

    // The comparison is on the low nibble only.
    c.write(0x0000, 0x1A);
    c.write(0xBFFF, 0x5A);
    assert_eq!(c.read(0xBFFF), 0x5A);

    c.write(0x0000, 0x00);
    assert_eq!(c.read(0xA000), 0xFF);
    assert_eq!(c.read(0xBFFF), 0xFF);
}

#[test]
fn mbc1_mode1_remaps_zero_bank() {
    // 1 MiB MBC1 with 32 KiB RAM: the 2-bit register drives ROM bits 5-6 and RAM bank 0-3.
    let mut c = cart(64, 0x03, 0x03);
    c.write(0x0000, 0x0A);
    c.write(0x6000, 0x01);
    c.write(0x4000, 0x01);

    assert_eq!(c.read(0x0000), bank_byte(0x20));
    assert_eq!(c.read(0x4000), bank_byte(0x21));

    // RAM bank 1 while mode is 1.
    c.write(0xA000, 0x5A);
    assert_eq!(c.ram()[0x2000], 0x5A);
    assert_eq!(c.read(0xA000), 0x5A);

    // Back to mode 0: bank 0 at $0000 and RAM bank 0 at $A000.
    c.write(0x6000, 0x00);
    assert_eq!(c.read(0x0000), bank_byte(0));
    assert_eq!(c.read(0x4000), bank_byte(0x21));
    assert_eq!(c.read(0xA000), 0x00);
}

#[test]
fn mbc1m_detected_by_logo_in_bank_16() {
    const LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00,
        0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD,
        0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB,
        0xB9, 0x33, 0x3E,
    ];

    let mut mbc1m = marked_rom(64, 0x01, 0x00);
    mbc1m[0x0104..0x0134].copy_from_slice(&LOGO);
    mbc1m[0x4_0104..0x4_0134].copy_from_slice(&LOGO);
    let mut c = Cartridge::new(mbc1m).expect("cartridge");
    assert_eq!(c.mbc_name(), "mbc1m");

    // The 2-bit register drives ROM bits 4-5, so $00 in the 5-bit register plus bank_high 1 is
    // bank $11, and mode 1 gives bank $10 at $0000.
    c.write(0x6000, 0x01);
    c.write(0x4000, 0x01);
    c.write(0x2000, 0x00);
    assert_eq!(c.read(0x4000), bank_byte(0x11));
    assert_eq!(c.read(0x0000), bank_byte(0x10));

    // A 1 MiB ROM without the logo in bank 16 is an ordinary MBC1: same writes, banks $21/$20.
    let mut plain = marked_rom(64, 0x01, 0x00);
    plain[0x0104..0x0134].copy_from_slice(&LOGO);
    let mut c = Cartridge::new(plain).expect("cartridge");
    assert_eq!(c.mbc_name(), "mbc1");
    c.write(0x6000, 0x01);
    c.write(0x4000, 0x01);
    c.write(0x2000, 0x00);
    assert_eq!(c.read(0x4000), bank_byte(0x21));
    assert_eq!(c.read(0x0000), bank_byte(0x20));
}

#[test]
fn mbc2_nibble_ram_and_echo() {
    let mut c = cart(8, 0x06, 0x00);
    assert_eq!(c.ram().len(), 512);

    c.write(0x0000, 0x0A);
    c.write(0xA000, 0xAB);
    // Only the low nibble is stored; the high nibble reads $F.
    assert_eq!(c.ram()[0], 0x0B);
    assert_eq!(c.read(0xA000), 0xFB);

    // The low nine address bits are all that is decoded, so the top half echoes the bottom.
    assert_eq!(c.read(0xA200), 0xFB);
    c.write(0xA1FF, 0x05);
    assert_eq!(c.read(0xBFFF), 0xF5);
    assert_eq!(c.read(0xA1FF), 0xF5);
    assert_eq!(c.read(0xA000), 0xFB);

    // Disabled: $FF, and the cell keeps its nibble.
    c.write(0x0000, 0x00);
    assert_eq!(c.read(0xA000), 0xFF);
    assert_eq!(c.ram()[0], 0x0B);
}

#[test]
fn mbc2_bank_uses_address_bit8() {
    // Bit 8 of the write address chooses the register: set means ROM bank, clear means RAM enable.
    let mut c = cart(8, 0x06, 0x00);
    c.write(0x2100, 0x03);
    assert_eq!(c.read(0x4000), bank_byte(3));

    // Bank 0 reads as bank 1.
    c.write(0x2100, 0x00);
    assert_eq!(c.read(0x4000), bank_byte(1));

    // A write with bit 8 clear is the RAM enable and leaves the bank alone.
    c.write(0x0000, 0x0A);
    assert_eq!(c.read(0x4000), bank_byte(1));
    c.write(0xA000, 0x07);
    assert_eq!(c.read(0xA000), 0xF7);

    // 4-bit register: $13 is bank 3.
    c.write(0x2100, 0x13);
    assert_eq!(c.read(0x4000), bank_byte(3));

    // Writes to $4000-$7FFF are not registers.
    let before = c.read(0x4000);
    c.write(0x4000, 0x05);
    assert_eq!(c.read(0x4000), before);
}

#[test]
fn mbc3_ram_and_rtc_select() {
    let mut c = cart(4, 0x10, 0x03);
    assert_eq!(c.ram().len(), 0x8000);
    c.write(0x0000, 0x0A);

    // $00-$07 pick a RAM bank.
    c.write(0x4000, 0x01);
    c.write(0xA000, 0x99);
    assert_eq!(c.ram()[0x2000], 0x99);
    c.write(0x4000, 0x00);
    assert_eq!(c.read(0xA000), 0x00);

    // $08-$0C pick a clock register, which needs a latch to have any value.
    c.write(0x4000, 0x08);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 0x00);

    // $0D and up are nothing at all.
    c.write(0x4000, 0x0D);
    assert_eq!(c.read(0xA000), 0xFF);
    c.write(0x4000, 0xFF);
    assert_eq!(c.read(0xA000), 0xFF);

    // Without a clock the $08-$0C selections read $FF.
    let mut no_clock = cart(4, 0x13, 0x02);
    no_clock.write(0x0000, 0x0A);
    no_clock.write(0x4000, 0x08);
    assert_eq!(no_clock.read(0xA000), 0xFF);
    no_clock.write(0xA000, 0x12);
    assert_eq!(no_clock.read(0xA000), 0xFF);
}

#[test]
fn mbc3_rtc_latch() {
    let mut c = cart(4, 0x10, 0x02);
    c.write(0x0000, 0x0A);
    c.write(0x4000, 0x08);
    {
        let rtc = c.rtc.as_mut().expect("clock");
        rtc.secs = 42;
        rtc.mins = 7;
    }

    // UNVERIFIED: the latch is taken on the $00 -> $01 transition, so $01 twice does not re-latch
    // and $01 without a preceding $00 does nothing (MBC3.html describes only the $00 then $01 pair).
    // Nothing is latched yet, so the latched copy is still zero.
    assert_eq!(c.read(0xA000), 0x00);

    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 42);
    c.write(0x4000, 0x09);
    assert_eq!(c.read(0xA000), 7);

    // The live counters move on; reads keep answering the latched copy.
    c.write(0x4000, 0x08);
    c.rtc.as_mut().expect("clock").secs = 43;
    assert_eq!(c.read(0xA000), 42);

    // $01 twice is not a latch; it takes $00 then $01.
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 42);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 43);
}

#[test]
fn mbc3_rtc_counts_across_sub_second_ticks() {
    // The front end ticks the clock once per frame, so a 16 ms step must not be rounded away: the
    // epoch only moves a whole second at a time.
    let mut c = cart(4, 0x10, 0x02);
    c.rtc.as_mut().expect("clock").base_ms = 1_000;
    for i in 1..=100 {
        c.tick_rtc(1_000 + i * 16);
    }
    let rtc = c.rtc.as_ref().expect("clock");
    assert_eq!(rtc.secs, 1, "1.6 s of frames is one second");
    assert_eq!(rtc.base_ms, 2_000, "the epoch moves in whole seconds");

    // The 600 ms left over is not lost: 400 ms more reaches the second after it.
    for i in 101..=125 {
        c.tick_rtc(1_000 + i * 16);
    }
    let rtc = c.rtc.as_ref().expect("clock");
    assert_eq!(rtc.secs, 2, "a second later");
    assert_eq!(rtc.base_ms, 3_000);
}

#[test]
fn mbc3_day_carry_sticks() {
    let mut c = cart(4, 0x0F, 0x00);
    c.write(0x0000, 0x0A);
    {
        let rtc = c.rtc.as_mut().expect("clock");
        rtc.secs = 59;
        rtc.mins = 59;
        rtc.hours = 23;
        rtc.days_low = 0xFF;
        rtc.days_high = 0x01; // day 511, the last one
        rtc.base_ms = 1_000;
    }

    // UNVERIFIED: on overflow the counter wraps to day 0 and the carry bit latches, which is what
    // MBC3.html says happens, but the wrap value itself is not spelled out there.
    // One second later the day counter overflows: it wraps to 0 and the carry bit comes on.
    c.tick_rtc(2_000);
    c.write(0x4000, 0x0C);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 0x80);
    c.write(0x4000, 0x0B);
    assert_eq!(c.read(0xA000), 0x00);

    // It stays set until the program clears it.
    c.tick_rtc(12_000);
    c.write(0x4000, 0x0C);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 0x80);

    c.write(0xA000, 0x00);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), 0x00);

    // Bit 6 stops the counter without disturbing the rest.
    c.write(0xA000, 0x40);
    c.write(0x4000, 0x08);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    let frozen = c.read(0xA000);
    c.tick_rtc(912_000);
    c.write(0x6000, 0x00);
    c.write(0x6000, 0x01);
    assert_eq!(c.read(0xA000), frozen);
}

#[test]
fn mbc5_nine_bit_bank() {
    // 512 banks is the most the 9-bit register can reach: 8 MiB. Two banks carry a marker that no
    // other bank shares, so a wrap cannot pass for a hit.
    let mut rom = rom_bytes(512, 0x19, 0x02);
    rom[0x23 * 0x4000] = 0x23;
    rom[0x123 * 0x4000] = 0x77;
    let mut c = Cartridge::new(rom).expect("cartridge");

    // $2000-$2FFF is the low eight bits, $3000-$3FFF the ninth.
    c.write(0x2000, 0x23);
    c.write(0x3000, 0x01);
    assert_eq!(c.read(0x4000), 0x77);

    c.write(0x3000, 0x00);
    assert_eq!(c.read(0x4000), 0x23);

    // Only one bit of the second address reaches the register.
    c.write(0x3000, 0xFF);
    assert_eq!(c.read(0x4000), 0x77);
    c.write(0x3000, 0x00);
}

#[test]
fn mbc5_bank_zero_is_zero() {
    let mut c = cart(4, 0x1B, 0x03);

    // MBC5 has no 0 -> 1 translation.
    c.write(0x2000, 0x00);
    assert_eq!(c.read(0x4000), bank_byte(0));

    c.write(0x2000, 0x03);
    assert_eq!(c.read(0x4000), bank_byte(3));

    // 4-bit RAM bank, and bit 3 is the rumble motor, not a bank bit.
    c.write(0x0000, 0x0A);
    c.write(0x4000, 0x09);
    c.write(0xA000, 0x7E);
    assert_eq!(c.ram()[0x2000], 0x7E);
    assert_eq!(c.read(0xA000), 0x7E);

    c.write(0x4000, 0x02);
    assert_eq!(c.read(0xA000), 0x00);

    // Disabled again.
    c.write(0x0000, 0x00);
    assert_eq!(c.read(0xA000), 0xFF);
}

#[test]
fn huc1_ram_enable_values() {
    let mut c = cart(4, 0xFF, 0x02);
    assert_eq!(c.mbc_name(), "huc1");

    assert_eq!(c.read(0xA000), 0xFF);
    // UNVERIFIED: Pan Docs has no HuC1 page; $A and $E as the enable nibbles with no IR port are
    // what the cartridge does, settled by its own "9 HuC1 titles" sweep.
    // HuC1 accepts $E as the enable nibble ...
    c.write(0x0000, 0x0E);
    assert_eq!(c.read(0xA000), 0x00);
    c.write(0xA000, 0x11);
    assert_eq!(c.read(0xA000), 0x11);

    // ... and $A as any MBC1 does.
    c.write(0x0000, 0x0A);
    c.write(0xA000, 0x22);
    assert_eq!(c.read(0xA000), 0x22);

    c.write(0x0000, 0x0B);
    assert_eq!(c.read(0xA000), 0xFF);

    // Same bank arithmetic as MBC1: 0 -> 1.
    assert_eq!(c.read(0x4000), bank_byte(1));
}

#[test]
fn romonly_reads_and_writes_flat_ram() {
    let mut c = cart(2, 0x00, 0x02);
    assert_eq!(c.ram().len(), 0x2000);
    assert_eq!(c.read(0x0000), bank_byte(0));
    assert_eq!(c.read(0x4000), bank_byte(1));

    // Writing to $2000-$5FFF does nothing: there is no bank register.
    c.write(0x2000, 0x03);
    assert_eq!(c.read(0x4000), bank_byte(1));
    assert_eq!(c.read(0x0000), bank_byte(0));

    c.write(0xA000, 0x5A);
    assert_eq!(c.read(0xA000), 0x5A);
    c.write(0xBFFF, 0xA5);
    assert_eq!(c.read(0xBFFF), 0xA5);
    assert_eq!(c.ram()[0x1FFF], 0xA5);

    // No RAM in the header: nothing to read or write.
    let mut none = cart(2, 0x00, 0x00);
    assert_eq!(none.ram().len(), 0);
    none.write(0xA000, 0x5A);
    assert_eq!(none.read(0xA000), 0xFF);
}

#[test]
fn rom_only_header_over_32k_is_treated_as_mbc1() {
    // A "ROM ONLY" header cannot address past the first 32 KiB, so a cartridge that declares one
    // and is larger than that is banked; Pan Docs (MBC1, "Note") says to treat it as MBC1.
    // Mick & Mack (proto) is 128 KiB behind $0147 = $00 and never drew while the writes were
    // swallowed.
    let mut c = cart(4, 0x00, 0x00);
    assert_eq!(c.mbc.name(), "mbc1");
    assert_eq!(c.read(0x4000), bank_byte(1));

    c.write(0x2000, 0x02);
    assert_eq!(c.read(0x4000), bank_byte(2));

    // Exactly 32 KiB is still ROM only: there is nothing to switch to.
    assert_eq!(cart(2, 0x00, 0x00).mbc.name(), "romonly");
}
