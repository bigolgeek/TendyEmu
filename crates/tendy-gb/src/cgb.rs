//! The Game Boy Color's own hardware: which mode the machine runs in, the four registers that say
//! so (`KEY0`, `KEY1`, `VBK`, `SVBK`), the `STOP` double-speed switch (Pan Docs, *CGB Registers*;
//! `docs/PLAN-gbc-gba.md` slice C1), and — from C2 — the two banks of colour palette RAM behind
//! `BCPS`/`BCPD` and `OCPS`/`OCPD` (Pan Docs, *Palettes*) and the `OPRI` object-priority flag.
//!
//! This is the mode and its registers, not the renderer: the attributes those palettes are indexed
//! by, the VRAM bank they draw from and the pixels they produce are the PPU's (`src/ppu/render.rs`),
//! which reads [`Palettes`] the way it reads `BGP`. What lands here is everything a cartridge can
//! ask about the console before it draws anything — whether it is on a CGB at all, which speed the
//! CPU is at — and the colour memory the PPU samples.
//!
//! **One clock, two speeds.** Pan Docs (*FF4D — KEY1/SPD*, "In Double Speed Mode the following will
//! operate twice as fast as normal") splits the machine in two: the CPU, the timer and divider, the
//! serial port and OAM DMA double; the LCD, `HDMA` and every sound timing do not. The CPU is the
//! thing that ticks, so the split is one number in [`crate::bus::Bus::tick`]: a machine cycle in
//! double speed advances the LCD and the APU by **2 dots** instead of 4, while the timer's counter
//! still takes its 4 T-cycles per machine cycle (it is clocked by the CPU, so it runs twice as fast
//! in wall time, which is what Pan Docs says) and the serial bit clock and OAM DMA stay at one
//! machine cycle per byte and per bit — both also twice as fast in wall time. The APU's own
//! *frame sequencer* is 512 Hz of wall time and is fed by `DIV`'s bit-4 falling edge, so it takes
//! every second edge while the CPU is fast.
//!
//! Sources: Pan Docs' *CGB Registers* (`KEY0`, `KEY1`, `VBK`, `SVBK`, "Transfer Timings") and
//! *Power-Up Sequence* (A = `$11` after a CGB boot). Nothing here is taken from another emulator.

use crate::state::{StateError, StateReader, StateWriter};

/// `KEY0` bit 2: DMG compatibility mode, 0 = full CGB mode (Pan Docs, "FF4C — KEY0/SYS").
pub const KEY0_DMG_COMPAT: u8 = 0x04;
/// `KEY1` bit 7: the current speed, 0 = normal, 1 = double. Read-only.
pub const KEY1_DOUBLE: u8 = 0x80;
/// `KEY1` bit 0: the switch is armed. Read/write.
pub const KEY1_ARMED: u8 = 0x01;
/// `KEY1`'s unused bits read 1, like every other `$FFxx` register's.
const KEY1_UNUSED: u8 = 0x7E;
/// `KEY0` reads `$C0` in full CGB mode; bits 3-2 are the documented mode bits.
const KEY0_READ: u8 = 0xC0;
/// `KEY0`'s writable bits: 3 (PGB, "research needed") and 2 (DMG compatibility).
const KEY0_MASK: u8 = 0x0C;
/// `VBK` keeps bit 0; every other bit reads 1 (Pan Docs: "all other bits will be set to 1").
const VBK_UNUSED: u8 = 0xFE;
/// `SVBK` keeps bits 2-0; the rest read 1.
const SVBK_UNUSED: u8 = 0xF8;
/// The `STOP` pause Pan Docs gives the speed switch: 2050 machine cycles = 8200 T-cycles.
pub const SWITCH_PAUSE_DOTS: u16 = 8_200;
/// `BGPI`/`OBPI` bit 7: the address field steps after every data write (Pan Docs, *FF68*).
pub const PAL_INDEX_INCREMENT: u8 = 0x80;
/// `BGPI`/`OBPI` bits 5-0: which of the palette bank's 64 bytes the data register reaches.
pub const PAL_INDEX_MASK: u8 = 0x3F;
/// `BGPI`/`OBPI` bit 6 has no documented meaning and, like every other unused `$FFxx` bit in this
/// machine, reads 1. Pan Docs' table leaves it out entirely; the choice is stated in the report.
const PAL_INDEX_UNUSED: u8 = 0x40;
/// 64 bytes a bank: 8 palettes x 4 colours x 2 bytes, the colour little-endian
/// (Pan Docs, *LCD Color Palettes*).
pub const PALETTE_RAM_SIZE: usize = 64;
/// A palette is four colours.
pub const COLOURS_PER_PALETTE: usize = 4;
/// RGB555's three channels are five bits each, red in bits 0-4 (Pan Docs, *LCD Color Palettes*).
pub const RGB555_WHITE: u16 = 0x7FFF;
/// `OPRI` bit 0: 1 = the DMG's object-priority rule (lowest X first), 0 = the CGB's (OAM order).
pub const OPRI_DMG: u8 = 0x01;
/// `OPRI`'s other seven bits are unused and read 1.
const OPRI_UNUSED: u8 = 0xFE;

/// The CGB's two banks of colour palette RAM: 8 background and 8 object palettes of four RGB555
/// colours, reached only through the index/data register pairs `BCPS`/`BCPD` (`$FF68`/`$FF69`) and
/// `OCPS`/`OCPD` (`$FF6A`/`$FF6B`) (Pan Docs, *Palettes*, *FF68-FF69*).
///
/// The bytes are kept as the cartridge wrote them, little-endian per colour, because the index
/// register addresses *bytes*: a cartridge writing one byte of a colour and reading the other back
/// must see what it wrote. [`Palettes::bg_colour`] is the one place the two bytes become a word.
///
/// **Post-boot state.** Pan Docs says the boot ROM initialises every *background* colour to white
/// and leaves the object colours uninitialised; this machine starts a CGB cartridge from the
/// post-boot register file (`A = $11`, C1) with the same palette memory, so the background bank is
/// `$7FFF` throughout and the object bank is zero. C4's CGB boot ROM will write both itself.
#[derive(Clone, Debug)]
pub struct Palettes {
    /// `BCPS`/`BGPI` (`$FF68`): bit 7 auto-increment, bits 5-0 the byte address.
    pub bg_index: u8,
    /// `OCPS`/`OBPI` (`$FF6A`), the same shape.
    pub obj_index: u8,
    /// The background/window bank, 64 bytes.
    pub bg: [u8; PALETTE_RAM_SIZE],
    /// The object bank, 64 bytes. Colour 0 of every object palette is never drawn.
    pub obj: [u8; PALETTE_RAM_SIZE],
}

impl Palettes {
    /// Post-boot: every background colour white, every object colour zero (see the type's note).
    pub fn new() -> Palettes {
        let mut bg = [0u8; PALETTE_RAM_SIZE];
        for colour in bg.as_chunks_mut::<2>().0 {
            *colour = RGB555_WHITE.to_le_bytes();
        }
        Palettes {
            bg_index: 0,
            obj_index: 0,
            bg,
            obj: [0; PALETTE_RAM_SIZE],
        }
    }

    /// One background colour as RGB555, bit 15 clear: Pan Docs' footnote says the rendering process
    /// ignores bit 15, so a bank filled with `$FF` bytes is white rather than something else.
    pub fn bg_colour(&self, palette: u8, index: u8) -> u16 {
        colour_at(&self.bg, palette, index)
    }

    /// One object colour as RGB555, the same rule.
    pub fn obj_colour(&self, palette: u8, index: u8) -> u16 {
        colour_at(&self.obj, palette, index)
    }

    /// A read of `$FF68-$FF6B`. The data registers are `$FF` while the PPU is reading palette RAM;
    /// that mask is the caller's, because the mode is the PPU's (see `Ppu::read_palette`).
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF68 => PAL_INDEX_UNUSED | self.bg_index,
            0xFF69 => self.bg[usize::from(self.bg_index & PAL_INDEX_MASK)],
            0xFF6A => PAL_INDEX_UNUSED | self.obj_index,
            0xFF6B => self.obj[usize::from(self.obj_index & PAL_INDEX_MASK)],
            _ => 0xFF,
        }
    }

    /// A write of `$FF68-$FF6B`. `blocked` is the caller's verdict that the PPU owns palette RAM
    /// this dot: the write is then dropped, but the index still steps — Pan Docs is explicit that
    /// auto-increment happens "even if the write fails due to CRAM being inaccessible", and that
    /// *reads* never step it.
    pub fn write(&mut self, addr: u16, value: u8, blocked: bool) {
        match addr {
            0xFF68 => self.bg_index = value & (PAL_INDEX_INCREMENT | PAL_INDEX_MASK),
            0xFF69 => {
                let index = self.bg_index & PAL_INDEX_MASK;
                if !blocked {
                    self.bg[usize::from(index)] = value;
                }
                self.bg_index = step(self.bg_index);
            }
            0xFF6A => self.obj_index = value & (PAL_INDEX_INCREMENT | PAL_INDEX_MASK),
            0xFF6B => {
                let index = self.obj_index & PAL_INDEX_MASK;
                if !blocked {
                    self.obj[usize::from(index)] = value;
                }
                self.obj_index = step(self.obj_index);
            }
            _ => {}
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.bg_index);
        w.u8(self.obj_index);
        w.bytes(&self.bg);
        w.bytes(&self.obj);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bg_index = r.u8()?;
        self.obj_index = r.u8()?;
        r.bytes_into(&mut self.bg)?;
        r.bytes_into(&mut self.obj)?;
        Ok(())
    }
}

impl Default for Palettes {
    fn default() -> Self {
        Palettes::new()
    }
}

/// Byte `palette`'s colour `index`, little-endian, bit 15 cleared.
fn colour_at(bank: &[u8; PALETTE_RAM_SIZE], palette: u8, index: u8) -> u16 {
    let at = usize::from(palette & 7) * COLOURS_PER_PALETTE * 2 + usize::from(index & 3) * 2;
    u16::from_le_bytes([bank[at], bank[at + 1]]) & RGB555_WHITE
}

/// One auto-increment step of an index register: the six-bit address wraps 63 -> 0 and bit 7 (the
/// auto-increment flag) stays put.
fn step(index: u8) -> u8 {
    let next = (index & PAL_INDEX_MASK).wrapping_add(1) & PAL_INDEX_MASK;
    (index & !PAL_INDEX_MASK) | next
}

/// What a `STOP` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The ordinary low-power stop: no CGB hardware, or `KEY1` was not armed.
    Halt,
    /// The speed switch is running: the CPU waits out the pause in [`Cgb::pause`].
    Switching,
    /// A CGB switch was asked for while the bus was busy. The request is consumed and the CPU
    /// carries on at the speed it already had.
    Refused,
}

/// The CGB state: is this machine a CGB, the two `KEY` registers, the two bank-select registers,
/// and the speed switch in flight.
#[derive(Clone, Debug)]
pub struct Cgb {
    /// True when this machine is a Game Boy Color console. A CGB cartridge sets it from `$0143`
    /// bit 7; [`crate::machine::Machine::new_cgb`] sets it for a monochrome cartridge so the
    /// compatibility palette applies. A DMG console leaves it false: the four registers then read
    /// `$FF`, ignore writes and the CPU stays at normal speed, which is the v2.8.0 machine exactly.
    pub hardware: bool,
    /// `$0143` bit 7: is the cartridge a CGB (enhanced `$80` or CGB-only `$C0`) as opposed to a
    /// monochrome one. On a CGB console a monochrome cartridge runs in DMG compatibility mode
    /// with the compatibility palette; a CGB cartridge runs in full CGB mode.
    pub cart_cgb: bool,
    /// `KEY0` bits 3-2 (`$FF4C`): bit 2 selects DMG compatibility mode within a CGB.
    pub key0: u8,
    /// `KEY1` (`$FF4D`): bit 7 the current speed, bit 0 the arm flag.
    pub key1: u8,
    /// `VBK` (`$FF4F`): bit 0, the VRAM bank. C3 reads it.
    pub vbk: u8,
    /// `SVBK` (`$FF70`): bits 2-0, the WRAM bank mapped at `$D000`.
    pub svbk: u8,
    /// `OPRI` (`$FF6C`): bit 0, which object-priority rule the PPU draws with. 0 = the CGB's (OAM
    /// order, the post-boot value a CGB cartridge gets), 1 = the DMG's (lowest X first).
    pub opri: u8,
    /// T-cycles left in the `STOP` pause; 0 when no switch is in flight.
    pub pause: u16,
    /// Set on every second `DIV`-APU edge while the CPU is fast, so the frame sequencer takes one
    /// edge in two: `DIV` doubles with the CPU, the sound timings do not (Pan Docs).
    pub apu_skip_edge: bool,
}

impl Cgb {
    /// A machine for a console of type `hardware` holding a cartridge of type `cart_cgb`
    /// (`$0143` bit 7). `hardware` is the console: a CGB cartridge's machine is a CGB console, and
    /// [`crate::machine::Machine::new_cgb`] asks for one explicitly so a monochrome cartridge gets
    /// the compatibility palette. A DMG console's machine is the v2.8.0 one, with this struct
    /// inert.
    pub fn new(hardware: bool, cart_cgb: bool) -> Cgb {
        Cgb {
            hardware,
            cart_cgb,
            key0: 0,
            key1: 0,
            vbk: 0,
            svbk: 0,
            opri: 0,
            pause: 0,
            apu_skip_edge: false,
        }
    }

    /// True when the machine runs a monochrome cartridge on a CGB console: the compatibility
    /// palettes apply and the PPU indexes `BGP`/`OBP0`/`OBP1` into them instead of shades of grey.
    pub fn compat_mode(&self) -> bool {
        self.hardware && !self.cart_cgb
    }

    /// True when CGB code is running: a CGB cartridge on CGB hardware, not in DMG compatibility
    /// mode. `KEY0` bit 2 is how a CGB is told to behave like a DMG.
    pub fn is_cgb(&self) -> bool {
        self.hardware && self.key0 & KEY0_DMG_COMPAT == 0
    }

    /// The CPU's current speed: 0 = normal, 1 = double (`KEY1` bit 7).
    pub fn double_speed(&self) -> bool {
        self.key1 & KEY1_DOUBLE != 0
    }

    /// 1 or 2, as the status bar and `System::cgb_speed` report it.
    pub fn speed(&self) -> u8 {
        if self.double_speed() { 2 } else { 1 }
    }

    /// The bank mapped at `$D000-$DFFF`: 1-7, with a written 0 meaning bank 1 (Pan Docs,
    /// "FF70 — SVBK/WBK"). [`crate::bus::Bus`] banks work RAM by it; this leg stores the value.
    pub fn wram_bank(&self) -> u8 {
        match self.svbk & 0x07 {
            0 => 1,
            bank => bank,
        }
    }

    /// The VRAM bank `VBK` selects. C3 does the banking.
    pub fn vram_bank(&self) -> u8 {
        self.vbk & 0x01
    }

    /// True while the CPU is held for the speed switch.
    pub fn switching(&self) -> bool {
        self.pause > 0
    }

    /// A read of `$FF4C`, `$FF4D`, `$FF4F`, `$FF6C` or `$FF70`. On a DMG every one of them reads
    /// `$FF`.
    ///
    /// The gate is [`Cgb::hardware`] and not [`Cgb::is_cgb`]: these are registers of a CGB, and a
    /// CGB in DMG compatibility mode still answers them (the mode changes what the PPU and the
    /// speed switch do, not what the register file holds). A DMG cartridge is the case that reads
    /// `$FF` and ignores writes.
    pub fn read(&self, addr: u16) -> u8 {
        if !self.hardware {
            return 0xFF;
        }
        match addr {
            0xFF4C => KEY0_READ | (self.key0 & KEY0_MASK),
            0xFF4D => KEY1_UNUSED | (self.key1 & (KEY1_DOUBLE | KEY1_ARMED)),
            0xFF4F => VBK_UNUSED | (self.vbk & 0x01),
            0xFF6C => OPRI_UNUSED | (self.opri & OPRI_DMG),
            0xFF70 => SVBK_UNUSED | (self.svbk & 0x07),
            _ => 0xFF,
        }
    }

    /// A write, with each register's mask. `KEY1` bit 7 is read-only: a write can arm the switch
    /// (bit 0 set) or clear the arm (bit 0 clear) and cannot set the speed itself.
    pub fn write(&mut self, addr: u16, value: u8) {
        if !self.hardware {
            return;
        }
        match addr {
            0xFF4C => self.key0 = value & KEY0_MASK,
            0xFF4D => self.key1 = (self.key1 & KEY1_DOUBLE) | (value & KEY1_ARMED),
            0xFF4F => self.vbk = value & 0x01,
            0xFF6C => self.opri = value & OPRI_DMG,
            0xFF70 => self.svbk = value & 0x07,
            _ => {}
        }
    }

    /// `STOP` on a CGB with `KEY1` bit 0 set: the switch is requested. [`Stop::Switching`] means
    /// the CPU must wait out [`SWITCH_PAUSE_DOTS`]; [`Stop::Halt`] is the ordinary low-power stop.
    ///
    /// Pan Docs does not say what a `STOP` does when the request cannot be honoured while a serial
    /// transfer or an HDMA is in flight; this machine consumes the request (the arm bit clears) and
    /// lets the CPU carry on at the speed it already had, so a game that switches during link
    /// activity stalls for neither the pause nor a button press. `blocked` is the caller's verdict
    /// — a serial transfer in flight today, an HDMA in flight from C3.
    pub fn stop(&mut self, blocked: bool) -> Stop {
        if !self.is_cgb() || self.key1 & KEY1_ARMED == 0 {
            return Stop::Halt;
        }
        self.key1 &= !KEY1_ARMED;
        if blocked {
            return Stop::Refused;
        }
        if self.switching() {
            return Stop::Switching;
        }
        self.pause = SWITCH_PAUSE_DOTS;
        Stop::Switching
    }

    /// One machine cycle of the `STOP` pause: the dots still run (the LCD, the APU and the serial
    /// port keep their clocks — Pan Docs lists all three as unaffected), `DIV` does not, and the
    /// speed flips when the pause runs out.
    pub fn pause_tick(&mut self, dots: u16) {
        if self.pause == 0 {
            return;
        }
        self.pause = self.pause.saturating_sub(dots);
        if self.pause == 0 {
            self.key1 ^= KEY1_DOUBLE;
            self.apu_skip_edge = false;
        }
    }

    /// True when this `DIV`-APU edge reaches the APU's frame sequencer. Normal speed: every edge.
    /// Double speed: one edge in two, because `DIV` is ticking twice as fast in wall time and the
    /// sound timings are not (Pan Docs, *FF4D*, "And the following will keep operating as usual").
    pub fn apu_edge(&mut self) -> bool {
        if !self.double_speed() {
            return true;
        }
        self.apu_skip_edge = !self.apu_skip_edge;
        !self.apu_skip_edge
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.hardware);
        w.bool(self.cart_cgb);
        w.u8(self.key0);
        w.u8(self.key1);
        w.u8(self.vbk);
        w.u8(self.svbk);
        w.u8(self.opri);
        w.u16(self.pause);
        w.bool(self.apu_skip_edge);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.hardware = r.bool()?;
        self.cart_cgb = r.bool()?;
        self.key0 = r.u8()?;
        self.key1 = r.u8()?;
        self.vbk = r.u8()?;
        self.svbk = r.u8()?;
        self.opri = r.u8()?;
        self.pause = r.u16()?;
        self.apu_skip_edge = r.bool()?;
        Ok(())
    }
}

impl Default for Cgb {
    fn default() -> Self {
        Cgb::new(false, false)
    }
}

/// Write a cartridge's selected compatibility triplet into palette RAM: the background into BG
/// palette 0, and the objects into OBJ palettes 0 and 1 (Pan Docs, "Compatibility palettes":
/// "the background uses BG palette 0 … and objects use OBJ palette 0 or 1"). The caller sets
/// `KEY0` to DMG compatibility and `OPRI` to DMG priority.
pub fn apply_compatibility_palette(palettes: &mut Palettes, selector: u8) {
    let triplet = crate::palette::compat_triplet(selector);
    write_compat_palette(&mut palettes.bg, 0, triplet.bg);
    write_compat_palette(&mut palettes.obj, 0, triplet.obj0);
    write_compat_palette(&mut palettes.obj, 1, triplet.obj1);
}

/// Four RGB555 colours into palette `index` of a 64-byte bank, little-endian per colour.
fn write_compat_palette(bank: &mut [u8; PALETTE_RAM_SIZE], index: usize, colours: [u16; 4]) {
    for (slot, colour) in colours.iter().enumerate() {
        let at = index * COLOURS_PER_PALETTE * 2 + slot * 2;
        bank[at] = (colour & 0xFF) as u8;
        bank[at + 1] = (colour >> 8) as u8;
    }
}

// ---------------------------------------------------------------------------------------------
// The compatibility palettes: the colour boot ROM's automatic palette choice for a
// monochrome cartridge. Pan Docs, *Power-Up Sequence*, "Compatibility palettes".
//
// The algorithm has four steps. The *index* it produces (Pan Docs' "palettes ID") is the position
// in [`TRIPLET_IDS_AND_FLAGS`]; that byte is a packed selector whose low five bits are the palette
// *entry* and whose high three are the *shuffling flags*, and `crate::palette::compat_triplet`
// resolves it to the three palettes (BG, OBP0, OBP1) the machine writes into palette RAM.
//
// **One documented simplification.** The machine applies this at power-on for any CGB console
// holding a monochrome cartridge (`$0143` bit 7 clear), *including with `Boot ROM` off*. Real
// hardware only colours the cartridge through the boot ROM, so a real CGB with no boot ROM would
// leave it grey; ours colours it regardless so the feature does not depend on which page is
// booted. See the report.

/// The 65 single title checksums (indices 0..=64), then the 14 "ambiguous" ones (indices 65..=78)
/// that are further disambiguated by the title's fourth letter (Pan Docs, "Compatibility
/// palettes", step 3; the values are the boot ROM's published title table).
pub const TITLE_CHECKSUMS: [u8; 79] = [
    0x00, 0x88, 0x16, 0x36, 0xd1, 0xdb, 0xf2, 0x3c, 0x8c, 0x92, 0x3d, 0x5c, 0x58, 0xc9, 0x3e, 0x70,
    0x1d, 0x59, 0x69, 0x19, 0x35, 0xa8, 0x14, 0xaa, 0x75, 0x95, 0x99, 0x34, 0x6f, 0x15, 0xff, 0x97,
    0x4b, 0x90, 0x17, 0x10, 0x39, 0xf7, 0xf6, 0xa2, 0x49, 0x4e, 0x43, 0x68, 0xe0, 0x8b, 0xf0, 0xce,
    0x0c, 0x29, 0xe8, 0xb7, 0x86, 0x9a, 0x52, 0x01, 0x9d, 0x71, 0x9c, 0xbd, 0x5d, 0x6d, 0x67, 0x3f,
    0x6b, // .ambiguous — indices 65..=78, disambiguated by the fourth title letter.
    0xb3, 0x46, 0x28, 0xa5, 0xc6, 0xd3, 0x27, 0x61, 0x18, 0x66, 0x6a, 0xbf, 0x0d, 0xf4,
];

/// The index of the first "ambiguous" checksum: entries at or past it need the fourth-letter
/// tie-breaker (Pan Docs, "Compatibility palettes", step 3/4 boundary).
pub const TITLE_CHECKSUMS_SINGLE: usize = 65;

/// The fourth-letter tie-breaker table, three rows of fourteen columns (the last row holds one
/// letter), read column-first: for the ambiguous checksum at index `65 + column`, the letters in
/// rows 0.. are at offsets `column`, `column + 14`, `column + 28` (Pan Docs, step 4).
pub const TITLE_FOURTH_LETTERS: [u8; 29] = *b"BEFAARBEKEK R-URAR INAILICE R";

/// The packed selector at each palette ID: `entry | flags << 5`, low five bits the palette entry,
/// high three the shuffling flags (Pan Docs, "a fairly complex mechanism"; the boot ROM's table).
pub const TRIPLET_IDS_AND_FLAGS: [u8; 94] = [
    0x7c, 0x08, 0x12, 0xa3, 0xa2, 0x07, 0x87, 0x4b, 0x20, 0x12, 0x65, 0xa8, 0x16, 0xa9, 0x86, 0xb1,
    0x68, 0xa0, 0x87, 0x66, 0x12, 0xa1, 0x30, 0x3c, 0x12, 0x85, 0x12, 0x64, 0x1b, 0x07, 0x06, 0x6f,
    0x6e, 0x6e, 0xae, 0xaf, 0x6f, 0xb2, 0xaf, 0xb2, 0xa8, 0xab, 0x6f, 0xaf, 0x86, 0xae, 0xa2, 0xa2,
    0x12, 0xaf, 0x13, 0x12, 0xa1, 0x6e, 0xaf, 0xaf, 0xad, 0x06, 0x4c, 0x6e, 0xaf, 0xaf, 0x12, 0x7c,
    0xac, 0xa8, 0x6a, 0x6e, 0x13, 0xa0, 0x2d, 0xa8, 0x2b, 0xac, 0x64, 0xac, 0x6d, 0x87, 0xbc, 0x60,
    0xb4, 0x13, 0x72, 0x7c, 0xb5, 0xae, 0xae, 0x7c, 0x7c, 0x65, 0xa2, 0x6c, 0x64, 0x85,
];

/// Why the selector landed where it did, for the debug panel and the tests' quoted reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatReason {
    /// The licensee is not Nintendo (`$014B` != `$33`/`$01`), so the default entry is used.
    NotNintendo,
    /// The title checksum is not in the table, so the default entry is used.
    TitleNotFound,
    /// An ambiguous checksum's fourth letter is not in the tie-breaker table.
    FourthLetterUnknown,
    /// The checksum matched a single entry at this index.
    Single(u8),
    /// The checksum matched an ambiguous entry; the fourth letter picked this index.
    Ambiguous(u8),
}

impl CompatReason {
    /// One line for the debug panel: which check failed, or which title checksum matched.
    pub fn describe(&self) -> String {
        match *self {
            CompatReason::NotNintendo => "not a Nintendo licensee".to_string(),
            CompatReason::TitleNotFound => "title checksum not in the table".to_string(),
            CompatReason::FourthLetterUnknown => "fourth letter not in the table".to_string(),
            CompatReason::Single(sum) => format!("title checksum ${sum:02X} matched"),
            CompatReason::Ambiguous(sum) => {
                format!("title checksum ${sum:02X}, fourth letter matched")
            }
        }
    }
}

/// The licence check: is the cartridge a Nintendo title? Old licensee `$33` means the new licensee
/// at `$0144-$0145` must be the ASCII string `"01"`; otherwise `$014B` must be `$01` (Pan Docs,
/// "Compatibility palettes", step 1).
fn licensee_is_nintendo(header: &crate::cart::Header) -> bool {
    if header.old_licensee == 0x33 {
        header.new_licensee == *b"01"
    } else {
        header.old_licensee == 0x01
    }
}

/// The title checksum: the sum of the 16 title bytes at `$0134-$0143` (Pan Docs, step 2).
fn title_sum(header: &crate::cart::Header) -> u8 {
    header
        .title_bytes
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

/// The compatibility palette ID — the index into [`TRIPLET_IDS_AND_FLAGS`] — the boot ROM's
/// algorithm produces for a cartridge header, and why (Pan Docs, "Compatibility palettes").
pub fn compatibility_choice(header: &crate::cart::Header) -> (u8, CompatReason) {
    if !licensee_is_nintendo(header) {
        return (0, CompatReason::NotNintendo);
    }
    let sum = title_sum(header);
    let mut index = 0usize;
    let mut found = false;
    for (i, &checksum) in TITLE_CHECKSUMS.iter().enumerate() {
        if checksum == sum {
            index = i;
            found = true;
            break;
        }
    }
    if !found {
        return (0, CompatReason::TitleNotFound);
    }
    if index < TITLE_CHECKSUMS_SINGLE {
        return (index as u8, CompatReason::Single(sum));
    }
    // Ambiguous: the fourth letter (`$0137`) picks a row of the tie-breaker table.
    let column = index - TITLE_CHECKSUMS_SINGLE;
    let fourth = header.title_bytes[3];
    for row in 0..3 {
        let offset = column + row * 14;
        if offset < TITLE_FOURTH_LETTERS.len() && TITLE_FOURTH_LETTERS[offset] == fourth {
            return ((index + row * 14) as u8, CompatReason::Ambiguous(sum));
        }
    }
    (0, CompatReason::FourthLetterUnknown)
}

/// The packed selector the choice resolves to (`entry | flags << 5`), for
/// [`crate::palette::compat_triplet`].
pub fn compatibility_selector(header: &crate::cart::Header) -> u8 {
    let (index, _) = compatibility_choice(header);
    TRIPLET_IDS_AND_FLAGS[usize::from(index)]
}
