use super::opcodes::Mode;
use super::{Bus, Cpu};

/// A resolved operand: the effective address, and whether indexing crossed a page boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operand {
    pub addr: u16,
    pub page_crossed: bool,
}

impl Cpu {
    /// Resolve the operand of a read instruction. See `operand_for`.
    pub fn operand(&mut self, bus: &mut impl Bus, mode: Mode) -> Operand {
        self.operand_for(bus, mode, false)
    }

    /// Read the operand bytes at pc (advancing pc past them) and return the effective address, making
    /// the same bus accesses as the 6502, including its dummy reads:
    /// - Imp and Acc read the next byte without advancing pc, and return addr 0.
    /// - Zpx and Zpy read the zero-page base address before adding the index; Izx reads the pointer
    ///   address before adding X.
    /// - Abx, Aby and Izy read the un-fixed address (base high byte, indexed low byte) when the index
    ///   crosses a page, or always when `writes` is set (stores and read-modify-write instructions).
    ///
    /// Imm returns the address of the immediate byte. Rel returns the branch target (pc after the
    /// operand plus the signed offset).
    pub fn operand_for(&mut self, bus: &mut impl Bus, mode: Mode, writes: bool) -> Operand {
        let (addr, page_crossed) = match mode {
            Mode::Imp | Mode::Acc => {
                bus.note_dummy();
                bus.read(self.pc);
                (0, false)
            }
            Mode::Imm => {
                let addr = self.pc;
                // The immediate byte is read over the bus by the instruction itself (not by a fetch), so
                // the log is told it is code here and that the coming read is not data.
                bus.note_code(addr);
                bus.note_dummy();
                self.pc = self.pc.wrapping_add(1);
                (addr, false)
            }
            Mode::Zp => (u16::from(self.fetch(bus)), false),
            Mode::Zpx => {
                let base = self.fetch(bus);
                bus.read(u16::from(base));
                (u16::from(base.wrapping_add(self.x)), false)
            }
            Mode::Zpy => {
                let base = self.fetch(bus);
                bus.read(u16::from(base));
                (u16::from(base.wrapping_add(self.y)), false)
            }
            Mode::Abs => (self.fetch16(bus), false),
            Mode::Abx => {
                let base = self.fetch16(bus);
                Cpu::indexed(bus, base, self.x, writes)
            }
            Mode::Aby => {
                let base = self.fetch16(bus);
                Cpu::indexed(bus, base, self.y, writes)
            }
            Mode::Ind => {
                // 6502 bug: the high byte is read from the same page as the low byte.
                let ptr = self.fetch16(bus);
                let lo = bus.read(ptr);
                let hi = bus.read((ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF));
                (u16::from_le_bytes([lo, hi]), false)
            }
            Mode::Izx => {
                let ptr = self.fetch(bus);
                bus.read(u16::from(ptr));
                let zp = ptr.wrapping_add(self.x);
                let lo = bus.read(u16::from(zp));
                let hi = bus.read(u16::from(zp.wrapping_add(1)));
                (u16::from_le_bytes([lo, hi]), false)
            }
            Mode::Izy => {
                let zp = self.fetch(bus);
                let lo = bus.read(u16::from(zp));
                let hi = bus.read(u16::from(zp.wrapping_add(1)));
                Cpu::indexed(bus, u16::from_le_bytes([lo, hi]), self.y, writes)
            }
            Mode::Rel => {
                let offset = self.fetch(bus) as i8;
                let addr = self.pc.wrapping_add_signed(i16::from(offset));
                (addr, self.pc & 0xFF00 != addr & 0xFF00)
            }
        };

        Operand { addr, page_crossed }
    }

    /// base + index. The 6502 adds the index to the low byte first and reads that un-fixed address;
    /// the read is wasted when a page is crossed (or always, for `writes`), and the high byte is then fixed.
    fn indexed(bus: &mut impl Bus, base: u16, index: u8, writes: bool) -> (u16, bool) {
        let addr = base.wrapping_add(u16::from(index));
        let crossed = base & 0xFF00 != addr & 0xFF00;
        if crossed || writes {
            bus.note_dummy();
            bus.read((base & 0xFF00) | (addr & 0x00FF));
        }
        (addr, crossed)
    }
}
