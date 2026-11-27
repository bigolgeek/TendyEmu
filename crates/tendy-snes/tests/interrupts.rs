//! Interrupts: the NMI flag and its latch, the H/V IRQ comparison, the level-driven IRQ, the TIMEUP
//! acknowledge, `RTI`'s status restore, NMI's priority and the two maths latencies (fullsnes
//! "SNES PPU Interrupts" and "SNES Maths Multiply/Divide").

mod common;

use tendy_snes::Machine;
use tendy_snes::addr::Addr;
use tendy_snes::cart::Cartridge;
use tendy_snes::cpu::Flags;
use tendy_snes::cpu::Vector;
use tendy_snes::ppu::LineEvents;

/// 256 `nop`s at `$7E:0000`: a main loop that advances exactly one byte a step. An empty WRAM is
/// `BRK`, which hides a wrong `RTI` return address behind the BRK vector instead of exposing it.
fn nop_main_loop() -> [u8; 256] {
    [0xEA; 256]
}

/// A machine running `code` from `$7E:0000`, with `patches` written into the ROM first.
fn machine_with(patches: &[(usize, &[u8])], code: &[u8]) -> Machine {
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
    machine.cpu.e = false;
    machine.cpu.p = Flags(Flags::I);
    machine
}

/// A bare machine whose ROM vectors the test does not care about.
fn bare() -> Machine {
    machine_with(&[], &[])
}

/// Steps the PPU's dot counter from `from` to `to` on `line`, one dot a tick, which is what makes
/// the H/V comparison's edge detection observable.
fn walk(bus: &mut tendy_snes::bus::Bus, line: u16, from: u16, to: u16) {
    bus.ppu.line = line;
    bus.ppu.dot = from;
    while bus.ppu.dot < to {
        bus.tick(4);
    }
}

/// Reads one of the S-CPU's ports without charging the CPU's clock, which the maths and interrupt
/// tests need because the same registers are also the subject of their timing assertions.
fn read_port(bus: &mut tendy_snes::bus::Bus, offset: u16) -> u8 {
    let mdr = bus.mdr;
    bus.io.read(Addr::new(0, offset), mdr)
}

/// A machine whose main loop is `nop`s at `$7E:0000`, NMI enabled, the native NMI vector at
/// `$00:0100`, and the PPU a few hundred clocks before V=225. The vector is never executed: the
/// tests read `PB:PC` after one step, which is `$7E:0001` when no NMI was taken and `$00:0100`
/// when it was.
fn machine_at_the_edge_of_vblank(dots_before: u16) -> Machine {
    let mut machine = machine_with(&[(0x7FEA, &[0x00, 0x01])], &nop_main_loop());
    machine.bus.io.nmitimen = 0x80;
    machine.bus.io.nmi_flag = false;
    machine.bus.ppu.line = 224;
    machine.bus.ppu.dot = 341 - dots_before;
    machine
}

/// Points channel 0 at an unused B-bus port, `bytes` long (a length of zero is fullsnes's
/// 65 536), without starting it. Reading the source from cart ROM and writing to `$2188` —
/// fullsnes's "dummy bbus destination address (unused port 2188h)" — has no side effect at all, so
/// the transfer's only effect is the clock it costs.
fn arm_dma(machine: &mut Machine, bytes: u16) {
    let c = &mut machine.bus.dma.channels[0];
    c.dmap = 0x00;
    c.bbad = 0x88;
    c.a1t = Addr::new(0xC0, 0x0000);
    c.das = bytes;
}

/// [`arm_dma`] and then the `$420B` store that runs it.
fn start_dma(machine: &mut Machine, bytes: u16) {
    arm_dma(machine, bytes);
    machine.bus.write(Addr::new(0, 0x420B), 0x01);
}

/// fullsnes "$420Bh - MDMAEN": "The CPU is paused during the transfer." The NMI latch is the
/// *CPU's* own flag — fullsnes "SNES PPU Interrupts": it "gets set when [4200h].7 AND [4210h].7
/// changes from 0-to-1" — so a v-blank that rises and falls entirely inside a general-purpose DMA
/// is not an NMI the CPU can see: the PPU sets and clears RDNMI.7 while the CPU's clock is stopped.
/// A transfer that outlives the whole 37-line window must therefore leave the CPU in its main
/// loop, one instruction further on.
#[test]
fn a_v_blank_that_begins_and_ends_inside_a_dma_is_not_taken() {
    let mut machine = machine_at_the_edge_of_vblank(11); // ~44 clocks to V=225
    // 65 536 bytes at 8 master clocks a byte is 524 288 clocks, 1.47 frames: the whole
    // V=225..261 window (37 lines, 50 468 clocks) is inside the transfer.
    start_dma(&mut machine, 0);
    assert!(
        !machine.bus.io.nmi_flag,
        "the v-blank ended while the DMA had the CPU paused, so RDNMI.7 is clear again"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x7E, 0x0001),
        "the main loop's next NOP ran: a v-blank the paused CPU never saw is not an NMI"
    );
}

/// The other half of the rule: a transfer that ends while RDNMI.7 is still high leaves the level
/// up, and the CPU takes the NMI at the first instruction boundary after the transfer.
#[test]
fn a_v_blank_that_ends_inside_a_short_dma_is_taken_after_it() {
    let mut machine = machine_at_the_edge_of_vblank(11);
    start_dma(&mut machine, 16); // 128 clocks: v-blank begins ~44 clocks in and lasts 50 468
    assert!(
        machine.bus.io.nmi_flag,
        "the transfer finished inside v-blank, so the NMI flag is still high"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x00, 0x0100),
        "the deferred NMI is taken at the first boundary after the DMA"
    );
}

#[test]
fn nmi_flag_sets_at_vblank_and_clears_at_v_zero() {
    let mut machine = bare();
    let bus = &mut machine.bus;
    // V=225: the PPU's V-blank event sets RDNMI bit 7 (§3.8).
    bus.ppu.line = 225;
    bus.io.nmi_flag = false;
    bus.on_line_events(LineEvents(LineEvents::VBLANK));
    assert!(bus.io.nmi_flag, "the v-blank event set the NMI flag");
    assert_eq!(
        read_port(bus, 0x4210),
        0x82,
        "$4210 reads the flag in bit 7 and CPU version 2 in bits 3-0"
    );
    assert!(!bus.io.nmi_flag, "reading $4210 acknowledges the flag");

    // V=0 clears it again: fullsnes "RDNMI": "The flag gets reset automatically at end of Vblank".
    bus.on_line_events(LineEvents(LineEvents::VBLANK));
    assert!(bus.io.nmi_flag);
    bus.ppu.line = 261;
    bus.ppu.dot = 340;
    bus.tick(4); // wraps the line counter to V=0
    assert_eq!(bus.ppu.line, 0, "the counter wrapped to V=0");
    assert!(!bus.io.nmi_flag, "V=0 cleared the NMI flag");
}

#[test]
fn the_nmi_level_is_high_only_in_vblank() {
    // fullsnes "4210h - RDNMI" (lines 1762-1764 of the cached page): "The NMI flag gets set at begin
    // of Vblank ... The flag gets reset automatically at end of Vblank". The line the chip drives is
    // therefore high for V=225..261 and low for V=0..224, which is what `Bus::nmi_pending` reads.
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.io.nmitimen = 0x80; // NMI enabled: the level is now an interrupt and not just a flag
    bus.ppu.line = 0;
    bus.ppu.dot = 0;
    bus.ppu.nmi = false;
    bus.io.nmi_flag = false;
    let mut high: Vec<u16> = Vec::new();
    for _ in 0..(tendy_snes::DOTS_PER_LINE as usize * tendy_snes::LINES_PER_FRAME as usize) {
        bus.tick(4);
        if bus.ppu.nmi && high.last() != Some(&bus.ppu.line) {
            high.push(bus.ppu.line);
        }
        if bus.ppu.line < tendy_snes::VBLANK_START {
            assert!(
                !bus.nmi_pending(),
                "line {} is outside v-blank, so the NMI line cannot be pending (fullsnes \"4210h\": \
                 the flag \"gets reset automatically at end of Vblank\")",
                bus.ppu.line
            );
        }
    }
    assert_eq!(
        high,
        (tendy_snes::VBLANK_START..tendy_snes::LINES_PER_FRAME as u16).collect::<Vec<u16>>(),
        "the NMI level is high on V=225..261 and on no other line of the frame"
    );
}

#[test]
fn nmi_runs_once_per_frame() {
    // The handler at $00:9000 counts itself into $00:1000 and returns; the NMI vector is $FFEA in
    // native mode. The main loop is the ROM's `bra *`.
    let patches: [(usize, &[u8]); 2] = [
        (0x1000, &[0xEE, 0x00, 0x10, 0x40]), // inc $1000 ; rti
        (0x7FEA, &[0x00, 0x90]),             // NMI vector -> $9000
    ];
    let mut machine = machine_with(&patches, &nop_main_loop());
    machine.bus.io.nmitimen = 0x80;
    for _ in 0..4 {
        machine.cpu.step(&mut machine.bus);
    }
    assert_eq!(machine.bus.peek(Addr::new(0, 0x1000)), 0, "no NMI yet");

    // The v-blank edge arms the latch, and the very next step takes the NMI.
    machine.bus.ppu.line = 224;
    machine.bus.ppu.dot = 340;
    machine.bus.tick(4);
    assert!(machine.bus.io.nmi_flag, "the counter reached V=225");
    for _ in 0..6 {
        machine.cpu.step(&mut machine.bus);
    }
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x1000)),
        1,
        "the handler ran once"
    );
    for _ in 0..40 {
        machine.cpu.step(&mut machine.bus);
    }
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x1000)),
        1,
        "the NMI latch is cleared when it is taken, so it does not run again in the same frame"
    );

    // The next frame's v-blank runs it again.
    machine.bus.ppu.line = 261;
    machine.bus.ppu.dot = 340;
    machine.bus.tick(4); // V=0, which clears the flag and so re-arms the edge
    machine.bus.ppu.line = 224;
    machine.bus.ppu.dot = 340;
    machine.bus.tick(4);
    for _ in 0..8 {
        machine.cpu.step(&mut machine.bus);
    }
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x1000)),
        2,
        "the second frame's v-blank ran the handler once more"
    );
}

#[test]
fn the_interrupt_frame_pushes_the_program_bank_first() {
    // Eyes & Lichty, *Programming the 65816*, §"How Interrupts Are Handled" (Figure 13.2): "in
    // native mode the program bank is pushed onto the stack first, before the program counter and
    // the status register". The frame therefore reads upwards as P, PCL, PCH, PB, which is exactly
    // the order `RTI` pulls; pushing PB after the program counter put every later `RTI` one byte out
    // and Super Mario World's NMI returned to $80:6B00 instead of $00:806B.
    let mut machine = machine_with(&[], &[]);
    machine.cpu.s = 0x01FF;
    machine.cpu.pb = 0x12;
    machine.cpu.pc = 0x3456;
    machine.cpu.p = Flags(Flags::M | Flags::X);
    machine.cpu.service_interrupt(&mut machine.bus, Vector::Nmi);
    assert_eq!(machine.cpu.s, 0x01FB, "native mode pushes four bytes");
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x01FC)),
        0x30,
        "P is on top of the frame"
    );
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x01FD)),
        0x56,
        "PC low comes next"
    );
    assert_eq!(machine.bus.peek(Addr::new(0, 0x01FE)), 0x34, "then PC high");
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x01FF)),
        0x12,
        "PB is pushed first, so it is the deepest byte"
    );
}

#[test]
fn h_irq_fires_at_htime() {
    // fullsnes `$4207`: the flag "gets set when the H-Counter gets equal to the H-Count register
    // value"; §3.8 places the match at dot `HTIME + 3`.
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.io.nmitimen = 0x10;
    bus.io.htime = 100;
    bus.io.irq_flag = false;
    walk(bus, 5, 0, 102);
    assert!(
        !bus.io.irq_flag,
        "no H-IRQ before dot {} on line 5",
        bus.ppu.dot
    );
    walk(bus, 5, 102, 103);
    assert!(bus.io.irq_flag, "the H-IRQ fired at dot HTIME + 3 = 103");
    assert!(bus.irq_pending(), "the IRQ line follows the flag");

    // Acknowledging and walking the same line again does not re-trigger it: it is an edge.
    read_port(&mut machine.bus, 0x4211);
    assert!(!machine.bus.io.irq_flag);
    walk(&mut machine.bus, 5, 103, 200);
    assert!(
        !machine.bus.io.irq_flag,
        "the compare fires once per line, not continuously"
    );
}

#[test]
fn v_irq_fires_at_vtime() {
    // §3.8: the V compare is at `H = 2.5, V = VTIME`, i.e. dot 2 in whole dots.
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.io.nmitimen = 0x20;
    bus.io.vtime = 10;
    bus.io.irq_flag = false;
    walk(bus, 9, 0, 200);
    assert!(!bus.io.irq_flag, "no V-IRQ on line 9");
    walk(bus, 10, 0, 1);
    assert!(!bus.io.irq_flag, "no V-IRQ before dot 2");
    walk(bus, 10, 1, 2);
    assert!(bus.io.irq_flag, "the V-IRQ fired at dot 2 of line 10");
}

#[test]
fn hv_irq_requires_both() {
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.io.nmitimen = 0x30;
    bus.io.htime = 100;
    bus.io.vtime = 10;
    bus.io.irq_flag = false;
    walk(bus, 5, 0, 200);
    assert!(
        !bus.io.irq_flag,
        "H+V mode needs the H compare on VTIME's line: line 5 is not it"
    );
    walk(bus, 10, 0, 102);
    assert!(!bus.io.irq_flag, "line 10 before dot 103");
    walk(bus, 10, 102, 103);
    assert!(bus.io.irq_flag, "line 10 at dot HTIME + 3");
}

#[test]
fn irq_is_level_triggered_until_acked() {
    // The handler at $00:9000 increments $00:1000 and returns; IRQ's native vector is $FFEE.
    let patches: [(usize, &[u8]); 2] = [
        (0x1000, &[0xEE, 0x00, 0x10, 0x40]), // inc $1000 ; rti
        (0x7FEE, &[0x00, 0x90]),             // IRQ vector -> $9000
    ];
    let mut machine = machine_with(&patches, &nop_main_loop());
    // H-only mode with an unreachable compare point, so the line stays low once it is raised.
    machine.bus.io.nmitimen = 0x10;
    machine.bus.io.htime = 0x01FF;
    machine.bus.io.irq_flag = true;
    machine.cpu.p.set_i(false);

    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.cpu.pc, 0x9000, "the IRQ was taken");
    assert!(
        machine.bus.io.irq_flag,
        "dispatching does not acknowledge: fullsnes says the flag is only cleared by reading $4211"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.bus.peek(Addr::new(0, 0x1000)), 1, "the handler ran");
    machine.cpu.step(&mut machine.bus); // RTI
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        machine.cpu.pc, 0x9000,
        "the handler was re-entered straight after RTI because the line is still low"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.bus.peek(Addr::new(0, 0x1000)), 2);

    // Reading TIMEUP acknowledges it and the handler is not re-entered.
    machine.bus.read(Addr::new(0, 0x4211));
    assert!(!machine.bus.io.irq_flag, "$4211 acknowledged the IRQ");
    machine.cpu.step(&mut machine.bus); // RTI back to the main loop
    let pc = machine.cpu.pc;
    machine.cpu.step(&mut machine.bus);
    assert_ne!(machine.cpu.pc, 0x9000, "no re-entry after the acknowledge");
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x1000)),
        2,
        "no further handler run"
    );
    assert_eq!(
        machine.cpu.pc,
        pc.wrapping_add(1),
        "the main loop continued"
    );
}

#[test]
fn timeup_read_in_the_window() {
    // The design pins v1's rule for the documented exception: a read always clears the flag, even
    // when it lands inside the 4-8 clock window where the hardware keeps it set (§4.6's last row).
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.io.nmitimen = 0x10;
    bus.io.htime = 100;
    bus.io.irq_flag = false;
    bus.mdr = 0x00;
    walk(bus, 5, 0, 103);
    assert!(bus.io.irq_flag);
    let value = read_port(bus, 0x4211);
    assert_eq!(value & 0x80, 0x80, "TIMEUP reads its flag in bit 7");
    assert_eq!(value & 0x7F, 0x00, "bits 6-0 are open bus");
    assert!(!bus.io.irq_flag, "v1 always clears the flag on a read");
    assert_eq!(
        read_port(bus, 0x4211) & 0x80,
        0,
        "a second read sees a cleared flag"
    );
}

#[test]
fn rti_restores_the_status_byte() {
    // The stack, pushes being highest-address-first: P at S+1, then PC low, PC high, and PB.
    let mut machine = machine_with(&[], &[0x40]); // RTI
    machine.cpu.s = 0x01FB;
    machine.bus.poke(Addr::new(0, 0x01FC), 0xB0); // N | M | X
    machine.bus.poke(Addr::new(0, 0x01FD), 0x34); // PC low
    machine.bus.poke(Addr::new(0, 0x01FE), 0x12); // PC high
    machine.bus.poke(Addr::new(0, 0x01FF), 0x7E); // PB
    machine.cpu.p = Flags(Flags::I);
    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.cpu.p.0, 0xB0, "RTI restored P from the stack");
    assert_eq!(machine.cpu.pc, 0x1234, "RTI restored the program counter");
    assert_eq!(machine.cpu.pb, 0x7E, "RTI restored the program bank");
    assert_eq!(
        machine.cpu.s, 0x01FF,
        "RTI popped four bytes in native mode"
    );

    // Emulation mode pops three and keeps M and X forced on.
    let mut machine = machine_with(&[], &[0x40]);
    machine.cpu.e = true;
    machine.cpu.s = 0x01FC;
    machine.bus.poke(Addr::new(0, 0x01FD), 0x30);
    machine.bus.poke(Addr::new(0, 0x01FE), 0x34);
    machine.bus.poke(Addr::new(0, 0x01FF), 0x12);
    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.cpu.pc, 0x1234);
    assert_eq!(
        machine.cpu.s, 0x01FF,
        "RTI popped three bytes in emulation mode"
    );
    assert!(
        machine.cpu.p.m() && machine.cpu.p.x(),
        "emulation mode forces M and X"
    );
}

#[test]
fn nmi_wins_over_irq() {
    // Both inputs pending at the same instruction boundary: the 65816 takes the NMI (§2.4's order).
    let patches: [(usize, &[u8]); 3] = [
        (0x1000, &[0xEA, 0x40]), // the NMI handler: nop ; rti
        (0x7FEA, &[0x00, 0x90]), // NMI -> $9000
        (0x7FEE, &[0x00, 0x91]), // IRQ -> $9100
    ];
    let mut machine = machine_with(&patches, &[]);
    machine.bus.io.nmitimen = 0x90; // NMI enable + H-only IRQ
    machine.bus.io.htime = 0x01FF;
    machine.bus.io.nmi_flag = true;
    machine.bus.io.irq_flag = true;
    machine.cpu.p.set_i(false);
    machine.cpu.step(&mut machine.bus);
    assert_eq!(machine.cpu.pc, 0x9000, "the NMI vector won");
    assert!(!machine.bus.nmi_latch, "taking the NMI cleared the latch");
    assert!(
        machine.bus.io.irq_flag,
        "the IRQ is still pending after the NMI"
    );
    // Taking an exception sets I, so the IRQ waits for the handler's RTI to restore the flag byte.
    machine.cpu.step(&mut machine.bus); // the handler's first instruction
    machine.cpu.step(&mut machine.bus); // RTI, which restores I = 0
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        machine.cpu.pc, 0x9100,
        "the IRQ is taken once the NMI handler returns"
    );
}

#[test]
fn multiply_takes_eight_cycles() {
    // fullsnes "SNES Maths Multiply/Divide": "then set WRMPYB, wait 8 clk cycles, then read the
    // 16bit result from Port 4216h-4217h". Eight CPU cycles at 6 master clocks is 48 clocks, and
    // the `$4203` write itself charges 6 of them.
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.write(Addr::new(0, 0x4202), 0x10);
    bus.write(Addr::new(0, 0x4203), 0x10);
    assert_eq!(
        bus.io.mpy_wait,
        48 - 6,
        "the write consumed its own 6 clocks"
    );
    assert_eq!(
        read_port(bus, 0x4216),
        0x00,
        "the product is not readable before the eight cycles are up"
    );
    bus.tick(41);
    assert_eq!(
        read_port(bus, 0x4216),
        0x00,
        "one clock short of eight cycles is still not the product"
    );
    bus.tick(1);
    assert_eq!(read_port(bus, 0x4216), 0x00, "low byte of $0100");
    assert_eq!(read_port(bus, 0x4217), 0x01, "high byte of $0100");
    // The hardware also writes the multiplier into RDDIVL and zeroes RDDIVH.
    assert_eq!(read_port(bus, 0x4214), 0x10);
    assert_eq!(read_port(bus, 0x4215), 0x00);
}

#[test]
fn divide_takes_sixteen_cycles() {
    // fullsnes: "then set WRDIVB, wait 16 clk cycles, then read the 16bit result and/or 16bit
    // remainder from Port 4214h-4217h". Sixteen CPU cycles is 96 master clocks.
    let mut machine = bare();
    let bus = &mut machine.bus;
    bus.write(Addr::new(0, 0x4204), 0x64); // dividend $0064 = 100
    bus.write(Addr::new(0, 0x4205), 0x00);
    bus.write(Addr::new(0, 0x4206), 0x07); // divisor 7
    assert_eq!(
        bus.io.div_wait,
        96 - 6,
        "the write consumed its own 6 clocks"
    );
    assert_eq!(read_port(bus, 0x4214), 0x00, "no result yet");
    bus.tick(89);
    assert_eq!(read_port(bus, 0x4214), 0x00, "one clock short");
    bus.tick(1);
    assert_eq!(read_port(bus, 0x4214), 14, "100 / 7 = 14");
    assert_eq!(read_port(bus, 0x4215), 0, "quotient's high byte");
    assert_eq!(read_port(bus, 0x4216), 2, "100 mod 7 = 2");
    assert_eq!(read_port(bus, 0x4217), 0, "remainder's high byte");

    // Division by zero answers $FFFF and the dividend (fullsnes `$4206`).
    bus.write(Addr::new(0, 0x4206), 0x00);
    bus.tick(96);
    assert_eq!(read_port(bus, 0x4214), 0xFF);
    assert_eq!(read_port(bus, 0x4215), 0xFF);
    assert_eq!(read_port(bus, 0x4216), 0x64);
    assert_eq!(read_port(bus, 0x4217), 0x00);
}

/// The bits `$4210` and `$4212` do not own are open bus.
///
/// fullsnes "SNES Unpredictable Things", the unused-bits table: "4210h 70h RDNMI Bit6-4 are open
/// bus" and "4212h 3Eh HVBJOY Bit5-1 are open bus". A machine that returns zero there hands the
/// cartridge a byte the console never drove — and because [`tendy_snes::bus::Bus::read`] loads the MDR
/// with what it returned, it also scrubs those bits from every later open-bus read.
#[test]
fn the_unused_bits_of_rdnmi_and_hvbjoy_are_open_bus() {
    let mut machine = bare();
    let bus = &mut machine.bus;

    bus.mdr = 0x70;
    assert_eq!(
        read_port(bus, 0x4210) & 0x70,
        0x70,
        "fullsnes: RDNMI's bits 6-4 are open bus, so a $70 on the data bus must read back as $70"
    );

    bus.mdr = 0x3E;
    assert_eq!(
        bus.read(Addr::new(0, 0x4212)) & 0x3E,
        0x3E,
        "fullsnes: HVBJOY's bits 5-1 are open bus, so a $3E on the data bus must read back as $3E"
    );
}

/// A `$4200` store that raises the NMI edge is late for its own instruction: the 65816 samples its
/// interrupt inputs "just before the final CPU cycle of the instruction" (Anomie, S-CPU
/// §Interrupts), and the store *is* that cycle — so the edge lands after the following instruction,
/// which is fullsnes' latch "cleared when the NMI gets executed (which should happen around after
/// the next opcode)".
///
/// The image enables NMI with RDNMI.7 already high (the disable/re-enable hazard), then runs one
/// marked instruction (`inc $0F00`, so "it ran" is a WRAM byte), then `nop`s.
///
/// Fails on the machine before that fix: the NMI is taken at the boundary after the `sta $4200`, so the
/// marked instruction never runs and the handler returns *into* it.
#[test]
fn a_write_raised_nmi_runs_after_the_next_instruction() {
    let patches: [(usize, &[u8]); 2] = [
        (0x1000, &[0xEE, 0x00, 0x10, 0x40]), // inc $1000 ; rti
        (0x7FEA, &[0x00, 0x90]),             // NMI vector -> $9000
    ];
    // lda #$80 ; sta $4200 ; inc $0F00 ; nop
    let code = [0xA9, 0x80, 0x8D, 0x00, 0x42, 0xEE, 0x00, 0x0F, 0xEA];
    let mut machine = machine_with(&patches, &code);
    machine.cpu.p.set_m(true);
    machine.cpu.p.set_x(true);
    machine.bus.ppu.line = 225; // inside v-blank, so the flag is legitimately high
    machine.bus.io.nmi_flag = true;

    machine.cpu.step(&mut machine.bus); // lda #$80
    machine.cpu.step(&mut machine.bus); // sta $4200: the store whose edge is held
    assert!(
        machine.bus.io.nmi_write_edge,
        "the store marked the edge it raised on its own final cycle"
    );

    machine.cpu.step(&mut machine.bus); // the marked instruction
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x0F00)),
        1,
        "the instruction after the store ran before the NMI (Anomie: the check precedes the final cycle)"
    );
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x7E, 0x0008),
        "the marked instruction completed"
    );

    machine.cpu.step(&mut machine.bus); // the NMI, one instruction later
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x00, 0x9000),
        "the write-raised NMI is taken at the boundary after the next instruction"
    );
    machine.cpu.step(&mut machine.bus); // inc $1000
    machine.cpu.step(&mut machine.bus); // rti
    assert_eq!(
        machine.bus.peek(Addr::new(0, 0x1000)),
        1,
        "the handler ran once"
    );
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x7E, 0x0008),
        "the handler resumed at the instruction after the marked one"
    );
}

/// The v-blank-raised path does not move: with NMI already enabled and the flag raised by the PPU,
/// the NMI is still taken at the very first boundary after the v-blank, not one instruction later.
///
/// Passes on the old code too — that is the point of pinning it.
#[test]
fn a_v_blank_raised_nmi_keeps_its_boundary() {
    let patches: [(usize, &[u8]); 1] = [(0x7FEA, &[0x00, 0x90])];
    let mut machine = machine_with(&patches, &nop_main_loop());
    machine.bus.io.nmitimen = 0x80;
    machine.bus.ppu.line = 224;
    machine.bus.ppu.dot = 340;
    machine.bus.tick(4); // V=225, the PPU raises the flag and latches it
    assert!(machine.bus.io.nmi_flag, "the counter reached V=225");
    assert!(
        !machine.bus.io.nmi_write_edge,
        "the PPU raised it, not a store, so nothing is held"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x00, 0x9000),
        "the v-blank-raised NMI keeps the boundary it had before the fix"
    );
}

/// A transfer in progress: fullsnes, the `$4200` paragraph — "if a DMA transfer is in progress,
/// then it is somewhere after the DMA". A delayed enable must not be dispatched inside the
/// transfer; it is the CPU's own internal latch, so it survives the flag going low while the CPU is
/// paused and is taken at the boundary after the transfer.
///
/// Fails on the machine before that fix: the NMI is taken before the `stx $420B` that starts the
/// transfer runs at all, so the transfer never happens.
#[test]
fn a_write_raised_nmi_waits_for_the_dma_to_finish() {
    let patches: [(usize, &[u8]); 1] = [(0x7FEA, &[0x00, 0x90])];
    // ldx #$01 ; lda #$80 ; sta $4200 ; stx $420B ; nop
    let code = [
        0xA2, 0x01, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0x8E, 0x0B, 0x42, 0xEA,
    ];
    let mut machine = machine_with(&patches, &code);
    machine.cpu.p.set_m(true);
    machine.cpu.p.set_x(true);
    arm_dma(&mut machine, 0); // 65 536 bytes: longer than the rest of v-blank
    machine.bus.ppu.line = 225;
    machine.bus.ppu.dot = 0;
    machine.bus.io.nmi_flag = true;

    machine.cpu.step(&mut machine.bus); // ldx #$01
    machine.cpu.step(&mut machine.bus); // lda #$80
    machine.cpu.step(&mut machine.bus); // sta $4200: the edge is held
    let before = machine.bus.master;
    machine.cpu.step(&mut machine.bus); // stx $420B: the whole transfer, inside the write
    assert!(
        machine.bus.master - before >= 65_536 * 8,
        "the transfer ran to completion inside the store (8 master clocks a byte)"
    );
    assert!(
        !machine.bus.io.nmi_flag,
        "the v-blank ended while the DMA had the CPU paused"
    );
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x7E, 0x000A),
        "no NMI was taken inside the transfer: the store finished and the next instruction is due"
    );
    machine.cpu.step(&mut machine.bus);
    assert_eq!(
        (machine.cpu.pb, machine.cpu.pc),
        (0x00, 0x9000),
        "the held edge is taken at the boundary after the transfer"
    );
}
