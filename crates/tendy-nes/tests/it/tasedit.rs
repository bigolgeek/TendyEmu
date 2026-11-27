//! TAS edit core: edits, greenzone invalidation and seek on a synthetic NROM.

use tendy_nes::controller::{BUTTON_A, BUTTON_RIGHT, BUTTON_START};
use tendy_nes::movie::{Movie, MovieError, MovieFrame, Player};
use tendy_nes::nes::Nes;
use tendy_nes::tasedit::TasProject;

/// NROM whose program strobes pad 1, shifts its 8 bits into $00, adds that to $02 and counts
/// loops in $01: RAM depends on every pad value ever read, so a stale edit shows.
fn rom() -> Vec<u8> {
    let code: [u8; 32] = [
        0xA9, 0x01, 0x8D, 0x16, 0x40, 0xA9, 0x00, 0x8D, 0x16, 0x40, // strobe
        0xA2, 0x08, // LDX #8
        0xAD, 0x16, 0x40, 0x4A, 0x26, 0x00, 0xCA, 0xD0, 0xF7, // read loop
        0xA5, 0x00, 0x18, 0x65, 0x02, 0x85, 0x02, 0xE6, 0x01, // accumulate, count
        0x4C, 0x00, // JMP $C000 (low byte; high below)
    ];
    let mut prg = vec![0u8; 0x4000];
    prg[..code.len()].copy_from_slice(&code);
    prg[code.len()] = 0xC0;
    prg[0x3FFA..].copy_from_slice(&[0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0]);
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.extend(prg);
    rom.extend(vec![0u8; 0x2000]);
    rom
}

fn movie(frames: usize) -> Movie {
    Movie {
        header: Vec::new(),
        frames: (0..frames)
            .map(|i| MovieFrame {
                commands: 0,
                pads: [(i as u8).wrapping_mul(37), 0],
            })
            .collect(),
    }
}

fn fresh() -> Nes {
    Nes::new(&rom()).unwrap()
}

/// Power-on run of `movie` to `frame` — the reference the project must match. It goes through the
/// same [`Player`] a seek replays with, so the only thing the comparison can catch is the
/// greenzone: a checkpoint that restored less than the whole machine, or a replay that applied the
/// wrong pads.
fn reference(movie: &Movie, frame: usize) -> Vec<u8> {
    let mut nes = fresh();
    let mut player = Player::new(movie.clone());
    while player.pos < frame {
        if !player
            .apply(&mut nes)
            .expect("these movies never power-cycle")
        {
            break;
        }
        nes.run_frame();
    }
    nes.save_state()
}

#[test]
fn edit_then_seek_matches_power_on_run() {
    let mut p = TasProject::new(movie(200), &fresh());
    let mut nes = fresh();
    assert_eq!(p.seek(&mut nes, 200).unwrap(), 200);
    assert!(p.toggle_button(100, 0, BUTTON_A | BUTTON_START));
    assert_eq!(p.seek(&mut nes, 150).unwrap(), 150);
    assert_eq!(nes.save_state(), reference(&p.movie, 150));
    // The edit is visible: the unedited movie reaches a different state.
    assert_ne!(nes.save_state(), reference(&movie(200), 150));
}

#[test]
fn edit_invalidates_later_entries_only() {
    let mut p = TasProject::new(movie(60), &fresh());
    let mut nes = fresh();
    p.seek(&mut nes, 60).unwrap();
    assert_eq!(p.greenzone.len(), 60);
    assert!(p.set_button(30, 0, BUTTON_RIGHT, true));
    assert_eq!(p.movie.frames[30].pads[0] & BUTTON_RIGHT, BUTTON_RIGHT);
    assert!(p.set_button(30, 0, BUTTON_RIGHT, false));
    assert_eq!(
        p.movie.frames[30].pads[0] & BUTTON_RIGHT,
        0,
        "clearing a button is the same edit"
    );
    assert!(p.greenzone.contains(30));
    assert!(!p.greenzone.contains(31));
    assert_eq!(p.greenzone.frames().last(), Some(&30));
    assert!(p.insert_blank(10));
    assert_eq!(p.greenzone.frames().last(), Some(&10));
    assert!(p.delete_frame(5));
    assert_eq!(p.greenzone.frames().last(), Some(&5));
    assert_eq!(p.movie.frames.len(), 60);
    p.seek(&mut nes, 60).unwrap();
    assert_eq!(nes.save_state(), reference(&p.movie, 60));
    assert!(!p.set_button(999, 0, 1, true));
    assert!(!p.insert_blank(61));
    assert!(!p.delete_frame(60));
}

#[test]
fn seek_backwards_restores_the_earlier_frame() {
    let mut p = TasProject::new(movie(80), &fresh());
    let mut nes = fresh();
    p.seek(&mut nes, 70).unwrap();
    p.seek(&mut nes, 20).unwrap();
    assert_eq!(nes.save_state(), reference(&p.movie, 20));
    p.seek(&mut nes, 0).unwrap();
    assert_eq!(nes.save_state(), reference(&p.movie, 0));
}

#[test]
fn seek_past_the_end_stops_at_the_last_frame() {
    let mut p = TasProject::new(movie(10), &fresh());
    let mut nes = fresh();
    assert_eq!(p.seek(&mut nes, 50).unwrap(), 10);
    assert_eq!(nes.save_state(), reference(&p.movie, 10));
}

#[test]
fn budget_trims_the_oldest_entries() {
    // A state is not a fixed size — a frame's worth of machine state is a byte or two longer than
    // power-on's — so the budget is sized from a measured maximum rather than one sample: three
    // states fit, and the trim has to drop everything older.
    let mut probe = fresh();
    let mut max_len = probe.save_state().len();
    for _ in 0..40 {
        probe.run_frame();
        max_len = max_len.max(probe.save_state().len());
    }
    let mut p = TasProject::with_budget(movie(40), &fresh(), max_len * 3);
    let mut nes = fresh();
    assert_eq!(p.seek(&mut nes, 40).unwrap(), 40);
    let kept = p.greenzone.frames();
    // The newest checkpoint always survives: a trim that took it would leave the frame on screen
    // with no state at all.
    assert_eq!(kept.last(), Some(&40));
    assert!(
        kept.len() >= 3 && kept.len() < 40,
        "budget for three states kept {kept:?}"
    );
    assert_eq!(
        kept,
        (kept[0]..=40).collect::<Vec<_>>(),
        "the oldest went first, with no gaps"
    );
    assert!(
        p.greenzone.bytes() <= p.greenzone.budget(),
        "{} bytes held",
        p.greenzone.bytes()
    );
    // A seek into the trimmed window is rebuilt from the origin, byte-exact, and its own
    // checkpoints stay inside the same budget.
    assert_eq!(p.seek(&mut nes, 12).unwrap(), 12);
    assert_eq!(nes.save_state(), reference(&p.movie, 12));
    assert!(p.greenzone.bytes() <= p.greenzone.budget());
}

#[test]
fn seek_to_the_edited_frame_replays_nothing() {
    let mut p = TasProject::new(movie(120), &fresh());
    let mut nes = fresh();
    p.seek(&mut nes, 120).unwrap();
    // Editing frame 100 keeps the checkpoint keyed 100 — the machine before frame 100's pads are
    // applied — so a seek back to the frame that was just edited restores it instead of replaying.
    assert!(p.set_button(100, 0, BUTTON_A, true));
    assert!(
        p.greenzone.contains(100),
        "the checkpoint before the edited frame is still valid"
    );
    let before = p.greenzone.frames();
    assert_eq!(p.seek(&mut nes, 100).unwrap(), 100);
    assert_eq!(
        p.greenzone.frames(),
        before,
        "a restore, so no checkpoint was recorded again"
    );
    assert_eq!(nes.save_state(), reference(&p.movie, 100));
}

#[test]
fn a_power_cycle_stops_the_replay_and_the_project_keeps_its_movie() {
    let mut m = movie(40);
    m.frames[10].commands = 0b10;
    let mut p = TasProject::new(m, &fresh());
    let mut nes = fresh();
    // A power cycle needs a rebuilt machine, which only a frontend can do, so `Player` refuses the
    // frame: the seek reports it instead of replaying on a console that should have been swapped.
    assert_eq!(p.seek(&mut nes, 30), Err(MovieError::PowerCycleUnsupported));
    assert_eq!(
        p.movie.frames.len(),
        40,
        "the movie is handed back, not taken by the failed seek"
    );
    // What played before the refused frame still replays byte-exactly.
    assert_eq!(p.seek(&mut nes, 10).unwrap(), 10);
    assert_eq!(nes.save_state(), reference(&p.movie, 10));
}

#[test]
fn a_soft_reset_in_the_movie_replays_identically_after_an_edit() {
    let mut m = movie(80);
    // Player's other command: frame 40 soft resets the console before it runs, so a replay that
    // dropped the command (or restored a checkpoint from after it) would not match power-on.
    m.frames[40].commands = 0b01;
    let mut p = TasProject::new(m, &fresh());
    let mut nes = fresh();
    assert_eq!(p.seek(&mut nes, 80).unwrap(), 80);
    assert!(p.set_button(20, 0, BUTTON_A, true));
    // The nearest surviving checkpoint is 20, so the reset at 40 is replayed.
    assert_eq!(p.seek(&mut nes, 60).unwrap(), 60);
    assert_eq!(nes.save_state(), reference(&p.movie, 60));
}

#[test]
fn a_checkpoint_that_will_not_load_is_dropped_and_the_seek_replays_past_it() {
    // A stale checkpoint (from another ROM build, or a damaged write) used to panic the seek. It is
    // now dropped and the replay starts from the next older checkpoint, or power-on.
    let mut p = TasProject::new(movie(60), &fresh());
    let mut nes = fresh();
    p.seek(&mut nes, 30).unwrap();
    p.greenzone.insert(45, vec![0u8; 4]);
    assert_eq!(p.seek(&mut nes, 60).unwrap(), 60);
    assert_eq!(nes.save_state(), reference(&p.movie, 60));
}
