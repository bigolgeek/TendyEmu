//! `tests/sprites.rs` — per-line evaluation, the two overflow flags, the index tie-break, the
//! priority rotation, flips, palettes, tall objects and the Y boundary.

use tendy_snes::ppu::Ppu;

/// Runs into `line`'s first dot, where the line is rendered.
fn at_line(ppu: &mut Ppu, line: u16) {
    let mut guard = 0u32;
    while !(ppu.line == line && ppu.dot == 1) {
        ppu.tick(4);
        guard += 1;
        assert!(guard < 341 * 262 * 2, "at_line({line}) never arrived");
    }
}

/// The finished frame row for screen line `line` (1..=224).
fn row_of(ppu: &Ppu, line: u16) -> Vec<u16> {
    let start = (usize::from(line) - 1) * 256;
    ppu.frame[start..start + 256].to_vec()
}

/// A PPU showing OBJs only: mode 0, `TM` = OBJ, brightness 15, 8x8/16x16 sizes, tile base 0.
fn sprite_ppu(obsel: u8) -> Ppu {
    let mut ppu = Ppu::new();
    ppu.write(0x00, 0x0F);
    ppu.write(0x01, obsel);
    ppu.write(0x05, 0x00);
    ppu.write(0x2C, 0x10);
    ppu
}

/// One 16-colour OBJ tile whose every pixel is `colour`.
fn solid_obj_tile(ppu: &mut Ppu, tile: usize, colour: u8) {
    let start = tile * 32;
    for plane in 0..4usize {
        let byte = if (colour >> plane) & 1 != 0 { 0xFF } else { 0 };
        for row in 0..8 {
            let offset = start + row * 2 + (plane >> 1) * 16 + (plane & 1);
            ppu.vram[offset & 0x7FFF] = byte;
        }
    }
}

/// One 16-colour OBJ tile whose colour depends on the pixel: `f(x, y)`.
fn obj_tile(ppu: &mut Ppu, tile: usize, f: impl Fn(usize, usize) -> u8) {
    let start = tile * 32;
    for row in 0..8usize {
        for column in 0..8usize {
            let colour = f(column, row);
            for plane in 0..4usize {
                let offset = start + row * 2 + (plane >> 1) * 16 + (plane & 1);
                let mask = 0x80u8 >> column;
                if (colour >> plane) & 1 != 0 {
                    ppu.vram[offset & 0x7FFF] |= mask;
                } else {
                    ppu.vram[offset & 0x7FFF] &= !mask;
                }
            }
        }
    }
}

/// One OAM entry, with the high table's X bit 8 and the size select.
fn put_sprite(ppu: &mut Ppu, index: usize, x: u16, y: u8, tile: u8, attr: u8, large: bool) {
    ppu.oam[index * 4] = x as u8;
    ppu.oam[index * 4 + 1] = y;
    ppu.oam[index * 4 + 2] = tile;
    ppu.oam[index * 4 + 3] = attr;
    let shift = (index % 4) * 2;
    let bits = ((x >> 8) as u8 & 0x01) | (u8::from(large) << 1);
    let byte = &mut ppu.oam[0x200 + index / 4];
    *byte = (*byte & !(0x03 << shift)) | (bits << shift);
}

/// A CGRAM entry in the OBJ palettes: palette 0, colour `index`.
fn obj_colour(ppu: &mut Ppu, index: usize, colour: u16) {
    ppu.cgram[128 + index] = colour;
}

mod sprites {
    use super::*;

    /// §5.5's per-line limit: at most 32 OBJs, and the 33rd raises `$213E` bit 6 without stopping
    /// the line.
    #[test]
    fn oam_evaluation_limits_32_per_line() {
        let mut ppu = sprite_ppu(0x00);
        solid_obj_tile(&mut ppu, 1, 1);
        obj_colour(&mut ppu, 1, 0x001F);
        for index in 0..32 {
            put_sprite(&mut ppu, index, 0, 20, 1, 0x00, false);
        }
        // The 33rd OBJ sits on its own column; it must not be drawn at all.
        put_sprite(&mut ppu, 32, 100, 20, 1, 0x00, false);
        at_line(&mut ppu, 25);
        let row = row_of(&ppu, 25);
        assert_eq!(row[0], 0x001F, "the first 32 OBJs are drawn");
        assert_eq!(
            row[100], 0,
            "a 33rd OBJ on the same line is dropped: fullsnes \"$213E\", 32 OBJs a scanline"
        );
        assert!(
            ppu.obj_range_overflow,
            "$213E bit 6 rises for more than 32 OBJs"
        );
        assert!(
            !ppu.obj_time_overflow,
            "32 OBJs of one tile each are 32 tiles, under 34"
        );
        assert_eq!(
            ppu.read(0x3E),
            Some(0x41),
            "STAT77 reports the range overflow"
        );
    }

    /// `$213E`'s other flag: more than 34 8-pixel OBJ tiles on a line.
    #[test]
    fn obj_overflow_flags() {
        // Nine 32x32 OBJs are 36 tiles: two more than the hardware fetches.
        let mut ppu = sprite_ppu(5 << 5); // OBSEL: small = 32x32
        solid_obj_tile(&mut ppu, 1, 1);
        obj_colour(&mut ppu, 1, 0x001F);
        for index in 0..9 {
            put_sprite(&mut ppu, index, (index as u16) * 24, 20, 1, 0x00, false);
        }
        at_line(&mut ppu, 25);
        assert!(ppu.obj_time_overflow, "$213E bit 7 rises past 34 OBJ tiles");
        assert!(
            !ppu.obj_range_overflow,
            "nine OBJs are under the count limit"
        );
        assert_eq!(
            ppu.read(0x3E),
            Some(0x81),
            "STAT77 reports the time overflow"
        );

        // The flags clear at V=0 and are recomputed per line. Everything else in OAM
        // is parked at Y = $FF so no zeroed entry covers the line.
        for index in 0..128 {
            ppu.oam[index * 4 + 1] = 0xFF;
        }
        at_line(&mut ppu, 3);
        let after = ppu.read(0x3E).expect("STAT77 is readable");
        assert_eq!(after & 0x80, 0, "the time flag, cleared at V=0");
    }

    /// §5.11's `UNVERIFIED:` row: at equal priority the lower OAM index is in front.
    #[test]
    fn equal_priority_lower_index_is_in_front() {
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |_, _| 1);
        obj_tile(&mut ppu, 2, |_, _| 2);
        obj_colour(&mut ppu, 1, 0x001F);
        obj_colour(&mut ppu, 2, 0x03E0);
        // Both 8x8, same X and Y, same priority 1.
        put_sprite(&mut ppu, 0, 0, 20, 1, 0x10, false);
        put_sprite(&mut ppu, 1, 0, 20, 2, 0x10, false);
        at_line(&mut ppu, 25);
        assert_eq!(
            row_of(&ppu, 25)[0],
            0x001F,
            "OBJ 0 is in front of OBJ 1 at equal priority"
        );

        // Swap the two entries: the lower index still wins, and it is now the other colour.
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |_, _| 1);
        obj_tile(&mut ppu, 2, |_, _| 2);
        obj_colour(&mut ppu, 1, 0x001F);
        obj_colour(&mut ppu, 2, 0x03E0);
        put_sprite(&mut ppu, 0, 0, 20, 2, 0x10, false);
        put_sprite(&mut ppu, 1, 0, 20, 1, 0x10, false);
        at_line(&mut ppu, 25);
        assert_eq!(
            row_of(&ppu, 25)[0],
            0x03E0,
            "the tie-break is the index, not the tile"
        );
    }

    /// `$2103` bit 7: OBJ *N* becomes the highest-priority one, shifting the order by *N*.
    #[test]
    fn priority_rotation_changes_the_order() {
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |_, _| 1);
        obj_tile(&mut ppu, 2, |_, _| 2);
        obj_colour(&mut ppu, 1, 0x001F);
        obj_colour(&mut ppu, 2, 0x03E0);
        put_sprite(&mut ppu, 0, 0, 20, 1, 0x10, false);
        put_sprite(&mut ppu, 1, 0, 20, 2, 0x10, false);
        at_line(&mut ppu, 25);
        assert_eq!(
            row_of(&ppu, 25)[0],
            0x001F,
            "without rotation, OBJ 0 is in front"
        );

        // Rotation with OBJ 1 as the highest priority: the address's bit 1 is the OBJ number.
        ppu.write(0x02, 0x02);
        ppu.write(0x03, 0x80);
        at_line(&mut ppu, 26);
        assert_eq!(
            row_of(&ppu, 26)[0],
            0x03E0,
            "$2103 bit 7 makes OBJ N the highest priority: fullsnes \"2102h/2103h\""
        );
    }

    /// §5.5's attribute bits: the two flips and the palette field.
    #[test]
    fn flips_and_palette_bits() {
        // Column 0 is colour 1, the rest colour 2; row 0 is colour 3 on the second tile.
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |column, _| if column == 0 { 1 } else { 2 });
        obj_tile(&mut ppu, 2, |_, row| if row == 0 { 3 } else { 2 });
        obj_colour(&mut ppu, 1, 0x001F);
        obj_colour(&mut ppu, 2, 0x03E0);
        obj_colour(&mut ppu, 3, 0x7C00);
        put_sprite(&mut ppu, 0, 0, 20, 1, 0x00, false);
        at_line(&mut ppu, 25);
        let row = row_of(&ppu, 25);
        assert_eq!(
            (row[0], row[1]),
            (0x001F, 0x03E0),
            "no flip: column 0 first"
        );

        // X flip (attribute bit 6): the marked column moves to the right edge.
        put_sprite(&mut ppu, 0, 0, 20, 1, 0x40, false);
        at_line(&mut ppu, 26);
        let row = row_of(&ppu, 26);
        assert_eq!(
            (row[0], row[7]),
            (0x03E0, 0x001F),
            "X flip mirrors the tile"
        );

        // Y flip (bit 7) on the two-row tile: the marked row moves to the bottom.
        put_sprite(&mut ppu, 0, 0, 20, 2, 0x00, false);
        at_line(&mut ppu, 21);
        let straight = row_of(&ppu, 21);
        put_sprite(&mut ppu, 0, 0, 20, 2, 0x80, false);
        at_line(&mut ppu, 22);
        let flipped = row_of(&ppu, 22);
        assert_eq!(
            straight[0], 0x7C00,
            "row 0 is the marked row without a flip"
        );
        at_line(&mut ppu, 28);
        assert_eq!(
            row_of(&ppu, 28)[0],
            0x7C00,
            "with Y flip the marked row is the OBJ's last line"
        );
        assert_eq!(flipped[0], 0x03E0, "and its first line is the old row 7");

        // Palette bits 3-1 index CGRAM 128+page*16; page 5 for attribute $0A.
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |_, _| 1);
        ppu.cgram[128 + 5 * 16 + 1] = 0x7FFF;
        put_sprite(&mut ppu, 0, 0, 20, 1, 0x0A, false);
        at_line(&mut ppu, 25);
        assert_eq!(
            row_of(&ppu, 25)[0],
            0x7FFF,
            "attribute bits 3-1 are the OBJ palette: fullsnes \"SNES PPU Sprites\""
        );
    }

    /// §5.3: big tiles are `N+1` right and `N+0x10` down, with no carry-out for OBJs.
    #[test]
    fn tall_objects_fetch_n_plus_0x10() {
        let mut ppu = sprite_ppu(0x00); // small 8x8, large 16x16
        obj_tile(&mut ppu, 0x10, |_, _| 1); // top left
        obj_tile(&mut ppu, 0x11, |_, _| 2); // top right
        obj_tile(&mut ppu, 0x20, |_, _| 3); // bottom left
        obj_tile(&mut ppu, 0x21, |_, _| 4); // bottom right
        for (index, colour) in [(1usize, 0x001Fu16), (2, 0x03E0), (3, 0x7C00), (4, 0x001F)] {
            obj_colour(&mut ppu, index, colour);
        }
        obj_colour(&mut ppu, 4, 0x7FFF);
        put_sprite(&mut ppu, 0, 0, 20, 0x10, 0x00, true);
        at_line(&mut ppu, 25);
        let top = row_of(&ppu, 25);
        at_line(&mut ppu, 33); // the OBJ's ninth line, the second tile row
        let bottom = row_of(&ppu, 33);
        assert_eq!(
            (top[0], top[8]),
            (0x001F, 0x03E0),
            "the top row is N and N+1"
        );
        assert_eq!(
            (bottom[0], bottom[8]),
            (0x7C00, 0x7FFF),
            "the bottom row is N+10h and N+11h: fullsnes \"16x16 (and bigger) Tiles\""
        );
    }

    /// The OBJ X high bit is a `-256`. Wiki `Sprites`, OAM: "X position gives the top left corner
    /// of the sprite. The high bit acts as a -256, allowing a sprite to be placed partially off the
    /// left side of the screen." An X low byte `$F8` with the high bit set is therefore -8, and the
    /// right half of a 16x16 OBJ is clipped pixel by pixel onto screen columns 0-7.
    #[test]
    fn obj_x_high_bit_is_minus_256() {
        let mut ppu = sprite_ppu(0x00); // small 8x8, large 16x16
        // The OBJ's top-right 8x8 tile is N+1; its columns carry colours 1-8, so the visible half
        // names the source column it came from.
        obj_tile(&mut ppu, 0x11, |column, _| column as u8 + 1);
        for index in 1..=8usize {
            obj_colour(&mut ppu, index, 0x0100 + index as u16);
        }
        put_sprite(&mut ppu, 0, 0x01F8, 20, 0x10, 0x00, true);
        at_line(&mut ppu, 25);
        let row = row_of(&ppu, 25);
        for (column, &pixel) in row.iter().enumerate().take(8) {
            assert_eq!(
                pixel,
                0x0100 + column as u16 + 1,
                "X = $1F8 is -8: screen pixel {column} is OBJ pixel {} (wiki \"Sprites\")",
                column + 8
            );
        }
        assert_eq!(row[8], 0, "the OBJ's right edge is at screen pixel 8");
    }

    /// §5.11's two `UNVERIFIED:` rows: the OBJ's first row is `Y+1` on the V counter, and an OBJ
    /// whose Y is `$FF` does not wrap into line 0 in 224-line mode.
    #[test]
    fn obj_y_wrap_at_the_borders() {
        let mut ppu = sprite_ppu(0x00);
        obj_tile(&mut ppu, 1, |_, _| 1);
        obj_colour(&mut ppu, 1, 0x001F);

        // Y = 0: the OBJ covers V=1..8, i.e. frame rows 0..7.
        put_sprite(&mut ppu, 0, 0, 0, 1, 0x00, false);
        at_line(&mut ppu, 1);
        assert_eq!(row_of(&ppu, 1)[0], 0x001F, "Y = 0 covers screen line 1");
        at_line(&mut ppu, 8);
        assert_eq!(row_of(&ppu, 8)[0], 0x001F, "and line 8 is its last");
        at_line(&mut ppu, 9);
        assert_eq!(row_of(&ppu, 9)[0], 0, "line 9 is past the OBJ");

        // Y = $FE and $FF: below the visible area, and no wrap in 224-line mode.
        for (y, line) in [(0xFEu8, 30u16), (0xFF, 31)] {
            let mut ppu = sprite_ppu(0x00);
            obj_tile(&mut ppu, 1, |_, _| 1);
            obj_colour(&mut ppu, 1, 0x001F);
            put_sprite(&mut ppu, 0, 0, y, 1, 0x00, false);
            at_line(&mut ppu, line);
            assert_eq!(
                row_of(&ppu, line)[0],
                0,
                "Y = ${y:02X} is off the bottom and does not wrap into line 0"
            );
            at_line(&mut ppu, 2);
            assert_eq!(
                row_of(&ppu, 2)[0],
                0,
                "and it is not drawn on line 2 either"
            );
        }

        // A 64-pixel OBJ at Y = $F0 would cover 241..304; without wrap none of it is visible.
        let mut ppu = sprite_ppu(2 << 5); // small 8x8, large 64x64
        obj_tile(&mut ppu, 1, |_, _| 1);
        obj_colour(&mut ppu, 1, 0x001F);
        put_sprite(&mut ppu, 0, 0, 0xF0, 1, 0x00, true);
        at_line(&mut ppu, 40);
        assert_eq!(
            row_of(&ppu, 40)[0],
            0,
            "the 64-pixel OBJ does not reach line 40"
        );
    }
}
