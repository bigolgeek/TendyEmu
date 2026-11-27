//! G4, the Game Boy Advance's timing hardware: the interrupt controller, `HALT`, the timers, DMA,
//! and the display events that drive them.
//!
//! Everything here is built from GBATEK (*GBA Interrupt Control*, *GBA Timers*, *GBA DMA Transfers*,
//! *HALTCNT*, *LCD Dimensions and Timings*) and from cartridges synthesized in code, so no test needs
//! a ROM file. The one test that does is a headless `--frames` run of a commercial cartridge —
//! not a unit test, because a ROM is never committed.

use tendy_gb::system::System;
use tendy_gba::bus::{
    CYCLES_PER_FRAME, CYCLES_PER_LINE, EWRAM_BASE, HBLANK_DRAW_CYCLES, IO_BASE, IWRAM_BASE,
    OAM_BASE, PALETTE_BASE, ROM_BASE, VBLANK_FIRST_LINE, VRAM_BASE, WAITCNT,
};
use tendy_gba::cart::Cartridge;
use tendy_gba::cpu::{MODE_IRQ, MODE_SYS};
use tendy_gba::{Gba, irq, timer};

/// A cartridge whose code starts at `$080000C0`, the usual entry: the header says so with a `b`.
fn synth_cart(code: &[u32]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-TEST\0");
    rom[0xAC..0xB0].copy_from_slice(b"TST1");
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

/// A machine running `code` from the cartridge's entry point.
fn machine(code: &[u32]) -> Gba {
    Gba::new(Cartridge::new(synth_cart(code)).expect("a synthesized cartridge"))
}

/// `mov r0, #0`.
const MOV_R0_0: u32 = 0xE3A0_0000;
/// `swi 020000h` — SWI 02, the BIOS `Halt` (GBATEK, *SWI 02h*).
const SWI_HALT: u32 = 0xEF02_0000;
/// `b .`
const SPIN: u32 = 0xEAFF_FFFE;
/// `mov r0, #04000000h`.
const MOV_R0_IO: u32 = 0xE3A0_0301;
/// `mov r1, #0`.
const MOV_R1_0: u32 = 0xE3A0_1000;
/// `strb r1, [r0, #301h]` — a store to `HALTCNT`.
const STRB_HALTCNT: u32 = 0xE5C0_1301;

/// `IE` at `$04000200` and friends, written straight to the bus: these tests are about the
/// controller, not about `str` instructions.
fn set16(gba: &mut Gba, offset: u32, value: u16) {
    gba.bus.write16(0x0400_0000 + offset, value);
}

fn set32(gba: &mut Gba, addr: u32, value: u32) {
    gba.bus.write32(addr, value);
}

#[test]
fn a_store_to_haltcnt_stops_the_cpu_between_instructions() {
    let mut gba = machine(&[MOV_R0_IO, MOV_R1_0, STRB_HALTCNT, SPIN]);
    // `mov`/`mov`/`strb`: the store is the third instruction, and the halt lands after it.
    gba.step_instruction();
    gba.step_instruction();
    assert!(!gba.cpu.halted, "not until the store has run");
    gba.step_instruction();
    assert!(gba.bus.halt_request, "the store was recorded");
    assert!(
        !gba.cpu.halted,
        "still inside the instruction that made the store"
    );
    gba.step_instruction();
    assert!(gba.cpu.halted, "the next step consumed the request");
    assert!(!gba.bus.halt_request);
}

#[test]
fn the_bios_halt_call_stops_the_cpu_too() {
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    gba.step_instruction();
    gba.step_instruction();
    assert!(gba.cpu.halted, "SWI 02 is the BIOS's Halt");
    assert_eq!(gba.bus.bios.calls[0x02], 1);
}

#[test]
fn a_halted_cpu_does_no_work_and_the_frame_still_costs_exactly_280896_cycles() {
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    gba.step_instruction();
    gba.step_instruction();
    assert!(gba.cpu.halted);
    // The frame the CPU halts in can end mid-instruction like any other; every frame after it is
    // the LCD's own 228 lines with nobody executing.
    gba.run_frame();
    let (cycles, instructions) = (gba.bus.cycles, gba.cpu.instructions);
    for _ in 0..59 {
        gba.run_frame();
    }
    assert_eq!(gba.frames(), 60);
    assert_eq!(
        gba.bus.cycles - cycles,
        59 * u64::from(CYCLES_PER_FRAME),
        "fifty-nine halted frames are fifty-nine of the LCD's frames"
    );
    assert_eq!(
        gba.cpu.instructions, instructions,
        "not one instruction retired while asleep"
    );
    assert!(gba.cpu.halted, "and still asleep: nothing was enabled");
}

#[test]
fn an_enabled_interrupt_wakes_the_halted_cpu_into_its_handler() {
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    // A handler the BIOS's IRQ prologue can jump to: `b .` at $03000100, pointed at from
    // [$03007FFC] (GBATEK, *BIOS Interrupt handling*).
    set32(&mut gba, IWRAM_BASE + 0x100, SPIN);
    set32(&mut gba, 0x0300_7FFC, IWRAM_BASE + 0x100);
    gba.step_instruction();
    gba.step_instruction();
    assert!(gba.cpu.halted);
    // Nothing enabled: a frame goes by and the CPU stays asleep.
    gba.run_frame();
    assert!(gba.cpu.halted);
    // Enable the VBlank request, raise it by hand (the display does this at line 160 — see
    // `the_display_raises_its_own_requests`), and set the master switch.
    set16(&mut gba, 0x200, irq::bit(irq::VBLANK));
    set16(&mut gba, 0x208, 1);
    gba.bus.raise(irq::VBLANK);
    gba.step_instruction();
    assert!(!gba.cpu.halted, "IE & IF woke it");
    assert_eq!(gba.cpu.mode(), MODE_IRQ, "and IME let the IRQ in");
    // The vector is `b 128h`, and the prologue at `$128` is what loads the pointer and jumps.
    gba.step_instruction();
    gba.step_instruction();
    assert_eq!(
        gba.cpu.pc(),
        IWRAM_BASE + 0x100,
        "the cartridge's handler is running"
    );
}

#[test]
fn halt_wakes_without_ime_but_does_not_take_the_interrupt() {
    // GBATEK, *HALTCNT*: "In Halt mode, the CPU is paused as long as (IE AND IF)=0", and the BIOS
    // *Halt* notes add that IME "is don't care" — the documented trap is a CPU that wakes, does not
    // dispatch, and runs on with the flag still set.
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    gba.step_instruction();
    gba.step_instruction();
    assert!(gba.cpu.halted);
    set16(&mut gba, 0x200, irq::bit(irq::VBLANK));
    gba.bus.raise(irq::VBLANK);
    gba.run_frame();
    assert!(!gba.cpu.halted, "IE & IF woke it");
    assert_eq!(
        gba.cpu.mode(),
        MODE_SYS,
        "IME is clear, so no exception was taken and the CPU is still in its boot mode"
    );
    assert_eq!(gba.cpu.pc(), 0x0800_00C8, "it carried on after the SWI");
}

#[test]
fn a_timer_overflows_through_the_bus_and_raises_its_own_request() {
    let mut gba = machine(&[SPIN]);
    // Timer 0: reload $FFF0, /1, IRQ on overflow, started. GBATEK's register layout.
    gba.bus.write16(0x0400_0100, 0xFFF0);
    gba.bus.write16(0x0400_0102, timer::START | timer::IRQ);
    assert_eq!(
        gba.bus.read16(0x0400_0100),
        0xFFF0,
        "the start edge copied the reload"
    );
    gba.bus.tick(0xF);
    assert_eq!(
        gba.bus.read16(0x0400_0100),
        0xFFFF,
        "the counter read back mid-count"
    );
    gba.bus.tick(1);
    assert_eq!(
        gba.bus.read16(0x0400_0100),
        0xFFF0,
        "an overflow reloads the counter"
    );
    assert_eq!(
        gba.bus.irq_flags(),
        irq::bit(irq::TIMER0),
        "and asks for its interrupt"
    );
}

#[test]
fn the_prescalers_and_the_cascade_are_the_hardware_s() {
    let mut gba = machine(&[SPIN]);
    // Timer 1 at /64, timer 2 cascaded onto it.
    gba.bus.write16(0x0400_0104, 0x0000);
    gba.bus.write16(0x0400_0106, timer::START | 0b01);
    gba.bus.write16(0x0400_0108, 0x0000);
    gba.bus
        .write16(0x0400_010A, timer::START | timer::CASCADE | 0b11);
    gba.bus.tick(63);
    assert_eq!(gba.bus.read16(0x0400_0104), 0, "not yet: 63 of 64 cycles");
    gba.bus.tick(1);
    assert_eq!(gba.bus.read16(0x0400_0104), 1, "one prescaled tick");
    assert_eq!(
        gba.bus.read16(0x0400_0108),
        0,
        "the cascade counts overflows, not ticks"
    );
    // 0xFFFF more prescaled ticks wrap timer 1 and give timer 2 exactly one.
    gba.bus.tick(0xFFFF * 64);
    assert_eq!(
        gba.bus.read16(0x0400_0104),
        0,
        "timer 1 wrapped to its reload"
    );
    assert_eq!(
        gba.bus.read16(0x0400_0108),
        1,
        "timer 2 counted the overflow"
    );
}

#[test]
fn a_halted_cpu_wakes_on_a_timer_overflow_without_the_display() {
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    gba.step_instruction();
    gba.step_instruction();
    assert!(gba.cpu.halted);
    // Timer 3 at /1 with a reload 16 below the top, IRQ on, and IE for it: the CPU must wake
    // inside the frame, without the display having anything to do with it.
    gba.bus.write16(0x0400_010C, 0xFFF0);
    gba.bus.write16(0x0400_010E, timer::START | timer::IRQ);
    set16(&mut gba, 0x200, irq::bit(irq::TIMER0 + 3));
    let before = gba.bus.cycles;
    gba.run_frame();
    assert!(!gba.cpu.halted, "the timer woke it");
    assert_ne!(
        gba.bus.irq_flags() & irq::bit(irq::TIMER0 + 3),
        0,
        "the overflow raised its request"
    );
    assert!(gba.bus.cycles > before);
}

#[test]
fn a_halted_frame_is_still_228_lines_of_1232_cycles() {
    // The measurement step 6 prints, as an assertion: a frame is 280 896 cycles however the CPU
    // spends them.
    let mut gba = machine(&[MOV_R0_0, SWI_HALT, SPIN]);
    gba.step_instruction();
    gba.step_instruction();
    assert_eq!(
        u64::from(CYCLES_PER_LINE) * u64::from(tendy_gba::bus::LINES_PER_FRAME),
        u64::from(CYCLES_PER_FRAME)
    );
    let before = gba.bus.cycles;
    gba.run_frame();
    let spent = gba.bus.cycles - before;
    assert!(
        spent <= u64::from(CYCLES_PER_FRAME),
        "the frame the CPU halted in cost {spent} cycles, no more than a whole one"
    );
    // From a frame boundary with the CPU asleep, every frame is exactly the LCD's own count.
    let before = gba.bus.cycles;
    gba.run_frame();
    assert_eq!(gba.bus.cycles - before, u64::from(CYCLES_PER_FRAME));
}

/// A 16-unit V-Blank transfer to a fixed destination: `DMAnCNT_L` counts units, so the source
/// walks 32 bytes and the fixed destination keeps the last halfword. The transfer's own cycles
/// are stolen from the CPU inside the frame without stretching it.
#[test]
fn a_vblank_dma_steals_its_cycles_inside_the_frame_without_stretching_it() {
    // The point of cycle stealing: the transfer runs, the CPU loses the time, and the frame is still
    // the display's 280 896 cycles.
    let mut gba = machine(&[SPIN]);
    gba.run_frame();
    gba.bus.write32(0x0400_00BC, EWRAM_BASE + 0x400);
    gba.bus.write32(0x0400_00C0, IWRAM_BASE + 0x800);
    gba.bus.write16(0x0400_00C4, 16);
    // Enable, V-Blank start timing, repeat, destination fixed.
    gba.bus
        .write16(0x0400_00C6, 0x8000 | 0x1000 | 0x0200 | 0x0040);
    for index in 0..32u32 {
        gba.bus.ewram[0x400 + index as usize] = 0x40 + index as u8;
    }
    let before = gba.bus.cycles;
    gba.bus.tick(CYCLES_PER_FRAME);
    let spent = gba.bus.cycles - before;
    assert_eq!(
        gba.bus.iwram[0x800..0x802],
        gba.bus.ewram[0x41E..0x420],
        "a fixed destination keeps the last unit of a 16-unit walk; channel 1 is {:?}, line {}, \
         frames {}",
        gba.bus.dma.channel(1),
        gba.bus.lcd.line,
        gba.bus.lcd.frames
    );
    assert_eq!(
        gba.bus.iwram[0x802], 0,
        "a fixed destination cannot block-copy: nothing past the last halfword was written"
    );
    assert_eq!(
        spent,
        u64::from(CYCLES_PER_FRAME) + 66,
        "the frame's 280 896 cycles plus the transfer's own halves: 1N+(n-1)S reading Work RAM \
         256K (3 + 15*3 = 48), 1N+(n-1)S writing Work RAM 32K (1 + 15*1 = 16), and 2I"
    );
    assert!(gba.bus.dma.channel(1).enabled(), "the repeat kept it armed");
}

// ----------------------------------------------------------------------------------------------------
// The three behaviours the bus priced as if every access were the best case: the depth of
// the Game Pak prefetch buffer, the forced non-sequential timing at each 128 KiB block, and the one
// extra cycle an access to the video memories owes while the display is drawing.
// ----------------------------------------------------------------------------------------------------

/// A synthesized cartridge of `len` bytes: the header and entry branch `synth_cart` writes, then
/// zeroes. A test that fetches next to `$08020000` needs a cartridge that reaches it.
fn machine_of_len(code: &[u32], len: usize) -> Gba {
    let mut rom = synth_cart(code);
    rom.resize(len, 0);
    Gba::new(Cartridge::new(rom).expect("a synthesized cartridge"))
}

/// The cycles one opcode fetch of a halfword really costs: the one cycle the core's flat model counts
/// for it plus whatever the bus said was owed.
fn fetch_half(gba: &mut Gba, addr: u32) -> u32 {
    gba.bus.waits = 0;
    let _ = gba.bus.fetch16(addr);
    1 + gba.bus.waits
}

/// The same for a word fetch, which the Game Pak prices as two 16-bit fragments.
fn fetch_word(gba: &mut Gba, addr: u32) -> u32 {
    gba.bus.waits = 0;
    let _ = gba.bus.fetch32(addr);
    1 + gba.bus.waits
}

/// The same for a CPU data access of `width` bytes.
fn data_cost(gba: &mut Gba, addr: u32, width: usize) -> u32 {
    gba.bus.waits = 0;
    match width {
        1 => {
            gba.bus.read8(addr);
        }
        2 => {
            gba.bus.read16(addr);
        }
        _ => {
            gba.bus.read32(addr);
        }
    }
    1 + gba.bus.waits
}

#[test]
fn the_prefetch_buffer_holds_eight_halfwords_so_a_short_forward_branch_is_a_hit() {
    // GBATEK, *GBA GamePak Prefetch*: "Memory access is then performed with 0 Waits if the CPU
    // requests data which is already stored in the buffer. The prefetch buffer stores up to eight
    // 16bit values." With `WAITCNT=4317h` a WS0 access is 4 waits and then 1, so an unbuffered
    // halfword costs 5 and a sequential one 2; a buffered fragment costs its own one cycle.
    let mut gba = machine_of_len(&[SPIN], 0x400);
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x4317);
    gba.bus.prefetch_next = None;
    gba.bus.last_fetch_end = None;
    let at = ROM_BASE + 0x100;

    assert_eq!(fetch_word(&mut gba, at), 6, "the first fetch pays the 4+2");
    assert_eq!(
        fetch_word(&mut gba, at + 4),
        2,
        "the word after it is buffered"
    );
    // Two words further on is still inside the eight halfwords the buffer holds: a forward branch
    // over one instruction costs nothing.
    assert_eq!(
        fetch_word(&mut gba, at + 12),
        2,
        "a two-word branch forward is a buffer hit"
    );
    // Eight halfwords past the last fetch is the first address the buffer does not hold.
    assert_eq!(
        fetch_word(&mut gba, at + 0x20),
        6,
        "a branch past the buffer pays the non-sequential cost"
    );
}

#[test]
fn a_rom_access_that_opens_a_128_kib_block_pays_the_non_sequential_wait() {
    // GBATEK, *GBA GamePak Prefetch → NOTES*: "The GBA forcefully uses non-sequential timing at the
    // beginning of each 128K-block of gamepak ROM, eg. "LDMIA [801fff8h],r0-r7" will have
    // non-sequential timing at 8020000h."
    let mut gba = machine_of_len(&[SPIN], 0x2_0004);
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x4317);
    gba.bus.prefetch_next = None;
    gba.bus.last_fetch_end = None;
    gba.bus.last_data_end = None;
    let last = ROM_BASE + 0x1_FFFE;

    assert_eq!(
        fetch_half(&mut gba, last),
        4,
        "the fetch below the block pays 3+1"
    );
    assert_eq!(
        fetch_half(&mut gba, last + 2),
        4,
        "the fetch that opens $08020000 is non-sequential, buffered or not"
    );
    assert_eq!(
        fetch_half(&mut gba, last + 4),
        1,
        "and the refill from the block's first fetch covers the next one"
    );

    // The same rule on the data side, which is the example the document gives: an `LDMIA` walking
    // through the boundary.
    gba.bus.last_data_end = None;
    assert_eq!(data_cost(&mut gba, last, 2), 4, "the word below the block");
    assert_eq!(
        data_cost(&mut gba, last + 2, 2),
        4,
        "the word that opens it is non-sequential"
    );
    assert_eq!(
        data_cost(&mut gba, last + 4, 2),
        2,
        "and the next one sequential"
    );
}

#[test]
fn a_video_memory_access_costs_one_more_cycle_only_while_the_display_draws() {
    // GBATEK, *Address Bus Width and CPU Read/Write Access Widths*: the table's `OAM 1/1/1 *`,
    // `Palette RAM 1/1/2 *` and `VRAM 1/1/2 *` rows, footnoted "* Plus 1 cycle if GBA accesses video
    // memory at the same time"; and *VRAM, OAM, and Palette RAM Access*: "a waitstate (one clock
    // cycle) being inserted automatically in case that the display controller was accessing memory
    // simultaneously."
    let mut gba = machine(&[SPIN]);
    gba.bus.write16(IO_BASE, 0x0000); // mode 0, display on, nothing force-blanked
    gba.bus.lcd.line = 0;
    gba.bus.lcd.line_cycle = 0;

    assert_eq!(
        data_cost(&mut gba, VRAM_BASE, 2),
        2,
        "VRAM while drawing: 1+1"
    );
    assert_eq!(data_cost(&mut gba, PALETTE_BASE, 2), 2, "palette: 1+1");
    assert_eq!(data_cost(&mut gba, OAM_BASE, 2), 2, "OAM: 1+1");
    assert_eq!(data_cost(&mut gba, VRAM_BASE, 4), 3, "a VRAM word: 2+1");
    assert_eq!(
        data_cost(&mut gba, IWRAM_BASE, 2),
        1,
        "work RAM has no footnote"
    );

    // A word store: the two cycles the core's flat `2N` already counts are the region's, so the
    // extra one is what the bus adds.
    gba.bus.waits = 0;
    gba.bus.write32(VRAM_BASE, 0x1234_5678);
    assert_eq!(gba.bus.waits, 1, "a VRAM word store while drawing owes 1");

    // The H-Blank dot: the controller has finished the line's fetches.
    gba.bus.lcd.line = 2;
    gba.bus.lcd.line_cycle = HBLANK_DRAW_CYCLES;
    assert_eq!(
        data_cost(&mut gba, VRAM_BASE, 2),
        1,
        "in H-Blank the 1 stands"
    );
    gba.bus.waits = 0;
    gba.bus.write32(VRAM_BASE, 0x1234_5678);
    assert_eq!(gba.bus.waits, 0, "and a store there owes nothing extra");

    // V-Blank, where the controller fetches nothing at all.
    gba.bus.lcd.line = VBLANK_FIRST_LINE;
    gba.bus.lcd.line_cycle = 0;
    assert_eq!(data_cost(&mut gba, VRAM_BASE, 2), 1, "in V-Blank too");

    // And forced blank, which GBATEK's *VRAM, OAM, and Palette RAM Access* names as the case where
    // there is no restriction: "unless display is disabled by Forced Blank bit in DISPCNT".
    gba.bus.lcd.line = 0;
    gba.bus.lcd.line_cycle = 0;
    gba.bus.write16(IO_BASE, 0x0080);
    assert_eq!(
        data_cost(&mut gba, VRAM_BASE, 2),
        1,
        "with the display force-blanked the access is the table's 1"
    );
}
