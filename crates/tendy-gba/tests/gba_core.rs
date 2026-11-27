//! The ARM7TDMI core, tested where the public suites cannot see.
//!
//! `roms/test/gba/arm.gba` and `thumb.gba` are the gate and they are thorough about the instruction
//! set; they cannot see the things this file checks, either because they need a second exception or
//! because they need a hand-built instruction whose encoding the suite never spells out: the two
//! pipeline offsets, an `LDM` naming its own base, `STM` of `R15`, the comparison opcodes' `Rd`
//! field, the ARM7TDMI's misaligned halfword rules, the block-transfer alignment of `LDM` and Thumb
//! `LDMIA`/`PUSH`/`POP` at a mis-aligned base, `MSR` into a banked `SPSR`, and FIQ's `R8`-`R12`
//! surviving an IRQ. The barrel shifter's carry on every shift type lives next to the shifter itself,
//! in `crate::cpu`.

use tendy_gb::System;
use tendy_gba::bus::IWRAM_BASE;
use tendy_gba::cart::Cartridge;
use tendy_gba::cpu;
use tendy_gba::{CLOCK_HZ, Gba};

/// A cartridge whose code starts at `$080000C0`: the entry point is the usual `b`.
fn synth_cart(code: &[u32]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-CORE\0");
    rom[0xAC..0xB0].copy_from_slice(b"TCPU");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    for (index, word) in code.iter().enumerate() {
        let at = 0xC0 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine running `code`.
fn machine(code: &[u32]) -> Gba {
    Gba::new(Cartridge::new(synth_cart(code)).expect("a synthesized cartridge"))
}

/// The machine after `steps` instructions.
fn ran(code: &[u32], steps: usize) -> Gba {
    let mut gba = machine(code);
    for _ in 0..steps {
        gba.step_instruction();
    }
    gba
}

/// The address of the `n`th instruction of the code.
fn at(n: usize) -> u32 {
    0x0800_00C0 + (n as u32) * 4
}

#[test]
fn a_data_processing_instruction_reads_r15_at_eight_and_a_register_shift_at_twelve() {
    // add r0, pc, #4 / mov r1, pc: both see the program counter 8 bytes past their own address.
    let gba = ran(&[0xE28F_0004, 0xE1A0_100F], 2);
    assert_eq!(gba.cpu.r[0], at(0) + 12);
    assert_eq!(gba.cpu.r[1], gba.cpu.r[0]);

    // add r0, pc, r0, lsl r0 with r0 = 0: a register-specified shift reads PC+12 — for the first
    // operand too — so the result still matches the following `mov r1, pc`.
    let gba = ran(&[0xE3A0_0000, 0xE08F_0010, 0xE1A0_100F], 3);
    assert_eq!(gba.cpu.r[0], at(1) + 12);
    assert_eq!(gba.cpu.r[1], gba.cpu.r[0]);
}

#[test]
fn an_ldm_whose_list_names_its_base_loads_the_base_and_writes_nothing_back() {
    // mov r0,mem / mov r1,#$11 / mov r2,#$22 / stmia r0!,{r1,r2} / sub r0,#8 / ldmia r0!,{r0,r1}.
    // On ARMv4 an LDM with the base in its list does not write the base back, so R0 keeps the
    // loaded value rather than the address past the block.
    let mut gba = machine(&[
        0xE3A0_0403, // mov r0,#$03000000
        0xE3A0_1011, // mov r1,#$11
        0xE3A0_2022, // mov r2,#$22
        0xE8A0_0006, // stmia r0!,{r1,r2}
        0xE240_0008, // sub r0,r0,#8
        0xE8B0_0003, // ldmia r0!,{r0,r1}
        0xEAFF_FFFE, // b .
    ]);
    for _ in 0..6 {
        gba.step_instruction();
    }
    assert_eq!(
        gba.cpu.r[0], 0x11,
        "the loaded value survives the write-back"
    );
    assert_eq!(gba.cpu.r[1], 0x22);
    assert_eq!(gba.bus.read32(IWRAM_BASE), 0x11);
    assert_eq!(gba.bus.read32(IWRAM_BASE + 4), 0x22);
}

#[test]
fn an_stm_stores_the_program_counter_as_its_own_address_plus_twelve() {
    // mov r0,mem / stmia r0,{r15} / mov r1,pc: the stored word is the address of the STM plus 12,
    // which is exactly what the next instruction's `mov r1, pc` reads.
    let gba = ran(
        &[
            0xE3A0_0403, // mov r0,#$03000000
            0xE880_8000, // stmia r0,{r15}
            0xE1A0_100F, // mov r1,pc
            0xE590_2000, // ldr r2,[r0]
            0xEAFF_FFFE, // b .
        ],
        4,
    );
    assert_eq!(gba.cpu.r[2], at(1) + 12, "the stored word");
    assert_eq!(gba.cpu.r[1], gba.cpu.r[2], "stored PC vs read PC");
}

#[test]
fn the_comparison_opcodes_never_write_their_rd_field() {
    // `cmp r0, r8`: the Rd bits are part of the encoding and must stay 0, so the check is that R0
    // survives and the flags are the comparison's. (A compare with Rd = 15 is the suite's "bad CMP"
    // of tests 234/235, which restores CPSR from SPSR without moving PC; `arm.gba` pins that.)
    let gba = ran(
        &[
            0xE3A0_0005, // mov r0,#5
            0xE3A0_8005, // mov r8,#5
            0xE150_0008, // cmp r0,r8
            0xE3A0_8001, // mov r8,#1
            0xEAFF_FFFE, // b .
        ],
        4,
    );
    assert_eq!(gba.cpu.r[0], 5, "cmp wrote its Rd");
    assert_eq!(gba.cpu.r[8], 1, "the instruction after the compare ran");
    assert_ne!(gba.cpu.cpsr & cpu::FLAG_Z, 0, "5 - 5 set Z");
    assert_eq!(
        gba.cpu.mode(),
        cpu::MODE_SYS,
        "no mode change: the documented boot mode is System, and these instructions never touch \
         the CPSR"
    );
    assert_eq!(gba.cpu.pc(), at(4));
}

#[test]
fn a_misaligned_halfword_load_rotates_the_aligned_word_and_a_signed_one_reads_a_byte() {
    let gba = ran(
        &[
            0xE3A0_0403, // mov r0,#$03000000
            0xE3E0_1000, // mvn r1,#0
            0xE580_1000, // str r1,[r0]        -> $FFFFFFFF everywhere
            0xE1D0_20B1, // ldrh r2,[r0,#1]
            0xE1D0_30F1, // ldrsh r3,[r0,#1]
            0xEAFF_FFFE, // b .
        ],
        5,
    );
    assert_eq!(gba.cpu.r[2], 0xFF00_00FF, "LDRH from an odd address");
    assert_eq!(
        gba.cpu.r[3], 0xFFFF_FFFF,
        "LDRSH from an odd address is LDRSB"
    );
}

#[test]
fn a_misaligned_halfword_store_is_forced_down_to_the_halfword_boundary() {
    let mut gba = ran(
        &[
            0xE3A0_0403, // mov r0,#$03000000
            0xE3E0_1000, // mvn r1,#0
            0xE1C0_10B1, // strh r1,[r0,#1]
            0xEAFF_FFFE, // b .
        ],
        3,
    );
    assert_eq!(
        gba.bus.read32(IWRAM_BASE),
        0x0000_FFFF,
        "written at the aligned address, not rotated into the upper half"
    );
}

#[test]
fn the_fiq_bank_of_r8_to_r12_survives_an_irq() {
    // Set the user's R8-R12, then FIQ's, then come back to SVC and raise an interrupt by hand. The
    // IRQ must not be able to see or touch FIQ's five registers.
    let mut gba = machine(&[
        0xE3A0_8011, // mov r8,#$11
        0xE3A0_9022, // mov r9,#$22
        0xE3A0_A033, // mov r10,#$33
        0xE3A0_B044, // mov r11,#$44
        0xE3A0_C055, // mov r12,#$55
        0xE321_F011, // msr cpsr_c,#MODE_FIQ
        0xE3A0_80AA, // mov r8,#$AA
        0xE3A0_90BB, // mov r9,#$BB
        0xE3A0_A0CC, // mov r10,#$CC
        0xE3A0_B0DD, // mov r11,#$DD
        0xE3A0_C0EE, // mov r12,#$EE
        0xE321_F013, // msr cpsr_c,#MODE_SVC    (IRQ and FIQ unmasked)
        0xE321_F011, // msr cpsr_c,#MODE_FIQ    <- the stub the CPU is sent to after the IRQ
        0xEAFF_FFFE, // b .
    ]);
    for _ in 0..12 {
        gba.step_instruction();
    }
    assert_eq!(gba.cpu.mode(), cpu::MODE_SVC);
    assert_eq!(gba.cpu.r[8], 0x11, "SVC sees the user's R8");

    // Raise VBlank: IE bit 0, IF bit 0, IME = 1. G4 owns the sources; the dispatch is the CPU's, and
    // this test raises the flag by hand. G2 made `IF` write-1-to-clear, as the hardware has it, so
    // the flag is set through the bus's own raise rather than by a store.
    gba.bus.write16(0x0400_0200, 1);
    gba.bus.raise_interrupt(1);
    gba.bus.write16(0x0400_0208, 1);
    let returned_to = gba.cpu.pc();
    gba.step_instruction();
    assert_eq!(gba.cpu.mode(), cpu::MODE_IRQ, "IRQ taken");
    assert_eq!(gba.cpu.pc(), cpu::VECTOR_IRQ);
    assert_eq!(gba.cpu.r[14], returned_to + 4, "LR_irq = return + 4");
    assert_eq!(
        gba.cpu.spsr[1] & 0x1F,
        cpu::MODE_SVC,
        "SPSR_irq holds the mode that was interrupted"
    );
    assert_ne!(gba.cpu.cpsr & cpu::FLAG_I, 0, "IRQs are masked on entry");
    assert_eq!(
        gba.cpu.r[8], 0x11,
        "IRQ mode has no bank of its own for R8-R12"
    );

    // Send the CPU to a stub that asks for FIQ's registers again.
    gba.cpu.r[15] = at(12);
    gba.step_instruction(); // msr cpsr_c,#MODE_FIQ
    assert_eq!(gba.cpu.mode(), cpu::MODE_FIQ);
    assert_eq!(
        [
            gba.cpu.r[8],
            gba.cpu.r[9],
            gba.cpu.r[10],
            gba.cpu.r[11],
            gba.cpu.r[12]
        ],
        [0xAA, 0xBB, 0xCC, 0xDD, 0xEE],
        "the FIQ bank survived the IRQ untouched"
    );
}

#[test]
fn msr_writes_the_current_modes_banked_spsr_and_mrs_reads_it_back() {
    let gba = ran(
        &[
            0xE321_F011, // msr cpsr_c,#MODE_FIQ
            0xE3A0_001F, // mov r0,#$1F
            0xE16F_F000, // msr spsr,r0
            0xE14F_1000, // mrs r1,spsr
            0xEAFF_FFFE, // b .
        ],
        4,
    );
    assert_eq!(gba.cpu.mode(), cpu::MODE_FIQ);
    assert_eq!(gba.cpu.r[1], 0x1F, "SPSR_fiq round-tripped");
    assert_eq!(gba.cpu.spsr[0], 0x1F);
}

#[test]
fn a_state_saved_in_the_fiq_bank_comes_back_in_it() {
    // The save state has to carry the banks, because G7 serialises this machine.
    let mut gba = machine(&[
        0xE3A0_8011, // mov r8,#$11
        0xE321_F011, // msr cpsr_c,#MODE_FIQ
        0xE3A0_80AA, // mov r8,#$AA
        0xEAFF_FFFE, // b .
    ]);
    for _ in 0..3 {
        gba.step_instruction();
    }
    let saved = gba.save_state();
    gba.cpu.r[8] = 0;
    gba.load_state(&saved).expect("our own state");
    assert_eq!(gba.cpu.mode(), cpu::MODE_FIQ);
    assert_eq!(gba.cpu.r[8], 0xAA, "R8_fiq is in the state");
    assert_eq!(gba.cpu.user_r8_r12[0], 0x11, "and so is the user's R8");
}

#[test]
fn an_instruction_costs_what_gbatek_says_it_costs() {
    // A plain instruction is 1S, a load 1S+1N+1I, a store 2N, a taken branch 2S+1N — three bus
    // cycles, not four. This is the cycle model the frame clock is charged. GBATEK's numbers are
    // the *core's* cycles; since G2 the memory's own wait states are added on top, so the
    // measurement is taken from IWRAM, which the bus answers in one cycle per access whatever
    // `WAITCNT` says (GBATEK's *Address Bus Width* table: "Work RAM 32K, 32 bit, 1/1/1").
    let mut gba = machine(&[
        0xE3A0_0001, // mov r0,#1        1
        0xE580_0000, // str r0,[r0]      2
        0xEAFF_FFFE, // b .              3
    ]);
    gba.bus.write32(IWRAM_BASE, 0xE3A0_0001);
    gba.bus.write32(IWRAM_BASE + 4, 0xE580_0000);
    gba.bus.write32(IWRAM_BASE + 8, 0xEAFF_FFFE);
    gba.cpu.r[15] = IWRAM_BASE;
    assert_eq!(gba.step_instruction(), 1);
    assert_eq!(gba.step_instruction(), 2);
    assert_eq!(gba.step_instruction(), 3);
    let spent = gba.bus.cycles;
    // The three steps above ran from IWRAM, so nothing was waiting on the Game Pak.
    assert_eq!(spent, 6);
    // And from the Game Pak the same instructions cost the ROM's wait states on top: the first
    // fetch is non-sequential (4 cycles at the post-boot `WAITCNT`), the rest are sequential
    // prefetched (1 each), which is 3 + 0 + 4 + 0 + 4 + 0 for the fetches and the store's own.
    let mut gba = machine(&[
        0xE3A0_0001, // mov r0,#1
        0xE580_0000, // str r0,[r0]
        0xEAFF_FFFE, // b .
    ]);
    let first = gba.step_instruction();
    assert!(
        first > 1,
        "a fetch from the Game Pak is not one cycle: {first}"
    );
}

#[test]
fn a_store_costs_the_memory_table_row_it_writes_to() {
    // GBATEK, *GBA Memory Map → Address Bus Width and CPU Read/Write Access Widths* prints the
    // cycles for one access of each width:
    //
    //   Work RAM 256K 16    8/16/32   8/16/32   3/3/6 **
    //   Work RAM 32K  32    8/16/32   8/16/32   1/1/1
    //
    // and *ARM CPU Instruction Cycle Times* says how an instruction's figure divides between code
    // and data: "1N is used for data access, plus (n-1)S for LDM/STM, the remaining S+N are code
    // access". So a store's data access is one transfer, and `STR 2N` is that transfer's two clock
    // cycles — the region's row, not the row plus two. This is the instruction the two cartridges'
    // work-RAM clear is made of, so its price is the price of their whole boot.
    let mut gba = machine(&[]);
    let code = [
        0xE3A0_0402, // mov r0,#$02000000  EWRAM
        0xE3A0_1401, // mov r1,#$01000000  (imm8 0x01 ror 8 — `0xE3A0_1001` loads 1, not $01000000)
        0xE580_1000, // str r1,[r0]        32-bit write: the row says 6
        0xE1C0_10B0, // strh r1,[r0]       16-bit write: 3
        0xE5C0_1000, // strb r1,[r0]        8-bit write: 3
        0xE3A0_0403, // mov r0,#$03000000  IWRAM
        0xE580_1000, // str r1,[r0]        "1/1/1": `2N` is two cycles
    ];
    for (index, word) in code.iter().enumerate() {
        gba.bus.write32(IWRAM_BASE + index as u32 * 4, *word);
    }
    gba.cpu.r[15] = IWRAM_BASE;
    assert_eq!(gba.step_instruction(), 1, "mov is 1S");
    assert_eq!(gba.step_instruction(), 1, "mov is 1S");
    assert_eq!(
        gba.step_instruction(),
        6,
        "a 32-bit store to Work RAM 256K is the table's 6 cycles"
    );
    assert_eq!(
        gba.step_instruction(),
        3,
        "a 16-bit store to Work RAM 256K is the table's 3 cycles"
    );
    assert_eq!(
        gba.step_instruction(),
        3,
        "an 8-bit store to Work RAM 256K is the table's 3 cycles"
    );
    assert_eq!(gba.step_instruction(), 1, "mov is 1S");
    assert_eq!(
        gba.step_instruction(),
        2,
        "a 32-bit store to Work RAM 32K is 2N: two cycles, one row"
    );
    assert_eq!(
        gba.bus.read32(0x0200_0000),
        0x0100_0000,
        "the stores landed"
    );
    assert_eq!(gba.bus.read32(IWRAM_BASE), 0x0100_0000);
}

#[test]
fn a_taken_branch_is_three_bus_cycles_and_a_condition_that_fails_is_one() {
    // GBATEK, *ARM CPU Instruction Cycle Times*: `B,BL 2S+1N` for a taken branch — in Thumb too,
    // and *THUMB.5* repeats the figure for `BX` — and `{cond} false 1S` for a conditional branch
    // whose condition fails. IWRAM answers every access in one cycle ("Work RAM 32K … 1/1/1"), so
    // the measurement is the core's own count: three taken, one not. Read from the Game Pak the
    // same branch adds the target's `N` wait states to the next fetch, never to this count.
    let mut gba = machine(&[]);
    // ARM, at `$03000100`: mov r0,#$03000000 / add r0,#1 / bx r0 — bit 0 picks Thumb and the
    // target is this same IWRAM, where the Thumb code below starts.
    gba.bus.write32(IWRAM_BASE + 0x100, 0xE3A0_0403);
    gba.bus.write32(IWRAM_BASE + 0x104, 0xE280_0001);
    gba.bus.write32(IWRAM_BASE + 0x108, 0xE12F_FF10);
    // Thumb: movs r0,#0 / beq +1 (Z set: taken, jumps the two `movs r0,#9`) / movs r0,#9 /
    // movs r0,#9 / movs r0,#0 (Z set) / bne +1 (Z set: not taken) / movs r0,#3 / b .
    // The Thumb condition field is not the ARM one: 0 is `beq`, 1 is `bne` (GBATEK, *THUMB.16*).
    let thumb: [u16; 8] = [
        0x2000, 0xD001, 0x2009, 0x2009, 0x2000, 0xD101, 0x2003, 0xE7FE,
    ];
    for (index, halfword) in thumb.iter().enumerate() {
        gba.bus.write16(IWRAM_BASE + index as u32 * 2, *halfword);
    }
    gba.cpu.r[15] = IWRAM_BASE + 0x100;
    assert_eq!(gba.step_instruction(), 1, "mov r0,#$03000000 is 1S");
    assert_eq!(gba.step_instruction(), 1, "add r0,#1 is 1S");
    assert_eq!(gba.step_instruction(), 3, "bx r0 is 2S+1N");
    // Thumb from here: the two branches are the measurement.
    assert_eq!(gba.step_instruction(), 1, "movs r0,#0 is 1S");
    assert_eq!(gba.step_instruction(), 3, "a taken beq is 2S+1N");
    assert_eq!(gba.cpu.r[0], 0, "the beq jumped the two movs it skips");
    assert_eq!(gba.step_instruction(), 1, "movs r0,#2 is 1S");
    assert_eq!(
        gba.step_instruction(),
        1,
        "a branch whose condition fails is 1S"
    );
    assert_eq!(gba.step_instruction(), 1, "movs r0,#3 is 1S");
    assert_eq!(gba.step_instruction(), 3, "b . is 2S+1N");
    assert_eq!(gba.cpu.r[0], 3);
}

#[test]
fn the_clock_is_the_one_the_core_counts_against() {
    // One emulated second is 16.78M cycles; the frame loop's counter and the LCD agree about it.
    assert_eq!(CLOCK_HZ, 16_777_216);
    let mut gba = machine(&[0xEAFF_FFFE]);
    for _ in 0..60 {
        gba.run_frame();
    }
    let cycles = gba.bus.cycles;
    let expected = 60 * u64::from(tendy_gba::bus::CYCLES_PER_FRAME);
    assert!(cycles >= expected, "{cycles} < {expected}");
    assert!(
        cycles < expected + 60 * 8,
        "sixty frames cost {cycles}, {} over",
        cycles - expected
    );
}

/// A machine whose Thumb code is one instruction at `$08000100`, followed by `b .`. Running it is
/// one `step_instruction` with `R15` there — the same direct-`R15` start `an_instruction_costs_what_
/// gbatek_says_it_costs` uses above.
fn thumb_at_a_hundred(instruction: u16) -> Gba {
    let mut code = vec![0u32; 17];
    let at_100 = (0x100 - 0xC0) / 4;
    code[at_100] = 0xE7FE_0000 | u32::from(instruction);
    let mut gba = machine(&code);
    gba.cpu.set_thumb(true);
    gba.cpu.r[15] = 0x0800_0100;
    gba
}

#[test]
fn an_ldm_at_an_odd_base_reads_the_word_boundary_below_it() {
    // mov r0,#$03000000 / add r0,r0,#1 / ldmia r0,{r1,r2}. GBATEK, *Mis-aligned
    // STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: "The mis-aligned low bit(s) are ignored, the memory
    // access goes to a forcibly aligned (rounded-down) memory address". So the base $03000001 loads
    // the word at $03000000 into R1 and the one at $03000004 into R2 — the two words at or above
    // the rounded-down base, not the four bytes starting at $…01.
    let mut gba = machine(&[
        0xE3A0_0403, // mov r0,#$03000000
        0xE280_0001, // add r0,r0,#1
        0xE890_0006, // ldmia r0,{r1,r2}
        0xEAFF_FFFE, // b .
    ]);
    gba.bus.write32(IWRAM_BASE, 0xAAAA_AAAA);
    gba.bus.write32(IWRAM_BASE + 4, 0xBBBB_BBBB);
    for _ in 0..3 {
        gba.step_instruction();
    }
    assert_eq!(
        gba.cpu.r[0],
        IWRAM_BASE + 1,
        "no write-back, the base stays odd"
    );
    assert_eq!(gba.cpu.r[1], 0xAAAA_AAAA, "the word at $03000000");
    assert_eq!(gba.cpu.r[2], 0xBBBB_BBBB, "the word at $03000004");
}

#[test]
fn a_thumb_ldmia_at_an_odd_base_reads_the_word_boundary_below_it() {
    // ldmia r0!,{r1,r2} with r0 = $03000001 — the same rule through the Thumb block transfer. The
    // two words come from $03000000 and $03000004, and the write-back is the register plus two
    // words, so r0 ends at $03000001 + 8 = $03000009, not at an aligned $03000008.
    let mut gba = thumb_at_a_hundred(0xC806);
    gba.bus.write32(IWRAM_BASE, 0xAAAA_AAAA);
    gba.bus.write32(IWRAM_BASE + 4, 0xBBBB_BBBB);
    gba.cpu.r[0] = IWRAM_BASE + 1;
    gba.step_instruction();
    assert_eq!(gba.cpu.r[1], 0xAAAA_AAAA);
    assert_eq!(gba.cpu.r[2], 0xBBBB_BBBB);
    assert_eq!(gba.cpu.r[0], IWRAM_BASE + 0x9, "odd base plus eight bytes");
}

#[test]
fn a_thumb_pop_from_an_odd_stack_pointer_reads_the_boundary_below_it() {
    // pop {r0,r1} with SP = $03000101. GBATEK's mis-aligned rule rounds the access down to
    // $03000100, and the walk still moves four bytes per word, so R0 = the word at $03000100 and
    // R1 = the word at $03000104. The stack pointer's own update is the architectural one — one
    // word per register off the value it started at — so SP = $03000101 + 8 = $03000109.
    let mut gba = thumb_at_a_hundred(0xBC03);
    gba.bus.write32(IWRAM_BASE + 0x100, 0x1111_1111);
    gba.bus.write32(IWRAM_BASE + 0x104, 0x2222_2222);
    gba.cpu.r[13] = IWRAM_BASE + 0x101;
    gba.step_instruction();
    assert_eq!(gba.cpu.r[0], 0x1111_1111, "the word at the aligned base");
    assert_eq!(gba.cpu.r[1], 0x2222_2222, "the word four bytes after it");
    assert_eq!(gba.cpu.r[13], IWRAM_BASE + 0x109, "SP plus two words");
}

#[test]
fn a_thumb_push_from_an_odd_stack_pointer_stores_on_the_boundary_below_it() {
    // push {r0,r1} with SP = $03000101: two words go to SP-8 = $030000F9, which the store rounds
    // down to $030000F8, and the next word to $030000FC. The stack pointer drops by the two words
    // the instruction names, from the odd value it held: $03000101 - 8 = $030000F9.
    let mut gba = thumb_at_a_hundred(0xB403);
    gba.cpu.r[0] = 0x1111_1111;
    gba.cpu.r[1] = 0x2222_2222;
    gba.cpu.r[13] = IWRAM_BASE + 0x101;
    gba.step_instruction();
    assert_eq!(
        gba.bus.read32(IWRAM_BASE + 0xF8),
        0x1111_1111,
        "R0 at the word boundary below the odd store address"
    );
    assert_eq!(gba.bus.read32(IWRAM_BASE + 0xFC), 0x2222_2222);
    assert_eq!(gba.cpu.r[13], IWRAM_BASE + 0xF9, "SP minus two words");
}

/// A machine running Thumb from `$03000000` — IWRAM, where every access is one cycle ("Work RAM 32K
/// … 1/1/1"), so `step_instruction` is the core's own count with no wait states added.
fn thumb_in_wram(halfwords: &[u16]) -> Gba {
    let mut gba = machine(&[]);
    for (index, halfword) in halfwords.iter().enumerate() {
        gba.bus.write16(IWRAM_BASE + index as u32 * 2, *halfword);
    }
    gba.cpu.set_thumb(true);
    gba.cpu.r[15] = IWRAM_BASE;
    gba
}

#[test]
fn a_thumb_pop_costs_the_documented_s_and_n_and_i() {
    // GBATEK, *THUMB.14*: "Execution Time: nS+1N+1I (POP), (n+1)S+2N+1I (POP PC), or (n-1)S+2N
    // (PUSH)." Two registers is n=2, so 2S+1N+1I is four cycles — the same figure its `LDMIA`
    // (`nS+1N+1I`, *THUMB.15*) charges for the same two words. This is every Thumb function
    // return. The code and the stack are both in IWRAM, so nothing is added on top.
    let mut gba = thumb_in_wram(&[0xBC03, 0xE7FE]); // pop {r0,r1} / b .
    gba.bus.write32(IWRAM_BASE + 0x100, 0x1111_1111);
    gba.bus.write32(IWRAM_BASE + 0x104, 0x2222_2222);
    gba.cpu.r[13] = IWRAM_BASE + 0x100;
    assert_eq!(
        gba.step_instruction(),
        4,
        "POP {{r0,r1}} is nS+1N+1I = 4 cycles for two words"
    );
    assert_eq!(gba.cpu.r[0], 0x1111_1111, "the first word loaded into R0");
    assert_eq!(gba.cpu.r[1], 0x2222_2222, "the second into R1");
    assert_eq!(gba.cpu.r[13], IWRAM_BASE + 0x108, "SP past the two words");
}

#[test]
fn a_thumb_pop_of_pc_costs_the_documented_s_and_two_n_and_i() {
    // GBATEK, *THUMB.14*: "(n+1)S+2N+1I (POP PC)", and *Jumps and Calls* repeats
    // `POP {Rlist,}PC (n+1)S+2N+1I` (G:92923). `n` counts the words popped *including* PC, so
    // `pop {r0,pc}` is (2+1)S+2N+1I = six cycles — the same `n+4` that the ARM `LDM PC` beside it
    // charges. GBATEK, *THUMB.14*: the LSB of the loaded PC is not used here ("remains in thumb
    // state even if bit0 was cleared"), so the target is masked, not an ARM switch.
    let mut gba = thumb_in_wram(&[0xBD01, 0xE7FE]); // pop {r0,pc} / b .
    gba.bus.write32(IWRAM_BASE + 0x100, 0x1111_1111);
    gba.bus.write32(IWRAM_BASE + 0x104, IWRAM_BASE + 0x21);
    gba.cpu.r[13] = IWRAM_BASE + 0x100;
    assert_eq!(
        gba.step_instruction(),
        6,
        "POP {{r0,PC}} is (n+1)S+2N+1I = 6 cycles for two words"
    );
    assert_eq!(gba.cpu.r[0], 0x1111_1111, "the low register still loads");
    assert_eq!(
        gba.cpu.r[15],
        IWRAM_BASE + 0x20,
        "PC is the popped word with bit 0 dropped, and the core stays in Thumb"
    );
    assert!(gba.cpu.thumb(), "a Thumb POP of PC does not switch state");
    assert_eq!(gba.cpu.r[13], IWRAM_BASE + 0x108, "SP past the two words");
}

#[test]
fn a_thumb_mul_costs_its_s_plus_the_m_of_its_multiplier() {
    // GBATEK, *THUMB.4*: "1S+mI for MUL on ARMv4 (m=1..4; depending on MSBs of incoming Rd
    // value)", and the summary's `MUL{S} Rd,Rs 1S+mI` (G:92909): one cycle for the instruction
    // plus the multiplier's significant bytes — `1 + m`, not `m`. `mul r0,r1` with R0 at $FF,
    // $1234, $123456 and $12345678 is m=1..4, so 2, 3, 4 and 5 cycles.
    let mut gba = thumb_in_wram(&[0x4348, 0x4348, 0x4348, 0x4348, 0xE7FE]); // mul r0,r1 ×4 / b .
    gba.cpu.r[1] = 3;
    for (rd, m) in [
        (0xFFu32, 1u32),
        (0x1234, 2),
        (0x12_3456, 3),
        (0x1234_5678, 4),
    ] {
        gba.cpu.r[0] = rd;
        assert_eq!(
            gba.step_instruction(),
            1 + m,
            "MUL with m={m} is 1S+{m}I = {} cycles",
            1 + m
        );
        assert_eq!(gba.cpu.r[0], rd.wrapping_mul(3), "Rd = Rd * Rs");
    }
}

#[test]
fn the_undefined_instruction_trap_costs_the_documented_four_cycles() {
    // GBATEK, *Undefined Instruction*: "Execution time: 2S+1I+1N." — four cycles. Both instruction
    // sets reach the trap through one function, and each caller returns that figure unchanged.
    // R14_und is the address of the next instruction, R15 the vector at `$04`.
    let mut gba = thumb_in_wram(&[0xDE00, 0xE7FE]); // undefined (cond 1111) / b .
    assert_eq!(
        gba.step_instruction(),
        4,
        "the trap is 2S+1I+1N = 4 cycles, not 5"
    );
    assert_eq!(gba.cpu.mode(), cpu::MODE_UND, "Undefined mode");
    assert_eq!(
        gba.cpu.r[14],
        IWRAM_BASE + 2,
        "R14_und is the instruction after the undefined one"
    );
    assert_eq!(gba.cpu.r[15], cpu::VECTOR_UND, "PC is the undefined vector");
    // The ARM decoder enters the same function: `LDC/STC` with no coprocessor to hand it to.
    let mut gba = machine(&[]);
    gba.bus.write32(IWRAM_BASE, 0xEC00_0010);
    gba.cpu.set_thumb(false);
    gba.cpu.r[15] = IWRAM_BASE;
    assert_eq!(
        gba.step_instruction(),
        4,
        "the ARM path charges the same 2S+1I+1N"
    );
    assert_eq!(gba.cpu.mode(), cpu::MODE_UND);
    assert_eq!(gba.cpu.r[14], IWRAM_BASE + 4, "next ARM instruction");
}

#[test]
fn a_software_interrupt_costs_the_documented_instruction_and_the_four_word_save() {
    // GBATEK, *Software Interrupt (SWI/BKPT)*: "Execution Time: 2S+1N" — three bus cycles for the
    // instruction, and *THUMB.17* prints the same figure for the Thumb encoding, so one charge in
    // the shared `software_interrupt` covers both decoders.
    //
    // GBATEK, *How BIOS Processes SWIs*: "Each time when calling a BIOS function 4 words (SPSR,
    // R11, R12, R14) are saved on Supervisor stack (_svc)". The boot sequence leaves `SP_svc` at
    // `$03007FE0` ("SP_svc=03007FE0h", *Default memory usage at 03007FXX*) in the arena *GBA BIOS
    // RAM Usage* prints as "3007FA0h 40h   Default Supervisor Stack (4 words/time)
    // (sp_svc=03007FE0h)", so the frame is `$03007FD0-$03007FDF` and the `SPSR` word is the top one,
    // at `$03007FDC-$03007FDF`. Four stores are `STR 2N` each — eight cycles — and the region they
    // land in is IWRAM, whose table row is `1/1/1`, so the bus adds nothing on top.
    //
    // The function behind the `SWI` is `Halt` (`$02`), whose own four cycles are a figure this
    // project chose (GBATEK prints no execution time for it), so the total is 3 + 8 + 4.
    let mut gba = thumb_in_wram(&[0xDF02, 0xE7FE]); // swi 2 / b .
    gba.cpu.r[11] = 0x1111_1111;
    gba.cpu.r[12] = 0x2222_2222;
    let caller_cpsr = gba.cpu.cpsr;
    let sp = cpu::SP_SVC;
    assert_eq!(
        gba.step_instruction(),
        15,
        "SWI is 2S+1N (3) + four IWRAM word stores at 2N (8) + Halt's own 4"
    );
    for (index, (value, what)) in [
        (0x1111_1111u32, "R11"),
        (0x2222_2222, "R12"),
        (
            IWRAM_BASE + 2,
            "R14: the return address, the instruction after the SWI",
        ),
        (caller_cpsr, "SPSR: the caller's CPSR"),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            gba.bus.read32(sp - 16 + index as u32 * 4),
            value,
            "word {index} of the Supervisor frame is {what}"
        );
    }
    assert_eq!(
        gba.cpu.bank_sp_lr[cpu::BANK_SVC][0],
        sp,
        "SP_svc is left where it was: the handler that saves is the one that restores"
    );
    // The ARM encoding charges the same instruction figure; `swi 20000h` is the same call.
    let mut gba = machine(&[]);
    gba.bus.write32(IWRAM_BASE, 0xEF02_0000); // swi 20000h
    gba.cpu.r[15] = IWRAM_BASE;
    assert_eq!(
        gba.step_instruction(),
        15,
        "the ARM SWI charges the same 2S+1N and the same four-word save"
    );
}

#[test]
fn the_supervisor_save_is_priced_by_the_region_the_stack_sits_in() {
    // The save is four 32-bit stores and their price comes from the memory table, not from a
    // constant: "The user may redefine these addresses and move stacks into other locations"
    // (GBATEK, *Default memory usage at 03007FXX*). With `SP_svc` moved into on-board work RAM the
    // same four words cost the `Work RAM 256K 16    8/16/32   8/16/32   3/3/6 **` row — six cycles a
    // word — so the `SWI` is 3 + 24 + Halt's 4 = 31, not 15.
    let mut gba = thumb_in_wram(&[0xDF02, 0xE7FE]); // swi 2 / b .
    let sp = 0x0200_0100; // EWRAM
    gba.cpu.bank_sp_lr[cpu::BANK_SVC][0] = sp;
    gba.cpu.r[11] = 0x1111_1111;
    assert_eq!(
        gba.step_instruction(),
        31,
        "the four stores are EWRAM's 6 cycles each, so 3 + 24 + 4"
    );
    assert_eq!(
        gba.bus.read32(sp - 16),
        0x1111_1111,
        "the frame landed in EWRAM"
    );
}
