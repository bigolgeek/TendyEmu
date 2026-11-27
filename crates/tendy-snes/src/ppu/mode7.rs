//! Mode 7: the rotation matrix, the per-line origin, the per-pixel step, 128x128 tile map, flips and
//! the three screen-over behaviours (fullsnes "SNES PPU Rotation/Scaling" and
//! "SNES PPU Video Memory").
//!
//! v1 computes the origin once per line at `START` and then evaluates fullsnes's closed form for each
//! pixel (§5.8). The inputs are constant across a line unless HDMA changes them, and HDMA runs at
//! H=278, between lines, so the line snapshot sees every change where hardware applies it (§5.1).
//! Drawing never touches `Ppu::m7_product`: §5.11's `layers::mode7_leaves_the_multiply_registers_alone`
//! pins that, because fullsnes documents the drawing-time multiply but not what a mid-line read of
//! `$2134`-`$2136` would return.

use super::colour::{self, Screen};
use super::{Layer, Pixel, Ppu, WIDTH};

/// Draws the mode-7 BG1 for the current line.
pub fn draw(ppu: &mut Ppu) {
    let regs = ppu.latched.clone();
    let main = regs.tm & 0x01 != 0;
    let sub = regs.ts & 0x01 != 0;
    if !(main || sub) {
        return;
    }
    let a = i32::from(regs.m7[0]);
    let b = i32::from(regs.m7[1]);
    let c = i32::from(regs.m7[2]);
    let d = i32::from(regs.m7[3]);
    let centre_x = i32::from(regs.m7x);
    let centre_y = i32::from(regs.m7y);
    let xflip = regs.m7sel & 0x01 != 0;
    let yflip = regs.m7sel & 0x02 != 0;
    let screen_over = regs.m7sel >> 6 & 0x03;

    // §5.10's mosaic, anchored at the screen's top edge; modes 5 and 6 do not exist here.
    let size = if regs.mosaic & 0x01 != 0 {
        usize::from(regs.mosaic >> 4) + 1
    } else {
        1
    };
    let row = usize::from(ppu.line);
    let mosaicked = row - row % size;
    // fullsnes: SCREEN.Y = (1..224) XOR (yflip*FFh).
    let screen_y = if yflip {
        (mosaicked ^ 0xFF) as i32
    } else {
        mosaicked as i32
    };

    // fullsnes: ORG.X = (M7HOFS-M7X) AND NOT 1C00h, and if that is negative, OR 1C00h back in.
    let origin_x = sign_fix(i32::from(regs.m7hofs) - centre_x);
    let origin_y = sign_fix(i32::from(regs.m7vofs) - centre_y);
    // fullsnes: VRAM.X = ((M7A*ORG.X) AND NOT 3Fh) + ((M7B*ORG.Y) AND NOT 3Fh) + M7X*100h, and the
    // same with M7C/M7D and M7Y for VRAM.Y; then the SCREEN.Y terms, then M7A*SCREEN.X per pixel.
    let base_x = ((a * origin_x) & !0x3F)
        + ((b * origin_y) & !0x3F)
        + centre_x * 0x100
        + ((b * screen_y) & !0x3F);
    let base_y = ((c * origin_x) & !0x3F)
        + ((d * origin_y) & !0x3F)
        + centre_y * 0x100
        + ((d * screen_y) & !0x3F);

    for x in 0..WIDTH {
        let mosaicked_x = x - x % size;
        let screen_x = if xflip {
            (mosaicked_x ^ 0xFF) as i32
        } else {
            mosaicked_x as i32
        };
        let vram_x = base_x + a * screen_x;
        let vram_y = base_y + c * screen_x;
        let Some(index) = sample(ppu, vram_x, vram_y, screen_over) else {
            continue;
        };
        if index == 0 {
            continue;
        }
        let pixel = Pixel {
            layer: Layer::Bg1,
            index,
            palette: 0,
            priority: 0,
        };
        if main {
            colour::offer(&mut ppu.main[x], &regs, Screen::Main, x, pixel);
        }
        if sub {
            colour::offer(&mut ppu.sub[x], &regs, Screen::Sub, x, pixel);
        }
    }
}

/// fullsnes's sign fix on the mode-7 origin: clear bits 10-12, then put them back as ones when the
/// result is negative, which is how the 13-bit value is extended.
fn sign_fix(value: i32) -> i32 {
    let masked = value & !0x1C00;
    if masked < 0 { masked | 0x1C00 } else { masked }
}

/// One mode-7 pixel: the map's tile number and the tile's 8-bit pixel, or `None` when the coordinate
/// is outside the 128x128 tile area and `M7SEL` bits 7-6 say "transparent".
///
/// The coordinates are in 1/256 pixels: bits 0-7 the fraction, 8-10 the pixel inside the tile, 11-17
/// the map index and bit 18 and up "nonzero when exceeding BG map size" (fullsnes).
fn sample(ppu: &Ppu, vram_x: i32, vram_y: i32, screen_over: u8) -> Option<u8> {
    let outside = vram_x < 0 || vram_y < 0 || (vram_x >> 18) != 0 || (vram_y >> 18) != 0;
    if outside {
        match screen_over {
            // 2 = transparent beyond the map; 3 = filled with tile 0; 0 and 1 wrap (the mask below
            // is the wrap).
            2 => return None,
            3 => return Some(tile_pixel(ppu, 0, vram_x, vram_y)),
            _ => {}
        }
    }
    let tile_x = (vram_x >> 11) & 0x7F;
    let tile_y = (vram_y >> 11) & 0x7F;
    let word = (tile_y * 128 + tile_x) & 0x3FFF;
    // "BG VRAM is divided into BG Map at even byte addresses": the map entry is the low byte of the
    // word and the tile's pixels are the odd bytes (fullsnes "SNES PPU Video Memory").
    let tile = usize::from(ppu.vram[(word as usize) * 2]);
    Some(tile_pixel(ppu, tile, vram_x, vram_y))
}

/// The pixel at `(vram_x, vram_y)` inside `tile`: row `r` is 16 bytes on, column `c` one word on, at
/// the odd byte (fullsnes "Mode 7 BG Tiles").
fn tile_pixel(ppu: &Ppu, tile: usize, vram_x: i32, vram_y: i32) -> u8 {
    let column = ((vram_x >> 8) & 0x07) as usize;
    let row = ((vram_y >> 8) & 0x07) as usize;
    let address = tile * 0x80 + row * 0x10 + column * 2 + 1;
    // Byte-addressed over all 64 KiB; see `layers::fetch`'s note about the mask.
    ppu.vram[address & 0xFFFF]
}
