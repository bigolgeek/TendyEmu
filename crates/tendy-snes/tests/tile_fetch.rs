//! `tests/tile_fetch.rs` — the BG character fetch, pinned from a hand-built tilemap and character
//! set.
//!
//! The picture that was broken was composed right and painted wrong: every tile's *pixels* came from
//! the wrong place in VRAM because the character fetch masked its **byte** offset with `0x7FFF`,
//! folding everything above 32 KiB onto the bottom half of the 64 KiB VRAM. This file builds a
//! synthetic scene with the character data deliberately above `$8000` and pins, from the pixels
//! outwards, the whole arithmetic the fetch rests on:
//!
//! - the map word's bit layout (`VH00PPPP NNNNNNNN`): low 10 bits the character number, bits 12-10
//!   the palette, bit 13 the priority, bits 14/15 horizontal and vertical flip (fullsnes
//!   "2107h - BG1SC" and "SNES PPU Video Memory");
//! - the map base: `BGnSC` bits 7-2 in 1K-**word** steps, so the first screen starts at word
//!   `(value >> 2) * 0x400` and one screen is 32x32 words;
//! - the character base: `BG12NBA`'s nibble in 4K-**word** steps, i.e. `nibble * 0x2000` bytes;
//! - 16 bytes per row-pair set: a 4bpp tile is 32 bytes (16 words), planes 0/1 in the first 16 bytes
//!   and 2/3 in the second, plane *p* of row *n* at byte `n * 2 + (p >> 1) * 16 + (p & 1)`;
//! - bit 7 of a plane byte is the leftmost pixel;
//! - a 16-colour layer reads CGRAM at `palette * 16 + index`.

use tendy_snes::WIDTH;
use tendy_snes::ppu::Ppu;

/// Dots in a line; four master clocks each.
const DOTS: u16 = 341;

/// The character base the scene uses: `BG12NBA` nibble 4, which is `4 * 0x2000` bytes = word
/// `0x4000`. Above `$8000` bytes on purpose — that is the half of VRAM the old mask dropped.
const CHAR_BASE_BYTES: usize = 4 * 0x2000;

/// The map base the scene uses: `BG1SC` bits 7-2 = 2, i.e. `2 * 0x400` words = byte `0x1000`.
const MAP_BASE_WORDS: usize = 2 * 0x400;

/// The tile the scene paints: not a round number, so a shifted address cannot land on it by luck.
const TILE: u16 = 0x123;
/// A second tile, solid colour index 9, so a palette error is visible as well as an address error.
const SOLID_TILE: u16 = 0x124;

/// Tick one dot at a time until `(line, dot)` is reached.
fn run_to(ppu: &mut Ppu, line: u16, dot: u16) {
    let mut guard = 0u32;
    while !(ppu.line == line && ppu.dot == dot) {
        ppu.tick(4);
        guard += 1;
        assert!(
            guard < u32::from(DOTS) * 262 * 2,
            "run_to({line}, {dot}) never arrived"
        );
    }
}

/// The finished frame row for screen line `line` (1..=224).
fn row_of(ppu: &Ppu, line: u16) -> Vec<u16> {
    let start = (usize::from(line) - 1) * WIDTH;
    ppu.frame[start..start + WIDTH].to_vec()
}

/// The colour index the scene's first tile carries at `(row, column)`: every one of the tile's 64
/// pixels a different index, cycling through 1-15 and never 0, so a wrong row, a wrong plane or a
/// wrong bit order all show up as a wrong colour.
fn index_at(row: usize, column: usize) -> u8 {
    ((row * 8 + column) % 15) as u8 + 1
}

/// Writes one 4bpp tile at `tile` of the character base, one byte per plane row, in fullsnes's
/// layout: planes 0/1 in the first 16 bytes of the tile, planes 2/3 in the second, row *n* at byte
/// `n * 2`, plane *p* at `(p >> 1) * 16 + (p & 1)`, bit 7 the leftmost pixel.
fn write_tile(ppu: &mut Ppu, tile: u16, index: impl Fn(usize, usize) -> u8) {
    let start = CHAR_BASE_BYTES + usize::from(tile) * 32;
    for row in 0..8usize {
        for column in 0..8usize {
            let colour = index(row, column);
            for plane in 0..4usize {
                if (colour >> plane) & 1 != 0 {
                    let offset = start + row * 2 + (plane >> 1) * 16 + (plane & 1);
                    ppu.vram[offset] |= 0x80 >> column;
                }
            }
        }
    }
}

/// Writes one map entry at `(column, row)` of the first screen.
fn put_map(ppu: &mut Ppu, column: usize, row: usize, entry: u16) {
    let word = MAP_BASE_WORDS + row * 32 + column;
    ppu.vram[word * 2] = entry as u8;
    ppu.vram[word * 2 + 1] = (entry >> 8) as u8;
}

/// The scene: mode 1, BG1 alone on the main screen, brightness 15, the two characters above, and a
/// four-cell row of map entries that between them exercise every field of the map word.
fn build() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.write(0x00, 0x0F); // brightness 15, forced blank off
    ppu.write(0x05, 0x01); // mode 1
    ppu.write(0x07, 0x08); // BG1SC: base 2 (word 0x800), one-screen
    ppu.write(0x0B, 0x04); // BG12NBA: BG1 base 4 (byte 0x8000), BG2 base 0
    ppu.write(0x0C, 0x00); // BG34NBA: nothing
    ppu.write(0x2C, 0x01); // TM: BG1 on the main screen
    ppu.write(0x2D, 0x00); // TS: nothing

    // Palette 0's sixteen colours and palette 2's, all distinct, so an index or a palette shift
    // changes the pixel rather than landing on the same black.
    for index in 0..16usize {
        ppu.cgram[index] = 0x0100 + index as u16;
        ppu.cgram[2 * 16 + index] = 0x0200 + index as u16;
    }

    write_tile(&mut ppu, TILE, index_at);
    write_tile(&mut ppu, SOLID_TILE, |_, _| 9);

    // Cell 0: the tile, palette 0, nothing else.
    put_map(&mut ppu, 0, 0, TILE);
    // Cell 1: the same tile mirrored horizontally: bit 14.
    put_map(&mut ppu, 1, 0, TILE | 0x4000);
    // Cell 2: mirrored vertically: bit 15.
    put_map(&mut ppu, 2, 0, TILE | 0x8000);
    // Cell 3: another tile, palette 2 and priority 1: bits 12-10 and bit 13.
    put_map(&mut ppu, 3, 0, SOLID_TILE | (2 << 10) | 0x2000);
    ppu
}

/// The colours the scene's palette holds, as the frame carries them.
fn colour(index: usize) -> u16 {
    0x0100 + index as u16
}

#[test]
fn the_fetch_reads_the_character_it_was_pointed_at() {
    let mut ppu = build();
    run_to(&mut ppu, 1, 1);
    let row = row_of(&ppu, 1);
    for (column, &pixel) in row.iter().enumerate().take(8) {
        assert_eq!(
            pixel,
            colour(usize::from(index_at(0, column))),
            "tile {TILE:#05x} at char base {CHAR_BASE_BYTES:#06x}: pixel {column} of row 0"
        );
    }
}

#[test]
fn every_row_of_the_tile_reads_its_own_bit_planes() {
    let mut ppu = build();
    for line in 1..=8u16 {
        run_to(&mut ppu, line, 1);
        let row = row_of(&ppu, line);
        let tile_row = usize::from(line) - 1;
        for (column, &pixel) in row.iter().enumerate().take(8) {
            assert_eq!(
                pixel,
                colour(usize::from(index_at(tile_row, column))),
                "line {line}, pixel {column}"
            );
        }
    }
}

#[test]
fn the_map_words_flip_and_palette_bits_land_where_fullsnes_puts_them() {
    let mut ppu = build();
    run_to(&mut ppu, 1, 1);
    let row = row_of(&ppu, 1);
    // Horizontal flip (bit 14): the row reversed.
    for (column, &pixel) in row[8..16].iter().enumerate() {
        assert_eq!(
            pixel,
            colour(usize::from(index_at(0, 7 - column))),
            "hflip, column {column}"
        );
    }
    // Vertical flip (bit 15) on the next cell over, same line: row 7 of the tile.
    for (column, &pixel) in row[16..24].iter().enumerate() {
        assert_eq!(
            pixel,
            colour(usize::from(index_at(7, column))),
            "vflip, column {column}"
        );
    }
    // Palette bits 12-10: a solid index 9 in palette 2 reads CGRAM at 2 * 16 + 9.
    for (column, &pixel) in row[24..32].iter().enumerate() {
        assert_eq!(pixel, 0x0200 + 9, "palette 2, index 9, column {column}");
    }
}

#[test]
fn a_character_base_above_thirty_two_kilobytes_is_not_folded_back() {
    // The defect, fixed in one line: with the byte offset masked by `0x7FFF` the fetch read
    // VRAM at `offset - 0x8000`, which here is an untouched region and renders as transparency.
    let mut ppu = build();
    run_to(&mut ppu, 1, 1);
    let row = row_of(&ppu, 1);
    assert!(
        row[..8].iter().any(|&pixel| pixel != ppu.cgram[0]),
        "a character at {CHAR_BASE_BYTES:#06x} fetched nothing: the offset was folded at 32 KiB"
    );
}
