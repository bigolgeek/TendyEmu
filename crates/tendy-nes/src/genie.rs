//! NES Game Genie code decoder.
//!
//! Ported from two working emulator implementations rather than from prose:
//!
//! * Mesen2 — `CheatManager::ConvertFromNesGameGenie`,
//!   <https://raw.githubusercontent.com/SourMesen/Mesen2/master/Core/Shared/CheatManager.cpp>.
//!   The path the issue named (`Core/NES/Cheats/GameGenieLoader.cs`) is no longer on
//!   `master`: it returns HTTP 404 and no file matching `genie` exists anywhere in the
//!   repository tree, so this is where Mesen2 actually decodes NES Game Genie codes today.
//! * FCEUX — `FCEUI_DecodeGG` and its `GGtobin` helper,
//!   <https://raw.githubusercontent.com/TASEmulators/fceux/master/src/cheat.cpp>.
//!
//! Both agree on the alphabet (`APZLGITYEOXUKSVN` → nibbles 0..15), on the bit shuffle
//! that rebuilds a 15-bit address (biased by `$8000`) and an 8-bit value from the six
//! nibbles, and on the seventh and eighth letters carrying the compare byte of an
//! 8-letter code. This module states the shuffle the way Mesen2 does — a bit-index table
//! per field, most significant bit first — and the FCEUX mask-and-shift form is the same
//! permutation; the two published vectors in the tests decode identically under both.
//!
//! Where the two differ, and what this module does instead:
//!
//! * FCEUX's `GGtobin` maps any character outside the alphabet to nibble 0, so `"?XIOPO"`
//!   silently decodes as `"AXIOPO"`; Mesen2 rejects it with a regex. We reject it, and
//!   report the offending character ([`GenieError::BadLetter`]).
//! * FCEUX leaves the check that letter 3's high bit (the 6/8 length bit) is clear for a
//!   6-letter code commented out; Mesen2 never reads that bit. Both therefore ignore it,
//!   and so do we — the length comes from the string, not from the code's own flag.
//! * Neither trims surrounding whitespace (both are handed an already-trimmed UI field);
//!   [`decode`] trims, and it is case-insensitive like both of them.
//! * Mesen2's validator is `^([…]{6})|([…]{8})$`, which only anchors the 6-letter
//!   alternative at the start, so `"AAAAAAxyz"` matches it. We check the length as a
//!   whole instead.

use std::fmt;

/// The Game Genie alphabet, in nibble order: letter `ALPHABET[n]` is nibble `n`.
const ALPHABET: &[u8; 16] = b"APZLGITYEOXUKSVN";

/// Address bits, most significant first, as indexes into the packed nibbles
/// (nibble `i` of the code occupies bits `4*i..4*i+4`). The result is the low 15 bits of
/// the CPU address; bit 15 is always set.
const ADDRESS_BITS: [u32; 15] = [14, 13, 12, 19, 22, 21, 20, 7, 10, 9, 8, 15, 18, 17, 16];

/// Value bits, most significant first, for a 6-letter code.
const VALUE_BITS: [u32; 8] = [3, 6, 5, 4, 23, 2, 1, 0];

/// Value bits for an 8-letter code: value bit 3 moves from letter 6 to letter 8, which is
/// where the compare byte's bit 3 also lives (`CD3` in the NESdev table).
const VALUE_BITS_8: [u32; 8] = [3, 6, 5, 4, 31, 2, 1, 0];

/// Compare-byte bits, most significant first. Only 8-letter codes carry these.
const COMPARE_BITS: [u32; 8] = [27, 30, 29, 28, 23, 26, 25, 24];

/// A decoded Game Genie code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    /// CPU address the code watches, `$8000..=$FFFF`.
    pub addr: u16,
    /// Byte substituted at `addr`.
    pub value: u8,
    /// For an 8-letter code, the byte that must currently be at `addr` for the
    /// substitution to happen; `None` for a 6-letter code, which always substitutes.
    pub compare: Option<u8>,
}

impl Code {
    /// The byte a read of `addr` returns: `value` when the address matches (and, for an
    /// 8-letter code, when the byte on the bus equals `compare`), otherwise the read
    /// byte, unchanged.
    pub fn apply(&self, addr: u16, value: u8) -> u8 {
        if addr != self.addr {
            return value;
        }
        match self.compare {
            Some(compare) if compare != value => value,
            _ => self.value,
        }
    }
}

/// The inverse of [`decode`]: the Game Genie letters for a code, 6 or 8 characters,
/// uppercase, from the same alphabet. Test support (the apply tests build their ROM
/// patches from a `Code` rather than guessing letters) and handy for printing a loaded
/// code back to the user. Like every Game Genie form one raw bit goes unpicked by the
/// tables — letter 3's 6/8-length flag — so `encode` leaves `raw` bit 11 clear, and
/// `decode(encode(c)) == c` always, even where `encode(decode(s)) == s` need not hold.
///
/// `code.addr` must be a code address as [`decode`] produces it (`$8000..=$FFFF`): only
/// the low 15 bits are carried, so a hand-built `Code` outside that range encodes to the
/// wrong address rather than failing.
#[must_use]
pub fn encode(code: Code) -> String {
    let compare_bits = match code.compare {
        None => {
            let mut raw = scatter(u32::from(code.addr) & 0x7FFF, &ADDRESS_BITS);
            raw |= scatter(u32::from(code.value), &VALUE_BITS);
            return letters(raw, 6);
        }
        Some(compare) => scatter(u32::from(compare), &COMPARE_BITS),
    };
    let mut raw = compare_bits;
    raw |= scatter(u32::from(code.addr) & 0x7FFF, &ADDRESS_BITS);
    raw |= scatter(u32::from(code.value), &VALUE_BITS_8);
    letters(raw, 8)
}

/// Lay the bits of `input` onto the bit positions `bits` names, most significant first.
/// The inverse of [`gather`]: the first `bits` entry receives input's top bit.
fn scatter(input: u32, bits: &[u32]) -> u32 {
    let mut out = 0u32;
    let mut rest = input;
    for &bit in bits.iter().rev() {
        out |= (rest & 1) << bit;
        rest >>= 1;
    }
    out
}

/// The characters of a Game Genie code: nibble `i` of `raw` is letter `i`.
fn letters(raw: u32, len: usize) -> String {
    (0..len)
        .map(|i| {
            let n = ((raw >> (4 * i)) & 0xF) as usize;
            ALPHABET[n] as char
        })
        .collect()
}

/// Why a string is not a Game Genie code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenieError {
    /// Not 6 or 8 letters (after trimming).
    BadLength,
    /// A character outside the Game Genie alphabet, as written by the caller.
    BadLetter(char),
}

impl fmt::Display for GenieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenieError::BadLength => write!(f, "a Game Genie code is 6 or 8 letters"),
            GenieError::BadLetter(c) => {
                write!(f, "{c:?} is not a Game Genie letter (APZLGITYEOXUKSVN)")
            }
        }
    }
}

impl std::error::Error for GenieError {}

/// Decode a 6- or 8-letter Game Genie code, case-insensitively, ignoring surrounding
/// whitespace.
pub fn decode(text: &str) -> Result<Code, GenieError> {
    let letters: Vec<char> = text.trim().chars().collect();
    if letters.len() != 6 && letters.len() != 8 {
        return Err(GenieError::BadLength);
    }

    // Pack nibble i (from letter i) into bits 4*i..4*i+4; the bit tables index this.
    let mut raw: u32 = 0;
    for (i, &letter) in letters.iter().enumerate() {
        raw |= u32::from(nibble(letter)?) << (4 * i);
    }

    let (value_bits, compare) = if letters.len() == 8 {
        (&VALUE_BITS_8, Some(gather(raw, &COMPARE_BITS) as u8))
    } else {
        (&VALUE_BITS, None)
    };

    Ok(Code {
        addr: 0x8000 | gather(raw, &ADDRESS_BITS),
        value: gather(raw, value_bits) as u8,
        compare,
    })
}

/// The nibble a letter stands for. `Err` carries the character as the caller wrote it.
fn nibble(letter: char) -> Result<u8, GenieError> {
    if !letter.is_ascii() {
        return Err(GenieError::BadLetter(letter));
    }
    let upper = letter.to_ascii_uppercase() as u8;
    ALPHABET
        .iter()
        .position(|&a| a == upper)
        .map(|index| index as u8)
        .ok_or(GenieError::BadLetter(letter))
}

/// Pull the bits a table names out of `raw`, most significant first, into an 8- or
/// 15-bit result.
fn gather(raw: u32, bits: &[u32]) -> u16 {
    let mut out = 0u16;
    for &bit in bits {
        out = (out << 1) | ((raw >> bit) & 1) as u16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_letter_code_matches_the_published_example() {
        // "SXIOPO" is the infinite-lives code for Super Mario Bros. Decodes to
        // $91D9:AD — three independent sources agree:
        //  * https://www.nesdev.org/nesgg.txt ("The value would be 10101101 or ADh in
        //    hex ... The address would be 001000111011001 or 11D9h ... just add 8000h"),
        //  * https://github.com/RigleGit/nes-specs/blob/main/cartridgesandmappers.md
        //    ("Example: Code \"SXIOPO\" changes [91D9h]=ADh"),
        //  * https://github.com/satoshinm/nes-game-genie (README: decodeGG('SXIOPO') ->
        //    { value: 173, address: 4569 }, i.e. $AD at $1123 + $8000).
        let code = decode("SXIOPO").unwrap();
        assert_eq!(
            code,
            Code {
                addr: 0x91D9,
                value: 0xAD,
                compare: None
            }
        );
    }

    #[test]
    fn eight_letter_code_matches_the_published_example() {
        // https://github.com/satoshinm/nes-game-genie README:
        //   decodeGG('SLXPLOVS') -> { value: 189, address: 4387, wantskey: true, key: 222 }
        // 189 = $BD, 4387 = $1123 (+$8000 = $9123), 222 = $DE.
        let code = decode("SLXPLOVS").unwrap();
        assert_eq!(
            code,
            Code {
                addr: 0x9123,
                value: 0xBD,
                compare: Some(0xDE)
            }
        );
    }

    #[test]
    fn decoding_is_case_insensitive_and_trims_whitespace() {
        let upper = decode("SXIOPO").unwrap();
        assert_eq!(decode("sxiopo").unwrap(), upper);
        assert_eq!(decode("  sXiOpO\n").unwrap(), upper);
        assert_eq!(decode("\tSLXPLOVS ").unwrap(), decode("slxplovs").unwrap());
    }

    #[test]
    fn bad_lengths_are_rejected() {
        for text in ["", "   ", "SXIOP", "SXIOPOO", "SXIOPOPSX", "SLXPLOV"] {
            assert_eq!(decode(text), Err(GenieError::BadLength), "{text:?}");
        }
    }

    #[test]
    fn bad_letters_report_the_offending_character() {
        // 'B' and '?' are not in APZLGITYEOXUKSVN; a lowercase letter that is in the
        // alphabet stays valid, one that is not is reported as written.
        assert_eq!(decode("SXIO?O"), Err(GenieError::BadLetter('?')));
        assert_eq!(decode("SXIOBO"), Err(GenieError::BadLetter('B')));
        assert_eq!(decode("SXIObO"), Err(GenieError::BadLetter('b')));
        assert_eq!(decode("SLXPLOVÉ"), Err(GenieError::BadLetter('É')));
    }

    #[test]
    fn errors_display_and_are_std_errors() {
        let boxed: Box<dyn std::error::Error> = Box::new(decode("SXIO?O").unwrap_err());
        assert_eq!(
            boxed.to_string(),
            "'?' is not a Game Genie letter (APZLGITYEOXUKSVN)"
        );
        assert_eq!(
            decode("").unwrap_err().to_string(),
            "a Game Genie code is 6 or 8 letters"
        );
    }

    /// `encode` is the inverse of `decode` on the published examples: the letters it writes
    /// back round-trip to the same `Code`. It does not have to reproduce the original
    /// spelling (the free letter-3 bit may differ); only the decoded code is pinned.
    #[test]
    fn encode_round_trips_through_decode() {
        for text in ["SXIOPO", "SLXPLOVS", "AAAAAE", "AAAAAEAE"] {
            let code = decode(text).unwrap();
            assert_eq!(decode(&encode(code)).unwrap(), code, "{text:?}");
        }
    }

    /// `encode` prints only Game Genie letters, in the right length, and accepts an
    /// arbitrary code — not just the published examples.
    #[test]
    fn encode_prints_alphabet_letters() {
        let six = encode(Code {
            addr: 0x8123,
            value: 0xBD,
            compare: None,
        });
        assert_eq!(six.len(), 6);
        assert!(six.chars().all(|c| "APZLGITYEOXUKSVN".contains(c)), "{six}");

        let eight = encode(Code {
            addr: 0x8123,
            value: 0xBD,
            compare: Some(0xDE),
        });
        assert_eq!(eight.len(), 8);
        assert!(
            eight.chars().all(|c| "APZLGITYEOXUKSVN".contains(c)),
            "{eight}"
        );
        assert_eq!(
            decode(&eight).unwrap(),
            Code {
                addr: 0x8123,
                value: 0xBD,
                compare: Some(0xDE)
            }
        );
    }

    #[test]
    fn apply_substitutes_only_at_the_code_address() {
        let code = decode("SXIOPO").unwrap();
        assert_eq!(code.apply(0x91D9, 0x00), 0xAD);
        // A 6-letter code has no compare byte, so it substitutes whatever is there.
        assert_eq!(code.apply(0x91D9, 0xFF), 0xAD);
        // One byte either side is untouched.
        assert_eq!(code.apply(0x91D8, 0x00), 0x00);
        assert_eq!(code.apply(0x91DA, 0x5A), 0x5A);
    }

    #[test]
    fn apply_honours_the_compare_byte() {
        let code = decode("SLXPLOVS").unwrap();
        assert_eq!(code.apply(0x9123, 0xDE), 0xBD);
        // Wrong byte at the right address: no substitution.
        assert_eq!(code.apply(0x9123, 0xDD), 0xDD);
        // Right byte at the wrong address: no substitution.
        assert_eq!(code.apply(0x9124, 0xDE), 0xDE);
    }

    #[test]
    fn value_bit_three_moves_between_letter_six_and_letter_eight() {
        // 'E' is nibble 8, so letter 6 with 'E' is exactly "high bit of letter 6 set".
        // In a 6-letter code that bit is the value's bit 3 (VALUE_BITS); in an 8-letter
        // code it is the compare byte's bit 3 and the value's bit 3 comes from letter 8
        // (VALUE_BITS_8 / COMPARE_BITS). Pinning both keeps the two tables honest.
        let six = decode("AAAAAE").unwrap();
        assert_eq!((six.addr, six.value, six.compare), (0x8000, 0x08, None));

        // Same bit, read as an 8-letter code: it stays at $8000, but now it is compare
        // bit 3 and the value comes out zero.
        let eight = decode("AAAAAEAA").unwrap();
        assert_eq!(
            (eight.addr, eight.value, eight.compare),
            (0x8000, 0x00, Some(0x08))
        );

        // And letter 8 supplies the value's bit 3 for the 8-letter form.
        let eight_value = decode("AAAAAEAE").unwrap();
        assert_eq!(
            (eight_value.addr, eight_value.value, eight_value.compare),
            (0x8000, 0x08, Some(0x08))
        );
    }
}
