//! The CGB's VRAM DMA: `GDMA` and `HDMA` at `$FF51-$FF55` (`docs/PLAN-gbc-gba.md` slice C3).
//!
//! One test per rule, named after the rule, each one failing if the feature were
//! removed. Pan Docs (*CGB Registers*, "LCD VRAM DMA Transfers") is the source for every rule here;
//! the comments quote the sentence the assertion comes from. Nothing from another emulator is used:
//! every transfer here moves bytes this file put in WRAM.

mod common;

use tendy_gb::cgb::Stop;
use tendy_gb::machine::Machine;
use tendy_gb::ppu::Mode;
use tendy_gb::vdma::{HDMA1, HDMA2, HDMA3, HDMA4, HDMA5};
use tendy_gb::{Cartridge, DOTS_PER_LINE, LINES_PER_FRAME};

/// `$0143` for a cartridge that only runs on a CGB.
const CGB_ONLY: u8 = 0xC0;
/// `$0143` for a plain DMG cartridge.
const DMG: u8 = 0x00;
/// Machine cycles one block of sixteen bytes takes at normal speed: "8 M-cycles in Normal Speed
/// Mode" (Pan Docs, "Transfer Timings").
const BLOCK_CYCLES: u64 = 8;

/// A ROM of `code` at `$0150` with the CGB flag and a matching header checksum — the shape
/// `tests/cgb.rs` uses.
fn rom(flag: u8, code: &[u8]) -> Vec<u8> {
    let mut rom = common::synth_rom(&common::entry(code));
    rom[0x0143] = flag;
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x014D] = x;
    rom
}

fn machine(flag: u8, code: &[u8]) -> Machine {
    Machine::new(Cartridge::new(rom(flag, code)).expect("synthesized ROM"))
}

/// A machine for `flag` running `code` from WRAM at `$C000`, as `tests/cgb.rs` does.
fn harness(flag: u8, code: &[u8]) -> Machine {
    let mut m = machine(flag, &[]);
    m.cpu.pc = 0xC000;
    m.bus.wram[..code.len()].copy_from_slice(code);
    m
}

/// The four `$FF51-$FF54` writes and then the `$FF55` byte, each one a machine cycle: what a
/// cartridge does to start a transfer.
fn start(m: &mut Machine, source: u16, dest: u16, value: u8) {
    m.bus.write(HDMA1, (source >> 8) as u8);
    m.bus.write(HDMA2, source as u8);
    m.bus.write(HDMA3, (dest >> 8) as u8);
    m.bus.write(HDMA4, dest as u8);
    m.bus.write(HDMA5, value);
}

/// `cycles` machine cycles with the CPU left out of it: the PPU still runs, so HBlanks still come.
fn run_cycles(m: &mut Machine, cycles: u64) {
    for _ in 0..cycles {
        m.bus.tick();
    }
}

/// The machine's dot clock, as one number: dots since power-on. A transfer's cost in PPU time is a
/// difference of two of these.
fn dot_clock(m: &Machine) -> u64 {
    let ppu = &m.bus.ppu;
    (ppu.frames * LINES_PER_FRAME as u64 + ppu.ly as u64) * DOTS_PER_LINE as u64 + ppu.dot as u64
}

/// Fill `count` bytes of WRAM at `$C000 + at` with `base`, `base + 1`, ...
fn fill_wram(m: &mut Machine, at: usize, count: usize, base: u8) {
    for i in 0..count {
        m.bus.wram[at + i] = base.wrapping_add(i as u8);
    }
}

/// Pan Docs, `$FF51-$FF52`: "The four lower bits of this address will be ignored and treated as 0."
#[test]
fn the_source_address_ignores_its_low_four_bits() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0x0100, 0x20, 0xA0);
    // $C10F must read from $C100: the block at $C110 is a different one and must not be touched.
    start(&mut m, 0xC10F, 0x8000, 0x00);
    run_cycles(&mut m, BLOCK_CYCLES);
    let expected: Vec<u8> = (0..16u8).map(|i| 0xA0u8.wrapping_add(i)).collect();
    assert_eq!(
        &m.bus.ppu.vram[..16],
        &expected[..],
        "the aligned block moved"
    );
    assert!(!m.bus.vdma.active, "one block, and the transfer is done");
    // The four address registers are write-only: they read $FF whatever they hold.
    for addr in [HDMA1, HDMA2, HDMA3, HDMA4] {
        assert_eq!(m.peek(addr), 0xFF, "{addr:#06X} is write-only");
    }
}

/// Pan Docs, `$FF53-$FF54`: "Only bits 12-4 are respected; others are ignored", from the `$8000`
/// base of VRAM.
#[test]
fn the_destination_address_is_masked_into_vram() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 16, 0x40);
    // $FFF5 is not a VRAM address at all; $FFF5 & $1FF0 is $1FF0, so the block lands at $9FF0.
    start(&mut m, 0xC000, 0xFFF5, 0x00);
    run_cycles(&mut m, BLOCK_CYCLES);
    let expected: Vec<u8> = (0..16u8).map(|i| 0x40u8.wrapping_add(i)).collect();
    assert_eq!(&m.bus.ppu.vram[0x1FF0..], &expected[..]);
    assert!(
        m.bus.ppu.vram[..0x1FF0].iter().all(|&b| b == 0),
        "nothing landed anywhere else in VRAM"
    );
}

/// Pan Docs, `$FF55`: the low seven bits are "the Transfer Length (divided by `$10`, minus 1), that
/// is, lengths of `$10-$800` bytes ... by the values `$00-$7F`".
#[test]
fn the_length_field_counts_blocks_minus_one() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x800, 0x10);
    // $04 means five blocks: 80 bytes. $00 would mean one; the $7F maximum comes next.
    start(&mut m, 0xC000, 0x8000, 0x04);
    assert!(m.bus.vdma.active);
    // Five blocks, less the two bytes the `$FF55` write's own cycle already moved.
    assert_eq!(m.bus.vdma.bytes_left(), 80 - 2);
    run_cycles(&mut m, 5 * BLOCK_CYCLES);
    assert!(!m.bus.vdma.active);
    assert_eq!(m.bus.vdma.blocks, 5);
    assert_eq!(&m.bus.ppu.vram[..0x50], &m.bus.wram[..0x50]);
    assert_eq!(m.peek(HDMA5), 0xFF, "a completed transfer reads $FF");
    assert!(
        m.bus.ppu.vram[0x50..0x100].iter().all(|&b| b == 0),
        "not one byte past the five blocks"
    );

    // "$7F" is 128 blocks, 2 048 bytes: one whole frame's worth of VBlank at 2 bytes a µs.
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x800, 0x00);
    start(&mut m, 0xC000, 0x8000, 0x7F);
    assert_eq!(m.bus.vdma.bytes_left(), 0x800 - 2);
    run_cycles(&mut m, 128 * BLOCK_CYCLES);
    assert_eq!(m.bus.vdma.blocks, 128);
    assert_eq!(&m.bus.ppu.vram[..0x800], &m.bus.wram[..0x800]);
}

/// Pan Docs, "Bit 7 = 0 — General-Purpose DMA": "The execution of the program is halted until the
/// transfer has completed", and "Transfer Timings": "8 M-cycles in Normal Speed Mode". The CPU
/// resumes at the dot the last block ends on.
#[test]
fn a_general_purpose_transfer_halts_the_cpu_for_eight_cycles_a_block() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x40, 0x00);
    m.bus.write(HDMA1, 0xC0);
    m.bus.write(HDMA2, 0x00);
    m.bus.write(HDMA3, 0x80);
    m.bus.write(HDMA4, 0x00);
    let cycles = m.bus.cycles;
    let dots = dot_clock(&m);
    let pc = m.cpu.pc;
    m.bus.write(HDMA5, 0x03); // four blocks, 64 bytes, general purpose

    let mut halted = 0;
    while m.bus.vdma.active {
        halted += 1;
        assert_eq!(m.step_instruction(), 1, "a halted step is one M-cycle");
        assert_eq!(
            m.cpu.pc, pc,
            "no instruction is fetched while a block moves"
        );
    }
    // Four blocks at 8 M-cycles each, the `$FF55` write's own cycle included.
    assert_eq!(m.bus.cycles - cycles, 4 * BLOCK_CYCLES);
    assert_eq!(
        dot_clock(&m) - dots,
        4 * BLOCK_CYCLES * 4,
        "32 dots a block"
    );
    assert_eq!(
        halted,
        4 * BLOCK_CYCLES - 1,
        "the write spent the first cycle"
    );
    assert_eq!(&m.bus.ppu.vram[..0x40], &m.bus.wram[..0x40]);
    // The CPU carries on: the next step executes an instruction (the ROM's first byte is a NOP).
    let pc = m.cpu.pc;
    m.step_instruction();
    assert_ne!(
        m.cpu.pc, pc,
        "the CPU fetches again once the last block is in"
    );
}

/// Pan Docs, "Bit 7 = 1 — HBlank DMA": "transfers $10 bytes of data during each HBlank, that is, at
/// LY=0-143, no data is transferred during VBlank (LY=144-153)". One block a line, and none while
/// the PPU is in modes 2 or 3.
#[test]
fn hblank_dma_takes_exactly_one_block_per_hblank_and_none_while_drawing() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x800, 0x00);
    start(&mut m, 0xC000, 0x8000, 0xFF); // HBlank mode, 128 blocks: fewer than a frame's lines
    let mut per_line = [0i32; LINES_PER_FRAME as usize];
    let mut line = m.bus.ppu.ly as usize;
    let mut seen = 0u64;
    while m.bus.ppu.frames == 0 {
        let before = m.bus.vdma.blocks;
        m.bus.tick();
        if m.bus.ppu.mode == Mode::Drawing {
            assert_eq!(m.bus.vdma.blocks, before, "no block while the PPU draws");
        }
        if m.bus.ppu.ly as usize != line {
            per_line[line] = (m.bus.vdma.blocks - seen) as i32;
            seen = m.bus.vdma.blocks;
            line = m.bus.ppu.ly as usize;
        }
    }
    for (ly, count) in per_line.iter().enumerate().take(128) {
        assert_eq!(*count, 1, "line {ly} took exactly one block");
    }
    for (ly, count) in per_line.iter().enumerate().skip(128) {
        assert_eq!(*count, 0, "line {ly} took none: the transfer was over");
    }
    assert_eq!(m.bus.vdma.blocks, 128);
    assert_eq!(
        m.bus.vdma.hblank_blocks, 128,
        "every one was an HBlank block"
    );
    assert_eq!(&m.bus.ppu.vram[..0x800], &m.bus.wram[..0x800]);
    assert_eq!(m.peek(HDMA5), 0xFF);
}

/// Pan Docs, "Bit 7 = 1 — HBlank DMA" says the transfer waits for HBlanks; with the LCD off no
/// HBlank ever comes, and hardware runs the transfer at once. This is the shortcut C3 implements.
#[test]
fn an_hblank_transfer_started_with_the_lcd_off_runs_at_once() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x40, 0x80);
    m.bus.write(0xFF40, 0x00); // LCDC.7 = 0: the PPU stops and no mode 0 follows a mode 3
    assert_eq!(m.peek(0xFF40) & 0x80, 0, "the LCD is off");
    start(&mut m, 0xC000, 0x8000, 0x83); // bit 7 set: HBlank mode, four blocks
    assert!(m.bus.vdma.active);
    run_cycles(&mut m, 4 * BLOCK_CYCLES);
    assert!(!m.bus.vdma.active, "it ran without waiting for a line");
    assert_eq!(m.bus.vdma.blocks, 4);
    assert_eq!(&m.bus.ppu.vram[..0x40], &m.bus.wram[..0x40]);
    assert_eq!(m.peek(HDMA5), 0xFF);
}

/// Pan Docs, `$FF55`: "It is also possible to terminate an active HBlank transfer by writing zero
/// to Bit 7 of FF55. In that case reading from FF55 will return how many `$10` 'blocks' remained
/// (minus 1) in the lower 7 bits, but Bit 7 will be read as '1'."
#[test]
fn writing_ff55_with_bit_seven_clear_stops_an_hblank_transfer() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x80, 0x80);
    start(&mut m, 0xC000, 0x8000, 0x87); // eight blocks
    while m.bus.vdma.blocks < 2 {
        m.bus.tick();
    }
    let done = m.bus.vdma.blocks;
    assert!(m.bus.vdma.active);
    m.bus.write(HDMA5, 0x00); // bit 7 clear: stop
    assert!(!m.bus.vdma.active, "the transfer stopped");
    let value = m.peek(HDMA5);
    assert_eq!(value & 0x80, 0x80, "a stopped transfer reads bit 7 set");
    assert_eq!(
        value & 0x7F,
        (8 - done - 1) as u8,
        "the blocks left, minus one"
    );
    // Nothing more moves, however many lines go by.
    let vram: Vec<u8> = m.bus.ppu.vram.to_vec();
    run_cycles(
        &mut m,
        2 * LINES_PER_FRAME as u64 * DOTS_PER_LINE as u64 / 4,
    );
    assert_eq!(m.bus.vdma.blocks, done);
    assert_eq!(
        m.bus.ppu.vram,
        vram.as_slice()[..],
        "not one byte past the stop"
    );
}

/// Pan Docs, *Confirming if the DMA Transfer is Active*: "Reading Bit 7 of FF55 ... (1=Not Active,
/// 0=Active)", the remaining length while it runs, and `$FF` once it has completed.
#[test]
fn ff55_reports_the_transfer_it_is_running() {
    let mut m = machine(CGB_ONLY, &[]);
    assert_eq!(m.peek(HDMA5), 0xFF, "post-boot: nothing has run");
    fill_wram(&mut m, 0, 0x80, 0x00);
    start(&mut m, 0xC000, 0x8000, 0x83); // HBlank mode, four blocks
    assert_eq!(
        m.peek(HDMA5),
        0x03,
        "four blocks left, minus one, bit 7 clear"
    );
    while m.bus.vdma.blocks < 1 {
        m.bus.tick();
    }
    assert_eq!(m.peek(HDMA5), 0x02, "three blocks left, minus one");
    while m.bus.vdma.active {
        m.bus.tick();
    }
    assert_eq!(m.peek(HDMA5), 0xFF, "a completed transfer reads $FF");

    // A general-purpose transfer reports the same field; only a `peek` can read it, because the
    // CPU is halted for the whole of it.
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x40, 0x00);
    start(&mut m, 0xC000, 0x8000, 0x01); // two blocks
    assert_eq!(m.peek(HDMA5) & 0x80, 0x00, "bit 7 clear while it runs");
    assert_eq!(m.peek(HDMA5) & 0x7F, 0x01);
}

/// Pan Docs, *VRAM Banks*: "Only bit 0 matters", and the transfer writes the bank `VBK` selects —
/// "the program should not change the Destination VRAM bank (FF4F) ... until the transfer has
/// completed".
#[test]
fn vbk_picks_the_bank_a_block_lands_in() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x20, 0x30);
    m.bus.write(0xFF4F, 0x01); // VBK = bank 1
    start(&mut m, 0xC000, 0x9000, 0x01); // two blocks into $9000
    run_cycles(&mut m, 2 * BLOCK_CYCLES);
    assert_eq!(&m.bus.ppu.vram1[0x1000..0x1020], &m.bus.wram[..0x20]);
    assert!(
        m.bus.ppu.vram[0x1000..0x1020].iter().all(|&b| b == 0),
        "bank 0 was not written"
    );
}

/// Pan Docs, *FF4D — KEY1*: the speed switch runs through the pause, and C1's contract is that a
/// transfer in flight refuses it rather than letting the two disagree about time.
#[test]
fn the_speed_switch_is_refused_while_an_hblank_transfer_is_running() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x80, 0x00);
    start(&mut m, 0xC000, 0x8000, 0x87);
    assert!(m.bus.vdma.active);
    m.bus.write(0xFF4D, 0x01); // KEY1 bit 0: the switch is armed
    assert_eq!(m.bus.stop(), Stop::Refused);
    assert_eq!(m.cgb_speed(), 1, "still at normal speed");
    // The same request once the transfer is over is honoured.
    while m.bus.vdma.active {
        m.bus.tick();
    }
    m.bus.write(0xFF4D, 0x01);
    assert_eq!(m.bus.stop(), Stop::Switching);
}

/// Pan Docs' `$FF55` starts a transfer and `$FF51-$FF54` are its registers; a general-purpose
/// transfer has the CPU in halt, so nothing can reach them, and a write that does is ignored
/// rather than allowed to shorten the transfer in flight.
#[test]
fn a_write_to_ff55_during_a_general_purpose_transfer_is_ignored() {
    let mut m = machine(CGB_ONLY, &[]);
    fill_wram(&mut m, 0, 0x40, 0x5A);
    start(&mut m, 0xC000, 0x8000, 0x03); // four blocks
    let reported = m.peek(HDMA5);
    assert!(m.bus.vdma.active);
    m.bus.write(HDMA5, 0x00); // a stop, if a general-purpose transfer could be stopped
    assert!(m.bus.vdma.active, "it cannot be cancelled");
    assert_eq!(m.peek(HDMA5), reported, "the length is unchanged");
    while m.bus.vdma.active {
        m.step_instruction();
    }
    assert_eq!(
        &m.bus.ppu.vram[..0x40],
        &m.bus.wram[..0x40],
        "all four blocks moved"
    );
}

/// Pan Docs, *CGB Registers*: "Upon halting the CPU (using the halt instruction), the transfer will
/// also be halted and will be resumed only when the CPU resumes execution".
#[test]
fn an_hblank_transfer_is_held_while_the_cpu_is_halted() {
    // HALT at $C000, then NOPs: with IE clear nothing wakes the CPU, so it stays halted.
    let mut m = harness(CGB_ONLY, &[0x76]);
    fill_wram(&mut m, 0x800, 0x200, 0x00);
    start(&mut m, 0xC800, 0x8000, 0xFF);
    assert!(m.bus.vdma.active);
    let mut frames = 0;
    while frames < 2 {
        m.cpu.step(&mut m.bus);
        if m.bus.ppu.take_frame() {
            frames += 1;
        }
    }
    assert!(m.cpu.halted, "the CPU is in HALT");
    assert_eq!(m.bus.vdma.blocks, 0, "a halted CPU holds the transfer");
    // An interrupt that wakes the CPU lets the blocks through again.
    m.cpu.halted = false;
    let mut frames = 0;
    while frames < 2 {
        m.cpu.step(&mut m.bus);
        if m.bus.ppu.take_frame() {
            frames += 1;
        }
    }
    assert!(m.bus.vdma.blocks >= 128, "two more frames, a block a line");
}

/// "Nothing else changes": a DMG cartridge has no VRAM DMA. `$FF51-$FF55` read `$FF` and
/// their writes are dropped, exactly as Pan Docs says a non-CGB is "operating in monochrome 'Non
/// CGB' compatibility mode".
#[test]
fn a_dmg_cartridge_has_no_vram_dma_registers() {
    let mut m = machine(DMG, &[]);
    fill_wram(&mut m, 0, 0x40, 0x00);
    for addr in [HDMA1, HDMA2, HDMA3, HDMA4, HDMA5] {
        assert_eq!(m.peek(addr), 0xFF, "{addr:#06X} reads $FF");
        m.bus.write(addr, 0x12);
        assert_eq!(m.peek(addr), 0xFF, "{addr:#06X} stays $FF");
    }
    // The write that would have started a transfer did nothing at all.
    start(&mut m, 0xC000, 0x8000, 0x00);
    run_cycles(&mut m, 2 * BLOCK_CYCLES);
    assert!(!m.bus.vdma.active);
    assert_eq!(m.bus.vdma.blocks, 0);
    assert!(m.bus.ppu.vram[..16].iter().all(|&b| b == 0));
}
