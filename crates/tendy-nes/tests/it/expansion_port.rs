//! The expansion port. With nothing attached every `$4016`/`$4017` read is unchanged; a
//! test device's D1 / D1-D4 bits are ORed into the reads, it sees the `$4016` OUT bits, and the
//! state (kind byte, u16 length, payload) round-trips.

use tendy_nes::cpu::Bus;
use tendy_nes::expansion::ExpansionDevice;
use tendy_nes::nes::Nes;

fn rom() -> Vec<u8> {
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.resize(16 + 0x4000 + 0x2000, 0);
    rom[16] = 0x4C; // JMP $C000
    rom[17] = 0x00;
    rom[18] = 0xC0;
    for v in [0x3FFA, 0x3FFC, 0x3FFE] {
        rom[16 + v] = 0x00;
        rom[16 + v + 1] = 0xC0;
    }
    rom
}

fn console() -> Nes {
    Nes::new(&rom()).expect("build console")
}

#[test]
fn no_device_leaves_every_read_unchanged() {
    let mut nes = console();
    nes.bus.pads[0].set_buttons(0x01);
    let mut plain = console();
    plain.bus.pads[0].set_buttons(0x01);
    assert_eq!(nes.bus.expansion, ExpansionDevice::None);
    for b in [&mut nes, &mut plain] {
        b.bus.write(0x4016, 1);
        b.bus.write(0x4016, 0);
    }
    for _ in 0..12 {
        assert_eq!(nes.bus.read(0x4016), plain.bus.read(0x4016));
        assert_eq!(nes.bus.read(0x4017), plain.bus.read(0x4017));
    }
    assert_eq!(nes.save_state().len(), plain.save_state().len());
}

#[test]
fn device_bits_appear_in_the_reads() {
    let mut nes = console();
    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0xFF,
        d4_4017: 0xFF,
        out: 0,
    };
    // Pad 1 idle: D0 = 0. $4016 gains only D1; $4017 gains D1-D4.
    let a = nes.bus.read(0x4016);
    assert_eq!(a & 0x1F, 0x02);
    let b = nes.bus.read(0x4017);
    assert_eq!(b & 0x1F, 0x1E);
    // Partial bits: D3 only on $4017; the masks drop everything else.
    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0x00,
        d4_4017: 0x08 | 0xE1,
        out: 0,
    };
    assert_eq!(nes.bus.read(0x4016) & 0x1F, 0x00);
    assert_eq!(nes.bus.read(0x4017) & 0x1F, 0x08);
}

#[test]
fn device_bits_merge_with_the_pad_bit() {
    let mut nes = console();
    nes.bus.pads[0].set_buttons(0x01);
    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0x02,
        d4_4017: 0,
        out: 0,
    };
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    assert_eq!(
        nes.bus.read(0x4016) & 0x1F,
        0x03,
        "pad A on D0 and the device on D1"
    );
}

#[test]
fn the_device_sees_the_out_bits() {
    let mut nes = console();
    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0,
        d4_4017: 0,
        out: 0,
    };
    nes.bus.write(0x4016, 0xFD);
    nes.run_frame();
    match nes.bus.expansion {
        ExpansionDevice::Fixed { out, .. } => assert_eq!(out, 0x05),
        _ => panic!("device vanished"),
    }
}

#[test]
fn state_round_trips_and_detached_block_is_three_zero_bytes() {
    let mut nes = console();
    nes.run_frame();
    let plain = nes.save_state();
    assert_eq!(&plain[4..6], &[12, 0], "STATE_VERSION 12");
    assert_eq!(plain[plain.len() - 3..], [0, 0, 0], "kind 0, length 0");

    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0x02,
        d4_4017: 0x14,
        out: 0x05,
    };
    let saved = nes.save_state();
    assert_eq!(
        saved.len(),
        plain.len() + 3,
        "kind byte + u16 length + 3-byte payload"
    );
    assert_eq!(saved[saved.len() - 6..], [1, 3, 0, 0x02, 0x14, 0x05]);

    let mut fresh = console();
    fresh.load_state(&saved).expect("load");
    assert_eq!(fresh.bus.expansion, nes.bus.expansion);
    // Loading a plain state detaches the device again.
    fresh.load_state(&plain).expect("load");
    assert_eq!(fresh.bus.expansion, ExpansionDevice::None);
}

#[test]
fn famicom_paddle_shows_knob_and_fire() {
    let mut nes = console();
    let mut p = tendy_nes::controller::Paddle::at(0xA5);
    p.fire = true;
    nes.bus.expansion = ExpansionDevice::Paddle(p);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    // Fire on $4016 D1 (not clocked by $4016 reads); knob MSB first, inverted, on $4017 D1.
    let mut got = 0u8;
    for _ in 0..8 {
        assert_eq!(nes.bus.read(0x4016) & 0x1E, 0x02, "fire on $4016 D1");
        let b = nes.bus.read(0x4017);
        assert_eq!(b & 0x1C, 0, "only D1 is driven");
        got = (got << 1) | (!b >> 1 & 1);
    }
    assert_eq!(got, 0xA5);
    assert_eq!(
        nes.bus.read(0x4017) & 0x02,
        0,
        "past the eighth bit reads 0"
    );
    nes.bus.expansion.set_paddle(0xA5, false);
    assert_eq!(nes.bus.read(0x4016) & 0x02, 0, "fire released");
}

#[test]
fn famicom_paddle_state_round_trips() {
    let mut nes = console();
    nes.bus.expansion = ExpansionDevice::Paddle(tendy_nes::controller::Paddle::at(0x3C));
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    nes.bus.read(0x4017);
    let state = nes.save_state();
    let before = nes.bus.expansion.clone();
    nes.bus.expansion = ExpansionDevice::None;
    nes.load_state(&state).expect("load");
    assert_eq!(nes.bus.expansion, before);
}

#[test]
fn power_pad_state_round_trips_with_its_read_position() {
    let mut nes = console();
    nes.bus.attach_power_pad(1);
    nes.bus.set_power_pad(1, 0b1010_0101_0011);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    for _ in 0..3 {
        nes.bus.read(0x4017);
    }
    let state = nes.save_state();
    let before = nes.bus.power_pads.clone();
    let rest: Vec<u8> = (0..8).map(|_| nes.bus.read(0x4017) & 0x18).collect();

    let mut fresh = console();
    fresh.load_state(&state).expect("load");
    assert_eq!(
        fresh.bus.power_pads, before,
        "buttons, latch and read position come back"
    );
    let again: Vec<u8> = (0..8).map(|_| fresh.bus.read(0x4017) & 0x18).collect();
    assert_eq!(
        again, rest,
        "the rest of the stream continues where the state was taken"
    );
    assert!(
        fresh.bus.power_pads[0].is_none(),
        "the empty port stays empty"
    );

    // A state saved with no mat detaches one again, like the other port devices.
    let plain = console().save_state();
    fresh.load_state(&plain).expect("load");
    assert!(fresh.bus.power_pads.iter().all(Option::is_none));
}

// --- The Family BASIC keyboard (NESdev "Family BASIC Keyboard") ---

use tendy_nes::expansion::{FAMILY_KEY_NAMES, FamilyKeyboard, KEYBOARD_ROWS};

/// A `$4016` write the CPU would have seen land: the bus holds the write for a cycle or two and
/// commits it on the next `$4016`/`$4017` read, so back-to-back test writes would overwrite each
/// other. The read it adds is discarded.
macro_rules! out {
    ($nes:expr, $v:expr) => {{
        $nes.bus.write(0x4016, $v);
        let _ = $nes.bus.read(0x4017);
    }};
}

fn keyboard_console(down: &[&str]) -> Nes {
    let mut held = [0u8; KEYBOARD_ROWS];
    for name in down {
        let id = FAMILY_KEY_NAMES
            .iter()
            .position(|n| n == name)
            .expect("known key");
        held[id / 8] |= 1 << (id % 8);
    }
    let mut nes = console();
    nes.bus.expansion = ExpansionDevice::Keyboard(FamilyKeyboard::default());
    nes.bus.expansion.set_keyboard(held);
    nes
}

/// The nesdev scan: `$05` reset, then per row `$04` (column 0), read, `$06` (column 1), read.
/// Returns `$4017` D4-D1 for `rows` rows by two columns.
fn scan(nes: &mut Nes, rows: usize) -> Vec<[u8; 2]> {
    out!(nes, 0x05);
    let mut out = Vec::new();
    for _ in 0..rows {
        out!(nes, 0x04);
        let c0 = nes.bus.read(0x4017) & 0x1E;
        out!(nes, 0x06);
        let c1 = nes.bus.read(0x4017) & 0x1E;
        out.push([c0, c1]);
    }
    out
}

fn scan_from_here(nes: &mut Nes) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..4 {
        out.push(nes.bus.read(0x4017) & 0x1E);
        out!(nes, 0x06);
        out.push(nes.bus.read(0x4017) & 0x1E);
        out!(nes, 0x04);
    }
    out
}

#[test]
fn keyboard_with_no_key_held_reads_all_ones_on_nine_rows_by_two_columns() {
    let mut nes = keyboard_console(&[]);
    let grid = scan(&mut nes, 9);
    assert_eq!(grid.len(), 9);
    assert!(grid.iter().all(|row| *row == [0x1E, 0x1E]), "{grid:?}");
}

#[test]
fn keyboard_one_held_key_clears_exactly_its_row_column_and_line() {
    // S: row 6, column 0, position 1 = $4017 D3. 1: row 7, column 1, position 5 = D3.
    // CLR HOME: row 8, column 0, position 3 = D1. STOP: row 0, column 1, position 0 = D4.
    for (key, row, col, bit) in [
        ("s", 6, 0, 0x08u8),
        ("1", 7, 1, 0x08),
        ("clrhome", 8, 0, 0x02),
        ("stop", 0, 1, 0x10),
    ] {
        let mut nes = keyboard_console(&[key]);
        let grid = scan(&mut nes, 9);
        for (r, cols) in grid.iter().enumerate() {
            for (c, value) in cols.iter().enumerate() {
                let want = if (r, c) == (row, col) {
                    0x1E & !bit
                } else {
                    0x1E
                };
                assert_eq!(*value, want, "key {key}: row {r} column {c}");
            }
        }
    }
}

#[test]
fn keyboard_reset_holds_row_zero_and_only_a_falling_column_bit_steps_the_row() {
    let mut nes = keyboard_console(&["rshift"]); // row 0, column 1, position 2 = D2
    out!(nes, 0x05);
    out!(nes, 0x04); // K did not fall: still row 0
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 0);
    out!(nes, 0x06);
    assert_eq!(
        nes.bus.read(0x4017) & 0x1E,
        0x1E & !0x04,
        "RSHIFT is column 1 D2"
    );
    out!(nes, 0x06); // K stays high: no step
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 0);
    out!(nes, 0x04); // K falls: row 1
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 1);
    out!(nes, 0x06);
    out!(nes, 0x05); // K falls with R high: the reset wins
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 0);
}

#[test]
fn keyboard_tenth_row_reads_ones_then_the_scan_wraps_and_a_disabled_matrix_reads_zero() {
    let mut nes = keyboard_console(&["a"]); // row 6
    let grid = scan(&mut nes, 10);
    assert_eq!(
        grid[9],
        [0x1E, 0x1E],
        "the keyless tenth row (Lode Runner's detection)"
    );
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 9);
    out!(nes, 0x04); // K falls from the last column select: wraps to row 0
    assert_eq!(nes.bus.expansion.keyboard().unwrap().row(), 0);
    out!(nes, 0x00); // matrix disabled
    assert_eq!(
        nes.bus.read(0x4017) & 0x1E,
        0,
        "disabled reads logical 0 always"
    );
}

#[test]
fn keyboard_leaves_the_pad_bits_alone() {
    let mut nes = keyboard_console(&["a"]);
    nes.bus.pads[0].set_buttons(0x01);
    out!(nes, 0x05); // bit 0 also strobes pad 1
    out!(nes, 0x04);
    assert_eq!(
        nes.bus.read(0x4016) & 0x1F,
        0x01,
        "$4016 D1 is the data recorder, not driven"
    );
}

#[test]
fn keyboard_state_round_trips_mid_scan_and_a_stateless_save_still_loads() {
    let mut nes = keyboard_console(&["a", "esc", "space"]);
    out!(nes, 0x05);
    for _ in 0..3 {
        out!(nes, 0x06);
        out!(nes, 0x04);
    }
    let state = nes.save_state();
    let before = nes.bus.expansion.clone();
    let rest = scan_from_here(&mut nes);

    let mut fresh = console();
    fresh.load_state(&state).expect("load");
    assert_eq!(
        fresh.bus.expansion, before,
        "held keys, row and column select come back"
    );
    assert_eq!(
        scan_from_here(&mut fresh),
        rest,
        "the scan continues where the state was taken"
    );

    // A state saved with no device still loads, and detaches the keyboard.
    let plain = console().save_state();
    fresh.load_state(&plain).expect("load");
    assert_eq!(fresh.bus.expansion, ExpansionDevice::None);
}

#[test]
fn keyboard_names_are_72_distinct_keys() {
    assert_eq!(FAMILY_KEY_NAMES.len(), 72);
    let mut sorted = FAMILY_KEY_NAMES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 72);
}
