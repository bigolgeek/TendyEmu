use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::state::{StateError, StateReader, StateWriter};

/// MMC3 scanline IRQ counter (MMC3-2). Pure: it never touches the bus, it is fed the A12
/// level of every PPU bus access plus the current CPU cycle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mmc3Irq {
    pub latch: u8,
    pub counter: u8,
    pub reload: bool,
    pub enabled: bool,
    pub pending: bool,
    /// Rev A ("old") behaviour: IRQ only when the counter becomes 0 from non-zero or by an explicit reload.
    pub rev_a: bool,
    /// MC-ACC (NES 2.0 submapper 3): the counter clocks on A12's *falling* edge, but only on the
    /// first fall of every eight. Derived from the cartridge, not state.
    pub acc: bool,
    /// MC-ACC pulse divider: falls since the current group began (0..=7). 0 means the next fall is
    /// the group's first and delivers an impulse; a $C001 write restarts the group.
    pub pulse: u8,
    a12: bool,
    low_since: u64,
}

impl Mmc3Irq {
    /// Decode a $C000-$E001 write (`addr & 0xE001`).
    pub fn write(&mut self, addr: u16, val: u8) {
        match addr & 0xE001 {
            0xC000 => self.latch = val,
            0xC001 => {
                self.counter = 0;
                self.reload = true;
                // Mesen's McAcc.h: "Writing to $C001 resets pulse counter." The next fall is the
                // first of a new group and therefore delivers an impulse.
                self.pulse = 0;
            }
            0xE000 => {
                self.enabled = false;
                self.pending = false;
            }
            0xE001 => self.enabled = true,
            _ => {}
        }
    }

    /// One clock impulse: a Sharp A12 rise that passed the filter, or MC-ACC's first fall of eight.
    pub fn clock(&mut self) {
        let old = self.counter;
        let reloaded = self.reload;
        if self.counter == 0 || self.reload {
            self.counter = self.latch;
            self.reload = false;
        } else {
            self.counter -= 1;
        }
        let fire = if self.rev_a {
            self.counter == 0 && (old != 0 || reloaded)
        } else {
            self.counter == 0
        };
        if fire && self.enabled {
            self.pending = true;
        }
    }

    /// Feed the current A12 level at CPU cycle `cycle`. A Sharp MMC3 clocks on a rise held low for at
    /// least 3 cycles; MC-ACC clocks on the fall itself, but only on the first fall of each group of
    /// eight, and its falls are never filtered.
    pub fn observe(&mut self, a12: bool, cycle: u64) {
        let fell = !a12 && self.a12;
        if self.acc {
            if fell {
                // Mesen's McAcc.h NotifyVramAddressChange: `_counter++`, clock when it reads 1, wrap
                // back to 0 when it reads 8 — so the impulses land on falls 1, 9, 17, ... of a group.
                self.pulse = self.pulse % 8 + 1;
                if self.pulse == 1 {
                    self.clock();
                } else if self.pulse == 8 {
                    self.pulse = 0;
                }
            }
        } else if a12 && !self.a12 && cycle.wrapping_sub(self.low_since) >= 3 {
            self.clock();
        }
        // The line has been low since its falling edge, however sparsely it was sampled afterwards.
        if fell {
            self.low_since = cycle;
        }
        self.a12 = a12;
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.latch);
        w.u8(self.counter);
        w.bool(self.reload);
        w.bool(self.enabled);
        w.bool(self.pending);
        w.bool(self.rev_a);
        w.u8(self.pulse);
        w.bool(self.a12);
        w.u64(self.low_since);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.latch = r.u8()?;
        self.counter = r.u8()?;
        self.reload = r.bool()?;
        self.enabled = r.bool()?;
        self.pending = r.bool()?;
        self.rev_a = r.bool()?;
        self.pulse = r.u8()?;
        self.a12 = r.bool()?;
        self.low_since = r.u64()?;
        Ok(())
    }
}

/// Mapper 4 (MMC3): eight 1 KB CHR banks and four 8 KB PRG banks, split by `bank_select` bit 6 and
/// 7, with the second-to-last and last PRG banks optionally fixed. The scanline IRQ counter lives in
/// [`Mmc3Irq`]; this step (MMC3-2) only wires its inputs, MMC3-3 tunes the timing against ROMs.
pub struct Mmc3 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    bank_select: u8,
    regs: [u8; 8],
    mirroring: Mirroring,
    four_screen: bool,
    /// MMC3C with the mirroring register hard-wired (NES 2.0 submapper 2): $A000 bit 0 is ignored.
    hardwired_mirroring: bool,
    pub irq: Mmc3Irq,
    cycle: u64,
    /// MMC6 (NES 2.0 submapper 1): 1 KB PRG RAM at $7000-$7FFF with per-half enables.
    mmc6: bool,
    /// MMC6 $A001: bit 7/6 = write/read $7200 half, bit 5/4 = write/read $7000 half.
    ram_protect: u8,
    /// Byte offsets of the four 8 KB CPU windows, from the current bank registers. Rebuilt
    /// by [`Mmc3::refresh_maps`] on every register write and state load, never saved.
    prg_map: [usize; 4],
    /// Byte offsets of the eight 1 KB PPU windows, likewise.
    chr_map: [usize; 8],
}

impl Mmc3 {
    /// Power-on: `bank_select` = 0 (PRG mode 0, CHR 2 KB pairs at $0000), every register 0.
    pub fn new(rom: RomData) -> Mmc3 {
        let four_screen = rom.header.mirroring == Mirroring::FourScreen;
        // MMC3 rev A ("old"/Sharp IRQ behaviour) is selected by NES 2.0 submapper 4.
        // MMC6 (submapper 1) shares the rev A IRQ rule: the 6-MMC6 ROM's source says it has the
        // "old" behaviour, so a submapper-1 cart must build with the rev A scanline counter
        // (blargg mmc3_test 6-MMC6 fails check 3 with the rev B rule).
        let submapper = if rom.header.nes2 {
            rom.header.submapper
        } else {
            0
        };
        let rev_a = submapper == 4 || submapper == 1;
        let mmc6 = submapper == 1;
        // MMC3C with hard-wired mirroring (submapper 2) and MC-ACC (submapper 3, IRQ on A12 fall).
        let hardwired_mirroring = submapper == 2;
        let acc = submapper == 3;
        let ram_size = if mmc6 {
            1024
        } else {
            rom.header.prg_ram_size()
        };
        let mut m = Mmc3 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; ram_size],
            bank_select: 0,
            regs: [0; 8],
            mirroring: rom.header.mirroring,
            four_screen,
            hardwired_mirroring,
            irq: Mmc3Irq {
                rev_a,
                acc,
                ..Mmc3Irq::default()
            },
            cycle: 0,
            mmc6,
            ram_protect: 0,
            prg_map: [0; 4],
            chr_map: [0; 8],
        };
        m.refresh_maps();
        m
    }

    /// Rebuild the two window tables after anything that moves a bank: the $8000/$8001
    /// registers, a state load or the randomised power-on. The offsets are the same numbers
    /// [`Mmc3::prg_offset`] and [`Mmc3::chr_offset`] compute per read, only computed now.
    fn refresh_maps(&mut self) {
        let mut prg_map = [0usize; 4];
        for (i, slot) in prg_map.iter_mut().enumerate() {
            *slot = self.prg_offset(0x8000 + i as u16 * 0x2000);
        }
        let mut chr_map = [0usize; 8];
        for (i, slot) in chr_map.iter_mut().enumerate() {
            *slot = self.chr_offset(i as u16 * 0x400);
        }
        self.prg_map = prg_map;
        self.chr_map = chr_map;
    }

    /// MMC6 RAM index for a CPU address, or None when that 512-byte half is not accessible.
    /// `write` selects the write-enable bit instead of the read-enable bit.
    fn mmc6_ram_index(&self, addr: u16, write: bool) -> Option<usize> {
        if self.bank_select & 0x20 == 0 || !(0x7000..=0x7FFF).contains(&addr) {
            return None;
        }
        let upper = addr & 0x200 != 0;
        let bit = match (upper, write) {
            (false, false) => 0x10,
            (false, true) => 0x20,
            (true, false) => 0x40,
            (true, true) => 0x80,
        };
        // A write-enabled half is only writable while the same half is also readable.
        let read_bit = if upper { 0x40 } else { 0x10 };
        if self.ram_protect & bit == 0 || (write && self.ram_protect & read_bit == 0) {
            return None;
        }
        Some(usize::from(addr & 0x3FF))
    }

    /// Feed the A12 level of a PPU bus address to the scanline counter.
    fn observe_a12(&mut self, addr: u16) {
        self.irq.observe(addr & 0x1000 != 0, self.cycle);
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    /// Byte offset into PRG-ROM for a CPU address in 0x8000..=0xFFFF, in 8 KB banks.
    fn prg_offset(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let fixed = n.saturating_sub(1);
        let second_last = n.saturating_sub(2);
        let mode = self.bank_select & 0x40 != 0;
        let bank = match (addr - 0x8000) / 0x2000 {
            0 if mode => second_last,
            0 => usize::from(self.regs[6]),
            1 => usize::from(self.regs[7]),
            2 if mode => usize::from(self.regs[6]),
            2 => second_last,
            _ => fixed,
        };
        (bank % n) * 0x2000 + usize::from(addr & 0x1FFF)
    }

    /// Byte offset into CHR for a PPU address in 0x0000..=0x1FFF, in 1 KB banks. `bank_select`
    /// bit 7 swaps the two 4 KB halves before the register split is applied.
    fn chr_offset(&self, addr: u16) -> usize {
        let off = addr & 0x1FFF;
        let a = if self.bank_select & 0x80 != 0 {
            off ^ 0x1000
        } else {
            off
        };
        let n = self.chr_bank_count();
        let (bank, within) = if a < 0x0800 {
            (
                usize::from(self.regs[0] & 0xFE) + usize::from((a >> 10) & 1),
                usize::from(a & 0x3FF),
            )
        } else if a < 0x1000 {
            (
                usize::from(self.regs[1] & 0xFE) + usize::from((a >> 10) & 1),
                usize::from(a & 0x3FF),
            )
        } else {
            let slot = usize::from((a - 0x1000) >> 10);
            (usize::from(self.regs[2 + slot]), usize::from(a & 0x3FF))
        };
        (bank % n) * 0x400 + within
    }
}

impl Mapper for Mmc3 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF if self.mmc6 => self
                .mmc6_ram_index(addr, false)
                .map_or(0, |i| self.prg_ram[i]),
            0x6000..=0x7FFF => self
                .prg_ram
                .get(usize::from(addr - 0x6000))
                .copied()
                .unwrap_or(0),
            0x8000..=0xFFFF => {
                let slot = usize::from((addr >> 13) & 3);
                self.prg[self.prg_map[slot] + usize::from(addr & 0x1FFF)]
            }
            _ => 0,
        }
    }

    /// CPU address to the PRG-ROM byte it reads right now, from the current bank registers;
    /// `None` outside $8000-$FFFF.
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| self.prg_offset(addr))
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF if self.mmc6 => {
                if let Some(i) = self.mmc6_ram_index(addr, true) {
                    self.prg_ram[i] = val;
                }
            }
            0x6000..=0x7FFF => {
                if let Some(b) = self.prg_ram.get_mut(usize::from(addr - 0x6000)) {
                    *b = val;
                }
            }
            0x8000..=0xFFFF => match addr & 0xE001 {
                0x8000 => {
                    self.bank_select = val;
                    if self.mmc6 && val & 0x20 == 0 {
                        self.ram_protect = 0;
                    }
                    self.refresh_maps();
                }
                0x8001 => {
                    self.regs[usize::from(self.bank_select & 7)] = val;
                    self.refresh_maps();
                }
                0xA000 => {
                    if !self.four_screen && !self.hardwired_mirroring {
                        self.mirroring = if val & 1 == 0 {
                            Mirroring::Vertical
                        } else {
                            Mirroring::Horizontal
                        };
                    }
                }
                // MMC3 ignores $A001 (RAM always enabled, as most emulators do); MMC6 uses it
                // for the per-half enables, writable only while $8000 bit 5 is set.
                0xA001 => {
                    if self.mmc6 && self.bank_select & 0x20 != 0 {
                        self.ram_protect = val & 0xF0;
                    }
                }
                0xC000 | 0xC001 | 0xE000 | 0xE001 => self.irq.write(addr, val),
                _ => {}
            },
            _ => {}
        }
    }

    /// The live 1 KiB CHR mapping without `observe_a12`: a viewer read must not clock the
    /// scanline counter or move the A12 filter's state.
    fn chr_peek(&self, addr: u16) -> u8 {
        let slot = usize::from((addr >> 10) & 7);
        chr_read(&self.chr, self.chr_map[slot] + usize::from(addr & 0x3FF))
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        let slot = usize::from((addr >> 10) & 7);
        crate::cart::chr_rom_index(
            &self.chr,
            self.chr_is_ram,
            self.chr_map[slot] + usize::from(addr & 0x3FF),
        )
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.observe_a12(addr);
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.observe_a12(addr);
        if self.chr_is_ram {
            let slot = usize::from((addr >> 10) & 7);
            let i = self.chr_map[slot] + usize::from(addr & 0x3FF);
            chr_write(&mut self.chr, i, val);
        }
    }

    /// $2006/$2007 moved `v` without an access: A12 can still change, so the counter sees it too.
    fn ppu_bus(&mut self, addr: u16) {
        self.observe_a12(addr);
    }

    fn mirroring(&self) -> Mirroring {
        if self.four_screen {
            Mirroring::FourScreen
        } else {
            self.mirroring
        }
    }

    /// Randomised power-on: the $8000 bank-select register and all eight bank registers
    /// take seed-derived values from the same splitmix64 stream the bus uses for randomised RAM
    /// (golden-ratio increment + avalanche mix, one byte per register), each folded into its own
    /// bank range. IRQ state and mirroring keep their normal power-on defaults.
    fn power_on_ram(&mut self, mode: crate::bus::RamPowerOn, seed: u64) {
        let chr: &mut [u8] = if self.chr_is_ram {
            &mut self.chr
        } else {
            &mut []
        };
        crate::cart::fill_power_on_ram(&mut self.prg_ram, chr, mode, seed);
    }

    fn power_on_random(&mut self, seed: u64) {
        let prg_n = self.prg_bank_count();
        let chr_n = self.chr_bank_count();
        // Registers 0 and 1 load 2 KB CHR pairs (& 0xFE + half index): index them in pairs.
        let chr_pairs = (chr_n / 2).max(1);
        // the shared generator, from the raw seed (not `rng::derive`, whose domain tag would
        // move the stream). One `next_u8` per register is the same step the hand-rolled copy took.
        let mut rng = crate::rng::SplitMix64::new(seed);
        let mut v: [u8; 9] = [0; 9];
        for b in v.iter_mut() {
            *b = rng.next_u8();
        }
        self.bank_select = v[0];
        self.regs[0] = (2 * (usize::from(v[1]) % chr_pairs)) as u8;
        self.regs[1] = (2 * (usize::from(v[2]) % chr_pairs)) as u8;
        self.regs[2] = (usize::from(v[3]) % chr_n) as u8;
        self.regs[3] = (usize::from(v[4]) % chr_n) as u8;
        self.regs[4] = (usize::from(v[5]) % chr_n) as u8;
        self.regs[5] = (usize::from(v[6]) % chr_n) as u8;
        self.regs[6] = (usize::from(v[7]) % prg_n) as u8;
        self.regs[7] = (usize::from(v[8]) % prg_n) as u8;
        self.refresh_maps();
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.bank_select);
        w.bytes(&self.regs);
        w.u8(self.mirroring.to_tag_horizontal_first());
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
        w.u64(self.cycle);
        self.irq.save_state(w);
        w.u8(self.ram_protect);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.bank_select = r.u8()?;
        r.bytes_into(&mut self.regs)?;
        self.mirroring = Mirroring::from_tag_horizontal_first(r.u8()?)?;
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        self.cycle = r.u64()?;
        self.irq.load_state(r)?;
        self.ram_protect = r.u8()?;
        self.refresh_maps();
        Ok(())
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

    /// Counts CPU cycles for the A12 filter and the scanline counter's time base.
    fn hooks(&self) -> CartHooks {
        CartHooks {
            irq: true,
            cpu_clock: true,
        }
    }

    fn cpu_clock(&mut self) {
        self.cycle += 1;
    }

    fn irq_pending(&self) -> bool {
        self.irq.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_irq(latch: u8, rev_a: bool) -> Mmc3Irq {
        let mut irq = Mmc3Irq {
            rev_a,
            ..Mmc3Irq::default()
        };
        irq.write(0xC000, latch);
        irq.write(0xC001, 0);
        irq.write(0xE001, 0);
        irq
    }

    #[test]
    fn counter_reloads_then_counts_down_to_irq() {
        let mut irq = enabled_irq(2, false);
        irq.clock();
        assert_eq!(irq.counter, 2, "C001 reload happens on the first clock");
        assert!(!irq.pending);
        irq.clock();
        assert_eq!(irq.counter, 1);
        assert!(!irq.pending);
        irq.clock();
        assert_eq!(irq.counter, 0);
        assert!(irq.pending);
    }

    #[test]
    fn e000_disables_and_clears_pending() {
        let mut irq = enabled_irq(0, false);
        irq.clock();
        assert!(irq.pending);
        irq.write(0xE000, 0);
        assert!(!irq.pending);
        assert!(!irq.enabled);
        irq.clock();
        assert!(!irq.pending, "a disabled counter never re-raises");
        irq.write(0xE001, 0);
        assert!(irq.enabled);
        assert!(!irq.pending, "E001 only enables, it does not re-fire");
    }

    #[test]
    fn latch_zero_rev_b_fires_on_every_clock() {
        let mut irq = enabled_irq(0, false);
        for _ in 0..3 {
            irq.pending = false;
            irq.clock();
            assert_eq!(irq.counter, 0);
            assert!(irq.pending);
        }
    }

    #[test]
    fn latch_zero_rev_a_fires_once_per_reload() {
        let mut irq = enabled_irq(0, true);
        irq.clock();
        assert!(irq.pending, "the explicit reload fires");

        irq.pending = false;
        irq.clock();
        assert_eq!(irq.counter, 0);
        assert!(!irq.pending, "no reload and old == 0: rev A stays quiet");
    }

    #[test]
    fn a12_filter_requires_three_cycles_low() {
        let mut irq = enabled_irq(2, false);
        // The first clock latches the reload, the second counts down, so the counter sits at 1.
        irq.clock();
        irq.clock();
        assert_eq!(irq.counter, 1);
        assert!(!irq.pending);

        irq.observe(true, 1); // already high: establishes the level without clocking
        irq.observe(false, 10);
        irq.observe(true, 12); // only 2 cycles low: filtered out
        assert_eq!(irq.counter, 1);
        assert!(!irq.pending);

        irq.observe(false, 20);
        irq.observe(true, 23); // 3 cycles low: one clock
        assert_eq!(irq.counter, 0);
        assert!(irq.pending);
    }

    #[test]
    fn a12_high_to_high_is_not_an_edge() {
        let mut irq = enabled_irq(3, false);
        irq.observe(true, 100); // first rise after reset: one clock, which latches 3
        assert_eq!(irq.counter, 3);
        irq.observe(true, 200);
        assert_eq!(irq.counter, 3, "no rise, no clock");
    }

    /// `ppu_read`/`ppu_write`/`ppu_bus` feed A12, and `irq_pending` surfaces the flag.
    #[test]
    fn ppu_bus_feeds_the_counter() {
        let mut rom = vec![
            0x4E, 0x45, 0x53, 0x1A, 2, 1, 0x41, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        rom.resize(16 + 0x8000 + 0x2000, 0);
        let mut m = crate::mapper::load(&rom).unwrap();
        m.cpu_write(0xC000, 0); // latch 0: every accepted edge fires
        m.cpu_write(0xE001, 0);

        m.ppu_read(0x0000); // A12 low at cycle 0
        assert!(!m.irq_pending());
        for _ in 0..3 {
            m.cpu_clock();
        }
        m.ppu_read(0x1000); // rise after 3 cycles low
        assert!(m.irq_pending());

        // A $2006/$2007 bus change with no access clocks too, and E000 clears the flag.
        m.cpu_write(0xE000, 0);
        assert!(!m.irq_pending());
        m.cpu_write(0xE001, 0); // E000 also disables, so re-enable
        m.ppu_bus(0x0000);
        for _ in 0..3 {
            m.cpu_clock();
        }
        m.ppu_bus(0x1400);
        assert!(m.irq_pending());
    }

    #[test]
    fn state_round_trip() {
        let a = Mmc3Irq {
            latch: 5,
            counter: 3,
            reload: true,
            enabled: true,
            pending: true,
            rev_a: true,
            acc: false,
            pulse: 7,
            a12: true,
            low_since: 0x1234,
        };
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();

        let mut b = Mmc3Irq::default();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a, b);
    }

    /// Bytes for a banked image where the same offset in two different banks never holds the same
    /// value: the bank index is in the high nibble, the offset's bank/16-th group in the low one.
    /// The old fixtures were all zeros, so replacing one bank with another read 0 either way and
    /// the differential test could not fail.
    fn distinct_banks(len: usize, bank: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (((i / bank) as u8) << 4) | ((i / (bank / 16)) as u8 & 0x0F))
            .collect()
    }

    /// MMC3 cart: 64 KB PRG (eight 8 KB banks) + 8 KB CHR ROM (eight 1 KB banks), every bank
    /// filled with [`distinct_banks`] so a window table pointing at the wrong bank cannot alias.
    fn mmc3_rom() -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 4, 1, 0x41, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v.resize(16 + 4 * 16384 + 8192, 0);
        v[16..16 + 0x1_0000].copy_from_slice(&distinct_banks(0x1_0000, 0x2000));
        v[16 + 0x1_0000..].copy_from_slice(&distinct_banks(0x2000, 0x400));
        v
    }

    /// The same board with a NES 2.0 header selecting submapper 1 (MMC6).
    fn mmc6_rom() -> Vec<u8> {
        let mut v = mmc3_rom();
        v[7] = (v[7] & 0xF0) | 0x08; // NES 2.0
        v[8] = 0x10; // submapper 1
        v
    }

    #[test]
    fn mmc6_builds_with_the_rev_a_irq_rule() {
        let m = Mmc3::new(crate::cart::split(&mmc6_rom()).unwrap());
        assert!(m.mmc6, "submapper 1 selects the MMC6 board");
        assert!(m.irq.rev_a, "MMC6 has the old (rev A) IRQ rule");
    }

    /// The same board with a NES 2.0 header selecting `submapper`.
    fn nes2_mmc3_rom(submapper: u8) -> Vec<u8> {
        let mut v = mmc3_rom();
        v[7] = (v[7] & 0xF0) | 0x08; // NES 2.0
        v[8] = submapper << 4;
        v
    }

    /// Submapper 2 (MMC3C with hard-wired mirroring): $A000 bit 0 has no effect, the header rules.
    #[test]
    fn submapper2_ignores_the_mirroring_register() {
        let mut m = Mmc3::new(crate::cart::split(&nes2_mmc3_rom(2)).unwrap());
        assert_eq!(
            m.mirroring(),
            Mirroring::Vertical,
            "the header's vertical mirroring"
        );
        m.cpu_write(0xA000, 1);
        assert_eq!(
            m.mirroring(),
            Mirroring::Vertical,
            "a hard-wired board ignores $A000 bit 0"
        );
    }

    /// Submapper 0 keeps the register: the same write switches the nametables.
    #[test]
    fn submapper0_mirroring_still_follows_the_register() {
        let mut m = Mmc3::new(crate::cart::split(&nes2_mmc3_rom(0)).unwrap());
        m.cpu_write(0xA000, 1);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    /// One A12 fall for MC-ACC: high, then low one cycle later.
    fn acc_fall(irq: &mut Mmc3Irq, cycle: u64) {
        irq.observe(true, cycle);
        irq.observe(false, cycle + 1);
    }

    /// A MC-ACC counter with `latch` loaded, re-armed by $C001 and enabled, ready for impulses.
    fn acc_irq(latch: u8) -> Mmc3Irq {
        let mut irq = Mmc3Irq {
            acc: true,
            ..Mmc3Irq::default()
        };
        irq.write(0xC000, latch);
        irq.write(0xC001, 0);
        irq.write(0xE001, 0);
        irq
    }

    /// Submapper 3 (MC-ACC): eight consecutive A12 falls deliver exactly one clock impulse
    /// (Mesen's McAcc.h: `_counter++`, clock at 1, wrap to 0 at 8). Every fall is still *seen*, but
    /// only the group's first one clocks, because the cartridge divides A12 down by 8.
    #[test]
    fn submapper3_clocks_once_per_eight_a12_falls() {
        let mut irq = acc_irq(5);
        for i in 0..8 {
            acc_fall(&mut irq, 10 * i);
        }
        assert_eq!(
            irq.counter, 5,
            "one impulse: $C001's reload loaded the latch, the other 7 falls were swallowed"
        );
        assert_eq!(
            irq.pulse, 0,
            "fall 8 wrapped the divider back to the start of the next group"
        );
        assert!(!irq.pending);
    }

    /// Nine falls reach a second group: the impulses land on fall 1 and fall 9, so the counter
    /// clocks twice (5, then 4) and the other seven falls still do nothing.
    #[test]
    fn submapper3_ninth_fall_starts_the_next_group() {
        let mut irq = acc_irq(5);
        for i in 0..8 {
            acc_fall(&mut irq, 10 * i);
        }
        assert_eq!(irq.counter, 5, "fall 1 loaded the latch");
        acc_fall(&mut irq, 80);
        assert_eq!(
            irq.counter, 4,
            "fall 9 opened the second group and clocked again"
        );
        assert_eq!(
            irq.pulse, 1,
            "the divider restarted at the group's first fall"
        );
        assert!(!irq.pending);
    }

    /// A $C001 write between falls restarts the divider, so the group re-phases onto the *next*
    /// fall instead of waiting for the eighth. With falls 1..4, $C001, falls 5..11:
    ///   fall 1 => pulse 1 => impulse (counter = latch 5)
    ///   falls 2..4 => pulse 2..4
    ///   $C001 => pulse 0, so fall 5 is a group's first fall => impulse (counter reloads to 5)
    ///   falls 6..11 => pulse 2..7
    /// The second impulse is on **fall 5**, not fall 12: the wrap is measured from the reset, so the
    /// next impulse after it would be at fall 5 + 8 = 13. Eleven falls therefore clock twice, while
    /// the same eleven falls without the $C001 would have clocked at 1 and 9.
    #[test]
    fn submapper3_c001_restarts_the_divider() {
        let mut irq = acc_irq(5);
        for i in 0..4 {
            acc_fall(&mut irq, 10 * i);
        }
        assert_eq!(irq.counter, 5, "fall 1 clocked");
        assert_eq!(irq.pulse, 4, "falls 1..4 advanced the divider to 4");

        irq.write(0xC001, 0); // reload = true, pulse = 0
        assert_eq!(irq.pulse, 0, "$C001 resets the pulse counter");

        for i in 4..11 {
            acc_fall(&mut irq, 10 * i);
        }
        assert_eq!(
            irq.counter, 5,
            "fall 5 clocked (its $C001 reload reloaded the latch), fall 13 is next"
        );
        assert_eq!(irq.pulse, 7, "falls 5..11 filled the new group to 7");
        assert!(!irq.pending);
    }

    /// The Sharp rule is unchanged: the rise clocks, the fall only remembers when the line went low.
    #[test]
    fn submapper0_clocks_the_counter_on_the_a12_rise() {
        let mut irq = Mmc3Irq::default();
        irq.write(0xC000, 1);
        irq.write(0xC001, 0);
        irq.write(0xE001, 0);

        irq.observe(false, 0);
        irq.observe(true, 3);
        assert_eq!(irq.counter, 1, "the rise after 3 low cycles clocks");
        irq.observe(false, 10);
        irq.observe(true, 13);
        assert!(irq.pending);
    }

    #[test]
    fn power_on_random_is_deterministic() {
        let seed = 0x0123_4567_89AB_CDEF;
        let mut a = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        a.power_on_random(seed);
        let snapshot = (a.bank_select, a.regs);
        a.power_on_random(seed);
        assert_eq!(
            (a.bank_select, a.regs),
            snapshot,
            "same seed twice, same registers"
        );
        let mut b = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        b.power_on_random(seed);
        assert_eq!((b.bank_select, b.regs), snapshot, "fresh boards agree too");
        // Seed-derived, but folded into each bank's own range: 8 CHR 1 KB banks (pairs for
        // regs 0/1) and 8 PRG 8 KB banks.
        assert!(a.regs[..2].iter().all(|r| usize::from(r & 0xFE) < 8));
        assert!(a.regs[2..6].iter().all(|r| usize::from(*r) < 8));
        assert!(a.regs[6..].iter().all(|r| usize::from(*r) < 8));
    }

    /// The shared generator draws the same values the board's private copy did. The expected
    /// registers are what the pre-fix hand-rolled loop produced for these seeds on `mmc3_rom()`
    /// (64 KB PRG, 8 KB CHR), so a move in the stream fails here rather than in a game.
    #[test]
    fn power_on_random_stream_is_the_old_hand_rolled_stream() {
        const CASES: [(u64, u8, [u8; 8]); 3] = [
            (0x0123_4567_89AB_CDEF, 0x9D, [6, 4, 4, 0, 2, 5, 1, 3]),
            (0, 0xAF, [0, 6, 4, 3, 2, 1, 4, 3]),
            (0xDEAD_BEEF_CAFE_F00D, 0xCB, [6, 0, 2, 4, 4, 3, 6, 5]),
        ];
        for (seed, bank_select, regs) in CASES {
            let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
            m.power_on_random(seed);
            assert_eq!(m.bank_select, bank_select, "seed {seed:#X}");
            assert_eq!(m.regs, regs, "seed {seed:#X}");
        }
    }

    #[test]
    fn power_on_random_never_panics() {
        for seed in [0u64, u64::MAX, 0xDEAD_BEEF_CAFE_F00D, 777] {
            let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
            m.power_on_random(seed);
            for addr in [0x6000u16, 0x7FFF, 0x8000, 0xA000, 0xBFFF, 0xC000, 0xFFFF] {
                m.cpu_read(addr);
            }
            for addr in [0x0000u16, 0x0FFF, 0x1000, 0x1FFF] {
                m.ppu_read(addr);
            }
            m.mirroring();
            m.cpu_clock();
            assert!(!m.irq_pending(), "bank randomisation never raises an IRQ");
        }
    }

    /// The `mmc3_rom()` board with CHR-RAM instead of CHR-ROM (no CHR banks in the header).
    fn mmc3_rom_chr_ram() -> Vec<u8> {
        let mut v = mmc3_rom();
        v[5] = 0;
        v.truncate(16 + 4 * 16384);
        v
    }

    fn mmc3_rom_with_chr_pattern() -> Vec<u8> {
        let mut v = mmc3_rom();
        for (i, b) in v[16 + 4 * 16384..].iter_mut().enumerate() {
            *b = (i % 7) as u8 + 1;
        }
        v
    }

    #[test]
    fn power_on_ram_random_fills_prg_and_chr_ram_deterministically() {
        use crate::bus::RamPowerOn;
        let seed = 0x0123_4567_89AB_CDEF;
        let make = |mode, seed| {
            let mut m = Mmc3::new(crate::cart::split(&mmc3_rom_chr_ram()).unwrap());
            m.power_on_ram(mode, seed);
            m
        };
        let a = make(RamPowerOn::Random, seed);
        let b = make(RamPowerOn::Random, seed);
        let c = make(RamPowerOn::Random, seed + 1);
        assert_eq!(a.prg_ram(), b.prg_ram());
        assert_eq!(a.chr, b.chr);
        assert_ne!(a.prg_ram(), c.prg_ram());
        assert!(
            a.prg_ram().iter().any(|&x| x != 0),
            "MMC3: PRG RAM was filled"
        );
        assert!(a.chr.iter().any(|&x| x != 0), "MMC3: CHR-RAM was filled");
        // One stream: PRG RAM first, CHR-RAM straight after it.
        let mut rng = crate::rng::derive(seed, crate::rng::SeedDomain::CartRam);
        let mut prg = vec![0u8; a.prg_ram().len()];
        let mut chr = vec![0u8; a.chr.len()];
        rng.fill_bytes(&mut prg);
        rng.fill_bytes(&mut chr);
        assert_eq!(a.prg_ram(), &prg[..]);
        assert_eq!(&a.chr[..], &chr[..]);
        let ones = make(RamPowerOn::Ones, 0);
        assert!(ones.prg_ram().iter().all(|&x| x == 0xFF));
        assert!(ones.chr.iter().all(|&x| x == 0xFF));
    }

    #[test]
    fn power_on_ram_zero_leaves_ram_zeroed() {
        use crate::bus::RamPowerOn;
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom_chr_ram()).unwrap());
        m.power_on_ram(RamPowerOn::Zero, 0x1234);
        assert!(m.prg_ram().iter().all(|&x| x == 0));
        assert!(m.chr.iter().all(|&x| x == 0));
    }

    #[test]
    fn power_on_ram_never_touches_chr_rom() {
        use crate::bus::RamPowerOn;
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            let mut m = Mmc3::new(crate::cart::split(&mmc3_rom_with_chr_pattern()).unwrap());
            let before: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            m.power_on_ram(mode, 99);
            let after: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            assert_eq!(before, after, "{mode:?}");
            assert!(!m.chr.is_empty(), "CHR-ROM still present");
        }
    }

    /// MMC6 has 1 KB of PRG RAM: the fill must size itself from the board, not assume 8 KB.
    #[test]
    fn power_on_ram_fills_exactly_the_mmc6_ram() {
        let mut m = Mmc3::new(crate::cart::split(&mmc6_rom()).unwrap());
        m.power_on_ram(crate::bus::RamPowerOn::Random, 5);
        assert_eq!(m.prg_ram().len(), 1024);
        assert!(m.prg_ram().iter().any(|&x| x != 0));
    }

    /// The invariant the two window tables exist for: each slot holds the offset the per-read
    /// `prg_offset`/`chr_offset` computes for that window's base address.
    fn assert_maps_match(m: &Mmc3, ctx: &str) {
        for (slot, window) in m.prg_map.iter().enumerate() {
            let base = 0x8000 + slot as u16 * 0x2000;
            assert_eq!(
                *window,
                m.prg_offset(base),
                "PRG window {slot} (${base:04X}) {ctx}"
            );
        }
        for (slot, window) in m.chr_map.iter().enumerate() {
            let base = slot as u16 * 0x400;
            assert_eq!(
                *window,
                m.chr_offset(base),
                "CHR window {slot} (${base:04X}) {ctx}"
            );
        }
    }

    /// The end-to-end half: every read through the public `Mapper` methods returns the byte the
    /// per-read offset names. This is the differential guarantee the tables were introduced under,
    /// and on the distinct fixture a stale window cannot alias onto the right byte.
    fn assert_reads_match(m: &mut Mmc3, ctx: &str) {
        for addr in 0x8000..=0xFFFFu16 {
            assert_eq!(
                m.cpu_read(addr),
                m.prg[m.prg_offset(addr)],
                "PRG read ${addr:04X} {ctx}"
            );
        }
        for addr in 0..=0x1FFFu16 {
            assert_eq!(
                m.chr_peek(addr),
                chr_read(&m.chr, m.chr_offset(addr)),
                "CHR read ${addr:04X} {ctx}"
            );
        }
    }

    /// `Mmc3::new` builds the tables for the power-on registers: PRG mode 0 leaves banks
    /// `n-2`/`n-1` at $C000/$E000, and the CHR pairs start at 1 KB banks 0..7.
    #[test]
    fn new_builds_the_bank_tables() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        assert_ne!(
            m.prg[0],
            m.prg[6 * 0x2000],
            "fixture: the same offset in two PRG banks differs"
        );
        assert_maps_match(&m, "at power-on");
        assert_reads_match(&mut m, "at power-on");
    }

    /// `$8000` alone must rebuild both tables. Bit 6 swaps the PRG window layout and bit 7
    /// swaps the CHR halves, neither of which needs a following `$8001` (QA: deleting this refresh
    /// left every test green because the old test always wrote `$8000` and `$8001` in pairs).
    #[test]
    fn bank_select_alone_rebuilds_the_tables() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        m.cpu_write(0x8000, 6);
        m.cpu_write(0x8001, 5); // regs[6] = 5: the $8000 window no longer matches the fixed pair
        m.cpu_write(0x8000, 2);
        m.cpu_write(0x8001, 5); // regs[2] = 5: 1 KB CHR windows 4..7
        assert_maps_match(&m, "before the bare $8000 writes");

        // PRG mode 1: $8000 takes the fixed second-last bank, $C000 takes regs[6].
        m.cpu_write(0x8000, 0x40 | 6);
        assert_maps_match(&m, "after $8000 = $46 with no $8001");
        assert_reads_match(&mut m, "after $8000 = $46 with no $8001");

        // CHR half swap, again from the bank-select write alone.
        m.cpu_write(0x8000, 0x80);
        assert_maps_match(&m, "after $8000 = $80 with no $8001");
        assert_reads_match(&mut m, "after $8000 = $80 with no $8001");
    }

    /// `$8001` alone must rebuild both tables.
    #[test]
    fn register_write_alone_rebuilds_the_tables() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        m.cpu_write(0x8000, 6); // select the PRG bank register; no mode bit, so nothing moves yet
        m.cpu_write(0x8001, 3);
        assert_maps_match(&m, "after $8001 = 3 for reg 6");
        assert_reads_match(&mut m, "after $8001 = 3 for reg 6");

        m.cpu_write(0x8000, 2); // CHR 1 KB register 2
        m.cpu_write(0x8001, 5);
        assert_maps_match(&m, "after $8001 = 5 for reg 2");
        assert_reads_match(&mut m, "after $8001 = 5 for reg 2");
    }

    /// Randomised power-on rewrites the bank-select register and all eight bank registers,
    /// so it must rebuild the tables from them.
    #[test]
    fn power_on_random_rebuilds_the_tables() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        for seed in [0u64, 0xDEAD_BEEF_CAFE_F00D] {
            m.power_on_random(seed);
            let ctx = format!("after power_on_random({seed:#X})");
            assert_maps_match(&m, &ctx);
            assert_reads_match(&mut m, &ctx);
        }
    }

    /// `load_state` restores the registers, so it must rebuild the tables from them.
    #[test]
    fn load_state_rebuilds_the_tables() {
        let mut saved = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        saved.cpu_write(0x8000, 6);
        saved.cpu_write(0x8001, 3);
        saved.cpu_write(0x8000, 0x80 | 2);
        saved.cpu_write(0x8001, 5);
        saved.cpu_write(0x8000, 0x40 | 6);
        let mut w = StateWriter::new();
        saved.save_state(&mut w);
        let bytes = w.finish();

        let mut fresh = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        assert_ne!(
            (saved.prg_map, saved.chr_map),
            (fresh.prg_map, fresh.chr_map),
            "the saved registers need different windows than power-on, so the assertions below can fail"
        );
        fresh.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            fresh.bank_select, 0x46,
            "the state restored the bank-select register"
        );
        assert_maps_match(&fresh, "after load_state");
        assert_reads_match(&mut fresh, "after load_state");
    }

    /// The precomputed window tables must index exactly what the per-read offset functions
    /// computed before them, for every address and a spread of register states.
    #[test]
    fn bank_tables_match_the_per_read_offsets() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        for bank_select in [0u8, 0x40, 0x80, 0xC0] {
            m.cpu_write(0x8000, bank_select);
            for reg in 0..8u8 {
                m.cpu_write(0x8000, bank_select | reg);
                m.cpu_write(0x8001, reg * 3 + 1);
            }
            for addr in 0x8000..=0xFFFFu16 {
                assert_eq!(
                    m.cpu_read(addr),
                    m.prg[m.prg_offset(addr)],
                    "PRG {addr:04X} with bank_select {bank_select:02X}"
                );
            }
            for addr in 0..=0x1FFFu16 {
                assert_eq!(
                    m.chr_peek(addr),
                    chr_read(&m.chr, m.chr_offset(addr)),
                    "CHR {addr:04X} with bank_select {bank_select:02X}"
                );
            }
        }

        // Randomised power-on moves every register, and a state load restores them: both must
        // rebuild the tables.
        for seed in [0u64, 0xDEAD_BEEF_CAFE_F00D] {
            m.power_on_random(seed);
            for addr in (0x8000..=0xFFFFu16).step_by(97) {
                assert_eq!(
                    m.cpu_read(addr),
                    m.prg[m.prg_offset(addr)],
                    "PRG {addr:04X} seed {seed}"
                );
            }
            for addr in (0..=0x1FFFu16).step_by(61) {
                assert_eq!(
                    m.chr_peek(addr),
                    chr_read(&m.chr, m.chr_offset(addr)),
                    "CHR {addr:04X}"
                );
            }
        }
        let mut w = StateWriter::new();
        m.save_state(&mut w);
        let bytes = w.finish();
        let mut fresh = Mmc3::new(crate::cart::split(&mmc3_rom()).unwrap());
        fresh.load_state(&mut StateReader::new(&bytes)).unwrap();
        for addr in 0x8000..=0xFFFFu16 {
            assert_eq!(
                fresh.cpu_read(addr),
                fresh.prg[fresh.prg_offset(addr)],
                "PRG {addr:04X} after load"
            );
        }
        for addr in 0..=0x1FFFu16 {
            assert_eq!(
                fresh.chr_peek(addr),
                chr_read(&fresh.chr, fresh.chr_offset(addr)),
                "CHR {addr:04X} after load"
            );
        }
    }

    /// A CHR-RAM board (the `%`-wrapping `chr_read` path included) maps through the same
    /// table. The RAM is filled through `load_chr_ram`, which never consults the window table, with
    /// distinct bytes per 1 KB bank: the old test wrote a 256-byte-periodic pattern through
    /// `ppu_write`, so a wrong bank read the same byte and deleting the `$8001` refresh stayed green.
    #[test]
    fn chr_map_matches_for_chr_ram() {
        let mut m = Mmc3::new(crate::cart::split(&mmc3_rom_chr_ram()).unwrap());
        m.load_chr_ram(&distinct_banks(0x2000, 0x400));
        for reg in 0..8u8 {
            m.cpu_write(0x8000, reg);
            m.cpu_write(0x8001, reg * 7 + 2);
        }
        m.cpu_write(0x8000, 0x80);
        assert_maps_match(&m, "CHR-RAM board");
        for addr in 0..=0x1FFFu16 {
            assert_eq!(
                m.chr_peek(addr),
                chr_read(&m.chr, m.chr_offset(addr)),
                "CHR-RAM {addr:04X}"
            );
        }
        // The write path uses the same window: a byte written at $0500 lands at chr_offset($0500).
        m.ppu_write(0x0500, 0xAB);
        assert_eq!(
            m.chr[m.chr_offset(0x0500)],
            0xAB,
            "ppu_write lands at chr_offset"
        );
    }

    fn nvram_board(chr_is_ram: bool) -> Mmc3 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Mmc3::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 4,
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
