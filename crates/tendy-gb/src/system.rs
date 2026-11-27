//! The one interface the front end drives.
//!
//! Everything above the machine — the window loop, the headless `--frames` run, the rewind ring,
//! the `F3` panel, the battery autosave — asks its questions through [`System`] instead of reaching
//! into `Machine` and its fields. The leg exists because a Game Boy Color changes what the machine
//! holds (a colour PPU, VRAM banks, double speed) and a Game Boy Advance replaces it outright
//! (another CPU, PPU, APU, BIOS): without one interface, each of those multiplies the front-end
//! work by the number of places that know the internals.
//!
//! The surface is what the front end actually asks for today and nothing more: the frame loop, the
//! save-state pair the `F5`/`F7` slots and the rewind ring share, the counters the bar prints, the
//! audio and input hooks, the cartridge identity that keys a save file and a profile, the battery
//! body the autosave writes, and the accessors the debugger panel reads. The list started shorter;
//! the debugger panel and the link cable landed later, and
//! their accessors are here rather than left as holes a front end would have to fill by downcasting.
//!
//! This leg is deliberately boring: [`System for Machine`] is delegation, one line per method, to
//! the method or field that already answered that question. Nothing here computes anything.

use crate::cheats::{CheatBook, CheatEntry};
use crate::debugger::ViewRegs;
use crate::joypad::Buttons;
use crate::machine::Machine;
use crate::sgb::{Border, MaskState, SgbFrame};
use crate::state::StateError;
use crate::{HEIGHT, OAM_SIZE, VRAM_SIZE, WIDTH};

/// One frame as the renderer consumes it: the sizes and the pixels, named once instead of a tuple
/// rebuilt at each call site.
///
/// **Two pixel formats, refused as one problem (C2).** A DMG frame is 23 040 shade indices and the
/// front end expands them through one of `palette.rs`'s four-shade tables. A CGB frame has no such
/// table — its colours are the cartridge's — so it also carries `colours`, one RGB555 word per
/// pixel, and the front end expands those instead. `colours` empty is the DMG case, and a caller
/// that only wants "which of four levels" keeps reading `pixels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Visible width in pixels.
    pub width: usize,
    /// Visible height in pixels.
    pub height: usize,
    /// `width * height` shade indices, row-major, top-left first. 0 = lightest, 3 = darkest. On a
    /// CGB these are the pre-palette colour indices of the picture.
    pub pixels: &'a [u8],
    /// `width * height` RGB555 colour words when the console draws in colour, empty otherwise.
    pub colours: &'a [u16],
}

impl<'a> Frame<'a> {
    /// A frame of `width` x `height` shade indices.
    pub fn new(width: usize, height: usize, pixels: &'a [u8]) -> Frame<'a> {
        Frame {
            width,
            height,
            pixels,
            colours: &[],
        }
    }

    /// The same frame with its colour words, for a console that draws in colour.
    pub fn with_colours(mut self, colours: &'a [u16]) -> Frame<'a> {
        self.colours = colours;
        self
    }

    /// True when `colours` is what the picture is drawn from.
    pub fn in_colour(&self) -> bool {
        !self.colours.is_empty()
    }

    /// Pixels in the picture.
    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }
}

impl std::ops::Deref for Frame<'_> {
    type Target = [u8];

    /// The shade indices, so a caller that only wants the picture reads it like the slice it was
    /// before this trait existed.
    fn deref(&self) -> &[u8] {
        self.pixels
    }
}

/// The Super Game Boy's picture: the machine's 256×224 border (ours until the cartridge sends its
/// own with `CHR_TRN`+`PCT_TRN`) with the Game Boy's own 160×144 frame composited into its
/// window.
///
/// The state the picture depends on — the palettes, the attribute map, `MASK_EN` and the border — lives
/// in the machine's `Sgb`, and this value is the front end's render cache: the composed picture,
/// rebuilt once per displayed frame, so the window, `F6` and `--screenshot` show the whole frame
/// the console would have put on the Super NES. It is not machine state and never enters a save
/// state; a loaded state recomposes from the border and the restored Game Boy frame.
#[derive(Clone, PartialEq, Eq)]
pub struct SgbScreen {
    frame: SgbFrame,
}

impl SgbScreen {
    /// A black picture to compose into.
    pub fn new() -> SgbScreen {
        SgbScreen {
            frame: SgbFrame::new(),
        }
    }

    /// Compose `machine`'s current frame into the bordered picture and return its 256×224 RGB555
    /// words, or `None` when the machine is not SGB — nothing has been composed then, and the
    /// front end keeps its 160×144 picture.
    ///
    /// The window's pixels are the machine's own `colours`: an SGB machine resolves every pixel
    /// through `Sgb::colour`, so they are already the console's RGB555 words and the
    /// border is composed around them, not recoloured afterwards.
    pub fn compose(&mut self, machine: &dyn System) -> Option<&[u16]> {
        let mask = machine.sgb_mask()?;
        let border = machine.sgb_border()?;
        let frame = machine.frame();
        self.frame.compose(border, frame.colours, mask);
        Some(self.frame.pixels())
    }
}

impl Default for SgbScreen {
    fn default() -> Self {
        SgbScreen::new()
    }
}

/// What a cartridge says about itself: the two facts that key a `.sav` file, a `[games."..."]`
/// profile and a `[cheats."..."]` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CartIdentity {
    /// The title in the header (`$0134-$0143`, up to the first NUL, printable ASCII only) - the
    /// name `save::save_path` builds the `.sav` file from.
    pub title: String,
    /// CRC-32 of the whole ROM, the disambiguator when two cartridges share a title.
    pub crc32: u32,
}

/// The CPU at one instant: what `--trace-pc` prints and what the debugger panel's header shows.
///
/// One type for both consoles, because the two machines disagree about every field's width and
/// about two of them entirely: a Game Boy Advance's program counter, stack pointer and link register
/// are 32 bits wide and its CPU has seven modes, and a Game Boy's counter is genuinely 16 bits, its
/// CPU has no modes and it has no link register at all. The Game Boy's values are widened (never
/// truncated, never sign-extended), so a Game Boy line prints the same digits it always would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuState {
    /// The program counter.
    pub pc: u32,
    /// The stack pointer.
    pub sp: u32,
    /// The link register; `None` on a CPU with no such register.
    pub lr: Option<u32>,
    /// The processor mode as it is spelled in a trace line: `usr`, `irq`, `svc`… on an Advance,
    /// and `none` on a Game Boy, whose CPU has no modes.
    pub mode: &'static str,
    /// True in Thumb state. Always false on a Game Boy.
    pub thumb: bool,
}

/// The eight 8-bit CPU registers the debugger panel's header prints. `PC` and `SP` have their own
/// accessors ([`System::pc`], [`System::sp`]); carrying them here as well would give one fact two
/// homes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuRegs {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
}

/// The Advance's register file as the panel's header prints it (H3).
///
/// Sixteen 32-bit registers, the CPSR and one saved status register per bank: the panel shows
/// `R0-R15`, the current mode and instruction set, and the five `SPSR`s a mode change left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvanceRegs {
    /// `R0-R15` as the running mode sees them; `R15` is the address of the next instruction.
    pub r: [u32; 16],
    /// The current program status register: flags, interrupt masks, Thumb bit and mode.
    pub cpsr: u32,
    /// One `SPSR` per bank, in `cpu::BANK_*` order (FIQ, IRQ, SVC, ABT, UND, user). The user bank
    /// has no saved status register and its slot is zero.
    pub spsr: [u32; 6],
    /// The mode's name: `usr`, `fiq`, `irq`, `svc`, `abt`, `und`, `sys`.
    pub mode: &'static str,
    /// True in Thumb state.
    pub thumb: bool,
}

/// What the front end is allowed to ask the machine.
///
/// The methods are grouped by who calls them: the frame loop, save states and rewind, the host
/// wiring (audio rate, clock, cartridge, battery), and the debugger panel.
pub trait System {
    // ---------------------------------------------------------------- the frame loop

    /// Advance exactly one frame's worth of emulation. Returns machine cycles consumed.
    fn run_frame(&mut self) -> u32;

    /// What the display shows right now.
    fn frame(&self) -> Frame<'_>;

    /// `MASK_EN`'s state when this machine is a Super Game Boy, `None` when it is not.
    ///
    /// This is the one fact [`SgbScreen::compose`] needs beyond the picture itself: whether the
    /// window is drawn, the last picture is kept, or it is blanked. A machine that is not SGB
    /// answers `None` and the front end never composes a bordered frame for it.
    fn sgb_mask(&self) -> Option<MaskState> {
        None
    }

    /// The SGB border the window is composed into — `None` when the machine is not SGB.
    fn sgb_border(&self) -> Option<&Border> {
        None
    }

    /// Append this frame's audio to `out`, in the interface's own sample format and rate, and
    /// return how many stereo frames were written.
    ///
    /// `&mut Vec<i16>` would need every sample converted; the machine produces interleaved stereo
    /// `f32` at its own rate, and the front end's ring, volume and rate converter all speak that
    /// format.
    /// Converting to `i16` in the adapter would move every sample the speakers hear, in the one leg
    /// that must move nothing.
    fn drain_audio(&mut self, out: &mut [f32]) -> usize;

    /// Buttons pressed this frame.
    fn set_buttons(&mut self, buttons: Buttons);

    /// The two shoulder buttons of a Game Boy Advance (G7). A Game Boy has none, so the default
    /// ignores them and no Game Boy reader has to know the pair exists.
    fn set_shoulders(&mut self, _shoulders: crate::joypad::Shoulders) {}

    /// MBC7's accelerometer, `-1..=1` per axis; a console without one ignores it.
    fn set_tilt(&mut self, x: i8, y: i8);

    /// Frames the machine has finished, boot-ROM frames excluded — the bar's `frame n` and the
    /// headless loop's stop condition.
    fn frames(&self) -> u64;

    /// True while a boot ROM still owns the machine and the cartridge has not started.
    fn booting(&self) -> bool;

    /// The running digest of every finished frame, the same number the headless loop prints.
    fn frame_checksum(&self) -> u64;

    /// What a person would call this console, for the bar and the drawer.
    fn console_name(&self) -> &'static str;

    /// True when the machine is a Game Boy Advance (G7). The bar names it, and the rows and keys
    /// that are Game Boy features say so instead of doing nothing.
    fn is_advance(&self) -> bool {
        false
    }

    /// The cartridge's storage, as one short string: the header's cartridge-type byte on a Game Boy
    /// (`MBC5+RAM+BATTERY`), and the save chip detected from the ROM's ID strings on an Advance
    /// (`SRAM`, `Flash 512 Kbit`), which has no such header field (G7).
    fn cartridge_storage(&self) -> &'static str;

    /// True when the machine is running a CGB cartridge in CGB mode. The bar shows
    /// `CGB` only when this is true, so a DMG cartridge's bar is unchanged.
    fn is_cgb(&self) -> bool;

    /// True when the machine *is* a Game Boy Color console, whatever it runs: a CGB
    /// cartridge, or a monochrome one forced onto the colour console. The status bar's `CGB` marker
    /// reads this, so a monochrome cartridge in compatibility mode still says `CGB`.
    fn is_cgb_console(&self) -> bool;

    /// The CPU's current speed, 1 or 2 (`KEY1` bit 7): the bar's `2x` marker. Always 1 on a DMG.
    fn cgb_speed(&self) -> u8;

    /// The compatibility palette ID and the reason it was chosen, when this machine is a CGB
    /// console holding a monochrome cartridge; `None` otherwise (a CGB cartridge, a DMG console,
    /// or the GBA). The debug panel shows the ID so a wrong palette can be diagnosed.
    fn compat_palette(&self) -> Option<(u8, crate::cgb::CompatReason)>;

    /// Cartridge title and checksum.
    fn cart_identity(&self) -> CartIdentity;

    // ---------------------------------------------------------------- states and rewind

    /// A complete, self-contained snapshot for save states and rewind.
    fn save_state(&mut self) -> Vec<u8>;

    /// Restore a snapshot. `Err` leaves the running machine untouched.
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError>;

    /// The sentence the bar shows when [`load_state`](System::load_state) refuses a state (G8).
    ///
    /// The default is the error's own name, which is all a Game Boy state refusal has ever said,
    /// and it stays that way. A console overrides this when the name alone would not tell the user
    /// *why* the state cannot be resumed — the Advance's container moved for a reason that is in
    /// the payload's absence, not in the version number.
    fn state_refusal(&self, why: StateError) -> String {
        format!("{why:?}")
    }

    /// Game > Reset: back to power-on with the timeline's memory intact.
    fn reset(&mut self);

    // ---------------------------------------------------------------- host wiring

    /// The rate the interface's `drain_audio` produces at (§12 #12).
    fn set_sample_rate(&mut self, hz: u32);

    /// The four CHANNELS rows, as one mask.
    fn set_channel_mask(&mut self, mask: u8);

    /// Take the cartridge's clock forward to a wall clock in milliseconds since the epoch.
    fn tick_rtc(&mut self, now_ms: u64);

    /// Text a ROM has printed over the serial port since the last call.
    fn take_serial(&mut self) -> String;

    /// True once per link-cable event worth a notice, then clears.
    fn take_link_notice(&mut self) -> bool;

    /// Plug the cable in or pull it out: `Link > Role` moving, and the headless `--link` pairing
    /// The cable is session state and never enters a save state.
    fn set_link(&mut self, link: Option<crate::link::Link>);

    /// The same plug, with the end of the cable named: `slot` 0 is the parent — the host of a
    /// socket — and 1-3 are children (G9b).
    ///
    /// The Game Boy's cable has no ends its machine can tell apart: two Game Boys shift into each
    /// other and neither is the parent, so the default here drops the slot and is
    /// [`System::set_link`], which is what a Game Boy has always run. A Game Boy Advance's
    /// multiplayer mode *does* care — the parent's slot is `SIOMULTI0` and a child's `SIOMULTI1` —
    /// so `Gba` overrides this one and its `set_link` keeps working for a caller that has no slot
    /// to give (the host end).
    fn attach_link(&mut self, link: Option<crate::link::Link>, slot: usize) {
        let _ = slot;
        self.set_link(link);
    }

    /// Where the cable stands, for the `Link > Role` row and the pairing line.
    fn link_status(&self) -> crate::link::LinkStatus;

    /// What the cable last had to say - the peer disconnected, or stopped answering - once.
    fn take_link_message(&mut self) -> Option<crate::serial::LinkMessage>;

    /// The `.sav` body, `None` when the cartridge has no RAM, clock or board bytes.
    fn battery_body(&self) -> Option<Vec<u8>>;

    /// Read a `.sav` body back, then take the clock forward over the time the program was closed.
    /// Returns whether anything was loaded; `Err` is the message for the error screen.
    fn load_battery_body(&mut self, bytes: &[u8], now_ms: u64) -> Result<bool, String>;

    /// Build the cheat book from the config file's entries for this cartridge and install it.
    /// Returns the notices for codes that cannot apply here.
    fn apply_cheats(&mut self, entries: &[CheatEntry]) -> Vec<String>;

    // ---------------------------------------------------------------- the debugger panel

    /// One CPU instruction, for the panel's `Space`. Frame accounting is `run_frame`'s.
    fn step_instruction(&mut self) -> u32;

    /// The program counter, as the panel and its breakpoints see it.
    ///
    /// `u32`, not `u16` (H3): an Advance's low 32 MiB of ROM puts its code at `$08000000`, and a
    /// 16-bit trait could only name `$0000` there. A Game Boy's counter is genuinely 16 bits and is
    /// widened into the low half — nothing is truncated and nothing is sign-extended, so the digits
    /// it prints are the digits it always printed.
    fn pc(&self) -> u32;

    /// The stack pointer, for the panel's header. Widened for the same reason as [`System::pc`].
    fn sp(&self) -> u32;

    /// The CPU as it stands, for `--trace-pc` (H3): the counter, the stack and link registers, the
    /// mode's name and the instruction set. Every field is a plain value, so a trace costs no
    /// machine cycle and cannot move the machine.
    fn cpu_state(&self) -> CpuState;

    /// The register file, for the panel's header.
    fn cpu_regs(&self) -> CpuRegs;

    /// The Advance's sixteen 32-bit registers and its status registers, for the panel's header
    /// (H3). `None` on a console whose CPU has the eight 8-bit ones [`System::cpu_regs`] returns —
    /// one accessor per shape, so neither console answers with zeroes for a register it lacks.
    fn advance_regs(&self) -> Option<AdvanceRegs> {
        None
    }

    /// The byte at `addr` as the machine holds it: no cycle, no ownership mask.
    ///
    /// 32 bits wide (H3): the Memory view walks an Advance's `$03000000` as readily as a Game Boy's
    /// `$C000`. A Game Boy's bus is 16 bits and takes the low half, which is every address it has.
    fn peek(&mut self, addr: u32) -> u8;

    /// `$8000-$9FFF`, as stored.
    fn vram(&self) -> &[u8; VRAM_SIZE];

    /// The 160 OAM bytes, as stored.
    fn oam(&self) -> &[u8; OAM_SIZE];

    /// The PPU registers the views read, copied in one go.
    fn view_regs(&self) -> ViewRegs;

    /// Machine cycles since reset: the panel's cache key, so one upload per step and none on an
    /// idle frame.
    fn cycles(&self) -> u64;

    /// Blocks the CGB's VRAM DMA has moved since power-on: the one number that says
    /// whether a cartridge uses `GDMA`/`HDMA` at all.
    fn vram_dma_blocks(&self) -> u64;

    /// `BLDCNT` and `BLDY` and the colour effect they select, as the frame this run stopped on left
    /// them: the one-colour verdict needs to know whether the picture was multiplied
    /// out of existence by a fade, so the headless run prints this beside its checksum.
    ///
    /// A Game Boy has no colour effect and answers `None`, which leaves its summary line exactly as
    /// it was.
    fn effect_state(&self) -> Option<crate::gba_types::EffectState> {
        None
    }

    /// CPU instructions retired since power-on, for the headless run's pacing line (G4). A console
    /// that does not count them answers zero, and the line is not printed for it.
    fn instructions(&self) -> u64 {
        0
    }

    /// `--trace-swi N`: record the next `N` software interrupts, with the caller's `R0`-`R3`, its
    /// `LR` and its return address. Only the Advance has `SWI`, so the default records nothing and a
    /// Game Boy run pays nothing for the flag.
    fn trace_swis(&mut self, _limit: usize) {}

    /// The interrupts [`System::trace_swis`] recorded, in order. Empty unless a trace was asked for.
    fn swi_trace(&self) -> Vec<crate::gba_types::SwiCall> {
        Vec::new()
    }
}

impl System for Machine {
    fn run_frame(&mut self) -> u32 {
        Machine::run_frame(self)
    }

    fn frame(&self) -> Frame<'_> {
        let ppu = &self.bus.ppu;
        let frame = Frame::new(WIDTH, HEIGHT, ppu.frame());
        // An SGB machine's colours are the console's: the scanline renderer resolves every pixel
        // through `Sgb::colour`, so `colours` is the 160×144 window a bordered frame is composed
        // around. A plain DMG never fills them, and keeps its shade-index picture.
        if ppu.cgb || ppu.sgb.enabled() {
            frame.with_colours(ppu.colours())
        } else {
            frame
        }
    }

    fn sgb_mask(&self) -> Option<MaskState> {
        let sgb = &self.bus.ppu.sgb;
        sgb.enabled().then(|| sgb.mask())
    }

    fn sgb_border(&self) -> Option<&Border> {
        let sgb = &self.bus.ppu.sgb;
        sgb.enabled().then(|| sgb.border())
    }

    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        Machine::drain_audio(self, out)
    }

    fn set_buttons(&mut self, buttons: Buttons) {
        Machine::set_buttons(self, buttons);
    }

    fn set_tilt(&mut self, x: i8, y: i8) {
        Machine::set_tilt(self, x, y);
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn booting(&self) -> bool {
        Machine::booting(self)
    }

    fn frame_checksum(&self) -> u64 {
        Machine::frame_checksum(self)
    }

    fn console_name(&self) -> &'static str {
        if self.is_cgb() {
            "Game Boy Color (CGB)"
        } else {
            "Game Boy (DMG)"
        }
    }

    fn cartridge_storage(&self) -> &'static str {
        crate::cart::header::cart_type_name(self.bus.cart.header.cart_type)
    }

    fn is_cgb(&self) -> bool {
        Machine::is_cgb(self)
    }

    fn is_cgb_console(&self) -> bool {
        Machine::is_cgb_console(self)
    }

    fn cgb_speed(&self) -> u8 {
        Machine::cgb_speed(self)
    }

    fn compat_palette(&self) -> Option<(u8, crate::cgb::CompatReason)> {
        Machine::compat_palette(self)
    }

    fn cart_identity(&self) -> CartIdentity {
        CartIdentity {
            title: self.bus.cart.header.title.clone(),
            crc32: self.bus.cart.rom_crc32,
        }
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.encode_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.decode_state(bytes)
    }

    fn reset(&mut self) {
        Machine::reset(self);
    }

    fn set_sample_rate(&mut self, hz: u32) {
        Machine::set_sample_rate(self, hz);
    }

    fn set_channel_mask(&mut self, mask: u8) {
        Machine::set_channel_mask(self, mask);
    }

    fn tick_rtc(&mut self, now_ms: u64) {
        Machine::tick_rtc(self, now_ms);
    }

    fn take_serial(&mut self) -> String {
        Machine::take_serial(self)
    }

    fn take_link_notice(&mut self) -> bool {
        Machine::take_link_notice(self)
    }

    fn set_link(&mut self, link: Option<crate::link::Link>) {
        Machine::set_link(self, link)
    }

    fn link_status(&self) -> crate::link::LinkStatus {
        Machine::link_status(self)
    }

    fn take_link_message(&mut self) -> Option<crate::serial::LinkMessage> {
        Machine::take_link_message(self)
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        crate::save::encode_save(self)
    }

    fn load_battery_body(&mut self, bytes: &[u8], now_ms: u64) -> Result<bool, String> {
        crate::save::load_save(self, bytes, now_ms)
    }

    fn apply_cheats(&mut self, entries: &[CheatEntry]) -> Vec<String> {
        let (book, notices) = CheatBook::from_entries(entries, &self.bus.cart);
        self.bus.cheats = book;
        notices
    }

    fn step_instruction(&mut self) -> u32 {
        Machine::step_instruction(self)
    }

    fn pc(&self) -> u32 {
        u32::from(Machine::pc(self))
    }

    fn sp(&self) -> u32 {
        u32::from(Machine::sp(self))
    }

    /// The Game Boy's CPU has no modes and no link register, so the trace line says `none` and
    /// prints dashes where an Advance prints `lr` (H3). The counter and the stack pointer keep the
    /// values the 16-bit accessors above them return.
    fn cpu_state(&self) -> CpuState {
        CpuState {
            pc: u32::from(Machine::pc(self)),
            sp: u32::from(Machine::sp(self)),
            lr: None,
            mode: "none",
            thumb: false,
        }
    }

    fn cpu_regs(&self) -> CpuRegs {
        let cpu = &self.cpu;
        CpuRegs {
            a: cpu.a,
            f: cpu.f.0,
            b: cpu.b,
            c: cpu.c,
            d: cpu.d,
            e: cpu.e,
            h: cpu.h,
            l: cpu.l,
        }
    }

    fn peek(&mut self, addr: u32) -> u8 {
        Machine::peek(self, addr as u16)
    }

    fn vram(&self) -> &[u8; VRAM_SIZE] {
        Machine::vram(self)
    }

    fn oam(&self) -> &[u8; OAM_SIZE] {
        Machine::oam(self)
    }

    fn view_regs(&self) -> ViewRegs {
        Machine::view_regs(self)
    }

    fn cycles(&self) -> u64 {
        self.bus.cycles
    }

    fn vram_dma_blocks(&self) -> u64 {
        Machine::vram_dma_blocks(self)
    }
}
