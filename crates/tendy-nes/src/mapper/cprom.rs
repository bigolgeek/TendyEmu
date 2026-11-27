use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Bytes of CHR-RAM on a CPROM board: two 6264s, which is four 4 KB pages.
const CHR_RAM_LEN: usize = 0x4000;
/// Size of one CHR page: the window the register switches.
const CHR_PAGE: usize = 0x1000;

/// Mapper 13 (CPROM): 32 KB of fixed PRG and 16 KB of CHR-RAM in two windows — the first 4 KB page
/// fixed at PPU $0000-$0FFF and one of the four pages, selected by writes to $8000-$FFFF (bits 0-1),
/// at $1000-$1FFF. The board has no conflict-prevention circuit (a 74HC161 latch fed straight from
/// the PRG data bus), so a write latches the value ANDed with the PRG byte at the written address.
pub struct Cprom {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// The 4 KB page at PPU $1000-$1FFF: bits 0-1 of the last register write, 0 through 3.
    bank: usize,
    mirroring: Mirroring,
}

impl Cprom {
    pub fn new(rom: RomData) -> Cprom {
        let mut chr = rom.chr;
        // The board carries 16 KB of CHR-RAM, which an iNES 1.0 header cannot state: it says "no CHR
        // banks", which every other board reads as 8 KB. Size the RAM up to the real board.
        if rom.chr_is_ram {
            chr.resize(CHR_RAM_LEN, 0);
        }
        Cprom {
            prg: rom.prg,
            chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            mirroring: rom.header.mirroring,
        }
    }

    /// The CHR-RAM page a PPU address lands in: page 0 at $0000-$0FFF, the selected page above it.
    fn chr_index(&self, addr: u16) -> usize {
        let page = if addr < CHR_PAGE as u16 { 0 } else { self.bank };
        page * CHR_PAGE + usize::from(addr & 0x0FFF)
    }

    /// The PRG byte at a written address, which the latch ANDs the register value with.
    fn prg_byte(&self, addr: u16) -> u8 {
        self.prg[usize::from(addr - 0x8000) % self.prg.len()]
    }
}

impl Mapper for Cprom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg_byte(addr),
            _ => 0,
        }
    }

    /// Bits 0-1 select the page at PPU $1000-$1FFF. The value is ANDed with the PRG byte at the
    /// written address: the board's bus conflict.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            self.bank = usize::from(val & self.prg_byte(addr) & 0x03);
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF. PRG is 32 KB fixed, so every address wraps over the whole image
    /// exactly as `cpu_read` does.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| usize::from(addr - 0x8000) % self.prg.len())
    }

    /// CPROM's only bank register is the one between $1000 and the CHR-RAM it selects.
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
            let index = self.chr_index(addr);
            chr_write(&mut self.chr, index, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// CPROM has no PRG RAM: the whole $6000-$7FFF window reads open bus.
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
