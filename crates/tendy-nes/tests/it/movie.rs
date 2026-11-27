//! Movies: recording a session to FM2 and playing it back on a
//! fresh `Nes` reproduces every framebuffer; playback from a save state too. The
//! other direction is a synthetic BizHawk `.bk2` archive parsing into the same movie stream.

use std::io::Write;
use tendy_nes::cart::Region;
use tendy_nes::console::ConsoleConfig;
use tendy_nes::controller::{
    BUTTON_A, BUTTON_B, BUTTON_DOWN, BUTTON_LEFT, BUTTON_RIGHT, BUTTON_SELECT, BUTTON_START,
    BUTTON_UP,
};
use tendy_nes::cpu::FLAG_I;
use tendy_nes::movie::{
    Movie, MovieError, Player, Recorder, fm2_header, fm2_header_console, guid_from_seed,
    parse_bk2_bytes, parse_fm2, rom_checksum, rom_checksum_for,
};
use tendy_nes::nes::{Nes, PhaseChoice, PowerOn};
use tendy_nes::state::fnv1a64;

use crate::common::rom_file;

fn set_pads(nes: &mut Nes, v: u8) {
    nes.bus.pads[0].buttons = v;
    nes.bus.pads[1].buttons = 0;
}

/// Scripted test input: START if i%97<2, A if i%53<2, RIGHT if i%31<2.
fn scripted(i: usize) -> u8 {
    let mut v = 0u8;
    if i % 97 < 2 {
        v |= BUTTON_START;
    }
    if i % 53 < 2 {
        v |= BUTTON_A;
    }
    if i % 31 < 2 {
        v |= BUTTON_RIGHT;
    }
    v
}

fn hash(nes: &Nes) -> u64 {
    fnv1a64(&nes.bus.ppu.framebuffer)
}

#[test]
fn record_then_play_reproduces_frame_hashes() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    let mut recorder = Recorder::new(vec![("version".to_string(), "3".to_string())]);
    let mut hashes = Vec::new();
    for i in 0..400 {
        set_pads(&mut a, scripted(i));
        recorder.record(&a, false);
        a.run_frame();
        hashes.push(hash(&a));
    }

    // Write the movie to FM2, parse it back, and play it on a new Nes.
    let text = recorder.movie.to_fm2();
    let mut b = Nes::new(&rom).unwrap();
    let mut player = Player::new(parse_fm2(&text).unwrap());
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

#[test]
fn play_from_a_state_matches_the_recording() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    // Warm 100 frames with the scripted input, then save the state.
    for i in 0..100 {
        set_pads(&mut a, scripted(i));
        a.run_frame();
    }
    let state = a.save_state();

    // Record 200 more frames on the warm machine.
    let mut recorder = Recorder::new(vec![]);
    let mut hashes = Vec::new();
    for i in 100..300 {
        set_pads(&mut a, scripted(i));
        recorder.record(&a, false);
        a.run_frame();
        hashes.push(hash(&a));
    }

    let text = recorder.movie.to_fm2();
    let mut b = Nes::new(&rom).unwrap();
    b.load_state(&state).unwrap();
    let mut player = Player::new(parse_fm2(&text).unwrap());
    for (k, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {k}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {k}");
    }
    assert_eq!(player.apply(&mut b), Ok(false));
}

#[test]
fn apply_sets_pads_as_recorded() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    let mut player = Player::new(Movie {
        header: vec![],
        frames: vec![tendy_nes::movie::MovieFrame {
            commands: 0,
            pads: [BUTTON_A | BUTTON_START, BUTTON_RIGHT],
        }],
    });
    assert_eq!(player.apply(&mut nes), Ok(true));
    assert_eq!(nes.bus.pads[0].buttons, BUTTON_A | BUTTON_START);
    assert_eq!(nes.bus.pads[1].buttons, BUTTON_RIGHT);
    // Movie of one frame has ended.
    assert_eq!(player.apply(&mut nes), Ok(false));
}

#[test]
fn apply_soft_resets_on_command_bit_0() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    nes.bus.pads[0].buttons = BUTTON_A;
    nes.run_frame();
    let sp_before = nes.cpu.sp;
    let mut player = Player::new(Movie {
        header: vec![],
        frames: vec![tendy_nes::movie::MovieFrame {
            commands: 1,
            pads: [0, 0],
        }],
    });
    assert_eq!(player.apply(&mut nes), Ok(true));
    // A soft reset sets I, drops 3 off the stack pointer and clears the pads.
    assert_eq!(nes.cpu.sp, sp_before.wrapping_sub(3));
    assert_ne!(nes.cpu.p & FLAG_I, 0);
    assert_eq!(nes.bus.pads[0].buttons, 0);
}

#[test]
fn power_cycle_command_is_reported_and_the_frame_is_kept() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    let mut player = Player::new(Movie {
        header: vec![],
        frames: vec![tendy_nes::movie::MovieFrame {
            commands: 2,
            pads: [0, 0],
        }],
    });
    assert_eq!(
        player.apply(&mut nes),
        Err(MovieError::PowerCycleUnsupported)
    );
    assert_eq!(
        player.pos, 0,
        "the frame is not consumed: the frontend replays it"
    );
}

/// After the frontend has rebuilt the machine, clearing the bit lets the frame play; a frame that
/// asked for both a power cycle and a soft reset still resets.
#[test]
fn apply_power_cycle_clears_only_the_power_cycle_bit() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    let mut player = Player::new(Movie {
        header: vec![],
        frames: vec![tendy_nes::movie::MovieFrame {
            commands: 3,
            pads: [BUTTON_B, 0],
        }],
    });
    assert_eq!(
        player.apply(&mut nes),
        Err(MovieError::PowerCycleUnsupported)
    );
    player.apply_power_cycle();
    assert_eq!(player.apply(&mut nes), Ok(true));
    assert_eq!(player.pos, 1);
    assert_eq!(
        nes.bus.pads[0].buttons, BUTTON_B,
        "the frame's pad state applies"
    );
}

/// A Reset pressed while recording is the frame's command bit 0, and replaying the written
/// movie resets on that same frame — the frame hashes stay identical from the reset onward.
#[test]
fn record_carries_the_reset_command_and_playback_resets_on_the_same_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    let mut recorder = Recorder::new(vec![("version".to_string(), "3".to_string())]);
    let mut hashes = Vec::new();
    for i in 0..200 {
        set_pads(&mut a, scripted(i));
        // What the R key does: the console resets, then the frame runs.
        let reset = i == 120;
        if reset {
            a.reset();
        }
        recorder.record(&a, reset);
        a.run_frame();
        hashes.push(hash(&a));
    }

    let text = recorder.movie.to_fm2();
    let parsed = parse_fm2(&text).expect("a recording with a reset parses");
    assert_eq!(
        parsed.frames[120].commands & 0b01,
        0b01,
        "the reset frame's command bit"
    );
    assert!(
        parsed
            .frames
            .iter()
            .enumerate()
            .all(|(i, f)| i == 120 || f.commands == 0),
        "only the reset frame carries a command"
    );

    let mut b = Nes::new(&rom).unwrap();
    let mut player = Player::new(parsed);
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

/// A console carrying the `--reset-align` and `--seed` options a recording runs with, everything
/// else default: the two values a movie header stores so a replay can press Reset the same way.
fn reset_console(reset_phase: PhaseChoice, seed: u64) -> ConsoleConfig {
    ConsoleConfig {
        power_on: PowerOn {
            reset_phase,
            seed,
            ..PowerOn::default()
        },
        quirks: tendy_nes::console::Quirks::default(),
    }
}

/// The machine a console configuration builds for `rom`, through the constructor the frontend uses.
fn boot(rom: &[u8], console: &ConsoleConfig) -> Nes {
    let data = tendy_nes::cart::split(rom).expect("parse rom");
    Nes::from_rom_data_console(data, tendy_nes::state::fnv1a64(rom), console)
        .expect("build console")
}

/// Record `frames` frames of scripted input with a Reset on each frame in `resets`, exactly as the
/// frontend does with `--reset-align`: the R key presses Reset with
/// `reset_aligned(power_on.reset_phase_for(n))` for the n-th press since power-on, and the frame it
/// lands on carries command bit 0. Returns the saved FM2 text and every frame's framebuffer hash.
fn record_resets(
    rom: &[u8],
    console: &ConsoleConfig,
    resets: &[usize],
    frames: usize,
) -> (String, Vec<u64>) {
    let mut nes = boot(rom, console);
    let header = fm2_header_console(
        "AccuracyCoin.nes",
        &rom_checksum_for(&tendy_nes::cart::split(rom).expect("parse rom")),
        &guid_from_seed(1),
        nes.region(),
        console,
        0,
    );
    let mut recorder = Recorder::new(header);
    let mut hashes = Vec::with_capacity(frames);
    let mut presses = 0u32;
    for i in 0..frames {
        set_pads(&mut nes, scripted(i));
        let reset = resets.contains(&i);
        if reset {
            nes.reset_aligned(console.power_on.reset_phase_for(presses));
            presses = presses.saturating_add(1);
        }
        recorder.record(&nes, reset);
        nes.run_frame();
        hashes.push(hash(&nes));
    }
    (recorder.to_fm2(), hashes)
}

/// Replay `text` on a fresh console and assert every frame hash matches. The console, and with it
/// the player's reset alignment, is the one the movie's own header names — the same
/// `console_from_movie_header` path the frontend takes — so this fails if the format stops
/// carrying what the replay needs (`nesrust_reset_align` and `nesrust_seed`).
fn assert_replays(rom: &[u8], text: &str, hashes: &[u64], console: &ConsoleConfig) {
    use tendy_nes::movie::console_from_movie_header;
    let parsed = parse_fm2(text).expect("the recorded movie parses back");
    let (movie_console, _) = console_from_movie_header(&parsed.header, &ConsoleConfig::default())
        .expect("the header reads back")
        .expect("the recording names its console");
    assert_eq!(movie_console, *console);
    let mut b = boot(rom, &movie_console);
    let mut player = Player::with_power_on(parsed, movie_console.power_on);
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

/// A Reset with `--reset-align 1`: the frame realigns the PPU by 1 dot, exactly as the R key did
/// while recording, so the replay does not drift — every one of 240 frames matches.
#[test]
fn a_reset_recorded_at_alignment_1_replays_every_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Exact(1), 0);
    let (text, hashes) = record_resets(&rom, &console, &[60, 140], 240);
    assert_replays(&rom, &text, &hashes, &console);
}

/// The same with `--reset-align 2`, same 240-frame replay.
#[test]
fn a_reset_recorded_at_alignment_2_replays_every_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Exact(2), 0);
    let (text, hashes) = record_resets(&rom, &console, &[60, 140], 240);
    assert_replays(&rom, &text, &hashes, &console);
}

/// The same with `--reset-align 3`, same 240-frame replay.
#[test]
fn a_reset_recorded_at_alignment_3_replays_every_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Exact(3), 0);
    let (text, hashes) = record_resets(&rom, &console, &[60, 140], 240);
    assert_replays(&rom, &text, &hashes, &console);
}

/// With `--reset-align random` and a fixed seed: the player draws each press's phase from the
/// header's seed and that press's index, so the second Reset uses the second phase, not the first.
#[test]
fn a_reset_recorded_at_random_alignment_replays_every_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Random, 42);
    // This seed's two presses realign by different phases, so a player that reused the first phase
    // (or drew none) cannot pass.
    assert_ne!(
        console.power_on.reset_phase_for(0),
        console.power_on.reset_phase_for(1),
        "the seed must exercise the per-press index"
    );
    let (text, hashes) = record_resets(&rom, &console, &[60, 140], 240);
    assert_replays(&rom, &text, &hashes, &console);
}

/// The recording starts mid-run, after two Resets have already been pressed, so its header
/// must carry that press count (2) — the first Reset it holds is the machine's third press, and a
/// player that numbers it from 0 realigns the PPU to the wrong phase and desyncs from there. Every
/// frame of the recorded stretch matches, in the picture and in the machine state; the state digest
/// is the one that catches a phase-only difference, because the ROM's screen is static here.
#[test]
fn a_recording_started_after_two_resets_replays_every_frame() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Random, 42);
    let mut a = boot(&rom, &console);
    // This seed realigns press 2 differently from press 0, so a player seeded from 0 cannot pass.
    assert_ne!(
        console.power_on.reset_phase_for(0),
        console.power_on.reset_phase_for(2),
        "the seed must separate the two press indices"
    );

    // Two Resets before F9: presses 0 and 1 since power-on.
    let mut presses = 0u32;
    for i in 0..100 {
        set_pads(&mut a, scripted(i));
        if i == 30 || i == 70 {
            a.reset_aligned(console.power_on.reset_phase_for(presses));
            presses = presses.saturating_add(1);
        }
        a.run_frame();
    }
    assert_eq!(presses, 2);

    // F9 mid-run: the start state, and a header carrying the count the machine is already at.
    let state = a.save_state();
    let header = fm2_header_console(
        "AccuracyCoin.nes",
        &rom_checksum_for(&tendy_nes::cart::split(&rom).expect("parse rom")),
        &guid_from_seed(1),
        a.region(),
        &console,
        presses,
    );
    let mut recorder = Recorder::new(header);
    let mut pictures = Vec::new();
    let mut states = Vec::new();
    for i in 100..340 {
        set_pads(&mut a, scripted(i));
        let reset = i == 140 || i == 220;
        if reset {
            a.reset_aligned(console.power_on.reset_phase_for(presses));
            presses = presses.saturating_add(1);
        }
        recorder.record(&a, reset);
        a.run_frame();
        pictures.push(hash(&a));
        states.push(fnv1a64(&a.save_state()));
    }
    assert_eq!(presses, 4);

    let text = recorder.to_fm2();
    let parsed = parse_fm2(&text).expect("the recorded movie parses back");
    assert_eq!(
        parsed.reset_start(),
        2,
        "the header carries where the recording began"
    );

    // The replay the frontend does: load the movie's start state, then play it.
    let mut b = boot(&rom, &console);
    b.load_state(&state)
        .expect("the recording's start state loads");
    let mut player = Player::with_power_on(parsed, console.power_on);
    for (k, (&picture, &state)) in pictures.iter().zip(states.iter()).enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {k}");
        b.run_frame();
        assert_eq!(hash(&b), picture, "frame {k}");
        assert_eq!(fnv1a64(&b.save_state()), state, "frame {k} state");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

/// The version-and-count format change is additive: a movie written before those keys existed has
/// neither, reads as press 0, and replays exactly as it always did — from power-on, its first Reset
/// is the machine's first press.
#[test]
fn a_movie_written_before_the_reset_count_still_plays() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Random, 42);
    let (text, hashes) = record_resets(&rom, &console, &[60, 140], 240);
    // The same recording, as the old writer left it: no version, no press count.
    let old: String = text
        .lines()
        .filter(|line| {
            !line.starts_with("nesrust_movie_version ") && !line.starts_with("nesrust_reset_start ")
        })
        .map(|line| format!("{line}\n"))
        .collect();
    let parsed = parse_fm2(&old).expect("the old file parses");
    assert_eq!((parsed.movie_version(), parsed.reset_start()), (0, 0));
    assert_replays(&rom, &old, &hashes, &console);
}

/// The fix changes no existing movie: a recording with no Reset still writes command 0 on every
/// frame, and replays identically.
#[test]
fn a_recording_without_a_reset_writes_zero_commands_and_replays_identically() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    let mut recorder = Recorder::new(vec![]);
    let mut hashes = Vec::new();
    for i in 0..200 {
        set_pads(&mut nes, scripted(i));
        recorder.record(&nes, false);
        nes.run_frame();
        hashes.push(hash(&nes));
    }
    assert!(recorder.movie.frames.iter().all(|f| f.commands == 0));
    let text = recorder.movie.to_fm2();
    assert!(
        text.lines()
            .filter(|line| line.starts_with('|'))
            .all(|line| line.starts_with("|0|")),
        "every frame line has no command:\n{text}"
    );
    let mut b = Nes::new(&rom).unwrap();
    let mut player = Player::new(parse_fm2(&text).unwrap());
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

/// A Reset after a realigned press still records the same command bit: the frame's command byte
/// says a Reset happened, and the phase it used lives in the header (`nesrust_reset_align`), which
/// is what the player reads back to perform the same reset.
#[test]
fn a_realigned_reset_records_command_bit_0() {
    let rom = rom_file("AccuracyCoin.nes");
    let console = reset_console(PhaseChoice::Exact(3), 0);
    let mut nes = Nes::new(&rom).unwrap();
    let mut recorder = Recorder::new(fm2_header_console(
        "AccuracyCoin.nes",
        &rom_checksum_for(&tendy_nes::cart::split(&rom).unwrap()),
        &guid_from_seed(1),
        nes.region(),
        &console,
        0,
    ));
    set_pads(&mut nes, 0);
    recorder.record(&nes, false);
    nes.run_frame();
    nes.reset_aligned(3);
    recorder.record(&nes, true);
    assert_eq!(recorder.movie.frames[0].commands, 0);
    assert_eq!(recorder.movie.frames[1].commands & 0b01, 0b01);
    // The alignment the press used is what the header carries, and what a player reads back.
    let parsed = parse_fm2(&recorder.to_fm2()).expect("a recording parses back");
    let power_on = PowerOn::from_header(&parsed.header).expect("the header names the console");
    assert_eq!(power_on.reset_phase, PhaseChoice::Exact(3));
}

#[test]
fn rerecording_from_a_state_rewinds_the_movie() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    let data = tendy_nes::cart::split(&rom).unwrap();
    let header = fm2_header(
        "AccuracyCoin.nes",
        &rom_checksum(&data.prg, &data.chr),
        &guid_from_seed(11),
        a.region(),
    );
    let mut recorder = Recorder::new(header);

    // Record 150 frames, keeping the state saved at frame 60 and its anchor.
    let mut hashes = Vec::new();
    let mut state = Vec::new();
    let mut anchor = None;
    for i in 0..150 {
        if i == 60 {
            state = a.save_state();
            anchor = Some(recorder.anchor().expect("a recording has a guid"));
        }
        set_pads(&mut a, scripted(i));
        recorder.record(&a, false);
        a.run_frame();
        hashes.push(hash(&a));
    }
    assert_eq!(recorder.movie.frames.len(), 150);
    assert_eq!(recorder.rerecord_count(), 0);

    // Rewinding to the state drops the 90 frames after it and counts the rerecord.
    a.load_state(&state).unwrap();
    let anchor = anchor.unwrap();
    assert_eq!(anchor.frame, 60);
    assert_eq!(recorder.truncate_to_anchor(&anchor), 90);
    assert_eq!(recorder.movie.frames.len(), 60);
    assert_eq!(recorder.rerecord_count(), 1);
    hashes.truncate(60);

    // Record a different segment from that same state.
    for i in 60..130 {
        set_pads(&mut a, scripted(i + 5));
        recorder.record(&a, false);
        a.run_frame();
        hashes.push(hash(&a));
    }
    assert_eq!(recorder.movie.frames.len(), 130);

    // Saving writes rerecordCount, and replaying the rerecorded movie from
    // power-on reproduces every frame hash: the segments are one timeline.
    let text = recorder.to_fm2();
    let parsed = parse_fm2(&text).expect("a saved recording parses");
    assert_eq!(parsed.frames.len(), 130);
    let count = parsed.header.iter().find(|(k, _)| k == "rerecordCount");
    assert_eq!(count.map(|(_, v)| v.as_str()), Some("1"));
    let mut b = Nes::new(&rom).unwrap();
    let mut player = Player::new(parsed);
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}

#[test]
fn recording_writes_the_required_fm2_header_keys() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut nes = Nes::new(&rom).unwrap();
    // The checksum covers the ROM's PRG+CHR data, as load_nes computes it.
    let data = tendy_nes::cart::split(&rom).unwrap();
    // The recording's checksum comes from the cartridge, and its timing from the loaded console.
    let checksum = rom_checksum_for(&data);
    let header = fm2_header(
        "AccuracyCoin.nes",
        &checksum,
        &guid_from_seed(7),
        nes.region(),
    );
    let mut recorder = Recorder::new(header.clone());
    for i in 0..2 {
        set_pads(&mut nes, scripted(i));
        recorder.record(&nes, false);
        nes.run_frame();
    }

    // What a tool opening the written file sees.
    let text = recorder.movie.to_fm2();
    let parsed = parse_fm2(&text).expect("a recorded movie parses back");
    for key in [
        "version",
        "emuVersion",
        "romFilename",
        "romChecksum",
        "guid",
        "palFlag",
        "fourscore",
        "port0",
        "port1",
        "port2",
        "NewPPU",
        "FDS",
    ] {
        assert!(
            parsed.header.iter().any(|(k, _)| k == key),
            "missing {key} in:\n{text}"
        );
    }
    let found = &parsed
        .header
        .iter()
        .find(|(k, _)| k == "romChecksum")
        .unwrap()
        .1;
    assert_eq!(found, &checksum);
    // `base64:` and a padded base64 16-byte digest (24 characters).
    assert_eq!(found.len(), "base64:".len() + 24);
    assert!(found.ends_with("=="));

    // Header order and every frame survive a parse/render round trip.
    assert_eq!(parsed.header, header);
    assert_eq!(parsed.to_fm2(), text);
    assert_eq!(parsed.frames, recorder.movie.frames);
}

/// FM2 `romChecksum` must be what FCEUX writes, so the digests below are hard-coded: they were
/// computed outside this crate (md5 of the ROM's data, then base64) for two ROMs in `roms/`.
#[test]
fn rom_checksum_matches_fceux_for_chr_rom_and_chr_ram() {
    // AccuracyCoin has 1 CHR bank: PRG bytes then CHR ROM bytes.
    let chr_rom = tendy_nes::cart::split(&rom_file("AccuracyCoin.nes")).unwrap();
    assert!(!chr_rom.chr_is_ram);
    assert_eq!(
        rom_checksum_for(&chr_rom),
        "base64:uTo0bzLCxMejeOuaplhPyg=="
    );
    assert_eq!(
        rom_checksum(&chr_rom.prg, &chr_rom.chr),
        "base64:uTo0bzLCxMejeOuaplhPyg=="
    );

    // 01.basics is CHR-RAM: FCEUX hashes the PRG bytes only. Hashing the 8 KiB CHR-RAM image
    // `cart::split` fabricates would give "base64:G2hAodh5jODFHeAElV9XJg==", which no other tool writes.
    let chr_ram = tendy_nes::cart::split(&rom_file("01.basics.nes")).unwrap();
    assert!(chr_ram.chr_is_ram);
    assert_eq!(
        rom_checksum_for(&chr_ram),
        "base64:/NoZmuLdGCTzd6w+xA5ozQ=="
    );
    assert_eq!(
        rom_checksum(&chr_ram.prg, &[]),
        "base64:/NoZmuLdGCTzd6w+xA5ozQ=="
    );
}

/// A PAL cartridge's recording must say `palFlag 1`, or FCEUX replays it at 60.1 Hz and desyncs.
#[test]
fn pal_recording_sets_pal_flag() {
    // NES 2.0 NROM image (`flags7 = 0x08`) whose timing byte (byte 12) is 1, i.e. PAL.
    let mut rom = vec![
        0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0x08, 0, 0, 0, 0, 1, 0, 0, 0,
    ];
    rom.resize(16 + 16384 + 8192, 0);
    let nes = Nes::new(&rom).unwrap();
    assert_eq!(nes.region(), Region::Pal);
    let header = fm2_header(
        "pal.nes",
        "base64:1B2M2Y8AsgTpgAmY7PhCfg==",
        &guid_from_seed(9),
        nes.region(),
    );
    let pal_flag = header
        .iter()
        .find(|(k, _)| k == "palFlag")
        .map(|(_, v)| v.as_str());
    assert_eq!(pal_flag, Some("1"));
    // The flag survives the writer and the parser other tools would use.
    let parsed =
        parse_fm2(&Recorder::new(header).movie.to_fm2()).expect("a header-only movie parses");
    let parsed_flag = parsed
        .header
        .iter()
        .find(|(k, _)| k == "palFlag")
        .map(|(_, v)| v.as_str());
    assert_eq!(parsed_flag, Some("1"));
}

/// Build a `.bk2` archive in memory from `(entry name, contents)` pairs. Synthetic: no real `.bk2`
/// is committed to this repo (they are TASVideos submissions and not redistributable), so the
/// archive is built from the format TASVideos documents.
fn bk2_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(*name, options).expect("start an entry");
        writer.write_all(bytes).expect("write an entry");
    }
    writer.finish().expect("finish the archive").into_inner()
}

/// One pad mask as a `.bk2` `LogKey` field, in BizHawk's NES column order
/// (`Up Down Left Right Start Select B A`), `.` for a button that is not pressed.
fn bk2_pad_field(mask: u8) -> String {
    [
        (BUTTON_UP, 'U'),
        (BUTTON_DOWN, 'D'),
        (BUTTON_LEFT, 'L'),
        (BUTTON_RIGHT, 'R'),
        (BUTTON_START, 'T'),
        (BUTTON_SELECT, 'S'),
        (BUTTON_B, 'B'),
        (BUTTON_A, 'A'),
    ]
    .iter()
    .map(|(bit, pressed)| if mask & bit != 0 { *pressed } else { '.' })
    .collect()
}

/// The frames a `.bk2` decodes to drive the machine through the same
/// `Player` an FM2 uses, and reproduce the framebuffer hashes of a run whose pads were set
/// directly. Synthetic input, real ROM.
#[test]
fn a_synthetic_bk2_plays_through_the_player() {
    let rom = rom_file("AccuracyCoin.nes");
    let mut a = Nes::new(&rom).unwrap();
    let mut log = String::from(
        "[Input]\n\
         LogKey:#Reset|Power|#P1 Up|P1 Down|P1 Left|P1 Right|P1 Start|P1 Select|P1 B|P1 A|\
         #P2 Up|P2 Down|P2 Left|P2 Right|P2 Start|P2 Select|P2 B|P2 A|\n",
    );
    let mut hashes = Vec::new();
    for i in 0..120 {
        let pads = [scripted(i), if i % 71 < 2 { BUTTON_LEFT } else { 0 }];
        log.push_str(&format!(
            "|..|{}|{}|\n",
            bk2_pad_field(pads[0]),
            bk2_pad_field(pads[1])
        ));
        a.bus.pads[0].buttons = pads[0];
        a.bus.pads[1].buttons = pads[1];
        a.run_frame();
        hashes.push(hash(&a));
    }

    let archive = bk2_zip(&[
        ("Header.txt", b"Platform NES\nCore QuickNes\n"),
        ("Input Log.txt", log.as_bytes()),
    ]);
    let movie = parse_bk2_bytes(&archive).expect("the synthetic .bk2 parses");
    assert_eq!(movie.frames.len(), 120);

    let mut b = Nes::new(&rom).unwrap();
    let mut player = Player::new(movie);
    for (i, &h) in hashes.iter().enumerate() {
        assert_eq!(player.apply(&mut b), Ok(true), "frame {i}");
        b.run_frame();
        assert_eq!(hash(&b), h, "frame {i}");
    }
    assert_eq!(player.apply(&mut b), Ok(false), "movie ended");
}
