use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Size of one CHR page on both boards: the window each 4 KB bank register switches.
const CHR_PAGE: usize = 0x1000;

/// Mapper 184 (Sunsoft-1): 32 KB of fixed PRG and two 4 KB CHR windows pointed by writes to
/// $6000-$7FFF — bits 0-2 select the page at PPU $0000-$0FFF, bits 4-5 the one at $1000-$1FFF.
/// The Sunsoft-1 IC leaves data bit 6 unconnected and ties it high, so the upper window only ever
/// shows CHR pages 4-7 (`4 | bits 4-5`); on the 8 KB CHR-ROM the board shipped with, that wraps to
/// the two pages it has. No bus conflicts.
pub struct Sunsoft1 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// The 4 KB page at PPU $0000-$0FFF: bits 0-2 of the last register write.
    low: usize,
    /// The 4 KB page at PPU $1000-$1FFF: bits 4-5 with the chip's bit 6 tied high, so 4 through 7.
    high: usize,
    mirroring: Mirroring,
}

impl Sunsoft1 {
    pub fn new(rom: RomData) -> Sunsoft1 {
        Sunsoft1 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            low: 0,
            high: 0x04,
            mirroring: rom.header.mirroring,
        }
    }

    fn chr_index(&self, addr: u16) -> usize {
        let page = if addr < CHR_PAGE as u16 {
            self.low
        } else {
            self.high
        };
        page * CHR_PAGE + usize::from(addr & 0x0FFF)
    }
}

impl Mapper for Sunsoft1 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg[usize::from(addr - 0x8000) % self.prg.len()],
            _ => 0,
        }
    }

    /// `$6000-$7FFF` holds the register, not RAM: bits 0-2 are the low page, bits 4-5 the high one
    /// (data bit 6, which the chip never receives, is the high page's third bit and always set).
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x6000..=0x7FFF).contains(&addr) {
            self.low = usize::from(val & 0x07);
            self.high = 0x04 | usize::from((val >> 4) & 0x03);
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF. PRG is 32 KB fixed, so every address wraps over the whole image
    /// exactly as `cpu_read` does.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| usize::from(addr - 0x8000) % self.prg.len())
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_index(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_index(addr))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// The board is CHR-ROM; a CHR-RAM upload still works.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let index = self.chr_index(addr);
            chr_write(&mut self.chr, index, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// The board has no PRG RAM: $6000-$7FFF is the register.
    fn prg_ram(&self) -> &[u8] {
        &[]
    }

    fn load_prg_ram(&mut self, _data: &[u8]) {}

    fn chr_ram(&self) -> &[u8] {
        chr_ram_slice(&self.chr, self.chr_is_ram)
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        if self.chr_is_ram {
            copy_prefix(&mut self.chr, data);
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.low);
        w.usize(self.high);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.low = r.usize()?;
        self.high = r.usize()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

/// Mapper 93 (Sunsoft-2 on the Sunsoft-3R board): a 16 KB PRG bank selected by bits 4-6 of a write
/// to $8000-$FFFF at $8000-$BFFF, the last bank fixed at $C000, and 8 KB of CHR-RAM that the same
/// write enables with bit 0. While bit 0 is clear the RAM ignores writes and reads back as this
/// crate's open-bus value, 0. The board has no conflict-prevention circuit, so the write is ANDed
/// with the PRG byte at the written address.
pub struct Sunsoft2 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    bank: usize,
    /// Bit 0 of the last register write: false means the CHR-RAM is disabled.
    chr_enabled: bool,
    mirroring: Mirroring,
}

impl Sunsoft2 {
    pub fn new(rom: RomData) -> Sunsoft2 {
        Sunsoft2 {
            prg: rom.prg,
            chr: rom.chr,
            bank: 0,
            chr_enabled: true,
            mirroring: rom.header.mirroring,
        }
    }

    /// PRG bank count, at least one so the modulo never divides by zero on an empty image.
    fn banks(&self) -> usize {
        (self.prg.len() / 0x4000).max(1)
    }

    /// The PRG byte at a written address, which the latch ANDs the register value with.
    fn prg_byte(&self, addr: u16) -> u8 {
        let base = if addr < 0xC000 {
            (self.bank % self.banks()) * 0x4000
        } else {
            (self.banks() - 1) * 0x4000
        };
        self.prg[base + usize::from(addr & 0x3FFF)]
    }
}

impl Mapper for Sunsoft2 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => self.prg[(self.bank % banks) * 0x4000 + usize::from(addr - 0x8000)],
            0xC000..=0xFFFF => self.prg[(banks - 1) * 0x4000 + usize::from(addr - 0xC000)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            let val = val & self.prg_byte(addr);
            self.bank = usize::from((val >> 4) & 0x07);
            self.chr_enabled = val & 0x01 != 0;
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF. The selected 16 KB bank sits at $8000, the fixed last one at
    /// $C000, exactly as `cpu_read` maps them.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => Some((self.bank % banks) * 0x4000 + usize::from(addr - 0x8000)),
            0xC000..=0xFFFF => Some((banks - 1) * 0x4000 + usize::from(addr - 0xC000)),
            _ => None,
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        if self.chr_enabled {
            chr_read(&self.chr, usize::from(addr & 0x1FFF))
        } else {
            0
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// CHR is 8 KB of RAM, and it only takes writes while bit 0 of the register is set.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_enabled {
            chr_write(&mut self.chr, usize::from(addr & 0x1FFF), val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// The board has no PRG RAM.
    fn prg_ram(&self) -> &[u8] {
        &[]
    }

    fn load_prg_ram(&mut self, _data: &[u8]) {}

    fn chr_ram(&self) -> &[u8] {
        &self.chr
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.chr, data);
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.bank);
        w.u8(u8::from(self.chr_enabled));
        w.bytes(&self.chr);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.usize()?;
        self.chr_enabled = r.u8()? != 0;
        r.bytes_into(&mut self.chr)?;
        Ok(())
    }
}
