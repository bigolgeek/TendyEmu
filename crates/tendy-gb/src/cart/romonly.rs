//! ROM only (type `$00`): 32 KiB direct, no bank register, flat 8 KiB RAM when the header says so.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// No mapper: `$0000-$7FFF` is the first 32 KiB, `$A000-$BFFF` is the cartridge RAM.
#[derive(Clone)]
pub struct RomOnly {
    /// Header RAM size, so `ram_access` knows whether a cell exists. Not saved: it follows the ROM.
    ram_bytes: usize,
}

impl RomOnly {
    pub fn new(ram_bytes: usize) -> RomOnly {
        RomOnly {
            ram_bytes: ram_bytes.min(0x2000),
        }
    }
}

impl Mbc for RomOnly {
    fn rom_offset(&self, addr: u16) -> usize {
        (addr & 0x7FFF) as usize
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if self.ram_bytes == 0 {
            RamAccess::Off
        } else {
            RamAccess::Ram((addr as usize) & 0x1FFF)
        }
    }

    fn write_rom(&mut self, _addr: u16, _value: u8) {}

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "romonly"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, _w: &mut StateWriter) {}

    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), StateError> {
        Ok(())
    }
}
