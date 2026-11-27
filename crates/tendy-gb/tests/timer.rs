//! Timer test names. The timer is driven M-cycle by M-cycle so each case is the
//! tick that matters, not a frame average.

mod common;

use tendy_gb::bus::IF_TIMER;
use tendy_gb::timer::Timer;

/// A timer with the counter zeroed and TIMA cleared, at `tac`.
fn armed(tac: u8) -> Timer {
    let mut t = Timer::new();
    t.write(0xFF07, tac);
    t.write(0xFF04, 0x00); // a DIV write zeroes the whole 16-bit counter
    t.tima = 0x00;
    t.overflow = 0; // and any reload the write itself triggered is not under test
    t.reload_value = 0;
    while t.take_div_apu_edge() {}
    t
}

/// Ticks until TIMA changes, returning the M-cycles spent.
fn ticks_until_tima_moves(t: &mut Timer) -> u32 {
    let start = t.tima;
    for spent in 1..=0x1_0000u32 {
        t.tick();
        if t.tima != start || t.overflow != 0 {
            return spent;
        }
    }
    panic!("TIMA never moved");
}

#[test]
fn div_counts_at_16384_hz() {
    // DIV is counter bits 8-15, and the counter is T-cycles: 4 194 304 / 256 = 16 384 Hz.
    for rate in [0x00u8, 0x04, 0x05] {
        let mut t = armed(rate);
        t.write(0xFF04, 0x00);
        assert_eq!(t.read(0xFF04), 0x00, "DIV restarts at 0");
        for cycle in 1..=1024u32 {
            t.tick();
            // DIV moves on every 256th M-cycle, whatever TAC is doing.
            let expected = (cycle * 4 / 256) as u8;
            assert_eq!(
                t.read(0xFF04),
                expected,
                "DIV after {cycle} M-cycles at TAC=${rate:02X}"
            );
        }
        assert_eq!(t.read(0xFF04), 0x10, "1 024 M-cycles is 4 096 T: DIV = $10");
    }
}

#[test]
fn tac_selects_the_four_periods() {
    // Periods: TAC 00 = 4096 Hz, 01 = 262144, 10 = 65536, 11 = 16384 (Pan Docs, `Timer`).
    // In M-cycles that is 256, 4, 16, 64: the selected counter bit's period divided by four.
    for (rate, period) in [(0x00u8, 256u32), (0x01, 4), (0x02, 16), (0x03, 64)] {
        let mut t = armed(0x04 | rate);
        let spent = ticks_until_tima_moves(&mut t);
        assert_eq!(spent, period, "TAC rate ${rate:02X}: TIMA period");
        // The next increment is the same distance away.
        let spent = ticks_until_tima_moves(&mut t);
        assert_eq!(
            spent, period,
            "TAC rate ${rate:02X}: the period is not a one-off"
        );
    }

    // TAC bit 2 clear stops TIMA entirely, whatever the rate bits say.
    let mut t = armed(0x00);
    for _ in 0..20_000 {
        t.tick();
    }
    assert_eq!(t.tima, 0x00, "TAC.2 = 0 leaves TIMA alone");
}

#[test]
fn div_write_can_tick() {
    // A DIV write zeroes the counter; if the selected bit was set, that is a falling edge and TIMA
    // ticks once (`Timer_Obscure_Behaviour.html`).
    let mut t = armed(0x05); // enabled, TAC 01 -> counter bit 3
    t.tima = 0x10;
    t.counter = 0x0008; // bit 3 set
    t.write(0xFF04, 0x00);
    assert_eq!(t.tima, 0x11, "the DIV write ticked TIMA once");
    assert_eq!(t.read(0xFF04), 0x00);

    // If the selected bit was clear, nothing ticks.
    t.tima = 0x10;
    t.counter = 0x0000;
    t.write(0xFF04, 0x00);
    assert_eq!(t.tima, 0x10, "a clear bit is not a falling edge");

    // With the timer disabled, a DIV write cannot tick either.
    t.tac = 0x01;
    t.tima = 0x10;
    t.counter = 0x0008;
    t.write(0xFF04, 0x00);
    assert_eq!(t.tima, 0x10, "TAC.2 = 0 means no tick");
}

#[test]
fn tac_change_can_tick() {
    // Disabling the timer while the selected bit is set is a falling edge.
    let mut t = armed(0x05);
    t.tima = 0x20;
    t.tick(); // let the counter move to a bit-3-set value
    while t.counter & 0x0008 == 0 {
        t.tick();
    }
    t.tima = 0x20;
    t.write(0xFF07, 0x00); // disable
    assert_eq!(t.tima, 0x21, "disabling with the bit set ticks TIMA");

    // Switching to a rate whose bit is clear is also a falling edge.
    let mut t = armed(0x05); // bit 3
    while t.counter & 0x0008 == 0 {
        t.tick();
    }
    // Keep bit 5 clear at the same time, so the switch is 1 -> 0.
    while t.counter & 0x0028 != 0x0008 {
        t.tick();
    }
    t.tima = 0x30;
    t.write(0xFF07, 0x06); // enabled, TAC 10 -> bit 5
    assert_eq!(t.tima, 0x31, "switching to a clear bit ticks TIMA");

    // Switching to a rate whose bit is already set is not an edge.
    let mut t = armed(0x05);
    while t.counter & 0x0008 == 0 || t.counter & 0x0020 == 0 {
        t.tick();
    }
    t.tima = 0x30;
    t.write(0xFF07, 0x06);
    assert_eq!(t.tima, 0x30, "1 -> 1 is not a falling edge");
}

#[test]
fn overflow_reloads_one_mcycle_later() {
    let mut t = armed(0x04); // 4096 Hz: 256 M-cycles between increments
    t.tma = 0x42;
    t.write(0xFF06, 0x42);
    t.tima = 0xFF;

    let mut bits_at_overflow = 0xFF;
    for _ in 0..300 {
        bits_at_overflow = t.tick();
        if t.tima != 0xFF {
            break;
        }
    }
    assert_eq!(t.tima, 0x00, "the overflow cycle leaves TIMA at $00");
    assert_eq!(
        bits_at_overflow & IF_TIMER,
        0,
        "no interrupt on the overflow cycle itself"
    );

    let reload = t.tick();
    assert_eq!(t.tima, 0x42, "TMA is loaded one M-cycle later");
    assert_ne!(
        reload & IF_TIMER,
        0,
        "and that is when IF.Timer is requested"
    );
    assert_eq!(t.read(0xFF05), 0x42);
}

#[test]
fn tima_write_during_overflow_cancels_reload() {
    let mut t = armed(0x04); // 4096 Hz: the next increment is 256 M-cycles away
    t.write(0xFF06, 0x42);
    t.tima = 0xFF;
    for _ in 0..512 {
        t.tick();
        if t.tima != 0xFF {
            break;
        }
    }
    assert_eq!(t.tima, 0x00, "overflowed");

    let bits = t.write(0xFF05, 0x7F); // write TIMA inside the overflow cycle
    assert_eq!(bits, 0, "a TIMA write raises nothing itself");
    assert_eq!(t.tima, 0x7F);

    let mut bits = 0;
    for _ in 0..300 {
        bits |= t.tick();
    }
    assert_eq!(
        bits & IF_TIMER,
        0,
        "the cancelled reload never raises IF.Timer"
    );
    assert_eq!(t.tima, 0x80, "one increment from $7F: no reload to $42");

    // The other half of §4.4: let a reload complete, then write TIMA in the reload cycle, which is
    // ignored, and one cycle later it is an ordinary write again.
    //
    // `UNVERIFIED:` the emulator places that window in the M-cycle the reload becomes visible in
    // (the write lands between that tick and the next), so it is a boundary, not a T-cycle inside
    // the tick.
    let mut t = armed(0x04);
    t.write(0xFF06, 0x42);
    t.tima = 0xFF;
    for _ in 0..300 {
        t.tick();
        if t.tima != 0xFF {
            break;
        }
    }
    assert_eq!(t.tima, 0x00, "overflowed again");
    assert_ne!(t.tick() & IF_TIMER, 0, "the reload cycle");
    assert_eq!(t.tima, 0x42, "TMA copied in");

    t.write(0xFF05, 0x99);
    assert_eq!(t.tima, 0x42, "a write in the reload cycle is ignored");
    t.tick();
    t.write(0xFF05, 0x77);
    assert_eq!(t.tima, 0x77, "the cycle after it is an ordinary write");
}

#[test]
fn div_bit4_falling_edge_is_512_hz() {
    // Counter bit 12 is set for 4 096 T and clear for 4 096: one falling edge per 8 192 T-cycles,
    // 512 Hz (Audio_details), and 2 048 M-cycles.
    let mut t = Timer::new();
    t.write(0xFF04, 0x00);
    while t.take_div_apu_edge() {}

    let mut edges = Vec::new();
    let mut since = 0u32;
    for _ in 0..5000 {
        t.tick();
        since += 1;
        if t.take_div_apu_edge() {
            edges.push(since);
            since = 0;
        }
    }
    assert!(edges.len() >= 2, "two edges in 5 000 M-cycles");
    for (i, gap) in edges.iter().enumerate() {
        assert_eq!(
            *gap,
            2048,
            "edge {} was {gap} M-cycles after the last",
            i + 1
        );
    }
    assert!(!t.take_div_apu_edge(), "the flag clears when read");
}
