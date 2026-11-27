//! The Game Boy Advance's picture, tested where the public ROMs cannot see.
//!
//! Three tiny public ROMs in `roms/test/gba/ppu/` are the gate, and their assembly sources sit
//! beside them: `shades.gba` and `stripes.gba` are mode 0, `hello.gba` is mode 4, and each expected
//! picture below is derived from the source, not from the emulator. The rest of the file covers what
//! no such ROM reaches — 8bpp and affine objects, double-size boxes, both windows, both brightness
//! directions, alpha blending against the backdrop, mosaic and priority.
//!
//! The ROMs are gitignored like every other public test ROM here, so the three gates are
//! `#[ignore]`d and run with `cargo test --release --test gba_video -- --ignored`.

use tendy_gb::System;
use tendy_gba::Gba;
use tendy_gba::bus::{IO_BASE, OAM_BASE, PALETTE_BASE, VRAM_BASE};
use tendy_gba::cart::Cartridge;
use tendy_gba::ppu::{HEIGHT, PIXELS, WIDTH};

// ---------------------------------------------------------------------------------------------
// The machine under test
// ---------------------------------------------------------------------------------------------

/// A cartridge whose code starts at `$080000C0` and does nothing: the entry point is the usual `b`.
fn synth_cart() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"GBA-VID\0");
    rom[0xAC..0xB0].copy_from_slice(b"TVID");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine with an empty cartridge, ready for a test to write its own registers and VRAM.
fn machine() -> Gba {
    Gba::new(Cartridge::new(synth_cart()).expect("a synthesized cartridge"))
}

/// Write a halfword to an I/O register.
fn io(gba: &mut Gba, offset: u32, value: u16) {
    gba.bus.write16(IO_BASE + offset, value);
}

/// Write one byte of VRAM. The video memory takes 16-bit writes only (GBATEK's *Address Bus Width*
/// table: "not VRAM" for 8-bit writes), so a test that means "this byte" reads the halfword, changes
/// its lane and writes the halfword back — exactly what a cartridge's own code has to do.
fn vram(gba: &mut Gba, offset: u32, value: u8) {
    let aligned = offset & !1;
    let word = gba.bus.read16(VRAM_BASE + aligned);
    let word = if offset & 1 == 0 {
        (word & 0xFF00) | u16::from(value)
    } else {
        (word & 0x00FF) | (u16::from(value) << 8)
    };
    gba.bus.write16(VRAM_BASE + aligned, word);
}

/// Write one background palette word.
fn bg_palette(gba: &mut Gba, index: u16, value: u16) {
    gba.bus.write16(PALETTE_BASE + u32::from(index) * 2, value);
}

/// Write a 32-bit value to an I/O register.
fn io32(gba: &mut Gba, offset: u32, value: u32) {
    gba.bus.write32(IO_BASE + offset, value);
}

/// Write a 28-bit reference point (`BGxX`/`BGxY`) as 8.8 fixed point.
fn io_point(gba: &mut Gba, offset: u32, value: i32) {
    io32(gba, offset, value as u32);
}

/// Write one OAM halfword.
fn oam(gba: &mut Gba, offset: u32, value: u16) {
    gba.bus.write16(OAM_BASE + offset, value);
}

/// Write one object's three attributes into OAM entry `index`.
fn object(gba: &mut Gba, index: u32, attr0: u16, attr1: u16, attr2: u16) {
    let at = index * 8;
    oam(gba, at, attr0);
    oam(gba, at + 2, attr1);
    oam(gba, at + 4, attr2);
}

/// Write one object palette word (the 256 object colours at `05000200h`).
fn obj_palette(gba: &mut Gba, index: u16, value: u16) {
    gba.bus
        .write16(PALETTE_BASE + 0x200 + u32::from(index) * 2, value);
}

/// Fill a 4bpp tile so that pixel `(x, y)` is palette index `first + x + y`.
fn gradient_tile(gba: &mut Gba, offset: u32, first: u8) {
    for row in 0..8u32 {
        for half in 0..4u32 {
            let lo = first + (half * 2) as u8 + row as u8;
            vram(gba, offset + row * 4 + half, lo | ((lo + 1) << 4));
        }
    }
}

/// Park all 128 objects. An all-zero OAM entry is a real object — 8x8 at (0,0), tile 0, palette 0 —
/// on this hardware, so a test that wants a clean canvas has to say so.
fn no_objects(gba: &mut Gba) {
    for index in 0..128u32 {
        oam(gba, index * 8, 0x0200); // attr0: "OBJ Disable"
    }
}

/// Run a frame after the registers are in place and return its picture.
///
/// Two frames, not one: a register written between frames lands after the current frame's line 0
/// has been drawn (the clock is already *at* line 0 when `run_frame` returns), so the first frame
/// carries the new value on all but its top line. The second frame carries it everywhere, which is
/// what a test that photographs the picture means to see.
fn shot(gba: &mut Gba) -> Vec<u16> {
    gba.run_frame();
    gba.run_frame();
    gba.frame().colours.to_vec()
}

/// One pixel of a picture.
fn at(picture: &[u16], x: usize, y: usize) -> u16 {
    picture[y * WIDTH + x]
}

/// How many pixels of `actual` equal `expected`, and how many do not.
fn diff(actual: &[u16], expected: &[u16]) -> (usize, usize) {
    assert_eq!(actual.len(), expected.len());
    let same = actual.iter().zip(expected).filter(|(a, e)| a == e).count();
    (same, actual.len() - same)
}

/// Fill a 4bpp tile with one palette index: 32 bytes of two pixels each.
fn solid_tile(gba: &mut Gba, char_base: u32, tile: u32, index: u8) {
    let byte = index | (index << 4);
    for i in 0..32 {
        vram(gba, char_base + tile * 32 + i, byte);
    }
}

/// Fill a text map's row 0 with tile numbers `tiles`, one entry a tile.
fn map_row(gba: &mut Gba, map_base: u32, tiles: &[u16]) {
    for (x, &tile) in tiles.iter().enumerate() {
        map_entry(gba, map_base, x, 0, tile);
    }
}

/// Write one 16-bit map entry at tile column `x`, row `y` — the flag bits (flip, palette) included.
fn map_entry(gba: &mut Gba, map_base: u32, x: usize, y: usize, entry: u16) {
    let at = map_base + ((y * 32 + x) as u32) * 2;
    vram(gba, at, entry as u8);
    vram(gba, at + 1, (entry >> 8) as u8);
}

// ---------------------------------------------------------------------------------------------
// Mode 0: text backgrounds
// ---------------------------------------------------------------------------------------------

/// A mode-0 machine with BG0 4bpp at char base `$4000` and map `$800`, its palette filled with
/// 16 distinguishable colours: entry `n` is `n << 10` (green), so a test can read the index back.
fn mode0() -> Gba {
    let mut gba = machine();
    io(&mut gba, 0x000, 1 << 8); // DISPCNT: mode 0, BG0 on
    io(&mut gba, 0x008, (1 << 2) | (1 << 8)); // BG0CNT: char base 1 ($4000), map 1 ($800)
    for n in 1..16u16 {
        bg_palette(&mut gba, n, n << 10);
    }
    gba
}

#[test]
fn a_text_background_draws_its_tiles_in_order() {
    let mut gba = mode0();
    solid_tile(&mut gba, 0x4000, 0, 1);
    solid_tile(&mut gba, 0x4000, 1, 2);
    // Tile 2 is never given any data, so every one of its pixels is index 0 — transparent.
    map_row(&mut gba, 0x800, &[0, 1, 2]);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 1 << 10, "tile 0, palette 1");
    assert_eq!(at(&picture, 7, 0), 1 << 10, "the whole 8-pixel tile");
    assert_eq!(at(&picture, 8, 0), 2 << 10, "tile 1, palette 2");
    assert_eq!(at(&picture, 15, 0), 2 << 10);
    assert_eq!(
        at(&picture, 16, 0),
        0,
        "a transparent tile shows the backdrop"
    );
    assert_eq!(
        at(&picture, 0, 8),
        1 << 10,
        "the map's next row is empty, which means tile 0 again"
    );
}

#[test]
fn a_tile_can_be_flipped_both_ways_by_its_map_entry() {
    let mut gba = mode0();
    // Tile 0: a single pixel of index 2 at its top-left corner. The rest is index 0, transparent.
    vram(&mut gba, 0x4000, 0x02);
    map_row(&mut gba, 0x800, &[0x0400]); // horizontal flip
    let flipped = shot(&mut gba);
    assert_eq!(at(&flipped, 7, 0), 2 << 10, "the corner moved to the right");
    assert_eq!(at(&flipped, 0, 0), 0);
    map_row(&mut gba, 0x800, &[0x0800]); // vertical flip
    let flipped = shot(&mut gba);
    assert_eq!(at(&flipped, 0, 7), 2 << 10, "and down to the bottom");
    assert_eq!(at(&flipped, 0, 0), 0);
}

#[test]
fn a_text_background_scrolls_by_its_offsets() {
    let mut gba = mode0();
    solid_tile(&mut gba, 0x4000, 0, 1);
    solid_tile(&mut gba, 0x4000, 1, 2);
    map_row(&mut gba, 0x800, &[0, 1, 2]); // tile 2 is transparent
    io(&mut gba, 0x010, 8); // BG0HOFS: one tile to the left
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 2 << 10, "tile 1 now starts the line");
    assert_eq!(at(&picture, 8, 0), 0, "the transparent tile 2 follows it");
    assert_eq!(
        at(&picture, 16, 0),
        1 << 10,
        "and the map wraps to tile 0 again"
    );
    // Vertical: scrolled four lines into the tile, and the map's next row is transparent.
    io(&mut gba, 0x010, 0);
    io(&mut gba, 0x012, 4);
    map_entry(&mut gba, 0x800, 0, 1, 2);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        1 << 10,
        "row 4 of the tile is still tile 0"
    );
    assert_eq!(at(&picture, 0, 3), 1 << 10);
    assert_eq!(
        at(&picture, 0, 4),
        0,
        "past its 8 pixels: the map's second row, made transparent"
    );
}

#[test]
fn an_8bpp_text_background_reads_one_byte_a_pixel_and_has_256_entries() {
    let mut gba = machine();
    io(&mut gba, 0x000, 1 << 8);
    io(&mut gba, 0x008, (1 << 2) | (1 << 7) | (1 << 8)); // 256 colours/1 palette
    bg_palette(&mut gba, 0x80, 0x7C00);
    bg_palette(&mut gba, 0xFF, 0x03E0);
    // Tile 0 is 64 bytes; its first row's pixel 1 is entry $80 and pixel 2 is $FF. A tile is eight
    // pixels wide, so these are screen pixels 1 and 2 — and the palette bank bits in the map entry
    // must be ignored in 256-colour mode.
    vram(&mut gba, 0x4000 + 1, 0x80);
    vram(&mut gba, 0x4000 + 2, 0xFF);
    map_row(&mut gba, 0x800, &[0xF000]); // palette bank 15, which 8bpp ignores
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0, "entry 0 is transparent");
    assert_eq!(at(&picture, 1, 0), 0x7C00, "index $80, no palette bank");
    assert_eq!(at(&picture, 2, 0), 0x03E0, "index $FF");
}

#[test]
fn lower_priority_number_wins_and_bg0_wins_a_tie() {
    let mut gba = machine();
    io(&mut gba, 0x000, (1 << 8) | (1 << 9)); // BG0 and BG1 on
    io(&mut gba, 0x008, (1 << 2) | (1 << 8)); // BG0: priority 0
    io(&mut gba, 0x00A, 1 | (2 << 2) | (1 << 8)); // BG1: priority 1, map $1000
    bg_palette(&mut gba, 1, 0x001F);
    bg_palette(&mut gba, 2, 0x03E0);
    solid_tile(&mut gba, 0x4000, 0, 1);
    solid_tile(&mut gba, 0x8000, 0, 2);
    map_row(&mut gba, 0x800, &[0]);
    map_row(&mut gba, 0x1000, &[0]);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F, "BG0 at priority 0 is on top");
    // Equal priorities: BG0 keeps the pixel even though BG1 is drawn later in the register file.
    io(&mut gba, 0x00A, (2 << 2) | (1 << 8)); // BG1: priority 0
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F, "the tie goes to BG0");
    // And a lower-priority number wins over a higher one.
    io(&mut gba, 0x008, 1 | (1 << 2) | (1 << 8)); // BG0: priority 1
    io(&mut gba, 0x00A, (2 << 2) | (1 << 8)); // BG1: priority 0
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x03E0, "BG1 at priority 0");
}

#[test]
fn a_background_that_draws_nothing_leaves_the_backdrop() {
    let mut gba = mode0();
    bg_palette(&mut gba, 0, 0x7FFF);
    let picture = shot(&mut gba);
    assert!(
        picture.iter().all(|&word| word == 0x7FFF),
        "an empty map shows palette entry 0 everywhere"
    );
}

#[test]
fn forced_blank_draws_white_over_everything() {
    let mut gba = mode0();
    bg_palette(&mut gba, 0, 0x001F);
    solid_tile(&mut gba, 0x4000, 0, 1);
    map_row(&mut gba, 0x800, &[0]);
    io(&mut gba, 0x000, (1 << 8) | (1 << 7)); // forced blank on top of mode 0 + BG0
    let picture = shot(&mut gba);
    assert!(
        picture.iter().all(|&word| word == 0x7FFF),
        "forced blank is white, not the backdrop"
    );
}

#[test]
fn a_text_background_with_mosaic_shows_the_block_corner() {
    let mut gba = mode0();
    // Tile 0: pixel x of each row is palette index (x / 2 + 1), so a 4-wide mosaic block is
    // readable in the first four pixels of a row.
    for row in 0..8u32 {
        for x in 0..8u32 {
            let index = (x / 2 + 1) as u8;
            let at = 0x4000 + row * 4 + x / 2;
            let existing = gba.bus.read8(VRAM_BASE + at);
            let byte = if x % 2 == 0 {
                (existing & 0xF0) | index
            } else {
                (existing & 0x0F) | (index << 4)
            };
            vram(&mut gba, at, byte);
        }
    }
    map_row(&mut gba, 0x800, &[0]);
    io(&mut gba, 0x008, (1 << 2) | (1 << 6) | (1 << 8)); // BG0 with mosaic
    io(&mut gba, 0x04C, 3 | (3 << 4)); // MOSAIC: BG 4x4 blocks
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 1 << 10, "the block's top-left pixel");
    assert_eq!(at(&picture, 3, 0), 1 << 10, "out to the block's edge");
    assert_eq!(
        at(&picture, 4, 0),
        3 << 10,
        "the next block starts at its own corner"
    );
    assert_eq!(at(&picture, 0, 3), 1 << 10, "and four lines down");
    assert_eq!(at(&picture, 4, 4), 3 << 10);
}

// ---------------------------------------------------------------------------------------------
// Modes 3-5: the bitmap backgrounds
// ---------------------------------------------------------------------------------------------

/// A mode-3 machine: 15-bit colour, BG2 on, palette entry 0 white so the backdrop is visible.
fn mode3() -> Gba {
    let mut gba = machine();
    io(&mut gba, 0x000, 3 | (1 << 10));
    bg_palette(&mut gba, 0, 0x7FFF);
    gba
}

#[test]
fn mode_3_shows_its_bitmap_one_word_a_pixel() {
    let mut gba = mode3();
    // Pixel (5, 3) is the fifth byte pair of the fourth row: 480 bytes a line.
    vram(&mut gba, 3 * 480 + 5 * 2, 0x34);
    vram(&mut gba, 3 * 480 + 5 * 2 + 1, 0x12);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 5, 3), 0x1234, "the word is the colour");
    assert_eq!(at(&picture, 5, 2), 0, "the line above is untouched");
    assert_eq!(
        at(&picture, 0, 0),
        0,
        "and colour 0 is opaque black, not transparent"
    );
    // The identity matrix is the post-boot state; with it the bitmap is 1:1. A PA of zero would
    // smear every line from one texel, which is what a missing post-boot default would look like.
    assert_eq!(at(&picture, 6, 3), 0);
}

#[test]
fn mode_4_is_paletted_transparent_at_zero_and_flips_its_page() {
    let mut gba = machine();
    io(&mut gba, 0x000, 4 | (1 << 10));
    bg_palette(&mut gba, 0, 0x7FFF);
    bg_palette(&mut gba, 1, 0x001F);
    bg_palette(&mut gba, 2, 0x03E0);
    // Page 0 has a red pixel at (20, 10); page 1 a green one at the same place.
    vram(&mut gba, 10 * 240 + 20, 1);
    vram(&mut gba, 0xA000 + 10 * 240 + 20, 2);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 20, 10), 0x001F, "page 0");
    assert_eq!(at(&picture, 21, 10), 0x7FFF, "index 0 is the backdrop");
    io(&mut gba, 0x000, 4 | (1 << 10) | (1 << 4));
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 20, 10), 0x03E0, "page 1 after the flip");
}

#[test]
fn mode_5_is_one_hundred_and_sixty_by_one_hundred_and_twenty_eight() {
    let mut gba = machine();
    io(&mut gba, 0x000, 5 | (1 << 10));
    bg_palette(&mut gba, 0, 0x7FFF);
    vram(&mut gba, 127 * 320 + 159 * 2, 0x1F);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 159, 127),
        0x001F,
        "the last pixel of the bitmap"
    );
    assert_eq!(at(&picture, 160, 0), 0x7FFF, "right of it: the backdrop");
    assert_eq!(at(&picture, 0, 128), 0x7FFF, "below it: the backdrop");
}

// ---------------------------------------------------------------------------------------------
// Modes 1 and 2: the rotation/scaling backgrounds
// ---------------------------------------------------------------------------------------------

/// A mode-2 machine with BG2 affine at char base 1 (`$4000`), map base 0, 128x128, priority 0, no
/// wrap. Tile 0 is index 1 everywhere and tile 1 is index 2; the map has tile 1 at (1, 2) and tile 0
/// elsewhere, so a test can see where a source position lands.
fn mode2() -> Gba {
    let mut gba = machine();
    io(&mut gba, 0x000, 2 | (1 << 10));
    io(&mut gba, 0x00C, 1 << 2); // BG2CNT: 128x128, char base 1, map base 0
    for n in 1..16u16 {
        bg_palette(&mut gba, n, n << 10);
    }
    for i in 0..64 {
        vram(&mut gba, 0x4000 + i, 1);
        vram(&mut gba, 0x4040 + i, 2);
    }
    for y in 0..16u32 {
        for x in 0..16u32 {
            vram(&mut gba, y * 16 + x, if (x, y) == (1, 2) { 1 } else { 0 });
        }
    }
    gba
}

/// The identity matrix: `PA = PD = 1.0`, `PB = PC = 0`.
fn identity(gba: &mut Gba) {
    io(gba, 0x020, 0x0100);
    io(gba, 0x022, 0);
    io(gba, 0x024, 0);
    io(gba, 0x026, 0x0100);
    io_point(gba, 0x028, 0);
    io_point(gba, 0x02C, 0);
}

#[test]
fn an_affine_background_with_the_identity_matrix_sits_where_its_reference_says() {
    let mut gba = mode2();
    identity(&mut gba);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        1 << 10,
        "tile 0 everywhere the map is zero"
    );
    // The map's marker is tile 1 at map (1, 2): pixels x 8..15, y 16..23.
    assert_eq!(at(&picture, 8, 16), 2 << 10, "the marker's top-left");
    assert_eq!(at(&picture, 15, 23), 2 << 10);
    assert_eq!(at(&picture, 7, 16), 1 << 10, "and nothing outside it");
    // Moving the reference scrolls the background: -8 pixels in x and +8 in y.
    io_point(&mut gba, 0x028, -8 << 8);
    io_point(&mut gba, 0x02C, 8 << 8);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 16, 8),
        2 << 10,
        "the marker has moved right 8 and up 8"
    );
    assert_eq!(at(&picture, 8, 16), 1 << 10);
}

#[test]
fn an_affine_background_rotates_by_ninety_degrees() {
    let mut gba = mode2();
    // dest(x, y) samples source(y, 127 - x): PA = 0, PB = 1, PC = -1, PD = 0, origin (0, 127).
    io(&mut gba, 0x020, 0x0000);
    io(&mut gba, 0x022, 0x0100);
    io(&mut gba, 0x024, 0xFF00);
    io(&mut gba, 0x026, 0x0000);
    io_point(&mut gba, 0x028, 0);
    io_point(&mut gba, 0x02C, 127 << 8);
    let picture = shot(&mut gba);
    // The marker tile covers source (8..15, 16..23); rotated that is dest x = 127-16..127-23 =
    // 104..111, dest y = 8..15.
    assert_eq!(at(&picture, 104, 8), 2 << 10, "the marker's rotated corner");
    assert_eq!(at(&picture, 111, 15), 2 << 10);
    assert_eq!(at(&picture, 103, 8), 1 << 10, "just outside it");
    assert_eq!(at(&picture, 104, 16), 1 << 10);
}

#[test]
fn an_affine_background_outside_its_map_is_transparent_or_wraps() {
    let mut gba = mode2();
    identity(&mut gba);
    bg_palette(&mut gba, 0, 0x7FFF);
    // The reference starts 120 lines down a 128-line map, so lines 0-7 are inside it and lines 8
    // on are past its bottom edge.
    io_point(&mut gba, 0x02C, 120 << 8);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        1 << 10,
        "source line 120 is inside the map"
    );
    assert_eq!(at(&picture, 0, 7), 1 << 10, "source line 127 is the last");
    assert_eq!(at(&picture, 0, 8), 0x7FFF, "source line 128 is transparent");
    assert_eq!(at(&picture, 0, 159), 0x7FFF, "all the way to the bottom");
    io(&mut gba, 0x00C, (1 << 2) | (1 << 13)); // BG2CNT: display area overflow
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 8),
        1 << 10,
        "wrapped: screen line 8 samples source line 128, which is line 0 again"
    );
    assert_eq!(
        at(&picture, 0, 159),
        1 << 10,
        "and every line below it wraps too"
    );
}

// ---------------------------------------------------------------------------------------------
// The clock, not the frame
// ---------------------------------------------------------------------------------------------

#[test]
fn a_scroll_written_mid_frame_lands_on_the_next_line() {
    let mut gba = mode0();
    solid_tile(&mut gba, 0x4000, 0, 1);
    solid_tile(&mut gba, 0x4000, 1, 2);
    map_row(&mut gba, 0x800, &[0, 1]);
    // Run into line 5, then scroll by one tile. Line 5 is already being drawn, so GBATEK's timing
    // makes the write visible from line 6 — that is the wavy-background effect, and it is why the
    // renderer is driven by the scanline rather than by the frame.
    while gba.bus.lcd.line < 5 {
        gba.step_instruction();
    }
    io(&mut gba, 0x010, 8);
    while gba.bus.lcd.frames == 0 {
        gba.step_instruction();
    }
    let picture = gba.frame().colours.to_vec();
    assert_eq!(at(&picture, 0, 4), 1 << 10, "line 4 is before the write");
    assert_eq!(
        at(&picture, 0, 5),
        1 << 10,
        "and so is line 5, already drawn"
    );
    assert_eq!(at(&picture, 0, 6), 2 << 10, "line 6 is the first to see it");
    assert_eq!(
        at(&picture, 0, 7),
        2 << 10,
        "and it holds to the tile's last line"
    );
}

// ---------------------------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------------------------

/// A mode-0 machine with no background and objects enabled: a canvas for object tests.
fn object_machine() -> Gba {
    let mut gba = machine();
    io(&mut gba, 0x000, 1 << 12);
    no_objects(&mut gba);
    gba
}

#[test]
fn an_object_draws_its_tile_at_its_place() {
    let mut gba = object_machine();
    obj_palette(&mut gba, 0x25, 0x001F);
    // Object tile 0 (4bpp) is index 5 everywhere; the object's palette bank is 2.
    for i in 0..32u32 {
        vram(&mut gba, 0x10000 + i, 0x55);
    }
    // attr0: Y = 20; attr1: X = 10, size 0 (8x8); attr2: tile 0, priority 0, palette bank 2.
    object(&mut gba, 0, 20, 10, 2 << 12);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 20), 0x001F, "the object's top-left");
    assert_eq!(at(&picture, 17, 27), 0x001F, "its bottom-right");
    assert_eq!(at(&picture, 9, 20), 0, "one pixel left is the backdrop");
    assert_eq!(at(&picture, 18, 27), 0, "and one past its right edge");
    assert_eq!(at(&picture, 10, 28), 0, "and one below its bottom");
}

#[test]
fn an_8bpp_object_reads_two_character_numbers_a_tile_in_the_2d_mapping() {
    let mut gba = object_machine();
    obj_palette(&mut gba, 1, 0x001F);
    obj_palette(&mut gba, 2, 0x03E0);
    // A 16x16 256-colour object at tile 4: its left tile is character 4 and the tile to its right
    // is character 6 — 64 bytes a tile, but a character number is 32 bytes.
    vram(&mut gba, 0x10000 + 4 * 32, 1);
    vram(&mut gba, 0x10000 + (4 + 2) * 32, 2);
    object(&mut gba, 0, 0x2000, 0x4000, 4);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F, "character 4");
    assert_eq!(at(&picture, 8, 0), 0x03E0, "character 6, one tile right");
    assert_eq!(at(&picture, 15, 15), 0, "and index 0 is transparent");
}

#[test]
fn a_one_dimensional_object_lays_its_tiles_consecutively() {
    let mut gba = object_machine();
    io(&mut gba, 0x000, (1 << 12) | (1 << 6)); // objects, 1D mapping
    obj_palette(&mut gba, 1, 0x001F);
    obj_palette(&mut gba, 2, 0x03E0);
    vram(&mut gba, 0x10000 + 4 * 32, 1);
    vram(&mut gba, 0x10000 + 4 * 32 + 64, 2); // the next 8x8 256-colour tile follows it
    object(&mut gba, 0, 0x2000, 0x4000, 4);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F);
    assert_eq!(at(&picture, 8, 0), 0x03E0, "the next tile is 64 bytes on");
}

#[test]
fn an_object_beats_the_background_it_ties_and_the_lowest_oam_index_wins() {
    let mut gba = mode0();
    bg_palette(&mut gba, 1, 0x001F); // BG0 red
    solid_tile(&mut gba, 0x4000, 0, 1);
    io(&mut gba, 0x000, (1 << 8) | (1 << 12)); // BG0 and objects
    no_objects(&mut gba);
    obj_palette(&mut gba, 5, 0x03E0); // object green
    obj_palette(&mut gba, 6, 0x7C00); // second object: blue
    for i in 0..32u32 {
        vram(&mut gba, 0x10000 + i, 0x55); // tile 0: index 5
        vram(&mut gba, 0x10000 + 32 + i, 0x66); // tile 1: index 6
    }
    // Object priority 0 ties BG0's priority 0: GBATEK says the object is displayed on top.
    object(&mut gba, 0, 10, 10, 0);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 10), 0x03E0, "the tie goes to the object");
    // A higher priority number loses to the background.
    object(&mut gba, 0, 10, 10, 1 << 10);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 10, 10),
        0x001F,
        "BG0 at priority 0 is above an object at priority 1"
    );
    // With no background, the lowest OAM index keeps a pixel even at a worse priority: GBATEK,
    // *DS Video OBJs*, "the GBA has been assigning OBJ priority in respect to the 7bit OAM entry
    // number, regardless of the OBJs 2bit BG-priority attribute".
    io(&mut gba, 0x000, 1 << 12);
    object(&mut gba, 0, 10, 10, 3 << 10);
    object(&mut gba, 1, 10, 10, 0);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 10), 0x03E0, "OAM 0 wins over OAM 1");
}

#[test]
fn an_affine_object_rotates_inside_a_double_size_box() {
    let mut gba = object_machine();
    obj_palette(&mut gba, 1, 0x7C00);
    // Tile 0 is one pixel of index 1 at its top-left corner.
    vram(&mut gba, 0x10000, 1);
    // Group 0: a quarter turn — PA = 0, PB = 1, PC = -1, PD = 0.
    oam(&mut gba, 0x06, 0x0000);
    oam(&mut gba, 0x0E, 0x0100);
    oam(&mut gba, 0x16, 0xFF00);
    oam(&mut gba, 0x1E, 0x0000);
    // An 8x8 affine object at (16, 16), double-size, selecting group 0 (attr1 bits 9-13 = 0).
    object(&mut gba, 0, 0x0100 | 0x0200 | 16, 16, 0);
    let picture = shot(&mut gba);
    // Dest (12, 4) of the 16x16 box samples source (0, 0): the tile's corner, turned.
    assert_eq!(at(&picture, 16 + 12, 16 + 4), 0x7C00);
    // Without the double-size bit the 8x8 box clips that pixel: a quarter-turned corner of the
    // object lies outside its own normal rectangle.
    object(&mut gba, 0, 0x0100 | 16, 16, 0);
    let picture = shot(&mut gba);
    let lit: Vec<_> = picture
        .iter()
        .enumerate()
        .filter(|&(_, &word)| word != 0)
        .map(|(i, &word)| (i % WIDTH, i / WIDTH, word))
        .collect();
    assert!(
        picture.iter().all(|&word| word == 0),
        "the rotated corner is outside the normal box, but these pixels are lit: {lit:?}"
    );
}

#[test]
fn an_object_with_mosaic_shows_the_block_corner() {
    let mut gba = object_machine();
    gradient_tile(&mut gba, 0x10000, 1); // pixel (x, y) of the tile is index 1 + x + y * 8
    for n in 1..=64u16 {
        obj_palette(&mut gba, n, n * 0x20);
    }
    io(&mut gba, 0x04C, (1 << 8) | (1 << 12)); // OBJ mosaic 2x2
    object(&mut gba, 0, 0x1000 | 10, 10, 0); // attr0 bit 12: this object mosaics
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 10), 0x20, "the block's top-left pixel");
    assert_eq!(at(&picture, 11, 10), 0x20, "two pixels wide");
    assert_eq!(at(&picture, 12, 10), 0x60, "the next block starts at x=2");
    assert_eq!(at(&picture, 10, 11), 0x20, "two lines tall");
    assert_eq!(
        at(&picture, 10, 13),
        0x60,
        "the block starting at y=2 takes its corner pixel, index 3"
    );
}

#[test]
fn an_object_whose_tile_is_below_512_is_not_displayed_in_a_bitmap_mode() {
    let mut gba = machine();
    io(&mut gba, 0x000, 3 | (1 << 10) | (1 << 12)); // mode 3, BG2, objects
    no_objects(&mut gba);
    obj_palette(&mut gba, 5, 0x001F);
    for i in 0..32u32 {
        vram(&mut gba, 0x14000 + i, 0x55); // character 512, where the object area starts here
    }
    object(&mut gba, 0, 0, 0, 0); // character 0
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        0,
        "characters 0-511 are not displayable"
    );
    object(&mut gba, 0, 0, 0, 512);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F, "character 512 is");
}

// ---------------------------------------------------------------------------------------------
// Windows and the colour effects
// ---------------------------------------------------------------------------------------------

#[test]
fn a_window_hides_a_layer_inside_its_rectangle_and_window_0_wins_an_overlap() {
    let mut gba = mode0();
    bg_palette(&mut gba, 0, 0x7FFF); // white backdrop
    bg_palette(&mut gba, 1, 0x001F); // BG0 red
    solid_tile(&mut gba, 0x4000, 0, 1);
    io(&mut gba, 0x000, (1 << 8) | (1 << 13)); // BG0 and window 0
    io(&mut gba, 0x040, (100 << 8) | 140); // WIN0H: X1 = 100, X2 = 140
    io(&mut gba, 0x044, (10 << 8) | 50); // WIN0V: Y1 = 10, Y2 = 50
    io(&mut gba, 0x048, 0x0000); // inside WIN0: no layers at all
    io(&mut gba, 0x04A, 0x3F); // outside: every layer
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 99, 20), 0x001F, "outside the window: BG0");
    assert_eq!(
        at(&picture, 100, 20),
        0x7FFF,
        "inside: BG0 hidden, backdrop"
    );
    assert_eq!(at(&picture, 139, 49), 0x7FFF, "the far corner inside");
    assert_eq!(at(&picture, 140, 20), 0x001F, "X2 is exclusive");
    assert_eq!(at(&picture, 100, 50), 0x001F, "Y2 is exclusive");
    assert_eq!(at(&picture, 100, 9), 0x001F, "Y1 is inclusive");
    // Window 1 over the right half of window 0: window 0 has the higher priority, so the overlap
    // keeps window 0's backdrop and the rest of window 1 shows BG0 again.
    io(&mut gba, 0x000, (1 << 8) | (1 << 13) | (1 << 14));
    io(&mut gba, 0x042, (120 << 8) | 180); // WIN1H: X1 = 120, X2 = 180
    io(&mut gba, 0x046, 160); // WIN1V: Y1 = 0, Y2 = 160
    io(&mut gba, 0x048, 0x3F00); // inside window 1: every layer
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 130, 20), 0x7FFF, "window 0 beats window 1");
    assert_eq!(at(&picture, 150, 20), 0x001F, "window 1 shows BG0 again");
}

#[test]
fn an_object_window_masks_a_rectangle_and_is_not_displayed_itself() {
    let mut gba = mode0();
    bg_palette(&mut gba, 0, 0x7FFF);
    bg_palette(&mut gba, 1, 0x001F);
    solid_tile(&mut gba, 0x4000, 0, 1);
    // BG0, objects and the object window; outside shows every layer, inside the object window
    // shows none (WINOUT's high six bits).
    io(&mut gba, 0x000, (1 << 8) | (1 << 12) | (1 << 15));
    io(&mut gba, 0x04A, 0x3F);
    no_objects(&mut gba);
    for i in 0..32u32 {
        vram(&mut gba, 0x10000 + i, 0x11); // a solid 8x8 tile for the window shape
    }
    // OBJ mode 2: the object marks a window and is not drawn.
    object(&mut gba, 0, 30 | (2 << 10), 30, 0);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 29, 30),
        0x001F,
        "outside the object window: BG0"
    );
    assert_eq!(
        at(&picture, 30, 30),
        0x7FFF,
        "inside it: the backdrop, because the object window has no layers"
    );
    assert_eq!(at(&picture, 37, 37), 0x7FFF);
    assert_eq!(
        at(&picture, 38, 37),
        0x001F,
        "the window is the object's own 8x8 box"
    );
}

#[test]
fn alpha_blending_mixes_the_two_targets_and_only_when_the_second_is_one() {
    let mut gba = machine();
    io(&mut gba, 0x000, (1 << 8) | (1 << 9)); // BG0 and BG1
    io(&mut gba, 0x008, (1 << 2) | (1 << 8)); // BG0: priority 0, map $800
    io(&mut gba, 0x00A, 1 | (1 << 2) | (2 << 8)); // BG1: priority 1, map $1000
    bg_palette(&mut gba, 1, 0x001F); // red
    bg_palette(&mut gba, 2, 0x03E0); // green
    solid_tile(&mut gba, 0x4000, 0, 1);
    solid_tile(&mut gba, 0x4000, 1, 2);
    map_entry(&mut gba, 0x1000, 0, 0, 1);
    // BG0 is the first target, BG1 the second, and the effect is alpha at half each.
    io(&mut gba, 0x050, (1 << 0) | (1 << 9) | (1 << 6));
    io(&mut gba, 0x052, 8 | (8 << 8));
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        0x01EF,
        "red and green at 8/16 each: (31*8 >> 4) in each channel is 15"
    );
    // Point the second target at BG0 instead: BG1 is no longer a second target, so nothing mixes
    // and the first target shows at full strength (GBATEK, *Color Special Effects*).
    io(&mut gba, 0x050, (1 << 0) | (1 << 8) | (1 << 6));
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 0, 0), 0x001F, "no second target, no blend");
}

#[test]
fn a_semi_transparent_object_always_alpha_blends() {
    let mut gba = machine();
    io(&mut gba, 0x000, (1 << 8) | (1 << 12)); // BG0 and objects
    io(&mut gba, 0x008, (1 << 2) | (1 << 8)); // BG0 at priority 0
    no_objects(&mut gba);
    bg_palette(&mut gba, 1, 0x03E0); // green
    solid_tile(&mut gba, 0x4000, 0, 1);
    obj_palette(&mut gba, 5, 0x001F); // red
    for i in 0..32u32 {
        vram(&mut gba, 0x10000 + i, 0x55);
    }
    // The object ties BG0's priority, so it is on top; it is mode 1 (semi-transparent).
    object(&mut gba, 0, 10 | (1 << 10), 10, 0);
    // BLDCNT: BG0 is a second target, the effect bits say "none" and OBJ is not a first target —
    // a semi-transparent object blends anyway.
    io(&mut gba, 0x050, 1 << 8);
    io(&mut gba, 0x052, 8 | (8 << 8));
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 10), 0x01EF, "red over green at half each");
    // With no second target under it, the object shows at full strength.
    io(&mut gba, 0x050, 0);
    let picture = shot(&mut gba);
    assert_eq!(at(&picture, 10, 10), 0x001F);
}

#[test]
fn brightness_fades_the_first_target_up_and_down() {
    let mut gba = mode0();
    bg_palette(&mut gba, 1, 0x1084); // (4, 4, 4)
    solid_tile(&mut gba, 0x4000, 0, 1);
    io(&mut gba, 0x000, 1 << 8);
    io(&mut gba, 0x050, (1 << 0) | (2 << 6)); // BG0 first target, brightness increase
    io(&mut gba, 0x054, 8);
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        0x4631,
        "I + (31-I)*8/16: 4 becomes 17 in each channel"
    );
    io(&mut gba, 0x050, (1 << 0) | (3 << 6)); // brightness decrease
    let picture = shot(&mut gba);
    assert_eq!(
        at(&picture, 0, 0),
        0x0842,
        "I - I*8/16: 4 becomes 2 in each channel"
    );
}

// ---------------------------------------------------------------------------------------------
// The public ROMs
// ---------------------------------------------------------------------------------------------

/// Load one of the three PPU test ROMs.
fn test_rom(name: &str) -> Gba {
    let path = format!(
        "{}/roms/test/gba/ppu/{name}.gba",
        env!("CARGO_MANIFEST_DIR")
    );
    let rom = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    Gba::new(Cartridge::new(rom).expect("a public test ROM"))
}

/// `shades.gba`'s picture, derived from `roms/test/gba/ppu/shades.asm`:
///
/// * the palette loop starts at 0 and adds `2 shl 10` (2048) after every store, so entry `n` is
///   `n * 2048` — blue intensity `2n`, not `n`;
/// * tiles 0-15 are each 32 bytes of one value: tile `n` is 32 bytes of `n * 0x11`, i.e. a solid
///   8x8 block of 4bpp index `n`;
/// * the map loop stores two halfwords per four bytes and adds one to the tile number, so a row of
///   the 32x32 map reads `0,0,1,1,2,2,...,15,15` — 16-pixel-wide bands in tile order.
///
/// The 240-pixel screen therefore shows 15 bands, `n = x / 16` from 0 to 14, on every line.
fn expected_shades() -> Vec<u16> {
    let mut expected = vec![0u16; PIXELS];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let n = (x / 16) as u16;
            expected[y * WIDTH + x] = n * 0x800;
        }
    }
    expected
}

/// `stripes.gba`'s picture, derived from `roms/test/gba/ppu/stripes.asm`:
///
/// * palette entry 0 is `$560B` and entry 1 is `$6290` (the two `strh`s at `MEM_PALETTE` and
///   `MEM_PALETTE+2`, in that order);
/// * tile 0 is 32 bytes of `$11111111`, i.e. 4bpp index 1 everywhere — `$6290`; the other 31 tiles
///   the loop writes are never in the map;
/// * the map loop stores `1` at every fourth byte and leaves the halfword at `+2` at zero, so the
///   entries alternate `1, 0, 1, 0, ...`: tile 1 (all zeroes, index 0, transparent → the backdrop,
///   `$560B`) then tile 0 (`$6290`).
///
/// Eight-pixel stripes, `$6290` where `x / 8` is odd and `$560B` where it is even, on every line.
fn expected_stripes() -> Vec<u16> {
    let mut expected = vec![0u16; PIXELS];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            expected[y * WIDTH + x] = if (x / 8) % 2 == 0 { 0x560B } else { 0x6290 };
        }
    }
    expected
}

#[test]
#[ignore = "needs roms/test/gba/ppu/shades.gba, a public test ROM kept out of the repository"]
fn the_shades_rom_draws_its_gradient_pixel_for_pixel() {
    let mut gba = test_rom("shades");
    for _ in 0..60 {
        gba.run_frame();
    }
    let actual = gba.frame().colours.to_vec();
    let expected = expected_shades();
    let (same, different) = diff(&actual, &expected);
    println!(
        "shades.gba frame 60: {same} of {} pixels match, {different} differ",
        PIXELS
    );
    assert_eq!(different, 0, "{same} of {PIXELS} pixels match");
}

#[test]
#[ignore = "needs roms/test/gba/ppu/stripes.gba, a public test ROM kept out of the repository"]
fn the_stripes_rom_draws_its_eight_pixel_stripes_pixel_for_pixel() {
    let mut gba = test_rom("stripes");
    for _ in 0..60 {
        gba.run_frame();
    }
    let actual = gba.frame().colours.to_vec();
    let expected = expected_stripes();
    let (same, different) = diff(&actual, &expected);
    println!(
        "stripes.gba frame 60: {same} of {} pixels match, {different} differ",
        PIXELS
    );
    assert_eq!(different, 0, "{same} of {PIXELS} pixels match");
}

/// The glyphs `hello.gba` draws, as `(character, rows 0-3, rows 4-7)`. These are the four-word entries
/// of `roms/test/gba/src/glyphs.asm` for the characters of "Hello world!", which `text_char` loads a
/// pair at a time: `glyphs + (char - 32) * 8` holds the upper word and the word after it. Each word
/// is thirty-two pixels — four rows of eight, bit 0 of each row the leftmost pixel, which is the
/// order `text_glyph_data` shifts them out in and the order it stores them in.
const HELLO_GLYPHS: [(char, u32, u32); 12] = [
    ('H', 0x7E66_6666, 0x0066_6666),
    ('e', 0x663C_0000, 0x003C_067E),
    ('l', 0x1818_1818, 0x0030_1818),
    ('l', 0x1818_1818, 0x0030_1818),
    ('o', 0x663C_0000, 0x003C_6666),
    (' ', 0x0000_0000, 0x0000_0000),
    ('w', 0x6363_0000, 0x0036_7F6B),
    ('o', 0x663C_0000, 0x003C_6666),
    ('r', 0x663E_0000, 0x0006_0606),
    ('l', 0x1818_1818, 0x0030_1818),
    ('d', 0x667C_6060, 0x007C_6666),
    ('!', 0x1818_1818, 0x0018_0018),
];

/// `hello.gba`'s picture, derived from its source:
///
/// * `text_init` selects mode 4 (the 8bpp bitmap, page 0) with BG2 enabled, so the picture is VRAM
///   byte a pixel, 240 bytes a line, and palette entry 0 is the transparent backdrop — black, since
///   the ROM colours only entry 1;
/// * `text_color 0xFFFF, 1` makes palette entry 1 white;
/// * `text_pos 72, 76` then twelve `text_char`s draw "Hello world!" left to right, eight pixels a
///   character, writing each glyph's bits straight into the bitmap at `240 * y + x`.
fn expected_hello() -> Vec<u16> {
    let mut expected = vec![0u16; PIXELS];
    for (index, (_, upper, lower)) in HELLO_GLYPHS.iter().enumerate() {
        let x = 72 + index * 8;
        for row in 0..8usize {
            let bits = if row < 4 {
                upper >> (8 * row)
            } else {
                lower >> (8 * (row - 4))
            };
            for column in 0..8usize {
                if bits & (1 << column) != 0 {
                    expected[(76 + row) * WIDTH + x + column] = 0xFFFF;
                }
            }
        }
    }
    expected
}

#[test]
#[ignore = "needs roms/test/gba/ppu/hello.gba, a public test ROM kept out of the repository"]
fn the_hello_rom_draws_its_text_pixel_for_pixel() {
    let mut gba = test_rom("hello");
    for _ in 0..60 {
        gba.run_frame();
    }
    let actual = gba.frame().colours.to_vec();
    let expected = expected_hello();
    let (same, different) = diff(&actual, &expected);
    let white = expected.iter().filter(|&&word| word == 0xFFFF).count();
    println!(
        "hello.gba frame 60: {same} of {} pixels match, {different} differ ({white} white)",
        PIXELS
    );
    assert_eq!(different, 0, "{same} of {PIXELS} pixels match");
}
