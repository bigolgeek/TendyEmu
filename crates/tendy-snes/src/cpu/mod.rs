//! The 65816: its status register, the exception vectors and the instruction step.
//!
//! The one entry point is [`Cpu::step`]: it samples the two interrupt inputs at an instruction
//! boundary, dispatches or idles, and otherwise fetches one opcode and lets `opcodes.rs` run it.
//! Every master clock the CPU spends is charged by the access that spends it (`Bus::read`/
//! `Bus::write`) or by `Bus::tick(FAST_ACCESS)` for an internal cycle — one ticking
//! rule. `step` returns the master clocks it charged, measured off `Bus::master`, which is a check
//! on the loop rather than the clock itself.
//!
//! Sources: fullsnes "CPU Registers and Flags", "CPU Clock Cycles", "CPU Exception Vectors" and the
//! per-opcode list in "CPU Opcode Overview"; every cycle figure this file and `opcodes.rs` quote was
//! read off that page.

pub mod addressing;
pub mod alu;
pub mod opcodes;

use crate::FAST_ACCESS;
use crate::addr::Addr;
use crate::bus::Bus;
use crate::state::{StateError, StateReader, StateWriter};

/// The status register, P.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(pub u8);

impl Flags {
    /// Negative.
    pub const N: u8 = 0x80;
    /// Overflow.
    pub const V: u8 = 0x40;
    /// Accumulator/memory 8-bit.
    pub const M: u8 = 0x20;
    /// Index 8-bit.
    pub const X: u8 = 0x10;
    /// Decimal.
    pub const D: u8 = 0x08;
    /// IRQ disable.
    pub const I: u8 = 0x04;
    /// Zero.
    pub const Z: u8 = 0x02;
    /// Carry.
    pub const C: u8 = 0x01;

    pub fn n(self) -> bool {
        self.0 & Flags::N != 0
    }
    pub fn set_n(&mut self, on: bool) {
        self.set_bit(Flags::N, on);
    }
    pub fn v(self) -> bool {
        self.0 & Flags::V != 0
    }
    pub fn set_v(&mut self, on: bool) {
        self.set_bit(Flags::V, on);
    }
    pub fn m(self) -> bool {
        self.0 & Flags::M != 0
    }
    pub fn set_m(&mut self, on: bool) {
        self.set_bit(Flags::M, on);
    }
    pub fn x(self) -> bool {
        self.0 & Flags::X != 0
    }
    pub fn set_x(&mut self, on: bool) {
        self.set_bit(Flags::X, on);
    }
    pub fn d(self) -> bool {
        self.0 & Flags::D != 0
    }
    pub fn set_d(&mut self, on: bool) {
        self.set_bit(Flags::D, on);
    }
    pub fn i(self) -> bool {
        self.0 & Flags::I != 0
    }
    pub fn set_i(&mut self, on: bool) {
        self.set_bit(Flags::I, on);
    }
    pub fn z(self) -> bool {
        self.0 & Flags::Z != 0
    }
    pub fn set_z(&mut self, on: bool) {
        self.set_bit(Flags::Z, on);
    }
    pub fn c(self) -> bool {
        self.0 & Flags::C != 0
    }
    pub fn set_c(&mut self, on: bool) {
        self.set_bit(Flags::C, on);
    }

    fn set_bit(&mut self, bit: u8, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// The byte `PHP` pushes: in emulation mode the X/M flags are replaced by B and a set bit 5.
    ///
    /// fullsnes "CPU Registers and Flags": bit 4 is "0=IRQ/NMI, 1=BRK/PHP", bit 5 is "Always 1" in
    /// emulation mode.
    pub fn pushed(self, e: bool, brk: bool) -> u8 {
        if e {
            let b = if brk { 0x10 } else { 0x00 };
            (self.0 & !(Flags::M | Flags::X)) | b | 0x20
        } else {
            // In native mode bit 4 is X and bit 5 is M, so the pushed byte is the register itself.
            self.0
        }
    }

    /// The inverse of [`Flags::pushed`]: `PLP` in either mode. The B and unused bits cannot be set
    /// by a pull (fullsnes "Push/Pull": "The B-flag and unused flags cannot be changed by PLP"), and
    /// in emulation mode M and X are forced to 1 because E implies them.
    pub fn pulled(value: u8, e: bool) -> Flags {
        if e {
            Flags((value & !(Flags::M | Flags::X)) | Flags::M | Flags::X)
        } else {
            // Native mode stores M and X in bits 5 and 4; there is no B bit to mask.
            Flags(value)
        }
    }
}

/// Which exception vector to take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vector {
    Nmi,
    Irq,
    Brk,
    Cop,
    Abort,
    Reset,
}

impl Vector {
    /// The vector's address in emulation or native mode (fullsnes "CPU Exception Vectors").
    ///
    /// The 6502-mode NMI is `$FFFA` and its IRQ/BRK `$FFFE`; 65816 mode splits COP/BRK/ABORT/NMI/IRQ
    /// across `$FFE4-$FFEE`. `$FFFC` is the reset vector in both modes.
    pub fn address(self, e: bool) -> u16 {
        match self {
            Vector::Nmi => {
                if e {
                    0xFFFA
                } else {
                    0xFFEA
                }
            }
            Vector::Irq => {
                if e {
                    0xFFFE
                } else {
                    0xFFEE
                }
            }
            Vector::Brk => {
                if e {
                    0xFFFE
                } else {
                    0xFFE6
                }
            }
            Vector::Cop => {
                if e {
                    0xFFF4
                } else {
                    0xFFE4
                }
            }
            Vector::Abort => {
                if e {
                    0xFFF8
                } else {
                    0xFFE8
                }
            }
            Vector::Reset => 0xFFFC,
        }
    }
}

/// How many control transfers the diagnosis ring can hold. A frame of ordinary code makes a few
/// hundred, so this covers well over a minute of transfers; a run that fills it keeps the newest.
pub const TRACE_LEN: usize = 65_536;

/// How many stack bytes a [`Transfer`] carries: the sixteen a series of pulls would consume, i.e.
/// the return address the instruction at `pc` will act on and the caller chain behind it.
pub const TRACE_STACK: usize = 16;

/// Which kind of control transfer a [`Transfer`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TransferKind {
    #[default]
    Jsr,
    Jsl,
    Jmp,
    Jml,
    Rts,
    Rtl,
    Rti,
    Brk,
    Cop,
    Nmi,
    Irq,
    /// A relative branch — the eight conditional branches, `BRA` and `BRL` — recorded whether it was
    /// taken or not. This is the one transfer kind that does not move the CPU somewhere unexpected:
    /// it is here because "the frame is being spent in branches" is a question about how often a
    /// flag test fails, which no jump log can answer.
    Branch,
}

impl TransferKind {
    /// The name the diagnosis harness prints.
    pub fn name(self) -> &'static str {
        match self {
            TransferKind::Jsr => "JSR",
            TransferKind::Jsl => "JSL",
            TransferKind::Jmp => "JMP",
            TransferKind::Jml => "JML",
            TransferKind::Rts => "RTS",
            TransferKind::Rtl => "RTL",
            TransferKind::Rti => "RTI",
            TransferKind::Brk => "BRK",
            TransferKind::Cop => "COP",
            TransferKind::Nmi => "NMI",
            TransferKind::Irq => "IRQ",
            TransferKind::Branch => "BR",
        }
    }
}

/// The `Transfer::indirect` code for an addressing mode: 0 direct, 1 `JMP (abs)`, 2
/// `JSR`/`JMP (abs,X)`, 3 `JML [abs]`.
pub(crate) fn indirect_code(mode: addressing::Mode) -> u8 {
    match mode {
        addressing::Mode::AbsInd => 1,
        addressing::Mode::AbsIndX => 2,
        addressing::Mode::AbsIndLong => 3,
        _ => 0,
    }
}

/// One control transfer, as the CPU saw it.
///
/// Recorded only while [`Cpu::enable_trace`] has been called (a diagnosis harness, never the frame
/// path). `pb:pc` is the instruction; `target` is where the CPU went; `return` is the address that
/// was pushed (`JSR`/`JSL`) or pulled (`RTS`/`RTL`/`RTI`/exception), so a `RTS` that returned into
/// data is visible as a return address that does not match any call. `stack` is the sixteen bytes
/// above `S` **before** the instruction ran, which is what a return consumes and what a call's
/// caller chain looks like.
#[derive(Clone, Copy, Default)]
pub struct Transfer {
    pub kind: TransferKind,
    pub opcode: u8,
    /// 0 for a direct transfer, 1 for `JMP (abs)`, 2 for `JSR`/`JMP (abs,X)`, 3 for `JML [abs]`.
    pub indirect: u8,
    pub pb: u8,
    pub pc: u16,
    pub db: u8,
    pub target_bank: u8,
    pub target_pc: u16,
    pub return_bank: u8,
    pub return_pc: u16,
    pub s_before: u16,
    pub s_after: u16,
    pub p: u8,
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub d: u16,
    /// The address the indirect transfer's pointer was read from, and the value it held: 0 when the
    /// transfer was direct. `ptr` is a 24-bit bus address.
    pub ptr: u32,
    pub ptr_value: u32,
    /// Set by the harness that owns the ring; the CPU does not know what a frame is.
    pub frame: u32,
    pub stack: [u8; TRACE_STACK],
    /// For a [`TransferKind::Branch`]: whether the test passed, and the displacement it added.
    /// `target_pc` is where the CPU went (the branch target when taken, the next instruction when
    /// not), and `p` is the flag byte the test read — a branch never changes a flag, so it is the
    /// flag set before and after.
    pub taken: bool,
    pub disp: i16,
}

/// The S-CPU's register file and mode.
#[derive(Clone)]
pub struct Cpu {
    /// 16 bits wide; `Flags::M` selects whether the low byte is the register.
    pub a: u16,
    pub x: u16,
    pub y: u16,
    /// Direct page, added to every 8-bit direct address.
    pub d: u16,
    /// Stack; in emulation mode only the low byte exists, forced to page 1.
    pub s: u16,
    pub pc: u16,
    pub pb: u8,
    pub db: u8,
    pub p: Flags,
    /// Emulation mode (XCE).
    pub e: bool,
    /// WAI: asleep until an interrupt is taken.
    pub waiting: bool,
    /// STP: asleep until reset.
    pub stopped: bool,
    pub nmi_line: bool,
    pub irq_line: bool,
    pub instructions: u64,
    /// The diagnosis ring (§ design: off by default). `None` until a harness calls
    /// [`Cpu::enable_trace`]; every transfer site checks it once, so a machine that is not being
    /// traced is unchanged.
    pub trace: Option<Box<[Transfer]>>,
    /// Transfers recorded since `enable_trace`, including any the ring has overwritten.
    pub trace_len: u64,
    /// The pointer an indirect transfer read, and its value, staged by `resolve` for the record.
    trace_ptr: u32,
    trace_ptr_value: u32,
}

impl Cpu {
    /// Power-on: `E = 1`, `I = 1`, `D = 0`, `DB = PB = 0`, `S = 0x01FF`, `PC` from the reset vector
    /// at `$00:FFFC`.
    ///
    /// The vector is read through `peek`, so building a machine charges no clocks.
    pub fn new(bus: &mut Bus) -> Cpu {
        let mut cpu = Cpu {
            a: 0,
            x: 0,
            y: 0,
            d: 0,
            s: 0x01FF,
            pc: 0,
            pb: 0,
            db: 0,
            p: Flags(Flags::I | Flags::Z | Flags::M | Flags::X),
            e: true,
            waiting: false,
            stopped: false,
            nmi_line: false,
            irq_line: false,
            instructions: 0,
            trace: None,
            trace_len: 0,
            trace_ptr: 0,
            trace_ptr_value: 0,
        };
        cpu.resume_at_reset_vector(bus);
        cpu
    }

    /// Executes one instruction, or one `WAI`/`STP` idle cycle, or one interrupt dispatch, charging
    /// every master clock it spends. The return value is that count.
    ///
    /// The order is the only one that keeps interrupts honest: STP idles, the
    /// NMI latch dispatches, the IRQ line dispatches when `I = 0`, `WAI` idles, and only then does
    /// an opcode run.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        let start = bus.master;
        // The diagnosis rings' `pb:pc`: the instruction whose reads and stores are about to happen,
        // captured before the opcode is fetched so the record names the instruction and not its
        // next byte.
        if bus.write_log.is_some() || bus.read_log.is_some() {
            bus.writer = (self.pb, self.pc);
        }

        if self.stopped {
            bus.tick(FAST_ACCESS);
            return (bus.master - start) as u32;
        }

        // The NMI input is `NMITIMEN.7 AND RDNMI.7`; the CPU's internal latch is set on its 0-to-1
        // and cleared when the NMI is taken (fullsnes "SNES PPU Interrupts": "The CPU includes
        // another internal NMI flag, which gets set when [4200h].7 AND [4210h].7 changes from 0-to-1,
        // and gets cleared when the NMI gets executed"). Deriving the edge here, from the level,
        // is what lets a cartridge clear and re-enable NMI during v-blank and see it run again.
        //
        // A `$4200` store raises its edge on the store's own final cycle, and the interrupt check
        // runs just before that cycle (Anomie, S-CPU §Interrupts), so the store's instruction
        // cannot see it: the NMI lands after the *next* instruction — fullsnes' latch is "cleared
        // when the NMI gets executed (which should happen around after the next opcode)".
        // `CpuIo::write` marks that store; latch it now and take it at the following boundary.
        // Every other edge keeps this boundary: the v-blank one (`Bus::vblank`), and a pending
        // latch that a DMA's clock stop deferred to the first instruction after the transfer.
        let nmi_src = bus.io.nmi_flag && bus.io.nmitimen & 0x80 != 0;
        let store_edge = bus.io.nmi_write_edge;
        bus.io.nmi_write_edge = false;
        if (nmi_src && !self.nmi_line) || store_edge {
            bus.nmi_latch = true;
        }
        self.nmi_line = nmi_src;
        if bus.nmi_latch && !store_edge {
            bus.nmi_latch = false;
            self.waiting = false;
            self.service_interrupt(bus, Vector::Nmi);
            self.instructions += 1;
            return (bus.master - start) as u32;
        }

        self.irq_line = bus.irq_pending();
        if self.irq_line && !self.p.i() {
            self.waiting = false;
            self.service_interrupt(bus, Vector::Irq);
            self.instructions += 1;
            return (bus.master - start) as u32;
        }

        if self.waiting {
            // WAI wakes on the interrupt request itself, even when `I` masks the dispatch (fullsnes
            // "CPU Clock Cycles": "in case of IRQs this works even if IRQs are disabled (via I=1)").
            if self.irq_line {
                self.waiting = false;
            }
            bus.tick(FAST_ACCESS);
            self.instructions += 1;
            return (bus.master - start) as u32;
        }

        let opcode = self.fetch8(bus);
        if self.trace.is_some() {
            self.trace_ptr = 0;
            self.trace_ptr_value = 0;
        }
        self.execute(bus, opcode);
        self.instructions += 1;
        (bus.master - start) as u32
    }

    /// Pushes the return state and takes `vector`. Returns the master clocks it cost.
    ///
    /// Seven cycles with `E = 1` (three pushes and two vector fetches, plus two internal) and eight
    /// in native mode, which also pushes `PB` (fullsnes "CPU Clock Cycles": "NNsSSSDD 7,8 Exception
    /// (/ABORT, /IRQ, /NMI, /RES) ;+s if E=0").
    pub fn service_interrupt(&mut self, bus: &mut Bus, vector: Vector) -> u32 {
        self.interrupt(bus, vector, false)
    }

    /// The shared body of every exception: push `PB` (when native), `PC`, then `P`, clear `D`
    /// and set `I`, and load `PC` from the vector (fullsnes "Interrupts, Exceptions, Breakpoints":
    /// "Exceptions do first change the B-flag (in 6502 mode), then write P to stack, and then set
    /// the I-flag, the D-flag IS cleared").
    ///
    /// The **order matters**: "in native mode the program bank is pushed onto the stack first,
    /// before the program counter and the status register" (Eyes & Lichty, *Programming the 65816*,
    /// Figure 13.2, §"How Interrupts Are Handled"), so the frame reads upwards as `P`, `PCL`,
    /// `PCH`, `PB` — which is exactly the order [`Cpu::execute`]'s `Rti` pulls. Pushing `PB` after
    /// the program counter instead left an `RTI` reading `PCL`, `PCH` and `PB` one byte
    /// off, and Super Mario World's NMI returned to `$80:6B00` instead of `$00:806B`.
    fn interrupt(&mut self, bus: &mut Bus, vector: Vector, brk: bool) -> u32 {
        let start = bus.master;
        let pc = self.pc;
        let (pb_before, s_before) = (self.pb, self.s);
        let p_before = self.p.0;
        if !self.e {
            self.push8(bus, self.pb);
        }
        self.push8(bus, (pc >> 8) as u8);
        self.push8(bus, pc as u8);
        self.push8(bus, self.p.pushed(self.e, brk));
        self.p.set_d(false);
        self.p.set_i(true);
        self.pb = 0;
        let vector_addr = vector.address(self.e);
        let lo = bus.read(Addr::new(0, vector_addr));
        let hi = bus.read(Addr::new(0, vector_addr.wrapping_add(1)));
        self.pc = u16::from_le_bytes([lo, hi]);
        if !brk {
            // fullsnes's exception rows: "NNsSSSDD 7,8" for an IRQ/NMI has two internal cycles;
            // "CPsSSSDD 7,8" for BRK/COP has none, because the opcode's parameter byte replaces them.
            bus.tick(FAST_ACCESS * 2);
        }
        let kind = match vector {
            Vector::Nmi => TransferKind::Nmi,
            Vector::Irq => TransferKind::Irq,
            Vector::Cop => TransferKind::Cop,
            _ => TransferKind::Brk,
        };
        let event = Transfer {
            kind,
            opcode: match kind {
                TransferKind::Nmi => 0x00,
                TransferKind::Irq => 0x00,
                TransferKind::Cop => 0x02,
                _ => 0x00,
            },
            pb: pb_before,
            pc,
            db: self.db,
            target_bank: 0,
            target_pc: self.pc,
            return_bank: pb_before,
            return_pc: pc,
            s_before,
            s_after: self.s,
            p: p_before,
            a: self.a,
            x: self.x,
            y: self.y,
            d: self.d,
            ..Transfer::default()
        };
        self.record(bus, event);
        (bus.master - start) as u32
    }

    /// The S-CPU's half of the diagnosis instrument: turns the control-transfer ring on.
    ///
    /// Allocates once, here, and never on the frame path; a harness calls this on a machine it has
    /// just built. Off by default so no cartridge can observe it.
    pub fn enable_trace(&mut self) {
        if self.trace.is_none() {
            self.trace = Some(vec![Transfer::default(); TRACE_LEN].into_boxed_slice());
        }
        self.trace_len = 0;
    }

    /// Turns the ring off and releases it.
    pub fn disable_trace(&mut self) {
        self.trace = None;
        self.trace_len = 0;
    }

    /// Transfers recorded since [`Cpu::enable_trace`], including ones the ring has overwritten.
    pub fn trace_len(&self) -> u64 {
        self.trace_len
    }

    /// The transfer with absolute index `n`, or `None` once the ring has overwritten it.
    pub fn trace_event(&self, n: u64) -> Option<&Transfer> {
        let ring = self.trace.as_ref()?;
        if n >= self.trace_len || self.trace_len - n > TRACE_LEN as u64 {
            return None;
        }
        Some(&ring[(n as usize) & (TRACE_LEN - 1)])
    }

    /// The same, mutably, so the harness can stamp the frame it belongs to.
    pub fn trace_event_mut(&mut self, n: u64) -> Option<&mut Transfer> {
        let len = self.trace_len;
        let ring = self.trace.as_mut()?;
        if n >= len || len - n > TRACE_LEN as u64 {
            return None;
        }
        Some(&mut ring[(n as usize) & (TRACE_LEN - 1)])
    }

    /// The register half of a transfer record: everything a control transfer leaves unchanged.
    /// `s_after` is read here, after the instruction has finished with the stack.
    fn transfer_fields(&self) -> Transfer {
        Transfer {
            db: self.db,
            a: self.a,
            x: self.x,
            y: self.y,
            d: self.d,
            p: self.p.0,
            s_after: self.s,
            ..Transfer::default()
        }
    }

    /// Records one control transfer, with the stack as it stood before it ran. No-op when tracing
    /// is off, which is every run that is not a diagnosis harness.
    fn record(&mut self, bus: &mut Bus, event: Transfer) {
        if self.trace.is_none() {
            return;
        }
        let mut event = event;
        // `S` points at the next free slot, so the item on top of the stack is at `S + 1`: these are
        // the bytes a `RTS`/`RTL`/`RTI` consumes and what a `JSR`'s caller chain looks like.
        for (i, byte) in event.stack.iter_mut().enumerate() {
            *byte = bus.peek(Addr::new(0, event.s_before.wrapping_add(1 + i as u16)));
        }
        if let Some(ring) = self.trace.as_mut() {
            let slot = (self.trace_len as usize) & (TRACE_LEN - 1);
            ring[slot] = event;
            self.trace_len += 1;
        }
    }

    /// The CPU's half of Game > Reset (§2.2): E set, I set, S back to page 1, PC from the vector.
    pub fn reset(&mut self, bus: &mut Bus) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.d = 0;
        self.s = 0x01FF;
        self.pb = 0;
        self.db = 0;
        self.p = Flags(Flags::I | Flags::Z | Flags::M | Flags::X);
        self.e = true;
        self.waiting = false;
        self.stopped = false;
        self.nmi_line = false;
        self.irq_line = false;
        self.resume_at_reset_vector(bus);
    }

    fn resume_at_reset_vector(&mut self, bus: &mut Bus) {
        let lo = bus.peek(Addr::new(0, 0xFFFC));
        let hi = bus.peek(Addr::new(0, 0xFFFD));
        self.pc = u16::from_le_bytes([lo, hi]);
    }

    /// A in the low half, P in the high.
    pub fn af(&self) -> u16 {
        self.a | (u16::from(self.p.0) << 8)
    }

    /// The inverse of [`Cpu::af`].
    pub fn set_af(&mut self, v: u16) {
        self.a = v & 0x00FF;
        self.p = Flags((v >> 8) as u8);
    }

    // ---- widths -------------------------------------------------------------------------------

    /// True when the accumulator and memory are 8-bit: `M` is set, or `E` forces it.
    fn m8(&self) -> bool {
        self.p.m() || self.e
    }

    /// True when the index registers are 8-bit: `X` is set, or `E` forces it.
    fn x8(&self) -> bool {
        self.p.x() || self.e
    }

    /// The mask for an A/memory-width value: `$FF` or `$FFFF`.
    fn m_mask(&self) -> u16 {
        if self.m8() { 0x00FF } else { 0xFFFF }
    }

    /// The mask for an X/Y-width value.
    fn x_mask(&self) -> u16 {
        if self.x8() { 0x00FF } else { 0xFFFF }
    }

    /// Writes A, keeping the high byte untouched in 8-bit mode (fullsnes "Accumulator/Memory Flag
    /// (M)": "the upper 8bit of A can be still accessed via XBA").
    fn set_a(&mut self, value: u16) {
        if self.m8() {
            self.a = (self.a & 0xFF00) | (value & 0x00FF);
        } else {
            self.a = value;
        }
    }

    /// Writes X. In 8-bit index mode the high byte is forced to zero (fullsnes "Index Register Flag
    /// (X)": "the upper 8bit are forcefully set to 00h").
    fn set_x(&mut self, value: u16) {
        self.x = value & self.x_mask();
    }

    fn set_y(&mut self, value: u16) {
        self.y = value & self.x_mask();
    }

    /// `N` and `Z` from a value of the given width.
    fn set_nz(&mut self, value: u16, mask: u16) {
        let v = value & mask;
        self.p.set_z(v == 0);
        self.p.set_n(v & (mask & !(mask >> 1)) != 0);
    }

    // ---- bus helpers --------------------------------------------------------------------------

    /// Fetches the byte at `PB:PC` and advances the program counter, wrapping inside the bank.
    fn fetch8(&mut self, bus: &mut Bus) -> u8 {
        let addr = Addr::new(self.pb, self.pc);
        self.pc = self.pc.wrapping_add(1);
        bus.read(addr)
    }

    /// Fetches a little-endian word at `PB:PC`.
    fn fetch16(&mut self, bus: &mut Bus) -> u16 {
        let lo = self.fetch8(bus);
        let hi = self.fetch8(bus);
        u16::from_le_bytes([lo, hi])
    }

    /// Reads an operand of width `mask` at `addr`. The second byte is the next 24-bit address, so a
    /// 16-bit read at `$xx:FFFF` takes its high byte from `$xx+1:0000`: 6502.org, *65C816 Opcodes*,
    /// §5.16 — "LDA $12FFFF loads the low byte of the data from address $12FFFF, and the high byte
    /// from address $130000" — and §2, "in native mode, there is no wrapping at page boundaries".
    ///
    /// Each byte is a bus access, and the read log is told the operand's width so a 16-bit `LDA`
    /// reads as one two-byte read rather than two unrelated ones (`Bus::read` resets the width
    /// after each access, so it is set before each of the pair).
    fn read_n(&mut self, bus: &mut Bus, addr: Addr, mask: u16) -> u16 {
        if mask == 0x00FF {
            bus.read_width = 1;
            u16::from(bus.read(addr))
        } else {
            bus.read_width = 2;
            let lo = bus.read(addr);
            bus.read_width = 2;
            let hi = bus.read(addressing::carried(addr.bank(), addr.offset(), 1));
            u16::from_le_bytes([lo, hi])
        }
    }

    /// Writes an operand of width `mask` at `addr`; the second byte carries into the next bank, the
    /// same rule [`Cpu::read_n`] cites. Each byte is a bus access, and the write log is told the
    /// store's width so a 16-bit `STA` reads as one two-byte store rather than two unrelated ones.
    fn write_n(&mut self, bus: &mut Bus, addr: Addr, value: u16, mask: u16) {
        if mask == 0x00FF {
            bus.write(addr, value as u8);
        } else {
            // `Bus::write` resets the width after each byte, so it is set before each of the pair.
            bus.write_width = 2;
            bus.write(addr, value as u8);
            bus.write_width = 2;
            bus.write(
                addressing::carried(addr.bank(), addr.offset(), 1),
                (value >> 8) as u8,
            );
        }
    }

    /// Charges `cycles` internal cycles at the fast rate.
    fn internal(bus: &mut Bus, cycles: u32) {
        for _ in 0..cycles {
            bus.tick(FAST_ACCESS);
        }
    }

    // ---- stack --------------------------------------------------------------------------------

    /// Pushes one byte. In emulation mode the stack is page one and only `S`'s low byte moves
    /// (fullsnes "Emulation Flag (E)": "S is 8bit (range 0100h..01FFh)").
    fn push8(&mut self, bus: &mut Bus, value: u8) {
        bus.write(Addr::new(0, self.s), value);
        let s = self.s.wrapping_sub(1);
        self.s = if self.e { 0x0100 | (s & 0x00FF) } else { s };
    }

    /// Pops one byte.
    fn pull8(&mut self, bus: &mut Bus) -> u8 {
        let s = self.s.wrapping_add(1);
        self.s = if self.e { 0x0100 | (s & 0x00FF) } else { s };
        let addr = Addr::new(0, self.s);
        bus.read(addr)
    }

    /// Pushes `value` at width `mask`, high byte first.
    fn push_n(&mut self, bus: &mut Bus, value: u16, mask: u16) {
        if mask != 0x00FF {
            self.push8(bus, (value >> 8) as u8);
        }
        self.push8(bus, value as u8);
    }

    /// Pops at width `mask`.
    fn pull_n(&mut self, bus: &mut Bus, mask: u16) -> u16 {
        let lo = self.pull8(bus);
        if mask == 0x00FF {
            u16::from(lo)
        } else {
            let hi = self.pull8(bus);
            u16::from_le_bytes([lo, hi])
        }
    }

    /// §9.2's CPU rows, in the design's order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.a);
        w.u16(self.x);
        w.u16(self.y);
        w.u16(self.d);
        w.u16(self.s);
        w.u16(self.pc);
        w.u8(self.p.0);
        w.u8(self.pb);
        w.u8(self.db);
        w.bool(self.e);
        w.bool(self.waiting);
        w.bool(self.stopped);
        w.bool(self.nmi_line);
        w.bool(self.irq_line);
    }

    /// The inverse of [`Cpu::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        self.a = r.u16()?;
        self.x = r.u16()?;
        self.y = r.u16()?;
        self.d = r.u16()?;
        self.s = r.u16()?;
        self.pc = r.u16()?;
        self.p = Flags(r.u8()?);
        self.pb = r.u8()?;
        self.db = r.u8()?;
        self.e = r.bool()?;
        self.waiting = r.bool()?;
        self.stopped = r.bool()?;
        self.nmi_line = r.bool()?;
        self.irq_line = r.bool()?;
        Ok(())
    }
}
