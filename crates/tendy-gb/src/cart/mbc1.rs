//! MBC1 (type bytes `$01-$03`, and HuC1 at `$FF`): 5-bit ROM bank, 2-bit second register, banking
//! mode, and the 1 MiB "MBC1M" multicart wiring (Pan Docs `MBC1.html`).
//!
//! HuC1 is the same wiring with one different rule (`huc1`: the RAM enable nibble is `$A` or `$E`)
//! and no IR port, which is the whole of the HuC1 row.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// MBC1, MBC1M and HuC1.
#[derive(Clone)]
pub struct Mbc1 {
    ram_enable: bool,
    /// `$2000-$3FFF`, 5 bits as written; the 0 -> 1 translation happens at use.
    bank_low: u8,
    /// `$4000-$5FFF`, 2 bits.
    bank_high: u8,
    /// `$6000-$7FFF`, bit 0.
    mode: bool,
    huc1: bool,
    mbc1m: bool,
    /// Bytes of ROM actually present; every offset wraps modulo this.
    rom_size: usize,
    /// Bytes of RAM actually present; 0 means the cartridge has none.
    ram_size: usize,
}

impl Mbc1 {
    pub fn new(rom_banks: usize, ram_bytes: usize, huc1: bool, mbc1m: bool) -> Mbc1 {
        Mbc1 {
            ram_enable: false,
            bank_low: 0,
            bank_high: 0,
            mode: false,
            huc1,
            mbc1m,
            rom_size: rom_banks.max(1) * 0x4000,
            ram_size: ram_bytes,
        }
    }

    /// Bits 4-5 of the bank number for MBC1M, bits 5-6 for everything else (`MBC1.html`).
    fn high_shift(&self) -> u32 {
        if self.mbc1m { 4 } else { 5 }
    }

    /// The bank in `$4000-$7FFF`: the 0 -> 1 translation applies to the full 5-bit register before
    /// MBC1M drops bit 4 (`MBC1.html`, "If the main 5-bit ROM banking register is 0 ...").
    fn banked(&self) -> usize {
        let mut low = self.bank_low & 0x1F;
        if low == 0 {
            low = 1;
        }
        if self.mbc1m {
            low &= 0x0F;
        }
        low as usize | ((self.bank_high as usize) << self.high_shift())
    }

    /// The bank in `$0000-$3FFF`: bank 0 in mode 0, the second register in mode 1.
    fn fixed(&self) -> usize {
        if self.mode {
            (self.bank_high as usize) << self.high_shift()
        } else {
            0
        }
    }

    fn rom_offset_at(&self, bank: usize, addr: u16) -> usize {
        let offset = (bank << 14) | (addr as usize & 0x3FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_offset(&self, addr: u16) -> usize {
        let bank = if self.mode {
            self.bank_high as usize
        } else {
            0
        };
        let offset = (bank << 13) | (addr as usize & 0x1FFF);
        if offset < self.ram_size {
            offset
        } else {
            offset % self.ram_size
        }
    }
}

/// The 48-byte Nintendo logo at `$0104-$0133` (`The_Cartridge_Header.html`, "0104-0133 — Nintendo
/// Logo"). MBC1M detection compares bank 16's copy with it.
const NINTENDO_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// True when `rom` is an MBC1M multi-game compilation: at least 1 MiB with the Nintendo logo in bank
/// 16 (`MBC1.html`, "MBC1M: 1 MiB Multi-Game Compilation Carts").
pub fn detect_mbc1m(rom: &[u8]) -> bool {
    rom.len() >= 0x10_0000 && rom[0x4_0104..0x4_0134] == NINTENDO_LOGO
}

impl Mbc for Mbc1 {
    fn rom_offset(&self, addr: u16) -> usize {
        let bank = if addr < 0x4000 {
            self.fixed()
        } else {
            self.banked()
        };
        self.rom_offset_at(bank, addr)
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        if !self.ram_enable || self.ram_size == 0 {
            return RamAccess::Off;
        }
        RamAccess::Ram(self.ram_offset(addr))
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                let nibble = value & 0x0F;
                // HuC1 accepts $E as well as $A.
                self.ram_enable = nibble == 0x0A || (self.huc1 && nibble == 0x0E);
            }
            0x2000..=0x3FFF => self.bank_low = value & 0x1F,
            0x4000..=0x5FFF => self.bank_high = value & 0x03,
            0x6000..=0x7FFF => self.mode = value & 0x01 != 0,
            _ => {}
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        if self.huc1 {
            "huc1"
        } else if self.mbc1m {
            "mbc1m"
        } else {
            "mbc1"
        }
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.bank_low);
        w.u8(self.bank_high);
        w.bool(self.mode);
        w.bool(self.huc1);
        w.bool(self.mbc1m);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let bank_low = r.u8()?;
        let bank_high = r.u8()?;
        let mode = r.bool()?;
        let huc1 = r.bool()?;
        let mbc1m = r.bool()?;
        if bank_low > 0x1F || bank_high > 0x03 || huc1 != self.huc1 || mbc1m != self.mbc1m {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.bank_low = bank_low;
        self.bank_high = bank_high;
        self.mode = mode;
        Ok(())
    }
}
