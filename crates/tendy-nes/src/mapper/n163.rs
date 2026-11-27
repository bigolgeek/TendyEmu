//! Namco 163 (iNES mapper 19) and its cost-reduced siblings the Namco 175/340 (mapper 210).
//!
//! ## Namco 163 (19)
//!
//! ```text
//! $4800-$4FFF  chip RAM data port (read/write, auto-increment through $F800)
//! $5000-$57FF  IRQ counter low (read/write; a write acknowledges the IRQ)
//! $5800-$5FFF  IRQ counter high 7 bits + enable (bit 7); a write acknowledges the IRQ
//! $8000-$DFFF  twelve 1 KB bank registers: PPU $0000-$1FFF then $2000-$2FFF ($800 spacing)
//! $E000-$E7FF  PRG bank at $8000; bit 6 disables the expansion audio
//! $E800-$EFFF  PRG bank at $A000; bits 6/7 turn CHR-RAM off for $0000/$1000
//! $F000-$F7FF  PRG bank at $C000
//! $F800-$FFFF  external-RAM write protect (upper nibble $4 enables writes) + audio address port
//! ```
//!
//! The IRQ is a 15-bit CPU-cycle *up* counter that stops at `$7FFF` and raises the line there. The
//! board also has 128 bytes of chip RAM that the audio chip owns; the same RAM is the game's
//! battery-backed save area on boards without external PRG RAM.
//!
//! Nametable banking is real on this board: registers 8-11 select a 1 KB page per nametable
//! window, either a CHR-ROM page or one of the two CIRAM pages, which is what the [nt_read]/
//! [nt_write] hooks are for. Bank values `$E0-$FF` used as *pattern* pages would mean "use CIRAM as
//! CHR-RAM"; pattern fetches have no path to the PPU's CIRAM, so those values index the same-numbered
//! CHR bank instead (the documented behaviour when CHR-RAM is disabled) — the one approximation in
//! this board.
//!
//! ## Namco 175/340 (210)
//!
//! The cheaper chips drop the IRQ, the chip RAM, the audio and (on the 340) the external PRG RAM:
//!
//! ```text
//! $8000-$BFFF  eight 1 KB CHR bank registers ($800 spacing)
//! $C000-$C7FF  external PRG RAM enable (175 only)
//! $E000-$E7FF  PRG bank at $8000; on the 340 bits 6-7 select mirroring
//! $E800-$EFFF  PRG bank at $A000
//! $F000-$F7FF  PRG bank at $C000
//! ```
//!
//! Without an NES 2.0 submapper the header cannot say which chip a 210 image uses; the wiki's rule
//! is to take a battery header as a 175 and anything else as a 340, which is what [`Namco340::new`]
//! does.

use crate::apu::expansion::Namco163Audio;
use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 19: Namco 129/163.
pub struct Namco163 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    /// Twelve 1 KB bank registers ($8000-$DFFF), pattern windows first.
    banks: [u8; 12],
    /// The three switchable 8 KB PRG banks ($E000, $E800, $F000).
    prg_banks: [u8; 3],
    /// `$E800` bits 6-7: CHR-RAM disabled for the low/high pattern half.
    chr_ram_disable: u8,
    /// `$F800` low nibble: one bit per 2 KB of external RAM, 1 = write-protected.
    ram_protect: u8,
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,
    /// The header's mirroring, used only as the fallback for accesses the board does not take
    /// (nametable banking normally answers every `$2000-$3EFF` access itself).
    mirroring: Mirroring,
    /// Header-derived: the chip RAM is part of the battery save and the chip stays muted
    /// when the header declares the board silent. Neither changes at runtime.
    battery: bool,
    no_audio: bool,
    audio: Namco163Audio,
}

impl Namco163 {
    pub fn new(rom: RomData) -> Namco163 {
        let mut audio = Namco163Audio::new();
        let submapper = if rom.header.nes2 {
            rom.header.submapper
        } else {
            0
        };
        audio.set_submapper(submapper);
        let no_audio = submapper == 1 || submapper == 2;
        let mut board = Namco163 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            banks: [0; 12],
            prg_banks: [0; 3],
            chr_ram_disable: 0,
            ram_protect: 0x0F,
            irq_counter: 0,
            irq_enabled: false,
            irq_pending: false,
            mirroring: rom.header.mirroring,
            battery: rom.header.battery,
            no_audio,
            audio,
        };
        // Submapper 1/2 is "no expansion sound"; the chip is muted rather than removed, because
        // the game still writes its registers. The `no_audio` field keeps it muted when the game
        // writes `$E000` to switch banks.
        if no_audio {
            board.audio.set_disabled(true);
        }
        board
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let bank = match addr {
            0x8000..=0x9FFF => usize::from(self.prg_banks[0] & 0x3F),
            0xA000..=0xBFFF => usize::from(self.prg_banks[1] & 0x3F),
            0xC000..=0xDFFF => usize::from(self.prg_banks[2] & 0x3F),
            _ => n - 1,
        } % n;
        bank * 0x2000 + usize::from(addr & 0x1FFF)
    }

    /// A pattern-table byte: bank register 0-7, with `$E0-$FF` treated as the CHR bank of the same
    /// number (CIRAM-as-CHR is not reachable from a pattern fetch here).
    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let bank = usize::from(self.banks[window]) % self.chr_bank_count();
        bank * 0x400 + usize::from(addr & 0x3FF)
    }

    /// A nametable byte from the bank registers 8-11, or `None` for the standard CIRAM mapping.
    fn nametable_byte(&self, addr: u16, ciram: &[u8]) -> Option<u8> {
        // $3000-$3EFF mirrors $2000-$2EFF, so the window is the low 12 bits' top two.
        let window = usize::from(((addr - 0x2000) & 0x0FFF) / 0x400);
        let value = self.banks[8 + window];
        let within = usize::from(addr & 0x3FF);
        if value >= 0xE0 {
            // CIRAM page A (even) or B (odd).
            Some(ciram[(usize::from(value & 1) * 0x400) + within])
        } else {
            Some(chr_read(&self.chr, usize::from(value) * 0x400 + within))
        }
    }

    /// The write-protect bit covering `addr` in `$6000-$7FFF`.
    fn ram_writable(&self, addr: u16) -> bool {
        let window = usize::from((addr - 0x6000) / 0x800);
        self.ram_protect & (1 << window) == 0
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0xDFFF => {
                let index = usize::from((addr - 0x8000) / 0x800);
                if index < 12 {
                    self.banks[index] = val;
                }
            }
            0xE000..=0xE7FF => {
                self.prg_banks[0] = val;
                // Bit 6 silences an ordinary board; a header-declared silent one stays muted
                // however often the game rewrites this bank register.
                self.audio.set_disabled(self.no_audio || val & 0x40 != 0);
            }
            0xE800..=0xEFFF => {
                self.prg_banks[1] = val;
                self.chr_ram_disable = val & 0xC0;
            }
            0xF000..=0xF7FF => self.prg_banks[2] = val,
            0xF800..=0xFFFF => {
                // Both the external-RAM write protect and the audio address latch live here.
                if val & 0xF0 == 0x40 {
                    self.ram_protect = val & 0x0F;
                } else {
                    self.ram_protect = 0x0F;
                }
                self.audio.write_address(val);
            }
            _ => {}
        }
    }
}

impl Mapper for Namco163 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x4800..=0x4FFF => self.audio.read_data(),
            0x5000..=0x57FF => self.irq_counter as u8,
            0x5800..=0x5FFF => {
                (u8::from(self.irq_enabled) << 7) | ((self.irq_counter >> 8) as u8 & 0x7F)
            }
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr - 0x6000) % self.prg_ram.len()],
            0x8000..=0xFFFF => self.prg[self.prg_offset(addr)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x4800..=0x4FFF => self.audio.write_data(val),
            0x5000..=0x57FF => {
                self.irq_counter = (self.irq_counter & 0x7F00) | u16::from(val);
                self.irq_pending = false;
            }
            0x5800..=0x5FFF => {
                self.irq_enabled = val & 0x80 != 0;
                self.irq_counter = (self.irq_counter & 0x00FF) | (u16::from(val & 0x7F) << 8);
                self.irq_pending = false;
            }
            0x6000..=0x7FFF => {
                if self.ram_writable(addr) {
                    let i = usize::from(addr - 0x6000) % self.prg_ram.len();
                    self.prg_ram[i] = val;
                }
            }
            0x8000..=0xFFFF => self.write_register(addr, val),
            _ => {}
        }
    }

    fn ext_read(&mut self, addr: u16) -> Option<u8> {
        if (0x4800..=0x5FFF).contains(&addr) {
            Some(self.cpu_read(addr))
        } else {
            None
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_offset(addr))
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let i = self.chr_offset(addr);
            chr_write(&mut self.chr, i, val);
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_offset(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_offset(addr))
    }

    fn nt_read(&mut self, addr: u16, ciram: &[u8]) -> Option<u8> {
        self.nametable_byte(addr, ciram)
    }

    /// The same lookup as `nt_read`, which is already side-effect-free.
    fn nt_peek(&self, addr: u16, ciram: &[u8]) -> Option<u8> {
        self.nametable_byte(addr, ciram)
    }

    fn nt_write(&mut self, addr: u16, val: u8, ciram: &mut [u8]) -> bool {
        let window = usize::from(((addr - 0x2000) & 0x0FFF) / 0x400);
        let bank = self.banks[8 + window];
        let within = usize::from(addr & 0x3FF);
        if bank >= 0xE0 {
            let page = usize::from(bank & 1);
            ciram[page * 0x400 + within] = val;
        } else if self.chr_is_ram {
            let i = usize::from(bank) * 0x400 + within;
            chr_write(&mut self.chr, i, val);
        }
        true
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

    /// The chip's 128 bytes of RAM are where a Namco 163 game saves when the board has no external
    /// PRG RAM, so a battery board persists them after the 8 KB of PRG RAM.
    fn battery_data(&self) -> Vec<u8> {
        let mut out = self.prg_ram.clone();
        if self.battery {
            out.extend_from_slice(self.audio.ram());
        }
        out
    }

    /// Restore [`Mapper::battery_data`]: PRG RAM first, then whatever follows it is the chip
    /// RAM. An 8 KB `.sav` that ends after the PRG RAM has nothing there and loads unchanged.
    fn load_battery_data(&mut self, data: &[u8]) {
        copy_prefix(&mut self.prg_ram, data);
        if self.battery && data.len() > self.prg_ram.len() {
            let tail = &data[self.prg_ram.len()..];
            let mut ram = *self.audio.ram();
            let m = tail.len().min(ram.len());
            ram[..m].copy_from_slice(&tail[..m]);
            self.audio.load_ram(&ram);
        }
    }

    /// The chip rewrites each enabled channel's phase counters — RAM bytes 1, 3 and 5 of its
    /// eight-byte block — on every channel update, one channel every 15 CPU cycles. They are the
    /// chip's running phase, not save data, so the autosave's change key blanks them: without this
    /// the chip-RAM block moves on its own and every 5-second autosave rewrites all 8320 bytes of
    /// the `.sav`. [`Mapper::battery_data`], which is what the file gets, is untouched.
    fn battery_change_key(&self) -> Vec<u8> {
        let mut out = self.prg_ram.clone();
        if self.battery {
            out.extend_from_slice(&self.audio.ram_for_change_check());
        }
        out
    }

    fn chr_ram(&self) -> &[u8] {
        chr_ram_slice(&self.chr, self.chr_is_ram)
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        if self.chr_is_ram {
            copy_prefix(&mut self.chr, data);
        }
    }

    fn hooks(&self) -> CartHooks {
        CartHooks {
            irq: true,
            cpu_clock: true,
        }
    }

    fn cpu_clock(&mut self) {
        if self.irq_enabled && self.irq_counter < 0x7FFF {
            self.irq_counter += 1;
            if self.irq_counter == 0x7FFF {
                self.irq_pending = true;
            }
        }
        self.audio.clock();
    }

    fn expansion_audio(&self) -> f32 {
        crate::apu::Apu::amplitude_to_pulse_units(self.audio.output())
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("N163")
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.bytes(&self.banks);
        w.bytes(&self.prg_banks);
        w.u8(self.chr_ram_disable);
        w.u8(self.ram_protect);
        w.u16(self.irq_counter);
        w.bool(self.irq_enabled);
        w.bool(self.irq_pending);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        r.bytes_into(&mut self.banks)?;
        r.bytes_into(&mut self.prg_banks)?;
        self.chr_ram_disable = r.u8()?;
        self.ram_protect = r.u8()?;
        self.irq_counter = r.u16()?;
        self.irq_enabled = r.bool()?;
        self.irq_pending = r.bool()?;
        self.audio.load_state(r)?;
        Ok(())
    }
}

/// Mapper 210: Namco 175 (submapper 1) or Namco 340 (submapper 2).
pub struct Namco340 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    /// Chip RAM is present only on the 175.
    has_ram: bool,
    /// The 340 selects mirroring from `$E000`; the 175's mirroring is hard-wired from the header.
    mirroring_from_register: bool,
    ram_enabled: bool,
    chr_banks: [u8; 8],
    prg_banks: [u8; 3],
    mirroring: Mirroring,
}

impl Namco340 {
    pub fn new(rom: RomData) -> Namco340 {
        let submapper = if rom.header.nes2 {
            rom.header.submapper
        } else {
            0
        };
        let has_ram = match submapper {
            1 => true,
            2 => false,
            // No submapper: the wiki's rule — a battery header means a 175, anything else a 340.
            _ => rom.header.battery,
        };
        Namco340 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            has_ram,
            mirroring_from_register: !has_ram,
            ram_enabled: false,
            chr_banks: [0; 8],
            prg_banks: [0; 3],
            mirroring: rom.header.mirroring,
        }
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let bank = match addr {
            0x8000..=0x9FFF => usize::from(self.prg_banks[0] & 0x3F),
            0xA000..=0xBFFF => usize::from(self.prg_banks[1] & 0x3F),
            0xC000..=0xDFFF => usize::from(self.prg_banks[2] & 0x3F),
            _ => n - 1,
        } % n;
        bank * 0x2000 + usize::from(addr & 0x1FFF)
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let bank = usize::from(self.chr_banks[window]) % self.chr_bank_count();
        bank * 0x400 + usize::from(addr & 0x3FF)
    }
}

impl Mapper for Namco340 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF if self.has_ram && self.ram_enabled => {
                self.prg_ram[usize::from(addr - 0x6000) % self.prg_ram.len()]
            }
            0x6000..=0x7FFF => 0,
            0x8000..=0xFFFF => self.prg[self.prg_offset(addr)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.has_ram && self.ram_enabled {
                    let i = usize::from(addr - 0x6000) % self.prg_ram.len();
                    self.prg_ram[i] = val;
                }
            }
            0x8000..=0xBFFF => {
                let index = usize::from((addr - 0x8000) / 0x800);
                self.chr_banks[index] = val;
            }
            0xC000..=0xC7FF => {
                if self.has_ram {
                    self.ram_enabled = val & 0x01 != 0;
                }
            }
            0xE000..=0xE7FF => {
                self.prg_banks[0] = val;
                if self.mirroring_from_register {
                    self.mirroring = match (val >> 6) & 0x03 {
                        0 => Mirroring::SingleScreenLower,
                        1 => Mirroring::Vertical,
                        2 => Mirroring::SingleScreenUpper,
                        _ => Mirroring::Horizontal,
                    };
                }
            }
            0xE800..=0xEFFF => self.prg_banks[1] = val,
            0xF000..=0xF7FF => self.prg_banks[2] = val,
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_offset(addr))
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let i = self.chr_offset(addr);
            chr_write(&mut self.chr, i, val);
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, self.chr_offset(addr))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(&self.chr, self.chr_is_ram, self.chr_offset(addr))
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

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.bool(self.ram_enabled);
        w.bytes(&self.chr_banks);
        w.bytes(&self.prg_banks);
        w.u8(self.mirroring.to_tag());
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        self.ram_enabled = r.bool()?;
        r.bytes_into(&mut self.chr_banks)?;
        r.bytes_into(&mut self.prg_banks)?;
        self.mirroring = Mirroring::from_tag(r.u8()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    fn rom_data(mapper: u16, submapper: u8, battery: bool) -> RomData {
        RomData {
            header: Header {
                prg_banks: 16, // 256 KB = 32 x 8 KB
                chr_banks: 16, // 128 KB = 128 x 1 KB
                mapper,
                mirroring: Mirroring::Vertical,
                submapper,
                nes2: submapper != 0,
                battery,
                ..Default::default()
            },
            prg: (0..0x40000).map(|i| (i / 0x2000) as u8).collect(),
            chr: (0..0x20000).map(|i| (i / 0x400) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        }
    }

    fn n163() -> Namco163 {
        Namco163::new(rom_data(19, 0, false))
    }

    /// Write one byte of chip RAM: latch the address, clock the latch in, then write the data. The
    /// `$F800` byte doubles as the external-RAM write protect, so this leaves it all writable.
    fn poke(m: &mut Namco163, addr: u8, val: u8) {
        m.cpu_write(0xF800, addr & 0x7F);
        m.cpu_clock();
        m.cpu_write(0x4800, val);
    }

    #[test]
    fn prg_windows_and_the_fixed_last_bank() {
        let mut m = n163();
        m.cpu_write(0xE000, 1);
        m.cpu_write(0xE800, 2);
        m.cpu_write(0xF000, 3);
        assert_eq!(m.cpu_read(0x8000), 1);
        assert_eq!(m.cpu_read(0xA000), 2);
        assert_eq!(m.cpu_read(0xC000), 3);
        assert_eq!(m.cpu_read(0xE000), 31);
    }

    #[test]
    fn pattern_banks_come_from_the_first_eight_registers() {
        let mut m = n163();
        for i in 0..8u16 {
            m.cpu_write(0x8000 + i * 0x800, u8::try_from(i).unwrap() + 8);
        }
        for i in 0..8u16 {
            assert_eq!(m.ppu_read(i * 0x400), 8 + i as u8);
        }
    }

    #[test]
    fn nametable_banks_map_chr_pages() {
        let mut m = n163();
        let ciram = [0u8; 0x800];
        m.cpu_write(0xC000, 0x11); // $2000 -> CHR bank $11
        m.cpu_write(0xC800, 0x12); // $2400 -> CHR bank $12
        assert_eq!(m.nt_read(0x2000, &ciram), Some(0x11));
        assert_eq!(m.nt_read(0x2410, &ciram), Some(0x12));
        // $3000-$3EFF mirrors $2000-$2EFF and must not index past the register array.
        assert_eq!(m.nt_read(0x3000, &ciram), Some(0x11));
        assert_eq!(
            m.nt_read(0x3EFF, &ciram),
            Some(m.nt_read(0x2EFF, &ciram).unwrap())
        );
        // CIRAM pages: $E0 = page A, $E1 = page B.
        m.cpu_write(0xD000, 0xE0);
        m.cpu_write(0xD800, 0xE1);
        let mut ciram = [0u8; 0x800];
        ciram[0x0005] = 0xAA;
        ciram[0x0405] = 0xBB;
        assert_eq!(m.nt_read(0x2805, &ciram), Some(0xAA));
        assert_eq!(m.nt_read(0x2C05, &ciram), Some(0xBB));
    }

    /// The read-only `nt_peek` must follow the same `bank >= 0xE0` branch as `nt_read`: the
    /// viewer shows the CIRAM page straight out of the PPU's 4 KB, not a CHR page.
    #[test]
    fn nt_peek_reads_the_ciram_page_branch() {
        let mut m = n163();
        m.cpu_write(0xC000, 0xE0); // $2000 -> CIRAM page A
        m.cpu_write(0xC800, 0xE1); // $2400 -> CIRAM page B
        let mut ciram = [0u8; 0x800];
        ciram[0x0010] = 0xAA;
        ciram[0x0410] = 0xBB;
        assert_eq!(m.nt_peek(0x2010, &ciram), Some(0xAA));
        assert_eq!(m.nt_peek(0x2410, &ciram), Some(0xBB));
        assert_eq!(m.nt_peek(0x3010, &ciram), Some(0xAA), "$3000 mirrors $2000");
    }

    #[test]
    fn nametable_writes_land_in_the_selected_page() {
        let mut m = n163();
        let mut ciram = [0u8; 0x800];
        m.cpu_write(0xC000, 0xE1); // page B
        assert!(m.nt_write(0x2000, 0x5A, &mut ciram));
        assert_eq!(ciram[0x0400], 0x5A);
        assert_eq!(ciram[0x0000], 0x00);
    }

    #[test]
    fn chip_ram_port_reads_and_writes_through_the_address_latch() {
        let mut m = n163();
        poke(&mut m, 0x10, 0x99);
        m.cpu_write(0xF800, 0x10);
        m.cpu_clock();
        assert_eq!(m.cpu_read(0x4800), 0x99);
        assert_eq!(
            m.ext_read(0x4800),
            Some(0x99),
            "the bus path sees the port too"
        );
    }

    #[test]
    fn external_ram_write_protect() {
        let mut m = n163();
        // Power-on protect: every 2 KB window read-only.
        m.cpu_write(0x6000, 0x11);
        assert_eq!(m.cpu_read(0x6000), 0);
        // $4x enables writes for the windows whose bit is clear.
        m.cpu_write(0xF800, 0x42); // protect windows 1-3, free window 0
        m.cpu_write(0x6100, 0x22);
        m.cpu_write(0x6900, 0x33);
        assert_eq!(m.cpu_read(0x6100), 0x22);
        assert_eq!(m.cpu_read(0x6900), 0, "window 1 is still protected");
        // A value outside $40-$4E protects everything again.
        m.cpu_write(0xF800, 0x00);
        m.cpu_write(0x6200, 0x44);
        assert_eq!(m.cpu_read(0x6200), 0, "the window is write-protected again");
    }

    #[test]
    fn irq_counts_up_and_stops_at_7fff() {
        let mut m = n163();
        m.cpu_write(0x5000, 0xFD);
        m.cpu_write(0x5800, 0x80 | 0x7F); // enable, high byte $7F -> $7FFD
        assert_eq!(m.cpu_read(0x5000), 0xFD);
        assert_eq!(m.cpu_read(0x5800) & 0x80, 0x80);
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0x7FFE);
        assert_eq!(m.cpu_read(0x5000), 0xFE, "the low byte is readable");
        assert_eq!(m.cpu_read(0x5800), 0x80 | 0x7F);
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0x7FFF);
        assert!(m.irq_pending());
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0x7FFF, "the counter stops at $7FFF");
    }

    #[test]
    fn irq_writes_acknowledge() {
        let mut m = n163();
        m.cpu_write(0x5000, 0xFE);
        m.cpu_write(0x5800, 0x80 | 0x7F); // $7FFE
        m.cpu_clock();
        assert!(m.irq_pending());
        m.cpu_write(0x5000, 0x00);
        assert!(!m.irq_pending());
        assert_eq!(m.irq_counter & 0xFF, 0x00);
    }

    #[test]
    fn disabled_irq_does_not_count() {
        let mut m = n163();
        m.cpu_write(0x5000, 0x00);
        m.cpu_write(0x5800, 0x00);
        for _ in 0..100 {
            m.cpu_clock();
        }
        assert_eq!(m.irq_counter, 0);
    }

    #[test]
    fn audio_is_audible_and_can_be_disabled_by_the_prg_register() {
        let mut m = n163();
        poke(&mut m, 0x00, 0x0F); // wave sample 0 = $F
        poke(&mut m, 0x7E, 0x00); // wave address
        poke(&mut m, 0x7F, 0x0F); // channel 8, volume 15
        for _ in 0..32 {
            m.cpu_clock();
        }
        assert!(m.expansion_audio() > 0.0);
        m.cpu_write(0xE000, 0x40); // bit 6: disable sound
        m.cpu_clock();
        assert_eq!(m.expansion_audio(), 0.0);
    }

    #[test]
    fn submapper_two_silences_the_chip() {
        let mut m = Namco163::new(rom_data(19, 2, false));
        poke(&mut m, 0x00, 0x0F);
        poke(&mut m, 0x7E, 0x00);
        poke(&mut m, 0x7F, 0x0F);
        for _ in 0..32 {
            m.cpu_clock();
        }
        assert_eq!(m.expansion_audio(), 0.0);
    }

    /// Games write `$E000` constantly to switch PRG banks, so a board the header declares
    /// silent must stay muted even when bit 6 is clear on those writes.
    #[test]
    fn header_declared_silent_board_stays_muted_across_prg_register_writes() {
        for submapper in [1u8, 2] {
            let mut m = Namco163::new(rom_data(19, submapper, false));
            poke(&mut m, 0x00, 0x0F);
            poke(&mut m, 0x7E, 0x00);
            poke(&mut m, 0x7F, 0x0F);
            m.cpu_write(0xE000, 0x05); // bank 5, bit 6 clear: "sound on" on an ordinary board
            for _ in 0..32 {
                m.cpu_clock();
            }
            assert_eq!(
                m.expansion_audio(),
                0.0,
                "submapper {submapper} stays silent"
            );
        }
    }

    /// The other side: an ordinary board still follows `$E000` bit 6 both ways.
    #[test]
    fn ordinary_board_still_follows_the_prg_register() {
        let mut m = Namco163::new(rom_data(19, 0, false));
        poke(&mut m, 0x00, 0x0F);
        poke(&mut m, 0x7E, 0x00);
        poke(&mut m, 0x7F, 0x0F);
        m.cpu_write(0xE000, 0x40); // bit 6: mute
        for _ in 0..32 {
            m.cpu_clock();
        }
        assert_eq!(m.expansion_audio(), 0.0);
        m.cpu_write(0xE000, 0x00); // bit 6 clear: sound again
        m.cpu_clock();
        assert!(m.expansion_audio() > 0.0);
    }

    #[test]
    fn state_round_trips() {
        let mut a = n163();
        a.cpu_write(0xE000, 5);
        a.cpu_write(0xC000, 0x33);
        a.cpu_write(0x5000, 0x10);
        a.cpu_write(0x5800, 0x80);
        poke(&mut a, 0x7F, 0x0F);
        for _ in 0..100 {
            a.cpu_clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = n163();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.cpu_read(0x8000), b.cpu_read(0x8000));
        assert_eq!(a.irq_counter, b.irq_counter);
        assert_eq!(a.expansion_audio(), b.expansion_audio());
        let ciram = [0u8; 0x800];
        assert_eq!(a.nt_read(0x2000, &ciram), b.nt_read(0x2000, &ciram));
    }

    fn n340(submapper: u8, battery: bool) -> Namco340 {
        Namco340::new(rom_data(210, submapper, battery))
    }

    #[test]
    fn namco340_chr_and_prg_banking() {
        let mut m = n340(2, false);
        for i in 0..8u8 {
            m.cpu_write(0x8000 + u16::from(i) * 0x800, i + 4);
        }
        for i in 0..8u16 {
            assert_eq!(m.ppu_read(i * 0x400), 4 + i as u8);
        }
        m.cpu_write(0xE000, 3);
        m.cpu_write(0xE800, 4);
        m.cpu_write(0xF000, 5);
        assert_eq!(m.cpu_read(0x8000), 3);
        assert_eq!(m.cpu_read(0xA000), 4);
        assert_eq!(m.cpu_read(0xC000), 5);
        assert_eq!(m.cpu_read(0xE000), 31);
    }

    #[test]
    fn namco340_mirroring_comes_from_the_prg_register() {
        let mut m = n340(2, false);
        for (bits, want) in [
            (0u8, Mirroring::SingleScreenLower),
            (1, Mirroring::Vertical),
            (2, Mirroring::SingleScreenUpper),
            (3, Mirroring::Horizontal),
        ] {
            m.cpu_write(0xE000, bits << 6);
            assert_eq!(m.mirroring(), want);
        }
    }

    #[test]
    fn namco175_has_wram_and_hardwired_mirroring() {
        let mut m = n340(1, true);
        assert_eq!(m.mirroring(), Mirroring::Vertical, "from the header");
        m.cpu_write(0xE000, 0xC0); // mirroring bits are ignored on the 175
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0, "RAM needs the enable bit");
        m.cpu_write(0xC000, 0x01);
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0x5A);
    }

    #[test]
    fn namco210_without_a_submapper_uses_the_battery_heuristic() {
        let no_battery = n340(0, false);
        assert!(!no_battery.has_ram, "treated as a 340");
        let battery = n340(0, true);
        assert!(battery.has_ram, "treated as a 175");
    }

    #[test]
    fn namco340_state_round_trips() {
        let mut a = n340(2, false);
        a.cpu_write(0x8000, 9);
        a.cpu_write(0xE000, 0x40);
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = n340(2, false);
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.ppu_read(0x0000), b.ppu_read(0x0000));
        assert_eq!(a.mirroring(), b.mirroring());
    }

    fn nvram_board(chr_is_ram: bool) -> Namco163 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Namco163::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 19,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr,
            chr_is_ram,
            trainer: None,
        })
    }

    #[test]
    fn chr_ram_is_exposed_for_saving() {
        let mut m = nvram_board(true);
        m.ppu_write(0x0010, 0x44);
        assert_eq!(m.chr_ram().len(), 8192);
        assert_eq!(m.chr_ram()[0x10], 0x44);
    }

    #[test]
    fn chr_rom_is_not_exposed_for_saving() {
        let m = nvram_board(false);
        assert!(m.chr_ram().is_empty());
    }

    #[test]
    fn load_chr_ram_copies() {
        let mut m = nvram_board(true);
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0002), 7);
        assert_eq!(m.ppu_read(0x0003), 0);
    }

    #[test]
    fn load_chr_ram_is_ignored_for_chr_rom() {
        let mut m = nvram_board(false);
        m.load_chr_ram(&[5, 6, 7]);
        assert_eq!(m.ppu_read(0x0000), 0x11);
    }

    /// A battery board saves the 8 KB of PRG RAM followed by the chip's 128 bytes, and the
    /// load restores both halves.
    #[test]
    fn battery_save_carries_the_chip_ram() {
        let mut m = Namco163::new(rom_data(19, 0, true));
        m.cpu_write(0xF800, 0x40); // enable writes to every external-RAM window
        m.cpu_write(0x6000, 0x5A);
        poke(&mut m, 0x10, 0xA5);
        let data = m.battery_data();
        assert_eq!(data.len(), 8192 + crate::apu::expansion::n163::N163_RAM_LEN);
        assert_eq!(data[0x0000], 0x5A, "PRG RAM comes first");
        assert_eq!(data[8192 + 0x10], 0xA5, "then the chip RAM");

        let mut fresh = Namco163::new(rom_data(19, 0, true));
        fresh.load_battery_data(&data);
        assert_eq!(fresh.cpu_read(0x6000), 0x5A);
        assert_eq!(fresh.audio.ram()[0x10], 0xA5);

        // A board without a battery keeps the plain 8 KB block.
        let plain = Namco163::new(rom_data(19, 0, false));
        assert_eq!(plain.battery_data().len(), 8192);
    }

    /// An 8 KB `.sav` with only PRG RAM still loads, leaving the chip RAM at its power-on state.
    #[test]
    fn legacy_prg_battery_save_still_loads() {
        let mut m = Namco163::new(rom_data(19, 0, true));
        m.load_battery_data(&[0x33; 8192]);
        assert_eq!(m.cpu_read(0x6000), 0x33);
        assert!(m.audio.ram().iter().all(|&b| b == 0));
    }

    /// The chip moves its own phase counters in chip RAM every channel update, so
    /// [`Mapper::battery_data`] changes without the game writing anything. The autosave's change
    /// key blanks them, or every 5-second autosave on a mapper-19 board would rewrite all 8320
    /// bytes of the `.sav`; a write from the game must still show up.
    #[test]
    fn battery_change_key_ignores_the_running_chip_phase() {
        let mut m = Namco163::new(rom_data(19, 0, true));
        m.cpu_write(0xF800, 0x40); // writable external RAM
        poke(&mut m, 0x78, 0x10); // channel 8 frequency low: the phase advances
        poke(&mut m, 0x7C, 0x00); // wave length 256 samples
        poke(&mut m, 0x7E, 0x00); // wave address 0
        poke(&mut m, 0x7F, 0x0F); // channel 8 alone enabled, volume 15
        let key = m.battery_change_key();
        let data = m.battery_data();
        for _ in 0..64 {
            m.cpu_clock();
        }
        assert!(
            m.battery_data() != data,
            "the chip moves its own phase bytes"
        );
        assert!(m.battery_change_key() == key, "the change key ignores them");

        poke(&mut m, 0x10, 0x77); // a wavetable byte the game wrote
        assert!(
            m.battery_change_key() != key,
            "a real write still changes the key"
        );
    }
}
