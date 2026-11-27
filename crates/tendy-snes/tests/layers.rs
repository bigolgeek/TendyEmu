//! `tests/layers.rs` — modes 0-7, the priority chart, windows, colour math, mosaic, offset-per-tile,
//! the 256-pixel rendering of modes 5/6 and the next-line rule.

use tendy_snes::WIDTH;
use tendy_snes::ppu::Ppu;

/// Dots in a line; four master clocks each.
const DOTS: u16 = 341;

/// An inchworm: tick one dot at a time until `(line, dot)` is reached.
fn run_to(ppu: &mut Ppu, line: u16, dot: u16) {
    let mut guard = 0u32;
    while !(ppu.line == line && ppu.dot == dot) {
        ppu.tick(4);
        guard += 1;
        assert!(guard < 341 * 262 * 2, "run_to({line}, {dot}) never arrived");
    }
}

/// Renders `line` (its first dot is where the picture is built).
fn at_line(ppu: &mut Ppu, line: u16) {
    run_to(ppu, line, 1);
}

/// The finished frame row for screen line `line` (1..=224).
fn row_of(ppu: &Ppu, line: u16) -> Vec<u16> {
    let start = (usize::from(line) - 1) * WIDTH;
    ppu.frame[start..start + WIDTH].to_vec()
}

/// A PPU with brightness 15, so a colour's value is not scaled.
fn new_ppu() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.write(0x00, 0x0F);
    ppu
}

/// A palette entry.
fn set_colour(ppu: &mut Ppu, index: usize, colour: u16) {
    ppu.cgram[index] = colour;
}

/// Writes one tile's bit-planes so every pixel has `colour`, at word `base + tile * depth * 8`.
fn solid_tile(ppu: &mut Ppu, base_word: usize, tile: usize, depth: u8, colour: u8) {
    let start = base_word * 2 + tile * usize::from(depth) * 8;
    for plane in 0..depth {
        let byte = if (colour >> plane) & 1 != 0 { 0xFF } else { 0 };
        for row in 0..8 {
            let offset = start + row * 2 + usize::from(plane >> 1) * 16 + usize::from(plane & 1);
            ppu.vram[offset & 0x7FFF] = byte;
        }
    }
}

/// Writes one tile so column `c` (0-7) has colour `colours[c]`, on every row.
fn column_tile(ppu: &mut Ppu, base_word: usize, tile: usize, depth: u8, colours: &[u8; 8]) {
    let start = base_word * 2 + tile * usize::from(depth) * 8;
    for plane in 0..depth {
        let mut byte = 0u8;
        for (column, &colour) in colours.iter().enumerate() {
            if (colour >> plane) & 1 != 0 {
                byte |= 0x80 >> column;
            }
        }
        for row in 0..8 {
            let offset = start + row * 2 + usize::from(plane >> 1) * 16 + usize::from(plane & 1);
            ppu.vram[offset & 0x7FFF] = byte;
        }
    }
}

/// A mode-7 tile: 8-bit pixels at the odd bytes of a 128-byte region (fullsnes "Mode 7 BG Tiles").
fn mode7_tile(ppu: &mut Ppu, tile: usize, colour: u8) {
    for row in 0..8 {
        for column in 0..8 {
            let address = tile * 0x80 + row * 0x10 + column * 2 + 1;
            ppu.vram[address & 0x7FFF] = colour;
        }
    }
}

/// Fills one map *column* — every tile row — with `entry`, so any screen line of the test shows it.
fn put_map(ppu: &mut Ppu, map_word: usize, tx: usize, entry: u16) {
    for ty in 0..32 {
        let word = (map_word + ty * 32 + tx) & 0x7FFF;
        ppu.vram[word * 2] = entry as u8;
        ppu.vram[word * 2 + 1] = (entry >> 8) as u8;
    }
}

/// Fills a whole 32x32 screen with one entry.
fn fill_map(ppu: &mut Ppu, map_word: usize, entry: u16) {
    for tx in 0..32 {
        put_map(ppu, map_word, tx, entry);
    }
}

/// `BGnSC`: the map base in 1K-word steps, one 32x32 screen.
fn map_base(ppu: &mut Ppu, layer: u8, map_word: u16) {
    ppu.write(0x07 + layer, ((map_word / 0x400) as u8) << 2);
}

/// One OAM entry and its two high bits.
fn put_sprite(ppu: &mut Ppu, index: usize, x: u16, y: u8, tile: u8, attr: u8) {
    ppu.oam[index * 4] = x as u8;
    ppu.oam[index * 4 + 1] = y;
    ppu.oam[index * 4 + 2] = tile;
    ppu.oam[index * 4 + 3] = attr;
    let shift = (index % 4) * 2;
    let byte = &mut ppu.oam[0x200 + index / 4];
    *byte = (*byte & !(0x03 << shift)) | (((x >> 8) as u8 & 0x01) << shift);
}

mod layers {
    use super::*;

    /// §5.4's mode-0 row: four 4-colour layers, and each layer's CGRAM block moves by 32 entries
    /// (fullsnes "CGRAM Palette Indices": BG2-4 in mode 0 use 21h/41h/61h, not 01h).
    #[test]
    fn mode0_four_layers_at_two_bpp() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x00); // mode 0
        for layer in 0..4u8 {
            map_base(&mut ppu, layer, 0x400 * (u16::from(layer) + 1));
            solid_tile(&mut ppu, 0, 1 + usize::from(layer), 2, 1);
            put_map(
                &mut ppu,
                0x400 * (usize::from(layer) + 1),
                0,
                1 + u16::from(layer),
            );
        }
        set_colour(&mut ppu, 0x01, 0x001F);
        set_colour(&mut ppu, 0x21, 0x03E0);
        set_colour(&mut ppu, 0x41, 0x7C00);
        set_colour(&mut ppu, 0x61, 0x7FFF);

        for (layer, expected, line) in [
            (0u8, 0x001F, 10u16),
            (1, 0x03E0, 11),
            (2, 0x7C00, 12),
            (3, 0x7FFF, 13),
        ] {
            ppu.write(0x2C, 1 << layer);
            at_line(&mut ppu, line);
            assert_eq!(
                row_of(&ppu, line)[0],
                expected,
                "mode 0 BG{} is 4-colour from its own CGRAM block: fullsnes \"CGRAM Palette Indices\"",
                layer + 1
            );
        }
    }

    /// §5.4's mode-1 column: `$2105` bit 3 moves BG3's high-priority tiles in front of everything
    /// (".1a") or behind OBJ 1 (".1b").
    #[test]
    fn mode1_three_layers_with_bg3_priority() {
        let mut ppu = new_ppu();
        map_base(&mut ppu, 0, 0x400);
        map_base(&mut ppu, 2, 0xC00);
        solid_tile(&mut ppu, 0, 1, 4, 1); // BG1, 16 colours, palette 1
        solid_tile(&mut ppu, 0, 2, 2, 1); // BG3, 4 colours, palette 0
        put_map(&mut ppu, 0x400, 0, 0x2000 | (1 << 10) | 1);
        put_map(&mut ppu, 0xC00, 0, 0x2000 | 2);
        set_colour(&mut ppu, 16 + 1, 0x001F); // BG1: palette 1, colour 1
        set_colour(&mut ppu, 0x01, 0x7C00); // BG3: palette 0, colour 1
        ppu.write(0x2C, 0x05); // BG1 and BG3

        ppu.write(0x05, 0x01); // $2105 bit 3 clear: BG3.1 sits behind OBJ 1
        at_line(&mut ppu, 20);
        assert_eq!(
            row_of(&ppu, 20)[0],
            0x001F,
            "with $2105 bit 3 clear, BG1.1 is in front of BG3.1 (fullsnes \"Background Priority Chart\")"
        );

        ppu.write(0x05, 0x09); // $2105 bit 3 set: BG3.1a is frontmost
        at_line(&mut ppu, 21);
        assert_eq!(
            row_of(&ppu, 21)[0],
            0x7C00,
            "with $2105 bit 3 set, BG3.1a is the frontmost row of the chart"
        );
    }

    /// §5.3: a 16x16 BG tile's map entry covers a 16x16-pixel region, so the lookup steps the map
    /// every 16 pixels. Wiki `Tilemaps`: "selecting a 2x2 group of adjacent 8x8 pixel tiles for a
    /// 16x16 pixel region in the background ... This doubles the tilemap's size in each dimension
    /// without increasing memory usage", and "the tile index specifies the top left 8x8 tile from
    /// VRAM, and it will automatically include the adjacent tiles horizontally (+1) and vertically
    /// (+16, +17)".
    #[test]
    fn big_bg_tiles_index_the_map_every_sixteen_pixels() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x11); // mode 1, BG1 16x16
        map_base(&mut ppu, 0, 0x1000); // above the character data, so tiles and map do not overlap
        solid_tile(&mut ppu, 0, 0x10, 4, 1); // entry 0, top left
        solid_tile(&mut ppu, 0, 0x11, 4, 2); // entry 0, top right (N+1)
        solid_tile(&mut ppu, 0, 0x20, 4, 5); // entry 0, bottom left (N+10h)
        solid_tile(&mut ppu, 0, 0x40, 4, 3); // entry 1, top left
        for (index, colour) in [(1usize, 0x001Fu16), (2, 0x03E0), (3, 0x7C00), (5, 0x7FFF)] {
            set_colour(&mut ppu, index, colour);
        }
        put_map(&mut ppu, 0x1000, 0, 0x10);
        put_map(&mut ppu, 0x1000, 1, 0x40);
        ppu.write(0x2C, 0x01); // BG1 on the main screen

        at_line(&mut ppu, 1);
        let top = row_of(&ppu, 1);
        assert_eq!(
            (top[0], top[8], top[16]),
            (0x001F, 0x03E0, 0x7C00),
            "map entry 0 covers pixels 0-15 as tile N then N+1; entry 1 starts at pixel 16: \
             wiki \"Tilemaps\", 16x16 tile modes"
        );
        at_line(&mut ppu, 9);
        assert_eq!(
            row_of(&ppu, 9)[0],
            0x7FFF,
            "the same entry's bottom half is N+10h, one tile row below"
        );
    }

    /// A background repeats at the end of its own map. Wiki `Scrolling_a_large_map`: "The reason
    /// this works is because tilemaps wrap around - attempting to display data past the end of one
    /// will just loop back to the left or top of the map." A 64-wide layer is two 32x32 tilemaps
    /// combined "into a larger background" (wiki `Tilemaps`), so its end is 64 tiles = 512 pixels,
    /// and columns 64-95 must read screen 0 again.
    ///
    /// The old code reduced the tile index (`column & 31`) but chose the screen from the unreduced
    /// column, so at 512 pixels it re-read screen 1's first tile instead of the map's first.
    #[test]
    fn a_64_wide_layer_wraps_at_512_pixels() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x01); // mode 1
        ppu.write(0x07, 0x05); // BG1SC: base word 0x400, 64x32 (horizontal count = 2)
        solid_tile(&mut ppu, 0, 1, 4, 1);
        solid_tile(&mut ppu, 0, 2, 4, 2);
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 2, 0x03E0);
        fill_map(&mut ppu, 0x400, 1); // screen 0
        fill_map(&mut ppu, 0x800, 2); // screen 1
        ppu.write(0x2C, 0x01); // BG1 on the main screen

        // HOFS = 256: fetch columns 32-63, screen 1. Both readings agree here.
        ppu.write(0x0D, 0x00);
        ppu.write(0x0D, 0x01);
        at_line(&mut ppu, 20);
        assert_eq!(
            row_of(&ppu, 20)[0],
            0x03E0,
            "columns 32-63 are the second 32x32 screen"
        );

        // HOFS = 512: fetch columns 64-95. The wrap reading reduces them to 0-31, screen 0.
        ppu.write(0x0D, 0x00);
        ppu.write(0x0D, 0x02);
        at_line(&mut ppu, 21);
        assert_eq!(
            row_of(&ppu, 21)[0],
            0x001F,
            "at 512 pixels a 64-wide map should have repeated from its first tile"
        );
    }

    /// §5.3's direct colour: `CGWSEL` bit 0 turns a 256-colour BG's index into `BBb00 RRRr0 GGGg0`.
    #[test]
    fn mode3_256_colour_and_direct_colour() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x03); // mode 3
        map_base(&mut ppu, 0, 0x400);
        solid_tile(&mut ppu, 0, 1, 8, 0x5A);
        // Palette bits 10-12 = 5, the `bgr` low bits of the direct-colour word.
        put_map(&mut ppu, 0x400, 0, 5 << 10 | 1);
        set_colour(&mut ppu, 0x5A, 0x1234);
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 30);
        assert_eq!(
            row_of(&ppu, 30)[0],
            0x1234,
            "index $5A is CGRAM $5A with direct colour off"
        );

        ppu.write(0x30, 0x01); // CGWSEL bit 0: direct colour
        at_line(&mut ppu, 31);
        // $5A = BBGGGRRR = 01 011 010: red 2, green 3, blue 1; palette 5 = bgr 101 adds 1 to red
        // and to blue, so the word is (blue 12, green 12, red 10).
        let expected = (12u16 << 10) | (12 << 5) | 10;
        assert_eq!(
            row_of(&ppu, 31)[0],
            expected,
            "direct colour: fullsnes \"Direct Color Mode\" (BBb00 RRRr0 GGGg0)"
        );
    }

    /// §5.8's matrix: `M7A` scales the map horizontally in 1/256-pixel units.
    #[test]
    fn mode7_matrix_rotates_the_map() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x07);
        for tile in 0..4usize {
            mode7_tile(&mut ppu, tile, tile as u8 + 1);
        }
        for column in 0..128usize {
            // Map entry: the low byte of the word is the tile number (fullsnes "Mode 7").
            let word = column;
            ppu.vram[word * 2] = (column & 0xFF) as u8;
        }
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 2, 0x03E0);
        set_colour(&mut ppu, 3, 0x7C00);
        ppu.write(0x2C, 0x01);
        for (register, value) in [
            (0x1B, 0x0200u16), // M7A: 2.0
            (0x1C, 0x0000),    // M7B: 0
            (0x1D, 0x0000),    // M7C: 0
            (0x1E, 0x0000),    // M7D: 0, a flat map so every line samples tile row 0
        ] {
            ppu.write(register, value as u8);
            ppu.write(register, (value >> 8) as u8);
        }
        at_line(&mut ppu, 40);
        let row = row_of(&ppu, 40);
        assert_eq!(row[0], 0x001F, "screen x=0 samples column 0 -> tile 0");
        assert_eq!(
            row[4], 0x03E0,
            "screen x=4 samples column 8 -> tile 1 at 2.0x"
        );
        assert_eq!(
            row[10], 0x7C00,
            "screen x=10 samples column 20 -> tile 2 at 2.0x"
        );
    }

    /// §5.8's screen-over: `M7SEL` bits 7-6 wrap, go transparent, or fill with tile 0.
    #[test]
    fn mode7_screen_over_wraps_or_fills() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x07);
        for tile in 0..128usize {
            mode7_tile(&mut ppu, tile, (tile + 1) as u8);
        }
        for ty in 0..128usize {
            for tx in 0..128usize {
                ppu.vram[(ty * 128 + tx) * 2] = tx as u8;
            }
        }
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 128, 0x7FFF);
        set_colour(&mut ppu, 0, 0x0000);
        ppu.write(0x2C, 0x01);
        // M7A = -1.0: screen x=1 lands on vram X = -256, outside the map.
        ppu.write(0x1B, 0x00);
        ppu.write(0x1B, 0xFF);
        ppu.write(0x1E, 0x00);
        ppu.write(0x1E, 0x01);

        at_line(&mut ppu, 50);
        assert_eq!(
            row_of(&ppu, 50)[1],
            0x7FFF,
            "screen-over 0 wraps: X=-256 is tile column 127 (fullsnes \"Screen Over\")"
        );

        ppu.write(0x1A, 0x80); // screen-over 2: transparent
        at_line(&mut ppu, 51);
        assert_eq!(
            row_of(&ppu, 51)[1],
            0x0000,
            "screen-over 2 is transparent outside the map"
        );

        ppu.write(0x1A, 0xC0); // screen-over 3: fill with tile 0
        at_line(&mut ppu, 52);
        assert_eq!(
            row_of(&ppu, 52)[1],
            0x001F,
            "screen-over 3 fills with tile 0's colour"
        );
    }

    /// §5.4's chart, one discriminating pair per mode. The pair is the one that differs between the
    /// columns of fullsnes's "Background Priority Chart": BG2.1 against OBJ.2 in modes 0-5, BG1.0
    /// against OBJ.2 in modes 6-7, and BG3.1a against OBJ.3 in mode 1.
    #[test]
    fn priority_chart_orders_every_mode() {
        // (mode, $2105 value, the BG layer, its depth, its map entry, its CGRAM colour,
        //  the OBJ priority, whether the BG wins)
        type Case = (u8, u8, u8, u8, u16, usize, u8, bool);
        let cases: [Case; 8] = [
            (0, 0x00, 0x02, 2, 0x2000 | 2, 34, 2, true), // BG2 in mode 0
            (1, 0x09, 0x04, 2, 0x2000 | 2, 2, 3, true),  // BG3.1a with $2105 bit 3
            (2, 0x02, 0x02, 4, 0x2000 | 2, 2, 2, false), // BG2.1 below OBJ.2
            (3, 0x03, 0x02, 4, 0x2000 | 2, 2, 2, false),
            (4, 0x04, 0x02, 2, 0x2000 | 2, 2, 2, false),
            (5, 0x05, 0x02, 2, 0x2000 | 2, 2, 2, false),
            (6, 0x06, 0x01, 4, 2, 2, 2, false), // BG1.0 below OBJ.2
            (7, 0x07, 0x01, 8, 2, 2, 2, false), // mode 7's BG1 below OBJ.2
        ];
        for (index, &(mode, bgmode, layer, depth, entry, bg_colour, obj_priority, bg_wins)) in
            cases.iter().enumerate()
        {
            let mut ppu = new_ppu();
            ppu.write(0x05, bgmode);
            ppu.write(0x2C, layer | 0x10); // that BG plus OBJ
            if mode == 7 {
                ppu.write(0x01, 0x00);
                mode7_tile(&mut ppu, 0, 2);
                ppu.vram[0] = 0; // map entry (0,0) = tile 0
                for register in [0x1B, 0x1C, 0x1D, 0x1E] {
                    ppu.write(register, 0x00);
                    ppu.write(register, 0x01);
                }
            } else {
                map_base(&mut ppu, layer.trailing_zeros() as u8, 0x400);
                solid_tile(&mut ppu, 0, 1, depth, 2);
                put_map(&mut ppu, 0x400, 0, (entry & !0x03FF) | 1);
            }
            set_colour(&mut ppu, bg_colour, 0x001F);
            set_colour(&mut ppu, 128 + 2, 0x7C00);
            // A 16-colour OBJ tile at tile 8, clear of every BG's data in this scene.
            solid_tile(&mut ppu, 0, 8, 4, 2);
            // The OBJ's Y byte is the 0-based top row, so it has to cover the line under test.
            let sprite_row = (60 + index as u16 - 1) as u8;
            put_sprite(&mut ppu, 0, 0, sprite_row, 8, obj_priority << 4);
            at_line(&mut ppu, 60 + index as u16);
            let pixel = row_of(&ppu, 60 + index as u16)[0];
            let expected = if bg_wins { 0x001F } else { 0x7C00 };
            assert_eq!(
                pixel,
                expected,
                "mode {mode}: fullsnes \"Background Priority Chart\", column {}",
                mode + 1
            );
        }
    }

    /// §5.7: there is no main/sub priority condition on colour math. v1 carried one (skip the
    /// blend when the sub pixel sat in front of the main one) on fullsnes's sentence, which marks
    /// its own wording "XXX or is it same or lower -- or is it ANY priority?"; wiki `Color_math`
    /// lists the rejections as a closed set and says the masking choice "is made based on the main
    /// screen only". The removal was measured over 24 titles: 2 moved, 22 byte-identical.
    #[test]
    fn colour_math_has_no_priority_condition() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x01); // mode 1
        map_base(&mut ppu, 0, 0x400);
        map_base(&mut ppu, 1, 0x800);
        solid_tile(&mut ppu, 0, 1, 4, 1);
        solid_tile(&mut ppu, 0, 2, 4, 1);
        // Main BG1.0, sub BG2.1: the sub pixel is behind the main one, so math applies.
        put_map(&mut ppu, 0x400, 0, 1);
        put_map(&mut ppu, 0x800, 0, 0x2000 | (1 << 10) | 2);
        set_colour(&mut ppu, 1, 0x001F); // BG1 palette 0 colour 1: red
        set_colour(&mut ppu, 16 + 1, 0x7C00); // BG2 palette 1 colour 1: blue
        ppu.write(0x2C, 0x01); // TM: BG1
        ppu.write(0x2D, 0x02); // TS: BG2
        ppu.write(0x30, 0x02); // CGWSEL: sub screen layers on
        ppu.write(0x31, 0x41); // CGADSUB: add, half, BG1
        at_line(&mut ppu, 70);
        // (red + blue) / 2 = (31, 0, 31) / 2 per channel = 15, 0, 15.
        let expected = (15u16 << 10) | 15;
        assert_eq!(
            row_of(&ppu, 70)[0],
            expected,
            "BG2.1 behind BG1.0: the addition applies"
        );

        // Now the main is BG1.1 in palette 1 (blue) and the sub BG2.0 in palette 0 (red): the sub
        // sits ahead of the main, and the blend applies just the same. There is no main/sub
        // priority condition — `Color_math` lists the rejections as a closed set, and that
        // measurement is what removed v1's version of one (§5.7). Blue alone is 0x7C00, so the
        // two readings differ: 15375 only if the blend happens.
        put_map(&mut ppu, 0x400, 0, 0x2000 | (1 << 10) | 1);
        put_map(&mut ppu, 0x800, 0, 1);
        at_line(&mut ppu, 71);
        assert_eq!(
            row_of(&ppu, 71)[0],
            expected,
            "BG2.0 ahead of BG1.1 blends too: no priority condition"
        );
    }

    /// §5.7's operators: add, subtract, the half divide, force-black and the clamp.
    #[test]
    fn colour_math_half_and_force_black() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x01);
        map_base(&mut ppu, 0, 0x400);
        solid_tile(&mut ppu, 0, 1, 4, 1);
        put_map(&mut ppu, 0x400, 0, 1);
        set_colour(&mut ppu, 1, 10); // main: red 10
        ppu.write(0x2C, 0x01);
        ppu.write(0x2D, 0x01); // the sub screen shows the same layer: BG1 + BG1
        ppu.write(0x30, 0x02);
        ppu.write(0x32, 0x7F); // the fixed colour is not used here, but set it to white
        ppu.write(0x31, 0x01); // add, no half
        at_line(&mut ppu, 80);
        assert_eq!(
            row_of(&ppu, 80)[0],
            20,
            "BG1 + BG1 adds the same pixel twice"
        );

        ppu.write(0x31, 0x41); // add, half
        at_line(&mut ppu, 81);
        assert_eq!(
            row_of(&ppu, 81)[0],
            10,
            "the half divide brings it back to 10"
        );

        ppu.write(0x31, 0x81); // subtract, no half
        at_line(&mut ppu, 82);
        assert_eq!(
            row_of(&ppu, 82)[0],
            0,
            "subtract BG1 from itself and the clamp stops at 0"
        );

        // Force black: the main goes black, and addition still applies without the half divide.
        ppu.write(0x30, 0xC2); // force black always, sub layers on
        ppu.write(0x31, 0x41); // add, half (ignored under force black)
        at_line(&mut ppu, 83);
        assert_eq!(
            row_of(&ppu, 83)[0],
            10,
            "force black + addition = the sub colour, and the half divide is ignored"
        );
        ppu.write(0x30, 0x02);
    }

    /// §5.6: the window's inside/outside selectors, the OR/AND/XOR/XNOR combine and `TMW`.
    #[test]
    fn window_mask_logic_and_masking() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x00);
        map_base(&mut ppu, 0, 0x400);
        solid_tile(&mut ppu, 0, 1, 2, 1);
        fill_map(&mut ppu, 0x400, 1);
        set_colour(&mut ppu, 1, 0x7FFF);
        ppu.write(0x2C, 0x01);
        ppu.write(0x2E, 0x01); // TMW: disable BG1 inside its window
        ppu.write(0x26, 10); // WH0
        ppu.write(0x27, 20); // WH1
        ppu.write(0x23, 0x02); // W12SEL: BG1 window 1 enabled, not inverted ($02 = enable:invert 10)
        at_line(&mut ppu, 90);
        let row = row_of(&ppu, 90);
        assert_eq!(row[9], 0x7FFF, "outside the window the layer is drawn");
        assert_eq!(
            row[10], 0,
            "inside the window TMW forces BG1 off (edges included)"
        );
        assert_eq!(row[20], 0);
        assert_eq!(row[21], 0x7FFF, "the window ends at WH1");

        // Both windows on, AND: only the overlap is masked. Window 2 covers 15-25.
        ppu.write(0x28, 15);
        ppu.write(0x29, 25);
        ppu.write(0x23, 0x0A); // BG1: window 1 and window 2, both enabled and not inverted
        ppu.write(0x2A, 0x01); // WBGLOG: BG1 = AND
        at_line(&mut ppu, 91);
        let row = row_of(&ppu, 91);
        assert_eq!(row[12], 0x7FFF, "12 is only in window 1");
        assert_eq!(row[17], 0, "17 is in both windows");
        assert_eq!(row[22], 0x7FFF, "22 is only in window 2");

        // XOR: inside exactly one of the two.
        ppu.write(0x2A, 0x02);
        at_line(&mut ppu, 92);
        let row = row_of(&ppu, 92);
        assert_eq!(row[12], 0, "12 is in window 1 only, so XOR masks it");
        assert_eq!(row[17], 0x7FFF, "17 is in both, so XOR leaves it alone");

        // XNOR: 1 XOR area1 XOR area2.
        ppu.write(0x2A, 0x03);
        at_line(&mut ppu, 93);
        let row = row_of(&ppu, 93);
        assert_eq!(row[17], 0, "XNOR masks the overlap");
        assert_eq!(row[5], 0, "and it masks outside both windows too");
    }

    /// One 2-bit field is `enable:invert` (wiki `PPU_registers`, W12SEL/W34SEL/WOBJSEL):
    ///
    /// ```text
    /// 7  bit  0
    /// ---- ----
    /// DdCc BbAa
    /// |||| ||||
    /// |||| |||+- Invert window 1 for BG1
    /// |||| ||+-- Enable window 1 for BG1
    /// ```
    ///
    /// so `2` (`10`) is enabled not inverted, `3` (`11`) is enabled and inverted (the area is the
    /// complement of WH0..WH1), and `0`/`1` are not enabled — wiki `Windows`: "If the window is
    /// disabled then invert has no effect (all 0 output)." `TMW` here hides the layer wherever the
    /// final window outputs 1, so a mask shows as the backdrop (0) and a drawn pixel as `colour`.
    ///
    /// `layer` is 0-3 (BG1-BG4) in mode 0; `register`/`shift` name the field; window 1 is 10..=20
    /// and window 2 is 30..=40, so the caller passes one x inside the field's window and one out.
    fn assert_bg_window_field(
        name: &str,
        layer: u8,
        register: u8,
        shift: u8,
        inside_x: usize,
        outside_x: usize,
    ) {
        let render = |selector: u8| {
            let mut ppu = new_ppu();
            ppu.write(0x05, 0x00); // mode 0: four 4-colour BGs
            map_base(&mut ppu, layer, 0x400);
            solid_tile(&mut ppu, 0, 1, 2, 1);
            fill_map(&mut ppu, 0x400, 1);
            // Mode 0's CGRAM bases: BG1 0x01, BG2 0x21, BG3 0x41, BG4 0x61 (fullsnes "CGRAM
            // Palette Indices").
            let palette = [0x01usize, 0x21, 0x41, 0x61][usize::from(layer)];
            set_colour(&mut ppu, palette, 0x7FFF);
            ppu.write(0x2C, 1 << layer); // TM: this layer only, main screen
            ppu.write(0x2E, 1 << layer); // TMW: window masking on for it
            ppu.write(0x26, 10);
            ppu.write(0x27, 20);
            ppu.write(0x28, 30);
            ppu.write(0x29, 40);
            ppu.write(register, selector << shift);
            at_line(&mut ppu, 60);
            row_of(&ppu, 60)
        };
        let row = render(2);
        assert_eq!(
            row[outside_x], 0x7FFF,
            "{name} = 2 (enabled): outside the window the layer is drawn"
        );
        assert_eq!(
            row[inside_x], 0,
            "{name} = 2 (enabled): inside the window TMW hides the layer"
        );
        let row = render(3);
        assert_eq!(
            row[outside_x], 0,
            "{name} = 3 (enabled + inverted): the area is the complement, hidden outside"
        );
        assert_eq!(
            row[inside_x], 0x7FFF,
            "{name} = 3 (enabled + inverted): drawn inside"
        );
        for off in [0u8, 1] {
            let row = render(off);
            assert_eq!(
                row[outside_x], 0x7FFF,
                "{name} = {off}: not enabled, so there is no window (wiki Windows: invert has no \
                 effect when disabled)"
            );
            assert_eq!(
                row[inside_x], 0x7FFF,
                "{name} = {off}: the layer is drawn everywhere"
            );
        }
    }

    /// `WOBJSEL`'s two OBJ fields the same way: a sprite at x=0..=7, window 1 at 0..=7, `TMW` bit 4.
    fn assert_obj_window_field(name: &str, register: u8, shift: u8) {
        let render = |selector: u8| {
            let mut ppu = new_ppu();
            ppu.write(0x05, 0x00);
            map_base(&mut ppu, 0, 0x400);
            solid_tile(&mut ppu, 0, 1, 2, 1);
            fill_map(&mut ppu, 0x400, 1);
            set_colour(&mut ppu, 0x01, 0x7FFF); // BG1, behind the sprite
            solid_tile(&mut ppu, 0, 8, 4, 2);
            set_colour(&mut ppu, 128 + 2, 0x001F); // OBJ palette 0, colour 2
            put_sprite(&mut ppu, 0, 0, 59, 8, 0x30); // y byte 59 draws line 60, priority 3
            ppu.write(0x2C, 0x11); // TM: BG1 and OBJ
            ppu.write(0x2E, 0x10); // TMW: mask OBJ
            ppu.write(0x26, 0);
            ppu.write(0x27, 7);
            ppu.write(0x28, 0);
            ppu.write(0x29, 7);
            ppu.write(register, selector << shift);
            at_line(&mut ppu, 60);
            row_of(&ppu, 60)
        };
        // The sprite only covers the window, so "masked" means BG1 shows at x=0 and "drawn" means
        // the sprite's colour does.
        assert_eq!(render(2)[0], 0x7FFF, "{name} = 2: TMW hides the OBJ inside");
        assert_eq!(
            render(3)[0],
            0x001F,
            "{name} = 3: inverted, the OBJ shows inside"
        );
        for off in [0u8, 1] {
            assert_eq!(
                render(off)[0],
                0x001F,
                "{name} = {off}: no window, so the OBJ is drawn"
            );
        }
    }

    /// `WOBJSEL`'s two colour fields: `CGWSEL` bits 5-4 = 1 applies math inside the math window
    /// only, and the fixed colour is added to a BG1 of 10, so 20 marks a mathed pixel.
    fn assert_colour_window_field(name: &str, shift: u8, inside_x: usize, outside_x: usize) {
        let render = |selector: u8| {
            let mut ppu = new_ppu();
            ppu.write(0x05, 0x00);
            map_base(&mut ppu, 0, 0x400);
            solid_tile(&mut ppu, 0, 1, 2, 1);
            fill_map(&mut ppu, 0x400, 1);
            set_colour(&mut ppu, 0x01, 10);
            ppu.write(0x2C, 0x01); // TM: BG1
            ppu.write(0x30, 0x10); // CGWSEL: math inside the math window, fixed colour
            ppu.write(0x31, 0x01); // CGADSUB: add, no half, BG1
            ppu.write(0x32, 0x2A); // COLDATA: red only, intensity 10
            ppu.write(0x26, 10);
            ppu.write(0x27, 20);
            ppu.write(0x28, 30);
            ppu.write(0x29, 40);
            ppu.write(0x25, selector << shift);
            at_line(&mut ppu, 60);
            row_of(&ppu, 60)
        };
        let row = render(2);
        assert_eq!(
            row[inside_x], 20,
            "{name} = 2 (enabled): math inside the window"
        );
        assert_eq!(row[outside_x], 10, "{name} = 2: no math outside it");
        let row = render(3);
        assert_eq!(
            row[inside_x], 10,
            "{name} = 3 (enabled + inverted): no math inside"
        );
        assert_eq!(row[outside_x], 20, "{name} = 3: math outside");
        for off in [0u8, 1] {
            let row = render(off);
            assert_eq!(
                row[inside_x], 10,
                "{name} = {off}: no window, so everything is outside it and math is off (wiki \
                 PPU_registers, CGWSEL)"
            );
            assert_eq!(row[outside_x], 10, "{name} = {off}: no math outside either");
        }
    }

    /// `W12SEL` bits 0-1: BG1, window 1.
    #[test]
    fn window_bg1_window_1_decodes_enable_invert() {
        assert_bg_window_field("W12SEL BG1 window 1", 0, 0x23, 0, 15, 5);
    }

    /// `W12SEL` bits 2-3: BG1, window 2.
    #[test]
    fn window_bg1_window_2_decodes_enable_invert() {
        assert_bg_window_field("W12SEL BG1 window 2", 0, 0x23, 2, 35, 25);
    }

    /// `W12SEL` bits 4-5: BG2, window 1.
    #[test]
    fn window_bg2_window_1_decodes_enable_invert() {
        assert_bg_window_field("W12SEL BG2 window 1", 1, 0x23, 4, 15, 5);
    }

    /// `W12SEL` bits 6-7: BG2, window 2.
    #[test]
    fn window_bg2_window_2_decodes_enable_invert() {
        assert_bg_window_field("W12SEL BG2 window 2", 1, 0x23, 6, 35, 25);
    }

    /// `W34SEL` bits 0-1: BG3, window 1.
    #[test]
    fn window_bg3_window_1_decodes_enable_invert() {
        assert_bg_window_field("W34SEL BG3 window 1", 2, 0x24, 0, 15, 5);
    }

    /// `W34SEL` bits 2-3: BG3, window 2.
    #[test]
    fn window_bg3_window_2_decodes_enable_invert() {
        assert_bg_window_field("W34SEL BG3 window 2", 2, 0x24, 2, 35, 25);
    }

    /// `W34SEL` bits 4-5: BG4, window 1.
    #[test]
    fn window_bg4_window_1_decodes_enable_invert() {
        assert_bg_window_field("W34SEL BG4 window 1", 3, 0x24, 4, 15, 5);
    }

    /// `W34SEL` bits 6-7: BG4, window 2.
    #[test]
    fn window_bg4_window_2_decodes_enable_invert() {
        assert_bg_window_field("W34SEL BG4 window 2", 3, 0x24, 6, 35, 25);
    }

    /// `WOBJSEL` bits 0-1: OBJ, window 1.
    #[test]
    fn window_obj_window_1_decodes_enable_invert() {
        assert_obj_window_field("WOBJSEL OBJ window 1", 0x25, 0);
    }

    /// `WOBJSEL` bits 2-3: OBJ, window 2.
    #[test]
    fn window_obj_window_2_decodes_enable_invert() {
        assert_obj_window_field("WOBJSEL OBJ window 2", 0x25, 2);
    }

    /// `WOBJSEL` bits 4-5: the colour window, window 1.
    #[test]
    fn window_colour_window_1_decodes_enable_invert() {
        assert_colour_window_field("WOBJSEL colour window 1", 4, 15, 5);
    }

    /// `WOBJSEL` bits 6-7: the colour window, window 2.
    #[test]
    fn window_colour_window_2_decodes_enable_invert() {
        assert_colour_window_field("WOBJSEL colour window 2", 6, 35, 25);
    }

    /// §5.10's `UNVERIFIED:` row: the first mosaic block is anchored at the screen edge, so a scroll
    /// does not move it.
    #[test]
    fn mosaic_anchor_is_the_screen_edge() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x01); // mode 1: BG1 is 16-colour, so eight column colours fit one tile
        map_base(&mut ppu, 0, 0x400);
        column_tile(&mut ppu, 0, 1, 4, &[1, 2, 3, 4, 5, 6, 7, 8]);
        put_map(&mut ppu, 0x400, 0, 1);
        for colour in 1..=8usize {
            set_colour(&mut ppu, colour, colour as u16);
        }
        ppu.write(0x2C, 0x01);
        ppu.write(0x06, 0x31); // size 4 (N=3), BG1 enable
        at_line(&mut ppu, 100);
        let row = row_of(&ppu, 100);
        assert_eq!(row[0], 1, "the block's first pixel is repeated");
        assert_eq!(row[3], 1);
        assert_eq!(row[4], 5, "the next block starts at screen x=4");
        assert_eq!(row[7], 5);

        // Scroll BG1 by one: the block is still anchored at the screen's left edge.
        ppu.write(0x0D, 0x01);
        ppu.write(0x0D, 0x00);
        at_line(&mut ppu, 101);
        let row = row_of(&ppu, 101);
        assert_eq!(
            row[0], 2,
            "the block still starts at x=0, now showing source column 1"
        );
        assert_eq!(row[3], 2);
        assert_eq!(
            row[4], 6,
            "and the next block starts at x=4, source column 5"
        );
    }

    /// §5.10's offset-per-tile, modes 2, 4 and 6: BG1/BG2 shift by the BG3 entry's 10-bit offset,
    /// horizontal by default and vertical with mode 4's bit 15; the low 3 bits of an H offset are
    /// ignored.
    #[test]
    fn offset_per_tile_horizontal_vertical_and_modes() {
        // Mode 2, horizontal: the offset moves BG1's column 0 to source column 1.
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x02);
        map_base(&mut ppu, 0, 0x400);
        map_base(&mut ppu, 2, 0xC00);
        solid_tile(&mut ppu, 0, 1, 4, 1);
        solid_tile(&mut ppu, 0, 2, 4, 2);
        put_map(&mut ppu, 0x400, 0, 1);
        put_map(&mut ppu, 0x400, 1, 2);
        // BG3's entry: enable BG1 (bit 13), offset 8 with the low 3 bits set to 7 to prove they are
        // ignored, so the effective offset is 8.
        put_map(&mut ppu, 0xC00, 0, 0x2000 | 8 | 7);
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 2, 0x03E0);
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 110);
        assert_eq!(
            row_of(&ppu, 110)[0],
            0x03E0,
            "BG1's first column shifts by 8 pixels: fullsnes \"Offset-per-Tile\""
        );
        // Column 1 has no offset, so screen x=8 is source x=8 and still colour 2.
        put_map(&mut ppu, 0xC00, 1, 0);
        at_line(&mut ppu, 111);
        assert_eq!(
            row_of(&ppu, 111)[8],
            0x03E0,
            "the next column carries no offset"
        );

        // Mode 4, vertical: bit 15 makes it a V offset on the same 256-colour BG1.
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x04);
        map_base(&mut ppu, 0, 0x400);
        map_base(&mut ppu, 2, 0xC00);
        for row in 0..8usize {
            let colour = row as u8 + 1; // row `r` is drawn in colour r+1
            for plane in 0..8usize {
                let byte = if (colour >> plane) & 1 != 0 { 0xFF } else { 0 };
                let offset = 64 + row * 2 + (plane >> 1) * 16 + (plane & 1);
                ppu.vram[offset] = byte;
            }
        }
        put_map(&mut ppu, 0x400, 0, 1);
        put_map(&mut ppu, 0xC00, 0, 0x8000 | 0x2000 | 2);
        for colour in 1..=8usize {
            set_colour(&mut ppu, colour, colour as u16);
        }
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 113); // screen row 112 is 0 mod 8, so the V offset shows cleanly
        assert_eq!(
            row_of(&ppu, 113)[0],
            3,
            "mode 4 bit 15 is a vertical offset: screen row 112 reads tile row 114 & 7 = 2"
        );

        // Mode 6 uses bit 13 for BG1, as the only BG.
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x06);
        map_base(&mut ppu, 0, 0x400);
        map_base(&mut ppu, 2, 0xC00);
        solid_tile(&mut ppu, 0, 1, 4, 1);
        solid_tile(&mut ppu, 0, 2, 4, 2);
        put_map(&mut ppu, 0x400, 0, 1);
        put_map(&mut ppu, 0x400, 1, 2);
        put_map(&mut ppu, 0xC00, 0, 0x2000 | 8);
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 2, 0x03E0);
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 113);
        assert_eq!(
            row_of(&ppu, 113)[0],
            0x03E0,
            "mode 6 has only BG1, and bit 13 enables its offset (the UNVERIFIED row of §5.11)"
        );
    }

    /// Modes 5 and 6 have no 8x8 tile size: with `BGMODE`'s size bit clear they are **16x8**, and
    /// the bit then selects 16x16 instead. fullsnes "Backgrounds": "The 8x8 pixel tilemap tile size
    /// selectable via BGMODE is replaced with a 16x8 pixel tile mode instead. The 16x16 mode is
    /// still 16x16." So one map entry covers 16 pixels across and 8 down: the left and right halves
    /// are that entry's tile `N` and its neighbour `N+1` (not the next map entry), while the map
    /// still advances one row every 8 lines (there is no `N+0x10` sub-tile).
    ///
    /// Before the fix `fetch` stepped the map every 8 pixels whenever the size bit was clear, so
    /// the second half-tile came from map entry 1 — the doubled tilemap.
    #[test]
    fn mode5_tiles_are_sixteen_by_eight_when_the_size_bit_is_clear() {
        /// One map entry at an explicit `(column, row)`; `put_map` fills a whole column instead.
        fn put_entry(ppu: &mut Ppu, map_word: usize, column: usize, row: usize, entry: u16) {
            let word = (map_word + row * 32 + column) & 0x7FFF;
            ppu.vram[word * 2] = entry as u8;
            ppu.vram[word * 2 + 1] = (entry >> 8) as u8;
        }

        let mut ppu = new_ppu();
        ppu.write(0x05, 0x05); // mode 5, BG1's size bit ($2105 bit 4) clear
        map_base(&mut ppu, 0, 0x1000); // above the character data, so tiles and map do not overlap
        solid_tile(&mut ppu, 0, 0x10, 4, 1); // map entry 0, left half
        solid_tile(&mut ppu, 0, 0x11, 4, 2); // map entry 0, right half (N+1)
        solid_tile(&mut ppu, 0, 0x20, 4, 4); // map entry 0 + one tile row: 16x16's bottom half only
        solid_tile(&mut ppu, 0, 0x40, 4, 3); // map entry 1, left half
        solid_tile(&mut ppu, 0, 0x41, 4, 5); // map entry 1, right half
        solid_tile(&mut ppu, 0, 0x60, 4, 6); // map row 1, column 0: the next 16x8 row
        for (index, colour) in [
            (1usize, 0x001Fu16),
            (2, 0x03E0),
            (3, 0x7C00),
            (4, 0x7FFF),
            (5, 0x0003),
            (6, 0x7FE0),
        ] {
            set_colour(&mut ppu, index, colour);
        }
        put_entry(&mut ppu, 0x1000, 0, 0, 0x10);
        put_entry(&mut ppu, 0x1000, 1, 0, 0x40);
        put_entry(&mut ppu, 0x1000, 0, 1, 0x60);
        ppu.write(0x2C, 0x01); // BG1 on the main screen

        // Output pixel x is source pixel 2x (the hi-res half-resolution rule of §5.9), so output
        // 0, 4, 8 and 12 sample source columns 0, 8, 16 and 24: the two halves of map entry 0 and
        // then the two halves of map entry 1.
        at_line(&mut ppu, 1);
        let top = row_of(&ppu, 1);
        assert_eq!(
            (top[0], top[4], top[8], top[12]),
            (0x001F, 0x03E0, 0x7C00, 0x0003),
            "mode 5 with the size bit clear is 16x8: pixels 0-7 are entry 0's N and N+1, and pixels \
             8-15 start map entry 1 — fullsnes \"Backgrounds\", \"replaced with a 16x8 pixel tile mode\""
        );
        // 16x8, not 16x16: line 9 is the next map row, so it reads entry $60, not entry 0's
        // bottom half ($20).
        at_line(&mut ppu, 9);
        assert_eq!(
            row_of(&ppu, 9)[0],
            0x7FE0,
            "a 16x8 entry covers 8 lines, so line 9 reads map row 1 — there is no N+0x10 sub-tile"
        );
    }

    /// §5.9's `UNVERIFIED:` row: modes 5 and 6 render the main screen's even dot, so a one-pixel
    /// checkerboard collapses to the even columns.
    #[test]
    fn mode5_renders_at_half_horizontal_resolution() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x05); // mode 5
        map_base(&mut ppu, 0, 0x400);
        // A mode-5 entry is 16x8, so the tilemap must show the same checkerboard in
        // both halves: tile 1 is the entry's N and tile 2 its N+1.
        column_tile(&mut ppu, 0, 1, 4, &[1, 2, 1, 2, 1, 2, 1, 2]);
        column_tile(&mut ppu, 0, 2, 4, &[1, 2, 1, 2, 1, 2, 1, 2]);
        fill_map(&mut ppu, 0x400, 1);
        set_colour(&mut ppu, 1, 0x001F);
        set_colour(&mut ppu, 2, 0x03E0);
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 120);
        let row = row_of(&ppu, 120);
        assert_eq!(row.len(), 256);
        assert!(
            row.iter().all(|&pixel| pixel == 0x001F),
            "every output pixel is the even dot of its pair: odd source columns never appear"
        );
    }

    /// §5.1 and §5.11: a register written during the visible dots applies from the next line.
    #[test]
    fn tm_change_mid_line_takes_effect_next_line() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x00);
        map_base(&mut ppu, 0, 0x400);
        solid_tile(&mut ppu, 0, 1, 2, 1);
        put_map(&mut ppu, 0x400, 0, 1);
        set_colour(&mut ppu, 1, 0x7FFF);
        ppu.write(0x2C, 0x01);
        at_line(&mut ppu, 130);
        assert_eq!(row_of(&ppu, 130)[0], 0x7FFF);
        // Dot 150 is inside the visible dots (22-277).
        run_to(&mut ppu, 131, 150);
        ppu.write(0x2C, 0x00);
        at_line(&mut ppu, 132);
        assert_eq!(
            row_of(&ppu, 131)[0],
            0x7FFF,
            "line 131 keeps the old TM: the write lands on the next line boundary"
        );
        assert_eq!(row_of(&ppu, 132)[0], 0, "line 132 takes the new TM");
        let _ = DOTS;
    }

    /// §5.11's `UNVERIFIED:` row: drawing mode 7 never touches `$2134`-`$2136`'s product.
    #[test]
    fn mode7_leaves_the_multiply_registers_alone() {
        let mut ppu = new_ppu();
        ppu.write(0x05, 0x07);
        mode7_tile(&mut ppu, 0, 1);
        set_colour(&mut ppu, 1, 0x001F);
        ppu.write(0x2C, 0x01);
        ppu.write(0x1B, 0x00);
        ppu.write(0x1B, 0x02); // M7A = $0200
        ppu.write(0x1C, 0x03); // M7B byte = 3
        let pinned = ppu.m7_product;
        assert_eq!(pinned, 0x0200 * 3);
        for line in 121..=126 {
            at_line(&mut ppu, line);
        }
        assert_eq!(
            ppu.m7_product, pinned,
            "drawing mode 7 must not refresh PPU1's multiplier"
        );
        assert_eq!(ppu.read(0x34), Some(0x00));
        assert_eq!(ppu.read(0x35), Some(0x06), "MPYM of 1536");
    }
}
