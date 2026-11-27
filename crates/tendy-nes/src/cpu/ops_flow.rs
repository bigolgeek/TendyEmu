use super::addressing::Operand;
use super::opcodes::Mn;
use super::{Bus, Cpu, FLAG_B, FLAG_C, FLAG_I, FLAG_N, FLAG_U, FLAG_V, FLAG_Z};

impl Cpu {
    /// Enter an interrupt: two dummy reads of pc, push pc and p, set I, jump through the vector.
    /// The vector address can be hijacked by NMI if it arrives before the vector fetch (cycle 5).
    /// Returns 7 cycles.
    pub fn interrupt(&mut self, bus: &mut impl Bus, vector: u16) -> u32 {
        bus.note_dummy();
        bus.read(self.pc);
        bus.note_dummy();
        bus.read(self.pc);
        let pc = self.pc;
        self.push16(bus, pc);
        let p = (self.p & !FLAG_B) | FLAG_U;
        self.push(bus, p);
        self.set_flag(FLAG_I, true);
        self.pc = Cpu::read_vector_or_hijack(bus, vector);
        self.skip_nmi_poll = true;
        7
    }

    /// Read the interrupt vector, potentially hijacked by NMI.
    /// If nmi_hijack() returns true, use the NMI vector ($FFFA) instead.
    fn read_vector_or_hijack(bus: &mut impl Bus, vector: u16) -> u16 {
        let hijacked_vector = if vector != 0xFFFA && bus.nmi_hijack() {
            0xFFFA
        } else {
            vector
        };
        Cpu::read16(bus, hijacked_vector)
    }

    /// Taken branch: jump and return the extra cycles (1, or 2 when the target is on another page). Not taken: 0.
    /// Each extra cycle is a dummy read: of the next opcode at pc, then (page crossed) of the target
    /// with pc's old high byte.
    fn branch(&mut self, bus: &mut impl Bus, taken: bool, o: Operand) -> u32 {
        if !taken {
            return 0;
        }
        if o.page_crossed {
            bus.note_dummy();
            bus.read(self.pc);
            bus.note_dummy();
            bus.read((self.pc & 0xFF00) | (o.addr & 0x00FF));
        } else {
            // No page cross: the extra cycle does not poll the interrupt lines, so an IRQ that rises during
            // the operand cycle is taken after the instruction at the target.
            bus.note_dummy();
            bus.read_unpolled(self.pc);
        }
        self.pc = o.addr;
        if o.page_crossed { 2 } else { 1 }
    }

    /// Returns the extra cycles. `mn` is dispatched here by the exhaustive match in `step`;
    /// JSR has its own path there, and a mnemonic from another group panics rather than acting as a
    /// silent no-op.
    pub fn exec_flow(&mut self, bus: &mut impl Bus, mn: Mn, o: Operand) -> u32 {
        match mn {
            Mn::Bcc => self.branch(bus, !self.flag(FLAG_C), o),
            Mn::Bcs => self.branch(bus, self.flag(FLAG_C), o),
            Mn::Bne => self.branch(bus, !self.flag(FLAG_Z), o),
            Mn::Beq => self.branch(bus, self.flag(FLAG_Z), o),
            Mn::Bpl => self.branch(bus, !self.flag(FLAG_N), o),
            Mn::Bmi => self.branch(bus, self.flag(FLAG_N), o),
            Mn::Bvc => self.branch(bus, !self.flag(FLAG_V), o),
            Mn::Bvs => self.branch(bus, self.flag(FLAG_V), o),
            Mn::Jmp => {
                self.pc = o.addr;
                0
            }
            Mn::Rts => {
                bus.read(0x0100 | u16::from(self.sp));
                let ret = self.pop16(bus);
                // Dummy read at the popped address before it is incremented.
                bus.note_dummy();
                bus.read(ret);
                self.pc = ret.wrapping_add(1);
                0
            }
            Mn::Rti => {
                bus.read(0x0100 | u16::from(self.sp));
                let p = self.pop(bus);
                self.p = (p & !FLAG_B) | FLAG_U;
                self.pc = self.pop16(bus);
                0
            }
            Mn::Brk => {
                // BRK skips a padding byte: the pushed return address is pc + 1.
                let ret = self.pc.wrapping_add(1);
                self.push16(bus, ret);
                let p = self.p | FLAG_B | FLAG_U;
                self.push(bus, p);
                self.set_flag(FLAG_I, true);
                self.pc = Cpu::read_vector_or_hijack(bus, 0xFFFE);
                self.skip_nmi_poll = true;
                0
            }
            Mn::Jam => {
                self.jammed = true;
                0
            }
            _ => unreachable!("exec_flow called with {mn:?}"),
        }
    }
}
