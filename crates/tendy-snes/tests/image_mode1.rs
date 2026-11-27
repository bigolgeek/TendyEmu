//! `tests/image_mode1.rs` — hand-built image 2 (§12.2): mode 1 with a scrolling 16-colour BG1, a
//! 4-colour BG2 behind it, eight sprites with flips, two priorities and a rotated `OBSEL` base, and
//! colour math adding BG2 to BG1 over a window.
//!
//! The picture is driven straight into `Ppu` — no 65816, no cartridge — so this file is green on the
//! scaffold's decode (§11.2). The checksum is pinned from a build whose other tests were green, and
//! it fails if the layer composition, the priority chart, the sprite evaluation, the window or the
//! colour-math arithmetic moves.

use tendy_snes::ppu::Ppu;
use tendy_snes::state::fnv1a64_slice;
use tendy_snes::{FRAME_PIXELS, WIDTH};

/// The pinned frame checksum, recorded from the run that made this test pass (§12.2's procedure):
/// `cargo test --test image_mode1` on a tree whose other tests were green.
const PINNED: u64 = 14437007599883084859;

/// Builds the scene: mode 1, BG1 16-colour scrolling, BG2 4-colour behind it, eight OBJs, colour
/// math inside a window.
fn build() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.write(0x00, 0x0F); // brightness 15, forced blank off
    ppu.write(0x05, 0x01); // mode 1

    // BG1: map at word $0400, tiles at word $1000 (BG12NBA low nibble 1 -> 0x2000 bytes).
    ppu.write(0x07, 0x04);
    ppu.write(0x08, 0x08);
    ppu.write(0x0B, 0x01);
    // BG2: map at word $0800, tiles at word $2000 (nibble 2).
    ppu.write(0x0B, 0x21);

    // BG1's tiles: sixteen 16-colour tiles, each a two-colour checker of `tile` and `tile + 8`.
    for tile in 0..16usize {
        let low = (tile % 8) as u8 + 1;
        let high = (tile % 8) as u8 + 9;
        let start = 0x1000 * 2 + tile * 32;
        for row in 0..8usize {
            for column in 0..8usize {
                let colour = if (column + row) & 1 == 0 { low } else { high };
                for plane in 0..4usize {
                    let offset = start + row * 2 + (plane >> 1) * 16 + (plane & 1);
                    if (colour >> plane) & 1 != 0 {
                        ppu.vram[offset & 0x7FFF] |= 0x80 >> column;
                    }
                }
            }
        }
    }
    // BG1's map: tile and palette both walk across the screen, and every other row is priority 1.
    for ty in 0..32usize {
        for tx in 0..32usize {
            let tile = (tx + ty) % 16;
            let palette = tx % 8;
            let priority = if ty & 1 == 0 { 0 } else { 0x2000 };
            let entry = (tile as u16) | ((palette as u16) << 10) | priority;
            let word = 0x0400 + ty * 32 + tx;
            ppu.vram[word * 2] = entry as u8;
            ppu.vram[word * 2 + 1] = (entry >> 8) as u8;
        }
    }

    // BG2's tiles: four solid 4-colour tiles in the low palettes.
    for tile in 0..4usize {
        let start = 0x2000 * 2 + tile * 16;
        for plane in 0..2usize {
            let byte = if (tile as u8 >> plane) & 1 != 0 {
                0xFF
            } else {
                0
            };
            for row in 0..8usize {
                ppu.vram[start + row * 2 + plane] = byte;
            }
        }
    }
    for ty in 0..32usize {
        for tx in 0..32usize {
            let entry = ((tx % 4) as u16) | (((tx % 4) as u16) << 10);
            let word = 0x0800 + ty * 32 + tx;
            ppu.vram[word * 2] = entry as u8;
            ppu.vram[word * 2 + 1] = (entry >> 8) as u8;
        }
    }

    // Scrolls: BG1 three to the right and five down, BG2 one and two, both through the write-twice
    // ports (low byte then high byte).
    for (register, value) in [(0x0D, 3u16), (0x0E, 5), (0x0F, 1), (0x10, 2)] {
        ppu.write(register, value as u8);
        ppu.write(register, 0);
    }

    // CGRAM: BG1's eight 16-colour pages, BG2's four 4-colour pages and the OBJ pages.
    for index in 0..128usize {
        ppu.cgram[index] = ((index as u16) << 5) | (index as u16 & 0x1F);
    }
    for index in 0..128usize {
        ppu.cgram[128 + index] = ((index as u16) << 7) | 0x001F;
    }

    // Eight OBJs: a rotated OBSEL base (1) so the tiles live at $4000, alternating flips, priorities
    // 3 and 1, three palettes.
    ppu.write(0x01, 0x21); // size pair 1 (8x8 / 32x32), base 1
    for tile in 0..4usize {
        let start = 0x4000 + tile * 32;
        let colour = (tile + 1) as u8;
        for plane in 0..4usize {
            let byte = if (colour >> plane) & 1 != 0 { 0xFF } else { 0 };
            for row in 0..8usize {
                ppu.vram[start + row * 2 + (plane >> 1) * 16 + (plane & 1)] = byte;
            }
        }
    }
    for index in 0..8usize {
        let x = (index as u16) * 31;
        let y = 20 + (index as u8) * 8;
        let tile = (index % 4) as u8;
        let flip = if index & 1 == 0 { 0x40 } else { 0x80 };
        let priority = if index < 4 { 3 } else { 2 };
        let palette = (index % 3) as u8;
        ppu.oam[index * 4] = x as u8;
        ppu.oam[index * 4 + 1] = y;
        ppu.oam[index * 4 + 2] = tile;
        ppu.oam[index * 4 + 3] = flip | (priority << 4) | (palette << 1);
        if x > 0xFF {
            let shift = (index % 4) * 2;
            ppu.oam[0x200 + index / 4] |= 0x01 << shift;
        }
    }
    // Every other OAM entry is parked off-screen, so the picture is only the eight OBJs above.
    for index in 8..128usize {
        ppu.oam[index * 4 + 1] = 0xFF;
    }

    // Colour math: BG1 on the main screen with OBJ, BG2 on the sub screen, add-and-halve over a
    // window from x=60 to x=180.
    ppu.write(0x2C, 0x11); // TM: BG1, OBJ
    ppu.write(0x2D, 0x02); // TS: BG2
    ppu.write(0x30, 0x12); // CGWSEL: math inside the math window, sub layers on
    ppu.write(0x31, 0x41); // CGADSUB: add, half, BG1
    ppu.write(0x25, 0x20); // WOBJSEL: the colour window's window 1 enabled, not inverted
    ppu.write(0x26, 60); // WH0
    ppu.write(0x27, 180); // WH1
    ppu.write(0x32, 0x7F); // a bright fixed colour, unused while the sub screen has a layer
    ppu
}

mod image_mode1 {
    use super::*;

    /// §12.2's image 2, pinned: one frame of the scene above, checksummed.
    #[test]
    fn mode1_background_and_sprite_matches_the_pinned_checksum() {
        let mut ppu = build();
        ppu.tick(tendy_snes::MASTER_TICKS_PER_FRAME);
        assert!(ppu.take_frame(), "a frame finished");
        assert_eq!(ppu.frame().len(), FRAME_PIXELS);
        let checksum = fnv1a64_slice(ppu.frame());

        // Structural checks first, so a failure says which part of the picture moved.
        let row = &ppu.frame[(100 - 1) * WIDTH..100 * WIDTH];
        let colours: std::collections::BTreeSet<u16> = ppu.frame().iter().copied().collect();
        assert!(
            colours.len() >= 8,
            "the picture uses at least eight colours, got {}",
            colours.len()
        );
        assert!(
            row.iter().any(|&pixel| pixel != row[0]),
            "row 100 is not one flat colour"
        );
        assert!(
            ppu.frame()[(20 - 1) * WIDTH + 2] != ppu.frame()[(20 - 1) * WIDTH + 40],
            "the OBJs at y=20 differ from the background between them"
        );

        assert_eq!(
            checksum, PINNED,
            "the mode-1 picture changed: the checksum was pinned from a green build"
        );
    }
}
