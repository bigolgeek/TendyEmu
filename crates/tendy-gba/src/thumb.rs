//! Thumb state: the nineteen formats of ARM DDI 0084D chapter 5 and GBATEK's *THUMB Opcodes*.
//!
//! Thumb is not a second instruction set so much as a shorter encoding of the same one, and every
//! operation here ends up in the same helpers [`Cpu`] keeps for ARM state: the barrel shifter, the
//! flag-setting adds and subtracts, the banked register access. What differs is the pipeline (a
//! halfword: `R15` reads at +4, and an `STM` stores address + 6), the register fields, and the
//! handful of formats whose flags are *not* written — `ADD`/`MOV`/`CMP` in the high-register format
//! write no flags except `CMP`, and the `ALU` operations set N/Z/C or N/Z/C/V depending on which
//! one they are.

use crate::bus::Bus;
use crate::cpu::{Cpu, FLAG_C, FLAG_V};

impl Cpu {
    /// One Thumb instruction. `R15` has already been advanced past it by the caller.
    pub(super) fn step_thumb(&mut self, bus: &mut Bus) -> u32 {
        let pc = self.r[15];
        let op = self.pipelined(pc, u32::from(bus.fetch16(pc & !1)));
        self.r[15] = pc.wrapping_add(2);
        // Format 2 before format 1: 00011 shares its top three bits with the shifts.
        if op & 0xF800 == 0x1800 {
            return self.add_sub(op);
        }
        if op & 0xE000 == 0x0000 {
            return self.shift_immediate(op);
        }
        if op & 0xE000 == 0x2000 {
            return self.immediate(op);
        }
        if op & 0xFC00 == 0x4000 {
            return self.alu(op);
        }
        if op & 0xFC00 == 0x4400 {
            return self.high_register(op);
        }
        if op & 0xF800 == 0x4800 {
            return self.load_literal(op, bus);
        }
        if op & 0xF000 == 0x5000 {
            return self.register_transfer(op, bus);
        }
        if op & 0xE000 == 0x6000 {
            return self.immediate_offset(op, bus);
        }
        if op & 0xF000 == 0x8000 {
            return self.halfword_offset(op, bus);
        }
        if op & 0xF000 == 0x9000 {
            return self.sp_offset(op, bus);
        }
        if op & 0xF000 == 0xA000 {
            return self.add_relative(op);
        }
        if op & 0xFF00 == 0xB000 {
            return self.adjust_sp(op);
        }
        if op & 0xFE00 == 0xB400 || op & 0xFE00 == 0xBC00 {
            return self.push_pop(op, bus);
        }
        if op & 0xF000 == 0xC000 {
            return self.thumb_block_transfer(op, bus);
        }
        if op & 0xF000 == 0xD000 {
            let condition = (op >> 8) & 0xF;
            if condition == 0xF {
                return self.software_interrupt(op & 0xFF, bus);
            }
            if condition == 0xE {
                // GBATEK: "Undefined, should not be used".
                return self.undefined_instruction();
            }
            if self.condition(condition) {
                let offset = ((op << 24) as i32 >> 23) as u32;
                self.branch_to(self.pc_read().wrapping_add(offset));
                // GBATEK, *ARM CPU Instruction Cycle Times*: `B,BL 2S+1N` — three bus cycles for a
                // taken branch, and no `I` cycle. The refill's extra cycles are the fetch of the
                // target, which the bus prices as an `N` cycle from the target's own region.
                return 3;
            }
            return 1;
        }
        if op & 0xF800 == 0xE000 {
            let offset = ((op << 21) as i32 >> 20) as u32;
            self.branch_to(self.pc_read().wrapping_add(offset));
            return 3; // B,BL 2S + 1N
        }
        if op & 0xF800 == 0xF000 {
            return self.bl_prefix(op);
        }
        if op & 0xF800 == 0xF800 {
            return self.bl_suffix(op);
        }
        self.undefined_instruction()
    }

    /// Format 1: `LSL`/`LSR`/`ASR Rd, Rs, #imm5`. The zero amount is `LSL #0` (nothing, carry kept)
    /// or `LSR`/`ASR #32`.
    fn shift_immediate(&mut self, op: u32) -> u32 {
        let kind = (op >> 11) & 3;
        let amount = (op >> 6) & 0x1F;
        let rs = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let (value, carry) =
            self.shift_by_immediate(self.r[rs], kind, amount, self.cpsr & FLAG_C != 0);
        self.r[rd] = value;
        self.set_nz(value);
        if kind != 0 || amount != 0 {
            self.set_flag(FLAG_C, carry);
        }
        1
    }

    /// Format 2: `ADD`/`SUB` with a register or a three-bit immediate. All four flags; the `ADD
    /// Rd, Rs, #0` encoding is `MOV` and sets them too.
    fn add_sub(&mut self, op: u32) -> u32 {
        let immediate = op & 0x0400 != 0;
        let subtract = op & 0x0200 != 0;
        let operand = (op >> 6) & 7;
        let rs = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let a = self.r[rs];
        let b = if immediate {
            operand
        } else {
            self.r[operand as usize]
        };
        let (result, carry, overflow) = if subtract {
            self.sub_flags(a, b, true)
        } else {
            self.add_flags(a, b, false)
        };
        self.r[rd] = result;
        self.set_nz(result);
        self.set_flag(FLAG_C, carry);
        self.set_flag(FLAG_V, overflow);
        1
    }

    /// Format 3: `MOV`/`CMP`/`ADD`/`SUB Rd, #imm8`. `MOV` writes N and Z only.
    fn immediate(&mut self, op: u32) -> u32 {
        let rd = ((op >> 8) & 7) as usize;
        let value = op & 0xFF;
        match (op >> 11) & 3 {
            0 => {
                self.r[rd] = value;
                self.set_nz(value);
            }
            1 => {
                let (result, carry, overflow) = self.sub_flags(self.r[rd], value, true);
                self.set_nz(result);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            2 => {
                let (result, carry, overflow) = self.add_flags(self.r[rd], value, false);
                self.r[rd] = result;
                self.set_nz(result);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            _ => {
                let (result, carry, overflow) = self.sub_flags(self.r[rd], value, true);
                self.r[rd] = result;
                self.set_nz(result);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
        }
        1
    }

    /// Format 4: the sixteen `ALU` operations on two low registers.
    fn alu(&mut self, op: u32) -> u32 {
        let operation = (op >> 6) & 0xF;
        let rs = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let a = self.r[rd];
        let b = self.r[rs];
        let carry_in = self.cpsr & FLAG_C != 0;
        let mut cycles = 1;
        match operation {
            0 => {
                self.r[rd] = a & b;
                self.set_nz(a & b);
            }
            1 => {
                self.r[rd] = a ^ b;
                self.set_nz(a ^ b);
            }
            2 | 3 | 4 | 7 => {
                let kind = match operation {
                    2 => 0,
                    3 => 1,
                    4 => 2,
                    _ => 3,
                };
                let (value, carry) = self.shift_by_register(a, kind, b, carry_in);
                self.r[rd] = value;
                self.set_nz(value);
                if b & 0xFF != 0 {
                    self.set_flag(FLAG_C, carry);
                }
                cycles += 1; // 1S + 1I
            }
            5 => {
                let (value, carry, overflow) = self.add_flags(a, b, carry_in);
                self.r[rd] = value;
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            6 => {
                let (value, carry, overflow) = self.sub_flags(a, b, carry_in);
                self.r[rd] = value;
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            8 => {
                // TST sets N and Z only.
                self.set_nz(a & b);
            }
            9 => {
                // NEG is RSB #0.
                let (value, carry, overflow) = self.sub_flags(0, b, true);
                self.r[rd] = value;
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            10 => {
                let (value, carry, overflow) = self.sub_flags(a, b, true);
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            11 => {
                let (value, carry, overflow) = self.add_flags(a, b, false);
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
            }
            12 => {
                self.r[rd] = a | b;
                self.set_nz(a | b);
            }
            13 => {
                let value = a.wrapping_mul(b);
                self.r[rd] = value;
                self.set_nz(value);
                // GBATEK, *THUMB.4*: "1S+mI for MUL on ARMv4 (m=1..4; depending on MSBs of
                // incoming Rd value)", and the summary's `MUL{S} Rd,Rs 1S+mI`. `multiply_cycles`
                // is the `m` and the instruction's own first cycle is the `S`, so the total is
                // `m + 1` — a plain `1` plus `m`, not `m`.
                cycles += Cpu::multiply_cycles(a, false);
            }
            14 => {
                self.r[rd] = a & !b;
                self.set_nz(a & !b);
            }
            _ => {
                self.r[rd] = !b;
                self.set_nz(!b);
            }
        }
        cycles
    }

    /// Format 5: the high-register operations and `BX`. `ADD`, `MOV` and `NOP` write no flags;
    /// `CMP` writes all four. A write to `R15` is a branch, and only `BX` may change instruction set.
    fn high_register(&mut self, op: u32) -> u32 {
        let operation = (op >> 8) & 3;
        let rd = (((op >> 4) & 0x8) | (op & 7)) as usize;
        let rs = (((op >> 3) & 0x8) | ((op >> 3) & 7)) as usize;
        match operation {
            0 => {
                let value = self.reg(rd).wrapping_add(self.reg(rs));
                let cycles = if rd == 15 { 3 } else { 1 };
                self.write_reg(rd, value);
                cycles
            }
            1 => {
                let (value, carry, overflow) = self.sub_flags(self.reg(rd), self.reg(rs), true);
                self.set_nz(value);
                self.set_flag(FLAG_C, carry);
                self.set_flag(FLAG_V, overflow);
                1
            }
            2 => {
                let value = self.reg(rs);
                let cycles = if rd == 15 { 3 } else { 1 };
                self.write_reg(rd, value);
                cycles
            }
            _ => {
                let target = self.reg(rs);
                self.set_thumb(target & 1 != 0);
                self.branch_to(target);
                3 // GBATEK, *THUMB.5*: "2S+1N for ADD/MOV with Rd=R15, and for BX"
            }
        }
    }

    /// Format 6: `LDR Rd, [PC, #imm8*4]`. The base is `(PC + 4) AND NOT 2`.
    fn load_literal(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let rd = ((op >> 8) & 7) as usize;
        let address = (self.pc_read() & !3).wrapping_add((op & 0xFF) * 4);
        self.r[rd] = bus.read32(address);
        3
    }

    /// Formats 7 and 8: loads and stores with a register offset, including the sign-extended ones.
    /// Bit 9 splits them: clear is the word/byte group, set is the halfword and signed group.
    fn register_transfer(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let operation = (op >> 10) & 3;
        let signed = op & 0x0200 != 0;
        let ro = ((op >> 6) & 7) as usize;
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let address = self.r[rb].wrapping_add(self.r[ro]);
        if signed {
            match operation {
                0 => {
                    // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the
                    // save window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
                    bus.write16(address, self.r[rd] as u16);
                    2
                }
                1 => {
                    self.r[rd] = bus.read8(address) as i8 as i32 as u32;
                    3
                }
                2 => {
                    self.r[rd] = self.load_halfword(bus, address);
                    3
                }
                _ => {
                    self.r[rd] = self.load_signed_halfword(bus, address);
                    3
                }
            }
        } else {
            match operation {
                0 => {
                    // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the
                    // save window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
                    bus.write32(address, self.r[rd]);
                    2
                }
                1 => {
                    bus.write8(address, self.r[rd] as u8);
                    2
                }
                2 => {
                    let mut value = bus.read32(address & !3);
                    let rotation = (address & 3) * 8;
                    if rotation != 0 {
                        value = value.rotate_right(rotation);
                    }
                    self.r[rd] = value;
                    3
                }
                _ => {
                    self.r[rd] = u32::from(bus.read8(address));
                    3
                }
            }
        }
    }

    /// Format 9: `LDR`/`STR` word and byte with a five-bit scaled immediate.
    fn immediate_offset(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let byte = op & 0x1000 != 0;
        let load = op & 0x0800 != 0;
        let offset = ((op >> 6) & 0x1F) * if byte { 1 } else { 4 };
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let address = self.r[rb].wrapping_add(offset);
        if byte {
            if load {
                self.r[rd] = u32::from(bus.read8(address));
                3
            } else {
                bus.write8(address, self.r[rd] as u8);
                2
            }
        } else if load {
            let mut value = bus.read32(address & !3);
            let rotation = (address & 3) * 8;
            if rotation != 0 {
                value = value.rotate_right(rotation);
            }
            self.r[rd] = value;
            3
        } else {
            // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the save
            // window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
            bus.write32(address, self.r[rd]);
            2
        }
    }

    /// Format 10: `LDRH`/`STRH` with a five-bit scaled immediate.
    fn halfword_offset(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let load = op & 0x0800 != 0;
        let offset = ((op >> 6) & 0x1F) * 2;
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let address = self.r[rb].wrapping_add(offset);
        if load {
            self.r[rd] = self.load_halfword(bus, address);
            3
        } else {
            // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the save
            // window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
            bus.write16(address, self.r[rd] as u16);
            2
        }
    }

    /// Format 11: `LDR`/`STR` against `SP`.
    fn sp_offset(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let load = op & 0x0800 != 0;
        let rd = ((op >> 8) & 7) as usize;
        let address = self.r[13].wrapping_add((op & 0xFF) * 4);
        if load {
            let mut value = bus.read32(address & !3);
            let rotation = (address & 3) * 8;
            if rotation != 0 {
                value = value.rotate_right(rotation);
            }
            self.r[rd] = value;
            3
        } else {
            // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the save
            // window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
            bus.write32(address, self.r[rd]);
            2
        }
    }

    /// Format 12: `ADD Rd, PC, #imm8*4` and `ADD Rd, SP, #imm8*4`. The PC form reads
    /// `(PC + 4) AND NOT 2`; neither writes flags.
    fn add_relative(&mut self, op: u32) -> u32 {
        let rd = ((op >> 8) & 7) as usize;
        let offset = (op & 0xFF) * 4;
        let base = if op & 0x0800 != 0 {
            self.r[13]
        } else {
            self.pc_read() & !3
        };
        self.r[rd] = base.wrapping_add(offset);
        1
    }

    /// Format 13: `ADD SP, #imm7*4` and `SUB SP, #imm7*4`. No flags, and the stack pointer is not
    /// aligned.
    fn adjust_sp(&mut self, op: u32) -> u32 {
        let offset = (op & 0x7F) * 4;
        if op & 0x80 != 0 {
            self.r[13] = self.r[13].wrapping_sub(offset);
        } else {
            self.r[13] = self.r[13].wrapping_add(offset);
        }
        1
    }

    /// Format 14: `PUSH {Rlist, LR}` and `POP {Rlist, PC}`. The stack pointer is not aligned, and a
    /// `POP` of `PC` stays in Thumb whatever the bit 0 of the value says (GBATEK: "POP {PC} ignores
    /// the least significant bit of the return address").
    #[allow(clippy::needless_range_loop)] // the index IS the register number; `self.r` is also read by `bus` calls between writes
    fn push_pop(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let pop = op & 0x0800 != 0;
        let extra = op & 0x0100 != 0;
        let list = op & 0xFF;
        let words = list.count_ones() + u32::from(extra);
        if pop {
            // GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: "The mis-aligned low
            // bit(s) are ignored, the memory access goes to a forcibly aligned (rounded-down) memory
            // address". A `POP` from an odd `SP` therefore reads from the even boundary below it and
            // walks up by four from there; the stack pointer itself is restored from the last
            // address, not from the masked one.
            let mut address = self.r[13];
            for register in 0..8 {
                if list & (1 << register) != 0 {
                    self.r[register] = bus.read32(address & !3);
                    address = address.wrapping_add(4);
                }
            }
            if extra {
                let return_address = bus.read32(address & !3);
                address = address.wrapping_add(4);
                self.r[13] = address;
                self.branch_to(return_address);
                // GBATEK, *THUMB.14*: "Execution Time: nS+1N+1I (POP), (n+1)S+2N+1I (POP PC)", and
                // *Jumps and Calls* prints `POP {Rlist,}PC (n+1)S+2N+1I`. `words` already counts
                // PC, so with PC in the list the figure is n+4 — the same `n+4` the ARM `LDM PC`
                // beside it charges.
                return words + 4;
            }
            self.r[13] = address;
            // GBATEK, *THUMB.14*: "nS+1N+1I (POP)" — n+2 with n registers, the same figure its
            // `LDMIA Rb!,{Rlist} nS+1N+1I` gives.
            words + 2
        } else {
            let mut address = self.r[13].wrapping_sub(words * 4);
            for register in 0..8 {
                if list & (1 << register) != 0 {
                    bus.write32(address, self.r[register]);
                    address = address.wrapping_add(4);
                }
            }
            if extra {
                bus.write32(address, self.r[14]);
            }
            self.r[13] = self.r[13].wrapping_sub(words * 4);
            words + 1
        }
    }

    /// Format 15: `STMIA Rb!, {Rlist}` and `LDMIA Rb!, {Rlist}`. The empty list is the ARM7TDMI's own
    /// quirk again — one transfer of `R15` and a base moved by $40 — and, as in ARM state on ARMv4, a
    /// load whose list names the base does not write the base back.
    #[allow(clippy::needless_range_loop)] // the index IS the register number, as in `push_pop`
    fn thumb_block_transfer(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let load = op & 0x0800 != 0;
        let rb = ((op >> 8) & 7) as usize;
        let empty = op & 0xFF == 0;
        let list = op as u8 as u32;
        let words = if empty { 16 } else { list.count_ones() };
        let mut address = self.r[rb];
        if load {
            // As in `PUSH`/`POP` above: a mis-aligned `LDMIA` base is rounded down to the word
            // boundary (GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*), and the walk
            // still moves four bytes per register from there.
            if empty {
                let value = bus.read32(address & !3);
                self.r[rb] = self.r[rb].wrapping_add(words * 4);
                self.branch_to(value);
                return 3;
            }
            for register in 0..8 {
                if list & (1 << register) != 0 {
                    self.r[register] = bus.read32(address & !3);
                    address = address.wrapping_add(4);
                }
            }
            if list & (1 << rb) == 0 {
                self.r[rb] = address;
            }
            words + 2
        } else {
            for register in 0..8 {
                if list & (1 << register) != 0 {
                    // As in ARM state (ARM DDI 0084D 4.11.6), a base that is the first register of
                    // its own list stores the old value and one stored later carries the new base.
                    let value = if register == rb {
                        if list & ((1 << rb) - 1) == 0 {
                            self.r[rb]
                        } else {
                            self.r[rb].wrapping_add(words * 4)
                        }
                    } else {
                        self.r[register]
                    };
                    bus.write32(address, value);
                    address = address.wrapping_add(4);
                }
            }
            if empty {
                // The empty list stores the PC, six bytes past the instruction in Thumb state.
                bus.write32(address, self.pc_read_late());
            }
            self.r[rb] = self.r[rb].wrapping_add(words * 4);
            words + 1
        }
    }

    /// Format 19, first instruction: `LR = PC + (off11 << 12)`.
    ///
    /// GBATEK: "LR = PC+4+(nn SHL 12)". The eleven bits are a *signed* field, so a suite that calls
    /// backwards leaves `LR` a page below where the call is; the second instruction's field is
    /// unsigned and brings it back up. `roms/test/gba/thumb.gba` test 154 is exactly that shape.
    fn bl_prefix(&mut self, op: u32) -> u32 {
        let offset = ((op << 21) as i32 >> 9) as u32;
        self.r[14] = self.pc_read().wrapping_add(offset);
        1
    }

    /// Format 19, second instruction: `PC = LR + (off11 << 1)` and `LR = (PC - 2) OR 1`, so that a
    /// `BX LR` returns into Thumb. `R15` already points at the instruction after the pair, which is
    /// the return address, and its bit 0 is set so the return stays in Thumb.
    fn bl_suffix(&mut self, op: u32) -> u32 {
        let target = self.r[14].wrapping_add((op & 0x7FF) << 1);
        self.r[14] = self.r[15] | 1;
        self.branch_to(target);
        // GBATEK, *THUMB.19*: "Execution Time: 3S+1N (first opcode 1S, second opcode 2S+1N)" — the
        // second half is the same three bus cycles as a `B`.
        3
    }

    /// `LDRH`, the ARM7TDMI's misaligned case included: from an odd address it is the aligned word
    /// rotated right by eight with bits 8-23 masked off (GBATEK, *Mis-aligned LDRH,LDRSH*).
    fn load_halfword(&self, bus: &mut Bus, address: u32) -> u32 {
        if address & 1 == 0 {
            u32::from(bus.read16(address))
        } else {
            bus.read32(address & !3).rotate_right(8) & 0xFF00_00FF
        }
    }

    /// `LDRSH`: a halfword from an even address, and a sign-extended *byte* from an odd one.
    fn load_signed_halfword(&self, bus: &mut Bus, address: u32) -> u32 {
        if address & 1 == 0 {
            bus.read16(address) as i16 as i32 as u32
        } else {
            bus.read8(address) as i8 as i32 as u32
        }
    }
}
