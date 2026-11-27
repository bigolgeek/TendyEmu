use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// PRG window: the board switches a whole 32 KB bank.
const PRG_WINDOW: usize = 0x8000;
/// CHR window: the board switches a whole 8 KB bank.
const CHR_WINDOW: usize = 0x2000;

/// Mapper 79 (NINA-03/NINA-06, and the functionally identical Sachen SA-016-1M): one latch in the
/// `$4100-$5FFF` range — nesdev's decode `010x xxx1 xxxx xxxx`, i.e. `addr & 0xE100 == 0x4100` —
/// with bits 0-2 selecting the 8 KB CHR bank and bit 3 the 32 KB PRG bank. The board has no PRG RAM
/// and no bus conflicts. Mirroring is the header's (solder pads) and never changes.
pub struct Nina {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_bank: usize,
    chr_bank: usize,
    mirroring: Mirroring,
}

impl Nina {
    pub fn new(rom: RomData) -> Nina {
        Nina {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_bank: 0,
            chr_bank: 0,
            mirroring: rom.header.mirroring,
        }
    }

    /// 32 KB bank count, at least one so the modulo never divides by zero on a short image.
    fn banks(&self) -> usize {
        (self.prg.len() / PRG_WINDOW).max(1)
    }

    fn prg_index(&self, addr: u16) -> usize {
        (self.prg_bank % self.banks()) * PRG_WINDOW + usize::from(addr - 0x8000)
    }

    fn chr_index(&self, addr: u16) -> usize {
        self.chr_bank * CHR_WINDOW + usize::from(addr & 0x1FFF)
    }
}

impl Mapper for Nina {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg[self.prg_index(addr)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank register;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_index(addr))
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr & 0xE100 == 0x4100 {
            self.chr_bank = usize::from(val & 0x07);
            self.prg_bank = usize::from((val >> 3) & 0x01);
        }
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

    /// The board is CHR-ROM; a CHR-RAM upload still lands in the selected bank.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let index = self.chr_index(addr);
            chr_write(&mut self.chr, index, val);
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
        chr_ram_slice(&self.chr, self.chr_is_ram)
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        if self.chr_is_ram {
            copy_prefix(&mut self.chr, data);
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.prg_bank);
        w.usize(self.chr_bank);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.prg_bank = r.usize()?;
        self.chr_bank = r.usize()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}
