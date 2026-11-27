use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 87 (J87): 32 KB of fixed PRG and an 8 KB CHR bank selected by writes to $6000-$7FFF. The
/// register sits in the PRG RAM window, so the board has no PRG RAM at all. The two low register
/// bits are crossed — bit 0 is the *high* bit of the CHR bank number and bit 1 the low one — which
/// is why a dump read with the CHR bit order reversed is mapper 101 instead. No bus conflicts.
pub struct J87 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    bank: usize,
    mirroring: Mirroring,
}

impl J87 {
    pub fn new(rom: RomData) -> J87 {
        J87 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            mirroring: rom.header.mirroring,
        }
    }

    fn chr_index(&self, addr: u16) -> usize {
        self.bank * 0x2000 + usize::from(addr & 0x1FFF)
    }
}

impl Mapper for J87 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg[usize::from(addr - 0x8000) % self.prg.len()],
            _ => 0,
        }
    }

    /// `$6000-$7FFF` holds the CHR bank register, not RAM: bits 0-1 enter it crossed, so the value
    /// 1 selects CHR bank 2 and 2 selects bank 1.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x6000..=0x7FFF).contains(&addr) {
            self.bank = usize::from(((val & 1) << 1) | ((val >> 1) & 1));
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

    /// CHR is ROM on the J87 boards in the wild; a CHR-RAM upload still works.
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
