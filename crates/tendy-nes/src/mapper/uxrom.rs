use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 2 (UxROM): a 16 KB PRG bank selected by writes to $8000-$FFFF is mapped at $8000;
/// the last 16 KB bank is fixed at $C000. CHR is 8 KB (usually RAM). NES 2.0 submapper 2 marks
/// boards without the conflict-prevention circuit: such a write latches the value ANDed with the
/// PRG ROM byte at the written address; submappers 0 and 1 latch all eight bits.
pub struct Uxrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    bank: usize,
    /// True only on NES 2.0 submapper 2, where register writes conflict with PRG ROM.
    bus_conflicts: bool,
    mirroring: Mirroring,
}

impl Uxrom {
    pub fn new(rom: RomData) -> Uxrom {
        Uxrom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; 8192],
            bank: 0,
            bus_conflicts: rom.header.nes2 && rom.header.submapper == 2,
            mirroring: rom.header.mirroring,
        }
    }

    /// The value the register latches: on submapper 2 the write is ANDed with the PRG ROM byte at
    /// the written address (current bank at $8000-$BFFF, fixed bank at $C000-$FFFF).
    fn latched(&self, addr: u16, val: u8) -> u8 {
        if !self.bus_conflicts {
            return val;
        }
        let banks = self.prg.len() / 0x4000;
        let base = if addr < 0xC000 {
            (self.bank % banks) * 0x4000
        } else {
            (banks - 1) * 0x4000
        };
        val & self.prg[base + usize::from(addr & 0x3FFF)]
    }
}

impl Mapper for Uxrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        let banks = self.prg.len() / 0x4000;
        match addr {
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr - 0x6000)],
            0x8000..=0xBFFF => self.prg[(self.bank % banks) * 0x4000 + usize::from(addr - 0x8000)],
            0xC000..=0xFFFF => self.prg[(banks - 1) * 0x4000 + usize::from(addr - 0xC000)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        let banks = self.prg.len() / 0x4000;
        match addr {
            0x8000..=0xBFFF => Some((self.bank % banks) * 0x4000 + usize::from(addr - 0x8000)),
            0xC000..=0xFFFF => Some((banks - 1) * 0x4000 + usize::from(addr - 0xC000)),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr - 0x6000)] = val,
            0x8000..=0xFFFF => self.bank = usize::from(self.latched(addr, val)),
            _ => {}
        }
    }

    /// UxROM has no CHR banking, so the peek is the read without any side effect.
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

    fn power_on_ram(&mut self, mode: crate::bus::RamPowerOn, seed: u64) {
        let chr: &mut [u8] = if self.chr_is_ram {
            &mut self.chr
        } else {
            &mut []
        };
        crate::cart::fill_power_on_ram(&mut self.prg_ram, chr, mode, seed);
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.bank);
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.usize()?;
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    fn rom_data(chr: Vec<u8>, chr_is_ram: bool) -> RomData {
        RomData {
            header: Header {
                prg_banks: 2,
                chr_banks: 0,
                mapper: 2,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr,
            chr_is_ram,
            trainer: None,
        }
    }

    #[test]
    fn chr_ram_is_exposed_for_saving() {
        let mut m = Uxrom::new(rom_data(vec![0; 8192], true));
        m.ppu_write(0x0010, 0x44);
        assert_eq!(m.chr_ram().len(), 8192);
        assert_eq!(m.chr_ram()[0x10], 0x44);
    }

    #[test]
    fn chr_rom_is_not_exposed_for_saving() {
        let m = Uxrom::new(rom_data(vec![0x11; 8192], false));
        assert!(m.chr_ram().is_empty());
    }

    #[test]
    fn load_chr_ram_copies() {
        let mut m = Uxrom::new(rom_data(vec![0; 8192], true));
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0002), 7);
        assert_eq!(m.ppu_read(0x0003), 0);
    }

    #[test]
    fn load_chr_ram_is_ignored_for_chr_rom() {
        let mut m = Uxrom::new(rom_data(vec![0x11; 8192], false));
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0000), 0x11);
    }

    #[test]
    fn power_on_ram_random_fills_prg_and_chr_ram_deterministically() {
        use crate::bus::RamPowerOn;
        let seed = 0x0123_4567_89AB_CDEF;
        let make = |mode, seed| {
            let mut m = Uxrom::new(rom_data(vec![0; 8192], true));
            m.power_on_ram(mode, seed);
            m
        };
        let a = make(RamPowerOn::Random, seed);
        let b = make(RamPowerOn::Random, seed);
        let c = make(RamPowerOn::Random, seed + 1);
        assert_eq!(a.prg_ram(), b.prg_ram());
        assert_eq!(a.chr, b.chr);
        assert_ne!(a.prg_ram(), c.prg_ram());
        assert!(
            a.prg_ram().iter().any(|&x| x != 0),
            "UxROM: PRG RAM was filled"
        );
        assert!(a.chr.iter().any(|&x| x != 0), "UxROM: CHR-RAM was filled");
        // One stream: PRG RAM first, CHR-RAM straight after it.
        let mut rng = crate::rng::derive(seed, crate::rng::SeedDomain::CartRam);
        let mut prg = vec![0u8; a.prg_ram().len()];
        let mut chr = vec![0u8; a.chr.len()];
        rng.fill_bytes(&mut prg);
        rng.fill_bytes(&mut chr);
        assert_eq!(a.prg_ram(), &prg[..]);
        assert_eq!(&a.chr[..], &chr[..]);
        let ones = make(RamPowerOn::Ones, 0);
        assert!(ones.prg_ram().iter().all(|&x| x == 0xFF));
        assert!(ones.chr.iter().all(|&x| x == 0xFF));
    }

    #[test]
    fn power_on_ram_zero_leaves_ram_zeroed() {
        use crate::bus::RamPowerOn;
        let mut m = Uxrom::new(rom_data(vec![0; 8192], true));
        m.power_on_ram(RamPowerOn::Zero, 0x1234);
        assert!(m.prg_ram().iter().all(|&x| x == 0));
        assert!(m.chr.iter().all(|&x| x == 0));
    }

    #[test]
    fn power_on_ram_never_touches_chr_rom() {
        use crate::bus::RamPowerOn;
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            let mut m = Uxrom::new(rom_data(
                (0..8192).map(|i| (i % 7) as u8 + 1).collect(),
                false,
            ));
            let before: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            m.power_on_ram(mode, 99);
            let after: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            assert_eq!(before, after, "{mode:?}");
            assert!(!m.chr.is_empty(), "CHR-ROM still present");
        }
    }
}
