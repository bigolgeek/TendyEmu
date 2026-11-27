//! MMM01 (type bytes `$0B-$0D`): a multi-game board that starts *unmapped*, showing the last 32 KiB
//! of the ROM, and becomes MBC1-like once the menu asks for it (Pan Docs `MMM01.html`).
//!
//! The two modes, from the page's register diagrams:
//!
//! * **Unmapped** (power-on): `$0000-$7FFF` is the last 32 KiB of the ROM, which is where the menu
//!   and its own header live ("the correct ROM header … needs to be located at offset (size - 32
//!   KiB) + `$0100`"). The extra registers below are writable only in this mode.
//! * **Mapped**: the board behaves like an MBC1 with a 9-bit ROM bank and a 4-bit RAM bank: the low
//!   five ROM-bank bits at `$2000`, two more at `$2000` bits 5-6, two at `$4000` bits 4-5, and a
//!   five-bit mask at `$6000` that pins the "which game" bits.
//!
//! Unimplemented: the multiplexed mode (`$6000` bit
//! 6, which Pan Docs says "was never used commercially"). The bit is latched and ignored.
//!
//! UNVERIFIED: Pan Docs says "it is currently unknown whether RAM access is possible while in
//! unmapped mode", so RAM reads `$FF` and writes are dropped until the board is mapped. RAM banking
//! in mode 0 ANDs the low RAM-bank register with its mask, the page's "bitwise and of RAM Bank Low &
//! RAM Bank Mask".

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// The `$0147` bytes this build routes to [`Mmm01`].
pub const CART_TYPES: [u8; 3] = [0x0B, 0x0C, 0x0D];

/// MMM01.
#[derive(Clone)]
pub struct Mmm01 {
    ram_enable: bool,
    /// `$2000-$3FFF` bits 0-4, the low five bits of a 16 KiB bank number.
    bank_low: u8,
    /// `$2000-$3FFF` bits 5-6.
    bank_mid: u8,
    /// `$4000-$5FFF` bits 4-5.
    bank_high: u8,
    /// `$6000-$7FFF` bits 1-5: which of the low five bank bits belong to the game, not the game's
    /// own banking. Bit 0 is never maskable, so bank 0 stays reachable.
    mask: u8,
    /// `$4000-$5FFF` bits 0-1.
    ram_low: u8,
    /// `$4000-$5FFF` bits 2-3.
    ram_high: u8,
    /// `$0000-$1FFF` bits 4-5: the two RAM-bank bits that belong to the game.
    ram_mask: u8,
    /// `$6000-$7FFF` bit 0, the MBC1-style banking mode.
    mode: bool,
    /// `$4000-$5FFF` bit 6: a game that does not expect the mode register can pin it.
    mode_disable: bool,
    /// `$6000-$7FFF` bit 6, latched and not implemented.
    multiplex: bool,
    /// `$0000-$1FFF` bit 6: set once, the board becomes MBC1-like for the rest of the run.
    mapped: bool,
    rom_size: usize,
    ram_size: usize,
}

impl Mmm01 {
    pub fn new(rom_banks: usize, ram_bytes: usize) -> Mmm01 {
        Mmm01 {
            ram_enable: false,
            bank_low: 0,
            bank_mid: 0,
            bank_high: 0,
            mask: 0,
            ram_low: 0,
            ram_high: 0,
            ram_mask: 0,
            mode: false,
            mode_disable: false,
            multiplex: false,
            mapped: false,
            rom_size: rom_banks.max(1) * 0x4000,
            ram_size: ram_bytes,
        }
    }

    /// The bank-number bits that come from `$2000` bits 5-6 and `$4000` bits 4-5.
    fn bank_base(&self) -> usize {
        ((self.bank_high as usize) << 7) | ((self.bank_mid as usize) << 5)
    }
}

impl Mbc for Mmm01 {
    fn rom_offset(&self, addr: u16) -> usize {
        if !self.mapped {
            // Unmapped mode: the last 32 KiB of the ROM, header and all (`MMM01.html`).
            let base = self.rom_size.saturating_sub(0x8000);
            return (base + (addr as usize & 0x7FFF)) % self.rom_size;
        }
        let bank = if addr < 0x4000 {
            // The page's `$0000-$3FFF` diagram: `0 | RAM Bank Low's game bits | Bank Mask & Bank
            // Low`, i.e. only the bits the mask leaves to the game, with no `| 1` in the fixed
            // window.
            (self.bank_low & self.mask) as usize
        } else {
            // `Bank High | Bank Mid | Bank Low`, and bank low zero becomes one (`MMM01.html`, the
            // "| 1" in the diagram).
            let low = if self.bank_low == 0 { 1 } else { self.bank_low };
            self.bank_base() | low as usize
        };
        let offset = (bank << 14) | (addr as usize & 0x3FFF);
        offset % self.rom_size
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if !self.ram_enable || self.ram_size == 0 || !self.mapped {
            return RamAccess::Off;
        }
        let bank = if self.mode {
            ((self.ram_high as usize) << 2) | self.ram_low as usize
        } else {
            ((self.ram_high as usize) << 2) | (self.ram_low & self.ram_mask) as usize
        };
        let offset = (bank << 13) | (addr as usize & 0x1FFF);
        RamAccess::Ram(offset % self.ram_size)
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                self.ram_enable = value & 0x0F == 0x0A;
                if !self.mapped {
                    self.ram_mask = (value >> 4) & 0x03;
                    // Bit 6 is the mapping enable: "only available in unmapped mode", and it stays
                    // on for the rest of the run.
                    if value & 0x40 != 0 {
                        self.mapped = true;
                    }
                }
            }
            0x2000..=0x3FFF => {
                self.bank_low = value & 0x1F;
                if !self.mapped {
                    self.bank_mid = (value >> 5) & 0x03;
                }
            }
            0x4000..=0x5FFF => {
                self.ram_low = value & 0x03;
                if !self.mapped {
                    self.ram_high = (value >> 2) & 0x03;
                    self.bank_high = (value >> 4) & 0x03;
                    self.mode_disable = value & 0x40 != 0;
                }
            }
            _ => {
                if !self.mapped {
                    // `$4000` bit 6 pins the mode register for games that do not expect it.
                    if !self.mode_disable {
                        self.mode = value & 0x01 != 0;
                    }
                    self.mask = (value >> 1) & 0x1F;
                    self.multiplex = value & 0x40 != 0;
                }
            }
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "mmm01"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.bank_low);
        w.u8(self.bank_mid);
        w.u8(self.bank_high);
        w.u8(self.mask);
        w.u8(self.ram_low);
        w.u8(self.ram_high);
        w.u8(self.ram_mask);
        w.bool(self.mode);
        w.bool(self.mode_disable);
        w.bool(self.multiplex);
        w.bool(self.mapped);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let bank_low = r.u8()?;
        let bank_mid = r.u8()?;
        let bank_high = r.u8()?;
        let mask = r.u8()?;
        let ram_low = r.u8()?;
        let ram_high = r.u8()?;
        let ram_mask = r.u8()?;
        let mode = r.bool()?;
        let mode_disable = r.bool()?;
        let multiplex = r.bool()?;
        let mapped = r.bool()?;
        if bank_low > 0x1F || bank_mid > 0x03 || bank_high > 0x03 || mask > 0x1F {
            return Err(StateError::BadValue);
        }
        if ram_low > 0x03 || ram_high > 0x03 || ram_mask > 0x03 {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.bank_low = bank_low;
        self.bank_mid = bank_mid;
        self.bank_high = bank_high;
        self.mask = mask;
        self.ram_low = ram_low;
        self.ram_high = ram_high;
        self.ram_mask = ram_mask;
        self.mode = mode;
        self.mode_disable = mode_disable;
        self.multiplex = multiplex;
        self.mapped = mapped;
        Ok(())
    }
}
