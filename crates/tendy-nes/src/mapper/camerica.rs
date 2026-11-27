use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

const PRG_PAGE: usize = 0x4000;

/// Mapper 71 (Camerica): $C000-$FFFF writes select a 16 KB PRG bank at $8000-$BFFF while the last
/// 16 KB bank stays fixed at $C000-$FFFF, and the board has 8 KB of CHR-RAM. Only NES 2.0
/// submapper 1 (GoldStar or Aladdin) supports the mirroring control: writes to $8000-$9FFF with
/// bit 4 set switch single-screen Upper, clear it switch Lower. On submapper 0 the header's
/// mirroring is immutable.
pub struct Camerica {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    bank: u8,
    /// True only on submapper 1, where $8000-$9FFF bit 4 drives the mirroring.
    mirroring_control: bool,
    mirroring: Mirroring,
}

impl Camerica {
    pub fn new(rom: RomData) -> Camerica {
        Camerica {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            mirroring_control: rom.header.submapper == 1,
            mirroring: rom.header.mirroring,
        }
    }

    fn prg_bank_start(&self) -> usize {
        usize::from(self.bank) % (self.prg.len() / PRG_PAGE) * PRG_PAGE
    }
}

impl Mapper for Camerica {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => self.prg[self.prg_bank_start() + usize::from(addr - 0x8000)],
            0xC000..=0xFFFF => self.prg[self.prg.len() - PRG_PAGE + usize::from(addr - 0xC000)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        match addr {
            0x8000..=0xBFFF => Some(self.prg_bank_start() + usize::from(addr - 0x8000)),
            0xC000..=0xFFFF => Some(self.prg.len() - PRG_PAGE + usize::from(addr - 0xC000)),
            _ => None,
        }
    }

    /// Any $C000-$FFFF write sets the switchable 16 KB bank. Submapper 1 additionally reads
    /// bit 4 of $8000-$9FFF writes as the single-screen half.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0xC000..=0xFFFF).contains(&addr) {
            self.bank = val;
        } else if self.mirroring_control && (0x8000..=0x9FFF).contains(&addr) {
            self.mirroring = if val & 0x10 == 0 {
                Mirroring::SingleScreenLower
            } else {
                Mirroring::SingleScreenUpper
            };
        }
    }

    /// Camerica has no CHR banking (CHR-RAM), so the peek is the read without any side effect.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, usize::from(addr & 0x1FFF))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// Camerica boards have CHR-RAM; pattern writes land there when CHR is RAM, and are otherwise
    /// ignored.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            chr_write(&mut self.chr, usize::from(addr & 0x1FFF), val);
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

    /// The mirroring byte is state only on submapper 1; on submapper 0 the header value is carried
    /// by the fresh mapper itself.
    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.bank);
        if self.mirroring_control {
            w.bool(self.mirroring == Mirroring::SingleScreenUpper);
        }
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.u8()?;
        if self.mirroring_control {
            self.mirroring = if r.bool()? {
                Mirroring::SingleScreenUpper
            } else {
                Mirroring::SingleScreenLower
            };
        }
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvram_board(chr_is_ram: bool) -> Camerica {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Camerica::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 71,
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
