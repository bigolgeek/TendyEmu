//! The SM83 arithmetic and logic unit: one function per operation, each returning the result and
//! the flags byte.
//!
//! Every function returns flags in the high nibble only (`Flags`' four bits), so a caller can
//! assign `Flags(f)` directly. Operations that leave a flag alone take the old flags byte and
//! return it with only the flags they own replaced. Sources: `CPU_Registers_and_Flags.html` for the
//! flag rules, `https://gbdev.io/gb-opcodes/optables/` for which opcode owns which flag.

use super::Flags;

/// `ADD A, x` / `ADC A, x`: Z, H and C from the sum, N = 0.
pub fn add8(a: u8, b: u8, carry: bool) -> (u8, u8) {
    let cin = u8::from(carry) as u16;
    let sum = a as u16 + b as u16 + cin;
    let result = sum as u8;
    let mut f = 0u8;
    if result == 0 {
        f |= Flags::Z;
    }
    if (a & 0x0F) as u16 + (b & 0x0F) as u16 + cin > 0x0F {
        f |= Flags::H;
    }
    if sum > 0xFF {
        f |= Flags::C;
    }
    (result, f)
}

/// `SUB x` / `SBC A, x` / `CP x`: Z, H and C from the difference, N = 1.
pub fn sub8(a: u8, b: u8, carry: bool) -> (u8, u8) {
    let cin = u16::from(carry);
    let result = a.wrapping_sub(b).wrapping_sub(carry as u8);
    let mut f = Flags::N;
    if result == 0 {
        f |= Flags::Z;
    }
    if ((a & 0x0F) as u16) < (b & 0x0F) as u16 + cin {
        f |= Flags::H;
    }
    if (a as u16) < b as u16 + cin {
        f |= Flags::C;
    }
    (result, f)
}

/// `AND x`: Z from the result, H = 1, N = C = 0.
pub fn and8(a: u8, b: u8) -> (u8, u8) {
    let result = a & b;
    let mut f = Flags::H;
    if result == 0 {
        f |= Flags::Z;
    }
    (result, f)
}

/// `OR x`: Z from the result, N = H = C = 0.
pub fn or8(a: u8, b: u8) -> (u8, u8) {
    let result = a | b;
    let mut f = 0u8;
    if result == 0 {
        f |= Flags::Z;
    }
    (result, f)
}

/// `XOR x`: Z from the result, N = H = C = 0.
pub fn xor8(a: u8, b: u8) -> (u8, u8) {
    let result = a ^ b;
    let mut f = 0u8;
    if result == 0 {
        f |= Flags::Z;
    }
    (result, f)
}

/// `INC r`: Z and H from the increment, N = 0, C preserved.
pub fn inc8(value: u8, old: u8) -> (u8, u8) {
    let result = value.wrapping_add(1);
    let mut f = old & Flags::C;
    if result == 0 {
        f |= Flags::Z;
    }
    if value & 0x0F == 0x0F {
        f |= Flags::H;
    }
    (result, f)
}

/// `DEC r`: Z and H from the decrement, N = 1, C preserved.
pub fn dec8(value: u8, old: u8) -> (u8, u8) {
    let result = value.wrapping_sub(1);
    let mut f = Flags::N | (old & Flags::C);
    if result == 0 {
        f |= Flags::Z;
    }
    if value & 0x0F == 0x00 {
        f |= Flags::H;
    }
    (result, f)
}

/// `ADD HL, rr`: H from bit 11, C from bit 15, N = 0, C replaced, Z preserved.
pub fn add_hl(a: u16, b: u16, old: u8) -> (u16, u8) {
    let sum = a as u32 + b as u32;
    let mut f = old & Flags::Z;
    if (a & 0x0FFF) + (b & 0x0FFF) > 0x0FFF {
        f |= Flags::H;
    }
    if sum > 0xFFFF {
        f |= Flags::C;
    }
    (sum as u16, f)
}

/// `RLCA`: A rotated left, C = the bit that wrapped, Z = N = H = 0.
pub fn rlca(a: u8) -> (u8, u8) {
    let carry = a & 0x80 != 0;
    let result = a.rotate_left(1);
    (result, if carry { Flags::C } else { 0 })
}

/// `RRCA`: A rotated right, C = the bit that wrapped, Z = N = H = 0.
pub fn rrca(a: u8) -> (u8, u8) {
    let carry = a & 0x01 != 0;
    let result = a.rotate_right(1);
    (result, if carry { Flags::C } else { 0 })
}

/// `RLA`: A rotated left through carry, Z = 0.
pub fn rla(a: u8, old: u8) -> (u8, u8) {
    let result = (a << 1) | u8::from(old & Flags::C != 0);
    let mut f = 0u8;
    if a & 0x80 != 0 {
        f |= Flags::C;
    }
    (result, f)
}

/// `RRA`: A rotated right through carry, Z = 0.
pub fn rra(a: u8, old: u8) -> (u8, u8) {
    let result = (a >> 1) | if old & Flags::C != 0 { 0x80 } else { 0 };
    let mut f = 0u8;
    if a & 0x01 != 0 {
        f |= Flags::C;
    }
    (result, f)
}

/// `CPL`: A inverted, N = H = 1, C preserved.
pub fn cpl(a: u8, old: u8) -> (u8, u8) {
    // `CPU_Registers_and_Flags.html`: CPL sets N and H and leaves Z and C alone.
    (!a, (old & (Flags::Z | Flags::C)) | Flags::N | Flags::H)
}

/// `SCF`: C = 1, N = H = 0, Z preserved.
pub fn scf(old: u8) -> u8 {
    (old & Flags::Z) | Flags::C
}

/// `CCF`: C inverted, N = H = 0, Z preserved.
pub fn ccf(old: u8) -> u8 {
    let carry = if old & Flags::C != 0 { 0 } else { Flags::C };
    (old & Flags::Z) | carry
}

/// `DAA`: decimal-adjust A after an addition or subtraction.
///
/// The rule is the one `CPU_Registers_and_Flags.html` states: after an addition, add `$60` when
/// the carry is set or the value exceeds `$99`, then add `$06` when the half-carry is set or the
/// low nibble exceeds `$09` (both checks see the value as adjusted so far); after a subtraction
/// the same amounts are subtracted. Z follows the result, H is cleared, N and C are unchanged
/// except that the first adjustment sets C.
pub fn daa(a: u8, old: u8) -> (u8, u8) {
    let mut result = a;
    let mut carry = old & Flags::C != 0;
    let nibble = old & Flags::H != 0;
    if old & Flags::N == 0 {
        if carry || result > 0x99 {
            result = result.wrapping_add(0x60);
            carry = true;
        }
        if nibble || result & 0x0F > 0x09 {
            result = result.wrapping_add(0x06);
        }
    } else {
        if carry {
            result = result.wrapping_sub(0x60);
        }
        if nibble {
            result = result.wrapping_sub(0x06);
        }
    }
    let mut f = old & Flags::N;
    if result == 0 {
        f |= Flags::Z;
    }
    if carry {
        f |= Flags::C;
    }
    (result, f)
}

/// The eight CB-block rotations/shifts: Z from the result, N = H = 0, C from the shifted-out bit.
fn shift_flags(result: u8, carry: bool) -> u8 {
    let mut f = 0u8;
    if result == 0 {
        f |= Flags::Z;
    }
    if carry {
        f |= Flags::C;
    }
    f
}

/// `RLC r`: rotate left, bit 7 into C.
pub fn rlc(value: u8) -> (u8, u8) {
    let result = value.rotate_left(1);
    (result, shift_flags(result, value & 0x80 != 0))
}

/// `RRC r`: rotate right, bit 0 into C.
pub fn rrc(value: u8) -> (u8, u8) {
    let result = value.rotate_right(1);
    (result, shift_flags(result, value & 0x01 != 0))
}

/// `RL r`: rotate left through C.
pub fn rl(value: u8, old: u8) -> (u8, u8) {
    let result = (value << 1) | u8::from(old & Flags::C != 0);
    (result, shift_flags(result, value & 0x80 != 0))
}

/// `RR r`: rotate right through C.
pub fn rr(value: u8, old: u8) -> (u8, u8) {
    let result = (value >> 1) | if old & Flags::C != 0 { 0x80 } else { 0 };
    (result, shift_flags(result, value & 0x01 != 0))
}

/// `SLA r`: arithmetic left, bit 7 into C.
pub fn sla(value: u8) -> (u8, u8) {
    let result = value << 1;
    (result, shift_flags(result, value & 0x80 != 0))
}

/// `SRA r`: arithmetic right, bit 0 into C, bit 7 preserved.
pub fn sra(value: u8) -> (u8, u8) {
    let result = (value >> 1) | (value & 0x80);
    (result, shift_flags(result, value & 0x01 != 0))
}

/// `SWAP r`: nibbles exchanged, C = 0, Z from the result.
pub fn swap(value: u8) -> (u8, u8) {
    let result = value.rotate_left(4);
    (result, shift_flags(result, false))
}

/// `SRL r`: logical right, bit 0 into C.
pub fn srl(value: u8) -> (u8, u8) {
    let result = value >> 1;
    (result, shift_flags(result, value & 0x01 != 0))
}

/// `BIT b, r`: Z = the complement of the bit, N = 0, H = 1, C preserved.
pub fn bit(value: u8, index: u8, old: u8) -> u8 {
    let mut f = (old & Flags::C) | Flags::H;
    if value & (1 << index) == 0 {
        f |= Flags::Z;
    }
    f
}
