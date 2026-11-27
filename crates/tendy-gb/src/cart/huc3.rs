//! HuC3 (type byte `$FE`): MBC5-style ROM banking plus the HuC-3's RAM/RTC/IR register and its
//! RTC MCU mailbox (Pan Docs `HuC3.html`).
//!
//! Banking is the documented half: `$0000-$1FFF` selects what `$A000-$BFFF` shows (only the low
//! nibble matters), `$2000-$3FFF` takes a 7-bit bank number with bank $00 mappable "like the MBC5",
//! `$4000-$5FFF` a RAM bank number of at least two bits, and `$6000-$7FFF` is written with $01 on
//! startup but "doesn't seem to have any observable effect".
//!
//! The other half is the mailbox. Register `$B` holds a command in bits 6-4 and an argument in bits
//! 3-0; writing `$D` with bit 0 clear executes it; `$D` reads back ready (bit 0 high) and `$C` reads
//! back the last command in bits 6-4 and its result in bits 3-0. Commands `$1/$3/$4/$5` walk a
//! 256-nybble window with an access address; `$6` runs an extended command (`$0` copy the current
//! time to `$00-$06`, `$1` write it back, `$2` status, `$E` tone generator).
//!
//! UNVERIFIED:
//!
//! * the layout of the current-time window `$00-$06` is not documented, so those nybbles are plain
//!   storage and command `$0` leaves them as they are: a game reads zeros (midnight, day 0) rather
//!   than a format this build made up;
//! * the documented counters at `$10-$12` (minute of day, rolls over at 1440) and `$13-$15` (day
//!   counter) are maintained from the front end's wall clock, fifteen minutes of gameplay at a time;
//! * the tone generator (command `$E` twice, `$26`/`$27`) is acknowledged and silent: there is no
//!   speaker on the board;
//! * the IR register is latched and never shifts a byte.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// The only `$0147` byte that selects this board.
pub const CART_TYPE: u8 = 0xFE;
/// Nybbles of RTC MCU memory the mailbox exposes.
const NYBBLES: usize = 256;

/// HuC3.
#[derive(Clone)]
pub struct HuC3 {
    /// `$2000-$3FFF`, seven bits.
    rom_bank: u8,
    /// `$4000-$5FFF`, two bits.
    ram_bank: u8,
    /// `$0000-$1FFF` low nibble: what `$A000-$BFFF` shows.
    select: u8,
    /// The mailbox's command/argument register (`$B`).
    cmd: u8,
    /// The result of the last executed command, read back through `$C`.
    result: u8,
    /// The access address into the nybble window.
    addr: u8,
    /// The 256-nybble window; `$10-$15` carry the clock counters.
    mem: [u8; NYBBLES],
    /// `$E` bit 0.
    ir: bool,
    /// Minutes elapsed since the cartridge was inserted, in total, so the day counter can follow.
    total_minutes: u64,
    /// `total_minutes` when `base_ms` was last taken.
    base_minutes: u64,
    /// The wall clock `total_minutes` is measured from, 0 before the first tick.
    base_ms: u64,
    /// `$10-$15` hold at most three nybbles each.
    rom_size: usize,
    ram_size: usize,
}

impl HuC3 {
    pub fn new(rom_banks: usize, ram_bytes: usize) -> HuC3 {
        HuC3 {
            rom_bank: 1,
            ram_bank: 0,
            select: 0,
            cmd: 0,
            result: 0,
            addr: 0,
            mem: [0; NYBBLES],
            ir: false,
            total_minutes: 0,
            base_minutes: 0,
            base_ms: 0,
            rom_size: rom_banks.max(1) * 0x4000,
            ram_size: ram_bytes,
        }
    }

    /// Multi-nybble values are stored "with the least significant nybble at the lowest address".
    fn set_nybbles(&mut self, at: usize, value: u16, count: usize) {
        for i in 0..count {
            self.mem[at + i] = ((value >> (4 * i)) & 0x0F) as u8;
        }
    }

    /// The documented counters: minute of day at `$10-$12`, day counter at `$13-$15`.
    fn sync_clock(&mut self) {
        let minute_of_day = (self.total_minutes % 1440) as u16;
        let days = ((self.total_minutes / 1440) & 0x0FFF) as u16;
        self.set_nybbles(0x10, minute_of_day, 3);
        self.set_nybbles(0x13, days, 3);
    }

    /// Runs the command in the mailbox (`HuC3.html`, "Known commands").
    fn execute(&mut self) {
        let command = self.cmd >> 4;
        let argument = self.cmd & 0x0F;
        self.result = 0;
        match command {
            1 => {
                self.result = self.mem[self.addr as usize] & 0x0F;
                self.addr = self.addr.wrapping_add(1);
            }
            3 => {
                self.mem[self.addr as usize] = argument;
                self.addr = self.addr.wrapping_add(1);
            }
            4 => self.addr = (self.addr & 0xF0) | argument,
            5 => self.addr = (self.addr & 0x0F) | (argument << 4),
            6 => match argument {
                // $00-$06 is the current-time window; its layout is undocumented, so the copy is a
                // no-op and the nybbles stay as the game left them.
                0 | 1 => {}
                // "The games will not start if the result is not $1."
                2 => self.result = 1,
                // The tone generator: acknowledged, silent.
                0xE => {}
                _ => {}
            },
            _ => {}
        }
    }

    fn ram_bank(&self) -> usize {
        ((self.ram_bank as usize) & 0x03) << 13
    }
}

impl Mbc for HuC3 {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            return (addr & 0x3FFF) as usize;
        }
        let bank = self.rom_bank & 0x7F;
        let offset = ((bank as usize) << 14) | (addr as usize & 0x3FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        // $0 is read-only RAM, $A read/write RAM; everything else is an I/O register.
        if !matches!(self.select & 0x0F, 0x0 | 0x0A) || self.ram_size == 0 {
            return RamAccess::Off;
        }
        let offset = self.ram_bank() | (addr as usize & 0x1FFF);
        RamAccess::Ram(offset % self.ram_size)
    }

    fn read_ram(&self, _addr: u16) -> Option<u8> {
        // Address lines A12-A0 are not connected, so the offset inside the window is ignored; D7 is
        // open bus and reads back high.
        Some(match self.select & 0x0F {
            // Cart RAM, in both of its modes, is `Cartridge`'s to read.
            0x00 | 0x0A => return None,
            // Command in bits 6-4, the last command's result in bits 3-0.
            0x0C => 0x80 | (self.cmd & 0x70) | (self.result & 0x0F),
            // Ready to accept a command.
            0x0D => 0x81,
            // The IR register's least significant bit, open bus above it.
            0x0E => 0x80 | self.ir as u8,
            _ => 0xFF,
        })
    }

    fn write_ram(&mut self, _addr: u16, value: u8) -> bool {
        let value = value & 0x7F;
        match self.select & 0x0F {
            // Read-only RAM: the write is dropped, as the register table says.
            0x00 => true,
            // Read/write RAM: let `Cartridge` store it.
            0x0A => false,
            // Command and argument: stored, not executed.
            0x0B => {
                self.cmd = value;
                true
            }
            // Bit 0 clear requests that the MCU execute the command written to $B.
            0x0D => {
                if value & 0x01 == 0 {
                    self.execute();
                }
                true
            }
            0x0E => {
                self.ir = value & 0x01 != 0;
                true
            }
            _ => true,
        }
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.select = value & 0x0F,
            0x2000..=0x3FFF => self.rom_bank = value & 0x7F,
            0x4000..=0x5FFF => self.ram_bank = value & 0x03,
            // "Games write $01 here on startup, but it doesn't seem to have any observable effect."
            _ => {}
        }
    }

    fn tick(&mut self, now_ms: u64) {
        if self.base_ms == 0 {
            self.base_ms = now_ms;
            self.base_minutes = self.total_minutes;
            self.sync_clock();
            return;
        }
        let elapsed = now_ms.saturating_sub(self.base_ms) / 60_000;
        self.total_minutes = self.base_minutes + elapsed;
        self.sync_clock();
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "huc3"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.rom_bank);
        w.u8(self.ram_bank);
        w.u8(self.select);
        w.u8(self.cmd);
        w.u8(self.result);
        w.u8(self.addr);
        w.bytes(&self.mem);
        w.bool(self.ir);
        w.u64(self.total_minutes);
        w.u64(self.base_minutes);
        w.u64(self.base_ms);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let rom_bank = r.u8()?;
        let ram_bank = r.u8()?;
        let select = r.u8()?;
        let cmd = r.u8()?;
        let result = r.u8()?;
        let addr = r.u8()?;
        r.bytes_into(&mut self.mem)?;
        let ir = r.bool()?;
        let total_minutes = r.u64()?;
        let base_minutes = r.u64()?;
        let base_ms = r.u64()?;
        if rom_bank > 0x7F || ram_bank > 0x03 || select > 0x0F || cmd > 0x7F || result > 0x0F {
            return Err(StateError::BadValue);
        }
        if self.mem.iter().any(|&n| n > 0x0F) {
            return Err(StateError::BadValue);
        }
        self.rom_bank = rom_bank;
        self.ram_bank = ram_bank;
        self.select = select;
        self.cmd = cmd;
        self.result = result;
        self.addr = addr;
        self.ir = ir;
        self.total_minutes = total_minutes;
        self.base_minutes = base_minutes;
        self.base_ms = base_ms;
        Ok(())
    }
}
