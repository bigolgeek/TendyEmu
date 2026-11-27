//! The cartridge's own border — `CHR_TRN`'s tiles, `PCT_TRN`'s map and palettes, the
//! colour-0 hole and the border in the save state.
//!
//! Every expectation is Pan Docs' *Border Commands* text, quoted where it is used:
//!
//! - *CHR_TRN*: "The tile data is sent by VRAM transfer (4 KiB)"; byte 1's "Bit 0 - Tile Numbers
//!   (0=Tiles $00-$7F, 1=Tiles $80-$FF)"; "Each tile occupies 32 bytes (8×8 pixels, 16 colors
//!   each)"; and "Each tile is stored in 4-bit-per-pixel format consisting of bit planes 0 and 1
//!   interleaved by row, followed by bit planes 2 and 3 interleaved by row."
//! - *PCT_TRN*: "BG Map 32×28 Entries of 16 bits each (1792 bytes)" and "BG Palette Data (Palettes
//!   4-6, 16 little-endian RGB555 colors each)" at `$800` of the transfer; each entry is "`VH01 PP00
//!   NNNN NNNN`" with "Bit 0-9 Character Number … Bit 10-12 Palette Number … Bit 14 X-Flip … Bit 15
//!   Y-Flip".
//! - The hole: "The 20×18 entries in the center of the 32×28 area should be set to a blank (solid
//!   color 0) tile as transparent space for the Game Boy window to be displayed inside.
//!   Non-transparent border data will cover the Game Boy window."
//!
//! The VRAM buffers here are synthetic: `CHR_TRN` and `PCT_TRN` carry no picture of their own, they
//! tell the SNES to read 4 KiB of the Game Boy's VRAM, so a hand-built 4 KiB buffer is exactly what
//! the command sees. The expected words are hand-written palette tables, not this file's own
//! shifting arithmetic.

mod common;

use tendy_gb::sgb::{
    Border, MaskState, Packet, SGB_FRAME_WIDTH, Sgb, SgbFrame, WINDOW_X, WINDOW_Y,
};

/// Palette 4 as this file sends it: colour `c` is `$00(c*$11)`, so every index has its own word and
/// an index read from the wrong plane cannot match. Colour 1 is the one every tile below uses.
const PAL4: [u16; 16] = [
    0x0000, 0x0011, 0x0022, 0x0033, 0x0044, 0x0055, 0x0066, 0x0077, 0x0088, 0x0099, 0x00AA, 0x00BB,
    0x00CC, 0x00DD, 0x00EE, 0x00FF,
];

/// `CHR_TRN`: header `$99` (`$13 << 3 | 1`), byte 1 the destination.
fn chr_trn(destination: u8) -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x99;
    bytes[1] = destination;
    Packet { bytes }
}

/// `PCT_TRN`: header `$A1` (`$14 << 3 | 1`), "Byte 1-F Not used (zero)" — the data is in VRAM.
fn pct_trn() -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xA1;
    Packet { bytes }
}

/// 4 KiB of tile data: `tiles[128 * 32]`, the 128 tiles one `CHR_TRN` carries, with `paint` writing
/// each one. Half 0 lands on tiles `$00-$7F`, half 1 on `$80-$FF`.
fn tile_vram(paint: impl Fn(&mut [u8])) -> Vec<u8> {
    let mut vram = vec![0u8; 4096];
    paint(&mut vram);
    vram
}

/// Every tile in the transfer filled with `byte`: index `byte & 1` in plane 0 and so on.
fn solid_tiles(byte: u8) -> Vec<u8> {
    tile_vram(|vram| vram.fill(byte))
}

/// 4 KiB for `PCT_TRN`: the map holds `entries` (`(cell index, entry)`, everything else tile 0 /
/// palette 4 / no flip) and palette 4 is [`PAL4`]; palettes 5 and 6 are left `$0011`-free.
fn map_vram(entries: &[(usize, u16)]) -> Vec<u8> {
    let mut vram = vec![0u8; 4096];
    for &(cell, entry) in entries {
        vram[cell * 2] = entry as u8;
        vram[cell * 2 + 1] = (entry >> 8) as u8;
    }
    for (colour, &word) in PAL4.iter().enumerate() {
        let at = 0x800 + colour * 2;
        vram[at] = word as u8;
        vram[at + 1] = (word >> 8) as u8;
    }
    vram
}

/// The border word at `(x, y)` of the 256-wide picture.
fn word(border: &Border, x: usize, y: usize) -> u16 {
    border.pixels()[y * SGB_FRAME_WIDTH + x]
}

/// Cell `col, row` of the map is entry `row * 32 + col`.
fn cell(col: usize, row: usize) -> usize {
    row * 32 + col
}

/// A tile number and palette 4 in one map entry: bits 0-9 the tile, bits 10-12 the palette.
fn tile_in_palette_4(tile: u16, flags: u16) -> u16 {
    tile | (4 << 10) | flags
}

// ---------------------------------------------------------------- CHR_TRN

/// Until `PCT_TRN` arrives the picture is the art, whatever tiles were uploaded: `CHR_TRN`
/// says where tile data goes, not where it is shown ("The actual tiles must be separately
/// transferred by using the CHR_TRN function" is `PCT_TRN`'s sentence, and it is a map that places
/// them).
#[test]
fn tiles_alone_keep_the_placeholder() {
    let mut sgb = Sgb::on();
    assert!(!sgb.border().has_map(), "no map has arrived yet");
    assert_eq!(sgb.border().pixels(), Border::new().pixels());

    sgb.apply(&chr_trn(0), &solid_tiles(0xFF));
    assert!(!sgb.border().has_map());
    assert_eq!(
        sgb.border().pixels(),
        Border::new().pixels(),
        "tiles nobody placed must not repaint the border"
    );
}

/// One tile in a known palette lands in the 8×8 cell its map entry names (spec test 1).
#[test]
fn a_tile_lands_in_the_cell_the_map_names() {
    // Tile 1 is 32 bytes of $FF: every pixel is index 15.
    let mut tiles = vec![0u8; 4096];
    tiles[32..64].fill(0xFF);

    let mut sgb = Sgb::on();
    sgb.apply(&chr_trn(0), &tiles);
    sgb.apply(
        &pct_trn(),
        &map_vram(&[(cell(3, 2), tile_in_palette_4(1, 0))]),
    );

    assert!(sgb.border().has_map());
    let border = sgb.border();
    // Cell (3,2) covers pixels x = 24..32, y = 16..24.
    for y in [16, 17, 23] {
        for x in [24, 25, 31] {
            assert_eq!(
                word(border, x, y),
                PAL4[15],
                "({x},{y}) is tile 1's index 15 in palette 4"
            );
        }
    }
    // The cell to its right and the one below hold tile 0, whose pixels are all index 0: the hole.
    assert_eq!(word(border, 32, 16), 0x0000, "cell (4,2) is tile 0");
    assert_eq!(word(border, 24, 24), 0x0000, "cell (3,3) is tile 0");
    assert_eq!(word(border, 0, 0), 0x0000, "cell (0,0) is tile 0");
}

/// X-flip and Y-flip mirror the tile inside its cell (spec test 2).
///
/// Tile 7 has one pixel at index 1 — its top-left, column 0 row 0, the MSB of the row's plane-0
/// byte — and index 0 everywhere else. So "no flip" puts `$0011` at the cell's (0,0), X-flip at its
/// (7,0), Y-flip at its (0,7) and both at its (7,7).
#[test]
fn x_and_y_flip_mirror_the_tile() {
    let mut tiles = vec![0u8; 4096];
    tiles[7 * 32] = 0x80; // tile 7, row 0, plane 0, column 0.

    let mut sgb = Sgb::on();
    sgb.apply(&chr_trn(0), &tiles);
    sgb.apply(
        &pct_trn(),
        &map_vram(&[
            (cell(0, 0), tile_in_palette_4(7, 0x0000)),
            (cell(1, 0), tile_in_palette_4(7, 0x4000)),
            (cell(2, 0), tile_in_palette_4(7, 0x8000)),
            (cell(3, 0), tile_in_palette_4(7, 0xC000)),
        ]),
    );

    let border = sgb.border();
    assert_eq!(word(border, 0, 0), PAL4[1], "no flip: (0,0)");
    assert_eq!(word(border, 7, 0), 0x0000, "no flip: the rest is the hole");
    assert_eq!(word(border, 8 + 7, 0), PAL4[1], "X-flip: column 7");
    assert_eq!(word(border, 8, 0), 0x0000, "X-flip: column 0 is empty");
    assert_eq!(word(border, 16, 7), PAL4[1], "Y-flip: row 7");
    assert_eq!(word(border, 16, 0), 0x0000, "Y-flip: row 0 is empty");
    assert_eq!(word(border, 24 + 7, 7), PAL4[1], "both: (7,7)");
    assert_eq!(word(border, 24, 0), 0x0000, "both: (0,0) is empty");
}

/// Byte 1 bit 0 picks the half: tiles `$00-$7F` or `$80-$FF` (spec test 3).
///
/// Half 0 arrives as 32 bytes of `$FF` per tile (index 15); half 1 as a plane-0 `$FF` only (index
/// 1). A map naming tile 0, 127, 128 and 255 then tells, in one picture, which half each transfer
/// wrote and that neither wrote over the other. The last part re-sends half 0 after the map exists,
/// which is the "re-render after CHR_TRN once a map exists" rule.
#[test]
fn chr_trn_byte_1_bit_0_picks_the_tile_half() {
    let mut sgb = Sgb::on();
    sgb.apply(&chr_trn(0b000), &solid_tiles(0xFF));
    // Half 1: index 1 everywhere — plane 0 of every row set, planes 1-3 clear.
    sgb.apply(
        &chr_trn(0b001),
        &tile_vram(|vram| {
            for tile in 0..128 {
                for row in 0..8 {
                    vram[tile * 32 + row * 2] = 0xFF;
                }
            }
        }),
    );
    sgb.apply(
        &pct_trn(),
        &map_vram(&[
            (cell(0, 0), tile_in_palette_4(0, 0)),
            (cell(1, 0), tile_in_palette_4(128, 0)),
            (cell(2, 0), tile_in_palette_4(127, 0)),
            (cell(3, 0), tile_in_palette_4(255, 0)),
        ]),
    );

    let border = sgb.border();
    assert_eq!(word(border, 0, 0), PAL4[15], "tile 0 came from half 0");
    assert_eq!(word(border, 8, 0), PAL4[1], "tile 128 came from half 1");
    assert_eq!(
        word(border, 16, 0),
        PAL4[15],
        "tile 127 is the last of half 0"
    );
    assert_eq!(
        word(border, 24, 0),
        PAL4[1],
        "tile 255 is the last of half 1"
    );

    // Bit 1 (BG/OBJ) is not a second destination: Pan Docs notes it "seems to have no effect".
    // A later half-0 transfer rewrites tiles 0-127 in the picture the map already places.
    sgb.apply(
        &chr_trn(0b010),
        &tile_vram(|vram| {
            for tile in 0..128 {
                for row in 0..8 {
                    vram[tile * 32 + row * 2] = 0xFF;
                }
            }
        }),
    );
    assert_eq!(
        word(sgb.border(), 0, 0),
        PAL4[1],
        "tiles 0-127 were replaced once the map existed"
    );
    assert_eq!(
        word(sgb.border(), 8, 0),
        PAL4[1],
        "tiles 128-255 were left alone"
    );
}

// ---------------------------------------------------------------- PCT_TRN

/// Colour 0 is the transparent hole, not palette 4's colour 0 (spec test 4).
///
/// Palette 4's colour 0 is `$1234` here, so a renderer that painted "colour 0" instead of leaving a
/// hole would write `$1234`. The tile is index 1 everywhere except its top-left pixel.
#[test]
fn colour_zero_is_a_hole_not_palette_colour_zero() {
    let mut vram = vec![0u8; 4096];
    // Tile 0, row 0, plane 0: every column but the top-left (column 0 is the MSB).
    vram[0] = 0x7F;
    // Rows 1-7, plane 0: all eight columns.
    for row in 1..8 {
        vram[row * 2] = 0xFF;
    }

    let mut sgb = Sgb::on();
    sgb.apply(&chr_trn(0), &vram);
    let mut map = map_vram(&[(cell(0, 0), tile_in_palette_4(0, 0))]);
    // Palette 4's colour 0: a nonzero word, so "index 0 paints nothing" is visible.
    map[0x800] = 0x34;
    map[0x801] = 0x12;
    sgb.apply(&pct_trn(), &map);

    let border = sgb.border();
    assert_eq!(
        word(border, 0, 0),
        0x0000,
        "an index-0 pixel leaves the hole, not palette colour 0 ($1234)"
    );
    assert_eq!(word(border, 1, 0), PAL4[1]);
    assert_eq!(word(border, 0, 1), PAL4[1]);
}

/// "Non-transparent border data will cover the Game Boy window" — the compositor's half of the rule.
///
/// The tile sits in cell (6,5), the window's first cell (pixels 48..56, 40..48), and is index 1
/// everywhere but its top-left pixel, which is the hole. So the screen shows at (48,40) and the
/// border's `$0011` shows at (49,40).
#[test]
fn an_opaque_border_pixel_covers_the_window() {
    let mut vram = vec![0u8; 4096];
    vram[0] = 0x7F;
    for row in 1..8 {
        vram[row * 2] = 0xFF;
    }

    let mut sgb = Sgb::on();
    sgb.apply(&chr_trn(0), &vram);
    sgb.apply(
        &pct_trn(),
        &map_vram(&[(cell(WINDOW_X / 8, WINDOW_Y / 8), tile_in_palette_4(0, 0))]),
    );

    let mut frame = SgbFrame::new();
    frame.compose(sgb.border(), &vec![0x0022; 160 * 144], MaskState::Active);
    let at = |x: usize, y: usize| frame.pixels()[y * SGB_FRAME_WIDTH + x];
    assert_eq!(
        at(WINDOW_X, WINDOW_Y),
        0x0022,
        "the hole at (48,40) shows the screen"
    );
    assert_eq!(
        at(WINDOW_X + 1, WINDOW_Y),
        PAL4[1],
        "opaque border data covers the window at (49,40)"
    );
    assert_eq!(
        at(WINDOW_X, WINDOW_Y + 1),
        PAL4[1],
        "and at (48,41), so the whole tile is placed"
    );
}

// ---------------------------------------------------------------- the save state

/// The border is game state: `CHR_TRN`'s tiles and `PCT_TRN`'s map and palettes ride in the
/// machine's snapshot and the pixels are rebuilt from them on load (spec test 5).
#[test]
fn the_border_survives_a_machine_snapshot() {
    let mut tiles = vec![0u8; 4096];
    tiles[32..64].fill(0xFF); // tile 1: index 15 everywhere.

    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.bus.ppu.sgb.apply(&chr_trn(0), &tiles);
    m.bus.ppu.sgb.apply(
        &pct_trn(),
        &map_vram(&[
            (cell(0, 0), tile_in_palette_4(1, 0)),
            (cell(31, 27), tile_in_palette_4(1, 0x4000)),
        ]),
    );
    let saved_border = m.bus.ppu.sgb.border().clone();
    assert!(saved_border.has_map());
    let state = m.encode_state();

    let mut other = common::machine(common::synth_rom(&[]));
    assert_eq!(
        other.bus.ppu.sgb.border().pixels(),
        Border::new().pixels(),
        "a fresh machine holds the placeholder"
    );
    other.decode_state(&state).expect("our own snapshot");
    assert!(other.bus.ppu.sgb.border().has_map(), "the map came back");
    assert_eq!(
        other.bus.ppu.sgb.border().pixels(),
        saved_border.pixels(),
        "every one of the 256x224 pixels was rebuilt"
    );
    // And the reloaded machine writes the same bytes again: nothing is lost in the round trip.
    assert_eq!(other.encode_state(), state);
}

// ---------------------------------------------------------------- which 4 KiB a transfer reads

/// Drive `$FF00` the way a game does: one write per pulse, `0x10` a one bit, `0x20` a zero, and the
/// stop bit last.
fn bus_send(m: &mut tendy_gb::Machine, bytes: &[u8]) {
    m.bus.write(0xFF00, 0x00);
    for &byte in bytes {
        for bit in 0..8 {
            m.bus
                .write(0xFF00, if (byte >> bit) & 1 == 1 { 0x10 } else { 0x20 });
        }
    }
    m.bus.write(0xFF00, 0x20);
}

/// Run `frames` frames: each completes one frame, which is the beginning of the next one, and that
/// is where Pan Docs puts a `_TRN` transfer's reads and the end of the fifth one.
fn settle(m: &mut tendy_gb::Machine, frames: usize) {
    for _ in 0..frames {
        m.run_frame();
    }
}

/// Write `bytes` into VRAM at `addr` as stored (no mode-3 lock: this is the cartridge's picture).
fn put_vram(m: &mut tendy_gb::Machine, addr: usize, bytes: &[u8]) {
    m.bus.ppu.vram[addr - 0x8000..addr - 0x8000 + bytes.len()].copy_from_slice(bytes);
}

/// The 4 KiB a `_TRN` transfer reads is the character data block LCDC bit 4 selects.
///
/// Pan Docs, *VRAM Transfers*: "the SNES receives the data in from display scanlines, it will
/// automatically re-produce the same ordering of bits and bytes, as being originally stored at
/// 8000-8FFF in Game Boy memory" — the source is the character data the display is showing, whose
/// base *LCDC* bit 4 picks (`0 = $8800 addressing, 1 = $8000 addressing`). Both real SGB
/// cartridges tested clear bit 4 and stage their 4 KiB at `$8800-$97FF` with the BG map showing it in order
/// ("BG Map must display unsigned characters $00-$FF on the screen; $00..$13 in first line"): a
/// `$8000-$8FFF` read there hands every `_TRN` command zeros, and the composed picture is black.
#[test]
fn a_trn_transfer_reads_the_block_lcdc_bit_4_selects() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    // $8800 addressing: the character data block is $8800-$97FF, the map $9800.
    m.bus.write(0xFF40, 0x81);

    // The block holds tile 1 as 32 bytes of `$FF` (index 15), so $8800 is the source of the tiles.
    let mut tiles = vec![0u8; 4096];
    tiles[32..64].fill(0xFF);
    put_vram(&mut m, 0x8800, &tiles);
    bus_send(&mut m, &chr_trn(0).bytes);
    settle(&mut m, 6);

    // Then the same block holds PCT_TRN's map and palettes, tile 1 in cell (0,0).
    let map = map_vram(&[(cell(0, 0), tile_in_palette_4(1, 0))]);
    put_vram(&mut m, 0x8800, &map);
    bus_send(&mut m, &pct_trn().bytes);
    settle(&mut m, 6);

    let border = m.bus.ppu.sgb.border().clone();
    assert!(border.has_map(), "the map arrived through the receiver");
    assert_eq!(
        word(&border, 0, 0),
        PAL4[15],
        "the tile came from $8800-$97FF"
    );
    assert_eq!(word(&border, 8, 0), 0x0000, "and not from tile 0");

    // Set bit 4: the block is $8000-$8FFF, which this picture left empty, so the same commands now
    // read 4 KiB of zeros and the border is nothing but holes.
    m.bus.write(0xFF40, 0x91);
    bus_send(&mut m, &chr_trn(0).bytes);
    settle(&mut m, 6);
    bus_send(&mut m, &pct_trn().bytes);
    settle(&mut m, 6);
    let border = m.bus.ppu.sgb.border();
    assert_eq!(
        word(border, 0, 0),
        0x0000,
        "with LCDC.4 set the transfer reads $8000-$8FFF"
    );
}

// ---------------------------------------------------------------- the five-frame window

/// A `_TRN` command's data is not there when the command arrives: the read starts at the beginning
/// of the next frame and the command lands at the end of the fifth frame after it.
///
/// Pan Docs, *VRAM Transfers*, *Transfer Time*: "The actual transfer starts at the beginning of the
/// next frame after the command has been sent, and the transfer ends at the end of the 5th frame
/// after the command has been sent (not counting the frame in which the command has been sent).
/// The displayed data must not be modified during the transfer, as the SGB reads it in multiple
/// chunks."
///
/// This is the test that fails on the old code: there the border's map arrived inside `bus_send`,
/// so the very first assertion after the command would already be true.
#[test]
fn a_trn_command_lands_at_the_end_of_its_fifth_frame() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.bus.write(0xFF40, 0x81); // $8800 addressing: the transfer block is $8800-$97FF.
    put_vram(
        &mut m,
        0x8800,
        &map_vram(&[(cell(0, 0), tile_in_palette_4(1, 0))]),
    );

    bus_send(&mut m, &pct_trn().bytes);
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "the command's own frame is not part of the transfer"
    );

    settle(&mut m, 1);
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "one frame in: the first chunk has only just been read"
    );
    settle(&mut m, 2);
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "three frames in: the transfer is still going"
    );
    settle(&mut m, 2);
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "five frames in: the fifth frame is still running"
    );
    settle(&mut m, 1);
    assert!(
        m.bus.ppu.sgb.border().has_map(),
        "the end of the fifth frame: the map is the cartridge's"
    );
    assert_eq!(
        word(m.bus.ppu.sgb.border(), 0, 0),
        0x0000,
        "tile 0 is holes"
    );
}

/// The block is read "in multiple chunks": bytes taken before the cartridge rewrites VRAM keep the
/// old value, bytes read afterwards see the new one.
///
/// Every tile in the block is `$FF` (index 15). One frame in, the first chunk — tiles 0-25 — has
/// been read; the cartridge then rewrites tile 1 (in that chunk) and tile 100 (in the fourth chunk,
/// not yet read). A transfer that copied the whole block at the command would show both rewrites; a
/// transfer that copies it at the end would show neither, and the border shows exactly one.
#[test]
fn a_transfer_reads_the_block_in_chunks_across_its_five_frames() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.bus.write(0xFF40, 0x81);
    put_vram(&mut m, 0x8800, &solid_tiles(0xFF));
    bus_send(&mut m, &chr_trn(0).bytes);

    // The first read is at the beginning of the next frame; these two writes come after it.
    settle(&mut m, 1);
    let tile1 = 0x800 + 32;
    let tile100 = 0x800 + 100 * 32;
    m.bus.ppu.vram[tile1..tile1 + 32].fill(0x00);
    m.bus.ppu.vram[tile100..tile100 + 32].fill(0x00);
    settle(&mut m, 5);

    // Place both tiles and let the map's own transfer finish.
    put_vram(
        &mut m,
        0x8800,
        &map_vram(&[
            (cell(0, 0), tile_in_palette_4(1, 0)),
            (cell(1, 0), tile_in_palette_4(100, 0)),
        ]),
    );
    bus_send(&mut m, &pct_trn().bytes);
    settle(&mut m, 6);

    let border = m.bus.ppu.sgb.border();
    assert_eq!(
        word(border, 0, 0),
        PAL4[15],
        "tile 1 was read before the cartridge rewrote it"
    );
    assert_eq!(
        word(border, 8, 0),
        0x0000,
        "tile 100 was read after the rewrite, so it is the hole"
    );
}

/// A transfer in flight is session state: a snapshot load drops it, so a rewind out of the five
/// frames cannot land a border command the restored timeline never finished.
///
/// The state is taken before the command; the machine then starts the transfer and one chunk is
/// read, so a `_TRN` is in flight when the state is loaded back. Six frames later the border is
/// still the placeholder — the read the load interrupted does not resume against restored VRAM.
#[test]
fn a_snapshot_load_drops_a_trn_transfer_in_flight() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.bus.write(0xFF40, 0x81); // $8800 addressing: the transfer block is $8800-$97FF.
    put_vram(
        &mut m,
        0x8800,
        &map_vram(&[(cell(0, 0), tile_in_palette_4(1, 0))]),
    );
    let before = m.encode_state();

    bus_send(&mut m, &pct_trn().bytes);
    settle(&mut m, 1); // the first chunk is read; the transfer is now in flight.
    m.decode_state(&before).expect("our own snapshot");
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "the snapshot was taken before the command, so no map is in it"
    );

    settle(&mut m, 6);
    assert!(
        !m.bus.ppu.sgb.border().has_map(),
        "the transfer the load interrupted is dropped, not resumed"
    );
}
