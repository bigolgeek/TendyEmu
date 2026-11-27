//! Cheat codes: the two formats v1 understands, the parse, and the book the bus's read path asks.
//!
//! The layouts are Pan Docs' "Game Genie/Shark Cheats" chapter, plus the Game Genie codebook's own
//! worked examples for the nine-digit compare byte:
//!
//! - **Game Genie**, six or nine hex digits written `ABC-DEF` / `ABC-DEF-GHI` (the dashes are
//!   optional). `AB` is the new byte, `FCDE` the ROM address with its top nibble complemented
//!   (`XOR $F000`), and in the nine-digit form `GI` carries the old byte that the patch only
//!   replaces while the cartridge still holds it. `H` is the adapter's own nibble and is ignored.
//! - **GameShark**, eight hex digits `01vvaabb`: the `01` says a RAM write, `vv` is the new byte
//!   and `aabb` the address with its two bytes the wrong way round (`010238CD` writes `$02` at
//!   `$CD38`).
//!
//! Both are read patches, which is what the hardware is: the adapter compares the address (and,
//! for a nine-digit code, the old byte) with the read the game is making and drives its own byte
//! onto the bus. The bus asks this book after its normal decode, and an empty book is one branch.

use crate::cart::Cartridge;

/// Which adapter a code was written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Game Genie: replaces the byte a ROM read answers with.
    Genie,
    /// GameShark: substitutes the byte at a work or cartridge RAM address.
    Shark,
}

impl Format {
    /// The name a refusal or the drawer uses.
    pub fn name(self) -> &'static str {
        match self {
            Format::Genie => "Game Genie",
            Format::Shark => "GameShark",
        }
    }
}

/// Why a piece of text is not a code v1 accepts. The variants are the words the refusal uses, so a
/// player sees `cheat 020238CD: unsupported prefix` rather than a silent drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer than six hex digits: shorter than any code of either format.
    TooShort,
    /// A character outside `0-9A-F`.
    NotHex,
    /// A length neither format uses.
    BadLength,
    /// Eight digits that do not begin `01`: a type this build does not implement.
    UnsupportedPrefix,
    /// A Game Genie address above `$7FFF`, which a ROM patch cannot reach.
    Address,
}

impl ParseError {
    /// The refusal's reason, as the drawer's notice line and the report spell it.
    pub fn label(self) -> &'static str {
        match self {
            ParseError::TooShort => "too short",
            ParseError::NotHex => "not hex",
            ParseError::BadLength => "wrong length (6 or 9 Game Genie digits, 8 GameShark digits)",
            ParseError::UnsupportedPrefix => "unsupported prefix (a GameShark code begins 01)",
            ParseError::Address => "address above $7FFF",
        }
    }

    /// The whole notice: the code as the player wrote it, then the reason.
    pub fn message(self, text: &str) -> String {
        format!("cheat {}: {}", text.trim(), self.label())
    }
}

/// One parsed code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub format: Format,
    /// The address the adapter compares every read with.
    pub addr: u16,
    /// The byte the game sees instead of the one at `addr`.
    pub value: u8,
    /// The nine-digit Game Genie form's old byte: the patch applies only while the read still
    /// returns it, which is how one code picks one ROM bank. `None` for six-digit codes and for
    /// GameShark.
    pub compare: Option<u8>,
    /// The spelling the drawer and `config.toml` keep: upper case, dashes in the Genie groups.
    pub text: String,
}

impl Code {
    /// Parse one code. Dashes are optional and lower case is accepted; the stored `text` is the
    /// canonical spelling.
    pub fn parse(text: &str) -> Result<Code, ParseError> {
        let raw: String = text.trim().chars().filter(|c| *c != '-').collect();
        let mut digits = Vec::with_capacity(raw.len());
        for ch in raw.chars() {
            match ch.to_digit(16) {
                Some(d) => digits.push(d as u8),
                None => return Err(ParseError::NotHex),
            }
        }
        match digits.len() {
            6 => Code::genie(&digits, None),
            9 => {
                let compare = decode_compare(digits[6], digits[8]);
                Code::genie(&digits, Some(compare))
            }
            8 => Code::shark(&digits),
            0..=5 => Err(ParseError::TooShort),
            _ => Err(ParseError::BadLength),
        }
    }

    /// `ABC-DEF` / `ABC-DEF-GHI`: `AB` new byte, `FCDE` the address with its top nibble
    /// complemented, `GI` the encoded old byte.
    fn genie(digits: &[u8], compare: Option<u8>) -> Result<Code, ParseError> {
        // `F` is the ninth digit of the code but the fourth address character: the address is
        // stored as F C D E, and the leading nibble is complemented so a code cannot name a byte
        // past the ROM's `$7FFF`.
        let addr = ((digits[5] ^ 0xF) as u16) << 12
            | (digits[2] as u16) << 8
            | (digits[3] as u16) << 4
            | digits[4] as u16;
        if addr > 0x7FFF {
            return Err(ParseError::Address);
        }
        let value = digits[0] << 4 | digits[1];
        let text = format!(
            "{:X}{:X}{:X}-{:X}{:X}{:X}",
            digits[0], digits[1], digits[2], digits[3], digits[4], digits[5]
        );
        let text = match &compare {
            Some(_) => format!("{text}-{:X}{:X}{:X}", digits[6], digits[7], digits[8]),
            None => text,
        };
        Ok(Code {
            format: Format::Genie,
            addr,
            value,
            compare,
            text,
        })
    }

    /// `01vvaabb`: `vv` the new byte, `aabb` the address with its low byte first.
    fn shark(digits: &[u8]) -> Result<Code, ParseError> {
        if digits[0] != 0 || digits[1] != 1 {
            return Err(ParseError::UnsupportedPrefix);
        }
        let value = digits[2] << 4 | digits[3];
        let addr = (digits[6] as u16) << 12
            | (digits[7] as u16) << 8
            | (digits[4] as u16) << 4
            | digits[5] as u16;
        let text = format!(
            "{:X}{:X}{:X}{:X}{:X}{:X}{:X}{:X}",
            digits[0], digits[1], digits[2], digits[3], digits[4], digits[5], digits[6], digits[7]
        );
        Ok(Code {
            format: Format::Shark,
            addr,
            value,
            compare: None,
            text,
        })
    }

    /// Whether this code can mean anything on `cart`, and if not, the reason for the notice line.
    ///
    /// A Game Genie code is always in the ROM range the parse already checked. A GameShark code
    /// has to name RAM: ROM and VRAM cannot be written by the adapter, and cartridge RAM has to
    /// exist before a code can patch it.
    pub fn applies_to(&self, cart: &Cartridge) -> Result<(), String> {
        match self.format {
            Format::Genie => Ok(()),
            Format::Shark => match self.addr {
                0xA000..=0xBFFF if cart.ram.is_empty() => Err(format!(
                    "cheat {}: address ${:04X} is cartridge RAM and this cartridge has none",
                    self.text, self.addr
                )),
                0xA000..=0xDFFF => Ok(()),
                0x0000..=0x7FFF => Err(format!(
                    "cheat {}: address ${:04X} is ROM, which a GameShark write cannot change",
                    self.text, self.addr
                )),
                _ => Err(format!(
                    "cheat {}: address ${:04X} is not work or cartridge RAM",
                    self.text, self.addr
                )),
            },
        }
    }

    /// What the code does, for a test or a report: `Game Genie $14BC = $00 (while $03)`.
    pub fn summary(&self) -> String {
        match self.compare {
            Some(old) => format!(
                "{} ${:04X} = ${:02X} (while ${:02X})",
                self.format.name(),
                self.addr,
                self.value,
                old
            ),
            None => format!(
                "{} ${:04X} = ${:02X}",
                self.format.name(),
                self.addr,
                self.value
            ),
        }
    }
}

/// The nine-digit Game Genie form's old byte, from its `G` and `I` digits.
///
/// The codebook gives the encoding as "invert, rotate right by two, invert bits 0, 2 and 6":
/// `ror2(x XOR $FF) XOR $45`, which is the same as `ror2(x) XOR $BA`. Pan Docs words it as
/// `XOR $BA` then rotate left by two; that reading does not reproduce the codebook's own worked
/// examples (`004-BCE-E66` and `068-5FF-E66` both decode to an old byte of `$03`), so the codebook
/// form is the one implemented here.
fn decode_compare(g: u8, i: u8) -> u8 {
    let raw = g << 4 | i;
    raw.rotate_right(2) ^ 0xBA
}

/// One line of a ROM's `config.toml` cheat section: what the player typed, and whether it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheatEntry {
    pub text: String,
    pub on: bool,
}

/// The section key a ROM's codes live under: the cartridges' own title and CRC, which is the stem
/// of its `.sav` (`<title> [<crc32>].sav`), so a code follows the game and not the file name or
/// the session.
pub fn key_for(title: &str, crc32: u32) -> String {
    // A double quote would end the section header early, so it cannot survive in a key.
    let title = title.replace('"', "'");
    format!("{title} [{crc32:08x}]")
}

/// The enabled codes of one ROM, in the order `config.toml` lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheatBook {
    codes: Vec<Code>,
}

impl CheatBook {
    /// An empty book, which the bus skips over entirely.
    pub fn new() -> CheatBook {
        CheatBook::default()
    }

    /// Parse the enabled entries and keep the ones that apply to `cart`. The returned notices name
    /// every code that was refused and why; nothing is dropped silently.
    pub fn from_entries(entries: &[CheatEntry], cart: &Cartridge) -> (CheatBook, Vec<String>) {
        let mut codes = Vec::new();
        let mut notices = Vec::new();
        for entry in entries.iter().filter(|e| e.on) {
            match Code::parse(&entry.text) {
                Ok(code) => match code.applies_to(cart) {
                    Ok(()) => codes.push(code),
                    Err(why) => notices.push(why),
                },
                Err(why) => notices.push(why.message(&entry.text)),
            }
        }
        (CheatBook { codes }, notices)
    }

    /// The codes in force, for a test or a report.
    pub fn codes(&self) -> &[Code] {
        &self.codes
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    /// The byte a read of `addr` answers: the first enabled code whose address matches (and whose
    /// old byte still matches, for a nine-digit code), else the bus's own value. Cost is one pass
    /// over the enabled codes, and the bus only calls this when the book is not empty.
    pub fn read(&self, addr: u16, byte: u8) -> u8 {
        for code in &self.codes {
            if code.addr == addr && code.compare.is_none_or(|old| old == byte) {
                return code.value;
            }
        }
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Code {
        Code::parse(text).unwrap_or_else(|why| panic!("{text}: {}", why.label()))
    }

    #[test]
    fn a_six_digit_genie_code_names_a_rom_byte() {
        // The codebook's own example: `0A1-B9F` replaces the byte at $01B9 with $0A.
        let c = code("0A1-B9F");
        assert_eq!(c.format, Format::Genie);
        assert_eq!(c.addr, 0x01B9);
        assert_eq!(c.value, 0x0A);
        assert_eq!(c.compare, None);
        // The dashes are the player's, not the format's.
        assert_eq!(code("0a1b9f"), c);
        assert_eq!(c.text, "0A1-B9F");
    }

    #[test]
    fn a_nine_digit_genie_code_carries_the_old_byte() {
        // Codebook examples, both documented as replacing an old byte of $03.
        let c = code("004-BCE-E66");
        assert_eq!((c.addr, c.value, c.compare), (0x14BC, 0x00, Some(0x03)));
        let c = code("068-5FF-E66");
        assert_eq!((c.addr, c.value, c.compare), (0x085F, 0x06, Some(0x03)));
        // The address is `FCDE` with its top nibble complemented, so $3D49 comes out of `9C4`.
        assert_eq!(code("05D-49C-E62").addr, 0x3D49);
        assert_eq!(code("004BCEE66").text, "004-BCE-E66");
    }

    #[test]
    fn a_gameshark_code_writes_a_ram_byte() {
        // Pan Docs: `010238CD` writes $02 at $CD38 (the address's two bytes are swapped).
        let c = code("010238CD");
        assert_eq!(c.format, Format::Shark);
        assert_eq!((c.addr, c.value, c.compare), (0xCD38, 0x02, None));
        assert_eq!(c.text, "010238CD");
        assert_eq!(code("010163D3").addr, 0xD363);
    }

    #[test]
    fn refusals_are_named() {
        assert_eq!(Code::parse("A1B2"), Err(ParseError::TooShort));
        assert_eq!(Code::parse(""), Err(ParseError::TooShort));
        assert_eq!(Code::parse("ZZZ-000-000"), Err(ParseError::NotHex));
        assert_eq!(Code::parse("0A1-B9F-C4Z"), Err(ParseError::NotHex));
        assert_eq!(Code::parse("010238C"), Err(ParseError::BadLength));
        assert_eq!(Code::parse("010238CDEF"), Err(ParseError::BadLength));
        assert_eq!(Code::parse("020238CD"), Err(ParseError::UnsupportedPrefix));
        // A Game Genie address past $7FFF: the top address nibble is stored complemented (it is
        // the code's sixth digit), so a digit below 8 names a byte no ROM has.
        assert_eq!(Code::parse("0A1-B91-000"), Err(ParseError::Address));
    }

    #[test]
    fn a_refusal_names_the_code_it_refused() {
        let why = Code::parse("ZZZ-000-000");
        assert_eq!(
            why.unwrap_err().message("ZZZ-000-000"),
            "cheat ZZZ-000-000: not hex"
        );
        assert_eq!(ParseError::TooShort.label(), "too short");
    }

    #[test]
    fn the_book_asks_only_for_the_enabled_codes() {
        let mut book = CheatBook::new();
        assert!(book.is_empty());
        assert_eq!(book.read(0x01B9, 0x00), 0x00);
        book.codes.push(code("0A1-B9F"));
        assert_eq!(book.len(), 1);
        assert_eq!(book.read(0x01B9, 0x00), 0x0A);
        assert_eq!(book.read(0x01BA, 0x00), 0x00);
    }

    #[test]
    fn a_compare_mismatch_does_not_patch() {
        let mut book = CheatBook::new();
        book.codes.push(code("004-BCE-E66"));
        assert_eq!(book.read(0x14BC, 0x03), 0x00);
        assert_eq!(book.read(0x14BC, 0x04), 0x04);
    }

    #[test]
    fn a_key_follows_the_title_and_the_crc() {
        assert_eq!(key_for("Tetris", 0xD3B2A1C0), "Tetris [d3b2a1c0]");
        assert_ne!(key_for("Tetris", 1), key_for("Tetris", 2));
        assert_eq!(key_for("a\"b", 0), "a'b [00000000]");
    }
}
