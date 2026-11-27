//! MBC5 (type bytes `$19-$1E`): a 9-bit ROM bank, a RAM bank, and the rumble bit accepted and
//! ignored (Pan Docs `MBC5.html`).
//!
//! `$1C-$1E` are the rumble boards: "Bit 3 of the RAM bank number controls the rumble motor" and
//! the RAM bank number is the low three bits (`MBC5.html` § Rumble). There is no motor to drive, so
//! this build accepts the bit and drops it: a write of `$09` selects RAM bank 1, never a ninth bank.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// MBC5, and the rumble variant when `rumble` is set.
#[derive(Clone)]
pub struct Mbc5 {
    ram_enable: bool,
    /// `$2000-$2FFF` low eight bits and `$3000-$3FFF` the ninth; bank 0 really is bank 0.
    rom_bank: u16,
    ram_bank: u8,
    /// `$1C-$1E`: bit 3 of the RAM bank register is the motor, not a bank number.
    rumble: bool,
    rom_size: usize,
    ram_size: usize,
}

impl Mbc5 {
    pub fn new(rom_banks: usize, ram_bytes: usize, rumble: bool) -> Mbc5 {
        Mbc5 {
            ram_enable: false,
            // Pan Docs `MBC5.html`: the ROM bank register powers on as 1, so $4000-$7FFF maps bank
            // 1 until a write says otherwise (unlike later writes, where 0 means bank 0).
            rom_bank: 1,
            ram_bank: 0,
            rumble,
            rom_size: rom_banks.max(1) * 0x4000,
            ram_size: ram_bytes,
        }
    }

    /// Three bits on a rumble cart, four otherwise (`MBC5.html` § Rumble).
    fn ram_bank_mask(&self) -> u8 {
        if self.rumble { 0x07 } else { 0x0F }
    }
}

impl Mbc for Mbc5 {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            return (addr & 0x3FFF) as usize;
        }
        let bank = (self.rom_bank & 0x1FF) as usize;
        let offset = (bank << 14) | (addr as usize & 0x3FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if !self.ram_enable || self.ram_size == 0 {
            return RamAccess::Off;
        }
        let offset = ((self.ram_bank as usize) << 13) | (addr as usize & 0x1FFF);
        RamAccess::Ram(if offset < self.ram_size {
            offset
        } else {
            offset % self.ram_size
        })
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enable = value & 0x0F == 0x0A,
            0x2000..=0x2FFF => self.rom_bank = (self.rom_bank & 0x100) | value as u16,
            0x3000..=0x3FFF => {
                self.rom_bank = (self.rom_bank & 0x0FF) | (((value & 0x01) as u16) << 8);
            }
            // Bit 3 is the rumble motor on the rumble carts; accepted and dropped here.
            0x4000..=0x5FFF => self.ram_bank = value & self.ram_bank_mask(),
            _ => {}
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        if self.rumble { "mbc5r" } else { "mbc5" }
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u16(self.rom_bank);
        w.u8(self.ram_bank);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let rom_bank = r.u16()?;
        let ram_bank = r.u8()?;
        if rom_bank > 0x1FF || ram_bank > self.ram_bank_mask() {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.rom_bank = rom_bank;
        self.ram_bank = ram_bank;
        Ok(())
    }
}
