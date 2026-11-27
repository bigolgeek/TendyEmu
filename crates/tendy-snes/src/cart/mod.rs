//! The cartridge: header parse, board pick, SRAM and the read/write dispatch.
//!
//! The three maps are pure address arithmetic in [`map`]; this file owns only the three-way
//! `match` on the bank and the refusal messages, so no mapper holds state in v1.

pub mod chipset;
pub mod header;
pub mod map;
pub mod refusal;

pub use chipset::Chipset;
pub use header::Header;
pub use refusal::{CHECKSUM_NOTICE, PAL_NOTICE, Refusal};

use crate::addr::Addr;
use crate::state::{StateError, StateReader, StateWriter};

/// A loaded cartridge image.
#[derive(Clone)]
pub struct Cartridge {
    /// Exactly the image, copier header stripped.
    pub rom: Vec<u8>,
    /// 0, 2, 8, 32 or more KiB, as the header asks.
    pub ram: Vec<u8>,
    pub header: Header,
    pub board: Board,
    pub chipset: Chipset,
    /// Keys the `.srm` and the state (§8.4, §9).
    pub rom_crc32: u32,
    pub ram_dirty: bool,
    /// The image's size rounded up to a power of two, for mirroring a small cartridge.
    pub rom_mask: usize,
}

impl Cartridge {
    /// Parses the header, picks the board, allocates SRAM.
    ///
    /// `Err` is a [`Refusal`], the one place §8.4's reasons are worded: a named coprocessor board,
    /// an image that is too small, or an image whose header cannot be found. Nothing else is a
    /// refusal — a bad checksum, a PAL country code and an implausible size code are all reported
    /// by [`Cartridge::notice`], not enforced.
    pub fn new(rom: Vec<u8>) -> Result<Cartridge, Refusal> {
        // §8.1: a 512-byte SWC/UFO header is present iff `(filesize AND 3FFh) = 200h`
        // (fullsnes "SNES Cartridge ROM-Image Headers and File Extensions").
        let has_copier_header = rom.len() % 1024 == 512;
        let mut rom = if has_copier_header {
            rom.get(512..).unwrap_or(&[]).to_vec()
        } else {
            rom
        };
        if rom.len() < 0x8000 {
            return Err(Refusal::TooSmall { bytes: rom.len() });
        }
        // §8.2 pads before the header is scored, so the score sees the size the hardware maps.
        pad_to_power_of_two(&mut rom);
        let (mut header, board) = Header::parse(&rom)?;
        header.has_copier_header = has_copier_header;
        let chipset =
            Chipset::from_byte(header.chipset_byte, header.chipset_subtype).for_rom_size(rom.len());
        if !chipset.is_supported() {
            return Err(Refusal::Coprocessor {
                chipset,
                byte: header.chipset_byte,
            });
        }
        let ram_len = ram_len_of(header.ram_size_code);
        let rom_mask = rom.len() - 1;
        let rom_crc32 = crate::checksum::crc32(&rom);
        Ok(Cartridge {
            rom,
            ram: vec![0; ram_len],
            header,
            board,
            chipset,
            rom_crc32,
            ram_dirty: false,
            rom_mask,
        })
    }

    pub fn read(&mut self, addr: Addr) -> u8 {
        if let Some(i) = self.board.ram_offset(addr, self.ram.len()) {
            return if self.ram.is_empty() {
                0xFF
            } else {
                self.ram[i]
            };
        }
        let i = self.board.rom_offset(addr, self.rom_mask);
        self.rom.get(i).copied().unwrap_or(0xFF)
    }

    pub fn write(&mut self, addr: Addr, value: u8) {
        let Some(i) = self.board.ram_offset(addr, self.ram.len()) else {
            return;
        };
        if self.ram.is_empty() || self.ram[i] == value {
            return;
        }
        self.ram[i] = value;
        self.ram_dirty = true;
    }

    /// True when this board asserts the CPU's `/IRQ`. Always false in v1, kept so the decode has
    /// one place to say so.
    pub fn irq(&self) -> bool {
        false
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// The one notice this cartridge puts on the status bar, if any (§8.4, §10.3).
    ///
    /// A PAL image is playable at NTSC timing, so it loads and says so; refusing it would hide a
    /// game behind a message, and running it silently would be a lie. A header whose checksum does
    /// not agree is equally playable (§8.2) and says so whenever the region is NTSC — the bar has
    /// one notice slot, and the region is the fact that changes what the user hears.
    pub fn notice(&self) -> Option<&'static str> {
        if self.header.is_pal() {
            Some(PAL_NOTICE)
        } else if !self.header.checksum_ok {
            Some(CHECKSUM_NOTICE)
        } else {
            None
        }
    }

    /// §9.2's cartridge rows, in the table's order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.header.map_mode);
        w.u8(self.header.chipset_byte);
        w.u8(self.header.rom_size_code);
        w.u8(self.header.ram_size_code);
        w.u16(self.header.checksum);
        w.u32(self.rom_crc32);
        w.u32(self.ram.len() as u32);
        w.bytes(&self.ram);
        // `ram_dirty` is host bookkeeping, written as `false` (§9.2).
        w.bool(false);
    }

    /// Reads §9.2's cartridge rows back.
    ///
    /// The state belongs to one cartridge: the recorded identity is compared before the RAM body
    /// is accepted, and a difference is `BadValue` (§9.1 — "the ROM [...] identified by
    /// `rom_crc32`, compared on load; a different cartridge is `BadValue`"). The length is
    /// compared before it is used, so a corrupt state cannot resize the chip.
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        let map_mode = r.u8()?;
        let chipset_byte = r.u8()?;
        let rom_size_code = r.u8()?;
        let ram_size_code = r.u8()?;
        let checksum = r.u16()?;
        let crc = r.u32()?;
        let len = r.u32()? as usize;
        if crc != self.rom_crc32
            || map_mode != self.header.map_mode
            || chipset_byte != self.header.chipset_byte
            || rom_size_code != self.header.rom_size_code
            || ram_size_code != self.header.ram_size_code
            || checksum != self.header.checksum
            || len != self.ram.len()
        {
            return Err(StateError::BadValue);
        }
        r.bytes_into(&mut self.ram)?;
        let _dirty = r.bool()?;
        Ok(())
    }
}

/// The three memory maps v1 decodes. A coprocessor board is refused at load, so it never gets here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Board {
    LoRom,
    HiRom,
    ExHiRom,
}

impl Board {
    pub fn name(self) -> &'static str {
        match self {
            Board::LoRom => "LoROM",
            Board::HiRom => "HiROM",
            Board::ExHiRom => "ExHiROM",
        }
    }

    /// The address inside `rom` that this board maps `addr` to, already masked to the image.
    pub fn rom_offset(self, addr: Addr, mask: usize) -> usize {
        map::rom_offset(self, addr, mask)
    }

    /// The SRAM index this board puts `addr` in, or `None` outside an SRAM window.
    pub fn ram_offset(self, addr: Addr, ram_len: usize) -> Option<usize> {
        map::ram_offset(self, addr, ram_len)
    }
}

/// `$FFD8`: `1 << n` KiB, 0 for none (fullsnes: "usually 1=2Kbyte .. 5=32Kbyte").
///
/// The cap is v1's: a corrupt byte must not allocate gigabytes, and one power of two above the
/// largest licensed chip leaves room for the 64 KiB boards the SRAM window's bank term serves.
fn ram_len_of(code: u8) -> usize {
    if code == 0 {
        0
    } else {
        1024usize << code.min(7)
    }
}

/// §8.2: a non-power-of-two image repeats its last power-of-two-sized chunk until it is one, which
/// is what the hardware's mapping does for a 10 Mbit cartridge ("mapped (and checksummed) as
/// 8Mbit + 4x2Mbit", fullsnes "ROM Size / Checksum Notes").
fn pad_to_power_of_two(rom: &mut Vec<u8>) {
    let len = rom.len();
    if len == 0 || len.is_power_of_two() {
        return;
    }
    let chunk = 1usize << (usize::BITS - 1 - len.leading_zeros());
    let target = chunk * 2;
    let tail = rom[len - chunk..].to_vec();
    while rom.len() < target {
        let need = (target - rom.len()).min(tail.len());
        rom.extend_from_slice(&tail[..need]);
    }
}
