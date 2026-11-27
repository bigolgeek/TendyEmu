//! The 256-entry 65816 opcode table and the instruction bodies.
//!
//! [`TABLE`] maps every opcode to the operation it performs and the mode its operand uses; the
//! cycle figures in [`base_cycles`] are fullsnes "CPU Clock Cycles" and the per-opcode list under
//! "CPU Opcode Overview" (the `Clks` column), both transcribed as 8-bit, no-penalty counts that the
//! addressing modes then add to.
//!
//! An instruction's clocks are never counted here in one place — the accesses are charged by
//! `Bus::read`/`Bus::write` as they happen and the internal cycles by `Cpu::internal`, which is
//! One ticking rule. `base_cycles` exists so a test can hold the table against the
//! source, not so the emulator can add it up.

use crate::addr::Addr;
use crate::bus::Bus;
use crate::cpu::addressing::{Access, Mode};
use crate::cpu::alu::{self, Shift};
use crate::cpu::{Cpu, Flags, Transfer, TransferKind, Vector, indirect_code};

/// What an opcode does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Ora,
    And,
    Eor,
    Adc,
    Sta,
    Lda,
    Cmp,
    Sbc,
    Bit,
    Stx,
    Stz,
    Sty,
    Ldx,
    Ldy,
    Cpx,
    Cpy,
    Tsb,
    Trb,
    Asl,
    Lsr,
    Rol,
    Ror,
    Inc,
    Dec,
    Inx,
    Iny,
    Dex,
    Dey,
    Brk,
    Cop,
    Wdm,
    Php,
    Plp,
    Pha,
    Pla,
    Phx,
    Plx,
    Phy,
    Ply,
    Phb,
    Plb,
    Phk,
    Phd,
    Pld,
    Pea,
    Pei,
    Per,
    Tcs,
    Tsc,
    Tcd,
    Tdc,
    Txy,
    Tyx,
    Tax,
    Txa,
    Tay,
    Tya,
    Tsx,
    Txs,
    Clc,
    Sec,
    Cli,
    Sei,
    Clv,
    Cld,
    Sed,
    Rep,
    Sep,
    Xce,
    Xba,
    Nop,
    Wai,
    Stp,
    Jmp,
    Jml,
    Jsr,
    Jsl,
    Rts,
    Rtl,
    Rti,
    Bra,
    Brl,
    Bpl,
    Bmi,
    Bvc,
    Bvs,
    Bcc,
    Bcs,
    Bne,
    Beq,
    Mvn,
    Mvp,
}

/// One row of the opcode table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Desc {
    pub op: Op,
    pub mode: Mode,
}

use Mode::{
    Abs, AbsInd, AbsIndLong, AbsIndX, AbsX, AbsY, Acc, Block, Dp, DpX, DpY, Imm, Imp, IndDp,
    IndDpX, IndDpY, IndLong, IndLongY, Long, LongX, Rel, RelLong, Sr, SrY,
};
use Op::*;

/// The opcode matrix: sixteen rows of sixteen, the high nibble of the opcode choosing the row.
///
/// Every entry appears exactly once, which is what makes "all 256 opcodes have a handler" a
/// property of the table rather than a claim.
#[rustfmt::skip]
pub const TABLE: [Desc; 256] = [
    // 0x00
    d(Brk, Imm), d(Ora, IndDpX), d(Cop, Imm), d(Ora, Sr), d(Tsb, Dp), d(Ora, Dp), d(Asl, Dp), d(Ora, IndLong),
    d(Php, Imp), d(Ora, Imm), d(Asl, Acc), d(Phd, Imp), d(Tsb, Abs), d(Ora, Abs), d(Asl, Abs), d(Ora, Long),
    // 0x10
    d(Bpl, Rel), d(Ora, IndDpY), d(Ora, IndDp), d(Ora, SrY), d(Trb, Dp), d(Ora, DpX), d(Asl, DpX), d(Ora, IndLongY),
    d(Clc, Imp), d(Ora, AbsY), d(Inc, Acc), d(Tcs, Imp), d(Trb, Abs), d(Ora, AbsX), d(Asl, AbsX), d(Ora, LongX),
    // 0x20
    d(Jsr, Abs), d(And, IndDpX), d(Jsl, Long), d(And, Sr), d(Bit, Dp), d(And, Dp), d(Rol, Dp), d(And, IndLong),
    d(Plp, Imp), d(And, Imm), d(Rol, Acc), d(Pld, Imp), d(Bit, Abs), d(And, Abs), d(Rol, Abs), d(And, Long),
    // 0x30
    d(Bmi, Rel), d(And, IndDpY), d(And, IndDp), d(And, SrY), d(Bit, DpX), d(And, DpX), d(Rol, DpX), d(And, IndLongY),
    d(Sec, Imp), d(And, AbsY), d(Dec, Acc), d(Tsc, Imp), d(Bit, AbsX), d(And, AbsX), d(Rol, AbsX), d(And, LongX),
    // 0x40
    d(Rti, Imp), d(Eor, IndDpX), d(Wdm, Imm), d(Eor, Sr), d(Mvp, Block), d(Eor, Dp), d(Lsr, Dp), d(Eor, IndLong),
    d(Pha, Imp), d(Eor, Imm), d(Lsr, Acc), d(Phk, Imp), d(Jmp, Abs), d(Eor, Abs), d(Lsr, Abs), d(Eor, Long),
    // 0x50
    d(Bvc, Rel), d(Eor, IndDpY), d(Eor, IndDp), d(Eor, SrY), d(Mvn, Block), d(Eor, DpX), d(Lsr, DpX), d(Eor, IndLongY),
    d(Cli, Imp), d(Eor, AbsY), d(Phy, Imp), d(Tcd, Imp), d(Jml, Long), d(Eor, AbsX), d(Lsr, AbsX), d(Eor, LongX),
    // 0x60
    d(Rts, Imp), d(Adc, IndDpX), d(Per, RelLong), d(Adc, Sr), d(Stz, Dp), d(Adc, Dp), d(Ror, Dp), d(Adc, IndLong),
    d(Pla, Imp), d(Adc, Imm), d(Ror, Acc), d(Rtl, Imp), d(Jmp, AbsInd), d(Adc, Abs), d(Ror, Abs), d(Adc, Long),
    // 0x70
    d(Bvs, Rel), d(Adc, IndDpY), d(Adc, IndDp), d(Adc, SrY), d(Stz, DpX), d(Adc, DpX), d(Ror, DpX), d(Adc, IndLongY),
    d(Sei, Imp), d(Adc, AbsY), d(Ply, Imp), d(Tdc, Imp), d(Jmp, AbsIndX), d(Adc, AbsX), d(Ror, AbsX), d(Adc, LongX),
    // 0x80
    d(Bra, Rel), d(Sta, IndDpX), d(Brl, RelLong), d(Sta, Sr), d(Sty, Dp), d(Sta, Dp), d(Stx, Dp), d(Sta, IndLong),
    d(Dey, Imp), d(Bit, Imm), d(Txa, Imp), d(Phb, Imp), d(Sty, Abs), d(Sta, Abs), d(Stx, Abs), d(Sta, Long),
    // 0x90
    d(Bcc, Rel), d(Sta, IndDpY), d(Sta, IndDp), d(Sta, SrY), d(Sty, DpX), d(Sta, DpX), d(Stx, DpY), d(Sta, IndLongY),
    d(Tya, Imp), d(Sta, AbsY), d(Txs, Imp), d(Txy, Imp), d(Stz, Abs), d(Sta, AbsX), d(Stz, AbsX), d(Sta, LongX),
    // 0xA0
    d(Ldy, Imm), d(Lda, IndDpX), d(Ldx, Imm), d(Lda, Sr), d(Ldy, Dp), d(Lda, Dp), d(Ldx, Dp), d(Lda, IndLong),
    d(Tay, Imp), d(Lda, Imm), d(Tax, Imp), d(Plb, Imp), d(Ldy, Abs), d(Lda, Abs), d(Ldx, Abs), d(Lda, Long),
    // 0xB0
    d(Bcs, Rel), d(Lda, IndDpY), d(Lda, IndDp), d(Lda, SrY), d(Ldy, DpX), d(Lda, DpX), d(Ldx, DpY), d(Lda, IndLongY),
    d(Clv, Imp), d(Lda, AbsY), d(Tsx, Imp), d(Tyx, Imp), d(Ldy, AbsX), d(Lda, AbsX), d(Ldx, AbsY), d(Lda, LongX),
    // 0xC0
    d(Cpy, Imm), d(Cmp, IndDpX), d(Rep, Imm), d(Cmp, Sr), d(Cpy, Dp), d(Cmp, Dp), d(Dec, Dp), d(Cmp, IndLong),
    d(Iny, Imp), d(Cmp, Imm), d(Dex, Imp), d(Wai, Imp), d(Cpy, Abs), d(Cmp, Abs), d(Dec, Abs), d(Cmp, Long),
    // 0xD0
    d(Bne, Rel), d(Cmp, IndDpY), d(Cmp, IndDp), d(Cmp, SrY), d(Pei, Dp), d(Cmp, DpX), d(Dec, DpX), d(Cmp, IndLongY),
    d(Cld, Imp), d(Cmp, AbsY), d(Phx, Imp), d(Stp, Imp), d(Jml, AbsIndLong), d(Cmp, AbsX), d(Dec, AbsX), d(Cmp, LongX),
    // 0xE0
    d(Cpx, Imm), d(Sbc, IndDpX), d(Sep, Imm), d(Sbc, Sr), d(Cpx, Dp), d(Sbc, Dp), d(Inc, Dp), d(Sbc, IndLong),
    d(Inx, Imp), d(Sbc, Imm), d(Nop, Imp), d(Xba, Imp), d(Cpx, Abs), d(Sbc, Abs), d(Inc, Abs), d(Sbc, Long),
    // 0xF0
    d(Beq, Rel), d(Sbc, IndDpY), d(Sbc, IndDp), d(Sbc, SrY), d(Pea, Abs), d(Sbc, DpX), d(Inc, DpX), d(Sbc, IndLongY),
    d(Sed, Imp), d(Sbc, AbsY), d(Plx, Imp), d(Xce, Imp), d(Jsr, AbsIndX), d(Sbc, AbsX), d(Inc, AbsX), d(Sbc, LongX),
];

/// Shorthand for one table row.
const fn d(op: Op, mode: Mode) -> Desc {
    Desc { op, mode }
}

/// The row an opcode selects.
pub fn table_entry(opcode: u8) -> Desc {
    TABLE[opcode as usize]
}

/// The source's cycle count for this row, at 8-bit widths with no direct-page or page-crossing
/// penalty — fullsnes "CPU Clock Cycles" and the `Clks` column of "CPU Opcode Overview".
///
/// The 16-bit data cycle (`d`/`dd`), the direct-page penalty (`n`) and the indexed-read and
/// emulation-branch penalties (`y`, `x`) are added by the addressing modes as real accesses and
/// internal cycles, so they are not in this number.
pub fn base_cycles(desc: Desc) -> u32 {
    let read = |mode: Mode| match mode {
        Imm => 2,
        Dp => 3,
        DpX | DpY | Abs | AbsX | AbsY | Sr => 4,
        Long | LongX | IndDp | IndDpY => 5,
        IndDpX | IndLong | IndLongY => 6,
        SrY => 7,
        _ => 0,
    };
    let write = |mode: Mode| match mode {
        Dp => 3,
        DpX | DpY | Abs | Sr => 4,
        AbsX | AbsY | Long | LongX | IndDp => 5,
        IndDpX | IndDpY | IndLong | IndLongY => 6,
        SrY => 7,
        _ => 0,
    };
    let rmw = |mode: Mode| match mode {
        Acc => 2,
        Dp => 5,
        DpX => 6,
        Abs => 6,
        AbsX => 7,
        _ => 0,
    };
    match desc.op {
        Ora | And | Eor | Adc | Sbc | Cmp | Lda | Ldx | Ldy | Bit => read(desc.mode),
        Sta | Stx | Sty | Stz => write(desc.mode),
        Tsb | Trb => {
            if desc.mode == Dp {
                5
            } else {
                6
            }
        }
        Asl | Lsr | Rol | Ror | Inc | Dec => rmw(desc.mode),
        Brk | Cop => 7,
        Wdm => 2,
        Php | Pha | Phx | Phy | Phb | Phk => 3,
        Plp | Pla | Plx | Ply | Plb => 4,
        Phd => 4,
        Pld => 5,
        Pea => 5,
        Pei => 6,
        Per => 6,
        Rts | Rtl | Rti => 6,
        Jsr => {
            if desc.mode == Abs {
                6
            } else {
                8
            }
        }
        Jsl => 8,
        Jmp => match desc.mode {
            Abs => 3,
            Long => 4,
            AbsInd => 5,
            _ => 6,
        },
        Jml => {
            if desc.mode == Long {
                4
            } else {
                6
            }
        }
        Bra => 3,
        Brl => 4,
        Bpl | Bmi | Bvc | Bvs | Bcc | Bcs | Bne | Beq => 2,
        Mvn | Mvp => 7,
        Xba | Wai | Stp => 3,
        Rep | Sep => 3,
        _ => 2,
    }
}

impl Cpu {
    /// Runs one opcode. Every access charges itself; `Cpu::internal` charges the internal cycles.
    pub fn execute(&mut self, bus: &mut Bus, opcode: u8) {
        let desc = TABLE[usize::from(opcode)];
        // Where the instruction is, for the transfer record: `fetch8` has already stepped `pc` past
        // the opcode byte, and the program bank cannot change until a long jump does it.
        let (pc0, pb0) = (self.pc.wrapping_sub(1), self.pb);
        match desc.op {
            Ora | And | Eor | Adc | Sbc | Cmp => self.alu_op(bus, desc),
            Bit => self.bit(bus, desc.mode),
            Lda => self.load(bus, desc.mode, Lda),
            Ldx => self.load(bus, desc.mode, Ldx),
            Ldy => self.load(bus, desc.mode, Ldy),
            Sta => {
                let value = self.a;
                self.store(bus, desc.mode, value, self.m_mask());
            }
            Stz => self.store(bus, desc.mode, 0, self.m_mask()),
            Stx => {
                let value = self.x;
                self.store(bus, desc.mode, value, self.x_mask());
            }
            Sty => {
                let value = self.y;
                self.store(bus, desc.mode, value, self.x_mask());
            }
            Cpx | Cpy => self.compare_index(bus, desc),
            Inx | Iny | Dex | Dey => self.index_step(bus, desc.op),
            Tsb | Trb | Asl | Lsr | Rol | Ror | Inc | Dec => self.rmw(bus, desc),
            Brk | Cop => {
                // fullsnes "Interrupts, Exceptions, Breakpoints": the return address "skips one
                // dummy/parameter byte following after the BRK opcode".
                self.fetch8(bus);
                let vector = if desc.op == Brk {
                    Vector::Brk
                } else {
                    Vector::Cop
                };
                self.interrupt(bus, vector, true);
            }
            Wdm => {
                self.fetch8(bus);
            }
            Nop => Cpu::internal(bus, 1),
            Php => {
                let byte = self.p.pushed(self.e, true);
                self.push8(bus, byte);
                Cpu::internal(bus, 1);
            }
            Plp => {
                let byte = self.pull8(bus);
                self.p = Flags::pulled(byte, self.e);
                // fullsnes "CPU Registers and Flags", Index Register Flag (X): "Switches X and Y to
                // 8bit mode (the upper 8bit are forcefully set to 00h)"; 6502.org, *65C816 Opcodes*,
                // §6.10.4: "when the e flag is 1, the m and x flags are forced to 1 (and
                // consequently the XH and YH registers are forced to $00)". The pulled x bit is a
                // width change like `REP`/`SEP`/`XCE`'s, so it owes the same `apply_width`.
                self.apply_width();
                Cpu::internal(bus, 2);
            }
            Pha => {
                let value = self.a;
                self.push_n(bus, value, self.m_mask());
                Cpu::internal(bus, 1);
            }
            Pla => {
                let mask = self.m_mask();
                let value = self.pull_n(bus, mask);
                self.set_a(value);
                self.set_nz(value, mask);
                Cpu::internal(bus, 2);
            }
            Phx => {
                let value = self.x;
                self.push_n(bus, value, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Phy => {
                let value = self.y;
                self.push_n(bus, value, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Plx => {
                let mask = self.x_mask();
                let value = self.pull_n(bus, mask);
                self.set_x(value);
                self.set_nz(self.x, mask);
                Cpu::internal(bus, 2);
            }
            Ply => {
                let mask = self.x_mask();
                let value = self.pull_n(bus, mask);
                self.set_y(value);
                self.set_nz(self.y, mask);
                Cpu::internal(bus, 2);
            }
            Phb => {
                let value = self.db;
                self.push8(bus, value);
                Cpu::internal(bus, 1);
            }
            Phk => {
                let value = self.pb;
                self.push8(bus, value);
                Cpu::internal(bus, 1);
            }
            Phd => {
                let value = self.d;
                self.push_n(bus, value, 0xFFFF);
                Cpu::internal(bus, 1);
            }
            Plb => {
                // fullsnes's 65816 instruction table: "PLB POP DB 8 DB=[S]" — one byte, unlike
                // PLD, whose row is "PLD POP D 16". Pulling a word here leaves the stack one byte
                // out for the `PHB / PHK / PLB / JSL / ... / PLB / RTL` idiom (SMW's `$01:B7B3`),
                // and the RTL then returns into whatever the shift landed on. The dev wiki's table
                // agrees (`AB | Stack (Pull) | 1 bytes | 4 cycles`). The two internal cycles are
                // `PLP`'s, the other four-clock one-byte pull in this file.
                let value = self.pull8(bus);
                self.db = value;
                self.set_nz(value.into(), 0x00FF);
                Cpu::internal(bus, 2);
            }
            Pld => {
                let value = self.pull_n(bus, 0xFFFF);
                self.d = value;
                self.set_nz(value, 0xFFFF);
                Cpu::internal(bus, 2);
            }
            Pea => {
                let value = self.fetch16(bus);
                self.push_n(bus, value, 0xFFFF);
            }
            Pei => {
                let addr = self.resolve(bus, desc.mode, Access::Read);
                let value = self.read_n(bus, addr, 0xFFFF);
                self.push_n(bus, value, 0xFFFF);
            }
            Per => {
                let disp = self.fetch16(bus);
                let value = self.pc.wrapping_add(disp);
                self.push_n(bus, value, 0xFFFF);
                Cpu::internal(bus, 1);
            }
            Tcs => {
                self.s = self.stack_word(self.a);
                Cpu::internal(bus, 1);
            }
            Tsc => {
                self.a = self.s;
                self.set_nz(self.a, 0xFFFF);
                Cpu::internal(bus, 1);
            }
            Tcd => {
                self.d = self.a;
                self.set_nz(self.d, 0xFFFF);
                Cpu::internal(bus, 1);
            }
            Tdc => {
                self.a = self.d;
                self.set_nz(self.a, 0xFFFF);
                Cpu::internal(bus, 1);
            }
            Txy => {
                let value = self.x;
                self.set_y(value);
                self.set_nz(self.y, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Tyx => {
                let value = self.y;
                self.set_x(value);
                self.set_nz(self.x, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Tax => {
                let value = self.a;
                self.set_x(value);
                self.set_nz(self.x, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Tay => {
                let value = self.a;
                self.set_y(value);
                self.set_nz(self.y, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Txa => {
                let value = self.x;
                self.set_a(value);
                self.set_nz(value, self.m_mask());
                Cpu::internal(bus, 1);
            }
            Tya => {
                let value = self.y;
                self.set_a(value);
                self.set_nz(value, self.m_mask());
                Cpu::internal(bus, 1);
            }
            Tsx => {
                self.set_x(self.s);
                self.set_nz(self.x, self.x_mask());
                Cpu::internal(bus, 1);
            }
            Txs => {
                self.s = self.stack_word(self.x);
                Cpu::internal(bus, 1);
            }
            Clc => {
                self.p.set_c(false);
                Cpu::internal(bus, 1);
            }
            Sec => {
                self.p.set_c(true);
                Cpu::internal(bus, 1);
            }
            Cli => {
                self.p.set_i(false);
                Cpu::internal(bus, 1);
            }
            Sei => {
                self.p.set_i(true);
                Cpu::internal(bus, 1);
            }
            Clv => {
                self.p.set_v(false);
                Cpu::internal(bus, 1);
            }
            Cld => {
                self.p.set_d(false);
                Cpu::internal(bus, 1);
            }
            Sed => {
                self.p.set_d(true);
                Cpu::internal(bus, 1);
            }
            Rep => {
                let mask = self.fetch8(bus);
                self.p.0 &= !mask;
                self.apply_width();
                Cpu::internal(bus, 1);
            }
            Sep => {
                let mask = self.fetch8(bus);
                self.p.0 |= mask;
                self.apply_width();
                Cpu::internal(bus, 1);
            }
            Xce => {
                let carry = self.p.c();
                let emulation = self.e;
                self.e = carry;
                self.p.set_c(emulation);
                if self.e {
                    self.p.set_m(true);
                    self.p.set_x(true);
                    self.s = 0x0100 | (self.s & 0x00FF);
                }
                self.apply_width();
                Cpu::internal(bus, 1);
            }
            Xba => {
                self.a = self.a.rotate_left(8);
                self.set_nz(self.a, 0x00FF);
                Cpu::internal(bus, 2);
            }
            Wai => {
                self.waiting = true;
                Cpu::internal(bus, 2);
            }
            Stp => {
                self.stopped = true;
                Cpu::internal(bus, 2);
            }
            Jmp | Jml => {
                let addr = self.resolve(bus, desc.mode, Access::Read);
                match desc.mode {
                    Long | AbsIndLong => {
                        self.pb = addr.bank();
                        self.pc = addr.offset();
                    }
                    _ => self.pc = addr.offset(),
                }
                let event = Transfer {
                    kind: if desc.op == Jml {
                        TransferKind::Jml
                    } else {
                        TransferKind::Jmp
                    },
                    opcode,
                    indirect: indirect_code(desc.mode),
                    pb: pb0,
                    pc: pc0,
                    target_bank: self.pb,
                    target_pc: self.pc,
                    return_bank: pb0,
                    s_before: self.s,
                    ptr: self.trace_ptr,
                    ptr_value: self.trace_ptr_value,
                    ..self.transfer_fields()
                };
                self.record(bus, event);
            }
            Jsr | Jsl => {
                let s_before = self.s;
                let addr = self.resolve(bus, desc.mode, Access::Read);
                // The pushed return address is the last byte of the operand, so `RTS`/`RTL` can
                // simply add one (fullsnes "Normal Jumps": "JSR nnnn ... [S]=PC+2" is the value
                // after the increment, i.e. the opcode's own last byte plus one).
                let return_addr = self.pc.wrapping_sub(1);
                if desc.mode == Long {
                    self.push8(bus, self.pb);
                }
                self.push8(bus, (return_addr >> 8) as u8);
                self.push8(bus, return_addr as u8);
                if desc.mode == Long {
                    self.pb = addr.bank();
                }
                self.pc = addr.offset();
                Cpu::internal(bus, 1);
                let event = Transfer {
                    kind: if desc.op == Jsl {
                        TransferKind::Jsl
                    } else {
                        TransferKind::Jsr
                    },
                    opcode,
                    indirect: indirect_code(desc.mode),
                    pb: pb0,
                    pc: pc0,
                    target_bank: self.pb,
                    target_pc: self.pc,
                    return_bank: pb0,
                    return_pc: return_addr,
                    s_before,
                    ptr: self.trace_ptr,
                    ptr_value: self.trace_ptr_value,
                    ..self.transfer_fields()
                };
                self.record(bus, event);
            }
            Rts => {
                let s_before = self.s;
                let lo = self.pull8(bus);
                let hi = self.pull8(bus);
                let pulled = u16::from_le_bytes([lo, hi]);
                self.pc = pulled.wrapping_add(1);
                Cpu::internal(bus, 3);
                let event = Transfer {
                    kind: TransferKind::Rts,
                    opcode,
                    pb: pb0,
                    pc: pc0,
                    target_bank: self.pb,
                    target_pc: self.pc,
                    return_bank: self.pb,
                    return_pc: pulled,
                    s_before,
                    ..self.transfer_fields()
                };
                self.record(bus, event);
            }
            Rtl => {
                let s_before = self.s;
                let lo = self.pull8(bus);
                let hi = self.pull8(bus);
                let bank = self.pull8(bus);
                let pulled = u16::from_le_bytes([lo, hi]);
                self.pc = pulled.wrapping_add(1);
                self.pb = bank;
                Cpu::internal(bus, 2);
                let event = Transfer {
                    kind: TransferKind::Rtl,
                    opcode,
                    pb: pb0,
                    pc: pc0,
                    target_bank: self.pb,
                    target_pc: self.pc,
                    return_bank: bank,
                    return_pc: pulled,
                    s_before,
                    ..self.transfer_fields()
                };
                self.record(bus, event);
            }
            Rti => {
                let s_before = self.s;
                let p = self.pull8(bus);
                self.p = Flags::pulled(p, self.e);
                // The pulled x bit decides the index width before the handler's next index is
                // added, so `RTI` applies the same rule as `PLP` (fullsnes "Index Register Flag
                // (X)": "the upper 8bit are forcefully set to 00h").
                self.apply_width();
                let lo = self.pull8(bus);
                let hi = self.pull8(bus);
                let pulled = u16::from_le_bytes([lo, hi]);
                self.pc = pulled;
                if !self.e {
                    self.pb = self.pull8(bus);
                }
                Cpu::internal(bus, 2);
                let event = Transfer {
                    kind: TransferKind::Rti,
                    opcode,
                    pb: pb0,
                    pc: pc0,
                    target_bank: self.pb,
                    target_pc: self.pc,
                    return_bank: self.pb,
                    return_pc: pulled,
                    s_before,
                    ..self.transfer_fields()
                };
                self.record(bus, event);
            }
            Bra => self.branch(bus, opcode, true, pc0, pb0),
            Brl => {
                let disp = self.fetch16(bus);
                self.pc = self.pc.wrapping_add(disp);
                Cpu::internal(bus, 1);
                if self.trace.is_some() {
                    // `BRL` is the 16-bit relative branch: unconditional, so `taken` is always true
                    // and the whole displacement is recorded rather than an 8-bit sign extension.
                    let event = Transfer {
                        kind: TransferKind::Branch,
                        opcode,
                        pb: pb0,
                        pc: pc0,
                        target_bank: pb0,
                        target_pc: self.pc,
                        taken: true,
                        disp: disp as i16,
                        s_before: self.s,
                        ..self.transfer_fields()
                    };
                    self.record(bus, event);
                }
            }
            Bpl | Bmi | Bvc | Bvs | Bcc | Bcs | Bne | Beq => {
                let taken = match desc.op {
                    Bpl => !self.p.n(),
                    Bmi => self.p.n(),
                    Bvc => !self.p.v(),
                    Bvs => self.p.v(),
                    Bcc => !self.p.c(),
                    Bcs => self.p.c(),
                    Bne => !self.p.z(),
                    _ => self.p.z(),
                };
                self.branch(bus, opcode, taken, pc0, pb0);
            }
            Mvn | Mvp => self.block_move(bus, desc.op == Mvn),
        }
    }

    /// `ORA`/`AND`/`EOR`/`ADC`/`SBC`/`CMP`: one operand, then the accumulator.
    fn alu_op(&mut self, bus: &mut Bus, desc: Desc) {
        let mask = self.m_mask();
        let operand = self.operand(bus, desc.mode, mask);
        match desc.op {
            Ora => {
                let value = self.a | operand;
                self.set_a(value);
                self.set_nz(value, mask);
            }
            And => {
                let value = self.a & operand;
                self.set_a(value);
                self.set_nz(value, mask);
            }
            Eor => {
                let value = self.a ^ operand;
                self.set_a(value);
                self.set_nz(value, mask);
            }
            Adc => {
                let r = alu::adc(self.a, operand, self.p.c(), self.p.d(), mask);
                self.set_a(r.value);
                self.set_flags(r);
            }
            Sbc => {
                let r = alu::sbc(self.a, operand, self.p.c(), self.p.d(), mask);
                self.set_a(r.value);
                self.set_flags(r);
            }
            _ => {
                // CMP is a subtraction whose result is thrown away; `V` is not its flag.
                let r = alu::sbc(self.a, operand, true, false, mask);
                self.p.set_c(r.carry);
                self.p.set_n(r.negative);
                self.p.set_z(r.zero);
            }
        }
    }

    /// `BIT`: `Z` from `A AND M`, `N` and `V` from the operand itself (fullsnes "Bit Test"):
    /// "Flags are set as Z=((A AND op)=0), and N=op.Bit(MSB), and V=op.Bit(MSB-1)". `BIT #nn` sets
    /// only `Z` (its `Flags` column is `-z----`).
    fn bit(&mut self, bus: &mut Bus, mode: Mode) {
        let mask = self.m_mask();
        let operand = self.operand(bus, mode, mask);
        self.p.set_z(self.a & operand & mask == 0);
        if mode != Imm {
            let sign = mask & !(mask >> 1);
            self.p.set_n(operand & sign != 0);
            self.p.set_v(operand & (sign >> 1) != 0);
        }
    }

    /// `LDA`/`LDX`/`LDY`.
    fn load(&mut self, bus: &mut Bus, mode: Mode, which: Op) {
        let mask = if which == Lda {
            self.m_mask()
        } else {
            self.x_mask()
        };
        let value = self.operand(bus, mode, mask);
        match which {
            Lda => self.set_a(value),
            Ldx => self.set_x(value),
            _ => self.set_y(value),
        }
        self.set_nz(value, mask);
    }

    /// `STA`/`STX`/`STY`/`STZ`.
    fn store(&mut self, bus: &mut Bus, mode: Mode, value: u16, mask: u16) {
        let addr = self.resolve(bus, mode, Access::Write);
        self.write_n(bus, addr, value, mask);
    }

    /// `INX`/`INY`/`DEX`/`DEY`: two cycles, the fetch plus one internal.
    fn index_step(&mut self, bus: &mut Bus, op: Op) {
        let mask = self.x_mask();
        match op {
            Inx => {
                let value = self.x.wrapping_add(1);
                self.set_x(value);
                self.set_nz(self.x, mask);
            }
            Iny => {
                let value = self.y.wrapping_add(1);
                self.set_y(value);
                self.set_nz(self.y, mask);
            }
            Dex => {
                let value = self.x.wrapping_sub(1);
                self.set_x(value);
                self.set_nz(self.x, mask);
            }
            _ => {
                let value = self.y.wrapping_sub(1);
                self.set_y(value);
                self.set_nz(self.y, mask);
            }
        }
        Cpu::internal(bus, 1);
    }

    /// `CPX`/`CPY`.
    fn compare_index(&mut self, bus: &mut Bus, desc: Desc) {
        let mask = self.x_mask();
        let operand = self.operand(bus, desc.mode, mask);
        let register = if desc.op == Cpx { self.x } else { self.y };
        let r = alu::sbc(register, operand, true, false, mask);
        self.p.set_c(r.carry);
        self.p.set_n(r.negative);
        self.p.set_z(r.zero);
    }

    /// The read-modify-write group, on A or on memory.
    fn rmw(&mut self, bus: &mut Bus, desc: Desc) {
        let mask = self.m_mask();
        let accumulator = desc.mode == Acc;
        let addr = if accumulator {
            Addr::new(0, 0)
        } else {
            self.resolve(bus, desc.mode, Access::Rmw)
        };
        let value = if accumulator {
            self.a & mask
        } else {
            self.read_n(bus, addr, mask)
        };
        let sign = mask & !(mask >> 1);
        let (result, carry) = match desc.op {
            Asl => alu::shift(Shift::Asl, value, self.p.c(), mask),
            Lsr => alu::shift(Shift::Lsr, value, self.p.c(), mask),
            Rol => alu::shift(Shift::Rol, value, self.p.c(), mask),
            Ror => alu::shift(Shift::Ror, value, self.p.c(), mask),
            Inc => ((value.wrapping_add(1)) & mask, self.p.c()),
            Dec => ((value.wrapping_sub(1)) & mask, self.p.c()),
            Tsb => {
                self.p.set_z(value & self.a & mask == 0);
                (value | (self.a & mask), self.p.c())
            }
            _ => {
                self.p.set_z(value & self.a & mask == 0);
                (value & !self.a & mask, self.p.c())
            }
        };
        if matches!(desc.op, Asl | Lsr | Rol | Ror | Inc | Dec) {
            self.p.set_c(carry);
            self.p.set_z(result & mask == 0);
            self.p.set_n(result & sign != 0);
        }
        if accumulator {
            self.set_a(result);
            Cpu::internal(bus, 1);
        } else {
            self.write_n(bus, addr, result, mask);
        }
    }

    /// A conditional branch: two cycles when not taken, three when taken, and one more in emulation
    /// mode when the target leaves the current page (fullsnes "CPU Clock Cycles": "CPNx 3,4
    /// relative 8bit jump ;+x if E=1 and crossing 100h-boundary").
    ///
    /// `taken` is the flag test the opcode already made; this records the decision in the transfer
    /// ring when tracing is on. A branch is not a control transfer in the ring's original sense, but
    /// it is the other half of the same question — where the CPU's time goes — and it shares the
    /// ring rather than growing a second mechanism.
    fn branch(&mut self, bus: &mut Bus, opcode: u8, taken: bool, pc0: u16, pb0: u8) {
        let p_before = self.p.0;
        let s_before = self.s;
        let disp = i16::from(self.fetch8(bus) as i8);
        let mut cycles = 0;
        if taken {
            let from = self.pc;
            let to = from.wrapping_add(disp as u16);
            cycles += 1;
            if self.e && (from & 0xFF00) != (to & 0xFF00) {
                cycles += 1;
            }
            self.pc = to;
        }
        Cpu::internal(bus, cycles);
        if self.trace.is_some() {
            let event = Transfer {
                kind: TransferKind::Branch,
                opcode,
                pb: pb0,
                pc: pc0,
                target_bank: pb0,
                target_pc: self.pc,
                taken,
                disp,
                s_before,
                p: p_before,
                ..self.transfer_fields()
            };
            self.record(bus, event);
        }
    }

    /// `MVN`/`MVP`: one byte per execution (fullsnes "Memory Block Transfer Commands"), charging the
    /// byte's read and write separately (rule 2) plus the five internal cycles that make
    /// up the source's seven (`Clks` column: "7x").
    fn block_move(&mut self, bus: &mut Bus, increment: bool) {
        let dest = self.fetch8(bus);
        let src = self.fetch8(bus);
        self.db = dest;
        let value = bus.read(Addr::new(src, self.x));
        bus.write(Addr::new(dest, self.y), value);
        let step = if increment { 1u16 } else { 0xFFFF };
        self.x = self.x.wrapping_add(step) & self.x_mask();
        self.y = self.y.wrapping_add(step) & self.x_mask();
        self.a = self.a.wrapping_sub(1);
        // fullsnes gives the block move as "7x" per byte. Re-entering the opcode means the byte is
        // five accesses — the opcode and its two operands, the read and the write — so two internal
        // cycles complete the seven. (Re-entry is what keeps one `step` from ever being handed a
        // whole block's clocks.)
        Cpu::internal(bus, 2);
        if self.a != 0xFFFF {
            self.pc = self.pc.wrapping_sub(3);
        }
    }

    // ---- small helpers ------------------------------------------------------------------------

    /// The operand of `mode` at the width `mask`, immediate included.
    fn operand(&mut self, bus: &mut Bus, mode: Mode, mask: u16) -> u16 {
        if mode == Imm {
            let lo = self.fetch8(bus);
            if mask == 0x00FF {
                u16::from(lo)
            } else {
                let hi = self.fetch8(bus);
                u16::from_le_bytes([lo, hi])
            }
        } else {
            let addr = self.resolve(bus, mode, Access::Read);
            self.read_n(bus, addr, mask)
        }
    }

    fn set_flags(&mut self, r: alu::Alu) {
        self.p.set_c(r.carry);
        self.p.set_v(r.overflow);
        self.p.set_n(r.negative);
        self.p.set_z(r.zero);
    }

    /// `S` after a `TCS`/`TXS`: in emulation mode only the low byte survives and it is page one.
    fn stack_word(&self, value: u16) -> u16 {
        if self.e {
            0x0100 | (value & 0x00FF)
        } else {
            value
        }
    }

    /// Forces the index registers' high bytes to zero after a width change (fullsnes "Index
    /// Register Flag (X)": "the upper 8bit are forcefully set to 00h").
    fn apply_width(&mut self) {
        if self.e {
            // In emulation mode the M and X bits read as 1 whatever `REP`/`SEP` wrote.
            self.p.set_m(true);
            self.p.set_x(true);
            self.s = 0x0100 | (self.s & 0x00FF);
        }
        if self.x8() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }
}
