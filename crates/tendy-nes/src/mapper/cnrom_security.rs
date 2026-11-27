use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 185 (CNROM with the security diodes): mapper 3's four-bit latch at `$8000-$FFFF`, written
/// through AND-type bus conflicts, on a board whose single 8 KB CHR-ROM chip has its A13/A14 pins
/// programmed as two chip selects instead of address lines (nesdev "CNROM"). When the latched value's
/// bits 0-1 are not the board's chip-select value, CHR-ROM is tri-stated: pattern reads return open
/// bus, which this crate's boards report as 0 (as `Sunsoft2` does).
///
/// Which value enables CHR-ROM is a diode configuration. NES 2.0 submapper 4+N names it (the mapper
/// page: "the submapper number denotes the correct Chip Select value"; the UNIF mapping table: "x=4..7
/// depending on diode configuration"). Submapper 0 — every iNES 1.0 file, whose value is unknown —
/// keeps CHR-ROM always enabled: nesdev's substitute, "disable CHR-ROM for the first two reads from
/// $2007 after a reset and then enable it", needs a read's originating CPU address, which
/// `Mapper::ppu_read` is not passed — `shortcut:` a submapper-0 dump that relies on the disable, plumb
/// $2007 read counts to the board.
pub struct CnromSecurity {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// The four-bit latch: bits 0-1 the chip selects, bits 2-3 the (unused) CHR address lines.
    bank: u8,
    /// The low two latch bits that enable CHR-ROM, when the header names them.
    enable_value: Option<u8>,
    mirroring: Mirroring,
}

impl CnromSecurity {
    pub fn new(rom: RomData) -> CnromSecurity {
        // NES 2.0 submapper 4..7 = chip-select value 0..3; anything else leaves it unknown.
        let enable_value = (4..=7)
            .contains(&rom.header.submapper)
            .then(|| rom.header.submapper - 4);
        CnromSecurity {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            bank: 0,
            enable_value,
            mirroring: rom.header.mirroring,
        }
    }

    fn chr_enabled(&self) -> bool {
        self.enable_value.is_none_or(|v| self.bank & 0x03 == v)
    }
}

impl Mapper for CnromSecurity {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.prg[usize::from(addr - 0x8000) % self.prg.len()],
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now; PRG is 32 KB fixed, so every
    /// address wraps over the whole image exactly as `cpu_read` does.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| usize::from(addr - 0x8000) % self.prg.len())
    }

    /// The latch, ANDed with the PRG byte at the written address: this board always has conflicts.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        if (0x8000..=0xFFFF).contains(&addr) {
            self.bank = val & self.prg[usize::from(addr - 0x8000) % self.prg.len()];
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        if self.chr_enabled() {
            chr_read(&self.chr, usize::from(addr & 0x1FFF))
        } else {
            0
        }
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// The board is CHR-ROM; a CHR-RAM upload still works while CHR-ROM is enabled.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram && self.chr_enabled() {
            chr_write(&mut self.chr, usize::from(addr & 0x1FFF), val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// The board has no PRG RAM.
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
        w.u8(self.bank);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank = r.u8()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}
