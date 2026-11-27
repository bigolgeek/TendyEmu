//! Determinism harness.
//!
//! The one test whose only job is *"same ROM + same power-on options + same input ⇒ same
//! output"*. Everything else in `tests/` asserts accuracy or a hash baseline; this asserts the
//! property those baselines quietly rely on, and it is the thing later option work has to keep
//! true.
//!
//! What "identical" means here (same binary, same machine):
//! * the **framebuffer digest of every frame** is identical,
//! * **`save_state()` bytes** are byte-identical at every 10th frame,
//! * the **drained audio samples** are bit-identical (`f32::to_bits`).
//!
//! Coverage:
//! * synthetic NROM / UxROM / MMC1 / MMC3 images built in the test. Each one carries a small
//!   6502 program (pulse-1 audio, a `$2007` poke, a joypad read stored to work RAM) so a run
//!   changes video, audio, and emulated state instead of idling on a zero-filled ROM. The
//!   program is the same code at a mapper-appropriate base, so a mapper bug that only shows up
//!   under a bank switch still reaches the state comparison through `random_mapper`.
//! * the local commercial/test ROMs in [`LOCAL_ROMS`] when their files exist. They are
//!   copyrighted and not in the repository, so a missing file is a **skip**, not a failure (the
//!   same convention as `tests/game_snapshots.rs`). The skipped count only reaches the log with
//!   `-- --nocapture`, so with `CI` set the test *asserts* that the ROMs
//!   `tools/fetch_test_roms.py` downloads actually ran instead of trusting a pass.
//!
//! Option sets (§7.4): default; `ram=Random, align=Random, random_mapper, seed=1`; the same with
//! seed 42. Each combination runs **twice in fresh `Nes` instances** for [`FRAMES`] frames with
//! the same xorshift-generated pad script.
//!
//! Extra cases: a split run (60 + save/load + 60) against a straight 120; a mid-run `reset()` at
//! frame 30; and the non-vacuity pair (seed 42 ≠ seed 1 state, scripted input ≠ no input).
//!
//! `-- --nocapture` prints the per-case lines and the total case count.

use tendy_nes::bus::RamPowerOn;
use tendy_nes::cart::{Region, split};
use tendy_nes::console::{ConsoleConfig, Quirks};
use tendy_nes::movie::{
    MovieFrame, Player, Recorder, console_from_movie_header, fm2_header, fm2_header_console,
    guid_from_seed, parse_fm2, rom_checksum_for,
};
use tendy_nes::nes::{Nes, PhaseChoice, PowerOn};
use tendy_nes::state::fnv1a64;

use crate::common::program;

/// Frames per run. 120 covers the second vblank, a scroll write and two audio frames' worth of
/// samples — enough that a timing change surfaces, small enough that the suite stays fast.
const FRAMES: usize = 120;
/// How often `save_state()` is captured and compared.
const STATE_EVERY: usize = 10;
/// Frame at which the reset case hits the reset button.
const RESET_FRAME: usize = 30;

/// Local ROMs exercised when present: name, what mapper/feature it brings, and whether
/// `tools/fetch_test_roms.py` downloads it. The fetched ones are the CI floor (below).
///
/// Kept short on purpose: the emulator runs ~7 M cycles/s, so each case (two 120-frame runs) is
/// about a second. These six cover NROM, MMC1, MMC3, MMC5's expansion audio, a commercial MMC1
/// game with PRG RAM, and a commercial MMC3 game; the synthetic matrix above covers the rest.
const LOCAL_ROMS: &[(&str, &str, bool)] = &[
    ("nestest.nes", "NROM, CPU test", true),
    ("mmc1_a12.nes", "MMC1, A12 clocking", true),
    ("mmc3_irq_1.Clocking.nes", "MMC3, IRQ clocking", true),
    ("tas_exram_mmc5exram.nes", "MMC5, expansion audio", true),
    (
        "Dragon Warrior 3 (U).nes",
        "MMC1 commercial, PRG RAM",
        false,
    ),
    ("roms/games/Air Fortress (U).nes", "MMC3 commercial", false),
];

/// The skipped ROMs the CI floor reports: only the ones `tools/fetch_test_roms.py` fetches.
/// The commercial entries in [`LOCAL_ROMS`] are expected to be absent on a runner, so naming them
/// suggested work that does not exist.
fn missing_fetchable<'a>(skipped: &[&'a str]) -> Vec<&'a str> {
    skipped
        .iter()
        .copied()
        .filter(|name| {
            LOCAL_ROMS
                .iter()
                .any(|(rom, _, fetchable)| rom == name && *fetchable)
        })
        .collect()
}

/// The CI floor's message names the four fetchable ROMs and none of the two commercial ones.
#[test]
fn the_ci_floor_message_names_only_the_fetchable_roms() {
    let skipped = [
        "Dragon Warrior 3 (U).nes",
        "nestest.nes",
        "roms/games/Air Fortress (U).nes",
        "tas_exram_mmc5exram.nes",
    ];
    assert_eq!(
        missing_fetchable(&skipped),
        ["nestest.nes", "tas_exram_mmc5exram.nes"]
    );
    assert_eq!(
        missing_fetchable(&[]),
        [] as [&str; 0],
        "nothing missing, nothing named"
    );
}

/// A deterministic pad script. `xorshift64` (the same shape `src/movie.rs` uses for GUIDs): it is
/// not the emulator's own splitmix64, so a bug in one does not hide in the other.
struct Script(u64);

impl Script {
    fn new(seed: u64) -> Script {
        // Any non-zero state works; `| 1` keeps a caller's 0 from pinning the generator.
        Script(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// One pad byte. The high bits are taken because xorshift's low bits are the weak ones; the
    /// D-pad conflicts that produces are dropped by `Controller::set_buttons`, deterministically.
    fn pad(&mut self) -> u8 {
        (self.next_u64() >> 40) as u8
    }
}

/// Everything a run records for comparison.
#[derive(Debug, PartialEq, Eq)]
struct Trace {
    /// Framebuffer digest, one per frame.
    digests: Vec<u64>,
    /// `save_state()` bytes at every [`STATE_EVERY`]th frame.
    states: Vec<Vec<u8>>,
    /// Drained audio samples, as `f32::to_bits`, flattened in order.
    audio: Vec<u32>,
    /// How many audio samples the run produced (kept so a silent run cannot pass vacuously).
    samples: usize,
}

impl Trace {
    fn new() -> Trace {
        Trace {
            digests: Vec::new(),
            states: Vec::new(),
            audio: Vec::new(),
            samples: 0,
        }
    }
}

/// FNV-1a 64 over the framebuffer: cheap enough to run on every frame, wide enough that a
/// one-pixel difference moves it.
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Build a fresh console: parse the image, apply the power-on options, reset the CPU.
fn boot(rom: &[u8], power_on: &PowerOn) -> Nes {
    Nes::from_rom_data_power_on(split(rom).expect("parse rom"), fnv1a64(rom), power_on)
        .expect("build console")
}

/// Run `frames` frames with the pad script, recording the trace. `nes` is driven in place so a
/// caller can continue a run (split-run case).
fn advance(
    nes: &mut Nes,
    script: &mut Script,
    trace: &mut Trace,
    first_frame: usize,
    frames: usize,
    reset_at: Option<usize>,
) {
    for i in first_frame..first_frame + frames {
        if reset_at == Some(i) {
            nes.reset();
        }
        nes.bus.pads[0].set_buttons(script.pad());
        nes.run_frame();
        trace.digests.push(digest(&nes.bus.ppu.framebuffer));
        let samples = std::mem::take(&mut nes.bus.apu.samples);
        trace.samples += samples.len();
        trace.audio.extend(samples.iter().map(|s| s.to_bits()));
        if (i + 1) % STATE_EVERY == 0 {
            trace.states.push(nes.save_state());
        }
    }
}

/// Boot and run one full case.
fn run(rom: &[u8], power_on: &PowerOn, script_seed: u64, reset_at: Option<usize>) -> (Nes, Trace) {
    let mut nes = boot(rom, power_on);
    let mut trace = Trace::new();
    let mut script = Script::new(script_seed);
    advance(&mut nes, &mut script, &mut trace, 0, FRAMES, reset_at);
    (nes, trace)
}

/// Compare two traces, reporting the first difference by name instead of dumping whole states.
fn assert_same(label: &str, a: &Trace, b: &Trace) {
    assert_eq!(a.digests.len(), b.digests.len(), "{label}: frame count");
    if let Some(i) = (0..a.digests.len()).find(|&i| a.digests[i] != b.digests[i]) {
        panic!(
            "{label}: framebuffer digest differs at frame {i}: {:016x} != {:016x}",
            a.digests[i], b.digests[i]
        );
    }
    assert_eq!(a.states.len(), b.states.len(), "{label}: state count");
    for (n, (x, y)) in a.states.iter().zip(&b.states).enumerate() {
        if let Some(off) = (0..x.len().min(y.len())).find(|&i| x[i] != y[i]) {
            panic!(
                "{label}: save_state differs at capture {n} (frame {}): len {} vs {}, first byte {off}: {:02x} != {:02x}",
                (n + 1) * STATE_EVERY,
                x.len(),
                y.len(),
                x[off],
                y[off]
            );
        }
        assert_eq!(
            x.len(),
            y.len(),
            "{label}: save_state length at capture {n}"
        );
    }
    assert_eq!(a.samples, b.samples, "{label}: audio sample count");
    if let Some(i) = (0..a.audio.len().min(b.audio.len())).find(|&i| a.audio[i] != b.audio[i]) {
        let (sa, sb) = (f32::from_bits(a.audio[i]), f32::from_bits(b.audio[i]));
        panic!(
            "{label}: audio sample {i} differs: {sa} ({:08x}) != {sb} ({:08x})",
            a.audio[i], b.audio[i]
        );
    }
    assert_eq!(a.audio.len(), b.audio.len(), "{label}: audio length");
}

/// The three option sets below.
fn option_sets() -> Vec<(&'static str, PowerOn)> {
    // The reset-button alignment is not a power-on option — it does not reach
    // construction — so these sets keep its default and `advance` resets with `reset()`.
    let random = |seed: u64| PowerOn {
        ram: RamPowerOn::Random,
        phase: PhaseChoice::Random,
        random_mapper: true,
        seed,
        seed_auto: false,
        reset_phase: PhaseChoice::Exact(0),
        overclock: 0,
    };
    vec![
        ("default", PowerOn::default()),
        ("random seed 1", random(1)),
        ("random seed 42", random(42)),
    ]
}

// ---------------------------------------------------------------------------------------------
// Synthetic images: a real (if tiny) program at a mapper-appropriate base.
// ---------------------------------------------------------------------------------------------

/// An iNES image for `mapper` with `prg_kb` of PRG, `chr_kb` of CHR (0 = CHR-RAM), the program at
/// the start of the bank that `base` lives in, and all three vectors pointing at it.
fn synthetic(mapper: u8, prg_kb: usize, chr_kb: usize, base: u16) -> Vec<u8> {
    let prg_banks = (prg_kb / 16) as u8;
    let chr_banks = (chr_kb / 8) as u8;
    let mut rom = vec![
        0x4E,
        0x45,
        0x53,
        0x1A, // "NES\x1a"
        prg_banks,
        chr_banks,
        (mapper & 0x0F) << 4, // flags 6: mapper low nibble, horizontal mirroring
        mapper & 0xF0,        // flags 7: mapper high nibble
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0, // flags 8-15
    ];
    let prg_len = prg_banks as usize * 0x4000;
    // Bank-number fill, like the mapper tests use: every 16 KB bank holds its own index, so a
    // wrong bank switch shows up in the state rather than as an identical zero page.
    rom.extend((0..prg_len).map(|i| (i / 0x4000) as u8));
    // The window `base` lives in: $C000 is the last 16 KB, $E000 the last 8 KB.
    let window = if base >= 0xE000 { 0x2000 } else { 0x4000 };
    let code = program(base);
    let start = 16 + prg_len - window;
    rom[start..start + code.len()].copy_from_slice(&code);
    // Vectors (NMI, reset, IRQ) all point at the program's first instruction.
    for vector in [16 + prg_len - 6, 16 + prg_len - 4, 16 + prg_len - 2] {
        rom[vector] = (base & 0xFF) as u8;
        rom[vector + 1] = (base >> 8) as u8;
    }
    rom.resize(16 + prg_len + chr_banks as usize * 0x2000, 0);
    rom
}

/// The synthetic matrix: one image per mapper the matrix names, each with the program in the bank
/// that mapper fixes last at power-on.
fn synthetic_matrix() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        // NROM-256: bank 1 fixed at $C000.
        ("nrom", synthetic(0, 32, 8, 0xC000)),
        // UxROM: last bank fixed at $C000.
        ("uxrom", synthetic(2, 32, 8, 0xC000)),
        // MMC1: powers up in PRG mode 3, last 16 KB at $C000.
        ("mmc1", synthetic(1, 64, 8, 0xC000)),
        // MMC3: last 8 KB bank at $E000.
        ("mmc3", synthetic(4, 32, 8, 0xE000)),
    ]
}

fn rom_path(name: &str) -> std::path::PathBuf {
    crate::common::rom_path(name)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// The core guarantee: for every synthetic mapper and every option set, two fresh consoles fed
/// the same ROM and the same pad script produce the same frames, states and audio.
#[test]
fn synthetic_mappers_replay_identically() {
    let mut cases = 0usize;
    for (name, rom) in synthetic_matrix() {
        for (label, power_on) in option_sets() {
            let (_, a) = run(&rom, &power_on, 0x5EED_1234, None);
            let (_, b) = run(&rom, &power_on, 0x5EED_1234, None);
            assert_same(&format!("{name} / {label}"), &a, &b);
            // A run that produced no audio samples or no state would pass vacuously.
            assert!(a.samples > 0, "{name} / {label}: no audio samples");
            assert!(!a.states.is_empty(), "{name} / {label}: no states captured");
            cases += 1;
        }
    }
    println!("determinism: {cases} synthetic cases (ROM x option set) identical");
    assert_eq!(cases, synthetic_matrix().len() * option_sets().len());
}

/// The same guarantee on the local real ROMs, skipping (not failing) the ones that are absent.
#[test]
fn local_roms_replay_identically() {
    let sets = option_sets().len();
    let total = LOCAL_ROMS.len() * sets;
    let mut ran = 0usize;
    let mut skipped: Vec<&str> = Vec::new();
    for (name, what, _) in LOCAL_ROMS {
        let path = rom_path(name);
        let rom = match std::fs::read(&path) {
            Ok(r) => r,
            Err(_) => {
                skipped.push(name);
                continue;
            }
        };
        for (label, power_on) in option_sets() {
            let (_, a) = run(&rom, &power_on, 0x0D15_EA5E, None);
            let (_, b) = run(&rom, &power_on, 0x0D15_EA5E, None);
            assert_same(&format!("{name} ({what}) / {label}"), &a, &b);
            ran += 1;
        }
    }
    println!(
        "determinism: {ran}/{total} local-ROM cases identical ({} roms present)",
        LOCAL_ROMS.len() - skipped.len()
    );
    println!("determinism: SKIPPED {}/{total} cases", total - ran);
    if !skipped.is_empty() {
        println!("determinism: skipped (no rom): {}", skipped.join(", "));
    }
    // A passing test's stdout is captured, so on CI a suite that skipped every ROM looked
    // exactly like one that ran them. A CI runner fetches everything in `tools/fetch_test_roms.py`
    // (four of these six), so those cases must actually run there; a renamed ROM in that script
    // now fails here instead of silently shrinking coverage. Locally, a skip stays a skip.
    //
    // The `fetchable` flag in [`LOCAL_ROMS`] is hand-kept, and `tools/fetch_test_roms.py` is the
    // source of truth for what a runner downloads: a ROM added there needs `true` here too. The
    // message names only the fetchable misses — the two commercial ROMs are *expected* to be
    // absent on a runner, and listing them read like work that does not exist.
    //
    // `CI=false` counts as CI on purpose: `var_os` tests presence, not value, and a locally
    // exported `CI=false` must not quietly turn the floor off.
    if std::env::var_os("CI").is_some() {
        let fetchable = LOCAL_ROMS.iter().filter(|r| r.2).count();
        let floor = fetchable * sets;
        let missing = missing_fetchable(&skipped);
        assert!(
            ran >= floor,
            "only {ran} of {floor} local ROM cases ran on CI, but tools/fetch_test_roms.py fetches \
             {fetchable} of the {} roms here ({sets} option sets each); missing: {missing:?}",
            LOCAL_ROMS.len()
        );
    }
}

/// A save/load in the middle of a run is invisible: 60 + state + 60 in a new console equals
/// 120 straight.
#[test]
fn split_run_matches_straight_run() {
    let rom = synthetic(0, 32, 8, 0xC000);
    let power_on = PowerOn::default();
    let half = FRAMES / 2;

    let (_, straight) = run(&rom, &power_on, 0xA1B2_C3D4, None);

    let mut nes = boot(&rom, &power_on);
    let mut script = Script::new(0xA1B2_C3D4);
    let mut first = Trace::new();
    advance(&mut nes, &mut script, &mut first, 0, half, None);
    let saved = nes.save_state();

    let mut resumed = boot(&rom, &power_on);
    resumed.load_state(&saved).expect("load mid-run state");
    let mut second = Trace::new();
    advance(
        &mut resumed,
        &mut script,
        &mut second,
        half,
        FRAMES - half,
        None,
    );

    assert_eq!(
        resumed.save_state(),
        straight
            .states
            .last()
            .expect("straight final state")
            .clone(),
        "split run: final save_state"
    );
    assert_eq!(
        digest(&resumed.bus.ppu.framebuffer),
        *straight.digests.last().expect("straight final frame"),
        "split run: final frame"
    );
    // The first half and the tail are both real work, not an idle machine.
    assert!(
        first.digests.iter().any(|d| *d != first.digests[0]),
        "split run: first half never changed a frame"
    );
    println!("determinism: split run (60 + load + 60) matches a straight 120");
}

/// The reset button mid-run is deterministic: two runs with a reset at frame 30 agree, and the
/// reset actually changed the outcome (otherwise the case would pass without exercising it).
#[test]
fn reset_mid_run_replays_identically() {
    for (name, rom) in synthetic_matrix() {
        let power_on = PowerOn::default();
        let (_, a) = run(&rom, &power_on, 0x2718_2818, Some(RESET_FRAME));
        let (_, b) = run(&rom, &power_on, 0x2718_2818, Some(RESET_FRAME));
        assert_same(&format!("{name} / reset at {RESET_FRAME}"), &a, &b);

        let (_, plain) = run(&rom, &power_on, 0x2718_2818, None);
        assert_ne!(
            a.states.last().unwrap(),
            plain.states.last().unwrap(),
            "{name}: resetting at frame {RESET_FRAME} left the final state unchanged, so the case proves nothing"
        );
    }
    println!("determinism: reset at frame {RESET_FRAME} replays identically on 4 mappers");
}

/// Non-vacuity: the comparison can tell seeds and inputs apart.
#[test]
fn seeds_and_inputs_are_distinguishable() {
    let rom = synthetic(1, 64, 8, 0xC000);
    let (_, one) = run(&rom, &option_sets()[1].1, 0x1234_5678, None);
    let (_, forty_two) = run(&rom, &option_sets()[2].1, 0x1234_5678, None);
    assert_ne!(
        one.states.last().unwrap(),
        forty_two.states.last().unwrap(),
        "seed 1 and seed 42 produced the same save_state: the seed is not reaching the console"
    );

    let (_, other_input) = run(&rom, &option_sets()[1].1, 0x8765_4321, None);
    assert_ne!(
        one.states.last().unwrap(),
        other_input.states.last().unwrap(),
        "two different pad scripts produced the same save_state: the input is not reaching the console"
    );
    println!("determinism: seeds and pad scripts change the state");
}

// ---------------------------------------------------------------------------------------------
// The console a recording ran on travels in the FM2 header, so a replay reproduces
// the run without being told what console to build.
// ---------------------------------------------------------------------------------------------

/// The console the recording was made on is in the FM2 text and nowhere else. Replaying that text
/// on a fresh machine built from the header must reproduce every frame's framebuffer and the final
/// `save_state()` byte for byte — with a command line that disagrees on every field.
#[test]
fn a_movie_replays_on_the_console_its_header_names() {
    // MMC1 so `random_mapper` has bank state to randomise, and every option moved from its
    // default except the ones the header only needs to spell.
    let rom = synthetic(1, 64, 8, 0xC000);
    let config = ConsoleConfig::with_quirks(
        PowerOn {
            ram: RamPowerOn::Random,
            phase: PhaseChoice::Random,
            reset_phase: PhaseChoice::Random,
            random_mapper: true,
            seed: 7,
            seed_auto: false,
            overclock: 0,
        },
        Quirks {
            ppu_warmup: true,
            oam_decay: true,
            ..Quirks::default()
        },
    );
    let frames = 60;
    let data = split(&rom).expect("parse rom");
    let mut recorded =
        Nes::from_rom_data_console(split(&rom).expect("parse rom"), fnv1a64(&rom), &config)
            .expect("build console");
    let mut recorder = Recorder::new(fm2_header_console(
        "synthetic.nes",
        &rom_checksum_for(&data),
        &guid_from_seed(3),
        recorded.region(),
        &config,
        0,
    ));
    let mut script = Script::new(11);
    let mut digests = Vec::new();
    for i in 0..frames {
        // A reset-button press mid-run, realigned from the seed and the press index: the
        // reset alignment is part of the recorded console too.
        if i == RESET_FRAME {
            recorded.reset_aligned(config.power_on.reset_phase_for(0));
        }
        let pad = script.pad();
        recorded.bus.pads[0].buttons = pad;
        recorder.movie.frames.push(MovieFrame {
            commands: 0,
            pads: [pad, 0],
        });
        recorded.run_frame();
        digests.push(digest(&recorded.bus.ppu.framebuffer));
    }
    let recorded_state = recorded.save_state();

    // Replay: no flags at all, so the header is the only thing that can build the right console.
    let parsed = parse_fm2(&recorder.to_fm2()).expect("a recorded movie parses");
    let (movie_console, diffs) =
        console_from_movie_header(&parsed.header, &ConsoleConfig::default())
            .expect("the header reads back")
            .expect("a recording names its console");
    assert_eq!(
        movie_console, config,
        "the header carries the console it was recorded on"
    );
    assert!(
        !diffs.is_empty(),
        "the default command line disagrees, and every field is reported"
    );

    let mut replayed = Nes::from_rom_data_console(
        split(&rom).expect("parse rom"),
        fnv1a64(&rom),
        &movie_console,
    )
    .expect("build the replayed console");
    let mut player = Player::new(parsed);
    for (i, expected) in digests.iter().enumerate() {
        if i == RESET_FRAME {
            replayed.reset_aligned(movie_console.power_on.reset_phase_for(0));
        }
        assert!(
            player.apply(&mut replayed).expect("the movie plays"),
            "frame {i}"
        );
        replayed.run_frame();
        assert_eq!(
            digest(&replayed.bus.ppu.framebuffer),
            *expected,
            "frame {i} differs: the replay did not run the recording's console"
        );
    }
    assert_eq!(
        replayed.save_state(),
        recorded_state,
        "the replayed state differs from the recorded one"
    );
    println!("determinism: a movie replayed on the console its header names");
}

/// The default path is unchanged by the new keys: a recording of the default console
/// carries the default configuration, and building from it is byte-identical to `Nes::new`.
#[test]
fn a_default_recording_replays_the_default_console() {
    let rom = synthetic(0, 32, 8, 0xC000);
    let header = fm2_header(
        "synthetic.nes",
        &rom_checksum_for(&split(&rom).expect("parse rom")),
        &guid_from_seed(1),
        Region::Ntsc,
    );
    let (config, diffs) = console_from_movie_header(&header, &ConsoleConfig::default())
        .expect("the header reads back")
        .expect("a recording names its console");
    assert_eq!(
        config,
        ConsoleConfig::default(),
        "a default recording names the default console"
    );
    assert!(
        diffs.is_empty(),
        "nothing disagrees with a default command line: {diffs:?}"
    );

    // Two frames on each: the pre-existing constructor and the one built from the header.
    let mut plain = Nes::new(&rom).expect("build console");
    let mut from_header =
        Nes::from_rom_data_console(split(&rom).expect("parse rom"), fnv1a64(&rom), &config)
            .expect("build console");
    for _ in 0..2 {
        plain.run_frame();
        from_header.run_frame();
    }
    assert_eq!(
        from_header.save_state(),
        plain.save_state(),
        "the default path moved"
    );
    assert_eq!(
        digest(&from_header.bus.ppu.framebuffer),
        digest(&plain.bus.ppu.framebuffer),
        "the default path moved"
    );
    println!("determinism: a default recording replays the default console");
}
