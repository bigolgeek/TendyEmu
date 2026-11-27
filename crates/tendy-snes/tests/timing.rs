//! The clock: the frame's length, the access-cost table, `MEMSEL`, the DRAM-refresh pause, open bus
//! and the bank wrap.

mod common;

use tendy_snes::addr::Addr;
use tendy_snes::cart::Cartridge;
use tendy_snes::machine::Machine;
use tendy_snes::ppu::LineEvents;
use tendy_snes::{
    DOTS_PER_FRAME, FAST_ACCESS, FPS, JOYPAD_ACCESS, LINES_PER_FRAME, MASTER_CLOCK_HZ,
    MASTER_TICKS_PER_FRAME, REFRESH_TICKS, SLOW_ACCESS,
};

fn fresh() -> Machine {
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    Machine::new(cart)
}

#[test]
fn fps_is_derived_from_the_frame_length() {
    // §2.1: FPS is `MASTER_CLOCK_HZ / MASTER_TICKS_PER_FRAME`, not a quoted constant.
    assert_eq!(MASTER_TICKS_PER_FRAME, 357_368);
    assert_eq!(MASTER_TICKS_PER_FRAME, LINES_PER_FRAME * 1364);
    assert_eq!(DOTS_PER_FRAME, 89_342);
    let derived = f64::from(MASTER_CLOCK_HZ) / f64::from(MASTER_TICKS_PER_FRAME);
    assert!(
        (FPS - derived).abs() < 1e-8,
        "FPS {FPS} is {derived} = {MASTER_CLOCK_HZ} / {MASTER_TICKS_PER_FRAME}"
    );
    // The design's claim that the two forms agree to the crystal's precision (§2.1).
    assert!((FPS * f64::from(MASTER_TICKS_PER_FRAME) - f64::from(MASTER_CLOCK_HZ)).abs() < 0.001);
}

#[test]
fn access_costs_follow_the_region_table() {
    // fullsnes "SNES CPU Clock Cycles", the "SNES Memory Speed" table; each row is
    // (bank:offset, master clocks, the source's row).
    let rows: &[(u8, u16, u32, &str)] = &[
        (
            0x00,
            0x0000,
            8,
            "WRAM (8K mirror of 7E:0000-1FFF) medium 2.68MHz",
        ),
        (0x00, 0x2000, 6, "I/O and Expansion fast 3.58MHz"),
        (0x00, 0x4000, 12, "Manual Joypad Reading slow 1.78MHz"),
        (0x00, 0x4200, 6, "I/O and Expansion fast"),
        (0x00, 0x6000, 8, "SRAM and Expansion medium"),
        (0x00, 0x8000, 8, "ROM (32K banks) medium"),
        (0x40, 0x0000, 8, "ROM/SRAM/WRAM medium"),
        (0x7E, 0x0000, 8, "WRAM medium"),
        (0x7F, 0xFFFF, 8, "WRAM medium"),
        (0x00, 0x1FFF, 8, "the last mirrored byte is still medium"),
        (0x80, 0x2000, 6, "the mirror bank keeps the fast window"),
        (0x80, 0x4000, 12, "the mirror bank keeps the joypad window"),
    ];
    let bus = fresh().bus;
    assert_eq!(SLOW_ACCESS, 8);
    assert_eq!(FAST_ACCESS, 6);
    assert_eq!(JOYPAD_ACCESS, 12);
    for &(bank, offset, clocks, source) in rows {
        assert_eq!(
            bus.access_ticks(Addr::new(bank, offset)),
            clocks,
            "${bank:02X}:{offset:04X} costs {clocks} master clocks ({source})"
        );
    }
}

#[test]
fn memsel_changes_the_rom_cost() {
    // fullsnes "SNES Memory Control" `$420D` bit 0: "Access Cycle for Memory-2 Area
    // (0=2.68MHz, 1=3.58MHz)". WS1 ($00-$3F) is fixed and must not move.
    let mut machine = fresh();
    let slow = Addr::new(0x80, 0x8000);
    let hi = Addr::new(0xC0, 0x0000);
    assert_eq!(
        machine.bus.access_ticks(slow),
        8,
        "MEMSEL=0: WS2 is 2.68MHz"
    );
    assert_eq!(
        machine.bus.access_ticks(hi),
        8,
        "MEMSEL=0: banks C0-FF are 2.68MHz"
    );
    machine.bus.write(Addr::new(0, 0x420D), 0x01);
    assert_eq!(
        machine.bus.access_ticks(slow),
        6,
        "MEMSEL=1: WS2 is 3.58MHz"
    );
    assert_eq!(
        machine.bus.access_ticks(hi),
        6,
        "MEMSEL=1: banks C0-FF are 3.58MHz"
    );
    assert_eq!(
        machine.bus.access_ticks(Addr::new(0x00, 0x8000)),
        8,
        "WS1 (banks 00-3F) is fixed at 2.68MHz whatever MEMSEL says"
    );
    assert_eq!(
        machine.bus.access_ticks(Addr::new(0x7E, 0x8000)),
        8,
        "WRAM is never speed-selectable"
    );
}

#[test]
fn refresh_stalls_the_cpu_forty_clocks() {
    // fullsnes "SNES Timing H/V Counters": "Refresh (per scanline) 40 master cycles". The PPU
    // raises the event at dot 134; the next CPU access burns whatever is left of the stall.
    let mut machine = fresh();
    let before = machine.bus.master;
    machine.bus.on_line_events(LineEvents(LineEvents::REFRESH));
    assert_eq!(machine.bus.stall, REFRESH_TICKS, "the stall is 40 clocks");
    machine.bus.read(Addr::new(0x7E, 0x0000));
    assert_eq!(
        machine.bus.master - before,
        40 + 8,
        "the stalled access costs its own 8 clocks plus the 40-clock pause"
    );
    let second = machine.bus.master;
    machine.bus.read(Addr::new(0x7E, 0x0000));
    assert_eq!(
        machine.bus.master - second,
        8,
        "the stall is consumed once, not per access"
    );
    assert_eq!(machine.bus.stall, 0);

    // A line start gives the CPU a fresh clock (§3.3).
    machine.bus.on_line_events(LineEvents(LineEvents::REFRESH));
    machine.bus.on_line_events(LineEvents(LineEvents::START));
    assert_eq!(machine.bus.stall, 0, "START cleared the stall");
}

#[test]
fn run_frame_is_357368_master_clocks() {
    // 262 lines x 1364 clocks, driven a dot at a time so nothing rounds: `MASTER_TICKS_PER_FRAME`.
    let mut machine = fresh();
    let mut ticks = 0u64;
    for _ in 0..DOTS_PER_FRAME {
        machine.bus.tick(4);
        ticks += 4;
        if machine.bus.ppu.frame_ready {
            break;
        }
    }
    assert!(
        machine.bus.ppu.frame_ready,
        "the frame ended after {ticks} clocks"
    );
    assert_eq!(
        ticks,
        u64::from(MASTER_TICKS_PER_FRAME),
        "one frame is 262 x 1364 = {MASTER_TICKS_PER_FRAME} master clocks"
    );
    assert_eq!(machine.bus.master, u64::from(MASTER_TICKS_PER_FRAME));
    assert_eq!(
        machine.bus.ppu.line, 0,
        "the frame ended on a line boundary"
    );

    // The frame loop hands the clock to the CPU instead, so its total is the frame plus at most one
    // instruction's overshoot (§3.6). One access is 6-12 clocks, so the overshoot is bounded by the
    // longest single access; the PPU stub's `ticks / 4` truncation is what keeps the sum above the
    // frame rather than at it (see the report: the dot counter needs to keep the master-tick
    // remainder, which is the PPU lane's file).
    let mut machine = fresh();
    let run = machine.run_frame();
    assert!(
        run >= MASTER_TICKS_PER_FRAME,
        "run_frame charged {run}, which is less than a frame's {MASTER_TICKS_PER_FRAME}"
    );
    assert_eq!(machine.frames, 1, "one call is one frame");
    assert_eq!(
        machine.bus.ppu.frames, 1,
        "the PPU finished exactly one frame"
    );
    assert_eq!(
        u64::from(run),
        machine.bus.master,
        "run_frame's return value is the clock the CPU charged"
    );
}

#[test]
fn open_bus_is_the_last_driven_value() {
    // "The data bus keeps its last driven value." A write-only PPU port is a good
    // witness, because nothing stores it: only the bus's own latch can answer.
    let mut machine = fresh();
    machine.bus.write(Addr::new(0x00, 0x2100), 0x42);
    assert_eq!(
        machine.bus.read(Addr::new(0x00, 0x2000)),
        0x42,
        "an unused $2000-$20FF byte returns the last driven value"
    );
    assert_eq!(
        machine.bus.read(Addr::new(0x00, 0x2100)),
        0x42,
        "a write-only PPU port returns the last driven value"
    );
    machine.bus.write(Addr::new(0x00, 0x2101), 0xA5);
    assert_eq!(
        machine.bus.read(Addr::new(0x00, 0x2200)),
        0xA5,
        "the mirror bank's unused window follows too"
    );
    // A read of a *driven* register replaces the latch as well.
    machine.bus.poke(Addr::new(0x7E, 0x0100), 0x5C);
    assert_eq!(machine.bus.read(Addr::new(0x7E, 0x0100)), 0x5C);
    assert_eq!(machine.bus.mdr, 0x5C);
}

#[test]
fn bank_wrap_stays_inside_the_bank() {
    // An operand fetch at the end of a bank must continue at offset $0000 of the *same* bank
    // ("a bank wrap ($00:FFFF -> $00:0000, not $01:0000)").
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    // `LDA $nnnn` with the opcode at $7E:FFFF: the operand bytes come from $7E:0000-$7E:0001.
    machine.bus.poke(Addr::new(0x7E, 0xFFFF), 0xAD);
    machine.bus.poke(Addr::new(0x7E, 0x0000), 0x00);
    machine.bus.poke(Addr::new(0x7E, 0x0001), 0x01);
    machine.bus.poke(Addr::new(0x7E, 0x0100), 0x3C);
    // The next bank holds different bytes, so a wrap into $7F would read $FF and fail.
    machine.bus.poke(Addr::new(0x7F, 0x0000), 0xFF);
    machine.bus.poke(Addr::new(0x7F, 0x0001), 0xFF);
    machine.cpu.pb = 0x7E;
    machine.cpu.pc = 0xFFFF;
    machine.cpu.e = true;
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        machine.cpu.a, 0x3C,
        "the operand wrapped to $7E:0000, not into bank $7F"
    );
    assert_eq!(machine.bus.read(Addr::new(0x7E, 0x0100)), 0x3C);
}
