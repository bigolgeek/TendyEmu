//! Four visible palettes of four RGB555 colours, and the 512 system palettes `PAL_TRN` fills
//! (Pan Docs, *Palette Commands*).
//!
//! **The clean-room line.** Pan Docs documents that "System color palette memory contains 512
//! pre-defined palettes" without printing them: those 512 × 4 words exist only inside a dumped SGB
//! BIOS, so this project does not have them and does not pretend to. What this file ships is a
//! table we chose ourselves ([`DEFAULT_RAMP`], repeated 512 times). A game that sends `PAL_TRN`
//! replaces it with data from its own VRAM; a game that calls `PAL_SET` without `PAL_TRN` gets our
//! ramp, which is grey.
//!
//! **The store is shared by BG and OBJ.** The attribute map names one palette per 8×8 cell and Pan
//! Docs' palette commands set four palettes for the whole screen, so there is one array of four,
//! not a BG set and an OBJ set.

use crate::sgb::Packet;
use crate::state::{StateError, StateReader, StateWriter};

/// How many palettes the screen has (Pan Docs: "four of these "logical" palettes to actual visible
/// "physical" SGB palettes").
pub const VISIBLE_PALETTES: usize = 4;
/// Colours in one palette.
pub const PALETTE_COLOURS: usize = 4;
/// Palettes `PAL_TRN` uploads: "System color palette memory contains 512 pre-defined palettes".
pub const SYSTEM_PALETTES: usize = 512;
/// Bytes `PAL_TRN` reads from VRAM: 512 palettes × 4 colours × 2 bytes.
pub const SYSTEM_PALETTE_BYTES: usize = SYSTEM_PALETTES * PALETTE_COLOURS * 2;

/// Our clean-room default palette: four evenly spaced greys, `$0000`, `$294A`, `$56B5`, `$7FFF`.
///
/// These are **our** values, not SGB BIOS data: each channel takes the 5-bit steps 0, 10, 21, 31
/// (`0x00`, `0x0A`, `0x15`, `0x1F`) so the ramp is evenly spaced in the SNES's 15-bit colour space.
/// A palette the cartridge sends replaces it; until then this is what a pixel looks like.
pub const DEFAULT_RAMP: [u16; PALETTE_COLOURS] = [0x7FFF, 0x56B5, 0x294A, 0x0000];

/// The four visible palettes and the 512 system palettes.
#[derive(Clone, PartialEq, Eq)]
pub struct SgbPalettes {
    /// The four palettes the picture uses, palette 0 first.
    visible: [[u16; PALETTE_COLOURS]; VISIBLE_PALETTES],
    /// `PAL_TRN`'s 512 × 4 words. Boxed: 4 KiB that only `PAL_SET` reads.
    system: Box<[[u16; PALETTE_COLOURS]; SYSTEM_PALETTES]>,
    /// `PAL_PRI`'s byte: `$00` until the cartridge sends one.
    priority: u8,
}

impl SgbPalettes {
    /// Our default: four copies of [`DEFAULT_RAMP`], 512 system palettes of the same, `PAL_PRI`
    /// off.
    pub fn new() -> SgbPalettes {
        SgbPalettes {
            visible: [DEFAULT_RAMP; VISIBLE_PALETTES],
            system: Box::new([DEFAULT_RAMP; SYSTEM_PALETTES]),
            priority: 0,
        }
    }

    /// The four visible palettes, palette 0 first.
    pub fn visible(&self) -> &[[u16; PALETTE_COLOURS]; VISIBLE_PALETTES] {
        &self.visible
    }

    /// The 512 system palettes `PAL_SET` copies out of.
    pub fn system(&self) -> &[[u16; PALETTE_COLOURS]; SYSTEM_PALETTES] {
        &self.system
    }

    /// One colour of one visible palette. `palette` and `index` are masked to 0-3, which is the
    /// range both callers can produce; this never panics on a bad attribute byte.
    pub fn colour(&self, palette: u8, index: u8) -> u16 {
        self.visible[usize::from(palette) & 3][usize::from(index) & 3]
    }

    /// `PAL_PRI` (`$19`): byte 1, stored as sent. Pan Docs calls this "System Palette Priority";
    /// this project draws nothing differently for it, and the byte is kept so a save state
    /// round-trips what the cartridge wrote.
    pub fn set_priority(&mut self, byte: u8) {
        self.priority = byte;
    }

    /// `PAL_PRI`'s byte as sent.
    pub fn priority(&self) -> u8 {
        self.priority
    }

    /// `PAL01`/`PAL23`/`PAL03`/`PAL12` (`$00`-`$03`), for the pair `first` and `second`.
    ///
    /// Pan Docs' byte table, with one detail the prose states and the table only implies: bytes 1-2
    /// are colour 0 of **both** palettes ("for SGB palette 0, color 0-3, and for SGB palette 1,
    /// color 1-3 (without separate color 0)"). Bytes 3-8 are colours 1-3 of `first`, bytes 9-14 are
    /// colours 1-3 of `second`, byte 15 is unused. Every word is little-endian.
    pub fn set_pair(&mut self, first: usize, second: usize, packet: &Packet) {
        // `first` and `second` are the dispatch's own pairs — `(0,1)`, `(2,3)`, `(0,3)`,
        // `(1,2)` — so the mask only keeps a caller mistake from indexing out of bounds.
        let (first, second) = (first & 3, second & 3);
        let word = |at: usize| u16::from(packet.bytes[at]) | (u16::from(packet.bytes[at + 1]) << 8);

        // Bytes 1-2: colour 0, for both palettes (Pan Docs: "for SGB palette 0, color 0-3, and
        // for SGB palette 1, color 1-3 (without separate color 0)").
        let shared = word(1);
        self.visible[first][0] = shared;
        self.visible[second][0] = shared;
        // Bytes 3-8: colours 1-3 of `first`. Bytes 9-14: colours 1-3 of `second`.
        for colour in 1..PALETTE_COLOURS {
            self.visible[first][colour] = word(1 + colour * 2);
            self.visible[second][colour] = word(7 + colour * 2);
        }
    }

    /// `PAL_SET` (`$0A`).
    ///
    /// Bytes 1-2, 3-4, 5-6 and 7-8 are four system-palette IDs, little-endian; byte 9 is the flags
    /// byte. This function copies the four system palettes into the four visible ones. The flags
    /// byte's ATF bit is **not** this function's: `Sgb::apply` reads it and calls
    /// [`crate::sgb::AttrMap::apply_file`] and [`crate::sgb::Sgb::set_mask`].
    pub fn pal_set(&mut self, packet: &Packet) {
        for (slot, palette) in self.visible.iter_mut().enumerate() {
            let id = (usize::from(packet.bytes[1 + slot * 2])
                | (usize::from(packet.bytes[2 + slot * 2]) << 8))
                // Pan Docs gives the ID as 0-511; a cartridge can send 16 bits, and an ID above
                // the 512 the box holds wraps to its low nine bits rather than reading out of it.
                & (SYSTEM_PALETTES - 1);
            *palette = self.system[id];
        }
    }

    /// `PAL_TRN` (`$0B`): 4 KiB of VRAM are 512 palettes of 4 little-endian RGB555 words.
    ///
    /// `vram` is the 4 KiB block the transfer reads ([`crate::ppu::Ppu::apply_sgb_packet`] picks
    /// the base LCDC selects). A slice that is not 4 096 bytes
    /// long is a programming error in the caller, not a cartridge fault.
    pub fn pal_trn(&mut self, vram: &[u8]) {
        assert!(
            vram.len() >= SYSTEM_PALETTE_BYTES,
            "PAL_TRN reads 4 096 bytes of the transfer block, got {} bytes",
            vram.len()
        );
        for (index, colour) in self.system.iter_mut().flatten().enumerate() {
            let at = index * 2;
            *colour = u16::from(vram[at]) | (u16::from(vram[at + 1]) << 8);
        }
    }

    /// Encode the palettes: 32 bytes of visible colours, 4 096 of the system box, then the
    /// `PAL_PRI` byte — the same order and widths for every machine, so a snapshot's length does
    /// not depend on what a cartridge happened to send.
    pub fn save_state(&self, w: &mut StateWriter) {
        for colour in self.visible.iter().flatten() {
            w.u16(*colour);
        }
        for colour in self.system.iter().flatten() {
            w.u16(*colour);
        }
        w.u8(self.priority);
    }

    /// Decode the palettes — the mirror of [`SgbPalettes::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        for colour in self.visible.iter_mut().flatten() {
            *colour = r.u16()?;
        }
        for colour in self.system.iter_mut().flatten() {
            *colour = r.u16()?;
        }
        self.priority = r.u8()?;
        Ok(())
    }
}

impl Default for SgbPalettes {
    fn default() -> Self {
        SgbPalettes::new()
    }
}
