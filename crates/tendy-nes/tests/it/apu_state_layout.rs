//! The APU and expansion-chip save-state blocks carry no derived state.
//!
//! Each block's size is pinned. A first cleanup removed fields that were written and restored but
//! never read: `Apu.frame_irq_new` (one byte), the Namco 163's CPU-cycle counter and `$F800`
//! timestamp (two `u64`s) and the VRC7's copy of its 64 register bytes (68 with its `StateWriter`
//! length prefix). A second cleanup removed state that is a function of other state: the VRC7's
//! patch-derived operator parameters (36 bytes of every 49-byte operator, 12 operators) and the
//! cached `level` of the VRC6, Namco 163 and 5B (four bytes each). `Opll::load_state` rebuilds the
//! operator parameters with `refresh_channel`, and the other three rebuild their level from the
//! loaded registers. None of this influenced the emulation, so the size of the block is the only
//! place the removals show — and changing a block is a `STATE_VERSION` bump (`src/state.rs`), so
//! these pins are what makes the deletions stay done.

use tendy_nes::apu::Apu;
use tendy_nes::apu::expansion::{n163::Namco163Audio, opll::Opll, vrc6::Vrc6Audio, ym2149::Ym2149};
use tendy_nes::state::StateWriter;

/// The length of one save-state block.
fn len(write: impl FnOnce(&mut StateWriter)) -> usize {
    let mut w = StateWriter::new();
    write(&mut w);
    w.finish().len()
}

#[test]
fn apu_block_has_no_dead_irq_byte() {
    // 260 before the dead byte went: `frame_irq_new` sat between `frame_irq` and
    // `frame_irq_clear_pending`.
    assert_eq!(
        len(|w| Apu::new().save_state(w)),
        259,
        "Apu::save_state: frame_irq_new is gone, so the block is one byte shorter"
    );
}

#[test]
fn namco163_block_has_no_dead_counters_or_cached_level() {
    // 178 before the dead counters went: the two `u64` write-timestamp words. 162 before the
    // cached `level` went.
    assert_eq!(
        len(|w| Namco163Audio::new().save_state(w)),
        158,
        "Namco163Audio::save_state: cycles and last_addr_cycle (16 bytes) are gone, and the level (4)"
    );
}

#[test]
fn opll_block_has_no_register_copy_or_derived_operators() {
    // 728 before the shadow copy went: it plus `StateWriter`'s four-byte length. 660 before the
    // derived operator parameters went (8 f32s and 4 bools = 36 bytes, 12 operators).
    assert_eq!(
        len(|w| Opll::new().save_state(w)),
        228,
        "Opll::save_state: the 64-byte shadow register file is gone, and the derived operator parameters (432 bytes)"
    );
}

#[test]
fn vrc6_block_has_no_cached_level() {
    // The two pulse channels (7 bytes each), the sawtooth (8) and `$9003` (1); 27 with the old
    // four-byte `level`.
    assert_eq!(
        len(|w| Vrc6Audio::new().save_state(w)),
        23,
        "Vrc6Audio::save_state: no cached level (4 bytes)"
    );
}

#[test]
fn ym2149_block_has_no_cached_level() {
    // The sixteen registers (20 with `StateWriter`'s length), latch, port enable, the three tone
    // counters/levels, noise, envelope and prescaler; 49 with the old four-byte `level`.
    assert_eq!(
        len(|w| Ym2149::new().save_state(w)),
        45,
        "Ym2149::save_state: no cached level (4 bytes)"
    );
}
