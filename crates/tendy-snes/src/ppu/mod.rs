//! The PPU: the register file, the line clock and the picture buffer.
//!
//! v1 renders one line at a time from a snapshot of the registers taken at that line's first dot
//! (§5.1). [`Ppu::tick`] counts dots at four master clocks each, raises §3.5's `LineEvents`, and at
//! the first dot of a visible line latches `PpuRegs`, evaluates the 128 OBJs, fills `main`/`sub` and
//! composes the line into `frame`.
//!
//! Two transient bitfields live in `hv_read`, because §2.5's field list has one `u8` for the read
//! flip-flops and no spare bytes anywhere:
//!
//! | bits | meaning |
//! |---|---|
//! | 0 | `$213C`'s first/second read flip-flop |
//! | 1 | `$213D`'s first/second read flip-flop |
//! | 2 | `$2122`/`$213B`'s first/second access flip-flop (fullsnes "2121h - CGADD": "Writing to this register resets the 1st/2nd access flipflop") |
//! | 3-4 | the clock phase: master clocks carried over from `Ppu::tick`'s last call, so 6-clock accesses are not rounded down to one dot |
//!
//! `hv_read == 0` still means "both read flip-flops are in their first-read state", which is what
//! §2.5 documents.

mod colour;
mod layers;
mod mode7;
mod regs;
mod sprites;

pub use regs::PpuRegs;

use crate::state::{StateError, StateReader, StateWriter};
use crate::{
    CGRAM_SIZE, DOTS_PER_LINE, FRAME_PIXELS, HEIGHT, LINES_PER_FRAME, OAM_SIZE, VRAM_SIZE, WIDTH,
};

/// One source pixel with everything composition needs, declared once for `layers.rs`,
/// `sprites.rs`, `mode7.rs` and `colour.rs` alike.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Pixel {
    pub layer: Layer,
    /// Colour index inside the layer's palette; 0 is transparent for every layer but `Backdrop`.
    pub index: u8,
    /// The CGRAM page: 0-7 for a BG, 8-15 for an OBJ, 0 for `Backdrop`.
    pub palette: u8,
    /// The per-tile or per-sprite priority bit(s) the mode's chart sorts on.
    pub priority: u8,
}

/// Which layer a pixel came from; the order is the priority chart's base order.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer {
    #[default]
    Backdrop = 0,
    Bg1,
    Bg2,
    Bg3,
    Bg4,
    Obj,
}

/// Events the PPU raises while the clock advances; `Bus::tick` acts on them (§3.3).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct LineEvents(pub u8);

impl LineEvents {
    /// H=0: latch registers, evaluate OBJs, draw the line.
    pub const START: u8 = 0x01;
    /// H=278: the per-line HDMA tick.
    pub const HDMA: u8 = 0x02;
    /// H=0.5 of V=225: NMI flag, joypad auto-read.
    pub const VBLANK: u8 = 0x04;
    /// H=274: the H-blank flag.
    pub const HBLANK: u8 = 0x08;
    /// The last dot of V=261: `frame_ready`.
    pub const END: u8 = 0x10;
    /// H=134: the DRAM-refresh pause of §3.2.
    pub const REFRESH: u8 = 0x20;
}

/// `hv_read` bit 0: `$213C` is waiting for its second read.
const HV_H: u8 = 0x01;
/// `hv_read` bit 1: `$213D` is waiting for its second read.
const HV_V: u8 = 0x02;
/// `hv_read` bit 2: `$2122` is holding a low byte.
const CG_FLIP: u8 = 0x04;
/// `hv_read` bits 3-4: the master clocks carried over from the last `tick`.
const PHASE: u8 = 0x18;
const PHASE_SHIFT: u32 = 3;

/// `Ppu::trace` length. A power of two, so the ring index is a mask, and large enough that a whole
/// frame of any of the three black-screen rows fits: the busiest of them moves 71 695 bytes to
/// `$2100-$213F` in its heaviest frame (a VRAM and CGRAM clear), and the probe says so if a frame
/// ever fills it.
pub const TRACE_LEN: usize = 131_072;

/// The picture processing unit.
#[derive(Clone)]
pub struct Ppu {
    /// Byte-addressed; word address = byte address >> 1.
    pub vram: Box<[u8; VRAM_SIZE]>,
    /// 128 x 4 bytes, then the 32-byte high table.
    pub oam: Box<[u8; OAM_SIZE]>,
    /// BGR555, 0 = backdrop.
    pub cgram: Box<[u16; CGRAM_SIZE]>,
    pub regs: PpuRegs,
    /// The snapshot the line being drawn uses.
    pub latched: PpuRegs,
    /// 57 344 BGR555 words, reused every frame.
    pub frame: Vec<u16>,
    pub main: [Pixel; WIDTH],
    pub sub: [Pixel; WIDTH],
    pub line_out: [u16; WIDTH],
    /// Dot inside the line; `line` is the V counter.
    pub dot: u16,
    pub line: u16,
    pub field: bool,
    /// `$2137` latches.
    pub latch_h: u16,
    pub latch_v: u16,
    pub latch_flag: bool,
    /// The read flip-flops and the clock phase; see the module comment.
    pub hv_read: u8,
    /// PPU1's own signed multiply result, 24 bits.
    pub m7_product: i32,
    /// The colour-math fixed colour, 15 bits.
    pub fixed_colour: u16,
    /// `$2139`/`$213A`: the VRAM read prefetch register.
    pub vram_prefetch: u16,
    /// The two interrupt lines this chip drives.
    pub nmi: bool,
    pub irq: bool,
    pub frame_ready: bool,
    /// `$213E` bit 7: more than 34 OBJ tiles on a line.
    pub obj_time_overflow: bool,
    /// `$213E` bit 6: more than 32 OBJs on a line.
    pub obj_range_overflow: bool,
    pub frames: u64,
    /// Diagnosis seam for the black-screen survey (issue #1 family F3): while `trace_on`, every
    /// write to `$2100-$213F` is also kept here as `(register << 8) | value`, so a probe can print
    /// **what** was written and not only how often — a count of palette writes cannot tell an
    /// upload from an upload of zeroes. Ring-indexed by `trace_len`; fixed size, no allocation, and
    /// off by default, so the frame path pays one predictable branch per PPU write and nothing else.
    pub trace: Box<[u16; TRACE_LEN]>,
    pub trace_len: u64,
    pub trace_on: bool,
}

// `new()` is the constructor §2.5 fixes; a `Default` impl is not part of the design's surface.
#[allow(clippy::new_without_default)]
impl Ppu {
    /// Power-on values of §4.5: every register and memory zero, counters at H=0 V=0.
    pub fn new() -> Ppu {
        Ppu {
            vram: Box::new([0; VRAM_SIZE]),
            oam: Box::new([0; OAM_SIZE]),
            cgram: Box::new([0; CGRAM_SIZE]),
            regs: PpuRegs::default(),
            latched: PpuRegs::default(),
            frame: vec![0; FRAME_PIXELS],
            main: [Pixel::default(); WIDTH],
            sub: [Pixel::default(); WIDTH],
            line_out: [0; WIDTH],
            dot: 0,
            line: 0,
            field: false,
            latch_h: 0,
            latch_v: 0,
            latch_flag: false,
            hv_read: 0,
            m7_product: 0,
            fixed_colour: 0,
            vram_prefetch: 0,
            nmi: false,
            irq: false,
            frame_ready: false,
            obj_time_overflow: false,
            obj_range_overflow: false,
            frames: 0,
            trace: Box::new([0; TRACE_LEN]),
            trace_len: 0,
            trace_on: false,
        }
    }

    /// Advances `ticks` master clocks, raising `LineEvents`.
    ///
    /// A dot is four master clocks, but an access costs 6, 8 or 12 (§3.2), so the two clocks a
    /// 6-clock access would otherwise drop are carried in `hv_read` bits 3-4: 341 dots per line and
    /// 262 lines per frame stay exact, which is what `io`'s H/V compares and `run_frame` assume.
    pub fn tick(&mut self, ticks: u32) -> LineEvents {
        let mut events = LineEvents(0);
        let clocks = u32::from((self.hv_read & PHASE) >> PHASE_SHIFT) + ticks;
        let dots = clocks / 4;
        self.hv_read = (self.hv_read & !PHASE) | (((clocks & 3) as u8) << PHASE_SHIFT);
        for _ in 0..dots {
            events.0 |= self.step_dot();
        }
        events
    }

    /// One dot: raise this dot's events, render if it is a visible line's first dot, then advance.
    fn step_dot(&mut self) -> u8 {
        let mut events = 0u8;
        if self.dot == 0 {
            events |= LineEvents::START;
            // §5.2 steps 1-8. Lines 0 and 225-261 are the OBJ-prefetch and V-blank lines; nothing is
            // visible there, and §5.2's step 8 only writes `frame` for 1..=224.
            if (1..=224).contains(&self.line) {
                self.render_line();
            }
            // §5.11: the overflow flags are set during evaluation and cleared at V=0.
            if self.line == 0 {
                self.obj_time_overflow = false;
                self.obj_range_overflow = false;
            }
            if self.line == crate::VBLANK_START {
                events |= LineEvents::VBLANK;
                self.nmi = true;
            }
        }
        if self.dot == 134 {
            events |= LineEvents::REFRESH;
        }
        if self.dot == 274 {
            events |= LineEvents::HBLANK;
        }
        if self.dot == 278 {
            events |= LineEvents::HDMA;
        }
        if self.line == crate::LAST_LINE && self.dot == (DOTS_PER_LINE - 1) as u16 {
            events |= LineEvents::END;
            self.frame_ready = true;
        }
        // Dot 340 is the last of 341, so the next dot starts the next line (fullsnes "SNES Timing
        // H/V Events": "H=339 this is last PPU-dot").
        self.dot += 1;
        if u32::from(self.dot) >= DOTS_PER_LINE {
            self.dot = 0;
            self.line += 1;
            if u32::from(self.line) >= LINES_PER_FRAME {
                self.line = 0;
                self.frames += 1;
                // The v-blank window has ended: fullsnes "4210h - RDNMI" (cached page lines
                // 1762-1764) "The NMI flag gets set at begin of Vblank ... The flag gets reset
                // automatically at end of Vblank", so the line this chip drives (§2.5: "The two
                // interrupt lines this chip drives, cleared when `CpuIo` acknowledges") is high on
                // V=225..261 only. It used to be raised at V=225 and never lowered, which made
                // `Ppu::nmi` a constant `true` after the first v-blank — no behavioural effect,
                // because `Bus::vblank` reads it at the same instant it sets `nmi_flag`, but a
                // false level in every probe's state dump.
                self.nmi = false;
            }
        }
        events
    }

    /// §5.2: latch, evaluate, render, compose, write the frame row.
    fn render_line(&mut self) {
        self.latched = self.regs.clone();
        self.main = [Pixel::default(); WIDTH];
        self.sub = [Pixel::default(); WIDTH];
        let mode = self.latched.bgmode & 0x07;
        if mode == 7 {
            mode7::draw(self);
        } else {
            layers::draw(self, mode);
        }
        sprites::draw(self);
        colour::compose(self);
    }

    /// `$2100`-`$213F` as an offset from `$2100`. Write-only ports are stored, never read back.
    pub fn write(&mut self, reg: u8, value: u8) {
        if self.trace_on {
            self.trace[(self.trace_len as usize) & (TRACE_LEN - 1)] =
                (u16::from(reg) << 8) | u16::from(value);
            self.trace_len += 1;
        }
        match reg {
            0x04 => self.write_oam_data(value),
            0x16 | 0x17 => {
                self.regs.write(reg, value);
                self.prefetch_vram();
            }
            0x18 => self.write_vram(value, true),
            0x19 => self.write_vram(value, false),
            0x1B | 0x1C => {
                self.regs.write(reg, value);
                // fullsnes "SNES Maths Multiply/Divide": the PPU maths starts on **any** M7A/M7B
                // write, unlike the CPU's WRMPYB-triggered engine; the multiplier is the last
                // `$211C` byte taken as a signed 8-bit value.
                self.m7_product = i32::from(self.regs.m7[0]) * i32::from(self.regs.m7b_byte as i8);
            }
            0x21 => {
                self.regs.write(reg, value);
                self.hv_read &= !CG_FLIP;
            }
            0x22 => self.write_cgram(value),
            0x32 => {
                self.regs.write(reg, value);
                self.apply_coldata(value);
            }
            _ => self.regs.write(reg, value),
        }
    }

    /// `$2100`-`$213F` as an offset from `$2100`; `None` means write-only, so the bus answers open
    /// bus. The read block is `$2134`-`$213F`; everything below `$2134` is write-only (fullsnes
    /// heads the block "PPU Picture Processing Unit (Write-Only Ports)").
    pub fn read(&mut self, reg: u8) -> Option<u8> {
        match reg {
            0x34 => Some(self.m7_product as u8),
            0x35 => Some((self.m7_product >> 8) as u8),
            0x36 => Some((self.m7_product >> 16) as u8),
            0x37 => {
                // `$2137` is a strobe: the value is the CPU's open bus and only the side effect is
                // the PPU's (§4.3). The `WRIO` bit-7 gate that fullsnes describes lives in `Bus`,
                // which this lane does not own; the lane reports it rather than editing that file.
                self.latch_hv();
                None
            }
            0x38 => Some(self.read_oam_data()),
            0x39 => Some(self.read_vram(true)),
            0x3A => Some(self.read_vram(false)),
            0x3B => Some(self.read_cgram()),
            0x3C => Some(self.read_counter(true)),
            0x3D => Some(self.read_counter(false)),
            0x3E => {
                // fullsnes "213Eh - STAT77": bit 5 master/slave (always master here), bit 4 PPU1
                // open bus (v1 answers 0), bits 3-0 version 1.
                let mut value = 0x01;
                if self.obj_time_overflow {
                    value |= 0x80;
                }
                if self.obj_range_overflow {
                    value |= 0x40;
                }
                Some(value)
            }
            0x3F => {
                // bit 7 interlace field, bit 6 latch flag, bit 5 PPU2 open bus (0), bit 4 PAL (0 =
                // NTSC), bits 3-0 PPU2 version 3. Reading clears the latch flag and both read
                // flip-flops (fullsnes "213Fh - STAT78").
                let mut value = 0x03;
                if self.field {
                    value |= 0x80;
                }
                if self.latch_flag {
                    value |= 0x40;
                }
                self.latch_flag = false;
                self.hv_read &= !(HV_H | HV_V);
                Some(value)
            }
            _ => None,
        }
    }

    /// True once per finished frame, clearing `frame_ready`.
    pub fn take_frame(&mut self) -> bool {
        let ready = self.frame_ready;
        self.frame_ready = false;
        ready
    }

    pub fn frame(&self) -> &[u16] {
        debug_assert_eq!(self.frame.len(), WIDTH * HEIGHT);
        &self.frame
    }

    /// Resets the H/V counter latch and the two read flip-flops (`$213F`).
    pub fn reset_latch(&mut self) {
        self.latch_flag = false;
        self.hv_read &= PHASE;
    }

    /// Copies the H and V counters into the latches: `$2137`, gated by `WRIO` bit 7 in `Bus`.
    pub fn latch_hv(&mut self) {
        self.latch_h = self.dot;
        self.latch_v = self.line;
        self.latch_flag = true;
    }

    /// Copies the 9-bit OAM address reload value into the address register: the PPU rewrites its
    /// OAM address from it at the start of vblank (fullsnes "SNES Memory OAM Access").
    ///
    /// `regs.oamadd` holds the live byte address, so what is restored here is that address with bit
    /// 0 forced even — the reload value itself after the usual "set the address, then transfer"
    /// sequence. See `regs.rs` for why the field cannot hold both.
    pub fn reload_oamadd(&mut self) {
        self.regs.oamadd &= !0x0001;
    }

    /// `$2115`'s temporary address translation, applied to an access only and never to `$2116`.
    fn vram_word_address(&self) -> u16 {
        let address = self.regs.vmadd;
        let bits = match (self.regs.vmain >> 2) & 0x03 {
            0 => return address,
            1 => 8,
            2 => 9,
            _ => 10,
        };
        let mask = (1u16 << bits) - 1;
        let low = address & mask;
        // fullsnes "2115h - VMAIN": "technically it does thrice left-rotate the lower 8, 9, or 10
        // bits of the Word-address", with the diagram `aaaaaaaaYYYxxxxx --> aaaaaaaaxxxxxYYY`.
        // The rotate is taken inside the field, or the top of the field would land in the
        // address's high half.
        let low = ((low << 3) | (low >> (bits - 3))) & mask;
        (address & !mask) | low
    }

    /// `$2115` bits 1-0: the step the word address takes after an access.
    fn vram_step(&self) -> u16 {
        match self.regs.vmain & 0x03 {
            0 => 1,
            1 => 32,
            2 | 3 => 128,
            _ => 1,
        }
    }

    /// `$2115` bit 7: true when this access is the one after which the address steps.
    fn increments_now(&self, low: bool) -> bool {
        (self.regs.vmain & 0x80 == 0) == low
    }

    /// `$2118`/`$2119`: one byte of the addressed word, then the step (fullsnes "2118h - VMDATAL").
    fn write_vram(&mut self, value: u8, low: bool) {
        let word = usize::from(self.vram_word_address() & 0x7FFF);
        let base = word * 2;
        self.vram[base + usize::from(!low)] = value;
        if self.increments_now(low) {
            let step = self.vram_step();
            self.regs.vmadd = self.regs.vmadd.wrapping_add(step);
        }
    }

    /// `$2139`/`$213A`. The prefetch register is filled after an `$2116`/`$2117` write and again
    /// **before** the increment, which is the "first word twice" behaviour (fullsnes "SNES Memory
    /// VRAM Access": "1st Send a byte from OLD prefetch value ... 2nd Load NEW value from OLD
    /// address ... 3rd Increment address").
    fn read_vram(&mut self, low: bool) -> u8 {
        let value = if low {
            self.vram_prefetch as u8
        } else {
            (self.vram_prefetch >> 8) as u8
        };
        if self.increments_now(low) {
            self.prefetch_vram();
            let step = self.vram_step();
            self.regs.vmadd = self.regs.vmadd.wrapping_add(step);
        }
        value
    }

    /// Fills the prefetch register from the currently addressed word.
    fn prefetch_vram(&mut self) {
        let word = usize::from(self.vram_word_address() & 0x7FFF);
        let base = word * 2;
        self.vram_prefetch = u16::from(self.vram[base]) | (u16::from(self.vram[base + 1]) << 8);
    }

    /// `$2104`. Below `$200` the port is a word: even byte latches, odd byte writes both halves.
    /// At `$200` and above every write lands on its own byte (fullsnes "2104h - OAMDATA").
    fn write_oam_data(&mut self, value: u8) {
        let live = self.regs.oamadd & regs::OAMADD_ADDRESS;
        let address = oam_physical(live);
        if live & 1 == 0 {
            self.regs.oam_lsb = value;
        }
        if live < 0x200 && live & 1 == 1 {
            self.oam[address - 1] = self.regs.oam_lsb;
            self.oam[address] = value;
        }
        if live >= 0x200 {
            self.oam[address] = value;
        }
        self.set_oam_live(live + 1);
    }

    /// `$2138`: one OAM byte, then the address steps (fullsnes "OAMDATAREAD").
    fn read_oam_data(&mut self) -> u8 {
        let live = self.regs.oamadd & regs::OAMADD_ADDRESS;
        let value = self.oam[oam_physical(live)];
        self.set_oam_live(live + 1);
        value
    }

    /// Stores the live OAM byte address, keeping the rotation flag.
    fn set_oam_live(&mut self, live: u16) {
        self.regs.oamadd =
            (self.regs.oamadd & !regs::OAMADD_ADDRESS) | (live & regs::OAMADD_ADDRESS);
    }

    /// `$2122`. Two writes make one BGR555 word (fullsnes "CGDATA - CGRAM data write").
    fn write_cgram(&mut self, value: u8) {
        if self.hv_read & CG_FLIP == 0 {
            self.regs.cgram_lsb = value;
            self.hv_read |= CG_FLIP;
        } else {
            let word = u16::from(self.regs.cgram_lsb) | (u16::from(value & 0x7F) << 8);
            let index = usize::from(self.regs.cgadd);
            self.cgram[index] = word;
            self.regs.cgadd = self.regs.cgadd.wrapping_add(1);
            self.hv_read &= !CG_FLIP;
        }
    }

    /// `$213B`: the low byte, then the high 7 bits (bit 15 reads as PPU2 open bus, so v1 answers 0).
    fn read_cgram(&mut self) -> u8 {
        let index = usize::from(self.regs.cgadd);
        let word = self.cgram[index];
        let second = self.hv_read & CG_FLIP != 0;
        if second {
            self.regs.cgadd = self.regs.cgadd.wrapping_add(1);
            self.hv_read &= !CG_FLIP;
            ((word >> 8) & 0x7F) as u8
        } else {
            self.hv_read |= CG_FLIP;
            word as u8
        }
    }

    /// `$213C`/`$213D`: the low byte, then bit 0; bits 7-1 of the second read are PPU2 open bus,
    /// which v1 answers as 0 (fullsnes "OPHCT/OPVCT").
    fn read_counter(&mut self, horizontal: bool) -> u8 {
        let (value, flag) = if horizontal {
            (self.latch_h, HV_H)
        } else {
            (self.latch_v, HV_V)
        };
        let second = self.hv_read & flag != 0;
        self.hv_read ^= flag;
        if second {
            (value >> 8) as u8 & 0x01
        } else {
            value as u8
        }
    }

    /// `$2132`: each write applies only its masked channels to the 15-bit fixed colour, so the
    /// accumulator cannot be rebuilt from the last byte (fullsnes "2132h - COLDATA").
    fn apply_coldata(&mut self, value: u8) {
        let intensity = u16::from(value & 0x1F);
        let mut fixed = self.fixed_colour;
        if value & 0x20 != 0 {
            fixed = (fixed & !0x001F) | intensity;
        }
        if value & 0x40 != 0 {
            fixed = (fixed & !0x03E0) | (intensity << 5);
        }
        if value & 0x80 != 0 {
            fixed = (fixed & !0x7C00) | (intensity << 10);
        }
        self.fixed_colour = fixed & 0x7FFF;
    }

    /// §9.2's rows: VRAM, OAM, CGRAM, both register files, the PPU's own latches and the counters.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.vram[..]);
        w.bytes(&self.oam[..]);
        for word in self.cgram.iter() {
            w.u16(*word);
        }
        write_regs(w, &self.regs);
        write_regs(w, &self.latched);
        w.i32(self.m7_product);
        w.u16(self.fixed_colour);
        w.u16(self.vram_prefetch);
        w.u16(self.dot);
        w.u16(self.line);
        w.u16(self.latch_h);
        w.u16(self.latch_v);
        w.u8(self.hv_read);
        w.bool(self.field);
        w.bool(self.latch_flag);
        w.bool(self.nmi);
        w.bool(self.irq);
        w.bool(self.frame_ready);
        w.bool(self.obj_time_overflow);
        w.bool(self.obj_range_overflow);
        w.u64(self.frames);
    }

    /// The mirror of [`Ppu::save_state`], in the same order.
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        r.bytes_into(&mut self.vram[..])?;
        r.bytes_into(&mut self.oam[..])?;
        for word in self.cgram.iter_mut() {
            *word = r.u16()?;
        }
        self.regs = read_regs(r)?;
        self.latched = read_regs(r)?;
        self.m7_product = r.i32()?;
        self.fixed_colour = r.u16()?;
        self.vram_prefetch = r.u16()?;
        self.dot = r.u16()?;
        self.line = r.u16()?;
        self.latch_h = r.u16()?;
        self.latch_v = r.u16()?;
        self.hv_read = r.u8()?;
        self.field = r.bool()?;
        self.latch_flag = r.bool()?;
        self.nmi = r.bool()?;
        self.irq = r.bool()?;
        self.frame_ready = r.bool()?;
        self.obj_time_overflow = r.bool()?;
        self.obj_range_overflow = r.bool()?;
        self.frames = r.u64()?;
        self.main = [Pixel::default(); WIDTH];
        self.sub = [Pixel::default(); WIDTH];
        Ok(())
    }
}

/// The OAM storage address for a live byte address: `$220`-`$3FF` mirror `$200`-`$21F`
/// (fullsnes "OAM Size is 220h bytes (addresses 220h..3FFh are mirrors of 200h..21Fh)").
fn oam_physical(live: u16) -> usize {
    if live < 0x200 {
        usize::from(live)
    } else {
        usize::from(0x200 | (live & 0x1F))
    }
}

/// §2.5's field order, which is also the wrap order of §9.2's `PpuRegs` row.
fn write_regs(w: &mut StateWriter, regs: &PpuRegs) {
    w.u8(regs.inidisp);
    w.u8(regs.obsel);
    w.u16(regs.oamadd);
    w.u8(regs.bgmode);
    w.u8(regs.mosaic);
    w.bytes(&regs.bgsc);
    w.bytes(&regs.nba);
    for value in regs.bghofs {
        w.u16(value);
    }
    for value in regs.bgvofs {
        w.u16(value);
    }
    w.u8(regs.vmain);
    w.u16(regs.vmadd);
    w.u8(regs.m7sel);
    for value in regs.m7 {
        w.i16(value);
    }
    w.i16(regs.m7x);
    w.i16(regs.m7y);
    w.i16(regs.m7hofs);
    w.i16(regs.m7vofs);
    w.u8(regs.cgadd);
    w.u8(regs.w12sel);
    w.u8(regs.w34sel);
    w.u8(regs.wobjsel);
    w.bytes(&regs.wh);
    w.u8(regs.wbglog);
    w.u8(regs.wobjlog);
    w.u8(regs.tm);
    w.u8(regs.ts);
    w.u8(regs.tmw);
    w.u8(regs.tsw);
    w.u8(regs.cgwsel);
    w.u8(regs.cgadsub);
    w.u8(regs.coldata);
    w.u8(regs.setini);
    w.u8(regs.bg_old);
    w.u8(regs.m7_old);
    w.u8(regs.vram_lsb);
    w.u8(regs.oam_lsb);
    w.u8(regs.cgram_lsb);
    w.u8(regs.m7b_byte);
}

/// The mirror of [`write_regs`].
#[allow(clippy::field_reassign_with_default)]
fn read_regs(r: &mut StateReader<'_>) -> Result<PpuRegs, StateError> {
    let mut regs = PpuRegs::default();
    regs.inidisp = r.u8()?;
    regs.obsel = r.u8()?;
    regs.oamadd = r.u16()?;
    regs.bgmode = r.u8()?;
    regs.mosaic = r.u8()?;
    r.bytes_into(&mut regs.bgsc)?;
    r.bytes_into(&mut regs.nba)?;
    for value in regs.bghofs.iter_mut() {
        *value = r.u16()?;
    }
    for value in regs.bgvofs.iter_mut() {
        *value = r.u16()?;
    }
    regs.vmain = r.u8()?;
    regs.vmadd = r.u16()?;
    regs.m7sel = r.u8()?;
    for value in regs.m7.iter_mut() {
        *value = r.i16()?;
    }
    regs.m7x = r.i16()?;
    regs.m7y = r.i16()?;
    regs.m7hofs = r.i16()?;
    regs.m7vofs = r.i16()?;
    regs.cgadd = r.u8()?;
    regs.w12sel = r.u8()?;
    regs.w34sel = r.u8()?;
    regs.wobjsel = r.u8()?;
    r.bytes_into(&mut regs.wh)?;
    regs.wbglog = r.u8()?;
    regs.wobjlog = r.u8()?;
    regs.tm = r.u8()?;
    regs.ts = r.u8()?;
    regs.tmw = r.u8()?;
    regs.tsw = r.u8()?;
    regs.cgwsel = r.u8()?;
    regs.cgadsub = r.u8()?;
    regs.coldata = r.u8()?;
    regs.setini = r.u8()?;
    regs.bg_old = r.u8()?;
    regs.m7_old = r.u8()?;
    regs.vram_lsb = r.u8()?;
    regs.oam_lsb = r.u8()?;
    regs.cgram_lsb = r.u8()?;
    regs.m7b_byte = r.u8()?;
    Ok(regs)
}
