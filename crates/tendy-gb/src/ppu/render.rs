//! One scanline: background, window, the ten objects and the palette lookup.
//!
//! A line is drawn once, when the dot counter enters mode 3 — the scanline renderer the design
//! chose over a pixel FIFO — so a register written mid-line takes effect on the next line. Nothing
//! here allocates: the work buffers are 160-byte and 160-word stack arrays.
//!
//! **One pass, two consoles.** Each pixel is resolved to a *colour word*: the shade that
//! `BGP`/`OBP0`/`OBP1` gives the pixel's colour index on a DMG, or the RGB555 word palette RAM
//! holds at that index on a CGB. The DMG case is literally the same expression with the DMG's
//! 2-bit palette in place of the CGB's colour bank, so there is one renderer and the DMG's pixels
//! stay the bytes they were before C2 (`dmg-acid2` proves it). `frame` gets the index, `colours`
//! gets the word.
//!
//! **What C2 adds to the scan** (Pan Docs, *Tile Maps* and *OAM*):
//!
//! - The attribute byte in VRAM bank 1 beside each map entry: BG palette (bits 0-2), tile VRAM
//!   bank (bit 3), X/Y flip (bits 5-6) and "BG over OBJ" (bit 7).
//! - The object attribute's own palette (bits 0-2), tile bank (bit 3) and flips.
//! - `LCDC.0` no longer disables the background: in CGB mode that bit only *priorities* the BG
//!   over the objects, and the BG and window are always drawn.
//! - Object priority: OAM order alone on a CGB (`OPRI` bit 0 clear), lowest X then OAM order on a
//!   DMG or with `OPRI` set — and the BG/OBJ decision is the three-flag rule of *Tile Maps*.

use super::Ppu;
use crate::ppu::Mode;
use crate::{HEIGHT, OAM_SIZE, WIDTH};

/// An object as the line's scan needs it: its OAM index, screen position, tile index, attributes.
#[derive(Clone, Copy, Default)]
struct Object {
    index: u8,
    x: i32,
    y: i32,
    tile: u8,
    attr: u8,
}

impl Ppu {
    /// Draws line `ly` into `frame` and `colours`: background, window, then the objects, then the
    /// palette turns each colour index into a shade or an RGB555 colour (`Rendering`,
    /// `OAM`, `Window`, `Tile Maps`).
    pub(crate) fn render_line(&mut self) {
        if self.first_frame_blank {
            return;
        }
        let ly = self.ly as usize;
        if ly >= HEIGHT || self.mode != Mode::Drawing {
            return;
        }

        // The window's Y condition is a latch: any line where WY == LY sets it, VBlank clears it
        // (`Window`).
        if self.wy == self.ly {
            self.window_y = true;
        }

        let lcdc = self.lcdc;
        let cgb = self.cgb;
        let compat = self.compat;
        // Full CGB mode (attribute map, its own BG-priority meaning of LCDC.0); `compat` is the
        // compatibility-palette path for a monochrome cartridge, which keeps the DMG's meaning.
        let full_cgb = cgb && !compat;
        // In full CGB mode the BG and window are always displayed: LCDC bit 0 only decides whether
        // the BG wins over the objects (`LCDC`: "In CGB Mode this bit has a different meaning").
        // In DMG compatibility mode it keeps the DMG meaning and disables the BG when clear.
        let bg_enabled = full_cgb || lcdc & 0x01 != 0;
        let height: i32 = if lcdc & 0x04 != 0 { 16 } else { 8 };

        // The first ten objects in OAM order whose Y range covers this line (`OAM`).
        let mut objects = [Object::default(); 10];
        let mut count = 0usize;
        if lcdc & 0x02 != 0 && !self.oam_dma_active {
            for i in 0..(OAM_SIZE / 4) {
                let y = self.oam[i * 4] as i32 - 16;
                if ly as i32 >= y && (ly as i32) < y + height {
                    objects[count] = Object {
                        index: i as u8,
                        x: self.oam[i * 4 + 1] as i32 - 8,
                        y,
                        tile: self.oam[i * 4 + 2],
                        attr: self.oam[i * 4 + 3],
                    };
                    count += 1;
                    if count == 10 {
                        break;
                    }
                }
            }
        }

        // Window geometry: the per-line counter starts at 7, so WX = 7 is the left edge and
        // WX = 0 moves the whole window left by SCX % 8 (`Window`, §4.4).
        let window_active = bg_enabled && lcdc & 0x20 != 0 && self.window_y && self.wx <= 166;
        let win_start = self.wx as i32 - 7;
        let win_shift = if self.wx == 0 {
            (self.scx & 7) as i32
        } else {
            0
        };
        let bg_map = if lcdc & 0x08 != 0 { 0x1C00 } else { 0x1800 };
        let win_map = if lcdc & 0x40 != 0 { 0x1C00 } else { 0x1800 };
        let signed_data = lcdc & 0x10 == 0;
        let bg_y = ((self.scy as usize) + ly) & 0xFF;
        let win_y = self.window_line as usize;

        let mut row = [0u8; WIDTH];
        let mut words = [0u16; WIDTH];
        // The BG's colour index and its "BG over OBJ" bit, per pixel: the object pass needs both
        // and neither is recoverable from the shade.
        let mut bg_index = [0u8; WIDTH];
        let mut bg_over_obj = [false; WIDTH];
        let mut used_window = false;

        for (x, out) in row.iter_mut().enumerate() {
            let x_i = x as i32;
            let in_window = window_active && x_i >= win_start;

            // The map entry this pixel draws, the tile data it points at, and that tile's
            // attributes in VRAM bank 1 (`Tile Maps`, "BG Map Attributes").
            let (map_base, px, py) = if in_window {
                used_window = true;
                let px = (x_i - win_start + win_shift) as usize;
                (win_map, px, win_y)
            } else {
                let px = ((self.scx as usize) + x) & 0xFF;
                (bg_map, px, bg_y)
            };
            let entry = map_base + ((py >> 3) & 31) * 32 + (px >> 3);
            let tile = self.vram[entry] as usize;
            // The attribute byte beside the map entry, in VRAM bank 1. In compatibility mode the
            // attribute map reads as all zeros, so the BG takes palette 0 and no flips.
            let attr = if full_cgb { self.vram1[entry] } else { 0 };
            let data = if full_cgb && attr & 0x08 != 0 {
                &self.vram1
            } else {
                &self.vram
            };
            let base = if signed_data {
                (0x1000 + (tile as i8 as isize) * 16) as usize
            } else {
                tile * 16
            };
            let bit = if full_cgb && attr & 0x20 != 0 {
                px & 7
            } else {
                7 - (px & 7)
            };
            let line_in_tile = if full_cgb && attr & 0x40 != 0 {
                7 - (py & 7)
            } else {
                py & 7
            };
            let addr = base + line_in_tile * 2;
            let lo = data[addr];
            let hi = data[addr + 1];
            let index = if bg_enabled {
                (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1)
            } else {
                0
            };

            bg_index[x] = index;
            bg_over_obj[x] = full_cgb && attr & 0x80 != 0;
            if compat {
                // Compatibility mode: `BGP` indexes BG palette 0 instead of the shades of grey
                // (Pan Docs, "Compatibility palettes").
                let shade = (self.bgp >> (index * 2)) & 3;
                *out = index;
                words[x] = self.palettes.bg_colour(0, shade);
            } else if cgb {
                let word = self.palettes.bg_colour(attr & 0x07, index);
                *out = index;
                words[x] = word;
            } else {
                let shade = (self.bgp >> (index * 2)) & 3;
                *out = shade;
                words[x] = self
                    .sgb
                    .colour(x >> 3, ly >> 3, index)
                    .unwrap_or(u16::from(shade));
            }

            // Objects: the highest-priority non-transparent pixel wins, and precedence is by OAM
            // order alone on a CGB (Pan Docs, *OAM*, "In CGB mode, only the object's location in
            // OAM determines its priority") or by lowest X and then OAM order on a DMG or with
            // `OPRI` set to the DMG rule.
            let mut best: Option<Object> = None;
            let mut best_index = 0u8;
            for obj in &objects[..count] {
                if x_i < obj.x || x_i >= obj.x + 8 {
                    continue;
                }
                let mut line = ly as i32 - obj.y;
                if obj.attr & 0x40 != 0 {
                    line = height - 1 - line;
                }
                let mut col = x_i - obj.x;
                if obj.attr & 0x20 != 0 {
                    col = 7 - col;
                }
                let tile = if height == 16 {
                    obj.tile & 0xFE
                } else {
                    obj.tile
                };
                let data = if full_cgb && obj.attr & 0x08 != 0 {
                    &self.vram1
                } else {
                    &self.vram
                };
                let addr = tile as usize * 16 + (line as usize) * 2;
                let lo = data[addr];
                let hi = data[addr + 1];
                let bit = 7 - col;
                let pixel = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);
                if pixel == 0 {
                    continue;
                }
                let better = match &best {
                    None => true,
                    Some(current) => {
                        if cgb && !self.opri_dmg {
                            obj.index < current.index
                        } else {
                            obj.x < current.x || (obj.x == current.x && obj.index < current.index)
                        }
                    }
                };
                if better {
                    best = Some(*obj);
                    best_index = pixel;
                }
            }

            // The BG/OBJ decision, the three-flag rule of *Tile Maps*: an object pixel shows over
            // the BG when the BG's own colour index is 0, or when LCDC.0 clears the BG's priority
            // entirely, or when neither the object's nor the tile's "BG over OBJ" bit is set.
            if let Some(obj) = best {
                let shown = if cgb {
                    bg_index[x] == 0
                        || lcdc & 0x01 == 0
                        || (obj.attr & 0x80 == 0 && !bg_over_obj[x])
                } else {
                    obj.attr & 0x80 == 0 || bg_index[x] == 0
                };
                if shown {
                    if compat {
                        // Compatibility mode: `OBP0`/`OBP1` index OBJ palette 0 or 1, chosen by
                        // bit 4 of the object's attribute (Pan Docs, "Compatibility palettes").
                        let palette = if obj.attr & 0x10 != 0 { 1 } else { 0 };
                        let obp = if obj.attr & 0x10 != 0 {
                            self.obp1
                        } else {
                            self.obp0
                        };
                        let shade = (obp >> (best_index * 2)) & 3;
                        *out = best_index;
                        words[x] = self.palettes.obj_colour(palette, shade);
                    } else if cgb {
                        *out = best_index;
                        words[x] = self.palettes.obj_colour(obj.attr & 0x07, best_index);
                    } else {
                        let palette = if obj.attr & 0x10 != 0 {
                            self.obp1
                        } else {
                            self.obp0
                        };
                        let shade = (palette >> (best_index * 2)) & 3;
                        *out = shade;
                        words[x] = self
                            .sgb
                            .colour(x >> 3, ly >> 3, best_index)
                            .unwrap_or(u16::from(shade));
                    }
                }
            }
        }

        let start = ly * WIDTH;
        self.frame[start..start + WIDTH].copy_from_slice(&row);
        self.colours[start..start + WIDTH].copy_from_slice(&words);

        if used_window {
            self.window_line = self.window_line.wrapping_add(1);
        }
    }
}
