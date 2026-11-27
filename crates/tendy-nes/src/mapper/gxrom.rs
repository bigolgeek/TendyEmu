use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 66 (GxROM): writes to $8000-$FFFF select both banks — bits 4-5 the 32 KB PRG bank and
/// bits 0-1 the 8 KB CHR bank. Mirroring is always the header value and never changes.
const PRG_WINDOW: usize = 0x8000;
const CHR_WINDOW: usize = 0x2000;

pub struct Gxrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    bank: u8,
    chr_bank: u8,
    mirroring: Mirroring,
}

impl Gxrom {
    pub fn new(rom: RomData) -> Gxrom {
        Gxrom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            chr_bank: 0,
            mirroring: rom.header.mirroring,
        }
    }
}

impl Mapper for Gxrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                let banks = self.prg.len() / PRG_WINDOW;
                let start = usize::from(self.bank) % banks * PRG_WINDOW;
                self.prg[start + usize::from(addr - 0x8000)]
            }
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| {
            usize::from(self.bank) % (self.prg.len() / PRG_WINDOW) * PRG_WINDOW
                + usize::from(addr - 0x8000)
        })
    }

    /// Bits 4-5 select the 32 KB PRG bank; bits 0-1 select the 8 KB CHR bank.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            self.bank = (val >> 4) & 0x03;
            self.chr_bank = val & 0x03;
        }
    }

    /// CHR-RAM window at the board's current 8 KB CHR bank.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(
            &self.chr,
            usize::from(self.chr_bank) * CHR_WINDOW + usize::from(addr & 0x1FFF),
        )
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(
            &self.chr,
            self.chr_is_ram,
            usize::from(self.chr_bank) * CHR_WINDOW + usize::from(addr & 0x1FFF),
        )
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// Writes land in CHR-RAM at the selected bank; CHR-ROM boards ignore them.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            chr_write(
                &mut self.chr,
                usize::from(self.chr_bank) * CHR_WINDOW + usize::from(addr & 0x1FFF),
                val,
            );
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

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
        w.u8(self.bank);
        w.u8(self.chr_bank);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.u8()?;
        self.chr_bank = r.u8()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvram_board(chr_is_ram: bool) -> Gxrom {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Gxrom::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 66,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr,
            chr_is_ram,
            trainer: None,
        })
    }

    #[test]
    fn chr_ram_is_exposed_for_saving() {
        let mut m = nvram_board(true);
        m.ppu_write(0x0010, 0x44);
        assert_eq!(m.chr_ram().len(), 8192);
        assert_eq!(m.chr_ram()[0x10], 0x44);
    }

    #[test]
    fn chr_rom_is_not_exposed_for_saving() {
        let m = nvram_board(false);
        assert!(m.chr_ram().is_empty());
    }

    #[test]
    fn load_chr_ram_copies() {
        let mut m = nvram_board(true);
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0002), 7);
        assert_eq!(m.ppu_read(0x0003), 0);
    }

    #[test]
    fn load_chr_ram_is_ignored_for_chr_rom() {
        let mut m = nvram_board(false);
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0000), 0x11);
    }
}
