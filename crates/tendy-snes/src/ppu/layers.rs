//! Background layers for modes 0-6: tile fetch, maps, big tiles, mosaic, offset-per-tile.
//! Mode 7 has its own file.
//!
//! Every layer is fetched once per pixel and offered to both screens: `TM`/`TS` decide which
//! screens get it, and `colour::offer` applies the window mask and the mode's priority chart, so
//! `main[x]`/`sub[x]` end up holding the frontmost pixel the chart allows.

use super::colour::{self, Screen};
use super::regs::PpuRegs;
use super::{Layer, Pixel, Ppu, WIDTH};

/// The layers a mode draws and their colour depth in bits: 2 (4 colours), 4 (16) or 8 (256).
/// fullsnes "2105h - BGMODE", the mode table. Modes 2, 4 and 6 have no BG3 of their own — BG3 is
/// the offset-per-tile provider in those modes (§5.10) and is not drawn.
static MODE0: [(Layer, u8); 4] = [
    (Layer::Bg1, 2),
    (Layer::Bg2, 2),
    (Layer::Bg3, 2),
    (Layer::Bg4, 2),
];
static MODE1: [(Layer, u8); 3] = [(Layer::Bg1, 4), (Layer::Bg2, 4), (Layer::Bg3, 2)];
static MODE2: [(Layer, u8); 2] = [(Layer::Bg1, 4), (Layer::Bg2, 4)];
static MODE3: [(Layer, u8); 2] = [(Layer::Bg1, 8), (Layer::Bg2, 4)];
static MODE4: [(Layer, u8); 2] = [(Layer::Bg1, 8), (Layer::Bg2, 2)];
static MODE5: [(Layer, u8); 2] = [(Layer::Bg1, 4), (Layer::Bg2, 2)];
static MODE6: [(Layer, u8); 1] = [(Layer::Bg1, 4)];
static MODE7: [(Layer, u8); 1] = [(Layer::Bg1, 8)];

/// Indexed by `BGMODE` bits 2-0.
static MODE_LAYERS: [&[(Layer, u8)]; 8] = [
    &MODE0, &MODE1, &MODE2, &MODE3, &MODE4, &MODE5, &MODE6, &MODE7,
];

/// The colour depth of `layer` in `mode`, in bits.
pub fn bpp(mode: u8, layer: Layer) -> u8 {
    if layer == Layer::Obj {
        // "OBJs are always 16 color" (fullsnes "SNES PPU Video Memory").
        return 4;
    }
    for &(candidate, depth) in MODE_LAYERS[(mode & 7) as usize] {
        if candidate == layer {
            return depth;
        }
    }
    2
}

/// The four BG layers of `mode` are drawn in this order; the chart in `colour.rs` sorts them.
pub fn draw(ppu: &mut Ppu, mode: u8) {
    let regs = ppu.latched.clone();
    for &(layer, depth) in MODE_LAYERS[usize::from(mode & 7)] {
        let index = layer_index(layer);
        let main = regs.tm & (1 << index) != 0;
        let sub = regs.ts & (1 << index) != 0;
        if main || sub {
            draw_layer(ppu, &regs, mode, layer, index, depth, main, sub);
        }
    }
}

/// `BGn` to its 0-3 index in `TM`, `TS`, `BGnSC` and the rest.
pub fn layer_index(layer: Layer) -> u8 {
    match layer {
        Layer::Bg1 => 0,
        Layer::Bg2 => 1,
        Layer::Bg3 => 2,
        _ => 3,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_layer(
    ppu: &mut Ppu,
    regs: &PpuRegs,
    mode: u8,
    layer: Layer,
    index: u8,
    depth: u8,
    main: bool,
    sub: bool,
) {
    let big = regs.bgmode & (0x10 << index) != 0;
    // §5.9: modes 5 and 6 are 512-wide on hardware; v1 renders the main screen's even dot of each
    // pair, so output pixel `x` is source pixel `2x` (the UNVERIFIED row of §5.11).
    let hires = mode == 5 || mode == 6;
    let mosaic_size = mosaic_size(regs, index);
    let row = usize::from(ppu.line) - 1;
    let source_y = if mosaic_size > 1 {
        row - row % mosaic_size
    } else {
        row
    };
    for x in 0..WIDTH {
        let source_x = if hires { x * 2 } else { x };
        let source_x = if mosaic_size > 1 {
            source_x - source_x % mosaic_size
        } else {
            source_x
        };
        // §5.10: modes 2, 4 and 6 take a per-column offset from BG3's map entry.
        let (add_x, add_y) = offset_per_tile(ppu, regs, mode, layer, source_x, source_y);
        let hofs = i32::from(regs.bghofs[usize::from(index)]) + add_x;
        let vofs = i32::from(regs.bgvofs[usize::from(index)]) + add_y;
        let fetch_x = (source_x as i32 + hofs).max(0) as usize;
        let fetch_y = (source_y as i32 + vofs).max(0) as usize;
        let pixel = fetch(ppu, regs, index, depth, fetch_x, fetch_y, big, mode);
        if pixel.index == 0 {
            // Index 0 is transparent for every BG.
            continue;
        }
        if main {
            colour::offer(&mut ppu.main[x], regs, Screen::Main, x, pixel);
        }
        if sub {
            colour::offer(&mut ppu.sub[x], regs, Screen::Sub, x, pixel);
        }
    }
}

/// §5.10: `N+1` pixels per block, `N` from `$2106` bits 7-4; 0 means 1x1, i.e. no mosaic.
fn mosaic_size(regs: &PpuRegs, index: u8) -> usize {
    if regs.mosaic & (1 << index) == 0 {
        return 1;
    }
    usize::from(regs.mosaic >> 4) + 1
}

/// §5.10, modes 2, 4 and 6: BG3's map entry for the column at `x` carries a 10-bit offset for BG1
/// or BG2. Returns the `(horizontal, vertical)` additions for this layer.
///
/// fullsnes "SNES PPU Video Memory", the "Offset-per-Tile" entry: bit 15 chooses H or V (**mode 4
/// only**), bit 14 enables BG2, bit 13 enables BG1, bits 9-0 are the offset, and a horizontal
/// offset's low 3 bits are ignored.
fn offset_per_tile(
    ppu: &Ppu,
    regs: &PpuRegs,
    mode: u8,
    layer: Layer,
    x: usize,
    y: usize,
) -> (i32, i32) {
    let index = match layer {
        Layer::Bg1 => 0,
        Layer::Bg2 => 1,
        _ => return (0, 0),
    };
    let (column, row) = match mode {
        2 | 4 | 6 => (
            (x + usize::from(regs.bghofs[2])) / 8,
            (y + usize::from(regs.bgvofs[2])) / 8,
        ),
        _ => return (0, 0),
    };
    let entry = map_entry(ppu, regs, 2, column, row);
    let enable = if index == 0 {
        entry & 0x2000 != 0
    } else {
        entry & 0x4000 != 0
    };
    if !enable {
        return (0, 0);
    }
    let offset = i32::from(entry & 0x03FF);
    if mode == 4 && entry & 0x8000 != 0 {
        (0, offset)
    } else {
        (offset & !0x7, 0)
    }
}

/// One map entry as the PPU sees it: the 32x32 screen arrangement of `$2107`-`$210A` bits 1-0.
/// Screen `SCn` is the `n`-th 32x32 block after the layer's base address.
///
/// The coordinate is reduced modulo the layer's own map before a screen is chosen. BGnSC bits 0-1
/// give the size — fullsnes "2107h - BG1SC": "SC Size (0=One-Screen, 1=V-Mirror, 2=H-Mirror,
/// 3=Four-Screen) (0=32x32, 1=64x32, 2=32x64, 3=64x64 tiles)" — and the whole background repeats
/// at the end of that map: wiki `Scrolling_a_large_map`, "The reason this works is because tilemaps
/// wrap around - attempting to display data past the end of one will just loop back to the left or
/// top of the map." Wiki `Tilemaps` says the pairs are combined "into a larger background", so a
/// 64-wide background's end is 64 tiles, i.e. 512 pixels, not 32 tiles. (The old code selected the
/// screen from the unreduced column, so columns 64-95 of a 64x32 map re-read screen 1.)
fn map_entry(ppu: &Ppu, regs: &PpuRegs, index: u8, column: usize, row: usize) -> u16 {
    let sc = regs.bgsc[usize::from(index)];
    let base = usize::from(sc >> 2) * 0x400;
    let width = if sc & 0x01 != 0 { 64 } else { 32 };
    let height = if sc & 0x02 != 0 { 64 } else { 32 };
    let (column, row) = (column % width, row % height);
    let (tile_x, tile_y) = (column & 31, row & 31);
    let screen = match sc & 0x03 {
        0 => 0,
        1 => usize::from(column >= 32),
        2 => usize::from(row >= 32),
        _ => usize::from(column >= 32) + 2 * usize::from(row >= 32),
    };
    let word = (base + screen * 0x400 + tile_y * 32 + tile_x) & 0x7FFF;
    let address = word * 2;
    u16::from(ppu.vram[address]) | (u16::from(ppu.vram[address + 1]) << 8)
}

/// One BG pixel: the map entry, the tile below it (big tiles are `N+1` right and `N+0x10` down,
/// carrying out for BGs), the bit-planes and the entry's priority and palette.
///
/// A 16x16 tile mode moves two things at once. Wiki `Tilemaps`: "The tiles of a tilemap can also
/// count \"double\" with BGMODE, selecting a 2x2 group of adjacent 8x8 pixel tiles for a 16x16 pixel
/// region in the background ... This doubles the tilemap's size in each dimension without
/// increasing memory usage", and "the tile index specifies the top left 8x8 tile from VRAM, and it
/// will automatically include the adjacent tiles horizontally (+1) and vertically (+16, +17)".
/// So one map entry now covers 16x16 pixels — the map is looked up every 16 pixels — while the
/// four sub-tiles inside it are still `N`, `N+1`, `N+0x10` and `N+0x11`.
#[allow(clippy::too_many_arguments)]
fn fetch(
    ppu: &Ppu,
    regs: &PpuRegs,
    index: u8,
    depth: u8,
    x: usize,
    y: usize,
    big: bool,
    mode: u8,
) -> Pixel {
    // fullsnes "Backgrounds": "The 8x8 pixel tilemap tile size selectable via BGMODE is replaced
    // with a 16x8 pixel tile mode instead. The 16x16 mode is still 16x16." Modes 5 and 6 have no
    // 8x8 size, so with the size bit clear a map entry covers 16x8 pixels — 16 across, 8 down —
    // and the bit then selects 16x16. `wide` is the horizontal half of that rule.
    let wide = big || matches!(mode, 5 | 6);
    let step_x = if wide { 16 } else { 8 };
    let step_y = if big { 16 } else { 8 };
    let entry = map_entry(ppu, regs, index, x / step_x, y / step_y);
    let mut pixel_x = x & 7;
    let mut pixel_y = y & 7;
    let mut tile = usize::from(entry & 0x03FF);
    if wide {
        // fullsnes "16x16 (and bigger) Tiles": the tile to the right is N+1, the one below N+10h,
        // and BG tiles carry out into the next row of the tile array (OBJ tiles do not). A 16x8
        // tile has the `N+1` neighbour only.
        let below = if big { ((y & 15) / 8) * 0x10 } else { 0 };
        tile = (tile + ((x & 15) / 8) + below) & 0x03FF;
        pixel_x = x & 7;
        pixel_y = y & 7;
    }
    if entry & 0x4000 != 0 {
        pixel_x = 7 - pixel_x;
    }
    if entry & 0x8000 != 0 {
        pixel_y = 7 - pixel_y;
    }
    let base = usize::from(regs.nba[usize::from(index)]) * 0x2000;
    let tile_base = base + tile * usize::from(depth) * 8;
    let row = tile_base + pixel_y * 2;
    let mut colour = 0u8;
    for plane in 0..depth {
        let offset = row + usize::from(plane >> 1) * 16 + usize::from(plane & 1);
        // `vram` is byte-addressed over all 64 KiB (32 Kwords, fullsnes "SNES Memory Map"), so the
        // 15-bit word address reaches byte `$FFFE` and the offset is masked with `0xFFFF`. A
        // `0x7FFF` mask here folded every character fetch above 32 KiB back onto the bottom half of
        // VRAM, which is what made Super Mario World's title screen a mosaic of wrong tiles.
        let byte = ppu.vram[offset & 0xFFFF];
        colour |= ((byte >> (7 - pixel_x)) & 1) << plane;
    }
    let palette = ((entry >> 10) & 0x07) as u8;
    // §5.3: `Pixel.palette` is the CGRAM page in 16-colour units for a 16-colour or OBJ layer, and
    // the absolute 4-colour palette number otherwise — mode 0 is the one mode where a 4-colour
    // BG's block moves with the layer (fullsnes "CGRAM Palette Indices": BG2-4 in mode 0 use
    // 21h-3Fh/41h-5Fh/61h-7Fh rather than 01h-1Fh).
    let palette = if depth == 2 && mode == 0 {
        index * 8 + palette
    } else {
        palette
    };
    Pixel {
        layer: match index {
            0 => Layer::Bg1,
            1 => Layer::Bg2,
            2 => Layer::Bg3,
            _ => Layer::Bg4,
        },
        index: colour,
        palette,
        priority: ((entry >> 13) & 0x01) as u8,
    }
}
