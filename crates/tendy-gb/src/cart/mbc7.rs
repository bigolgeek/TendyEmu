//! MBC7 (type byte `$22`): the game's 256-byte 93LC56 EEPROM and a 2-axis accelerometer behind a
//! bank of registers at `$A000-$BFFF` (Pan Docs `MBC7.html`).
//!
//! The board is an MBC5 for ROM banking ("The ROM bank number goes here", "Same as for MBC5") and
//! has no battery RAM: `$A000-$BFFF` holds one register at a time, selected by address bits 4-7 with
//! bits 0-3 and 8-11 ignored, and only while RAM is enabled twice over — `$0A` to `$0000-$1FFF` and
//! `$40` to `$4000-$5FFF`. Otherwise reads are `$FF` and writes do nothing.
//!
//! | register | meaning |
//! |---|---|
//! | `Ax0x` | write `$55` erases the latched sample (back to `$8000`); reads `$FF` |
//! | `Ax1x` | write `$AA` latches a new sample; reads `$FF` |
//! | `Ax2x`/`Ax3x` | X, low then high byte: `$81D0` at rest, gravity is about `$70` |
//! | `Ax4x`/`Ax5x` | Y, low then high byte |
//! | `Ax6x`/`Ax7x` | `$00` and `$FF` |
//! | `Ax8x` | the EEPROM's pins: bit 0 DO, bit 1 DI, bit 6 CLK, bit 7 CS |
//!
//! The EEPROM is driven as the 93LC56 is: CS up, then ten bits of `1` + 2-bit opcode + 7-bit address,
//! then sixteen bits of data shifted out (`READ`) or in (`WRITE`/`WRAL`), MSB first. `EWEN` has to
//! precede every programming operation, as the page says.
//!
//! The accelerometer is fed from the front end: [`Mbc::set_tilt`] takes -1, 0 or +1 per axis while
//! the right mouse button is held, and each unit is one gravity of `$70` on top of the `$81D0` rest
//! value, with X positive to the left and Y positive to the top, which is the direction the page
//! documents.
//!
//! UNVERIFIED:
//!
//! * a programmed word is committed on the rising edge of its sixteenth data bit; a real part waits
//!   for CS to fall or another clock, and its DO reports "busy" until the write settles. This build
//!   is always ready, so the ready bit the games poll reads 1 at once;
//! * the EEPROM's words are persisted in the `.sav` low byte first (`word n` at bytes `2n`, `2n+1`);
//!   Pan Docs does not say which half of the word a byte of a dump is;
//! * the ROM bank is eight bits as on an MBC5, with bank 0 mapping bank 0 ("Bank 0 mapping needs
//!   confirmation");
//! * latching does not require the 1.2 ms of stopped HALT the page warns about, because there is no
//!   PHI model to stall.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// The only `$0147` byte that selects this board.
pub const CART_TYPE: u8 = 0x22;
/// The 93LC56 is 256 bytes, addressed as 128 sixteen-bit words.
const WORDS: usize = 128;
/// The accelerometer's rest value (`MBC7.html`).
const CENTRE: i32 = 0x81D0;
/// One gravity, "roughly `$70`".
const GRAVITY: i32 = 0x70;

/// What the EEPROM is doing with the bits being clocked in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Idle,
    Read,
    Write,
    Wral,
}

/// The 93LC56: 128 sixteen-bit words kept as 256 bytes (little-endian, so a dump is byte for byte),
/// a chip-select/clock/data-in latch and the bit counter that decodes the ten-bit instruction stream.
#[derive(Clone)]
struct Eeprom {
    bytes: [u8; WORDS * 2],
    cs: bool,
    clk: bool,
    di: bool,
    out: bool,
    /// Bits shifted in since CS rose; the first ten are the instruction.
    bits: u16,
    /// Instruction or data being shifted.
    shift: u16,
    op: Op,
    addr: usize,
    /// Data being shifted out by `READ`.
    data: u16,
    /// Set by `EWEN`, cleared by `EWDS`; every programming operation needs it.
    write_enabled: bool,
}

impl Eeprom {
    fn new() -> Eeprom {
        Eeprom {
            // Erased EEPROM reads $FFFF per word.
            bytes: [0xFF; WORDS * 2],
            cs: false,
            clk: false,
            di: false,
            out: false,
            bits: 0,
            shift: 0,
            op: Op::Idle,
            addr: 0,
            data: 0,
            write_enabled: false,
        }
    }

    fn word(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.bytes[2 * at], self.bytes[2 * at + 1]])
    }

    fn set_word(&mut self, at: usize, word: u16) {
        self.bytes[2 * at..2 * at + 2].copy_from_slice(&word.to_le_bytes());
    }

    /// A write to the pin register: CS, CLK and DI edges are all that matter.
    fn pins(&mut self, value: u8) {
        let cs = value & 0x80 != 0;
        let clk = value & 0x40 != 0;
        let di = value & 0x02 != 0;
        if cs != self.cs {
            // Either edge of CS abandons whatever instruction was in progress.
            self.bits = 0;
            self.shift = 0;
            self.op = Op::Idle;
            self.out = false;
        } else if cs && clk && !self.clk {
            self.clock(di);
        }
        self.cs = cs;
        self.clk = clk;
        self.di = di;
    }

    fn clock(&mut self, di: bool) {
        if self.bits < 10 {
            self.shift = (self.shift << 1) | di as u16;
            self.bits += 1;
            if self.bits == 10 {
                self.decode();
            }
            return;
        }
        match self.op {
            Op::Read => {
                // The first bit is already on the pin from `decode`; each further clock shifts the
                // register and puts the next one there, so a read between two clocks sees the bit
                // the last clock produced.
                self.data <<= 1;
                self.out = self.data & 0x8000 != 0;
            }
            Op::Write | Op::Wral => {
                self.shift = (self.shift << 1) | di as u16;
                if self.bits == 25 {
                    let word = self.shift;
                    if self.write_enabled {
                        match self.op {
                            Op::Write => self.set_word(self.addr, word),
                            _ => {
                                for at in 0..WORDS {
                                    self.set_word(at, word);
                                }
                            }
                        }
                    }
                    self.op = Op::Idle;
                    self.bits = 0;
                    self.shift = 0;
                    return;
                }
            }
            Op::Idle => {}
        }
        self.bits += 1;
    }

    /// Ten bits received: `1`, a two-bit opcode, and seven address bits.
    fn decode(&mut self) {
        let instruction = self.shift & 0x03FF;
        let opcode = (instruction >> 7) & 0x03;
        let sub = (instruction >> 5) & 0x03;
        self.addr = (instruction & 0x7F) as usize;
        match opcode {
            // `10xAAAAAAA`: read a word.
            0b10 => {
                self.op = Op::Read;
                self.data = self.word(self.addr);
                self.out = self.data & 0x8000 != 0;
            }
            // `01xAAAAAAA`: write a word.
            0b01 => {
                self.op = Op::Write;
                self.shift = 0;
            }
            // `11xAAAAAAA`: fill one address with `$FFFF`.
            0b11 => {
                if self.write_enabled {
                    self.set_word(self.addr, 0xFFFF);
                }
                self.op = Op::Idle;
            }
            // `00` then two more bits: the four housekeeping commands.
            _ => match sub {
                0b11 => self.write_enabled = true,
                0b00 => self.write_enabled = false,
                0b10 => {
                    if self.write_enabled {
                        self.bytes = [0xFF; WORDS * 2];
                    }
                    self.op = Op::Idle;
                }
                _ => {
                    self.op = Op::Wral;
                    self.shift = 0;
                }
            },
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.bytes);
        w.bool(self.cs);
        w.bool(self.clk);
        w.bool(self.di);
        w.bool(self.out);
        w.u16(self.bits);
        w.u16(self.shift);
        w.u8(match self.op {
            Op::Idle => 0,
            Op::Read => 1,
            Op::Write => 2,
            Op::Wral => 3,
        });
        w.u8(self.addr as u8);
        w.u16(self.data);
        w.bool(self.write_enabled);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.bytes)?;
        let cs = r.bool()?;
        let clk = r.bool()?;
        let di = r.bool()?;
        let out = r.bool()?;
        let bits = r.u16()?;
        let shift = r.u16()?;
        let op = r.u8()?;
        let addr = r.u8()? as usize;
        let data = r.u16()?;
        let write_enabled = r.bool()?;
        if bits > 26 || addr >= WORDS {
            return Err(StateError::BadValue);
        }
        self.op = match op {
            0 => Op::Idle,
            1 => Op::Read,
            2 => Op::Write,
            3 => Op::Wral,
            _ => return Err(StateError::BadValue),
        };
        self.cs = cs;
        self.clk = clk;
        self.di = di;
        self.out = out;
        self.bits = bits;
        self.shift = shift;
        self.addr = addr;
        self.data = data;
        self.write_enabled = write_enabled;
        Ok(())
    }
}

/// MBC7.
#[derive(Clone)]
pub struct Mbc7 {
    ram_enable: bool,
    /// "The ROM bank number goes here", MBC5-style: eight bits, bank 0 is bank 0.
    rom_bank: u8,
    /// `$4000-$5FFF`: writing `$40` enables the registers, anything else disables them.
    registers_enable: bool,
    /// The latched sample, `$8000` until the first latch.
    x: u16,
    y: u16,
    /// "You cannot re-latch the accelerometer value without first erasing it."
    latched: bool,
    /// The front end's tilt, -1, 0 or +1 per axis: X positive is left, Y positive is top.
    tilt_x: i8,
    tilt_y: i8,
    eeprom: Eeprom,
    rom_size: usize,
}

impl Mbc7 {
    pub fn new(rom_banks: usize) -> Mbc7 {
        Mbc7 {
            ram_enable: false,
            rom_bank: 1,
            registers_enable: false,
            x: 0x8000,
            y: 0x8000,
            latched: false,
            tilt_x: 0,
            tilt_y: 0,
            eeprom: Eeprom::new(),
            rom_size: rom_banks.max(1) * 0x4000,
        }
    }

    /// The register index: bits 4-7 of the address, with 0-3 and 8-11 ignored.
    fn register(addr: u16) -> u8 {
        ((addr >> 4) & 0x0F) as u8
    }

    fn axis(centre: i32, tilt: i8) -> u16 {
        (centre + tilt as i32 * GRAVITY).clamp(0, 0xFFFF) as u16
    }

    /// Latches a new sample; only erasing (`Ax0x`) clears the latch again.
    fn latch(&mut self) {
        self.x = Mbc7::axis(CENTRE, self.tilt_x);
        self.y = Mbc7::axis(CENTRE, self.tilt_y);
        self.latched = true;
    }
}

impl Mbc for Mbc7 {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            return (addr & 0x3FFF) as usize;
        }
        let offset = (((self.rom_bank as usize) & 0xFF) << 14) | (addr as usize & 0x3FFF);
        if offset < self.rom_size {
            offset
        } else {
            offset % self.rom_size
        }
    }

    fn ram_access(&self, _addr: u16) -> RamAccess {
        // The board has no battery RAM: $A000-$BFFF is registers only, and they are read and written
        // through `read_ram`/`write_ram`.
        RamAccess::Off
    }

    fn read_ram(&self, addr: u16) -> Option<u8> {
        if !self.ram_enable || !self.registers_enable || addr >= 0xB000 {
            return Some(0xFF);
        }
        Some(match Mbc7::register(addr) {
            // The latch and erase registers read $FF.
            0x0 | 0x1 => 0xFF,
            0x2 => self.x as u8,
            0x3 => (self.x >> 8) as u8,
            0x4 => self.y as u8,
            0x5 => (self.y >> 8) as u8,
            0x6 => 0x00,
            0x7 => 0xFF,
            // DO in bit 0, DI in bit 1, CLK in bit 6 and CS in bit 7, as written.
            0x8 => {
                (self.eeprom.cs as u8) << 7
                    | (self.eeprom.clk as u8) << 6
                    | (self.eeprom.di as u8) << 1
                    | self.eeprom.out as u8
            }
            _ => 0xFF,
        })
    }

    fn write_ram(&mut self, addr: u16, value: u8) -> bool {
        if !self.ram_enable || !self.registers_enable || addr >= 0xB000 {
            // "otherwise reads read $FF and writes do nothing"
            return true;
        }
        match Mbc7::register(addr) {
            // Erase the latched sample, back to the documented power-on value.
            0x0 => {
                if value == 0x55 {
                    self.x = 0x8000;
                    self.y = 0x8000;
                    self.latched = false;
                }
            }
            // Latch the accelerometer, if the previous sample was erased first.
            0x1 => {
                if value == 0xAA && !self.latched {
                    self.latch();
                }
            }
            0x8 => self.eeprom.pins(value),
            _ => {}
        }
        true
    }

    fn set_tilt(&mut self, x: i8, y: i8) {
        self.tilt_x = x.clamp(-1, 1);
        self.tilt_y = y.clamp(-1, 1);
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enable = value & 0x0F == 0x0A,
            0x2000..=0x3FFF => self.rom_bank = value,
            // "Writing $40 to this region enables access to the RAM registers. Writing any other
            // value appears to disable access."
            0x4000..=0x5FFF => self.registers_enable = value == 0x40,
            _ => {}
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "mbc7"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn saved_data(&self) -> Option<&[u8]> {
        Some(&self.eeprom.bytes)
    }

    fn load_saved_data(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() != WORDS * 2 {
            return Err(format!(
                "EEPROM image is {} bytes, need {}",
                bytes.len(),
                WORDS * 2
            ));
        }
        self.eeprom.bytes.copy_from_slice(bytes);
        Ok(())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.ram_enable);
        w.u8(self.rom_bank);
        w.bool(self.registers_enable);
        w.u16(self.x);
        w.u16(self.y);
        w.bool(self.latched);
        w.u8(self.tilt_x as u8);
        w.u8(self.tilt_y as u8);
        self.eeprom.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ram_enable = r.bool()?;
        let rom_bank = r.u8()?;
        let registers_enable = r.bool()?;
        let x = r.u16()?;
        let y = r.u16()?;
        let latched = r.bool()?;
        let tilt_x = r.u8()? as i8;
        let tilt_y = r.u8()? as i8;
        self.eeprom.load_state(r)?;
        if !(-1..=1).contains(&tilt_x) || !(-1..=1).contains(&tilt_y) {
            return Err(StateError::BadValue);
        }
        self.ram_enable = ram_enable;
        self.rom_bank = rom_bank;
        self.registers_enable = registers_enable;
        self.x = x;
        self.y = y;
        self.latched = latched;
        self.tilt_x = tilt_x;
        self.tilt_y = tilt_y;
        Ok(())
    }
}
