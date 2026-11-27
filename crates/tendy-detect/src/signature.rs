//! Header signatures (SPEC §4 step 2, DESIGN §3.2).

use tendy_core::SystemKind;
use tendy_snes::cart::header::{CANDIDATES, score};

use crate::{Format, GbFlags};

/// A signature match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// The core.
    pub system: tendy_core::SystemKind,
    /// The layout (`FdsRaw` here even for a `.qd` file; `extension::resolve` turns it into `Qd`).
    pub format: crate::Format,
    /// Game Boy flags, for `Format::Gb`.
    pub gb: Option<crate::GbFlags>,
    /// SNES copier header present.
    pub copier_header: bool,
}

/// The lowest SNES header score (of 13) that counts as a signature (§3.2 row 8, §9 Q2).
pub const SNES_SIGNATURE_MIN_SCORE: i32 = 8;

/// The 48-byte Nintendo logo at GB `$0104-$0133` (gb `src/cart/mbc1.rs:95` `NINTENDO_LOGO`).
const GB_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Runs the §3.2 table in order and returns the first row that matches.
pub fn identify(bytes: &[u8]) -> Option<Match> {
    let len = bytes.len();

    // Row 1: iNES / NES 2.0. Bit 3 set and bit 2 clear is NES 2.0 (§3.6 C4).
    if len >= 16 && bytes.starts_with(b"NES\x1A") {
        let format = if bytes[7] & 0x0C == 0x08 {
            Format::Nes20
        } else {
            Format::Ines
        };
        return Some(matched(SystemKind::Nes, format));
    }
    // Row 2: UNIF, a 32-byte header.
    if len >= 32 && bytes.starts_with(b"UNIF") {
        return Some(matched(SystemKind::Nes, Format::Unif));
    }
    // Row 3: FDS with the 16-byte fwNES header.
    if len >= 16 && bytes.starts_with(b"FDS\x1A") {
        return Some(matched(SystemKind::Nes, Format::FdsHeadered));
    }
    // Row 4: an FDS/QD disk's block 1, with no header.
    if len >= 15 && bytes[0] == 0x01 && bytes[1..15] == *b"*NINTENDO-HVC*" {
        return Some(matched(SystemKind::Nes, Format::FdsRaw));
    }
    // Row 5: NSF, a 128-byte header.
    if len >= 0x80 && bytes.starts_with(b"NESM\x1A") {
        return Some(matched(SystemKind::Nes, Format::Nsf));
    }
    // Row 6: GBA.
    if is_gba(bytes) {
        return Some(matched(SystemKind::Gba, Format::Gba));
    }
    // Row 7: GB / CGB / SGB, by the Nintendo logo.
    if len >= 0x150 && bytes[0x104..0x134] == GB_LOGO {
        return Some(Match {
            system: SystemKind::Gb,
            format: Format::Gb,
            gb: Some(GbFlags {
                cgb: bytes[0x143] & 0x80 != 0,
                sgb: bytes[0x146] == 0x03 && bytes[0x14B] == 0x33,
            }),
            copier_header: false,
        });
    }
    // Row 8: SNES, a score rather than a magic number.
    snes_match(bytes)
}

/// A match with no Game Boy flags and no copier header.
fn matched(system: SystemKind, format: Format) -> Match {
    Match {
        system,
        format,
        gb: None,
        copier_header: false,
    }
}

/// §3.2 row 6: an ARM branch at 0, `$96` at `$B2`, the complement at `$BD`.
///
/// The logo is not part of the check: gb's loader records it and refuses on the other three, and
/// detection matches what the loader accepts.
fn is_gba(bytes: &[u8]) -> bool {
    if bytes.len() < 0xC0 {
        return false;
    }
    let entry = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if (entry >> 25) & 0b111 != 0b101 || bytes[0xB2] != 0x96 {
        return false;
    }
    let mut check: u8 = 0;
    for &byte in &bytes[0xA0..=0xBC] {
        check = check.wrapping_sub(byte);
    }
    bytes[0xBD] == check.wrapping_sub(0x19)
}

/// §3.2 row 8: the best candidate score, with a copier header stripped first.
fn snes_match(bytes: &[u8]) -> Option<Match> {
    // `len % 1024 == 512` means a 512-byte copier header is present (§3.2, snes `src/cart/mod.rs:45`).
    let copier_header = bytes.len() % 1024 == 512;
    let rom = if copier_header { &bytes[512..] } else { bytes };
    let mut best = i32::MIN;
    // A later candidate replaces the best only on a strictly greater score: a tie keeps the
    // earlier one, as the core's own header parse does (`header.rs:164`).
    for (at, board) in CANDIDATES {
        best = best.max(score(rom, at, board));
    }
    if best < SNES_SIGNATURE_MIN_SCORE {
        return None;
    }
    Some(Match {
        system: SystemKind::Snes,
        format: Format::Snes,
        gb: None,
        copier_header,
    })
}
