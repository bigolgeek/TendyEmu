//! Mapper 5 (MMC5 / ExROM): PRG and CHR banking, ExRAM, per-1 KiB nametable mapping,
//! extended attributes, vertical split screen, scanline IRQ, multiplier and expansion audio.
//!
//! The PPU cannot describe an MMC5 screen with a [`Mirroring`] value: each 1 KiB nametable
//! slot can point at one of two CIRAM pages, the on-cart ExRAM, or fill mode, and the board
//! substitutes nametable/attribute/pattern bytes for the vertical split screen. The PPU
//! therefore asks the board for its nametable bytes (`nt_read`/`nt_write`) and reports every
//! PPU memory access through `ppu_read`/`ppu_read_sprite`; this module keeps the chip's own
//! fetch counter from those accesses, the way the real MMC5 watches the PPU address bus.
//!
//! References: NESdev wiki *MMC5* and *MMC5 audio*, Mesen2 `Core/NES/Mappers/Nintendo/MMC5.h`
//! and `Audio/Mmc5Audio.h` for the fetch-sequenced split/extended-attribute substitutions.

use crate::cart::{
    CartHooks, Mapper, Mirroring, RomData, chr_ram_slice, chr_read, chr_write, copy_prefix,
};
use crate::state::{StateError, StateReader, StateWriter};

/// 32-entry length counter table, same as the 2A03.
const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

/// 4 duty cycles x 8 steps, identical to the 2A03 pulse channel.
const DUTY: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

/// CPU cycles between the MMC5's 240 Hz envelope/length clocks (NTSC CPU clock / 240).
const AUDIO_240HZ_DIVIDER: u16 = 7457;

/// One MMC5 pulse channel: 2A03 pulse minus the sweep unit, with the length counter and
/// envelope clocked at a fixed 240 Hz. Registers are `$5000-$5003` / `$5004-$5007`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mmc5Pulse {
    pub enabled: bool,
    pub duty: u8,
    pub halt: bool,
    pub constant: bool,
    /// Envelope period (decay rate) or constant volume.
    pub volume: u8,
    pub period: u16,
    pub timer: u16,
    pub step: u8,
    pub length: u8,
    pub env_start: bool,
    pub env_divider: u8,
    pub env_decay: u8,
}

impl Mmc5Pulse {
    /// `reg` is 0..=3 (the low two bits of the register address).
    fn write(&mut self, reg: u16, val: u8) {
        match reg & 3 {
            0 => {
                self.duty = (val >> 6) & 3;
                self.halt = val & 0x20 != 0;
                self.constant = val & 0x10 != 0;
                self.volume = val & 0x0F;
            }
            // $5001/$5005 is the sweep slot: the MMC5 pulses have no sweep unit.
            1 => {}
            2 => self.period = (self.period & 0x0700) | u16::from(val),
            3 => {
                self.period = (self.period & 0x00FF) | (u16::from(val & 7) << 8);
                if self.enabled {
                    self.length = LENGTH_TABLE[usize::from(val >> 3)];
                }
                self.step = 0;
                self.env_start = true;
            }
            _ => {}
        }
    }

    /// `$5015` bit: a disabled channel's length count is forced to 0 (silence).
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
    }

    /// One APU clock (every second CPU cycle): advance the 11-bit timer and duty step.
    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            self.step = (self.step + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }

    /// 240 Hz envelope clock.
    fn clock_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.volume;
        } else if self.env_divider == 0 {
            self.env_divider = self.volume;
            if self.env_decay > 0 {
                self.env_decay -= 1;
            } else if self.halt {
                self.env_decay = 15;
            }
        } else {
            self.env_divider -= 1;
        }
    }

    /// 240 Hz length clock (the MMC5 length counter runs at twice the APU's 120 Hz rate).
    fn clock_length(&mut self) {
        if !self.halt && self.length > 0 {
            self.length -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.length == 0 || DUTY[usize::from(self.duty & 3)][usize::from(self.step & 7)] == 0 {
            0
        } else if self.constant {
            self.volume
        } else {
            self.env_decay
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.u8(self.duty);
        w.bool(self.halt);
        w.bool(self.constant);
        w.u8(self.volume);
        w.u16(self.period);
        w.u16(self.timer);
        w.u8(self.step);
        w.u8(self.length);
        w.bool(self.env_start);
        w.u8(self.env_divider);
        w.u8(self.env_decay);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.enabled = r.bool()?;
        self.duty = r.u8()?;
        self.halt = r.bool()?;
        self.constant = r.bool()?;
        self.volume = r.u8()?;
        self.period = r.u16()?;
        self.timer = r.u16()?;
        self.step = r.u8()?;
        self.length = r.u8()?;
        self.env_start = r.bool()?;
        self.env_divider = r.u8()?;
        self.env_decay = r.u8()?;
        Ok(())
    }
}

/// The chip's two CHR bank-register sets. `$5120-$5127` always serve sprite fetches; `$5128-$512B`
/// serve background fetches while 8x16 sprites are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChrSet {
    A,
    B,
}

/// MMC5 (mapper 5).
pub struct Mmc5 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    /// The board's own 2 KiB CIRAM, addressed through `$5105`.
    vram: Vec<u8>,
    /// 1 KiB of ExRAM behind `$5C00-$5FFF` (also the extra/fill nametable source).
    exram: [u8; 0x400],
    /// PRG-RAM as the wiki's 64 KiB "compatible superset": licensed boards expose 8/16/32 KiB in
    /// one or two chips, and no game depends on the empty sockets answering open bus.
    prg_ram: Vec<u8>,

    prg_mode: u8,
    chr_mode: u8,
    /// `$5102`/`$5103`: PRG-RAM writes are enabled only while these hold 0b10 and 0b01.
    ram_protect1: u8,
    ram_protect2: u8,
    exram_mode: u8,
    /// `$5105`: four 2-bit nametable sources, low pair = table 0.
    nametable_map: u8,
    fill_tile: u8,
    fill_color: u8,
    /// `$5114`..`$5117` raw register values: bit 7 selects ROM (1) or PRG-RAM (0).
    prg_banks: [u8; 4],
    /// `$5113`: 8 KiB PRG-RAM bank at `$6000-$7FFF`.
    prg_ram_bank: u8,
    /// `$5120-$512B`: 10-bit CHR banks (low 8 bits written, high 2 from `$5130` at write time).
    chr_banks: [u16; 12],
    chr_upper: u8,
    /// The bank set most recently written; `$2007` uses it when 8x16 sprites are enabled.
    last_chr_set: ChrSet,
    /// `$2000` bit 5 / `$2001` bits 3-4 as the chip last saw them written (it snoops the PPU).
    sprites_8x16: bool,
    rendering: bool,

    // Vertical split ($5200-$5202).
    split_enabled: bool,
    split_right: bool,
    split_delimiter: u8,
    split_scroll: u8,
    split_bank: u8,
    split_in_region: bool,
    split_tile: u16,
    split_tile_number: u16,

    // Extended attributes ($5104 mode 01).
    ex_attr_last_nt: u16,
    ex_fetch_counter: u8,
    ex_chr_bank: u16,

    // Scanline IRQ.
    irq_target: u8,
    irq_enabled: bool,
    irq_pending: bool,
    scanline_counter: u8,

    // Scanline detector: three identical $2xxx reads arm the next read as a line start.
    need_in_frame: bool,
    ppu_in_frame: bool,
    ppu_idle: u8,
    last_ppu_addr: Option<u16>,
    nt_read_counter: u8,

    mul_a: u8,
    mul_b: u8,

    // Audio.
    pulse1: Mmc5Pulse,
    pulse2: Mmc5Pulse,
    pcm_ctrl: u8,
    pcm_output: u8,
    audio_divider: u16,
    audio_phase: u8,
}

impl Mmc5 {
    /// Power-on state: PRG mode 3 (four 8 KiB banks) and `$5117` = `$FF`, so the reset vector at
    /// `$FFFC` lands in the last PRG bank. Every other register is zero.
    pub fn new(rom: RomData) -> Mmc5 {
        let chr_is_ram = rom.chr_is_ram;
        let chr = if chr_is_ram && rom.chr.is_empty() {
            vec![0; 8192]
        } else {
            rom.chr
        };
        Mmc5 {
            prg: rom.prg,
            chr,
            chr_is_ram,
            vram: vec![0; 0x800],
            exram: [0; 0x400],
            prg_ram: vec![0; 0x1_0000],
            prg_mode: 3,
            chr_mode: 0,
            ram_protect1: 0,
            ram_protect2: 0,
            exram_mode: 0,
            nametable_map: 0,
            fill_tile: 0,
            fill_color: 0,
            prg_banks: [0, 0, 0, 0xFF],
            prg_ram_bank: 0,
            chr_banks: [0; 12],
            chr_upper: 0,
            last_chr_set: ChrSet::A,
            sprites_8x16: false,
            rendering: false,
            split_enabled: false,
            split_right: false,
            split_delimiter: 0,
            split_scroll: 0,
            split_bank: 0,
            split_in_region: false,
            split_tile: 0,
            split_tile_number: 0,
            ex_attr_last_nt: 0,
            ex_fetch_counter: 0,
            ex_chr_bank: 0,
            irq_target: 0,
            irq_enabled: false,
            irq_pending: false,
            scanline_counter: 0,
            need_in_frame: false,
            ppu_in_frame: false,
            ppu_idle: 0,
            last_ppu_addr: None,
            nt_read_counter: 0,
            mul_a: 0,
            mul_b: 0,
            pulse1: Mmc5Pulse::default(),
            pulse2: Mmc5Pulse::default(),
            pcm_ctrl: 0,
            pcm_output: 0,
            audio_divider: AUDIO_240HZ_DIVIDER,
            audio_phase: 0,
        }
    }

    fn prg_8k_pages(&self) -> usize {
        (self.prg.len() / 0x2000).max(1)
    }

    /// A register value's bank index, wrapped into the image.
    fn wrap(&self, bank: usize, count: usize) -> usize {
        bank % count.max(1)
    }

    /// Offset into the 64 KiB PRG-RAM superset for an 8 KiB bank value (bits 2..0).
    fn prg_ram_offset(&self, raw: u8, addr: u16) -> usize {
        let bank = usize::from(raw & 0x07);
        (bank * 0x2000 + usize::from(addr & 0x1FFF)) % self.prg_ram.len().max(1)
    }

    fn prg_ram_writable(&self) -> bool {
        self.ram_protect1 & 3 == 2 && self.ram_protect2 & 3 == 1
    }

    /// `(slot, size, offset_in_slot)` for a CPU address in `$8000-$FFFF`; `slot` indexes
    /// `prg_banks` (0 = `$5114`; slot 4 is `$5117`).
    fn prg_window(&self, addr: u16) -> (usize, usize, usize) {
        let off = usize::from(addr - 0x8000);
        match self.prg_mode & 3 {
            0 => (3, 0x8000, off),
            1 => {
                if off < 0x4000 {
                    (1, 0x4000, off)
                } else {
                    (3, 0x4000, off - 0x4000)
                }
            }
            2 => {
                if off < 0x4000 {
                    (1, 0x4000, off)
                } else if off < 0x6000 {
                    (2, 0x2000, off - 0x4000)
                } else {
                    (3, 0x2000, off - 0x6000)
                }
            }
            _ => {
                let slot = off / 0x2000;
                (slot, 0x2000, off % 0x2000)
            }
        }
    }

    /// True when the slot maps PRG-RAM (bit 7 clear), except `$5117`, which is always ROM.
    fn slot_is_ram(&self, slot: usize) -> bool {
        slot != 3 && self.prg_banks[slot] & 0x80 == 0
    }

    fn read_prg(&self, addr: u16) -> u8 {
        let (slot, size, off) = self.prg_window(addr);
        if self.slot_is_ram(slot) {
            // A 16 KiB window's bit 0 is ignored and CPU A13 picks the second 8 KiB page.
            let raw = self.prg_banks[slot];
            let page = if size == 0x4000 {
                (raw & 0x06) | u8::from(off >= 0x2000)
            } else {
                raw
            };
            return self.prg_ram[self.prg_ram_offset(page, off as u16)];
        }
        // `$5117` is always ROM. In a 16 KiB window the low bit of the register is ignored and
        // CPU A13 selects the second 8 KiB page; in a 32 KiB window CPU A14/A13 select pages.
        let raw = self.prg_banks[slot];
        let page = match size {
            0x4000 => (raw & 0x7E) | u8::from(off >= 0x2000),
            0x8000 => (raw & 0x7C) | ((off >> 13) as u8 & 3),
            _ => raw,
        };
        let bank = self.wrap(usize::from(page & 0x7F), self.prg_8k_pages());
        self.prg[bank * 0x2000 + (off & 0x1FFF)]
    }

    fn write_prg(&mut self, addr: u16, val: u8) {
        if !(0x8000..=0xFFFF).contains(&addr) {
            return;
        }
        let (slot, size, off) = self.prg_window(addr);
        if !self.slot_is_ram(slot) || !self.prg_ram_writable() {
            return;
        }
        let raw = self.prg_banks[slot];
        let page = if size == 0x4000 {
            (raw & 0x06) | u8::from(off >= 0x2000)
        } else {
            raw
        };
        let idx = self.prg_ram_offset(page, off as u16);
        self.prg_ram[idx] = val;
    }

    /// CHR offset through one bank set, per the `$5101` table.
    fn chr_set_offset(&self, addr: u16, set: ChrSet) -> usize {
        let a = usize::from(addr & 0x1FFF);
        let (size, bank) = match (self.chr_mode & 3, set) {
            (0, ChrSet::A) => (0x2000, self.chr_banks[7]),
            (0, ChrSet::B) => (0x2000, self.chr_banks[11]),
            (1, ChrSet::A) => (0x1000, self.chr_banks[(a >> 12) * 4 + 3]),
            (1, ChrSet::B) => (0x1000, self.chr_banks[11]),
            (2, ChrSet::A) => (0x0800, self.chr_banks[(a >> 11) * 2 + 1]),
            (2, ChrSet::B) => (0x0800, self.chr_banks[((a >> 11) & 1) * 2 + 9]),
            (_, ChrSet::A) => (0x0400, self.chr_banks[a >> 10]),
            (_, ChrSet::B) => (0x0400, self.chr_banks[(a >> 10) & 3 | 8]),
        };
        let banks = self.chr.len().div_ceil(size).max(1);
        (usize::from(bank) % banks) * size + (a & (size - 1))
    }

    /// The bank set a background fetch or `$2007` access uses.
    fn bg_chr_set(&self) -> ChrSet {
        if !self.sprites_8x16 {
            ChrSet::A
        } else if self.rendering && self.ppu_in_frame {
            ChrSet::B
        } else {
            self.last_chr_set
        }
    }

    /// CHR byte for a PPU address, with the vertical-split and ExGrafix overrides applied only to
    /// background fetches inside the render window.
    fn chr_byte(&self, addr: u16, sprite: bool) -> u8 {
        let off = if sprite {
            self.chr_set_offset(addr, ChrSet::A)
        } else {
            self.chr_set_offset(addr, self.bg_chr_set())
        };
        chr_read(&self.chr, off)
    }

    /// Nametable byte for `$2000-$2FFF` (already mirrored from `$3000-$3EFF`).
    fn nametable_byte(&self, addr: u16) -> u8 {
        let off = usize::from(addr & 0x3FF);
        let table = usize::from((addr >> 10) & 3);
        match (self.nametable_map >> (table * 2)) & 3 {
            0 => self.vram[off],
            1 => self.vram[0x400 + off],
            2 => {
                if self.exram_mode <= 1 {
                    self.exram[off]
                } else {
                    0
                }
            }
            _ => {
                if off < 0x3C0 {
                    self.fill_tile
                } else {
                    self.fill_color * 0x55
                }
            }
        }
    }

    fn nametable_write(&mut self, addr: u16, val: u8) {
        let off = usize::from(addr & 0x3FF);
        let table = usize::from((addr >> 10) & 3);
        match (self.nametable_map >> (table * 2)) & 3 {
            0 => self.vram[off] = val,
            1 => self.vram[0x400 + off] = val,
            2 if self.exram_mode <= 1 => self.exram[off] = val,
            // Fill-mode and the read-only ExRAM nametable modes drop PPU writes.
            _ => {}
        }
    }

    /// The chip's scanline detector: three consecutive PPU reads of one nametable address arm the
    /// next read, whatever its address, as a line start (the attribute fetch at PPU cycle 4).
    fn detect_scanline_start(&mut self, addr: u16) {
        if self.nt_read_counter >= 2 {
            if !self.ppu_in_frame && !self.need_in_frame {
                self.need_in_frame = true;
                self.scanline_counter = 0;
            } else {
                self.scanline_counter = self.scanline_counter.wrapping_add(1);
                // A $5203 value of 0 never compares equal.
                if self.irq_target != 0 && self.scanline_counter == self.irq_target {
                    self.irq_pending = true;
                }
            }
        } else if (0x2000..=0x2FFF).contains(&addr) && self.last_ppu_addr == Some(addr) {
            self.nt_read_counter += 1;
            if self.nt_read_counter >= 2 {
                self.split_tile_number = 0;
            }
        }
        if self.last_ppu_addr != Some(addr) {
            self.nt_read_counter = 0;
        }
    }

    /// The "in frame" flag is cleared, the IRQ acknowledged and the counter reset.
    fn leave_frame(&mut self) {
        self.ppu_in_frame = false;
        self.need_in_frame = false;
        self.irq_pending = false;
        self.scanline_counter = 0;
    }

    /// The byte a CPU read of `addr` returns, without the NMI-vector `leave_frame()` side
    /// effect: the part `cpu_read` and `cpu_peek` share.
    fn read_prg_or_ram(&mut self, addr: u16) -> u8 {
        if addr >= 0x8000 {
            return self.read_prg(addr);
        }
        match addr {
            0x6000..=0x7FFF => self.prg_ram[self.prg_ram_offset(self.prg_ram_bank, addr)],
            _ => self.ext_read(addr).unwrap_or(0),
        }
    }

    /// Book-keeping shared by every PPU read (`mem_read` on the PPU's side), returning the split /
    /// extended-attribute substitution when one applies. `addr` is a 14-bit PPU address. `sprite`
    /// marks a sprite pattern fetch: those still feed the scanline detector but never take the
    /// background-only split/ExGrafix substitutions (a sprite-slot dummy nametable read would
    /// otherwise arm the ExGrafix latch for the sprite's own pattern bytes).
    fn ppu_fetch_event(&mut self, addr: u16, sprite: bool) -> Option<u8> {
        let is_nt_fetch = (0x2000..=0x2FFF).contains(&addr) && (addr & 0x3FF) < 0x3C0;
        if is_nt_fetch {
            self.split_tile_number = self.split_tile_number.wrapping_add(1);
            if self.ppu_in_frame {
                // The 8x16 sprite/bank window is open for background fetches.
            } else if self.need_in_frame {
                self.need_in_frame = false;
                self.ppu_in_frame = true;
            }
        }
        self.detect_scanline_start(addr);
        self.ppu_idle = 3;
        self.last_ppu_addr = Some(addr);

        if sprite || self.exram_mode > 1 || !self.ppu_in_frame {
            return None;
        }

        if self.split_enabled {
            let scanline = if self.split_tile_number >= 41 {
                self.scanline_counter.wrapping_add(1)
            } else {
                self.scanline_counter
            };
            let vscroll = u16::from(scanline.wrapping_add(self.split_scroll) % 240);
            let column = self.split_tile_number.wrapping_add(2) % 42;
            if (0x2000..=0x2FFF).contains(&addr) {
                if is_nt_fetch {
                    if column == 0 {
                        self.split_in_region = !self.split_right;
                    }
                    if column == u16::from(self.split_delimiter) && self.split_tile_number < 42 {
                        self.split_in_region = !self.split_in_region;
                    } else if column > 32 {
                        self.split_in_region = false;
                    }
                    if self.split_in_region {
                        self.split_tile = ((vscroll & 0xF8) << 2) | column;
                        return Some(self.exram[usize::from(self.split_tile) & 0x3FF]);
                    }
                } else if self.split_in_region {
                    // Attribute fetch for the split region, taken from ExRAM's attribute table.
                    let t = usize::from(self.split_tile);
                    let shift = ((t >> 4) & 0x04) | (t & 0x02);
                    let at = 0x3C0 | ((t & 0x380) >> 4) | ((t & 0x1F) >> 2);
                    let palette = (self.exram[at & 0x3FF] >> shift) & 3;
                    return Some(palette * 0x55);
                }
            } else if self.split_in_region {
                let off = (u16::from(self.split_bank) << 12)
                    + (((addr & !0x07) | (vscroll & 0x07)) & 0x0FFF);
                return Some(chr_read(&self.chr, usize::from(off)));
            }
        }

        if self.exram_mode == 1 && (self.split_tile_number < 32 || self.split_tile_number >= 40) {
            if is_nt_fetch {
                self.ex_attr_last_nt = addr & 0x3FF;
                self.ex_fetch_counter = 3;
            } else if self.ex_fetch_counter > 0 {
                self.ex_fetch_counter -= 1;
                match self.ex_fetch_counter {
                    2 => {
                        let value = self.exram[usize::from(self.ex_attr_last_nt) & 0x3FF];
                        self.ex_chr_bank =
                            u16::from(value & 0x3F) | (u16::from(self.chr_upper) << 6);
                        return Some(((value & 0xC0) >> 6) * 0x55);
                    }
                    _ => {
                        let off = (self.ex_chr_bank << 12) + (addr & 0x0FFF);
                        return Some(chr_read(&self.chr, usize::from(off)));
                    }
                }
            }
        }
        None
    }

    /// The mixer value the chip contributes, in APU pulse units and with the MMC5's inverted
    /// polarity. Two pulse channels read 0..=15 each; the raw PCM channel is scaled so full scale
    /// is one pulse channel's peak.
    pub fn audio_output(&self) -> f32 {
        let pulses = f32::from(self.pulse1.output() + self.pulse2.output());
        let pcm = f32::from(self.pcm_output) * (15.0 / 255.0);
        -(pulses + pcm)
    }

    fn clock_audio(&mut self) {
        self.audio_divider = self.audio_divider.saturating_sub(1);
        if self.audio_divider == 0 {
            self.audio_divider = AUDIO_240HZ_DIVIDER;
            self.pulse1.clock_length();
            self.pulse1.clock_envelope();
            self.pulse2.clock_length();
            self.pulse2.clock_envelope();
        }
        self.audio_phase ^= 1;
        if self.audio_phase == 0 {
            self.pulse1.clock_timer();
            self.pulse2.clock_timer();
        }
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            0x5000..=0x5003 => self.pulse1.write(addr, val),
            0x5004..=0x5007 => self.pulse2.write(addr, val),
            0x5010 => self.pcm_ctrl = val,
            0x5011 => {
                // Writes are ignored in PCM read mode; a write of 0 does not change the DAC.
                if self.pcm_ctrl & 1 == 0 && val != 0 {
                    self.pcm_output = val;
                }
            }
            0x5015 => {
                self.pulse1.set_enabled(val & 1 != 0);
                self.pulse2.set_enabled(val & 2 != 0);
            }
            0x5100 => self.prg_mode = val & 3,
            0x5101 => self.chr_mode = val & 3,
            0x5102 => self.ram_protect1 = val & 3,
            0x5103 => self.ram_protect2 = val & 3,
            0x5104 => {
                self.exram_mode = val & 3;
                self.ex_fetch_counter = 0;
            }
            0x5105 => self.nametable_map = val,
            0x5106 => self.fill_tile = val,
            0x5107 => self.fill_color = val & 3,
            0x5113 => self.prg_ram_bank = val,
            0x5114..=0x5117 => self.prg_banks[usize::from(addr - 0x5114)] = val,
            0x5120..=0x5127 => {
                self.chr_banks[usize::from(addr - 0x5120)] =
                    u16::from(val) | (u16::from(self.chr_upper) << 8);
                self.last_chr_set = ChrSet::A;
            }
            0x5128..=0x512B => {
                self.chr_banks[usize::from(addr - 0x5128) + 8] =
                    u16::from(val) | (u16::from(self.chr_upper) << 8);
                self.last_chr_set = ChrSet::B;
            }
            0x5130 => self.chr_upper = val & 3,
            0x5200 => {
                self.split_enabled = val & 0x80 != 0;
                self.split_right = val & 0x40 != 0;
                self.split_delimiter = val & 0x1F;
            }
            0x5201 => self.split_scroll = val,
            0x5202 => self.split_bank = val,
            0x5203 => self.irq_target = val,
            0x5204 => self.irq_enabled = val & 0x80 != 0,
            0x5205 => self.mul_a = val,
            0x5206 => self.mul_b = val,
            0x5C00..=0x5FFF => {
                let i = usize::from(addr - 0x5C00);
                match self.exram_mode {
                    2 => self.exram[i] = val,
                    3 => {}
                    // Modes 0/1: the ExRAM is write-only, and only while the PPU is rendering.
                    _ => self.exram[i] = if self.ppu_in_frame { val } else { 0 },
                }
            }
            _ => {}
        }
    }
}

impl Mapper for Mmc5 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        // A read of the NMI vector clears the in-frame flag and acknowledges the IRQ.
        if addr >= 0x8000 && addr & 0xFFFE == 0xFFFA {
            self.leave_frame();
        }
        self.read_prg_or_ram(addr)
    }

    /// The same byte without `leave_frame()`, so a trace line or a debugger peek cannot
    /// acknowledge the scanline IRQ behind the game's back.
    fn cpu_peek(&mut self, addr: u16) -> u8 {
        self.read_prg_or_ram(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_writable() {
                    let i = self.prg_ram_offset(self.prg_ram_bank, addr);
                    self.prg_ram[i] = val;
                }
            }
            0x8000..=0xFFFF => self.write_prg(addr, val),
            _ => self.write_register(addr, val),
        }
    }

    /// CPU access in `$4020-$5FFF`: the board's register and ExRAM window. `None` means open bus.
    fn ext_read(&mut self, addr: u16) -> Option<u8> {
        let v = match addr {
            0x5015 => u8::from(self.pulse1.length > 0) | (u8::from(self.pulse2.length > 0) << 1),
            0x5204 => {
                let v = (u8::from(self.ppu_in_frame) << 6) | (u8::from(self.irq_pending) << 7);
                self.irq_pending = false;
                v
            }
            0x5205 => (u16::from(self.mul_a) * u16::from(self.mul_b)) as u8,
            0x5206 => ((u16::from(self.mul_a) * u16::from(self.mul_b)) >> 8) as u8,
            0x5C00..=0x5FFF => {
                if self.exram_mode <= 1 {
                    return None; // write-only in modes 0/1
                }
                self.exram[usize::from(addr - 0x5C00)]
            }
            0x5000..=0x5014 => 0,
            _ => return None,
        };
        Some(v)
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        if let Some(v) = self.ppu_fetch_event(addr & 0x1FFF, false) {
            return v;
        }
        self.chr_byte(addr, false)
    }

    /// Sprite pattern fetch: always the `$5120-$5127` bank set, whatever `$2001` says.
    fn ppu_read_sprite(&mut self, addr: u16) -> u8 {
        if let Some(v) = self.ppu_fetch_event(addr & 0x1FFF, true) {
            return v;
        }
        self.chr_byte(addr, true)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if !self.chr_is_ram {
            return;
        }
        let off = self.chr_set_offset(addr, ChrSet::A);
        chr_write(&mut self.chr, off, val);
    }

    fn nt_read(&mut self, addr: u16, _ciram: &[u8]) -> Option<u8> {
        let addr = addr & 0x3FFF;
        if !(0x2000..=0x3EFF).contains(&addr) {
            return None;
        }
        let a = addr & 0x2FFF;
        if let Some(v) = self.ppu_fetch_event(a, false) {
            return Some(v);
        }
        Some(self.nametable_byte(a))
    }

    /// [`nt_read`](Mapper::nt_read) without the split-screen fetch clock: the debug viewer
    /// must see the same bytes while MMC5's `ppu_fetch_event` state stays untouched.
    fn nt_peek(&self, addr: u16, _ciram: &[u8]) -> Option<u8> {
        let addr = addr & 0x3FFF;
        if !(0x2000..=0x3EFF).contains(&addr) {
            return None;
        }
        Some(self.nametable_byte(addr & 0x2FFF))
    }

    fn nt_write(&mut self, addr: u16, val: u8, _ciram: &mut [u8]) -> bool {
        let addr = addr & 0x3FFF;
        if !(0x2000..=0x3EFF).contains(&addr) {
            return false;
        }
        self.nametable_write(addr & 0x2FFF, val);
        true
    }

    fn ppu_register_write(&mut self, addr: u16, val: u8) {
        // The chip decodes the PPU's own registers at $2000/$2001 (not their mirrors).
        if addr > 0x2007 {
            return;
        }
        match addr & 7 {
            0 => {
                self.sprites_8x16 = val & 0x20 != 0;
                if !self.sprites_8x16 {
                    self.last_chr_set = ChrSet::A;
                }
            }
            1 => {
                self.rendering = val & 0x18 != 0;
                if !self.rendering {
                    self.ppu_in_frame = false;
                    self.need_in_frame = false;
                }
            }
            _ => {}
        }
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        let off = self.chr_set_offset(addr, ChrSet::A);
        chr_read(&self.chr, off)
    }

    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        crate::cart::chr_rom_index(
            &self.chr,
            self.chr_is_ram,
            self.chr_set_offset(addr, ChrSet::A),
        )
    }

    /// The screen layout is runtime data (`$5105`), not a mirroring enum; `FourScreen` tells the PPU
    /// nothing here because every nametable access goes through `nt_read`/`nt_write`.
    fn mirroring(&self) -> Mirroring {
        Mirroring::FourScreen
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

    fn irq_pending(&self) -> bool {
        self.irq_pending && self.irq_enabled
    }

    fn hooks(&self) -> CartHooks {
        CartHooks {
            irq: true,
            cpu_clock: true,
        }
    }

    fn cpu_clock(&mut self) {
        self.clock_audio();
        if self.ppu_idle > 0 {
            self.ppu_idle -= 1;
            if self.ppu_idle == 0 {
                // Three CPU cycles with no PPU read: rendering has stopped, leave the frame.
                self.ppu_in_frame = false;
            }
        }
    }

    fn expansion_audio(&self) -> f32 {
        self.audio_output()
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("MMC5")
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.prg_mode);
        w.u8(self.chr_mode);
        w.u8(self.ram_protect1);
        w.u8(self.ram_protect2);
        w.u8(self.exram_mode);
        w.u8(self.nametable_map);
        w.u8(self.fill_tile);
        w.u8(self.fill_color);
        w.u8(self.prg_ram_bank);
        w.bytes(&self.prg_banks);
        for b in &self.chr_banks {
            w.u16(*b);
        }
        w.u8(self.chr_upper);
        w.u8(match self.last_chr_set {
            ChrSet::A => 0,
            ChrSet::B => 1,
        });
        w.bool(self.sprites_8x16);
        w.bool(self.rendering);
        w.bool(self.split_enabled);
        w.bool(self.split_right);
        w.u8(self.split_delimiter);
        w.u8(self.split_scroll);
        w.u8(self.split_bank);
        w.bool(self.split_in_region);
        w.u16(self.split_tile);
        w.u16(self.split_tile_number);
        w.u16(self.ex_attr_last_nt);
        w.u8(self.ex_fetch_counter);
        w.u16(self.ex_chr_bank);
        w.u8(self.irq_target);
        w.bool(self.irq_enabled);
        w.bool(self.irq_pending);
        w.u8(self.scanline_counter);
        w.bool(self.need_in_frame);
        w.bool(self.ppu_in_frame);
        w.u8(self.ppu_idle);
        match self.last_ppu_addr {
            Some(a) => {
                w.u8(1);
                w.u16(a);
            }
            None => w.u8(0),
        }
        w.u8(self.nt_read_counter);
        w.u8(self.mul_a);
        w.u8(self.mul_b);
        self.pulse1.save_state(w);
        self.pulse2.save_state(w);
        w.u8(self.pcm_ctrl);
        w.u8(self.pcm_output);
        w.u16(self.audio_divider);
        w.u8(self.audio_phase);
        w.bytes(&self.exram);
        w.bytes(&self.vram);
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.prg_mode = r.u8()?;
        self.chr_mode = r.u8()?;
        self.ram_protect1 = r.u8()?;
        self.ram_protect2 = r.u8()?;
        self.exram_mode = r.u8()?;
        self.nametable_map = r.u8()?;
        self.fill_tile = r.u8()?;
        self.fill_color = r.u8()?;
        self.prg_ram_bank = r.u8()?;
        r.bytes_into(&mut self.prg_banks)?;
        for b in &mut self.chr_banks {
            *b = r.u16()?;
        }
        self.chr_upper = r.u8()?;
        self.last_chr_set = if r.u8()? == 1 { ChrSet::B } else { ChrSet::A };
        self.sprites_8x16 = r.bool()?;
        self.rendering = r.bool()?;
        self.split_enabled = r.bool()?;
        self.split_right = r.bool()?;
        self.split_delimiter = r.u8()?;
        self.split_scroll = r.u8()?;
        self.split_bank = r.u8()?;
        self.split_in_region = r.bool()?;
        self.split_tile = r.u16()?;
        self.split_tile_number = r.u16()?;
        self.ex_attr_last_nt = r.u16()?;
        self.ex_fetch_counter = r.u8()?;
        self.ex_chr_bank = r.u16()?;
        self.irq_target = r.u8()?;
        self.irq_enabled = r.bool()?;
        self.irq_pending = r.bool()?;
        self.scanline_counter = r.u8()?;
        self.need_in_frame = r.bool()?;
        self.ppu_in_frame = r.bool()?;
        self.ppu_idle = r.u8()?;
        self.last_ppu_addr = if r.bool()? { Some(r.u16()?) } else { None };
        self.nt_read_counter = r.u8()?;
        self.mul_a = r.u8()?;
        self.mul_b = r.u8()?;
        self.pulse1.load_state(r)?;
        self.pulse2.load_state(r)?;
        self.pcm_ctrl = r.u8()?;
        self.pcm_output = r.u8()?;
        self.audio_divider = r.u16()?;
        self.audio_phase = r.u8()?;
        r.bytes_into(&mut self.exram)?;
        r.bytes_into(&mut self.vram)?;
        r.bytes_into(&mut self.prg_ram)?;
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::{Header, Timing};

    /// PRG-ROM where every byte is its 8 KiB bank number; CHR where every byte is its 1 KiB page
    /// number modulo 251 (a prime, so 10-bit bank numbers stay distinguishable).
    fn rom_data(prg_16k: usize, chr_kb: usize) -> RomData {
        let prg = (0..prg_16k * 0x4000)
            .map(|i| (i / 0x2000) as u8)
            .collect::<Vec<_>>();
        let chr = (0..chr_kb * 0x400)
            .map(|i| ((i / 0x400) % 251) as u8)
            .collect::<Vec<_>>();
        RomData {
            header: Header {
                prg_banks: prg_16k,
                chr_banks: chr_kb / 8,
                mapper: 5,
                mirroring: Mirroring::Vertical,
                nes2: false,
                timing: Timing::Ntsc,
                ..Default::default()
            },
            prg,
            chr,
            chr_is_ram: chr_kb == 0,
            trainer: None,
        }
    }

    fn board(prg_16k: usize, chr_kb: usize) -> Mmc5 {
        Mmc5::new(rom_data(prg_16k, chr_kb))
    }

    fn unlock_ram(m: &mut Mmc5) {
        m.cpu_write(0x5102, 0x02);
        m.cpu_write(0x5103, 0x01);
    }

    /// Drive the detector to the first line start: three identical nametable reads, the attribute
    /// read that detects the line, then the next nametable fetch that opens the frame. A CHR read
    /// breaks the identical-address run so the next line's triple starts clean.
    fn enter_frame(m: &mut Mmc5) {
        m.nt_read(0x2000, &[]);
        m.nt_read(0x2000, &[]);
        m.nt_read(0x2000, &[]);
        m.nt_read(0x23C0, &[]);
        m.nt_read(0x2000, &[]);
        m.ppu_read(0x0000);
    }

    /// One later scanline: three identical nametable fetches, then the attribute read.
    fn next_scanline(m: &mut Mmc5) {
        m.nt_read(0x2000, &[]);
        m.nt_read(0x2000, &[]);
        m.nt_read(0x2000, &[]);
        m.nt_read(0x23C0, &[]);
    }

    #[test]
    fn power_on_selects_the_last_prg_bank() {
        let mut m = board(8, 8);
        // $5114-$5116 power on at 0: bit 7 clear, so those windows map PRG-RAM (all zero here).
        assert_eq!(m.cpu_read(0x8000), 0);
        assert_eq!(
            m.cpu_read(0xE000),
            15,
            "$5117 powers on at $FF, the last 8 KiB bank"
        );
    }

    #[test]
    fn prg_mode_0_is_one_32k_window() {
        let mut m = board(8, 8);
        m.cpu_write(0x5100, 0);
        m.cpu_write(0x5117, 0x04); // 32 KiB bank 4 = 8 KiB pages 4..7
        assert_eq!(m.cpu_read(0x8000), 4);
        assert_eq!(m.cpu_read(0xA000), 5);
        assert_eq!(m.cpu_read(0xC000), 6);
        assert_eq!(m.cpu_read(0xFFFF), 7);
    }

    #[test]
    fn prg_mode_1_is_two_16k_windows() {
        let mut m = board(8, 8);
        m.cpu_write(0x5100, 1);
        m.cpu_write(0x5115, 0x86); // pages 6,7 at $8000-$BFFF
        m.cpu_write(0x5117, 0x80); // pages 0,1 at $C000-$FFFF
        assert_eq!(m.cpu_read(0x8000), 6);
        assert_eq!(m.cpu_read(0xA000), 7);
        assert_eq!(m.cpu_read(0xC000), 0);
        assert_eq!(m.cpu_read(0xE000), 1);
    }

    #[test]
    fn prg_mode_2_is_16k_plus_two_8k() {
        let mut m = board(8, 8);
        m.cpu_write(0x5100, 2);
        m.cpu_write(0x5115, 0x82); // pages 2,3 (bit 0 ignored)
        m.cpu_write(0x5116, 0x84);
        m.cpu_write(0x5117, 0x85);
        assert_eq!(m.cpu_read(0x8000), 2);
        assert_eq!(m.cpu_read(0xA000), 3);
        assert_eq!(m.cpu_read(0xC000), 4);
        assert_eq!(m.cpu_read(0xE000), 5);
    }

    #[test]
    fn prg_mode_3_is_four_8k_windows() {
        let mut m = board(8, 8);
        m.cpu_write(0x5100, 3);
        for (i, reg) in [0x5114u16, 0x5115, 0x5116, 0x5117].into_iter().enumerate() {
            m.cpu_write(reg, 0x80 | (i + 1) as u8);
        }
        assert_eq!(m.cpu_read(0x8000), 1);
        assert_eq!(m.cpu_read(0xA000), 2);
        assert_eq!(m.cpu_read(0xC000), 3);
        assert_eq!(m.cpu_read(0xE000), 4);
    }

    #[test]
    fn prg_ram_banks_through_5113() {
        let mut m = board(8, 8);
        unlock_ram(&mut m);
        m.cpu_write(0x6000, 0xAA);
        assert_eq!(m.cpu_read(0x6000), 0xAA);
        m.cpu_write(0x5113, 1);
        assert_ne!(m.cpu_read(0x6000), 0xAA, "bank 1 is a different page");
        m.cpu_write(0x6000, 0xBB);
        m.cpu_write(0x5113, 0);
        assert_eq!(m.cpu_read(0x6000), 0xAA);
        m.cpu_write(0x5113, 1);
        assert_eq!(m.cpu_read(0x6000), 0xBB);
    }

    #[test]
    fn prg_ram_writes_need_the_protect_pair() {
        let mut m = board(8, 8);
        m.cpu_write(0x6000, 0x12);
        assert_eq!(m.cpu_read(0x6000), 0, "locked RAM drops writes");
        m.cpu_write(0x5102, 0x02);
        m.cpu_write(0x6000, 0x34);
        assert_eq!(m.cpu_read(0x6000), 0, "only half of the pair set");
        m.cpu_write(0x5103, 0x01);
        m.cpu_write(0x6000, 0x56);
        assert_eq!(m.cpu_read(0x6000), 0x56);
        m.cpu_write(0x5102, 0x00);
        m.cpu_write(0x6000, 0x78);
        assert_eq!(m.cpu_read(0x6000), 0x56, "re-locking drops writes again");
    }

    #[test]
    fn prg_ram_can_be_mapped_into_8000() {
        let mut m = board(8, 8);
        unlock_ram(&mut m);
        m.cpu_write(0x5100, 3);
        m.cpu_write(0x5114, 0x00); // bit 7 clear: PRG-RAM at $8000-$9FFF
        m.cpu_write(0x8000, 0x5A);
        assert_eq!(m.cpu_read(0x8000), 0x5A);
        m.cpu_write(0x5114, 0x81); // bit 7 set: ROM bank 1, writes dropped
        m.cpu_write(0x8000, 0x77);
        assert_eq!(m.cpu_read(0x8000), 1);
    }

    #[test]
    fn chr_mode_3_is_eight_1k_banks() {
        let mut m = board(8, 32);
        m.cpu_write(0x5101, 3);
        for (i, reg) in (0x5120u16..=0x5127).enumerate() {
            m.cpu_write(reg, i as u8);
        }
        for i in 0..8u16 {
            assert_eq!(m.ppu_read(i * 0x400), i as u8, "1 KiB bank {i}");
            assert_eq!(m.ppu_read_sprite(i * 0x400), i as u8, "sprites share set A");
        }
    }

    #[test]
    fn chr_mode_0_is_one_8k_bank() {
        let mut m = board(8, 32);
        m.cpu_write(0x5101, 0);
        m.cpu_write(0x5127, 1); // 8 KiB bank 1 = 1 KiB pages 8..15
        assert_eq!(m.ppu_read(0x0000), 8);
        assert_eq!(m.ppu_read(0x1C00), 15);
    }

    #[test]
    fn chr_mode_1_is_two_4k_banks() {
        let mut m = board(8, 32);
        m.cpu_write(0x5101, 1);
        m.cpu_write(0x5123, 2); // pages 8..11
        m.cpu_write(0x5127, 3); // pages 12..15
        assert_eq!(m.ppu_read(0x0000), 8);
        assert_eq!(m.ppu_read(0x0C00), 11);
        assert_eq!(m.ppu_read(0x1000), 12);
        assert_eq!(m.ppu_read(0x1C00), 15);
    }

    #[test]
    fn chr_mode_2_is_four_2k_banks() {
        let mut m = board(8, 32);
        m.cpu_write(0x5101, 2);
        for (i, reg) in (0x5121u16..=0x5127).step_by(2).enumerate() {
            m.cpu_write(reg, (i + 1) as u8);
        }
        // $5121=1 -> pages 2,3; $5123=2 -> 4,5; $5125=3 -> 6,7; $5127=4 -> 8,9
        assert_eq!(m.ppu_read(0x0000), 2);
        assert_eq!(m.ppu_read(0x0800), 4);
        assert_eq!(m.ppu_read(0x1000), 6);
        assert_eq!(m.ppu_read(0x1800), 8);
    }

    #[test]
    fn chr_upper_bits_extend_the_bank_number() {
        let mut m = board(8, 512);
        m.cpu_write(0x5101, 3);
        m.cpu_write(0x5130, 1); // upper bits 01 -> bank += 256
        m.cpu_write(0x5120, 44);
        // 1 KiB page 300, distinguishable from page 44 because the fixture is page % 251.
        assert_eq!(m.ppu_read(0x0000), (300 % 251) as u8);
    }

    #[test]
    fn background_chr_uses_set_b_with_8x16_sprites() {
        let mut m = board(8, 32);
        m.cpu_write(0x5101, 3);
        m.cpu_write(0x5120, 0);
        m.cpu_write(0x5128, 16); // B set, 1 KiB bank 16
        m.ppu_register_write(0x2000, 0x20); // 8x16 sprites
        m.ppu_register_write(0x2001, 0x08); // background on
        enter_frame(&mut m); // rendering fetches are background fetches
        assert_eq!(m.ppu_read(0x0000), 16, "background fetch uses $5128");
        assert_eq!(m.ppu_read_sprite(0x0000), 0, "sprite fetch uses $5120");
        // A $2007-style access outside the frame uses the set written last (B here).
        m.cpu_clock();
        m.cpu_clock();
        m.cpu_clock();
        assert!(!m.ppu_in_frame);
        assert_eq!(m.ppu_read(0x0000), 16);
    }

    #[test]
    fn multiplier_reads_the_16_bit_product() {
        let mut m = board(8, 8);
        m.cpu_write(0x5205, 200);
        m.cpu_write(0x5206, 200);
        assert_eq!(m.ext_read(0x5205), Some(0x40)); // 40000 & 0xFF
        assert_eq!(m.ext_read(0x5206), Some(0x9C)); // 40000 >> 8
        m.cpu_write(0x5205, 0x12);
        m.cpu_write(0x5206, 0x34);
        assert_eq!(m.ext_read(0x5205), Some(0xA8)); // 0x05A8 low
        assert_eq!(m.ext_read(0x5206), Some(0x03));
    }

    #[test]
    fn exram_modes_control_cpu_access() {
        let mut m = board(8, 8);
        // Mode 2: general purpose RAM.
        m.cpu_write(0x5104, 2);
        m.cpu_write(0x5C10, 0xAB);
        assert_eq!(m.ext_read(0x5C10), Some(0xAB));
        // Mode 3: read-only.
        m.cpu_write(0x5104, 3);
        m.cpu_write(0x5C10, 0xCD);
        assert_eq!(m.ext_read(0x5C10), Some(0xAB));
        // Modes 0/1: write-only, and writes outside rendering store 0.
        m.cpu_write(0x5104, 0);
        assert_eq!(m.ext_read(0x5C10), None, "write-only in mode 0");
        m.cpu_write(0x5C20, 0x11);
        assert_eq!(m.exram[0x20], 0, "not rendering: the write stores 0");
        enter_frame(&mut m);
        m.cpu_write(0x5C20, 0x22);
        assert_eq!(m.exram[0x20], 0x22, "rendering: the write lands");
    }

    #[test]
    fn nametable_mapping_selects_ciram_exram_and_fill() {
        let mut m = board(8, 8);
        // CIRAM page 0 for every table: table 1 mirrors table 0.
        m.cpu_write(0x5105, 0x00);
        assert!(m.nt_write(0x2000, 0x99, &mut []));
        assert_eq!(m.nt_read(0x2400, &[]), Some(0x99));
        // Vertical arrangement: tables 2/3 point at CIRAM page 1.
        m.cpu_write(0x5105, 0x44);
        assert_eq!(m.nt_read(0x2400, &[]), Some(0x00));
        assert_eq!(m.nt_read(0x2800, &[]), Some(0x99));
        // ExRAM nametable after filling ExRAM in mode 2.
        m.cpu_write(0x5104, 2);
        m.cpu_write(0x5C00, 0x42);
        m.cpu_write(0x5105, 0xAA);
        m.cpu_write(0x5104, 0); // mode 0 makes ExRAM the nametable source
        assert_eq!(m.nt_read(0x2000, &[]), Some(0x42));
        assert_eq!(
            m.nt_read(0x2C00, &[]),
            Some(0x42),
            "every table points at ExRAM"
        );
        // Fill mode.
        m.cpu_write(0x5105, 0xFF);
        m.cpu_write(0x5106, 0x77);
        m.cpu_write(0x5107, 2);
        assert_eq!(m.nt_read(0x2000, &[]), Some(0x77));
        assert_eq!(
            m.nt_read(0x23C0, &[]),
            Some(0xAA),
            "$5107 replicated into all four indexes"
        );
    }

    #[test]
    fn scanline_irq_counts_lines_and_acks_on_5204() {
        let mut m = board(8, 8);
        m.cpu_write(0x5203, 2);
        m.cpu_write(0x5204, 0x80);
        enter_frame(&mut m);
        assert!(!m.irq_pending(), "line 0 detects without firing");
        next_scanline(&mut m); // line 1
        assert_eq!(m.scanline_counter, 1);
        assert!(!m.irq_pending());
        next_scanline(&mut m); // line 2 == $5203
        assert_eq!(m.scanline_counter, 2);
        assert!(m.irq_pending(), "the IRQ line is asserted");

        let status = m.ext_read(0x5204).unwrap();
        assert_eq!(status & 0xC0, 0xC0, "pending + in frame");
        assert!(!m.irq_pending(), "reading $5204 acknowledges the IRQ");
        assert_eq!(m.ext_read(0x5204).unwrap() & 0x80, 0);
    }

    #[test]
    fn scanline_irq_needs_enable_and_ignores_target_zero() {
        let mut m = board(8, 8);
        m.cpu_write(0x5204, 0x00); // disabled
        m.cpu_write(0x5203, 1);
        enter_frame(&mut m);
        next_scanline(&mut m);
        assert!(
            m.irq_pending,
            "the pending flag is raised regardless of enable"
        );
        assert!(!m.irq_pending(), "the line stays low while disabled");
        m.cpu_write(0x5204, 0x80);
        assert!(m.irq_pending(), "enabling a pending IRQ asserts the line");

        let mut m = board(8, 8);
        m.cpu_write(0x5203, 0);
        m.cpu_write(0x5204, 0x80);
        enter_frame(&mut m);
        for _ in 0..5 {
            next_scanline(&mut m);
        }
        assert!(!m.irq_pending(), "$5203 = 0 never compares");
    }

    #[test]
    fn nmi_vector_read_leaves_the_frame() {
        let mut m = board(8, 8);
        enter_frame(&mut m);
        assert!(m.ppu_in_frame);
        m.cpu_read(0xFFFA);
        assert!(!m.ppu_in_frame);
    }

    #[test]
    fn vertical_split_replaces_nametable_data_from_exram() {
        let mut m = board(8, 32);
        m.cpu_write(0x5104, 2);
        m.cpu_write(0x5C00, 0x00);
        m.exram.fill(0x00);
        // The split tile at the top of the screen (scroll 0, column 17) is $5C11.
        m.exram[0x11] = 0x42;
        m.exram[0x3C0] = 0xFF; // attribute table for the region: palette 3 everywhere
        m.cpu_write(0x5104, 0);
        m.cpu_write(0x5200, 0x80 | 0x10); // enabled, left side, delimiter column 16
        m.cpu_write(0x5201, 0x00); // no extra scroll
        m.cpu_write(0x5202, 2); // 4 KiB CHR bank 2 for the split region
        enter_frame(&mut m);
        // Feed one line's background nametable fetches; distinct addresses keep the detector quiet.
        // The first fetch inside the split region also checks the region's CHR bank.
        let mut seen_split = false;
        for n in 0..32u16 {
            let v = m.nt_read(0x2000 + n * 2, &[]).unwrap();
            if v == 0x42 {
                if !seen_split {
                    assert_eq!(
                        m.ppu_read(0x0000),
                        8,
                        "split-region CHR comes from 4 KiB bank $5202"
                    );
                }
                seen_split = true;
            }
        }
        assert!(seen_split, "the split region must read its tile from ExRAM");
    }

    #[test]
    fn extended_attributes_replace_the_palette_and_chr_bank() {
        let mut m = board(8, 32);
        m.cpu_write(0x5104, 1); // extended attributes
        m.cpu_write(0x5130, 0);
        m.exram[0] = 0b11_000010; // palette 3, 4 KiB CHR bank 2 (pages 8..11)
        enter_frame(&mut m);
        // A tile fetch at nametable offset 0: NT byte, then the attribute read is replaced.
        let tile = m.nt_read(0x2000, &[]).unwrap();
        assert_eq!(tile, 0, "the nametable byte still comes from CIRAM");
        let at = m.nt_read(0x23C0, &[]).unwrap();
        assert_eq!(at, 0xFF, "palette 3 replicated into all four indexes");
        assert_eq!(
            m.ppu_read(0x0000),
            8,
            "pattern comes from ExRAM's 4 KiB bank"
        );
        assert_eq!(m.ppu_read(0x0008), 8);
    }

    #[test]
    fn exgrafix_leaves_sprite_pattern_fetches_alone() {
        let mut m = board(8, 32);
        m.cpu_write(0x5104, 1);
        m.exram[0] = 0b00_000010; // palette 0, 4 KiB CHR bank 2
        enter_frame(&mut m);
        m.nt_read(0x2000, &[]); // arms the ExGrafix latch for this tile
        assert_eq!(
            m.ppu_read_sprite(0x0000),
            0,
            "a sprite fetch uses its normal bank, not the latched ExRAM bank"
        );
        // The background attribute + pattern fetches that follow still take ExRAM's bank.
        assert_eq!(m.nt_read(0x23C0, &[]), Some(0x00));
        assert_eq!(m.ppu_read(0x0000), 8);
    }

    #[test]
    fn audio_pulses_are_mixed_inverted_and_silenced_by_5015() {
        let mut m = board(8, 8);
        assert_eq!(m.audio_output(), 0.0);
        m.cpu_write(0x5015, 0x03);
        m.cpu_write(0x5000, 0xBF); // duty 2, halt, constant volume 15
        m.cpu_write(0x5004, 0xBF);
        m.cpu_write(0x5002, 0x10);
        m.cpu_write(0x5003, 0x08); // length 254, period $010
        m.cpu_write(0x5006, 0x10);
        m.cpu_write(0x5007, 0x08);
        let mut loudest = 0.0f32;
        for _ in 0..64 {
            m.cpu_clock();
            loudest = loudest.min(m.audio_output());
        }
        assert!(
            loudest < -10.0,
            "two full pulses read as -30 .. -10, got {loudest}"
        );
        assert_eq!(
            m.ext_read(0x5015),
            Some(0x03),
            "both length counters are active"
        );
        m.cpu_write(0x5015, 0x00);
        assert_eq!(
            m.audio_output(),
            0.0,
            "disabling both channels silences them"
        );
        assert_eq!(m.ext_read(0x5015), Some(0x00));
    }

    #[test]
    fn pcm_channel_ignores_zero_and_read_mode() {
        let mut m = board(8, 8);
        m.cpu_write(0x5011, 0xFF);
        assert!(
            (m.audio_output() + 15.0).abs() < 1e-3,
            "full-scale PCM is one pulse peak"
        );
        m.cpu_write(0x5011, 0x00);
        assert!(
            (m.audio_output() + 15.0).abs() < 1e-3,
            "a zero write does not move the DAC"
        );
        m.cpu_write(0x5010, 0x01); // read mode
        m.cpu_write(0x5011, 0x00);
        assert!((m.audio_output() + 15.0).abs() < 1e-3);
    }

    #[test]
    fn state_round_trip() {
        let mut a = board(8, 32);
        unlock_ram(&mut a);
        a.cpu_write(0x5104, 2);
        a.cpu_write(0x5C00, 0x5A);
        a.cpu_write(0x6000, 0x77);
        a.cpu_write(0x5203, 7);
        a.cpu_write(0x5204, 0x80);
        a.cpu_write(0x5205, 3);
        a.cpu_write(0x5206, 4);
        a.ppu_write(0x0000, 0x00);
        a.cpu_write(0x5015, 0x03);
        a.cpu_write(0x5000, 0xBF);
        a.cpu_write(0x5003, 0x08);
        a.ppu_register_write(0x2000, 0x20);
        enter_frame(&mut a);
        a.cpu_clock();

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();

        let mut b = board(8, 32);
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);

        for addr in (0x5000u32..=0xFFFF).step_by(0x11) {
            assert_eq!(
                a.cpu_read(addr as u16),
                b.cpu_read(addr as u16),
                "cpu {addr:04X}"
            );
        }
        for addr in (0x0000u16..=0x1FFF).step_by(0x41) {
            assert_eq!(a.chr_peek(addr), b.chr_peek(addr), "chr {addr:04X}");
        }
        assert_eq!(a.nt_read(0x2000, &[]), b.nt_read(0x2000, &[]));
        assert_eq!(a.irq_pending, b.irq_pending);
        assert_eq!(a.audio_output(), b.audio_output());
    }

    fn nvram_board(chr_is_ram: bool) -> Mmc5 {
        let chr = if chr_is_ram {
            vec![0; 8192]
        } else {
            vec![0x11; 8192]
        };
        Mmc5::new(crate::cart::RomData {
            header: crate::cart::Header {
                mapper: 5,
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
