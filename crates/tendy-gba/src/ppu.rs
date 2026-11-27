//! The Game Boy Advance's picture (G3), drawn one scanline at a time.
//!
//! GBATEK's video chapters are the whole of this file: `DISPCNT`'s modes 0-5, `BG0CNT`-`BG3CNT`,
//! `BGxHOFS`/`VOFS`, the affine matrix and reference points, `OAM`, `WIN0`/`WIN1`/`WINOUT`,
//! `BLDCNT`/`BLDALPHA`/`BLDY` and `MOSAIC`. The renderer is driven by the frame clock rather than
//! called once per frame — [`Ppu::sync`] looks at the line the LCD is on and draws each line as it
//! *begins*, so a `BGxHOFS` write during one line's HBlank is seen on the next line and not on the
//! one being drawn. That is the wavy-background trick games use, and it is why a whole-frame
//! renderer is not enough.
//!
//! **Resolution of the brief's one ambiguity** (GBATEK, *BG Mode* table): modes 6 and 7 are not
//! defined on this hardware — the table says "6-7=Prohibited" and gives them no layout. A cartridge
//! that asks for one gets the forced-blank white field rather than a guess at mode 3's bitmap, so a
//! nonsense mode is visibly nonsense instead of plausibly wrong.
//!
//! **Objects** are one more layer drawn after the backgrounds: all 128 OAM entries, the twelve
//! shapes and sizes, 4bpp and 8bpp tiles in both the 2D and 1D mappings, the two flips or the
//! rotation/scaling matrix that replaces them, double-size bounding boxes and mosaic. GBATEK is
//! explicit that the GBA picks the top-most object pixel by OAM entry number and *not* by the
//! two-bit BG-priority attribute (*DS Video OBJs*: "The GBA has been assigning OBJ priority in
//! respect to the 7bit OAM entry number, regardless of the OBJs 2bit BG-priority attribute"), so
//! the lower index wins a pixel and only then does its priority compete with the backgrounds. In
//! bitmap modes the lower 16 KiB of the object area belongs to the bitmap, so a tile number below
//! 512 is not displayable and the object is skipped, as GBATEK says.
//!
//! **Windows and the colour effects** are applied at the same compose step: [`Windows`] picks the
//! WIN0/WIN1/object-window region a pixel is in and so which layers and whether a special effect
//! are enabled there; [`compose`] keeps the top two non-transparent pixels in priority order and
//! applies `BLDCNT`'s first/second targets, `BLDALPHA`'s alpha blend or `BLDY`'s brightness. A
//! semi-transparent object is always a first target and always alpha-blends, whatever `BLDCNT`
//! says.
//!
//! **What is not modelled.** The per-line sprite limits (up to 128 dots, and the cycle budget that
//! `DISPCNT` bit 5 shrinks) are G4's timing budget, not the picture's; every object on a line is
//! drawn. The LCD's interlace dimming and the top-eight-lines shadow are physical panel effects no
//! framebuffer can show. Neither is claimed anywhere.
//!
//! The picture is RGB555, 240x160, in the same colour type the Game Boy Color's PPU hands the front
//! end. [`Ppu::frame`] is the last *complete* frame: [`Ppu::sync`] starts drawing the next frame's
//! line 0 the moment the clock wraps, so the live buffer is always one line ahead of the picture a
//! screenshot should see.

use crate::bus::{Bus, LINES_PER_FRAME, VISIBLE_LINES};
use tendy_gb::state::{StateError, StateReader, StateWriter};

/// Visible picture width in pixels (GBATEK, *LCD Dimensions*: "Visible 240 dots").
pub const WIDTH: usize = 240;
/// Visible picture height in pixels: "Visible (*) 160 lines".
pub const HEIGHT: usize = 160;
/// One frame of RGB555 words.
pub const PIXELS: usize = WIDTH * HEIGHT;

/// White: the colour forced blank draws (GBATEK, *Blanking Bits*: "causing the video controller to
/// display white lines").
const WHITE: u16 = 0x7FFF;

/// I/O offsets of the registers this renderer reads, from GBATEK's *GBA I/O Map*.
const DISPCNT: usize = 0x000;
const BG0CNT: usize = 0x008;
const BG0HOFS: usize = 0x010;
const BG2PA: usize = 0x020;
const BG2X: usize = 0x028;
const BG3PA: usize = 0x030;
const BG3X: usize = 0x038;
const WIN0H: usize = 0x040;
const WIN1H: usize = 0x042;
const WIN0V: usize = 0x044;
const WIN1V: usize = 0x046;
const WININ: usize = 0x048;
const WINOUT: usize = 0x04A;
const MOSAIC: usize = 0x04C;
const BLDCNT: usize = 0x050;
const BLDALPHA: usize = 0x052;
const BLDY: usize = 0x054;

/// `DISPCNT` bits, named where they are read.
const DISPCNT_FORCED_BLANK: u16 = 1 << 7;
/// `DISPCNT`: which bitmap page modes 4 and 5 display.
const DISPCNT_FRAME_SELECT: u16 = 1 << 4;
/// `DISPCNT`: object character mapping — 1D (consecutive tiles) rather than 2D.
const DISPCNT_1D_OBJECTS: u16 = 1 << 6;
/// `DISPCNT`: objects enabled at all.
const DISPCNT_OBJECTS: u16 = 1 << 12;
/// `DISPCNT`: window 0, window 1, and the object window are active regions.
const DISPCNT_WIN0: u16 = 1 << 13;
const DISPCNT_WIN1: u16 = 1 << 14;
const DISPCNT_OBJ_WINDOW: u16 = 1 << 15;

/// Where object tiles live in VRAM (GBATEK, *LCD VRAM Overview*: `06010000-06017FFF`), and the
/// 16 KiB the bitmap takes from the bottom of that area in modes 3-5.
const OBJ_VRAM: usize = 0x1_0000;
/// The first of the four `BGxCNT` registers; BG1's is two bytes later, and so on.
const BG_CNT_STRIDE: usize = 2;
/// The first of the eight `BGxHOFS`/`BGxVOFS` registers: BG0's pair, then BG1's, four bytes apart.
const BG_SCROLL_STRIDE: usize = 4;

/// A byte of VRAM at `offset`, or 0 when the offset is past the 96 KiB the machine holds. The
/// background and object areas are masked by their callers — the tile maps live inside 64 KiB of BG
/// VRAM, whose addressing wraps on hardware — so this only defends the array bounds.
fn vram(bus: &Bus, offset: usize) -> u8 {
    bus.vram.get(offset).copied().unwrap_or(0)
}

/// A byte of the video half of VRAM (`$06000000-$0600FFFF`), where tile maps and tile data live in
/// modes 0-2. The hardware decodes 16 bits of the offset there, so the addresses wrap inside it.
fn bg_vram(bus: &Bus, offset: usize) -> u8 {
    vram(bus, offset & 0xFFFF)
}

/// The palette word at `index` (GBATEK, *Color Palette RAM*: 256 words for backgrounds, then 256 for
/// objects).
fn palette_word(bus: &Bus, index: usize) -> u16 {
    let at = (index & 0xFF) * 2;
    u16::from_le_bytes([bus.palette[at], bus.palette[at + 1]])
}

/// One halfword of the I/O page. Every register this renderer reads is stored there by `Bus`.
fn io16(bus: &Bus, offset: usize) -> u16 {
    u16::from_le_bytes([bus.io[offset], bus.io[offset + 1]])
}

/// One halfword of OAM: an object's attribute, or half of a rotation/scaling group.
fn oam16(bus: &Bus, offset: usize) -> u16 {
    let at = offset & (crate::bus::OAM_SIZE - 1);
    u16::from_le_bytes([bus.oam[at], bus.oam[at + 1]])
}

/// The object palette's word at `index`: GBATEK, *Color Palette RAM* — the 256 object colours follow
/// the 256 background ones at `05000200h`.
fn object_palette_word(bus: &Bus, index: usize) -> u16 {
    let at = 0x200 + (index & 0xFF) * 2;
    u16::from_le_bytes([bus.palette[at], bus.palette[at + 1]])
}

/// A byte of the object's 32 KiB of VRAM, which wraps inside itself as GBATEK's *LCD VRAM Overview*
/// describes.
fn obj_vram(bus: &Bus, offset: usize) -> u8 {
    vram(bus, OBJ_VRAM + (offset & 0x7FFF))
}

/// A signed 8.8 fixed-point affine parameter: GBATEK's "0-7 Fractional, 8-14 Integer, 15 Sign".
fn fixed16(word: u16) -> i32 {
    i32::from(word as i16)
}

/// A background's four rotation/scaling parameters, as `(PA, PB, PC, PD)` in 8.8 fixed point. `PA`
/// moves along x on the same line, `PB` along x on the next line, `PC` along y on the same line and
/// `PD` along y on the next line (GBATEK, *BG Rotation/Scaling*).
fn matrix(bus: &Bus, base: usize) -> (i32, i32, i32, i32) {
    (
        fixed16(io16(bus, base)),
        fixed16(io16(bus, base + 2)),
        fixed16(io16(bus, base + 4)),
        fixed16(io16(bus, base + 6)),
    )
}

/// A 28-bit reference point pair, sign-extended to 8.8 fixed point: GBATEK says "Normal signed 32bit
/// values may be written... the most significant bits will be ignored and the value will be cut-down
/// to 28bits".
fn register_point(bus: &Bus, offset: usize) -> (i32, i32) {
    let read = |at: usize| -> i32 {
        let raw = u32::from(io16(bus, at)) | (u32::from(io16(bus, at + 2) & 0x0FFF) << 16);
        let raw = raw & 0x0FFF_FFFF;
        let signed = if raw & 0x0800_0000 != 0 {
            raw | 0xF000_0000
        } else {
            raw
        };
        signed as i32
    };
    (read(offset), read(offset + 4))
}

/// One background's pixels of a line: the colour and whether anything is there at all. Colour 0 is
/// not "black" on this hardware, it is transparency, so a separate bit says which.
type LinePixels = ([u16; WIDTH], [bool; WIDTH]);

/// One affine background's internal reference point.
///
/// GBATEK, *Internal Reference Point Registers*: the register "is automatically copied to internal
/// registers during each vblank, specifying the origin for the first scanline. The internal
/// registers are then incremented by dmx and dmy after each scanline. Caution: Writing to a
/// reference point register by software outside of the Vblank period does immediately copy the new
/// value to the corresponding internal register". So the register is watched rather than written
/// back: [`Ppu::sync_reference`] notices a new register value and adopts it, and the internal point
/// advances by `(PB, PD)` after every line it draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reference {
    /// X coordinate of the origin of the line about to be drawn, in 8.8 fixed point.
    x: i32,
    /// Y coordinate of the same point.
    y: i32,
    /// The register pair this reference was last taken from. `None` until the first sync, so a
    /// power-on machine adopts the register even when it reads zero.
    register: Option<(i32, i32)>,
}

/// The Game Boy Advance's renderer.
#[derive(Clone)]
pub struct Ppu {
    /// The frame being drawn, row-major RGB555.
    live: Vec<u16>,
    /// The last complete frame, which is what [`Ppu::frame`] hands out.
    shown: Vec<u16>,
    /// The absolute line number (`frames * 228 + VCOUNT`) of the next line to start drawing. Seeing
    /// it move is what tells the renderer a scanline has begun.
    next: u64,
    /// False until the first [`Ppu::sync`], so the first line adopts `next` instead of chasing it.
    started: bool,
    /// The frame index `live` belongs to, or `None` before the first line.
    drawn_frame: Option<u64>,
    /// The internal origins of BG2 and BG3, in that order.
    reference: [Reference; 2],
    /// The origin each affine layer's current mosaic block samples from, if it is inside a block.
    mosaic_origin: [Option<(i32, i32)>; 2],
    /// Visible lines drawn since power-on — the renderer's own odometer, for the tests.
    pub lines_drawn: u64,
}

impl Default for Ppu {
    fn default() -> Ppu {
        Ppu::new()
    }
}

impl Ppu {
    /// Power-on: a black field, the clock at line 0, nothing drawn.
    pub fn new() -> Ppu {
        Ppu {
            live: vec![0; PIXELS],
            shown: vec![0; PIXELS],
            next: 0,
            started: false,
            drawn_frame: None,
            reference: [Reference::default(); 2],
            mosaic_origin: [None; 2],
            lines_drawn: 0,
        }
    }

    /// The last complete frame, 240x160 RGB555 words.
    pub fn frame(&self) -> &[u16] {
        &self.shown
    }

    /// This renderer's state, appended to a save state: the line cursor, the two affine origins and
    /// both 240x160 buffers.
    pub fn encode(&self, w: &mut StateWriter) {
        w.bool(self.started);
        w.u64(self.next);
        w.u64(self.drawn_frame.unwrap_or(u64::MAX));
        for reference in &self.reference {
            w.i32(reference.x);
            w.i32(reference.y);
            w.bool(reference.register.is_some());
            let (x, y) = reference.register.unwrap_or((0, 0));
            w.i32(x);
            w.i32(y);
        }
        for slot in &self.mosaic_origin {
            w.bool(slot.is_some());
            let (x, y) = slot.unwrap_or((0, 0));
            w.i32(x);
            w.i32(y);
        }
        for &word in &self.live {
            w.u16(word);
        }
        for &word in &self.shown {
            w.u16(word);
        }
    }

    /// Restore [`Ppu::encode`]'s body. Every field is read into a local before anything is written,
    /// so a state that turns out to be damaged leaves the running picture untouched.
    pub fn decode(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        let started = r.bool()?;
        let next = r.u64()?;
        let drawn_frame = r.u64()?;
        let mut reference = [Reference::default(); 2];
        for slot in &mut reference {
            slot.x = r.i32()?;
            slot.y = r.i32()?;
            let has_register = r.bool()?;
            let x = r.i32()?;
            let y = r.i32()?;
            slot.register = if has_register { Some((x, y)) } else { None };
        }
        let mut mosaic_origin = [None; 2];
        for slot in &mut mosaic_origin {
            let present = r.bool()?;
            let x = r.i32()?;
            let y = r.i32()?;
            *slot = if present { Some((x, y)) } else { None };
        }
        let mut live = vec![0u16; PIXELS];
        for word in &mut live {
            *word = r.u16()?;
        }
        let mut shown = vec![0u16; PIXELS];
        for word in &mut shown {
            *word = r.u16()?;
        }
        self.started = started;
        self.next = next;
        self.drawn_frame = if drawn_frame == u64::MAX {
            None
        } else {
            Some(drawn_frame)
        };
        self.reference = reference;
        self.mosaic_origin = mosaic_origin;
        self.live = live;
        self.shown = shown;
        Ok(())
    }

    /// Draw every scanline the LCD has entered since the last call, in order.
    ///
    /// Called after each instruction's cycles are charged to the clock, so the granularity is one
    /// instruction — the same granularity every other part of this machine runs at. A line is drawn
    /// when the clock *enters* it: the register state at that moment is the state the whole line
    /// sees, which is what makes a write during the previous line's HBlank land on this line and a
    /// write during this line's own drawing land on the next one.
    pub fn sync(&mut self, bus: &Bus) {
        let now = bus.lcd.frames * u64::from(LINES_PER_FRAME) + u64::from(bus.lcd.line);
        if !self.started {
            self.started = true;
            self.next = now;
        }
        while self.next <= now {
            let line = (self.next % u64::from(LINES_PER_FRAME)) as u16;
            let frame = self.next / u64::from(LINES_PER_FRAME);
            if line < VISIBLE_LINES {
                // A new frame starts with its line 0: the frame just finished is complete, so it
                // becomes the picture before the new line overwrites it.
                if self.drawn_frame != Some(frame) {
                    self.shown.copy_from_slice(&self.live);
                    self.drawn_frame = Some(frame);
                }
                self.sync_reference(bus);
                self.render_line(bus, line);
                self.lines_drawn += 1;
            } else if line == VISIBLE_LINES {
                self.latch_reference(bus);
            }
            self.next += 1;
        }
    }

    /// Adopt an affine reference point the software has written outside VBlank: GBATEK, *Internal
    /// Reference Point Registers* — "Writing to a reference point register by software outside of the
    /// Vblank period does immediately copy the new value to the corresponding internal register, that
    /// means: in the current frame, the new value specifies the origin of the current scanline".
    /// Comparing the register with the value last adopted is how a write is noticed without the bus
    /// having to tell us; the register is not written back, so the internal point is free to advance.
    fn sync_reference(&mut self, bus: &Bus) {
        for (bg, base) in [(0usize, BG2X), (1, BG3X)] {
            let written = register_point(bus, base);
            let slot = &mut self.reference[bg];
            if slot.register != Some(written) {
                slot.x = written.0;
                slot.y = written.1;
                slot.register = Some(written);
            }
        }
    }

    /// Copy both registers into their internal origins: at the start of VBlank, which is when GBATEK
    /// says the hardware does it.
    fn latch_reference(&mut self, bus: &Bus) {
        for (bg, base) in [(0usize, BG2X), (1, BG3X)] {
            let written = register_point(bus, base);
            let slot = &mut self.reference[bg];
            slot.x = written.0;
            slot.y = written.1;
            slot.register = Some(written);
        }
    }

    /// Move each affine origin on by `(PB, PD)`: GBATEK, "the internal registers are then incremented
    /// by dmx and dmy after each scanline".
    fn advance_reference(&mut self, bus: &Bus, backgrounds: &[usize]) {
        for &bg in backgrounds {
            let base = if bg == 0 { BG2PA } else { BG3PA };
            let (_, pb, _, pd) = matrix(bus, base);
            let slot = &mut self.reference[bg];
            slot.x = slot.x.wrapping_add(pb);
            slot.y = slot.y.wrapping_add(pd);
        }
    }

    /// Draw one visible scanline into `live`.
    fn render_line(&mut self, bus: &Bus, line: u16) {
        let row = usize::from(line) * WIDTH;
        let dispcnt = io16(bus, DISPCNT);
        let mode = (dispcnt & 7) as u8;
        if dispcnt & DISPCNT_FORCED_BLANK != 0 || mode > 5 {
            self.live[row..row + WIDTH].fill(WHITE);
            return;
        }
        let backdrop = palette_word(bus, 0);
        // Each background as it is at the start of this line: its priority and its pixels.
        let mut layers: [Option<(u8, LinePixels)>; 4] = [None, None, None, None];
        // Which backgrounds rotate/scale in this mode, indexed into `self.reference`: BG2 is 0 and
        // BG3 is 1. GBATEK's mode table — mode 1 is BG0/BG1 text plus an affine BG2, mode 2 is two
        // affine backgrounds, and modes 3-5 are one bitmap that *is* BG2 and takes the matrix too.
        let affine: &[usize] = match mode {
            1 | 3..=5 => &[0],
            2 => &[0, 1],
            _ => &[],
        };
        let text_layers = match mode {
            0 => 4,
            1 => 2,
            _ => 0,
        };
        for (bg, layer) in layers.iter_mut().enumerate().take(text_layers) {
            if dispcnt & (1 << (8 + bg)) != 0 {
                *layer = Some(text_layer(bus, bg, line));
            }
        }
        let origin = |ppu: &Ppu, bg: usize| (ppu.reference[bg].x, ppu.reference[bg].y);
        if mode == 1 || mode == 2 {
            for &bg in affine {
                let index = bg + 2;
                if dispcnt & (1 << (8 + index)) != 0 {
                    layers[index] = Some(self.affine_layer(bus, index, line, origin(self, bg)));
                }
            }
        }
        if (3..=5).contains(&mode) && dispcnt & (1 << 10) != 0 {
            let cnt = io16(bus, BG0CNT + 2 * BG_CNT_STRIDE);
            let (block_h, block_v) = if cnt & 0x0040 != 0 {
                bg_mosaic(bus)
            } else {
                (1, 1)
            };
            let start = self.mosaic_line_origin(0, line, block_v, origin(self, 0));
            layers[2] = Some(bitmap_layer(bus, mode, line, start, block_h));
        }
        // The objects are one more layer, and their non-transparent dots can also mark the object
        // window. Both come from the same pass over OAM: the scan is a function of the line.
        let objects = if dispcnt & DISPCNT_OBJECTS != 0 {
            Some(object_scan(bus, line, mode, dispcnt))
        } else {
            None
        };
        let windows = Windows::new(bus, dispcnt);
        let blend = Blending::new(bus);
        for x in 0..WIDTH {
            let obj_window = objects.as_ref().is_some_and(|o| o.window[x]);
            let bits = windows.region(x, line, obj_window);
            let mut candidates: [Option<Pixel>; 6] = [None; 6];
            for (bg, layer) in layers.iter().enumerate() {
                if bits & (1 << bg) == 0 {
                    continue;
                }
                if let Some((priority, (pixels, visible))) = layer
                    && visible[x]
                {
                    candidates[bg] = Some(Pixel {
                        priority: *priority,
                        order: 1 + bg as u8,
                        colour: pixels[x],
                        target: Target::Bg(bg),
                    });
                }
            }
            if bits & 0x10 != 0
                && let Some(objects) = &objects
                && objects.visible[x]
            {
                candidates[4] = Some(Pixel {
                    priority: objects.priority[x],
                    order: 0,
                    colour: objects.colour[x],
                    target: Target::Object {
                        semi: objects.semi[x],
                    },
                });
            }
            candidates[5] = Some(Pixel {
                priority: u8::MAX,
                order: u8::MAX,
                colour: backdrop,
                target: Target::Backdrop,
            });
            self.live[row + x] = compose(candidates, bits & 0x20 != 0, blend);
        }
        // The origins move on after the line, not before it: `PB`/`PD` are the step *between* lines.
        self.advance_reference(bus, affine);
    }

    /// One rotation/scaling background's line: 8bpp tiles, a map of one byte a tile, and a reference
    /// point that the line's `x` moves along by `PA` and `PC`.
    fn affine_layer(
        &mut self,
        bus: &Bus,
        bg: usize,
        line: u16,
        origin: (i32, i32),
    ) -> (u8, LinePixels) {
        let cnt = io16(bus, BG0CNT + bg * BG_CNT_STRIDE);
        let priority = (cnt & 3) as u8;
        let char_base = usize::from((cnt >> 2) & 0xF) * 0x4000;
        let map_base = usize::from((cnt >> 8) & 0x1F) * 0x800;
        let wrap = cnt & 0x2000 != 0;
        // GBATEK, *Internal Screen Size*: 128x128, 256x256, 512x512, 1024x1024.
        let size = 128usize << ((cnt >> 14) & 3);
        let (pa, _, pc, _) = matrix(bus, if bg == 2 { BG2PA } else { BG3PA });
        let (block_h, block_v) = if cnt & 0x0040 != 0 {
            bg_mosaic(bus)
        } else {
            (1, 1)
        };
        let origin = self.mosaic_line_origin(bg - 2, line, block_v, origin);
        let tiles = size / 8;
        let mut out: LinePixels = ([0; WIDTH], [false; WIDTH]);
        for x in 0..WIDTH {
            let screen_x = (x - x % block_h) as i32;
            let tex_x = origin.0.wrapping_add(pa.wrapping_mul(screen_x)) >> 8;
            let tex_y = origin.1.wrapping_add(pc.wrapping_mul(screen_x)) >> 8;
            let (tx, ty) = if wrap {
                (
                    (tex_x & (size as i32 - 1)) as usize,
                    (tex_y & (size as i32 - 1)) as usize,
                )
            } else if tex_x < 0 || tex_y < 0 || tex_x >= size as i32 || tex_y >= size as i32 {
                // GBATEK: outside the background is "displayed as transparent" unless the area
                // overflow bit wraps it.
                continue;
            } else {
                (tex_x as usize, tex_y as usize)
            };
            let tile = usize::from(bg_vram(bus, map_base + (ty / 8) * tiles + tx / 8));
            let index = usize::from(bg_vram(
                bus,
                char_base + tile * 64 + (ty & 7) * 8 + (tx & 7),
            ));
            if index == 0 {
                continue;
            }
            out.0[x] = palette_word(bus, index);
            out.1[x] = true;
        }
        (priority, out)
    }

    /// The origin a mosaic block samples from: its top line decides, so that origin is kept until
    /// the block ends. GBATEK, *Mosaic Size*: "pixels 0-5 of each display row are colorized as pixel
    /// 0" — the whole covered area takes the colour of its upper-left pixel.
    fn mosaic_line_origin(
        &mut self,
        slot: usize,
        line: u16,
        block_v: usize,
        origin: (i32, i32),
    ) -> (i32, i32) {
        if block_v <= 1 {
            return origin;
        }
        if usize::from(line) % block_v == 0 || self.mosaic_origin[slot].is_none() {
            self.mosaic_origin[slot] = Some(origin);
        }
        self.mosaic_origin[slot].unwrap_or(origin)
    }

    /// Draw one frame without a clock, for a test that wants a picture from registers alone.
    #[cfg(test)]
    pub fn render_frame_for_test(&mut self, bus: &Bus) {
        for line in 0..VISIBLE_LINES {
            self.render_line(bus, line);
        }
        self.shown.copy_from_slice(&self.live);
    }
}

/// The mosaic block sizes for backgrounds, `(horizontal, vertical)`, each at least 1. GBATEK,
/// *Mosaic Size*: "When setting H-Size to 5, then pixels 0-5 of each display row are colorized as
/// pixel 0", so the stored value is the size minus one.
fn bg_mosaic(bus: &Bus) -> (usize, usize) {
    let mosaic = io16(bus, MOSAIC);
    (
        usize::from(mosaic & 0xF) + 1,
        usize::from((mosaic >> 4) & 0xF) + 1,
    )
}

/// One text background's line: modes 0 and 1 are "Tile/Map based", 4bpp or 8bpp, 256x256 to 512x512
/// pixels of 8x8 tiles scrolled by `BGxHOFS`/`VOFS`.
fn text_layer(bus: &Bus, bg: usize, line: u16) -> (u8, LinePixels) {
    let cnt = io16(bus, BG0CNT + bg * BG_CNT_STRIDE);
    let priority = (cnt & 3) as u8;
    let char_base = usize::from((cnt >> 2) & 3) * 0x4000;
    let map_base = usize::from((cnt >> 8) & 0x1F) * 0x800;
    let colour8 = cnt & 0x0080 != 0;
    let size = (cnt >> 14) & 3;
    let (map_w, map_h) = match size {
        0 => (256usize, 256usize),
        1 => (512, 256),
        2 => (256, 512),
        _ => (512, 512),
    };
    let scroll = BG0HOFS + bg * BG_SCROLL_STRIDE;
    let hofs = usize::from(io16(bus, scroll) & 0x1FF);
    let vofs = usize::from(io16(bus, scroll + 2) & 0x1FF);
    let (block_h, block_v) = if cnt & 0x0040 != 0 {
        bg_mosaic(bus)
    } else {
        (1, 1)
    };
    let line = usize::from(line);
    let source_y = (line - line % block_v + vofs) & (map_h - 1);

    let mut out: LinePixels = ([0; WIDTH], [false; WIDTH]);
    for x in 0..WIDTH {
        let source_x = (x - x % block_h + hofs) & (map_w - 1);
        // Which 256x256 screen block of the map this pixel falls in (GBATEK, *Text Modes*: SC0 at
        // base+0, then SC1 at +2K, SC2 at +4K, SC3 at +6K, laid out left-right then top-bottom).
        let block = match size {
            0 => 0,
            1 => source_x / 256,
            2 => source_y / 256,
            _ => (source_y / 256) * 2 + source_x / 256,
        };
        let tile_x = (source_x & 0xFF) >> 3;
        let tile_y = (source_y & 0xFF) >> 3;
        let entry_at = map_base + block * 0x800 + (tile_y * 32 + tile_x) * 2;
        let entry =
            u16::from(bg_vram(bus, entry_at)) | (u16::from(bg_vram(bus, entry_at + 1)) << 8);
        let tile = usize::from(entry & 0x3FF);
        let mut px = source_x & 7;
        let mut py = source_y & 7;
        if entry & 0x0400 != 0 {
            px = 7 - px;
        }
        if entry & 0x0800 != 0 {
            py = 7 - py;
        }
        let palette_bank = usize::from((entry >> 12) & 0xF);
        let (index, transparent) = if colour8 {
            let index = usize::from(bg_vram(bus, char_base + tile * 64 + py * 8 + px));
            (index, index == 0)
        } else {
            let byte = usize::from(bg_vram(bus, char_base + tile * 32 + py * 4 + px / 2));
            let nibble = if px & 1 == 0 { byte & 0xF } else { byte >> 4 };
            // GBATEK: "Color 0 of all BG and OBJ palettes is transparent" — every 16-colour
            // palette has its own transparent entry 0, not just palette 0.
            ((palette_bank << 4) | nibble, nibble == 0)
        };
        if transparent {
            continue;
        }
        out.0[x] = palette_word(bus, index);
        out.1[x] = true;
    }
    (priority, out)
}

/// The bitmap background of modes 3, 4 and 5: a single picture in VRAM, `BG2`, which takes the same
/// rotation/scaling matrix as a tile background — GBATEK, *Bitmap BG Modes*: "Bitmaps are
/// implemented as BG2, with Rotation/Scaling support". Mode 3 is 240x160 at 15 bits a pixel and has
/// no second page; mode 4 is 240x160 at 8 bits a pixel with a page flip; mode 5 is 160x128 at 15 bits
/// with the same flip. Outside the bitmap is transparent in every one of them (the area overflow bit
/// is ignored in bitmap modes).
fn bitmap_layer(
    bus: &Bus,
    mode: u8,
    _line: u16,
    origin: (i32, i32),
    block_h: usize,
) -> (u8, LinePixels) {
    let cnt = io16(bus, BG0CNT + 2 * BG_CNT_STRIDE);
    let priority = (cnt & 3) as u8;
    let (pa, _, pc, _) = matrix(bus, BG2PA);
    let page = if mode == 3 {
        0
    } else if io16(bus, DISPCNT) & DISPCNT_FRAME_SELECT != 0 {
        0xA000
    } else {
        0
    };
    // Width, height, and bytes in a row: two bytes a pixel in the 15-bit modes.
    let (tex_w, tex_h, stride) = match mode {
        3 => (240usize, 160usize, 480usize),
        4 => (240, 160, 240),
        _ => (160, 128, 320),
    };
    let mut out: LinePixels = ([0; WIDTH], [false; WIDTH]);
    for x in 0..WIDTH {
        let screen_x = (x - x % block_h) as i32;
        let tex_x = origin.0.wrapping_add(pa.wrapping_mul(screen_x)) >> 8;
        let tex_y = origin.1.wrapping_add(pc.wrapping_mul(screen_x)) >> 8;
        if tex_x < 0 || tex_y < 0 || tex_x >= tex_w as i32 || tex_y >= tex_h as i32 {
            continue;
        }
        let (tx, ty) = (tex_x as usize, tex_y as usize);
        if mode == 4 {
            let index = usize::from(vram(bus, page + ty * stride + tx));
            // "Color 0 (backdrop) is transparent, and OBJs may be displayed behind the bitmap."
            if index == 0 {
                continue;
            }
            out.0[x] = palette_word(bus, index);
        } else {
            let at = page + ty * stride + tx * 2;
            out.0[x] = u16::from(vram(bus, at)) | (u16::from(vram(bus, at + 1)) << 8);
        }
        out.1[x] = true;
    }
    (priority, out)
}

// ---------------------------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------------------------

/// The object layer's pixels of one line, and the object-window mask the same pass produces.
struct Objects {
    colour: [u16; WIDTH],
    visible: [bool; WIDTH],
    priority: [u8; WIDTH],
    semi: [bool; WIDTH],
    window: [bool; WIDTH],
}

/// GBATEK's twelve shapes: `(width, height)` for an OBJ shape and size.
fn object_size(shape: u16, size: u16) -> (usize, usize) {
    const SQUARE: [(usize, usize); 4] = [(8, 8), (16, 16), (32, 32), (64, 64)];
    const HORIZONTAL: [(usize, usize); 4] = [(16, 8), (32, 8), (32, 16), (64, 32)];
    const VERTICAL: [(usize, usize); 4] = [(8, 16), (8, 32), (16, 32), (32, 64)];
    let size = usize::from(size & 3);
    match shape & 3 {
        0 => SQUARE[size],
        1 => HORIZONTAL[size],
        _ => VERTICAL[size],
    }
}

/// One of the 32 rotation/scaling groups: group `g` has `PA` at `06h + g*20h`, then `PB` eight bytes
/// on, `PC` sixteen and `PD` twenty-four (GBATEK, *Location of Rotation/Scaling Parameters in OAM*).
fn object_matrix(bus: &Bus, group: usize) -> (i32, i32, i32, i32) {
    let base = 0x06 + group * 0x20;
    (
        fixed16(oam16(bus, base)),
        fixed16(oam16(bus, base + 8)),
        fixed16(oam16(bus, base + 0x10)),
        fixed16(oam16(bus, base + 0x18)),
    )
}

/// One pixel of an object's tiles, or `None` when it is transparent (palette index 0).
///
/// GBATEK, *LCD OBJ - VRAM Character (Tile) Mapping*: the 2D mapping is a 32x32 matrix of 8x8 tiles
/// whose character numbers step by 32 a tile row — by two a tile in 256-colour mode, where one 8x8
/// tile is 64 bytes — and the 1D mapping lays the object's tiles consecutively after the character
/// name, 32 or 64 bytes each.
fn object_pixel(
    bus: &Bus,
    tile_number: usize,
    colour8: bool,
    one_d: bool,
    width: usize,
    px: usize,
    py: usize,
) -> Option<usize> {
    let tile_x = px / 8;
    let tile_y = py / 8;
    let tiles_per_row = width / 8;
    let offset = if one_d {
        tile_number * 32 + (tile_y * tiles_per_row + tile_x) * if colour8 { 64 } else { 32 }
    } else if colour8 {
        (tile_number + tile_x * 2 + tile_y * 32) * 32
    } else {
        (tile_number + tile_x + tile_y * 32) * 32
    };
    if colour8 {
        let index = obj_vram(bus, offset + (py & 7) * 8 + (px & 7));
        (index != 0).then_some(usize::from(index))
    } else {
        let byte = obj_vram(bus, offset + (py & 7) * 4 + (px & 7) / 2);
        let nibble = if px & 1 == 0 { byte & 0xF } else { byte >> 4 };
        (nibble != 0).then_some(usize::from(nibble))
    }
}

/// All 128 objects on one scanline: the top-most visible pixel of each object pixel column and the
/// object-window mask. The pass runs once a line, so a mid-line OAM or matrix write lands on the
/// next line exactly as a register write does.
fn object_scan(bus: &Bus, line: u16, mode: u8, dispcnt: u16) -> Objects {
    let mut out = Objects {
        colour: [0; WIDTH],
        visible: [false; WIDTH],
        priority: [0; WIDTH],
        semi: [false; WIDTH],
        window: [false; WIDTH],
    };
    let one_d = dispcnt & DISPCNT_1D_OBJECTS != 0;
    let obj_window_enabled = dispcnt & DISPCNT_OBJ_WINDOW != 0;
    let mosaic = io16(bus, MOSAIC);
    let mosaic_h = usize::from((mosaic >> 8) & 0xF) + 1;
    let mosaic_v = usize::from((mosaic >> 12) & 0xF) + 1;
    for index in 0..128usize {
        let at = index * 8;
        let attr0 = oam16(bus, at);
        let attr1 = oam16(bus, at + 2);
        let attr2 = oam16(bus, at + 4);
        let shape = attr0 >> 14;
        let object_mode = (attr0 >> 10) & 3;
        if shape == 3 || object_mode == 3 {
            continue; // GBATEK: both are "Prohibited", so nothing is displayed.
        }
        let is_window = object_mode == 2;
        if is_window && !obj_window_enabled {
            // An object-window OBJ is only a region when DISPCNT bit 15 is set, and it is never
            // displayed itself (GBATEK, *The OBJ Window*).
            continue;
        }
        let rotated = attr0 & 0x0100 != 0;
        let double = rotated && attr0 & 0x0200 != 0;
        if !rotated && attr0 & 0x0200 != 0 {
            continue; // the "OBJ Disable" bit
        }
        let (width, height) = object_size(shape, (attr1 >> 14) & 3);
        let (box_w, box_h) = if double {
            (width * 2, height * 2)
        } else {
            (width, height)
        };
        let y = i32::from(attr0 & 0xFF);
        let x = i32::from(attr1 & 0x1FF);
        // GBATEK's Y coordinate is 8 bits and the comparison wraps inside it: that is exactly the
        // documented exception where a tall object at Y>128 is "treated as at Y>-128" and shows at
        // the top of the screen instead of the bottom.
        let dy = (i32::from(line) - y).rem_euclid(256) as usize;
        if dy >= box_h {
            continue;
        }
        let tile_number = usize::from(attr2 & 0x3FF);
        // GBATEK, *OBJ Tile Number*: "When using BG Mode 3-5 ... only tile numbers 512-1023 may be
        // used ... Attempts to use tiles 0-511 are ignored (not displayed)."
        if (3..=5).contains(&mode) && tile_number < 512 {
            continue;
        }
        let colour8 = attr0 & 0x2000 != 0;
        let palette_bank = usize::from((attr2 >> 12) & 0xF);
        let priority = ((attr2 >> 10) & 3) as u8;
        let (block_h, block_v) = if attr0 & 0x1000 != 0 {
            (mosaic_h, mosaic_v)
        } else {
            (1, 1)
        };
        let (pa, pb, pc, pd) = if rotated {
            object_matrix(bus, usize::from((attr1 >> 9) & 0x1F))
        } else {
            (0, 0, 0, 0)
        };
        // The rotation centre is the middle of the *normal* object, which is also the middle of a
        // double-size box: GBATEK, *OBJ Reference Point & Rotation Center*.
        let centre_x = (box_w / 2) as i32;
        let centre_y = (box_h / 2) as i32;
        for screen_x in 0..WIDTH {
            // The X coordinate is 9 bits wide, so the comparison wraps at 512: an object at X=500
            // shows its first columns at the left edge (the horizontal half of the Y wrap above).
            let dx = (screen_x as i32 - x).rem_euclid(512) as usize;
            if dx >= box_w {
                continue;
            }
            let (mut px, mut py) = if rotated {
                let sx = dx as i32 - centre_x;
                let sy = dy as i32 - centre_y;
                let ox = ((pa * sx + pb * sy) >> 8) + (width / 2) as i32;
                let oy = ((pc * sx + pd * sy) >> 8) + (height / 2) as i32;
                if ox < 0 || oy < 0 || ox >= width as i32 || oy >= height as i32 {
                    continue;
                }
                (ox as usize, oy as usize)
            } else {
                let mut px = dx;
                let mut py = dy;
                if attr1 & 0x1000 != 0 {
                    px = width - 1 - px;
                }
                if attr1 & 0x2000 != 0 {
                    py = height - 1 - py;
                }
                (px, py)
            };
            // Mosaic: the block takes the colour of its upper-left covered pixel (GBATEK,
            // *Mosaic Size*), in the object's own coordinates.
            px -= px % block_h;
            py -= py % block_v;
            let Some(index) = object_pixel(bus, tile_number, colour8, one_d, width, px, py) else {
                continue;
            };
            if is_window {
                out.window[screen_x] = true;
                continue;
            }
            // GBATEK, *DS Video OBJs*: the top-most object pixel is chosen by OAM entry number, so
            // the first object to claim a pixel keeps it; its two-bit priority then competes with
            // the backgrounds in `compose`.
            if !out.visible[screen_x] {
                out.visible[screen_x] = true;
                out.colour[screen_x] = if colour8 {
                    object_palette_word(bus, index)
                } else {
                    object_palette_word(bus, (palette_bank << 4) | index)
                };
                out.priority[screen_x] = priority;
                out.semi[screen_x] = object_mode == 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Windows and the colour effects
// ---------------------------------------------------------------------------------------------

/// One window's rectangle, GBATEK's clamps already applied.
struct WindowRect {
    x1: u16,
    x2: u16,
    y1: u16,
    y2: u16,
}

impl WindowRect {
    fn read(bus: &Bus, horizontal: usize, vertical: usize) -> WindowRect {
        let hor = io16(bus, horizontal);
        let ver = io16(bus, vertical);
        let (x1, mut x2) = (hor >> 8, hor & 0xFF);
        // GBATEK: "Garbage values of X2>240 or X1>X2 are interpreted as X2=240."
        if x2 > WIDTH as u16 || x1 > x2 {
            x2 = WIDTH as u16;
        }
        let (y1, mut y2) = (ver >> 8, ver & 0xFF);
        if y2 > HEIGHT as u16 || y1 > y2 {
            y2 = HEIGHT as u16;
        }
        WindowRect { x1, x2, y1, y2 }
    }

    fn inside(&self, x: usize, line: u16) -> bool {
        let x = x as u16;
        x >= self.x1 && x < self.x2 && line >= self.y1 && line < self.y2
    }
}

/// The window registers of one line: which region a pixel falls in decides which layers display
/// there and whether a colour special effect runs there (GBATEK, *LCD I/O Window Feature*).
struct Windows {
    win0: WindowRect,
    win1: WindowRect,
    winin: u16,
    winout: u16,
    dispcnt: u16,
}

impl Windows {
    fn new(bus: &Bus, dispcnt: u16) -> Windows {
        Windows {
            win0: WindowRect::read(bus, WIN0H, WIN0V),
            win1: WindowRect::read(bus, WIN1H, WIN1V),
            winin: io16(bus, WININ),
            winout: io16(bus, WINOUT),
            dispcnt,
        }
    }

    /// The six enable bits of the region a pixel is in: BG0-BG3, objects, and the colour effect.
    /// GBATEK, *Window Priority*: "Window 0 is having highest priority, Window 1 medium, and Obj
    /// Window lowest", and outside is what is left. A window only exists when DISPCNT enables it.
    fn region(&self, x: usize, line: u16, obj_window: bool) -> u16 {
        // With no window enabled at all the window feature is off and every layer the display
        // control allows shows everywhere — that is why a cartridge that never touches WININ or
        // WINOUT still draws. GBATEK: "if any of these regions is enabled then the 'Outside of
        // Windows' region is automatically enabled, too".
        let any = DISPCNT_WIN0 | DISPCNT_WIN1 | DISPCNT_OBJ_WINDOW;
        if self.dispcnt & any == 0 {
            return 0x3F;
        }
        if self.dispcnt & DISPCNT_WIN0 != 0 && self.win0.inside(x, line) {
            return self.winin & 0x3F;
        }
        if self.dispcnt & DISPCNT_WIN1 != 0 && self.win1.inside(x, line) {
            return (self.winin >> 8) & 0x3F;
        }
        if self.dispcnt & DISPCNT_OBJ_WINDOW != 0 && obj_window {
            return (self.winout >> 8) & 0x3F;
        }
        self.winout & 0x3F
    }
}

/// Which surface a pixel came from, as `BLDCNT` names them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Bg(usize),
    Object { semi: bool },
    Backdrop,
}

/// One layer's candidate pixel at one column.
#[derive(Clone, Copy)]
struct Pixel {
    priority: u8,
    order: u8,
    colour: u16,
    target: Target,
}

impl Pixel {
    /// Display order: lower priority number first, then the tie rules — BG0 below BG1 and so on,
    /// and the object layer above every background at equal priority (GBATEK, *Priority*).
    fn key(self) -> u16 {
        (u16::from(self.priority) << 8) | u16::from(self.order)
    }
}

/// `BLDCNT`'s first-target bit for a layer.
fn target_bit(target: Target) -> u16 {
    match target {
        Target::Bg(bg) => 1 << bg,
        Target::Object { .. } => 1 << 4,
        Target::Backdrop => 1 << 5,
    }
}

/// `BLDCNT`, `BLDALPHA` and `BLDY` for one line. The coefficients are clamped at 16/16 as GBATEK
/// says ("17..31=16/16").
#[derive(Clone, Copy)]
struct Blending {
    bldcnt: u16,
    eva: u32,
    evb: u32,
    evy: u32,
}

impl Blending {
    fn new(bus: &Bus) -> Blending {
        let bldcnt = io16(bus, BLDCNT);
        let alpha = io16(bus, BLDALPHA);
        Blending {
            bldcnt,
            eva: u32::from(alpha & 0x1F).min(16),
            evb: u32::from((alpha >> 8) & 0x1F).min(16),
            evy: u32::from(io16(bus, BLDY) & 0x1F).min(16),
        }
    }
}

/// Choose the top two pixels in display order and apply the colour effect to the top one.
///
/// GBATEK, *Color Special Effects*: alpha blending mixes the top-most non-transparent pixel that is
/// a first target with the next-lower non-transparent pixel **only if that one is a second target**;
/// otherwise the top pixel shows at normal intensity. Brightness has no second target. A
/// semi-transparent object is always a first target and always alpha-blends, whatever `BLDCNT`'s
/// effect bits say.
fn compose(candidates: [Option<Pixel>; 6], effects: bool, blend: Blending) -> u16 {
    let mut first: Option<Pixel> = None;
    let mut second: Option<Pixel> = None;
    for pixel in candidates.into_iter().flatten() {
        match first {
            None => first = Some(pixel),
            Some(top) => {
                if pixel.key() < top.key() {
                    if second.is_none_or(|s: Pixel| top.key() < s.key()) {
                        second = Some(top);
                    }
                    first = Some(pixel);
                } else if second.is_none_or(|s: Pixel| pixel.key() < s.key()) {
                    second = Some(pixel);
                }
            }
        }
    }
    // The backdrop is always offered, so this is never empty.
    let first = first.expect("the backdrop is a candidate");
    let colour = first.colour;
    if !effects {
        return colour;
    }
    let semi = matches!(first.target, Target::Object { semi: true });
    let effect = (blend.bldcnt >> 6) & 3;
    let first_is_target = blend.bldcnt & target_bit(first.target) != 0;
    let second_is_target = second.is_some_and(|s| blend.bldcnt & (target_bit(s.target) << 8) != 0);
    if semi || (effect == 1 && first_is_target) {
        return match second {
            Some(second) if second_is_target => {
                alpha_blend(colour, second.colour, blend.eva, blend.evb)
            }
            _ => colour,
        };
    }
    if !first_is_target {
        return colour;
    }
    match effect {
        2 => brighten(colour, blend.evy),
        3 => darken(colour, blend.evy),
        _ => colour,
    }
}

/// `I = MIN(31, (I1*EVA + I2*EVB) / 16)` per channel: GBATEK, *BLDALPHA*.
fn alpha_blend(first: u16, second: u16, eva: u32, evb: u32) -> u16 {
    let mut out = 0u16;
    for shift in [0u32, 5, 10] {
        let a = u32::from((first >> shift) & 0x1F);
        let b = u32::from((second >> shift) & 0x1F);
        out |= (((a * eva + b * evb) >> 4).min(31) as u16) << shift;
    }
    out
}

/// `I = I1 + (31-I1)*EVY/16` per channel: GBATEK, *BLDY* brightness increase.
fn brighten(colour: u16, evy: u32) -> u16 {
    let mut out = 0u16;
    for shift in [0u32, 5, 10] {
        let i = u32::from((colour >> shift) & 0x1F);
        out |= ((i + (((31 - i) * evy) >> 4)) as u16) << shift;
    }
    out
}

/// `I = I1 - I1*EVY/16` per channel: GBATEK, *BLDY* brightness decrease.
fn darken(colour: u16, evy: u32) -> u16 {
    let mut out = 0u16;
    for shift in [0u32, 5, 10] {
        let i = u32::from((colour >> shift) & 0x1F);
        out |= ((i - ((i * evy) >> 4)) as u16) << shift;
    }
    out
}
