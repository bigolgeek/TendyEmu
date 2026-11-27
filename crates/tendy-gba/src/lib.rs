//! A Game Boy Advance that loads a cartridge, holds the whole memory map, and produces frames.
//!
//! This is the shelf the other nine GBA slices sit on: the cartridge's header is parsed, every
//! region of GBATEK's *GBA Memory Map* answers, the frame clock counts 228 scanlines of 1232 cycles
//! at 59.7275 Hz with `DISPSTAT`'s VBlank flag where GBATEK puts it, and the machine comes up in the
//! state the boot sequence leaves.
//!
//! **The CPU is the ARM7TDMI** (G1): [`cpu`] holds the register banks, the exceptions and the
//! barrel shifter, [`arm`] and [`thumb`] the two instruction sets. **The picture is [`ppu`]** (G3):
//! modes 0-5, four backgrounds, 128 objects, windows and the colour effects, drawn a scanline at a
//! time off the frame clock. There is still no sound (G5), no timers or DMA or interrupt *sources*
//! (G4) and no BIOS beyond the one system call a failing test ROM needs: `$00000000` reads open bus
//! until G2's high-level implementations arrive, and `SWI` records its comment, services `Div` ($06)
//! and halts on everything else.
//!
//! `frame()` hands out what the cartridge drew: 240x160 RGB555, the same colour type the Game Boy
//! Color's PPU produces. Nothing on the screen is white paper any more.
//!
//! The GBA path shares no bus, no timing helper and no state container with the Game Boy's: the
//! rule here is that a shared helper is how DMG accuracy regresses, so the two
//! machines meet only at the [`System`] trait the front end speaks.

pub mod arm;
pub mod bios;
pub mod bus;
pub mod cart;
pub mod cpu;
pub mod dma;
pub mod irq;
pub mod notes;
pub mod ppu;
pub mod save;
pub mod serial;
pub mod sound;
pub mod sound_driver;
pub mod thumb;
pub mod timer;

mod machine_impl;

use crate::bus::{Bus, CYCLES_PER_FRAME};
use crate::cart::Cartridge;
use crate::cpu::Cpu;
use crate::ppu::Ppu;
use crate::save::{Rtc, Save};
use crate::sound::Sound;
use tendy_gb::cheats::CheatEntry;
use tendy_gb::debugger::ViewRegs;
use tendy_gb::joypad::{Buttons, Shoulders};
use tendy_gb::state::{
    StateError, StateReader, StateWriter, fnv1a64_colours, fnv1a64_colours_continue,
};
use tendy_gb::system::{AdvanceRegs, CartIdentity, CpuRegs, CpuState, Frame, System};

pub use machine_impl::{GbaMachine, native_buttons};

/// Visible picture width in pixels (GBATEK, *LCD Dimensions*: "Visible 240 dots").
pub const WIDTH: usize = 240;
/// Visible picture height in pixels: "Visible (*) 160 lines".
pub const HEIGHT: usize = 160;
/// One frame of shade indices.
pub const PIXELS: usize = WIDTH * HEIGHT;

pub use tendy_gb::gba_types::{Effect, EffectState};

/// The master clock: 16.78 MHz, "16*1024*1024 Hz" (GBATEK, *System Clock*).
pub const CLOCK_HZ: u32 = 16_777_216;

/// Frames per second: the clock divided by a frame's 280 896 cycles, i.e. 59.7275 Hz.
pub fn fps() -> f64 {
    f64::from(CLOCK_HZ) / f64::from(CYCLES_PER_FRAME)
}

/// First four bytes of a Game Boy Advance state. Deliberately not the Game Boy's `GBST`: a state
/// that belongs to the other machine must be refused by its magic, not by a version number that
/// happens to be different today.
const STATE_MAGIC: [u8; 4] = *b"GBAG";
/// Wire version of this payload. Version 2 carried the ARM7TDMI's banked registers, the six saved
/// status registers, the halt flag and the instruction counter — everything G1 added to the
/// machine, so that a state saved in an exception handler comes back in it. Version 3 appended the
/// software-interrupt count, which the public suites' gate reads. Version 4 appended G2's BIOS record
/// — the opcode latch, the per-call counts and the call it refused — and the bus's timing state.
/// Version 5 is where G3's PPU (line cursor, affine reference points, both 240x160 buffers) and G6's
/// save chip (SRAM, flash or EEPROM) and RTC clock landed together. Version 6 appends G4's timing
/// state: the four timers' reload, counter, control and prescaler remainder (their registers are
/// answered by the timer unit rather than the I/O page, so they are no longer inside `io`) and the
/// one-bit halt request a store to `HALTCNT` leaves for the next instruction. Version 7 appends G5's
/// sound state: the sound registers, both FIFOs with their DAC samples and pop accumulators, the two
/// sample-clock periods, and the resampler's position. Version 8 appends G7's two shoulder buttons and
/// the four DMA channels' internal cursors, which a state taken mid-transfer needs: the address
/// registers keep their written values, so without them a resumed transfer would walk from the start.
/// Version 9 appends the BIOS sound driver's work-area pointer: the driver's own state lives in that
/// area (caller memory, already in the payload), so the address is the one field a resumed machine
/// could not recover — and a version-8 state has no copy of it, so such a state is refused rather
/// than resumed with the driver unbound. Version 10 appends the BIOS's parked `IntrWait` address,
/// which a state taken mid-wait needs to know which instruction to resume the caller at — without
/// it a resumed machine would run that caller while the wait is still pending. Version 11 appends the
/// prefetch buffer's depth next to its address, because a state taken with eight entries in the
/// buffer would otherwise resume with an empty one. Version 12 appends the `RegisterRamReset`
/// clears a V-Blank cut short and the address that call is parked at, which a state taken inside
/// one needs to know which regions are still to be cleared and where the handler returns to.
const STATE_VERSION: u16 = 12;

/// One section of a state payload: the label a byte breakdown reports and the bytes it wrote.
///
/// [`Gba::state_sections`] hands these out in wire order; the bytes sum to the payload's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateSection {
    pub name: &'static str,
    pub bytes: usize,
}

/// The whole Game Boy Advance as this slice knows it: a bus, the ARM7TDMI core, and the counters a
/// frame loop and a status bar read.
pub struct Gba {
    pub bus: Bus,
    pub cpu: Cpu,
    /// The picture: modes 0-5, objects, windows and the colour effects, one scanline at a time.
    pub ppu: Ppu,
    /// Frames finished since power-on.
    pub frames: u64,
    /// `fnv1a64` folded over every finished frame, in order — the same running digest the headless
    /// `--frames` run prints for a Game Boy cartridge.
    pub checksum: u64,
    /// The 240x160 picture `System::frame` hands out, as shade indices. A Game Boy Advance draws in
    /// colour and has no four-shade picture at all; the array exists only because the `Frame` type
    /// asks for one, and the front end reads `colours` because it is non-empty.
    pixels: Vec<u8>,
    /// The rate the front end asked for, kept so a later slice has the number it was given.
    pub sample_rate: u32,
    /// The four channel mutes the drawer's rows ask for (G5 gives them something to mute).
    pub channel_mask: u8,
    /// This end of the cable, when one is plugged in: a second handle to the same transport
    /// [`Gba::attach_link`] gave the serial port, so the front end can ask where the cable stands
    /// (G9b). Session state, never saved — a state is a machine, not a pair of windows.
    link: Option<tendy_gb::link::Link>,
}

impl Gba {
    /// Power-on to GBATEK's post-boot state: the cartridge in the slot, the CPU at the cartridge's
    /// entry point in ARM state, `VCOUNT` 0, and the I/O page at its published values.
    pub fn new(cart: Cartridge) -> Gba {
        let entry_point = cart.entry_point();
        let mut gba = Gba {
            bus: Bus::new(cart),
            cpu: Cpu::post_boot(entry_point),
            ppu: Ppu::new(),
            frames: 0,
            checksum: fnv1a64_colours(&[]),
            pixels: vec![0; PIXELS],
            sample_rate: 44_100,
            channel_mask: 0xFF,
            link: None,
        };
        gba.bus.sound.set_sample_rate(gba.sample_rate);
        gba.bus.sound.set_channel_mask(gba.channel_mask);
        gba
    }

    /// Plug a link cable in, or pull it out with `None`, as unit `slot` of it: 0 is the parent
    /// (the host of a socket cable), 1-3 are children.
    ///
    /// This is the engine's seam, not the window's: with no cable the port behaves exactly as a
    /// real Advance's does with nothing plugged in — a normal-mode transfer shifts ones in and
    /// completes on its own clock, and a multiplayer transfer finds no units. The front end's
    /// `Link > Role` row calls this through [`System::attach_link`] (G9b), because a
    /// [`tendy_gb::link::Link`] does not carry which end of the cable it is; the handle is kept here
    /// as well, so [`System::link_status`] can say where the cable stands without asking the
    /// serial engine, which owns the port and not the socket.
    pub fn attach_link(&mut self, link: Option<tendy_gb::link::Link>, slot: usize) {
        match link {
            Some(link) => {
                self.link = Some(link.clone());
                self.bus.serial.attach(
                    serial::Cable::new(serial::SocketPort::new(link, slot)),
                    slot,
                );
            }
            None => {
                self.link = None;
                self.bus.serial.detach();
            }
        }
    }

    /// Advance exactly one frame: run instructions until the frame clock rolls over 228 scanlines,
    /// fold the picture into the running digest, and report the cycles it cost.
    ///
    /// The clock is the frame: the loop stops when the LCD has finished a frame, so the frame
    /// boundary never drifts however long the last instruction was. It costs one instruction's
    /// worth of overshoot per frame — a handful of cycles at most — which is why a test asserts
    /// `bus.cycles` is 60 x 280 896 *within one instruction* rather than exactly equal.
    ///
    /// **A halted CPU does not spin.** While [`Cpu::halted`] and nothing has asked for an interrupt
    /// (`IE & IF` is zero), stepping would execute a one-cycle no-op 280 896 times over. Instead the
    /// clock jumps to the next hardware event, which is the only thing that can wake the CPU; the
    /// frame is still the LCD's 228 lines, so it still costs exactly 280 896 cycles.
    ///
    /// The picture is drawn inside the loop, not after it: [`Ppu::sync`] is handed the clock after
    /// every instruction, so each scanline is drawn as the clock enters it and a mid-frame register
    /// write lands on the line the hardware would have it land on.
    pub fn run_frame(&mut self) -> u32 {
        let started = self.bus.cycles;
        let target = self.bus.lcd.frames + 1;
        while self.bus.lcd.frames < target {
            if self.cpu.halted && !self.bus.irq_requested() {
                // Asleep: nothing to execute, so spend the time the hardware would spend rather
                // than a cycle at a time. `tick` is what raises the request that wakes us.
                self.bus.tick(self.bus.cycles_to_next_event());
            } else {
                let cycles = self.cpu.step(&mut self.bus);
                self.bus.tick(cycles);
            }
            self.ppu.sync(&self.bus);
        }
        self.frames += 1;
        self.checksum = fnv1a64_colours_continue(self.checksum, self.ppu.frame());
        (self.bus.cycles - started) as u32
    }

    /// One instruction, for a debugger's `Space` (G7 wires the key). Charges the clock exactly as
    /// the frame loop does, so a stepped machine and a running one agree — picture included.
    pub fn step_instruction(&mut self) -> u32 {
        let cycles = self.cpu.step(&mut self.bus);
        self.bus.tick(cycles);
        self.ppu.sync(&self.bus);
        cycles
    }

    /// Power-on again, with the cartridge still in the slot. Game > Reset's job; for
    /// now it is `Gba::new` with the same ROM bytes.
    pub fn reset(&mut self) {
        let cart = self.bus.cart.clone();
        let sample_rate = self.sample_rate;
        let channel_mask = self.channel_mask;
        // The cable is session state, not machine state: `Game > Reset` must not unplug the other
        // player, exactly as on the Game Boy's side.
        let cable = self.bus.serial.cable();
        let slot = self.bus.serial.slot();
        let link = self.link.clone();
        *self = Gba::new(cart);
        self.link = link;
        self.sample_rate = sample_rate;
        self.channel_mask = channel_mask;
        self.bus.sound.set_sample_rate(sample_rate);
        self.bus.sound.set_channel_mask(channel_mask);
        if let Some(cable) = cable {
            self.bus.serial.attach(cable, slot);
        }
    }

    /// The picture as RGBA, for a screenshot: each RGB555 word through the same `(v << 3) | (v >> 2)`
    /// expansion the front end uses, so a caller inside the library gets the pixels the window and
    /// the `F6` key would write.
    pub fn rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(PIXELS * 4);
        for &word in self.ppu.frame() {
            rgba.extend_from_slice(&tendy_gb::palette::rgb555(word));
        }
        rgba
    }

    /// The payload's sections in wire order, with the bytes each one wrote.
    ///
    /// The same code that writes a state answers this — [`Gba::encode`] is this function with the
    /// recorder switched off — so a breakdown of the 583 599 bytes cannot drift away from the
    /// format it describes. The bytes sum to `save_state().len()`; a test holds that true.
    pub fn state_sections(&self) -> Vec<StateSection> {
        self.encode_recorded(true).1
    }

    /// A state that can be carried back to this exact moment: registers, the clock, every internal
    /// memory and the I/O page. The cartridge is not in it — a ROM is the machine's identity, not
    /// its contents — but the SRAM is, as it is on the Game Boy side.
    fn encode(&self) -> Vec<u8> {
        self.encode_recorded(false).0
    }

    /// [`Gba::encode`], optionally recording where each section starts and ends.
    // The last `marked = w.len()` is the payload's end, which nothing reads again.
    #[allow(unused_assignments)]
    fn encode_recorded(&self, record: bool) -> (Vec<u8>, Vec<StateSection>) {
        let mut sections: Vec<StateSection> = Vec::new();
        let mut marked = 0usize;
        let mut w = StateWriter::new();
        // One macro rather than a closure: it borrows neither `w` nor `sections` for longer than
        // the statement it sits in, so the encoder body below stays the plain list of writes it was.
        macro_rules! section {
            ($name:expr) => {
                if record {
                    sections.push(StateSection {
                        name: $name,
                        bytes: w.len() - marked,
                    });
                    marked = w.len();
                }
            };
        }
        for &byte in &STATE_MAGIC {
            w.u8(byte);
        }
        w.u16(STATE_VERSION);
        section!("header");
        w.u64(self.frames);
        w.u64(self.checksum);
        w.u64(self.bus.cycles);
        w.u16(self.bus.lcd.line);
        w.u32(self.bus.lcd.line_cycle);
        w.u64(self.bus.lcd.frames);
        section!("clock & counters");
        for register in self.cpu.r {
            w.u32(register);
        }
        w.u32(self.cpu.cpsr);
        for spsr in self.cpu.spsr {
            w.u32(spsr);
        }
        for [sp, lr] in self.cpu.bank_sp_lr {
            w.u32(sp);
            w.u32(lr);
        }
        for register in self.cpu.fiq_r8_r12 {
            w.u32(register);
        }
        for register in self.cpu.user_r8_r12 {
            w.u32(register);
        }
        w.u8(u8::from(self.cpu.halted));
        w.u64(self.cpu.instructions);
        w.u64(self.cpu.swi_calls);
        section!("cpu registers");
        self.ppu.encode(&mut w);
        section!("ppu (live + shown frames)");
        w.u32(self.bus.open_bus);
        w.u8(self.bus.buttons.0);
        w.u8(self.bus.shoulders.0);
        section!("keypad & open bus");
        w.bytes(&self.bus.io);
        section!("io page");
        w.bytes(&self.bus.palette);
        section!("palette");
        w.bytes(&self.bus.oam);
        section!("oam");
        self.bus.save.encode(&mut w);
        section!("save chip");
        match &self.bus.rtc {
            Some(rtc) => {
                w.bool(true);
                rtc.encode(&mut w);
            }
            None => w.bool(false),
        }
        section!("rtc");
        w.bytes(&self.bus.ewram[..]);
        section!("ewram");
        w.bytes(&self.bus.iwram[..]);
        section!("iwram");
        w.bytes(&self.bus.vram[..]);
        section!("vram");
        // G2: the BIOS's own record and the bus's timing state, in two fixed-size blocks so this
        // payload stays a list of arrays rather than a list of fields.
        w.bytes(&self.bus.bios.encode());
        section!("bios record");
        w.bytes(&self.bus.encode_timing());
        section!("bus timing");
        // G4: the timers, and the halt request a store to `HALTCNT` leaves for the next step.
        w.bytes(&self.bus.encode_timers());
        section!("timers");
        w.u8(u8::from(self.bus.halt_request));
        section!("halt request");
        // G5: the sound registers, both FIFOs, the sample clocks and the resampler's position.
        self.bus.sound.encode(&mut w);
        section!("sound");
        // G7: the DMA channels' internal cursors, which are what a transfer in progress walks.
        w.bytes(&self.bus.dma.encode());
        section!("dma cursors");
        (w.finish(), sections)
    }

    /// Restore [`Gba::encode`]'s body. Every field is read into a local before anything is written,
    /// so a state that turns out to be damaged leaves the running machine untouched.
    fn decode(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        let mut r = StateReader::new(bytes);
        let mut magic = [0u8; 4];
        for byte in &mut magic {
            *byte = r.u8()?;
        }
        if magic != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let version = r.u16()?;
        if version != STATE_VERSION {
            return Err(StateError::BadVersion(version));
        }
        let frames = r.u64()?;
        let checksum = r.u64()?;
        let cycles = r.u64()?;
        let line = r.u16()?;
        let line_cycle = r.u32()?;
        let lcd_frames = r.u64()?;
        let mut registers = [0u32; 16];
        for register in &mut registers {
            *register = r.u32()?;
        }
        let cpsr = r.u32()?;
        let mut spsr = [0u32; cpu::BANK_COUNT];
        for slot in &mut spsr {
            *slot = r.u32()?;
        }
        let mut bank_sp_lr = [[0u32; 2]; cpu::BANK_COUNT];
        for pair in &mut bank_sp_lr {
            pair[0] = r.u32()?;
            pair[1] = r.u32()?;
        }
        let mut fiq_r8_r12 = [0u32; 5];
        for slot in &mut fiq_r8_r12 {
            *slot = r.u32()?;
        }
        let mut user_r8_r12 = [0u32; 5];
        for slot in &mut user_r8_r12 {
            *slot = r.u32()?;
        }
        let halted = r.u8()? != 0;
        let instructions = r.u64()?;
        let swi_calls = r.u64()?;
        // The picture is read into a copy for the same reason the registers are read into locals:
        // a state that fails a check further down leaves the running machine as it was.
        let mut ppu = self.ppu.clone();
        ppu.decode(&mut r)?;
        let open_bus = r.u32()?;
        let buttons = r.u8()?;
        let shoulders = r.u8()?;
        let mut io = [0u8; bus::IO_SIZE];
        let mut palette = [0u8; bus::PALETTE_SIZE];
        let mut oam = [0u8; bus::OAM_SIZE];
        let mut ewram = Box::new([0u8; bus::EWRAM_SIZE]);
        let mut iwram = Box::new([0u8; bus::IWRAM_SIZE]);
        let mut vram = Box::new([0u8; bus::VRAM_SIZE]);
        let mut bios_state = [0u8; bios::Bios::STATE_LEN];
        let mut timing = [0u8; 20];
        r.bytes_into(&mut io)?;
        r.bytes_into(&mut palette)?;
        r.bytes_into(&mut oam)?;
        let mut save = Save::default();
        save.decode(&mut r)?;
        let rtc = if r.bool()? {
            let mut rtc = Rtc::new();
            rtc.decode(&mut r)?;
            Some(rtc)
        } else {
            None
        };
        r.bytes_into(&mut ewram[..])?;
        r.bytes_into(&mut iwram[..])?;
        r.bytes_into(&mut vram[..])?;
        r.bytes_into(&mut bios_state)?;
        r.bytes_into(&mut timing)?;
        let mut timers = [0u8; 32];
        r.bytes_into(&mut timers)?;
        let halt_request = r.u8()? != 0;
        let mut sound = Sound::new();
        sound.decode(&mut r)?;
        let mut dma_cursors = [0u8; 4 * 8];
        r.bytes_into(&mut dma_cursors)?;
        if r.remaining() != 0 {
            return Err(StateError::BadValue);
        }
        self.frames = frames;
        self.checksum = checksum;
        self.bus.cycles = cycles;
        self.bus.lcd.line = line;
        self.bus.lcd.line_cycle = line_cycle;
        self.bus.lcd.frames = lcd_frames;
        self.cpu.r = registers;
        self.cpu.cpsr = cpsr;
        self.cpu.spsr = spsr;
        self.cpu.bank_sp_lr = bank_sp_lr;
        self.cpu.fiq_r8_r12 = fiq_r8_r12;
        self.cpu.user_r8_r12 = user_r8_r12;
        self.cpu.halted = halted;
        self.cpu.instructions = instructions;
        self.cpu.swi_calls = swi_calls;
        self.ppu = ppu;
        self.bus.open_bus = open_bus;
        self.bus.buttons = Buttons(buttons);
        self.bus.shoulders = Shoulders(shoulders);
        self.bus.io = io;
        // The link port's registers travelled in the page, as the hardware's are storage there;
        // the transfer in flight did not, because a state never holds one.
        self.bus.serial.after_load(&mut self.bus.io);
        self.bus.palette = palette;
        self.bus.oam = oam;
        self.bus.save = save;
        self.bus.rtc = rtc;
        self.bus.ewram = ewram;
        self.bus.iwram = iwram;
        self.bus.vram = vram;
        self.bus.bios.decode(&bios_state);
        self.bus.decode_timing(&timing);
        self.bus.decode_timers(&timers);
        self.bus.halt_request = halt_request;
        self.bus.sound = sound;
        self.bus.dma.decode(&dma_cursors);
        Ok(())
    }
}

/// A DMG-shaped VRAM array, all zeroes. The Game Boy Advance's 96 KiB of VRAM and 1 KiB of OAM do
/// not fit the `System` trait's fixed-size views, which the debugger panel draws; the GBA panel is
/// G7's rework, and until then this is the honest answer rather than a truncated copy of the real
/// memory pretending to be all of it.
static NO_VRAM: [u8; tendy_gb::VRAM_SIZE] = [0; tendy_gb::VRAM_SIZE];
/// See [`NO_VRAM`].
static NO_OAM: [u8; tendy_gb::OAM_SIZE] = [0; tendy_gb::OAM_SIZE];

impl System for Gba {
    fn run_frame(&mut self) -> u32 {
        Gba::run_frame(self)
    }

    /// The picture, in colour: 240x160 RGB555 words, which is what the front end expands. `pixels`
    /// stays a zeroed length-38 400 array because the type asks for it; a Game Boy Advance has no
    /// shade indices to put there.
    fn frame(&self) -> Frame<'_> {
        Frame::new(WIDTH, HEIGHT, &self.pixels).with_colours(self.ppu.frame())
    }

    /// Interleaved stereo f32 at the rate last set by `set_sample_rate`; returns frames written.
    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        self.bus.sound.read_samples(out)
    }

    fn set_buttons(&mut self, buttons: Buttons) {
        self.bus.set_buttons(buttons);
    }

    /// The two shoulder buttons, which reach `KEYINPUT` bits 8 (R) and 9 (L). The Game Boy's
    /// [`Buttons`] has one byte for eight buttons, so the pair travels on its own (G7).
    fn set_shoulders(&mut self, shoulders: Shoulders) {
        self.bus.set_shoulders(shoulders);
    }

    fn set_tilt(&mut self, _x: i8, _y: i8) {}

    fn frames(&self) -> u64 {
        self.frames
    }

    /// Always false: this machine has no boot ROM, so the cartridge owns it from the first
    /// instruction. G2's BIOS replaces nothing here — it replaces the open-bus `$00000000`.
    fn booting(&self) -> bool {
        false
    }

    fn frame_checksum(&self) -> u64 {
        self.checksum
    }

    fn console_name(&self) -> &'static str {
        "Game Boy Advance"
    }

    fn is_advance(&self) -> bool {
        true
    }

    /// The save chip [`crate::save::detect`] picked from the ROM's ID strings — a GBA header
    /// carries no backup-type field, so the detection is the only truth there is (G7).
    fn cartridge_storage(&self) -> &'static str {
        self.bus.save.kind().label()
    }

    fn is_cgb(&self) -> bool {
        false
    }

    /// A Game Boy Advance is its own console, never a Game Boy Color.
    fn is_cgb_console(&self) -> bool {
        false
    }

    fn cgb_speed(&self) -> u8 {
        1
    }

    fn compat_palette(&self) -> Option<(u8, tendy_gb::cgb::CompatReason)> {
        None
    }

    fn cart_identity(&self) -> CartIdentity {
        CartIdentity {
            title: self.bus.cart.header.title.clone(),
            crc32: self.bus.cart.crc32,
        }
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.encode()
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.decode(bytes)
    }

    /// Why an older Advance state is refused, in the words the bar shows (G8).
    ///
    /// The bump to version 9 was not a rearrangement: the BIOS sound driver keeps almost all of its
    /// state in the caller's work area, which travels with the machine, but the *address* of that
    /// area has nowhere to live except the machine's own field. A version-8 state has no copy of
    /// it, so resuming one would leave the driver bound to nothing and drop the music silently —
    /// refusing is the honest answer. The Game Boy's own container is a different format, so its
    /// states never reach this arm, and `GBST` stays at version 5.
    fn state_refusal(&self, why: StateError) -> String {
        match why {
            StateError::BadVersion(version) => format!(
                "this version-{version} Game Boy Advance state was written before the sound \
                 driver kept its work-area pointer, so it is refused rather than resumed with the \
                 driver unbound — Game Boy and Game Boy Advance states are separate"
            ),
            other => format!("{other:?}"),
        }
    }

    fn reset(&mut self) {
        Gba::reset(self)
    }

    fn set_sample_rate(&mut self, hz: u32) {
        self.sample_rate = hz;
        self.bus.sound.set_sample_rate(hz);
    }

    fn set_channel_mask(&mut self, mask: u8) {
        self.channel_mask = mask;
        self.bus.sound.set_channel_mask(mask);
    }

    fn tick_rtc(&mut self, _now_ms: u64) {}

    /// Nothing yet. The GBA has no link-port driver (G9) and no BIOS debug output (G2), so a ROM
    /// that prints has nowhere to print to.
    fn take_serial(&mut self) -> String {
        String::new()
    }

    /// Whether the link port has something to say (a peer that left, a transfer that timed out),
    /// taken once. The Advance's own port is [`crate::serial::Serial`]; this arm is here
    /// because the front end asks every machine through one trait.
    fn take_link_notice(&mut self) -> bool {
        let had = self.bus.serial.message.is_some();
        // The message itself is what the bar prints, so only a caller that wants a bare flag
        // pays for this: it leaves the message in place.
        had
    }

    /// The host end of a cable, for a caller that has no slot to give (G9b). The window and the
    /// headless pairing both know their role, so they call [`System::attach_link`] with it.
    fn set_link(&mut self, link: Option<tendy_gb::link::Link>) {
        Gba::attach_link(self, link, 0);
    }

    /// The same plug, through the trait: the window's `Link > Role` row hands over the end of the
    /// cable it dialled or listened as.
    fn attach_link(&mut self, link: Option<tendy_gb::link::Link>, slot: usize) {
        Gba::attach_link(self, link, slot);
    }

    /// Where the cable stands: asked of the socket's own handle, because the serial engine holds
    /// the port and a port answers [`crate::serial::Lines`], not a [`LinkStatus`].
    fn link_status(&self) -> tendy_gb::link::LinkStatus {
        match &self.link {
            Some(link) => link.status(),
            None => tendy_gb::link::LinkStatus::Off,
        }
    }

    /// The Advance's own message, in the words the Game Boy's bar already uses: the two enums carry
    /// the same two events and the same sentences, so the bar needs no second set of strings.
    fn take_link_message(&mut self) -> Option<tendy_gb::serial::LinkMessage> {
        self.bus.serial.message.take().map(|message| match message {
            serial::Message::PeerGone => tendy_gb::serial::LinkMessage::PeerGone,
            serial::Message::PeerSilent => tendy_gb::serial::LinkMessage::PeerSilent,
        })
    }

    /// The save chip's bytes, with the RTC's clock as a footer when the cartridge has one — the
    /// same body the Game Boy path writes for a cartridge with RAM and a clock.
    fn battery_body(&self) -> Option<Vec<u8>> {
        crate::save::encode_battery(&self.bus.save, self.bus.rtc.as_ref())
    }

    fn load_battery_body(&mut self, bytes: &[u8], _now_ms: u64) -> Result<bool, String> {
        crate::save::load_battery(&mut self.bus.save, &mut self.bus.rtc, bytes)
    }

    /// Cheats are a Game Boy feature: the codes are GameShark/Game Genie encodings of a memory
    /// address on a different bus. Refused by name rather than silently ignored.
    fn apply_cheats(&mut self, entries: &[CheatEntry]) -> Vec<String> {
        if entries.is_empty() {
            return Vec::new();
        }
        vec![crate::notes::CHEATS.to_owned()]
    }

    fn step_instruction(&mut self) -> u32 {
        Gba::step_instruction(self)
    }

    /// The real 32-bit program counter (H3). Until H3 this was `as u16` — the low half of
    /// `$08000123` is `$0123`, which names nothing an Advance can execute.
    fn pc(&self) -> u32 {
        self.cpu.pc()
    }

    fn sp(&self) -> u32 {
        self.cpu.r[13]
    }

    /// The four facts a trace line needs, in the shape `--trace-pc` prints them: the ARM7TDMI's
    /// counter, stack and link registers, its `CPSR` mode by name, and the instruction set bit.
    fn cpu_state(&self) -> CpuState {
        CpuState {
            pc: self.cpu.pc(),
            sp: self.cpu.r[13],
            lr: Some(self.cpu.r[14]),
            mode: crate::notes::mode_name(self.cpu.mode()),
            thumb: self.cpu.thumb(),
        }
    }

    /// Zeroes: the eight 8-bit registers a Game Boy has do not exist on a GBA, which has sixteen
    /// 32-bit ones — [`System::advance_regs`] is where the panel reads those (H3).
    fn cpu_regs(&self) -> CpuRegs {
        CpuRegs::default()
    }

    /// `R0-R15`, the `CPSR` and the six banked status registers, as they stand right now.
    fn advance_regs(&self) -> Option<AdvanceRegs> {
        Some(AdvanceRegs {
            r: self.cpu.r,
            cpsr: self.cpu.cpsr,
            spsr: self.cpu.spsr,
            mode: crate::notes::mode_name(self.cpu.mode()),
            thumb: self.cpu.thumb(),
        })
    }

    /// The whole 32-bit address space, through the bus's own decoder: no cycle, no side effect.
    fn peek(&mut self, addr: u32) -> u8 {
        self.bus.read8(addr)
    }

    fn vram(&self) -> &[u8; tendy_gb::VRAM_SIZE] {
        &NO_VRAM
    }

    fn oam(&self) -> &[u8; tendy_gb::OAM_SIZE] {
        &NO_OAM
    }

    fn view_regs(&self) -> ViewRegs {
        ViewRegs {
            lcdc: 0,
            scx: 0,
            scy: 0,
            bgp: 0,
            obp0: 0,
            obp1: 0,
            ly: 0,
            wx: 0,
            wy: 0,
        }
    }

    fn cycles(&self) -> u64 {
        self.bus.cycles
    }

    /// `BLDCNT` and `BLDY` as the last frame left them. The bytes are the I/O page's
    /// own — offset `$050` halfword and `$054` byte (GBATEK, `04000050h`/`04000054h`) — so this
    /// reports what the renderer applied, not a second copy that could drift from it.
    fn effect_state(&self) -> Option<EffectState> {
        let bldcnt = u16::from_le_bytes([self.bus.io[0x050], self.bus.io[0x051]]);
        Some(EffectState::new(bldcnt, self.bus.io[0x054]))
    }

    /// Zero: the CGB's VRAM DMA does not exist on a GBA, whose four DMA channels are G4's.
    fn vram_dma_blocks(&self) -> u64 {
        0
    }

    /// The count the pacing line prints: what the CPU retired, as against the cycles the frame cost.
    /// The gap between the two is what DMA and halt took away from the CPU's own work.
    fn instructions(&self) -> u64 {
        self.cpu.instructions
    }

    /// `--trace-swi N`: hand the limit to the BIOS, which records the next `N` calls.
    fn trace_swis(&mut self, limit: usize) {
        self.bus.bios.trace_calls(limit);
    }

    fn swi_trace(&self) -> Vec<crate::bios::SwiCall> {
        self.bus.bios.trace.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, EffectState};

    /// The sweep's `faded` bucket has to be read off the registers the renderer read, so
    /// the mode and the "is a brightness effect in force" question are pinned here — `dn`'s measured
    /// case is `BLDCNT` darken with `EVY` 16, which is `I - I*16/16 = 0` on every channel.
    #[test]
    fn bldcnt_picks_the_mode_and_a_brightness_effect_is_in_force_only_when_it_moves_a_pixel() {
        // The black frames bucket `dn` measured: darken, EVY 16, background 2/3 and the backdrop
        // as targets — one colour because the picture was multiplied to nothing.
        let dark = EffectState::new(0x00F4, 16);
        assert_eq!(dark.effect, Effect::Darken);
        assert_eq!(dark.evy(), 16);
        assert!(dark.brightness_in_force());
        assert_eq!(dark.line(), "BLDCNT $00F4 BLDY 16 (darken, in force)");

        // Register $0000: no effect selected, so a one-colour frame is a blank, not a fade.
        assert_eq!(Effect::from_bldcnt(0x0000), Effect::None);
        assert!(!EffectState::new(0x0000, 16).brightness_in_force());

        // Alpha mixes two targets; it does not multiply one, and EVY does not apply to it.
        assert_eq!(Effect::from_bldcnt(0x0044), Effect::Alpha);
        assert!(!EffectState::new(0x0044, 16).brightness_in_force());

        // Brighten with EVY 0 is the identity: selected, but nothing moves.
        assert_eq!(Effect::from_bldcnt(0x0084), Effect::Brighten);
        assert!(!EffectState::new(0x0084, 0).brightness_in_force());
        assert!(EffectState::new(0x0084, 16).brightness_in_force());

        // No first target selected (bits 0-5 clear): the effect cannot apply to any layer.
        assert!(!EffectState::new(0x00C0, 16).brightness_in_force());

        // EVY is five bits clamped at 16/16, as the renderer clamps it (GBATEK: "17..31=16/16").
        assert_eq!(EffectState::new(0x00F4, 20).evy(), 16);
        assert_eq!(EffectState::new(0x00F4, 31).evy(), 16);
    }
}
