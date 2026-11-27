//! PPU tile trace: for every pixel of the frame, what produced it.
//!
//! A debug view for the PPU viewer and for a later HD-pack pass (Mesen `hires.txt` `<tile>` rules
//! match on a tile's CHR bytes and palette). It is **off by default**; while off the PPU records
//! nothing and `tile_trace` returns `None`. It is not machine state: it is not saved, not loaded,
//! and nothing in emulation reads it, so enabling it cannot change a frame.
//!
//! Provenance is kept beside the pixel pipeline, not inside it. The background keeps two tile
//! descriptors mirroring the two bytes of the 16-bit shifters (the high byte is the tile on
//! screen); each sprite slot keeps the CHR address it fetched and the OAM index it was copied from.
//! `output_pixel` attaches the winner's descriptors to the queued pixel and `finalize_lag_pixel`
//! writes the plane entry with the same winner rule it uses for the colour, so layer hiding and
//! the PPUMASK gates are reflected.

use super::Ppu;

/// Which layer produced a pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// Not written since the plane was enabled.
    #[default]
    None,
    /// Neither layer drew: the backdrop colour shows.
    Backdrop,
    Background,
    /// A sprite, by its OAM index (0..=63).
    Sprite(u8),
}

/// The identity of one on-screen pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelTrace {
    pub layer: Layer,
    /// PPU pattern-table address (0..=0x1FFF) of the low bit-plane byte of the tile row the pixel
    /// came from; the high plane is at `+ 8`. A pattern-table address, not a CHR-ROM offset: the
    /// mapper's bank at the time of the fetch decides which ROM bytes it was. For 8x16 sprites
    /// it is the half (top or bottom tile) the row fell in.
    pub chr_addr: u16,
    /// Palette index 0..=3 for the background, 4..=7 for sprites (palette RAM at `$3F00 + 4 * palette`).
    pub palette: u8,
    /// The two-bit colour index inside the palette (0 for the backdrop).
    pub pixel: u8,
    /// The winner's sprite attribute byte: bits 7:6 are the flips, bit 5 background
    /// priority. 0 on a background pixel.
    pub attr: u8,
    /// CHR-ROM byte offset of the winner's tile at the time of the fetch, so a board
    /// whose CHR bank moves during rendering (MMC2/MMC4) still names the bytes it drew. `None` for
    /// CHR RAM.
    pub chr_rom: Option<u32>,
    /// The background tile under this pixel, whatever layer won it: the row's
    /// pattern-table address with its low four bits cleared, and its palette index. `bg_palette`
    /// is `0xFF` where the background layer drew nothing, so it can never match a palette (0..=3).
    pub bg_chr_addr: u16,
    pub bg_palette: u8,
    /// The background tile's CHR-ROM offset at the fetch, `None` for CHR RAM.
    pub bg_chr_rom: Option<u32>,
}

impl PixelTrace {
    /// The "never written" entry.
    pub const NONE: PixelTrace = PixelTrace {
        layer: Layer::None,
        chr_addr: 0,
        palette: 0,
        pixel: 0,
        attr: 0,
        chr_rom: None,
        bg_chr_addr: 0,
        bg_palette: 0xFF,
        bg_chr_rom: None,
    };
}

impl Default for PixelTrace {
    fn default() -> Self {
        PixelTrace::NONE
    }
}

/// One tile row's identity while it travels through the pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub chr_addr: u16,
    pub palette: u8,
    /// OAM index, sprites only.
    pub oam: u8,
    /// The sprite attribute byte at the fetch; 0 for the background.
    pub attr: u8,
    /// CHR-ROM byte offset of this tile's first byte at the fetch; `None` for CHR RAM.
    pub chr_rom: Option<u32>,
}

/// The recording state behind `Ppu::trace`.
#[derive(Clone, Debug)]
pub struct TileTrace {
    /// 256 x 240 entries, row-major, parallel to `Ppu::framebuffer`.
    pub(super) plane: Vec<PixelTrace>,
    /// Background tile on screen / next in line, mirroring the high and low byte of the shifters.
    pub(super) bg_cur: Source,
    pub(super) bg_next: Source,
    /// Pattern address of the background tile row last fetched, waiting for the shifter load.
    pub(super) bg_fetch_addr: u16,
    /// The CHR-ROM offset of that fetch.
    pub(super) bg_fetch_rom: Option<u32>,
    /// What each of the 8 sprite slots fetched for the line being drawn.
    pub(super) spr: [Source; 8],
    /// OAM index each secondary-OAM slot was copied from during evaluation.
    pub(super) oam2_src: [u8; 8],
    /// Identity of the sprite that won the last `sprite_pixel` call: its OAM index and what it
    /// fetched. A `Source`, not a slot index: a 9th-or-later sprite (sprite-limit removal)
    /// has no hardware slot, and keying by slot blamed slot 0 for it.
    pub(super) last_spr: Source,
}

impl TileTrace {
    fn new() -> Self {
        TileTrace {
            plane: vec![PixelTrace::NONE; 256 * 240],
            bg_cur: Source::default(),
            bg_next: Source::default(),
            bg_fetch_addr: 0,
            bg_fetch_rom: None,
            spr: [Source::default(); 8],
            oam2_src: [0; 8],
            last_spr: Source::default(),
        }
    }
}

impl Ppu {
    /// Turn the tile trace on or off. Turning it on starts from an all-`Layer::None` plane; turning
    /// it off drops the plane. Already enabled: no change.
    pub fn set_tile_trace(&mut self, on: bool) {
        match (on, self.trace.is_some()) {
            (true, false) => {
                self.trace = Some(Box::new(TileTrace::new()));
                // Pixels already inside the two-dot pipeline were queued while nothing recorded
                // where they came from, so those two entries start clean instead of carrying the
                // descriptors of whatever was traced before (docs/ppu-pixel.md §3).
                self.lag_src = Default::default();
            }
            (false, true) => self.trace = None,
            _ => {}
        }
    }

    /// True when the tile trace is recording.
    pub fn tile_trace_enabled(&self) -> bool {
        self.trace.is_some()
    }

    /// The per-pixel trace of the current frame (256 x 240, row-major), or `None` while off.
    /// Entries are rewritten as the frame is drawn, so mid-frame the lower rows are the last frame's.
    pub fn tile_trace(&self) -> Option<&[PixelTrace]> {
        self.trace.as_deref().map(|t| t.plane.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::load;

    fn cart() -> Box<dyn crate::cart::Mapper> {
        let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        v.resize(16 + 16384, 0);
        load(&v).unwrap()
    }

    /// Run until `frames` frames have completed.
    fn run(ppu: &mut Ppu, cart: &mut dyn crate::cart::Mapper, frames: u64) {
        let target = ppu.frame + frames;
        while ppu.frame < target {
            ppu.tick(cart);
        }
    }

    /// Background tile 5 at nametable cell (0,0) with attribute palette 1, pattern table 1:
    /// row 3 of the tile has its leftmost pixel set.
    fn bg_scene(ppu: &mut Ppu, cart: &mut dyn crate::cart::Mapper) {
        ppu.ctrl = 0x10;
        ppu.mem_write(cart, 0x2000, 5);
        ppu.mem_write(cart, 0x23C0, 0x01);
        ppu.mem_write(cart, 0x1000 + 5 * 16 + 3, 0x80);
        ppu.mask = 0x1E;
    }

    #[test]
    fn background_pixel_maps_to_its_tile_row_and_palette() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        bg_scene(&mut ppu, cart.as_mut());
        run(&mut ppu, cart.as_mut(), 2);
        let plane = ppu.tile_trace().unwrap();
        // Pixel (0, 3) is the set pixel of tile 5's row 3.
        let t = plane[3 * 256];
        assert_eq!(t.layer, Layer::Background);
        assert_eq!(t.chr_addr, 0x1000 + 5 * 16 + 3);
        assert_eq!(t.palette, 1);
        assert_eq!(t.pixel, 1);
        // The pixel next to it is transparent in the tile: backdrop, no tile identity.
        assert_eq!(plane[3 * 256 + 1].layer, Layer::Backdrop);
        // Same tile, another row: the address follows the row.
        ppu.mem_write(cart.as_mut(), 0x1000 + 5 * 16 + 6, 0x80);
        run(&mut ppu, cart.as_mut(), 1);
        let t = ppu.tile_trace().unwrap()[6 * 256];
        assert_eq!(
            (t.layer, t.chr_addr),
            (Layer::Background, 0x1000 + 5 * 16 + 6)
        );
    }

    #[test]
    fn background_trace_follows_fine_x_scroll() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        bg_scene(&mut ppu, cart.as_mut());
        // Tile 6 in cell 2 (the top-right attribute quadrant, bits 3:2): palette 2.
        ppu.mem_write(cart.as_mut(), 0x2002, 6);
        ppu.mem_write(cart.as_mut(), 0x1000 + 6 * 16 + 3, 0xFF);
        ppu.mem_write(cart.as_mut(), 0x23C0, 0x01 | (2 << 2));
        ppu.fine_x = 3;
        run(&mut ppu, cart.as_mut(), 2);
        let plane = ppu.tile_trace().unwrap();
        // Fine X 3 shifts the picture left by three: tile 6 starts at screen x = 16 - 3 = 13, and the
        // tile on screen switches mid-tile, three pixels before the shifters reload.
        let t = plane[3 * 256 + 13];
        assert_eq!(
            (t.layer, t.chr_addr, t.palette),
            (Layer::Background, 0x1000 + 6 * 16 + 3, 2)
        );
        assert_eq!(plane[3 * 256 + 12].layer, Layer::Backdrop);
    }

    #[test]
    fn sprite_pixel_reports_its_oam_slot_and_chr_address() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        ppu.ctrl = 0x08; // sprites from pattern table 1
        ppu.mask = 0x1E;
        // OAM sprite 9: Y 19 (drawn from line 20), tile 7, palette 2, X 40.
        ppu.oam[9 * 4..9 * 4 + 4].copy_from_slice(&[19, 7, 0x02, 40]);
        // Park the other sprites off screen.
        for n in (0..64).filter(|&n| n != 9) {
            ppu.oam[n * 4] = 0xFF;
        }
        ppu.mem_write(cart.as_mut(), 0x1000 + 7 * 16, 0xFF);
        run(&mut ppu, cart.as_mut(), 2);
        let plane = ppu.tile_trace().unwrap();
        let t = plane[20 * 256 + 40];
        assert_eq!(t.layer, Layer::Sprite(9));
        assert_eq!(t.chr_addr, 0x1000 + 7 * 16);
        assert_eq!((t.palette, t.pixel), (4 + 2, 1));
        // Eight pixels wide, and the neighbours are backdrop.
        assert_eq!(plane[20 * 256 + 47].layer, Layer::Sprite(9));
        assert_eq!(plane[20 * 256 + 48].layer, Layer::Backdrop);
        assert_eq!(plane[20 * 256 + 39].layer, Layer::Backdrop);
    }

    #[test]
    fn sprite_behind_background_reports_the_background() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        bg_scene(&mut ppu, cart.as_mut());
        ppu.mem_write(cart.as_mut(), 0x1000 + 5 * 16 + 3, 0xFF);
        // Sprite 0 behind background (attr bit 5) over the tile's row 3 (line 3 = Y 2).
        for n in 0..64 {
            ppu.oam[n * 4] = 0xFF;
        }
        ppu.oam[0..4].copy_from_slice(&[2, 5, 0x20, 0]);
        ppu.mem_write(cart.as_mut(), 0x5 * 16, 0xFF); // pattern table 0 row 0 for the sprite
        run(&mut ppu, cart.as_mut(), 2);
        let t = ppu.tile_trace().unwrap()[3 * 256];
        assert_eq!(t.layer, Layer::Background);
    }

    #[test]
    fn disabled_plane_is_absent_and_enabling_starts_unwritten() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        bg_scene(&mut ppu, cart.as_mut());
        run(&mut ppu, cart.as_mut(), 2);
        // Off: nothing recorded, nothing to read.
        assert!(!ppu.tile_trace_enabled());
        assert!(ppu.tile_trace().is_none());
        // Enabling mid-run starts from the sentinel; no frame has been drawn into it yet.
        ppu.set_tile_trace(true);
        assert!(
            ppu.tile_trace()
                .unwrap()
                .iter()
                .all(|t| *t == PixelTrace::NONE)
        );
        // Turning it off and on again discards what was recorded.
        run(&mut ppu, cart.as_mut(), 2);
        assert!(
            ppu.tile_trace()
                .unwrap()
                .iter()
                .any(|t| t.layer == Layer::Background)
        );
        ppu.set_tile_trace(false);
        assert!(ppu.tile_trace().is_none());
        ppu.set_tile_trace(true);
        assert!(
            ppu.tile_trace()
                .unwrap()
                .iter()
                .all(|t| *t == PixelTrace::NONE)
        );
    }

    #[test]
    fn tracing_does_not_change_the_picture() {
        let make = |on: bool| {
            let mut cart = cart();
            let mut ppu = Ppu::new();
            ppu.set_tile_trace(on);
            bg_scene(&mut ppu, cart.as_mut());
            ppu.oam[0..4].copy_from_slice(&[2, 5, 0x00, 4]);
            run(&mut ppu, cart.as_mut(), 3);
            (
                ppu.framebuffer.clone(),
                ppu.emphasis.clone(),
                ppu.status,
                ppu.dot,
                ppu.scanline,
            )
        };
        assert_eq!(make(false), make(true));
    }

    #[test]
    fn hidden_layers_are_reflected() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        bg_scene(&mut ppu, cart.as_mut());
        ppu.layers.hide_bg = true;
        run(&mut ppu, cart.as_mut(), 2);
        assert_eq!(ppu.tile_trace().unwrap()[3 * 256].layer, Layer::Backdrop);
    }

    /// Nine sprites in range on one line with the sprite limit removed: the 9th has no hardware
    /// slot, so the trace must name it by its OAM index rather than blaming slot 0.
    #[test]
    fn extra_sprite_is_traced_by_its_oam_index() {
        let mut cart = cart();
        let mut ppu = Ppu::new();
        ppu.set_tile_trace(true);
        ppu.sprite_limit_removed = true;
        ppu.ctrl = 0x08;
        ppu.mask = 0x1E;
        for n in 0..9usize {
            ppu.oam[n * 4..n * 4 + 4].copy_from_slice(&[19, n as u8, 0x00, (20 + 20 * n) as u8]);
            for row in 0..8u16 {
                ppu.mem_write(cart.as_mut(), 0x1000 + n as u16 * 16 + row, 0xFF);
            }
        }
        for n in 9..64usize {
            ppu.oam[n * 4] = 0xFF;
        }
        run(&mut ppu, cart.as_mut(), 2);
        let plane = ppu.tile_trace().unwrap();
        // Sprite 8 (the 9th in OAM order) is the only one covering x = 180: its own tile, sprite palette 0.
        let t = plane[20 * 256 + 180];
        assert_eq!(t.layer, Layer::Sprite(8));
        assert_eq!(t.chr_addr, 0x1000 + 8 * 16);
        assert_eq!((t.palette, t.pixel), (4, 1));
        // Sprite 0's own pixel still reports sprite 0 and its own tile row.
        let t = plane[20 * 256 + 20];
        assert_eq!((t.layer, t.chr_addr), (Layer::Sprite(0), 0x1000));
        assert_eq!(plane[20 * 256 + 188].layer, Layer::Backdrop);
    }
}
