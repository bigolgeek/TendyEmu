//! The 64 KiB address decode and the one `tick()` that fans out to every device.
//!
//! The decode is real; every device it calls is a zero-value stub in the scaffold.

use crate::apu::Apu;
use crate::cart::Cartridge;
use crate::cgb::Cgb;
use crate::cheats::CheatBook;
use crate::joypad::Joypad;
use crate::machine::BOOT_ROM_SIZE;
use crate::ppu::Mode;
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::state::{StateError, StateReader, StateWriter};
use crate::timer::Timer;
use crate::vdma::VramDma;
use crate::{HRAM_SIZE, OAM_SIZE, WRAM_BANK_SIZE, WRAM_BYTES};

/// VBlank interrupt bit.
pub const IF_VBLANK: u8 = 0x01;
/// LCD STAT interrupt bit.
pub const IF_STAT: u8 = 0x02;
/// Timer interrupt bit.
pub const IF_TIMER: u8 = 0x04;
/// Serial interrupt bit.
pub const IF_SERIAL: u8 = 0x08;
/// Joypad interrupt bit.
pub const IF_JOYPAD: u8 = 0x10;

/// One OAM DMA transfer: 160 bytes from `$XX00` to `$FE00`.
#[derive(Clone)]
pub struct Dma {
    /// Value written to `$FF46` << 8.
    pub source: u16,
    /// 0..160 bytes copied.
    pub index: u8,
    /// M-cycles left before the first byte moves, in the low seven bits. Bit 7 says the bus is
    /// already owned: a fresh transfer sets it when its delay expires, and a restart that lands on
    /// a running transfer carries it across, so OAM never becomes visible between them.
    pub start_delay: u8,
    pub active: bool,
}

/// Bit 7 of `Dma::start_delay`: the transfer owns the bus.
const DMA_HELD: u8 = 0x80;

impl Dma {
    pub fn new() -> Dma {
        Dma {
            // Post-boot `$FF46` reads `$FF`; nothing is copied until a write.
            source: 0xFF00,
            index: 0,
            start_delay: 0,
            active: false,
        }
    }

    pub fn start(&mut self, value: u8) {
        self.source = (value as u16) << 8;
        self.index = 0;
        // `oam_dma_start.s`, "Expected timing": the write cycle is M = 0, M = 1 is an idle cycle in
        // which OAM is still accessible, and the transfer blocks from M = 2. Its restart case
        // (`M = 1: previous DMA is running (OAM *not* accessible)`, then `M = 2: new DMA starts`)
        // means a write that lands on a running transfer never un-blocks OAM, hence the carried bit.
        let held = if self.active { DMA_HELD } else { 0 };
        self.start_delay = 2 | held;
        self.active = true;
    }

    /// True from the first blocked M-cycle to the last: `oam_dma_start.s` puts the first one two
    /// M-cycles after the write, and the transfer is 160 bytes at one byte per M-cycle.
    pub fn blocks(&self) -> bool {
        self.active && self.start_delay & DMA_HELD != 0
    }

    /// M-cycles left in the start window, without the "owns the bus" bit.
    pub fn remaining_delay(&self) -> u8 {
        self.start_delay & !DMA_HELD
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.source);
        w.u8(self.index);
        w.u8(self.start_delay);
        w.bool(self.active);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.source = r.u16()?;
        self.index = r.u8()?;
        self.start_delay = r.u8()?;
        self.active = r.bool()?;
        Ok(())
    }
}

impl Default for Dma {
    fn default() -> Self {
        Dma::new()
    }
}

/// The whole machine's address space.
#[derive(Clone)]
pub struct Bus {
    pub cart: Cartridge,
    pub ppu: Ppu,
    pub apu: Apu,
    pub timer: Timer,
    pub joypad: Joypad,
    /// The SGB packet receiver on `$FF00` bits 4-5. Inert unless the PPU holds an
    /// enabled [`crate::sgb::Sgb`]; it is session state, not save-state.
    pub sgb_rx: crate::sgb::Responder,
    pub serial: Serial,
    pub dma: Dma,
    /// The CGB's VRAM DMA (`$FF51-$FF55`): general-purpose and HBlank transfers into either VRAM
    /// bank without the CPU. Inert on a DMG cartridge.
    pub vdma: VramDma,
    /// True while the CPU is executing `HALT`'s idle cycles. Written by the CPU on every step so
    /// the VRAM DMA unit can hold its HBlank blocks: Pan Docs, *CGB Registers*, "Upon halting the
    /// CPU (using the halt instruction), the transfer will also be halted".
    pub cpu_halted: bool,
    /// The Game Boy Color's mode registers and its double-speed state. Inert on a DMG
    /// cartridge: the four registers read `$FF`, writes are dropped and the clock never changes.
    pub cgb: Cgb,
    /// All 32 KiB of work RAM: eight 4 KiB banks, bank `n` at `n * WRAM_BANK_SIZE`.
    ///
    /// A DMG cartridge sees the first 8 KiB of it through `$C000-$DFFF` — banks 0 and 1 laid end to
    /// end, which is the one contiguous array this field used to be. A CGB cartridge sees bank 0 at
    /// `$C000-$CFFF` always, and the bank `SVBK` names at `$D000-$DFFF`; see [`Bus::wram_index`].
    pub wram: [u8; WRAM_BYTES],
    pub hram: [u8; HRAM_SIZE],
    pub ie: u8,
    /// Only bits 0-4 exist; writes through `$FF0F` see `$E0` set.
    pub iflag: u8,
    pub boot: Option<Box<[u8]>>,
    /// False once `$FF50` is written or the boot ROM finishes.
    pub boot_mapped: bool,
    /// The enabled cheat codes of the cartridge in `cart`. Empty for a game with no
    /// codes on, which is the state the read path is tuned for.
    pub cheats: CheatBook,
    /// Machine cycles since reset.
    pub cycles: u64,
}

impl Bus {
    pub fn new(cart: Cartridge, console_cgb: bool) -> Bus {
        // `$0143` bit 7 says the cartridge is a CGB (`$80` enhanced, `$C0` CGB-only). `console_cgb`
        // says the console is one: a CGB cartridge runs on a CGB console, and a monochrome
        // cartridge is a CGB console when `Machine::new_cgb` asked for one, so the compatibility
        // palette applies (Pan Docs, "Unlocking CGB functions", "Compatibility palettes").
        let cgb = Cgb::new(console_cgb, cart.header.cgb_flag & 0x80 != 0);
        Bus {
            cart,
            ppu: Ppu::new(),
            apu: Apu::new(),
            timer: Timer::new(),
            joypad: Joypad::new(),
            sgb_rx: crate::sgb::Responder::new(),
            serial: Serial::new(),
            dma: Dma::new(),
            vdma: VramDma::new(),
            cpu_halted: false,
            cgb,
            wram: [0; WRAM_BYTES],
            hram: [0; HRAM_SIZE],
            ie: 0,
            iflag: IF_VBLANK,
            boot: None,
            boot_mapped: false,
            cheats: CheatBook::new(),
            cycles: 0,
        }
    }

    /// The value at `addr` without spending a machine cycle — for a debugger or a test that must
    /// look at memory without perturbing the timing under test. Never call this on a machine
    /// cycle's path: `read` is the one the CPU uses.
    pub fn peek(&mut self, addr: u16) -> u8 {
        self.read_unblocked(addr)
    }

    /// The byte as stored, for the debugger's viewers: no machine cycle, and none of the
    /// masks a cycle's access wears. `$8000-$9FFF` answers while the PPU is in mode 3, `$FE00`
    /// answers during an OAM DMA, and channel 3's wave RAM answers while it owns it — the point of
    /// a viewer is what the machine holds, not what a CPU fetch at this instant would be refused.
    ///
    /// A register still reads through its own rules (a write-only one is `$FF`), `DIV` is the
    /// counter's high byte, and the cheat book still applies: a code replaces the byte the game
    /// reads, and the memory view shows what the game reads. Work RAM answers the bank `SVBK` has
    /// selected at `$D000`, the byte the CPU would reach, not a flattened 8 KiB.
    pub fn peek_stored(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x8000..=0x9FFF => self.ppu.vram_banked(self.cgb.vram_bank())[(addr as usize) & 0x1FFF],
            0xFE00..=0xFE9F => self.ppu.oam[(addr as usize - 0xFE00) % OAM_SIZE],
            0xFF30..=0xFF3F => self.apu.wave[(addr - 0xFF30) as usize],
            _ => return self.read_unblocked(addr),
        };
        if self.cheats.is_empty() {
            value
        } else {
            self.cheats.read(addr, value)
        }
    }

    /// Serves the access, then ticks once.
    ///
    /// While an OAM DMA transfer owns the bus it owns OAM: reads of `$FE00-$FE9F` return `$FF` and
    /// writes to them are dropped. Everything else still answers — see [`Bus::dma_blocked`].
    pub fn read(&mut self, addr: u16) -> u8 {
        if self.dma_blocked(addr) {
            self.tick();
            return 0xFF;
        }
        let value = self.read_unblocked(addr);
        self.tick();
        value
    }

    /// Serves the write, then ticks once.
    pub fn write(&mut self, addr: u16, value: u8) {
        if self.dma_blocked(addr) {
            self.tick();
            return;
        }
        self.write_unblocked(addr, value);
        self.tick();
    }

    /// True when an OAM DMA transfer is between the CPU and this address.
    ///
    /// Only OAM is blocked. `Pan_Docs/OAM_DMA` says the CPU "can access only HRAM" during a DMG
    /// transfer, but the hardware-verified timing ROMs cannot pass under that rule and this one is
    /// what they measure:
    ///
    /// - `oam_dma_start.s` jumps to `$FDFF` and expects `$FE00`'s `INC B` to run at M = 1 and
    ///   `$FE01` to read `$FF` at M = 2, then lands on the `$0038` vector and runs
    ///   `wait_dma_finish` — 159 M-cycles of ROM code — before the transfer ends. A blocked ROM
    ///   (or WRAM) would turn that fetch into `RST $38` and the ROM would loop forever.
    /// - `jp_timing.s`/`call_timing.s` execute `jp`/`call` from `$FDFE` (WRAM echo) inside the
    ///   transfer and only the `$FE00` operand read is expected to come back `$FF`.
    /// - `oam_dma/reg_read.s` reads `$FF46` two cycles into a transfer and expects the value
    ///   written, and `oam_dma_restart.s` writes `$FF46` mid-transfer.
    fn dma_blocked(&self, addr: u16) -> bool {
        self.dma.blocks() && (0xFE00..=0xFE9F).contains(&addr)
    }

    /// One machine cycle. Fans out to every device; IF is updated in place.
    ///
    /// **Two speeds (CGB).** In double speed the CPU runs at 8.388 MHz while the LCD and the sound
    /// chip keep their real-time rates (Pan Docs, *FF4D — KEY1/SPD*), so a machine cycle is half as
    /// long: the PPU and the APU advance **2 dots** instead of 4. The timer still takes its 4
    /// T-cycles per machine cycle — it is clocked by the CPU, so it runs twice as fast in wall time
    /// too, as Pan Docs says — and the serial bit clock and OAM DMA stay at one machine cycle per
    /// bit and per byte, which doubles those as well. The APU's 512 Hz frame sequencer takes one
    /// `DIV` edge in two (`Cgb::apu_edge`) because `DIV` doubled and the sound timings did not.
    ///
    /// **The STOP pause.** While a speed switch is in flight the CPU is held, `DIV` does not tick
    /// (Pan Docs), and everything else — LCD, APU, serial — keeps running for the 8 200 T-cycles.
    pub fn tick(&mut self) {
        let dots: u32 = if self.cgb.double_speed() { 2 } else { 4 };
        let paused = self.cgb.switching();
        let mut bits = 0;
        if !paused {
            bits = self.timer.tick();
            if self.timer.take_div_apu_edge() && self.cgb.apu_edge() {
                self.apu.div_apu_event();
            }
        }
        self.apu.tick(dots);
        // The PPU draws in colour exactly while this is a CGB console (a CGB cartridge in CGB
        // mode, or a monochrome cartridge through the compatibility palette), and with the
        // object-priority rule `OPRI` names. `compat` splits the two colour paths in the renderer:
        // a CGB cartridge indexes the attribute map's palette, a monochrome one indexes
        // `BGP`/`OBP0`/`OBP1` into the compatibility triplet. Both are read here, per machine
        // cycle, so a `KEY0` or `OPRI` write takes effect on the next line the PPU renders.
        self.ppu.cgb = self.cgb.is_cgb() || self.cgb.compat_mode();
        self.ppu.compat = self.cgb.compat_mode();
        self.ppu.opri_dmg = self.cgb.opri & crate::cgb::OPRI_DMG != 0;
        let mode_before = self.ppu.mode;
        bits |= self.ppu.tick(dots);
        self.tick_vdma();
        // A block begins as the PPU enters mode 0: "The HBlank DMA transfers $10 bytes of data
        // during each HBlank, that is, at LY=0-143" (Pan Docs). The edge, not the level, is what
        // starts it, so a transfer never takes a second block in the same HBlank.
        if mode_before != Mode::HBlank
            && self.ppu.mode == Mode::HBlank
            && self.ppu.ly < crate::VBLANK_LINE
            && self.ppu.lcdc & 0x80 != 0
            && self.vdma.active
            && self.vdma.hblank
            && !self.vdma.halting()
            && !self.cpu_halted
        {
            self.vdma.begin_block();
        }
        bits |= self.serial.tick();
        self.tick_dma();
        if paused {
            self.cgb.pause_tick(dots as u16);
        }
        self.iflag |= bits & 0x1F;
        self.cycles = self.cycles.wrapping_add(1);
    }

    /// `STOP` (`$10`). On a CGB with `KEY1` armed this begins the double-speed switch and the CPU
    /// waits out the pause; otherwise the caller enters the ordinary low-power stop.
    ///
    /// A serial transfer in flight blocks the switch (and an HDMA), because Pan Docs
    /// lists both as running through the pause and the switch is only specified for an idle bus.
    pub fn stop(&mut self) -> crate::cgb::Stop {
        let blocked = self.serial.transferring() || self.vdma.active;
        self.cgb.stop(blocked)
    }

    pub fn request(&mut self, bits: u8) {
        self.iflag |= bits & 0x1F;
    }

    /// The pending interrupt bits: `IE & IF & $1F`.
    pub fn pending(&self) -> u8 {
        self.ie & self.iflag & 0x1F
    }

    /// The work RAM bank mapped at `$D000-$DFFF`: `SVBK`'s low three bits on a CGB cartridge (with
    /// 0 meaning bank 1), and bank 1 on a DMG cartridge, whose `$C000-$DFFF` is one contiguous
    /// 8 KiB (Pan Docs, *Memory Map*: `$D000-$DFFF` is "switchable bank 1–7" in CGB mode only).
    ///
    /// The gate is [`Cgb::hardware`], the same one the register file uses: a CGB in DMG
    /// compatibility mode still has 32 KiB of work RAM and still answers `SVBK`, so the banking
    /// follows the cartridge's `$0143` bit 7 and not `KEY0` bit 2.
    fn wram_bank_at_d000(&self) -> u8 {
        if self.cgb.hardware {
            self.cgb.wram_bank()
        } else {
            1
        }
    }

    /// The index into [`Bus::wram`] of the byte `addr` names inside `$C000-$DFFF` or its echo
    /// `$E000-$FDFF`.
    ///
    /// Pan Docs, *Memory Map*: `$C000-$CFFF` is work RAM bank 0, `$D000-$DFFF` is the switchable
    /// bank, and `$E000-$FDFF` is "Echo RAM (mirror of `$C000-$DDFF`)" — so the echo is folded down
    /// onto the two halves it mirrors and then takes the same bank. Folding covers `$E000-$EFFF`
    /// onto bank 0 and `$F000-$FDFF` onto whatever bank `$D000` has, exactly as the mirror says.
    fn wram_index(&self, addr: u16) -> usize {
        let addr = if addr >= 0xE000 { addr - 0x2000 } else { addr };
        let (bank, offset) = if addr < 0xD000 {
            (0, addr - 0xC000)
        } else {
            (self.wram_bank_at_d000(), addr - 0xD000)
        };
        bank as usize * WRAM_BANK_SIZE + offset as usize
    }

    /// The boot ROM byte the CPU sees at `addr`, or `None` when `addr` is outside the ROM's
    /// windows (which, for the colour page, includes the cartridge header gap at `$0100-$01FF`).
    ///
    /// A 256-byte image is the DMG page, mapped at `$0000-$00FF`; a 2048-byte image is the CGB
    /// page, mapped at `$0000-$00FF` and `$0200-$08FF` (Pan Docs, *Power-Up Sequence*, "Color
    /// models"). One path serves either shape: the image's length is the shape.
    fn boot_byte(&self, addr: u16) -> Option<u8> {
        if !self.boot_mapped {
            return None;
        }
        let boot = self.boot.as_deref()?;
        match boot.len() {
            BOOT_ROM_SIZE => (addr < 0x100).then(|| boot[addr as usize]),
            crate::machine::CGB_BOOT_ROM_SIZE => {
                if addr < 0x100 {
                    Some(boot[addr as usize])
                } else if (0x200..0x900).contains(&addr) {
                    Some(boot[(addr - 0x100) as usize])
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// The decode without the OAM-DMA block, so the DMA unit's own fetch is not blocked by the
    /// block it causes.
    ///
    /// The cheat book has the last word: a code replaces the byte the game reads at its address,
    /// exactly as the adapter would drive the bus. An empty book costs one length check
    /// and the value goes through untouched — no allocation, no branch per code. The boot ROM's
    /// own bytes are not patched: the adapter sits between the cartridge and the console.
    fn read_unblocked(&mut self, addr: u16) -> u8 {
        if let Some(byte) = self.boot_byte(addr) {
            return byte;
        }
        let value = match addr {
            0x0000..=0x7FFF | 0xA000..=0xBFFF => self.cart.read(addr),
            0x8000..=0x9FFF => self.ppu.read_vram(addr, self.cgb.vram_bank()),
            0xC000..=0xFDFF => {
                let index = self.wram_index(addr);
                self.wram[index]
            }
            // OAM is addressed by its index, not by its absolute address: $FE00 is byte 0.
            0xFE00..=0xFE9F => self.ppu.read_oam(addr - 0xFE00),
            0xFEA0..=0xFEFF => 0x00,
            0xFF00..=0xFF7F => self.read_io(addr),
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            // IE is a full eight-bit register: its three unused bits store and read back whatever
            // was written, unlike every other `$FFxx` register, whose unused bits read as ones
            // (`bits/unused_hwio-GS` breaks the rule deliberately: `test IE %11100000 %11100000
            // %11100000` writes `$E0` and expects `$E0` back, while `IF` with the same mask
            // expects `$E0` for a written `$00`).
            0xFFFF => self.ie,
        };
        if self.cheats.is_empty() {
            value
        } else {
            self.cheats.read(addr, value)
        }
    }

    fn write_unblocked(&mut self, addr: u16, value: u8) {
        if self.boot_byte(addr).is_some() {
            return;
        }
        match addr {
            0x0000..=0x7FFF | 0xA000..=0xBFFF => self.cart.write(addr, value),
            0x8000..=0x9FFF => self.ppu.write_vram(addr, self.cgb.vram_bank(), value),
            0xC000..=0xFDFF => {
                let index = self.wram_index(addr);
                self.wram[index] = value;
            }
            0xFE00..=0xFE9F => self.ppu.write_oam(addr - 0xFE00, value),
            0xFEA0..=0xFEFF => {}
            0xFF00..=0xFF7F => self.write_io(addr, value),
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = value,
            0xFFFF => self.ie = value,
        }
    }

    fn read_io(&mut self, addr: u16) -> u8 {
        match addr {
            0xFF00 => {
                let value = self.joypad.read();
                if self.ppu.sgb.enabled() {
                    self.sgb_rx.read_joyp(value)
                } else {
                    value
                }
            }
            0xFF01 | 0xFF02 => self.serial.read(addr),
            0xFF04..=0xFF07 => self.timer.read(addr),
            0xFF0F => 0xE0 | self.iflag,
            0xFF10..=0xFF3F => self.apu.read(addr),
            0xFF46 => (self.dma.source >> 8) as u8,
            0xFF40..=0xFF4B => self.ppu.read(addr),
            // The four CGB mode registers read `$FF` on a DMG cartridge and through their own masks
            // on a CGB (Pan Docs, *CGB Registers*), and `OPRI` with them.
            0xFF4C | 0xFF4D | 0xFF4F | 0xFF6C | 0xFF70 => self.cgb.read(addr),
            // The CGB's palette RAM: two index/data pairs on CGB hardware, `$FF` on a DMG.
            0xFF68..=0xFF6B => {
                if self.cgb.hardware {
                    self.ppu.read_palette(addr)
                } else {
                    0xFF
                }
            }
            0xFF50 => 0xFF,
            // The CGB's VRAM DMA registers (`$FF51-$FF55`): the four address registers are
            // write-only, `$FF55` reports the transfer. `$FF` on a DMG cartridge.
            0xFF51..=0xFF55 => {
                if self.cgb.hardware {
                    self.vdma.read(addr)
                } else {
                    0xFF
                }
            }
            0xFF03 | 0xFF08..=0xFF0E | 0xFF4E | 0xFF56..=0xFF7F => 0xFF,
            _ => 0xFF,
        }
    }

    fn write_io(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF00 => {
                let bits = self.joypad.write(value);
                self.iflag |= bits & 0x1F;
                // Pan Docs, *Command Packet Transfers*: every SGB packet arrives as writes to
                // `JOYP` bits 4-5, so this is the one place the receiver sees them.
                if self.ppu.sgb.enabled()
                    && let Some(packet) = self.sgb_rx.write_joyp(value)
                    && !self.sgb_rx.deliver(&packet)
                {
                    self.ppu.apply_sgb_packet(&packet);
                }
            }
            0xFF01 | 0xFF02 => {
                let bits = self.serial.write(addr, value);
                self.iflag |= bits & 0x1F;
            }
            0xFF04..=0xFF07 => {
                let bits = self.timer.write(addr, value);
                self.iflag |= bits & 0x1F;
            }
            0xFF0F => self.iflag = value & 0x1F,
            0xFF10..=0xFF3F => self.apu.write(addr, value),
            0xFF46 => self.dma.start(value),
            0xFF40..=0xFF4B => {
                let bits = self.ppu.write(addr, value);
                self.iflag |= bits & 0x1F;
            }
            0xFF4C | 0xFF4D | 0xFF4F | 0xFF6C | 0xFF70 => self.cgb.write(addr, value),
            // Palette RAM: a DMG cartridge's writes are dropped (Pan Docs, *Palettes*).
            0xFF68..=0xFF6B => {
                if self.cgb.hardware {
                    self.ppu.write_palette(addr, value);
                }
            }
            0xFF50 => {
                // The boot ROM unmaps itself; the bytes stay in `boot` so a reset can map them
                // again (§3.2). `boot_mapped` is the only switch.
                self.boot_mapped = false;
            }
            // VRAM DMA: the four address registers, then `$FF55` starting, stopping or completing
            // a transfer. A DMG cartridge has none of them, so its writes are dropped.
            0xFF51..=0xFF55 if self.cgb.hardware => self.write_vdma(addr, value),
            _ => {}
        }
    }

    /// `$FF51-$FF55` (`Pan Docs`, *CGB Registers*, "LCD VRAM DMA Transfers").
    fn write_vdma(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF55 => self.start_vdma(value),
            _ => self.vdma.write_register(addr, value),
        }
    }

    /// A `$FF55` write: the three things Pan Docs gives it.
    fn start_vdma(&mut self, value: u8) {
        if self.vdma.active {
            // A general-purpose transfer owns the CPU until its last block, so no program can
            // reach this; anything that does (a debugger poke, or hardware this machine cannot
            // model) is ignored rather than allowed to corrupt the transfer in flight.
            if !self.vdma.hblank {
                return;
            }
            if value & 0x80 == 0 {
                // "It is also possible to terminate an active HBlank transfer by writing zero to
                // Bit 7 of FF55." The block in flight is abandoned with the rest.
                self.vdma.cancel();
            }
            // A bit-7-set write on a running transfer is not specified; Pan Docs' `$FF55` only
            // starts a transfer, so it is ignored and the one in flight keeps its length.
            return;
        }
        self.vdma.start(value);
        if self.vdma.hblank && !self.lcd_on() {
            // With the LCD off there are no HBlanks to wait for, and hardware runs the whole
            // transfer at once; this is the same path a general-purpose transfer takes.
            self.vdma.hblank = false;
        }
        if !self.vdma.hblank || self.ppu.mode == Mode::HBlank {
            // General purpose: the data moves now. HBlank mode started while the PPU is already in
            // mode 0 takes its first block at once too (hardware does; Pan Docs' warning is only
            // that a cartridge should not start one there).
            self.vdma.begin_block();
        }
    }

    /// True while the LCD is on and drawing (`LCDC` bit 7).
    fn lcd_on(&self) -> bool {
        self.ppu.lcdc & 0x80 != 0
    }

    /// One machine cycle of a VRAM DMA block: 2 bytes at normal speed, 1 at double speed, so a
    /// block takes 8 µs either way (Pan Docs, "Transfer Timings": "8 M-cycles in Normal Speed Mode,
    /// and 16 'fast' M-cycles in Double Speed Mode").
    fn tick_vdma(&mut self) {
        let mut budget: u32 = if self.cgb.double_speed() { 1 } else { 2 };
        while budget > 0 && self.vdma.in_block > 0 {
            let value = self.vdma_source_read(self.vdma.source);
            // A VRAM DMA writes VRAM past the PPU's mode-3 lock: Pan Docs, "the General Purpose DMA
            // blindly attempts to copy the data, even if the LCD controller is currently accessing
            // VRAM". The battery's `VBK` bit picks the bank, read per byte as hardware does.
            self.ppu
                .write_vram_dma(self.vdma.dest, self.cgb.vram_bank(), value);
            self.vdma.source = self.vdma.source.wrapping_add(1);
            self.vdma.dest = self.vdma.dest.wrapping_add(1);
            self.vdma.in_block -= 1;
            budget -= 1;
            if self.vdma.in_block == 0 {
                self.vdma.end_block();
            }
        }
    }

    /// A VRAM DMA's read of its source. Pan Docs names ROM, SRAM and WRAM as the sensible source
    /// ("either in range 0000-7FF0 or A000-DFF0") and says a source in VRAM "will cause garbage to
    /// be copied"; every range outside those reads `$FF`, which is this machine's garbage.
    fn vdma_source_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF | 0xA000..=0xBFFF => self.cart.read(addr),
            0xC000..=0xFDFF => {
                // A source past `$DFFF` is read through its WRAM echo, bank for bank: the address
                // space does not change under a transfer.
                let index = self.wram_index(addr);
                self.wram[index]
            }
            _ => 0xFF,
        }
    }

    /// One OAM byte per M-cycle, after the two idle cycles the write costs; `Ppu::oam_dma_active`
    /// mirrors `Dma::blocks`, so the PPU knows when the bus is not its to drive.
    fn tick_dma(&mut self) {
        if self.dma.active {
            let remaining = self.dma.remaining_delay();
            if remaining > 0 {
                // The delay runs down even when the bus is already owned (a restart), and reaching
                // zero is what starts the block.
                self.dma.start_delay = if remaining == 1 {
                    DMA_HELD
                } else {
                    (self.dma.start_delay & DMA_HELD) | (remaining - 1)
                };
            } else {
                // A source at or past $E000 is read through its WRAM echo: `oam_dma/sources-GS`
                // starts transfers from $E000, $FE00 and $FF00 and expects the bytes at $C000,
                // $DE00 and $DF00 ("including the area past $DFFF"). Below $E000 the address is
                // used as-is, so clearing bit 13 wholesale would break $A000/$BF00.
                let mut addr = self.dma.source | self.dma.index as u16;
                if addr >= 0xE000 {
                    addr -= 0x2000;
                }
                let value = self.read_unblocked(addr);
                self.ppu.oam_dma_write(self.dma.index as usize, value);
                self.dma.index = self.dma.index.wrapping_add(1);
                if self.dma.index >= crate::OAM_SIZE as u8 {
                    self.dma.active = false;
                }
            }
        }
        self.ppu.oam_dma_active = self.dma.blocks();
    }
}
