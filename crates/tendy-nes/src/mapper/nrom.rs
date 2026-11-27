use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

pub struct Nrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    mirroring: Mirroring,
    /// `prg.len() - 1` when that length is a power of two — every real NROM image — so a CPU read
    /// masks instead of dividing. `None` keeps `%` for an odd NES 2.0 size.
    prg_mask: Option<usize>,
}

impl Nrom {
    pub fn new(rom: RomData) -> Nrom {
        let prg_mask = rom.prg.len().is_power_of_two().then(|| rom.prg.len() - 1);
        Nrom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; 8192],
            mirroring: rom.header.mirroring,
            prg_mask,
        }
    }

    /// Byte offset into PRG-ROM for a CPU address in 0x8000..=0xFFFF: the image mirrors to fill the
    /// 32 KB window.
    fn prg_index(&self, addr: u16) -> usize {
        let i = usize::from(addr - 0x8000);
        match self.prg_mask {
            Some(mask) => i & mask,
            None => i % self.prg.len(),
        }
    }
}

impl Mapper for Nrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[(addr - 0x6000) as usize],
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
        if (0x6000..=0x7FFF).contains(&addr) {
            self.prg_ram[(addr - 0x6000) as usize] = val;
        }
    }

    /// NROM maps the whole CHR image at $0000, so the peek is the read without any side effect.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, addr as usize & 0x1FFF)
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            chr_write(&mut self.chr, addr as usize & 0x1FFF, val);
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
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
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

    fn rom_data(prg_size: usize, chr: Vec<u8>, chr_is_ram: bool) -> RomData {
        RomData {
            header: Header {
                prg_banks: prg_size / 16384,
                chr_banks: 1,
                mapper: 0,
                mirroring: Mirroring::Vertical,
                battery: false,
                trainer: false,
                ..Default::default()
            },
            prg: (0..prg_size).map(|i| (i % 251) as u8).collect(),
            chr,
            chr_is_ram,
            trainer: None,
        }
    }

    #[test]
    fn nrom_256_reads() {
        let mut m = Nrom::new(rom_data(32768, vec![0; 8192], false));
        assert_eq!(m.cpu_read(0x8000), 0);
        assert_eq!(m.cpu_read(0x8001), 1);
        assert_eq!(m.cpu_read(0xFFFF), 137);
    }

    #[test]
    fn nrom_128_mirrors() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        assert_eq!(m.cpu_read(0xC005), 5);
        assert_eq!(m.cpu_read(0xC000), m.cpu_read(0x8000));
    }

    #[test]
    fn prg_ram_read_write() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        m.cpu_write(0x6000, 0x42);
        m.cpu_write(0x7FFF, 7);
        assert_eq!(m.cpu_read(0x6000), 0x42);
        assert_eq!(m.cpu_read(0x7FFF), 7);
        assert_eq!(m.prg_ram().len(), 8192);
    }

    #[test]
    fn prg_rom_is_read_only() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        m.cpu_write(0x8000, 0x99);
        assert_eq!(m.cpu_read(0x8000), 0);
    }

    #[test]
    fn unmapped_reads_zero() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        assert_eq!(m.cpu_read(0x5000), 0);
    }

    #[test]
    fn chr_rom_is_read_only() {
        let mut m = Nrom::new(rom_data(16384, vec![0x11; 8192], false));
        m.ppu_write(0x0000, 0x22);
        assert_eq!(m.ppu_read(0x0000), 0x11);
    }

    #[test]
    fn chr_ram_is_writable() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], true));
        m.ppu_write(0x1FFF, 0x33);
        assert_eq!(m.ppu_read(0x1FFF), 0x33);
    }

    #[test]
    fn mirroring_from_header() {
        let m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        assert_eq!(m.mirroring(), Mirroring::Vertical);
    }

    #[test]
    fn load_prg_ram_copies() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        m.load_prg_ram(&[1, 2, 3]);
        assert_eq!(m.cpu_read(0x6002), 3);
        assert_eq!(m.cpu_read(0x6003), 0);
    }

    /// A power-of-two image masks, an odd NES 2.0 one falls back to `%`; both mirror the same.
    #[test]
    fn prg_mirroring_is_the_same_for_odd_sizes() {
        for prg_size in [16384usize, 32768, 49152] {
            let mut m = Nrom::new(RomData {
                header: Header {
                    mapper: 0,
                    ..Default::default()
                },
                prg: (0..prg_size).map(|i| (i % 251) as u8).collect(),
                chr: vec![0; 8192],
                chr_is_ram: false,
                trainer: None,
            });
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
    fn no_irq() {
        let m = Nrom::new(rom_data(16384, vec![0; 8192], false));
        assert!(!m.irq_pending());
    }

    #[test]
    fn chr_ram_is_exposed_for_saving() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], true));
        m.ppu_write(0x0010, 0x44);
        assert_eq!(m.chr_ram().len(), 8192);
        assert_eq!(m.chr_ram()[0x10], 0x44);
    }

    #[test]
    fn chr_rom_is_not_exposed_for_saving() {
        let m = Nrom::new(rom_data(16384, vec![0x11; 8192], false));
        assert!(m.chr_ram().is_empty());
    }

    #[test]
    fn load_chr_ram_copies() {
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], true));
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0002), 7);
        assert_eq!(m.ppu_read(0x0003), 0);
    }

    #[test]
    fn load_chr_ram_is_ignored_for_chr_rom() {
        let mut m = Nrom::new(rom_data(16384, vec![0x11; 8192], false));
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0000), 0x11);
    }

    #[test]
    fn power_on_ram_random_fills_prg_and_chr_ram_deterministically() {
        use crate::bus::RamPowerOn;
        let seed = 0x0123_4567_89AB_CDEF;
        let make = |mode, seed| {
            let mut m = Nrom::new(rom_data(16384, vec![0; 8192], true));
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
            "NROM: PRG RAM was filled"
        );
        assert!(a.chr.iter().any(|&x| x != 0), "NROM: CHR-RAM was filled");
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
        let mut m = Nrom::new(rom_data(16384, vec![0; 8192], true));
        m.power_on_ram(RamPowerOn::Zero, 0x1234);
        assert!(m.prg_ram().iter().all(|&x| x == 0));
        assert!(m.chr.iter().all(|&x| x == 0));
    }

    #[test]
    fn power_on_ram_never_touches_chr_rom() {
        use crate::bus::RamPowerOn;
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            let mut m = Nrom::new(rom_data(
                16384,
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
