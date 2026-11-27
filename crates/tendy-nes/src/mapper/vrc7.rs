//! Konami VRC7 (iNES mapper 85) with its six-channel OPLL FM audio.
//!
//! ```text
//! $6000-$7FFF  8 KB PRG RAM, enabled by $E000 bit 6
//! $8000-$9FFF  8 KB switchable PRG ROM  ($8000, 6 bits)
//! $A000-$BFFF  8 KB switchable PRG ROM  ($8008 or $8010, 6 bits)
//! $C000-$DFFF  8 KB switchable PRG ROM  ($9000, 6 bits)
//! $E000-$FFFF  8 KB PRG ROM, fixed to the last bank
//! $A000-$D000  eight 1 KB CHR banks: base, then base + $8 (VRC7b) or + $10 (VRC7a)
//! $E000        mirroring (bits 0-1), WRAM enable (bit 6), sound reset (bit 7)
//! $E008/$E010  IRQ latch
//! $F000        IRQ control
//! $F008/$F010  IRQ acknowledge
//! $9010        audio register select      ($9030 is the data port; the two differ in A5)
//! ```
//!
//! Two boards exist: the VRC7b (submapper 1) selects its second register with A3 (`$x008`), the
//! VRC7a (submapper 2) with A4 (`$x010`). Without a submapper the board accepts both, which is
//! unambiguous because the two decodes are mutually exclusive in practice.
//!
//! **Write timing.** The OPLL is a separate chip behind a slow serial path: after selecting a
//! register the program must wait 6 CPU cycles before writing data, and after writing data 42
//! cycles before touching the port again. Writes that arrive too early are dropped, so the board
//! timestamps every audio access against its CPU-cycle counter rather than applying it blindly.
//! The audio core itself lives in [`crate::apu::expansion::opll`].

use crate::apu::expansion::Opll;
use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::mapper::vrc2_4::VrcIrq;
use crate::state::{StateError, StateReader, StateWriter};

/// Cycles the program must leave between a register select and the data write.
pub const AUDIO_SELECT_GUARD: u64 = 6;
/// Cycles the program must leave after a data write before touching the port again.
pub const AUDIO_DATA_GUARD: u64 = 42;

pub struct Vrc7 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    prg_regs: [u8; 3],
    chr_regs: [u8; 8],
    mirroring: Mirroring,
    wram_enable: bool,
    /// `$E000` bit 7: the sound chip is held in reset and its port ignores writes.
    sound_reset: bool,
    audio: Opll,
    pub irq: VrcIrq,
    /// CPU cycles since power-on, the time base for the audio write guards.
    cycles: u64,
    last_select_cycle: u64,
    last_data_cycle: u64,
    /// True once an audio access has happened, so the guards do not fire on the first write.
    audio_touched: bool,
}

impl Vrc7 {
    pub fn new(rom: RomData) -> Vrc7 {
        Vrc7 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            prg_regs: [0; 3],
            chr_regs: [0; 8],
            mirroring: rom.header.mirroring,
            wram_enable: false,
            sound_reset: false,
            audio: Opll::new(),
            irq: VrcIrq::new(),
            cycles: 0,
            last_select_cycle: 0,
            last_data_cycle: 0,
            audio_touched: false,
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
            0x8000..=0x9FFF => usize::from(self.prg_regs[0] & 0x3F),
            0xA000..=0xBFFF => usize::from(self.prg_regs[1] & 0x3F),
            0xC000..=0xDFFF => usize::from(self.prg_regs[2] & 0x3F),
            _ => n - 1,
        } % n;
        bank * 0x2000 + usize::from(addr & 0x1FFF)
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let bank = usize::from(self.chr_regs[window]) % self.chr_bank_count();
        bank * 0x400 + usize::from(addr & 0x3FF)
    }

    /// `$9010`: select an internal OPLL register, unless the previous data write is still settling.
    fn write_audio_select(&mut self, val: u8) {
        if self.sound_reset {
            return;
        }
        if self.audio_touched && self.cycles < self.last_data_cycle + AUDIO_DATA_GUARD {
            return;
        }
        self.audio.write_select(val);
        self.last_select_cycle = self.cycles;
        self.audio_touched = true;
    }

    /// `$9030`: write the selected register, unless the select has not settled yet.
    fn write_audio_data(&mut self, val: u8) {
        if self.sound_reset {
            return;
        }
        if self.audio_touched && self.cycles < self.last_select_cycle + AUDIO_SELECT_GUARD {
            return;
        }
        self.audio.write_data(val);
        self.last_data_cycle = self.cycles;
        self.audio_touched = true;
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        // The second register of each pair lives at +$8 (VRC7b) or +$10 (VRC7a).
        let high = addr & 0x18 != 0;
        match addr & 0xF000 {
            0x8000 => {
                if high {
                    self.prg_regs[1] = val & 0x3F;
                } else {
                    self.prg_regs[0] = val & 0x3F;
                }
            }
            0x9000 => {
                if addr & 0x30 != 0 {
                    // $9010 selects an OPLL register, $9030 writes it (the two differ in A5).
                    if addr & 0x20 != 0 {
                        self.write_audio_data(val);
                    } else {
                        self.write_audio_select(val);
                    }
                } else {
                    self.prg_regs[2] = val & 0x3F;
                }
            }
            0xA000 => self.chr_regs[usize::from(high)] = val,
            0xB000 | 0xC000 | 0xD000 => {
                let group = usize::from(((addr & 0xF000) - 0xB000) >> 12);
                self.chr_regs[(group + 1) * 2 + usize::from(high)] = val;
            }
            0xE000 => {
                if high {
                    self.irq.write_latch(val);
                } else {
                    self.mirroring = match val & 0x03 {
                        0 => Mirroring::Vertical,
                        1 => Mirroring::Horizontal,
                        2 => Mirroring::SingleScreenLower,
                        _ => Mirroring::SingleScreenUpper,
                    };
                    self.wram_enable = val & 0x40 != 0;
                    self.sound_reset = val & 0x80 != 0;
                    if self.sound_reset {
                        self.audio.reset();
                    }
                }
            }
            0xF000 => {
                if high {
                    self.irq.acknowledge();
                } else {
                    self.irq.write_control(val);
                }
            }
            _ => {}
        }
    }
}

impl Mapper for Vrc7 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.wram_enable {
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
                if self.wram_enable {
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
        self.cycles += 1;
        self.irq.clock();
        self.audio.clock();
    }

    fn expansion_audio(&self) -> f32 {
        crate::apu::Apu::amplitude_to_pulse_units(self.audio.output())
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("VRC7")
    }

    fn irq_pending(&self) -> bool {
        self.irq.pending
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.bytes(&self.prg_regs);
        w.bytes(&self.chr_regs);
        w.u8(self.mirroring.to_tag());
        w.bool(self.wram_enable);
        w.bool(self.sound_reset);
        w.u64(self.cycles);
        w.u64(self.last_select_cycle);
        w.u64(self.last_data_cycle);
        w.bool(self.audio_touched);
        self.irq.save_state(w);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        r.bytes_into(&mut self.prg_regs)?;
        r.bytes_into(&mut self.chr_regs)?;
        self.mirroring = Mirroring::from_tag(r.u8()?)?;
        self.wram_enable = r.bool()?;
        self.sound_reset = r.bool()?;
        self.cycles = r.u64()?;
        self.last_select_cycle = r.u64()?;
        self.last_data_cycle = r.u64()?;
        self.audio_touched = r.bool()?;
        self.irq.load_state(r)?;
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
                prg_banks: 16, // 256 KB = 32 x 8 KB
                chr_banks: 16, // 128 KB = 128 x 1 KB
                mapper: 85,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: (0..0x40000).map(|i| (i / 0x2000) as u8).collect(),
            chr: (0..0x20000).map(|i| (i / 0x400) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        }
    }

    fn board() -> Vrc7 {
        Vrc7::new(rom_data())
    }

    /// Let the audio port settle: enough cycles for both guards.
    fn settle(m: &mut Vrc7) {
        for _ in 0..64 {
            m.cpu_clock();
        }
    }

    #[test]
    fn prg_windows() {
        let mut m = board();
        m.cpu_write(0x8000, 3);
        m.cpu_write(0x8010, 4);
        m.cpu_write(0x9000, 5);
        assert_eq!(m.cpu_read(0x8000), 3);
        assert_eq!(m.cpu_read(0xA000), 4);
        assert_eq!(m.cpu_read(0xC000), 5);
        assert_eq!(m.cpu_read(0xE000), 31);
    }

    #[test]
    fn chr_registers_accept_both_board_variants() {
        for offset in [0x08u16, 0x10] {
            let mut m = board();
            m.cpu_write(0xA000, 9); // CHR 0
            m.cpu_write(0xA000 + offset, 10); // CHR 1
            m.cpu_write(0xB000, 11); // CHR 2
            m.cpu_write(0xB000 + offset, 12); // CHR 3
            assert_eq!(m.ppu_read(0x0000), 9);
            assert_eq!(m.ppu_read(0x0400), 10);
            assert_eq!(m.ppu_read(0x0800), 11);
            assert_eq!(m.ppu_read(0x0C00), 12);
        }
    }

    #[test]
    fn mirroring_and_wram_control() {
        let mut m = board();
        for (val, want) in [
            (0u8, Mirroring::Vertical),
            (1, Mirroring::Horizontal),
            (2, Mirroring::SingleScreenLower),
            (3, Mirroring::SingleScreenUpper),
        ] {
            m.cpu_write(0xE000, val);
            assert_eq!(m.mirroring(), want);
        }
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0, "WRAM starts protected");
        m.cpu_write(0xE000, 0x40);
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0x5A);
    }

    #[test]
    fn audio_writes_need_the_settling_delays() {
        let mut m = board();
        m.cpu_write(0x9010, 0x00); // select custom patch register 0
        // Immediately writing data is too early: the select needs 6 cycles.
        m.cpu_write(0x9030, 0x05);
        assert_eq!(m.audio.patch(0)[0], 0x00, "the early write is dropped");
        for _ in 0..AUDIO_SELECT_GUARD {
            m.cpu_clock();
        }
        m.cpu_write(0x9030, 0x05);
        assert_eq!(m.audio.patch(0)[0], 0x05);

        // Now the data port needs 42 cycles before the next access.
        m.cpu_write(0x9010, 0x01);
        for _ in 0..AUDIO_DATA_GUARD {
            m.cpu_clock();
        }
        m.cpu_write(0x9010, 0x01);
        for _ in 0..AUDIO_SELECT_GUARD {
            m.cpu_clock();
        }
        m.cpu_write(0x9030, 0x07);
        assert_eq!(m.audio.patch(0)[1], 0x07);
        assert_eq!(m.audio.patch(0)[0], 0x05, "the earlier byte is untouched");
    }

    #[test]
    fn sound_reset_bit_silences_and_blocks_the_port() {
        let mut m = board();
        m.cpu_write(0x9010, 0x30);
        settle(&mut m);
        m.cpu_write(0x9030, 0x05);
        settle(&mut m);
        m.cpu_write(0x9010, 0x10); // channel 0 fnum low
        settle(&mut m);
        m.cpu_write(0x9030, 0x80);
        settle(&mut m);
        m.cpu_write(0xE000, 0x80); // sound reset
        assert_eq!(m.expansion_audio(), 0.0);
        // While held, the port ignores writes.
        m.cpu_write(0x9010, 0x30);
        settle(&mut m);
        m.cpu_write(0x9030, 0x0F);
        settle(&mut m);
        assert_eq!(
            m.audio.patch(0),
            [0; 8],
            "reset cleared the patch and the port is held"
        );
        // Clearing the bit lets the chip run again.
        m.cpu_write(0xE000, 0x00);
        m.cpu_write(0x9010, 0x00);
        settle(&mut m);
        m.cpu_write(0x9030, 0x05);
        settle(&mut m);
        assert_eq!(m.audio.patch(0)[0], 0x05, "the port works again");
    }

    #[test]
    fn a_keyed_note_plays_through_the_board() {
        let mut m = board();
        // Program a fast custom patch (attack 15) in instrument 0.
        let patch: [(u8, u8); 8] = [
            (0x00, 0x01),
            (0x01, 0x01),
            (0x02, 0x00),
            (0x03, 0x00),
            (0x04, 0xF0),
            (0x05, 0xF0),
            (0x06, 0x00),
            (0x07, 0x00),
        ];
        for (reg, val) in patch {
            m.cpu_write(0x9010, reg);
            settle(&mut m);
            m.cpu_write(0x9030, val);
            settle(&mut m);
        }
        // Instrument 0, octave 4, fnum $180, volume 0, then the trigger bit.
        m.cpu_write(0x9010, 0x30);
        settle(&mut m);
        m.cpu_write(0x9030, 0x00);
        settle(&mut m);
        m.cpu_write(0x9010, 0x10);
        settle(&mut m);
        m.cpu_write(0x9030, 0x80);
        settle(&mut m);
        m.cpu_write(0x9010, 0x20);
        settle(&mut m);
        m.cpu_write(0x9030, 0x10 | (4 << 1) | 1);
        let mut peak: f32 = 0.0;
        for _ in 0..100_000 {
            m.cpu_clock();
            peak = peak.max(m.expansion_audio().abs());
        }
        assert!(peak > 0.04, "the OPLL channel is audible: {peak}");
    }

    #[test]
    fn irq_registers_and_timing() {
        let mut m = board();
        m.cpu_write(0xE008, 0x00); // latch
        m.cpu_write(0xF000, 0x06); // enabled, cycle mode
        for _ in 0..256 {
            m.cpu_clock();
        }
        assert!(m.irq_pending());
        m.cpu_write(0xF010, 0x00); // acknowledge through the VRC7a offset
        assert!(!m.irq_pending());
    }

    #[test]
    fn state_round_trips() {
        let mut a = board();
        a.cpu_write(0x8000, 5);
        a.cpu_write(0xE000, 0x41);
        a.cpu_write(0xA000, 7);
        a.cpu_write(0x9010, 0x30);
        settle(&mut a);
        a.cpu_write(0x9030, 0x03);
        for _ in 0..500 {
            a.cpu_clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = board();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.cpu_read(0x8000), b.cpu_read(0x8000));
        assert_eq!(a.ppu_read(0x0000), b.ppu_read(0x0000));
        assert_eq!(a.mirroring(), b.mirroring());
        assert_eq!(a.expansion_audio(), b.expansion_audio());
        for _ in 0..500 {
            a.cpu_clock();
            b.cpu_clock();
            assert_eq!(a.expansion_audio(), b.expansion_audio());
        }
    }

    fn nvram_board(chr_is_ram: bool) -> Vrc7 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Vrc7::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 85,
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
