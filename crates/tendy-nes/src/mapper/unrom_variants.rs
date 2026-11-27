use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// PRG bank size: both boards switch a 16 KB window.
const PRG_BANK: usize = 0x4000;

/// Mapper 94 (UN1ROM): a 16 KB PRG bank at $8000-$BFFF with the last bank fixed at $C000, exactly
/// like UxROM except that the register is shifted by two bits — write value 0x0C selects bank 3, not
/// bank 12. CHR is 8 KB of RAM. The board has no conflict-prevention circuit, so a write latches the
/// value ANDed with the PRG byte at the written address.
pub struct Un1rom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    bank: usize,
    mirroring: Mirroring,
}

impl Un1rom {
    pub fn new(rom: RomData) -> Un1rom {
        Un1rom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            mirroring: rom.header.mirroring,
        }
    }

    fn banks(&self) -> usize {
        (self.prg.len() / PRG_BANK).max(1)
    }

    /// Start of the PRG bank a written address falls in, for the bus conflict: the selected bank
    /// below $C000, the fixed last one above it.
    fn base(&self, addr: u16) -> usize {
        if addr < 0xC000 {
            (self.bank % self.banks()) * PRG_BANK
        } else {
            (self.banks() - 1) * PRG_BANK
        }
    }
}

impl Mapper for Un1rom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => {
                self.prg[(self.bank % banks) * PRG_BANK + usize::from(addr - 0x8000)]
            }
            0xC000..=0xFFFF => self.prg[(banks - 1) * PRG_BANK + usize::from(addr - 0xC000)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            let val = val & self.prg[self.base(addr) + usize::from(addr & 0x3FFF)];
            self.bank = usize::from((val >> 2) & 0x07);
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF. The selected 16 KB bank sits at $8000, the fixed last one at
    /// $C000, exactly as `cpu_read` maps them.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => Some((self.bank % banks) * PRG_BANK + usize::from(addr - 0x8000)),
            0xC000..=0xFFFF => Some((banks - 1) * PRG_BANK + usize::from(addr - 0xC000)),
            _ => None,
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, usize::from(addr & 0x1FFF))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
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
        chr_ram_slice(&self.chr, self.chr_is_ram)
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        if self.chr_is_ram {
            copy_prefix(&mut self.chr, data);
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.bank);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.usize()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

/// Mapper 180 (UxROM inverted): 16 KB of PRG fixed to the *first* bank at $8000-$BFFF and a 16 KB
/// bank selected by bits 0-2 of a write to $8000-$FFFF at $C000-$FFFF — the AND-logic mirror image
/// of UxROM's fixed last bank. CHR is 8 KB of RAM, and the board has bus conflicts, so a write
/// latches the value ANDed with the PRG byte at the written address.
pub struct UnromInverted {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    bank: usize,
    mirroring: Mirroring,
}

impl UnromInverted {
    pub fn new(rom: RomData) -> UnromInverted {
        UnromInverted {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            mirroring: rom.header.mirroring,
        }
    }

    fn banks(&self) -> usize {
        (self.prg.len() / PRG_BANK).max(1)
    }

    /// Start of the PRG bank a written address falls in, for the bus conflict: the fixed first bank
    /// below $C000, the selected bank above it.
    fn base(&self, addr: u16) -> usize {
        if addr < 0xC000 {
            0
        } else {
            (self.bank % self.banks()) * PRG_BANK
        }
    }
}

impl Mapper for UnromInverted {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => self.prg[usize::from(addr - 0x8000)],
            0xC000..=0xFFFF => {
                self.prg[(self.bank % banks) * PRG_BANK + usize::from(addr - 0xC000)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            let val = val & self.prg[self.base(addr) + usize::from(addr & 0x3FFF)];
            self.bank = usize::from(val & 0x07);
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF. The fixed first 16 KB bank sits at $8000, the selected one at
    /// $C000, exactly as `cpu_read` maps them.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        let banks = self.banks();
        match addr {
            0x8000..=0xBFFF => Some(usize::from(addr - 0x8000)),
            0xC000..=0xFFFF => Some((self.bank % banks) * PRG_BANK + usize::from(addr - 0xC000)),
            _ => None,
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, usize::from(addr & 0x1FFF))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
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
        chr_ram_slice(&self.chr, self.chr_is_ram)
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        if self.chr_is_ram {
            copy_prefix(&mut self.chr, data);
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.bank);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.usize()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}
