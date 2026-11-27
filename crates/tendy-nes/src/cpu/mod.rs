pub mod addressing;
pub mod opcodes;
pub mod ops_alu;
pub mod ops_basic;
pub mod ops_flow;
pub mod ops_unofficial;
pub mod step;
mod state;
pub mod trace;

/// Status flag bits in `Cpu::p`.
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
pub const FLAG_B: u8 = 0x10;
pub const FLAG_U: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

/// Everything the CPU can read or write. Each read or write is one CPU cycle.
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);

    /// A read whose cycle does not update the interrupt poll: the extra cycle of a taken branch with no
    /// page cross (hardware keeps the poll result from the operand cycle). Buses that do not model the
    /// poll default to a plain read, so their access lists are unchanged.
    fn read_unpolled(&mut self, addr: u16) -> u8 {
        self.read(addr)
    }

    /// Called at the end of every instruction: true when an NMI edge had been seen by the end of the
    /// instruction's second-to-last cycle (the NMI is then taken next). Consumes that edge.
    fn nmi_poll(&mut self) -> bool {
        false
    }

    /// Called at the end of every instruction: the IRQ line level at the end of the second-to-last
    /// cycle, or None for a bus that does not drive IRQ (tests set `Cpu::irq_line` themselves).
    fn irq_poll(&self) -> Option<bool> {
        None
    }

    /// True if a DMA cycle (OAM or DMC) just completed on the previous CPU cycle.
    /// Used to detect unstable opcode behavior (e.g., SHx drops `& (H+1)` when DMA lands on previous cycle).
    fn dma_just_occurred(&self) -> bool {
        false
    }

    /// Called during BRK or IRQ entry, after the pushes and before the vector fetch.
    /// Returns true if an NMI edge should hijack the vector (use $FFFA instead of the original vector).
    /// If true, the hook must clear the NMI state so it doesn't fire again after this handler.
    fn nmi_hijack(&mut self) -> bool {
        false
    }

    /// Side-effect-free byte read for debugger snapshots and disassembly: RAM and cartridge only
    /// (the same rule the trace logger's `op_byte` uses), `None` for I/O addresses, which must not
    /// be touched outside a real bus access. The default (always `None`) is a bus without such a
    /// region.
    fn peek(&mut self, addr: u16) -> Option<u8> {
        let _ = addr;
        None
    }

    /// The byte at `pc` is being used as code: an opcode or operand byte. The code/data log
    /// marks it; every other bus ignores it, so the default is a no-op and `TestBus` is unchanged.
    fn note_code(&mut self, pc: u16) {
        let _ = pc;
    }

    /// The next `read` is not a data access: an instruction fetch or a dummy read. The
    /// code/data log skips data marking for it; the default is a no-op.
    fn note_dummy(&mut self) {}
}

/// Read a code byte (opcode or operand): tell the bus it is code and that the read is not data.
fn read_code(bus: &mut impl Bus, addr: u16) -> u8 {
    bus.note_code(addr);
    bus.note_dummy();
    bus.read(addr)
}

/// A flat 64 KB memory, for tests.
pub struct TestBus {
    pub mem: Vec<u8>,
}

impl TestBus {
    pub fn new() -> TestBus {
        TestBus {
            mem: vec![0; 0x10000],
        }
    }
}

impl Default for TestBus {
    fn default() -> Self {
        TestBus::new()
    }
}

impl Bus for TestBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }
    fn write(&mut self, addr: u16, val: u8) {
        self.mem[addr as usize] = val;
    }
    fn peek(&mut self, addr: u16) -> Option<u8> {
        Some(self.mem[addr as usize])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub p: u8,
    /// Total CPU cycles since power-on.
    pub cycles: u64,
    /// Set by the PPU at vblank; serviced before the next instruction.
    pub nmi_pending: bool,
    /// Set by interrupt entry and BRK: the handler's first instruction runs before the next NMI is taken.
    pub skip_nmi_poll: bool,
    /// Level-triggered IRQ line (APU frame counter, DMC, mappers).
    pub irq_line: bool,
    /// The I flag as the CPU saw it when it last polled for interrupts (CLI, SEI and PLP change I
    /// after the poll, so their effect on IRQ is one instruction late). None until the first poll.
    pub poll_i: Option<bool>,
    /// Set when a JAM opcode halts the CPU.
    pub jammed: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    /// Power-on state: a = x = y = 0, sp = 0xFD, pc = 0, p = 0x24, cycles = 0, all bools false.
    pub fn new() -> Cpu {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            p: 0x24,
            cycles: 0,
            nmi_pending: false,
            skip_nmi_poll: false,
            irq_line: false,
            poll_i: None,
            jammed: false,
        }
    }

    /// Load pc from the reset vector at 0xFFFC (low byte) / 0xFFFD (high byte), then sp = 0xFD, p = 0x24, cycles = 7, jammed = false. a, x, y are unchanged.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.pc = Self::read16(bus, 0xFFFC);
        self.sp = 0xFD;
        self.p = 0x24;
        self.cycles = 7;
        self.jammed = false;
    }

    /// Reset button (not power-on): as on the 6502, A/X/Y are kept, S drops by 3, I is set, PC comes from $FFFC.
    pub fn soft_reset(&mut self, bus: &mut impl Bus) {
        self.pc = Self::read16(bus, 0xFFFC);
        self.sp = self.sp.wrapping_sub(3);
        self.p |= FLAG_I;
        self.jammed = false;
    }

    pub fn flag(&self, f: u8) -> bool {
        self.p & f != 0
    }

    pub fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.p |= f;
        } else {
            self.p &= !f;
        }
    }

    /// The vector the next `Cpu::step` enters *instead of* executing the instruction at PC, or
    /// `None` when PC runs next. NMI first, then an IRQ the I flag does not mask, matching the
    /// order `step` uses. `poll_i` is read, not consumed, and a jammed CPU enters no vector.
    pub fn next_interrupt_vector(&self) -> Option<u16> {
        if self.jammed {
            return None;
        }
        self.interrupt_vector_for(self.poll_i.unwrap_or(self.flag(FLAG_I)))
    }

    /// The vector an interrupt entry takes, given the I flag as the CPU last polled it. `step` and
    /// `next_interrupt_vector` share this, so a debugger's prediction and the core's decision
    /// cannot drift apart.
    fn interrupt_vector_for(&self, i_masked: bool) -> Option<u16> {
        if self.nmi_pending {
            Some(0xFFFA)
        } else if self.irq_line && !i_masked {
            Some(0xFFFE)
        } else {
            None
        }
    }

    /// Set Z when v == 0 and N when bit 7 of v is set (clear them otherwise).
    pub fn set_zn(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, v & 0x80 != 0);
    }

    /// Read the byte at pc and advance pc by 1 (wrapping).
    pub fn fetch(&mut self, bus: &mut impl Bus) -> u8 {
        let v = read_code(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    /// Two fetches, little-endian: low byte first.
    pub fn fetch16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    /// Little-endian 16-bit read at addr and addr + 1 (wrapping).
    pub fn read16(bus: &mut impl Bus, addr: u16) -> u16 {
        let lo = bus.read(addr);
        let hi = bus.read(addr.wrapping_add(1));
        u16::from_le_bytes([lo, hi])
    }

    /// Write v at 0x0100 + sp, then sp = sp.wrapping_sub(1).
    pub fn push(&mut self, bus: &mut impl Bus, v: u8) {
        bus.write(0x0100 | u16::from(self.sp), v);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// sp = sp.wrapping_add(1), then read 0x0100 + sp.
    pub fn pop(&mut self, bus: &mut impl Bus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | u16::from(self.sp))
    }

    /// Push the high byte first, then the low byte.
    pub fn push16(&mut self, bus: &mut impl Bus, v: u16) {
        let [lo, hi] = v.to_le_bytes();
        self.push(bus, hi);
        self.push(bus, lo);
    }

    /// Pop the low byte first, then the high byte.
    pub fn pop16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.pop(bus);
        let hi = self.pop(bus);
        u16::from_le_bytes([lo, hi])
    }
}
