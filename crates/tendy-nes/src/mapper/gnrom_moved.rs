use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// PRG window: both boards switch a whole 32 KB bank.
const PRG_WINDOW: usize = 0x8000;
/// CHR window: both boards switch a whole 8 KB bank.
const CHR_WINDOW: usize = 0x2000;

/// Which board a [`GnromMoved`] is: they differ only in where the latch listens and in which nibble
/// of the written byte holds which bank.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// nesdev "INES Mapper 038": latch at `$7000-$7FFF`, PRG bank in bits 0-1, CHR bank in bits 4-5.
    BitCorp,
    /// nesdev "INES Mapper 140": latch at `$6000-$7FFF`, PRG bank in bits 4-5, CHR bank in bits 0-3.
    JalecoJf,
}

/// Mappers 38 (Bit Corp. UNL-PCI556) and 140 (Jaleco JF-11/JF-14): GNROM's (66) single latch — bits
/// of one write select a 32 KB PRG bank and an 8 KB CHR bank — with the writable port moved off
/// `$8000`, down to `$7000` on Bit Corp. and `$6000` on Jaleco, so neither board has PRG RAM.
/// Mirroring is the header's and never changes. Neither board has bus conflicts.
///
/// Mapper 38 also bankswitches on writes to `$F000-$FFFF` on real hardware (nesdev: "very likely");
/// well-behaved code never writes there, so only `$7000-$7FFF` is decoded — `shortcut:` a game that
/// bankswitches by writing `$F000-$FFFF`, add that second port.
pub struct GnromMoved {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    kind: Kind,
    prg_bank: usize,
    chr_bank: usize,
    mirroring: Mirroring,
}

impl GnromMoved {
    pub fn new(rom: RomData, kind: Kind) -> GnromMoved {
        GnromMoved {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            kind,
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

impl Mapper for GnromMoved {
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
        match self.kind {
            Kind::BitCorp if (0x7000..=0x7FFF).contains(&addr) => {
                self.prg_bank = usize::from(val & 0x03);
                self.chr_bank = usize::from((val >> 4) & 0x03);
            }
            Kind::JalecoJf if (0x6000..=0x7FFF).contains(&addr) => {
                self.prg_bank = usize::from((val >> 4) & 0x03);
                self.chr_bank = usize::from(val & 0x0F);
            }
            _ => {}
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

    /// Both boards are CHR-ROM; a CHR-RAM upload still lands in the selected bank.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let index = self.chr_index(addr);
            chr_write(&mut self.chr, index, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// Neither board has PRG RAM: the latch lives in the $6000-$7FFF window.
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
