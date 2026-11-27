use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 34. Two boards share the number: **BNROM** (PRG-RAM-free, CHR-RAM) where any write to
/// $8000-$FFFF selects the 32 KB PRG bank with all eight bits, and **NINA-001**, whose registers
/// live at the top of an 8 KB PRG RAM at $6000: writes to $7FFD-$7FFF both land in the RAM and act
/// as the bank registers — $7FFD bit 0 picks the 32 KB PRG bank, $7FFE picks the 4 KB CHR page at
/// PPU $0000 and $7FFF the one at PPU $1000. Mirroring is always the header value.
pub struct Bnrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// 8 KB of RAM at $6000 on the NINA board; empty on BNROM.
    prg_ram: Vec<u8>,
    nina: bool,
    bank: u8,
    chr_bank: [u8; 2],
    mirroring: Mirroring,
}

impl Bnrom {
    pub fn new(rom: RomData) -> Self {
        let nina =
            rom.header.submapper == 1 || (rom.header.submapper == 0 && rom.header.chr_banks > 1);
        Bnrom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: if nina { vec![0; 8192] } else { Vec::new() },
            nina,
            bank: 0,
            chr_bank: [0, 0],
            mirroring: rom.header.mirroring,
        }
    }

    fn prg_bank_start(&self) -> usize {
        usize::from(self.bank) % (self.prg.len() / PRG_WINDOW) * PRG_WINDOW
    }

    /// Byte offset into CHR for a PPU address in 0x0000..=0x1FFF; `chr_read`/`chr_write` wrap it
    /// to the size the board allocated.
    fn chr_index(&self, addr: u16) -> usize {
        if self.nina {
            let reg = usize::from(addr >> 12 == 1);
            let page = usize::from(self.chr_bank[reg]) % (self.chr.len() / CHR_PAGE).max(1);
            page * CHR_PAGE + usize::from(addr & 0x0FFF)
        } else {
            usize::from(addr & 0x1FFF)
        }
    }
}

const PRG_WINDOW: usize = 0x8000;
const CHR_PAGE: usize = 0x1000;

impl Mapper for Bnrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF if self.nina => self.prg_ram[usize::from(addr - 0x6000)],
            0x8000..=0xFFFF => self.prg[self.prg_bank_start() + usize::from(addr - 0x8000)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_bank_start() + usize::from(addr - 0x8000))
    }

    /// BNROM: any $8000-$FFFF write sets the 32 KB PRG bank. NINA: writes in $6000-$7FFF land in
    /// the RAM, and $7FFD-$7FFF are the bank registers.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if self.nina && (0x6000..=0x7FFF).contains(&addr) {
            self.prg_ram[usize::from(addr - 0x6000)] = val;
            if addr < 0x7FFD {
                return;
            }
        }
        match addr {
            0x8000..=0xFFFF if !self.nina => self.bank = val,
            0x7FFD => self.bank = val & 0x01,
            0x7FFE => self.chr_bank[0] = val,
            0x7FFF => self.chr_bank[1] = val,
            _ => {}
        }
    }

    /// BxROM/before NINA: the peek follows the same `chr_index` mapping the PPU reads.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_index(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_index(addr))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let i = self.chr_index(addr);
            chr_write(&mut self.chr, i, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn prg_ram(&self) -> &[u8] {
        &self.prg_ram
    }

    fn load_prg_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.prg_ram, data);
    }

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
        if self.nina {
            w.u8(self.chr_bank[0]);
            w.u8(self.chr_bank[1]);
            w.bytes(&self.prg_ram);
        }
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.u8()?;
        if self.nina {
            self.chr_bank[0] = r.u8()?;
            self.chr_bank[1] = r.u8()?;
            r.bytes_into(&mut self.prg_ram)?;
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

    fn nvram_board(chr_is_ram: bool) -> Bnrom {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Bnrom::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 34,
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
