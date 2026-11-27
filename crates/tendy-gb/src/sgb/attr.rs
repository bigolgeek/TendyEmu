//! The 20×18 attribute map, its 45 uploaded files, and the packing rule between them
//! (Pan Docs, *Color Attribute Commands*).
//!
//! Pan Docs, *ATTR_TRN*: "Each ATF consists of 90 bytes, that are 5 bytes (20×2 bits) for each of
//! the 18 character lines of the Game Boy window. The two most significant bits of the first byte
//! define the color attribute (0-3) for the first character of the first line, the next two bits
//! the next character, and so on."
//!
//! So one entry is **two bits — a palette number 0-3 — per 8×8 cell**, laid out row-major, 20
//! cells across and 18 down. There is no separate BG/OBJ nibble: the four palettes are shared, and
//! a sprite takes the palette of the cell it is drawn into. The grid is the 160×144 **window's**
//! own, not the tile map's: cell `(0,0)` is screen pixel `(0,0)` whatever `SCX`/`SCY` say.

use crate::state::{StateError, StateReader, StateWriter};

/// Cells across the attribute map: 160 / 8.
pub const CELLS_WIDE: usize = 20;
/// Cells down the attribute map: 144 / 8.
pub const CELLS_DOWN: usize = 18;
/// Bytes in one attribute file: 20 × 18 × 2 bits = 90.
pub const ATF_BYTES: usize = 90;
/// Attribute files `ATTR_TRN` uploads: ATF0-ATF44 (`$00-$2C`), 45 × 90 = 4 050 bytes.
pub const ATF_FILES: usize = 45;
/// Bytes `ATTR_TRN` reads from VRAM: 4 050 of the 4 KiB block, the rest unused.
pub const ATF_FILES_BYTES: usize = ATF_FILES * ATF_BYTES;

/// All 45 attribute files and the one live map the renderer reads.
#[derive(Clone, PartialEq, Eq)]
pub struct AttrMap {
    /// ATF0 through ATF44, 90 bytes each, straight from `ATTR_TRN`'s VRAM block.
    files: Box<[u8; ATF_FILES_BYTES]>,
    /// The live map: what [`AttrMap::palette_at`] reads. `ATTR_SET`, and `PAL_SET` with its ATF
    /// bit, copy one of `files` here.
    live: [u8; ATF_BYTES],
}

impl AttrMap {
    /// Every cell on palette 0, no files uploaded.
    pub fn blank() -> AttrMap {
        AttrMap {
            files: Box::new([0; ATF_FILES_BYTES]),
            live: [0; ATF_BYTES],
        }
    }

    /// The 45 uploaded files, ATF0 first.
    pub fn files(&self) -> &[u8; ATF_FILES_BYTES] {
        &self.files
    }

    /// The live 90-byte map.
    pub fn live(&self) -> &[u8; ATF_BYTES] {
        &self.live
    }

    /// The palette number for cell `(cell_x, cell_y)`, 0-3.
    ///
    /// Out-of-range coordinates clamp to the last cell rather than panicking: the renderer passes
    /// `x / 8` and `ly / 8` for a 160×144 window, and a caller outside it is a bug worth seeing,
    /// but not worth a crash. The packing is Pan Docs' — MSB pair first.
    pub fn palette_at(&self, cell_x: usize, cell_y: usize) -> u8 {
        let x = cell_x.min(CELLS_WIDE - 1);
        let y = cell_y.min(CELLS_DOWN - 1);
        let cell = y * CELLS_WIDE + x;
        (self.live[cell / 4] >> (6 - 2 * (cell % 4))) & 0x03
    }

    /// Write one cell's palette number, 0-3 (`areas` is the only caller).
    ///
    /// The mirror of [`AttrMap::palette_at`], with the same packing: the cell's two bits are the
    /// pair at `6 - 2 * (entry % 4)` in byte `entry / 4`, and the other three cells in that byte
    /// keep their values. A palette above 3 is masked, not rejected (Pan Docs gives every palette
    /// field two bits; a control byte's unused high bits are masked the same way).
    ///
    /// A cell off the 20×18 grid is dropped rather than clamped: the area commands clamp their
    /// own geometry before they get here, and silently moving a write to the last column would
    /// hide a bug in that clamping.
    pub fn set_cell(&mut self, cell_x: usize, cell_y: usize, palette: u8) {
        if cell_x >= CELLS_WIDE || cell_y >= CELLS_DOWN {
            return;
        }
        let cell = cell_y * CELLS_WIDE + cell_x;
        let shift = 6 - 2 * (cell % 4);
        let byte = &mut self.live[cell / 4];
        *byte = (*byte & !(0x03 << shift)) | ((palette & 0x03) << shift);
    }

    /// `ATTR_TRN` (`$15`): 4 KiB of VRAM are ATF0 … ATF44, 90 bytes each, the last 46 unused.
    ///
    /// The live map is **not** touched. An upload changes the files; only `ATTR_SET` (or
    /// `PAL_SET`'s ATF bit) changes what the screen shows — Pan Docs keeps the two apart, and a
    /// cartridge that uploads and then never selects a file must not see a new grid.
    ///
    /// `vram` is the 4 KiB transfer block. A shorter slice is a caller bug, not a cartridge fault,
    /// so it is an assertion rather than silently padded.
    pub fn attr_trn(&mut self, vram: &[u8]) {
        assert!(
            vram.len() >= 4096,
            "ATTR_TRN reads the 4 KiB transfer block, got {} bytes",
            vram.len()
        );
        self.files.copy_from_slice(&vram[..ATF_FILES_BYTES]);
    }

    /// `ATTR_SET` (`$16`): copy attribute file `number` (`$00-$2C`) into the live map.
    ///
    /// A number above `$2C` is invalid per Pan Docs ("Values greater than $2C are invalid") and
    /// must be ignored, leaving the live map as it was. `Sgb::apply` masks the byte with `& 0x3F`
    /// first, so 45-63 arrive here and are the ones this branch drops.
    pub fn apply_file(&mut self, number: u8) {
        let file = usize::from(number);
        if file >= ATF_FILES {
            return;
        }
        let start = file * ATF_BYTES;
        self.live
            .copy_from_slice(&self.files[start..start + ATF_BYTES]);
    }

    /// Encode the files and the live map: 4 050 bytes then 90, in that order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.files[..]);
        w.bytes(&self.live);
    }

    /// Decode the files and the live map — the mirror of [`AttrMap::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.files[..])?;
        r.bytes_into(&mut self.live)?;
        Ok(())
    }
}

impl Default for AttrMap {
    fn default() -> Self {
        AttrMap::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `set_cell` is the writer half of the packing: it must change its cell's two bits and leave
    /// the other three cells of the same byte alone, and it must drop a cell off the grid.
    #[test]
    fn set_cell_writes_one_pair_and_keeps_its_neighbours() {
        let mut map = AttrMap::blank();
        // Cell (1,0) is entry 1: byte 0, bits 5-4.
        map.set_cell(1, 0, 0b10);
        assert_eq!(map.live()[0], 0b00_10_00_00);
        assert_eq!(map.palette_at(0, 0), 0);
        assert_eq!(map.palette_at(1, 0), 2);
        assert_eq!(map.palette_at(2, 0), 0);

        // Overwriting masks to two bits and touches no other pair: 0xFF -> 3 in bits 7-6 only.
        map.set_cell(0, 0, 0xFF);
        assert_eq!(map.live()[0], 0b11_10_00_00);
        assert_eq!(map.palette_at(0, 0), 3);
        assert_eq!(map.palette_at(1, 0), 2);

        // The last cell, entry 359, is byte 89 at bits 1-0.
        map.set_cell(19, 17, 3);
        assert_eq!(map.live()[89], 0b00_00_00_11);
        assert_eq!(map.palette_at(18, 17), 0);

        // Off the grid: dropped, not clamped, and nothing panics.
        map.set_cell(20, 0, 3);
        map.set_cell(0, 18, 3);
        assert_eq!(
            map.live()[0],
            0b11_10_00_00,
            "no write landed outside the grid"
        );
        assert_eq!(map.palette_at(19, 17), 3);
    }
}
