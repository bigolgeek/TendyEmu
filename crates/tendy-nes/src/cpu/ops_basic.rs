use super::opcodes::Mn;
use super::{Bus, Cpu, FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_U, FLAG_V, FLAG_Z};

impl Cpu {
    /// First two cycles of a read-modify-write on memory: read the value, then write it back unchanged.
    pub fn read_modify(bus: &mut impl Bus, addr: u16) -> u8 {
        let v = bus.read(addr);
        bus.write(addr, v);
        v
    }

    /// JSR: fetch the target's low byte, a dummy stack read, push the address of the high byte, then
    /// fetch the high byte and jump. Returns 6 cycles.
    pub fn jsr(&mut self, bus: &mut impl Bus) -> u32 {
        let lo = self.fetch(bus);
        bus.read(0x0100 | u16::from(self.sp));
        let ret = self.pc;
        self.push16(bus, ret);
        let hi = super::read_code(bus, self.pc);
        self.pc = u16::from_le_bytes([lo, hi]);
        6
    }

    /// Execute a load/store/transfer/stack/flag/inc-dec/logic instruction. Returns the extra cycles
    /// (always 0 for this group). `step`'s exhaustive dispatch is what proves `mn` is one of
    /// them; a mnemonic from another group reaches the last arm and panics rather than acting as a
    /// silent no-op.
    pub fn exec_basic(&mut self, bus: &mut impl Bus, mn: Mn, addr: u16) -> u32 {
        match mn {
            Mn::Lda => {
                let v = bus.read(addr);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Ldx => {
                let v = bus.read(addr);
                self.x = v;
                self.set_zn(v);
            }
            Mn::Ldy => {
                let v = bus.read(addr);
                self.y = v;
                self.set_zn(v);
            }
            Mn::Sta => {
                bus.write(addr, self.a);
            }
            Mn::Stx => {
                bus.write(addr, self.x);
            }
            Mn::Sty => {
                bus.write(addr, self.y);
            }
            Mn::Tax => {
                self.x = self.a;
                self.set_zn(self.x);
            }
            Mn::Tay => {
                self.y = self.a;
                self.set_zn(self.y);
            }
            Mn::Txa => {
                self.a = self.x;
                self.set_zn(self.a);
            }
            Mn::Tya => {
                self.a = self.y;
                self.set_zn(self.a);
            }
            Mn::Tsx => {
                self.x = self.sp;
                self.set_zn(self.x);
            }
            Mn::Txs => {
                self.sp = self.x;
            }
            Mn::Pha => {
                let v = self.a;
                self.push(bus, v);
            }
            Mn::Php => {
                let v = self.p | FLAG_B | FLAG_U;
                self.push(bus, v);
            }
            Mn::Pla => {
                // Dummy read of the stack before the pointer is incremented.
                bus.read(0x0100 | u16::from(self.sp));
                let v = self.pop(bus);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Plp => {
                bus.read(0x0100 | u16::from(self.sp));
                let v = self.pop(bus);
                self.p = (v & !FLAG_B) | FLAG_U;
            }
            Mn::Clc => {
                self.set_flag(FLAG_C, false);
            }
            Mn::Sec => {
                self.set_flag(FLAG_C, true);
            }
            Mn::Cli => {
                self.set_flag(FLAG_I, false);
            }
            Mn::Sei => {
                self.set_flag(FLAG_I, true);
            }
            Mn::Cld => {
                self.set_flag(FLAG_D, false);
            }
            Mn::Sed => {
                self.set_flag(FLAG_D, true);
            }
            Mn::Clv => {
                self.set_flag(FLAG_V, false);
            }
            Mn::Inc => {
                let v = Cpu::read_modify(bus, addr).wrapping_add(1);
                bus.write(addr, v);
                self.set_zn(v);
            }
            Mn::Dec => {
                let v = Cpu::read_modify(bus, addr).wrapping_sub(1);
                bus.write(addr, v);
                self.set_zn(v);
            }
            Mn::Inx => {
                self.x = self.x.wrapping_add(1);
                self.set_zn(self.x);
            }
            Mn::Iny => {
                self.y = self.y.wrapping_add(1);
                self.set_zn(self.y);
            }
            Mn::Dex => {
                self.x = self.x.wrapping_sub(1);
                self.set_zn(self.x);
            }
            Mn::Dey => {
                self.y = self.y.wrapping_sub(1);
                self.set_zn(self.y);
            }
            Mn::And => {
                let v = self.a & bus.read(addr);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Ora => {
                let v = self.a | bus.read(addr);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Eor => {
                let v = self.a ^ bus.read(addr);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Bit => {
                let m = bus.read(addr);
                self.set_flag(FLAG_Z, self.a & m == 0);
                self.set_flag(FLAG_V, m & 0x40 != 0);
                self.set_flag(FLAG_N, m & 0x80 != 0);
            }
            Mn::Nop => {}
            _ => unreachable!("exec_basic called with {mn:?}"),
        }
        0
    }
}
