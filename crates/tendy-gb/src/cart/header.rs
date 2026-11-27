//! The cartridge header: title, size codes, both checksums and the refusal message.

use crate::ROM_BANK_SIZE;

/// The parsed header and its two size fields.
#[derive(Clone)]
pub struct Header {
    /// `$0134-$0143`, printable ASCII, trailing NULs trimmed.
    pub title: String,
    /// `$0134-$0143` exactly as stored, so the compatibility palette can sum them and read the
    /// fourth letter (Pan Docs, *Power-Up Sequence*, "Compatibility palettes").
    pub title_bytes: [u8; 16],
    /// `$0144-$0145`, the new licensee code (Pan Docs, *The Cartridge Header*).
    pub new_licensee: [u8; 2],
    /// `$014B`, the old licensee code.
    pub old_licensee: u8,
    /// `$0147`.
    pub cart_type: u8,
    /// `$0148`.
    pub rom_size_code: u8,
    /// `$0149`.
    pub ram_size_code: u8,
    /// `$0143`, kept so a refusal can name it.
    pub cgb_flag: u8,
    pub rom_bytes: usize,
    pub ram_bytes: usize,
    /// `$0147` is `$0F` or `$10`.
    pub has_rtc: bool,
    /// `$014D` as stored.
    pub header_checksum: u8,
    /// The header checksum's comparison.
    pub header_ok: bool,
}

impl Header {
    /// Parses the header. `Err` is the refusal message.
    pub fn parse(rom: &[u8]) -> Result<Header, String> {
        if rom.len() < 0x0150 {
            return Err(format!("ROM is {} bytes, need at least 32768", rom.len()));
        }
        let cart_type = rom[0x0147];
        let rom_size_code = rom[0x0148];
        let ram_size_code = rom[0x0149];
        let rom_bytes = match rom_size_code {
            code @ 0x00..=0x08 => 0x8000usize << code,
            0x52 => 72 * ROM_BANK_SIZE,
            0x53 => 80 * ROM_BANK_SIZE,
            0x54 => 96 * ROM_BANK_SIZE,
            _ => 0x8000,
        };
        let ram_bytes = match ram_size_code {
            0x00 => 0,
            0x01 | 0x02 => 0x2000,
            0x03 => 0x8000,
            0x04 => 0x2_0000,
            0x05 => 0x1_0000,
            _ => 0,
        };
        let mut x: u8 = 0;
        for &byte in &rom[0x0134..=0x014C] {
            x = x.wrapping_sub(byte).wrapping_sub(1);
        }
        let header_checksum = rom[0x014D];
        Ok(Header {
            title: title_of(rom),
            title_bytes: rom[0x0134..=0x0143].try_into().expect("16 title bytes"),
            new_licensee: [rom[0x0144], rom[0x0145]],
            old_licensee: rom[0x014B],
            cart_type,
            rom_size_code,
            ram_size_code,
            cgb_flag: rom[0x0143],
            rom_bytes,
            ram_bytes,
            has_rtc: matches!(cart_type, 0x0F | 0x10),
            header_checksum,
            header_ok: header_checksum == x,
        })
    }
}

/// `$0134-$0143` up to the first NUL, printable ASCII only.
fn title_of(rom: &[u8]) -> String {
    let raw = &rom[0x0134..=0x0143];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    raw[..end]
        .iter()
        .filter(|b| (0x20..=0x7E).contains(*b))
        .map(|&b| b as char)
        .collect()
}

/// The Pan Docs name for a `$0147` byte, `unknown` when the table has none.
pub fn cart_type_name(cart_type: u8) -> &'static str {
    match cart_type {
        0x00 => "ROM ONLY",
        0x01 => "MBC1",
        0x02 => "MBC1+RAM",
        0x03 => "MBC1+RAM+BATTERY",
        0x05 => "MBC2",
        0x06 => "MBC2+BATTERY",
        0x08 => "ROM+RAM",
        0x09 => "ROM+RAM+BATTERY",
        0x0B => "MMM01",
        0x0C => "MMM01+RAM",
        0x0D => "MMM01+RAM+BATTERY",
        0x0F => "MBC3+TIMER+BATTERY",
        0x10 => "MBC3+TIMER+RAM+BATTERY",
        0x11 => "MBC3",
        0x12 => "MBC3+RAM",
        0x13 => "MBC3+RAM+BATTERY",
        0x19 => "MBC5",
        0x1A => "MBC5+RAM",
        0x1B => "MBC5+RAM+BATTERY",
        0x1C => "MBC5+RUMBLE",
        0x1D => "MBC5+RUMBLE+RAM",
        0x1E => "MBC5+RUMBLE+RAM+BATTERY",
        0x20 => "MBC6",
        0x22 => "MBC7+SENSOR+RUMBLE+RAM+BATTERY",
        0xE1 => "M161",
        0xFC => "POCKET CAMERA",
        0xFD => "BANDAI TAMA5",
        0xFE => "HuC3",
        0xFF => "HuC1+RAM+BATTERY",
        _ => "unknown",
    }
}

/// The one-line refusal.
pub fn refusal(cart_type: u8) -> String {
    format!(
        "unsupported cartridge type ${cart_type:02X} ({}) - supported: ROM only, MBC1, MBC1M, MBC2, MBC3, MBC30, MBC5, MBC5+rumble, MBC6, MBC7, MMM01, HuC1, HuC3, M161",
        cart_type_name(cart_type)
    )
}
