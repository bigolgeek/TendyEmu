//! The 65816's arithmetic: `ADC`, `SBC`, `CMP` in binary and packed-BCD decimal, and the four
//! shifts (fullsnes "CPU Arithmetic/Logical Operations", "CPU Rotate and Shift
//! Instructions", "CPU Registers and Flags").
//!
//! Every function takes its width mask (`$00FF` or `$FFFF`) so one body serves both M/X settings,
//! and returns the result plus the four flags it defines. `CMP`,`CPX`,`CPY` reuse `sbc` and keep its
//! carry, negative and zero (their operand is the *difference*; `V` is untouched, which is why the
//! caller only takes those three).

/// A result and the flags it defines.
#[derive(Clone, Copy, Debug)]
pub struct Alu {
    /// The value to store in A (or the difference, for a compare).
    pub value: u16,
    pub carry: bool,
    pub overflow: bool,
    pub negative: bool,
    pub zero: bool,
}

/// `ADC`: `A + M + C` (`fullsnes: "A=A+C+op"`).
pub fn adc(a: u16, b: u16, carry: bool, decimal: bool, mask: u16) -> Alu {
    let a = a & mask;
    let b = b & mask;
    let sum = u32::from(a) + u32::from(b) + u32::from(carry);
    let binary = (sum as u16) & mask;
    let sign = mask & !(mask >> 1);
    // fullsnes "Zero Flag, Negative/Sign Flag, Overflow Flag": "V is set when an
    // addition/subtraction exceeded the maximum range for signed numbers".
    let overflow = (!(a ^ b) & (a ^ binary) & sign) != 0;
    let (value, carry_out) = if decimal {
        if mask == 0x00FF {
            let (v, c) = bcd_add8(a as u8, b as u8, carry);
            (u16::from(v), c)
        } else {
            let (lo, c1) = bcd_add8(a as u8, b as u8, carry);
            let (hi, c2) = bcd_add8((a >> 8) as u8, (b >> 8) as u8, c1);
            (u16::from_le_bytes([lo, hi]), c2)
        }
    } else {
        (binary, sum > u32::from(mask))
    };
    let value = value & mask;
    Alu {
        value,
        carry: carry_out,
        overflow,
        negative: value & sign != 0,
        zero: value == 0,
    }
}

/// `SBC`: `A + NOT M + C`, i.e. `A - M - (1 - C)` (fullsnes: "A=A+C-1-op").
///
/// `decimal` selects packed BCD: the subtraction is done a nibble at a time and the carry means "no
/// borrow", which is 65816 convention (fullsnes "Carry Flag (C)": "When used for subtractions (SBC
/// and CMP), the carry flag is having opposite meaning ... it is SET when above-or-equal").
pub fn sbc(a: u16, b: u16, carry: bool, decimal: bool, mask: u16) -> Alu {
    let a = a & mask;
    let b = b & mask;
    let borrow = !carry;
    let binary = a.wrapping_sub(b).wrapping_sub(u16::from(borrow)) & mask;
    let sign = mask & !(mask >> 1);
    let overflow = ((a ^ b) & (a ^ binary) & sign) != 0;
    let (value, borrowed) = if decimal {
        if mask == 0x00FF {
            let (v, br) = bcd_sub8(a as u8, b as u8, borrow);
            (u16::from(v), br)
        } else {
            let (lo, b1) = bcd_sub8(a as u8, b as u8, borrow);
            let (hi, b2) = bcd_sub8((a >> 8) as u8, (b >> 8) as u8, b1);
            (u16::from_le_bytes([lo, hi]), b2)
        }
    } else {
        let borrowed = u32::from(a) < u32::from(b) + u32::from(borrow);
        (binary, borrowed)
    };
    let value = value & mask;
    Alu {
        value,
        carry: !borrowed,
        overflow,
        negative: value & sign != 0,
        zero: value == 0,
    }
}

/// One packed-BCD nibble addition, returning the byte and the carry out of the tens digit.
fn bcd_add8(a: u8, b: u8, carry: bool) -> (u8, bool) {
    let mut low = u16::from(a & 0x0F) + u16::from(b & 0x0F) + u16::from(carry);
    let mut high = u16::from(a >> 4) + u16::from(b >> 4);
    if low > 9 {
        low += 6;
        high += 1;
    }
    let carry_out = high > 9;
    if carry_out {
        high += 6;
    }
    (((high as u8 & 0x0F) << 4) | (low as u8 & 0x0F), carry_out)
}

/// One packed-BCD nibble subtraction, returning the byte and the borrow out of the tens digit.
fn bcd_sub8(a: u8, b: u8, borrow: bool) -> (u8, bool) {
    let mut low = i16::from(a & 0x0F) - i16::from(b & 0x0F) - i16::from(borrow);
    let mut high = i16::from(a >> 4) - i16::from(b >> 4);
    if low < 0 {
        low += 10;
        high -= 1;
    }
    let borrow_out = high < 0;
    if borrow_out {
        high += 10;
    }
    (((high as u8 & 0x0F) << 4) | (low as u8 & 0x0F), borrow_out)
}

/// The four shifts and rotates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Asl,
    Lsr,
    Rol,
    Ror,
}

/// Applies `op` to `value` at the given width, returning the result and the new carry.
///
/// fullsnes "CPU Rotate and Shift Instructions": `ASL` "SHL A", `LSR` "SHR A", `ROL`/`ROR`
/// "rotate through carry (rotates 9bits in total)".
pub fn shift(op: Shift, value: u16, carry: bool, mask: u16) -> (u16, bool) {
    let value = value & mask;
    let sign = mask & !(mask >> 1);
    match op {
        Shift::Asl => ((value << 1) & mask, value & sign != 0),
        Shift::Lsr => (value >> 1, value & 1 != 0),
        Shift::Rol => (((value << 1) | u16::from(carry)) & mask, value & sign != 0),
        Shift::Ror => ((value >> 1) | if carry { sign } else { 0 }, value & 1 != 0),
    }
}
