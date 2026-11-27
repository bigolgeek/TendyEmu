//! The 65816's addressing modes: the effective address, and the internal cycles each mode owes
//! (fullsnes "CPU Memory Addressing" and "CPU Clock Cycles").
//!
//! The cycle model here is fullsnes's `CP…Dd` legend, one letter at a time:
//!
//! - `C` the opcode fetch, `P` the operand bytes and `D` the data byte are all real bus accesses, so
//!   the CPU never charges for them here — `Bus::read`/`Bus::write` do, at the region's cost (§3.2).
//! - `N` internal cycles, and the `n`/`x`/`y` modifiers, are charged by [`Cpu::resolve`] through
//!   `Bus::tick(FAST_ACCESS)`. `n` is the direct-page penalty ("+n if (D AND 00FFh)>0"), `x` the
//!   emulation-mode branch penalty and `y` the indexed-read penalty ("+y if X=0 or indexing across
//!   page boundaries").
//!
//! The 16-bit data cycle (`d`, and `dd` for a read-modify-write) needs no code: `Cpu::read_data` and
//! `Cpu::write_data` perform the second access themselves, which is what the extra cycle *is*.

use crate::addr::Addr;
use crate::bus::Bus;
use crate::cpu::Cpu;

/// `base + index` as a 24-bit address in `bank`: the carry out of the offset goes into the bank.
///
/// The indexed address is the full 24-bit address plus the index — 6502.org, *65C816 Opcodes*, §2:
/// "in native mode, there is no wrapping at page boundaries", and §5.17 works the case through:
/// "If the X register is $000A and the m flag is 0, then LDA $12FFFE,X loads the low byte of the
/// data from address $130008, and the high byte from address $130009." A carry out of bank `$FF`
/// is the bus's own 24-bit wrap.
pub(crate) fn carried(bank: u8, base: u16, index: u16) -> Addr {
    let (offset, carry) = base.overflowing_add(index);
    Addr::new(bank.wrapping_add(u8::from(carry)), offset)
}

/// How the operand of an instruction is addressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// No operand: `NOP`, `CLC`, `INX`.
    Imp,
    /// The accumulator itself: `ASL A`.
    Acc,
    /// `#imm`, one or two bytes by the M or X flag.
    Imm,
    /// `nn` — direct page.
    Dp,
    /// `nn,X`.
    DpX,
    /// `nn,Y`.
    DpY,
    /// `nnnn` — absolute, in `DB`.
    Abs,
    /// `nnnn,X`.
    AbsX,
    /// `nnnn,Y`.
    AbsY,
    /// `nnnnnn` — long, a full 24-bit address.
    Long,
    /// `nnnnnn,X`.
    LongX,
    /// `(nn,X)` — pointer in the direct page, indexed.
    IndDpX,
    /// `(nn)` — pointer in the direct page.
    IndDp,
    /// `(nn),Y` — direct-page pointer, indexed.
    IndDpY,
    /// `[nn]` — long pointer in the direct page.
    IndLong,
    /// `[nn],Y`.
    IndLongY,
    /// `nn,S` — stack relative.
    Sr,
    /// `(nn,S),Y`.
    SrY,
    /// `(nnnn)` — the `JMP` pointer, which lives in bank `$00`.
    AbsInd,
    /// `(nnnn,X)` — the `JSR`/`JMP` pointer, read from `PB`.
    AbsIndX,
    /// `[nnnn]` — the `JML` long pointer, read from bank `$00`.
    AbsIndLong,
    /// `dd` — an 8-bit signed branch displacement.
    Rel,
    /// `dddd` — a 16-bit signed displacement (`BRL`).
    RelLong,
    /// `ss,dd` — a block move's source and destination banks.
    Block,
}

/// What the instruction does with the address, which decides the mode's internal cycles.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// One operand read.
    Read,
    /// One operand write.
    Write,
    /// A read and a write of the same address.
    Rmw,
}

/// The internal cycles `mode` owes for `access`, i.e. the part of fullsnes's cycle count that is not
/// a bus access.
///
/// Read off fullsnes "CPU Clock Cycles": `CPnDd 3,4,5`, `CPnDdNDd 5..8`, `CPnNDd 4,5,6`,
/// `CPnNDdNDd 6..9`, `CPNDd 4,5`, `CPPDd 4,5`, `CPPDdNDd 6,8`, `CPPyDd 4,5,6`, `CPPNDd 5,6`,
/// `CPPNDdNDd 7,9`, `CPPPDd 5,6`, `CPnAADd 5,6,7`, `CPnAAyDd 5..8`, `CPnAANDd 6..8`,
/// `CPnNAADd 6,7,8` and `CPNAANDd 7,8`.
pub fn internals(mode: Mode, access: Access) -> u32 {
    use Access::{Read, Rmw, Write};
    use Mode::*;
    match (mode, access) {
        (Dp, Rmw) => 1,
        (DpX | DpY, Read | Write) => 1,
        (DpX | DpY, Rmw) => 2,
        (Abs, Rmw) => 1,
        (AbsX | AbsY, Write) => 1,
        (AbsX | AbsY, Rmw) => 2,
        (IndDpX, Read | Write) => 1,
        (IndDpX, Rmw) => 2,
        (IndDpY, Write) => 1,
        (Sr, Read | Write) => 1,
        (SrY, Read | Write) => 2,
        _ => 0,
    }
}

impl Cpu {
    /// Resolves the effective address of `mode`, charging the internal cycles the mode owes for
    /// `access` and the direct-page and page-crossing penalties.
    pub fn resolve(&mut self, bus: &mut Bus, mode: Mode, access: Access) -> Addr {
        let mut cycles = internals(mode, access);
        // fullsnes: "+n if (D AND 00FFh)>0" on every direct-page mode.
        let dp_penalty = u32::from(
            matches!(
                mode,
                Mode::Dp
                    | Mode::DpX
                    | Mode::DpY
                    | Mode::IndDpX
                    | Mode::IndDp
                    | Mode::IndDpY
                    | Mode::IndLong
                    | Mode::IndLongY
            ) && self.direct_page_offset() != 0,
        );
        let addr = match mode {
            Mode::Dp => Addr::new(0, self.fetch_dp(bus)),
            Mode::DpX => {
                let base = self.fetch_dp(bus);
                Addr::new(0, base.wrapping_add(self.x))
            }
            Mode::DpY => {
                let base = self.fetch_dp(bus);
                Addr::new(0, base.wrapping_add(self.y))
            }
            Mode::Abs => Addr::new(self.db, self.fetch16(bus)),
            Mode::AbsX => {
                let base = self.fetch16(bus);
                let addr = carried(self.db, base, self.x);
                cycles += u32::from(
                    access == Access::Read && self.indexed_read_penalty(base, addr.offset()),
                );
                addr
            }
            Mode::AbsY => {
                let base = self.fetch16(bus);
                let addr = carried(self.db, base, self.y);
                cycles += u32::from(
                    access == Access::Read && self.indexed_read_penalty(base, addr.offset()),
                );
                addr
            }
            Mode::Long => {
                let lo = self.fetch16(bus);
                let bank = self.fetch8(bus);
                Addr::new(bank, lo)
            }
            Mode::LongX => {
                let lo = self.fetch16(bus);
                let bank = self.fetch8(bus);
                let addr = carried(bank, lo, self.x);
                cycles += u32::from(
                    access == Access::Read && self.indexed_read_penalty(lo, addr.offset()),
                );
                addr
            }
            Mode::IndDpX => {
                let base = self.fetch_dp(bus);
                let ptr = base.wrapping_add(self.x);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                Addr::new(self.db, u16::from_le_bytes([lo, hi]))
            }
            Mode::IndDp => {
                let ptr = self.fetch_dp(bus);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                Addr::new(self.db, u16::from_le_bytes([lo, hi]))
            }
            Mode::IndDpY => {
                let ptr = self.fetch_dp(bus);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                let base = u16::from_le_bytes([lo, hi]);
                let addr = carried(self.db, base, self.y);
                cycles += u32::from(
                    access == Access::Read && self.indexed_read_penalty(base, addr.offset()),
                );
                addr
            }
            Mode::IndLong => {
                let ptr = self.fetch_dp(bus);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                let bank = self.peek_bank0(bus, ptr.wrapping_add(2));
                Addr::new(bank, u16::from_le_bytes([lo, hi]))
            }
            Mode::IndLongY => {
                let ptr = self.fetch_dp(bus);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                let bank = self.peek_bank0(bus, ptr.wrapping_add(2));
                let base = u16::from_le_bytes([lo, hi]);
                let addr = carried(bank, base, self.y);
                cycles += u32::from(
                    access == Access::Read && self.indexed_read_penalty(base, addr.offset()),
                );
                addr
            }
            Mode::Sr => {
                let off = self.fetch8(bus);
                Addr::new(0, self.s.wrapping_add(u16::from(off)))
            }
            Mode::SrY => {
                let off = self.fetch8(bus);
                let ptr = self.s.wrapping_add(u16::from(off));
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                let base = u16::from_le_bytes([lo, hi]);
                carried(self.db, base, self.y)
            }
            Mode::AbsInd => {
                let ptr = self.fetch16(bus);
                let lo = self.peek_bank0(bus, ptr);
                // fullsnes "Normal Jumps": "For JMP [nnnn] the operand word cannot cross page
                // boundaries, ie. JMP [03FFh] would fetch the MSB from [0300h]".
                let hi_ptr = (ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF);
                let hi = self.peek_bank0(bus, hi_ptr);
                let target = u16::from_le_bytes([lo, hi]);
                self.stage_pointer(u32::from(ptr), u32::from(target));
                Addr::new(0, target)
            }
            Mode::AbsIndX => {
                let base = self.fetch16(bus);
                let ptr = base.wrapping_add(self.x);
                let addr = Addr::new(self.pb, ptr);
                let lo = bus.read(addr);
                let hi = bus.read(addr.next());
                let target = u16::from_le_bytes([lo, hi]);
                self.stage_pointer(addr.0, u32::from(target));
                Addr::new(0, target)
            }
            Mode::AbsIndLong => {
                let ptr = self.fetch16(bus);
                let lo = self.peek_bank0(bus, ptr);
                let hi = self.peek_bank0(bus, ptr.wrapping_add(1));
                let bank = self.peek_bank0(bus, ptr.wrapping_add(2));
                let value = u32::from(lo) | (u32::from(hi) << 8) | (u32::from(bank) << 16);
                self.stage_pointer(u32::from(ptr), value);
                Addr::new(bank, u16::from_le_bytes([lo, hi]))
            }
            Mode::Imp | Mode::Acc | Mode::Imm | Mode::Rel | Mode::RelLong | Mode::Block => {
                Addr::new(0, self.pc)
            }
        };
        cycles += dp_penalty;
        Cpu::internal(bus, cycles);
        addr
    }

    /// Reads a pointer byte from bank `$00`, where every direct-page and `JMP` pointer lives.
    fn peek_bank0(&mut self, bus: &mut Bus, offset: u16) -> u8 {
        bus.read(Addr::new(0, offset))
    }

    /// Stages the pointer an indirect transfer read, for `Cpu::record`. No-op — one predictable
    /// branch — while the diagnosis ring is off, which is every run that is not a harness.
    fn stage_pointer(&mut self, ptr: u32, value: u32) {
        if self.trace.is_some() {
            self.trace_ptr = ptr;
            self.trace_ptr_value = value;
        }
    }

    /// The direct-page operand byte, added to `D`.
    fn fetch_dp(&mut self, bus: &mut Bus) -> u16 {
        let n = self.fetch8(bus);
        self.direct_page(n)
    }

    /// `D + n` inside bank `$00`, wrapping at `$FFFF` (fullsnes "Zero Page": the offset is "used
    /// even for nn+X and nn+Y", so the index and the add stay inside the 64 KiB bank).
    fn direct_page(&self, n: u8) -> u16 {
        self.d.wrapping_add(u16::from(n))
    }

    /// `D`'s low byte, whose non-zero value costs the extra direct-page cycle.
    fn direct_page_offset(&self) -> u8 {
        self.d as u8
    }

    /// True when an indexed read costs its extra cycle: a 16-bit index (fullsnes's "X=0" case) or a
    /// carry out of the low byte.
    fn indexed_read_penalty(&self, base: u16, addr: u16) -> bool {
        !self.x8() || (base & 0xFF00) != (addr & 0xFF00)
    }
}
