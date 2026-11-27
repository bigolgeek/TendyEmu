//! The machine: one CPU, one bus, and the frame loop.

use crate::apu::channel::{Noise, Pulse, Wave};
use crate::bus::Bus;
use crate::cart::Cartridge;
use crate::cpu::{Cpu, Flags};
use crate::debugger::ViewRegs;
use crate::joypad::Buttons;
use crate::ppu::Mode;
use crate::state::{
    STATE_MAGIC, STATE_VERSION, StateError, StateReader, StateWriter, fnv1a64, fnv1a64_colours,
    fnv1a64_colours_continue, fnv1a64_continue,
};
use crate::{OAM_SIZE, VRAM_SIZE};

/// Size of the DMG boot ROM: `$0000-$00FF`.
pub const BOOT_ROM_SIZE: usize = 256;
/// Size of the CGB boot ROM: 2048 bytes, mapped as `$0000-$00FF` + `$0200-$08FF` (the header is
/// visible in the gap at `$0100-$01FF`).
pub const CGB_BOOT_ROM_SIZE: usize = 2048;

/// Everything a test or the front end drives.
#[derive(Clone)]
pub struct Machine {
    pub bus: Bus,
    pub cpu: Cpu,
    pub frames: u64,
    /// fnv1a64 folded over every finished frame, in order.
    pub checksum: u64,
    /// fnv1a64 of the last finished frame alone.
    pub last_frame_checksum: u64,
    /// `bus.cycles` held when the boot ROM handed over; 0 until then and 0 without a boot ROM.
    /// Not part of the save state: it describes a moment of this power-on, not the machine.
    pub boot_cycles: u64,
    /// True once the boot ROM has been unmapped and the CPU has reached `$0100`. Derived from the
    /// save state on load (a state can only be written between frames, i.e. after the handover).
    boot_done: bool,
    /// The user's own SGB cartridge image when the "SGB ROM file" setting named one and
    /// [`crate::sgb::sound::load_rom`] accepted it; `None` otherwise, which is every
    /// machine that has no such file. The machine boots the file's sound program out of it, and
    /// it is not part of the save state: it belongs to this launch, not to the game.
    pub sgb_rom: Option<Vec<u8>>,
}

impl Machine {
    /// Post-boot state (SPEC R9): `Cpu::post_boot(cart.header.header_checksum)`, no boot ROM,
    /// registers and I/O as the boot image lists.
    pub fn new(cart: Cartridge) -> Machine {
        // The console matches the cartridge: a CGB cartridge on a CGB console, a monochrome one on
        // a DMG console (the v2.8.0 machine). [`Machine::new_cgb`] asks for a CGB console
        // explicitly.
        let console_cgb = cart.header.cgb_flag & 0x80 != 0;
        Machine::build(cart, console_cgb, false)
    }

    /// A machine that is a Game Boy Color console regardless of the cartridge, so a monochrome
    /// cartridge gets the compatibility palette — the automatic colouring a real CGB gives it.
    pub fn new_cgb(cart: Cartridge) -> Machine {
        Machine::build(cart, true, false)
    }

    /// A machine on a console of the caller's choosing: `console_cgb` true is a Game Boy Color,
    /// false a DMG. [`Machine::new`] is this with the cartridge's own flag; the `--cgb` flag and the
    /// `Emulation > Console` row ask for it explicitly.
    pub fn new_on(cart: Cartridge, console_cgb: bool) -> Machine {
        Machine::build(cart, console_cgb, false)
    }

    /// A Super Game Boy console: the DMG machine with [`crate::sgb::Sgb::on`] and the SGB
    /// boot ROM's register file. The `--sgb` flag and the `console = "sgb"` row ask for it; nothing
    /// picks it from a header.
    pub fn new_sgb(cart: Cartridge) -> Machine {
        Machine::build(cart, false, true)
    }

    /// Give an SGB machine the sound chip, from the user's own cartridge image, and boot the
    /// cartridge's own driver into it.
    ///
    /// Called only when the console is an SGB and the "SGB ROM file" setting named a file
    /// `crate::sgb::sound::load_rom` accepted. A machine that never calls this has no chip and mixes
    /// exactly what it mixed before; a machine that is not an SGB ignores it, because the SGB's sound
    /// only exists inside the Super Game Boy.
    ///
    /// The driver is loaded straight into APU RAM and started at `$0400` — `docs/SGB-SOUND.md` §1's
    /// equivalent of the 97-frame handshake — with the idle word `$A9A9` on the ports. A file whose
    /// table is not the one the document measured boots nothing and stays silent: the game's own
    /// sound is never worth a refusal.
    pub fn set_sgb_sound(&mut self, rom: Vec<u8>) -> Option<String> {
        if !self.bus.ppu.sgb.enabled() {
            return None;
        }
        let mut sound = crate::sgb::SgbSound::new();
        let refused = sound.boot(&rom).err();
        self.sgb_rom = Some(rom);
        self.bus.apu.sgb = Some(Box::new(sound));
        refused
    }

    /// Re-build the SGB's sound chip after the APU was replaced by a fresh one (a reset, or the boot
    /// ROM's handover), and load the cartridge's driver into it again.
    ///
    /// The chip is not device state: it runs the cartridge's own program, so the way back is the way
    /// in — `docs/SGB-SOUND.md` §1's direct load. No file, no chip, and still none.
    fn reinstall_sgb_sound(&mut self) {
        if !self.bus.ppu.sgb.enabled() {
            return;
        }
        let Some(rom) = self.sgb_rom.as_ref() else {
            return;
        };
        let mut sound = crate::sgb::SgbSound::new();
        let _ = sound.boot(rom);
        self.bus.apu.sgb = Some(Box::new(sound));
    }

    /// The shared power-on: the CPU register file for the console, the bus, then the compatibility
    /// palette when it applies.
    fn build(cart: Cartridge, console_cgb: bool, sgb: bool) -> Machine {
        let checksum_byte = cart.header.header_checksum;
        let entry = cart.entry_point();
        let cart_cgb = cart.header.cgb_flag & 0x80 != 0;
        // A CGB console running a CGB cartridge gets the CGB post-boot register file — A = `$11` is
        // how it knows the machine is a Game Boy Color (Pan Docs). A CGB console running a
        // monochrome cartridge, and a DMG console, get the DMG one: the compatibility palette
        // colours the picture, not the CPU's register file. An SGB is a DMG with its own,
        // checksum-independent register file.
        let cpu = if console_cgb && cart_cgb {
            Cpu::post_boot_cgb(entry)
        } else if sgb {
            Cpu::post_boot_sgb(entry)
        } else {
            Cpu::post_boot_at(checksum_byte, entry)
        };
        let mut machine = Machine {
            bus: Bus::new(cart, console_cgb),
            cpu,
            frames: 0,
            checksum: fnv1a64(&[]),
            last_frame_checksum: 0,
            boot_cycles: 0,
            boot_done: false,
            sgb_rom: None,
        };
        if sgb {
            machine.bus.ppu.sgb = crate::sgb::Sgb::on();
        }
        machine.apply_compat_mode();
        machine
    }

    /// A CGB console holding a monochrome cartridge starts in DMG compatibility mode with the
    /// compatibility palette applied (Pan Docs, "Compatibility palettes"): `KEY0` `$04`, `OPRI`
    /// `$01`, and the selected triplet written into palette RAM. The *machine* applies this at
    /// power-on, so it colours the cartridge with `Boot ROM` off too — the one documented
    /// simplification, chosen so the feature does not depend on which page is booted (real
    /// hardware colours the cartridge only through the boot ROM).
    fn apply_compat_mode(&mut self) {
        if !self.bus.cgb.compat_mode() {
            return;
        }
        self.bus.cgb.key0 = crate::cgb::KEY0_DMG_COMPAT;
        self.bus.cgb.opri = crate::cgb::OPRI_DMG;
        let selector = crate::cgb::compatibility_selector(&self.bus.cart.header);
        crate::cgb::apply_compatibility_palette(&mut self.bus.ppu.palettes, selector);
    }

    /// Runs the user's boot ROM before the cartridge entry: `Cpu::new()` (PC = `$0000`, every
    /// register zero) and the boot image mapped by [`Bus`]. A 256-byte image maps as the DMG page
    /// (`$0000-$00FF`); a 2048-byte image maps as the CGB page (`$0000-$00FF` + `$0200-$08FF`).
    pub fn with_boot_rom(cart: Cartridge, boot: Vec<u8>) -> Machine {
        let console_cgb = cart.header.cgb_flag & 0x80 != 0;
        Machine::with_boot_rom_on(cart, boot, console_cgb)
    }

    /// [`Machine::with_boot_rom`] on a console of the caller's choosing, so `--cgb` puts a
    /// monochrome cartridge on a colour console and still boots it through a boot ROM.
    pub fn with_boot_rom_on(cart: Cartridge, boot: Vec<u8>, console_cgb: bool) -> Machine {
        Machine::with_boot(cart, boot, console_cgb, false)
    }

    /// [`Machine::new_sgb`] with a boot ROM: the 256-byte DMG page, since the SGB's own boot ROM is
    /// the monochrome one (Pan Docs, *Power Up Sequence*: it "only forwards logo to SGB BIOS,
    /// performs no checks"). SGB stays on through the handover.
    pub fn with_boot_rom_sgb(cart: Cartridge, boot: Vec<u8>) -> Machine {
        Machine::with_boot(cart, boot, false, true)
    }

    fn with_boot(cart: Cartridge, boot: Vec<u8>, console_cgb: bool, sgb: bool) -> Machine {
        let mut machine = Machine::build(cart, console_cgb, sgb);
        machine.bus.boot = Some(boot.into_boxed_slice());
        machine.bus.boot_mapped = true;
        machine.cpu = Cpu::new();
        machine.boot_done = false;
        machine.boot_cycles = 0;
        machine
    }

    /// True while the boot ROM still owns the machine: mapped in, or unmapped but not yet at
    /// `$0100`. False for a machine started without one.
    pub fn booting(&self) -> bool {
        self.bus.boot.is_some() && !self.boot_done
    }

    /// The instruction boundary at which the boot ROM has written `$FF50` and jumped to `$0100`.
    fn handover_due(&self) -> bool {
        self.booting() && !self.bus.boot_mapped && self.cpu.pc == 0x0100
    }

    /// Hands the cartridge the machine `Machine::new` would have given it.
    ///
    /// The boot ROM drew a splash and played a chime, and none of that may leak into the cartridge:
    /// the PPU, APU, timer, joypad, serial port and DMA go back to their post-boot values, VRAM and
    /// OAM are emptied, HRAM is zeroed (the ROM used the top of it as a scratch for `POP AF`), and
    /// the PPU restarts the frame at dot 0 so the cartridge's first frame is the same frame it
    /// would have drawn with no boot ROM at all. This is a deliberate departure from real hardware,
    /// which keeps VRAM and the PPU's phase; it is what makes "identical to the no-boot path" true
    /// for every cartridge instead of only for the ones that clear VRAM themselves.
    fn hand_over(&mut self) {
        // SGB is the console, not the PPU's device state: the handover keeps it on, the way it
        // keeps the cable plugged in below.
        let sgb = self.bus.ppu.sgb.clone();
        self.bus.ppu = crate::ppu::Ppu::new();
        self.bus.ppu.sgb = sgb;
        self.bus.apu = crate::apu::Apu::new();
        self.reinstall_sgb_sound();
        self.bus.timer = crate::timer::Timer::new();
        self.bus.joypad = crate::joypad::Joypad::new();
        // The cable is session state, not device state: a handover or a reset keeps it plugged in.
        self.bus.serial.restart();
        self.bus.dma = crate::bus::Dma::new();
        self.bus.vdma = crate::vdma::VramDma::new();
        self.bus.hram = [0; crate::HRAM_SIZE];
        self.bus.ie = 0;
        self.bus.iflag = crate::bus::IF_VBLANK;
        self.bus.boot_mapped = false;
        self.boot_done = true;
        self.boot_cycles = self.bus.cycles;
    }

    /// Steps the boot ROM to the handover without running a cartridge frame, so a test can look at
    /// the machine exactly at `$0100`. Returns false when the ROM parked (failed header checksum)
    /// or `max_cycles` ran out first.
    pub fn boot_to_handover(&mut self, max_cycles: u64) -> bool {
        while self.booting() && self.bus.cycles < max_cycles {
            if self.handover_due() {
                self.hand_over();
                break;
            }
            if self.bus.ppu.frame_ready {
                self.bus.ppu.take_frame();
            }
            self.cpu.step(&mut self.bus);
        }
        self.boot_done
    }

    /// Game > Reset. The CPU, PPU, APU, timer, serial, joypad and DMA go to
    /// their post-boot values; WRAM, HRAM, VRAM, OAM, cartridge RAM and the RTC keep their contents.
    /// A session opened with a boot ROM replays it from `$0000` (the ROM clears VRAM itself).
    pub fn reset(&mut self) {
        let vram = self.bus.ppu.vram;
        let vram1 = self.bus.ppu.vram1;
        let oam = self.bus.ppu.oam;
        // On a CGB the palette RAM survives a reset too: it is memory, like VRAM, and the boot ROM
        // is what refills it from scratch (C4). `KEY0`/`VBK`/`SVBK` are registers and go back to 0.
        let palettes = self.bus.ppu.palettes.clone();
        // SGB is the console, not device state, so a reset keeps it on: back to `Sgb::on()`, whose
        // palettes and attribute map are the power-on defaults.
        let sgb = self.bus.ppu.sgb.enabled();
        let replays_boot = self.bus.boot.is_some();
        let console_cgb = self.bus.cgb.hardware;
        let cart_cgb = self.bus.cart.header.cgb_flag & 0x80 != 0;
        self.cpu = if replays_boot {
            Cpu::new()
        } else if console_cgb && cart_cgb {
            Cpu::post_boot_cgb(self.bus.cart.entry_point())
        } else if sgb {
            Cpu::post_boot_sgb(self.bus.cart.entry_point())
        } else {
            Cpu::post_boot_at(
                self.bus.cart.header.header_checksum,
                self.bus.cart.entry_point(),
            )
        };
        // Power-on speed is normal, `KEY0`/`VBK`/`SVBK` are 0 and no switch is in flight (Pan Docs:
        // "The CGB is operating in Normal Speed Mode when it is first turned on").
        self.bus.cgb = crate::cgb::Cgb::new(console_cgb, cart_cgb);
        self.bus.ppu = crate::ppu::Ppu::new();
        self.bus.ppu.vram = vram;
        self.bus.ppu.vram1 = vram1;
        self.bus.ppu.oam = oam;
        self.bus.ppu.palettes = palettes;
        if sgb {
            self.bus.ppu.sgb = crate::sgb::Sgb::on();
        }
        self.bus.apu = crate::apu::Apu::new();
        self.reinstall_sgb_sound();
        self.bus.timer = crate::timer::Timer::new();
        self.bus.joypad = crate::joypad::Joypad::new();
        // The cable is session state, not device state: a handover or a reset keeps it plugged in.
        self.bus.serial.restart();
        self.bus.dma = crate::bus::Dma::new();
        self.bus.vdma = crate::vdma::VramDma::new();
        self.bus.ie = 0;
        self.bus.iflag = crate::bus::IF_VBLANK;
        self.bus.cycles = 0;
        self.bus.boot_mapped = replays_boot;
        self.boot_done = false;
        self.boot_cycles = 0;
        // A reset goes back to power-on, so a monochrome cartridge on a CGB console is put back
        // into DMG compatibility mode with its palette (`KEY0`/`OPRI` were zeroed above).
        self.apply_compat_mode();
    }

    /// Runs whole frames until the PPU completes one. Returns machine cycles consumed.
    ///
    /// With a boot ROM loaded its own frames are drawn for the window but are neither counted in
    /// `frames` nor folded into the checksum: those count cartridge frames, and the first one
    /// begins at the handover with a PPU exactly as `Machine::new` leaves it. A boot ROM that parks
    /// on a failed header checksum just keeps returning its splash frame.
    pub fn run_frame(&mut self) -> u32 {
        let mut cycles = 0;
        while self.booting() {
            if self.handover_due() {
                self.hand_over();
                break;
            }
            if self.bus.ppu.frame_ready {
                self.bus.ppu.take_frame();
                if self.booting() {
                    return cycles;
                }
            }
            cycles += self.cpu.step(&mut self.bus);
        }
        while !self.bus.ppu.frame_ready {
            cycles += self.cpu.step(&mut self.bus);
        }
        self.count_frame();
        cycles
    }

    /// Folds the frame the PPU has just finished into `frames`, `checksum` and
    /// `last_frame_checksum`, and clears the flag. One place, so `run_frame`'s frame loop and the
    /// debugger's `step_instruction` can never disagree about what a counted frame is.
    ///
    /// The SGB's sound requests are handed to its chip here, at the frame's end: the SGB
    /// program writes its ports once per pass (`docs/SGB-SOUND.md` §2's sender), so a frame is the
    /// granularity the cartridge itself uses.
    fn count_frame(&mut self) {
        self.bus.ppu.take_frame();
        self.pump_sgb_sound();
        self.frames += 1;
        // A DMG frame is folded as the shade bytes it has always been, so every DMG checksum in
        // this project is unchanged; a CGB frame is folded as its RGB555 words, because two
        // palettes that map the same indices differently draw different pictures and must digest
        // differently.
        if self.bus.ppu.cgb {
            let colours = self.bus.ppu.colours();
            self.last_frame_checksum = fnv1a64_colours(colours);
            self.checksum = fnv1a64_colours_continue(self.checksum, colours);
        } else {
            let frame = self.bus.ppu.frame();
            self.last_frame_checksum = fnv1a64(frame);
            self.checksum = fnv1a64_continue(self.checksum, frame);
        }
    }

    /// Hand the SGB's sound chip whatever its cartridge asked for since the last frame:
    /// one `SOUND` (`$08`) request's two port words, or a landed `SOU_TRN` (`$09`) record list.
    ///
    /// Each is taken once — the chip's echo, not the pump, is how the cartridge learns a request was
    /// taken (`docs/SGB-SOUND.md` §2) — and with no chip nothing is taken, so a machine without one
    /// keeps the requests recorded exactly as they arrived.
    fn pump_sgb_sound(&mut self) {
        let Some(chip) = self.bus.apu.sgb.as_mut() else {
            return;
        };
        if let Some(request) = self.bus.ppu.sgb.take_sound() {
            chip.play(request);
        }
        if let Some(transfer) = self.bus.ppu.sgb.take_sound_transfer() {
            chip.transfer(&transfer);
        }
    }

    /// One CPU instruction, for the debugger's `Space`. Returns machine cycles, like
    /// `Cpu::step`.
    ///
    /// The frame accounting is `run_frame`'s: an instruction that completes a frame counts it and
    /// folds it into the checksum, so a machine stepped to the end of frame *n* reports the frame
    /// *n* checksum `run_frame` would. While the boot ROM still owns the machine its frames are
    /// drawn and dropped exactly as `run_frame` does, and the handover is taken at the same
    /// instruction boundary.
    pub fn step_instruction(&mut self) -> u32 {
        if self.handover_due() {
            self.hand_over();
        }
        let cycles = self.cpu.step(&mut self.bus);
        if self.bus.ppu.frame_ready {
            if self.booting() {
                self.bus.ppu.take_frame();
            } else {
                self.count_frame();
            }
        }
        cycles
    }

    /// One whole frame, for the debugger's `N` — `run_frame` under the name the panel's footer
    /// uses.
    pub fn step_frame(&mut self) -> u32 {
        self.run_frame()
    }

    /// The program counter, as the panel and its breakpoints see it.
    pub fn pc(&self) -> u16 {
        self.cpu.pc
    }

    /// The stack pointer, for the panel's header.
    pub fn sp(&self) -> u16 {
        self.cpu.sp
    }

    /// The byte at `addr` as the machine holds it, for the viewers: no cycle, no ownership mask
    /// (see [`Bus::peek_stored`]).
    pub fn peek(&mut self, addr: u16) -> u8 {
        self.bus.peek_stored(addr)
    }

    /// `$8000-$9FFF`, as stored.
    pub fn vram(&self) -> &[u8; VRAM_SIZE] {
        &self.bus.ppu.vram
    }

    /// The 160 OAM bytes, as stored.
    pub fn oam(&self) -> &[u8; OAM_SIZE] {
        &self.bus.ppu.oam
    }

    /// The PPU registers the viewers read, copied in one go.
    pub fn view_regs(&self) -> ViewRegs {
        let ppu = &self.bus.ppu;
        ViewRegs {
            lcdc: ppu.lcdc,
            scx: ppu.scx,
            scy: ppu.scy,
            bgp: ppu.bgp,
            obp0: ppu.obp0,
            obp1: ppu.obp1,
            ly: ppu.ly,
            wx: ppu.wx,
            wy: ppu.wy,
        }
    }

    /// 23 040 shade indices, row-major, top-left first. 0 = lightest, 3 = darkest. On a CGB these
    /// are the pre-palette colour indices of the picture; the colours are [`Machine::colours`].
    pub fn frame(&self) -> &[u8] {
        self.bus.ppu.frame()
    }

    /// 23 040 colour words, row-major: the shade index on a DMG, the RGB555 colour on a CGB.
    pub fn colours(&self) -> &[u16] {
        self.bus.ppu.colours()
    }

    /// Running checksum, never 0 after one frame.
    pub fn frame_checksum(&self) -> u64 {
        self.checksum
    }

    pub fn last_frame_checksum(&self) -> u64 {
        self.last_frame_checksum
    }

    /// True when CGB code is running: a CGB cartridge on this machine and not in DMG compatibility
    /// mode. False for every DMG cartridge, which is the whole of v2.8.0's behaviour.
    pub fn is_cgb(&self) -> bool {
        self.bus.cgb.is_cgb()
    }

    /// True when this machine *is* a Game Boy Color console, whatever it runs: a CGB cartridge, or
    /// a monochrome one forced onto the colour console by `--cgb` or the `Console` row. The status
    /// bar's `CGB` marker reads this, so a monochrome cartridge in compatibility mode still says
    /// `CGB`, while [`Machine::is_cgb`] keeps its "CGB code is running" meaning.
    pub fn is_cgb_console(&self) -> bool {
        self.bus.cgb.hardware
    }

    /// Blocks the CGB's VRAM DMA (`GDMA`/`HDMA`) has moved since power-on — the instrumentation
    /// that proves a title uses the hardware path. Zero on a DMG cartridge, and zero on
    /// any cartridge that never starts a transfer.
    pub fn vram_dma_blocks(&self) -> u64 {
        self.bus.vdma.blocks
    }

    /// The HBlank-block part of [`Machine::vram_dma_blocks`]: blocks that arrived one per mode 0
    /// rather than in a general-purpose batch. A title whose count here is non-zero is doing
    /// mid-frame effects with `HDMA`, which is the thing C3 exists for.
    pub fn vram_dma_hblank_blocks(&self) -> u64 {
        self.bus.vdma.hblank_blocks
    }

    /// The CPU's current speed, 1 or 2 (`KEY1` bit 7). Always 1 when `is_cgb()` is false.
    pub fn cgb_speed(&self) -> u8 {
        self.bus.cgb.speed()
    }

    /// The compatibility palette ID and the reason it was chosen, when this machine is a CGB
    /// console holding a monochrome cartridge; `None` otherwise. The debug panel shows it.
    pub fn compat_palette(&self) -> Option<(u8, crate::cgb::CompatReason)> {
        if self.bus.cgb.compat_mode() {
            Some(crate::cgb::compatibility_choice(&self.bus.cart.header))
        } else {
            None
        }
    }

    pub fn buttons(&self) -> Buttons {
        self.bus.joypad.pressed
    }

    pub fn set_buttons(&mut self, buttons: Buttons) {
        let bits = self.bus.joypad.set(buttons);
        self.bus.request(bits);
    }

    /// MBC7's accelerometer: -1, 0 or +1 per axis while the right mouse button is held
    /// (`MBC7.html`).
    pub fn set_tilt(&mut self, x: i8, y: i8) {
        self.bus.cart.set_tilt(x, y);
    }

    /// Interleaved stereo f32 at the rate last set by `set_sample_rate`; returns frames written.
    pub fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        self.bus.apu.read_samples(out)
    }

    pub fn set_sample_rate(&mut self, hz: u32) {
        self.bus.apu.set_sample_rate(hz);
    }

    /// The four CHANNELS rows.
    pub fn set_channel_mask(&mut self, mask: u8) {
        self.bus.apu.set_channel_mask(mask);
    }

    /// Cartridge RAM as the chip holds it, `None` when the cartridge has none.
    pub fn battery(&self) -> Option<&[u8]> {
        if self.bus.cart.ram.is_empty() {
            None
        } else {
            Some(self.bus.cart.ram())
        }
    }

    pub fn load_battery(&mut self, ram: &[u8]) -> Result<(), String> {
        if self.bus.cart.ram.is_empty() {
            return Err("cartridge has no battery RAM".to_string());
        }
        if ram.len() != self.bus.cart.ram.len() {
            return Err(format!(
                "save is {} bytes, cartridge RAM is {}",
                ram.len(),
                self.bus.cart.ram.len()
            ));
        }
        self.bus.cart.ram.copy_from_slice(ram);
        Ok(())
    }

    /// Latched RTC registers, `None` for cartridges without a clock.
    pub fn rtc(&self) -> Option<[u8; 5]> {
        self.bus.cart.rtc_regs()
    }

    pub fn set_rtc(&mut self, regs: [u8; 5]) {
        self.bus.cart.set_rtc_regs(regs);
    }

    /// Wall clock for the MBC3 RTC, milliseconds since the Unix epoch.
    pub fn tick_rtc(&mut self, now_ms: u64) {
        self.bus.cart.tick_rtc(now_ms);
    }

    /// Text a ROM has printed over the serial port since the last call, UTF-8 lossy.
    pub fn take_serial(&mut self) -> String {
        let bytes = std::mem::take(&mut self.bus.serial.out);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// True once per external-clock start, then clears.
    pub fn take_link_notice(&mut self) -> bool {
        let notice = self.bus.serial.link_notice;
        self.bus.serial.link_notice = false;
        notice
    }

    /// Plug a cable in, or pull it out with `None`. The front end owns the setting and
    /// the socket; the machine only swaps bytes through it.
    pub fn set_link(&mut self, link: Option<crate::link::Link>) {
        self.bus.serial.attach(link);
    }

    /// Where the cable stands: `Off` when none is plugged in.
    pub fn link_status(&self) -> crate::link::LinkStatus {
        match self.bus.serial.link.as_ref() {
            Some(link) => link.status(),
            None => crate::link::LinkStatus::Off,
        }
    }

    /// What the cable last had to say, once: the front end shows it and takes it.
    pub fn take_link_message(&mut self) -> Option<crate::serial::LinkMessage> {
        self.bus.serial.link_message.take()
    }

    /// The state's rows, in table order: the CPU, the bus, the PPU, the APU, the timer, the joypad,
    /// the serial port, the DMA unit, the cartridge, then `Machine`'s own counters. Written field by
    /// field from the devices' public fields, so no device's `save_state` method has to agree with
    /// this one. Fixed-size arrays go through `StateWriter::bytes`, which length-prefixes them
    /// exactly as it does a `Vec`.
    pub fn encode_state(&self) -> Vec<u8> {
        let mut w = StateWriter::new();
        for byte in STATE_MAGIC {
            w.u8(byte);
        }
        w.u16(STATE_VERSION);

        let cpu = &self.cpu;
        for value in [cpu.a, cpu.f.0, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l] {
            w.u8(value);
        }
        w.u16(cpu.sp);
        w.u16(cpu.pc);
        for flag in [
            cpu.ime,
            cpu.ime_pending,
            cpu.halted,
            cpu.halt_bug,
            cpu.stopped,
            cpu.locked,
        ] {
            w.bool(flag);
        }

        w.bytes(&self.bus.wram);
        w.bytes(&self.bus.hram);
        w.u8(self.bus.ie);
        w.u8(self.bus.iflag);
        w.u64(self.bus.cycles);
        w.bool(self.bus.boot_mapped);
        w.bool(self.bus.boot.is_some());
        if let Some(boot) = &self.bus.boot {
            w.bytes(boot);
        }

        let ppu = &self.bus.ppu;
        w.bytes(&ppu.vram);
        w.bytes(&ppu.vram1);
        w.bytes(&ppu.oam);
        w.bytes(ppu.frame());
        // A frame drawn in colour carries its RGB555 words; a DMG frame's are its shades, which
        // `frame` already holds (see `Ppu::save_state`).
        w.bool(ppu.cgb);
        if ppu.cgb {
            for word in &ppu.colours {
                w.u16(*word);
            }
        }
        w.bool(ppu.compat);
        ppu.palettes.save_state(&mut w);
        for value in [
            ppu.lcdc, ppu.stat, ppu.scy, ppu.scx, ppu.ly, ppu.lyc, ppu.bgp, ppu.obp0, ppu.obp1,
            ppu.wy, ppu.wx,
        ] {
            w.u8(value);
        }
        w.u32(ppu.dot);
        w.u32(ppu.lcd_off_dots);
        w.u8(ppu.mode as u8);
        w.u8(ppu.window_line);
        for flag in [
            ppu.stat_line,
            ppu.frame_ready,
            ppu.window_y,
            ppu.oam_dma_active,
            ppu.first_frame_blank,
        ] {
            w.bool(flag);
        }
        w.u64(ppu.frames);
        // The SGB palettes, the attribute map, `MASK_EN`'s state and the border, as
        // `Ppu::save_state` writes them: this list is that one's mirror.
        w.bool(ppu.sgb.enabled());
        ppu.sgb.palettes().save_state(&mut w);
        ppu.sgb.attr().save_state(&mut w);
        ppu.sgb.mask().save_state(&mut w);
        ppu.sgb.border().save_state(&mut w);

        let apu = &self.bus.apu;
        w.bytes(&apu.nr);
        w.bytes(&apu.wave);
        write_pulse(&mut w, &apu.ch1);
        write_pulse(&mut w, &apu.ch2);
        write_wave(&mut w, &apu.ch3);
        write_noise(&mut w, &apu.ch4);
        w.u8(apu.div_apu);
        w.u32(apu.sample_rate);
        w.f64(apu.acc);
        // The SGB's sound chip exactly as `Apu::save_state` writes it: whether there is one,
        // and then its 64 KiB of RAM, its SPC700, its DSP and the clock's remainder. The user's
        // cartridge image is not here: it belongs to the launch, not to the game.
        w.bool(apu.sgb.is_some());
        if let Some(sgb) = &apu.sgb {
            sgb.save_state(&mut w);
        }
        // The resampler's position between the chip's frames, which is `Apu::save_state`'s mirror.
        w.u32(apu.sgb_phase);
        for value in [
            apu.sgb_cur[0],
            apu.sgb_cur[1],
            apu.sgb_next[0],
            apu.sgb_next[1],
        ] {
            w.u32(value.to_bits());
        }

        let timer = &self.bus.timer;
        w.u16(timer.counter);
        w.u8(timer.tima);
        w.u8(timer.tma);
        w.u8(timer.tac);
        w.u8(timer.overflow);
        w.u8(timer.reload_value);

        w.u8(self.bus.joypad.pressed.0);
        w.u8(self.bus.joypad.select);
        w.u8(self.bus.joypad.last_low);

        w.u8(self.bus.serial.sb);
        w.u8(self.bus.serial.sc);
        w.u8(self.bus.serial.bits_left);
        w.u16(self.bus.serial.counter);

        w.u16(self.bus.dma.source);
        w.u8(self.bus.dma.index);
        w.u8(self.bus.dma.start_delay);
        w.bool(self.bus.dma.active);

        // The VRAM DMA unit: the registers, the transfer's mode and length, and the
        // block in flight, so a state saved mid-transfer resumes where it was.
        self.bus.vdma.save_state(&mut w);

        // The CGB's mode and speed. `hardware` is a fact of the cartridge, stored anyway
        // so a state can never flip a DMG machine into CGB mode.
        self.bus.cgb.save_state(&mut w);

        self.bus.cart.save_state(&mut w);

        w.u64(self.frames);
        w.u64(self.checksum);

        let body = w.finish();
        let sum = fnv1a64(&body[4..]);
        let mut out = body;
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// Reads §6's rows into a clone and swaps only on success, so a corrupt state leaves the running
    /// machine untouched. `last_frame_checksum` is derived and is reset to 0.
    pub fn decode_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        if bytes.len() < 4 + 2 + 8 {
            return Err(StateError::Truncated);
        }
        if bytes[..4] != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let body_end = bytes.len() - 8;
        let stored = u64::from_le_bytes(
            bytes[body_end..]
                .try_into()
                .map_err(|_| StateError::Truncated)?,
        );
        if fnv1a64(&bytes[4..body_end]) != stored {
            return Err(StateError::BadChecksum);
        }
        let mut r = StateReader::new(&bytes[4..body_end]);
        let version = r.u16()?;
        if version != STATE_VERSION {
            return Err(StateError::BadVersion(version));
        }
        let mut machine = self.clone();
        machine.read_state(&mut r)?;
        if r.remaining() != 0 {
            return Err(StateError::BadValue);
        }
        machine.last_frame_checksum = 0;
        machine.boot_done = machine.bus.boot.is_some() && !machine.bus.boot_mapped;
        machine.boot_cycles = 0;
        *self = machine;
        Ok(())
    }

    /// The payload reader half of `decode_state`, in the same order as `encode_state`.
    fn read_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let cpu = &mut self.cpu;
        cpu.a = r.u8()?;
        cpu.f = Flags(r.u8()?);
        cpu.b = r.u8()?;
        cpu.c = r.u8()?;
        cpu.d = r.u8()?;
        cpu.e = r.u8()?;
        cpu.h = r.u8()?;
        cpu.l = r.u8()?;
        cpu.sp = r.u16()?;
        cpu.pc = r.u16()?;
        cpu.ime = r.bool()?;
        cpu.ime_pending = r.bool()?;
        cpu.halted = r.bool()?;
        cpu.halt_bug = r.bool()?;
        cpu.stopped = r.bool()?;
        cpu.locked = r.bool()?;

        r.bytes_into(&mut self.bus.wram)?;
        r.bytes_into(&mut self.bus.hram)?;
        self.bus.ie = r.u8()?;
        self.bus.iflag = r.u8()?;
        self.bus.cycles = r.u64()?;
        self.bus.boot_mapped = r.bool()?;
        self.bus.boot = if r.bool()? {
            Some(r.bytes()?.into_boxed_slice())
        } else {
            None
        };

        let ppu = &mut self.bus.ppu;
        r.bytes_into(&mut ppu.vram)?;
        r.bytes_into(&mut ppu.vram1)?;
        r.bytes_into(&mut ppu.oam)?;
        r.bytes_into(&mut ppu.frame)?;
        ppu.cgb = r.bool()?;
        if ppu.cgb {
            for word in &mut ppu.colours {
                *word = r.u16()?;
            }
        }
        ppu.compat = r.bool()?;
        ppu.palettes.load_state(r)?;
        ppu.lcdc = r.u8()?;
        ppu.stat = r.u8()?;
        ppu.scy = r.u8()?;
        ppu.scx = r.u8()?;
        ppu.ly = r.u8()?;
        ppu.lyc = r.u8()?;
        ppu.bgp = r.u8()?;
        ppu.obp0 = r.u8()?;
        ppu.obp1 = r.u8()?;
        ppu.wy = r.u8()?;
        ppu.wx = r.u8()?;
        ppu.dot = r.u32()?;
        ppu.lcd_off_dots = r.u32()?;
        ppu.mode = match r.u8()? {
            0 => Mode::HBlank,
            1 => Mode::VBlank,
            2 => Mode::OamScan,
            3 => Mode::Drawing,
            _ => return Err(StateError::BadValue),
        };
        ppu.window_line = r.u8()?;
        ppu.stat_line = r.bool()?;
        ppu.frame_ready = r.bool()?;
        ppu.window_y = r.bool()?;
        ppu.oam_dma_active = r.bool()?;
        ppu.first_frame_blank = r.bool()?;
        ppu.frames = r.u64()?;
        ppu.sgb.set_enabled(r.bool()?);
        ppu.sgb.palettes_mut().load_state(r)?;
        ppu.sgb.attr_mut().load_state(r)?;
        ppu.sgb.mask_mut().load_state(r)?;
        ppu.sgb.border_mut().load_state(r)?;
        ppu.sgb.clear_transfer();
        ppu.clear_sgb_transfers();

        let apu = &mut self.bus.apu;
        r.bytes_into(&mut apu.nr)?;
        r.bytes_into(&mut apu.wave)?;
        read_pulse(r, &mut apu.ch1)?;
        read_pulse(r, &mut apu.ch2)?;
        read_wave(r, &mut apu.ch3)?;
        read_noise(r, &mut apu.ch4)?;
        apu.div_apu = r.u8()?;
        apu.sample_rate = r.u32()?;
        apu.acc = r.f64()?;
        apu.sgb = if r.bool()? {
            let mut sgb = crate::sgb::SgbSound::new();
            sgb.load_state(r)?;
            Some(Box::new(sgb))
        } else {
            None
        };
        apu.sgb_phase = r.u32()?;
        for sample in apu.sgb_cur.iter_mut() {
            *sample = f32::from_bits(r.u32()?);
        }
        for sample in apu.sgb_next.iter_mut() {
            *sample = f32::from_bits(r.u32()?);
        }

        let timer = &mut self.bus.timer;
        timer.counter = r.u16()?;
        timer.tima = r.u8()?;
        timer.tma = r.u8()?;
        timer.tac = r.u8()?;
        timer.overflow = r.u8()?;
        timer.reload_value = r.u8()?;

        self.bus.joypad.pressed = Buttons(r.u8()?);
        self.bus.joypad.select = r.u8()?;
        self.bus.joypad.last_low = r.u8()?;

        self.bus.serial.sb = r.u8()?;
        self.bus.serial.sc = r.u8()?;
        self.bus.serial.bits_left = r.u8()?;
        self.bus.serial.counter = r.u16()?;
        // A state never holds a transfer in flight or a cable: the four fields above are
        // the whole port, and the link stays whatever the front end has it set to.
        self.bus.serial.drop_transfer();

        self.bus.dma.source = r.u16()?;
        self.bus.dma.index = r.u8()?;
        self.bus.dma.start_delay = r.u8()?;
        self.bus.dma.active = r.bool()?;

        self.bus.vdma.load_state(r)?;

        self.bus.cgb.load_state(r)?;

        self.bus.cart.load_state(r)?;

        self.frames = r.u64()?;
        self.checksum = r.u64()?;
        Ok(())
    }
}

fn write_pulse(w: &mut StateWriter, c: &Pulse) {
    w.bool(c.enabled);
    w.bool(c.dac);
    w.u8(c.duty);
    w.u8(c.duty_pos);
    w.u16(c.period);
    w.u16(c.timer);
    w.u16(c.length);
    w.bool(c.length_enable);
    w.u8(c.vol);
    w.bool(c.env_dir_up);
    w.u8(c.env_period);
    w.u8(c.env_timer);
    w.u8(c.sweep_period);
    w.bool(c.sweep_dir_down);
    w.u8(c.sweep_shift);
    w.u8(c.sweep_timer);
    w.bool(c.sweep_enabled);
    w.u16(c.sweep_shadow);
}

fn read_pulse(r: &mut StateReader, c: &mut Pulse) -> Result<(), StateError> {
    c.enabled = r.bool()?;
    c.dac = r.bool()?;
    c.duty = r.u8()?;
    c.duty_pos = r.u8()?;
    c.period = r.u16()?;
    c.timer = r.u16()?;
    c.length = r.u16()?;
    c.length_enable = r.bool()?;
    c.vol = r.u8()?;
    c.env_dir_up = r.bool()?;
    c.env_period = r.u8()?;
    c.env_timer = r.u8()?;
    c.sweep_period = r.u8()?;
    c.sweep_dir_down = r.bool()?;
    c.sweep_shift = r.u8()?;
    c.sweep_timer = r.u8()?;
    c.sweep_enabled = r.bool()?;
    c.sweep_shadow = r.u16()?;
    Ok(())
}

fn write_wave(w: &mut StateWriter, c: &Wave) {
    w.bool(c.enabled);
    w.bool(c.dac);
    w.u16(c.period);
    w.u16(c.timer);
    w.u16(c.length);
    w.bool(c.length_enable);
    w.u8(c.volume_code);
    w.u8(c.pos);
}

fn read_wave(r: &mut StateReader, c: &mut Wave) -> Result<(), StateError> {
    c.enabled = r.bool()?;
    c.dac = r.bool()?;
    c.period = r.u16()?;
    c.timer = r.u16()?;
    c.length = r.u16()?;
    c.length_enable = r.bool()?;
    c.volume_code = r.u8()?;
    c.pos = r.u8()?;
    Ok(())
}

fn write_noise(w: &mut StateWriter, c: &Noise) {
    w.bool(c.enabled);
    w.bool(c.dac);
    w.u16(c.period);
    w.u16(c.timer);
    w.u16(c.length);
    w.bool(c.length_enable);
    w.u8(c.vol);
    w.bool(c.env_dir_up);
    w.u8(c.env_period);
    w.u8(c.env_timer);
    w.u8(c.clock_shift);
    w.bool(c.width_mode);
    w.u8(c.divisor_code);
    w.u16(c.lfsr);
}

fn read_noise(r: &mut StateReader, c: &mut Noise) -> Result<(), StateError> {
    c.enabled = r.bool()?;
    c.dac = r.bool()?;
    c.period = r.u16()?;
    c.timer = r.u16()?;
    c.length = r.u16()?;
    c.length_enable = r.bool()?;
    c.vol = r.u8()?;
    c.env_dir_up = r.bool()?;
    c.env_period = r.u8()?;
    c.env_timer = r.u8()?;
    c.clock_shift = r.u8()?;
    c.width_mode = r.bool()?;
    c.divisor_code = r.u8()?;
    c.lfsr = r.u16()?;
    Ok(())
}
