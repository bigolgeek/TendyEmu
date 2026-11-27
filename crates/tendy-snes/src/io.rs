//! `$4200-$421F`: the S-CPU's own ports, plus the WRAM port's address counter.
//!
//! Owned by the CPU lane: every field is either an interrupt latch the CPU samples, a CPU-cycle
//! count, or a counter the CPU's B-bus writes move. Sources: fullsnes "SNES PPU Interrupts" (the
//! three interrupt registers and `HVBJOY`), "SNES Maths Multiply/Divide" (`$4202-$4206`,
//! `$4214-$4217`), "SNES Memory Work RAM Access" (`$2180-$2183`) and "SNES Controllers I/O Ports"
//! (`$4201`).
//!
//! `$4212` and `$4218-$421F` are *not* decoded here: `HVBJOY` needs the PPU's line counter and the
//! auto-read's busy flag, and the two joypad words belong to `Joypad`, so `Bus` serves both
//! (`bus.rs`), which is also why `CpuIo::read` never sees them.

use crate::WRAM_SIZE;
use crate::addr::Addr;
use crate::state::{StateError, StateReader, StateWriter};

/// The `$4202` x `$4203` multiply's latency: eight CPU cycles (fullsnes "SNES Maths Multiply/Divide":
/// "then set WRMPYB, wait 8 clk cycles"), counted here in master clocks at the 3.58 MHz rate.
const MULTIPLY_TICKS: u8 = 8 * 6;
/// The `$4204`/`$4206` divide's latency: sixteen CPU cycles.
const DIVIDE_TICKS: u8 = 16 * 6;

/// The S-CPU's I/O block.
#[derive(Clone)]
pub struct CpuIo {
    /// `$4200`: NMI enable (7), H/V IRQ mode (5-4), joypad auto-read (0).
    pub nmitimen: u8,
    /// `$4207`/`$4208`, 9 bits used.
    pub htime: u16,
    /// `$4209`/`$420A`, 9 bits used.
    pub vtime: u16,
    /// `$420D`: bit 0 FastROM for `$80-$FF`.
    pub memsel: u8,
    /// `$4210` bit 7.
    pub nmi_flag: bool,
    /// Set by a `$4200` write that takes the NMI input (`NMITIMEN.7 AND RDNMI.7`) from 0 to 1,
    /// i.e. an edge raised inside an instruction's own final cycle. `Cpu::step` consumes it at the
    /// next boundary and holds the dispatch one instruction. Not a §9.2 field: it cannot outlive
    /// the instruction after the store (`Cpu::step` clears it), so a save never needs it.
    pub nmi_write_edge: bool,
    /// `$4211` bit 7.
    pub irq_flag: bool,
    /// `$4212` bit 0.
    pub auto_joy: bool,
    /// `$4212` bit 6.
    pub hblank: bool,
    pub mpya: u8,
    pub mpyb: u8,
    pub mpy_result: u16,
    pub divl: u16,
    pub divb: u8,
    pub div_result: u16,
    pub div_remainder: u16,
    /// Master clocks left before the result is readable: 8 and 16 CPU cycles.
    pub mpy_wait: u8,
    pub div_wait: u8,
    /// `$4201`, joypad port I/O; bit 7 is the H/V latch enable.
    pub wrio: u8,
    /// `$4016` bit 0.
    pub joy_strobe: u8,
    /// `$2180-$2183`: the WRAM port's 17-bit byte address.
    pub wmadd: u32,
}

impl Default for CpuIo {
    /// Power-on values from fullsnes's I/O map, which prints each port's reset value beside it:
    /// `4201h - WRIO - Joypad Programmable I/O Port (Open-Collector Output)  FFh`.
    ///
    /// `WRIO.7` is the H/V latch enable, and fullsnes "2137h - SLHV" says the software latch, the
    /// `WRIO` bit-7 1-to-0 edge and the light-gun edge "are working only if WRIO.Bit7 is (or was)
    /// set". With `FFh` it is set at power-on, which is what a cartridge that reads `$2137`
    /// without ever writing `$4201` relies on — Jimmy Houston's Bass Tournament's v-blank handler
    /// does exactly that, and with `WRIO` at zero the latch never happens, so its `$213D` read
    /// returns the power-on zero and the handler always takes its "wrong scanline" exit.
    ///
    /// The other documented reset values (`4202h`-`4206h` `FFh`, `4207h`/`4209h` `FFh`,
    /// `4208h`/`420Ah` `01h`) are left at zero: no measurement here depends on them, and moving
    /// `VTIME`/`HTIME` changes where an H/V IRQ fires, which is a separate experiment with its own
    /// population check.
    fn default() -> CpuIo {
        CpuIo {
            nmitimen: 0,
            htime: 0,
            vtime: 0,
            memsel: 0,
            nmi_flag: false,
            nmi_write_edge: false,
            irq_flag: false,
            auto_joy: false,
            hblank: false,
            mpya: 0,
            mpyb: 0,
            mpy_result: 0,
            divl: 0,
            divb: 0,
            div_result: 0,
            div_remainder: 0,
            mpy_wait: 0,
            div_wait: 0,
            wrio: 0xFF,
            joy_strobe: 0,
            wmadd: 0,
        }
    }
}

impl CpuIo {
    /// `$4200-$421F` as an `Addr` inside a system-area bank; `mdr` is what an unowned or
    /// write-only port answers.
    ///
    /// `$4216`/`$4217` are the multiply product *and* the divide remainder (fullsnes: "Unsigned
    /// Division Remainder / Multiply Product"), which is one register pair with two names.
    pub fn read(&mut self, addr: Addr, mdr: u8) -> u8 {
        match addr.offset() {
            // RDNMI: the flag, the CPU version nibble (v1 pins 2; §4.6's UNVERIFIED row), and bits
            // 6-4 open bus — fullsnes "SNES Unpredictable Things": "4210h 70h RDNMI Bit6-4 are open
            // bus".
            0x4210 => {
                let value = (u8::from(self.nmi_flag) << 7) | (mdr & 0x70) | 0x02;
                self.nmi_flag = false;
                value
            }
            // TIMEUP: bit 7 is the flag and bits 6-0 are open bus. v1 always clears on read, which
            // is the documented exception it does not model (§4.6's last row).
            0x4211 => {
                let value = (u8::from(self.irq_flag) << 7) | (mdr & 0x7F);
                self.irq_flag = false;
                value
            }
            // RDIO: the state of the programmable I/O port.
            0x4213 => self.wrio,
            0x4214 => self.div_result as u8,
            0x4215 => (self.div_result >> 8) as u8,
            0x4216 => self.mpy_result as u8,
            0x4217 => (self.mpy_result >> 8) as u8,
            _ => mdr,
        }
    }

    /// `$4200-$421F`, write side. A disabled interrupt mode acknowledges the flag it no longer
    /// drives (fullsnes `$4200`: "Disabling IRQs (via bit4-5) does additionally acknowledge IRQs").
    pub fn write(&mut self, addr: Addr, value: u8) {
        match addr.offset() {
            0x4200 => {
                let was_irq = self.nmitimen & 0x30 != 0;
                // The NMI input is `NMITIMEN.7 AND RDNMI.7`, and the CPU's internal latch is set
                // on its 0-to-1 (fullsnes "SNES PPU Interrupts"). A store raises that edge on its
                // own final cycle, after the interrupt check that precedes the cycle (Anomie,
                // S-CPU §Interrupts: "The actual check occurs just before the final CPU cycle of
                // the instruction"), so mark it for `Cpu::step` to hold one instruction. `$4200`
                // is not on the B-bus, so no other bus master can write it.
                // UNVERIFIED: an RMW store or a block move that writes `$4200` mid-instruction is
                // treated as a final-cycle store too; fullsnes gives no cycle for either, and the
                // test that would settle it is one of those two forms against a real cart.
                if self.nmi_flag && self.nmitimen & 0x80 == 0 && value & 0x80 != 0 {
                    self.nmi_write_edge = true;
                }
                self.nmitimen = value;
                if was_irq && value & 0x30 == 0 {
                    self.irq_flag = false;
                }
            }
            0x4201 => self.wrio = value,
            0x4202 => self.mpya = value,
            0x4203 => {
                self.mpyb = value;
                self.mpy_wait = MULTIPLY_TICKS;
            }
            0x4204 => self.divl = (self.divl & 0xFF00) | u16::from(value),
            0x4205 => self.divl = (self.divl & 0x00FF) | (u16::from(value) << 8),
            0x4206 => {
                self.divb = value;
                self.div_wait = DIVIDE_TICKS;
            }
            0x4207 => self.htime = (self.htime & 0x0100) | u16::from(value),
            0x4208 => self.htime = (self.htime & 0x00FF) | (u16::from(value & 0x01) << 8),
            0x4209 => self.vtime = (self.vtime & 0x0100) | u16::from(value),
            0x420A => self.vtime = (self.vtime & 0x00FF) | (u16::from(value & 0x01) << 8),
            0x420D => self.memsel = value,
            _ => {}
        }
    }

    /// Counts the multiply and divide results down; called once per `Bus::tick`.
    pub fn tick(&mut self, ticks: u32) {
        let mut left = ticks;
        if self.mpy_wait > 0 {
            let wait = u32::from(self.mpy_wait);
            if left >= wait {
                left -= wait;
                self.mpy_wait = 0;
                self.finish_multiply();
            } else {
                self.mpy_wait -= left as u8;
                left = 0;
            }
        }
        if self.div_wait > 0 && left > 0 {
            let wait = u32::from(self.div_wait);
            if left >= wait {
                self.div_wait = 0;
                self.finish_divide();
            } else {
                self.div_wait -= left as u8;
            }
        }
    }

    /// The unsigned `A x B` product, plus the hardware's side effect on the divide result
    /// (fullsnes: "the hardware does additionally set RDDIVL=WRMPYB, and RDDIVH=00h").
    fn finish_multiply(&mut self) {
        self.mpy_result = u16::from(self.mpya) * u16::from(self.mpyb);
        self.div_result = u16::from(self.mpyb);
        self.div_remainder = 0;
    }

    /// `dividend / divisor`, with division by zero answering `FFFFh` and the dividend as the
    /// remainder (fullsnes `$4206`: "Division by zero returns Result=FFFFh, Remainder=Dividend").
    ///
    /// A 16-bit dividend over an 8-bit divisor cannot overflow the 16-bit quotient, so the only
    /// special case is the zero divisor.
    fn finish_divide(&mut self) {
        let divisor = u16::from(self.divb);
        let (result, remainder) = match self.divl.checked_div(divisor) {
            Some(quotient) => (quotient, self.divl % divisor),
            // fullsnes `$4206`: "Division by zero returns Result=FFFFh, Remainder=Dividend".
            None => (0xFFFF, self.divl),
        };
        self.div_result = result;
        self.div_remainder = remainder;
        // `$4216`/`$4217` is one register pair with two names: the multiply product and the
        // divide remainder (fullsnes: "Unsigned Division Remainder / Multiply Product").
        self.mpy_result = remainder;
    }

    /// `$2181`-`$2183`: the low, middle or high byte of `wmadd`.
    pub fn write_wram_port(&mut self, offset: u16, value: u8) {
        let shift = match offset {
            0x2181 => 0,
            0x2182 => 8,
            0x2183 => 16,
            _ => return,
        };
        self.wmadd = (self.wmadd & !(0xFF << shift)) | (u32::from(value) << shift);
        self.wmadd &= 0x1_FFFF;
    }

    /// `$2180`: `wram[wmadd]`, then `wmadd += 1` (wrapping inside 17 bits).
    pub fn read_wram_data(&mut self, wram: &mut [u8; WRAM_SIZE]) -> u8 {
        let value = wram[(self.wmadd as usize) & (WRAM_SIZE - 1)];
        self.wmadd = (self.wmadd + 1) & 0x1_FFFF;
        value
    }

    /// `$2180`: `wram[wmadd] = value`, then `wmadd += 1`.
    pub fn write_wram_data(&mut self, wram: &mut [u8; WRAM_SIZE], value: u8) {
        wram[(self.wmadd as usize) & (WRAM_SIZE - 1)] = value;
        self.wmadd = (self.wmadd + 1) & 0x1_FFFF;
    }

    /// §9.2's `CpuIo` rows, in the design's order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.nmitimen);
        w.u8(self.memsel);
        w.u8(self.wrio);
        w.u8(self.joy_strobe);
        w.u16(self.htime);
        w.u16(self.vtime);
        w.u16(self.divl);
        w.bool(self.nmi_flag);
        w.bool(self.irq_flag);
        w.bool(self.auto_joy);
        w.bool(self.hblank);
        w.u8(self.mpya);
        w.u8(self.mpyb);
        w.u16(self.mpy_result);
        w.u8(self.mpy_wait);
        w.u8(self.divb);
        w.u16(self.div_result);
        w.u16(self.div_remainder);
        w.u8(self.div_wait);
        w.u32(self.wmadd);
    }

    /// The inverse of [`CpuIo::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        self.nmitimen = r.u8()?;
        self.memsel = r.u8()?;
        self.wrio = r.u8()?;
        self.joy_strobe = r.u8()?;
        self.htime = r.u16()?;
        self.vtime = r.u16()?;
        self.divl = r.u16()?;
        self.nmi_flag = r.bool()?;
        self.irq_flag = r.bool()?;
        self.auto_joy = r.bool()?;
        self.hblank = r.bool()?;
        self.mpya = r.u8()?;
        self.mpyb = r.u8()?;
        self.mpy_result = r.u16()?;
        self.mpy_wait = r.u8()?;
        self.divb = r.u8()?;
        self.div_result = r.u16()?;
        self.div_remainder = r.u16()?;
        self.div_wait = r.u8()?;
        self.wmadd = r.u32()?;
        // Not a §9.2 field (see [`CpuIo::nmi_write_edge`]): a restored machine starts at an
        // instruction boundary, where no store's edge is outstanding.
        self.nmi_write_edge = false;
        Ok(())
    }
}
