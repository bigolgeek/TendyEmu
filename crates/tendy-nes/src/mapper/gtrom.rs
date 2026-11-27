//! Mapper 111 (GTROM, "Cheapocabra"): a 512 KB SST39SF040 flash in a 32 KB PRG window, 16 KB of
//! CHR RAM and 16 KB of nametable RAM, all selected by one register at $5000-$5FFF / $7000-$7FFF.
//!
//! Everything here comes from nesdev "GTROM" — the register (§ Registers), the flash window and
//! its command addresses (§ PRG ROM, § Self-flashing), the PPU RAM (§ PPU RAM including § Bonus
//! RAM) — except the mapper number itself, which nesdev "INES Mapper 111" also gives to an
//! unrelated MMC1 variant (see `crate::mapper::build`).
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! GRNC PPPP
//! |||| ||||
//! |||| ++++- 32 KB PRG page for CPU $8000-$FFFF
//! |||+------ CHR RAM page for PPU $0000-$1FFF
//! ||+------- nametable RAM page for PPU $2000-$3EFF
//! |+-------- red LED: 0 = on, 1 = off (the board's own light; no NES output)
//! +--------- green LED: 0 = on, 1 = off (likewise)
//! ```

use crate::cart::{Mapper, Mirroring, RomData, chr_read, chr_write};
use crate::mapper::flash::{ERASED, Flash, Part};
use crate::state::{StateError, StateReader, StateWriter};

/// CHR RAM on the board: the first two 8 KB pages of its 32 KB RAM (nesdev "GTROM" § CHR RAM).
const CHR_RAM_LEN: usize = 0x4000;
/// Nametable RAM: the last two 8 KB pages (nesdev "GTROM" § VRAM).
const NT_RAM_LEN: usize = 0x4000;

pub struct Gtrom {
    /// The control register. 0 at power-on: page 0 of everything, both LEDs on.
    reg: u8,
    /// The board's PRG storage is the flash part itself, not a copy of the ROM image.
    flash: Flash,
    chr: Vec<u8>,
    /// Two 8 KB pages, each covering PPU $2000-$3FFF: the four nametables plus the bonus RAM.
    nt: Vec<u8>,
}

impl Gtrom {
    pub fn new(rom: RomData) -> Gtrom {
        // The 512 KB part *is* the cartridge's PRG ROM (nesdev "GTROM" § PRG ROM, § Self-flashing):
        // an image shorter than the part leaves the rest erased, and one longer is cut at the
        // part's last address, which is all the chip can decode.
        let mut image = rom.prg;
        image.resize(Part::Sf040.bytes(), ERASED);
        Gtrom {
            reg: 0,
            flash: Flash::from_image(Part::Sf040, image),
            chr: vec![0; CHR_RAM_LEN],
            nt: vec![0; NT_RAM_LEN],
        }
    }

    /// The flash chip, for the byte-program test and for the file save path.
    pub fn flash(&self) -> &Flash {
        &self.flash
    }

    /// Chip address of a CPU access in the PRG window: the selected 32 KB page's base plus the
    /// window offset. A command written at CPU $D555 therefore lands on chip $5555 in every page,
    /// which is what nesdev "GTROM" § Self-flashing says the part sees.
    fn chip_addr(&self, addr: u16) -> u32 {
        u32::from(self.reg & 0x0F) * 0x8000 + u32::from(addr & 0x7FFF)
    }

    /// CHR RAM index: bit 4 selects one of the two 8 KB pages (nesdev "GTROM" § CHR RAM).
    fn chr_index(&self, addr: u16) -> usize {
        usize::from((self.reg >> 4) & 1) * 0x2000 + usize::from(addr & 0x1FFF)
    }

    /// Nametable RAM index: bit 5 selects one of the two 8 KB pages, and a page covers the whole
    /// $2000-$3FFF range — on this board $3000-$3EFF is the bonus RAM, not a mirror of $2000-$2EFF
    /// (nesdev "GTROM" § VRAM, § Bonus RAM).
    fn nt_index(&self, addr: u16) -> usize {
        usize::from((self.reg >> 5) & 1) * 0x2000 + usize::from(addr & 0x1FFF)
    }
}

impl Mapper for Gtrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if addr >= 0x8000 {
            self.flash.read(self.chip_addr(addr))
        } else {
            // No PRG RAM: $6000-$7FFF reads back what a board with nothing on the bus shows.
            0
        }
    }

    /// The register is decoded on A15 = 0, A14 = 1 and A12 = 1 — one address range for both
    /// $5000-$5FFF and $7000-$7FFF, with $6000-$6FFF (A12 = 0) not part of it (nesdev "GTROM"
    /// § Bank select; the teardown's "if /ROMSEL, A14, and A12 all high"). Bits 6-7 are the LEDs and
    /// have no emulated output. A write to $8000-$FFFF is a flash cycle, not a register write.
    ///
    /// nesdev also notes that *reading* the register latches open bus into it; that is the same
    /// register write and no game can rely on it without its own RAM copy, so it is not modelled.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x5000..=0x5FFF | 0x7000..=0x7FFF => self.reg = val,
            // The program window doubles as the flash part's command interface: the command
            // sequences go through these same writes (nesdev "GTROM" § Self-flashing).
            0x8000..=0xFFFF => self.flash.write(self.chip_addr(addr), val),
            _ => {}
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now: the flash image at the selected
    /// page's offset.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.chip_addr(addr) as usize)
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_index(addr))
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.chr_peek(addr)
    }

    /// CHR RAM is always writable (nesdev "GTROM" § PPU RAM: "The board contains a 32 KiB of RAM.
    /// This RAM is always enabled").
    fn ppu_write(&mut self, addr: u16, val: u8) {
        let index = self.chr_index(addr);
        chr_write(&mut self.chr, index, val);
    }

    /// The board's own nametable RAM is always enabled and the NES's 2 KB CIRAM never is (nesdev
    /// "GTROM" § PPU RAM), so `ciram` is ignored on both sides.
    fn nt_read(&mut self, addr: u16, _ciram: &[u8]) -> Option<u8> {
        Some(self.nt[self.nt_index(addr)])
    }

    fn nt_write(&mut self, addr: u16, val: u8, _ciram: &mut [u8]) -> bool {
        let index = self.nt_index(addr);
        self.nt[index] = val;
        true
    }

    fn nt_peek(&self, addr: u16, _ciram: &[u8]) -> Option<u8> {
        Some(self.nt[self.nt_index(addr)])
    }

    /// Four fixed screens: the board has real RAM for all four nametables and no switch for the
    /// arrangement (nesdev "GTROM" § VRAM).
    fn mirroring(&self) -> Mirroring {
        Mirroring::FourScreen
    }

    fn prg_ram(&self) -> &[u8] {
        &[]
    }

    fn load_prg_ram(&mut self, _data: &[u8]) {}

    /// The whole 512 KB flash image is the save: the board has no PRG RAM, and
    /// nesdev "GTROM" § Self-flashing says the game keeps its saves "on the 'ROM' chip" itself —
    /// GTROM is self-flashable by construction, so unlike UNROM 512 it needs no battery bit to opt
    /// in. [`Mapper::battery_change_key`] keeps its default, the same image, so an untouched flash
    /// compares equal and is not rewritten.
    fn battery_data(&self) -> Vec<u8> {
        self.flash.image().to_vec()
    }

    /// Restore [`Mapper::battery_data`]. A file of any other length belongs to another part (or is
    /// truncated) and is ignored whole, never partly applied.
    fn load_battery_data(&mut self, data: &[u8]) {
        let part = self.flash.part();
        if data.len() != part.bytes() {
            eprintln!(
                "ignoring flash save of {} bytes: the part's image is {} bytes",
                data.len(),
                part.bytes()
            );
            return;
        }
        self.flash = Flash::from_image(part, data.to_vec());
    }

    /// The register, the flash chip (image, decoder position and dirty bitmap) and the two RAM
    /// chips; [`Flash::save_state`] carries the chip's own half of it.
    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.reg);
        self.flash.save_state(w);
        w.bytes(&self.chr);
        w.bytes(&self.nt);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.reg = r.u8()?;
        self.flash.load_state(r)?;
        r.bytes_into(&mut self.chr)?;
        r.bytes_into(&mut self.nt)?;
        Ok(())
    }
}
