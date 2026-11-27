//! Konami VRC6 (iNES mappers 24 and 26) with its three-channel expansion audio.
//!
//! ```text
//! $6000-$7FFF  8 KB PRG RAM (enabled by $B003 bit 7)
//! $8000-$BFFF  16 KB switchable PRG ROM  ($8000-$8003, 4 bits)
//! $C000-$DFFF  8 KB switchable PRG ROM   ($C000-$C003, 5 bits)
//! $E000-$FFFF  8 KB PRG ROM, fixed to the last bank
//! PPU $0000-$1FFF  eight 1 KB CHR banks: R0-R3 at $D000-$D003, R4-R7 at $E000-$E003
//! $9000-$9003  audio pulse 1 + frequency scaling
//! $A000-$A002  audio pulse 2
//! $B000-$B002  audio sawtooth
//! $B003        PPU banking style: PRG RAM enable, A10 source, ROM nametables, mirroring, CHR mode
//! $F000-$F002  IRQ latch, control, acknowledge
//! ```
//!
//! The VRC6a (mapper 24) and VRC6b (mapper 26) boards wire address bits A0 and A1 to the chip the
//! other way round; the board swaps those two bits before decoding, so everything below sees a
//! mapper-24 address.
//!
//! CHR banking follows the wiki's three modes for the pattern tables. Commercial games all use
//! mode 0 (eight independent 1 KB banks) and CIRAM nametables; modes 1-3 are implemented for the
//! pattern tables, while the ROM-nametable path (`$B003` bit 4) is not — those boards had extra
//! nametable RAM that no released game used, so nametables stay with CIRAM and the mode-0
//! mirroring rules.
//!
//! The audio chip itself lives in [`crate::apu::expansion::vrc6`]; this board only decodes the
//! registers and forwards the chip's level through [`Mapper::expansion_audio`].

use crate::apu::expansion::Vrc6Audio;
use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::mapper::vrc2_4::VrcIrq;
use crate::state::{StateError, StateReader, StateWriter};

pub struct Vrc6 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    /// `$8000-$8003` (16 KB bank) and `$C000-$C003` (8 KB bank).
    prg_16k: u8,
    prg_8k: u8,
    /// R0-R7, the CHR bank registers at `$D000-$E003`.
    chr_regs: [u8; 8],
    /// `$B003`.
    style: u8,
    mirroring: Mirroring,
    /// Mapper 26: swap address bits A0/A1 on the way into the register file.
    swap_bits: bool,
    audio: Vrc6Audio,
    pub irq: VrcIrq,
}

impl Vrc6 {
    pub fn new(rom: RomData) -> Vrc6 {
        Vrc6 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            prg_16k: 0,
            prg_8k: 0,
            chr_regs: [0; 8],
            style: 0,
            mirroring: rom.header.mirroring,
            swap_bits: rom.header.mapper == 26,
            audio: Vrc6Audio::new(),
            irq: VrcIrq::new(),
        }
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    fn decode(&self, addr: u16) -> u16 {
        if self.swap_bits && (addr & 0x03 == 0x01 || addr & 0x03 == 0x02) {
            addr ^ 0x03
        } else {
            addr
        }
    }

    /// The 1 KB CHR register a window uses, and whether the window's A10 comes from the PPU
    /// (`$B003` bit 5 set, the commercial configuration).
    fn chr_window(&self, window: usize) -> (u8, bool) {
        let passthrough = self.style & 0x20 != 0;
        match self.style & 0x03 {
            // Mode 0: eight independent 1 KB banks.
            0 => (self.chr_regs[window], false),
            // Mode 1: R0 | R1 (2 KB) | R3 | R2 (2 KB) | R3 (2 KB).
            1 => {
                let reg = match window {
                    0 => 0,
                    1 | 2 => 1,
                    3 => 3,
                    4 | 5 => 2,
                    _ => 3,
                };
                (self.chr_regs[reg], passthrough)
            }
            // Modes 2 and 3: four 2 KB banks R0 | R2 | R4 | R5.
            _ => {
                let reg = match window {
                    0 | 1 => 0,
                    2 | 3 => 2,
                    4 | 5 => 4,
                    _ => 5,
                };
                (self.chr_regs[reg], passthrough)
            }
        }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let (reg, split) = self.chr_window(window);
        let bank = if split {
            (u16::from(reg) & 0xFE) | (window as u16 & 1)
        } else {
            u16::from(reg)
        };
        let bank = usize::from(bank) % self.chr_bank_count();
        bank * 0x400 + usize::from(addr & 0x3FF)
    }

    fn prg_ram_enabled(&self) -> bool {
        self.style & 0x80 != 0
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let last = n - 1;
        let bank = match addr {
            0x8000..=0xBFFF => usize::from(self.prg_16k & 0x0F) * 2 + usize::from((addr >> 13) & 1),
            0xC000..=0xDFFF => usize::from(self.prg_8k & 0x1F),
            _ => last,
        } % n;
        bank * 0x2000 + usize::from(addr & 0x1FFF)
    }

    fn mirroring_mode(&self) -> Mirroring {
        match self.style & 0x0C {
            0x00 => Mirroring::Vertical,
            0x04 => Mirroring::Horizontal,
            0x08 => Mirroring::SingleScreenLower,
            _ => Mirroring::SingleScreenUpper,
        }
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        let addr = self.decode(addr);
        match addr {
            0x8000..=0x8003 => self.prg_16k = val,
            0x9000..=0x9003 | 0xA000..=0xA002 | 0xB000..=0xB002 => self.audio.write(addr, val),
            0xB003 => {
                self.style = val;
                self.mirroring = self.mirroring_mode();
            }
            0xC000..=0xC003 => self.prg_8k = val,
            0xD000..=0xD003 => self.chr_regs[usize::from(addr & 0x03)] = val,
            0xE000..=0xE003 => self.chr_regs[4 + usize::from(addr & 0x03)] = val,
            0xF000 => self.irq.write_latch(val),
            0xF001 => self.irq.write_control(val),
            0xF002 => self.irq.acknowledge(),
            _ => {}
        }
    }
}

impl Mapper for Vrc6 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enabled() {
                    self.prg_ram[usize::from(addr - 0x6000) % self.prg_ram.len()]
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => self.prg[self.prg_offset(addr)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enabled() {
                    let i = usize::from(addr - 0x6000) % self.prg_ram.len();
                    self.prg_ram[i] = val;
                }
            }
            0x8000..=0xFFFF => self.write_register(addr, val),
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
        self.irq.clock();
        self.audio.clock();
    }

    fn expansion_audio(&self) -> f32 {
        crate::apu::Apu::amplitude_to_pulse_units(self.audio.output())
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("VRC6")
    }

    fn irq_pending(&self) -> bool {
        self.irq.pending
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.u8(self.prg_16k);
        w.u8(self.prg_8k);
        w.bytes(&self.chr_regs);
        w.u8(self.style);
        w.u8(self.mirroring.to_tag());
        self.irq.save_state(w);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        self.prg_16k = r.u8()?;
        self.prg_8k = r.u8()?;
        r.bytes_into(&mut self.chr_regs)?;
        self.style = r.u8()?;
        self.mirroring = Mirroring::from_tag(r.u8()?)?;
        self.irq.load_state(r)?;
        self.audio.load_state(r)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    fn rom_data(mapper: u16) -> RomData {
        RomData {
            header: Header {
                prg_banks: 8,  // 128 KB = 16 x 8 KB banks
                chr_banks: 16, // 128 KB = 128 x 1 KB banks
                mapper,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: (0..0x20000).map(|i| (i / 0x2000) as u8).collect(),
            chr: (0..0x20000).map(|i| (i / 0x400) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        }
    }

    #[test]
    fn prg_windows() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0x8000, 2); // 16 KB bank 2 -> 8 KB banks 4 and 5
        m.cpu_write(0xC000, 9);
        assert_eq!(m.cpu_read(0x8000), 4);
        assert_eq!(m.cpu_read(0xA000), 5);
        assert_eq!(m.cpu_read(0xC000), 9);
        assert_eq!(m.cpu_read(0xE000), 15, "the last 8 KB bank is fixed");
    }

    #[test]
    fn prg_ram_is_gated_by_the_style_register() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0);
        m.cpu_write(0xB003, 0x80);
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0x5A);
    }

    #[test]
    fn chr_mode_zero_maps_eight_banks() {
        let mut m = Vrc6::new(rom_data(24));
        for i in 0..4u8 {
            m.cpu_write(0xD000 + u16::from(i), i + 8);
        }
        for i in 0..4u8 {
            m.cpu_write(0xE000 + u16::from(i), i + 20);
        }
        for i in 0..8u16 {
            assert_eq!(
                m.ppu_read(i * 0x400),
                if i < 4 { 8 + i as u8 } else { 16 + i as u8 }
            );
        }
    }

    #[test]
    fn chr_mode_one_uses_two_kilobyte_banks() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0xB003, 0x21); // bit 5 set (A10 from the PPU), mode 1
        for i in 0..8u8 {
            m.cpu_write(
                0xD000 + u16::from(i % 4) + if i >= 4 { 0x1000 } else { 0 },
                i,
            );
        }
        // R1 at $0400 and $0800, split by PPU A10 (bit 10 selects the half).
        m.cpu_write(0xD001, 0x10);
        assert_eq!(m.ppu_read(0x0400), 0x11, "A10 = 1 picks the second half");
        assert_eq!(m.ppu_read(0x0800), 0x10, "A10 = 0 picks the first half");
        // R2 at $1000 and $1400.
        m.cpu_write(0xD002, 0x20);
        assert_eq!(m.ppu_read(0x1000), 0x20);
        assert_eq!(m.ppu_read(0x1400), 0x21);
    }

    #[test]
    fn chr_without_a10_passthrough_uses_the_register_lsb() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0xB003, 0x01); // bit 5 clear, mode 1
        m.cpu_write(0xD001, 0x11);
        assert_eq!(m.ppu_read(0x0400), 0x11);
        assert_eq!(m.ppu_read(0x0800), 0x11, "the PPU's A10 is not used");
    }

    #[test]
    fn style_register_sets_mirroring() {
        let mut m = Vrc6::new(rom_data(24));
        for (style, want) in [
            (0x00u8, Mirroring::Vertical),
            (0x04, Mirroring::Horizontal),
            (0x08, Mirroring::SingleScreenLower),
            (0x0C, Mirroring::SingleScreenUpper),
        ] {
            m.cpu_write(0xB003, style);
            assert_eq!(m.mirroring(), want);
        }
    }

    #[test]
    fn vrc6b_swaps_address_bits_zero_and_one() {
        // On mapper 26 the CHR registers move: $D001 selects R2, not R1.
        let mut b = Vrc6::new(rom_data(26));
        b.cpu_write(0xD001, 9);
        assert_eq!(b.ppu_read(0x0800), 9, "R2 on a VRC6b board");
        assert_ne!(b.ppu_read(0x0400), 9, "R1 is untouched");

        let mut a = Vrc6::new(rom_data(24));
        a.cpu_write(0xD001, 9);
        assert_eq!(a.ppu_read(0x0400), 9, "R1 on a VRC6a board");

        // The 16 KB PRG register accepts every address in $8000-$8003 on both boards.
        b.cpu_write(0x8001, 5);
        assert_eq!(b.cpu_read(0x8000), 10);
    }

    #[test]
    fn audio_writes_reach_the_chip() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0x9000, 0x7F); // pulse 1: mode on, volume 15
        m.cpu_write(0x9001, 0x00);
        m.cpu_write(0x9002, 0x80); // enable, period 0
        let mut peak: f32 = 0.0;
        for _ in 0..200 {
            m.cpu_clock();
            peak = peak.max(m.expansion_audio());
        }
        assert!(peak > 0.0);
    }

    #[test]
    fn irq_registers_are_decoded() {
        let mut m = Vrc6::new(rom_data(24));
        m.cpu_write(0xF000, 0x00);
        m.cpu_write(0xF001, 0x06); // enabled, cycle mode
        assert!(!m.irq_pending());
        for _ in 0..256 {
            m.cpu_clock();
        }
        assert!(m.irq_pending());
        m.cpu_write(0xF002, 0);
        assert!(!m.irq_pending());
    }

    #[test]
    fn state_round_trips() {
        let mut a = Vrc6::new(rom_data(24));
        a.cpu_write(0x8000, 3);
        a.cpu_write(0xB003, 0x84);
        a.cpu_write(0xD000, 7);
        a.cpu_write(0x9000, 0x7F);
        a.cpu_write(0x9002, 0x81);
        for _ in 0..500 {
            a.cpu_clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Vrc6::new(rom_data(24));
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.expansion_audio(), b.expansion_audio());
        assert_eq!(a.cpu_read(0x8000), b.cpu_read(0x8000));
        assert_eq!(a.ppu_read(0x0000), b.ppu_read(0x0000));
        for _ in 0..500 {
            a.cpu_clock();
            b.cpu_clock();
            assert_eq!(a.expansion_audio(), b.expansion_audio());
        }
    }

    fn nvram_board(chr_is_ram: bool) -> Vrc6 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Vrc6::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 24,
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
}
