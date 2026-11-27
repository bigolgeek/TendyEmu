//! The 65816: the opcode table against its source, the ALU and its flags, decimal arithmetic,
//! block moves, the width and mode switches, the stack and the addressing penalties
//! (fullsnes "CPU Clock Cycles", "CPU Opcode Overview", "CPU Registers
//! and Flags", "CPU Arithmetic/Logical Operations", "CPU Memory Addressing").
//!
//! Every clock figure here is master clocks measured off `Bus::master`, and each test says which
//! access count and internal-cycle count it is made of: an access in WRAM (`$7E` or the `$00:0000`
//! mirror) costs 8 master clocks and an internal cycle 6 (§3.1, §3.2). The CPU-cycle counts in the
//! source are therefore `cost = 8 * accesses + 6 * internals`, and a test that asserts a source
//! figure prints the arithmetic in its failure message.

mod common;

use tendy_snes::Machine;
use tendy_snes::addr::Addr;
use tendy_snes::cart::Cartridge;
use tendy_snes::cpu::Flags;
use tendy_snes::cpu::opcodes::{base_cycles, table_entry};

/// A machine whose 65816 runs `code` from `$7E:0000`: every fetch and data access is WRAM at 8
/// master clocks, so a step's cost is a clean multiple of the region cost.
fn machine_with(code: &[u8]) -> Machine {
    machine_with_rom(code, &[])
}

/// The same, with `patches` written into the ROM image first as `(rom offset, bytes)`.
fn machine_with_rom(code: &[u8], patches: &[(usize, &[u8])]) -> Machine {
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    for (offset, bytes) in patches {
        rom[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    for (i, byte) in code.iter().enumerate() {
        machine.bus.poke(Addr::new(0x7E, i as u16), *byte);
    }
    machine.cpu.pb = 0x7E;
    machine.cpu.pc = 0x0000;
    machine
}

/// 8-bit accumulator and index registers, emulation mode: the boot state.
fn eight_bit(machine: &mut Machine) {
    machine.cpu.e = true;
    machine.cpu.p = Flags(Flags::M | Flags::X | Flags::I);
    machine.cpu.s = 0x01FF;
}

/// Native mode with 16-bit registers: `M` and `X` clear.
fn sixteen_bit(machine: &mut Machine) {
    machine.cpu.e = false;
    machine.cpu.p = Flags(Flags::I);
}

fn step(machine: &mut Machine) -> u32 {
    machine.cpu.step(&mut machine.bus)
}

#[test]
fn opcode_table_matches_the_source_cycles() {
    // fullsnes "CPU Opcode Overview" and "CPU Clock Cycles": the `Clks` column, 8-bit widths, no
    // direct-page or page-crossing penalty. Each row is (opcode, the source's cycle count).
    let rows: &[(u8, u32, &str)] = &[
        (0xA9, 2, "LDA #nn 2"),
        (0xA5, 3, "LDA nn 3"),
        (0xB5, 4, "LDA nn,X 4"),
        (0xAD, 4, "LDA nnnn 4"),
        (0xBD, 4, "LDA nnnn,X 4*"),
        (0xB9, 4, "LDA nnnn,Y 4*"),
        (0xAF, 5, "LDA nnnnnn 5"),
        (0xA1, 6, "LDA (nn,X) 6"),
        (0xB2, 5, "LDA (nn) 5"),
        (0xB1, 5, "LDA (nn),Y 5*"),
        (0xA7, 6, "LDA [nn] 6"),
        (0xB7, 6, "LDA [nn],y 6"),
        (0xA3, 4, "LDA nn,S 4"),
        (0xB3, 7, "LDA (nn,S),Y 7"),
        (0x85, 3, "STA nn 3"),
        (0x95, 4, "STA nn,X 4"),
        (0x8D, 4, "STA nnnn 4"),
        (0x9D, 5, "STA nnnn,X 5"),
        (0x99, 5, "STA nnnn,Y 5"),
        (0x81, 6, "STA (nn,X) 6"),
        (0x91, 6, "STA (nn),Y 6"),
        (0x06, 5, "ASL nn 5"),
        (0x16, 6, "ASL nn,X 6"),
        (0x0E, 6, "ASL nnnn 6"),
        (0x1E, 7, "ASL nnnn,X 7"),
        (0x1A, 2, "INC A 2"),
        (0xE6, 5, "INC nn 5"),
        (0x04, 5, "TSB nn 5"),
        (0x0C, 6, "TSB nnnn 6"),
        (0x24, 3, "BIT nn 3"),
        (0x89, 2, "BIT #nn 2"),
        (0x20, 6, "JSR nnnn 6"),
        (0x22, 8, "JSL nnnnnn 8"),
        (0x4C, 3, "JMP nnnn 3"),
        (0x5C, 4, "JMP nnnnnn 4"),
        (0x6C, 5, "JMP [nnnn] 5"),
        (0x7C, 6, "JMP [nnnn+X] 6"),
        (0xDC, 6, "JMP FAR[nnnn] 6"),
        (0xFC, 8, "CALL [nnnn+X] 8"),
        (0x60, 6, "RTS 6"),
        (0x6B, 6, "RTL 6"),
        (0x40, 6, "RTI 6"),
        (0x00, 7, "BRK 7"),
        (0x02, 7, "COP 7"),
        (0x54, 7, "MVN 7x"),
        (0x44, 7, "MVP 7x"),
        (0x48, 3, "PHA 3"),
        (0x68, 4, "PLA 4"),
        (0xDA, 3, "PHX 3"),
        (0xFA, 4, "PLX 4"),
        (0x0B, 4, "PHD 4"),
        (0x2B, 5, "PLD 5"),
        (0xF4, 5, "PEA 5"),
        (0xD4, 6, "PEI 6"),
        (0x62, 6, "PER 6"),
        (0xC2, 3, "REP 3"),
        (0xE2, 3, "SEP 3"),
        (0xEB, 3, "XBA 3"),
        (0xCB, 3, "WAI 3"),
        (0xDB, 3, "STP 3"),
        (0xFB, 2, "XCE 2"),
        (0xEA, 2, "NOP 2"),
        (0x80, 3, "BRA 3"),
        (0x82, 4, "BRL 4"),
        (0xF0, 2, "BEQ 2 (not taken)"),
        (0xAA, 2, "TAX 2"),
        (0x18, 2, "CLC 2"),
        (0x42, 2, "WDM 2"),
    ];
    for &(opcode, cycles, source) in rows {
        let desc = table_entry(opcode);
        assert_eq!(
            base_cycles(desc),
            cycles,
            "opcode ${opcode:02X} ({source}) is {cycles} cycles in fullsnes \"CPU Clock Cycles\""
        );
    }

    // Every opcode has a row and a non-zero cycle count, which is what "the table covers all 256"
    // means: an unhandled opcode would have to be a `Desc` with no cycles.
    let mut seen = [false; 256];
    for opcode in 0u16..=255 {
        let desc = table_entry(opcode as u8);
        assert!(
            base_cycles(desc) > 0,
            "opcode ${opcode:02X} has no cycle count"
        );
        seen[usize::from(opcode)] = true;
    }
    assert!(seen.iter().all(|&s| s), "every opcode is in the table");

    // The same figure end to end: `LDA #nn` is two accesses (the opcode and the immediate), so 16
    // master clocks in WRAM, and `LDA nn` is three, so 24.
    let mut machine = machine_with(&[0xA9, 0x12, 0xA5, 0x10]);
    eight_bit(&mut machine);
    machine.bus.poke(Addr::new(0, 0x0010), 0x34);
    assert_eq!(step(&mut machine), 16, "LDA #nn: 2 accesses x 8 clocks");
    assert_eq!(step(&mut machine), 24, "LDA nn: 3 accesses x 8 clocks");
    assert_eq!(machine.cpu.a, 0x34);
}

#[test]
fn adc_sbc_8_and_16_bit_flags() {
    // 8-bit: $7F + $01 overflows into the sign bit, so V and N are set and C is not.
    let mut machine = machine_with(&[0xA9, 0x7F, 0x69, 0x01, 0xA9, 0xFF, 0x69, 0x01]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x80, "ADC $7F+$01");
    assert!(machine.cpu.p.v(), "ADC $7F+$01 sets V");
    assert!(machine.cpu.p.n(), "ADC $7F+$01 sets N");
    assert!(!machine.cpu.p.c(), "ADC $7F+$01 does not carry out");
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x00, "ADC $FF+$01");
    assert!(machine.cpu.p.c(), "ADC $FF+$01 carries out");
    assert!(machine.cpu.p.z(), "ADC $FF+$01 is zero");

    // 16-bit: the same test one byte wider.
    let mut machine = machine_with(&[0xA9, 0xFF, 0x7F, 0x69, 0x01, 0x00]);
    sixteen_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x8000, "ADC $7FFF+$0001");
    assert!(
        machine.cpu.p.v() && machine.cpu.p.n(),
        "ADC $7FFF+1 V and N"
    );
    assert!(!machine.cpu.p.c(), "ADC $7FFF+1 does not carry out");

    // SBC borrows: $00 - $01 = $FF with C clear and N set, and C set means "no borrow".
    let mut machine = machine_with(&[0xA9, 0x00, 0x38, 0xE9, 0x01]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0xFF, "SBC $00-$01");
    assert!(!machine.cpu.p.c(), "SBC $00-$01 borrows");
    assert!(machine.cpu.p.n(), "SBC $00-$01 sets N");
}

#[test]
fn cmp_and_bit_set_nvz() {
    // CMP sets no V; Z on equality and C when the accumulator is the larger unsigned value.
    let mut machine = machine_with(&[0xA9, 0x05, 0xC9, 0x05, 0xC9, 0x06]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert!(machine.cpu.p.z() && machine.cpu.p.c(), "CMP equal: Z and C");
    step(&mut machine);
    assert!(!machine.cpu.p.c(), "CMP $05,$06: C clear (less)");
    assert!(machine.cpu.p.n(), "CMP $05,$06: N set");

    // BIT: Z from A AND M, N and V from the operand's top two bits (fullsnes "Bit Test").
    let mut machine = machine_with(&[0xA9, 0x00, 0x2C, 0x00, 0x01]);
    eight_bit(&mut machine);
    machine.bus.poke(Addr::new(0, 0x0100), 0xC0);
    step(&mut machine);
    step(&mut machine);
    assert!(machine.cpu.p.z(), "BIT with A=0 sets Z");
    assert!(machine.cpu.p.n(), "BIT takes N from operand bit 7");
    assert!(machine.cpu.p.v(), "BIT takes V from operand bit 6");

    // CPX/CPY use the index width and leave the accumulator alone.
    let mut machine = machine_with(&[0xA2, 0x10, 0xE0, 0x10]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert!(machine.cpu.p.z() && machine.cpu.p.c(), "CPX equal");
}

#[test]
fn decimal_mode_add_sub() {
    // Packed BCD: $09 + $01 = $10 with no carry out, and $99 + $01 = $00 with one.
    let mut machine = machine_with(&[0xF8, 0x18, 0xA9, 0x09, 0x69, 0x01, 0xA9, 0x99, 0x69, 0x01]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x10, "decimal $09 + $01 = $10");
    assert!(!machine.cpu.p.c(), "decimal $09 + $01 does not carry");
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x00, "decimal $99 + $01 = $00");
    assert!(machine.cpu.p.c(), "decimal $99 + $01 carries out");
    assert!(
        machine.cpu.p.z(),
        "decimal $99 + $01 leaves the result zero"
    );

    // 16-bit decimal: `REP`/`SEP` cannot clear M in emulation mode, so this one runs native.
    let mut machine = machine_with(&[0xF8, 0x18, 0xA9, 0x99, 0x99, 0x69, 0x01, 0x00]);
    sixteen_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x0000, "decimal $9999 + $0001 = $0000");
    assert!(machine.cpu.p.c(), "decimal $9999 + $0001 carries out");

    // Decimal subtraction borrows per nibble: $20 - $01 = $19, $00 - $01 = $99.
    let mut machine = machine_with(&[0xF8, 0x38, 0xA9, 0x20, 0xE9, 0x01]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x19, "decimal $20 - $01 = $19");
    assert!(
        machine.cpu.p.c(),
        "decimal subtraction leaves C set (no borrow)"
    );
    let mut machine = machine_with(&[0xF8, 0x38, 0xA9, 0x00, 0xE9, 0x01]);
    eight_bit(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x99, "decimal $00 - $01 = $99");
    assert!(!machine.cpu.p.c(), "decimal $00 - $01 borrows");
}

#[test]
fn mvp_mvn_block_move() {
    // `MVN` copies four bytes from $7E:1000 to $7E:2000; `A` counts down to $FFFF and the opcode
    // is not re-entered, so one step moves the whole block.
    let mut machine = machine_with(&[0x54, 0x7E, 0x7E]);
    sixteen_bit(&mut machine);
    machine.cpu.a = 3;
    machine.cpu.x = 0x1000;
    machine.cpu.y = 0x2000;
    for i in 0u16..4 {
        machine
            .bus
            .poke(Addr::new(0x7E, 0x1000 + i), (0xA0 + i) as u8);
    }
    // `MVN` moves one byte per execution and re-enters itself by backing `PC` up three, so the
    // harness steps it once per byte (`A` counts down to $FFFF on the last one).
    let mut cost = 0;
    for _ in 0..4 {
        cost += step(&mut machine);
    }
    for i in 0u16..4 {
        assert_eq!(
            machine.bus.peek(Addr::new(0x7E, 0x2000 + i)),
            (0xA0 + i) as u8,
            "MVN copied byte {i}"
        );
    }
    assert_eq!(machine.cpu.a, 0xFFFF, "MVN counts A down to $FFFF");
    assert_eq!(machine.cpu.x, 0x1004, "MVN walked X up");
    assert_eq!(machine.cpu.y, 0x2004, "MVN walked Y up");
    assert_eq!(machine.cpu.db, 0x7E, "MVN sets DB to the destination bank");
    // Each byte is the re-entered opcode's two fetches, the read and the write — five accesses —
    // plus two internal cycles, which is fullsnes's "7x": 4 x (5*8 + 2*6).
    assert_eq!(cost, 4 * (5 * 8 + 2 * 6), "MVN's clocks");

    // `MVP` walks both offsets backwards.
    let mut machine = machine_with(&[0x44, 0x7E, 0x7E]);
    sixteen_bit(&mut machine);
    machine.cpu.a = 3;
    machine.cpu.x = 0x1003;
    machine.cpu.y = 0x2003;
    for i in 0u16..4 {
        machine
            .bus
            .poke(Addr::new(0x7E, 0x1000 + i), (0xB0 + i) as u8);
    }
    for _ in 0..4 {
        step(&mut machine);
    }
    assert_eq!(
        machine.bus.peek(Addr::new(0x7E, 0x2003)),
        0xB3,
        "MVP high byte"
    );
    assert_eq!(
        machine.bus.peek(Addr::new(0x7E, 0x2000)),
        0xB0,
        "MVP low byte"
    );
    assert_eq!(machine.cpu.x, 0x0FFF, "MVP walked X down");
}

#[test]
fn xce_switches_widths() {
    // `CLC` then `XCE`: E takes C (0), and C takes the old E (1). M and X stay as they were, so
    // `REP #$30` is what makes the registers 16 bits wide.
    let mut machine = machine_with(&[0x18, 0xFB, 0xC2, 0x30, 0xA9, 0x34, 0x12]);
    eight_bit(&mut machine);
    machine.cpu.p.set_c(false);
    step(&mut machine);
    step(&mut machine);
    assert!(!machine.cpu.e, "XCE left emulation mode");
    assert!(machine.cpu.p.c(), "XCE moved the old E into C");
    assert!(
        machine.cpu.p.m() && machine.cpu.p.x(),
        "XCE does not change M or X"
    );
    step(&mut machine);
    assert!(
        !machine.cpu.p.m() && !machine.cpu.p.x(),
        "REP #$30 cleared M and X"
    );
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x1234, "the immediate was two bytes");

    // The way back: with C set, XCE re-enters emulation mode, forces the 8-bit flags and pulls the
    // stack into page one.
    let mut machine = machine_with(&[0x38, 0xFB]);
    sixteen_bit(&mut machine);
    machine.cpu.s = 0x1234;
    step(&mut machine);
    step(&mut machine);
    assert!(machine.cpu.e, "XCE re-entered emulation mode");
    assert!(machine.cpu.p.m() && machine.cpu.p.x(), "E forces M and X");
    assert_eq!(machine.cpu.s, 0x0134, "the emulation stack is page one");
}

#[test]
fn stack_is_page_one_in_emulation_mode() {
    let mut machine = machine_with(&[0x48, 0x48, 0x48]);
    eight_bit(&mut machine);
    machine.cpu.s = 0x0100;
    machine.cpu.a = 0x5A;
    step(&mut machine);
    assert_eq!(machine.cpu.s, 0x01FF, "the stack wrapped inside page one");
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x0100)),
        0x5A,
        "pushed at $0100"
    );
    step(&mut machine);
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x01FF)),
        0x5A,
        "pushed at $01FF"
    );
    assert_eq!(machine.cpu.s, 0x01FE);

    // Native mode's stack is the whole 64 KiB: an 8-bit push at $0100 goes to $00FF.
    let mut machine = machine_with(&[0x48]);
    machine.cpu.e = false;
    machine.cpu.p = Flags(Flags::I | Flags::M | Flags::X);
    machine.cpu.s = 0x0100;
    machine.cpu.a = 0x77;
    step(&mut machine);
    assert_eq!(
        machine.cpu.s, 0x00FF,
        "native mode's push falls out of page one"
    );
    assert_eq!(machine.bus.peek(Addr::new(0, 0x0100)), 0x77);
}

#[test]
fn direct_page_zero_is_a_penalty() {
    // `LDA nn` is three accesses plus the direct-page penalty: fullsnes "+n if (D AND 00FFh)>0".
    let mut machine = machine_with(&[0xA5, 0x10]);
    eight_bit(&mut machine);
    machine.bus.poke(Addr::new(0, 0x0010), 0x11);
    assert_eq!(
        step(&mut machine),
        24,
        "LDA nn with D=0: 3 accesses x 8 clocks"
    );

    let mut machine = machine_with(&[0xA5, 0x10]);
    eight_bit(&mut machine);
    machine.cpu.d = 0x0110;
    machine.bus.poke(Addr::new(0, 0x0120), 0x22);
    assert_eq!(
        step(&mut machine),
        24 + 6,
        "LDA nn with D=$0110: 3 accesses x 8 clocks plus one internal cycle of 6"
    );
    assert_eq!(machine.cpu.a, 0x22, "the address is D + nn");
}

#[test]
fn page_crossing_adds_a_cycle() {
    // fullsnes "CPU Clock Cycles": "CPPyDd 4,5,6 ... +y" — one cycle when indexing crosses a page.
    let mut machine = machine_with(&[0xBD, 0xF0, 0x13]);
    eight_bit(&mut machine);
    machine.cpu.x = 0x0F;
    machine.bus.poke(Addr::new(0, 0x13FF), 0x01);
    assert_eq!(
        step(&mut machine),
        32,
        "LDA $13F0,X with X=$0F: 4 accesses, no page cross"
    );

    let mut machine = machine_with(&[0xBD, 0xF0, 0x13]);
    eight_bit(&mut machine);
    machine.cpu.x = 0x20;
    machine.bus.poke(Addr::new(0, 0x1410), 0x02);
    assert_eq!(
        step(&mut machine),
        32 + 6,
        "LDA $13F0,X with X=$20 crosses into $1400: one extra internal cycle"
    );
    assert_eq!(machine.cpu.a, 0x02);
}

#[test]
fn rep_sep_change_width_next_instruction() {
    // `REP #$20` changes M for the *next* instruction; its own operand is always one byte. This
    // has to run in native mode: in emulation mode `E` forces M and X on, so `REP` cannot clear
    // them (asserted at the end).
    let mut machine = machine_with(&[0xC2, 0x20, 0xA9, 0x34, 0x12, 0xE2, 0x20, 0xA9, 0x56]);
    machine.cpu.e = false;
    machine.cpu.p = Flags(Flags::I | Flags::M | Flags::X);
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x0002, "REP consumed one operand byte");
    assert!(!machine.cpu.p.m(), "REP #$20 cleared M");
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x1234, "the next instruction was 16-bit");
    step(&mut machine);
    assert!(machine.cpu.p.m(), "SEP #$20 set M again");
    step(&mut machine);
    assert_eq!(machine.cpu.a, 0x1256, "the 8-bit write kept A's high byte");
    assert_eq!(machine.cpu.pc, 0x0009, "SEP's immediate was one byte");

    // `REP` cannot clear M or X in emulation mode.
    let mut machine = machine_with(&[0xC2, 0x30]);
    eight_bit(&mut machine);
    step(&mut machine);
    assert!(
        machine.cpu.p.m() && machine.cpu.p.x(),
        "emulation mode keeps M and X set"
    );
}

#[test]
fn wai_stops_until_an_interrupt() {
    // Native mode, so the IRQ vector is `$FFEE` and the test can patch it in the synthesized ROM.
    let patches: [(usize, &[u8]); 1] = [(0x7FEE, &[0x00, 0x90])];
    let mut machine = machine_with_rom(&[0xCB, 0xEA, 0xEA], &patches);
    sixteen_bit(&mut machine);
    machine.cpu.p.set_i(true);
    step(&mut machine);
    assert!(machine.cpu.waiting, "WAI put the CPU to sleep");
    let pc = machine.cpu.pc;
    for _ in 0..8 {
        assert_eq!(
            step(&mut machine),
            6,
            "a waiting step is one internal cycle"
        );
    }
    assert_eq!(machine.cpu.pc, pc, "a waiting CPU does not advance");

    // An IRQ wakes it even with I set, and the next step enters the handler once I is clear.
    machine.bus.io.nmitimen = 0x10;
    machine.bus.io.irq_flag = true;
    step(&mut machine);
    assert!(!machine.cpu.waiting, "the IRQ woke the CPU even with I set");
    assert_eq!(
        machine.cpu.pc, pc,
        "the masked IRQ still did not dispatch at that step"
    );
    machine.cpu.p.set_i(false);
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x9000, "the IRQ vector was taken");
}

#[test]
fn stp_stops_until_reset() {
    let mut machine = machine_with(&[0xDB, 0xEA]);
    eight_bit(&mut machine);
    step(&mut machine);
    assert!(machine.cpu.stopped, "STP stopped the CPU");
    let pc = machine.cpu.pc;
    for _ in 0..4 {
        assert_eq!(
            step(&mut machine),
            6,
            "a stopped step is one internal cycle"
        );
    }
    assert_eq!(machine.cpu.pc, pc, "a stopped CPU does not advance");

    let mut bus = machine.bus;
    machine.cpu.reset(&mut bus);
    assert!(!machine.cpu.stopped, "reset cleared STP");
    assert_eq!(machine.cpu.pc, 0x8000, "reset reloads the reset vector");
}

#[test]
fn brk_and_cop_use_their_vectors() {
    // BRK in emulation mode: vector $FFFE, three bytes pushed, and the pushed P has B and U set.
    let mut machine = machine_with_rom(&[0x00], &[(0x7FFE, &[0x00, 0x90])]);
    eight_bit(&mut machine);
    machine.cpu.s = 0x01FF;
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x9000, "BRK took $00:FFFE");
    assert_eq!(machine.cpu.s, 0x01FC, "BRK pushed three bytes");
    let pushed = machine.bus.peek(Addr::new(0, 0x01FD));
    assert_eq!(pushed & 0x30, 0x30, "BRK pushes B and U set: ${pushed:02X}");
    assert_eq!(machine.cpu.pc, 0x9000, "the vector's word is little-endian");

    // COP in native mode: vector $FFE4, four bytes pushed (PB included), B clear.
    let mut machine = machine_with_rom(&[0x02], &[(0x7FE4, &[0x00, 0x90])]);
    sixteen_bit(&mut machine);
    machine.cpu.s = 0x01FF;
    machine.cpu.pb = 0x7E;
    let cost = step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x9000, "COP took $00:FFE4");
    assert_eq!(machine.cpu.pb, 0, "exceptions run in bank $00");
    assert_eq!(
        machine.cpu.s, 0x01FB,
        "COP pushed four bytes in native mode"
    );
    // fullsnes "CPsSSSDD 7,8": the opcode, its parameter, four pushes and two vector bytes is eight
    // accesses and no internal cycle, so 8 x 8 master clocks in WRAM.
    assert_eq!(cost, 8 * 8, "COP's clocks in native mode");
}

#[test]
fn jsl_and_rtl_restore_banks() {
    // `JSL $7F5000` runs from $7E:0000; the byte at $7F:5000 is an `RTL`, so the test needs no ROM
    // patch and exercises the bank switch through WRAM.
    let mut machine = machine_with(&[0x22, 0x00, 0x50, 0x7F, 0xEA]);
    sixteen_bit(&mut machine);
    machine.bus.poke(Addr::new(0x7F, 0x5000), 0x6B);
    step(&mut machine);
    assert_eq!(machine.cpu.pb, 0x7F, "JSL switched the program bank");
    assert_eq!(machine.cpu.pc, 0x5000, "JSL set the offset");
    assert_eq!(machine.cpu.s, 0x01FC, "JSL pushed three bytes");
    step(&mut machine);
    assert_eq!(machine.cpu.pb, 0x7E, "RTL restored the program bank");
    assert_eq!(
        machine.cpu.pc, 0x0004,
        "RTL returned past the JSL's operands"
    );
    assert_eq!(machine.cpu.s, 0x01FF);

    // `JSR`/`RTS` are the same shape without the bank. `JSR` does not change `PB`, so the target
    // lives in the code's own bank here.
    let mut machine = machine_with(&[0x20, 0x00, 0x90, 0xEA]);
    sixteen_bit(&mut machine);
    machine.bus.poke(Addr::new(0x7E, 0x9000), 0x60);
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x9000, "JSR to $9000");
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x0003, "RTS returned to the byte after JSR");
}

#[test]
fn plb_pulls_one_byte_and_leaves_the_caller_frame_intact() {
    // The stock "DB = PB" prologue pushes one byte per `PHB`/`PHK`, so the two `PLB`s that bracket
    // the call must pull one byte each: fullsnes "Push/Pull" (`AB nz---- 4 PLB POP DB 8 DB=[S]`) and
    // the dev wiki's instruction table (`AB | Stack (Pull) | 1 bytes | 4 cycles`). A two-byte `PLB`
    // ate a byte of the `JSL` frame per prologue, so the matching `RTL` returned into whatever the
    // stack held one byte lower — Super Mario World's boot returned to `$9A:A894` from `$01:B7B3`.
    // `PLD` is the 16-bit pull; `PLB` is not.
    let prologue = [0x8B, 0x4B, 0xAB, 0xAB, 0x6B]; // PHB ; PHK ; PLB ; PLB ; RTL
    let mut machine = machine_with(&[0x22, 0x10, 0x00, 0x7E, 0xEA]); // JSL $7E:0010 ; nop
    sixteen_bit(&mut machine);
    for (i, byte) in prologue.iter().enumerate() {
        machine.bus.poke(Addr::new(0x7E, 0x0010 + i as u16), *byte);
    }
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x0010, "JSL reached the prologue");
    assert_eq!(machine.cpu.s, 0x01FC, "JSL pushed three bytes");
    for _ in 0..4 {
        step(&mut machine); // PHB, PHK, PLB, PLB
    }
    assert_eq!(
        machine.cpu.s, 0x01FC,
        "PHB and PHK push one byte each, so two PLBs must pull one byte each"
    );
    assert_eq!(machine.cpu.db, 0x00, "the second PLB restored DB");
    step(&mut machine); // RTL
    assert_eq!(
        machine.cpu.pc, 0x0004,
        "RTL returned past the JSL's operands"
    );
    assert_eq!(machine.cpu.pb, 0x7E, "RTL restored the program bank");
    assert_eq!(machine.cpu.s, 0x01FF, "the frame is balanced");

    // And the pulled byte is what DB and the flags see.
    let mut machine = machine_with(&[0xAB]);
    sixteen_bit(&mut machine);
    machine.cpu.s = 0x01FE;
    machine.bus.poke(Addr::new(0, 0x01FF), 0x80);
    step(&mut machine);
    assert_eq!(machine.cpu.db, 0x80, "DB is the pulled byte");
    assert_eq!(machine.cpu.s, 0x01FF, "PLB pulled exactly one byte");
    assert!(machine.cpu.p.n(), "PLB sets N from bit 7 of that byte");
    assert!(!machine.cpu.p.z(), "PLB clears Z for a non-zero byte");
}

#[test]
fn all_256_opcodes_have_a_handler() {
    // Running every opcode from a NOP-filled WRAM in both modes: a missing handler is either a
    // missing table row (asserted above) or a `panic` here.
    for opcode in 0u16..=255 {
        for native in [false, true] {
            let mut machine = machine_with(&[]);
            for offset in 0u16..0x400 {
                machine.bus.poke(Addr::new(0x7E, offset), 0xEA);
            }
            machine.bus.poke(Addr::new(0x7E, 0), opcode as u8);
            if native {
                sixteen_bit(&mut machine);
            } else {
                eight_bit(&mut machine);
            }
            let cost = step(&mut machine);
            assert!(
                cost > 0,
                "opcode ${opcode:02X} charged no clocks in {} mode",
                if native { "native" } else { "emulation" }
            );
        }
    }
}

#[test]
fn plp_clears_the_index_high_byte_when_x_is_set() {
    // fullsnes "CPU Registers and Flags", Index Register Flag (X): "Switches X and Y to 8bit mode
    // (the upper 8bit are forcefully set to 00h)." 6502.org, *65C816 Opcodes*, §6.10.4 (XCE):
    // "when the e flag is 1, the m and x flags are forced to 1 (and consequently the XH and YH
    // registers are forced to $00)". `REP`, `SEP` and `XCE` apply that through `apply_width`; `PLP`
    // loads P from the stack and must apply the same rule, or a callee's $01xx survives into the
    // caller's 8-bit index and every `LDA table,X` reads $01xx too far.
    //
    // The instruction sequence is the idiom the issue names: `PHP` (x set) : `REP #$10` (16-bit
    // index) : `LDX #$0123` : `PLP` (x set again) must leave X = $0023.
    let mut machine = machine_with(&[0x08, 0xC2, 0x10, 0xA2, 0x23, 0x01, 0x28, 0xC2, 0x20, 0x8A]);
    machine.cpu.e = false;
    machine.cpu.p = Flags(Flags::I | Flags::M | Flags::X);
    step(&mut machine); // PHP
    step(&mut machine); // REP #$10: x clear, so the index registers are 16-bit
    step(&mut machine); // LDX #$0123
    assert_eq!(
        machine.cpu.x, 0x0123,
        "the sequence needs a 16-bit X to hold $0123"
    );
    machine.cpu.y = 0x0145;
    step(&mut machine); // PLP: x set again
    assert!(machine.cpu.p.x(), "the pulled P has x set");
    assert_eq!(
        machine.cpu.x, 0x0023,
        "PLP restored x=1, which forces XH to $00 (fullsnes: \"the upper 8bit are forcefully set \
         to 00h\"); a stale $01 would make this $0123"
    );
    assert_eq!(
        machine.cpu.y, 0x0045,
        "the same rule clears YH: $0145 becomes $0045"
    );
    step(&mut machine); // REP #$20: 16-bit A
    step(&mut machine); // TXA
    assert_eq!(
        machine.cpu.a, 0x0023,
        "TXA into a 16-bit A shows $0023; $0123 is the stale high byte"
    );
}

#[test]
fn rti_clears_the_index_high_byte_when_x_is_set() {
    // The same rule on the interrupt return's frame: fullsnes "Index Register Flag (X)": "the upper
    // 8bit are forcefully set to 00h", and 6502.org §6.10.4 for the flag that forces it. `RTI` pulls
    // P first, so the x bit it restores decides X's width before any further index is added.
    let mut machine = machine_with(&[0x40, 0xEA]);
    sixteen_bit(&mut machine); // 16-bit index, so X can hold $0123 when the frame is built
    machine.cpu.x = 0x0123;
    machine.cpu.y = 0x01FF;
    // The frame `BRK` leaves, top-down: P, PCL, PCH, PBR. The pulled P has x set.
    machine.cpu.s = 0x01FB;
    machine
        .bus
        .poke(Addr::new(0, 0x01FC), Flags::I | Flags::M | Flags::X);
    machine.bus.poke(Addr::new(0, 0x01FD), 0x00);
    machine.bus.poke(Addr::new(0, 0x01FE), 0x40);
    machine.bus.poke(Addr::new(0, 0x01FF), 0x7E);
    step(&mut machine);
    assert_eq!(machine.cpu.pc, 0x4000, "RTI pulled the return address");
    assert!(machine.cpu.p.x(), "the pulled P has x set");
    assert_eq!(
        machine.cpu.x, 0x0023,
        "RTI restored x=1, which forces XH to $00 (fullsnes); $0123 is the stale high byte"
    );
    assert_eq!(machine.cpu.y, 0x00FF, "the same rule clears YH");
}

#[test]
fn sixteen_bit_data_read_carries_into_the_next_bank() {
    // 6502.org, *65C816 Opcodes*, §5.16 (LONG): "If the m flag is 0, then LDA $12FFFF loads the low
    // byte of the data from address $12FFFF, and the high byte from address $130000." §2: "in
    // native mode, there is no wrapping at page boundaries".
    //
    // $7E/$7F stand in for that pair because both bytes have to be writable: a LoROM image maps
    // $12:FFFF to cartridge ROM. Bank $7E is the low 64 KiB of WRAM and $7F the high, so $7F:0000
    // and the byte a wrap inside the bank would fetch ($7E:0000) are different bytes. The opcode
    // sits above the pair, so $7E:0000 can hold the byte the wrap would fetch by mistake.
    let mut machine = machine_with(&[0xEA, 0xEA, 0xEA, 0xEA, 0xAF, 0xFF, 0xFF, 0x7E]);
    sixteen_bit(&mut machine);
    machine.cpu.pc = 0x0004;
    machine.bus.poke(Addr::new(0x7E, 0xFFFF), 0x34); // the low byte, where the operand points
    machine.bus.poke(Addr::new(0x7F, 0x0000), 0x12); // the high byte, one byte on across the bank
    machine.bus.poke(Addr::new(0x7E, 0x0000), 0xAB); // what a wrap inside the bank would fetch
    step(&mut machine);
    assert_eq!(
        machine.cpu.a, 0x1234,
        "LDA $7EFFFF (m=0) reads $7E:FFFF and then $7F:0000, not $7E:0000 ($AB)"
    );
}

#[test]
fn sixteen_bit_data_write_carries_into_the_next_bank() {
    // The store side of §5.16's split: `STA $7EFFFF` with a 16-bit A puts the high byte at
    // $7F:0000, one 24-bit address on from $7E:FFFF (6502.org §2: no wrapping at page boundaries).
    let mut machine = machine_with(&[0xEA, 0xEA, 0xEA, 0xEA, 0x8F, 0xFF, 0xFF, 0x7E]);
    sixteen_bit(&mut machine);
    machine.cpu.pc = 0x0004;
    machine.cpu.a = 0x1234;
    machine.bus.poke(Addr::new(0x7E, 0x0000), 0xAB);
    step(&mut machine);
    assert_eq!(
        machine.bus.peek(Addr::new(0x7E, 0xFFFF)),
        0x34,
        "the low byte went where the operand points"
    );
    assert_eq!(
        machine.bus.peek(Addr::new(0x7F, 0x0000)),
        0x12,
        "the high byte carried into $7F:0000; a bank-local wrap writes $7E:0000"
    );
    assert_eq!(
        machine.bus.peek(Addr::new(0x7E, 0x0000)),
        0xAB,
        "the byte at $7E:0000 is untouched"
    );
}

#[test]
fn abs_indexed_carry_into_the_next_bank() {
    // 6502.org §2: "in native mode, there is no wrapping at page boundaries". §5.17 (LONG,X) states
    // the same rule case by case for an indexed address: "If the X register is $000A and the m flag
    // is 0, then LDA $12FFFE,X loads the low byte of the data from address $130008, and the high
    // byte from address $130009."
    //
    // The pairs below are $7E/$7F rather than the tutorial's $12/$13 because the test has to see
    // which bank was read: banks $00-$3F map offsets $0000-$1FFF to the *same* WRAM bytes whatever
    // the bank (`Bus`'s low mirror is `wram[offset]`), so $12:0008 and $13:0008 are one byte and
    // the wrapped and carried addresses would read the same value. $7E is WRAM's low 64 KiB and
    // $7F its high, so $7F:0008 and the byte a wrap inside the bank would fetch are different.
    let mut machine = machine_with(&[0xBD, 0xF0, 0xFF]);
    sixteen_bit(&mut machine);
    machine.cpu.db = 0x7E;
    machine.cpu.x = 0x0020;
    machine.bus.poke(Addr::new(0x7F, 0x0010), 0x56);
    machine.bus.poke(Addr::new(0x7F, 0x0011), 0x34);
    machine.bus.poke(Addr::new(0x7E, 0x0010), 0xAA);
    machine.bus.poke(Addr::new(0x7E, 0x0011), 0xBB);
    step(&mut machine);
    assert_eq!(
        machine.cpu.a, 0x3456,
        "LDA $7EFFF0,X with X=$20 reads $7F:0010 and $7F:0011, not $7E:0010/$7E:0011 ($BBAA)"
    );

    // The tutorial's base and index, one page lower: $FFFE + $000A wraps to $0008 inside the bank,
    // and the carry moves that $0008 into $7F.
    let mut machine = machine_with(&[0xBD, 0xFE, 0xFF]);
    sixteen_bit(&mut machine);
    machine.cpu.db = 0x7E;
    machine.cpu.x = 0x000A;
    machine.bus.poke(Addr::new(0x7F, 0x0008), 0x56);
    machine.bus.poke(Addr::new(0x7F, 0x0009), 0x34);
    machine.bus.poke(Addr::new(0x7E, 0x0008), 0xAA);
    machine.bus.poke(Addr::new(0x7E, 0x0009), 0xBB);
    step(&mut machine);
    assert_eq!(
        machine.cpu.a, 0x3456,
        "LDA $7EFFFE,X with X=$000A reads $7F:0008 and $7F:0009, not $7E:0008/$7E:0009 ($BBAA)"
    );

    // `abs,Y` is the same mode with the other index.
    let mut machine = machine_with(&[0xB9, 0xFE, 0xFF]);
    sixteen_bit(&mut machine);
    machine.cpu.db = 0x7E;
    machine.cpu.y = 0x000A;
    machine.bus.poke(Addr::new(0x7F, 0x0008), 0x56);
    machine.bus.poke(Addr::new(0x7F, 0x0009), 0x34);
    machine.bus.poke(Addr::new(0x7E, 0x0008), 0xAA);
    machine.bus.poke(Addr::new(0x7E, 0x0009), 0xBB);
    step(&mut machine);
    assert_eq!(
        machine.cpu.a, 0x3456,
        "LDA $7EFFFE,Y with Y=$000A reads $7F:0008 and $7F:0009"
    );
}

#[test]
fn long_x_carries_into_the_next_bank() {
    // 6502.org §5.17 (LONG,X): "If the X register is $000A and the m flag is 0, then LDA
    // $12FFFE,X loads the low byte of the data from address $130008, and the high byte from
    // address $130009." The carry goes into the bank the operand names. $7E/$7F stand in for
    // $12/$13 for the reason the `abs,X` test gives: in the mirror the two banks' low pages are
    // the same bytes, and this machine reads them from the same place.
    let mut machine = machine_with(&[0xBF, 0xFE, 0xFF, 0x7E]);
    sixteen_bit(&mut machine);
    machine.cpu.x = 0x000A;
    machine.bus.poke(Addr::new(0x7F, 0x0008), 0x56);
    machine.bus.poke(Addr::new(0x7F, 0x0009), 0x34);
    machine.bus.poke(Addr::new(0x7E, 0x0008), 0xAA);
    machine.bus.poke(Addr::new(0x7E, 0x0009), 0xBB);
    step(&mut machine);
    assert_eq!(
        machine.cpu.a, 0x3456,
        "LDA $7EFFFE,X (long) reads $7F:0008 and $7F:0009, not $7E:0008/$7E:0009 ($BBAA)"
    );
}

#[test]
fn indirect_indexed_carry_into_the_next_bank() {
    // The three indirect modes that add Y to a pointer: fullsnes's 65XX table gives them as
    // "MOV A,[FAR[D+nn]+Y]" and "A=[FAR[nnnnnn+X]+Y]" — a 24-bit address plus the index — and
    // 6502.org §2 rules out page-boundary wrapping in native mode. Each case below points at
    // $FFFF and adds Y = $0001, so the data byte is at offset $0000 of the next bank: DB = $7E
    // reads $7F:0000, and $7E:0000 holds what a wrap inside the bank would fetch.
    let cases: &[(&str, &[u8], bool)] = &[
        ("LDA ($10),Y", &[0xEA, 0xEA, 0xB1, 0x10], false),
        ("LDA [$10],Y", &[0xEA, 0xEA, 0xB7, 0x10], true),
        ("LDA ($10,S),Y", &[0xEA, 0xEA, 0xB3, 0x10], false),
    ];
    for (name, code, long_pointer) in cases {
        let mut machine = machine_with(code);
        sixteen_bit(&mut machine);
        machine.cpu.pc = 0x0002;
        machine.cpu.db = 0x7E;
        machine.cpu.y = 0x0001;
        machine.cpu.s = 0x01E0;
        machine.cpu.d = 0;
        // The pointer: $FFFF, in bank $00 (direct page) or $01F0 (stack relative, S + $10).
        let at = if *name == "LDA ($10,S),Y" {
            0x01F0
        } else {
            0x0010
        };
        machine.bus.poke(Addr::new(0, at), 0xFF);
        machine.bus.poke(Addr::new(0, at + 1), 0xFF);
        if *long_pointer {
            machine.bus.poke(Addr::new(0, at + 2), 0x7E);
        }
        machine.bus.poke(Addr::new(0x7F, 0x0000), 0x56);
        machine.bus.poke(Addr::new(0x7F, 0x0001), 0x34);
        machine.bus.poke(Addr::new(0x7E, 0x0000), 0xAA);
        machine.bus.poke(Addr::new(0x7E, 0x0001), 0xBB);
        step(&mut machine);
        assert_eq!(
            machine.cpu.a, 0x3456,
            "{name} with the pointer at $FFFF and Y=$0001 reads $7F:0000 and $7F:0001, not \
             $7E:0000/$7E:0001 ($BBAA): a 24-bit address carries into the next bank"
        );
    }
}
