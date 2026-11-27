use crate::cart::{Mapper, Mirroring, RomData, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 99, the Vs. System board with no daughterboard (Vs. Super Mario Bros. and friends):
/// 8 KiB of switchable PRG at $8000-$9FFF over a fixed 24 KiB, one 8 KiB CHR bank, and
/// four-screen nametables from the cabinet's 4 KiB of video RAM.
///
/// The bank register is not in the cartridge's own window but in the CPU's `$4016` write, whose
/// bit 2 is wired to the board's A13 (nesdev "Vs. System", "$4016 write": "Select 8 KiB CHR ROM
/// bank for PPU $0000-$1FFF (mapper 99 games only) ... In case of games with 40KiB PRG-ROM ... the
/// above bit additionally changes 8KiB PRG-ROM at $8000-$9FFF"; nesdev "INES Mapper 099").
pub struct Vs {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// 2 KiB of work RAM, mirrored four times across $6000-$7FFF (nesdev "INES Mapper 099":
    /// "CPU $6000-$7FFF: 2 KiB RAM").
    prg_ram: Vec<u8>,
    /// Bit 2 of the last `$4016` write.
    bank: bool,
}

impl Vs {
    pub fn new(rom: RomData) -> Vs {
        Vs {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; 2048],
            bank: false,
        }
    }

    /// The 8 KiB PRG bank a CPU address reads (nesdev "INES Mapper 099"): $A000-$FFFF is the fixed
    /// 24 KiB above it, bank 1 to 3, and $8000-$9FFF follows `$4016` bit 2 — but only a 40 KiB
    /// image (Vs. Gumshoe, laid out as banks 0-3 then the extra 8 KiB) has a second bank there. An
    /// unpopulated socket drives nothing, so a missing bank reads $FF instead of mirroring.
    fn prg_bank(&self, addr: u16) -> usize {
        if addr < 0xA000 {
            usize::from(self.bank && self.prg.len() > 4 * 0x2000) * 4
        } else {
            1 + usize::from(addr - 0xA000) / 0x2000
        }
    }

    fn prg_index(&self, addr: u16) -> usize {
        self.prg_bank(addr) * 0x2000 + usize::from(addr & 0x1FFF)
    }

    /// Byte offset into CHR-ROM for a PPU address, or `None` for a pattern-memory socket with no
    /// chip in it: with 8 KiB of CHR and `$4016` bit 2 high the PPU has no tile data at all
    /// (nesdev "INES Mapper 099", "unlike all other mappers, an undersize mapper 99 image implies
    /// open bus instead of mirroring").
    fn chr_index(&self, addr: u16) -> Option<usize> {
        let index = usize::from(self.bank) * 0x2000 + usize::from(addr & 0x1FFF);
        (index < self.chr.len()).then_some(index)
    }
}

impl Mapper for Vs {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[usize::from((addr - 0x6000) & 0x07FF)],
            0x8000..=0xFFFF => self.prg.get(self.prg_index(addr)).copied().unwrap_or(0xFF),
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now; `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_index(addr))
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x6000..=0x7FFF = addr {
            self.prg_ram[usize::from((addr - 0x6000) & 0x07FF)] = val;
        }
    }

    /// The bank register: bit 2 of a `$4016` write (nesdev "Vs. System"; mapper 99 has no bus
    /// conflicts). The other bits belong to the controller strobe and the DualSystem's shared-RAM
    /// select, which is why the whole write is not used as the bank number.
    fn write_4016(&mut self, val: u8) {
        self.bank = val & 0x04 != 0;
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        match self.chr_index(addr) {
            Some(index) => chr_read(&self.chr, index),
            // Hardware leaves an empty socket on the open bus; a constant is close enough (no known
            // game reads these holes, and the Mapper trait has no open-bus path).
            None => 0xFF,
        }
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_index(addr)?)
    }

    /// CHR is ROM on a Vs. board; a CHR-RAM board (iNES header with no CHR banks) writes the page
    /// the bank bit selects.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let index = usize::from(self.bank) * 0x2000 + usize::from(addr & 0x1FFF);
            chr_write(&mut self.chr, index, val);
        }
    }

    /// The Vs. System always has 4 KiB of nametable RAM and every game uses four screens (nesdev
    /// "Vs. System"), so the header's mirroring bits are ignored.
    fn mirroring(&self) -> Mirroring {
        Mirroring::FourScreen
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.bank);
        w.bytes(&self.prg_ram);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.bool()?;
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
