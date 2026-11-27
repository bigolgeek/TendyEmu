//! The compatibility palettes: the colour boot ROM's automatic palette choice for a
//! monochrome cartridge. Every branch of Pan Docs' four-step algorithm (*Power-Up Sequence*,
//! "Compatibility palettes") is exercised here — the licensee check, the title sum, the table
//! lookup, and the 14×-fourth-letter tie-breaker — and each test quotes the ID the branch produces
//! and the game group it lands on.

mod common;

use tendy_gb::Cartridge;
use tendy_gb::cgb::{CompatReason, compatibility_choice, compatibility_selector};

/// A header with a chosen 16-byte title (whose bytes sum to `sum`, with `fourth` in the fourth
/// position) and licensee codes, parsed from a synthetic ROM.
fn header(sum: u8, fourth: u8, old_licensee: u8, new_licensee: [u8; 2]) -> tendy_gb::cart::Header {
    let mut rom = common::synth_rom(&[]);
    let title = {
        let mut t = [0u8; 16];
        t[0] = sum.wrapping_sub(fourth);
        t[3] = fourth;
        t
    };
    rom[0x0134..0x0144].copy_from_slice(&title);
    rom[0x0144] = new_licensee[0];
    rom[0x0145] = new_licensee[1];
    rom[0x014B] = old_licensee;
    Cartridge::new(rom).expect("synthetic ROM").header
}

/// A Nintendo cartridge: old licensee `$01`, the common case (Pan Docs step 1).
fn nintendo(sum: u8, fourth: u8) -> tendy_gb::cart::Header {
    header(sum, fourth, 0x01, [0x00, 0x00])
}

/// A Nintendo cartridge through the old-licensee `$33` + new-licensee `"01"` path (Pan Docs step 1).
fn nintendo_new_code(sum: u8, fourth: u8) -> tendy_gb::cart::Header {
    header(sum, fourth, 0x33, *b"01")
}

/// Tetris (World)'s title checksum `$DB`: a single entry at index 5 → the white/yellow/red/black
/// Tetris palette (packed `$07`, entry `$07` flags `$00`).
#[test]
fn the_old_licensee_33_new_01_path_is_nintendo() {
    let (index, reason) = compatibility_choice(&nintendo_new_code(0xDB, 0));
    assert_eq!(reason, CompatReason::Single(0xDB));
    assert_eq!(index, 5, "Tetris' checksum $DB is the sixth single entry");
    assert_eq!(compatibility_selector(&nintendo_new_code(0xDB, 0)), 0x07);
    // The Tetris palette: white, yellow, red, black.
    assert_eq!(
        tendy_gb::palette::compat_triplet(0x07).bg,
        [0x7fff, 0x03ff, 0x001f, 0x0000]
    );
}

/// The plain `$01` old licensee is the other Nintendo path.
#[test]
fn the_plain_01_licensee_is_nintendo() {
    let (index, reason) = compatibility_choice(&nintendo(0xDB, 0));
    assert_eq!(reason, CompatReason::Single(0xDB));
    assert_eq!(index, 5);
    assert_eq!(compatibility_selector(&nintendo(0xDB, 0)), 0x07);
}

/// A non-Nintendo cartridge (old licensee `$00`) gets the default entry regardless of its title.
#[test]
fn a_non_nintendo_cartridge_gets_the_default_entry() {
    let (index, reason) = compatibility_choice(&header(0xDB, 0, 0x00, [0x00, 0x00]));
    assert_eq!(reason, CompatReason::NotNintendo);
    assert_eq!(index, 0);
    // The dummy entry, packed $7C — still coloured, not grey.
    assert_eq!(
        compatibility_selector(&header(0xDB, 0, 0x00, [0x00, 0x00])),
        0x7C
    );
}

/// A title whose sum is not in the table also falls back to the default entry.
#[test]
fn a_title_not_in_the_table_gets_the_default_entry() {
    // $80 is not one of the boot ROM's title checksums.
    let h = nintendo(0x80, 0);
    let (index, reason) = compatibility_choice(&h);
    assert_eq!(reason, CompatReason::TitleNotFound);
    assert_eq!(index, 0);
    assert_eq!(compatibility_selector(&h), 0x7C);
}

/// An ambiguous checksum's fourth letter picks a row: index + 14 × row. `$B3` is the first
/// ambiguous entry (index 65); its letters are B (row 0), U (row 1) and R (row 2).
#[test]
fn the_fourth_letter_tie_breaker_adds_14_times_the_row() {
    // Row 0: 'B' → Hoshi no Kirby / Kirby's Dream Land (packed $A8, entry $08 flags $05).
    let b = nintendo(0xB3, b'B');
    assert_eq!(
        compatibility_choice(&b),
        (65, CompatReason::Ambiguous(0xB3))
    );
    assert_eq!(compatibility_selector(&b), 0xA8);

    // Row 1: 'U' → Moguranya / Mole Mania (packed $60, entry $00 flags $03).
    let u = nintendo(0xB3, b'U');
    assert_eq!(
        compatibility_choice(&u),
        (79, CompatReason::Ambiguous(0xB3))
    );
    assert_eq!(compatibility_selector(&u), 0x60);

    // Row 2: 'R' → Tetris Attack / Yoshi no Panepon (packed $85, entry $05 flags $04).
    let r = nintendo(0xB3, b'R');
    assert_eq!(
        compatibility_choice(&r),
        (93, CompatReason::Ambiguous(0xB3))
    );
    assert_eq!(compatibility_selector(&r), 0x85);
}

/// An ambiguous checksum whose fourth letter is not in the tie-breaker table falls back.
#[test]
fn an_unknown_fourth_letter_gets_the_default_entry() {
    let h = nintendo(0xB3, b'X');
    assert_eq!(
        compatibility_choice(&h),
        (0, CompatReason::FourthLetterUnknown)
    );
    assert_eq!(compatibility_selector(&h), 0x7C);
}

/// The resolved triplet for a packed selector is one of the three documented palettes; check the
/// background of a couple of named entries end-to-end.
#[test]
fn a_selected_triplet_is_the_documented_colours() {
    // Super Mario Land (packed $6A): the boot ROM's distinct blue/green palette.
    let sml = tendy_gb::palette::compat_triplet(0x6A);
    assert_eq!(sml.bg, [0x7ed6, 0x4bff, 0x2175, 0x0000]);
    assert_eq!(sml.obj0, [0x0000, 0x7fff, 0x421f, 0x1cf2]);

    // The dummy entry (packed $7C) is reachable and coloured, not grey.
    let dummy = tendy_gb::palette::compat_triplet(0x7C);
    assert_eq!(dummy.bg, [0x7fff, 0x1bef, 0x6180, 0x0000]);
}
