use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// PRG bank size: the switchable 16 KB window at $8000-$BFFF.
const PRG_BANK: usize = 0x4000;
/// CHR bank size: one 8 KB window at PPU $0000-$1FFF.
const CHR_BANK: usize = 0x2000;

/// Which register layout a [`CnromUnrom`] has: the two boards carry the same four register fields
/// in opposite nibbles.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// nesdev "INES Mapper 078": bits 0-2 the PRG bank, bit 3 the mirroring bit, bits 4-7 the CHR bank.
    M78,
    /// nesdev "INES Mapper 152": bit 7 the mirroring bit, bits 4-6 the PRG bank, bits 0-3 the CHR bank.
    M152,
}

/// Mappers 78 (Holy Diver, Uchuusen - Cosmo Carrier) and 152 (Arkanoid II, Gegege no Kitarou 2):
/// "simply a CNROM and a UNROM put together" (nesdev) — an 8 KB CHR bank and a 16 KB PRG bank at
/// $8000-$BFFF with the last 16 KB fixed at $C000-$FFFF, plus one mirroring bit. Both boards have
/// AND-type bus conflicts, so a write latches the value ANDed with the PRG byte at that address.
///
/// The mirroring bit means different things per board. On mapper 152 it is CIRAM A10 (0 = one-screen
/// lower, 1 = one-screen upper). On mapper 78 it is wired to a CIRAM A10 mux on Holy Diver (NES 2.0
/// submapper 3: 0 = horizontal, 1 = vertical) and straight to CIRAM A10 on Cosmo Carrier (submapper
/// 1: 0 = one-screen lower, 1 = one-screen upper), the two being incompatible; a submapper-0 header
/// names neither, and nesdev records that emulators default to Cosmo Carrier, which `hv = false`.
pub struct CnromUnrom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    kind: Kind,
    /// True on Holy Diver's H/V wiring and false on the one-screen wiring.
    hv: bool,
    prg_bank: usize,
    chr_bank: usize,
    /// The latched mirroring bit: bit 3 on mapper 78, bit 7 on mapper 152.
    mirror_bit: bool,
}

impl CnromUnrom {
    pub fn new(rom: RomData, kind: Kind, hv: bool) -> CnromUnrom {
        CnromUnrom {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            kind,
            hv,
            prg_bank: 0,
            chr_bank: 0,
            mirror_bit: false,
        }
    }

    /// 16 KB bank count, at least one so the modulo never divides by zero on a short image.
    fn banks(&self) -> usize {
        (self.prg.len() / PRG_BANK).max(1)
    }

    /// Start of the PRG bank mapped at `addr`: the selected one below $C000, the fixed last above.
    fn prg_base(&self, addr: u16) -> usize {
        if addr < 0xC000 {
            (self.prg_bank % self.banks()) * PRG_BANK
        } else {
            (self.banks() - 1) * PRG_BANK
        }
    }

    /// The PRG byte a write to `addr` conflicts with, which is the byte the CPU would read there.
    fn bus_byte(&self, addr: u16) -> u8 {
        self.prg[self.prg_base(addr) + usize::from(addr & 0x3FFF)]
    }
}

impl Mapper for CnromUnrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg[self.prg_base(addr) + usize::from(addr & 0x3FFF)],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank register;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_base(addr) + usize::from(addr & 0x3FFF))
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            let val = val & self.bus_byte(addr);
            match self.kind {
                Kind::M78 => {
                    self.prg_bank = usize::from(val & 0x07);
                    self.mirror_bit = val & 0x08 != 0;
                    self.chr_bank = usize::from(val >> 4);
                }
                Kind::M152 => {
                    self.prg_bank = usize::from((val >> 4) & 0x07);
                    self.mirror_bit = val & 0x80 != 0;
                    self.chr_bank = usize::from(val & 0x0F);
                }
            }
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(
            &self.chr,
            self.chr_bank * CHR_BANK + usize::from(addr & 0x1FFF),
        )
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(
            &self.chr,
            self.chr_is_ram,
            self.chr_bank * CHR_BANK + usize::from(addr & 0x1FFF),
        )
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// Both boards are CHR-ROM; a CHR-RAM upload still lands in the selected bank.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let index = self.chr_bank * CHR_BANK + usize::from(addr & 0x1FFF);
            chr_write(&mut self.chr, index, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        match (self.hv, self.mirror_bit) {
            (false, false) => Mirroring::SingleScreenLower,
            (false, true) => Mirroring::SingleScreenUpper,
            (true, false) => Mirroring::Horizontal,
            (true, true) => Mirroring::Vertical,
        }
    }

    /// Neither board has PRG RAM.
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
        w.u8(u8::from(self.mirror_bit));
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.prg_bank = r.usize()?;
        self.chr_bank = r.usize()?;
        self.mirror_bit = r.u8()? != 0;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}
