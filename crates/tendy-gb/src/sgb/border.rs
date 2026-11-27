//! Where the 160×144 window sits in the bordered picture, `MASK_EN`, and the border itself
//! (Pan Docs, *MASK_EN* and *Border and OBJ Commands*).
//!
//! **Geometry, and where each number comes from.** Pan Docs, *PCT_TRN*: "The 32×28 map entries
//! correspond to 256×224 pixels of the Super NES screen. The 20×18 entries in the center of the
//! 32×28 area should be set to a blank (solid color 0) tile as transparent space for the Game Boy
//! window to be displayed inside." So the bordered picture is **256×224**, the window is
//! **160×144**, and "in the center" puts its top-left at **48, 40** —
//! `(256 - 160) / 2` and `(224 - 144) / 2`, both of which are whole 8×8 tiles: the window starts at
//! tile **6, 5** of the 32×28 map. Nothing here is remembered: every number is that sentence read
//! twice.
//!
//! **`MASK_EN`** has four values and three states. Pan Docs: "0 Cancel Mask (Display activated),
//! 1 Freeze Screen (Keep displaying current picture), 2 Blank Screen (Black), 3 Blank Screen
//! (Color 0)". 2 and 3 are the same state with two different colours, which is why
//! [`MaskState::Blanked`] carries the colour it blanks to. Pan Docs also notes that "SGB
//! freezes/unfreezes the Game Boy screen automatically whenever the LCD is turned off/on", and
//! that a freeze only works "if the SNES has stored a picture" — a freeze on the frame the window
//! was first shown keeps that frame, which is what "keep displaying current picture" means here.

use crate::state::{StateError, StateReader, StateWriter};

/// Width of the bordered picture: Pan Docs' 256 pixels of Super NES screen.
pub const SGB_FRAME_WIDTH: usize = 256;
/// Height of the bordered picture: Pan Docs' 224 pixels of Super NES screen.
pub const SGB_FRAME_HEIGHT: usize = 224;
/// Pixels in the bordered picture.
pub const SGB_FRAME_PIXELS: usize = SGB_FRAME_WIDTH * SGB_FRAME_HEIGHT;
/// Left edge of the Game Boy window: `(256 - 160) / 2`, tile 6 across.
pub const WINDOW_X: usize = 48;
/// Top edge of the Game Boy window: `(224 - 144) / 2`, tile 5 down.
pub const WINDOW_Y: usize = 40;
/// Width of the Game Boy window.
pub const WINDOW_WIDTH: usize = crate::WIDTH;
/// Height of the Game Boy window.
pub const WINDOW_HEIGHT: usize = crate::HEIGHT;

/// Tiles a border can name: Pan Docs, *CHR_TRN*'s destination byte, "0=Tiles $00-$7F, 1=Tiles
/// $80-$FF" — two 4 KiB transfers fill the 256 an 8-bit character number can reach.
pub const BORDER_TILES: usize = 256;
/// Bytes in one tile: Pan Docs, *CHR_TRN*: "Each tile occupies 32 bytes (8×8 pixels, 16 colors
/// each)."
pub const BORDER_TILE_BYTES: usize = 32;
/// Bytes one `CHR_TRN` transfer carries: "Bitmap data for 128 Tiles" at 32 bytes each.
pub const BORDER_TRANSFER_BYTES: usize = 128 * BORDER_TILE_BYTES;
/// Map columns: Pan Docs, *PCT_TRN*: "BG Map 32×28 Entries of 16 bits each (1792 bytes)".
pub const BORDER_MAP_COLS: usize = 32;
/// Map rows: the same sentence. 28 rows of 8 pixels is the picture's 224.
pub const BORDER_MAP_ROWS: usize = 28;
/// Bytes the map fills: 32 × 28 × 2.
pub const BORDER_MAP_BYTES: usize = BORDER_MAP_COLS * BORDER_MAP_ROWS * 2;
/// Border palettes: Pan Docs, *PCT_TRN*: "BG Palette Data (Palettes 4-6, 16 little-endian RGB555
/// colors each)" — three of them, 96 bytes, at `$800-$85F` of the transfer.
pub const BORDER_PALETTES: usize = 3;
/// Colours in one border palette.
pub const BORDER_PALETTE_COLOURS: usize = 16;
/// Where `PCT_TRN`'s palette data starts in its 4 KiB: `$800`.
pub const BORDER_PALETTE_OFFSET: usize = 0x800;

/// `MASK_EN`'s three states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskState {
    /// `MASK_EN` 0: the window is drawn every frame.
    Active,
    /// `MASK_EN` 1: the last composed picture stays on screen; new frames are not drawn into it.
    Frozen,
    /// `MASK_EN` 2 and 3: the window shows one colour. 2 is black (`$0000`), 3 is palette 0's
    /// colour 0, resolved when the command arrives.
    Blanked(u16),
}

impl MaskState {
    /// Encode `MASK_EN`'s state.
    ///
    /// One tag byte — 0 `Active`, 1 `Frozen`, 2 `Blanked` — then, only for `Blanked`, the word it
    /// fills the window with. Frozen carries no word: "keep displaying current picture" is the
    /// picture already in the composed frame, which is not this value's business.
    pub fn save_state(&self, w: &mut StateWriter) {
        match *self {
            MaskState::Active => w.u8(0),
            MaskState::Frozen => w.u8(1),
            MaskState::Blanked(word) => {
                w.u8(2);
                w.u16(word);
            }
        }
    }

    /// Decode `MASK_EN`'s state. An unknown tag is a corrupt state, not a value to guess
    /// at: [`StateError::BadValue`], and `self` is left alone.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        *self = match r.u8()? {
            0 => MaskState::Active,
            1 => MaskState::Frozen,
            2 => MaskState::Blanked(r.u16()?),
            _ => return Err(StateError::BadValue),
        };
        Ok(())
    }
}

/// The border: 256×224 RGB555 pixels — ours and authored in this repository until a
/// cartridge sends `PCT_TRN`, then the cartridge's own.
///
/// **The game's own border.** Pan Docs, *CHR_TRN*: "The tile data is sent by VRAM transfer
/// (4 KiB)" and byte 1 bit 0 picks the half — "0=Tiles $00-$7F, 1=Tiles $80-$FF" — so two
/// transfers fill the 256 tiles. *PCT_TRN* then sends the map and the palettes the map names:
/// "BG Map 32×28 Entries of 16 bits each (1792 bytes)" and "BG Palette Data (Palettes 4-6, 16
/// little-endian RGB555 colors each)". Tiles alone change nothing: a cartridge that has not said
/// where the tiles go keeps the placeholder picture.
///
/// **Colour 0 is transparent.** Pan Docs, *PCT_TRN*: "The 20×18 entries in the center of the 32×28
/// area should be set to a blank (solid color 0) tile as transparent space for the Game Boy window
/// to be displayed inside. Non-transparent border data will cover the Game Boy window." A tile
/// pixel whose 4-bit colour index is 0 therefore paints nothing and the word stays `$0000`;
/// [`SgbFrame::compose`] lets the Game Boy's picture through exactly those pixels.
///
/// shortcut: `$0000` at a nonzero index (palette colour 0 of `$0000`, an opaque black pixel) is
/// indistinguishable from that hole and is drawn as transparent too; upgrade if a cartridge needs
/// opaque black over the window.
#[derive(Clone, PartialEq, Eq)]
pub struct Border {
    /// 256×224 words, row-major: the picture drawn from `tiles`+`map`+`palettes`, or the
    /// placeholder art while `has_map` is false.
    pixels: Vec<u16>,
    /// 256 tiles, 32 bytes each, 4 bits per pixel.
    tiles: Box<[u8; BORDER_TILES * BORDER_TILE_BYTES]>,
    /// The 32×28 map, two little-endian bytes an entry.
    map: Box<[u8; BORDER_MAP_BYTES]>,
    /// Palettes 4, 5 and 6 — the only ones a map entry may name.
    palettes: Box<[[u16; BORDER_PALETTE_COLOURS]; BORDER_PALETTES]>,
    /// True once a `PCT_TRN` has arrived: the map exists, so the picture is the cartridge's.
    has_map: bool,
}

impl Border {
    /// Our border, until a cartridge replaces it.
    ///
    /// The art is this repository's own: Pan Docs fixes only the picture's size (256×224) and where
    /// the window sits (48, 40), and never shows a border, since the built-in ones live in the SGB
    /// BIOS. So the design is a frame of two 8-pixel rings — blue `$7C00` at the
    /// outermost edge, green `$03E0` from 8 to 15 pixels in — with a white `$7FFF` 16×16 block in
    /// each of the four corners one ring in, at (16,16), (224,16), (16,184) and (224,184).
    ///
    /// "Distance to the nearest edge" of `(x, y)` is `min(x, 255 - x, y, 223 - y)`, so the blue
    /// ring is every pixel whose distance is under 8 and the green ring 8 to 15. Everything else,
    /// the whole 160×144 window at (48, 40) included, stays `$0000`: the compositor paints the
    /// Game Boy's picture over it, and a masked or frozen frame must have nothing of the border
    /// showing through the window.
    ///
    /// Pure: no clock, no randomness, no file, no static — the same call draws the same picture.
    /// No tiles, map or palettes: this value has not heard from a cartridge.
    pub fn new() -> Border {
        Border {
            pixels: placeholder_pixels(),
            tiles: Box::new([0; BORDER_TILES * BORDER_TILE_BYTES]),
            map: Box::new([0; BORDER_MAP_BYTES]),
            palettes: Box::new([[0; BORDER_PALETTE_COLOURS]; BORDER_PALETTES]),
            has_map: false,
        }
    }

    /// A border from 256×224 RGB555 words, row-major (the compositor tests use it for a marker
    /// border).
    ///
    /// The tiles, map and palettes stay empty and `has_map` false, so these pixels are the caller's
    /// and not something a state could rebuild: a save state restores the cartridge's border, and
    /// a `from_pixels` border that is saved comes back as the placeholder.
    ///
    /// # Panics
    ///
    /// If `pixels` is not [`SGB_FRAME_PIXELS`] long: a border of another size cannot be composited,
    /// and every caller in this project is code, not a cartridge.
    pub fn from_pixels(pixels: Vec<u16>) -> Border {
        assert_eq!(
            pixels.len(),
            SGB_FRAME_PIXELS,
            "a border is {SGB_FRAME_WIDTH}x{SGB_FRAME_HEIGHT}"
        );
        Border {
            pixels,
            ..Border::new()
        }
    }

    /// The border's pixels, row-major, 256 to a row.
    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    /// Whether a `PCT_TRN` has arrived: true means the picture is the cartridge's, not the
    /// built-in art.
    pub fn has_map(&self) -> bool {
        self.has_map
    }

    /// `CHR_TRN` (`$13`): 128 of the 256 tiles from 4 KiB of VRAM.
    ///
    /// Pan Docs, *CHR_TRN*'s byte 1: "Bit 0 - Tile Numbers (0=Tiles $00-$7F, 1=Tiles $80-$FF)",
    /// so `destination & 1` picks the 4 KiB half of the 8 KiB tile store. Bit 1 (BG/OBJ) is
    /// ignored, as Pan Docs notes it "seems to have no effect and writes to the same VRAM
    /// addresses for both BG and OBJ". The picture is redrawn only once a map exists — tiles
    /// nobody has placed would not be visible anyway.
    ///
    /// `vram` is the 4 KiB transfer block. A shorter slice is a caller bug, not a cartridge fault,
    /// so it is an assertion rather than a silent pad.
    pub fn chr_trn(&mut self, destination: u8, vram: &[u8]) {
        assert!(
            vram.len() >= BORDER_TRANSFER_BYTES,
            "CHR_TRN reads the 4 KiB transfer block, got {} bytes",
            vram.len()
        );
        let start = usize::from(destination & 1) * BORDER_TRANSFER_BYTES;
        self.tiles[start..start + BORDER_TRANSFER_BYTES]
            .copy_from_slice(&vram[..BORDER_TRANSFER_BYTES]);
        if self.has_map {
            self.render();
        }
    }

    /// `PCT_TRN` (`$14`): the 32×28 map and palettes 4-6, by 4 KiB VRAM transfer.
    ///
    /// Pan Docs, *PCT_TRN*'s layout: `000-6FF` the map, `740-7FF` "Not used, don't care",
    /// `800-85F` the palettes. The `700-73F` row is a second screen's row 28 ("BG Map 1×28 extra
    /// row") and is not part of the 256×224 picture, so it is not read.
    ///
    /// The map is what makes the picture the cartridge's: before this command the border is
    /// the art, whatever tiles arrived.
    pub fn pct_trn(&mut self, vram: &[u8]) {
        assert!(
            vram.len() >= BORDER_TRANSFER_BYTES,
            "PCT_TRN reads the 4 KiB transfer block, got {} bytes",
            vram.len()
        );
        self.map.copy_from_slice(&vram[..BORDER_MAP_BYTES]);
        for (index, colour) in self.palettes.iter_mut().flatten().enumerate() {
            let at = BORDER_PALETTE_OFFSET + index * 2;
            *colour = u16::from(vram[at]) | (u16::from(vram[at + 1]) << 8);
        }
        self.has_map = true;
        self.render();
    }

    /// Redraw the 256×224 picture from the map and the tiles.
    ///
    /// Pan Docs, *CHR_TRN*: each tile is "stored in 4-bit-per-pixel format consisting of bit planes
    /// 0 and 1 interleaved by row, followed by bit planes 2 and 3 interleaved by row" — bytes `2r`
    /// and `2r+1` of the tile are planes 0 and 1 of row `r`, bytes `16 + 2r` and `16 + 2r + 1` are
    /// planes 2 and 3. A map entry is `VH01 PP00 NNNN NNNN` with "Bit 0-9 Character Number ... Bit
    /// 10-12 Palette Number ... Bit 14 X-Flip ... Bit 15 Y-Flip"; the palette field may name only
    /// 4-6.
    fn render(&mut self) {
        self.pixels.fill(0);
        for row in 0..BORDER_MAP_ROWS {
            for col in 0..BORDER_MAP_COLS {
                let at = (row * BORDER_MAP_COLS + col) * 2;
                let entry = u16::from(self.map[at]) | (u16::from(self.map[at + 1]) << 8);
                // "use only $00-$FF, upper 2 bits zero": the 8 KiB store holds 256 tiles.
                let tile = usize::from(entry as u8);
                // "Palette Number (use only 4-6)"; a cartridge outside that range gets palette 4.
                let palette = match (entry >> 10) & 0x07 {
                    4..=6 => usize::from(((entry >> 10) & 0x07) - 4),
                    _ => 0,
                };
                let colours = self.palettes[palette];
                let base = tile * BORDER_TILE_BYTES;
                for r in 0..8 {
                    let tile_row = if entry & 0x8000 != 0 { 7 - r } else { r };
                    let planes01 = base + tile_row * 2;
                    let planes23 = base + 16 + tile_row * 2;
                    for c in 0..8 {
                        let tile_col = if entry & 0x4000 != 0 { 7 - c } else { c };
                        let bit = 7 - tile_col;
                        let index = ((self.tiles[planes01] >> bit) & 1)
                            | (((self.tiles[planes01 + 1] >> bit) & 1) << 1)
                            | (((self.tiles[planes23] >> bit) & 1) << 2)
                            | (((self.tiles[planes23 + 1] >> bit) & 1) << 3);
                        // Colour 0 is the transparent hole the window shows through.
                        if index != 0 {
                            let pixel = (row * 8 + r) * SGB_FRAME_WIDTH + col * 8 + c;
                            self.pixels[pixel] = colours[usize::from(index)];
                        }
                    }
                }
            }
        }
    }

    /// Encode the border: whether a map arrived, then the 8 KiB of tiles, the 1 792-byte map and
    /// the three palettes. The pixels are derived, so they are not written.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.has_map);
        w.bytes(&self.tiles[..]);
        w.bytes(&self.map[..]);
        for colour in self.palettes.iter().flatten() {
            w.u16(*colour);
        }
    }

    /// Decode the border and redraw its pixels — the mirror of [`Border::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.has_map = r.bool()?;
        r.bytes_into(&mut self.tiles[..])?;
        r.bytes_into(&mut self.map[..])?;
        for colour in self.palettes.iter_mut().flatten() {
            *colour = r.u16()?;
        }
        if self.has_map {
            self.render();
        } else {
            self.pixels = placeholder_pixels();
        }
        Ok(())
    }
}

/// The art, the picture a border shows until a cartridge sends its own.
fn placeholder_pixels() -> Vec<u16> {
    let mut pixels = vec![0u16; SGB_FRAME_PIXELS];
    for y in 0..SGB_FRAME_HEIGHT {
        for x in 0..SGB_FRAME_WIDTH {
            let ring = x
                .min(SGB_FRAME_WIDTH - 1 - x)
                .min(y)
                .min(SGB_FRAME_HEIGHT - 1 - y);
            pixels[y * SGB_FRAME_WIDTH + x] = if ring < 8 {
                0x7C00
            } else if ring < 16 {
                0x03E0
            } else {
                0x0000
            };
        }
    }
    // The blocks are drawn last, so they sit over the green ring they overlap. Their top-left
    // corners are the ring's inner edge, 16 pixels in from each side.
    for &(cx, cy) in &[(16, 16), (224, 16), (16, 184), (224, 184)] {
        for y in cy..cy + 16 {
            for x in cx..cx + 16 {
                pixels[y * SGB_FRAME_WIDTH + x] = 0x7FFF;
            }
        }
    }
    pixels
}

impl Default for Border {
    fn default() -> Self {
        Border::new()
    }
}

/// One bordered frame: the border with the Game Boy window composited into it.
///
/// This is the last step before a front end shows a picture; nothing in the emulator calls it, and
/// `src/display.rs`'s picture size is untouched.
#[derive(Clone, PartialEq, Eq)]
pub struct SgbFrame {
    pixels: Vec<u16>,
}

impl SgbFrame {
    /// A black 256×224 picture.
    pub fn new() -> SgbFrame {
        SgbFrame {
            pixels: vec![0; SGB_FRAME_PIXELS],
        }
    }

    /// The composed picture, row-major, 256 to a row.
    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    /// Draw one frame: `border` under, `gb` (23 040 words) into the window, `mask` deciding
    /// whether the window is drawn, the previous picture kept, or one colour shown.
    ///
    /// `Frozen` writes nothing at all, which is exactly "keep displaying current picture". A
    /// `gb` slice that is not 23 040 words is a caller bug, not a cartridge fault.
    ///
    /// The window's first word is `pixels[WINDOW_Y * SGB_FRAME_WIDTH + WINDOW_X]`, border pixel
    /// `(47, 40)` to its left and `(48, 39)` above it, so the offset is 48, 40 and not an
    /// 8-pixel one. Every pixel of `pixels` is written in an `Active` or `Blanked` frame: the
    /// border copy covers the whole picture, so no row is left half-initialised.
    ///
    /// **Transparency.** Pan Docs, *PCT_TRN*: "Non-transparent border data will cover the Game Boy
    /// window." So in an `Active` frame a window pixel is written only where the border holds
    /// `$0000` — the hole a colour-0 tile pixel leaves ([`Border`]) — and an opaque border pixel
    /// keeps its own word. A `Blanked` frame fills the whole rectangle: the mask is what it is.
    pub fn compose(&mut self, border: &Border, gb: &[u16], mask: MaskState) {
        // "Keep displaying current picture": the composed frame is not touched, not even by the
        // border, or a frozen frame would lose the window the next border painted over it.
        if mask == MaskState::Frozen {
            return;
        }
        assert_eq!(
            gb.len(),
            WINDOW_WIDTH * WINDOW_HEIGHT,
            "the Game Boy window is {WINDOW_WIDTH}x{WINDOW_HEIGHT} words"
        );
        self.pixels.copy_from_slice(border.pixels());
        match mask {
            // Returned above; listed so a new state cannot be added silently.
            MaskState::Frozen => {}
            // `MASK_EN` 2 and 3: one colour over the window, the border untouched.
            MaskState::Blanked(colour) => {
                for y in WINDOW_Y..WINDOW_Y + WINDOW_HEIGHT {
                    let row = &mut self.pixels[y * SGB_FRAME_WIDTH + WINDOW_X..][..WINDOW_WIDTH];
                    row.fill(colour);
                }
            }
            // `MASK_EN` 0: row `y` of `gb` into row `WINDOW_Y + y`, at column `WINDOW_X`, only
            // where the border left that pixel transparent.
            MaskState::Active => {
                for y in 0..WINDOW_HEIGHT {
                    let src = &gb[y * WINDOW_WIDTH..y * WINDOW_WIDTH + WINDOW_WIDTH];
                    let row = &mut self.pixels[(WINDOW_Y + y) * SGB_FRAME_WIDTH + WINDOW_X..]
                        [..WINDOW_WIDTH];
                    for (dst, &word) in row.iter_mut().zip(src) {
                        if *dst == 0 {
                            *dst = word;
                        }
                    }
                }
            }
        }
    }
}

impl Default for SgbFrame {
    fn default() -> Self {
        SgbFrame::new()
    }
}
