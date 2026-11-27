//! MBC3 (type bytes `$0F-$13`) and MBC30: a ROM bank, a RAM-bank/clock-register selector and the
//! `$00` then `$01` latch pair (Pan Docs `MBC3.html`).
//!
//! No clock here: the counters are `Cartridge::rtc`, and this mapper only answers
//! `RamAccess::Rtc(reg)`.
//!
//! **MBC30** is "practically identical to MBC3 in operation, but is capable of addressing twice as
//! much memory for both ROM and RAM" (`MBC3.html` § MBC30: 4 MiB ROM, 64 KiB RAM, timer). Pan Docs
//! does not give it a type byte of its own; its `The_Cartridge_Header.html` footnote says a cartridge
//! with type `$10`/`$12`/`$13` **and 64 KiB of SRAM** is an MBC30. So the board is the same wiring
//! with the ROM bank register's eighth bit live: 256 banks of 16 KiB instead of 128.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// MBC3, and MBC30 when `mbc30` is set.
#[derive(Clone)]
pub struct Mbc3 {
    ram_enable: bool,
    /// `$2000-$3FFF`, 7 bits as written (8 on an MBC30); the 0 -> 1 translation happens at use.
    rom_bank: u8,
    /// `$4000-$5FFF`: 0-7 selects a RAM bank, 8-12 a clock register.
    select: u8,
    /// The previous write to `$6000-$7FFF`, so `$00` then `$01` is the only latch trigger.
    latch_prev: u8,
    /// Set for the one write that completed the latch pair; taken by `Cartridge::write`.
    latch_request: bool,
    has_rtc: bool,
    /// MBC30: the ROM bank register is eight bits wide, not seven.
    mbc30: bool,
    rom_size: usize,
    ram_size: usize,
}

impl Mbc3 {
    pub fn new(rom_banks: usize, ram_bytes: usize, has_rtc: bool, mbc30: bool) -> Mbc3 {
        Mbc3 {
            ram_enable: false,
            rom_bank: 0,
            select: 0,
            latch_prev: 0,
            latch_request: false,
            has_rtc,
            mbc30,
            rom_size: rom_banks.max(1) * 0x4000,
            ram_size: ram_bytes,
        }
    }

    /// 7 bits on an MBC3, 8 on an MBC30 (`MBC3.html` § MBC30).
    fn bank_mask(&self) -> u8 {
        if self.mbc30 { 0xFF } else { 0x7F }
    }
}

impl Mbc for Mbc3 {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            return (addr & 0x3FFF) as usize;
        }
        let mut bank = self.rom_bank & self.bank_mask();
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
        match self.select {
            0x00..=0x07 => {
                if self.ram_size == 0 {
                    return RamAccess::Off;
                }
                let offset = ((self.select as usize) << 13) | (addr as usize & 0x1FFF);
                RamAccess::Ram(if offset < self.ram_size {
                    offset
                } else {
                    offset % self.ram_size
                })
            }
            // $08-$0C are the clock; a cartridge without one reads $FF (`MBC3.html`).
            0x08..=0x0C => {
                if self.has_rtc {
                    RamAccess::Rtc(self.select - 0x08)
                } else {
                    RamAccess::Off
                }
            }
            _ => RamAccess::Off,
        }
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enable = value & 0x0F == 0x0A,
            0x2000..=0x3FFF => self.rom_bank = value & self.bank_mask(),
            0x4000..=0x5FFF => self.select = value,
            0x6000..=0x7FFF => {
                self.latch_request = value == 0x01 && self.latch_prev == 0x00;
                self.latch_prev = value;
            }
            _ => {}
        }
    }

    fn take_latch_request(&mut self) -> bool {
        std::mem::take(&mut self.latch_request)
    }

    fn name(&self) -> &'static str {
        if self.mbc30 { "mbc30" } else { "mbc3" }
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.rom_bank);
        w.u8(self.select);
        w.u8(self.latch_prev);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let rom_bank = r.u8()?;
        let select = r.u8()?;
        let latch_prev = r.u8()?;
        if rom_bank > self.bank_mask() {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.rom_bank = rom_bank;
        self.select = select;
        self.latch_prev = latch_prev;
        Ok(())
    }
}
