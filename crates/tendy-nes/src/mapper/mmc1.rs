use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix,
};
use crate::state::{StateError, StateReader, StateWriter};

/// Mapper 1 (MMC1): a 5-bit serial shift register loads four internal registers that control
/// mirroring, PRG banking (32 KB or 16 KB with either half fixed) and CHR banking (8 KB or two 4 KB).
/// Two NES 2.0 submappers change this: submapper 5 (SEROM/SHROM/SH1ROM) wires PRG A14 straight to
/// the CPU, so the whole 32 KB of PRG is fixed and the PRG bank register has no function; submapper
/// 7 (Kaiser KS-7058) hard-wires the nametable arrangement, ignoring the control register's bits 0-1.
pub struct Mmc1 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    /// 8 KiB PRG-RAM pages the board carries: one on SNROM/SKROM and the rest of the 8 KiB boards,
    /// two on SOROM and four on SXROM. Derived from the header's PRG-RAM/NVRAM size, never saved.
    prg_ram_pages: usize,
    shift: u8,
    count: u8,
    control: u8,
    chr0: u8,
    chr1: u8,
    prg_bank: u8,
    /// True only on NES 2.0 submapper 5: the 32 KB PRG is unbanked.
    fixed_prg: bool,
    /// The nametable arrangement when hard-wired (submapper 7); None follows the control register.
    hardwired_mirroring: Option<Mirroring>,
    /// CPU cycles (M2) seen by this board, advanced by `cpu_clock`.
    cycle: u64,
    /// `cycle` of the last write to $8000-$FFFF (ignored writes included); None before the first.
    last_write: Option<u64>,
    /// Byte offsets of the four 8 KB CPU windows from the current registers; rebuilt by
    /// [`Mmc1::refresh_maps`] whenever a register changes, never saved.
    prg_map: [usize; 4],
    /// Byte offsets of the eight 1 KB PPU windows, likewise.
    chr_map: [usize; 8],
}

impl Mmc1 {
    /// Power-on: control = 0x0C (PRG mode 3: 16 KB switchable at $8000, last bank fixed at $C000).
    pub fn new(rom: RomData) -> Mmc1 {
        // NESdev MMC1: "PRG RAM capacity 32 KiB" and "The SOROM board only implements the upper S
        // bit", so the header's PRG-RAM + NVRAM size picks the board: 8 KiB on SNROM and the rest
        // (iNES 1.0 always reports 8 KiB, the format has no usable field), 16 KiB SOROM and 32 KiB
        // SXROM. The MMC1 addresses no more than 32 KiB, so a larger claim is cut to that rather
        // than refused — a malformed header on a playable game must still play — and the load says
        // so once instead of clamping silently.
        let declared = rom.header.prg_ram_size();
        if declared > 0x8000 {
            eprintln!(
                "warning: a NES 2.0 header declares {declared} bytes of PRG-RAM; MMC1 addresses at most 32768, so the rest is ignored"
            );
        }
        let prg_ram_len = declared.clamp(0x2000, 0x8000);
        let mut m = Mmc1 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; prg_ram_len],
            prg_ram_pages: prg_ram_len / 0x2000,
            shift: 0,
            count: 0,
            control: 0x0C,
            chr0: 0,
            chr1: 0,
            prg_bank: 0,
            fixed_prg: rom.header.nes2 && rom.header.submapper == 5,
            hardwired_mirroring: if rom.header.nes2 && rom.header.submapper == 7 {
                Some(rom.header.mirroring)
            } else {
                None
            },
            cycle: 0,
            last_write: None,
            prg_map: [0; 4],
            chr_map: [0; 8],
        };
        m.refresh_maps();
        m
    }

    /// Rebuild the window tables from the current registers. Same numbers
    /// [`Mmc1::prg_offset`] and [`Mmc1::chr_offset`] produce per read, computed once per write.
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

    fn write_register(&mut self, addr: u16, val: u8) {
        match (addr >> 13) & 0x03 {
            0 => self.control = val,
            1 => self.chr0 = val,
            2 => self.chr1 = val,
            _ => self.prg_bank = val & 0x0F,
        }
        self.refresh_maps();
    }

    /// Byte offset into PRG-ROM for a CPU address in 0x8000..=0xFFFF.
    /// Submapper 5 (SEROM/SHROM/SH1ROM) fixes the whole 32 KB: PRG A14 comes straight from the CPU,
    /// so neither the PRG mode nor the PRG bank register can switch it.
    /// PRG banking works inside a 256 KB window. 512 KB boards (SUROM) pick the window with
    /// bit 4 of the CHR0 register, so the "fixed last bank" is the last bank of that window.
    fn prg_offset(&self, addr: u16) -> usize {
        if self.fixed_prg {
            return usize::from(addr & 0x7FFF) % self.prg.len();
        }
        let banks = (self.prg.len() / 0x4000).min(16);
        let outer = if self.prg.len() > 0x40000 {
            usize::from((self.chr0 >> 4) & 1) * 16
        } else {
            0
        };
        let bank = usize::from(self.prg_bank);
        let offset = usize::from(addr & 0x3FFF);
        let page = match (self.control >> 2) & 0x03 {
            0 | 1 => (bank & !1) + usize::from(addr >= 0xC000),
            2 => {
                if addr < 0xC000 {
                    0
                } else {
                    bank
                }
            }
            _ => {
                if addr < 0xC000 {
                    bank
                } else {
                    banks - 1
                }
            }
        };
        (outer + page % banks) * 0x4000 + offset
    }

    /// Byte offset of the 8 KiB PRG-RAM page that answers at $6000, from the CHR bank 0 register.
    ///
    /// NESdev MMC1, CHR bank 0: bits 3-2 are the "S" bits, "PRG-RAM A13 if the PRG-RAM size is 16
    /// KiB, or PRG-RAM A14 if the PRG-RAM size is 32 KiB" (bit 3) and "PRG-RAM A13 if the PRG-RAM
    /// size is 32 KiB" (bit 2). SOROM has 16 KiB and "only implements the upper S bit", so bit 3 is
    /// its page; SXROM has 32 KiB, so bits 3-2 are `(chr0 >> 2) & 3`. One page (every SNROM,
    /// SKROM, SLROM dump without a NES 2.0 size) ignores both bits and the register is masked by
    /// the page count, so no 5-bit value can index past the RAM.
    fn prg_ram_page(&self) -> usize {
        let page = match self.prg_ram_pages {
            2 => (self.chr0 >> 3) & 1,
            _ => (self.chr0 >> 2) & 3,
        };
        usize::from(page) % self.prg_ram_pages * 0x2000
    }

    /// Byte offset into CHR for a PPU address in 0x0000..=0x1FFF; `chr_read`/`chr_write` wrap it
    /// to the size the board allocated.
    fn chr_offset(&self, addr: u16) -> usize {
        let addr = usize::from(addr & 0x1FFF);
        if self.control & 0x10 == 0 {
            usize::from(self.chr0 & 0x1E) * 0x1000 + addr
        } else if addr < 0x1000 {
            usize::from(self.chr0) * 0x1000 + addr
        } else {
            usize::from(self.chr1) * 0x1000 + (addr - 0x1000)
        }
    }
}

impl Mapper for Mmc1 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[self.prg_ram_page() + usize::from(addr - 0x6000)],
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

    /// Writes with bit 7 set reset the shift register (and set PRG mode 3); otherwise bit 0 is
    /// shifted in, and the fifth write stores the value in the register chosen by address bits 13-14.
    ///
    /// The serial port ignores a write on the CPU cycle right after another write: of the
    /// two back-to-back writes of a read-modify-write instruction only the first counts, and Bill &
    /// Ted's reset code (`$FF` then `$00` one cycle later) relies on it. Two writes cannot land on
    /// the same cycle, so "exactly one cycle later" and "within one cycle" are the same rule. Every
    /// write, ignored or not and bit-7 reset or not, records its cycle; that is cycle-based, so a
    /// reset write does not need to clear `last_write` -- it only matters for the very next cycle.
    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                let page = self.prg_ram_page(); // before the mutable borrow of prg_ram
                self.prg_ram[page + usize::from(addr - 0x6000)] = val;
            }
            0x8000..=0xFFFF => {
                let back_to_back = self.last_write == Some(self.cycle.wrapping_sub(1));
                self.last_write = Some(self.cycle);
                if back_to_back {
                    return;
                }
                if val & 0x80 != 0 {
                    self.shift = 0;
                    self.count = 0;
                    self.control |= 0x0C;
                    self.refresh_maps();
                    return;
                }
                self.shift = (self.shift >> 1) | ((val & 0x01) << 4);
                self.count += 1;
                if self.count == 5 {
                    let v = self.shift;
                    self.write_register(addr, v);
                    self.shift = 0;
                    self.count = 0;
                }
            }
            _ => {}
        }
    }

    /// The live 4 KiB/8 KiB CHR mapping without the shift-register or write-timing state
    /// `ppu_read` touches: MMC1's CHR banks are pure registers.
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
        self.chr_peek(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_is_ram {
            let slot = usize::from((addr >> 10) & 7);
            let i = self.chr_map[slot] + usize::from(addr & 0x3FF);
            chr_write(&mut self.chr, i, val);
        }
    }

    fn mirroring(&self) -> Mirroring {
        if let Some(m) = self.hardwired_mirroring {
            return m;
        }
        match self.control & 0x03 {
            0 => Mirroring::SingleScreenLower,
            1 => Mirroring::SingleScreenUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        }
    }

    /// Randomised power-on: the shift register and control register take seed-derived
    /// garbage. Both are only read through masks (control bits 0-1 mirroring, 2-3 PRG mode,
    /// bit 4 CHR mode) and bank indexes reduce modulo the bank count, so any 5-bit value is
    /// garbage-but-valid: no out-of-range read, no panic.
    fn power_on_ram(&mut self, mode: crate::bus::RamPowerOn, seed: u64) {
        let chr: &mut [u8] = if self.chr_is_ram {
            &mut self.chr
        } else {
            &mut []
        };
        crate::cart::fill_power_on_ram(&mut self.prg_ram, chr, mode, seed);
    }

    fn power_on_random(&mut self, seed: u64) {
        self.shift = (seed & 0x1F) as u8;
        self.count = 0;
        self.control = ((seed >> 8) & 0x1F) as u8;
        self.refresh_maps();
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.shift);
        w.u8(self.count);
        w.u8(self.control);
        w.u8(self.chr0);
        w.u8(self.chr1);
        w.u8(self.prg_bank);
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.shift = r.u8()?;
        self.count = r.u8()?;
        self.control = r.u8()?;
        self.chr0 = r.u8()?;
        self.chr1 = r.u8()?;
        self.prg_bank = r.u8()?;
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        // `cycle`/`last_write` are deliberately not serialised (format unchanged). States are taken
        // and loaded between instructions, and every instruction starts with an opcode-fetch read,
        // so the next cart write is at least two cycles after any write before the snapshot: the
        // one-cycle ignore rule can never fire across a save/load boundary. Clearing `last_write`
        // drops the live session's stale history so it cannot match the restored timeline either.
        self.last_write = None;
        self.refresh_maps();
        Ok(())
    }

    fn hooks(&self) -> CartHooks {
        CartHooks {
            irq: false,
            cpu_clock: true,
        }
    }

    fn cpu_clock(&mut self) {
        self.cycle = self.cycle.wrapping_add(1);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes for a banked image where the same offset in two different banks never holds the same
    /// value: the bank index is in the high nibble, the offset's bank/16-th group in the low one.
    /// The old fixtures were all zeros, so a wrong bank read 0 either way and the differential test
    /// could not fail.
    fn distinct_banks(len: usize, bank: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (((i / bank) as u8) << 4) | ((i / (bank / 16)) as u8 & 0x0F))
            .collect()
    }

    /// MMC1 cart with mapper-1 header: 32 KB PRG (four 8 KB banks) + 16 KB CHR ROM (four 4 KB
    /// banks), every bank filled with [`distinct_banks`] so a stale window cannot alias.
    fn rom() -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 2, 2, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v.resize(16 + 2 * 16384 + 16384, 0);
        v[16..16 + 0x8000].copy_from_slice(&distinct_banks(0x8000, 0x2000));
        v[16 + 0x8000..].copy_from_slice(&distinct_banks(0x4000, 0x400));
        v
    }

    /// A NES 2.0 mapper-1 cart whose header declares `64 << prg_ram_shift` bytes of PRG RAM, 8 KiB
    /// CHR RAM and battery backing: the SOROM (shift 8, 16 KiB) and SXROM (shift 9, 32 KiB) boards.
    fn nes2(prg_ram_shift: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 2, 0, 0x12, 0x08, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v[10] = prg_ram_shift;
        v[11] = 0x07; // 8 KiB CHR RAM
        v.resize(16 + 0x8000, 0);
        v[16..16 + 0x8000].copy_from_slice(&distinct_banks(0x8000, 0x2000));
        v
    }

    /// NESdev MMC1, CHR bank 0: bit 3 is "PRG-RAM A13 if the PRG-RAM size is 16 KiB, or PRG-RAM
    /// A14 if the PRG-RAM size is 32 KiB" and bit 2 is "PRG-RAM A13 if the PRG-RAM size is 32 KiB",
    /// so SXROM's four 8 KiB pages are `(chr0 >> 2) & 3`.
    #[test]
    fn sxrom_selects_one_of_four_prg_ram_pages_from_chr0() {
        let mut m = Mmc1::new(crate::cart::split(&nes2(9)).unwrap());
        assert_eq!(
            m.prg_ram.len(),
            32768,
            "a NES 2.0 size of 32 KiB is four pages"
        );
        for page in 0u8..4 {
            m.chr0 = page << 2;
            m.cpu_write(0x6000, 0xA0 + page);
        }
        for page in 0u8..4 {
            m.chr0 = page << 2;
            assert_eq!(m.cpu_read(0x6000), 0xA0 + page, "page {page} of the SRAM");
        }
    }

    /// A NES 2.0 header asking for more PRG-RAM than the MMC1 can address still plays — the
    /// claim is cut to four 8 KiB pages rather than refused, and the load warns (next test).
    #[test]
    fn nes2_prg_ram_over_32k_is_cut_to_32k() {
        let mut m = Mmc1::new(crate::cart::split(&nes2(10)).unwrap());
        assert_eq!(
            m.prg_ram.len(),
            32768,
            "64 << 10 = 64 KiB declared, 32 KiB addressable"
        );
        m.chr0 = 0;
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(
            m.cpu_read(0x6000),
            0x5A,
            "the cut cart still has working SRAM"
        );
    }

    /// The warning reaches stderr. A test cannot read its own libtest-captured stderr, so this
    /// reruns the test above in a child process with `--nocapture` and reads the child's stderr:
    /// without the `eprintln!` in `Mmc1::new` the probe prints nothing and this fails.
    #[test]
    fn nes2_prg_ram_over_32k_warns_on_stderr() {
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "mapper::mmc1::tests::nes2_prg_ram_over_32k_is_cut_to_32k",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the probe test failed in the child process"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("warning:"), "no warning on stderr: {err:?}");
        assert!(
            err.contains("65536"),
            "the declared size must be named: {err:?}"
        );
        assert!(
            err.contains("32768"),
            "the size actually used must be named: {err:?}"
        );
    }

    /// SOROM has 16 KiB and "only implements the upper S bit", so bit 3 of CHR bank 0 alone picks
    /// the page; bit 2 is a no-op there.
    #[test]
    fn sorom_switches_prg_ram_on_chr0_bit_3_only() {
        let mut m = Mmc1::new(crate::cart::split(&nes2(8)).unwrap());
        assert_eq!(
            m.prg_ram.len(),
            16384,
            "a NES 2.0 size of 16 KiB is two pages"
        );
        m.chr0 = 0x00;
        m.cpu_write(0x6000, 0x11);
        m.chr0 = 0x08;
        m.cpu_write(0x6000, 0x22);
        m.chr0 = 0x04;
        assert_eq!(
            m.cpu_read(0x6000),
            0x11,
            "bit 2 must leave the page alone on SOROM"
        );
        m.chr0 = 0x08;
        assert_eq!(m.cpu_read(0x6000), 0x22, "bit 3 picks the second page");
    }

    /// Plain 8 KiB boards (every iNES 1.0 mapper-1 dump) keep one page; the S bits must not reach
    /// it, or a SNROM save would appear to move whenever a CHR bank is written.
    #[test]
    fn a_plain_8k_board_ignores_the_prg_ram_bits() {
        let mut m = board();
        assert_eq!(m.prg_ram.len(), 8192);
        m.cpu_write(0x6000, 0x77);
        for chr0 in [0x04u8, 0x08, 0x0C, 0x14] {
            m.chr0 = chr0;
            assert_eq!(m.cpu_read(0x6000), 0x77, "chr0 {chr0:#04X}");
        }
    }

    #[test]
    fn power_on_random_is_deterministic() {
        let seed = 0xDEAD_BEEF_CAFE_F00D;
        let mut a = Mmc1::new(crate::cart::split(&rom()).unwrap());
        a.power_on_random(seed);
        let snapshot = (a.shift, a.count, a.control);
        a.power_on_random(seed);
        assert_eq!(
            (a.shift, a.count, a.control),
            snapshot,
            "same seed twice, same registers"
        );
        let mut b = Mmc1::new(crate::cart::split(&rom()).unwrap());
        b.power_on_random(seed);
        assert_eq!(
            (b.shift, b.count, b.control),
            snapshot,
            "fresh boards agree too"
        );
    }

    #[test]
    fn power_on_random_never_panics() {
        for seed in [0u64, 0x1F_FFFF_FFFF_FFFF, u64::MAX, 0xABCD_1234_5678_9FFF] {
            let mut m = Mmc1::new(crate::cart::split(&rom()).unwrap());
            m.power_on_random(seed);
            // Garbage registers must still land every read inside the cart.
            for addr in [0x6000u16, 0x7FFF, 0x8000, 0xA000, 0xBFFF, 0xC000, 0xFFFF] {
                m.cpu_read(addr);
            }
            for addr in [0x0000u16, 0x0FFF, 0x1000, 0x1FFF] {
                m.ppu_read(addr);
            }
            m.mirroring();
        }
    }

    fn board() -> Mmc1 {
        Mmc1::new(crate::cart::split(&rom()).unwrap())
    }

    #[test]
    fn write_one_cycle_after_another_is_ignored() {
        let mut m = board();
        m.cpu_write(0x8000, 0x01);
        m.cpu_clock();
        m.cpu_write(0x8000, 0x01);
        assert_eq!(
            (m.shift, m.count),
            (0x10, 1),
            "second write, one cycle later, must not shift"
        );
    }

    #[test]
    fn write_two_cycles_after_another_is_accepted() {
        let mut m = board();
        m.cpu_write(0x8000, 0x01);
        m.cpu_clock();
        m.cpu_clock();
        m.cpu_write(0x8000, 0x01);
        assert_eq!(
            (m.shift, m.count),
            (0x18, 2),
            "two cycles apart, both writes shift"
        );
    }

    /// Bill & Ted's reset code: `$FF` (reset) then `$00` one cycle later; the `$00` must not count.
    #[test]
    fn write_right_after_a_reset_write_is_ignored() {
        let mut m = board();
        m.cpu_write(0x8000, 0x01);
        m.cpu_clock();
        m.cpu_clock();
        m.cpu_write(0xFFFF, 0xFF);
        m.cpu_clock();
        m.cpu_write(0xFFFF, 0x00);
        assert_eq!(
            (m.shift, m.count),
            (0, 0),
            "reset applies, the following write is dropped"
        );
        // Five spaced writes then latch normally: PRG bank register = 0b00101.
        for bit in [1u8, 0, 1, 0, 0] {
            m.cpu_clock();
            m.cpu_clock();
            m.cpu_write(0xE000, bit);
        }
        assert_eq!((m.prg_bank, m.count), (0x05, 0));
    }

    #[test]
    fn save_load_round_trip_does_not_carry_write_timing() {
        let mut m = board();
        m.cpu_write(0x8000, 0x01);
        let mut w = crate::state::StateWriter::new();
        m.save_state(&mut w);
        let data = w.finish();
        let mut fresh = board();
        // The live board wrote one cycle before the load; that history must not survive it.
        fresh.cpu_write(0x8000, 0x00);
        fresh.cpu_clock();
        fresh
            .load_state(&mut crate::state::StateReader::new(&data))
            .unwrap();
        fresh.cpu_write(0x8000, 0x01);
        assert_eq!(
            (fresh.shift, fresh.count),
            (0x18, 2),
            "first write after a load always counts"
        );
    }

    /// The `rom()` board with CHR-RAM instead of CHR-ROM (no CHR banks in the header).
    fn rom_chr_ram() -> Vec<u8> {
        let mut v = rom();
        v[5] = 0;
        v.truncate(16 + 2 * 16384);
        v
    }

    fn rom_with_chr_pattern() -> Vec<u8> {
        let mut v = rom();
        for (i, b) in v[16 + 2 * 16384..].iter_mut().enumerate() {
            *b = (i % 7) as u8 + 1;
        }
        v
    }

    #[test]
    fn power_on_ram_random_fills_prg_and_chr_ram_deterministically() {
        use crate::bus::RamPowerOn;
        let seed = 0x0123_4567_89AB_CDEF;
        let make = |mode, seed| {
            let mut m = Mmc1::new(crate::cart::split(&rom_chr_ram()).unwrap());
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
            "MMC1: PRG RAM was filled"
        );
        assert!(a.chr.iter().any(|&x| x != 0), "MMC1: CHR-RAM was filled");
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
        let mut m = Mmc1::new(crate::cart::split(&rom_chr_ram()).unwrap());
        m.power_on_ram(RamPowerOn::Zero, 0x1234);
        assert!(m.prg_ram().iter().all(|&x| x == 0));
        assert!(m.chr.iter().all(|&x| x == 0));
    }

    #[test]
    fn power_on_ram_never_touches_chr_rom() {
        use crate::bus::RamPowerOn;
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            let mut m = Mmc1::new(crate::cart::split(&rom_with_chr_pattern()).unwrap());
            let before: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            m.power_on_ram(mode, 99);
            let after: Vec<u8> = (0..0x2000u16).map(|a| m.chr_peek(a)).collect();
            assert_eq!(before, after, "{mode:?}");
            assert!(!m.chr.is_empty(), "CHR-ROM still present");
        }
    }

    /// The invariant the two window tables exist for: each slot holds the offset the per-read
    /// `prg_offset`/`chr_offset` computes for that window's base address.
    fn assert_maps_match(m: &Mmc1, ctx: &str) {
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
    /// per-read offset names. On the distinct fixture a stale window cannot alias onto the right
    /// byte, so this is the differential guarantee the tables were introduced under.
    fn assert_reads_match(m: &mut Mmc1, ctx: &str) {
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

    /// Five serial bits with no reset, for tests that set up the shift register themselves.
    fn serial_bits(m: &mut Mmc1, addr: u16, val: u8) {
        for bit in 0..5 {
            m.cpu_clock();
            m.cpu_clock();
            m.cpu_write(addr, (val >> bit) & 1);
        }
    }

    /// One MMC1 serial write: a bit-7 reset, then five bits two cycles apart (the board ignores a
    /// write on the cycle right after another).
    fn serial_write(m: &mut Mmc1, addr: u16, val: u8) {
        m.cpu_write(0x8000, 0x80); // reset the shift register
        serial_bits(m, addr, val);
    }

    /// `Mmc1::new` builds the tables for its power-on registers (control $0C: PRG mode 3,
    /// the last 16 KB bank at $C000; CHR 8 KB mode).
    #[test]
    fn new_builds_the_bank_tables() {
        let mut m = board();
        assert_ne!(
            m.prg[0], m.prg[0x4000],
            "fixture: the same offset in two 16 KB PRG banks differs"
        );
        assert_maps_match(&m, "at power-on");
        assert_reads_match(&mut m, "at power-on");
    }

    /// The fifth serial write latches a register and must rebuild the tables: the PRG mode
    /// bits, the PRG bank, and both CHR halves each move a window without any other write.
    #[test]
    fn serial_register_write_rebuilds_the_tables() {
        let mut m = board();
        serial_write(&mut m, 0xE000, 1); // prg_bank = 1
        assert_maps_match(&m, "after the PRG bank register");
        assert_reads_match(&mut m, "after the PRG bank register");
        serial_write(&mut m, 0x8000, 0x08); // control: PRG mode 2 ($8000 fixed, $C000 switchable)
        assert_maps_match(&m, "after control = $08 (PRG mode 2)");
        assert_reads_match(&mut m, "after control = $08 (PRG mode 2)");
        serial_write(&mut m, 0x8000, 0x1C); // control: PRG mode 3, CHR 4 KB mode
        assert_maps_match(&m, "after control = $1C (CHR 4 KB mode)");
        serial_write(&mut m, 0xA000, 1); // chr0 = 1
        assert_maps_match(&m, "after chr0 = 1");
        assert_reads_match(&mut m, "after chr0 = 1");
        serial_write(&mut m, 0xC000, 2); // chr1 = 2
        assert_maps_match(&m, "after chr1 = 2");
        assert_reads_match(&mut m, "after chr1 = 2");
    }

    /// A bit-7 reset write forces the PRG mode bits to 3, so it must rebuild the tables
    /// without going through `write_register`.
    #[test]
    fn reset_write_rebuilds_the_tables() {
        let mut m = board();
        serial_write(&mut m, 0x8000, 0x08); // control: PRG mode 2
        serial_bits(&mut m, 0xE000, 1); // prg_bank = 1, still PRG mode 2
        // PRG mode 2: $8000 is fixed at the first bank and $C000 takes bank 1, so the table below
        // is [0, $2000, $4000, $6000]; the reset's mode-3 rebuild has to move it.
        assert_maps_match(&m, "in PRG mode 2 with prg_bank = 1");
        m.cpu_clock();
        m.cpu_clock();
        m.cpu_write(0x8000, 0x80); // control |= $0C: PRG mode 3
        assert_maps_match(&m, "after a bit-7 reset write");
        assert_reads_match(&mut m, "after a bit-7 reset write");
    }

    /// `power_on_random` replaces the control register (PRG and CHR modes), so it must
    /// rebuild the tables from it.
    #[test]
    fn power_on_random_rebuilds_the_tables() {
        let mut m = board();
        serial_write(&mut m, 0xE000, 1); // prg_bank = 1
        serial_write(&mut m, 0xA000, 1); // chr0 = 1, so a mode change moves a window
        let before = (m.prg_map, m.chr_map);
        let mut moved = false;
        for seed in [0u64, 0xDEAD_BEEF_CAFE_F00D, 0x0123_4567_89AB_CDEF] {
            m.power_on_random(seed);
            let ctx = format!("after power_on_random({seed:#X})");
            assert_maps_match(&m, &ctx);
            assert_reads_match(&mut m, &ctx);
            moved |= (m.prg_map, m.chr_map) != before;
        }
        assert!(
            moved,
            "at least one seed must move a window, or these assertions are vacuous"
        );
    }

    /// `load_state` restores all six registers, so it must rebuild the tables from them.
    #[test]
    fn load_state_rebuilds_the_tables() {
        let mut saved = board();
        serial_write(&mut saved, 0x8000, 0x1C); // PRG mode 3, CHR 4 KB mode
        serial_write(&mut saved, 0xE000, 1); // prg_bank = 1
        serial_write(&mut saved, 0xA000, 1); // chr0 = 1
        serial_write(&mut saved, 0xC000, 2); // chr1 = 2
        let mut w = StateWriter::new();
        saved.save_state(&mut w);
        let bytes = w.finish();

        let mut fresh = board();
        assert_ne!(
            (saved.prg_map, saved.chr_map),
            (fresh.prg_map, fresh.chr_map),
            "the saved registers need different windows than power-on, so the assertions below can fail"
        );
        fresh.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            fresh.prg_bank, 1,
            "the state restored the PRG bank register"
        );
        assert_maps_match(&fresh, "after load_state");
        assert_reads_match(&mut fresh, "after load_state");
    }

    /// The precomputed window tables must index exactly what the per-read offset functions
    /// computed before them, for every address and a spread of register states.
    #[test]
    fn bank_tables_match_the_per_read_offsets() {
        let mut m = board();
        for (control, chr0, chr1, prg_bank) in [
            (0x0Cu8, 0x00u8, 0x00u8, 0x00u8),
            (0x1C, 0x02, 0x04, 0x05),
            (0x08, 0x06, 0x08, 0x01),
            (0x0C, 0x10, 0x12, 0x0F),
        ] {
            serial_write(&mut m, 0x8000, control);
            serial_write(&mut m, 0xA000, chr0);
            serial_write(&mut m, 0xC000, chr1);
            serial_write(&mut m, 0xE000, prg_bank);
            for addr in 0x8000..=0xFFFFu16 {
                assert_eq!(
                    m.cpu_read(addr),
                    m.prg[m.prg_offset(addr)],
                    "PRG {addr:04X} control {control:02X} chr0 {chr0:02X} prg_bank {prg_bank:02X}"
                );
            }
            for addr in 0..=0x1FFFu16 {
                assert_eq!(
                    m.chr_peek(addr),
                    chr_read(&m.chr, m.chr_offset(addr)),
                    "CHR {addr:04X} control {control:02X} chr0 {chr0:02X} chr1 {chr1:02X}"
                );
            }
        }

        // The bit-7 reset write rewrites the PRG mode bits from mode 2, and `power_on_random`
        // rewrites control. Two cycles first: the board ignores a write on the next cycle.
        serial_write(&mut m, 0x8000, 0x08);
        m.cpu_clock();
        m.cpu_clock();
        m.cpu_write(0x8000, 0xFF);
        for addr in 0x8000..=0xFFFFu16 {
            assert_eq!(
                m.cpu_read(addr),
                m.prg[m.prg_offset(addr)],
                "PRG {addr:04X} after reset"
            );
        }
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
        let mut fresh = board();
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

    fn nvram_board(chr_is_ram: bool) -> Mmc1 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Mmc1::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 1,
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
