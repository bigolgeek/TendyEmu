//! MBC2 (type bytes `$05/$06`): a 4-bit ROM bank, and 512 x 4 bits of RAM built into the chip
//! (Pan Docs `MBC2.html`).
//!
//! The 4-bit cells live in `Cartridge::ram` one byte per cell; `Cartridge::read`/`write` add the
//! `$F` high nibble on a read and keep only the low nibble on a write, because `MBC2.html` says the
//! upper four bits are undefined and read as `$F`.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// Bytes of built-in RAM: 512 half-bytes.
pub const RAM_SIZE: usize = 0x200;

/// MBC2.
#[derive(Clone)]
pub struct Mbc2 {
    ram_enable: bool,
    /// `$2100`-style writes, 4 bits as written; the 0 -> 1 translation happens at use.
    bank: u8,
    rom_size: usize,
}

impl Mbc2 {
    pub fn new(rom_banks: usize) -> Mbc2 {
        Mbc2 {
            ram_enable: false,
            bank: 0,
            rom_size: rom_banks.max(1) * 0x4000,
        }
    }
}

impl Mbc for Mbc2 {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            return (addr & 0x3FFF) as usize;
        }
        // Only 16 banks exist, and a 0 in the register reads as bank 1.
        let mut bank = self.bank & 0x0F;
        if bank == 0 {
            bank = 1;
        }
        let offset = ((bank as usize) << 14) | (addr as usize & 0x3FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if !self.ram_enable {
            return RamAccess::Off;
        }
        // Only the low 9 address bits are decoded, so $A200-$BFFF echoes $A000-$A1FF.
        RamAccess::Ram(addr as usize & 0x1FF)
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        if addr >= 0x4000 {
            return;
        }
        if addr & 0x0100 == 0 {
            self.ram_enable = value & 0x0F == 0x0A;
        } else {
            self.bank = value & 0x0F;
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "mbc2"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.bank);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let bank = r.u8()?;
        if bank > 0x0F {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.bank = bank;
        Ok(())
    }
}
