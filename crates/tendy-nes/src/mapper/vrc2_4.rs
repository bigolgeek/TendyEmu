//! Konami VRC2 and VRC4 (iNES mappers 21, 22, 23 and 25).
//!
//! The two chips are near-identical register files: eight 1 KB CHR banks, four 8 KB PRG windows,
//! and a register-select scheme that wires two of the CPU address lines to the chip's two register
//! bits. The VRC4 adds an 8 KB PRG bank at `$C000`, one-screen mirroring, a PRG swap mode and the
//! CPU-cycle IRQ; the VRC2 only has 256 KB of CHR, horizontal/vertical mirroring and a one-bit
//! "Microwire" latch at `$6000-$6FFF` that several games read back.
//!
//! The boards differ only in *which* address lines select the register (NESdev's VRC2b/4f table):
//!
//! ```text
//! variant  A0 column  A1 column  register addresses        iNES mapper
//! VRC2a    A1         A0         $x000 $x002 $x001 $x003   22
//! VRC2b    A0         A1         $x000 $x001 $x002 $x003   23
//! VRC2c    A1         A0         $x000 $x002 $x001 $x003   25
//! VRC4a    A1         A2         $x000 $x002 $x004 $x006   21
//! VRC4b    A1         A0         $x000 $x002 $x001 $x003   25
//! VRC4c    A6         A7         $x000 $x040 $x080 $x0C0   21
//! VRC4d    A3         A2         $x000 $x008 $x004 $x00C   25
//! VRC4e    A2         A3         $x000 $x004 $x008 $x00C   23
//! VRC4f    A0         A1         $x000 $x001 $x002 $x003   23
//! ```
//!
//! A single iNES mapper number covers two variants whose address lines are disjoint, so without an
//! NES 2.0 submapper this board ORs both variants' select lines together: for mapper 21 the low
//! register bit is `A1 | A6` and the high bit `A2 | A7`, which decodes a VRC4a address ($x000-$x006,
//! A6/A7 clear) and a VRC4c address ($x000/$x040/$x080/$x0C0, A1/A2 clear) correctly and never lets
//! one layout's writes alias into the other's. NES 2.0 submappers narrow that to the exact variant.
//! Every mapper-22 image is a VRC2a.

use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_bank_count, chr_ram_slice, chr_read, chr_write,
    copy_prefix, prg_bank_count,
};
use crate::state::{StateError, StateReader, StateWriter};

/// The CPU-cycle IRQ shared by the VRC4, VRC6 and VRC7.
///
/// In scanline mode the prescaler divides the CPU clock by 113⅔ (341/3): starting at 341 and
/// subtracting 3 per cycle clocks the counter on the 114th cycle, then every 114/114/113. In cycle
/// mode the counter is clocked every CPU cycle. The counter counts *up*: reaching `$FF` reloads it
/// from the latch and raises the IRQ. The `A` control bit is copied into `E` by a write to the
/// acknowledge register, which is how a one-shot IRQ disables itself.
#[derive(Clone, Debug, Default)]
pub struct VrcIrq {
    pub latch: u8,
    pub counter: u8,
    pub enabled: bool,
    /// `M`: cycle mode when set, scanline mode when clear.
    pub cycle_mode: bool,
    /// `A`: the enable state installed on the next acknowledge.
    pub after_ack: bool,
    pub pending: bool,
    prescaler: i16,
}

impl VrcIrq {
    pub fn new() -> Self {
        VrcIrq {
            prescaler: 341,
            ..VrcIrq::default()
        }
    }

    /// `$F000` on the VRC4: the latch's low four bits.
    pub fn write_latch_low(&mut self, val: u8) {
        self.latch = (self.latch & 0xF0) | (val & 0x0F);
    }

    /// `$F001` on the VRC4: the latch's high four bits.
    pub fn write_latch_high(&mut self, val: u8) {
        self.latch = (self.latch & 0x0F) | ((val & 0x0F) << 4);
    }

    /// `$F000` on the VRC6/VRC7: the whole byte.
    pub fn write_latch(&mut self, val: u8) {
        self.latch = val;
    }

    /// `$F002` (VRC4) / `$F001` (VRC6, VRC7) control write: acknowledges, resets the prescaler and,
    /// when `E` is set, reloads the counter from the latch.
    pub fn write_control(&mut self, val: u8) {
        self.after_ack = val & 0x01 != 0;
        self.enabled = val & 0x02 != 0;
        self.cycle_mode = val & 0x04 != 0;
        self.pending = false;
        self.prescaler = 341;
        if self.enabled {
            self.counter = self.latch;
        }
    }

    /// `$F003` (VRC4) / `$F002` (VRC6, VRC7) acknowledge: clears the line and copies `A` into `E`.
    pub fn acknowledge(&mut self) {
        self.pending = false;
        self.enabled = self.after_ack;
    }

    /// One CPU cycle.
    pub fn clock(&mut self) {
        if !self.enabled {
            return;
        }
        if self.cycle_mode {
            self.tick();
        } else {
            self.prescaler -= 3;
            if self.prescaler <= 0 {
                self.prescaler += 341;
                self.tick();
            }
        }
    }

    fn tick(&mut self) {
        if self.counter == 0xFF {
            self.counter = self.latch;
            self.pending = true;
        } else {
            self.counter += 1;
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.latch);
        w.u8(self.counter);
        w.bool(self.enabled);
        w.bool(self.cycle_mode);
        w.bool(self.after_ack);
        w.bool(self.pending);
        w.u16(self.prescaler as u16);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.latch = r.u8()?;
        self.counter = r.u8()?;
        self.enabled = r.bool()?;
        self.cycle_mode = r.bool()?;
        self.after_ack = r.bool()?;
        self.pending = r.bool()?;
        self.prescaler = r.u16()? as i16;
        Ok(())
    }
}

/// Which address lines a board wires to the chip's two register-select inputs, and which chip it
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// CPU address bit positions OR'd into the chip's register bit 0.
    low: [u8; 2],
    /// CPU address bit positions OR'd into register bit 1.
    high: [u8; 2],
    vrc4: bool,
    /// VRC2a: the CHR bank's low bit is not connected (banks are 2 KB-aligned).
    vrc2a: bool,
}

impl Layout {
    fn of(mapper: u16, submapper: u8) -> Layout {
        let (low, high, vrc4, vrc2a) = match (mapper, submapper) {
            // 21: VRC4a (A1/A2) and VRC4c (A6/A7).
            (21, 1) => ([1, 1], [2, 2], true, false),
            (21, 2) => ([6, 6], [7, 7], true, false),
            (21, _) => ([1, 6], [2, 7], true, false),
            // 22: always VRC2a.
            (22, _) => ([1, 1], [0, 0], false, true),
            // 23: VRC4f (A0/A1), VRC4e (A2/A3) and VRC2b (A0/A1).
            (23, 1) => ([0, 0], [1, 1], true, false),
            (23, 2) => ([2, 2], [3, 3], true, false),
            (23, 3) => ([0, 0], [1, 1], false, false),
            (23, _) => ([0, 2], [1, 3], true, false),
            // 25: VRC4b (A1/A0), VRC4d (A3/A2) and VRC2c (A1/A0); the VRC4 is presumed for the
            // union because two of the three variants on this mapper are VRC4s.
            (25, 1) => ([1, 1], [0, 0], true, false),
            (25, 2) => ([3, 3], [2, 2], true, false),
            (25, 3) => ([1, 1], [0, 0], false, false),
            (25, _) => ([1, 3], [0, 2], true, false),
            _ => ([0, 0], [1, 1], true, false),
        };
        Layout {
            low,
            high,
            vrc4,
            vrc2a,
        }
    }

    fn register(&self, addr: u16) -> u8 {
        let bit = |positions: [u8; 2]| {
            positions
                .iter()
                .fold(0u8, |acc, &p| acc | ((addr >> p) as u8 & 1))
        };
        bit(self.low) | (bit(self.high) << 1)
    }
}

/// The VRC2/VRC4 board.
pub struct Vrc2_4 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    prg_ram: Vec<u8>,
    layout: Layout,
    /// `$8000` (PRG select 0) and `$A000` (PRG select 1).
    prg_regs: [u8; 2],
    /// Nine-bit CHR bank per 1 KB window, assembled from the low/high register pairs.
    chr_banks: [u16; 8],
    chr_low: [u8; 8],
    chr_high: [u8; 8],
    mirroring: Mirroring,
    /// VRC4 `$9002` bit 1: `$8000` and `$C000` swap windows.
    swap_mode: bool,
    /// VRC4 `$9002` bit 0: PRG RAM is readable/writable at `$6000-$7FFF`.
    wram_enable: bool,
    /// VRC2's one-bit latch at `$6000-$6FFF`.
    latch: u8,
    pub irq: VrcIrq,
}

impl Vrc2_4 {
    pub fn new(rom: RomData) -> Vrc2_4 {
        let submapper = if rom.header.nes2 {
            rom.header.submapper
        } else {
            0
        };
        let layout = Layout::of(rom.header.mapper, submapper);
        let mut board = Vrc2_4 {
            prg: rom.prg,
            chr: rom.chr,
            chr_is_ram: rom.chr_is_ram,
            prg_ram: vec![0; rom.header.prg_ram_size().max(8192)],
            layout,
            prg_regs: [0; 2],
            chr_banks: [0; 8],
            chr_low: [0; 8],
            chr_high: [0; 8],
            mirroring: rom.header.mirroring,
            swap_mode: false,
            wram_enable: false,
            latch: 0,
            irq: VrcIrq::new(),
        };
        board.rebuild_chr();
        board
    }

    fn prg_bank_count(&self) -> usize {
        prg_bank_count(&self.prg)
    }

    fn chr_bank_count(&self) -> usize {
        chr_bank_count(&self.chr)
    }

    /// `slot` is the 8 KB CPU window; `n` is the 8 KB bank count, always even because `cart::split`
    /// pads PRG to 16 KB. The VRC2's fixed 16 KB bank at `$C000` is therefore exactly
    /// `second_last`/`last`, the same values the VRC4 shows without swap mode.
    fn prg_bank(&self, addr: u16) -> usize {
        let n = self.prg_bank_count();
        let last = n - 1;
        let second_last = n.saturating_sub(2);
        let slot = usize::from((addr - 0x8000) / 0x2000);
        let bank = match slot {
            0 => {
                if self.layout.vrc4 && self.swap_mode {
                    second_last
                } else {
                    usize::from(self.prg_regs[0] & 0x3F)
                }
            }
            1 => usize::from(self.prg_regs[1] & 0x3F),
            2 => {
                if self.layout.vrc4 && self.swap_mode {
                    usize::from(self.prg_regs[0] & 0x3F)
                } else {
                    second_last
                }
            }
            _ => last,
        };
        bank % n
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let window = usize::from((addr & 0x1FFF) / 0x400);
        let within = usize::from(addr & 0x3FF);
        let bank = usize::from(self.chr_banks[window]) % self.chr_bank_count();
        bank * 0x400 + within
    }

    fn rebuild_chr(&mut self) {
        for i in 0..8 {
            let mut bank = u16::from(self.chr_low[i] & 0x0F)
                | (u16::from(self.chr_high[i] & if self.layout.vrc4 { 0x1F } else { 0x0F }) << 4);
            if self.layout.vrc2a {
                bank >>= 1;
            }
            self.chr_banks[i] = bank;
        }
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        let reg = self.layout.register(addr);
        match addr & 0xF000 {
            0x8000 => self.prg_regs[0] = val & 0x1F,
            0x9000 => match reg {
                0 => {
                    let mode = if self.layout.vrc4 {
                        val & 0x03
                    } else {
                        val & 0x01
                    };
                    self.mirroring = match mode {
                        0 => Mirroring::Vertical,
                        1 => Mirroring::Horizontal,
                        2 => Mirroring::SingleScreenLower,
                        _ => Mirroring::SingleScreenUpper,
                    };
                }
                2 if self.layout.vrc4 => {
                    self.wram_enable = val & 0x01 != 0;
                    self.swap_mode = val & 0x02 != 0;
                }
                _ => {}
            },
            0xA000 => self.prg_regs[1] = val & 0x1F,
            0xB000..=0xE000 => {
                let group = usize::from(((addr & 0xF000) - 0xB000) >> 12);
                let bank = group * 2 + usize::from((reg >> 1) & 1);
                if reg & 1 == 0 {
                    self.chr_low[bank] = val;
                } else {
                    self.chr_high[bank] = val;
                }
                self.rebuild_chr();
            }
            0xF000 if self.layout.vrc4 => match reg {
                0 => self.irq.write_latch_low(val),
                1 => self.irq.write_latch_high(val),
                2 => self.irq.write_control(val),
                _ => self.irq.acknowledge(),
            },
            _ => {}
        }
    }
}

impl Mapper for Vrc2_4 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.layout.vrc4 {
                    if self.wram_enable {
                        self.prg_ram[usize::from(addr - 0x6000) % self.prg_ram.len()]
                    } else {
                        0
                    }
                } else if addr < 0x7000 {
                    // VRC2's Microwire data pin is tied to the latch on the boards Contra (J) and
                    // Ganbare Goemon 2 run on, so reads return the write value in bit 0 and open
                    // bus ($60, the address's high byte) in the rest.
                    0x60 | self.latch
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => {
                let bank = self.prg_bank(addr);
                self.prg[bank * 0x2000 + usize::from(addr & 0x1FFF)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.layout.vrc4 {
                    if self.wram_enable {
                        let i = usize::from(addr - 0x6000) % self.prg_ram.len();
                        self.prg_ram[i] = val;
                    }
                } else if addr < 0x7000 {
                    self.latch = val & 0x01;
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
        for i in 0..8 {
            w.u8(self.chr_low[i]);
            w.u8(self.chr_high[i]);
            w.u16(self.chr_banks[i]);
        }
        w.u8(self.mirroring.to_tag());
        w.bool(self.swap_mode);
        w.bool(self.wram_enable);
        w.u8(self.latch);
        self.irq.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        r.bytes_into(&mut self.prg_regs)?;
        for i in 0..8 {
            self.chr_low[i] = r.u8()?;
            self.chr_high[i] = r.u8()?;
            self.chr_banks[i] = r.u16()?;
        }
        self.mirroring = Mirroring::from_tag(r.u8()?)?;
        self.swap_mode = r.bool()?;
        self.wram_enable = r.bool()?;
        self.latch = r.u8()?;
        self.irq.load_state(r)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Header;

    /// 256 KB of PRG where every byte holds its 8 KB bank number, 128 KB of CHR where every byte
    /// holds its 1 KB bank number, so a bank check is a single byte read.
    fn rom_data(mapper: u16, submapper: u8, chr_kb: usize) -> RomData {
        RomData {
            header: Header {
                prg_banks: 16,
                chr_banks: chr_kb / 8,
                mapper,
                mirroring: Mirroring::Vertical,
                submapper,
                nes2: submapper != 0,
                ..Default::default()
            },
            prg: (0..0x40000).map(|i| (i / 0x2000) as u8).collect(),
            chr: (0..chr_kb * 1024).map(|i| (i / 0x400) as u8).collect(),
            chr_is_ram: false,
            trainer: None,
        }
    }

    fn board(mapper: u16, submapper: u8) -> Vrc2_4 {
        Vrc2_4::new(rom_data(mapper, submapper, 128))
    }

    /// `rom_data` with `prg_banks` 16 KB banks instead of the fixed 256 KB, so a sweep can reach
    /// bank counts small enough for register values to wrap.
    fn rom_data_prg(mapper: u16, submapper: u8, prg_banks: usize) -> RomData {
        let mut rom = rom_data(mapper, submapper, 128);
        rom.header.prg_banks = prg_banks;
        rom.prg = (0..prg_banks * 0x4000)
            .map(|i| (i / 0x2000) as u8)
            .collect();
        rom
    }

    fn fold(digest: &mut u64, byte: u8) {
        *digest = (*digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
    }

    /// Every reachable `prg_bank` answer - every variant, every value of both PRG registers,
    /// both swap states and four PRG sizes - folded into one FNV-1a digest. `cart::split` pads PRG
    /// to 16 KB, so the bank count `n` is always even and an odd `n` cannot reach `prg_bank`.
    /// The pinned value is what the pre-fix build produced, so a moved digest means the dead
    /// branches were not dead.
    #[test]
    fn prg_bank_sweep_digest_is_pinned() {
        const VARIANTS: [(u16, u8); 9] = [
            (21, 0),
            (21, 1),
            (21, 2),
            (22, 0),
            (23, 0),
            (23, 1),
            (23, 2),
            (23, 3),
            (25, 3),
        ];
        let mut digest = 0xCBF2_9CE4_8422_2325u64;
        for (mapper, submapper) in VARIANTS {
            for prg_banks in [1usize, 2, 4, 16] {
                let mut m = Vrc2_4::new(rom_data_prg(mapper, submapper, prg_banks));
                for reg0 in 0..=31u8 {
                    for reg1 in 0..=31u8 {
                        for swap in [false, true] {
                            m.prg_regs = [reg0, reg1];
                            m.swap_mode = swap;
                            for start in [0x8000u16, 0xA000, 0xC000, 0xE000] {
                                let byte = m.cpu_read(start);
                                fold(&mut digest, byte);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(digest, 0x7CCD_DF84_9A25_C525, "prg_bank sweep digest");
    }

    #[test]
    fn vrc4a_prg_windows() {
        let mut m = board(21, 1);
        m.cpu_write(0x8000, 5);
        m.cpu_write(0xA000, 9);
        assert_eq!(m.cpu_read(0x8000), 5);
        assert_eq!(m.cpu_read(0xA000), 9);
        assert_eq!(
            m.cpu_read(0xC000),
            30,
            "$C000 is the second-to-last 8 KB bank"
        );
        assert_eq!(m.cpu_read(0xE000), 31, "$E000 is the last bank");
    }

    #[test]
    fn vrc4a_swap_mode_moves_the_switchable_window_to_c000() {
        let mut m = board(21, 1);
        m.cpu_write(0x8000, 5);
        m.cpu_write(0x9004, 0x03); // W = 1, M = 1 (register 2 for the A1/A2 wiring)
        assert_eq!(
            m.cpu_read(0x8000),
            30,
            "$8000 is fixed to the second-to-last bank"
        );
        assert_eq!(m.cpu_read(0xC000), 5, "the register now drives $C000");
    }

    #[test]
    fn vrc4c_uses_a6_and_a7_for_registers() {
        // A VRC4c board (mapper 21 submapper 2) selects registers with A6/A7.
        let mut m = board(21, 2);
        m.cpu_write(0x8040, 7);
        m.cpu_write(0xA080, 11);
        m.cpu_write(0x9000, 1); // horizontal, register 0
        m.cpu_write(0x9040, 0); // register 1: not the mirroring register
        assert_eq!(m.cpu_read(0x8000), 7);
        assert_eq!(m.cpu_read(0xA000), 11);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn mapper_21_default_decodes_both_vrc4a_and_vrc4c_wiring() {
        let mut m = board(21, 0);
        m.cpu_write(0x8000, 3); // VRC4a style
        assert_eq!(m.cpu_read(0x8000), 3);
        m.cpu_write(0x8040, 6); // VRC4c style writes the same register
        assert_eq!(m.cpu_read(0x8000), 6);
        // Mirroring is register 0 on every variant, so $9000 reaches it for both wirings.
        m.cpu_write(0x9000, 1);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn vrc4_mirroring_modes() {
        let mut m = board(21, 1);
        for (val, want) in [
            (0u8, Mirroring::Vertical),
            (1, Mirroring::Horizontal),
            (2, Mirroring::SingleScreenLower),
            (3, Mirroring::SingleScreenUpper),
        ] {
            m.cpu_write(0x9000, val);
            assert_eq!(m.mirroring(), want);
        }
    }

    #[test]
    fn vrc2_ignores_the_second_mirroring_bit() {
        let mut m = board(22, 0);
        m.cpu_write(0x9000, 0x01);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        m.cpu_write(0x9000, 0xFF);
        assert_eq!(m.mirroring(), Mirroring::Horizontal, "VRC2 only has bit 0");
    }

    #[test]
    fn chr_bank_pairs_compose_nine_bits() {
        let mut m = board(21, 1); // VRC4a: registers at $x000/$x002/$x004/$x006
        m.cpu_write(0xB000, 0x05); // CHR 0 low
        m.cpu_write(0xB004, 0x05); // CHR 1 low
        m.cpu_write(0xB002, 0x02); // CHR 0 high -> bank 0x25
        m.cpu_write(0xB006, 0x01); // CHR 1 high -> bank 0x15
        assert_eq!(m.ppu_read(0x0000), 0x25);
        assert_eq!(m.ppu_read(0x0400), 0x15);
        m.cpu_write(0xC000, 0x0C); // CHR 2 low
        assert_eq!(m.ppu_read(0x0800), 0x0C);
    }

    #[test]
    fn vrc2_high_chr_bit_four_is_ignored() {
        let mut m = board(23, 3);
        m.cpu_write(0xB000, 0x01);
        m.cpu_write(0xB001, 0x10); // bit 4: VRC4 only
        assert_eq!(m.chr_banks[0], 0x01);
        m.cpu_write(0xB001, 0x0F);
        assert_eq!(m.chr_banks[0], 0xF1, "four high bits are used");
    }

    #[test]
    fn vrc2a_ignores_the_chr_low_bit() {
        let mut m = board(22, 0);
        m.cpu_write(0xB000, 0x05);
        assert_eq!(m.ppu_read(0x0000), 0x02, "the value is halved");
    }

    #[test]
    fn vrc2_latch_reads_back_in_bit_zero() {
        let mut m = board(23, 3); // VRC2b, the variant with the Microwire latch
        m.cpu_write(0x6000, 0x01);
        assert_eq!(m.cpu_read(0x6000), 0x61);
        m.cpu_write(0x6000, 0x00);
        assert_eq!(m.cpu_read(0x6100), 0x60);
        assert_eq!(m.cpu_read(0x7000), 0x00, "$7000-$7FFF is open bus only");
    }

    #[test]
    fn vrc4_prg_ram_needs_the_write_enable_bit() {
        let mut m = board(21, 1);
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0, "W = 0 leaves the window open bus");
        m.cpu_write(0x9004, 0x01); // W = 1, M = 0 (register 2)
        m.cpu_write(0x6000, 0x5A);
        assert_eq!(m.cpu_read(0x6000), 0x5A);
    }

    #[test]
    fn irq_scanline_mode_clocks_every_114_cycles() {
        let mut irq = VrcIrq::new();
        irq.write_latch(0);
        irq.write_control(0x02); // enabled, scanline mode
        assert_eq!(irq.counter, 0);
        for _ in 0..113 {
            irq.clock();
        }
        assert_eq!(irq.counter, 0, "the first clock is on cycle 114");
        irq.clock();
        assert_eq!(irq.counter, 1);
        for _ in 0..114 {
            irq.clock();
        }
        assert_eq!(irq.counter, 2);
    }

    #[test]
    fn irq_counter_reloads_and_fires_at_ff() {
        let mut irq = VrcIrq::new();
        irq.write_latch(0xFE);
        irq.write_control(0x06); // enabled, cycle mode
        assert_eq!(irq.counter, 0xFE);
        irq.clock();
        assert_eq!(irq.counter, 0xFF);
        assert!(!irq.pending);
        irq.clock();
        assert_eq!(irq.counter, 0xFE, "reloaded from the latch");
        assert!(irq.pending);
    }

    #[test]
    fn irq_acknowledge_copies_a_into_e() {
        let mut irq = VrcIrq::new();
        irq.write_latch(0);
        irq.write_control(0x06); // enabled, cycle mode, A = 0
        assert_eq!(irq.counter, 0);
        for _ in 0..0xFF {
            irq.clock();
        }
        assert_eq!(irq.counter, 0xFF);
        assert!(!irq.pending);
        irq.clock();
        assert_eq!(irq.counter, 0, "reloaded from the latch");
        assert!(irq.pending);
        irq.acknowledge();
        assert!(!irq.pending);
        assert!(!irq.enabled, "A = 0 disables the next IRQ");
        irq.clock();
        assert!(!irq.pending);
    }

    #[test]
    fn irq_control_write_acknowledges_and_reloads() {
        let mut irq = VrcIrq::new();
        irq.write_latch(0x10);
        irq.write_control(0x06);
        irq.counter = 0xAA;
        irq.pending = true;
        irq.write_control(0x06);
        assert!(!irq.pending);
        assert_eq!(irq.counter, 0x10, "E set reloads from the latch");
        irq.write_control(0x04); // E clear: counter unchanged
        assert_eq!(irq.counter, 0x10);
        assert!(!irq.enabled);
    }

    #[test]
    fn board_irq_reaches_the_mapper_interface() {
        let mut m = board(21, 1); // VRC4a: latch/control/ack at $F000/$F002/$F004/$F006
        m.cpu_write(0xF000, 0x00); // latch low
        m.cpu_write(0xF002, 0x00); // latch high
        m.cpu_write(0xF004, 0x06); // enabled, cycle mode
        assert!(!m.irq_pending());
        for _ in 0..256 {
            m.cpu_clock();
        }
        assert!(m.irq_pending());
        m.cpu_write(0xF006, 0x00); // acknowledge
        assert!(!m.irq_pending());
    }

    #[test]
    fn state_round_trips() {
        let mut a = board(21, 1);
        a.cpu_write(0x8000, 5);
        a.cpu_write(0xB000, 3);
        a.cpu_write(0x9004, 0x01);
        a.cpu_write(0xF000, 0x20);
        a.cpu_write(0xF004, 0x06);
        for _ in 0..300 {
            a.cpu_clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = board(21, 1);
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(a.cpu_read(0x8000), b.cpu_read(0x8000));
        assert_eq!(a.cpu_read(0xC000), b.cpu_read(0xC000));
        assert_eq!(a.ppu_read(0x0000), b.ppu_read(0x0000));
        assert_eq!(a.irq_pending(), b.irq_pending());
    }

    fn nvram_board(chr_is_ram: bool) -> Vrc2_4 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Vrc2_4::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 21,
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
