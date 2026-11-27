use crate::cart::{Mapper, Mirroring, RomData, chr_read};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 228 (Active Enterprises: Action 52, Cheetahmen II). Every write to $8000-$FFFF latches
/// both the address and the data:
///   address 1.MH HPPP PPS. CCCC   data ......cc
///   M = mirroring (0 vertical, 1 horizontal), HH = 512 KB PRG chip, PPPPP = 16 KB PRG page on that chip,
///   S = PRG mode (0: 32 KB, page with bit 0 clear at $8000 and set at $C000; 1: same 16 KB page twice),
///   CCCCcc = 8 KB CHR bank.
/// The ROM file holds chips 0, 1 and 3 back to back; chip 2 does not exist and reads as open bus.
pub struct Action52 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    addr: u16,
    data: u8,
}

impl Action52 {
    /// Power-on state is a write of $00 to $8000: chip 0, page 0, 32 KB mode, CHR bank 0, vertical.
    pub fn new(rom: RomData) -> Action52 {
        Action52 {
            prg: rom.prg,
            chr: rom.chr,
            addr: 0x8000,
            data: 0,
        }
    }

    /// Index of the selected chip's data in the PRG file, or None for the missing chip 2.
    fn chip_in_file(&self) -> Option<usize> {
        match (self.addr >> 11) & 0x03 {
            0 => Some(0),
            1 => Some(1),
            2 => None,
            _ => Some(2),
        }
    }

    /// Byte offset into PRG for a CPU address in 0x8000..=0xFFFF, or None when chip 2 is selected.
    fn prg_offset(&self, addr: u16) -> Option<usize> {
        let chip = self.chip_in_file()?;
        let page = usize::from((self.addr >> 6) & 0x1F);
        let page = if self.addr & 0x20 != 0 {
            page
        } else {
            (page & !1) | usize::from((addr >= 0xC000) as u8)
        };
        let offset = chip * 0x8_0000 + page * 0x4000 + usize::from(addr & 0x3FFF);
        Some(offset % self.prg.len())
    }

    fn chr_bank(&self) -> usize {
        (usize::from(self.addr & 0x0F) << 2) | usize::from(self.data & 0x03)
    }
}

impl Mapper for Action52 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            // Open bus approximation for the missing chip: the high byte of the address.
            0x8000..=0xFFFF => self
                .prg_offset(addr)
                .map_or((addr >> 8) as u8, |i| self.prg[i]),
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr >= 0x8000 {
            self.addr = addr;
            self.data = val;
        }
    }

    /// CHR-ROM window at the 8 KB bank the board's latch currently selects.
    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(
            &self.chr,
            self.chr_bank() * 0x2000 + usize::from(addr & 0x1FFF),
        )
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// CHR is ROM on this board, so pattern writes are ignored.
    fn ppu_write(&mut self, _addr: u16, _val: u8) {}

    fn mirroring(&self) -> Mirroring {
        if self.addr & 0x2000 != 0 {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        }
    }

    /// No PRG RAM on this board.
    fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.addr);
        w.u8(self.data);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.addr = r.u16()?;
        self.data = r.u8()?;
        Ok(())
    }

    fn prg_ram(&self) -> &[u8] {
        &[]
    }

    fn load_prg_ram(&mut self, _data: &[u8]) {}
}
