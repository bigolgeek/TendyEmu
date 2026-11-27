//! The SPC700's tables and addressing.
//!
//! Every expected number comes from fullsnes's SPC700 command tables (the `Clk` column), its
//! "SNES APU Memory and I/O Map" and its "SNES APU SPC700 I/O Ports" section; the failure messages
//! say which.

use tendy_snes::APU_RAM_SIZE;
use tendy_snes::apu::Dsp;
use tendy_snes::apu::spc700::{Psw, Spc700, SpcBus};

/// An SPC700 with its own RAM, DSP and latches — the smallest thing `SpcBus` needs.
struct Spc {
    ram: Box<[u8; APU_RAM_SIZE]>,
    cpu: Spc700,
    dsp: Dsp,
    to_apu: [u8; 4],
    from_apu: [u8; 4],
}

impl Spc {
    fn new() -> Spc {
        let mut spc = Spc {
            ram: Box::new([0; APU_RAM_SIZE]),
            cpu: Spc700::new(),
            dsp: Dsp::new(),
            to_apu: [0; 4],
            from_apu: [0; 4],
        };
        // The boot ROM is off so the program under test is what runs.
        spc.cpu.control = 0;
        spc.cpu.pc = 0x0200;
        spc.cpu.sp = 0xEF;
        spc.cpu.psw = Psw(0);
        spc
    }

    fn step(&mut self) -> u32 {
        let mut bus = SpcBus {
            ram: &mut self.ram,
            dsp: &mut self.dsp,
            to_apu: &mut self.to_apu,
            from_apu: &mut self.from_apu,
        };
        self.cpu.step(&mut bus)
    }

    fn read(&mut self, addr: u16) -> u8 {
        let mut bus = SpcBus {
            ram: &mut self.ram,
            dsp: &mut self.dsp,
            to_apu: &mut self.to_apu,
            from_apu: &mut self.from_apu,
        };
        self.cpu.read(&mut bus, addr)
    }

    fn write(&mut self, addr: u16, value: u8) {
        let mut bus = SpcBus {
            ram: &mut self.ram,
            dsp: &mut self.dsp,
            to_apu: &mut self.to_apu,
            from_apu: &mut self.from_apu,
        };
        self.cpu.write(&mut bus, addr, value);
    }

    /// Puts `bytes` at `$0200`.
    fn load(&mut self, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.ram[0x0200 + i] = *byte;
        }
    }

    fn run(&mut self, instructions: usize) -> u32 {
        let mut total = 0;
        for _ in 0..instructions {
            total += self.step();
        }
        total
    }
}

/// One row of the cycle table: the opcode, its operand bytes, the PSW it runs with, any bytes to set
/// up first, and the `Clk` value fullsnes's table gives.
struct Row {
    opcode: u8,
    operands: &'static [u8],
    psw: u8,
    pre: &'static [(u16, u8)],
    cycles: u32,
    name: &'static str,
}

const fn row(opcode: u8, operands: &'static [u8], cycles: u32, name: &'static str) -> Row {
    Row {
        opcode,
        operands,
        psw: 0,
        pre: &[],
        cycles,
        name,
    }
}

#[test]
fn opcode_cycle_counts_match_the_tables() {
    // All numbers are fullsnes's `Clk` column for the state the row sets up. With PSW = 0 a
    // conditional branch is taken by BPL/BVC/BCC/BNE (the flag is clear) and not by BMI/BVS/BCS/BEQ.
    let rows = [
        // Loads, stores and addressing modes.
        row(0x00, &[], 2, "NOP"),
        row(0xE8, &[0x12], 2, "MOV A,#nn"),
        row(0xCD, &[0x12], 2, "MOV X,#nn"),
        row(0x8D, &[0x12], 2, "MOV Y,#nn"),
        row(0x7D, &[], 2, "MOV A,X"),
        row(0x5D, &[], 2, "MOV X,A"),
        row(0xDD, &[], 2, "MOV A,Y"),
        row(0xFD, &[], 2, "MOV Y,A"),
        row(0x9D, &[], 2, "MOV X,SP"),
        row(0xBD, &[], 2, "MOV SP,X"),
        row(0xE4, &[0x10], 3, "MOV A,dp"),
        row(0xF4, &[0x10], 4, "MOV A,dp+X"),
        row(0xE5, &[0x00, 0x05], 4, "MOV A,abs"),
        row(0xF5, &[0x00, 0x05], 5, "MOV A,abs+X"),
        row(0xF6, &[0x00, 0x05], 5, "MOV A,abs+Y"),
        row(0xE6, &[], 3, "MOV A,(X)"),
        row(0xBF, &[], 4, "MOV A,(X)+"),
        row(0xF7, &[0x30], 6, "MOV A,[dp]+Y"),
        row(0xE7, &[0x30], 6, "MOV A,[dp+X]"),
        row(0xF8, &[0x10], 3, "MOV X,dp"),
        row(0xF9, &[0x10], 4, "MOV X,dp+Y"),
        row(0xE9, &[0x00, 0x05], 4, "MOV X,abs"),
        row(0xEB, &[0x10], 3, "MOV Y,dp"),
        row(0xFB, &[0x10], 4, "MOV Y,dp+X"),
        row(0xEC, &[0x00, 0x05], 4, "MOV Y,abs"),
        row(0xBA, &[0x10], 5, "MOVW YA,dp"),
        row(0x8F, &[0x12, 0x10], 5, "MOV dp,#nn"),
        row(0xFA, &[0x10, 0x11], 5, "MOV dp,dp"),
        row(0xC4, &[0x10], 4, "MOV dp,A"),
        row(0xD8, &[0x10], 4, "MOV dp,X"),
        row(0xCB, &[0x10], 4, "MOV dp,Y"),
        row(0xD4, &[0x10], 5, "MOV dp+X,A"),
        row(0xDB, &[0x10], 5, "MOV dp+X,Y"),
        row(0xD9, &[0x10], 5, "MOV dp+Y,X"),
        row(0xC5, &[0x00, 0x05], 5, "MOV abs,A"),
        row(0xC9, &[0x00, 0x05], 5, "MOV abs,X"),
        row(0xCC, &[0x00, 0x05], 5, "MOV abs,Y"),
        row(0xD5, &[0x00, 0x05], 6, "MOV abs+X,A"),
        row(0xD6, &[0x00, 0x05], 6, "MOV abs+Y,A"),
        row(0xAF, &[], 4, "MOV (X)+,A"),
        row(0xC6, &[], 4, "MOV (X),A"),
        row(0xD7, &[0x30], 7, "MOV [dp]+Y,A"),
        row(0xC7, &[0x30], 7, "MOV [dp+X],A"),
        row(0xDA, &[0x10], 5, "MOVW dp,YA"),
        // Push and pop.
        row(0x2D, &[], 4, "PUSH A"),
        row(0x4D, &[], 4, "PUSH X"),
        row(0x6D, &[], 4, "PUSH Y"),
        row(0x0D, &[], 4, "PUSH PSW"),
        row(0xAE, &[], 4, "POP A"),
        row(0xCE, &[], 4, "POP X"),
        row(0xEE, &[], 4, "POP Y"),
        row(0x8E, &[], 4, "POP PSW"),
        // The twelve ALU forms, from the OR row (`x+04` and the rest).
        row(0x04, &[0x10], 3, "OR A,dp"),
        row(0x05, &[0x00, 0x05], 4, "OR A,abs"),
        row(0x06, &[], 3, "OR A,(X)"),
        row(0x07, &[0x30], 6, "OR A,[dp+X]"),
        row(0x08, &[0x12], 2, "OR A,#nn"),
        row(0x09, &[0x10, 0x11], 6, "OR dp,dp"),
        row(0x14, &[0x10], 4, "OR A,dp+X"),
        row(0x15, &[0x00, 0x05], 5, "OR A,abs+X"),
        row(0x16, &[0x00, 0x05], 5, "OR A,abs+Y"),
        row(0x17, &[0x30], 6, "OR A,[dp]+Y"),
        row(0x18, &[0x12, 0x10], 5, "OR dp,#nn"),
        row(0x19, &[], 5, "OR (X),(Y)"),
        row(0x24, &[0x10], 3, "AND A,dp"),
        row(0x44, &[0x10], 3, "EOR A,dp"),
        row(0x64, &[0x10], 3, "CMP A,dp"),
        row(0x84, &[0x10], 3, "ADC A,dp"),
        row(0xA4, &[0x10], 3, "SBC A,dp"),
        // Compares against X and Y.
        row(0xC8, &[0x12], 2, "CMP X,#nn"),
        row(0xAD, &[0x12], 2, "CMP Y,#nn"),
        row(0x3E, &[0x10], 3, "CMP X,dp"),
        row(0x7E, &[0x10], 3, "CMP Y,dp"),
        row(0x1E, &[0x00, 0x05], 4, "CMP X,abs"),
        row(0x5E, &[0x00, 0x05], 4, "CMP Y,abs"),
        // Shifts, increments and decrements.
        row(0x1C, &[], 2, "ASL A"),
        row(0x3C, &[], 2, "ROL A"),
        row(0x5C, &[], 2, "LSR A"),
        row(0x7C, &[], 2, "ROR A"),
        row(0x9C, &[], 2, "DEC A"),
        row(0xBC, &[], 2, "INC A"),
        row(0x3D, &[], 2, "INC X"),
        row(0x1D, &[], 2, "DEC X"),
        row(0xFC, &[], 2, "INC Y"),
        row(0xDC, &[], 2, "DEC Y"),
        row(0x0B, &[0x10], 4, "ASL dp"),
        row(0x1B, &[0x10], 5, "ASL dp+X"),
        row(0x0C, &[0x00, 0x05], 5, "ASL abs"),
        // 16-bit.
        row(0x7A, &[0x10], 5, "ADDW YA,dp"),
        row(0x9A, &[0x10], 5, "SUBW YA,dp"),
        row(0x5A, &[0x10], 4, "CMPW YA,dp"),
        row(0x3A, &[0x10], 6, "INCW dp"),
        row(0x1A, &[0x10], 6, "DECW dp"),
        row(0x9E, &[], 12, "DIV YA,X"),
        row(0xCF, &[], 9, "MUL YA"),
        // Single bits. The operand is one 16-bit `aaa.b` word — bits 0-12 the address, 13-15 the bit
        // number — so these rows carry two operand bytes, as fullsnes's `AA aa ba` column does.
        row(0x0A, &[0x00, 0x05], 5, "OR1 C,aaa.b"),
        row(0x2A, &[0x00, 0x05], 5, "OR1 C,/aaa.b"),
        row(0x4A, &[0x00, 0x05], 4, "AND1 C,aaa.b"),
        row(0x6A, &[0x00, 0x05], 4, "AND1 C,/aaa.b"),
        row(0x8A, &[0x00, 0x05], 5, "EOR1 C,aaa.b"),
        row(0xEA, &[0x00, 0x05], 5, "NOT1 aaa.b"),
        row(0xAA, &[0x00, 0x05], 4, "MOV1 C,aaa.b"),
        row(0xCA, &[0x00, 0x05], 6, "MOV1 aaa.b,C"),
        row(0x0E, &[0x00, 0x05], 6, "TSET1 abs"),
        row(0x4E, &[0x00, 0x05], 6, "TCLR1 abs"),
        row(0x02, &[0x10], 4, "SET1 dp.0"),
        row(0x12, &[0x10], 4, "CLR1 dp.0"),
        row(0x60, &[], 2, "CLRC"),
        row(0x80, &[], 2, "SETC"),
        row(0xED, &[], 3, "NOTC"),
        row(0xE0, &[], 2, "CLRV"),
        row(0x9F, &[], 5, "XCN A"),
        row(0xDF, &[], 3, "DAA A"),
        row(0xBE, &[], 3, "DAS A"),
        row(0x20, &[], 2, "CLRP"),
        row(0x40, &[], 2, "SETP"),
        row(0xA0, &[], 3, "EI"),
        row(0xC0, &[], 3, "DI"),
        row(0xEF, &[], 3, "SLEEP"),
        row(0xFF, &[], 3, "STOP"),
        // Jumps, calls and branches: PSW = 0 (taken for BPL/BVC/BCC/BNE).
        row(0x2F, &[0x02], 4, "BRA"),
        row(0x5F, &[0x00, 0x05], 3, "JMP abs"),
        row(0x1F, &[0x00, 0x05], 6, "JMP [abs+X]"),
        row(0x3F, &[0x00, 0x05], 8, "CALL abs"),
        row(0x4F, &[0x10], 6, "PCALL $FFnn"),
        row(0x6F, &[], 5, "RET"),
        row(0x7F, &[], 6, "RET1"),
        row(0x0F, &[], 8, "BRK"),
        row(0x01, &[], 8, "TCALL 0"),
        row(0xF1, &[], 8, "TCALL 15"),
        row(0x10, &[0x02], 4, "BPL taken"),
        row(0x30, &[0x02], 2, "BMI not taken"),
        row(0x50, &[0x02], 4, "BVC taken"),
        row(0x70, &[0x02], 2, "BVS not taken"),
        row(0x90, &[0x02], 4, "BCC taken"),
        row(0xB0, &[0x02], 2, "BCS not taken"),
        row(0xD0, &[0x02], 4, "BNE taken"),
        row(0xF0, &[0x02], 2, "BEQ not taken"),
        row(0x2E, &[0x10, 0x02], 7, "CBNE dp taken"),
        row(0xDE, &[0x10, 0x02], 8, "CBNE dp+X taken"),
        row(0x03, &[0x10, 0x02], 5, "BBS not taken"),
        row(0x13, &[0x10, 0x02], 7, "BBC taken"),
        row(0xFE, &[0x02], 6, "DBNZ Y taken"),
        row(0x6E, &[0x10, 0x02], 7, "DBNZ dp taken"),
    ];

    for r in &rows {
        let mut spc = Spc::new();
        for (addr, value) in r.pre {
            spc.ram[usize::from(*addr)] = *value;
        }
        spc.cpu.psw = Psw(r.psw);
        spc.cpu.a = 0x12;
        spc.cpu.y = 2;
        let mut bytes = vec![r.opcode];
        bytes.extend_from_slice(r.operands);
        spc.load(&bytes);
        let cycles = spc.step();
        assert_eq!(
            cycles, r.cycles,
            "{} (opcode ${:02X}) takes {} clocks in fullsnes's Clk column, not {}",
            r.name, r.opcode, r.cycles, cycles
        );
    }

    // The other side of the conditional branches: every flag set.
    for (opcode, expected) in [
        (0x10u8, 2u32),
        (0x30, 4),
        (0x50, 2),
        (0x70, 4),
        (0x90, 2),
        (0xB0, 4),
        (0xD0, 2),
        (0xF0, 4),
    ] {
        let mut spc = Spc::new();
        spc.cpu.psw = Psw(Psw::N | Psw::V | Psw::C | Psw::Z);
        spc.load(&[opcode, 0x02]);
        assert_eq!(
            spc.step(),
            expected,
            "opcode ${opcode:02X} with N/V/C/Z set takes {expected} clocks"
        );
    }
}

#[test]
fn load_store_and_addressing_modes() {
    let mut spc = Spc::new();
    // A pointer at $30 -> $0500, and $0502 holding a marker the indirect load must find.
    spc.ram[0x30] = 0x00;
    spc.ram[0x31] = 0x05;
    spc.ram[0x0502] = 0x99;
    spc.load(&[
        0xE8, 0x34, // MOV A,#$34
        0xCD, 0x03, // MOV X,#3
        0xD4, 0x20, // MOV $20+X,A     -> $0023 = $34
        0xAF, // MOV (X)+,A      -> $0003 = $34, X = 4
        0x8F, 0x5A, 0x40, // MOV $40,#$5A
        0xE4, 0x40, // MOV A,$40
        0x8D, 0x02, // MOV Y,#2
        0xF7, 0x30, // MOV A,[$30]+Y   -> A = $99
        0x8F, 0x11, 0x50, // MOV $50,#$11
        0x8F, 0x22, 0x51, // MOV $51,#$22
        0xBA, 0x50, // MOVW YA,$50
    ]);
    spc.run(11);
    assert_eq!(spc.ram[0x0023], 0x34, "MOV dp+X,A stores at $0023");
    assert_eq!(spc.ram[0x0003], 0x34, "MOV (X)+,A stores at $0003");
    assert_eq!(spc.cpu.x, 4, "MOV (X)+,A post-increments X");
    assert_eq!(spc.ram[0x0040], 0x5A, "MOV dp,#nn writes the direct page");
    assert_eq!(spc.cpu.a, 0x11, "MOVW YA,dp loads the low byte into A");
    assert_eq!(spc.cpu.y, 0x22, "MOVW YA,dp loads the high byte into Y");

    // The indirect load itself, before the later stores changed A.
    let mut spc = Spc::new();
    spc.ram[0x30] = 0x00;
    spc.ram[0x31] = 0x05;
    spc.ram[0x0502] = 0x99;
    spc.load(&[0x8D, 0x02, 0xF7, 0x30]);
    spc.run(2);
    assert_eq!(spc.cpu.a, 0x99, "MOV A,[dp]+Y reads Word[dp]+Y");
}

#[test]
fn psw_direct_page_bit_selects_page_one() {
    let mut spc = Spc::new();
    spc.load(&[
        0x8F, 0x5A, 0x40, // MOV $40,#$5A     (P = 0 -> $0040)
        0x40, // SETP
        0x8F, 0x7E, 0x40, // MOV $40,#$7E     (P = 1 -> $0140)
        0xE4, 0x40, // MOV A,$40        (P = 1 -> $0140)
    ]);
    spc.run(4);
    assert!(spc.cpu.psw.p(), "SETP sets PSW bit 5");
    assert_eq!(
        spc.ram[0x0040], 0x5A,
        "PSW.P = 0 addresses the direct page at $00xx"
    );
    assert_eq!(
        spc.ram[0x0140], 0x7E,
        "PSW.P = 1 addresses the direct page at $01xx"
    );
    assert_eq!(spc.cpu.a, 0x7E, "MOV A,dp reads through the page bit");
}

#[test]
fn x_and_y_indirect_follow_the_direct_page_bit() {
    // fullsnes "SNES APU SPC700 CPU Overview", Addressing Modes, lists `(X)` and `(Y)` in the same
    // group as `aa`, `aa+X` and `aa+Y`:
    //
    // ```text
    //   aa+X           [aa+X]         ; (or at [0100..01FF when flag P=1)
    //   aa+Y           [aa+Y]         ; (aa+X and aa+Y wrap within that area,
    //   (X)            [X]            ; ie. addr = (aa+X) AND 0FFh)
    //   (Y)            [Y]            ;/
    // ```
    //
    // so an 8-bit index picks a byte of the page PSW.P names — not always page 0. A driver that
    // sets P to keep its variables in page 1 and clears one with `MOV (X),A` would otherwise write
    // page 0's byte of the same offset, which for offset 0 is the command byte it is about to
    // dispatch (Tetris Attack's driver does exactly this at $137B).

    // MOV (X),A: P = 1 must write $0110, and must not touch $0010.
    let mut spc = Spc::new();
    spc.load(&[
        0xCD, 0x10, // MOV X,#$10
        0xE8, 0xA5, // MOV A,#$A5
        0x40, // SETP
        0xC6, // MOV (X),A
    ]);
    spc.run(4);
    assert_eq!(
        spc.ram[0x0110], 0xA5,
        "PSW.P = 1: `(X)` addresses $0100+X (fullsnes \"Addressing Modes\")"
    );
    assert_eq!(
        spc.ram[0x0010], 0x00,
        "PSW.P = 1: `(X)` must not reach page 0's $10"
    );

    // MOV A,(X): P = 0 still reads page 0.
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x3C;
    spc.ram[0x0110] = 0x7E;
    spc.load(&[
        0xCD, 0x10, // MOV X,#$10
        0xE6, // MOV A,(X)
    ]);
    spc.run(2);
    assert_eq!(
        spc.cpu.a, 0x3C,
        "PSW.P = 0: `(X)` addresses $00xx (fullsnes \"Addressing Modes\")"
    );

    // MOV A,(X)+ / MOV (X)+,A: the same page, and the index wraps inside it.
    let mut spc = Spc::new();
    spc.ram[0x01FF] = 0x11;
    spc.load(&[
        0xCD, 0xFF, // MOV X,#$FF
        0x40, // SETP
        0xBF, // MOV A,(X)+  -> reads $01FF, X -> $00
        0xAF, // MOV (X)+,A  -> writes $0100, X -> $01
    ]);
    spc.run(4);
    assert_eq!(
        spc.cpu.a, 0x11,
        "MOV A,(X)+ reads the page PSW.P names (fullsnes: addr = (aa+X) AND 0FFh)"
    );
    assert_eq!(
        spc.ram[0x0100], 0x11,
        "MOV (X)+,A wraps inside page 1 and writes $0100, not $0000"
    );
    assert_eq!(spc.cpu.x, 0x01, "MOV (X)+,A increments X");

    // The ALU forms: `A op (X)` is form $06 and `(X) op (Y)` is form $19.
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x0F;
    spc.ram[0x0110] = 0xF0;
    spc.load(&[
        0xCD, 0x10, // MOV X,#$10
        0xE8, 0x01, // MOV A,#$01
        0x40, // SETP
        0x06, // OR A,(X)   -> $0110 must supply $F0, so A = $F1
    ]);
    spc.run(4);
    assert_eq!(
        spc.cpu.a, 0xF1,
        "`OR A,(X)` reads the page PSW.P names (fullsnes form $06)"
    );

    let mut spc = Spc::new();
    spc.ram[0x000F] = 0x01; // page 0 decoy for (Y)
    spc.ram[0x0010] = 0x02; // page 0 decoy for (X)
    spc.ram[0x010F] = 0xF0; // what (Y) must read with P = 1
    spc.load(&[
        0xCD, 0x10, // MOV X,#$10  (destination, page 1 at $0110 = $00)
        0x8D, 0x0F, // MOV Y,#$0F  (source, page 1 at $010F = $F0)
        0x40, // SETP
        0x19, // OR (X),(Y)  -> $0110 = $00 | $F0
    ]);
    spc.run(4);
    assert_eq!(
        spc.ram[0x0110], 0xF0,
        "`(X) op (Y)` addresses both operands in the page PSW.P names (fullsnes form $19); \
         reading page 0 would have given $01"
    );
    assert_eq!(
        spc.ram[0x0010], 0x02,
        "`(X) op (Y)` must not write the page-0 byte of the same offset"
    );
}

#[test]
fn timers_divide_the_8khz_source() {
    // fullsnes "SNES Timing Oscillators": "SPC700 timer 0+1 8000Hz", and `TnDIV` divides it.
    // The tick argument is the 64 kHz tick the timers run at (T1 and T17 of the DSP's loop), so an
    // 8 kHz pulse is one in eight.
    let mut spc = Spc::new();
    spc.cpu.control = 0x01; // timer 0 enabled
    spc.cpu.t0div = 1; // divide by 1

    let mut pulses = 0;
    for tick in 0..64u64 {
        let before = spc.cpu.t0out;
        spc.cpu.timer_clock(tick);
        if spc.cpu.t0out != before {
            pulses += 1;
        }
    }
    assert_eq!(
        pulses, 8,
        "an 8 kHz source over 64 ticks at 64 kHz is 8 pulses, not {pulses}"
    );
    assert_eq!(spc.cpu.t0out, 8, "timer 0's counter after 8 pulses");

    // Divide by 2 halves it again.
    let mut spc = Spc::new();
    spc.cpu.control = 0x01;
    spc.cpu.t0div = 2;
    for tick in 0..64u64 {
        spc.cpu.timer_clock(tick);
    }
    assert_eq!(spc.cpu.t0out, 4, "T0DIV = 2 halves the 8 kHz rate");

    // Reading `$FD` returns the counter and resets it ("reset to 0 after reading").
    let mut spc = Spc::new();
    spc.cpu.t0out = 0x0F;
    assert_eq!(spc.read(0x00FD), 0x0F, "T0OUT reads its 4 bits");
    assert_eq!(spc.cpu.t0out, 0, "a read of T0OUT resets it");
}

#[test]
fn timer2_counts_at_64khz() {
    // "timer 2 64000Hz" — the same tick as the timer's own clock, so no eighth.
    let mut spc = Spc::new();
    spc.cpu.control = 0x04;
    spc.cpu.t2div = 1;
    for tick in 0..8u64 {
        spc.cpu.timer_clock(tick);
    }
    assert_eq!(
        spc.cpu.t2out, 8,
        "timer 2 at 64 kHz takes every tick; 8 ticks give 8 counts"
    );

    // The same eight ticks give timer 0 exactly one.
    let mut spc = Spc::new();
    spc.cpu.control = 0x05;
    spc.cpu.t0div = 1;
    spc.cpu.t2div = 1;
    for tick in 0..8u64 {
        spc.cpu.timer_clock(tick);
    }
    assert_eq!(
        spc.cpu.t0out, 1,
        "timer 0 counts one per eight 64 kHz ticks"
    );
    assert_eq!(spc.cpu.t2out, 8, "timer 2 counts one per 64 kHz tick");

    // A four-bit counter that overflows wraps to zero: bits 4-7 are unused and always read zero.
    // fullsnes `00FDh - T0OUT - Timer 0 Output (R)`: "0-3 Incremented at the rate selected via TnDIV
    // (reset to 0 after reading) / 4-7 Not used (always zero)". The bit-7 overflow flag this test
    // used to assert does not exist on the chip; a driver that reads the counter as a loop count
    // (MOV Y,$T1OUT / DEC Y / BNE) sees 0-15, never 128-143.
    let mut spc = Spc::new();
    spc.cpu.t2out = 0x0F;
    spc.cpu.control = 0x04;
    spc.cpu.t2div = 1;
    spc.cpu.timer_clock(0);
    assert_eq!(
        spc.cpu.t2out, 0x00,
        "a 4-bit overflow wraps, it does not set bit 7"
    );
}

#[test]
fn compare_to_memory_does_not_store() {
    // fullsnes "SNES APU SPC700 CPU ALU Commands": the group is `CMP a,b  60+x ...    .. a-b` with
    // the flags `N.....ZC` — a flag result with no `a=result`, unlike `OR`'s `a=a OR b`. The three
    // forms whose destination is memory (`cmd aa,bb` x+09, `cmd aa,#nn` x+18, `cmd (X),(Y)` x+19)
    // therefore have no store cycle. That is visible on hardware: a write to `$00F0-$00FF` is not
    // silent — fullsnes "SPC700 I/O Ports" gives `$F4-$F7` as the S-CPU's output latches — so a
    // `CMP $F7,#$00` poll that stored would echo the S-CPU's own byte back to it a byte early.
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x40;
    spc.to_apu[3] = 0x22;
    spc.load(&[
        0x78, 0x11, 0x10, // CMP $10,#$11 -> $10-$11 = $2F, and $10 must keep $40
        0x78, 0x11, 0xF7, // CMP $F7,#$11 -> $F7 is the S-CPU's latch, not a workspace
    ]);
    spc.step();
    assert_eq!(
        spc.ram[0x0010], 0x40,
        "a compare leaves its destination alone"
    );
    assert!(!spc.cpu.psw.z(), "40h - 11h is not zero");
    spc.step();
    assert_eq!(
        spc.from_apu[3], 0x00,
        "CMP $F7,#$11 must not echo $11 back to the S-CPU on $2143"
    );
    assert_eq!(
        spc.read(0x00F7),
        0x22,
        "the S-CPU's byte is still there to read"
    );
}

#[test]
fn timer_output_is_four_bits() {
    // fullsnes "SPC700 I/O Ports", `00FDh - T0OUT`: "0-3 Incremented at the rate selected via TnDIV
    // (reset to 0 after reading) / 4-7 Not used (always zero)". Twenty 8 kHz pulses are twenty
    // counts on a four-bit counter, so the byte the cartridge reads back is 20 & $0F = 4. (A core
    // with an overflow flag in bit 7 would answer $84, and a driver that turns the counter into a
    // `DEC`/`BNE` loop count would run 128-143 iterations instead of 0-15.)
    let mut spc = Spc::new();
    spc.cpu.control = 0x01;
    spc.cpu.t0div = 1;
    for tick in 0..160u64 {
        spc.cpu.timer_clock(tick);
    }
    assert_eq!(
        spc.read(0x00FD),
        0x04,
        "twenty counts on a four-bit counter read back as 4"
    );
}

#[test]
fn io_writes_also_land_in_ram() {
    let mut spc = Spc::new();
    spc.to_apu[0] = 0x33;

    spc.write(0x00F2, 0x4C);
    assert_eq!(spc.cpu.dspaddr, 0x4C, "$F2 is the DSP register index");
    assert_eq!(spc.ram[0x00F2], 0x4C, "an I/O write also lands in RAM");

    spc.write(0x00F3, 0x1F);
    assert_eq!(
        spc.dsp.regs[0x4C], 0x1F,
        "$F3 writes the register $F2 selects"
    );
    assert_eq!(spc.ram[0x00F3], 0x1F, "an I/O write also lands in RAM");

    spc.write(0x00F4, 0x77);
    assert_eq!(spc.from_apu[0], 0x77, "$F4 writes the S-CPU's output latch");
    assert_eq!(spc.ram[0x00F4], 0x77, "an I/O write also lands in RAM");
    assert_eq!(
        spc.read(0x00F4),
        0x33,
        "the SPC reads the S-CPU's input latch at $F4, not its own output"
    );

    spc.write(0x00F0, 0x0A);
    assert_eq!(spc.cpu.test, 0x0A, "$F0 is TEST");
    assert_eq!(spc.ram[0x00F0], 0x0A, "TEST also lands in RAM");

    // `$FD-$FF` are read-only: a write reaches RAM but not the counter.
    spc.cpu.t0out = 0x03;
    spc.write(0x00FD, 0x55);
    assert_eq!(spc.cpu.t0out, 0x03, "T0OUT ignores a write");
    assert_eq!(spc.ram[0x00FD], 0x55, "but the byte still lands in RAM");
}

#[test]
fn stack_and_call_return() {
    let mut spc = Spc::new();
    // The vector BRK and TCALL 0 share, and a little routine at $0300.
    spc.ram[0xFFDE] = 0x00;
    spc.ram[0xFFDF] = 0x03;
    spc.ram[0x0300] = 0x7F; // RET1
    spc.load(&[
        0x3F, 0x10, 0x02, // CALL $0210
        0x00, // NOP          (only reached if the return worked)
        0x0F, // BRK
    ]);
    spc.ram[0x0210] = 0x3D; // INC X
    spc.ram[0x0211] = 0x6F; // RET

    let first = spc.step();
    assert_eq!(first, 8, "CALL charges 8 clocks");
    assert_eq!(spc.cpu.pc, 0x0210, "CALL jumps to its operand");
    assert_eq!(spc.cpu.sp, 0xED, "CALL pushes two bytes");
    assert_eq!(spc.ram[0x01EF], 0x02, "the return address's high byte");
    assert_eq!(spc.ram[0x01EE], 0x03, "the return address's low byte");

    assert_eq!(spc.step(), 2, "INC X charges 2 clocks");
    assert_eq!(spc.step(), 5, "RET charges 5 clocks");
    assert_eq!(spc.cpu.pc, 0x0203, "RET returns to the exact address");
    assert_eq!(spc.cpu.sp, 0xEF, "RET pops both bytes");
    assert_eq!(spc.cpu.x, 1, "the called routine ran");

    // The NOP at $0203 proves the return landed there, then the BRK goes through $FFDE.
    assert_eq!(spc.step(), 2, "the NOP after the return charges 2 clocks");
    assert_eq!(spc.cpu.pc, 0x0204, "the NOP leaves PC at the BRK");

    // BRK pushes PC and PSW and jumps through $FFDE; RET1 restores both.
    let psw_before = spc.cpu.psw.0;
    spc.cpu.a = 0x44;
    let brk = spc.step();
    assert_eq!(brk, 8, "BRK charges 8 clocks");
    assert_eq!(spc.cpu.pc, 0x0300, "BRK jumps through Word[$FFDE]");
    assert_eq!(spc.cpu.sp, 0xEC, "BRK pushes three bytes");
    assert_eq!(
        spc.ram[0x01EE], 0x05,
        "BRK pushes the address after the opcode"
    );
    assert!(spc.cpu.psw.b(), "BRK sets B");
    assert!(!spc.cpu.psw.i(), "BRK clears I");

    let ret1 = spc.step();
    assert_eq!(ret1, 6, "RET1 charges 6 clocks");
    assert_eq!(spc.cpu.pc, 0x0205, "RET1 returns past the BRK");
    assert_eq!(
        spc.cpu.psw.0, psw_before,
        "RET1 restores the flags BRK changed"
    );

    // TCALL 0 pushes the PC and jumps through the same vector.
    let mut spc = Spc::new();
    spc.ram[0xFFDE] = 0x00;
    spc.ram[0xFFDF] = 0x03;
    spc.load(&[0x01]);
    assert_eq!(spc.step(), 8, "TCALL charges 8 clocks");
    assert_eq!(spc.cpu.pc, 0x0300, "TCALL 0 jumps through Word[$FFDE]");
    assert_eq!(spc.ram[0x01EF], 0x02, "TCALL pushes the return address");
    assert_eq!(spc.ram[0x01EE], 0x01, "TCALL pushes the returned-to PC");
}

/// `MOV SP,X` moves the stack pointer, and a driver depends on it. fullsnes "SNES APU SPC700
/// Load/Store Commands" gives `MOV SP,X BD 2 SP=X`.
///
/// The shape below is the one 18 of the 81 blank rows take (Air Cavalry, WWF RAW, Super Jedi, Chavez
/// II, BoogerMan at `$0005`; Bill Walsh, Lester, Madden NFL '94, WE'RE BACK at `$01F0`; …): the driver
/// moves its stack out from under a RAM clear, calls a routine that zeroes `$0000-$00EF` and
/// `$0100-$01EF` (the stack page included, deliberately leaving `$01F0-$01FF` alone), and returns. With
/// `MOV SP,X` a no-op, SP stays at the IPL's `$EF`, `CALL` pushes its return address at `$01ED-$01EF` —
/// inside the region the clear loop zeroes — and `RET` pops `$0000`, so the SPC walks into zero page and
/// stops on whatever byte it finds there (`$FF` at `$0005` for the five above).
#[test]
fn mov_sp_x_moves_the_stack_pointer_out_of_a_clear() {
    let mut spc = Spc::new();
    spc.load(&[
        0xCD, 0xFF, // MOV X,#$FF
        0xBD, // MOV SP,X
        0x3F, 0x10, 0x02, // CALL $0210
        0xE8, 0x5A, // MOV A,#$5A     (only reached if the return worked)
    ]);
    spc.ram[0x0210] = 0xE8; // MOV A,#$00
    spc.ram[0x0211] = 0x00;
    spc.ram[0x0212] = 0x5D; // MOV X,A        (X = 0)
    spc.ram[0x0213] = 0xD5; // MOV $0100+X,A
    spc.ram[0x0214] = 0x00;
    spc.ram[0x0215] = 0x01;
    spc.ram[0x0216] = 0x3D; // INC X
    spc.ram[0x0217] = 0xC8; // CMP X,#$F0
    spc.ram[0x0218] = 0xF0;
    spc.ram[0x0219] = 0x90; // BCC $0213
    spc.ram[0x021A] = 0xF8;
    spc.ram[0x021B] = 0x6F; // RET

    assert_eq!(spc.step(), 2, "MOV X,#nn charges 2 clocks");
    assert_eq!(spc.step(), 2, "MOV SP,X charges 2 clocks");
    assert_eq!(spc.cpu.sp, 0xFF, "MOV SP,X sets SP from X");

    assert_eq!(spc.step(), 8, "CALL charges 8 clocks");
    assert_eq!(spc.cpu.pc, 0x0210, "CALL jumps to its operand");
    assert_eq!(spc.cpu.sp, 0xFD, "CALL pushes two bytes below $01FF");
    assert_eq!(spc.ram[0x01FF], 0x02, "the return address's high byte");
    assert_eq!(spc.ram[0x01FE], 0x06, "the return address's low byte");

    // `MOV A,#$00` and `MOV X,A`, then 240 passes of `MOV $0100+X,A` / `INC X` / `CMP X,#$F0` /
    // `BCC` (X = $00..$EF), then `RET`.
    for _ in 0..(2 + 0xF0 * 4 + 1) {
        spc.step();
    }
    assert!(
        spc.ram[0x0100..=0x01EF].iter().all(|&b| b == 0),
        "the routine zeroed $0100-$01EF, the stack page included"
    );
    assert_eq!(
        spc.ram[0x01FF], 0x02,
        "the return address sits above the cleared range"
    );
    assert_eq!(
        spc.cpu.pc, 0x0206,
        "RET returns to the instruction after CALL"
    );
    spc.step();
    assert_eq!(spc.cpu.a, 0x5A, "the caller carried on");
}

/// `INCW`/`DECW` set N and Z from the **16-bit** result, and `ADDW`/`SUBW` too — fullsnes
/// "16bit ALU Operations" gives `INCW aa`/`DECW aa` as `N.....Z.` and `ADDW YA,aa`/`SUBW YA,aa` as
/// `NV..H.ZC`, all with `Word[aa]` as the result. A driver's block loop is
/// `- DECW $0A : BNE -`, so a `DECW` that sets no flags leaves Z carrying whatever the previous
/// instruction left and the loop runs until the 16-bit pointer itself wraps — the whole of APU RAM
/// instead of the block the driver asked for.
#[test]
fn decw_sets_the_flags_its_countdown_loop_tests() {
    // `MOV $0A,#$02 / - DECW $0A / BNE -`: two passes and out, not 65 536.
    let mut spc = Spc::new();
    spc.load(&[0x8F, 0x02, 0x0A, 0x1A, 0x0A, 0xD0, 0xFC]);
    spc.run(2); // MOV, first DECW
    assert_eq!(spc.ram[0x0A], 0x01, "DECW counts the word down");
    assert!(!spc.cpu.psw.z(), "the counter is not zero yet");
    spc.run(2); // BNE back to the DECW, then the DECW that reaches zero
    assert_eq!(spc.ram[0x0A], 0x00, "DECW reaches zero");
    assert!(
        spc.cpu.psw.z(),
        "DECW sets Z from the 16-bit result, which the loop's BNE reads"
    );
    assert!(!spc.cpu.psw.n(), "a zero word is not negative");
    spc.step(); // BNE: not taken, so the loop is over
    assert_eq!(spc.cpu.pc, 0x0207, "the loop ends on the counter");

    // N is bit 15 of the word: $8000 - 1 = $7FFF clears it, $0000 - 1 = $FFFF sets it.
    let mut spc = Spc::new();
    spc.write(0x0010, 0x00);
    spc.write(0x0011, 0x80);
    spc.load(&[0x1A, 0x10]);
    spc.step();
    assert_eq!(spc.read(0x0010), 0xFF, "low byte of $8000 - 1");
    assert_eq!(spc.read(0x0011), 0x7F, "high byte of $8000 - 1");
    assert!(
        !spc.cpu.psw.n(),
        "N is bit 15 of the result, not bit 7 of its low byte"
    );
    assert!(!spc.cpu.psw.z(), "a $7FFF result is not zero");

    let mut spc = Spc::new();
    spc.load(&[0x1A, 0x10]);
    spc.step();
    assert_eq!(spc.read(0x0010), 0xFF, "low byte of $0000 - 1");
    assert_eq!(spc.read(0x0011), 0xFF, "high byte of $0000 - 1");
    assert!(spc.cpu.psw.n(), "N set: bit 15 of $FFFF");
    assert!(!spc.cpu.psw.z(), "a $FFFF result is not zero");

    // INCW is the mirror: $7FFF + 1 = $8000 sets N, and $FFFF + 1 = $0000 sets Z.
    let mut spc = Spc::new();
    spc.write(0x0010, 0xFF);
    spc.write(0x0011, 0x7F);
    spc.load(&[0x3A, 0x10]);
    spc.step();
    assert_eq!(spc.read(0x0011), 0x80, "high byte of $7FFF + 1");
    assert!(spc.cpu.psw.n(), "N is bit 15 of the result");

    let mut spc = Spc::new();
    spc.write(0x0010, 0xFF);
    spc.write(0x0011, 0xFF);
    spc.load(&[0x3A, 0x10]);
    spc.step();
    assert_eq!(spc.read(0x0010), 0x00, "low byte of $FFFF + 1");
    assert_eq!(spc.read(0x0011), 0x00, "high byte of $FFFF + 1");
    assert!(spc.cpu.psw.z(), "Z is the whole word being zero");
}

/// `ADDW`/`SUBW` set N and Z from the 16-bit `YA` they leave, not from its low byte
/// (fullsnes's `NV..H.ZC` row). `YA=$00FF + $0001` is `$0100`: not negative, not zero.
#[test]
fn addw_sets_the_flags_from_the_word_it_leaves() {
    let mut spc = Spc::new();
    spc.cpu.a = 0xFF;
    spc.cpu.y = 0x00;
    spc.write(0x0010, 0x01);
    spc.write(0x0011, 0x00);
    spc.load(&[0x7A, 0x10]); // ADDW YA,$10
    spc.step();
    assert_eq!(spc.cpu.a, 0x00, "YA = $0100");
    assert_eq!(spc.cpu.y, 0x01, "YA = $0100");
    assert!(!spc.cpu.psw.n(), "N is bit 15 of YA");
    assert!(
        !spc.cpu.psw.z(),
        "Z is the whole word, so a $0100 result is not zero"
    );

    let mut spc = Spc::new();
    spc.cpu.a = 0x01;
    spc.cpu.y = 0x00;
    spc.write(0x0010, 0xFF);
    spc.write(0x0011, 0xFF);
    spc.load(&[0x7A, 0x10]);
    spc.step();
    assert_eq!(spc.cpu.a, 0x00, "YA = $0000");
    assert_eq!(spc.cpu.y, 0x00, "YA = $0000");
    assert!(spc.cpu.psw.z(), "Z set on a zero word");
}

/// The `$F1` bit-7 overlay is a *read* window: a store into `$FFC0-$FFFF` always reaches RAM.
///
/// fullsnes "00F1h - CONTROL - Timer, I/O and ROM Control (W)", bit 7: "ROM at FFC0h-FFFFh (0=RAM,
/// 1=ROM) (writes do always go to RAM)", and "SNES APU Memory and I/O Map": `FFC0h..FFFFh 64-byte
/// Boot ROM or RAM (selectable via Port 00F1h)`. The distinction matters for a driver that uses the
/// window as a TCALL vector table: it is a plain RAM write while `$F1` bit 7 is clear, which is the
/// state this driver puts the APU in (`MOV $F1,#$30` at `$1004`).
#[test]
fn a_write_to_the_window_always_lands_in_ram() {
    // fullsnes "00F1h - CONTROL - Timer, I/O and ROM Control (W)", bit 7: "ROM at FFC0h-FFFFh
    // (0=RAM, 1=ROM) (writes do always go to RAM)" — and its "SNES APU Memory and I/O Map",
    // `FFC0h..FFFFh 64-byte Boot ROM or RAM (selectable via Port 00F1h)`. So the overlay is a read
    // window only: a store into `$FFC0-$FFFF` is accepted whether the ROM is mapped or not, and what
    // changes with bit 7 is what a *read* returns.
    let mut spc = Spc::new();
    spc.cpu.control = 0xB0; // the power-on value, ROM enabled.

    // With the ROM mapped, the store is still taken by RAM...
    spc.write(0xFFC0, 0x5A);
    assert_eq!(
        spc.ram[0xFFC0], 0x5A,
        "a store to the window lands in RAM even while the boot ROM is mapped"
    );
    // ...but a read returns the ROM's byte, which is `MOV X,#$EF` at $FFC0.
    assert_eq!(
        spc.read(0xFFC0),
        0xCD,
        "with $F1 bit 7 set, $FFC0 reads the boot ROM, not the byte stored under it"
    );

    // A driver disables the ROM before it uses the window as a table (this driver was measured
    // doing `MOV $F1,#$30` at $1004): with bit 7 clear the same address reads the stored byte.
    spc.cpu.control = 0x30;
    assert_eq!(
        spc.read(0xFFC0),
        0x5A,
        "with $F1 bit 7 clear the window is RAM, so the store is visible"
    );
    // And the 16-bit read `TCALL` performs (`Word[$FFDE - n*2]`) sees the same RAM.
    spc.write(0xFFDE, 0x8F);
    spc.write(0xFFDF, 0x05);
    let vector = u16::from(spc.read(0xFFDE)) | (u16::from(spc.read(0xFFDF)) << 8);
    assert_eq!(
        vector, 0x058F,
        "TCALL 15's vector is read out of the RAM the driver copied its table into"
    );
}

/// `$FFC0-$FFFF` is the boot ROM's own 64 bytes while `$F1` bit 7 is set, and RAM otherwise.
///
/// fullsnes "SNES APU Memory and I/O Map": `FFC0h..FFFFh 64-byte Boot ROM or RAM (selectable via
/// Port 00F1h)`. Before this test passed, the window read as zeros, so `Word[$FFDE]` read `$0000`
/// where the ROM has `E4 F5` (`MOV A,$F5`) and a `BRK`/`TCALL 0` taken in the window would reach
/// `$0000` instead of `$F5E4`.
#[test]
fn the_boot_rom_window_reads_the_roms_own_bytes() {
    let mut spc = Spc::new();
    spc.cpu.control = 0xB0; // ROM enabled, every timer disabled (the power-on value).
    assert_eq!(spc.read(0xFFC0), 0xCD, "$FFC0 = MOV X,#$EF");
    assert_eq!(spc.read(0xFFC9), 0x8F, "$FFC9 = MOV $F4,#$AA, the greeting");
    assert_eq!(spc.read(0xFFDE), 0xE4, "$FFDE = MOV A,$F5's opcode");
    assert_eq!(spc.read(0xFFDF), 0xF5, "$FFDE = MOV A,$F5's operand");
    let vector = u16::from(spc.read(0xFFDE)) | (u16::from(spc.read(0xFFDF)) << 8);
    assert_eq!(
        vector, 0xF5E4,
        "Word[$FFDE] is the ROM's own bytes, not a zero vector"
    );
    assert_eq!(spc.read(0xFFFE), 0xC0, "the reset vector is $FFC0");
    assert_eq!(spc.read(0xFFFF), 0xFF, "the reset vector is $FFC0");

    // With `$F1` bit 7 clear the same addresses are plain RAM, and a write lands in it.
    spc.cpu.control = 0x00;
    spc.write(0xFFDE, 0xAA);
    spc.write(0xFFDF, 0xBB);
    assert_eq!(
        spc.read(0xFFC0),
        0x00,
        "the window is RAM when the ROM is off"
    );
    let ram_vector = u16::from(spc.read(0xFFDE)) | (u16::from(spc.read(0xFFDF)) << 8);
    assert_eq!(
        ram_vector, 0xBBAA,
        "RAM holds what was written to the window"
    );
}

#[test]
fn control_bits_4_and_5_reset_the_input_latches() {
    // fullsnes "SPC700 I/O Ports", `00F1h - CONTROL`: "4 Reset Port 00F4h/00F5h Input-Latches (0=No
    // change, 1=Reset to 00h)", "5 Reset Port 00F6h/00F7h Input-Latches (0=No change, 1=Reset to
    // 00h)", "Note: The CPUIO inputs are latched inside of the SPC700 (at time when the Main CPU
    // writes them), above two bits allow to reset these latches." A driver that has consumed the
    // S-CPU's byte clears the latch so the same byte is not read a second time as a second command;
    // a core that ignores the bits hands it the stale byte for ever.
    let mut spc = Spc::new();
    spc.to_apu = [0x11, 0x22, 0x33, 0x44];

    // Bit 4: ports 0 and 1 read back $00, ports 2 and 3 still carry the S-CPU's bytes.
    spc.write(0x00F1, 0x10);
    assert_eq!(
        spc.read(0x00F4),
        0x00,
        "bit 4 resets the $F4 input latch to 00h"
    );
    assert_eq!(
        spc.read(0x00F5),
        0x00,
        "bit 4 resets the $F5 input latch to 00h"
    );
    assert_eq!(spc.read(0x00F6), 0x33, "bit 4 leaves $F6 alone");
    assert_eq!(spc.read(0x00F7), 0x44, "bit 4 leaves $F7 alone");

    // Bit 5: the other pair, and the first two are untouched.
    spc.to_apu = [0x55, 0x66, 0x77, 0x88];
    spc.write(0x00F1, 0x20);
    assert_eq!(spc.read(0x00F4), 0x55, "bit 5 leaves $F4 alone");
    assert_eq!(spc.read(0x00F5), 0x66, "bit 5 leaves $F5 alone");
    assert_eq!(
        spc.read(0x00F6),
        0x00,
        "bit 5 resets the $F6 input latch to 00h"
    );
    assert_eq!(
        spc.read(0x00F7),
        0x00,
        "bit 5 resets the $F7 input latch to 00h"
    );

    // The reset is the SPC's *input* side only: what the S-CPU reads at $2140-$2143 is the other
    // bank, and a write with both bits clear changes nothing.
    spc.from_apu = [0xAA, 0xBB, 0xCC, 0xDD];
    spc.to_apu = [0x01, 0x02, 0x03, 0x04];
    spc.write(0x00F1, 0x30);
    assert_eq!(spc.to_apu, [0x00; 4], "bits 4 and 5 reset all four latches");
    assert_eq!(
        spc.from_apu,
        [0xAA, 0xBB, 0xCC, 0xDD],
        "the S-CPU's read side is not the latch being reset"
    );

    spc.to_apu = [0x09, 0x0A, 0x0B, 0x0C];
    spc.write(0x00F1, 0x01);
    assert_eq!(
        spc.to_apu,
        [0x09, 0x0A, 0x0B, 0x0C],
        "a write with bits 4 and 5 clear does not touch the latches"
    );
}

/// `PUSH A : MOV A,$48 : AND A,$1D : POP A : BNE` is how Nintendo's sound drivers gate a voice
/// write on a mute mask — SMW's driver does exactly this at APU `$068F`, one instruction before it
/// writes `$F2`/`$F3` — and the branch only tests the mask if `POP A` leaves the flags the `AND`
/// set. fullsnes "SNES APU SPC700 CPU Load/Store Commands":
///
/// ```text
/// PUSH A   PUSH A   2D  4  [SP]=A, SP=SP-1        ........
/// POP A    POP A    AE  4  SP=SP+1, A=[SP]        ........
/// POP X    POP X    CE  4  SP=SP+1, X=[SP]        ........
/// POP Y    POP Y    EE  4  SP=SP+1, Y=[SP]        ........
/// POP PSW  POP PSW  8E  4  SP=SP+1, Flags=[SP]    NVPBHIZC
/// ```
///
/// The flag column is empty for the three register pops: only `POP PSW` restores flags.
#[test]
fn pop_does_not_touch_the_flags_and_only_pop_psw_restores_them() {
    // The driver's idiom: the mask decides the branch, and A comes back with the byte it pushed.
    let mut spc = Spc::new();
    spc.cpu.a = 0x24;
    spc.load(&[
        0x2D, // PUSH A
        0xE8, 0x01, // MOV A,#$01
        0x28, 0x00, // AND A,#$00  -> Z=1, N=0
        0xAE, // POP A
    ]);
    spc.run(4);
    assert_eq!(spc.cpu.a, 0x24, "POP A restores the byte that was pushed");
    assert!(
        spc.cpu.psw.z(),
        "the Z set by `AND A,#$00` survives `POP A` (it is what the driver's BNE reads)"
    );
    assert!(!spc.cpu.psw.n(), "and N stays as the AND left it");

    // The other side of the same branch: a non-zero mask leaves Z clear, and POP A must not set it
    // from the value it pops.
    let mut spc = Spc::new();
    spc.cpu.a = 0x00;
    spc.load(&[
        0x2D, // PUSH A
        0xE8, 0x01, // MOV A,#$01
        0x28, 0x01, // AND A,#$01  -> Z=0
        0xAE, // POP A
    ]);
    spc.run(4);
    assert_eq!(spc.cpu.a, 0x00, "the popped byte is $00");
    assert!(
        !spc.cpu.psw.z(),
        "`POP A` of $00 must not turn the AND's clear Z into a set one"
    );

    // POP X and POP Y: flags untouched, register restored.
    let mut spc = Spc::new();
    spc.cpu.x = 0x7F;
    spc.load(&[
        0x4D, // PUSH X
        0xCD, 0x00, // MOV X,#$00 -> Z=1
        0xCE, // POP X
    ]);
    spc.run(3);
    assert_eq!(spc.cpu.x, 0x7F, "POP X restores X");
    assert!(spc.cpu.psw.z(), "POP X leaves Z alone");

    let mut spc = Spc::new();
    spc.cpu.y = 0x01;
    spc.load(&[
        0x6D, // PUSH Y
        0x8D, 0xFF, // MOV Y,#$FF -> N=1
        0xEE, // POP Y
    ]);
    spc.run(3);
    assert_eq!(spc.cpu.y, 0x01, "POP Y restores Y");
    assert!(spc.cpu.psw.n(), "POP Y leaves N alone");

    // POP PSW is the one that does restore flags, so the tests above cannot be satisfied by
    // leaving every pop alone.
    let mut spc = Spc::new();
    spc.cpu.psw = Psw(Psw::N | Psw::V | Psw::C);
    spc.load(&[0x0D, 0x8E]); // PUSH PSW ; POP PSW
    spc.run(1); // PUSH PSW of $83
    spc.cpu.psw = Psw(0);
    spc.cpu.pc = 0x0201;
    spc.run(1); // POP PSW
    assert_eq!(
        spc.cpu.psw.0,
        Psw::N | Psw::V | Psw::C,
        "POP PSW restores every flag byte it pushed"
    );
}

/// #9 item 1: `SET1` and `CLR1` are eight opcodes each, and the decode mask has to be five bits wide.
///
/// fullsnes "SNES APU SPC700 CPU ALU Commands", 1bit ALU Operations:
///
/// ```text
/// CLR1 aa.b      CLR  [aa].b     b*20+12 aa  4  [aa].bit_b=0           ........
/// SET1 aa.b      SET  [aa].b     b*20+02 aa  4  [aa].bit_b=1           ........
/// ```
///
/// so `SET1` is `$02, $22 … $E2` and `CLR1` is `$12, $32 … $F2`: the two families differ in bit 4 and
/// share bits 0-4, which is what `opcode & 0x1F` isolates. The decode arm matched on
/// `opcode & 0x0F == 0x02` instead, and every `CLR1` opcode ends in `2`, so the `CLR1` arm after it was
/// unreachable and `CLR1 dp.n` **set** bit n. The cycle-count table could not see it: both take 4.
#[test]
fn set1_and_clr1_set_and_clear_only_their_own_bit() {
    for bit in 0..8u8 {
        let set1 = 0x02 + bit * 0x20;
        let clr1 = 0x12 + bit * 0x20;
        let mask = 1u8 << bit;

        // SET1 of a clear byte sets exactly its bit.
        let mut spc = Spc::new();
        spc.ram[0x0010] = 0x00;
        spc.load(&[set1, 0x10]);
        assert_eq!(
            spc.step(),
            4,
            "SET1 (${set1:02X}) takes 4 clocks in fullsnes's Clk column"
        );
        assert_eq!(
            spc.cpu.pc, 0x0202,
            "SET1 is two bytes: the opcode and one direct-page byte"
        );
        assert_eq!(
            spc.ram[0x0010], mask,
            "SET1 ${set1:02X} sets bit {bit} of a $00 byte"
        );

        // CLR1 of a set byte clears exactly its bit.
        let mut spc = Spc::new();
        spc.ram[0x0010] = 0xFF;
        spc.load(&[clr1, 0x10]);
        assert_eq!(
            spc.step(),
            4,
            "CLR1 (${clr1:02X}) takes 4 clocks in fullsnes's Clk column"
        );
        assert_eq!(
            spc.cpu.pc, 0x0202,
            "CLR1 is two bytes: the opcode and one direct-page byte"
        );
        assert_eq!(
            spc.ram[0x0010], !mask,
            "CLR1 ${clr1:02X} clears bit {bit} of a $FF byte and leaves the other seven set"
        );

        // And the other fill value for each: neither disturbs a neighbouring bit.
        let mut spc = Spc::new();
        spc.ram[0x0010] = 0xFF;
        spc.load(&[set1, 0x10]);
        spc.step();
        assert_eq!(
            spc.ram[0x0010], 0xFF,
            "SET1 ${set1:02X} of a bit that is already set changes nothing"
        );

        let mut spc = Spc::new();
        spc.ram[0x0010] = 0x00;
        spc.load(&[clr1, 0x10]);
        spc.step();
        assert_eq!(
            spc.ram[0x0010], 0x00,
            "CLR1 ${clr1:02X} of a bit that is already clear changes nothing"
        );
    }
}

/// The single operand of the 1-bit family (fullsnes "Addressing Modes"):
///
/// ```text
/// aaa.b          [aaa].b         ;-Bit0..7 of address [0000..1FFF] (13bit addr)
/// ```
///
/// Bits 0-12 are the address and bits 13-15 the bit number, packed into one 16-bit little-endian
/// operand. That is why the ALU table's byte column reads `AA aa ba` — three bytes, not four — and
/// why the decode has no separate bit byte to fetch.
fn aaa_b(addr: u16, bit: u8) -> [u8; 2] {
    let word = (addr & 0x1FFF) | (u16::from(bit) << 13);
    [word as u8, (word >> 8) as u8]
}

/// #9 item 2: `MOV1 C,aaa.b` — fullsnes "1bit ALU Operations":
/// `MOV1 C,aaa.b  MOV  C,[aaa].b  AA aa ba  4  C=[aaa].bit_b  .......C`.
///
/// Three bytes, so PC advances by three and the bit number comes from the operand's bits 13-15.
#[test]
fn mov1_c_aaa_b_loads_c_from_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x1234, 5);
    assert_eq!(
        [lo, hi],
        [0x34, 0xB2],
        "the operand for $1234.5 is $1234 | (5 << 13) = $B234, little-endian"
    );

    let mut spc = Spc::new();
    spc.write(0x1234, 0x20);
    spc.load(&[0xAA, lo, hi]);
    assert_eq!(
        spc.step(),
        4,
        "MOV1 C,aaa.b takes 4 clocks in fullsnes's Clk column"
    );
    assert_eq!(
        spc.cpu.pc, 0x0203,
        "MOV1 C,aaa.b is three bytes: the opcode and one 16-bit operand"
    );
    assert!(spc.cpu.psw.c(), "C = [$1234] bit 5 = 1");

    let mut spc = Spc::new();
    spc.write(0x1234, 0xDF);
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0xAA, lo, hi]);
    spc.step();
    assert!(!spc.cpu.psw.c(), "C = [$1234] bit 5 = 0");
}

/// #9 item 2: `MOV1 aaa.b,C` — fullsnes: `MOV1 aaa.b,C  MOV  [aaa].b,C  CA aa ba  6  [aaa].bit_b=C`.
#[test]
fn mov1_aaa_b_c_stores_c_into_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x1ABC, 7);

    let mut spc = Spc::new();
    spc.write(0x1ABC, 0x00);
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0xCA, lo, hi]);
    assert_eq!(
        spc.step(),
        6,
        "MOV1 aaa.b,C takes 6 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "MOV1 aaa.b,C is three bytes");
    assert_eq!(
        spc.read(0x1ABC),
        0x80,
        "bit 7 of $1ABC takes C = 1 and the other bits stay clear"
    );

    let mut spc = Spc::new();
    spc.write(0x1ABC, 0xFF);
    spc.load(&[0xCA, lo, hi]); // PSW = 0, so C = 0
    spc.step();
    assert_eq!(
        spc.read(0x1ABC),
        0x7F,
        "bit 7 of $1ABC is cleared from C = 0 and the other bits stay set"
    );
}

/// #9 item 2: `NOT1 aaa.b` — fullsnes: `NOT1 aaa.b  NOT  [aaa].b  EA aa ba  5  invert [aaa].bit_b`.
#[test]
fn not1_aaa_b_inverts_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x0010, 3);

    let mut spc = Spc::new();
    spc.write(0x0010, 0xF7);
    spc.load(&[0xEA, lo, hi]);
    assert_eq!(
        spc.step(),
        5,
        "NOT1 takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "NOT1 is three bytes");
    assert_eq!(
        spc.read(0x0010),
        0xFF,
        "bit 3 of $F7 is inverted from 0 to 1"
    );

    let mut spc = Spc::new();
    spc.write(0x0010, 0xFF);
    spc.load(&[0xEA, lo, hi]);
    spc.step();
    assert_eq!(
        spc.read(0x0010),
        0xF7,
        "bit 3 of $FF is inverted from 1 to 0"
    );
}

/// #9 item 2: `OR1 C,aaa.b` — fullsnes: `OR1  C,aaa.b  OR   C,[aaa].b  0A aa ba  5  C=C OR [aaa].bit_b`.
#[test]
fn or1_c_aaa_b_sets_c_from_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x0500, 2);

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04);
    spc.load(&[0x0A, lo, hi]); // C = 0
    assert_eq!(
        spc.step(),
        5,
        "OR1 C,aaa.b takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "OR1 C,aaa.b is three bytes");
    assert!(spc.cpu.psw.c(), "C = 0 OR [$0500] bit 2 = 1");

    let mut spc = Spc::new();
    spc.write(0x0500, 0xFB);
    spc.load(&[0x0A, lo, hi]);
    spc.step();
    assert!(
        !spc.cpu.psw.c(),
        "C = 0 OR [$0500] bit 2 = 0 leaves C clear"
    );
}

/// #9 item 2: `OR1 C,/aaa.b` — fullsnes: `OR1  C,/aaa.b  OR   C,not[].b  2A aa ba  5  C=C OR not[aaa].bit_b`.
#[test]
fn or1_c_not_aaa_b_sets_c_from_the_inverted_bit() {
    let [lo, hi] = aaa_b(0x0500, 2);

    let mut spc = Spc::new();
    spc.write(0x0500, 0xFB); // bit 2 clear, so its inverse is 1
    spc.load(&[0x2A, lo, hi]); // C = 0
    assert_eq!(
        spc.step(),
        5,
        "OR1 C,/aaa.b takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "OR1 C,/aaa.b is three bytes");
    assert!(spc.cpu.psw.c(), "C = 0 OR NOT([$0500] bit 2) = 1");

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04); // bit 2 set, so its inverse is 0
    spc.load(&[0x2A, lo, hi]);
    spc.step();
    assert!(!spc.cpu.psw.c(), "C = 0 OR NOT([$0500] bit 2) = 0");
}

/// #9 item 2: `AND1 C,aaa.b` — fullsnes: `AND1 C,aaa.b  AND  C,[aaa].b  4A aa ba  4  C=C AND [aaa].bit_b`.
#[test]
fn and1_c_aaa_b_ands_c_with_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x0500, 2);

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04);
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0x4A, lo, hi]);
    assert_eq!(
        spc.step(),
        4,
        "AND1 C,aaa.b takes 4 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "AND1 C,aaa.b is three bytes");
    assert!(spc.cpu.psw.c(), "C = 1 AND [$0500] bit 2 = 1");

    let mut spc = Spc::new();
    spc.write(0x0500, 0xFB);
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0x4A, lo, hi]);
    spc.step();
    assert!(!spc.cpu.psw.c(), "C = 1 AND [$0500] bit 2 = 0 clears C");
}

/// #9 item 2: `AND1 C,/aaa.b` — fullsnes: `AND1 C,/aaa.b  AND  C,not[].b  6A aa ba  4  C=C AND not[aaa].bit_b`.
#[test]
fn and1_c_not_aaa_b_ands_c_with_the_inverted_bit() {
    let [lo, hi] = aaa_b(0x0500, 2);

    let mut spc = Spc::new();
    spc.write(0x0500, 0xFB); // bit 2 clear, so its inverse is 1
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0x6A, lo, hi]);
    assert_eq!(
        spc.step(),
        4,
        "AND1 C,/aaa.b takes 4 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "AND1 C,/aaa.b is three bytes");
    assert!(spc.cpu.psw.c(), "C = 1 AND NOT([$0500] bit 2) = 1");

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04); // bit 2 set, so its inverse is 0
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0x6A, lo, hi]);
    spc.step();
    assert!(!spc.cpu.psw.c(), "C = 1 AND NOT([$0500] bit 2) = 0");
}

/// #9 item 2: `EOR1 C,aaa.b` — fullsnes: `EOR1 C,aaa.b  XOR  C,[aaa].b  8A aa ba  5  C=C XOR [aaa].bit_b`.
#[test]
fn eor1_c_aaa_b_xors_the_addressed_bit() {
    let [lo, hi] = aaa_b(0x0500, 2);

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04);
    spc.load(&[0x8A, lo, hi]); // C = 0
    assert_eq!(
        spc.step(),
        5,
        "EOR1 C,aaa.b takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.pc, 0x0203, "EOR1 C,aaa.b is three bytes");
    assert!(spc.cpu.psw.c(), "C = 0 XOR [$0500] bit 2 = 1");

    let mut spc = Spc::new();
    spc.write(0x0500, 0x04);
    spc.cpu.psw = Psw(Psw::C);
    spc.load(&[0x8A, lo, hi]);
    spc.step();
    assert!(!spc.cpu.psw.c(), "C = 1 XOR [$0500] bit 2 = 1 clears C");
}

/// #9 item 3a: a direct-page index wraps inside the page.
///
/// fullsnes "SNES APU SPC700 CPU Overview", Addressing Modes:
///
/// ```text
///   aa+X           [aa+X]         ; (or at [0100..01FF] when flag P=1)
///   aa+Y           [aa+Y]         ; (aa+X and aa+Y wrap within that area,
///   (X)            [X]            ; ie. addr = (aa+X) AND 0FFh)
/// ```
///
/// Every `dp+X`/`dp+Y`/`[dp+X]` arm used `fetch_dp(..).wrapping_add(x)`, which adds **after** the
/// direct-page byte has become an address, so `MOV A,$F0+X` with X=$20 read $0110 instead of $0010.
/// Each block below puts a different byte at the address the bug would have used, so the assertion
/// cannot pass by accident.
#[test]
fn direct_page_indices_wrap_within_the_page() {
    // MOV A,dp+X (F4)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x99;
    spc.ram[0x0110] = 0x11;
    spc.cpu.x = 0x20;
    spc.load(&[0xF4, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.a, 0x99,
        "MOV A,$F0+X with X=$20 reads $0010, not $0110"
    );

    // MOV A,[dp+X] (E7)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x00;
    spc.ram[0x0011] = 0x05;
    spc.ram[0x0110] = 0x00;
    spc.ram[0x0111] = 0x06;
    spc.ram[0x0500] = 0x42;
    spc.ram[0x0600] = 0x43;
    spc.cpu.x = 0x20;
    spc.load(&[0xE7, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.a, 0x42,
        "MOV A,[$F0+X] with X=$20 takes its pointer from $0010, not $0110"
    );

    // MOV X,dp+Y (F9)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x77;
    spc.ram[0x0110] = 0x11;
    spc.cpu.y = 0x20;
    spc.load(&[0xF9, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.x, 0x77,
        "MOV X,$F0+Y with Y=$20 reads $0010, not $0110"
    );

    // MOV Y,dp+X (FB)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x66;
    spc.ram[0x0110] = 0x11;
    spc.cpu.x = 0x20;
    spc.load(&[0xFB, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.y, 0x66,
        "MOV Y,$F0+X with X=$20 reads $0010, not $0110"
    );

    // MOV dp+X,A (D4)
    let mut spc = Spc::new();
    spc.cpu.a = 0x5A;
    spc.cpu.x = 0x20;
    spc.load(&[0xD4, 0xF0]);
    spc.step();
    assert_eq!(
        spc.ram[0x0010], 0x5A,
        "MOV $F0+X,A with X=$20 stores at $0010"
    );
    assert_eq!(spc.ram[0x0110], 0x00, "and leaves $0110 alone");

    // MOV dp+X,Y (DB)
    let mut spc = Spc::new();
    spc.cpu.y = 0x5B;
    spc.cpu.x = 0x20;
    spc.load(&[0xDB, 0xF0]);
    spc.step();
    assert_eq!(
        spc.ram[0x0010], 0x5B,
        "MOV $F0+X,Y with X=$20 stores at $0010"
    );
    assert_eq!(spc.ram[0x0110], 0x00, "and leaves $0110 alone");

    // MOV dp+Y,X (D9)
    let mut spc = Spc::new();
    spc.cpu.x = 0x5C;
    spc.cpu.y = 0x20;
    spc.load(&[0xD9, 0xF0]);
    spc.step();
    assert_eq!(
        spc.ram[0x0010], 0x5C,
        "MOV $F0+Y,X with Y=$20 stores at $0010"
    );
    assert_eq!(spc.ram[0x0110], 0x00, "and leaves $0110 alone");

    // MOV [dp+X],A (C7)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x00;
    spc.ram[0x0011] = 0x05;
    spc.ram[0x0110] = 0x00;
    spc.ram[0x0111] = 0x06;
    spc.cpu.a = 0x5D;
    spc.cpu.x = 0x20;
    spc.load(&[0xC7, 0xF0]);
    spc.step();
    assert_eq!(
        spc.ram[0x0500], 0x5D,
        "MOV [$F0+X],A with X=$20 writes through the pointer at $0010"
    );
    assert_eq!(spc.ram[0x0600], 0x00, "and not through the one at $0110");

    // ASL dp+X (1B)
    let mut spc = Spc::new();
    spc.ram[0x0010] = 0x01;
    spc.ram[0x0110] = 0x40;
    spc.cpu.x = 0x20;
    spc.load(&[0x1B, 0xF0]);
    spc.step();
    assert_eq!(
        spc.ram[0x0010], 0x02,
        "ASL $F0+X with X=$20 shifts the byte at $0010"
    );
    assert_eq!(spc.ram[0x0110], 0x40, "and leaves $0110 alone");

    // CBNE dp+X,rel (DE): the branch is taken only if the byte at $0010 differs.
    let mut spc = Spc::new();
    spc.cpu.a = 0x01;
    spc.ram[0x0010] = 0x02;
    spc.ram[0x0110] = 0x01;
    spc.cpu.x = 0x20;
    spc.load(&[0xDE, 0xF0, 0x02]);
    spc.step();
    assert_eq!(
        spc.cpu.pc, 0x0205,
        "CBNE $F0+X compares $0010, finds $02 <> A and branches"
    );

    // OR A,dp+X (14)
    let mut spc = Spc::new();
    spc.cpu.a = 0x01;
    spc.ram[0x0010] = 0x02;
    spc.ram[0x0110] = 0x80;
    spc.cpu.x = 0x20;
    spc.load(&[0x14, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.a, 0x03,
        "OR A,$F0+X with X=$20 reads $0010, not $0110"
    );

    // OR A,[dp+X] (07)
    let mut spc = Spc::new();
    spc.cpu.a = 0x01;
    spc.ram[0x0010] = 0x00;
    spc.ram[0x0011] = 0x05;
    spc.ram[0x0110] = 0x00;
    spc.ram[0x0111] = 0x06;
    spc.ram[0x0500] = 0x10;
    spc.ram[0x0600] = 0x40;
    spc.cpu.x = 0x20;
    spc.load(&[0x07, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.a, 0x11,
        "OR A,[$F0+X] with X=$20 takes its pointer from $0010, not $0110"
    );

    // With PSW.P set the wrap stays inside page one: ($F0 + $20) & $FF = $10 -> $0110.
    let mut spc = Spc::new();
    spc.cpu.psw = Psw(Psw::P);
    spc.ram[0x0010] = 0x11;
    spc.ram[0x0110] = 0x99;
    spc.ram[0x0210] = 0x22;
    spc.cpu.x = 0x20;
    spc.load(&[0xF4, 0xF0]);
    spc.step();
    assert_eq!(
        spc.cpu.a, 0x99,
        "with P=1 the index wraps inside $0100-$01FF, so $F0+X is $0110, not $0210"
    );
}

/// #9 item 3b: `TSET1`/`TCLR1` set N and Z and leave C alone.
///
/// fullsnes "Special ALU Operations":
/// `TSET1 !aaaa  TSET [aaaa],A  0E aa aa  6  [aaaa]=[aaaa]OR A  N.....Z.` and
/// `TCLR1 !aaaa  TCLR [aaaa],A  4E aa aa  6  [aaaa]=[aaaa]AND NOT A  N.....Z.`, with
/// `For TCLR/TSET, Z+N flags are set as for "CMP A,[aaaa]" (before [aaaa] gets changed).`
///
/// The flag column is `N.....Z.`: C is not part of it. The code called `cmp_a`, which sets C from the
/// same subtraction, so a driver's carry survived (or was destroyed) by a test-and-set it did not ask
/// for.
#[test]
fn tset1_and_tclr1_leave_c_alone() {
    // C set before TSET1, and the equality test's borrow would clear it.
    let mut spc = Spc::new();
    spc.cpu.a = 0x0F;
    spc.cpu.psw = Psw(Psw::C);
    spc.write(0x0010, 0x20);
    spc.load(&[0x0E, 0x10, 0x00]);
    assert_eq!(
        spc.step(),
        6,
        "TSET1 takes 6 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.read(0x0010), 0x2F, "TSET1 ORs A into [abs]");
    assert!(
        spc.cpu.psw.c(),
        "TSET1 must not clear C: its flag column is N.....Z."
    );
    assert!(
        spc.cpu.psw.n(),
        "N from A - [abs] = $0F - $20 = $EF, negative"
    );
    assert!(!spc.cpu.psw.z(), "A - [abs] is not zero");

    // C clear before TCLR1, and the equality test would set it.
    let mut spc = Spc::new();
    spc.cpu.a = 0x0F;
    spc.write(0x0010, 0x0F);
    spc.load(&[0x4E, 0x10, 0x00]);
    assert_eq!(
        spc.step(),
        6,
        "TCLR1 takes 6 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.read(0x0010), 0x00, "TCLR1 ANDs NOT A into [abs]");
    assert!(
        !spc.cpu.psw.c(),
        "TCLR1 must not set C: its flag column is N.....Z."
    );
    assert!(spc.cpu.psw.z(), "Z from A - [abs] = 0");
}

/// #9 item 3c: `MOVW YA,dp` takes N and Z from the 16-bit word it loads.
///
/// fullsnes "16bit ALU Operations": `MOVW YA,aa  MOVW YA,[aa]  BA aa  5  ...  N.....Z.`; the wiki's
/// row is `MOVW YA, dp | BA | 2 | 5 | N.....Z.`. N is bit 15 of the word and Z is the whole word
/// being zero. The code passed only Y to `nz`, so a word like `$0034` left Z **set** and a
/// `MOVW YA,$10 : BEQ` took the branch.
#[test]
fn movw_ya_dp_takes_its_flags_from_the_word() {
    let mut spc = Spc::new();
    spc.write(0x0010, 0x34);
    spc.write(0x0011, 0x00);
    spc.load(&[0xBA, 0x10]);
    assert_eq!(
        spc.step(),
        5,
        "MOVW YA,dp takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.a, 0x34, "the low byte goes to A");
    assert_eq!(spc.cpu.y, 0x00, "the high byte goes to Y");
    assert!(
        !spc.cpu.psw.z(),
        "the word $0034 is not zero, so Z is clear even though Y is"
    );
    assert!(!spc.cpu.psw.n(), "bit 15 of $0034 is clear");

    let mut spc = Spc::new();
    spc.write(0x0010, 0x00);
    spc.write(0x0011, 0x80);
    spc.load(&[0xBA, 0x10]);
    spc.step();
    assert!(spc.cpu.psw.n(), "N is bit 15 of the word, set by $8000");
    assert!(!spc.cpu.psw.z(), "and a $8000 word is not zero");

    let mut spc = Spc::new();
    spc.load(&[0xBA, 0x10]);
    spc.step();
    assert!(spc.cpu.psw.z(), "a $0000 word sets Z");
    assert!(!spc.cpu.psw.n(), "and clears N");
}

/// #9 item 3d: `SBC`/`SUBW` report H in the documented sense — a carry out, not a half-borrow.
///
/// fullsnes "Flags (PSW)": `Bit3  H  Half-carry  (0=Borrow, or no-carry, 1=Carry, or no-borrow)`, and
/// under "16bit ALU Operations": `For ADDW/SUBW, H is carry from bit11 to bit12.` The two subtraction
/// paths set H on a borrow out of bit 3 / bit 11 — that row inverted — so a driver that reads H, and
/// `DAS`, which adjusts on it, saw the opposite bit.
///
/// `DAS` reads H, and its half-borrow test is flipped together with this one, so a `SBC : DAS`
/// sequence lands on exactly the byte it landed on before this change.
#[test]
fn sbc_and_subw_report_h_as_a_carry_out() {
    // $45 - $12: the low nibble does not borrow, so H is set ("no-borrow").
    let mut spc = Spc::new();
    spc.cpu.a = 0x45;
    spc.cpu.psw = Psw(Psw::C);
    spc.write(0x0010, 0x12);
    spc.load(&[0xA4, 0x10]);
    assert_eq!(
        spc.step(),
        3,
        "SBC A,dp takes 3 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.a, 0x33, "$45 - $12 = $33");
    assert!(
        spc.cpu.psw.h(),
        "$45 - $12 does not borrow out of bit 3, so H is set"
    );

    // $42 - $15: the low nibble borrows, so H is clear ("Borrow").
    let mut spc = Spc::new();
    spc.cpu.a = 0x42;
    spc.cpu.psw = Psw(Psw::C);
    spc.write(0x0010, 0x15);
    spc.load(&[0xA4, 0x10]);
    spc.step();
    assert_eq!(spc.cpu.a, 0x2D, "$42 - $15 = $2D");
    assert!(
        !spc.cpu.psw.h(),
        "$42 - $15 borrows out of bit 3, so H is clear"
    );

    // SUBW: H is the carry from bit 11 to bit 12.
    let mut spc = Spc::new();
    spc.cpu.a = 0x34;
    spc.cpu.y = 0x12;
    spc.write(0x0010, 0x11);
    spc.write(0x0011, 0x00);
    spc.load(&[0x9A, 0x10]);
    assert_eq!(
        spc.step(),
        5,
        "SUBW YA,dp takes 5 clocks in fullsnes's Clk column"
    );
    assert_eq!(spc.cpu.a, 0x23, "$1234 - $0011 = $1223");
    assert_eq!(spc.cpu.y, 0x12, "$1234 - $0011 = $1223");
    assert!(
        spc.cpu.psw.h(),
        "$234 - $011 does not borrow out of bit 11, so H is set"
    );

    // The borrow side of SUBW: $1200 - $0301 borrows out of bit 11 ($200 < $301).
    let mut spc = Spc::new();
    spc.cpu.a = 0x00;
    spc.cpu.y = 0x12;
    spc.write(0x0010, 0x01);
    spc.write(0x0011, 0x03);
    spc.load(&[0x9A, 0x10]);
    spc.step();
    assert_eq!(spc.cpu.a, 0xFF, "$1200 - $0301 = $0EFF");
    assert_eq!(spc.cpu.y, 0x0E, "$1200 - $0301 = $0EFF");
    assert!(
        !spc.cpu.psw.h(),
        "$200 - $301 borrows out of bit 11, so H is clear"
    );
}

/// #9 item 3e: `DAS` corrects a *borrowed* tens digit, and reports that borrow as C clear.
///
/// fullsnes "Flags (PSW)": `Bit0  C  Carry Flag  (0=Borrow, or no-carry, 1=Carry, or no-borrow)` and
/// `Bit3  H  Half-carry  (0=Borrow, or no-carry, 1=Carry, or no-borrow)`. "Special ALU Operations"
/// gives `DAS A  DAS A  BE  3  BCD adjust after SBC  N.....ZC`: C is in `DAS`'s flag column, and the
/// carry it reports is the borrow out of the tens digit. `DAS` adjusts *after a subtraction*, so
/// that borrow is C **clear** — the opposite test from `DAA`, which adds.
///
/// Every row below runs a real `SBC A,#nn` first, so A, H and C are what the subtraction leaves
/// rather than hand-set flags, and the expected byte is the packed-BCD answer. Both the digits and
/// the carry out are asserted: a `DAS` that lands `$27` but claims a carry has not fixed anything.
#[test]
fn das_corrects_a_borrowed_tens_digit() {
    // (minuend, subtrahend, SBC carry in, BCD answer, carry out of the BCD subtraction)
    // A carry out of 1 means no borrow: `$42 - $15` needs none, `$00 - $01` borrows.
    const ROWS: [(u8, u8, bool, u8, bool); 9] = [
        (0x42, 0x15, true, 0x27, true),
        (0x50, 0x25, true, 0x25, true),
        (0x10, 0x01, true, 0x09, true),
        (0x99, 0x01, true, 0x98, true),
        (0x42, 0x15, false, 0x26, true),
        (0x49, 0x15, true, 0x34, true),
        (0x00, 0x01, true, 0x99, false),
        // Neither branch taken: no `$60` and no `$06` correction is owed.
        (0x55, 0x11, true, 0x44, true),
        (0x20, 0x10, true, 0x10, true),
    ];
    for (lhs, rhs, carry_in, bcd, c_out) in ROWS {
        let mut spc = Spc::new();
        spc.cpu.a = lhs;
        spc.cpu.psw = Psw(if carry_in { Psw::C } else { 0 });
        // SBC A,#nn, then DAS A.
        spc.load(&[0xA8, rhs, 0xBE]);
        assert_eq!(
            spc.run(2),
            5,
            "SBC A,#nn is 2 clocks and DAS A is 3 in fullsnes's Clk column"
        );
        assert_eq!(
            spc.cpu.a, bcd,
            "BCD ${lhs:02X} - ${rhs:02X} with C in {carry_in} is ${bcd:02X}"
        );
        assert_eq!(
            spc.cpu.psw.c(),
            c_out,
            "BCD ${lhs:02X} - ${rhs:02X} with C in {carry_in} leaves C = {c_out} \
             (1 = no borrow out of the tens digit, 0 = borrow)"
        );
    }

    // Why C is written only inside the tens-digit branch, and not cleared when no correction is
    // made: entering that branch *is* the borrow — the instruction is subtracting `$60` out of the
    // tens digit — so the branch has a borrow to report and clears C. When it does not run, nothing
    // was borrowed out of the tens digit, and for a byte subtraction the borrow out of bit 7 (what
    // `SBC` left in C) is already the borrow out of the tens digit; writing C there could only
    // overwrite a correct flag with a second opinion. The last two rows are that case, and they
    // fail if the flag is written outside the branch.
}

/// #9 item 3f: `DAA` reports the decimal carry *out* — the mirror of `DAS`, and the regression cover
/// for leaving `daa()` alone.
///
/// fullsnes "Special ALU Operations": `DAA A  DAA A  DF  3  BCD adjust after ADC  N.....ZC`. `DAA`
/// adds, so its tens-digit branch runs on a carry (C set, or A above `$99`) and sets C. A `SBC`-style
/// test there would be as wrong as the `DAS` one was, so these three sums pin the current behaviour.
#[test]
fn daa_reports_the_decimal_carry_out() {
    // (augend, addend, BCD answer, carry out of the BCD addition)
    const ROWS: [(u8, u8, u8, bool); 3] = [
        (0x09, 0x01, 0x10, false),
        (0x45, 0x38, 0x83, false),
        (0x99, 0x01, 0x00, true),
    ];
    for (lhs, rhs, bcd, c_out) in ROWS {
        let mut spc = Spc::new();
        spc.cpu.a = lhs;
        spc.cpu.psw = Psw(0);
        // ADC A,#nn, then DAA A.
        spc.load(&[0x88, rhs, 0xDF]);
        assert_eq!(
            spc.run(2),
            5,
            "ADC A,#nn is 2 clocks and DAA A is 3 in fullsnes's Clk column"
        );
        assert_eq!(spc.cpu.a, bcd, "BCD ${lhs:02X} + ${rhs:02X} is ${bcd:02X}");
        assert_eq!(
            spc.cpu.psw.c(),
            c_out,
            "BCD ${lhs:02X} + ${rhs:02X} leaves C = {c_out} (1 = carry out of the tens digit)"
        );
    }
}
