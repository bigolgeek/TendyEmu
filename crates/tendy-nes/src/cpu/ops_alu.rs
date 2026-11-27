use super::opcodes::{Mn, Mode};
use super::{Bus, Cpu, FLAG_C, FLAG_V};

impl Cpu {
    /// A + v + C with carry and signed-overflow flags. SBC uses this with !v.
    pub fn adc_value(&mut self, v: u8) {
        let carry = u16::from(self.flag(FLAG_C));
        let sum = u16::from(self.a) + u16::from(v) + carry;
        let result = sum as u8;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_V, (self.a ^ result) & (v ^ result) & 0x80 != 0);
        self.a = result;
        self.set_zn(result);
    }

    /// CMP/CPX/CPY: C = reg >= v, Z and N from reg - v.
    pub fn compare(&mut self, reg: u8, v: u8) {
        self.set_flag(FLAG_C, reg >= v);
        self.set_zn(reg.wrapping_sub(v));
    }

    pub fn asl_val(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        self.set_zn(r);
        r
    }

    pub fn lsr_val(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = v >> 1;
        self.set_zn(r);
        r
    }

    pub fn rol_val(&mut self, v: u8) -> u8 {
        let carry_in = u8::from(self.flag(FLAG_C));
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | carry_in;
        self.set_zn(r);
        r
    }

    pub fn ror_val(&mut self, v: u8) -> u8 {
        let carry_in = if self.flag(FLAG_C) { 0x80 } else { 0 };
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = (v >> 1) | carry_in;
        self.set_zn(r);
        r
    }

    /// Read-modify-write through `f`: on the accumulator when mode is Acc, otherwise on memory at addr
    /// (read, write the old value back, write the result).
    pub fn modify(&mut self, bus: &mut impl Bus, mode: Mode, addr: u16, f: fn(&mut Cpu, u8) -> u8) {
        if mode == Mode::Acc {
            let v = self.a;
            self.a = f(self, v);
        } else {
            let v = bus.read(addr);
            // The 6502 writes the unmodified value back before writing the result.
            bus.write(addr, v);
            let r = f(self, v);
            bus.write(addr, r);
        }
    }

    /// Execute an arithmetic/logic/shift instruction. Returns the extra cycles (always 0 for this
    /// group); `mn` is dispatched here by the exhaustive match in `step`, and a mnemonic from
    /// another group panics rather than acting as a silent no-op.
    pub fn exec_alu(&mut self, bus: &mut impl Bus, mn: Mn, mode: Mode, addr: u16) -> u32 {
        match mn {
            Mn::Adc => {
                let v = bus.read(addr);
                self.adc_value(v);
            }
            Mn::Sbc => {
                let v = bus.read(addr);
                self.adc_value(!v);
            }
            Mn::Cmp => {
                let v = bus.read(addr);
                self.compare(self.a, v);
            }
            Mn::Cpx => {
                let v = bus.read(addr);
                self.compare(self.x, v);
            }
            Mn::Cpy => {
                let v = bus.read(addr);
                self.compare(self.y, v);
            }
            Mn::Asl => self.modify(bus, mode, addr, Cpu::asl_val),
            Mn::Lsr => self.modify(bus, mode, addr, Cpu::lsr_val),
            Mn::Rol => self.modify(bus, mode, addr, Cpu::rol_val),
            Mn::Ror => self.modify(bus, mode, addr, Cpu::ror_val),
            _ => unreachable!("exec_alu called with {mn:?}"),
        }
        0
    }
}
