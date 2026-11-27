//! The link port: the registers, the transfer engine, the multiplayer
//! protocol, and what two or three Advances do on an in-process cable.
//!
//! Nothing here needs a window: the machines are real [`Gba`]s running a cartridge that does
//! nothing but spin, and the test pokes the serial registers the way a cartridge would. The
//! two-unit tests go through [`tendy_gb::link::pipe`] — the same byte transport the localhost cable
//! uses — and the multiplayer tests through [`serial::bench`], the in-process cable that carries
//! four units.

use tendy_gb::System;
use tendy_gba::cart::Cartridge;
use tendy_gba::serial::{self, Cable, MULTI_BAUD, bench};
use tendy_gba::{Gba, irq};

const SIODATA32: u32 = 0x0400_0120;
const SIOMULTI1: u32 = 0x0400_0122;
const SIOMULTI2: u32 = 0x0400_0124;
const SIOMULTI3: u32 = 0x0400_0126;
const SIOCNT: u32 = 0x0400_0128;
const SIOMLT_SEND: u32 = 0x0400_012A;
const RCNT: u32 = 0x0400_0134;
const JOYCNT: u32 = 0x0400_0140;
const JOY_RECV: u32 = 0x0400_0150;
const JOY_TRANS: u32 = 0x0400_0154;
const JOYSTAT: u32 = 0x0400_0158;

/// A cartridge whose code starts at `$080000C0` and is one instruction: `b .`.
fn spin_cart() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-LINK\0");
    rom[0xAC..0xB0].copy_from_slice(b"TLNK");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    // b . — branch to the instruction itself, so the machine runs and executes nothing else.
    rom[0xC0..0xC4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    rom
}

fn machine() -> Gba {
    Gba::new(Cartridge::new(spin_cart()).expect("a synthesized cartridge"))
}

fn busy(gba: &mut Gba) -> bool {
    gba.bus.read16(SIOCNT) & serial::START != 0
}

/// Step `gba` until it has spent `cycles`, which is how the tests get two machines to the same
/// instant.
fn run(gba: &mut Gba, cycles: u64) {
    let start = gba.bus.cycles;
    while gba.bus.cycles - start < cycles {
        gba.step_instruction();
    }
}

/// Step two machines in lockstep — whichever is behind takes one instruction — recording the
/// first one's busy bit before every step, and stop when both are idle again.
fn lockstep_until_idle(a: &mut Gba, b: &mut Gba, limit: u64) -> Vec<bool> {
    let start = a.bus.cycles;
    let mut seen = Vec::new();
    loop {
        seen.push(busy(a));
        if !busy(a) && !busy(b) {
            break;
        }
        if a.bus.cycles - start > limit {
            break;
        }
        if a.bus.cycles <= b.bus.cycles {
            a.step_instruction();
        } else {
            b.step_instruction();
        }
    }
    seen
}

/// How many times a `Vec<bool>` goes from true to false.
fn falling(seen: &[bool]) -> usize {
    seen.windows(2).filter(|pair| pair[0] && !pair[1]).count()
}

/// Two machines on one `link::pipe`, both armed at the same instant: the parent on its own clock
/// (normal mode, internal), the child on the external one.
fn pair() -> (Gba, Gba) {
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    let mut child = machine();
    parent.attach_link(Some(a), 0);
    child.attach_link(Some(b), 1);
    parent.bus.write32(SIODATA32, 0x1122_3344);
    child.bus.write32(SIODATA32, 0x5566_7788);
    run(&mut parent, 4_000);
    run(&mut child, 4_000);
    assert_eq!(
        parent.bus.cycles, child.bus.cycles,
        "the two machines must start from the same cycle for the arithmetic below to mean anything"
    );
    parent.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::IRQ_ENABLE | serial::START,
    );
    child.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::IRQ_ENABLE | serial::START,
    );
    (parent, child)
}

#[test]
fn a_normal_transfer_swaps_a_word_between_two_advances_over_the_pipe() {
    let (mut parent, mut child) = pair();
    let armed = parent.bus.cycles;
    assert!(busy(&mut parent), "the parent is busy the moment it starts");
    assert!(
        busy(&mut child),
        "so is the child: SIOCNT bit 7 is the transfer"
    );
    assert!(
        parent.bus.serial.driving_clock(),
        "SIOCNT bit 0 = 1: the parent's clock"
    );
    assert!(
        !child.bus.serial.driving_clock(),
        "the child's bit 0 = 0: the peer's clock"
    );

    let seen = lockstep_until_idle(&mut parent, &mut child, 100_000);
    assert_eq!(falling(&seen), 1, "the busy bit is cleared exactly once");

    assert_eq!(
        parent.bus.read32(SIODATA32),
        0x5566_7788,
        "the child's word"
    );
    assert_eq!(
        child.bus.read32(SIODATA32),
        0x1122_3344,
        "the parent's word"
    );

    // 32 bits at 256 KHz: 16 777 216 / 262 144 = 64 cycles a bit, so 2 048 for the transfer, and
    // both units are in the same window because the parent's clock is what drives it.
    let transfer = 32 * serial::CYCLES_PER_BIT_SLOW;
    assert_eq!(transfer, 2_048);
    assert_eq!(parent.bus.serial.transcript.len(), 1);
    assert_eq!(child.bus.serial.transcript.len(), 1);
    let crossed = parent.bus.serial.transcript[0];
    assert_eq!(crossed.cycle, armed + u64::from(transfer));
    assert_eq!(crossed.bits, 32);
    assert_eq!(crossed.out, 0x1122_3344);
    assert_eq!(crossed.units, 2);
    assert_eq!(
        crossed.inp,
        [0x7788, 0x5566, 0, 0],
        "the answer is the child's word, low half first"
    );
    assert_eq!(
        child.bus.serial.transcript[0].cycle,
        armed + u64::from(transfer),
        "the clocked end finishes on the same cycle the clocking one does"
    );

    // One interrupt each, and it is request 7, the serial one.
    assert_eq!(
        parent.bus.irq_flags() & irq::bit(irq::SERIAL),
        irq::bit(irq::SERIAL)
    );
    assert_eq!(
        child.bus.irq_flags() & irq::bit(irq::SERIAL),
        irq::bit(irq::SERIAL)
    );
    // Acknowledged, it stays acknowledged: the transfer did not raise it twice.
    parent.bus.write16(0x0400_0202, irq::bit(irq::SERIAL));
    child.bus.write16(0x0400_0202, irq::bit(irq::SERIAL));
    run(&mut parent, 4_000);
    run(&mut child, 4_000);
    assert_eq!(parent.bus.irq_flags(), 0, "one transfer, one request");
    assert_eq!(child.bus.irq_flags(), 0);
}

#[test]
fn the_transfer_time_is_the_bit_count_and_the_clock_and_nothing_else() {
    // The same pair, 8 bits at 2 MHz: 8 x 8 = 64 cycles, and then 8 bits at 256 KHz: 512. The
    // difference between the two completions is exactly the difference of the two transfer times,
    // which is what "the cycle counts differ by exactly the transfer time" can mean on a cable
    // that carries bytes and no clock line.
    let mut measured = Vec::new();
    for (length, clock) in [
        (0u16, serial::SC_FAST),
        (0, 0),
        (serial::LENGTH32, serial::SC_FAST),
        (serial::LENGTH32, 0),
    ] {
        let (a, b) = tendy_gb::link::pipe();
        let mut parent = machine();
        let mut child = machine();
        parent.attach_link(Some(a), 0);
        child.attach_link(Some(b), 1);
        parent.bus.write32(SIODATA32, 0xAABB_CCDD);
        child.bus.write32(SIODATA32, 0x1234_5678);
        run(&mut parent, 4_000);
        run(&mut child, 4_000);
        let armed = parent.bus.cycles;
        parent
            .bus
            .write16(SIOCNT, length | clock | serial::SC_INTERNAL | serial::START);
        child.bus.write16(SIOCNT, length | clock | serial::START);
        lockstep_until_idle(&mut parent, &mut child, 100_000);
        let done = parent.bus.serial.transcript[0].cycle - armed;
        let bits = if length == serial::LENGTH32 { 32 } else { 8 };
        assert_eq!(done, u64::from(serial::normal_cycles(length | clock, bits)));
        measured.push(done);
    }
    assert_eq!(measured, vec![64, 512, 256, 2_048]);
    assert_eq!(
        measured[1] - measured[0],
        448,
        "8 bits at 256 KHz minus 2 MHz"
    );
    assert_eq!(
        measured[3] - measured[2],
        1_792,
        "32 bits at 256 KHz minus 2 MHz"
    );
}

#[test]
fn with_no_cable_the_shift_is_the_clock_and_the_byte_is_ones() {
    // A real Advance with nothing in the port: the eight bits shift in ones and the transfer ends
    // on its own clock, with no two-frame wait — there is nobody to wait for.
    let mut gba = machine();
    run(&mut gba, 1_000);
    let armed = gba.bus.cycles;
    gba.bus.write8(SIOMLT_SEND, 0x5A);
    gba.bus.write16(SIOCNT, serial::SC_INTERNAL | serial::START);
    assert!(busy(&mut gba));
    run(&mut gba, 4_000);
    assert!(!busy(&mut gba));
    assert_eq!(gba.bus.read8(SIOMLT_SEND), 0xFF, "the line reads high");
    assert_eq!(gba.bus.serial.transcript[0].cycle, armed + 512);
    assert_eq!(gba.bus.serial.transcript[0].units, 1);
    assert!(gba.bus.serial.message.is_none(), "no cable, nothing to say");
}

#[test]
fn a_peer_that_never_answers_costs_two_frames_and_then_ones() {
    // The cable is up — the port says two units — but the other machine never arms. The transfer
    // still ends: the shift is 2 048 cycles, then the answer gets the same two-frame budget the
    // Game Boy's cable gives it, and then the register holds ones.
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    parent.attach_link(Some(a), 0);
    let _quiet = b; // held, never armed
    parent.bus.write32(SIODATA32, 0xCAFE_F00D);
    run(&mut parent, 1_000);
    let armed = parent.bus.cycles;
    parent.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );

    let frames_before = parent.bus.lcd.frames;
    run(&mut parent, 700_000);
    assert!(!busy(&mut parent), "no hang: the unit gives up");
    assert_eq!(parent.bus.read32(SIODATA32), 0xFFFF_FFFF);
    let crossed = parent.bus.serial.transcript[0];
    let waited = crossed.cycle - armed - 2_048;
    assert!(
        (u64::from(serial::LINK_TIMEOUT_CYCLES)..u64::from(serial::LINK_TIMEOUT_CYCLES) + 200)
            .contains(&waited),
        "waited {waited} cycles, which should be two frames ({}) plus a poll",
        serial::LINK_TIMEOUT_CYCLES
    );
    assert_eq!(
        parent.bus.serial.message,
        Some(serial::Message::PeerSilent),
        "and the player is told once"
    );
    println!(
        "silent peer: armed {armed}, shift 2048, gave up after {} cycles ({} = two frames)",
        crossed.cycle - armed,
        serial::LINK_TIMEOUT_CYCLES
    );
    assert!(
        parent.bus.lcd.frames > frames_before + 1,
        "the machine kept running"
    );
}

#[test]
fn a_peer_that_leaves_mid_transfer_ends_it_and_says_so() {
    // The other end of the pipe is dropped while the transfer waits: the unit notices at its next
    // look at the cable, ends with ones, and the player is told once — the Game Boy's `PeerGone`,
    // for the Advance's port.
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    let mut child = machine();
    child.attach_link(Some(b), 1);
    parent.attach_link(Some(a), 0);
    parent.bus.write32(SIODATA32, 0x1122_3344);
    run(&mut parent, 1_000);
    let armed = parent.bus.cycles;
    parent.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );
    assert!(busy(&mut parent));
    drop(child);

    run(&mut parent, 4_000);
    assert!(!busy(&mut parent), "the transfer ended with the cable");
    assert_eq!(parent.bus.serial.message, Some(serial::Message::PeerGone));
    assert_eq!(parent.bus.read32(SIODATA32), 0xFFFF_FFFF);
    assert_eq!(
        parent.bus.serial.transcript[0].cycle,
        armed + 2_048,
        "noticed on the cycle the shift ended, not two frames later"
    );
}

#[test]
fn three_units_on_one_cable_get_the_same_four_slots() {
    let cables = bench(3);
    let values = [0x1111u16, 0x2222, 0x3333];
    let mut units: Vec<Gba> = cables
        .into_iter()
        .enumerate()
        .map(|(slot, cable)| {
            let mut gba = machine();
            gba.bus.serial.attach(cable, slot);
            gba
        })
        .collect();
    for gba in &mut units {
        run(gba, 2_000);
    }
    let armed = units[0].bus.cycles;

    // Every unit selects multiplayer mode, writes its halfword, and then starts, all before any of
    // them runs: a byte transport has no clock line, so each unit arms its own transfer, and the
    // round on the in-process cable wants every value present. On hardware the parent starts and
    // the children merely receive (see the report).
    for (slot, gba) in units.iter_mut().enumerate() {
        let mode = serial::MULTI_MODE | 0b11 | serial::IRQ_ENABLE;
        gba.bus.write16(SIOCNT, mode);
        gba.bus.write16(SIOMLT_SEND, values[slot]);
        gba.bus.write16(SIOCNT, mode | serial::START);
        assert!(busy(gba), "unit {slot} is busy");
    }

    let total = serial::multi_cycles(MULTI_BAUD[3], 3);
    for gba in &mut units {
        run(gba, u64::from(total) + 10_000);
    }
    println!("three units, {total} cycles a transfer; armed on cycle {armed}");
    for (slot, gba) in units.iter_mut().enumerate() {
        let crossed = gba.bus.serial.transcript[0];
        println!(
            "unit {slot}: frame {} cycle {} slots [{:04X}, {:04X}, {:04X}, {:04X}] units {}",
            crossed.frame,
            crossed.cycle,
            crossed.inp[0],
            crossed.inp[1],
            crossed.inp[2],
            crossed.inp[3],
            crossed.units
        );
    }

    for (slot, gba) in units.iter_mut().enumerate() {
        assert!(!busy(gba), "unit {slot} finished");
        assert_eq!(
            gba.bus.read16(SIODATA32),
            values[0],
            "slot 0 is the parent's"
        );
        assert_eq!(
            gba.bus.read16(SIOMULTI1),
            values[1],
            "slot 1 is the first child's"
        );
        assert_eq!(
            gba.bus.read16(SIOMULTI2),
            values[2],
            "slot 2 is the second child's"
        );
        assert_eq!(gba.bus.read16(SIOMULTI3), 0xFFFF, "no fourth unit");
        let siocnt = gba.bus.read16(SIOCNT);
        assert_eq!(
            (siocnt >> 4) & 0b11,
            slot as u16,
            "the ID bits name the slot"
        );
        assert_eq!(siocnt & serial::MULTI_ERROR, 0, "every unit answered");
        let crossed = gba.bus.serial.transcript[0];
        assert_eq!(crossed.bits, 16);
        assert_eq!(crossed.cycle, armed + u64::from(total));
        assert_eq!(crossed.inp[0], values[0]);
        assert_eq!(crossed.inp[1], values[1]);
        assert_eq!(crossed.inp[2], values[2]);
        assert_eq!(crossed.inp[3], 0xFFFF);
    }
}

#[test]
fn a_multiplayer_unit_that_never_answers_sets_the_error_bit_and_does_not_hang() {
    let cables = bench(3);
    let mut parent = machine();
    parent
        .bus
        .serial
        .attach(cables.into_iter().next().expect("one end"), 0);
    // The other two ends are dropped on the floor: they are on the cable as far as the port's
    // unit count goes, and they never answer.
    run(&mut parent, 2_000);
    let armed = parent.bus.cycles;
    let mode = serial::MULTI_MODE | 0b11;
    parent.bus.write16(SIOCNT, mode);
    parent.bus.write16(SIOMLT_SEND, 0xBEEF);
    parent.bus.write16(SIOCNT, mode | serial::START);

    let total = serial::multi_cycles(MULTI_BAUD[3], 3);
    run(&mut parent, u64::from(total) + 10_000);
    assert!(
        !busy(&mut parent),
        "the clock ends the transfer, answered or not"
    );
    assert_eq!(
        parent.bus.read16(SIODATA32),
        0xBEEF,
        "the parent's own slot"
    );
    assert_eq!(parent.bus.read16(SIOMULTI1), 0xFFFF);
    assert_eq!(parent.bus.read16(SIOMULTI2), 0xFFFF);
    assert_ne!(
        parent.bus.read16(SIOCNT) & serial::MULTI_ERROR,
        0,
        "SIOCNT bit 6 says a unit did not answer"
    );
    assert_eq!(
        parent.bus.serial.transcript[0].cycle,
        armed + u64::from(total),
        "one transfer's worth of clock, not a wait"
    );
}

#[test]
fn the_registers_read_back_what_the_hardware_documents() {
    let mut gba = machine();
    // SIOCNT, normal mode: bit 15 always 0, bit 13 (multiplayer) 0, bits 4-6 read 0, bit 2 is the
    // live SI line (high with nothing plugged in) — so 5E7F reads 5E0F.
    gba.bus.write16(SIOCNT, 0x5E7F);
    assert_eq!(gba.bus.read16(SIOCNT), 0x5E0F);
    // In multiplayer mode the low bits are the baud rate, bits 2-7 are live, bit 12 must read 0.
    gba.bus.write16(SIOCNT, 0x6F7B);
    assert_eq!(gba.bus.read16(SIOCNT), 0x6F03);
    // RCNT: bits 9-13 read 0 and bit 15 must be zero; SC, SI and SO read high while the port
    // idles with nothing plugged in.
    gba.bus.write16(RCNT, 0xFFFF);
    assert_eq!(gba.bus.read16(RCNT), 0x41FD);
    // JOYCNT: bits 0-2 are flags acknowledged by writing a 1, so with no JOY bus device there is
    // nothing behind them to clear; bit 6 is the interrupt enable and is storage.
    gba.bus.write8(JOYCNT, 0x47);
    assert_eq!(gba.bus.read8(JOYCNT), 0x40);
    gba.bus.write8(JOYCNT, 0x00);
    assert_eq!(
        gba.bus.read8(JOYCNT),
        0x40,
        "a store of 0 leaves the flags alone"
    );
    // JOYSTAT: bits 4-5 are general-purpose storage, bit 1 rises on a JOY_TRANS write, and bit 3
    // falls when JOY_RECV is read.
    gba.bus.write8(JOYSTAT, 0b11_1000);
    assert_eq!(gba.bus.read8(JOYSTAT), 0b11_1000);
    gba.bus.write8(JOY_TRANS, 0x12);
    assert_eq!(gba.bus.read8(JOYSTAT), 0b11_1010);
    assert_eq!(gba.bus.read8(JOY_RECV), 0);
    assert_eq!(gba.bus.read8(JOYSTAT), 0b11_0010);
}

#[test]
fn a_state_never_holds_a_transfer_so_load_drops_the_busy_bit_and_keeps_the_cable() {
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    let mut child = machine();
    parent.attach_link(Some(a), 0);
    child.attach_link(Some(b), 1);
    parent.bus.write32(SIODATA32, 1);
    child.bus.write32(SIODATA32, 2);
    run(&mut parent, 1_000);
    run(&mut child, 1_000);
    parent.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );
    child.bus.write16(SIOCNT, serial::LENGTH32 | serial::START);
    assert!(busy(&mut parent));

    let bytes = parent.save_state();
    parent
        .load_state(&bytes)
        .expect("a state this machine wrote");
    assert!(!busy(&mut parent), "the transfer did not survive the state");
    assert_eq!(parent.bus.read32(SIODATA32), 1, "and the data register did");
    // The child's own transfer was already on the wire; it finishes and takes what the parent put
    // there before the state was written.
    run(&mut child, 4_000);
    assert_eq!(child.bus.read32(SIODATA32), 1);

    // The cable is session state, so it is still plugged in: a second transfer still crosses.
    parent.bus.write32(SIODATA32, 0x0BAD_CAFE);
    child.bus.write16(SIOCNT, serial::LENGTH32 | serial::START);
    parent.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );
    lockstep_until_idle(&mut parent, &mut child, 100_000);
    assert_eq!(child.bus.read32(SIODATA32), 0x0BAD_CAFE);
}

#[test]
fn the_transcript_prints_the_bytes_and_the_frame_each_transfer_completed_on() {
    // Eight 8-bit transfers from the parent, the child answering with a count. This is the
    // transcript the report prints: it comes from the in-process cable, not from two windows.
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    let mut child = machine();
    parent.attach_link(Some(a), 0);
    child.attach_link(Some(b), 1);
    run(&mut parent, 4_000);
    run(&mut child, 4_000);
    for round in 0..8u8 {
        parent.bus.write8(SIOMLT_SEND, 0x40 + round);
        child.bus.write8(SIOMLT_SEND, 0x90 + round);
        parent.bus.write16(
            SIOCNT,
            serial::SC_INTERNAL | serial::SC_FAST | serial::START,
        );
        child.bus.write16(SIOCNT, serial::SC_FAST | serial::START);
        lockstep_until_idle(&mut parent, &mut child, 10_000);
        // A frame between transfers, so the transcript's frame numbers move: this is what a
        // two-player session's log would look like.
        run(&mut parent, 280_896);
        run(&mut child, 280_896);
    }
    assert_eq!(parent.bus.serial.transcript.len(), 8);
    assert_eq!(child.bus.serial.transcript.len(), 8);
    println!("frame  cycle    bits  sent  got   units");
    for crossed in &parent.bus.serial.transcript {
        println!(
            "{:5}  {:8}  {:4}  0x{:02X}  0x{:02X}  {}",
            crossed.frame,
            crossed.cycle,
            crossed.bits,
            crossed.out,
            crossed.inp[0] as u8,
            crossed.units
        );
        assert_eq!(crossed.bits, 8);
    }
    for (round, crossed) in parent.bus.serial.transcript.iter().enumerate() {
        assert_eq!(crossed.out, 0x40 + round as u32);
        assert_eq!(crossed.inp[0], 0x90 + round as u16);
        assert_eq!(crossed.units, 2);
    }
    for (round, crossed) in child.bus.serial.transcript.iter().enumerate() {
        assert_eq!(crossed.out, 0x90 + round as u32);
        assert_eq!(crossed.inp[0], 0x40 + round as u16);
    }
}

#[test]
fn the_multiplayer_bits_of_siocnt_are_read_only_where_the_hardware_says_they_are() {
    let cables = bench(2);
    let mut parent = machine();
    let mut child = machine();
    for (slot, gba) in [&mut parent, &mut child].into_iter().enumerate() {
        gba.bus.serial.attach(cables[slot].clone(), slot);
        gba.bus.write16(SIOCNT, serial::MULTI_MODE);
    }
    // A store cannot fake the terminal bits: a child reads bit 2 set whatever it writes.
    assert_ne!(child.bus.read16(SIOCNT) & (1 << 2), 0, "1 = child");
    assert_eq!(parent.bus.read16(SIOCNT) & (1 << 2), 0, "0 = parent");
    // SD says both ends are in multiplayer mode.
    assert_ne!(parent.bus.read16(SIOCNT) & serial::SD_READY, 0);
    // Before the first transfer the ID bits are undefined; GBATEK models that as 0.
    assert_eq!(parent.bus.read16(SIOCNT) & 0b11_0000, 0);
    assert_eq!(child.bus.read16(SIOCNT) & 0b11_0000, 0);
}

#[test]
fn the_request_bits_name_the_serial_interrupt() {
    assert_eq!(irq::bit(irq::SERIAL), 1 << 7);
    assert_eq!(irq::name(irq::SERIAL), "Serial");
}

#[test]
fn the_cables_are_what_the_engine_thinks_they_are() {
    // The bench reports its units; a lone machine has none. This is the trait the wiring card will
    // implement a socket on, so it gets a test of its own shape.
    let cables: Vec<Cable> = bench(4);
    assert_eq!(cables.len(), 4);
    let (a, _b) = tendy_gb::link::pipe();
    let mut port = serial::SocketPort::new(a, 0);
    assert_eq!(serial::LinkPort::lines(&mut port).units, 2);
    let lone = serial::Lines::default();
    assert_eq!(lone.units, 1);
    assert!(!lone.ready);
}
