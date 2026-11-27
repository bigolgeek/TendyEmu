use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Latch values: which of the two tiles is being fetched.
const TILE_FD: u8 = 0xFD;
const TILE_FE: u8 = 0xFE;

/// PRG window: MMC2 switches 8 KB, MMC4 a whole 16 KB.
const MMC2_PRG_BANK: usize = 0x2000;
const MMC4_PRG_BANK: usize = 0x4000;

/// Mappers 9 (MMC2, Punch-Out!!) and 10 (MMC4, Fire Emblem) are sister chips: 8 KB PRG RAM and 4 KB
/// CHR banks that switch themselves when the PPU fetches tile $FD or $FE from either pattern table.
/// They differ in three places only — the PRG window (MMC2 switches 8 KB at $8000 with the last
/// three 8 KB banks fixed at $A000-$FFFF, MMC4 switches 16 KB at $8000 with the last 16 KB fixed at
/// $C000) and the address range the left pattern table's latch reacts to (one address on MMC2, a
/// range on MMC4) — so both are this one struct, with `mmc4` picking the rules. One implementation,
/// so a fix cannot land in one chip and miss the other.
pub struct Mmc2Mmc4 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    prg_bank: u8,
    /// 4 KB CHR bank at PPU $0000 while latch 0 holds $FD / $FE.
    chr_0_fd: u8,
    chr_0_fe: u8,
    /// 4 KB CHR bank at PPU $1000 while latch 1 holds $FD / $FE.
    chr_1_fd: u8,
    chr_1_fe: u8,
    latch_0: u8,
    latch_1: u8,
    mirroring: Mirroring,
    /// Mapper 10 (MMC4) rather than mapper 9 (MMC2).
    mmc4: bool,
}

impl Mmc2Mmc4 {
    /// `mmc4` selects MMC4's 16 KB PRG banking and wide left latch; `false` is MMC2.
    pub fn new(rom: RomData, mmc4: bool) -> Mmc2Mmc4 {
        Mmc2Mmc4 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size()],
            prg_bank: 0,
            chr_0_fd: 0,
            chr_0_fe: 0,
            chr_1_fd: 0,
            chr_1_fe: 0,
            // Power-on latches are $FE, so the $C000/$E000 registers are the ones in use.
            latch_0: TILE_FE,
            latch_1: TILE_FE,
            mirroring: rom.header.mirroring,
            mmc4,
        }
    }

    fn prg_bank_size(&self) -> usize {
        if self.mmc4 {
            MMC4_PRG_BANK
        } else {
            MMC2_PRG_BANK
        }
    }

    /// Byte at `offset` of PRG bank `bank`; banks are 8 KB on MMC2 and 16 KB on MMC4.
    fn prg_at(&self, bank: usize, offset: u16) -> u8 {
        let size = self.prg_bank_size();
        debug_assert!(
            size.is_power_of_two(),
            "PRG bank size must be a power of two for the offset mask"
        );
        let banks = self.prg.len() / size;
        if banks == 0 {
            return 0;
        }
        self.prg[(bank % banks) * size + (usize::from(offset) & (size - 1))]
    }

    /// The 4 KB CHR bank the latch selects for the half of the pattern tables `addr` falls in.
    fn chr_page(&self, addr: u16) -> usize {
        let reg = if addr & 0x1000 == 0 {
            if self.latch_0 == TILE_FE {
                self.chr_0_fe
            } else {
                self.chr_0_fd
            }
        } else if self.latch_1 == TILE_FE {
            self.chr_1_fe
        } else {
            self.chr_1_fd
        };
        usize::from(reg & 0x1F)
    }

    fn chr_index(&self, addr: u16) -> usize {
        // `max(1)` keeps a board whose CHR is smaller than one 4 KiB page (or absent) out of a
        // divide-by-zero; `chr_read`/`chr_write` then wrap the index to the real CHR size, so a
        // sub-4 KiB CHR-RAM still mirrors correctly instead of reading `chr[0]` for every address.
        let banks = (self.chr.len() / 0x1000).max(1);
        (self.chr_page(addr) % banks) * 0x1000 + usize::from(addr & 0x0FFF)
    }

    /// A pattern fetch of tile $FD/$FE switched the latch for that pattern table. Called by
    /// `ppu_read` *after* the value has been taken, so the triggered fetch still uses the old bank.
    fn update_latch(&mut self, addr: u16) {
        // MMC2's left pattern table latch triggers on the exact addresses $0FD8 / $0FE8 only;
        // MMC4's on the $0FD8-$0FDF / $0FE8-$0FEF ranges. The right table uses ranges on both chips.
        match (self.mmc4, addr & 0x1FFF) {
            (false, 0x0FD8) => self.latch_0 = TILE_FD,
            (false, 0x0FE8) => self.latch_0 = TILE_FE,
            (true, 0x0FD8..=0x0FDF) => self.latch_0 = TILE_FD,
            (true, 0x0FE8..=0x0FEF) => self.latch_0 = TILE_FE,
            (_, 0x1FD8..=0x1FDF) => self.latch_1 = TILE_FD,
            (_, 0x1FE8..=0x1FEF) => self.latch_1 = TILE_FE,
            _ => {}
        }
    }
}

impl Mapper for Mmc2Mmc4 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if let 0x6000..=0x7FFF = addr {
            return self
                .prg_ram
                .get(usize::from(addr - 0x6000))
                .copied()
                .unwrap_or(0);
        }
        let banks = self.prg.len() / self.prg_bank_size();
        if self.mmc4 {
            match addr {
                0x8000..=0xBFFF => self.prg_at(usize::from(self.prg_bank & 0x0F), addr - 0x8000),
                0xC000..=0xFFFF => self.prg_at(banks.saturating_sub(1), addr - 0xC000),
                _ => 0,
            }
        } else {
            match addr {
                0x8000..=0x9FFF => self.prg_at(usize::from(self.prg_bank & 0x0F), addr - 0x8000),
                0xA000..=0xBFFF => self.prg_at(banks.saturating_sub(3), addr - 0xA000),
                0xC000..=0xDFFF => self.prg_at(banks.saturating_sub(2), addr - 0xC000),
                0xE000..=0xFFFF => self.prg_at(banks.saturating_sub(1), addr - 0xE000),
                _ => 0,
            }
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if let Some(b) = self.prg_ram.get_mut(usize::from(addr - 0x6000)) {
                    *b = val;
                }
            }
            0xA000..=0xAFFF => self.prg_bank = val & 0x0F,
            0xB000..=0xBFFF => self.chr_0_fd = val & 0x1F,
            0xC000..=0xCFFF => self.chr_0_fe = val & 0x1F,
            0xD000..=0xDFFF => self.chr_1_fd = val & 0x1F,
            0xE000..=0xEFFF => self.chr_1_fe = val & 0x1F,
            0xF000..=0xFFFF => {
                self.mirroring = if val & 1 == 0 {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                }
            }
            _ => {}
        }
    }

    /// The live 4 KiB CHR mapping for the *current* latch state. Deliberately does not call
    /// `update_latch`: reading a pattern table through a viewer must not switch the bank the
    /// PPU is drawing from.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_index(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_index(addr))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let value = self.chr_peek(addr);
        self.update_latch(addr);
        value
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
        w.u8(self.prg_bank);
        w.u8(self.chr_0_fd);
        w.u8(self.chr_0_fe);
        w.u8(self.chr_1_fd);
        w.u8(self.chr_1_fe);
        w.u8(self.latch_0);
        w.u8(self.latch_1);
        w.bool(self.mirroring == Mirroring::Horizontal);
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.prg_bank = r.u8()?;
        self.chr_0_fd = r.u8()?;
        self.chr_0_fe = r.u8()?;
        self.chr_1_fd = r.u8()?;
        self.chr_1_fe = r.u8()?;
        self.latch_0 = r.u8()?;
        self.latch_1 = r.u8()?;
        self.mirroring = if r.bool()? {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        };
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvram_board(chr_is_ram: bool, mmc4: bool) -> Mmc2Mmc4 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Mmc2Mmc4::new(
            crate::cart::RomData {
                header: crate::cart::Header {
                    mapper: if mmc4 { 10 } else { 9 },
                    ..Default::default()
                },
                prg: vec![0; 32768],
                chr,
                chr_is_ram,
                trainer: None,
            },
            mmc4,
        )
    }

    #[test]
    fn chr_ram_is_exposed_for_saving() {
        for mmc4 in [false, true] {
            let mut m = nvram_board(true, mmc4);
            m.ppu_write(0x0010, 0x44);
            assert_eq!(m.chr_ram().len(), 8192);
            assert_eq!(m.chr_ram()[0x10], 0x44);
        }
    }

    #[test]
    fn chr_rom_is_not_exposed_for_saving() {
        for mmc4 in [false, true] {
            let m = nvram_board(false, mmc4);
            assert!(m.chr_ram().is_empty());
        }
    }

    #[test]
    fn load_chr_ram_copies() {
        for mmc4 in [false, true] {
            let mut m = nvram_board(true, mmc4);
            m.load_chr_ram(&[5, 6, 7]);
            assert_eq!(m.ppu_read(0x0002), 7);
            assert_eq!(m.ppu_read(0x0003), 0);
        }
    }

    #[test]
    fn load_chr_ram_is_ignored_for_chr_rom() {
        for mmc4 in [false, true] {
            let mut m = nvram_board(false, mmc4);
            m.load_chr_ram(&[5, 6, 7]);
            assert_eq!(m.ppu_read(0x0000), 0x11);
        }
    }
}
