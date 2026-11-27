//! RAM search / RAM watch, end to end: a watch list follows a real guest's variables across
//! frames, observing them does not perturb the emulation, and a search over two real snapshots
//! finds the counter the guest is driving.

use tendy_nes::nes::Nes;
use tendy_nes::watch::{Condition, RamWatch, narrow_between, search_between};

use crate::common::program_code;

/// Forever: INC $0043; JMP $8000 - a counter climbing once per loop pass, plus a byte that never
/// moves ($0044 stays 0), so the watch list has one address of each kind.
fn counter_program() -> Vec<u8> {
    program_code(&[0xE6, 0x43, 0x4C, 0x00, 0x80])
}

#[test]
fn a_watch_list_follows_a_running_guest() {
    let mut nes = Nes::new(&counter_program()).unwrap();
    let mut watch = RamWatch::new();
    assert!(watch.add(0x0043));
    assert!(watch.add(0x0044));

    // First poll: a reading, but nothing to compare against yet.
    watch.poll(&mut nes.bus);
    let first = watch.rows()[0].value();
    assert_eq!(first, nes.bus.ram[0x0043]);
    assert!(!watch.any_changed());

    let mut moved = 0;
    for _ in 0..5 {
        nes.run_frame();
        watch.poll(&mut nes.bus);
        // The reading is the guest's real byte, not a stale copy.
        assert_eq!(watch.rows()[0].value(), nes.bus.ram[0x0043]);
        // $0044 is never written by this program.
        assert_eq!(watch.rows()[1].value(), 0);
        assert!(!watch.rows()[1].changed());
        if watch.rows()[0].changed() {
            moved += 1;
        }
    }
    assert_eq!(moved, 5, "the counter moves every frame");
}

#[test]
fn watching_does_not_perturb_the_emulation() {
    let mut watched = Nes::new(&counter_program()).unwrap();
    let mut plain = Nes::new(&counter_program()).unwrap();
    let mut watch = RamWatch::new();
    watch.add(0x0043);

    for _ in 0..20 {
        watched.run_frame();
        // Polling peeks the bus: no cycle is clocked, so the two machines must stay in lockstep.
        watch.poll(&mut watched.bus);
        plain.run_frame();
    }
    assert_eq!(watched.save_state(), plain.save_state());
}

#[test]
fn a_search_over_two_frames_narrows_to_the_counter() {
    let mut nes = Nes::new(&counter_program()).unwrap();
    let mut watch = RamWatch::new();
    watch.add(0x0043);

    nes.run_frame();
    let before = nes.bus.cheat_snapshot().to_vec();
    let candidates = search_between(&before, &before, Condition::Equal(before[0x0043]));
    assert!(candidates.contains(&0x0043));

    nes.run_frame();
    watch.poll(&mut nes.bus);
    let after = nes.bus.cheat_snapshot().to_vec();
    assert_ne!(before[0x0043], after[0x0043]);

    // The counter changed; everything that did not move is gone.
    let narrowed = narrow_between(&before, &after, &candidates, Condition::Changed);
    assert!(narrowed.contains(&0x0043));
    for addr in &narrowed {
        assert_ne!(before[usize::from(*addr)], after[usize::from(*addr)]);
    }
    // ...and the second step keeps it: it still changed between the next two frames.
    let third = {
        nes.run_frame();
        nes.bus.cheat_snapshot().to_vec()
    };
    let narrowed = narrow_between(&after, &third, &narrowed, Condition::Changed);
    assert!(narrowed.contains(&0x0043));
}
