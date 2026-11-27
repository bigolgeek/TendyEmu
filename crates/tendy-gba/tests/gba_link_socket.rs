//! The multiplayer send register over a *socket*, which is the one path where a store and the
//! value the hardware samples can come apart.
//!
//! The in-process [`bench`](tendy_gba::serial::bench) cable keeps one value a unit, so a store that
//! reaches the cable early is overwritten by the next one and nothing is ever stale. Two real
//! [`SocketPort`](tendy_gba::serial::SocketPort)s carry a byte queue instead, and a queue remembers
//! every word a store put on it: a cartridge that writes `SIOMLT_SEND` twice before setting the
//! start bit — GBATEK's procedure allows it (\"Write outgoing data to SIODATA_SEND. Master must set
//! Start bit\") — hands the peer the first write rather than the value the start sampled.
//!
//! Both tests here go through two sockets' worth of the same [`tendy_gb::link::pipe`] transport the
//! localhost cable uses, in multiplayer mode, because that is where the defect lived.

use tendy_gba::Gba;
use tendy_gba::cart::Cartridge;
use tendy_gba::serial::{self, Cable, MULTI_BAUD, SocketPort};

const SIODATA32: u32 = 0x0400_0120;
const SIOMULTI1: u32 = 0x0400_0122;
const SIOCNT: u32 = 0x0400_0128;
const SIOMLT_SEND: u32 = 0x0400_012A;

/// Multiplayer mode at 115 200 bps (the fastest of the four `SIOCNT` rates), with the serial
/// interrupt off: nothing here reads `IF`.
const MODE: u16 = serial::MULTI_MODE | 0b11;

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
    rom[0xC0..0xC4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    rom
}

fn machine() -> Gba {
    Gba::new(Cartridge::new(spin_cart()).expect("a synthesized cartridge"))
}

fn busy(gba: &mut Gba) -> bool {
    gba.bus.read16(SIOCNT) & serial::START != 0
}

/// Step `gba` until it has spent `cycles`, which is how two machines are brought to one instant.
fn run(gba: &mut Gba, cycles: u64) {
    let start = gba.bus.cycles;
    while gba.bus.cycles - start < cycles {
        gba.step_instruction();
    }
}

/// Two machines on two real socket ports, multiplayer mode selected, cycles aligned, nothing armed.
fn socket_pair() -> (Gba, Gba) {
    let (a, b) = tendy_gb::link::pipe();
    let mut parent = machine();
    let mut child = machine();
    parent
        .bus
        .serial
        .attach(Cable::new(SocketPort::new(a, 0)), 0);
    child
        .bus
        .serial
        .attach(Cable::new(SocketPort::new(b, 1)), 1);
    parent.bus.write16(SIOCNT, MODE);
    child.bus.write16(SIOCNT, MODE);
    assert_eq!(
        parent.bus.read16(SIOCNT) & serial::SD_READY,
        serial::SD_READY,
        "both ends must be in multiplayer mode for the cable to be a good connection"
    );
    run(&mut parent, 4_000);
    run(&mut child, 4_000);
    assert_eq!(
        parent.bus.cycles, child.bus.cycles,
        "the two machines must start from the same cycle for the arithmetic below to mean anything"
    );
    (parent, child)
}

/// Arm both ends on the same instant: a byte transport carries no clock, so each unit arms its own
/// transfer (see the module documentation of `crate::serial`).
fn arm(parent: &mut Gba, child: &mut Gba) {
    parent.bus.write16(SIOCNT, MODE | serial::START);
    child.bus.write16(SIOCNT, MODE | serial::START);
}

#[test]
fn a_double_write_before_the_start_sends_the_latest_value_over_a_socket() {
    let (mut parent, mut child) = socket_pair();

    // The cartridge fills the send register twice before it starts, which GBATEK's multiplayer
    // procedure allows: the register is a latch the start bit samples, not a transmit queue.
    parent.bus.write16(SIOMLT_SEND, 0xAAAA);
    parent.bus.write16(SIOMLT_SEND, 0xBBBB);
    assert_eq!(
        parent.bus.read16(SIOMLT_SEND),
        0xBBBB,
        "the register itself holds the second write"
    );
    child.bus.write16(SIOMLT_SEND, 0xCCCC);

    let armed = parent.bus.cycles;
    arm(&mut parent, &mut child);
    let total = serial::multi_cycles(MULTI_BAUD[3], 2);
    run(&mut parent, u64::from(total) + 10_000);
    run(&mut child, u64::from(total) + 10_000);

    println!(
        "parent's SIOMLT_SEND is 0x{:04X} at the start; the child received 0x{:04X}",
        parent.bus.read16(SIOMLT_SEND),
        child.bus.read16(SIODATA32)
    );
    assert_eq!(
        child.bus.read16(SIODATA32),
        0xBBBB,
        "the child must receive the value the start sampled, not the earlier write"
    );
    assert_eq!(
        parent.bus.read16(SIOMULTI1),
        0xCCCC,
        "and the parent must receive the child's one write"
    );

    // Both transfers really ran, on the cycle the multiplayer clock names.
    let crossed = child.bus.serial.transcript[0];
    assert_eq!(crossed.cycle, armed + u64::from(total));
    assert_eq!(crossed.bits, 16);
    assert_eq!(crossed.units, 2);
    assert_eq!(
        child.bus.read16(SIOCNT) & serial::MULTI_ERROR,
        0,
        "the peer answered, so the error bit stays clear"
    );
}

#[test]
fn the_child_is_busy_while_the_masters_transfer_is_in_flight_and_clear_after() {
    let (mut parent, mut child) = socket_pair();
    parent.bus.write16(SIOMLT_SEND, 0x1234);
    child.bus.write16(SIOMLT_SEND, 0x5678);

    let armed = parent.bus.cycles;
    arm(&mut parent, &mut child);
    assert!(
        busy(&mut child),
        "the slave's SIOCNT bit 7 is set as soon as its transfer is armed"
    );

    let total = serial::multi_cycles(MULTI_BAUD[3], 2);
    run(&mut parent, u64::from(total) / 2);
    run(&mut child, u64::from(total) / 2);
    assert_eq!(parent.bus.cycles, child.bus.cycles);
    assert!(
        busy(&mut child),
        "and it stays set while the master's transfer is in flight ({} of {} cycles)",
        child.bus.cycles - armed,
        total
    );
    assert!(busy(&mut parent), "the master is in flight too");

    // Sample the slave's busy bit at every instruction boundary until both ends are idle. A
    // transfer has one falling edge, so a cartridge polling the bit sees one completion.
    let mut seen = Vec::new();
    loop {
        seen.push(busy(&mut child));
        if !busy(&mut child) && !busy(&mut parent) {
            break;
        }
        assert!(
            child.bus.cycles - armed < u64::from(total) * 2,
            "the transfer must end by its own clock"
        );
        if child.bus.cycles <= parent.bus.cycles {
            child.step_instruction();
        } else {
            parent.step_instruction();
        }
    }
    assert!(
        !busy(&mut child),
        "the slave is idle once the transfer ends"
    );
    assert_eq!(
        seen.windows(2).filter(|pair| pair[0] && !pair[1]).count(),
        1,
        "exactly one falling edge on the slave's busy bit"
    );

    // The slave's own transcript says when it was made idle, and what crossed.
    let crossed = child.bus.serial.transcript[0];
    assert_eq!(crossed.cycle, armed + u64::from(total));
    assert_eq!(crossed.units, 2);
    assert_eq!(child.bus.read16(SIODATA32), 0x1234, "the parent's word");
    assert_eq!(child.bus.read16(SIOMULTI1), 0x5678, "its own slot");
}
