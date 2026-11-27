//! Interrupt test names: the halt wake-up, IF/IE plumbing, and the five-M-cycle
//! dispatch, driven through the real `Bus` so IF and IE are the registers the ROM sees.

mod common;

use common::{entry, machine, synth_rom};
use tendy_gb::Machine;
use tendy_gb::bus::{IF_JOYPAD, IF_SERIAL, IF_STAT, IF_TIMER, IF_VBLANK};
use tendy_gb::cpu::{Cpu, Flags};
use tendy_gb::joypad::Buttons;

/// A post-boot machine with no interrupt armed and PC in WRAM.
fn quiet() -> Machine {
    let mut m = machine(synth_rom(&entry(&[])));
    m.cpu = Cpu::post_boot(0);
    m.bus.ie = 0;
    m.bus.iflag = 0;
    m.cpu.pc = 0xC000;
    m.bus.wram[0] = 0x00; // nop
    m
}

#[test]
fn vblank_wakes_halt() {
    let mut m = quiet();
    m.cpu.halted = true;
    assert_eq!(m.cpu.step(&mut m.bus), 1, "a halted step is one M-cycle");
    assert!(m.cpu.halted, "nothing pending: still halted");

    m.bus.request(IF_VBLANK); // IE is still 0, so this must not count as pending
    assert_eq!(m.cpu.step(&mut m.bus), 1);
    assert!(m.cpu.halted, "IE = 0 means IF is not a wake-up");
    assert_eq!(m.bus.iflag, IF_VBLANK, "but IF does latch the request");

    m.bus.ie = IF_VBLANK;
    assert_eq!(m.cpu.step(&mut m.bus), 1);
    assert!(!m.cpu.halted, "the VBlank request wakes HALT");
    assert_eq!(m.cpu.pc, 0xC000, "waking is not dispatching");

    // With IME set the same request dispatches on the next step.
    m.cpu.ime = true;
    let cycles = m.cpu.step(&mut m.bus);
    assert_eq!(cycles, 5);
    assert_eq!(m.cpu.pc, 0x40);
    assert_eq!(m.bus.iflag & IF_VBLANK, 0, "the serviced bit is cleared");
}

#[test]
fn lowest_pending_bit_wins() {
    for (pending, vector) in [
        (IF_VBLANK | IF_SERIAL, 0x40u16), // bit 0
        (IF_STAT | IF_TIMER, 0x48),       // bit 1
        (IF_TIMER | IF_JOYPAD, 0x50),     // bit 2
        (IF_JOYPAD | 0x20, 0x60),         // bit 4
        (IF_SERIAL, 0x58),                // bit 3
    ] {
        let mut m = quiet();
        m.bus.ie = 0x1F;
        m.bus.iflag = pending;
        m.cpu.ime = true;
        m.cpu.step(&mut m.bus);
        assert_eq!(m.cpu.pc, vector, "IF=${pending:02X}");
        assert_eq!(
            m.bus.iflag,
            pending & !(vector_word(vector)),
            "only one bit is cleared for IF=${pending:02X}"
        );
    }
}

/// The IF bit a vector belongs to: `($40 - $40) / 8` … `($60 - $40) / 8`.
fn vector_word(vector: u16) -> u8 {
    1 << ((vector - 0x40) / 8)
}

#[test]
fn dispatch_costs_five_mcycles() {
    let mut m = quiet();
    m.bus.ie = IF_VBLANK;
    m.bus.iflag = IF_VBLANK;
    m.cpu.ime = true;
    m.cpu.sp = 0xFFFE;
    let before = m.bus.cycles;

    let cycles = m.cpu.step(&mut m.bus);
    assert_eq!(cycles, 5, "the dispatch is five M-cycles");
    assert_eq!(m.bus.cycles - before, 5, "and it ticked exactly that many");

    // Two bytes pushed, high half first, return address = the instruction it did not run.
    assert_eq!(m.cpu.sp, 0xFFFC);
    assert_eq!(m.bus.hram[0x7D], 0xC0, "high byte at $FFFD");
    assert_eq!(m.bus.hram[0x7C], 0x00, "low byte at $FFFC");

    // RETI comes back and re-enables IME.
    m.bus.wram[0] = 0xD9;
    m.cpu.pc = 0xC000;
    m.cpu.ime = true;
    let cycles = m.cpu.step(&mut m.bus);
    assert_eq!(cycles, 4, "RETI is four M-cycles");
    assert_eq!(m.cpu.pc, 0xC000, "RETI returns to the pushed address");
    assert!(m.cpu.ime, "RETI sets IME");
}

#[test]
fn if_upper_bits_read_one() {
    let mut m = quiet();
    m.bus.iflag = 0;
    assert_eq!(
        m.bus.read(0xFF0F),
        0xE0,
        "IF reads $E0 when nothing is requested"
    );

    m.bus.request(IF_TIMER | IF_JOYPAD);
    assert_eq!(m.bus.read(0xFF0F), 0xE0 | IF_TIMER | IF_JOYPAD);

    // A write stores only bits 4-0.
    m.bus.write(0xFF0F, 0xFF);
    assert_eq!(m.bus.iflag, 0x1F, "the upper three bits are not storage");
    assert_eq!(m.bus.read(0xFF0F), 0xFF, "and they read as ones");

    m.bus.write(0xFF0F, 0x00);
    assert_eq!(
        m.bus.read(0xFF0F),
        0xE0,
        "a write of 0 clears every request"
    );

    // IE is the one `$FFxx` register where `bits/unused_hwio-GS` breaks its own "unused bits read
    // as ones" rule: `test IE %11100000 %11100000 %11100000` writes `$E0` and expects `$E0` back,
    // so all eight bits are storage.
    m.bus.write(0xFFFF, 0xFF);
    assert_eq!(m.bus.ie, 0xFF, "IE keeps every bit");
    assert_eq!(m.bus.read(0xFFFF), 0xFF, "and reads it back as stored");
    m.bus.write(0xFFFF, 0x10);
    assert_eq!(m.bus.ie, 0x10, "only the written bit is stored");
    assert_eq!(m.bus.read(0xFFFF), 0x10, "read back as stored");
}

#[test]
fn joypad_edge_requests_interrupt() {
    let mut m = quiet();
    m.bus.write(0xFF00, 0x20); // bit 5 high, bit 4 low: the d-pad row is selected
    assert_eq!(m.bus.read(0xFF00) & 0x0F, 0x0F, "nothing pressed");

    m.set_buttons(Buttons::RIGHT);
    assert_ne!(
        m.bus.iflag & IF_JOYPAD,
        0,
        "pressing a selected button requests IF.Joypad"
    );
    assert_eq!(m.bus.read(0xFF00) & 0x0F, 0x0E, "Right pulls bit 0 low");

    // Holding it does not request again.
    m.bus.iflag = 0;
    m.set_buttons(Buttons::RIGHT);
    assert_eq!(m.bus.iflag & IF_JOYPAD, 0, "only the falling edge requests");

    // Releasing is 0 -> 1: no request.
    m.set_buttons(Buttons::NONE);
    assert_eq!(m.bus.iflag & IF_JOYPAD, 0, "release is not an edge down");
    assert_eq!(m.bus.read(0xFF00) & 0x0F, 0x0F);

    // A button in a row that is not selected does not reach the matrix.
    m.bus.write(0xFF00, 0x20); // d-pad row selected
    m.bus.iflag = 0;
    m.set_buttons(Buttons::A);
    assert_eq!(m.bus.read(0xFF00) & 0x0F, 0x0F, "A is not in the d-pad row");
    assert_eq!(
        m.bus.iflag & IF_JOYPAD,
        0,
        "and so it is not an edge on P10-P13"
    );

    // Selecting the button row makes the held A visible, and that write is itself the edge.
    m.bus.write(0xFF00, 0x10); // bit 4 low, bit 5 high: the button row
    assert_eq!(m.bus.read(0xFF00) & 0x0F, 0x0E, "A pulls bit 0 low here");
    assert_ne!(
        m.bus.iflag & IF_JOYPAD,
        0,
        "the select write is a falling edge too"
    );
}

#[test]
fn ime_zero_still_wakes_halt() {
    let mut m = quiet();
    m.bus.ie = IF_TIMER;
    m.cpu.ime = false;
    m.cpu.halted = true;
    assert_eq!(m.cpu.step(&mut m.bus), 1);
    assert!(m.cpu.halted, "nothing pending yet");

    m.bus.request(IF_TIMER);
    assert_eq!(m.cpu.step(&mut m.bus), 1);
    assert!(!m.cpu.halted, "IME = 0 still wakes");
    assert_eq!(m.cpu.pc, 0xC000, "but nothing is dispatched");
    assert_eq!(m.cpu.sp, 0xFFFE, "and nothing is pushed");
    assert_eq!(m.bus.iflag, IF_TIMER, "the request stays latched");

    // The very next step runs the instruction, still with IME = 0.
    m.cpu.f = Flags(0);
    m.bus.wram[0] = 0x3C; // inc a
    let a0 = m.cpu.a;
    m.cpu.step(&mut m.bus);
    assert_eq!(m.cpu.a, a0 + 1, "execution resumed without a dispatch");
}

#[test]
fn interrupt_service_clears_halt() {
    let mut m = quiet();
    m.bus.wram[0] = 0x76; // halt
    m.bus.ie = IF_VBLANK;
    m.cpu.ime = true;
    m.cpu.step(&mut m.bus); // HALT with nothing pending
    assert!(m.cpu.halted, "HALT is entered");

    m.bus.request(IF_VBLANK);
    m.cpu.step(&mut m.bus); // the dispatch
    assert_eq!(m.cpu.pc, 0x40, "the handler runs");
    assert!(!m.cpu.halted, "servicing the interrupt ends HALT");
}
