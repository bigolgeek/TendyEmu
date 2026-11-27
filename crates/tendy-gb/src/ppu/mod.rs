//! The PPU: dots, modes, `LY`/`LYC`/`STAT`, register access, VRAM/OAM gating and the LCD on/off
//! rules. `render.rs` draws one scanline when the dot counter enters
//! mode 3.
//!
//! The dot clock is real and one-dot granular: `tick(4)` — the M-cycle the bus hands it — walks
//! four dots so a mode change and the `STAT` line land on the dot Pan Docs gives them. Nothing
//! here allocates after `Ppu::new`.
//!
//! **Two picture formats, one buffer per format (C2).** The scanline renderer produces a *colour
//! word* per pixel: on a DMG that word is the shade index `BGP`/`OBP0`/`OBP1` selected, and on a
//! CGB it is the RGB555 colour out of palette RAM. `frame` keeps the shade index (DMG) or the
//! pre-palette colour index (CGB), which is what the checksum folded before C2 and what a caller
//! that only wants "which of four levels" reads; `colours` holds the words themselves and is the
//! one the front end turns into RGBA. `Ppu::cgb` says which of the two is live, and is set from
//! the bus every machine cycle so a `KEY0` write mid-frame is honoured on the next line.

use crate::cgb::Palettes;
use crate::sgb::{MaskState, Sgb};
use crate::state::{StateError, StateReader, StateWriter};

mod render;

use crate::bus::{IF_STAT, IF_VBLANK};
use crate::{
    DOTS_PER_FRAME, DOTS_PER_LINE, FRAME_PIXELS, LINES_PER_FRAME, OAM_SIZE, VBLANK_LINE, VRAM_SIZE,
};

/// Dots mode 2 (OAM scan) occupies at the start of every visible line (`Rendering`).
const MODE2_DOTS: u32 = 80;
/// Dots mode 3 (drawing) occupies before its penalties: Pan Docs' 172-289 lower bound
/// (`Rendering`, "Mode 3 length"). `SCX % 8` and the OBJ penalty are added per line.
const MODE3_DOTS: u32 = 172;

/// The four PPU modes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Drawing = 3,
}

/// The LCD and its memories.
#[derive(Clone)]
pub struct Ppu {
    /// VRAM bank 0: tile data at `$8000-$97FF` and the two tile maps at `$9800-$9BFF` /
    /// `$9C00-$9FFF`.
    pub vram: [u8; VRAM_SIZE],
    /// VRAM bank 1 (CGB only): tile data at `$8000-$97FF`, and the *attribute* map for each of the
    /// two tile maps at `$9800-$9BFF` / `$9C00-$9FFF` (Pan Docs, *Tile Maps*, "BG Map Attributes").
    /// Both banks are addressed by `VBK`; a DMG cartridge cannot select it, so this stays empty.
    pub vram1: [u8; VRAM_SIZE],
    pub oam: [u8; OAM_SIZE],
    /// 23 040 shade indices (DMG) or pre-palette colour indices (CGB), reused every frame.
    pub frame: Vec<u8>,
    /// 23 040 colour words, one per pixel: the shade index on a DMG and the RGB555 colour on a CGB.
    /// Filled by the same scanline pass as `frame`, so the two can never disagree.
    pub colours: Vec<u16>,
    /// The CGB's palette RAM, sampled by the renderer in CGB mode.
    pub palettes: Palettes,
    /// The Super Game Boy's four palettes, its attribute map and `MASK_EN`'s state.
    ///
    /// It sits here, beside `palettes`, because the scanline renderer is where an SGB pixel gets its
    /// colour: `render_line` already resolves one pixel to one word, and reaching SGB state through
    /// any other owner would mean a new argument through `Ppu::tick`. Every machine this tree builds
    /// holds `Sgb::off()`, whose `colour` keeps the DMG shade path.
    pub sgb: Sgb,
    /// True while this PPU draws in colour: a CGB cartridge in CGB mode, or a monochrome cartridge
    /// on a CGB console. Set from the bus on every machine cycle.
    pub cgb: bool,
    /// True while the colour comes from the compatibility palette (a monochrome cartridge on a
    /// CGB console) rather than the attribute map: `BGP`/`OBP0`/`OBP1` index the compatibility
    /// triplet instead of shades of grey. Only meaningful when [`Ppu::cgb`] is true.
    pub compat: bool,
    /// `OPRI` (`$FF6C`) bit 0: the object-priority rule for a CGB frame. 0 = the CGB's (OAM order),
    /// 1 = the DMG's (lowest X, then OAM order).
    pub opri_dmg: bool,
    pub lcdc: u8,
    pub stat: u8,
    pub scy: u8,
    pub scx: u8,
    pub ly: u8,
    pub lyc: u8,
    pub bgp: u8,
    pub obp0: u8,
    pub obp1: u8,
    pub wy: u8,
    pub wx: u8,
    /// 0..456 within the line.
    pub dot: u32,
    /// Dots counted while LCDC.7 is 0; 0 while the LCD is on.
    pub lcd_off_dots: u32,
    pub mode: Mode,
    /// The window's internal line counter.
    pub window_line: u8,
    /// The "Y condition": set at the start of any line where `WY == LY`, cleared at VBlank.
    pub window_y: bool,
    /// STAT bit 2: `LY == LYC`, latched every dot while the LCD is on and frozen while it is off
    /// (`stat_lyc_onoff`: the bit survives the LCD going off and LYC changing), so the interrupt
    /// line is driven by the latch rather than a live comparison.
    pub lyc_flag: bool,
    /// Set when the LCD is turned on: line 0 begins at dot 0 but its mode 2 is never reported, so
    /// the PPU holds mode 0 until mode 3 starts at dot 80 (`lcdon_timing-GS`: line 0 starts in
    /// mode 0 and goes straight to mode 3, and only line 0 differs).
    pub line0_skip_mode2: bool,
    /// Dots this line's mode 3 lasts: [`MODE3_DOTS`] plus `SCX % 8` and the line's OBJ penalty,
    /// settled when the line enters mode 3 (`Rendering`, "Mode 3 length"). Derived from `mode`,
    /// `ly`, `scx`, `lcdc` and `oam`, so `load_state` recomputes it rather than storing it.
    pub mode3_len: u32,
    /// The previous OR-of-enabled-conditions, for edge detection.
    pub stat_line: bool,
    /// True while an OAM DMA runs; set by `Bus::tick_dma`, never by the PPU itself.
    pub oam_dma_active: bool,
    pub frame_ready: bool,
    /// Set when the LCD turns on: the next frame is drawn blank; cleared at that frame's end.
    pub first_frame_blank: bool,
    pub frames: u64,
    /// `_TRN` transfers in flight, one per command, each reading its 4 KiB a chunk a frame.
    /// Session state, not save-state: a snapshot load drops them, like the receiver's
    /// packet in flight and the area buffer's continuation packets.
    sgb_trn: Vec<crate::sgb::Trn>,
}

impl Ppu {
    /// Post-boot: LCDC=91 STAT=85 (DMG) LY=0 BGP=FC, OBP0/OBP1 = `$FF`.
    pub fn new() -> Ppu {
        Ppu {
            vram: [0; VRAM_SIZE],
            vram1: [0; VRAM_SIZE],
            oam: [0; OAM_SIZE],
            frame: vec![0; FRAME_PIXELS],
            colours: vec![0; FRAME_PIXELS],
            palettes: Palettes::new(),
            sgb: Sgb::off(),
            cgb: false,
            compat: false,
            opri_dmg: false,
            lcdc: 0x91,
            stat: 0x85,
            scy: 0x00,
            scx: 0x00,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0x00,
            wx: 0x00,
            dot: 0,
            lcd_off_dots: 0,
            mode: Mode::OamScan,
            window_line: 0,
            window_y: false,
            lyc_flag: false,
            line0_skip_mode2: false,
            mode3_len: MODE3_DOTS,
            stat_line: false,
            oam_dma_active: false,
            frame_ready: false,
            first_frame_blank: false,
            frames: 0,
            sgb_trn: Vec::new(),
        }
    }

    /// Advances `dots` dots; returns the IF bits raised. With LCDC.7 off it only counts
    /// `lcd_off_dots` and returns 0.
    pub fn tick(&mut self, dots: u32) -> u8 {
        if self.lcdc & 0x80 == 0 {
            self.lcd_off_dots = self.lcd_off_dots.saturating_add(dots);
            if self.lcd_off_dots >= DOTS_PER_FRAME {
                self.lcd_off_dots -= DOTS_PER_FRAME;
                self.complete_blank_frame();
            }
            return 0;
        }
        let mut bits = 0;
        for _ in 0..dots {
            bits |= self.tick_dot();
        }
        bits
    }

    /// One dot: advance the counter, take any line transition, then settle the mode and the `STAT`
    /// line, which Pan Docs computes every dot (§4.4).
    fn tick_dot(&mut self) -> u8 {
        let mut bits = 0;
        self.dot += 1;
        if self.dot >= DOTS_PER_LINE {
            self.dot = 0;
            bits |= self.advance_line();
        }

        let mode = self.mode_at();
        if mode != self.mode {
            self.mode = mode;
            if mode == Mode::Drawing {
                // The line's mode 3 length is settled as it starts: `SCX % 8` plus the dots the
                // scan spends fetching this line's OBJs (`Rendering`, "Mode 3 length").
                self.mode3_len = self.line_mode3_len();
                self.render_line();
            }
        }

        // STAT bit 2 is a latch that only the running LCD clock updates.
        self.lyc_flag = self.ly == self.lyc;

        // On the first dot of VBlank the DMG also asserts the mode-2 condition for one dot, so an
        // OAM interrupt can fire alongside `IF_VBLANK` (`vblank_stat_intr-GS`).
        let vblank_oam = self.dot == 0 && self.ly == VBLANK_LINE;
        let now = self.stat_condition_with(if vblank_oam { 0x20 } else { 0 });
        if now && !self.stat_line {
            bits |= IF_STAT;
        }
        self.stat_line = now;
        bits
    }

    /// One line boundary: `LY` advances, VBlank begins at line 144 and the frame ends at 153 -> 0.
    fn advance_line(&mut self) -> u8 {
        self.ly += 1;
        // Line 0 reporting mode 0 instead of mode 2 is a property of that one line only.
        self.line0_skip_mode2 = false;
        if self.ly == VBLANK_LINE {
            self.window_y = false;
            self.mode = Mode::VBlank;
            return IF_VBLANK;
        }
        if self.ly >= LINES_PER_FRAME as u8 {
            self.ly = 0;
            self.mode = Mode::OamScan;
            self.line0_skip_mode2 = false;
            self.complete_frame();
            return 0;
        }
        self.mode = if self.ly < VBLANK_LINE {
            Mode::OamScan
        } else {
            Mode::VBlank
        };
        0
    }

    /// The mode the current dot sits in (`Rendering`: 80 dots of mode 2, `mode3_len` of mode 3,
    /// the rest of the 456-dot line in mode 0, and 4 560 dots of mode 1).
    fn mode_at(&self) -> Mode {
        if self.ly >= VBLANK_LINE {
            Mode::VBlank
        } else if self.dot < MODE2_DOTS {
            if self.line0_skip_mode2 {
                Mode::HBlank
            } else {
                Mode::OamScan
            }
        } else if self.dot < MODE2_DOTS + self.mode3_len {
            Mode::Drawing
        } else {
            Mode::HBlank
        }
    }

    /// Dots this line's mode 3 lasts: the 172-dot minimum, `SCX % 8` and the OBJ penalty
    /// (`Rendering`, "Mode 3 length").
    fn line_mode3_len(&self) -> u32 {
        MODE3_DOTS + (self.scx & 7) as u32 + self.obj_penalty()
    }

    /// Pan Docs' OBJ penalty algorithm (`Rendering`): per OBJ drawn on this line, in draw order.
    ///
    /// The scan picks the first ten OAM entries whose Y range covers the line; the fetches then
    /// run leftmost first, ties by OAM index. An OBJ's tile is the background tile its leftmost
    /// pixel falls in, `(X - 8 + SCX) / 8`; the first OBJ to reach a tile pays the dots between
    /// that pixel and the end of the tile less two, and every OBJ drawn pays a flat six. An OBJ at
    /// `X = 0` sits completely off the left edge and pays the full eleven (before the trim at the
    /// end of this function), and one at `X >= 168` is not drawn at all.
    fn obj_penalty(&self) -> u32 {
        if self.lcdc & 0x02 == 0 {
            return 0;
        }
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };
        let ly = self.ly as i32;
        let scx = self.scx as i32;
        let mut objs = [(0i32, 0usize); 10];
        let mut count = 0;
        for i in 0..OAM_SIZE / 4 {
            if count == objs.len() {
                break;
            }
            let top = self.oam[i * 4] as i32 - 16;
            if ly < top || ly >= top + height {
                continue;
            }
            objs[count] = (self.oam[i * 4 + 1] as i32, i);
            count += 1;
        }
        let objs = &mut objs[..count];
        objs.sort_by_key(|&(x, index)| (x, index));

        let mut seen = [false; 32];
        let mut dots = 0;
        for &(x, _) in objs.iter() {
            if x >= 168 {
                continue;
            }
            let pixel = x - 8 + scx;
            let tile = pixel.div_euclid(8).rem_euclid(32) as usize;
            if !seen[tile] {
                seen[tile] = true;
                // Pixels strictly right of this one in the tile, less two; X = 0 bypasses SCX.
                let column = if x == 0 { 0 } else { pixel.rem_euclid(8) };
                dots += (7 - column - 2).max(0) as u32;
            }
            dots += 6;
        }
        // UNVERIFIED: the documented per-OBJ dots overstate the mode-3 end by three in this
        // emulator. `intr_2_mode0_timing_sprites` calibrates all 105 of its cases as "the fewest
        // NOPs after the STAT handler that still see mode 0" against the same interrupt chain
        // that `intr_2_mode0_timing` and `hblank_ly_scx_timing-GS` pin for sprite-less lines, so
        // the chain cannot absorb the difference; with the documented sums 61 of the 105 cases
        // disagree with the ROM's own NOP counts and with these three dots removed all 105 agree
        // (one sprite at X=0 must end mode 3 before dot 260, i.e. pay 8, not 11). No gbdev note
        // accounts for the three dots; the ROM's case table is the only source for it here.
        if dots > 0 { dots - 3 } else { dots }
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => {
                // The LYC bit reads clear in the four dots before the line boundary, where LY has
                // already stepped but the coincidence latch has not yet been re-evaluated
                // (`lcdon_timing-GS`: with LYC=0 the bit reads set at dot 448 and clear at 452;
                // with LYC=1 it reads clear at 452 and set at 456). Only the read is masked, so
                // the STAT interrupt's own latch keeps its edges.
                let lyc = if self.lyc_flag && self.dot < DOTS_PER_LINE - 4 {
                    0x04
                } else {
                    0x00
                };
                0x80 | (self.stat & 0x78) | (self.mode as u8) | lyc
            }
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            // LY runs four dots ahead of the mode grid: the register steps at dot 452 of each
            // line while the modes and the STAT coincidence latch switch at the 456-dot boundary
            // (`lcdon_timing-GS`: line 0 reads LY=1 at dot 452 with STAT still in mode 0, and the
            // next line's mode 2 only begins at dot 456 — the two are four dots apart).
            0xFF44 => {
                if self.dot >= DOTS_PER_LINE - 4 && (self.ly as u32) < LINES_PER_FRAME - 1 {
                    self.ly + 1
                } else {
                    self.ly
                }
            }
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) -> u8 {
        let mut bits = 0;
        match addr {
            0xFF40 => {
                let was_on = self.lcdc & 0x80 != 0;
                self.lcdc = value;
                let now_on = value & 0x80 != 0;
                // Pan Docs, *MASK_EN*: "SGB freezes/unfreezes the Game Boy screen automatically
                // whenever the LCD is turned off/on" — off freezes what was there, on cancels the
                // mask. Guarded on `enabled()` so an SGB-off machine's LCD write is the write it
                // always was.
                if self.sgb.enabled() {
                    if was_on && !now_on {
                        self.sgb.set_mask(MaskState::Frozen);
                    } else if !was_on && now_on {
                        self.sgb.set_mask(MaskState::Active);
                    }
                }
                if was_on && !now_on {
                    self.lcd_off();
                } else if !was_on && now_on {
                    bits |= self.lcd_on();
                }
            }
            0xFF41 => {
                // DMG: the write behaves as if `$FF` had been written for one M-cycle, then the
                // written value for the next (`STAT`, "Spurious STAT interrupts"). `$FF` selects
                // all four conditions, so that one cycle's line is high in modes 0/1/2 and on any
                // line where LY == LYC — and a rising edge there requests the interrupt even when
                // the value being written selects nothing (writing `$00` can fire it).
                //
                // The quirk needs a running PPU: with the display disabled the mode field only
                // *reports* 0, nothing is scanned, so no condition holds and no edge appears.
                // Joe & Mac and Super Scrabble turn the display off mid-frame, write STAT, and
                // their STAT handler then waits for LY to move; a request raised here entered that
                // handler with LY parked at 0 and hung the game.
                let as_if_ff =
                    self.lcdc & 0x80 != 0 && (self.mode != Mode::Drawing || self.lyc_flag);
                let was_high = self.stat_line;
                self.stat = value & 0x78;
                let now = self.stat_condition();
                if as_if_ff && !was_high {
                    bits |= IF_STAT;
                }
                self.stat_line = now || as_if_ff;
            }
            0xFF42 => self.scy = value,
            0xFF43 => self.scx = value,
            0xFF45 => self.lyc = value,
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            _ => {}
        }
        bits
    }

    /// `$FF` while the PPU owns VRAM (`Accessing_VRAM_and_OAM`: mode 0-2 only). A read is also
    /// held off for the last four dots of mode 2: the PPU has started fetching for mode 3 there
    /// (`lcdon_timing-GS` pass 2 reads `$FF` at line 1 dot 76 while pass 1 reads `$00` at line 0
    /// dot 76, which is mode 0). Writes still land in that window (`lcdon_write_timing-GS`
    /// round 16 writes at line 1 dot 76 and passes), so the two are gated separately.
    ///
    /// `bank` is `VBK` bit 0: bank 1 is the CGB's attribute map and its second tile-data bank. A
    /// DMG cartridge never selects it, so its reads are this method with `bank` = 0.
    pub fn read_vram(&self, addr: u16, bank: u8) -> u8 {
        let drawing = self.lcdc & 0x80 != 0
            && (self.mode == Mode::Drawing
                || (self.mode == Mode::OamScan && self.dot >= MODE2_DOTS - 4));
        if drawing {
            0xFF
        } else {
            self.vram_banked(bank)[(addr as usize) & 0x1FFF]
        }
    }

    pub fn write_vram(&mut self, addr: u16, bank: u8, value: u8) {
        if self.lcdc & 0x80 != 0 && self.mode == Mode::Drawing {
            return;
        }
        self.write_vram_dma(addr, bank, value);
    }

    /// A VRAM write from the CGB's VRAM DMA unit: it is not the CPU, so the PPU's mode-3 lock does
    /// not hold it back (Pan Docs: the general-purpose transfer "blindly attempts to copy the
    /// data, even if the LCD controller is currently accessing VRAM").
    pub fn write_vram_dma(&mut self, addr: u16, bank: u8, value: u8) {
        let at = (addr as usize) & 0x1FFF;
        match bank & 1 {
            0 => self.vram[at] = value,
            _ => self.vram1[at] = value,
        }
    }

    /// One VRAM bank by number, for the renderer and the debugger's memory view.
    pub fn vram_banked(&self, bank: u8) -> &[u8; VRAM_SIZE] {
        match bank & 1 {
            0 => &self.vram,
            _ => &self.vram1,
        }
    }

    /// Hand one decoded SGB packet to [`Sgb::apply`], with the 4 KiB VRAM block a `_TRN` command
    /// reads.
    ///
    /// The split borrow is why this lives on `Ppu` and not on `Sgb`: the packet handler needs
    /// `$8000-$9FFF`, which `Ppu` owns, and the SGB state beside it.
    ///
    /// **Which 4 KiB.** Pan Docs, *VRAM Transfers*: "the 4K data block is then read-out by the SNES
    /// from Game Boy display memory during the next frame", and "even though the SNES receives the
    /// data in from display scanlines, it will automatically re-produce the same ordering of bits
    /// and bytes, as being originally stored at 8000-8FFF in Game Boy memory" — so the source is
    /// the character data the display is showing, and *LCDC* bit 4 says where that is: `$8000`
    /// addressing reads `$8000-$8FFF`, `$8800` addressing reads `$8800-$97FF`. The "Preparing the
    /// Display" paragraph is the cartridge's half of that deal ("BG Map must display unsigned
    /// characters $00-$FF on the screen; $00..$13 in first line"), so the block is read linearly
    /// from the base the bit selects. Both real SGB cartridges tested clear bit 4 and stage
    /// their 4 KiB at `$8800-$97FF`; reading `$8000-$8FFF` there hands every `_TRN` command zeros.
    pub fn apply_sgb_packet(&mut self, packet: &crate::sgb::Packet) {
        let base = if self.lcdc & 0x10 != 0 { 0x000 } else { 0x800 };
        // A `_TRN` command only starts a five-frame read of this block (Pan Docs, *VRAM
        // Transfers*: "the transfer starts at the beginning of the next frame after the command has
        // been sent"). [`Ppu::tick_sgb_transfers`] reads it a chunk a frame and applies the packet
        // with the finished block, so a cartridge that polls sees the transfer in progress.
        //
        // The area-command guard comes first: a continuation packet is raw data, and data that
        // spells a `_TRN` header must reach `Sgb::apply`'s continuation branch rather than start a
        // bogus transfer.
        if !self.sgb.receiving() && crate::sgb::transfer::is_trn(packet) {
            self.sgb_trn.push(crate::sgb::Trn::new(packet, base));
            return;
        }
        let (sgb, vram) = (&mut self.sgb, &self.vram);
        sgb.apply(packet, &vram[base..base + 0x1000]);
    }

    /// One frame start for every `_TRN` transfer in flight: read the next chunk of its block and,
    /// once the fifth read has happened, apply the command it was started by.
    ///
    /// Called from [`Ppu::complete_frame`], which is the `LY` 153 -> 0 wrap: the end of one frame is
    /// the beginning of the next, and Pan Docs has the reads at those beginnings. A transfer whose
    /// block has been read arrives at [`crate::sgb::Sgb::apply`] with the whole 4 KiB, exactly as a
    /// direct read at the command would have.
    fn tick_sgb_transfers(&mut self) {
        let mut index = 0;
        while index < self.sgb_trn.len() {
            let done = self.sgb_trn[index].tick(&self.vram);
            if !done {
                index += 1;
                continue;
            }
            let trn = self.sgb_trn.remove(index);
            self.sgb.apply(trn.packet(), trn.block());
        }
    }

    /// Drop any `_TRN` transfer in flight. A snapshot load calls this beside
    /// [`crate::sgb::Sgb::clear_transfer`]: the snapshot holds the border as it stood, so a read the
    /// machine was in the middle of when the state was written must not finish on the restored
    /// timeline. Session state, like the receiver's packet in flight.
    pub fn clear_sgb_transfers(&mut self) {
        self.sgb_trn.clear();
    }

    /// A read of `$FF68-$FF6B` (Pan Docs, *Palettes*). The two index registers answer all the time;
    /// the two data registers are `$FF` in mode 3, where the PPU is reading palette RAM.
    pub fn read_palette(&self, addr: u16) -> u8 {
        if matches!(addr, 0xFF69 | 0xFF6B) && self.cram_blocked() {
            return 0xFF;
        }
        self.palettes.read(addr)
    }

    /// A write of `$FF68-$FF6B`. In mode 3 the data byte is dropped but the index still steps, so
    /// a cartridge that writes a colour during rendering comes out of it with the index where it
    /// would have been (Pan Docs, *FF68*: auto-increment happens "even if the write fails").
    pub fn write_palette(&mut self, addr: u16, value: u8) {
        self.palettes.write(addr, value, self.cram_blocked());
    }

    /// True while the PPU is reading palette RAM: mode 3 of a running LCD.
    fn cram_blocked(&self) -> bool {
        self.lcdc & 0x80 != 0 && self.mode == Mode::Drawing
    }

    /// True while the PPU owns OAM. Mode 3 owns it throughout; mode 2 only for its first 76
    /// dots — its last four are the sprite sort, which does not read OAM, so a write or read
    /// there lands (`lcdon_write_timing-GS` round 9: a write at line 1 dot 76 passes, while dots
    /// 72 and 80 both fail).
    fn oam_write_blocked(&self) -> bool {
        if self.lcdc & 0x80 == 0 {
            return false;
        }
        match self.mode {
            Mode::Drawing => true,
            Mode::OamScan => self.dot < MODE2_DOTS - 4,
            _ => false,
        }
    }

    /// OAM reads are owned for the whole of mode 2 and for the last four dots of mode 0, where the
    /// next line's scan has already begun (`lcdon_timing-GS` pass 2 reads `$FF` at line 0 dot 452
    /// while STAT still reports mode 0 there). Writes are not held off in that window.
    fn oam_read_blocked(&self) -> bool {
        if self.lcdc & 0x80 == 0 {
            return false;
        }
        match self.mode {
            Mode::Drawing | Mode::OamScan => true,
            _ => self.dot >= DOTS_PER_LINE - 4,
        }
    }

    /// `$FF` while the PPU owns OAM.
    pub fn read_oam(&self, addr: u16) -> u8 {
        if self.oam_read_blocked() {
            0xFF
        } else {
            self.oam[(addr as usize) % OAM_SIZE]
        }
    }

    pub fn write_oam(&mut self, addr: u16, value: u8) {
        if self.oam_write_blocked() {
            return;
        }
        self.oam[(addr as usize) % OAM_SIZE] = value;
    }

    /// Writes OAM past the mode gate, for the DMA unit.
    pub fn oam_dma_write(&mut self, index: usize, value: u8) {
        self.oam[index % OAM_SIZE] = value;
    }

    /// True once per completed frame.
    pub fn take_frame(&mut self) -> bool {
        let ready = self.frame_ready;
        self.frame_ready = false;
        ready
    }

    pub fn frame(&self) -> &[u8] {
        &self.frame
    }

    /// The picture's colour words: 23 040 of them, one per pixel, the shade index on a DMG and
    /// RGB555 on a CGB.
    pub fn colours(&self) -> &[u16] {
        &self.colours
    }

    /// The `LY` 153 -> 0 wrap: the picture was drawn line by line, so only the per-frame counters
    /// and the blank-first-frame flag move here.
    fn complete_frame(&mut self) {
        self.window_line = 0;
        self.window_y = false;
        self.first_frame_blank = false;
        self.frame_ready = true;
        self.frames += 1;
        // The beginning of the next frame is where Pan Docs puts a `_TRN` transfer's next read.
        self.tick_sgb_transfers();
    }

    /// A frame that ended while the LCD was off: `frame` is shade 0 everywhere (§4.4).
    fn complete_blank_frame(&mut self) {
        self.blank_picture();
        self.complete_frame();
    }

    /// The blank picture: shade 0 (white) everywhere, in both formats — on a CGB the screen after
    /// a boot is white too, since Pan Docs has the boot ROM fill the background bank with `$7FFF`.
    fn blank_picture(&mut self) {
        self.frame.fill(0);
        self.colours.fill(if self.cgb {
            crate::cgb::RGB555_WHITE
        } else {
            0
        });
    }

    /// The OR of the four `STAT` conditions this line enables (§4.4). Bit 2 of `STAT` is the
    /// `LY == LYC` flag, bit 6 the interrupt that uses it.
    fn stat_condition(&self) -> bool {
        self.stat_condition_with(0)
    }

    /// `stat_condition` with extra mode bits asserted on top of the current mode's own bit.
    fn stat_condition_with(&self, extra_mode_bit: u8) -> bool {
        let mode_bit = match self.mode {
            Mode::HBlank => 0x08,
            Mode::VBlank => 0x10,
            Mode::OamScan => 0x20,
            Mode::Drawing => 0x00,
        } | extra_mode_bit;
        let lyc_bit = if self.lyc_flag { 0x40 } else { 0x00 };
        self.stat & (mode_bit | lyc_bit) != 0
    }

    /// LCDC.7 1 -> 0: blank the picture, free VRAM/OAM, hold mode 0 and `LY = 0`, start the
    /// off-period counter (§4.4). The `LY == LYC` latch and the STAT line are kept: the comparison
    /// clock stops, so the bit holds its value and changing `LYC` cannot move it
    /// (`stat_lyc_onoff`).
    fn lcd_off(&mut self) {
        self.mode = Mode::HBlank;
        self.ly = 0;
        self.dot = 0;
        self.lcd_off_dots = 0;
        self.line0_skip_mode2 = false;
        self.window_line = 0;
        self.window_y = false;
        self.first_frame_blank = false;
        self.blank_picture();
    }

    /// LCDC.7 0 -> 1: a blank first frame, then line 0 at dot 0 (§4.4). Restarting the comparison
    /// clock can move the `LY == LYC` latch, so it is settled here and the resulting rising edge
    /// requests the interrupt (`stat_lyc_onoff` round 4).
    fn lcd_on(&mut self) -> u8 {
        self.first_frame_blank = true;
        // The PPU restarts the line with the enable: line 0 begins at dot 0, is a full 456 dots,
        // and does not report mode 2 — it holds mode 0 until mode 3 begins at dot 80
        // (`lcdon_timing-GS`, `lcdon_write_timing-GS`).
        self.dot = 0;
        self.lcd_off_dots = 0;
        self.ly = 0;
        self.mode = Mode::HBlank;
        self.mode3_len = MODE3_DOTS;
        self.line0_skip_mode2 = true;
        self.window_line = 0;
        self.window_y = false;
        self.blank_picture();
        self.lyc_flag = self.ly == self.lyc;
        let was_high = self.stat_line;
        let now = self.stat_condition();
        self.stat_line = now;
        if now && !was_high {
            return IF_STAT;
        }
        0
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.vram);
        w.bytes(&self.vram1);
        w.bytes(&self.oam);
        w.bytes(&self.frame);
        // The colour words are only stored for a frame drawn in colour: a DMG's are its shade
        // indices, which `frame` already holds, and half of every snapshot in the rewind ring is
        // worth not spending twice.
        w.bool(self.cgb);
        if self.cgb {
            for word in &self.colours {
                w.u16(*word);
            }
        }
        w.bool(self.compat);
        w.bool(self.opri_dmg);
        self.palettes.save_state(w);
        for v in [
            self.lcdc, self.stat, self.scy, self.scx, self.ly, self.lyc, self.bgp, self.obp0,
            self.obp1, self.wy, self.wx,
        ] {
            w.u8(v);
        }
        w.u32(self.dot);
        w.u32(self.lcd_off_dots);
        w.u8(self.mode as u8);
        w.u8(self.window_line);
        w.bool(self.lyc_flag);
        w.bool(self.line0_skip_mode2);
        w.bool(self.stat_line);
        w.bool(self.frame_ready);
        w.bool(self.window_y);
        w.bool(self.oam_dma_active);
        w.bool(self.first_frame_blank);
        w.u64(self.frames);
        // The two SGB fields the picture depends on: whether the renderer asks at all,
        // then 32 + 4 096 + 1 bytes of palette.
        w.bool(self.sgb.enabled());
        self.sgb.palettes().save_state(w);
        // The 45 uploaded attribute files and the live 20x18 map: 4 050 + 90 bytes.
        self.sgb.attr().save_state(w);
        // `MASK_EN`'s state: a tag byte, plus the word for a blanked window.
        self.sgb.mask().save_state(w);
        // The border: whether a map arrived, then 8 KiB of tiles, 1 792 bytes of map
        // and three palettes. The 256x224 pixels are derived from those and are not written.
        self.sgb.border().save_state(w);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.vram)?;
        r.bytes_into(&mut self.vram1)?;
        r.bytes_into(&mut self.oam)?;
        r.bytes_into(&mut self.frame)?;
        self.cgb = r.bool()?;
        if self.cgb {
            for word in &mut self.colours {
                *word = r.u16()?;
            }
        }
        self.compat = r.bool()?;
        self.opri_dmg = r.bool()?;
        self.palettes.load_state(r)?;
        self.lcdc = r.u8()?;
        self.stat = r.u8()?;
        self.scy = r.u8()?;
        self.scx = r.u8()?;
        self.ly = r.u8()?;
        self.lyc = r.u8()?;
        self.bgp = r.u8()?;
        self.obp0 = r.u8()?;
        self.obp1 = r.u8()?;
        self.wy = r.u8()?;
        self.wx = r.u8()?;
        self.dot = r.u32()?;
        self.lcd_off_dots = r.u32()?;
        self.mode = match r.u8()? {
            1 => Mode::VBlank,
            2 => Mode::OamScan,
            3 => Mode::Drawing,
            _ => Mode::HBlank,
        };
        self.window_line = r.u8()?;
        self.lyc_flag = r.bool()?;
        self.line0_skip_mode2 = r.bool()?;
        self.stat_line = r.bool()?;
        self.frame_ready = r.bool()?;
        self.window_y = r.bool()?;
        self.oam_dma_active = r.bool()?;
        self.first_frame_blank = r.bool()?;
        self.frames = r.u64()?;
        self.sgb.set_enabled(r.bool()?);
        self.sgb.palettes_mut().load_state(r)?;
        self.sgb.attr_mut().load_state(r)?;
        self.sgb.mask_mut().load_state(r)?;
        self.sgb.border_mut().load_state(r)?;
        // A `_TRN` transfer in flight is session state: the snapshot holds the border as
        // it stood, so the read it was in the middle of is dropped, as a state load drops the area
        // buffer's continuation packets.
        self.clear_sgb_transfers();
        // Derived, not stored: a state loaded mid-line must finish the line with the same length.
        self.mode3_len = self.line_mode3_len();
        Ok(())
    }
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
    }
}
