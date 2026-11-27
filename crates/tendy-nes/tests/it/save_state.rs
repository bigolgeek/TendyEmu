//! Whole-machine save states: a loaded copy must run bit-identically to the original.

use tendy_nes::controller::BUTTON_A;
use tendy_nes::nes::Nes;
use tendy_nes::state::{STATE_VERSION, StateError};

use crate::common::rom_file;

fn check(name: &str, warm: usize, extra_steps: usize, run: usize, hold_a: bool) {
    let rom = rom_file(name);
    let mut a = Nes::new(&rom).unwrap();
    for f in 0..warm {
        if hold_a {
            a.bus.pads[0].buttons = if (10..=12).contains(&f) { BUTTON_A } else { 0 };
        }
        a.run_frame();
    }
    for _ in 0..extra_steps {
        a.step();
    }
    let s = a.save_state();
    let mut b = Nes::new(&rom).unwrap();
    b.load_state(&s).unwrap();
    assert_eq!(b.save_state(), s, "{name}: re-save differs");
    for f in 0..run {
        if hold_a {
            let v = if (10..=12).contains(&f) { BUTTON_A } else { 0 };
            a.bus.pads[0].buttons = v;
            b.bus.pads[0].buttons = v;
        }
        a.run_frame();
        b.run_frame();
        assert!(
            a.bus.ppu.framebuffer == b.bus.ppu.framebuffer,
            "{name}: framebuffer differs at frame {f}"
        );
        assert!(
            a.bus.ppu.emphasis == b.bus.ppu.emphasis,
            "{name}: emphasis differs at frame {f}"
        );
    }
    assert!(
        a.save_state() == b.save_state(),
        "{name}: final state differs"
    );
}

#[test]
fn accuracycoin_with_input() {
    check("AccuracyCoin.nes", 200, 777, 300, true);
}

#[test]
fn mmc1_a12() {
    check("mmc1_a12.nes", 60, 100, 120, false);
}

#[test]
fn dmc_basics() {
    check("7-dmc_basics.nes", 30, 5, 200, false);
}

#[test]
fn sprdma_and_dmc_dma() {
    check("sprdma_and_dmc_dma.nes", 20, 3, 100, false);
}

#[test]
fn irq_and_dma() {
    check("4-irq_and_dma.nes", 10, 50, 60, false);
}

#[test]
fn bad_states_are_rejected() {
    let mut a = Nes::new(&rom_file("AccuracyCoin.nes")).unwrap();
    a.run_frame();
    let s = a.save_state();
    let mut other = Nes::new(&rom_file("nestest.nes")).unwrap();
    assert_eq!(other.load_state(&s), Err(StateError::RomMismatch));
    assert_eq!(a.load_state(&s[..10]), Err(StateError::Truncated));
    let mut bad = s.clone();
    bad[0] ^= 0xFF;
    assert_eq!(a.load_state(&bad), Err(StateError::BadMagic));
    // A state from an older layout is rejected on its version, before anything is loaded.
    let mut old = s.clone();
    old[4..6].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(a.load_state(&old), Err(StateError::BadVersion(1)));
}

/// A 16 KB NROM image of NOPs whose reset vector points at `$8000`: the CPU walks the PRG and then
/// the zeroed work RAM, so two machines at different step counts have different states.
fn nop_nrom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16_384 + 8_192, 0xEA);
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

/// The header is checked before anything is touched, but a body that stops half way has
/// already overwritten the CPU and part of the bus when `StateReader` reports `Truncated`. The
/// console must come back exactly as it was, not half-loaded with another machine's state.
#[test]
fn a_damaged_state_body_leaves_the_running_machine_untouched() {
    let rom = nop_nrom();
    let mut nes = Nes::new(&rom).unwrap();
    for _ in 0..200 {
        nes.step();
    }
    let before = nes.save_state();

    let mut other = Nes::new(&rom).unwrap();
    for _ in 0..900 {
        other.step();
    }
    let good = other.save_state();

    // Cut short inside the bus block: the header, the CPU and most of the bus load, then it stops.
    let mut truncated = good.clone();
    truncated.truncate(truncated.len() - 24);
    let err = nes
        .load_state(&truncated)
        .expect_err("a cut-short body must be refused");
    assert_eq!(err, StateError::Truncated);
    assert!(
        nes.save_state() == before,
        "a truncated load must not leave the machine half-loaded"
    );

    // Trailing junk fails the length check after the whole body loaded: same rollback.
    let mut long = good.clone();
    long.push(0x00);
    let err = nes
        .load_state(&long)
        .expect_err("a state with trailing bytes must be refused");
    assert!(
        matches!(err, StateError::LengthMismatch { found: 1, .. }),
        "{err:?}"
    );
    assert!(
        nes.save_state() == before,
        "a length mismatch must not leave the machine half-loaded"
    );
}

/// `Nes::load_state` is not atomic, so the version check is
/// the only thing standing between an old-layout state and a half-overwritten machine. A state whose
/// header names an unknown version must be rejected **and** must leave the console byte-identical.
#[test]
fn old_version_is_rejected_before_touching_the_machine() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    a.run_frame();
    let mut old = a.save_state();

    let mut b = Nes::new(&rom).unwrap();
    for _ in 0..3 {
        b.run_frame();
    }
    let before = b.save_state();
    old[4..6].copy_from_slice(&1u16.to_le_bytes());

    assert_eq!(b.load_state(&old), Err(StateError::BadVersion(1)));
    assert_eq!(
        b.save_state(),
        before,
        "a rejected state must not touch the machine"
    );
}

/// The expansion-chip blocks changed the layout then, so a state written at the version
/// before the current one must be rejected on its version — before any of the shorter blocks is
/// read — rather than loaded field by field into the new layout.
#[test]
fn a_state_from_the_previous_layout_version_is_rejected() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    a.run_frame();
    let mut old = a.save_state();
    assert_eq!(
        &old[4..6],
        &STATE_VERSION.to_le_bytes(),
        "the state is written at the current layout version"
    );

    let mut b = Nes::new(&rom).unwrap();
    for _ in 0..3 {
        b.run_frame();
    }
    let before = b.save_state();
    old[4..6].copy_from_slice(&(STATE_VERSION - 1).to_le_bytes());

    assert_eq!(
        b.load_state(&old),
        Err(StateError::BadVersion(STATE_VERSION - 1))
    );
    assert_eq!(
        b.save_state(),
        before,
        "a rejected state must not touch the machine"
    );
}
