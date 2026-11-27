//! Raw-RAM cheats, end to end: a running guest cannot defeat a frozen value, pokes are
//! one-shot, and the search helpers answer over a real frame's RAM snapshot.

use tendy_nes::nes::Nes;
use tendy_nes::watch::{Condition, narrow_between, search_between};

use crate::common::program_code;

/// Forever: LDA #$07; STA $0043; LDA $0043; STA $0044; JMP. The guest hammers $0043 with 0x07
/// every pass, and copies the byte it just read back out of it into $0044.
fn hammer_program() -> Vec<u8> {
    program_code(&[
        0xA9, 0x07, 0x85, 0x43, 0xA5, 0x43, 0x85, 0x44, 0x4C, 0x00, 0x80,
    ])
}

#[test]
fn a_running_guest_cannot_defeat_a_freeze() {
    let mut nes = Nes::new(&hammer_program()).unwrap();
    nes.bus.cheat_freeze(0x0043, 0x2A);
    assert!(nes.bus.cheats_armed());
    nes.run_frame();
    nes.run_frame();
    // $0043 survives the STA $0043 writes the loop drives every pass...
    assert_eq!(nes.bus.ram[0x0043], 0x2A);
    // ...and the guest's own LDA $0043 already read the frozen value through, so $0044 is 0x2A
    // too - enforcement happened on the write path, before the next read cycle.
    assert_eq!(nes.bus.ram[0x0044], 0x2A);
}

#[test]
fn unfreezing_hands_the_address_back_to_the_guest() {
    let mut nes = Nes::new(&hammer_program()).unwrap();
    nes.bus.cheat_freeze(0x0043, 0x2A);
    nes.run_frame();
    assert_eq!(nes.bus.ram[0x0043], 0x2A);
    assert!(nes.bus.cheat_unfreeze(0x0043));
    assert!(!nes.bus.cheats_armed());
    nes.run_frame();
    // The guest's own 0x07 wins again within one pass of the loop.
    assert_eq!(nes.bus.ram[0x0043], 0x07);
    assert_eq!(nes.bus.ram[0x0044], 0x07);
}

#[test]
fn pokes_are_oneshot_and_apply_all_reasserts() {
    let mut nes = Nes::new(&hammer_program()).unwrap();
    nes.bus.cheat_poke(0x0043, 0x2A);
    nes.run_frame();
    // A poke is one-shot: the guest overwrote it during the frame.
    assert_eq!(nes.bus.ram[0x0043], 0x07);
    // Per-frame application re-imposes it, the way a snapshot-style frontend drives cheats.
    nes.bus.cheat_apply();
    assert_eq!(nes.bus.ram[0x0043], 0x07, "apply_all is empty-table safe");
    // Once frozen, apply_all (and every write) keeps it at 0x2A from here on.
    nes.bus.cheat_freeze(0x0043, 0x2A);
    nes.bus.cheat_apply();
    assert_eq!(nes.bus.ram[0x0043], 0x2A);
    assert_eq!(nes.bus.frozen_count(), 1);
}

#[test]
fn load_state_does_not_carry_the_cheat_set() {
    let mut nes = Nes::new(&hammer_program()).unwrap();
    nes.bus.cheat_freeze(0x0043, 0x2A);
    nes.run_frame();
    let s = nes.save_state();
    // A second session with its own freeze loads the state: guest memory comes back, the cheat
    // set is session state - $0043's freeze does not ride along, its own one does.
    let mut other = Nes::new(&hammer_program()).unwrap();
    other.bus.cheat_freeze(0x0010, 0x99);
    other.load_state(&s).unwrap();
    assert_eq!(other.bus.ram[0x0043], 0x2A, "RAM comes back from the state");
    assert_eq!(other.bus.frozen_count(), 1);
    assert!(
        !other.bus.cheat_unfreeze(0x0043),
        "$0043's freeze is not part of the state"
    );
}

/// Fill code: LDX #0; loop { TXA; STA $0080,X; INX; CPX #$41; BNE loop }; JMP self. After it,
/// ram[$0080 + X] = X for X in 0..=0x40 (so value 0x40 itself is among the written bytes).
fn fill_program() -> Vec<u8> {
    program_code(&[
        0xA2, 0x00, 0x8A, 0x95, 0x80, 0xE8, 0xE0, 0x41, 0xD0, 0xF8, 0x4C, 0x0A, 0x80,
    ])
}

#[test]
fn search_answers_over_a_real_frame_snapshot() {
    let mut nes = Nes::new(&fill_program()).unwrap();
    nes.run_frame();
    assert_eq!(nes.bus.ram[0x0080], 0x00);
    assert_eq!(nes.bus.ram[0x00C0], 0x40);

    // First pass over the full snapshot (index = RAM address), copied out so the pokes below can
    // borrow the bus again.
    let snapshot: [u8; 0x0800] = *nes.bus.cheat_snapshot();
    assert_eq!(
        search_between(&snapshot, &snapshot, Condition::Equal(0x37)),
        vec![0x00B7]
    );
    let less = search_between(&snapshot, &snapshot, Condition::Less(0x04));
    // The page is zeroed (power-on) except the fill band $0080-$00C0: only the first four fill
    // bytes sit under the target.
    let expected: Vec<u16> = (0..=0x83u16).chain(0xC1..0x0800).collect();
    assert_eq!(less, expected);
    let greater = search_between(&snapshot, &snapshot, Condition::Greater(0x3E));
    assert_eq!(greater, vec![0x00BF, 0x00C0]);

    // A later frame narrows the earlier candidates: only $0082 rises past 0x04 in this listing.
    nes.bus.cheat_poke(0x0081, 0x02);
    nes.bus.cheat_poke(0x0082, 0x09);
    nes.bus.cheat_poke(0x0083, 0x01);
    let after: [u8; 0x0800] = *nes.bus.cheat_snapshot();
    let kept = narrow_between(&snapshot, &after, &less, Condition::Greater(0x04));
    assert_eq!(kept, vec![0x0082]);
}

/// Scope guard: a guest that stores ONLY to addresses outside the work-RAM window. Naming
/// $2005, $4005 and $8005 matters - their low 11 bits collide the frozen $0005 - and $91D9
/// collides $01D9; $8000 collides $0000. If the freeze hook ever ran on one of these stores
/// (it today lives only in the `0x0000..=0x1FFF` arm of `access_write`), the frozen slot at the
/// store's masked index would be re-asserted, and these assertions would catch it.
fn out_of_window_program() -> Vec<u8> {
    program_code(&[
        0xA9, 0x05, 0x8D, 0x00, 0x20, // LDA #$05; STA $2000 - PPUCTRL, no NMI bit
        0xA9, 0x08, 0x8D, 0x14, 0x40, // LDA #$08; STA $4014 - queue an OAM DMA of page $0008
        0xA9, 0xC3, 0x8D, 0x16, 0x40, // LDA #$C3; STA $4016 - joypad strobe
        0xA9, 0xDE, 0x8D, 0x05, 0x20, // LDA #$DE; STA $2005 - PPU scroll (masks to $0005)
        0xA9, 0xF0, 0x8D, 0x05, 0x40, // LDA #$F0; STA $4005 - APU noise (masks to $0005)
        0xA9, 0x11, 0x8D, 0x05, 0x80, // LDA #$11; STA $8005 - PRG write (masks to $0005)
        0xA9, 0x77, 0x8D, 0x00, 0x80, // LDA #$77; STA $8000 - PRG write (masks to $0000)
        0xA9, 0xBE, 0x8D, 0xD9, 0x91, // LDA #$BE; STA $91D9 - PRG write (masks to $01D9)
        0xA9, 0x69, 0x8D, 0x04,
        0x05, // LDA #$69; STA $0504 - in-window, no masked freeze there
        0x4C, 0x2D, 0x80, // JMP $802D - idle forever
    ])
}

/// A freeze must enforce ONLY on writes that land in work RAM or one of its mirrors
/// ($0000-$1FFF). Stores to PPU/APU/PRG addresses share low bits with RAM slots but never touch
/// them. Proven with a poke: right after freezing $0005/$01D9, their slots are re-poked to
/// values other than the frozen ones, so anything that re-asserts the freeze out of window would
/// move the slot and fail this test. (Mutant check that motivates it: moving the hook out of the
/// RAM arm to run for every `access_write` branch with idx = addr & 0x07FF - enforcing, with or
/// without the masked store - passed the whole suite before this test; under it
/// STA $2005/$4005/$8005 re-assert ram[0x0005] back to the frozen 0x2A and STA $91D9
/// re-asserts ram[0x01D9] back to 0x5A.)
#[test]
fn writes_outside_the_ram_window_never_touch_work_ram() {
    let mut nes = Nes::new(&out_of_window_program()).unwrap();
    nes.bus.cheat_freeze(0x0005, 0x2A);
    nes.bus.cheat_freeze(0x01D9, 0x5A);
    nes.bus.cheat_poke(0x0005, 0x07);
    nes.bus.cheat_poke(0x01D9, 0x09);
    assert!(nes.bus.cheats_armed());
    nes.run_frame();
    // Sanity: the guest really ran all the stores above (its unaffected in-window store sticks).
    assert_eq!(
        nes.bus.ram[0x0504], 0x69,
        "the guest's own in-window store went through"
    );
    // The out-of-window stores above mask onto $0005/$01D9 but must leave the poked bytes alone.
    assert_eq!(
        nes.bus.ram[0x0005], 0x07,
        "$2005/$4005/$8005 stores must not reach work RAM"
    );
    assert_eq!(
        nes.bus.ram[0x01D9], 0x09,
        "the $91D9 store must not reach work RAM"
    );
    // ...and the frozen table itself is still armed at both addresses.
    assert_eq!(nes.bus.frozen_count(), 2);
}

/// The other edge of the same scope: mirrors that ARE inside $0000-$1FFF still enforce after the
/// guard. A counter-mutant that "fixes" the hook by narrowing it to the first 2KB page only
/// ($0000-$07FF) would let these stores defeat the freeze.
fn mirror_pounder_program() -> Vec<u8> {
    program_code(&[
        0xA9, 0x77, 0x8D, 0x04,
        0x04, // LDA #$77; STA $0404 - plain RAM (idx $404), not frozen
        0xA9, 0x11, 0x8D, 0x05, 0x08, // LDA #$11; STA $0805 - RAM mirror (masks to $0005)
        0xA9, 0x22, 0x8D, 0x05, 0x10, // LDA #$22; STA $1005 - RAM mirror (masks to $0005)
        0xA9, 0x33, 0x8D, 0x05, 0x18, // LDA #$33; STA $1805 - RAM mirror (masks to $0005)
        0x4C, 0x28, 0x80, // JMP $8028 - idle forever
    ])
}

#[test]
fn ram_mirrors_inside_the_window_still_enforce() {
    let mut nes = Nes::new(&mirror_pounder_program()).unwrap();
    nes.bus.cheat_freeze(0x0005, 0x2A);
    nes.run_frame();
    // Sanity: the guest really ran - its store into the unfrozen plain RAM slot $0404 sticks.
    assert_eq!(
        nes.bus.ram[0x0404], 0x77,
        "an unfrozen RAM store goes through untouched"
    );
    // Each mirror store ($0805/$1005/$1805) landed on the same frozen slot $0005 and got
    // re-asserted: the guest's own 0x11/0x22/0x33 never sticks, in any order.
    assert_eq!(
        nes.bus.ram[0x0005], 0x2A,
        "every in-window mirror write must re-assert the freeze"
    );
    assert_eq!(nes.bus.frozen_count(), 1);
}
