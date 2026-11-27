//! MBC6 (type byte `$20`): 1 MiB of ROM and 1 MiB of flash addressed in 8 KiB banks, two switchable
//! 8 KiB windows at `$4000` and `$6000`, 8 x 4 KiB of cartridge RAM, and the flash command set (Pan
//! Docs `MBC6.html`).
//!
//! Pan Docs gives the register map:
//!
//! | address | register |
//! |---|---|
//! | `$0000-$03FF` | RAM enable (`$A` in the low nibble) |
//! | `$0400-$07FF` | RAM bank A (0-7, 4 KiB each) |
//! | `$0800-$0BFF` | RAM bank B |
//! | `$0C00-$0FFF` | flash enable, bit 0 |
//! | `$1000` | flash write enable, bit 0 |
//! | `$2000-$27FF` | bank A number (8 KiB) |
//! | `$2800-$2FFF` | bank A select: `$00` ROM, `$08` flash |
//! | `$3000-$37FF` | bank B number |
//! | `$3800-$3FFF` | bank B select |
//!
//! `$0000-$3FFF` is always ROM banks 0 and 1, so the board only ever *reads* flash through the two
//! upper windows: that is what [`Mbc::read_rom`] is for.
//!
//! UNVERIFIED:
//!
//! * the flash write-enable register is decoded as the `$1000-$13FF` block, because every other
//!   register in the map is a `$400` block and Pan Docs prints a single address for this one;
//! * the bank *select* registers are read as bit 3 (`$08` = flash, anything else = ROM), which is
//!   what the two documented values imply;
//! * programming ANDs the buffered bytes into the erased cell, the "it will only allow the stored
//!   bytes to be anded instead of replaced" note on the flash page, and a page is committed when its
//!   last byte is written, when the address leaves the page, or on `$F0`;
//! * the chip reports itself ready at once: there is no timing model, so the status byte a real part
//!   would return while busy never appears;
//! * the 1 MiB of flash and the 256-byte hidden region are persisted in the `.sav` as the board's
//!   own bytes, but are *not* part of the emulator state, like the ROM itself.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// Bytes of cartridge RAM: eight 4 KiB banks, the SRAM the board carries.
pub const RAM_SIZE: usize = 0x8000;
/// Bytes of flash, "1MB of flash ROM" on the `MBC6.html` page.
pub const FLASH_SIZE: usize = 0x10_0000;
/// The separate 256-byte hidden region at the end of the flash.
pub const HIDDEN_SIZE: usize = 0x100;
/// A flash sector is 128 KiB; the chip has eight of them and sector 0 is protected at power-on.
const SECTOR_SIZE: usize = 0x2_0000;
/// The documented unlock addresses, as addresses inside the 1 MiB flash.
const UNLOCK_1: usize = 0x5555;
const UNLOCK_2: usize = 0x2AAA;

/// Which interface the flash is presenting (`MBC6.html`, "Flash Commands").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Read,
    Id,
    Program,
    HiddenRead,
    HiddenProgram,
}

/// The flash chip: 1 MiB of cells plus the 256-byte hidden region, in one buffer so a `.sav` is one
/// slice.
#[derive(Clone)]
struct Flash {
    /// `FLASH_SIZE` bytes of flash, then `HIDDEN_SIZE` bytes of hidden region.
    bytes: Vec<u8>,
    /// Unlock progress: 0 idle, 1 `$AA`, 2 `$55`, 3-5 erase, 6-7 hidden read, 8-9 protection.
    step: u8,
    mode: Mode,
    /// Sector 0's protection bit: "this sector cannot be erased ... until the protection is
    /// removed", set out of the box.
    sector0_locked: bool,
    /// The 128-byte program page being filled, and which of its bytes have been written.
    page: usize,
    buffer: [u8; 128],
    written: [u8; 128],
}

impl Flash {
    fn new() -> Flash {
        Flash {
            // Erased flash reads $FF.
            bytes: vec![0xFF; FLASH_SIZE + HIDDEN_SIZE],
            step: 0,
            mode: Mode::Read,
            sector0_locked: true,
            page: 0,
            buffer: [0; 128],
            written: [0; 128],
        }
    }

    fn read(&self, addr: usize) -> u8 {
        match self.mode {
            // "The manufacturer ID is read at the start of the flash, and the device ID after it."
            Mode::Id => {
                if addr & 1 == 0 {
                    0xC2
                } else {
                    0x81
                }
            }
            Mode::HiddenRead | Mode::HiddenProgram => self.bytes[FLASH_SIZE + (addr & 0xFF)],
            _ => self.bytes[addr % FLASH_SIZE],
        }
    }

    fn write(&mut self, addr: usize, value: u8) {
        let addr = addr % FLASH_SIZE;
        if value == 0xF0 {
            self.commit();
            self.mode = Mode::Read;
            self.step = 0;
            return;
        }
        match self.mode {
            Mode::Program => return self.program(addr, value),
            Mode::HiddenProgram => {
                let i = FLASH_SIZE + (addr & 0xFF);
                self.bytes[i] &= value;
                return;
            }
            Mode::Id => return,
            _ => {}
        }
        match self.step {
            0 => {
                if addr == UNLOCK_1 && value == 0xAA {
                    self.step = 1;
                }
            }
            1 => {
                self.step = if addr == UNLOCK_2 && value == 0x55 {
                    2
                } else {
                    0
                };
            }
            2 => {
                self.step = 0;
                match (addr, value) {
                    (UNLOCK_1, 0x80) => self.step = 3,
                    (UNLOCK_1, 0x90) => self.mode = Mode::Id,
                    (UNLOCK_1, 0xA0) => {
                        self.mode = Mode::Program;
                        self.page = addr & !0x7F;
                        self.buffer = [0; 128];
                        self.written = [0; 128];
                    }
                    (UNLOCK_1, 0x77) => self.step = 6,
                    (UNLOCK_1, 0x60) => self.step = 8,
                    _ => {}
                }
            }
            3 => {
                self.step = if addr == UNLOCK_1 && value == 0xAA {
                    4
                } else {
                    0
                }
            }
            4 => {
                self.step = if addr == UNLOCK_2 && value == 0x55 {
                    5
                } else {
                    0
                }
            }
            5 => {
                self.step = 0;
                match value {
                    0x30 => self.erase_sector(addr),
                    0x10 => self.erase_chip(),
                    _ => {}
                }
            }
            6 => {
                self.step = match (addr, value) {
                    // The second unlock cycle starts the sequence over, not ends it.
                    (UNLOCK_1, 0xAA) => 6,
                    (UNLOCK_2, 0x55) => 7,
                    _ => 0,
                }
            }
            7 => {
                self.step = 0;
                if value == 0x77 {
                    self.mode = Mode::HiddenRead;
                }
            }
            8 => {
                self.step = match (addr, value) {
                    (UNLOCK_1, 0xAA) => 8,
                    (UNLOCK_2, 0x55) => 9,
                    _ => 0,
                }
            }
            9 => {
                self.step = 0;
                match value {
                    0xE0 => self.mode = Mode::HiddenProgram,
                    0x40 => self.sector0_locked = false,
                    0x20 => self.sector0_locked = true,
                    _ => {}
                }
            }
            _ => self.step = 0,
        }
    }

    /// A program-mode write lands in the 128-byte page it belongs to; the page is committed when the
    /// address leaves it or when it is full.
    fn program(&mut self, addr: usize, value: u8) {
        let page = addr & !0x7F;
        if page != self.page {
            self.commit();
            self.page = page;
        }
        let slot = addr & 0x7F;
        self.buffer[slot] = value;
        self.written[slot] = 1;
        if self.written.iter().all(|&w| w != 0) {
            self.commit();
        }
    }

    /// Flash programming can only clear bits, so the page is ANDed in.
    fn commit(&mut self) {
        for slot in 0..128 {
            if self.written[slot] != 0 {
                let i = self.page + slot;
                self.bytes[i] &= self.buffer[slot];
            }
        }
        self.buffer = [0; 128];
        self.written = [0; 128];
    }

    fn erase_sector(&mut self, addr: usize) {
        let first = (addr / SECTOR_SIZE) * SECTOR_SIZE;
        if first == 0 && self.sector0_locked {
            return;
        }
        self.bytes[first..first + SECTOR_SIZE].fill(0xFF);
    }

    /// "Chip erase ... erases all sectors except sector 0 if it is protected."
    fn erase_chip(&mut self) {
        let first = if self.sector0_locked { SECTOR_SIZE } else { 0 };
        self.bytes[first..FLASH_SIZE].fill(0xFF);
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.step);
        w.u8(match self.mode {
            Mode::Read => 0,
            Mode::Id => 1,
            Mode::Program => 2,
            Mode::HiddenRead => 3,
            Mode::HiddenProgram => 4,
        });
        w.bool(self.sector0_locked);
        w.u32(self.page as u32);
        w.bytes(&self.buffer);
        w.bytes(&self.written);
        // The 256-byte hidden region is small enough to keep with the state; the 1 MiB of flash is
        // the storage medium, like the ROM, and lives in the `.sav` only.
        w.bytes(&self.bytes[FLASH_SIZE..]);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let step = r.u8()?;
        let mode = r.u8()?;
        let sector0_locked = r.bool()?;
        let page = r.u32()? as usize;
        r.bytes_into(&mut self.buffer)?;
        r.bytes_into(&mut self.written)?;
        r.bytes_into(&mut self.bytes[FLASH_SIZE..])?;
        if step > 9 || page > FLASH_SIZE || self.written.iter().any(|&w| w > 1) {
            return Err(StateError::BadValue);
        }
        self.step = step;
        self.mode = match mode {
            0 => Mode::Read,
            1 => Mode::Id,
            2 => Mode::Program,
            3 => Mode::HiddenRead,
            4 => Mode::HiddenProgram,
            _ => return Err(StateError::BadValue),
        };
        self.sector0_locked = sector0_locked;
        self.page = page;
        Ok(())
    }
}

/// MBC6.
#[derive(Clone)]
pub struct Mbc6 {
    ram_enable: bool,
    ram_a: u8,
    ram_b: u8,
    flash_enable: bool,
    flash_write_enable: bool,
    rom_a: u8,
    select_a: u8,
    rom_b: u8,
    select_b: u8,
    rom_size: usize,
    flash: Flash,
}

impl Mbc6 {
    pub fn new(rom_banks: usize) -> Mbc6 {
        Mbc6 {
            ram_enable: false,
            ram_a: 0,
            ram_b: 0,
            flash_enable: false,
            flash_write_enable: false,
            rom_a: 0,
            select_a: 0,
            rom_b: 1,
            select_b: 0,
            rom_size: (rom_banks.max(1) * 0x4000).max(0x2000),
            flash: Flash::new(),
        }
    }

    /// An 8 KiB bank's offset into the ROM, wrapped to the image.
    fn bank_offset(&self, bank: u8) -> usize {
        let offset = ((bank as usize) & 0x7F) << 13;
        offset % self.rom_size
    }

    /// True when the upper window containing `addr` has the flash selected.
    fn window_selects_flash(&self, addr: u16) -> bool {
        let select = if addr < 0x6000 {
            self.select_a
        } else {
            self.select_b
        };
        select & 0x08 != 0
    }

    /// The flash address a write or read in `$4000-$7FFF` presents: the window's bank number as the
    /// top bits and the address inside the window as the low 13 (`MBC6.html`'s "2:5555" notation).
    fn flash_addr(&self, addr: u16) -> usize {
        let bank = if addr < 0x6000 {
            self.rom_a
        } else {
            self.rom_b
        };
        ((bank as usize) << 13) | (addr as usize & 0x1FFF)
    }
}

impl Mbc for Mbc6 {
    fn rom_offset(&self, addr: u16) -> usize {
        match addr {
            0x0000..=0x3FFF => (addr as usize) % self.rom_size,
            0x4000..=0x5FFF => {
                (self.bank_offset(self.rom_a) + (addr as usize & 0x1FFF)) % self.rom_size
            }
            _ => (self.bank_offset(self.rom_b) + (addr as usize & 0x1FFF)) % self.rom_size,
        }
    }

    fn read_rom(&self, addr: u16) -> Option<u8> {
        if addr < 0x4000 || !self.window_selects_flash(addr) {
            return None;
        }
        if !self.flash_enable {
            // The chip is not enabled, so nothing drives the bus.
            return Some(0xFF);
        }
        Some(self.flash.read(self.flash_addr(addr)))
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if !self.ram_enable {
            return RamAccess::Off;
        }
        let bank = if addr < 0xB000 {
            self.ram_a
        } else {
            self.ram_b
        };
        let offset = (((bank & 0x07) as usize) << 12) | (addr as usize & 0x0FFF);
        RamAccess::Ram(offset % RAM_SIZE)
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x03FF => self.ram_enable = value & 0x0F == 0x0A,
            0x0400..=0x07FF => self.ram_a = value & 0x07,
            0x0800..=0x0BFF => self.ram_b = value & 0x07,
            0x0C00..=0x0FFF => self.flash_enable = value & 0x01 != 0,
            0x1000..=0x13FF => self.flash_write_enable = value & 0x01 != 0,
            0x2000..=0x27FF => self.rom_a = value,
            0x2800..=0x2FFF => self.select_a = value,
            0x3000..=0x37FF => self.rom_b = value,
            0x3800..=0x3FFF => self.select_b = value,
            _ => {
                if self.window_selects_flash(addr) && self.flash_enable && self.flash_write_enable {
                    let at = self.flash_addr(addr);
                    self.flash.write(at, value);
                }
            }
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "mbc6"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn saved_data(&self) -> Option<&[u8]> {
        Some(&self.flash.bytes)
    }

    fn load_saved_data(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() != FLASH_SIZE + HIDDEN_SIZE {
            return Err(format!(
                "flash image is {} bytes, need {}",
                bytes.len(),
                FLASH_SIZE + HIDDEN_SIZE
            ));
        }
        self.flash.bytes.copy_from_slice(bytes);
        Ok(())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.ram_a);
        w.u8(self.ram_b);
        w.bool(self.flash_enable);
        w.bool(self.flash_write_enable);
        w.u8(self.rom_a);
        w.u8(self.select_a);
        w.u8(self.rom_b);
        w.u8(self.select_b);
        self.flash.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let ram_a = r.u8()?;
        let ram_b = r.u8()?;
        let flash_enable = r.bool()?;
        let flash_write_enable = r.bool()?;
        let rom_a = r.u8()?;
        let select_a = r.u8()?;
        let rom_b = r.u8()?;
        let select_b = r.u8()?;
        if ram_a > 0x07 || ram_b > 0x07 {
            return Err(StateError::BadValue);
        }
        self.flash.load_state(r)?;
        self.ram_enable = ram_enable;
        self.ram_a = ram_a;
        self.ram_b = ram_b;
        self.flash_enable = flash_enable;
        self.flash_write_enable = flash_write_enable;
        self.rom_a = rom_a;
        self.select_a = select_a;
        self.rom_b = rom_b;
        self.select_b = select_b;
        Ok(())
    }
}
