use super::addressing::Operand;
use super::opcodes::Mn;
use super::{Bus, Cpu, FLAG_C, FLAG_N, FLAG_V};

impl Cpu {
    /// SHX/SHY/AHX/TAS store `reg & (H + 1)`, where H is the high byte of the base address before indexing.
    /// When indexing crossed a page, the stored value also replaces the high byte of the address.
    /// When a DMA just occurred, the `& (H + 1)` term is dropped (unstable opcode behavior).
    fn store_high_and(bus: &mut impl Bus, o: Operand, reg: u8, dma_just_occurred: bool) {
        let hi = (o.addr >> 8) as u8;
        let base_hi = if o.page_crossed {
            hi.wrapping_sub(1)
        } else {
            hi
        };
        // If DMA just occurred, the opcode is unstable: skip the `& (H + 1)` term.
        let v = if dma_just_occurred {
            reg
        } else {
            reg & base_hi.wrapping_add(1)
        };
        let addr = if o.page_crossed {
            (u16::from(v) << 8) | (o.addr & 0x00FF)
        } else {
            o.addr
        };
        bus.write(addr, v);
    }

    /// Execute one of the unofficial opcodes. Returns the extra cycles (always 0 for this group);
    /// `mn` is dispatched here by the exhaustive match in `step`, and a mnemonic from another
    /// group panics rather than acting as a silent no-op.
    pub fn exec_unofficial(&mut self, bus: &mut impl Bus, mn: Mn, o: Operand) -> u32 {
        let addr = o.addr;
        match mn {
            Mn::Lax => {
                let v = bus.read(addr);
                self.a = v;
                self.x = v;
                self.set_zn(v);
            }
            Mn::Sax => bus.write(addr, self.a & self.x),
            Mn::Dcp => {
                let v = Cpu::read_modify(bus, addr).wrapping_sub(1);
                bus.write(addr, v);
                self.compare(self.a, v);
            }
            Mn::Isb => {
                let v = Cpu::read_modify(bus, addr).wrapping_add(1);
                bus.write(addr, v);
                self.adc_value(!v);
            }
            Mn::Slo => {
                let v = Cpu::read_modify(bus, addr);
                let r = self.asl_val(v);
                bus.write(addr, r);
                self.a |= r;
                self.set_zn(self.a);
            }
            Mn::Rla => {
                let v = Cpu::read_modify(bus, addr);
                let r = self.rol_val(v);
                bus.write(addr, r);
                self.a &= r;
                self.set_zn(self.a);
            }
            Mn::Sre => {
                let v = Cpu::read_modify(bus, addr);
                let r = self.lsr_val(v);
                bus.write(addr, r);
                self.a ^= r;
                self.set_zn(self.a);
            }
            Mn::Rra => {
                let v = Cpu::read_modify(bus, addr);
                let r = self.ror_val(v);
                bus.write(addr, r);
                self.adc_value(r);
            }
            Mn::Anc => {
                self.a &= bus.read(addr);
                self.set_zn(self.a);
                self.set_flag(FLAG_C, self.flag(FLAG_N));
            }
            Mn::Alr => {
                let v = self.a & bus.read(addr);
                self.a = self.lsr_val(v);
            }
            Mn::Arr => {
                let v = self.a & bus.read(addr);
                let carry_in = if self.flag(FLAG_C) { 0x80 } else { 0 };
                let r = (v >> 1) | carry_in;
                self.a = r;
                self.set_zn(r);
                self.set_flag(FLAG_C, r & 0x40 != 0);
                self.set_flag(FLAG_V, ((r >> 6) ^ (r >> 5)) & 0x01 != 0);
            }
            Mn::Axs => {
                let ax = self.a & self.x;
                let m = bus.read(addr);
                self.set_flag(FLAG_C, ax >= m);
                self.x = ax.wrapping_sub(m);
                self.set_zn(self.x);
            }
            Mn::Xaa => {
                let v = (self.a | 0xEE) & self.x & bus.read(addr);
                self.a = v;
                self.set_zn(v);
            }
            Mn::Las => {
                let v = bus.read(addr) & self.sp;
                self.a = v;
                self.x = v;
                self.sp = v;
                self.set_zn(v);
            }
            Mn::Shx => Cpu::store_high_and(bus, o, self.x, bus.dma_just_occurred()),
            Mn::Shy => Cpu::store_high_and(bus, o, self.y, bus.dma_just_occurred()),
            Mn::Ahx => Cpu::store_high_and(bus, o, self.a & self.x, bus.dma_just_occurred()),
            Mn::Tas => {
                self.sp = self.a & self.x;
                Cpu::store_high_and(bus, o, self.sp, bus.dma_just_occurred());
            }
            _ => unreachable!("exec_unofficial called with {mn:?}"),
        }
        0
    }
}
