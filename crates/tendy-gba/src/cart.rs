//! The Game Boy Advance cartridge: the header at `$08000000` and the ROM the bus reads.
//!
//! GBATEK, *GBA Cartridge Header*, is the only source here: `$000` is the entry point (one 32-bit
//! ARM branch), `$004-$09F` the Nintendo logo, `$0A0-$0AB` the game title, `$0AC-$0AF` the game
//! code, `$0B0-$0B1` the maker code, `$0B2` a fixed `$96`, `$0BC` the software version and `$0BD`
//! the complement check of `$0A0-$0BC`. There is **no size field** anywhere in it — a Game Boy
//! Advance cartridge does not say how big it is, unlike a Game Boy one (`$0148`) — so the size the
//! machine works with is the length of the file, which is what the hardware sees too.
//!
//! A file that carries no plausible header is refused with a sentence naming the byte that failed,
//! the way `src/cart/header.rs` refuses a Game Boy cartridge. The refusal is deliberately generous
//! about what it *accepts* (any title, any maker): this is a plausibility test for "is this a GBA
//! cartridge at all", not a licence check.

use tendy_gb::checksum::crc32;

/// Bytes in the header: `$08000000-$080000BF`.
pub const HEADER_SIZE: usize = 0xC0;

/// Where a cartridge's ROM is mapped when the CPU first runs it (GBATEK, *GBA Memory Map*:
/// "`08000000-09FFFFFF` Game Pak ROM … Wait State 0").
pub const ROM_BASE: u32 = 0x0800_0000;

/// `$0B2`'s fixed byte (GBATEK: "Fixed value (must be 96h, required!)").
const FIXED_BYTE: u8 = 0x96;

/// The parsed header: the facts a person or a save file names a cartridge by, plus the two checks
/// that say it is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The word at `$000`, as stored.
    pub entry: u32,
    /// Where that branch lands, in cartridge address space: `$08000000 + 8 + offset`.
    pub entry_point: u32,
    /// `$0A0-$0AB`, up to the first NUL, printable ASCII only.
    pub title: String,
    /// `$0AC-$0AF`, four characters as stored.
    pub game_code: String,
    /// `$0B0-$0B1`.
    pub maker: String,
    /// `$0BC`.
    pub version: u8,
    /// The file's length: a GBA header names no size, so the cartridge's size is how much ROM there
    /// is on the file.
    pub rom_bytes: usize,
    /// `$0BD`.
    pub complement: u8,
    /// The complement check recomputed over `$0A0-$0BC` (GBATEK's one-byte sum).
    pub complement_ok: bool,
    /// Whether `$004-$09F` is the published Nintendo logo. Recorded, not required: the logo is a
    /// licence check on hardware, and a homebrew cartridge that omits it is still a cartridge this
    /// machine can read.
    pub logo_ok: bool,
}

impl Header {
    /// The header of `rom`, or the sentence that says why it is not one.
    pub fn parse(rom: &[u8]) -> Result<Header, String> {
        if rom.len() < HEADER_SIZE {
            return Err(format!(
                "ROM is {} bytes; a Game Boy Advance header alone needs {HEADER_SIZE} ($C0)",
                rom.len()
            ));
        }
        let word = |at: usize| u32::from_le_bytes([rom[at], rom[at + 1], rom[at + 2], rom[at + 3]]);
        let entry = word(0x000);
        // GBATEK, *000h - Entry Point*: "Space for a single 32bit ARM opcode that redirects to the
        // actual startaddress of the cartridge, this should be usually a 'B <start>' instruction."
        // Bits 27-25 = 101 is that branch; a file whose first word is not one is not a cartridge.
        if (entry >> 25) & 0b111 != 0b101 {
            return Err(format!(
                "the entry point at $08000000 is ${entry:08X}, not an ARM branch - this is not a Game Boy Advance cartridge"
            ));
        }
        if rom[0xB2] != FIXED_BYTE {
            return Err(format!(
                "header byte $B2 is ${:02X}, not ${FIXED_BYTE:02X} - this is not a Game Boy Advance cartridge",
                rom[0xB2]
            ));
        }
        // GBATEK's complement check: the two's complement of `$19` plus the sum of `$0A0-$0BC`,
        // compared with `$0BD`. Hardware locks up when it fails (the BIOS shows nothing), so a
        // mismatch is refused here rather than turned into a machine that runs nonsense.
        let mut x: u8 = 0;
        for &byte in &rom[0xA0..=0xBC] {
            x = x.wrapping_sub(byte);
        }
        x = x.wrapping_sub(0x19);
        let complement = rom[0xBD];
        let complement_ok = x == complement;
        if !complement_ok {
            return Err(format!(
                "header complement check is ${complement:02X}, need ${x:02X} - the cartridge header is damaged, or this is not a Game Boy Advance cartridge"
            ));
        }
        // The branch lands 8 bytes past the instruction, as every ARM branch does.
        let offset = ((entry << 8) as i32 >> 6) as u32;
        Ok(Header {
            entry_point: ROM_BASE.wrapping_add(8).wrapping_add(offset),
            entry,
            title: ascii_run(&rom[0xA0..0xAC]),
            game_code: ascii_run(&rom[0xAC..0xB0]),
            maker: ascii_run(&rom[0xB0..0xB2]),
            version: rom[0xBC],
            rom_bytes: rom.len(),
            complement,
            complement_ok,
            logo_ok: rom[0x04..0xA0] == NINTENDO_LOGO,
        })
    }
}

/// The published 156-byte Nintendo logo (`$004-$09F`), read from GBATEK's *004h..09Fh* table. Kept
/// as bytes rather than a bitmap because nothing here decodes it: it is compared, byte for byte.
///
/// It lives here so `logo_ok` can be recorded without a reader having to trust this file's copy of
/// the table over the cartridge's.
const NINTENDO_LOGO: [u8; 156] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD,
    0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20,
    0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC,
    0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76,
    0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25,
    0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xCB, 0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07,
];

/// A run of header text up to the first NUL, keeping only printable ASCII (the same rule
/// `src/cart/header.rs` applies to a Game Boy title, so a `.sav` name is built the same way).
fn ascii_run(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .filter(|b| (0x20..=0x7E).contains(*b))
        .map(|&b| b as char)
        .collect()
}

/// A cartridge: the ROM's bytes, its parsed header and the CRC-32 that keys a save file.
///
/// Read-only. Writes to cartridge ROM are ignored by the bus (GBATEK: the ROM region's write
/// column is empty); the save chips — SRAM, flash, EEPROM and the RTC — live in [`crate::save`]
/// and are built from the ROM's ID strings by [`crate::bus::Bus`].
#[derive(Clone, Debug)]
pub struct Cartridge {
    pub rom: Vec<u8>,
    pub header: Header,
    /// CRC-32 of the whole ROM: the disambiguator when two cartridges share a title, as on the
    /// Game Boy side.
    pub crc32: u32,
}

impl Cartridge {
    /// Parse `rom`'s header and hand back the cartridge, or the refusal sentence.
    pub fn new(rom: Vec<u8>) -> Result<Cartridge, String> {
        let header = Header::parse(&rom)?;
        let crc32 = crc32(&rom);
        Ok(Cartridge { rom, header, crc32 })
    }

    /// Where the CPU starts: the cartridge's entry point.
    pub fn entry_point(&self) -> u32 {
        self.header.entry_point
    }

    /// One byte of ROM at a cartridge offset, `0` past the end. The bus folds addresses past the
    /// ROM's size back into it before calling this, so a `0` here means an empty cartridge.
    pub fn read(&self, offset: usize) -> u8 {
        self.rom.get(offset).copied().unwrap_or(0)
    }

    /// Bytes of ROM.
    pub fn len(&self) -> usize {
        self.rom.len()
    }

    /// True for a cartridge with no ROM, which cannot happen through `new` and is only here so a
    /// reader of `len` does not have to wonder.
    pub fn is_empty(&self) -> bool {
        self.rom.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header for a cartridge whose code starts at `$080000C0`, with `title`/`code`/`maker` set.
    fn header_bytes(title: &[u8], code: &[u8], maker: &[u8]) -> [u8; HEADER_SIZE] {
        let mut rom = [0u8; HEADER_SIZE];
        // `b $080000C0`: the offset is (0xC0 - 8) / 4 = 0x2E.
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        rom[0x04..0xA0].copy_from_slice(&NINTENDO_LOGO);
        rom[0xA0..0xA0 + title.len()].copy_from_slice(title);
        rom[0xAC..0xAC + code.len()].copy_from_slice(code);
        rom[0xB0..0xB0 + maker.len()].copy_from_slice(maker);
        rom[0xB2] = FIXED_BYTE;
        let mut x: u8 = 0;
        for &byte in &rom[0xA0..=0xBC] {
            x = x.wrapping_sub(byte);
        }
        rom[0xBD] = x.wrapping_sub(0x19);
        rom
    }

    #[test]
    fn a_header_parses_its_entry_point_and_its_names() {
        let rom = header_bytes(b"TEST GAME\0\0\0", b"ABCD", b"01").to_vec();
        let cart = Cartridge::new(rom).expect("a synthesized header");
        assert_eq!(cart.entry_point(), 0x0800_00C0);
        assert_eq!(cart.header.title, "TEST GAME");
        assert_eq!(cart.header.game_code, "ABCD");
        assert_eq!(cart.header.maker, "01");
        assert_eq!(cart.header.rom_bytes, HEADER_SIZE);
        assert!(cart.header.complement_ok);
        assert!(cart.header.logo_ok);
    }

    #[test]
    fn a_file_too_short_for_a_header_is_refused_by_size() {
        let why = Cartridge::new(vec![0u8; 0x40]).unwrap_err();
        assert!(why.contains("64 bytes"), "{why}");
    }

    #[test]
    fn a_first_word_that_is_not_a_branch_is_refused() {
        let mut rom = header_bytes(b"TEST GAME\0\0\0", b"ABCD", b"01").to_vec();
        rom[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let why = Cartridge::new(rom).unwrap_err();
        assert!(why.contains("not an ARM branch"), "{why}");
    }

    #[test]
    fn a_bad_fixed_byte_is_refused() {
        let mut rom = header_bytes(b"TEST GAME\0\0\0", b"ABCD", b"01").to_vec();
        rom[0xB2] = 0x00;
        let why = Cartridge::new(rom).unwrap_err();
        assert!(why.contains("$B2"), "{why}");
    }

    #[test]
    fn a_bad_complement_check_is_refused() {
        let mut rom = header_bytes(b"TEST GAME\0\0\0", b"ABCD", b"01").to_vec();
        rom[0xBD] = rom[0xBD].wrapping_add(1);
        let why = Cartridge::new(rom).unwrap_err();
        assert!(why.contains("complement check"), "{why}");
    }
}
