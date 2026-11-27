//! ARM state: the sixteen data-processing opcodes, multiplies, swaps, the three transfer families,
//! the block transfers, `MRS`/`MSR`, `B`/`BL`/`BX` and `SWI`.
//!
//! Every rule here is ARM DDI 0084D's (chapter 4) or GBATEK's *ARM Opcodes*; the section numbers in
//! the comments are to DDI 0084D unless they say GBATEK. The decoder follows GBATEK's *ARM Binary
//! Opcode Format* table, whose bit patterns are reproduced in the match below.

use crate::bus::Bus;
use crate::cpu::{Cpu, FLAG_C, FLAG_N, FLAG_V, FLAG_Z};

/// The field mask of an `MSR`: bits 19-16 select which bytes of the status register are written.
fn field_mask(op: u32) -> u32 {
    let mut mask = 0;
    if op & (1 << 19) != 0 {
        mask |= 0xFF00_0000;
    }
    if op & (1 << 18) != 0 {
        mask |= 0x00FF_0000;
    }
    if op & (1 << 17) != 0 {
        mask |= 0x0000_FF00;
    }
    if op & (1 << 16) != 0 {
        mask |= 0x0000_00FF;
    }
    mask
}

impl Cpu {
    /// One ARM instruction. `R15` has already been advanced past it by the caller.
    pub(super) fn step_arm(&mut self, bus: &mut Bus) -> u32 {
        let pc = self.r[15];
        let op = self.pipelined(pc, bus.fetch32(pc & !3));
        self.r[15] = pc.wrapping_add(4);
        let condition = op >> 28;
        if condition == 0xF {
            // `cond=1111` is `NV`: GBATEK, *ARM CPU Flags & Condition Field (cond)*, prints
            // "F: NV - never (ARMv1,v2 only) (Reserved ARMv3 and up)" (line 91368). Reserved on
            // ARMv4T means it is neither a false condition (1S, not executed) nor the space that
            // simply runs: the encoding is undefined. SWI is the one exception and keeps its
            // documented encoding — bits 27-24 = `1111b` under this condition field (GBATEK,
            // *Software Interrupt (SWI/BKPT)*, line 91906); the "ignored by processor" phrase
            // there is the 24-bit comment field (line 91909), not the condition. Every other
            // reserved `cond=1111` word takes the Undefined trap: no document on this machine
            // says ARM7TDMI executes one, and a trap cannot silently execute a stray word.
            if (op >> 24) & 0xF == 0xF {
                return self.software_interrupt(op & 0x00FF_FFFF, bus);
            }
            return self.undefined_instruction();
        }
        if !self.condition(condition) {
            return 1; // "cond=false 1S" — GBATEK's instruction summary.
        }
        // `BX Rn`: 0001 0010 1111 1111 1111 0001 Rn. `BLX` is ARMv5 and does not exist here.
        if op & 0x0FFF_FFF0 == 0x012F_FF10 {
            let target = self.reg((op & 0xF) as usize);
            self.set_thumb(target & 1 != 0);
            self.branch_to(target);
            return 3; // GBATEK, *ARM CPU Instruction Cycle Times*: `BX{cond} Rn 2S+1N`
        }
        match (op >> 25) & 0b111 {
            // 27-26 = 00: data processing, multiply, swap, halfword transfer, MRS/MSR. Bit 25 splits
            // the immediate form of the data-processing operand off from the rest.
            0b000 | 0b001 => {
                if op & (1 << 25) != 0 {
                    // `MSR` immediate sits where a data-processing instruction with an unrotated
                    // immediate would be; the PSR encodings all have bit 20 clear (GBATEK: "Must be
                    // 0b for this instruction (otherwise TST,TEQ,CMP,CMN)").
                    if op & (1 << 20) == 0 && (op >> 23) & 3 == 2 && op & (1 << 21) != 0 {
                        return self.msr(op, true);
                    }
                    return self.data_processing(op);
                }
                // 0001 0 with bit 20 clear: the PSR transfer and swap family, which sits where
                // TST/TEQ/CMP/CMN would be. Those three always have the S bit set, which is what
                // separates them from this group.
                // A halfword transfer with P=1, U=0 has the same bits 24-23 and (with W set, or Rn =
                // R15) the same bit 21 and Rn as MSR and MRS; GBATEK, *Halfword ... Data Transfer*:
                // "Reserved, must be set (1)" for bits 7 and 4, with the opcode in bits 6-5 non-zero,
                // so a set bit 7, bit 4 and non-zero SH is never a PSR transfer.
                let halfword = op & 0x90 == 0x90 && (op >> 5) & 3 != 0;
                if !halfword && op & (1 << 20) == 0 && (op >> 23) & 3 == 2 {
                    if op & (1 << 21) != 0 {
                        return self.msr(op, false);
                    }
                    if (op >> 16) & 0xF == 0xF {
                        return self.mrs(op);
                    }
                    if op & 0xFF0 == 0x090 {
                        return self.swap(op, bus);
                    }
                }
                // Bits 7 and 4 both set: multiply, multiply long, or a halfword transfer. Bits 6-5
                // are the SH field of a transfer and are 00 for every multiply, which is what tells
                // them apart (a post-indexed STRH has bits 27-23 = 00001 and would otherwise look
                // like a UMULL).
                if op & 0x90 == 0x90 {
                    if (op >> 5) & 3 == 0 {
                        if (op >> 23) & 0x1F == 1 {
                            return self.multiply_long(op);
                        }
                        if (op >> 22) & 0x3F == 0 {
                            return self.multiply(op);
                        }
                        return self.undefined_instruction();
                    }
                    return self.halfword_transfer(op, bus);
                }
                self.data_processing(op)
            }
            // 27-26 = 01: single data transfer.
            0b010 | 0b011 => self.single_transfer(op, bus),
            // 27-25 = 100: block data transfer.
            0b100 => self.block_transfer(op, bus),
            // 27-25 = 101: B, BL.
            0b101 => self.branch(op),
            // 111 with bits 27-24 = 1111 is `SWI`, whose condition field GBATEK notes is "ignored by
            // processor". Everything else here is coprocessor space, and a Game Boy Advance has no
            // coprocessor to hand it to.
            0b111 if (op >> 24) & 0xF == 0xF => self.software_interrupt(op & 0x00FF_FFFF, bus),
            _ => self.undefined_instruction(),
        }
    }

    /// `B` and `BL`: `PC = PC + 8 + nn*4`, and for `BL`, `LR = PC + 4`, which is `R15` after the
    /// fetch (4.4).
    fn branch(&mut self, op: u32) -> u32 {
        let offset = ((op << 8) as i32 >> 6) as u32;
        let target = self.pc_read().wrapping_add(offset);
        if op & (1 << 24) != 0 {
            self.r[14] = self.r[15];
        }
        self.branch_to(target);
        // GBATEK, *ARM CPU Instruction Cycle Times*: `B,BL 2S+1N` — three bus cycles, of which the
        // `N` is the target fetch the bus prices from the region the branch lands in.
        3
    }

    /// The sixteen data-processing opcodes (4.5). The second operand comes through the barrel shifter
    /// and the carry it produces is what an `S` instruction stores — for logical operations only when
    /// the shift moved something ("Carry flag affected only if Op2 contains a non-zero shift amount",
    /// GBATEK), and always for arithmetic ones.
    fn data_processing(&mut self, op: u32) -> u32 {
        let opcode = (op >> 21) & 0xF;
        let set_flags = op & (1 << 20) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let immediate = op & (1 << 25) != 0;
        let register_shift = !immediate && op & (1 << 4) != 0;
        let compare = (8..=11).contains(&opcode);

        // 4.5.5: with a register-specified shift the PC is 12 bytes ahead, and that applies to the
        // first operand too — `add r0, pc, r0, lsl r0` reads PC+12 (test 225 of `arm.gba`).
        let a = if rn == 15 && register_shift {
            self.pc_read_late()
        } else {
            self.reg(rn)
        };
        let (b, shifter_carry, shifted) = if immediate {
            let imm = op & 0xFF;
            let rotate = ((op >> 8) & 0xF) * 2;
            if rotate == 0 {
                (imm, self.cpsr & FLAG_C != 0, false)
            } else {
                // The carry out of a rotate is the last bit rotated through: bit (rotate - 1).
                (
                    imm.rotate_right(rotate),
                    (imm >> (rotate - 1)) & 1 != 0,
                    true,
                )
            }
        } else {
            let rm = (op & 0xF) as usize;
            let value = if register_shift {
                self.reg_late(rm)
            } else {
                self.reg(rm)
            };
            let kind = (op >> 5) & 3;
            let carry_in = self.cpsr & FLAG_C != 0;
            if register_shift {
                let amount = self.reg(((op >> 8) & 0xF) as usize);
                let (value, carry) = self.shift_by_register(value, kind, amount, carry_in);
                (value, carry, amount & 0xFF != 0)
            } else {
                let amount = (op >> 7) & 0x1F;
                let (value, carry) = self.shift_by_immediate(value, kind, amount, carry_in);
                (value, carry, kind != 0 || amount != 0)
            }
        };

        let logical = matches!(opcode, 0b0000 | 0b0001 | 0b1000 | 0b1001 | 0b1100..=0b1111);
        let carry = self.cpsr & FLAG_C != 0;
        let (result, carry_out, overflow) = match opcode {
            0b0000 | 0b1000 => (a & b, shifter_carry, false),
            0b0001 | 0b1001 => (a ^ b, shifter_carry, false),
            0b1100 => (a | b, shifter_carry, false),
            0b1101 => (b, shifter_carry, false),
            0b1110 => (a & !b, shifter_carry, false),
            0b1111 => (!b, shifter_carry, false),
            0b0010 | 0b1010 => self.sub_flags(a, b, true),
            0b0011 => self.sub_flags(b, a, true),
            0b0100 | 0b1011 => self.add_flags(a, b, false),
            0b0101 => self.add_flags(a, b, carry),
            0b0110 => self.sub_flags(a, b, carry),
            _ => self.sub_flags(b, a, carry),
        };

        if compare {
            // TST, TEQ, CMP and CMN never write a register, even with Rd = R15. What an Rd of 15
            // does do is what GBATEK describes as "write to the PSR bits in R15 without modifying
            // the PC": the flags are set and, in a privileged mode, the CPSR is replaced by the
            // SPSR — tests 234 and 235 of `arm.gba` pin both halves of that.
            self.set_nz(result);
            if logical {
                if shifted {
                    self.set_flag(FLAG_C, carry_out);
                }
            } else {
                self.set_flag(FLAG_C, carry_out);
                self.set_flag(FLAG_V, overflow);
            }
            if rd == 15 {
                self.restore_cpsr();
            }
            return 1 + u32::from(register_shift);
        }

        if set_flags {
            self.set_nz(result);
            if logical {
                if shifted {
                    self.set_flag(FLAG_C, carry_out);
                }
            } else {
                self.set_flag(FLAG_C, carry_out);
                self.set_flag(FLAG_V, overflow);
            }
        }

        let mut cycles = 1 + u32::from(register_shift);
        if rd == 15 {
            // 4.5.4: with S set this is an exception return — PC = result and CPSR = SPSR; without
            // S it is a plain branch.
            if set_flags {
                self.restore_cpsr();
            }
            self.branch_to(result);
            cycles += 2;
        } else {
            self.r[rd] = result;
        }
        cycles
    }

    /// `MRS`: the CPSR, or the SPSR of the current mode, into an ARM register (4.6).
    fn mrs(&mut self, op: u32) -> u32 {
        let rd = ((op >> 12) & 0xF) as usize;
        let value = if op & (1 << 22) != 0 {
            self.spsr_of(self.mode()).unwrap_or(0)
        } else {
            self.cpsr
        };
        self.write_reg(rd, value);
        1
    }

    /// `MSR`: an 8-bit immediate rotated right, or a register, into the selected fields of the CPSR
    /// or the current mode's SPSR (4.6). A CPSR write that names the control field changes the mode
    /// and therefore the register bank.
    fn msr(&mut self, op: u32, immediate: bool) -> u32 {
        let mask = field_mask(op);
        let value = if immediate {
            (op & 0xFF).rotate_right(((op >> 8) & 0xF) * 2)
        } else {
            self.reg((op & 0xF) as usize)
        };
        if op & (1 << 22) != 0 {
            let mode = self.mode();
            if let Some(current) = self.spsr_of(mode) {
                let bank = Cpu::bank_of(mode);
                self.spsr[bank] = (current & !mask) | (value & mask);
            }
        } else {
            self.write_cpsr_fields(value, mask);
        }
        1
    }

    /// `MUL` and `MLA` (4.7). `Rd = Rm * Rs` with the low 32 bits kept — "no sign/zero extension is
    /// required and MUL and MLA can be used for both signed and unsigned calculations" — plus `Rn`
    /// for `MLA`. Only N and Z are written.
    fn multiply(&mut self, op: u32) -> u32 {
        let accumulate = op & (1 << 21) != 0;
        let set_flags = op & (1 << 20) != 0;
        let rd = ((op >> 16) & 0xF) as usize;
        let rn = ((op >> 12) & 0xF) as usize;
        let rs = ((op >> 8) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let multiplier = self.reg_late(rs);
        let mut result = self.reg_late(rm).wrapping_mul(multiplier);
        if accumulate {
            result = result.wrapping_add(self.reg_late(rn));
        }
        self.r[rd] = result;
        if set_flags {
            // GBATEK records that ARMv4 also destroys C here; leaving it alone is the behaviour the
            // suites in `roms/test/gba/` ask for and the one a later ARM keeps.
            self.set_nz(result);
        }
        1 + Cpu::multiply_cycles(multiplier, false) + u32::from(accumulate)
    }

    /// `UMULL`, `UMLAL`, `SMULL` and `SMLAL` (4.8): a 64-bit product in `RdLo`/`RdHi`.
    fn multiply_long(&mut self, op: u32) -> u32 {
        let signed = op & (1 << 22) != 0;
        let accumulate = op & (1 << 21) != 0;
        let set_flags = op & (1 << 20) != 0;
        let hi = ((op >> 16) & 0xF) as usize;
        let lo = ((op >> 12) & 0xF) as usize;
        let rs = ((op >> 8) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let multiplier = self.reg_late(rs);
        let product = if signed {
            ((self.reg_late(rm) as i32 as i64) * (multiplier as i32 as i64)) as u64
        } else {
            u64::from(self.reg_late(rm)) * u64::from(multiplier)
        };
        let total = if accumulate {
            let held = (u64::from(self.r[hi]) << 32) | u64::from(self.r[lo]);
            product.wrapping_add(held)
        } else {
            product
        };
        self.r[lo] = total as u32;
        self.r[hi] = (total >> 32) as u32;
        if set_flags {
            self.set_flag(FLAG_N, self.r[hi] & 0x8000_0000 != 0);
            self.set_flag(FLAG_Z, total == 0);
        }
        // 1S + (m+1)I, plus one more internal cycle when there is something to accumulate into.
        1 + Cpu::multiply_cycles(multiplier, signed) + 1 + u32::from(accumulate)
    }

    /// `SWP` and `SWPB` (4.14): `Rd = [Rn]`, `[Rn] = Rm`, with the read rotated when the address is
    /// not word-aligned and the write not — "a combination of LDR and STR" (GBATEK, *Mis-aligned
    /// LDR,SWP*), which is what test 452 of `arm.gba` checks.
    fn swap(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let byte = op & (1 << 22) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let address = self.reg(rn);
        let source = self.reg(rm);
        if byte {
            let loaded = u32::from(bus.read8(address));
            bus.write8(address, source as u8);
            self.r[rd] = loaded;
        } else {
            let mut loaded = bus.read32(address & !3);
            let rotation = (address & 3) * 8;
            if rotation != 0 {
                loaded = loaded.rotate_right(rotation);
            }
            bus.write32(address & !3, source);
            self.r[rd] = loaded;
        }
        4 // 1S + 2N + 1I
    }

    /// `LDR` and `STR`, word and byte (4.9). A word read from a misaligned address is the aligned
    /// word rotated right by 8 for each byte of misalignment. A word *write* hands the full address
    /// to the bus, which force-aligns it for 16/32-bit regions and keeps the low bits for the 8-bit
    /// save window's byte lane (GBATEK, *Mis-aligned LDR,SWP*, *Mis-aligned STR* and *Accessing SRAM
    /// Area by 16bit/32bit*).
    fn single_transfer(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let pre = op & (1 << 24) != 0;
        let up = op & (1 << 23) != 0;
        let byte = op & (1 << 22) != 0;
        let writeback = op & (1 << 21) != 0;
        let load = op & (1 << 20) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let base = self.reg(rn);
        let offset = if op & (1 << 25) != 0 {
            // 4.9.3: Rm with a shift by immediate. The carry it produces goes nowhere.
            let value = self.reg_late((op & 0xF) as usize);
            let (value, _) = self.shift_by_immediate(
                value,
                (op >> 5) & 3,
                (op >> 7) & 0x1F,
                self.cpsr & FLAG_C != 0,
            );
            value
        } else {
            op & 0xFFF
        };
        let offset_address = if up {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if pre { offset_address } else { base };
        let mut cycles = if load { 3 } else { 2 };

        if load {
            let value = if byte {
                u32::from(bus.read8(address))
            } else {
                let mut value = bus.read32(address & !3);
                let rotation = (address & 3) * 8;
                if rotation != 0 {
                    value = value.rotate_right(rotation);
                }
                value
            };
            self.write_reg(rd, value);
            if rd == 15 {
                cycles += 2; // LDR PC is 2S + 2N + 1I
            }
        } else {
            // 4.9.4: an STR whose source is R15 stores the address of the instruction plus 12.
            let value = self.reg_late(rd);
            if byte {
                bus.write8(address, value as u8);
            } else {
                // The full address is handed to the bus: the 16/32-bit regions force-align it, the
                // 8-bit save window keeps the low bits for the byte lane (GBATEK, *Accessing SRAM
                // Area by 16bit/32bit*).
                bus.write32(address, value);
            }
        }
        // Post-indexing always writes the base back; pre-indexing only with the W bit. A load whose
        // destination is the base wins over the write-back (tests 360 and 361), so the write-back is
        // skipped in exactly that case rather than ordered around it.
        if (!pre || writeback) && !(load && rd == rn) {
            self.r[rn] = offset_address;
        }
        cycles
    }

    /// `LDRH`, `STRH`, `LDRSB` and `LDRSH` (4.10). The misaligned halfword reads are the ARM7TDMI's
    /// own, which the data sheet calls unpredictable and GBATEK measured: `LDRH` from an odd address
    /// is the aligned word rotated right by eight with bits 8-23 masked off, and `LDRSH` from an odd
    /// address is `LDRSB` from it — "sign-expand BYTE value" (GBATEK, *Mis-aligned LDRH,LDRSH*).
    fn halfword_transfer(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let pre = op & (1 << 24) != 0;
        let up = op & (1 << 23) != 0;
        let immediate = op & (1 << 22) != 0;
        let writeback = op & (1 << 21) != 0;
        let load = op & (1 << 20) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let kind = (op >> 5) & 3;
        let base = self.reg(rn);
        let offset = if immediate {
            ((op >> 4) & 0xF0) | (op & 0xF)
        } else {
            self.reg((op & 0xF) as usize)
        };
        let offset_address = if up {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if pre { offset_address } else { base };
        let mut cycles = if load { 3 } else { 2 };

        if load {
            let value = match kind {
                1 => {
                    if address & 1 == 0 {
                        u32::from(bus.read16(address))
                    } else {
                        bus.read32(address & !3).rotate_right(8) & 0xFF00_00FF
                    }
                }
                2 => bus.read8(address) as i8 as i32 as u32,
                _ => {
                    if address & 1 == 0 {
                        bus.read16(address) as i16 as i32 as u32
                    } else {
                        bus.read8(address) as i8 as i32 as u32
                    }
                }
            };
            self.write_reg(rd, value);
            if rd == 15 {
                cycles += 2;
            }
        } else {
            let value = self.reg_late(rd);
            // Full address to the bus: force-aligned for 16/32-bit regions, byte-lane for the save
            // window (GBATEK, *Accessing SRAM Area by 16bit/32bit*).
            bus.write16(address, value as u16);
        }
        if (!pre || writeback) && !(load && rd == rn) {
            self.r[rn] = offset_address;
        }
        cycles
    }

    /// `LDM` and `STM` in all four addressing modes (4.11).
    ///
    /// Three details are the ones a first implementation gets wrong, and all three are in the
    /// suites: an empty register list transfers `R15` alone but moves the base by $40 (GBATEK,
    /// *Strange Effects on Invalid Rlist's*); an `STM` whose base is the first register of its list
    /// stores the old base and otherwise the new one (4.11.6); and an `LDM` with the base in its
    /// list does not write the base back at all on ARMv4, so the loaded value survives.
    fn block_transfer(&mut self, op: u32, bus: &mut Bus) -> u32 {
        let pre = op & (1 << 24) != 0;
        let up = op & (1 << 23) != 0;
        let user_bank = op & (1 << 22) != 0;
        let writeback = op & (1 << 21) != 0;
        let load = op & (1 << 20) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let empty = op & 0xFFFF == 0;
        let list: u32 = if empty { 1 << 15 } else { op & 0xFFFF };
        let words = if empty { 16 } else { list.count_ones() };
        let transfer_words = if empty { 1 } else { words };
        let base = self.reg(rn);
        let start = match (pre, up) {
            (false, true) => base,                                          // IA
            (true, true) => base.wrapping_add(4),                           // IB
            (false, false) => base.wrapping_sub(words * 4).wrapping_add(4), // DA
            (true, false) => base.wrapping_sub(words * 4),                  // DB
        };
        let written_back = if up {
            base.wrapping_add(words * 4)
        } else {
            base.wrapping_sub(words * 4)
        };
        // With S set and R15 *not* in the list, the transfer names the user bank (4.11.4); with R15
        // in the list it means "restore the CPSR", which is an exception return.
        let user = user_bank && list & (1 << 15) == 0;

        if load {
            let mut address = start;
            let mut pc_value = None;
            for register in 0..16 {
                if list & (1 << register) == 0 {
                    continue;
                }
                // GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: an `LDM` word access
                // at a mis-aligned base is "a forcibly aligned (rounded-down) memory address", so
                // the low two bits of the walking address are ignored here — never in `read32`,
                // which a plain `LDR` needs unaligned for its rotation.
                let value = bus.read32(address & !3);
                address = address.wrapping_add(4);
                if register == 15 {
                    pc_value = Some(value);
                } else if user {
                    self.write_user_reg(register, value);
                } else {
                    self.r[register] = value;
                }
            }
            if writeback && list & (1 << rn) == 0 {
                self.r[rn] = written_back;
            }
            if let Some(value) = pc_value {
                if user_bank {
                    self.restore_cpsr();
                }
                self.branch_to(value);
            }
            transfer_words + 2 + u32::from(pc_value.is_some()) * 2
        } else {
            // 4.11.6: the base is written back at the end of the second cycle, and the first register
            // is stored at the start of it — so a base stored first keeps its old value and a base
            // stored later carries the new one.
            let base_first = list.trailing_zeros() == rn as u32;
            let base_value = if base_first { base } else { written_back };
            let mut address = start;
            for register in 0..16 {
                if list & (1 << register) == 0 {
                    continue;
                }
                let value = if register == rn {
                    base_value
                } else if user {
                    self.read_user_reg(register)
                } else {
                    self.reg_late(register)
                };
                bus.write32(address, value);
                address = address.wrapping_add(4);
            }
            if writeback {
                self.r[rn] = written_back;
            }
            // GBATEK, *Block Data Transfer (LDM,STM)*: "Execution Time: For normal LDM, nS+1N+1I.
            // For LDM PC, (n+1)S+2N+1I. For STM (n-1)S+2N. Where n is the number of words
            // transferred." The summary row prints the same figure (`STM (n-1)S+2N`), which is
            // `n+1` bus cycles: one `S` per word after the first, and the `2N` that opens and
            // closes the transfer. `LDM` above keeps `nS+1N+1I` — one more, not the same number.
            transfer_words + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{IWRAM_BASE, ROM_BASE};
    use crate::cart::{Cartridge, Header};

    /// A bus whose cartridge is a placeholder: every test here runs its code and its data from
    /// Work RAM 32K, which answers every access in one cycle (GBATEK's *Address Bus Width* row,
    /// "Work RAM 32K 32 8/16/32 8/16/32 1/1/1"), so the figure an instruction returns is its own
    /// bus-cycle count with no wait states folded in.
    fn bus() -> Bus {
        Bus::new(Cartridge {
            rom: vec![0u8; 0x100],
            header: Header {
                entry: 0xEA00_002E,
                entry_point: ROM_BASE + 0xC0,
                title: "TEST".to_owned(),
                game_code: "TEST".to_owned(),
                maker: "00".to_owned(),
                version: 0,
                rom_bytes: 0x100,
                complement: 0,
                complement_ok: true,
                logo_ok: true,
            },
            crc32: 0,
        })
    }

    /// GBATEK, *Block Data Transfer (LDM,STM)*: "Execution Time: For normal LDM, nS+1N+1I. For LDM
    /// PC, (n+1)S+2N+1I. For STM (n-1)S+2N. Where n is the number of words transferred." The
    /// summary table's row repeats it — "STM (n-1)S+2N" — and with `S` and `N` one cycle each that
    /// is `n+1` bus cycles, not `n+2`: one `S` per word after the first, and the `2N` that opens
    /// and closes the transfer. `LDMIA` of the same list is `nS+1N+1I` = `n+2`, one more, and is
    /// the wrong figure for `STM` — which is what this test pins in both directions.
    #[allow(clippy::needless_range_loop)] // the index IS the register number, as in `thumb.rs`
    #[test]
    fn stm_costs_the_documented_n_minus_one_s_plus_two_n() {
        // `stmia r0!, {r1-r4}`: bits 27-25 = 100 (block data transfer), P=0 (offset after),
        // U=1 (increment), S=0 (not the user bank), W=1 (write back), L=0 (store), Rn=0,
        // Rlist = $1E = r1-r4 — four words. Written out so a wrong constant cannot pass silently.
        const STMIA: u32 = 0xE8A0_001E;
        // `ldmia r0, {r1-r4}`: the same list and addressing, L=1 and W=0 — `n+2` = six cycles.
        const LDMIA: u32 = 0xE890_001E;
        assert_eq!(
            (STMIA >> 25) & 0b111,
            0b100,
            "the opcode is a block data transfer"
        );
        for (op, name, p, u, s, w, l) in [
            (STMIA, "STMIA", false, true, false, true, false),
            (LDMIA, "LDMIA", false, true, false, false, true),
        ] {
            for (bit, set, field) in [
                (24, p, "P"),
                (23, u, "U"),
                (22, s, "S"),
                (21, w, "W"),
                (20, l, "L"),
            ] {
                assert_eq!(op & (1 << bit) != 0, set, "{name} {field}");
            }
            assert_eq!((op >> 16) & 0xF, 0, "{name} base is r0");
            assert_eq!((op & 0xFFFF).count_ones(), 4, "{name} list is four words");
        }

        let mut bench = bus();
        bench.write32(IWRAM_BASE, STMIA);
        let mut cpu = Cpu::post_boot(IWRAM_BASE);
        cpu.r[0] = IWRAM_BASE + 0x100;
        for register in 1..5 {
            cpu.r[register] = 0x1111_1111 * register as u32;
        }
        assert_eq!(
            cpu.step(&mut bench),
            5,
            "STM of four words is (4-1)S+2N = 5 bus cycles, not 6"
        );
        // The store landed, and the write-back moved the base by the four words it wrote: the
        // instruction really is the `STMIA` the constant claims.
        assert_eq!(bench.read32(IWRAM_BASE + 0x100), 0x1111_1111);
        assert_eq!(bench.read32(IWRAM_BASE + 0x10C), 0x4444_4444);
        assert_eq!(cpu.r[0], IWRAM_BASE + 0x110, "write-back of four words");

        // The `LDM` beside it is the other figure and must not follow: `nS+1N+1I` = 6.
        let mut bench = bus();
        bench.write32(IWRAM_BASE, LDMIA);
        bench.write32(IWRAM_BASE + 0x100, 0xAAAA_AAAA);
        let mut cpu = Cpu::post_boot(IWRAM_BASE);
        cpu.r[0] = IWRAM_BASE + 0x100;
        assert_eq!(
            cpu.step(&mut bench),
            6,
            "LDM of four words is nS+1N+1I = 6, untouched by the STM row"
        );
        assert_eq!(cpu.r[1], 0xAAAA_AAAA);
    }
}
