use crate::cart::{Mapper, Mirroring, RomData, chr_read, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 3 (CNROM): fixed PRG (16 KB mirrored or 32 KB) and an 8 KB CHR bank selected by writes
/// to $8000-$FFFF. NES 2.0 submapper 2 marks boards without conflict prevention: such a write
/// latches the value ANDed with the PRG ROM byte at the written address; submappers 0 and 1 latch
/// all eight bits.
pub struct Cnrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    prg_ram: Vec<u8>,
    bank: usize,
    /// True only on NES 2.0 submapper 2, where register writes conflict with PRG ROM.
    bus_conflicts: bool,
    mirroring: Mirroring,
    /// `prg.len() - 1` when that length is a power of two — every real CNROM image — so a CPU read
    /// masks instead of dividing. `None` keeps `%` for an odd NES 2.0 size.
    prg_mask: Option<usize>,
}

impl Cnrom {
    pub fn new(rom: RomData) -> Cnrom {
        let prg_mask = rom.prg.len().is_power_of_two().then(|| rom.prg.len() - 1);
        Cnrom {
            prg: rom.prg,
            chr: rom.chr,
            prg_ram: vec![0; 8192],
            bank: 0,
            bus_conflicts: rom.header.nes2 && rom.header.submapper == 2,
            mirroring: rom.header.mirroring,
            prg_mask,
        }
    }

    /// Byte offset into PRG-ROM for a CPU address in 0x8000..=0xFFFF; the image mirrors to fill the
    /// 32 KB window.
    fn prg_index(&self, addr: u16) -> usize {
        let i = usize::from(addr - 0x8000);
        match self.prg_mask {
            Some(mask) => i & mask,
            None => i % self.prg.len(),
        }
    }

    fn chr_index(&self, addr: u16) -> usize {
        let banks = (self.chr.len() / 0x2000).max(1);
        (self.bank % banks) * 0x2000 + usize::from(addr & 0x1FFF)
    }
}

impl Mapper for Cnrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr - 0x6000)],
            0x8000..=0xFFFF => self.prg[self.prg_index(addr)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_index(addr))
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr - 0x6000)] = val,
            0x8000..=0xFFFF => {
                // Submapper 2: the write is ANDed with the PRG ROM byte at the written address.
                let val = if self.bus_conflicts {
                    val & self.prg[self.prg_index(addr)]
                } else {
                    val
                };
                self.bank = usize::from(val);
            }
            _ => {}
        }
    }

    /// CNROM's CHR bank register is the only thing between $0000 and the CHR image.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_index(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        // CNROM's CHR is always ROM: the bank register wraps onto the image.
        crate::cart::chr_rom_index(&self.chr, false, self.chr_index(addr))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// CHR is ROM on CNROM boards, so pattern writes are ignored.
    fn ppu_write(&mut self, _addr: u16, _val: u8) {}

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.bank);
        w.bytes(&self.prg_ram);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.usize()?;
        r.bytes_into(&mut self.prg_ram)?;
        Ok(())
    }

    fn prg_ram(&self) -> &[u8] {
        &self.prg_ram
    }

    fn load_prg_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.prg_ram, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    fn board(prg_size: usize) -> Cnrom {
        Cnrom::new(RomData {
            header: Header {
                mapper: 3,
                ..Default::default()
            },
            prg: (0..prg_size).map(|i| (i % 251) as u8).collect(),
            chr: vec![0x11; 8192],
            chr_is_ram: false,
            trainer: None,
        })
    }

    /// A power-of-two PRG masks, an odd NES 2.0 one falls back to `%`; both mirror the same.
    #[test]
    fn prg_mirroring_is_the_same_for_odd_sizes() {
        for prg_size in [16384usize, 32768, 49152] {
            let mut m = board(prg_size);
            assert_eq!(
                m.prg_mask.is_some(),
                prg_size.is_power_of_two(),
                "{prg_size} bytes"
            );
            for addr in (0x8000..=0xFFFFu16).step_by(7) {
                let want = usize::from(addr - 0x8000) % prg_size;
                assert_eq!(m.cpu_read(addr), (want % 251) as u8, "read {addr:04X}");
                assert_eq!(m.prg_rom_offset(addr), Some(want), "offset {addr:04X}");
            }
        }
    }

    #[test]
    fn chr_bank_register_selects_an_8k_page_and_wraps_on_a_small_image() {
        let mut m = Cnrom::new(RomData {
            header: Header {
                mapper: 3,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr: (0..32768).map(|i| (i % 251) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        });
        m.cpu_write(0x8000, 2);
        assert_eq!(m.chr_index(0x1FFF), 2 * 0x2000 + 0x1FFF);
        assert_eq!(m.ppu_read(0x1FFF), ((2 * 0x2000 + 0x1FFF) % 251) as u8);
        // An 8 KB image has a single page, so the register wraps onto it.
        let mut one_page = board(32768);
        one_page.cpu_write(0x8000, 2);
        assert_eq!(one_page.chr_index(0x1FFF), 0x1FFF);
    }
}
