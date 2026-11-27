//! The cartridge: ROM/RAM vectors, the header, one mapper behind `Box<dyn Mbc>`, and the clock.
//!
//! `Cartridge` is the only place a slice is indexed, so no mapper can read past `rom` or `ram`:
//! `rom_offset` answers with an offset already wrapped to `rom.len()`, `RamAccess::Ram` with an
//! index already banked.

use crate::cart::header::refusal;
use crate::cart::huc3::HuC3;
use crate::cart::m161::M161;
use crate::cart::mbc1::{Mbc1, detect_mbc1m};
use crate::cart::mbc2::Mbc2;
use crate::cart::mbc3::Mbc3;
use crate::cart::mbc5::Mbc5;
use crate::cart::mbc6::Mbc6;
use crate::cart::mbc7::Mbc7;
use crate::cart::mmm01::Mmm01;
use crate::cart::romonly::RomOnly;
use crate::checksum::crc32;
use crate::state::{StateError, StateReader, StateWriter};

pub mod header;
pub mod huc3;
pub mod m161;
pub mod mbc1;
pub mod mbc2;
pub mod mbc3;
pub mod mbc5;
pub mod mbc6;
pub mod mbc7;
pub mod mmm01;
pub mod romonly;
pub mod unl;

pub use header::Header;

/// Which register file serves an access to `$A000-$BFFF`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RamAccess {
    /// RAM disabled, absent or not selected: reads `$FF`, writes dropped.
    Off,
    /// Index into `Cartridge::ram`, already offset by the bank.
    Ram(usize),
    /// Clock register `$08-$0C` as 0-4, the `Rtc` register index.
    Rtc(u8),
}

/// A mapper maps addresses; `Cartridge` owns ROM, RAM and the clock and does the indexing.
pub trait Mbc: Send {
    /// Offset into `Cartridge::rom` for an access to `$0000-$7FFF`.
    fn rom_offset(&self, addr: u16) -> usize;
    /// A read of `$0000-$7FFF` the board answers itself, before `rom_offset` is consulted: MBC6's
    /// two upper windows can show the flash chip instead of ROM.
    fn read_rom(&self, _addr: u16) -> Option<u8> {
        None
    }
    /// Where an access to `$A000-$BFFF` goes.
    fn ram_access(&self, addr: u16) -> RamAccess;
    /// A read of `$A000-$BFFF` the board answers itself, before `ram_access` is consulted: MBC7's
    /// one-register window and HuC3's mailbox.
    fn read_ram(&self, _addr: u16) -> Option<u8> {
        None
    }
    /// A write to `$A000-$BFFF` the board handles itself; `true` means it was consumed.
    fn write_ram(&mut self, _addr: u16, _value: u8) -> bool {
        false
    }
    /// A write to `$0000-$7FFF`:
    fn write_rom(&mut self, addr: u16, value: u8);
    /// True once, when the write just made completed MBC3's `$00` then `$01` latch pair.
    fn take_latch_request(&mut self) -> bool;
    /// Where the cartridge's own code starts once the post-boot state is in place. Every licensed
    /// cartridge starts at `$0100`, the address the boot ROM jumps to; the unlicensed boards whose
    /// images have no logo at all put their reset vector at `$0000` instead (§5.5).
    fn entry_point(&self) -> u16 {
        0x0100
    }
    /// MBC7's accelerometer, -1, 0 or +1 per axis (X positive left, Y positive up). No other board
    /// reads it.
    fn set_tilt(&mut self, _x: i8, _y: i8) {}
    /// The front end's wall clock, for a board that keeps time of its own (HuC3's RTC counters).
    fn tick(&mut self, _now_ms: u64) {}
    /// Bytes the board owns and wants in the `.sav`: MBC6's flash and hidden region, MBC7's EEPROM.
    /// They follow the cartridge RAM in the file.
    fn saved_data(&self) -> Option<&[u8]> {
        None
    }
    /// Loads those bytes back; `Err` is the message `decode_save` refuses the file with.
    fn load_saved_data(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("this cartridge has no board-owned save data".to_string())
    }
    fn name(&self) -> &'static str;
    /// `Machine` is `Clone` and a mapper lives behind a trait object, so it clones as a box.
    fn clone_box(&self) -> Box<dyn Mbc>;
    fn save_state(&self, w: &mut StateWriter);
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError>;
}

impl Clone for Box<dyn Mbc> {
    fn clone(&self) -> Box<dyn Mbc> {
        self.clone_box()
    }
}

/// An inserted cartridge.
#[derive(Clone)]
pub struct Cartridge {
    pub header: Header,
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
    pub mbc: Box<dyn Mbc>,
    /// The one RTC owner; `Some` only for type bytes `$0F`/`$10`.
    pub rtc: Option<Rtc>,
    /// crc32 of `rom`, computed once at load.
    pub rom_crc32: u32,
    pub ram_dirty: bool,
}

impl Cartridge {
    /// Parses the header, allocates RAM, picks the mapper. `Err` is the refusal message of §5.3.
    pub fn new(rom: Vec<u8>) -> Result<Cartridge, String> {
        if rom.len() < 0x8000 {
            return Err(format!("ROM is {} bytes, need at least 32768", rom.len()));
        }
        let header = Header::parse(&rom)?;
        let rom_banks = rom.len() / crate::ROM_BANK_SIZE;
        // Rocket Games' decoy header RAM size is replaced by what the image actually does; this is
        // computed once so the mapper's internal RAM size and `Cartridge::ram` agree.
        let rocket_ram = unl::rocket_ram_bytes(&rom);
        // §5.5: an unlicensed board is identified from its boot code before the header-type table
        // gets a say, because these images carry decoy headers (a Sachen menu has no logo at all, a
        // Pro Action Replay claims MBC1+RAM+battery with a RAM size of "none").
        let mbc: Box<dyn Mbc> = match unl::detect(&rom) {
            Some(unl::Board::Sachen) => Box::new(unl::Sachen::new(rom_banks)),
            Some(unl::Board::WisdomTree) => Box::new(unl::WisdomTree::new(rom_banks)),
            Some(unl::Board::ProActionReplay) => Box::new(unl::ProActionReplay),
            None => match header.cart_type {
                0x00 if rom.len() > 0x8000 => {
                    // A "ROM ONLY" header can't address past the first 32 KiB, so a cartridge that
                    // declares one and is larger than that is really banked; Pan Docs (MBC1, "Note")
                    // says to treat such a cartridge as MBC1. Mick & Mack (proto) has exactly this
                    // header ($0147 = $00, $0148 = $00, 128 KiB of data) and never drew anything
                    // while the writes to $2000-$3FFF were swallowed.
                    Box::new(Mbc1::new(
                        rom_banks,
                        header.ram_bytes,
                        false,
                        detect_mbc1m(&rom),
                    ))
                }
                0x00 => Box::new(RomOnly::new(header.ram_bytes)),
                0x01..=0x03 => Box::new(Mbc1::new(
                    rom_banks,
                    header.ram_bytes,
                    false,
                    detect_mbc1m(&rom),
                )),
                0x05 | 0x06 => Box::new(Mbc2::new(rom_banks)),
                0x0B..=0x0D => Box::new(Mmm01::new(rom_banks, header.ram_bytes)),
                0x0F..=0x13 => {
                    // MBC30 has no type byte of its own: the header page's footnote says a
                    // `$10`/`$12`/`$13` cartridge with 64 KiB of SRAM is one, and `MBC3.html` says
                    // it addresses twice the ROM and twice the RAM. So it is the same board with an
                    // eight-bit ROM bank register, selected by a RAM size MBC3 cannot have or a ROM
                    // size its seven bits cannot reach.
                    // UNVERIFIED: neither page gives a rule for telling the two apart, so the
                    // 64 KiB-RAM / >2 MiB-ROM heuristic is this build's reading; no ROM in the
                    // target set needs it, and an MBC3 cart below those sizes is untouched.
                    let mbc30 = header.ram_bytes == 0x1_0000 || rom_banks > 0x80;
                    Box::new(Mbc3::new(
                        rom_banks,
                        header.ram_bytes,
                        header.has_rtc,
                        mbc30,
                    ))
                }
                0x19..=0x1B => Box::new(Mbc5::new(rom_banks, header.ram_bytes, false)),
                0x1C..=0x1E => Box::new(Mbc5::new(rom_banks, header.ram_bytes, true)),
                0x20 => Box::new(Mbc6::new(rom_banks)),
                0x22 => Box::new(Mbc7::new(rom_banks)),
                0xE1 => Box::new(M161::new(rom_banks)),
                0xFE => Box::new(HuC3::new(rom_banks, header.ram_bytes)),
                0xFF => Box::new(Mbc1::new(rom_banks, header.ram_bytes, true, false)),
                // Rocket Games (Datel/InterAct) unlicensed GBC carts: type `$97` (single games) and
                // `$99` (the multicarts and Race Time). The board banks through `$2000-$3FFF` with
                // one five-bit register exactly as MBC1 does — every one of the ten writes the full
                // bank number to that range and none splits it across `$2000`/`$3000` as MBC5 would
                // — so the ten ride the MBC1 mapper. The header is a decoy (no logo, RAM size bytes
                // that do not match the silicon), so the RAM size is taken from what the image
                // actually does, not the header: see `unl::rocket_ram_bytes`.
                0x97 | 0x99 => Box::new(Mbc1::new(rom_banks, rocket_ram, false, false)),
                other => return Err(refusal(other)),
            },
        };
        // MBC2 has no external RAM: its 512 x 4-bit cells are in the chip (MBC2.html), one byte each
        // here, and the header's RAM size byte is 0 by the header page's own rule.
        let ram_bytes = if mbc.name() == "mbc2" {
            mbc2::RAM_SIZE
        } else if mbc.name() == "romonly" {
            // ROM only has a flat 8 KiB at `$A000-$BFFF`.
            header.ram_bytes.min(0x2000)
        } else if mbc.name() == "actionreplay" {
            // The header says "no RAM", but the cartridge's own work RAM at `$A000-$BFFF` is what
            // its boot code fills in and branches on (§5.5).
            0x2000
        } else if mbc.name() == "mbc6" {
            // The SRAM is in the board: eight 4 KiB banks (`MBC6.html`), so the header's size byte
            // has no say.
            mbc6::RAM_SIZE
        } else if mbc.name() == "mbc7" {
            // No battery RAM at all: the 256-byte EEPROM is the board's own, kept behind `$A000`.
            0
        } else if matches!(header.cart_type, 0x97 | 0x99) {
            // Rocket Games: the RAM size comes from the image's enable nibble, not the decoy header
            // byte (`rocket_ram` above), so the mapper's window and `Cartridge::ram` stay in step.
            rocket_ram
        } else {
            header.ram_bytes
        };
        let ram = vec![0u8; ram_bytes];
        let rtc = if header.has_rtc {
            Some(Rtc::new())
        } else {
            None
        };
        let rom_crc32 = crc32(&rom);
        Ok(Cartridge {
            header,
            rom,
            ram,
            mbc,
            rtc,
            rom_crc32,
            ram_dirty: false,
        })
    }

    /// Where the cartridge's own code starts; see [`Mbc::entry_point`].
    pub fn entry_point(&self) -> u16 {
        self.mbc.entry_point()
    }

    /// MBC2's cells are 4 bits wide: a read shows `$F` in the high nibble, a write keeps the low
    /// one (`MBC2.html`, "A000-A1FF — Built-in RAM").
    fn nibble_ram(&self) -> bool {
        matches!(self.header.cart_type, 0x05 | 0x06)
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => match self.mbc.read_rom(addr) {
                Some(value) => value,
                None => {
                    let offset = self.mbc.rom_offset(addr);
                    self.rom[offset]
                }
            },
            0xA000..=0xBFFF => {
                if let Some(value) = self.mbc.read_ram(addr) {
                    return value;
                }
                match self.mbc.ram_access(addr) {
                    RamAccess::Off => 0xFF,
                    RamAccess::Ram(i) => {
                        let value = self.ram[i];
                        if self.nibble_ram() {
                            0xF0 | (value & 0x0F)
                        } else {
                            value
                        }
                    }
                    RamAccess::Rtc(r) => self.rtc.as_ref().map_or(0xFF, |rtc| rtc.read(r)),
                }
            }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF => {
                self.mbc.write_rom(addr, value);
                if self.mbc.take_latch_request()
                    && let Some(rtc) = &mut self.rtc
                {
                    rtc.latch();
                }
            }
            0xA000..=0xBFFF => {
                if self.mbc.write_ram(addr, value) {
                    return;
                }
                match self.mbc.ram_access(addr) {
                    RamAccess::Off => {}
                    RamAccess::Ram(i) => {
                        let value = if self.nibble_ram() {
                            value & 0x0F
                        } else {
                            value
                        };
                        if self.ram[i] != value {
                            self.ram[i] = value;
                            self.ram_dirty = true;
                        }
                    }
                    RamAccess::Rtc(r) => {
                        if let Some(rtc) = &mut self.rtc {
                            rtc.write(r, value);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    pub fn mbc_name(&self) -> &'static str {
        self.mbc.name()
    }

    /// The latched copy, which is what `Machine::rtc` returns.
    pub fn rtc_regs(&self) -> Option<[u8; 5]> {
        self.rtc.as_ref().map(|rtc| rtc.latched)
    }

    pub fn set_rtc_regs(&mut self, regs: [u8; 5]) {
        if let Some(rtc) = &mut self.rtc {
            rtc.secs = regs[0];
            rtc.mins = regs[1];
            rtc.hours = regs[2];
            rtc.days_low = regs[3];
            rtc.days_high = regs[4];
            rtc.latched = regs;
        }
    }

    pub fn tick_rtc(&mut self, now_ms: u64) {
        if let Some(rtc) = &mut self.rtc {
            rtc.advance(now_ms);
        }
        // HuC3 keeps its own counters (`HuC3.html`); every other board ignores this.
        self.mbc.tick(now_ms);
    }

    /// MBC7's accelerometer, from the front end's arrow keys.
    pub fn set_tilt(&mut self, x: i8, y: i8) {
        self.mbc.set_tilt(x, y);
    }

    /// The board's own non-volatile bytes, after the RAM in a `.sav`.
    pub fn board_save(&self) -> Option<&[u8]> {
        self.mbc.saved_data()
    }

    /// Loads a board's own non-volatile bytes back; `Err` is the refusal message.
    pub fn load_board_save(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.mbc.load_saved_data(bytes)
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.header.cart_type);
        w.u8(self.header.rom_size_code);
        w.u8(self.header.ram_size_code);
        w.u8(self.header.header_checksum);
        w.u32(self.rom.len() as u32);
        w.u32(self.ram.len() as u32);
        w.u32(self.rom_crc32);
        w.bytes(&self.ram);
        w.u32(mbc_id(self.mbc.name()));
        self.mbc.save_state(w);
        w.bool(self.rtc.is_some());
        if let Some(rtc) = &self.rtc {
            rtc.save_state(w);
        }
        w.bool(self.ram_dirty);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let cart_type = r.u8()?;
        let rom_size_code = r.u8()?;
        let ram_size_code = r.u8()?;
        let header_checksum = r.u8()?;
        if cart_type != self.header.cart_type
            || rom_size_code != self.header.rom_size_code
            || ram_size_code != self.header.ram_size_code
            || header_checksum != self.header.header_checksum
        {
            return Err(StateError::BadValue);
        }
        let rom_len = r.u32()? as usize;
        let ram_len = r.u32()? as usize;
        if rom_len != self.rom.len() || ram_len != self.ram.len() {
            return Err(StateError::BadValue);
        }
        if r.u32()? != self.rom_crc32 {
            return Err(StateError::BadValue);
        }
        r.bytes_into(&mut self.ram)?;
        if r.u32()? != mbc_id(self.mbc.name()) {
            return Err(StateError::BadValue);
        }
        self.mbc.load_state(r)?;
        let has_rtc = r.bool()?;
        if has_rtc {
            if let Some(rtc) = &mut self.rtc {
                rtc.load_state(r)?;
            } else {
                return Err(StateError::BadValue);
            }
        }
        // §6: `ram_dirty` is written as `false` on load, so a restored state never writes a `.sav`.
        let _stored_dirty = r.bool()?;
        self.ram_dirty = false;
        if let Some(rtc) = &mut self.rtc {
            rtc.latch();
        }
        Ok(())
    }
}

/// `mbc_name` on the wire: the id §6's Cartridge row lists.
fn mbc_id(name: &str) -> u32 {
    match name {
        "romonly" => 0,
        "mbc1" => 1,
        "mbc1m" => 2,
        "huc1" => 3,
        "mbc2" => 4,
        "mbc3" => 5,
        "mbc5" => 6,
        "mbc30" => 7,
        "mbc5r" => 8,
        "mmm01" => 9,
        "mbc6" => 10,
        "mbc7" => 11,
        "huc3" => 12,
        "m161" => 13,
        _ => u32::MAX,
    }
}

/// The MBC3 clock. One owner, `Cartridge::rtc`; a mapper only answers `RamAccess::Rtc(reg)`.
#[derive(Clone)]
pub struct Rtc {
    pub secs: u8,
    pub mins: u8,
    pub hours: u8,
    pub days_low: u8,
    pub days_high: u8,
    pub latched: [u8; 5],
    /// Wall clock when the live counters were last advanced.
    pub base_ms: u64,
}

impl Rtc {
    pub fn new() -> Rtc {
        Rtc {
            secs: 0,
            mins: 0,
            hours: 0,
            days_low: 0,
            days_high: 0,
            latched: [0; 5],
            base_ms: 0,
        }
    }

    /// The 9-bit day counter, `DH` bit 0 being bit 8 (`MBC3.html`, "The Day Counter").
    fn day_count(&self) -> u16 {
        u16::from(self.days_high & 0x01) << 8 | u16::from(self.days_low)
    }

    /// Adds the elapsed milliseconds, obeying `DH` bit 6 (halt, "0=Active, 1=Stop Timer"). The
    /// 9-bit day counter wraps and sets the carry bit, which stays set until the program clears it.
    pub fn advance(&mut self, now_ms: u64) {
        // The first tick only establishes the epoch: a cartridge powered on at `now_ms` starts its
        // counters from the values the program has written, not from 1970.
        if self.base_ms == 0 {
            self.base_ms = now_ms;
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(self.base_ms);
        if self.days_high & 0x40 != 0 {
            // Halted: the time is dropped, not banked for when the program starts the clock again.
            self.base_ms = now_ms;
            return;
        }
        let elapsed = elapsed_ms / 1000;
        if elapsed == 0 {
            // `tick_rtc` runs once per frame, so `base_ms` may only move a whole second at a time;
            // moving it every call would throw the remainder away and the clock would never count.
            return;
        }
        self.base_ms += elapsed * 1000;
        let mut total = u64::from(self.day_count()) * 86_400
            + u64::from(self.hours) * 3_600
            + u64::from(self.mins) * 60
            + u64::from(self.secs)
            + elapsed;
        let mut days = total / 86_400;
        total %= 86_400;
        let mut carry = self.days_high & 0x80;
        if days > 0x1FF {
            days &= 0x1FF;
            carry = 0x80;
        }
        self.secs = (total % 60) as u8;
        self.mins = (total / 60 % 60) as u8;
        self.hours = (total / 3_600) as u8;
        self.days_low = days as u8;
        self.days_high = carry | (self.days_high & 0x40) | (days >> 8) as u8;
    }

    /// Live -> `latched`, which is what reads return.
    pub fn latch(&mut self) {
        self.latched = [
            self.secs,
            self.mins,
            self.hours,
            self.days_low,
            self.days_high,
        ];
    }

    pub fn read(&self, reg: u8) -> u8 {
        self.latched.get(reg as usize).copied().unwrap_or(0xFF)
    }

    /// Writes the live registers, not the latched copy. `DH` keeps only the day's top bit, the halt
    /// flag and the carry bit.
    pub fn write(&mut self, reg: u8, value: u8) {
        match reg {
            0 => self.secs = value,
            1 => self.mins = value,
            2 => self.hours = value,
            3 => self.days_low = value,
            4 => self.days_high = value & 0xC1,
            _ => {}
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.secs);
        w.u8(self.mins);
        w.u8(self.hours);
        w.u8(self.days_low);
        w.u8(self.days_high);
        w.u64(self.base_ms);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.secs = r.u8()?;
        self.mins = r.u8()?;
        self.hours = r.u8()?;
        self.days_low = r.u8()?;
        self.days_high = r.u8()? & 0xC1;
        self.base_ms = r.u64()?;
        Ok(())
    }
}

impl Default for Rtc {
    fn default() -> Self {
        Rtc::new()
    }
}
