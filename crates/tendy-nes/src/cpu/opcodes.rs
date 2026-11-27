/// Addressing modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Imp,
    Acc,
    Imm,
    Zp,
    Zpx,
    Zpy,
    Abs,
    Abx,
    Aby,
    Ind,
    Izx,
    Izy,
    Rel,
}

/// Instruction mnemonics (official 6502 plus the unofficial ones the NES test ROMs use). Written in CamelCase on purpose: fully upper-case names fail clippy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mn {
    Adc,
    Ahx,
    Alr,
    Anc,
    And,
    Arr,
    Asl,
    Axs,
    Bcc,
    Bcs,
    Beq,
    Bit,
    Bmi,
    Bne,
    Bpl,
    Brk,
    Bvc,
    Bvs,
    Clc,
    Cld,
    Cli,
    Clv,
    Cmp,
    Cpx,
    Cpy,
    Dcp,
    Dec,
    Dex,
    Dey,
    Eor,
    Inc,
    Inx,
    Iny,
    Isb,
    Jam,
    Jmp,
    Jsr,
    Las,
    Lax,
    Lda,
    Ldx,
    Ldy,
    Lsr,
    Nop,
    Ora,
    Pha,
    Php,
    Pla,
    Plp,
    Rla,
    Rol,
    Ror,
    Rra,
    Rti,
    Rts,
    Sax,
    Sbc,
    Sec,
    Sed,
    Sei,
    Shx,
    Shy,
    Slo,
    Sre,
    Sta,
    Stx,
    Sty,
    Tas,
    Tax,
    Tay,
    Tsx,
    Txa,
    Txs,
    Tya,
    Xaa,
}

/// One opcode table entry. `cycles` is the base cycle count; `page_penalty` means +1 cycle when the indexed address crosses a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    pub mn: Mn,
    pub mode: Mode,
    pub cycles: u8,
    pub page_penalty: bool,
    pub official: bool,
    /// [`Mn::writes`] precomputed per opcode: a store or read-modify-write always makes the
    /// indexed dummy read.
    pub writes: bool,
}

impl Mn {
    /// True for the store and read-modify-write instructions: they make the indexed dummy read even
    /// when the address does not cross a page. [`op`] copies this into each table row when the
    /// table is compiled, so `execute` reads a flag instead of re-testing twenty mnemonics for every
    /// instruction it runs.
    pub const fn writes(self) -> bool {
        matches!(
            self,
            Mn::Sta
                | Mn::Stx
                | Mn::Sty
                | Mn::Sax
                | Mn::Shx
                | Mn::Shy
                | Mn::Ahx
                | Mn::Tas
                | Mn::Asl
                | Mn::Lsr
                | Mn::Rol
                | Mn::Ror
                | Mn::Inc
                | Mn::Dec
                | Mn::Slo
                | Mn::Rla
                | Mn::Sre
                | Mn::Rra
                | Mn::Dcp
                | Mn::Isb
        )
    }
}

pub const fn op(mn: Mn, mode: Mode, cycles: u8, page_penalty: bool, official: bool) -> Op {
    Op {
        mn,
        mode,
        cycles,
        page_penalty,
        official,
        writes: mn.writes(),
    }
}

/// Indexed by opcode byte: one row per opcode, in opcode order.
pub const OPS: [Op; 256] = [
    op(Mn::Brk, Mode::Imp, 7, false, true),  // 0x00
    op(Mn::Ora, Mode::Izx, 6, false, true),  // 0x01
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x02
    op(Mn::Slo, Mode::Izx, 8, false, false), // 0x03
    op(Mn::Nop, Mode::Zp, 3, false, false),  // 0x04
    op(Mn::Ora, Mode::Zp, 3, false, true),   // 0x05
    op(Mn::Asl, Mode::Zp, 5, false, true),   // 0x06
    op(Mn::Slo, Mode::Zp, 5, false, false),  // 0x07
    op(Mn::Php, Mode::Imp, 3, false, true),  // 0x08
    op(Mn::Ora, Mode::Imm, 2, false, true),  // 0x09
    op(Mn::Asl, Mode::Acc, 2, false, true),  // 0x0A
    op(Mn::Anc, Mode::Imm, 2, false, false), // 0x0B
    op(Mn::Nop, Mode::Abs, 4, false, false), // 0x0C
    op(Mn::Ora, Mode::Abs, 4, false, true),  // 0x0D
    op(Mn::Asl, Mode::Abs, 6, false, true),  // 0x0E
    op(Mn::Slo, Mode::Abs, 6, false, false), // 0x0F
    op(Mn::Bpl, Mode::Rel, 2, false, true),  // 0x10
    op(Mn::Ora, Mode::Izy, 5, true, true),   // 0x11
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x12
    op(Mn::Slo, Mode::Izy, 8, false, false), // 0x13
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0x14
    op(Mn::Ora, Mode::Zpx, 4, false, true),  // 0x15
    op(Mn::Asl, Mode::Zpx, 6, false, true),  // 0x16
    op(Mn::Slo, Mode::Zpx, 6, false, false), // 0x17
    op(Mn::Clc, Mode::Imp, 2, false, true),  // 0x18
    op(Mn::Ora, Mode::Aby, 4, true, true),   // 0x19
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0x1A
    op(Mn::Slo, Mode::Aby, 7, false, false), // 0x1B
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0x1C
    op(Mn::Ora, Mode::Abx, 4, true, true),   // 0x1D
    op(Mn::Asl, Mode::Abx, 7, false, true),  // 0x1E
    op(Mn::Slo, Mode::Abx, 7, false, false), // 0x1F
    op(Mn::Jsr, Mode::Abs, 6, false, true),  // 0x20
    op(Mn::And, Mode::Izx, 6, false, true),  // 0x21
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x22
    op(Mn::Rla, Mode::Izx, 8, false, false), // 0x23
    op(Mn::Bit, Mode::Zp, 3, false, true),   // 0x24
    op(Mn::And, Mode::Zp, 3, false, true),   // 0x25
    op(Mn::Rol, Mode::Zp, 5, false, true),   // 0x26
    op(Mn::Rla, Mode::Zp, 5, false, false),  // 0x27
    op(Mn::Plp, Mode::Imp, 4, false, true),  // 0x28
    op(Mn::And, Mode::Imm, 2, false, true),  // 0x29
    op(Mn::Rol, Mode::Acc, 2, false, true),  // 0x2A
    op(Mn::Anc, Mode::Imm, 2, false, false), // 0x2B
    op(Mn::Bit, Mode::Abs, 4, false, true),  // 0x2C
    op(Mn::And, Mode::Abs, 4, false, true),  // 0x2D
    op(Mn::Rol, Mode::Abs, 6, false, true),  // 0x2E
    op(Mn::Rla, Mode::Abs, 6, false, false), // 0x2F
    op(Mn::Bmi, Mode::Rel, 2, false, true),  // 0x30
    op(Mn::And, Mode::Izy, 5, true, true),   // 0x31
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x32
    op(Mn::Rla, Mode::Izy, 8, false, false), // 0x33
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0x34
    op(Mn::And, Mode::Zpx, 4, false, true),  // 0x35
    op(Mn::Rol, Mode::Zpx, 6, false, true),  // 0x36
    op(Mn::Rla, Mode::Zpx, 6, false, false), // 0x37
    op(Mn::Sec, Mode::Imp, 2, false, true),  // 0x38
    op(Mn::And, Mode::Aby, 4, true, true),   // 0x39
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0x3A
    op(Mn::Rla, Mode::Aby, 7, false, false), // 0x3B
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0x3C
    op(Mn::And, Mode::Abx, 4, true, true),   // 0x3D
    op(Mn::Rol, Mode::Abx, 7, false, true),  // 0x3E
    op(Mn::Rla, Mode::Abx, 7, false, false), // 0x3F
    op(Mn::Rti, Mode::Imp, 6, false, true),  // 0x40
    op(Mn::Eor, Mode::Izx, 6, false, true),  // 0x41
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x42
    op(Mn::Sre, Mode::Izx, 8, false, false), // 0x43
    op(Mn::Nop, Mode::Zp, 3, false, false),  // 0x44
    op(Mn::Eor, Mode::Zp, 3, false, true),   // 0x45
    op(Mn::Lsr, Mode::Zp, 5, false, true),   // 0x46
    op(Mn::Sre, Mode::Zp, 5, false, false),  // 0x47
    op(Mn::Pha, Mode::Imp, 3, false, true),  // 0x48
    op(Mn::Eor, Mode::Imm, 2, false, true),  // 0x49
    op(Mn::Lsr, Mode::Acc, 2, false, true),  // 0x4A
    op(Mn::Alr, Mode::Imm, 2, false, false), // 0x4B
    op(Mn::Jmp, Mode::Abs, 3, false, true),  // 0x4C
    op(Mn::Eor, Mode::Abs, 4, false, true),  // 0x4D
    op(Mn::Lsr, Mode::Abs, 6, false, true),  // 0x4E
    op(Mn::Sre, Mode::Abs, 6, false, false), // 0x4F
    op(Mn::Bvc, Mode::Rel, 2, false, true),  // 0x50
    op(Mn::Eor, Mode::Izy, 5, true, true),   // 0x51
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x52
    op(Mn::Sre, Mode::Izy, 8, false, false), // 0x53
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0x54
    op(Mn::Eor, Mode::Zpx, 4, false, true),  // 0x55
    op(Mn::Lsr, Mode::Zpx, 6, false, true),  // 0x56
    op(Mn::Sre, Mode::Zpx, 6, false, false), // 0x57
    op(Mn::Cli, Mode::Imp, 2, false, true),  // 0x58
    op(Mn::Eor, Mode::Aby, 4, true, true),   // 0x59
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0x5A
    op(Mn::Sre, Mode::Aby, 7, false, false), // 0x5B
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0x5C
    op(Mn::Eor, Mode::Abx, 4, true, true),   // 0x5D
    op(Mn::Lsr, Mode::Abx, 7, false, true),  // 0x5E
    op(Mn::Sre, Mode::Abx, 7, false, false), // 0x5F
    op(Mn::Rts, Mode::Imp, 6, false, true),  // 0x60
    op(Mn::Adc, Mode::Izx, 6, false, true),  // 0x61
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x62
    op(Mn::Rra, Mode::Izx, 8, false, false), // 0x63
    op(Mn::Nop, Mode::Zp, 3, false, false),  // 0x64
    op(Mn::Adc, Mode::Zp, 3, false, true),   // 0x65
    op(Mn::Ror, Mode::Zp, 5, false, true),   // 0x66
    op(Mn::Rra, Mode::Zp, 5, false, false),  // 0x67
    op(Mn::Pla, Mode::Imp, 4, false, true),  // 0x68
    op(Mn::Adc, Mode::Imm, 2, false, true),  // 0x69
    op(Mn::Ror, Mode::Acc, 2, false, true),  // 0x6A
    op(Mn::Arr, Mode::Imm, 2, false, false), // 0x6B
    op(Mn::Jmp, Mode::Ind, 5, false, true),  // 0x6C
    op(Mn::Adc, Mode::Abs, 4, false, true),  // 0x6D
    op(Mn::Ror, Mode::Abs, 6, false, true),  // 0x6E
    op(Mn::Rra, Mode::Abs, 6, false, false), // 0x6F
    op(Mn::Bvs, Mode::Rel, 2, false, true),  // 0x70
    op(Mn::Adc, Mode::Izy, 5, true, true),   // 0x71
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x72
    op(Mn::Rra, Mode::Izy, 8, false, false), // 0x73
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0x74
    op(Mn::Adc, Mode::Zpx, 4, false, true),  // 0x75
    op(Mn::Ror, Mode::Zpx, 6, false, true),  // 0x76
    op(Mn::Rra, Mode::Zpx, 6, false, false), // 0x77
    op(Mn::Sei, Mode::Imp, 2, false, true),  // 0x78
    op(Mn::Adc, Mode::Aby, 4, true, true),   // 0x79
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0x7A
    op(Mn::Rra, Mode::Aby, 7, false, false), // 0x7B
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0x7C
    op(Mn::Adc, Mode::Abx, 4, true, true),   // 0x7D
    op(Mn::Ror, Mode::Abx, 7, false, true),  // 0x7E
    op(Mn::Rra, Mode::Abx, 7, false, false), // 0x7F
    op(Mn::Nop, Mode::Imm, 2, false, false), // 0x80
    op(Mn::Sta, Mode::Izx, 6, false, true),  // 0x81
    op(Mn::Nop, Mode::Imm, 2, false, false), // 0x82
    op(Mn::Sax, Mode::Izx, 6, false, false), // 0x83
    op(Mn::Sty, Mode::Zp, 3, false, true),   // 0x84
    op(Mn::Sta, Mode::Zp, 3, false, true),   // 0x85
    op(Mn::Stx, Mode::Zp, 3, false, true),   // 0x86
    op(Mn::Sax, Mode::Zp, 3, false, false),  // 0x87
    op(Mn::Dey, Mode::Imp, 2, false, true),  // 0x88
    op(Mn::Nop, Mode::Imm, 2, false, false), // 0x89
    op(Mn::Txa, Mode::Imp, 2, false, true),  // 0x8A
    op(Mn::Xaa, Mode::Imm, 2, false, false), // 0x8B
    op(Mn::Sty, Mode::Abs, 4, false, true),  // 0x8C
    op(Mn::Sta, Mode::Abs, 4, false, true),  // 0x8D
    op(Mn::Stx, Mode::Abs, 4, false, true),  // 0x8E
    op(Mn::Sax, Mode::Abs, 4, false, false), // 0x8F
    op(Mn::Bcc, Mode::Rel, 2, false, true),  // 0x90
    op(Mn::Sta, Mode::Izy, 6, false, true),  // 0x91
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0x92
    op(Mn::Ahx, Mode::Izy, 6, false, false), // 0x93
    op(Mn::Sty, Mode::Zpx, 4, false, true),  // 0x94
    op(Mn::Sta, Mode::Zpx, 4, false, true),  // 0x95
    op(Mn::Stx, Mode::Zpy, 4, false, true),  // 0x96
    op(Mn::Sax, Mode::Zpy, 4, false, false), // 0x97
    op(Mn::Tya, Mode::Imp, 2, false, true),  // 0x98
    op(Mn::Sta, Mode::Aby, 5, false, true),  // 0x99
    op(Mn::Txs, Mode::Imp, 2, false, true),  // 0x9A
    op(Mn::Tas, Mode::Aby, 5, false, false), // 0x9B
    op(Mn::Shy, Mode::Abx, 5, false, false), // 0x9C
    op(Mn::Sta, Mode::Abx, 5, false, true),  // 0x9D
    op(Mn::Shx, Mode::Aby, 5, false, false), // 0x9E
    op(Mn::Ahx, Mode::Aby, 5, false, false), // 0x9F
    op(Mn::Ldy, Mode::Imm, 2, false, true),  // 0xA0
    op(Mn::Lda, Mode::Izx, 6, false, true),  // 0xA1
    op(Mn::Ldx, Mode::Imm, 2, false, true),  // 0xA2
    op(Mn::Lax, Mode::Izx, 6, false, false), // 0xA3
    op(Mn::Ldy, Mode::Zp, 3, false, true),   // 0xA4
    op(Mn::Lda, Mode::Zp, 3, false, true),   // 0xA5
    op(Mn::Ldx, Mode::Zp, 3, false, true),   // 0xA6
    op(Mn::Lax, Mode::Zp, 3, false, false),  // 0xA7
    op(Mn::Tay, Mode::Imp, 2, false, true),  // 0xA8
    op(Mn::Lda, Mode::Imm, 2, false, true),  // 0xA9
    op(Mn::Tax, Mode::Imp, 2, false, true),  // 0xAA
    op(Mn::Lax, Mode::Imm, 2, false, false), // 0xAB
    op(Mn::Ldy, Mode::Abs, 4, false, true),  // 0xAC
    op(Mn::Lda, Mode::Abs, 4, false, true),  // 0xAD
    op(Mn::Ldx, Mode::Abs, 4, false, true),  // 0xAE
    op(Mn::Lax, Mode::Abs, 4, false, false), // 0xAF
    op(Mn::Bcs, Mode::Rel, 2, false, true),  // 0xB0
    op(Mn::Lda, Mode::Izy, 5, true, true),   // 0xB1
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0xB2
    op(Mn::Lax, Mode::Izy, 5, true, false),  // 0xB3
    op(Mn::Ldy, Mode::Zpx, 4, false, true),  // 0xB4
    op(Mn::Lda, Mode::Zpx, 4, false, true),  // 0xB5
    op(Mn::Ldx, Mode::Zpy, 4, false, true),  // 0xB6
    op(Mn::Lax, Mode::Zpy, 4, false, false), // 0xB7
    op(Mn::Clv, Mode::Imp, 2, false, true),  // 0xB8
    op(Mn::Lda, Mode::Aby, 4, true, true),   // 0xB9
    op(Mn::Tsx, Mode::Imp, 2, false, true),  // 0xBA
    op(Mn::Las, Mode::Aby, 4, true, false),  // 0xBB
    op(Mn::Ldy, Mode::Abx, 4, true, true),   // 0xBC
    op(Mn::Lda, Mode::Abx, 4, true, true),   // 0xBD
    op(Mn::Ldx, Mode::Aby, 4, true, true),   // 0xBE
    op(Mn::Lax, Mode::Aby, 4, true, false),  // 0xBF
    op(Mn::Cpy, Mode::Imm, 2, false, true),  // 0xC0
    op(Mn::Cmp, Mode::Izx, 6, false, true),  // 0xC1
    op(Mn::Nop, Mode::Imm, 2, false, false), // 0xC2
    op(Mn::Dcp, Mode::Izx, 8, false, false), // 0xC3
    op(Mn::Cpy, Mode::Zp, 3, false, true),   // 0xC4
    op(Mn::Cmp, Mode::Zp, 3, false, true),   // 0xC5
    op(Mn::Dec, Mode::Zp, 5, false, true),   // 0xC6
    op(Mn::Dcp, Mode::Zp, 5, false, false),  // 0xC7
    op(Mn::Iny, Mode::Imp, 2, false, true),  // 0xC8
    op(Mn::Cmp, Mode::Imm, 2, false, true),  // 0xC9
    op(Mn::Dex, Mode::Imp, 2, false, true),  // 0xCA
    op(Mn::Axs, Mode::Imm, 2, false, false), // 0xCB
    op(Mn::Cpy, Mode::Abs, 4, false, true),  // 0xCC
    op(Mn::Cmp, Mode::Abs, 4, false, true),  // 0xCD
    op(Mn::Dec, Mode::Abs, 6, false, true),  // 0xCE
    op(Mn::Dcp, Mode::Abs, 6, false, false), // 0xCF
    op(Mn::Bne, Mode::Rel, 2, false, true),  // 0xD0
    op(Mn::Cmp, Mode::Izy, 5, true, true),   // 0xD1
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0xD2
    op(Mn::Dcp, Mode::Izy, 8, false, false), // 0xD3
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0xD4
    op(Mn::Cmp, Mode::Zpx, 4, false, true),  // 0xD5
    op(Mn::Dec, Mode::Zpx, 6, false, true),  // 0xD6
    op(Mn::Dcp, Mode::Zpx, 6, false, false), // 0xD7
    op(Mn::Cld, Mode::Imp, 2, false, true),  // 0xD8
    op(Mn::Cmp, Mode::Aby, 4, true, true),   // 0xD9
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0xDA
    op(Mn::Dcp, Mode::Aby, 7, false, false), // 0xDB
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0xDC
    op(Mn::Cmp, Mode::Abx, 4, true, true),   // 0xDD
    op(Mn::Dec, Mode::Abx, 7, false, true),  // 0xDE
    op(Mn::Dcp, Mode::Abx, 7, false, false), // 0xDF
    op(Mn::Cpx, Mode::Imm, 2, false, true),  // 0xE0
    op(Mn::Sbc, Mode::Izx, 6, false, true),  // 0xE1
    op(Mn::Nop, Mode::Imm, 2, false, false), // 0xE2
    op(Mn::Isb, Mode::Izx, 8, false, false), // 0xE3
    op(Mn::Cpx, Mode::Zp, 3, false, true),   // 0xE4
    op(Mn::Sbc, Mode::Zp, 3, false, true),   // 0xE5
    op(Mn::Inc, Mode::Zp, 5, false, true),   // 0xE6
    op(Mn::Isb, Mode::Zp, 5, false, false),  // 0xE7
    op(Mn::Inx, Mode::Imp, 2, false, true),  // 0xE8
    op(Mn::Sbc, Mode::Imm, 2, false, true),  // 0xE9
    op(Mn::Nop, Mode::Imp, 2, false, true),  // 0xEA
    op(Mn::Sbc, Mode::Imm, 2, false, false), // 0xEB
    op(Mn::Cpx, Mode::Abs, 4, false, true),  // 0xEC
    op(Mn::Sbc, Mode::Abs, 4, false, true),  // 0xED
    op(Mn::Inc, Mode::Abs, 6, false, true),  // 0xEE
    op(Mn::Isb, Mode::Abs, 6, false, false), // 0xEF
    op(Mn::Beq, Mode::Rel, 2, false, true),  // 0xF0
    op(Mn::Sbc, Mode::Izy, 5, true, true),   // 0xF1
    op(Mn::Jam, Mode::Imp, 2, false, false), // 0xF2
    op(Mn::Isb, Mode::Izy, 8, false, false), // 0xF3
    op(Mn::Nop, Mode::Zpx, 4, false, false), // 0xF4
    op(Mn::Sbc, Mode::Zpx, 4, false, true),  // 0xF5
    op(Mn::Inc, Mode::Zpx, 6, false, true),  // 0xF6
    op(Mn::Isb, Mode::Zpx, 6, false, false), // 0xF7
    op(Mn::Sed, Mode::Imp, 2, false, true),  // 0xF8
    op(Mn::Sbc, Mode::Aby, 4, true, true),   // 0xF9
    op(Mn::Nop, Mode::Imp, 2, false, false), // 0xFA
    op(Mn::Isb, Mode::Aby, 7, false, false), // 0xFB
    op(Mn::Nop, Mode::Abx, 4, true, false),  // 0xFC
    op(Mn::Sbc, Mode::Abx, 4, true, true),   // 0xFD
    op(Mn::Inc, Mode::Abx, 7, false, true),  // 0xFE
    op(Mn::Isb, Mode::Abx, 7, false, false), // 0xFF
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lda_immediate() {
        assert_eq!(OPS[0xA9], op(Mn::Lda, Mode::Imm, 2, false, true));
    }

    #[test]
    fn brk() {
        assert_eq!(OPS[0x00], op(Mn::Brk, Mode::Imp, 7, false, true));
    }

    #[test]
    fn jmp_indirect() {
        assert_eq!(OPS[0x6C], op(Mn::Jmp, Mode::Ind, 5, false, true));
    }

    #[test]
    fn lda_abx_has_page_penalty() {
        assert!(OPS[0xBD].page_penalty);
    }

    #[test]
    fn sta_abx_has_no_page_penalty() {
        assert!(!OPS[0x9D].page_penalty);
        assert_eq!(OPS[0x9D].cycles, 5);
    }

    #[test]
    fn branch_entry() {
        assert_eq!(OPS[0xD0], op(Mn::Bne, Mode::Rel, 2, false, true));
    }

    #[test]
    fn official_count() {
        assert_eq!(OPS.iter().filter(|o| o.official).count(), 151);
    }

    #[test]
    fn unofficial_lax() {
        assert_eq!(OPS[0xA7], op(Mn::Lax, Mode::Zp, 3, false, false));
    }

    #[test]
    fn unofficial_sbc() {
        assert_eq!(OPS[0xEB], op(Mn::Sbc, Mode::Imm, 2, false, false));
    }

    #[test]
    fn jam() {
        assert_eq!(OPS[0x02].mn, Mn::Jam);
    }

    #[test]
    fn nop_abx_penalty() {
        assert_eq!(OPS[0x1C], op(Mn::Nop, Mode::Abx, 4, true, false));
    }

    #[test]
    fn dcp_izy() {
        assert_eq!(OPS[0xD3], op(Mn::Dcp, Mode::Izy, 8, false, false));
    }
}
