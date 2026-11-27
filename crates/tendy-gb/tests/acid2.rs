//! `dmg-acid2` and `cgb-acid2`. The ROMs and their reference images are fetched into
//! `roms/test/` by `scripts/fetch-test-roms.sh` and never committed, so the tests are `#[ignore]`d
//! and `cargo test` stays green without them.
//!
//! The ROM judges itself by drawing a smiling face when its checks pass, and `img/reference-dmg.png`
//! is that picture as 160x144, 2 bits per pixel greyscale (0 = black, 3 = white). The comparison is
//! `scripts/compare-acid2.py`: it decodes both PNGs with the standard library, ranks each picture's
//! four shades (so any of the emulator's palettes compares equal) and diffs them pixel by pixel.
//! Run it with
//!
//! ```text
//! cargo run --release -- roms/test/dmg-acid2.gb --frames 120 --screenshot target/acid2.png
//! scripts/compare-acid2.py target/acid2.png roms/test/dmg-acid2-reference.png
//! ```
//!
//! The 120-frame run now matches the reference pixel for pixel, frame checksum `fnv1a64 f272a8ffe3db4c16`
//! (0 of 23 040 pixels differ). It first failed because the CPU stayed in `HALT` after servicing an
//! interrupt, so the ROM's registers lagged one interrupt behind and its rows missed their writes;
//! the fix landed in `src/cpu/` (test `interrupt_service_clears_halt` in `tests/interrupts.rs`).
//!
//! `cgb-acid2` is the colour picture: eight colours, no shades to rank, so the comparison is RGB24
//! exact (`scripts/compare-cgb-acid2.py`) and this file pins it two ways — the count of pixels in
//! each of the eight colours, and an `fnv1a64` over the frame's 15-bit colour words.

mod common;

use tendy_gb::state::{fnv1a64, fnv1a64_colours};

/// A test ROM: the folder `TENDY_TEST_ROMS` names when it is set, else the one the fetch script
/// writes.
fn test_rom(name: &str) -> std::path::PathBuf {
    match std::env::var_os("TENDY_TEST_ROMS") {
        Some(dir) => std::path::PathBuf::from(dir).join(name),
        None => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("roms/test")
            .join(name),
    }
}

/// The fnv1a64 of the 23 040-byte frame `dmg-acid2` draws when it passes, verified pixel for pixel
/// against `roms/test/dmg-acid2-reference.png`.
const ACID2_PASS_FNV: Option<u64> = Some(0xf272_a8ff_e3db_4c16);

/// The fnv1a64 of the frame `cgb-acid2` draws when it passes, folded over its 23 040 RGB555 colour
/// words (both bytes, little-endian) — the fold `Machine::frame_checksum` uses for a CGB frame.
///
/// The constant is **the reference image's own number**: `scripts/compare-cgb-acid2.py` decodes
/// `roms/test/cgb-acid2-reference.png`, recovers each pixel's 15-bit word with `v8 >> 3` (the exact
/// inverse of the `(v << 3) | (v >> 2)` the reference was made with) and folds those words the same
/// way. It prints `78ce869d9b004a6f` for the reference, and the same number for the emulator's frame
/// — which the script's pixel diff confirms is every pixel of it, 0 of 23 040 differing.
const CGB_ACID2_PASS_FNV: u64 = 0x78ce_869d_9b00_4a6f;

/// The eight colours the reference picture uses, as RGB555, with how many pixels each covers. Read
/// out of `roms/test/cgb-acid2-reference.png` by the same script: the picture is a CGB frame, so its
/// palette is a fact about the test and not about this emulator, and 23 040 pixels in a picture
/// whose colours are known pins the picture even before the arrangement is folded.
const CGB_ACID2_COLOURS: [(u16, usize); 8] = [
    (0x7FFF, 12_853), // white: the field
    (0x03FF, 6_060),  // yellow: the face
    (0x0000, 3_373),  // black: the head's outline, the nose, the mouth, the footer text
    (0x7EED, 332),    // light blue: the band behind "HELLO WORLD!"
    (0x7C00, 194),    // blue: the "HELLO WORLD!" letters and the "!"
    (0x0260, 152),    // green: the two eye pupils
    (0x01CE, 40),     // darker yellow: the eyes' inner ring
    (0x02B5, 36),     // dark yellow: the eyes' outer ring
];

#[test]
#[ignore = "needs roms/test/dmg-acid2.gb and roms/test/dmg-acid2-reference.png"]
fn dmg_acid2_matches_reference() {
    let path = test_rom("dmg-acid2.gb");
    let rom = match std::fs::read(&path) {
        Ok(rom) => rom,
        Err(err) => panic!(
            "cannot read {}: {err} — run scripts/fetch-test-roms.sh",
            path.display()
        ),
    };
    let mut machine = common::machine(rom);
    for _ in 0..120 {
        machine.run_frame();
    }
    let hash = fnv1a64(machine.frame());

    let expected = ACID2_PASS_FNV.unwrap_or_else(|| {
        panic!(
            "BLOCKED: dmg-acid2's picture does not match its reference image — 6 995 of 23 040 \
             pixels differ, in the rows the ROM writes from LY=LYC interrupts (see the module \
             comment). This machine's 120-frame checksum is {hash:016x}. Check a run against the \
             picture with `scripts/compare-acid2.py target/acid2.png \
             roms/test/dmg-acid2-reference.png`, and set ACID2_PASS_FNV once that reports every \
             pixel matching."
        )
    });
    assert_eq!(
        hash, expected,
        "dmg-acid2's picture differs from the reference"
    );
}

#[test]
#[ignore = "needs roms/test/cgb-acid2.gbc and roms/test/cgb-acid2-reference.png"]
fn cgb_acid2_matches_reference() {
    let path = test_rom("cgb-acid2.gbc");
    let rom = match std::fs::read(&path) {
        Ok(rom) => rom,
        Err(err) => panic!(
            "cannot read {}: {err} — run scripts/fetch-test-roms.sh",
            path.display()
        ),
    };
    let mut machine = common::machine(rom);
    assert!(machine.is_cgb(), "cgb-acid2's header must select CGB mode");
    for _ in 0..120 {
        machine.run_frame();
    }

    // Every pixel one of the reference's eight colours, in the reference's own quantities.
    let mut counts = [0usize; CGB_ACID2_COLOURS.len()];
    for &word in machine.colours() {
        match CGB_ACID2_COLOURS
            .iter()
            .position(|(colour, _)| *colour == word)
        {
            Some(index) => counts[index] += 1,
            None => panic!("the picture has {word:04x}, a colour the reference does not use"),
        }
    }
    for (index, (word, want)) in CGB_ACID2_COLOURS.iter().enumerate() {
        assert_eq!(
            counts[index], *want,
            "colour {word:04x}: {} pixels, the reference has {want}",
            counts[index]
        );
    }
    assert_eq!(counts.iter().sum::<usize>(), 160 * 144);

    let hash = fnv1a64_colours(machine.colours());
    assert_eq!(
        hash, CGB_ACID2_PASS_FNV,
        "cgb-acid2's picture differs from the reference — check it with \
         `scripts/compare-cgb-acid2.py target/cgb-acid2.png roms/test/cgb-acid2-reference.png`"
    );
}
