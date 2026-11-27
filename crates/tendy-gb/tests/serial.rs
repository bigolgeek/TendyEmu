//! Serial test names: the internal-clock transfer's 1 024 M-cycles, the `$FF` the
//! disconnected line shifts in, and the external-clock transfer that never completes in v1.

mod common;

use tendy_gb::bus::IF_SERIAL;
use tendy_gb::serial::Serial;

/// A serial port with a transfer started on `sb` at the internal clock.
fn started(sb: u8) -> Serial {
    let mut s = Serial::new();
    s.write(0xFF01, sb);
    s.write(0xFF02, 0x81);
    s
}

#[test]
fn internal_transfer_returns_ff() {
    let mut s = started(0x42);
    assert_eq!(s.sb, 0x42, "SB holds the byte to send before the transfer");

    let mut bits = 0;
    while s.bits_left > 0 {
        bits |= s.tick();
    }
    assert_eq!(s.sb, 0xFF, "the disconnected line shifts ones in");
    assert_eq!(
        s.out.last(),
        Some(&0x42),
        "the log holds what the game sent"
    );
    assert_ne!(bits & IF_SERIAL, 0, "the transfer ends with IF.Serial");
    assert_eq!(
        s.read(0xFF02) & 0x80,
        0x00,
        "SC.7 clears when the transfer ends"
    );
    assert_eq!(
        s.read(0xFF02) & 0x7E,
        0x7E,
        "the unused SC bits read as ones"
    );
}

#[test]
fn transfer_takes_1024_mcycles() {
    // 8 192 Hz bit clock over 8 bits: 1 024 M-cycles at 4 T-cycles each.
    let mut s = started(0x55);
    let mut spent = 0u32;
    while s.bits_left > 0 {
        s.tick();
        spent += 1;
        assert!(spent <= 2048, "the transfer never finished");
    }
    assert_eq!(spent, 1024);

    // Every bit is 128 M-cycles: the eighth one is the last, and only it raises IF.
    let mut s = started(0x55);
    for bit in 1..=8 {
        let mut raised = 0;
        for _ in 0..128 {
            raised |= s.tick();
        }
        if bit < 8 {
            assert_eq!(raised & IF_SERIAL, 0, "bit {bit} ends without an interrupt");
            assert_eq!(s.bits_left, 8 - bit);
        } else {
            assert_ne!(raised & IF_SERIAL, 0, "the eighth bit raises IF.Serial");
        }
    }
}

#[test]
fn external_clock_never_completes() {
    let mut s = Serial::new();
    s.write(0xFF01, 0x99);
    s.write(0xFF02, 0x80); // SC.7 set, SC.0 clear: external clock
    assert!(
        s.link_notice,
        "the front end is told the link cable is not emulated"
    );
    assert_eq!(s.bits_left, 0, "nothing is shifting");

    for _ in 0..5000 {
        assert_eq!(s.tick(), 0, "an external-clock transfer raises nothing");
    }
    assert!(s.out.is_empty(), "and it never logs a byte");
    assert_eq!(s.sb, 0x99, "SB is untouched");
    assert_eq!(
        s.read(0xFF02) & 0x80,
        0x80,
        "SC.7 stays set: the transfer never ends"
    );
}
