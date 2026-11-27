use crate::cart::{Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix};
use crate::mapper::flash::{self, Flash, Part};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 30 (UNROM 512): a 16 KB PRG window, 32 KB of banked CHR-RAM and an optional PRG flash.
///
/// nesdev "UNROM 512": CPU `$8000-$BFFF` is a 16 KB window into the PRG, `$C000-$FFFF` the last
/// 16 KB fixed, and PPU `$0000-$1FFF` an 8 KB window into 32 KB of CHR-RAM. One write-only
/// register, `D~[NCCP PPPP]`: bits 0-4 are the 16 KB PRG bank, bits 5-6 the 8 KB CHR-RAM bank,
/// bit 7 the nametable arrangement. It decodes `$8000-$FFFF` on a board without the battery bit
/// (submapper 0 without it, and submapper 2) and `$C000-$FFFF` on a board with it (submappers 1, 3
/// and 4, and submapper 0's battery configuration).
///
/// The battery bit also makes the board flashable: the PRG is an SST39SF0x0 and writes to
/// `$8000-$BFFF` are its command cycles. nesdev "Flash ROM Save" prints each cycle as a pair,
/// `$C000:$01, $9555:$AA`: the `$C000` write is the register above (PRG bank 1), so `$9555` in
/// that window is chip address `$5555` — the three-cycle unlock of [`Flash`]'s Table 4-2 with the
/// register's bank supplying A14 and up. Without the battery bit there is no flash: the writes are
/// the register alone, ANDed with the PRG byte on the bus (nesdev "Bus conflict", the board's
/// non-flashable configuration).
///
/// A four-screen header is read as the board's one-screen configuration and the register's bit 7
/// picks the half, which is the iNES meaning nesdev gives `%....1..0`; the "4-screen with
/// cartridge VRAM" arrangement (last 8 KB of CHR-RAM as nametables) needs a board-side nametable
/// window this core does not build, and this repository's header parser cannot tell the two
/// apart anyway (both set the four-screen bit). shortcut: four-screen-with-cartridge-VRAM reads as
/// one-screen; upgrade if a ROM for it appears.
pub struct Unrom512 {
    /// PRG ROM on a board without the battery bit; empty when [`Unrom512::flash`] holds it.
    prg: Vec<u8>,
    /// The PRG chip, when the header's battery bit makes the board self-flashable.
    flash: Option<Flash>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// The `D~[NCCP PPPP]` register, as written.
    latch: u8,
    /// The header's nametable arrangement; bit 7 of the latch steers a one-screen one.
    mirroring: Mirroring,
    /// 16 KB banks in the PRG image, which is what `$C000` fixes on.
    banks: usize,
    /// The register decodes `$C000-$FFFF` only (battery bit, or submapper 1, 3 or 4).
    latches_upper_only: bool,
    /// Register writes AND with the PRG byte on the bus (the non-flashable configuration).
    bus_conflicts: bool,
    /// Submapper 3: bit 7 picks horizontal or vertical rather than the one-screen half.
    hv_from_latch: bool,
}

impl Unrom512 {
    pub fn new(rom: RomData) -> Unrom512 {
        let battery = rom.header.battery;
        let submapper = rom.header.submapper;
        // nesdev "UNROM 512", submappers: 1, 3 and 4 never conflict, 2 always does, and without a
        // submapper the battery bit is what says the conflict-prevention circuit is there.
        let bus_conflicts = match submapper {
            1 | 3 | 4 => false,
            2 => true,
            _ => !battery,
        };
        let banks = (rom.prg.len() / 0x4000).max(1);
        let mut prg = rom.prg;
        // nesdev lists 256K and 512K PRG for the board; the 256 KB part also covers a smaller
        // image, and Flash aliases anything above the part's address pins.
        let flash = if battery {
            let part = if prg.len() <= 256 * 1024 {
                Part::Sf020
            } else {
                Part::Sf040
            };
            prg.resize(part.bytes(), flash::ERASED);
            Some(Flash::from_image(part, std::mem::take(&mut prg)))
        } else {
            None
        };
        // The board carries 32 KB of CHR-RAM (nesdev "Banks"); iNES sizes it 8 KB by default.
        let mut chr = rom.chr;
        if rom.chr_is_ram {
            chr.resize(32 * 1024, 0);
        }
        Unrom512 {
            prg,
            flash,
            chr,
            chr_is_ram: rom.chr_is_ram,
            latch: 0,
            mirroring: rom.header.mirroring,
            banks,
            latches_upper_only: battery || matches!(submapper, 1 | 3 | 4),
            bus_conflicts,
            hv_from_latch: submapper == 3,
        }
    }

    /// The PRG flash chip, when the header's battery bit made the board self-flashable.
    pub fn flash(&self) -> Option<&Flash> {
        self.flash.as_ref()
    }

    /// `$8000-$BFFF` as a byte offset into the PRG image, which is the chip address on a flash
    /// board: the register's low five bits are A14-A18.
    fn window_offset(&self, addr: u16) -> usize {
        (usize::from(self.latch & 0x1F) % self.banks) << 14 | usize::from(addr & 0x3FFF)
    }

    /// `$C000-$FFFF` as a byte offset into the PRG image: the last 16 KB bank.
    fn fixed_offset(&self, addr: u16) -> usize {
        (self.banks - 1) << 14 | usize::from(addr & 0x3FFF)
    }

    fn read_offset(&self, offset: usize) -> u8 {
        match &self.flash {
            Some(flash) => flash.read(offset as u32),
            None => self.prg[offset],
        }
    }

    fn chr_index(&self, addr: u16) -> usize {
        usize::from((self.latch >> 5) & 0x03) * 0x2000 + usize::from(addr & 0x1FFF)
    }
}

impl Mapper for Unrom512 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => self.read_offset(self.window_offset(addr)),
            0xC000..=0xFFFF => self.read_offset(self.fixed_offset(addr)),
            _ => 0,
        }
    }

    /// CPU address to the PRG byte it reads right now, from the current register.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        match addr {
            0x8000..=0xBFFF => Some(self.window_offset(addr)),
            0xC000..=0xFFFF => Some(self.fixed_offset(addr)),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if !(0x8000..=0xFFFF).contains(&addr) {
            return;
        }
        // nesdev "Flash ROM Save": on a flashable board the whole `$8000-$BFFF` window is the
        // chip's data bus, and the register sits above it.
        if addr < 0xC000 {
            let offset = self.window_offset(addr) as u32;
            if let Some(flash) = &mut self.flash {
                flash.write(offset, val);
                return;
            }
        }
        if self.latches_upper_only && addr < 0xC000 {
            return;
        }
        // nesdev "Bus conflict": the register latches the value ANDed with what the PRG drives,
        // which is the ROM byte at the written address — at `$C000-$FFFF` the fixed last bank's.
        self.latch = if self.bus_conflicts {
            let partner = if addr < 0xC000 {
                self.window_offset(addr)
            } else {
                self.fixed_offset(addr)
            };
            val & self.read_offset(partner)
        } else {
            val
        };
    }

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

    /// nesdev "UNROM 512", register bit 7: on a one-screen header it picks the half (CIRAM A10 =
    /// 0 or 1), and on submapper 3 it picks horizontal (CIRAM A10 = PPU A11) or vertical (CIRAM
    /// A10 = PPU A10) instead.
    fn mirroring(&self) -> Mirroring {
        let bit = self.latch & 0x80 != 0;
        if self.hv_from_latch {
            return if bit {
                Mirroring::Vertical
            } else {
                Mirroring::Horizontal
            };
        }
        match self.mirroring {
            Mirroring::FourScreen if bit => Mirroring::SingleScreenUpper,
            Mirroring::FourScreen => Mirroring::SingleScreenLower,
            m => m,
        }
    }

    /// The board has no PRG RAM: the register is above the window and there is nothing at
    /// `$6000-$7FFF`.
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

    /// The whole PRG flash image is the save. The board has no PRG RAM (nesdev
    /// "UNROM 512" § Flash ROM Save: the battery bit marks the self-flashable configuration,
    /// "The Battery bit is only set if the game makes use of the self-flashing capability"), and a
    /// game saves by rewriting the flash, so the chip's image is what a `<rom>.sav` holds.
    /// [`Mapper::battery_change_key`] keeps its default, the same full image: an erased or
    /// programmed cell stays put, so an untouched flash compares equal and is not rewritten.
    fn battery_data(&self) -> Vec<u8> {
        self.flash
            .as_ref()
            .map(|flash| flash.image().to_vec())
            .unwrap_or_default()
    }

    /// Restore [`Mapper::battery_data`]. A file of any other length belongs to a different part
    /// (or is truncated) and is ignored whole, never partly applied.
    fn load_battery_data(&mut self, data: &[u8]) {
        let Some(flash) = &mut self.flash else {
            return;
        };
        let part = flash.part();
        if data.len() != part.bytes() {
            eprintln!(
                "ignoring flash save of {} bytes: the part's image is {} bytes",
                data.len(),
                part.bytes()
            );
            return;
        }
        *flash = Flash::from_image(part, data.to_vec());
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.latch);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        if let Some(flash) = &self.flash {
            flash.save_state(w);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.latch = r.u8()?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        if let Some(flash) = &mut self.flash {
            flash.load_state(r)?;
        }
        Ok(())
    }
}
