//! The EEPROM protocol at the lengths the Classic NES Series cartridges ask for (H19).
//!
//! The five cartridges that went black when the chip first answered are one family: twelve Classic
//! NES Series cartridges, all with `EEPROM_V124`, all of which draw the cartridge's own "GAME PAK
//! ERROR / TURN THE POWER OFF." screen in this build. What H19 measured, and what these tests pin:
//!
//! - the two documented request shapes and the sizes they decide are GBATEK's: `9`-bit read /
//!   `73`-bit write = 512 bytes, `17`-bit read / `81`-bit write = 8 KiB (*GBA Cart Backup EEPROM*:
//!   "n bits eeprom address (MSB first, 6 or 14 bits, depending on EEPROM)"; "for 512 Bytes EEPROMS:
//!   an address range of 0-3Fh, 6bit bus width; and for 8KByte EEPROMs: a range of 0-3FFh, 14bit bus
//!   width");
//! - the 8 KiB chip round-trips an 81-bit write and a 17-bit read, and the 512-byte chip the same
//!   in its own length;
//! - **the trailing bit those cartridges send is not a request field at all** — it is bit 49 of the
//!   block the cartridge itself last wrote, 52 of 52 write-then-read pairs across all twelve
//!   cartridges. That is the evidence this file pins. The chip used to enforce GBATEK's "1 bit 0"
//!   there and refuse the read; it now clocks the bit without sampling it (the chip drives that
//!   wire itself on a read: `save.rs`'s note), because enforcing it refused reads that three other
//!   cartridges — 0352, 0490 and 0785 — issue on every save, and turned them into 12.7M Ready polls
//!   per run. The twelve Classic NES Series cartridges' own behaviour is unchanged by that: all
//!   twelve still show the same one-colour screen at frame 1 200.
//!
//! The chip is driven the way the hardware is, one halfword per bit with only bit 0 used (GBATEK,
//! *Using DMA*), through [`Eeprom::write_bit`] and [`Eeprom::read_bit`].

use tendy_gba::save::{Eeprom, EepromSize};

/// A read request: `11`, the address MSB first, and the trailing bit the caller supplies. GBATEK's
/// *Set Address (For Reading)* ends it with `0`; the Classic NES Series ends it with data.
fn read_request(addr: u16, address_bits: usize, trailing: bool) -> Vec<bool> {
    let mut bits = vec![true, true];
    for bit in (0..address_bits).rev() {
        bits.push(addr & (1 << bit) != 0);
    }
    bits.push(trailing);
    bits
}

/// A write request: `10`, the address MSB first, 64 data bits MSB first, and a `0` — GBATEK's
/// *Write Data to Address*.
fn write_request(addr: u16, data: [u8; 8], address_bits: usize) -> Vec<bool> {
    let mut bits = vec![true, false];
    for bit in (0..address_bits).rev() {
        bits.push(addr & (1 << bit) != 0);
    }
    for byte in data {
        for bit in (0..8).rev() {
            bits.push(byte & (1 << bit) != 0);
        }
    }
    bits.push(false);
    bits
}

/// Clock a request into the chip one bit at a time, as a DMA of halfwords does.
fn clock_in(chip: &mut Eeprom, bits: &[bool]) {
    for (index, &bit) in bits.iter().enumerate() {
        chip.write_bit(bit, 300_000 + index as u64);
    }
}

/// Clock `count` bits out of the chip and decode the 64 data bits as eight bytes, byte 0 first
/// (GBATEK: "4 bits - ignore these / 64 bits - data").
fn clock_out(chip: &mut Eeprom, count: usize) -> [u8; 8] {
    let bits: Vec<bool> = (0..count).map(|_| chip.read_bit(900_000)).collect();
    let mut bytes = [0u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        for bit in 0..8 {
            if bits.get(4 + index * 8 + bit) == Some(&true) {
                *byte |= 1 << (7 - bit);
            }
        }
    }
    bytes
}

#[test]
fn both_chips_round_trip_a_write_and_a_read_in_their_own_lengths() {
    for (size, address_bits, addr) in [
        (EepromSize::K4bit, 6usize, 31u16),
        (EepromSize::K64kbit, 14, 511),
    ] {
        let mut chip = Eeprom::with_size(size);
        let block = [0xA5, 0x5A, 0x00, 0xFF, 0x12, 0x34, 0x56, 0x78];
        clock_in(&mut chip, &write_request(addr, block, address_bits));
        clock_in(&mut chip, &read_request(addr, address_bits, false));
        assert_eq!(
            clock_out(&mut chip, 68),
            block,
            "{size:?} block {addr} round-trips"
        );
    }
}

#[test]
fn the_eight_kib_chip_is_the_one_the_two_zelda_cartridges_ask_for() {
    // 1512 and 1745 clock 17-bit reads and 81-bit writes; 1516, 1521 and 1747 clock the 512-byte
    // shapes. Both sizes store and return the same eight bytes.
    let mut chip = Eeprom::new();
    clock_in(&mut chip, &read_request(511, 14, false));
    assert_eq!(
        chip.known_size(),
        Some(EepromSize::K64kbit),
        "a 17-bit read is the 8 KiB chip's: GBATEK's \"14 bits, depending on EEPROM\""
    );
    let block = [0x04, 0x4C, 0x45, 0x5A, 0x49, 0x10, 0x13, 0x03];
    clock_in(&mut chip, &write_request(511, block, 14));
    clock_in(&mut chip, &read_request(511, 14, false));
    assert_eq!(clock_out(&mut chip, 68), block);
}

#[test]
fn the_stale_trailing_bit_the_classic_nes_cartridges_send_is_not_sampled() {
    // H19's finding: across all twelve Classic NES Series cartridges, 52 of 52 read requests that
    // follow a write carry bit 49 of the written block in the trailing position instead of GBATEK's
    // "0". The bit is the cartridge's own buffer, not a request field, and on a read the chip is
    // driving that wire itself (save.rs's note) — so it is clocked and not sampled, and the block
    // comes back either way.
    let mut chip = Eeprom::with_size(EepromSize::K4bit);
    let block = [0x5F, 0x4F, 0x49, 0x52, 0x31, 0x18, 0x16, 0x03]; // 0x16, bit 6 is data bit 49
    clock_in(&mut chip, &write_request(31, block, 6));
    clock_in(&mut chip, &read_request(31, 6, true));
    assert_eq!(
        clock_out(&mut chip, 68),
        block,
        "the read is answered with the block, not with 68 Ready bits"
    );
    // The documented shape reads the same block, so the trailing bit is the only difference.
    clock_in(&mut chip, &read_request(31, 6, false));
    assert_eq!(clock_out(&mut chip, 68), block);
    // After either read-out the chip is idle, so bit 0 of the window is Ready again.
    assert_eq!(
        u8::from(chip.read_bit(1_000_000)),
        1,
        "a complete read leaves the chip Ready"
    );
}
