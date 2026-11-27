//! Session history: a recorded session seeks back by loading a keyframe and
//! replaying the logged pads and reset commands, and the machine it lands on is the machine a
//! straight run would have produced.
//!
//! Every case runs on a hand-built NROM cartridge, so the only thing moving the machine is the
//! program below: it latches pad port 1 into `$0300`, counts its own loops in `$10` and counts boots
//! in `$0302`. That last byte is how a replay can be asked whether the logged reset really happened
//! (a reset restarts the program, which increments it again instead of clearing it, because RAM
//! survives the reset button, as on hardware).

use tendy_nes::history::{
    COMMAND_POWER_CYCLE, COMMAND_RESET, DEFAULT_KEYFRAME_INTERVAL, History, SeekError,
};
use tendy_nes::nes::Nes;
use tendy_nes::rng::SplitMix64;

/// Where the program lives: the last 16 KB bank of a 32 KB NROM image, and what all three vectors
/// point at, so a reset restarts it.
const BASE: u16 = 0xC000;

/// The program: `INC $0302` (one per boot), latch eight pad bits into `$20` through port 1,
/// `STA $0300`, `INC $10`, and jump back to the latch. It never waits for vblank: `run_frame` stops
/// the CPU wherever the PPU is, which is what makes the state depend on the exact frame count.
fn program() -> Vec<u8> {
    let mut p: Vec<u8> = Vec::new();
    p.extend_from_slice(&[0x78]); // SEI
    p.extend_from_slice(&[0xEE, 0x02, 0x03]); // INC $0302 (boot counter)
    let latch = p.len() as u16;
    p.extend_from_slice(&[0xA9, 0x01, 0x8D, 0x16, 0x40]); // LDA #$01 / STA $4016 (strobe high)
    p.extend_from_slice(&[0xA9, 0x00, 0x8D, 0x16, 0x40]); // LDA #$00 / STA $4016 (shift mode)
    for _ in 0..8 {
        p.extend_from_slice(&[0xAD, 0x16, 0x40]); // LDA $4016
        p.extend_from_slice(&[0x4A]); // LSR A
        p.extend_from_slice(&[0x26, 0x20]); // ROL $20: the first read's bit lands in bit 0
    }
    p.extend_from_slice(&[0xA5, 0x20, 0x8D, 0x00, 0x03]); // LDA $20 / STA $0300
    p.extend_from_slice(&[0xE6, 0x10]); // INC $10
    let back = BASE + latch;
    p.extend_from_slice(&[0x4C, back as u8, (back >> 8) as u8]); // JMP latch
    p
}

/// A 32 KB PRG + 8 KB CHR NROM image with the program at `$C000` and every vector pointing at it.
fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![
        0x4E, 0x45, 0x53, 0x1A, // "NES\x1a"
        2, 1, // 32 KB PRG, 8 KB CHR
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // mapper 0, horizontal mirroring
    ];
    let mut prg = vec![0u8; 0x8000];
    let code = program();
    prg[0x4000..0x4000 + code.len()].copy_from_slice(&code);
    for vector in [0x7FFA, 0x7FFC, 0x7FFE] {
        prg[vector] = BASE as u8;
        prg[vector + 1] = (BASE >> 8) as u8;
    }
    rom.extend_from_slice(&prg);
    rom.resize(rom.len() + 0x2000, 0); // CHR
    rom
}

/// The seeded pad script: one splitmix64 draw per frame, derived from the frame number so that two
/// runs of the same length see the same byte at the same frame however they got there.
fn pad(seed: u64, frame: usize) -> u8 {
    let mut rng = SplitMix64::new(seed ^ (frame as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    rng.next_u8()
}

const SEED: u64 = 0x8313_83A0_5EED_0001;

/// Set both pads from the script and run one frame, through the history.
fn step(nes: &mut Nes, history: &mut History, seed: u64, frame: usize) {
    nes.bus.pads[0].buttons = pad(seed, frame);
    nes.bus.pads[1].buttons = 0;
    history.step(nes);
}

/// The same, with a soft reset on `reset_at`.
fn drive(nes: &mut Nes, history: &mut History, seed: u64, from: usize, to: usize, reset_at: usize) {
    for frame in from..to {
        nes.bus.pads[0].buttons = pad(seed, frame);
        nes.bus.pads[1].buttons = 0;
        let commands = if frame == reset_at { COMMAND_RESET } else { 0 };
        history.step_with(nes, commands);
    }
}

/// The acceptance case: 2000 frames of a seeded script, then `seek(1234)` plus ten more
/// frames of the same script, ends byte-identical to a straight run of 1244 frames — and the seek
/// itself already lands on the straight run's frame 1234.
#[test]
fn seek_then_ten_frames_equals_a_straight_run() {
    let rom = synthetic_rom();

    // The straight run, keeping the state of frame 1234 on the way past it.
    let mut straight = Nes::new(&rom).unwrap();
    let mut at_1234 = None;
    for frame in 0..1244 {
        straight.bus.pads[0].buttons = pad(SEED, frame);
        straight.bus.pads[1].buttons = 0;
        straight.run_frame();
        if frame + 1 == 1234 {
            at_1234 = Some(straight.save_state());
        }
    }
    let straight = straight;

    // The recorded session: 2000 frames, so the seek has a future to overwrite.
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::new();
    for frame in 0..2000 {
        step(&mut nes, &mut history, SEED, frame);
    }
    assert_eq!(history.keyframe_interval(), DEFAULT_KEYFRAME_INTERVAL);
    assert_eq!(
        history.keyframe_frames(),
        vec![0, 600, 1200, 1800],
        "a keyframe at attach and every 600 frames"
    );
    assert_eq!(history.position(), 2000);
    assert_eq!(history.start(), 0);
    assert_eq!(history.end(), 2000);
    // The program really is reading the pads, so the comparison above cannot pass vacuously.
    assert_eq!(
        nes.bus.ram[0x0300],
        pad(SEED, 1999),
        "the latch holds frame 1999's pad"
    );

    // Seek back: frame 1234, byte for byte, from the keyframe at 1200 plus 34 replayed frames.
    history.seek(&mut nes, 1234).unwrap();
    assert_eq!(history.position(), 1234);
    assert_eq!(
        history.keyframe_for(1234).map(|k| k.frame()),
        Some(1200),
        "the seek starts from the nearest earlier keyframe"
    );
    assert_eq!(
        nes.save_state(),
        at_1234.unwrap(),
        "seek lands exactly on frame 1234"
    );
    assert_eq!(history.len(), 2000, "a seek does not truncate");

    // Ten more frames of the same script: the future is overwritten from 1234, as resuming does.
    for frame in 1234..1244 {
        step(&mut nes, &mut history, SEED, frame);
    }
    assert_eq!(history.position(), 1244);
    assert_eq!(history.end(), 1244, "the future after 1234 was dropped");
    assert_eq!(
        nes.save_state(),
        straight.save_state(),
        "ten frames after the seek equal a straight run to 1244"
    );
    assert_eq!(
        history.keyframe_frames(),
        vec![0, 600, 1200],
        "the 1800 keyframe went with the future"
    );
}

/// Past the budget the oldest keyframe goes first, and the inputs before the new oldest keyframe go
/// with it: the history then starts where the oldest kept keyframe is.
///
/// The budget is two of the *largest* state the run produces, so "room for exactly two keyframes"
/// stays exact even though a state's length moves by a byte or two from frame to frame (the bus
/// writes its `Option` latches as present-or-absent, and a buffered `$4016` strobe is one of them).
#[test]
fn the_budget_drops_the_oldest_keyframe_first() {
    let rom = synthetic_rom();
    let seed = SEED + 1;
    // The largest state the script produces, measured on its own machine first.
    let max_state = {
        let mut probe = Nes::new(&rom).unwrap();
        let mut max = probe.save_state().len();
        for frame in 0..1800 {
            probe.bus.pads[0].buttons = pad(seed, frame);
            probe.bus.pads[1].buttons = 0;
            probe.run_frame();
            max = max.max(probe.save_state().len());
        }
        max
    };

    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::with_budget(max_state * 2);
    assert_eq!(history.budget(), max_state * 2);

    for frame in 0..1800 {
        step(&mut nes, &mut history, seed, frame);
        if frame + 1 == 1200 {
            assert_eq!(
                history.keyframe_frames(),
                vec![600, 1200],
                "the keyframe at 0 is the first to go"
            );
            assert_eq!(history.start(), 600, "and the inputs before 600 go with it");
            assert_eq!(history.end(), 1200);
            assert_eq!(history.input(0), None);
        }
    }

    assert_eq!(history.keyframe_frames(), vec![1200, 1800]);
    assert_eq!(history.start(), 1200);
    assert_eq!(history.end(), 1800);
    assert_eq!(history.len(), 600);
    assert!(
        history.keyframe_bytes() <= history.budget(),
        "{} bytes of keyframes against a {} byte budget",
        history.keyframe_bytes(),
        history.budget()
    );

    // The frames before the oldest kept keyframe are gone, not merely keyframe-less.
    assert_eq!(history.input(1199), None);
    assert_eq!(
        history.seek(&mut nes, 1199),
        Err(SeekError::TooOld {
            asked: 1199,
            oldest: 1200
        })
    );
    assert!(
        history.keyframe_for(1199).is_none(),
        "no keyframe covers a dropped frame"
    );

    // The oldest kept keyframe and the newest frame are both still reachable.
    history.seek(&mut nes, 1200).unwrap();
    assert_eq!(history.position(), 1200);
    assert_eq!(history.keyframe_for(1250).map(|k| k.frame()), Some(1200));
    history.seek(&mut nes, 1800).unwrap();
    assert_eq!(history.position(), 1800);
    assert_eq!(
        history.input(1799).map(|f| f.pads[0]),
        Some(pad(seed, 1799))
    );

    // A truncate before the retained window drops the whole log, not the keyframe it starts from:
    // frames older than `start` are already gone, so `start` is the earliest the window can reach.
    assert_eq!(history.truncate_after(0), 600);
    assert_eq!(history.start(), 1200);
    assert_eq!(history.end(), 1200);
    assert_eq!(history.keyframe_frames(), vec![1200]);
    assert_eq!(history.seek(&mut nes, 1200), Ok(()));
}

/// A budget smaller than one state still keeps one keyframe: a history with none could not seek at
/// all, so the newest state survives over budget rather than leaving nothing to load (the same rule
/// `rewind::kept_frames` follows).
#[test]
fn one_keyframe_survives_a_budget_smaller_than_a_state() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::with_budget(nes.save_state().len() / 2);
    for frame in 0..700 {
        step(&mut nes, &mut history, SEED + 6, frame);
    }
    assert_eq!(
        history.keyframe_frames(),
        vec![600],
        "only the newest keyframe is left"
    );
    assert_eq!(history.start(), 600);
    assert_eq!(history.keyframe_count(), 1);
    assert!(
        history.keyframe_bytes() > history.budget(),
        "one keyframe survives even over budget"
    );
    history.seek(&mut nes, 600).unwrap();
    assert_eq!(history.position(), 600);
}

/// A reset command in the log replays as a reset: the program boots a second time, at the same frame,
/// on the way back through it.
#[test]
fn a_reset_command_in_the_log_replays_as_a_reset() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::new();
    drive(&mut nes, &mut history, SEED + 2, 0, 300, 150);

    assert_eq!(
        nes.bus.ram[0x0302], 2,
        "one boot at power-on plus the reset at frame 150"
    );
    let reset_frame = history.input(150).expect("frame 150 is logged");
    assert_eq!(reset_frame.commands & COMMAND_RESET, COMMAND_RESET);
    assert_eq!(
        history.input(149).unwrap().commands,
        0,
        "the command is on one frame only"
    );

    // Back to before the reset: the program has booted once.
    history.seek(&mut nes, 100).unwrap();
    assert_eq!(nes.bus.ram[0x0302], 1, "frame 100 is before the reset");

    // Forward through the reset again, on the log's own command.
    drive(&mut nes, &mut history, SEED + 2, 100, 300, 150);
    assert_eq!(
        nes.bus.ram[0x0302], 2,
        "the replayed reset boots the program again"
    );

    // And the replayed session is the recorded session, byte for byte.
    let mut straight = Nes::new(&rom).unwrap();
    let mut plain = History::new();
    drive(&mut straight, &mut plain, SEED + 2, 0, 300, 150);
    assert_eq!(nes.save_state(), straight.save_state());
}

/// Resuming from a past frame: the future is dropped, the new future is recorded from there, and a
/// seek past the new end is reported as beyond the history rather than silently clamped.
#[test]
fn truncate_after_drops_the_future() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::new();
    drive(&mut nes, &mut history, SEED + 3, 0, 200, usize::MAX);
    assert_eq!(history.seek(&mut nes, 100), Ok(()));

    assert_eq!(
        history.truncate_after(100),
        100,
        "frames 100..200 are dropped"
    );
    assert_eq!(history.end(), 100);
    assert_eq!(history.len(), 100);
    assert_eq!(
        history.position(),
        100,
        "the machine is where it was seeked to"
    );
    assert_eq!(
        history.seek(&mut nes, 150),
        Err(SeekError::BeyondHistory {
            asked: 150,
            end: 100
        })
    );

    // A different script from 100 on, recorded over the old future.
    for frame in 100..150 {
        nes.bus.pads[0].buttons = pad(SEED + 4, frame);
        nes.bus.pads[1].buttons = 0;
        history.step(&mut nes);
    }
    assert_eq!(history.end(), 150);
    assert_eq!(history.input(140).unwrap().pads[0], pad(SEED + 4, 140));
    assert_eq!(
        history.input(90).unwrap().pads[0],
        pad(SEED + 3, 90),
        "the past is kept"
    );

    // The same two scripts on a fresh machine land in the same state.
    let mut straight = Nes::new(&rom).unwrap();
    for frame in 0..100 {
        straight.bus.pads[0].buttons = pad(SEED + 3, frame);
        straight.bus.pads[1].buttons = 0;
        straight.run_frame();
    }
    for frame in 100..150 {
        straight.bus.pads[0].buttons = pad(SEED + 4, frame);
        straight.bus.pads[1].buttons = 0;
        straight.run_frame();
    }
    assert_eq!(nes.save_state(), straight.save_state());
}

/// A history that has recorded nothing can seek nothing, and one keyframe is enough history to seek
/// to its own frame.
#[test]
fn an_empty_history_says_so() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::new();
    assert!(history.is_empty());
    assert_eq!(history.keyframe_count(), 0);
    assert_eq!(history.seek(&mut nes, 0), Err(SeekError::Empty));

    history.attach(&nes);
    assert!(!history.is_empty());
    assert_eq!(history.keyframe_frames(), vec![0]);
    assert_eq!(history.keyframe_count(), 1);
    assert_eq!(history.end(), 0);
    assert_eq!(history.len(), 0);
    assert_eq!(history.start(), 0);
    assert_eq!(history.position(), 0);
    assert_eq!(
        history.seek(&mut nes, 0),
        Ok(()),
        "frame 0 is the attached state"
    );
    assert_eq!(
        history.seek(&mut nes, 1),
        Err(SeekError::BeyondHistory { asked: 1, end: 0 })
    );
}

/// The keyframe cadence is the interval, not the default: a small interval proves the arithmetic
/// without playing 600 frames, and the default is what the 80-minute figure is built on.
#[test]
fn keyframes_land_every_interval() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::with_limits(7, 64 * 1024 * 1024);
    assert_eq!(history.keyframe_interval(), 7);
    for frame in 0..30 {
        step(&mut nes, &mut history, SEED + 5, frame);
    }
    assert_eq!(history.keyframe_frames(), vec![0, 7, 14, 21, 28]);
    assert_eq!(history.position(), 30);
    assert_eq!(history.keyframe_count(), 5);
}

/// The command bits are FM2's: bit 0 is the reset this module replays, bit 1 is the power cycle it
/// deliberately does not (a power cycle builds a new machine, so the frontend starts a new history).
#[test]
fn the_command_bits_are_the_movie_bits() {
    assert_eq!(COMMAND_RESET, 0b01);
    assert_eq!(COMMAND_POWER_CYCLE, 0b10);
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut history = History::new();
    nes.bus.pads[0].buttons = 0x81;
    nes.bus.pads[1].buttons = 0x42;
    history.step_with(&mut nes, 0);
    let frame = history.input(0).expect("frame 0 is logged");
    assert_eq!(frame.commands, 0);
    assert_eq!(frame.pads, [0x81, 0x42], "both pads, in joypad bit order");
}
