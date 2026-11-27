//! Sunsoft FME-7 / Sunsoft 5A / Sunsoft 5B (iNES mapper 69).
//!
//! The board is programmed through a command register and a parameter register:
//!
//! ```text
//! $8000-$9FFF  command number (low 4 bits)
//! $A000-$BFFF  parameter; writing it executes the command
//! $C000-$DFFF  (5B only) audio register select: low 4 bits, high nibble disables the data port
//! $E000-$FFFF  (5B only) audio data for the selected register
//! ```
//!
//! | command | register |
//! |---------|----------|
//! | `$0-$7` | CHR bank 0-7 (1 KB windows at PPU `$0000-$1FFF`) |
//! | `$8`    | PRG bank for `$6000-$7FFF`: bit 7 = RAM/ROM select, bit 6 = RAM enable |
//! | `$9-$B` | PRG bank 1-3 (CPU `$8000-$9FFF`, `$A000-$BFFF`, `$C000-$DFFF`) |
//! | `$C`    | mirroring: 0 vertical, 1 horizontal, 2 one-screen `$2000`, 3 one-screen `$2400` |
//! | `$D`    | IRQ control: bit 0 = IRQ enable, bit 7 = counter enable; any write acknowledges |
//! | `$E`    | IRQ counter low byte |
//! | `$F`    | IRQ counter high byte |
//!
//! `$E000-$FFFF` is fixed to the last 8 KB bank. The IRQ is a 16-bit CPU-cycle *down* counter that
//! fires when it rolls from `$0000` to `$FFFF`; the line stays asserted until a write to `$D`.
//! Writes to the counter registers set the counter directly (no reload latch, unlike the MMC3).
//!
//! The 5B's YM2149F audio ([`crate::apu::expansion::ym2149`]) is wired into every mapper-69 board:
//! the FME-7 and 5A simply ignore `$C000`/`$E000` writes, so decoding them unconditionally is
//! harmless, and it is the only way an iNES header (which does not distinguish the chips) can
//! play Gimmick!'s music without a game database.

use crate::apu::expansion::Ym2149;
use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::state::{StateError, StateReader, StateWriter};

pub struct Fme7 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    /// The command written to `$8000-$9FFF`.
    command: u8,
    /// Eight 1 KB CHR banks, from commands `$0-$7`.
    chr_regs: [u8; 8],
    /// The four PRG bank registers (`$8-$B`); index 0 also carries the RAM select/enable bits.
    prg_regs: [u8; 4],
    mirroring: Mirroring,
    /// `$D` control bits (bit 0 IRQ enable, bit 7 counter enable).
    irq_control: u8,
    irq_counter: u16,
    irq_pending: bool,
    audio: Ym2149,
}

impl Fme7 {
    pub fn new(rom: RomData) -> Fme7 {
        Fme7 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            command: 0,
            chr_regs: [0; 8],
            prg_regs: [0; 4],
            mirroring: rom.header.mirroring,
            irq_control: 0,
            irq_counter: 0,
            irq_pending: false,
            audio: Ym2149::new(),
        }
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    /// `$6000-$7FFF`: PRG RAM when the bank register selects and enables it, PRG ROM otherwise.
    fn ram_selected(&self) -> bool {
        self.prg_regs[0] & 0x80 != 0
    }

    fn ram_enabled(&self) -> bool {
        self.prg_regs[0] & 0x40 != 0
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let bank = match addr {
            0x6000..=0x7FFF => usize::from(self.prg_regs[0] & 0x3F),
            0x8000..=0x9FFF => usize::from(self.prg_regs[1] & 0x3F),
            0xA000..=0xBFFF => usize::from(self.prg_regs[2] & 0x3F),
            0xC000..=0xDFFF => usize::from(self.prg_regs[3] & 0x3F),
            _ => n - 1,
        } % n;
        bank * 0x2000 + usize::from(addr & 0x1FFF)
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let bank = usize::from(self.chr_regs[window]) % self.chr_bank_count();
        bank * 0x400 + usize::from(addr & 0x3FF)
    }

    fn execute(&mut self, param: u8) {
        match self.command & 0x0F {
            0x0..=0x7 => self.chr_regs[usize::from(self.command & 0x07)] = param,
            0x8 => self.prg_regs[0] = param,
            0x9 => self.prg_regs[1] = param,
            0xA => self.prg_regs[2] = param,
            0xB => self.prg_regs[3] = param,
            0xC => {
                self.mirroring = match param & 0x03 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenLower,
                    _ => Mirroring::SingleScreenUpper,
                }
            }
            0xD => {
                self.irq_control = param;
                self.irq_pending = false;
            }
            0xE => self.irq_counter = (self.irq_counter & 0xFF00) | u16::from(param),
            0xF => self.irq_counter = (self.irq_counter & 0x00FF) | (u16::from(param) << 8),
            _ => {}
        }
    }
}

impl Mapper for Fme7 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.ram_selected() {
                    if self.ram_enabled() {
                        self.prg_ram[usize::from(addr - 0x6000) % self.prg_ram.len()]
                    } else {
                        0
                    }
                } else {
                    self.prg[self.prg_offset(addr)]
                }
            }
            0x8000..=0xFFFF => self.prg[self.prg_offset(addr)],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.ram_selected() && self.ram_enabled() {
                    let i = usize::from(addr - 0x6000) % self.prg_ram.len();
                    self.prg_ram[i] = val;
                }
            }
            0x8000..=0x9FFF => self.command = val,
            0xA000..=0xBFFF => self.execute(val),
            0xC000..=0xDFFF => self.audio.write_select(val),
            0xE000..=0xFFFF => self.audio.write_data(val),
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
        if self.irq_control & 0x80 != 0 {
            if self.irq_counter == 0 {
                self.irq_counter = 0xFFFF;
                if self.irq_control & 0x01 != 0 {
                    self.irq_pending = true;
                }
            } else {
                self.irq_counter -= 1;
            }
        }
        self.audio.clock();
    }

    fn expansion_audio(&self) -> f32 {
        crate::apu::Apu::amplitude_to_pulse_units(self.audio.output())
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("FME-7")
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.u8(self.command);
        w.bytes(&self.chr_regs);
        w.bytes(&self.prg_regs);
        w.u8(self.mirroring.to_tag());
        w.u8(self.irq_control);
        w.u16(self.irq_counter);
        w.bool(self.irq_pending);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        self.command = r.u8()?;
        r.bytes_into(&mut self.chr_regs)?;
        r.bytes_into(&mut self.prg_regs)?;
        self.mirroring = Mirroring::from_tag(r.u8()?)?;
        self.irq_control = r.u8()?;
        self.irq_counter = r.u16()?;
        self.irq_pending = r.bool()?;
        self.audio.load_state(r)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    fn rom_data() -> RomData {
        RomData {
            header: Header {
                prg_banks: 16, // 256 KB = 32 x 8 KB banks
                chr_banks: 16, // 128 KB
                mapper: 69,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: (0..0x40000).map(|i| (i / 0x2000) as u8).collect(),
            chr: (0..0x20000).map(|i| (i / 0x400) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        }
    }

    fn board() -> Fme7 {
        Fme7::new(rom_data())
    }

    fn command(m: &mut Fme7, cmd: u8, param: u8) {
        m.cpu_write(0x8000, cmd);
        m.cpu_write(0xA000, param);
    }

    #[test]
    fn chr_commands_bank_each_window() {
        let mut m = board();
        for i in 0..8u8 {
            command(&mut m, i, i + 3);
        }
        for i in 0..8u16 {
            assert_eq!(m.ppu_read(i * 0x400), 3 + i as u8);
        }
    }

    #[test]
    fn prg_commands_bank_each_window() {
        let mut m = board();
        command(&mut m, 0x9, 1);
        command(&mut m, 0xA, 2);
        command(&mut m, 0xB, 3);
        assert_eq!(m.cpu_read(0x8000), 1);
        assert_eq!(m.cpu_read(0xA000), 2);
        assert_eq!(m.cpu_read(0xC000), 3);
        assert_eq!(m.cpu_read(0xE000), 31, "the last bank is fixed");
    }

    #[test]
    fn command_8_selects_rom_or_ram_at_6000() {
        let mut m = board();
        command(&mut m, 0x8, 0x05); // ROM, RAM disabled
        assert_eq!(m.cpu_read(0x6000), 5, "PRG ROM bank 5");
        m.cpu_write(0x6000, 0x99);
        assert_eq!(m.cpu_read(0x6000), 5, "writes to a ROM window are dropped");

        command(&mut m, 0x8, 0xC0); // RAM selected + enabled, bank 0
        m.cpu_write(0x6000, 0x99);
        assert_eq!(m.cpu_read(0x6000), 0x99);
        assert_eq!(m.cpu_read(0x7FFF), 0x00);

        command(&mut m, 0x8, 0x80); // RAM selected, disabled: open bus
        assert_eq!(m.cpu_read(0x6000), 0);
    }

    #[test]
    fn mirroring_command() {
        let mut m = board();
        for (param, want) in [
            (0u8, Mirroring::Vertical),
            (1, Mirroring::Horizontal),
            (2, Mirroring::SingleScreenLower),
            (3, Mirroring::SingleScreenUpper),
        ] {
            command(&mut m, 0xC, param);
            assert_eq!(m.mirroring(), want);
        }
    }

    #[test]
    fn irq_counts_down_and_fires_on_the_rollover() {
        let mut m = board();
        command(&mut m, 0xE, 0x02);
        command(&mut m, 0xF, 0x00);
        command(&mut m, 0xD, 0x81); // IRQ enable + counter enable
        m.cpu_clock();
        assert_eq!(m.irq_counter, 1);
        assert!(!m.irq_pending());
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0);
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0xFFFF);
        assert!(m.irq_pending(), "the 0 -> FFFF rollover raises the line");
    }

    #[test]
    fn irq_counter_does_not_decrement_while_disabled() {
        let mut m = board();
        command(&mut m, 0xE, 0x10);
        command(&mut m, 0xD, 0x01); // IRQ enabled, counter disabled
        for _ in 0..100 {
            m.cpu_clock();
        }
        assert_eq!(m.irq_counter, 0x10);
        assert!(!m.irq_pending());
    }

    #[test]
    fn irq_without_enable_still_rolls_the_counter_silently() {
        let mut m = board();
        command(&mut m, 0xE, 0x00);
        command(&mut m, 0xD, 0x80); // counter on, IRQ off
        m.cpu_clock();
        assert_eq!(m.irq_counter, 0xFFFF);
        assert!(!m.irq_pending());
    }

    #[test]
    fn any_control_write_acknowledges() {
        let mut m = board();
        command(&mut m, 0xE, 0x00);
        command(&mut m, 0xD, 0x81);
        m.cpu_clock();
        assert!(m.irq_pending());
        command(&mut m, 0xD, 0x81);
        assert!(!m.irq_pending());
    }

    #[test]
    fn audio_registers_reach_the_5b_chip() {
        let mut m = board();
        m.cpu_write(0xC000, 0x07); // select $07 (tone/noise enables)
        m.cpu_write(0xE000, 0x3F); // all tone and noise disabled -> constant output
        m.cpu_write(0xC000, 0x08);
        m.cpu_write(0xE000, 0x0F); // channel A full volume
        for _ in 0..64 {
            m.cpu_clock();
        }
        assert!(m.expansion_audio() > 0.0, "the 5B plays through the board");
    }

    #[test]
    fn audio_select_high_nibble_closes_the_data_port() {
        let mut m = board();
        m.cpu_write(0xC000, 0x08);
        m.cpu_write(0xE000, 0x0F);
        m.cpu_write(0xC000, 0x18); // disable writes
        m.cpu_write(0xE000, 0x00);
        assert_eq!(m.audio.register(0x08), 0x0F);
    }

    #[test]
    fn state_round_trips() {
        let mut a = board();
        command(&mut a, 0x8, 0xC0);
        a.cpu_write(0x6000, 0x42);
        command(&mut a, 0x9, 7);
        command(&mut a, 0x2, 9);
        command(&mut a, 0xC, 2);
        a.cpu_write(0xC000, 0x08);
        a.cpu_write(0xE000, 0x0F);
        for _ in 0..200 {
            a.cpu_clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = board();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.cpu_read(0x6000), b.cpu_read(0x6000));
        assert_eq!(a.cpu_read(0x8000), b.cpu_read(0x8000));
        assert_eq!(a.ppu_read(0x0800), b.ppu_read(0x0800));
        assert_eq!(a.mirroring(), b.mirroring());
        assert_eq!(a.expansion_audio(), b.expansion_audio());
    }

    fn nvram_board(chr_is_ram: bool) -> Fme7 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Fme7::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 69,
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
