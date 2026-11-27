//! M161 (type byte `$E1` in this build): eight 32 KiB banks, and a latch that takes exactly one
//! bank write (Pan Docs `M161.html`).
//!
//! Pan Docs (`M161.html`): "This mapper only allows for 8 separate 32 KiB banks, with a limit of 1
//! bankswitch, preventing any bankswitches afterward"; the register is "`$0000-$7FFF` — ROM Bank
//! Number [write only]", "This 3-bit register (range `$00-$07`) selects the ROM bank number for the
//! `$0000-$7FFF` region. Like other mappers, the high bits are discarded", and "Only 1 bankswitch is
//! allowed per session, once any write to this register occurs (even if set to 0), all future writes
//! will be ignored until the mapper is powered down". So the whole 32 KiB range is the window, the
//! register powers on at bank 0, and the first write *anywhere* in `$0000-$7FFF` both selects the
//! bank and spends the latch.
//!
//! There is no RAM and no clock on the board.
//!
//! UNVERIFIED: `M161.html` gives the board no `$0147` byte and the header page's type table has no
//! entry for it, so `$E1` here is the selector the emulator's mapper table names, not something Pan Docs
//! documents. Everything inside the mapper is the documented behaviour above.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// The `$0147` byte this build routes to [`M161`].
pub const CART_TYPE: u8 = 0xE1;

/// M161: 32 KiB banks, one bank write for the life of the cartridge.
#[derive(Clone)]
pub struct M161 {
    /// `$0000-$7FFF` bits 0-2, 0-7.
    bank: u8,
    /// Set by the first write; every later write is ignored (`M161.html`).
    latched: bool,
    rom_size: usize,
}

impl M161 {
    pub fn new(rom_banks: usize) -> M161 {
        M161 {
            bank: 0,
            latched: false,
            rom_size: rom_banks.max(1) * 0x4000,
        }
    }
}

impl Mbc for M161 {
    fn rom_offset(&self, addr: u16) -> usize {
        let offset = ((self.bank as usize) << 15) | (addr as usize & 0x7FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_access(&self, _addr: u16) -> RamAccess {
        RamAccess::Off
    }

    fn write_rom(&mut self, _addr: u16, value: u8) {
        // Pan Docs gives the register the whole `$0000-$7FFF` range: the latch is driven by A15, so
        // any write below `$8000` counts, and only the first one does.
        if !self.latched {
            self.bank = value & 0x07;
            self.latched = true;
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "m161"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.bank);
        w.bool(self.latched);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let bank = r.u8()?;
        let latched = r.bool()?;
        if bank > 0x07 {
            return Err(StateError::BadValue);
        }
        self.bank = bank;
        self.latched = latched;
        Ok(())
    }
}
