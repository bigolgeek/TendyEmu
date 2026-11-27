//! The cartridge header: where it is, what it says and whether its checksum is good.
//! A bad checksum is **not** a refusal — homebrew images usually have several header errors,
//! and refusing them would refuse the test images too.

use crate::cart::Board;
use crate::cart::chipset::Chipset;
use crate::cart::refusal::Refusal;

/// The header, as the design's §2.7 declares it.
#[derive(Clone)]
pub struct Header {
    /// `$FFC0`-`$FFD4`, 21 bytes, trailing spaces/NULs trimmed.
    pub title: String,
    /// `$FFD5` bits 3-0: 0 LoROM, 1 HiROM, 5 ExHiROM.
    pub map_mode: u8,
    /// `$FFD5` bit 4: the cartridge asks for 3.58 MHz.
    pub fast: bool,
    /// `$FFD6`.
    pub chipset_byte: u8,
    /// `$FFBF`, only meaningful when `chipset_byte` is `$Fx`.
    pub chipset_subtype: u8,
    /// `$FFD7`: `(1 << n)` KiB.
    pub rom_size_code: u8,
    /// `$FFD8`: `(1 << n)` KiB, 0 = none.
    pub ram_size_code: u8,
    /// `$FFD9`.
    pub country: u8,
    /// `$FFDB`.
    pub version: u8,
    /// `$FFDE`.
    pub checksum: u16,
    /// `$FFDC`.
    pub checksum_complement: u16,
    /// `$FFDC` XOR `$FFDE` == `$FFFF` and the sum matches (§8.2).
    pub checksum_ok: bool,
    /// Where in the image the header was found.
    pub header_offset: usize,
    /// A 512-byte SWC/UFO header was stripped (§8.1).
    pub has_copier_header: bool,
}

/// The three candidate header positions, in the order a tie goes to.
const CANDIDATES: [(usize, Board); 3] = [
    (0x7FC0, Board::LoRom),
    (0xFFC0, Board::HiRom),
    (0x40FFC0, Board::ExHiRom),
];

/// `$FFD5`'s low nibble for a board.
fn map_mode_of(board: Board) -> u8 {
    match board {
        Board::LoRom => 0,
        Board::HiRom => 1,
        Board::ExHiRom => 5,
    }
}

impl Header {
    /// fullsnes "Country (FFD9h)": `$02-$0C` and `$11` are the PAL systems. §8.4 makes this a
    /// notice, not a refusal: v1 runs the cartridge at NTSC timing and the status bar says so.
    pub fn is_pal(&self) -> bool {
        matches!(self.country, 0x02..=0x0C | 0x11)
    }

    /// Finds the header in `rom` (copier header already stripped) and the board it describes.
    ///
    /// `Err` carries §8.4's `Refusal::NoHeader` for "the header could not be found". Everything
    /// else about a header — a bad checksum, a PAL country code, an oversized size code — is
    /// reported, not refused.
    ///
    /// An image whose every byte is the same value is refused however it scores: a zero-filled
    /// 32 KiB file satisfies `$FFD5`'s map nibble and nothing else, and running it as LoROM is a
    /// blank screen with a board name invented for it (§8.1).
    pub fn parse(rom: &[u8]) -> Result<(Header, Board), Refusal> {
        let mut best: Option<(i32, usize, Board)> = None;
        for (at, board) in CANDIDATES {
            let score = score(rom, at, board);
            if best.is_none_or(|(b, _, _)| score > b) {
                best = Some((score, at, board));
            }
        }
        let (best_score, at, board) = best.unwrap_or((0, CANDIDATES[0].0, CANDIDATES[0].1));
        let filler = rom
            .first()
            .is_some_and(|first| rom.iter().all(|byte| byte == first));
        if best_score < 4 || filler {
            return Err(Refusal::NoHeader {
                best_score: best_score.clamp(0, 13),
            });
        }
        let h = rom.get(at..at + 0x30).unwrap_or(&[]);
        let checksum = read_u16(h, 0x1E);
        let complement = read_u16(h, 0x1C);
        Ok((
            Header {
                title: title_of(rom, at),
                map_mode: byte(h, 0x15) & 0x0F,
                fast: byte(h, 0x15) & 0x10 != 0,
                chipset_byte: byte(h, 0x16),
                chipset_subtype: at.checked_sub(1).map_or(0, |i| rom[i]),
                rom_size_code: byte(h, 0x17),
                ram_size_code: byte(h, 0x18),
                country: byte(h, 0x19),
                version: byte(h, 0x1B),
                checksum,
                checksum_complement: complement,
                checksum_ok: (complement ^ checksum) == 0xFFFF && rom_checksum(rom, at) == checksum,
                header_offset: at,
                has_copier_header: false,
            },
            board,
        ))
    }
}

fn byte(h: &[u8], i: usize) -> u8 {
    h.get(i).copied().unwrap_or(0)
}

fn read_u16(h: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([byte(h, i), byte(h, i + 1)])
}

/// `$FFC0`-`$FFD4`, trailing spaces and NULs trimmed.
fn title_of(rom: &[u8], at: usize) -> String {
    let raw = rom.get(at..at + 0x15).unwrap_or(&[]);
    let text = String::from_utf8_lossy(raw);
    text.trim_end_matches([' ', '\0']).to_string()
}

/// The sum of every byte in the image, with the four checksum bytes counted as `FF FF 00 00`
/// (fullsnes `$FFDE`). The complement is its XOR with `$FFFF`.
fn rom_checksum(rom: &[u8], at: usize) -> u16 {
    let mut sum: u32 = 0;
    for (i, b) in rom.iter().enumerate() {
        sum += match i.wrapping_sub(at) {
            0x1C | 0x1D => 0xFF,
            0x1E | 0x1F => 0x00,
            _ => u32::from(*b),
        };
    }
    sum as u16
}

/// `(1 << code)` KiB is the declared size. The image is padded to a power of two at load and
/// fullsnes says the field is "rounded-up for carts with 10,12,20,24 Mbits", so v1 accepts a
/// declaration within one power of two either way; §8.6 leaves the exact window to the sweep.
fn rom_size_fits(code: u8, len: usize) -> bool {
    if code == 0 || code > 24 {
        return false;
    }
    let declared = 1024usize << code;
    declared >= len / 2 && declared <= len.saturating_mul(2)
}

/// Whether `$FFD6` names a board this table knows.
fn chipset_byte_is_named(byte: u8) -> bool {
    !matches!(Chipset::from_byte(byte, 0), Chipset::Unknown(_))
}

/// §8.1's score for one candidate: `$FFD5`'s map nibble (4), its speed bits (1), the checksum (4),
/// a plausible `$FFD7` (2), a printable title (1) and a named `$FFD6` (1). 13 is the maximum,
/// which is why a refusal prints `of 13`. A tie goes to the earlier candidate because
/// [`Header::parse`] only replaces the best on a strictly greater score.
pub fn score(rom: &[u8], at: usize, board: Board) -> i32 {
    if at + 0x30 > rom.len() {
        return i32::MIN;
    }
    let h = &rom[at..at + 0x30];
    let mut s = 0;
    if h[0x15] & 0x0F == map_mode_of(board) {
        s += 4;
    }
    if h[0x15] & 0xE0 == 0x20 {
        s += 1;
    }
    if checksum_matches(rom, at) {
        s += 4;
    }
    if h[0x17] != 0 && rom_size_fits(h[0x17], rom.len()) {
        s += 2;
    }
    if h[0x00..0x15]
        .iter()
        .all(|b| (0x20..=0x7E).contains(b) || *b == 0)
    {
        s += 1;
    }
    if chipset_byte_is_named(h[0x16]) {
        s += 1;
    }
    s
}

/// The header's own complement and sum agree and the image's sum matches.
fn checksum_matches(rom: &[u8], at: usize) -> bool {
    let h = &rom[at..at + 0x30];
    let checksum = u16::from_le_bytes([h[0x1E], h[0x1F]]);
    let complement = u16::from_le_bytes([h[0x1C], h[0x1D]]);
    (complement ^ checksum) == 0xFFFF && rom_checksum(rom, at) == checksum
}
