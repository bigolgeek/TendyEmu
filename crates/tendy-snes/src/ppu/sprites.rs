//! Sprites: per-line evaluation, the two overflow flags, the 32-pixel tile budget, flips,
//! palettes, sizes, tall objects and the priority rotation.
//!
//! Evaluation walks OAM in priority-rotation order, collects at most 32 OBJs that cover this line,
//! and draws them front to back. Two OBJs that end up at the same chart rank keep the first one
//! drawn, which is the lower OAM index — the `UNVERIFIED:` tie-break of §5.11, pinned by
//! `sprites::equal_priority_lower_index_is_in_front`.

use super::colour::{self, Screen};
use super::{Layer, Pixel, Ppu, WIDTH};

/// The most OBJs hardware puts on one line (`$213E` bit 6).
const MAX_OBJS: usize = 32;
/// The most 8-pixel OBJ tiles hardware fetches on one line (`$213E` bit 7).
const MAX_TILES: usize = 34;

/// One evaluated OBJ for the line being drawn.
#[derive(Clone, Copy, Default)]
struct Sprite {
    /// X from OAM, sign-extended: bit 8 is a `-256`, so the field is -256..255 (wiki `Sprites`:
    /// "The high bit acts as a -256, allowing a sprite to be placed partially off the left side of
    /// the screen"). Pixels outside 0-255 are clipped one by one when the OBJ is drawn.
    x: i16,
    /// The top line of the OBJ in 0-based screen rows.
    top: usize,
    tile: usize,
    attr: u8,
    width: usize,
    height: usize,
}

/// The (small, large) size pair `OBSEL` bits 7-5 select (fullsnes "2101h - OBSEL").
fn sizes(obsel: u8) -> ((usize, usize), (usize, usize)) {
    match obsel >> 5 {
        0 => ((8, 8), (16, 16)),
        1 => ((8, 8), (32, 32)),
        2 => ((8, 8), (64, 64)),
        3 => ((16, 16), (32, 32)),
        4 => ((16, 16), (64, 64)),
        5 => ((32, 32), (64, 64)),
        6 => ((16, 32), (32, 64)),
        _ => ((16, 32), (32, 32)),
    }
}

/// OAM's 2-bit-per-OBJ high table: bit 0 is X's bit 8, bit 1 is the size select.
fn high_bits(ppu: &Ppu, index: usize) -> u8 {
    let byte = ppu.oam[0x200 + index / 4];
    (byte >> ((index % 4) * 2)) & 0x03
}

/// Draws this line's OBJs into the two screens (§5.2 step 4).
pub fn draw(ppu: &mut Ppu) {
    let regs = ppu.latched.clone();
    let main = regs.tm & 0x10 != 0;
    let sub = regs.ts & 0x10 != 0;
    if !(main || sub) {
        return;
    }
    let (small, large) = sizes(regs.obsel);
    let row = usize::from(ppu.line) - 1;
    let mut list = [Sprite::default(); MAX_OBJS];
    let mut count = 0usize;
    let mut tiles = 0usize;
    let mut range_overflow = false;
    let mut time_overflow = false;
    // §5.5: OAM index 0 first, unless `$2103` bit 7 makes OBJ N the highest-priority one.
    let first = if regs.oam_rotating() {
        usize::from(regs.oam_rotation_index())
    } else {
        0
    };
    for step in 0..128 {
        let index = (first + step) & 0x7F;
        let high = high_bits(ppu, index);
        let (width, height) = if high & 0x02 != 0 { large } else { small };
        let top = usize::from(ppu.oam[index * 4 + 1]);
        // §5.5's coverage, `UNVERIFIED:` in §5.11: the OBJ's first row is screen line Y+1, i.e.
        // 0-based row Y, and it covers Y ..= Y+height-1 with no wrap in 224-line mode.
        if row < top || row >= top + height {
            continue;
        }
        if count == MAX_OBJS {
            range_overflow = true;
            continue;
        }
        let across = width / 8;
        if tiles + across > MAX_TILES {
            time_overflow = true;
            continue;
        }
        tiles += across;
        // Wiki `Sprites`, OAM: "X position gives the top left corner of the sprite. The high bit
        // acts as a -256"; with the high bit set the low byte is 248..255, i.e. -8..-1.
        let x_low = i16::from(ppu.oam[index * 4]);
        list[count] = Sprite {
            x: if high & 0x01 != 0 { x_low - 256 } else { x_low },
            top,
            tile: usize::from(ppu.oam[index * 4 + 2])
                | (usize::from(ppu.oam[index * 4 + 3] & 0x01) << 8),
            attr: ppu.oam[index * 4 + 3],
            width,
            height,
        };
        count += 1;
    }
    ppu.obj_range_overflow |= range_overflow;
    ppu.obj_time_overflow |= time_overflow;

    for sprite in &list[..count] {
        let palette = 8 + ((sprite.attr >> 1) & 0x07);
        let priority = (sprite.attr >> 4) & 0x03;
        let dy = row - sprite.top;
        for dx in 0..sprite.width {
            let screen_x = i32::from(sprite.x) + dx as i32;
            if !(0..WIDTH as i32).contains(&screen_x) {
                continue;
            }
            let index = sprite_pixel(ppu, regs.obsel, sprite, dx, dy);
            if index == 0 {
                continue;
            }
            let pixel = Pixel {
                layer: Layer::Obj,
                index,
                palette,
                priority,
            };
            let x = screen_x as usize;
            if main {
                colour::offer(&mut ppu.main[x], &regs, Screen::Main, x, pixel);
            }
            if sub {
                colour::offer(&mut ppu.sub[x], &regs, Screen::Sub, x, pixel);
            }
        }
    }
}

/// One sprite pixel: the flips, the 16x16-and-bigger tile array (no carry-outs for OBJs) and the
/// four bit-planes of a 16-colour tile (fullsnes "SNES PPU Video Memory", "16x16 (and bigger)
/// Tiles": "for OBJ tiles, there are no carry-outs from x+1 to y, nor from y+1 to Y").
fn sprite_pixel(ppu: &Ppu, obsel: u8, sprite: &Sprite, dx: usize, dy: usize) -> u8 {
    let px = if sprite.attr & 0x40 != 0 {
        sprite.width - 1 - dx
    } else {
        dx
    };
    let py = if sprite.attr & 0x80 != 0 {
        sprite.height - 1 - dy
    } else {
        dy
    };
    // "The hex-tile numbers could be thought of as Yyxh, with x being the 4-bit x-index and Yy the
    // y-index", and the OBJ array is 16 wide by 32 tall: both counters wrap in place.
    let column = ((sprite.tile & 0x0F) + px / 8) & 0x0F;
    let row = (((sprite.tile >> 4) & 0x1F) + py / 8) & 0x1F;
    let tile = (row << 4) | column;
    let base = usize::from(obsel & 0x07) * 0x4000
        + if sprite.tile >= 0x100 {
            usize::from((obsel >> 3) & 0x03) * 0x2000
        } else {
            0
        };
    let address = base + tile * 32 + (py & 7) * 2;
    let bit = 7 - (px & 7);
    let mut colour = 0u8;
    for plane in 0..4usize {
        let offset = address + (plane >> 1) * 16 + (plane & 1);
        // Byte-addressed over all 64 KiB; see `layers::fetch`'s note about the mask.
        let byte = ppu.vram[offset & 0xFFFF];
        colour |= ((byte >> bit) & 1) << plane;
    }
    colour
}
