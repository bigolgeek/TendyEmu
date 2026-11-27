//! The 256 base opcodes and the 256 CB-prefixed opcodes, with the machine-cycle count each one
//! spends (`https://gbdev.io/gb-opcodes/optables/`, rule 3).
//!
//! The tables below are the *oracle*: `Cpu::execute` never reads them for timing — it ticks where
//! the work happens — and `cpu::official_opcodes_cycles` / `cpu::cb_opcodes_cycles` compare the
//! count `Cpu::step` actually ticked against the table, so a missing or doubled internal cycle
//! fails a test instead of quietly shifting every later frame.
//!
//! Cycle counts are machine cycles (4 T-cycles). `OPCODE_CYCLES` holds the taken/plain count;
//! `official_cycles(op, taken)` supplies the shorter count for the four conditional `RET`/`JP`/
//! `CALL` groups and `JR cc`. Illegal opcodes are `0`.

use super::alu;
use super::{Cpu, Flags};
use crate::bus::Bus;

/// Machine cycles per base opcode (taken branch for conditionals). `0` = illegal on the SM83.
pub const OPCODE_CYCLES: [u8; 256] = [
    1, 3, 2, 2, 1, 1, 2, 1, 5, 2, 2, 2, 1, 1, 2, 1, // 00
    2, 3, 2, 2, 1, 1, 2, 1, 3, 2, 2, 2, 1, 1, 2, 1, // 10
    3, 3, 2, 2, 1, 1, 2, 1, 3, 2, 2, 2, 1, 1, 2, 1, // 20
    3, 3, 2, 2, 3, 3, 3, 1, 3, 2, 2, 2, 1, 1, 2, 1, // 30
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // 40
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // 50
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // 60
    2, 2, 2, 2, 2, 2, 1, 2, 1, 1, 1, 1, 1, 1, 2, 1, // 70
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // 80
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // 90
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // A0
    1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1, 2, 1, // B0
    5, 3, 4, 4, 6, 4, 2, 4, 5, 4, 4, 1, 6, 6, 2, 4, // C0
    5, 3, 4, 0, 6, 4, 2, 4, 5, 4, 4, 0, 6, 0, 2, 4, // D0
    3, 3, 2, 0, 0, 4, 2, 4, 4, 1, 4, 0, 0, 0, 2, 4, // E0
    3, 3, 2, 1, 0, 4, 2, 4, 3, 2, 4, 1, 0, 0, 2, 4, // F0
];

/// Machine cycles per CB-prefixed opcode, prefix included.
pub const CB_CYCLES: [u8; 256] = [
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 00
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 10
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 20
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 30
    2, 2, 2, 2, 2, 2, 3, 2, 2, 2, 2, 2, 2, 2, 3, 2, // 40
    2, 2, 2, 2, 2, 2, 3, 2, 2, 2, 2, 2, 2, 2, 3, 2, // 50
    2, 2, 2, 2, 2, 2, 3, 2, 2, 2, 2, 2, 2, 2, 3, 2, // 60
    2, 2, 2, 2, 2, 2, 3, 2, 2, 2, 2, 2, 2, 2, 3, 2, // 70
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 80
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // 90
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // A0
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // B0
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // C0
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // D0
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // E0
    2, 2, 2, 2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 4, 2, // F0
];

/// Opcodes with no defined operation: the SM83 locks up on them (SPEC R1).
pub fn is_illegal(op: u8) -> bool {
    matches!(
        op,
        0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD
    )
}

/// The table's count for `op`, on the branch `taken` selects (conditionals only).
pub fn official_cycles(op: u8, taken: bool) -> u8 {
    let base = OPCODE_CYCLES[op as usize];
    if taken {
        return base;
    }
    match op {
        0x20 | 0x28 | 0x30 | 0x38 => 2, // JR cc
        0xC0 | 0xC8 | 0xD0 | 0xD8 => 2, // RET cc
        0xC2 | 0xCA | 0xD2 | 0xDA => 3, // JP cc
        0xC4 | 0xCC | 0xD4 | 0xDC => 3, // CALL cc
        _ => base,
    }
}

impl Cpu {
    /// Reads one byte and advances `PC` (unless the HALT bug suppresses that advance).
    pub(super) fn fetch_opcode(&mut self, bus: &mut Bus) -> u8 {
        let at = self.pc;
        let op = bus.read(at);
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.pc = at.wrapping_add(1);
        }
        op
    }

    fn fetch8(&mut self, bus: &mut Bus) -> u8 {
        let value = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn fetch16(&mut self, bus: &mut Bus) -> u16 {
        let low = self.fetch8(bus);
        let high = self.fetch8(bus);
        u16::from_le_bytes([low, high])
    }

    /// Register index of the `r` field: B C D E H L (HL) A.
    fn reg(&mut self, bus: &mut Bus, index: u8) -> u8 {
        match index {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => {
                let addr = self.hl();
                bus.read(addr)
            }
            _ => self.a,
        }
    }

    fn set_reg(&mut self, bus: &mut Bus, index: u8, value: u8) {
        match index {
            0 => self.b = value,
            1 => self.c = value,
            2 => self.d = value,
            3 => self.e = value,
            4 => self.h = value,
            5 => self.l = value,
            6 => {
                let addr = self.hl();
                bus.write(addr, value);
            }
            _ => self.a = value,
        }
    }

    /// Register pair index of the `p` field: BC DE HL SP.
    fn pair(&self, index: u8) -> u16 {
        match index {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            _ => self.sp,
        }
    }

    fn set_pair(&mut self, index: u8, value: u16) {
        match index {
            0 => self.set_bc(value),
            1 => self.set_de(value),
            2 => self.set_hl(value),
            _ => self.sp = value,
        }
    }

    /// Condition index of the `cc` field: NZ Z NC C.
    fn cond(&self, index: u8) -> bool {
        match index {
            0 => !self.f.z(),
            1 => self.f.z(),
            2 => !self.f.c(),
            _ => self.f.c(),
        }
    }

    /// One internal M-cycle, then two writes: `PUSH`.
    fn push(&mut self, bus: &mut Bus, value: u16) {
        bus.tick();
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, (value >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, value as u8);
    }

    fn pop(&mut self, bus: &mut Bus) -> u16 {
        let low = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        let high = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_le_bytes([low, high])
    }

    /// The eight ALU operations the `$80-$BF` block and the `$C6`/`$CE`/… immediates share.
    fn alu(&mut self, kind: u8, operand: u8) {
        let (result, flags) = match kind {
            0 => alu::add8(self.a, operand, false),
            1 => alu::add8(self.a, operand, self.f.c()),
            2 => alu::sub8(self.a, operand, false),
            3 => alu::sub8(self.a, operand, self.f.c()),
            4 => alu::and8(self.a, operand),
            5 => alu::xor8(self.a, operand),
            6 => alu::or8(self.a, operand),
            _ => alu::sub8(self.a, operand, false),
        };
        if kind != 7 {
            self.a = result;
        }
        self.f = Flags(flags);
    }

    /// Executes the opcode already fetched. Ticks once per M-cycle it spends, through `Bus::read`,
    /// `Bus::write` and `Bus::tick` (§2 rules 1-2).
    pub(crate) fn execute(&mut self, bus: &mut Bus, op: u8) {
        match op {
            0x00 => {} // NOP
            0x01 | 0x11 | 0x21 | 0x31 => {
                let value = self.fetch16(bus);
                self.set_pair((op >> 4) & 3, value);
            }
            0x02 => {
                let addr = self.bc();
                bus.write(addr, self.a);
            }
            0x12 => {
                let addr = self.de();
                bus.write(addr, self.a);
            }
            0x22 => {
                let addr = self.hl();
                bus.write(addr, self.a);
                self.set_hl(addr.wrapping_add(1));
            }
            0x32 => {
                let addr = self.hl();
                bus.write(addr, self.a);
                self.set_hl(addr.wrapping_sub(1));
            }
            0x03 | 0x13 | 0x23 | 0x33 => {
                let index = (op >> 4) & 3;
                let value = self.pair(index).wrapping_add(1);
                self.set_pair(index, value);
                bus.tick();
            }
            0x0B | 0x1B | 0x2B | 0x3B => {
                let index = (op >> 4) & 3;
                let value = self.pair(index).wrapping_sub(1);
                self.set_pair(index, value);
                bus.tick();
            }
            0x09 | 0x19 | 0x29 | 0x39 => {
                let addend = self.pair((op >> 4) & 3);
                let (value, flags) = alu::add_hl(self.hl(), addend, self.f.0);
                self.set_hl(value);
                self.f = Flags(flags);
                bus.tick();
            }
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let index = (op >> 3) & 7;
                let value = self.reg(bus, index);
                let (result, flags) = alu::inc8(value, self.f.0);
                self.set_reg(bus, index, result);
                self.f = Flags(flags);
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let index = (op >> 3) & 7;
                let value = self.reg(bus, index);
                let (result, flags) = alu::dec8(value, self.f.0);
                self.set_reg(bus, index, result);
                self.f = Flags(flags);
            }
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let index = (op >> 3) & 7;
                let value = self.fetch8(bus);
                self.set_reg(bus, index, value);
            }
            0x07 => {
                let (result, flags) = alu::rlca(self.a);
                self.a = result;
                self.f = Flags(flags);
            }
            0x0F => {
                let (result, flags) = alu::rrca(self.a);
                self.a = result;
                self.f = Flags(flags);
            }
            0x17 => {
                let (result, flags) = alu::rla(self.a, self.f.0);
                self.a = result;
                self.f = Flags(flags);
            }
            0x1F => {
                let (result, flags) = alu::rra(self.a, self.f.0);
                self.a = result;
                self.f = Flags(flags);
            }
            0x08 => {
                let addr = self.fetch16(bus);
                let sp = self.sp;
                bus.write(addr, sp as u8);
                bus.write(addr.wrapping_add(1), (sp >> 8) as u8);
            }
            0x0A => {
                let addr = self.bc();
                self.a = bus.read(addr);
            }
            0x1A => {
                let addr = self.de();
                self.a = bus.read(addr);
            }
            0x2A => {
                let addr = self.hl();
                self.a = bus.read(addr);
                self.set_hl(addr.wrapping_add(1));
            }
            0x3A => {
                let addr = self.hl();
                self.a = bus.read(addr);
                self.set_hl(addr.wrapping_sub(1));
            }
            0x10 => {
                // STOP: one more M-cycle reads the operand byte (which the SM83 ignores). On a CGB
                // with KEY1 armed the STOP is the speed switch and the CPU waits out the pause in
                // `Cgb`; a refused switch leaves the CPU running; every other machine halts.
                let _padding = self.fetch8(bus);
                if bus.stop() == crate::cgb::Stop::Halt {
                    self.stopped = true;
                }
            }
            0x18 => {
                let offset = self.fetch8(bus) as i8;
                bus.tick();
                self.pc = self.pc.wrapping_add(offset as i16 as u16);
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                let offset = self.fetch8(bus) as i8;
                if self.cond((op >> 3) & 3) {
                    bus.tick();
                    self.pc = self.pc.wrapping_add(offset as i16 as u16);
                }
            }
            0x27 => {
                let (result, flags) = alu::daa(self.a, self.f.0);
                self.a = result;
                self.f = Flags(flags);
            }
            0x2F => {
                let (result, flags) = alu::cpl(self.a, self.f.0);
                self.a = result;
                self.f = Flags(flags);
            }
            0x37 => self.f = Flags(alu::scf(self.f.0)),
            0x3F => self.f = Flags(alu::ccf(self.f.0)),
            0x76 => {
                // HALT, including the halt bug: with IME = 0 and an interrupt pending the CPU
                // does not halt, and the following byte is read twice (`halt.html`).
                if !self.ime && bus.pending() != 0 {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
            }
            0x40..=0x7F => {
                let source = self.reg(bus, op & 7);
                self.set_reg(bus, (op >> 3) & 7, source);
            }
            0x80..=0xBF => {
                let operand = self.reg(bus, op & 7);
                self.alu((op >> 3) & 7, operand);
            }
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                bus.tick();
                if self.cond((op >> 3) & 3) {
                    let target = self.pop(bus);
                    self.pc = target;
                    bus.tick();
                }
            }
            0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let value = self.pop(bus);
                match op {
                    0xC1 => self.set_bc(value),
                    0xD1 => self.set_de(value),
                    0xE1 => self.set_hl(value),
                    _ => self.set_af(value),
                }
            }
            0xC2 | 0xCA | 0xD2 | 0xDA => {
                let target = self.fetch16(bus);
                if self.cond((op >> 3) & 3) {
                    bus.tick();
                    self.pc = target;
                }
            }
            0xC3 => {
                let target = self.fetch16(bus);
                bus.tick();
                self.pc = target;
            }
            0xC4 | 0xCC | 0xD4 | 0xDC => {
                let target = self.fetch16(bus);
                if self.cond((op >> 3) & 3) {
                    let ret = self.pc;
                    self.push(bus, ret);
                    self.pc = target;
                }
            }
            0xC5 | 0xD5 | 0xE5 | 0xF5 => {
                let value = match op {
                    0xC5 => self.bc(),
                    0xD5 => self.de(),
                    0xE5 => self.hl(),
                    _ => self.af(),
                };
                self.push(bus, value);
            }
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let operand = self.fetch8(bus);
                self.alu((op >> 3) & 7, operand);
            }
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                let target = (op & 0x38) as u16;
                let ret = self.pc;
                self.push(bus, ret);
                self.pc = target;
            }
            0xC9 => {
                let target = self.pop(bus);
                self.pc = target;
                bus.tick();
            }
            0xD9 => {
                let target = self.pop(bus);
                self.pc = target;
                self.ime = true;
                bus.tick();
            }
            0xCB => {
                let cb = self.fetch8(bus);
                self.execute_cb(bus, cb);
            }
            0xCD => {
                let target = self.fetch16(bus);
                let ret = self.pc;
                self.push(bus, ret);
                self.pc = target;
            }
            0xE0 => {
                let offset = self.fetch8(bus);
                bus.write(0xFF00 | offset as u16, self.a);
            }
            0xE2 => {
                let addr = 0xFF00 | self.c as u16;
                bus.write(addr, self.a);
            }
            0xE8 => {
                let offset = self.fetch8(bus);
                bus.tick();
                self.add_sp_offset(offset);
                bus.tick();
            }
            0xE9 => self.pc = self.hl(),
            0xEA => {
                let addr = self.fetch16(bus);
                bus.write(addr, self.a);
            }
            0xF0 => {
                let offset = self.fetch8(bus);
                self.a = bus.read(0xFF00 | offset as u16);
            }
            0xF2 => {
                let addr = 0xFF00 | self.c as u16;
                self.a = bus.read(addr);
            }
            0xF3 => {
                self.ime = false;
                self.ime_pending = false;
            }
            0xF8 => {
                let offset = self.fetch8(bus);
                let result = self.sp.wrapping_add(offset as i8 as i16 as u16);
                self.f = Flags(self.sp_offset_flags(offset));
                self.set_hl(result);
                bus.tick();
            }
            0xF9 => {
                let value = self.hl();
                self.sp = value;
                bus.tick();
            }
            0xFA => {
                let addr = self.fetch16(bus);
                self.a = bus.read(addr);
            }
            0xFB => self.ime_pending = true,
            // Every remaining byte is one of the eleven illegal opcodes; `Cpu::step` handles them
            // before dispatching, so reaching this arm is a programming error.
            _ => debug_assert!(is_illegal(op), "undefined opcode {op:02X}"),
        }
    }

    /// `SP + e`, and the Z = N = 0 flags the half-carry and carry of an unsigned 8-bit add.
    fn add_sp_offset(&mut self, offset: u8) {
        self.f = Flags(self.sp_offset_flags(offset));
        self.sp = self.sp.wrapping_add(offset as i8 as i16 as u16);
    }

    fn sp_offset_flags(&self, offset: u8) -> u8 {
        let mut f = 0u8;
        if (self.sp & 0x000F) + (offset as u16 & 0x000F) > 0x000F {
            f |= Flags::H;
        }
        if (self.sp & 0x00FF) + offset as u16 > 0x00FF {
            f |= Flags::C;
        }
        f
    }

    /// The `$CB` block: eight rotations/shifts, `BIT`, `RES` and `SET`, over `r` or `(HL)`.
    fn execute_cb(&mut self, bus: &mut Bus, cb: u8) {
        let index = cb & 7;
        let bit = (cb >> 3) & 7;
        let value = self.reg(bus, index);
        match cb >> 6 {
            0 => {
                let (result, flags) = match bit {
                    0 => alu::rlc(value),
                    1 => alu::rrc(value),
                    2 => alu::rl(value, self.f.0),
                    3 => alu::rr(value, self.f.0),
                    4 => alu::sla(value),
                    5 => alu::sra(value),
                    6 => alu::swap(value),
                    _ => alu::srl(value),
                };
                self.set_reg(bus, index, result);
                self.f = Flags(flags);
            }
            1 => {
                let flags = alu::bit(value, bit, self.f.0);
                self.f = Flags(flags);
            }
            2 => self.set_reg(bus, index, value & !(1 << bit)),
            _ => self.set_reg(bus, index, value | (1 << bit)),
        }
    }
}
