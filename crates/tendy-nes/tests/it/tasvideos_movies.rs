//! TASVideos publication replay: NES Action 52: Non Human by LoganTheTASer,
//! publication 5403M (3397 frames, FCEUX 2.6.4 FM2, power-on start).
//!
//! Local only (`#[ignore]`): it needs the commercial ROM `roms/Action 52 (U) [!].nes` (PRG+CHR md5
//! 233cddd94c3f3d5b9a9f4670a914e74d) and the movie, which is not committed:
//!
//! ```text
//! curl -L -o m.zip "https://tasvideos.org/5403M?handler=Download" && unzip m.zip -d roms/movies
//! ```
//!
//! OUTCOME: PARTIAL, the replay desyncs. What was measured on this branch:
//! - The FM2 parses and `Player` replays all 3397 frames without error.
//! - Played from frame 0 the menu input lands on the wrong game (Jupiter Scope). The game menu is
//!   timing sensitive, and the emulator reaches it about 3 frames before FCEUX does. With 3 blank
//!   frames in front of the movie (any lead of 3..=14 gives the same trajectory) Non Human starts and
//!   plays correctly for a while: movie frame 1200 shows the player on the first platform.
//! - It then diverges from the real run: after the last input frame the character is still in the level
//!   instead of at its end, and with no further input the game settles on a static screen with the
//!   character back at the start (final hash `05736bb44bcf682a` at frame 3400).
//! - Console power-on RAM is not the cause: all-zero, all-$FF and FCEUX's `(i & 4) ? FF : 00` patterns
//!   give identical frame hashes. The remaining suspects are an FCEUX-specific behaviour (frame
//!   boundary or `$4016` read convention, NewPPU 0 sprite timing) or a mapper 228 / PPU inaccuracy
//!   that this game exercises. Neither was isolated.
//!
//! The test pins the part that does work (the movie reaches Non Human gameplay at a known frame) so a
//! later fix of the desync shows up as a hash change, not a silent pass. It is a regression canary
//! for our own output, not proof of TAS-exact playback.

use tendy_nes::bus::RamPowerOn;
use tendy_nes::movie::{Movie, MovieFrame, Player, parse_bk2, parse_fm2};
use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;

const ROM: &str = "roms/Action 52 (U) [!].nes";
const MOVIE: &str = "roms/movies/logansgamingroom-action52nonhuman.fm2";
/// Blank frames put in front of the movie so the Start press lands after the emulator's menu is ready.
const LEAD_IN: usize = 3;

fn read(rel: &str) -> Vec<u8> {
    let path = crate::common::rom_path(rel);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (see the module docs for how to fetch it)",
            path.display()
        )
    })
}

fn load_movie(lead_in: usize) -> Movie {
    let text = String::from_utf8(read(MOVIE)).expect("FM2 is UTF-8");
    let mut movie = parse_fm2(&text).expect("movie parses");
    for _ in 0..lead_in {
        movie.frames.insert(
            0,
            MovieFrame {
                commands: 0,
                pads: [0, 0],
            },
        );
    }
    movie
}

#[test]
#[ignore = "local only: needs a commercial ROM and a TASVideos movie (see module docs); replay desyncs"]
fn non_human_movie_parses_and_reaches_gameplay() {
    let raw = load_movie(0);
    assert_eq!(raw.frames.len(), 3397);
    assert!(
        raw.frames.iter().all(|f| f.commands == 0),
        "power-on start, no reset commands"
    );
    assert!(
        raw.header.iter().any(|(k, v)| k == "palFlag" && v == "0"),
        "NTSC movie"
    );

    let mut nes = Nes::new(&read(ROM)).expect("ROM loads (mapper 228)");
    let mut player = Player::new(load_movie(LEAD_IN));
    let mut frames = 0usize;
    let mut at_1200 = 0u64;
    while player.apply(&mut nes).expect("replay error") {
        nes.run_frame();
        frames += 1;
        if frames == 1200 {
            at_1200 = fnv1a64(&nes.bus.ppu.framebuffer);
        }
    }
    assert_eq!(frames, 3397 + LEAD_IN, "every movie frame was played");
    // Non Human, first platform. Our own output, pinned as a canary (see module docs).
    assert_eq!(
        at_1200, 0x34d8_f707_a12b_03b8,
        "frame 1200 framebuffer hash changed"
    );
}

// ---------------------------------------------------------------------------------------------
// Alter Ego. A freely downloadable ROM, so unlike Action 52 above these run wherever
// `tools/fetch_test_roms.py` has run (CI does). Alter Ego is Shiru's freeware NES homebrew; the
// movie is TASVideos publication 4129M, "NES Alter Ego" by aiqiyou (6:35.615, FCEUX 2.2.3, NewPPU
// off, 23776 frames, power-on start, 3637 rerecords).
//
// OUTCOME: PARTIAL, the replay desyncs; the full replay is `#[ignore]`d and the issue stays open
// (103b). Measured here:
// - The ROM is the right one: its PRG+CHR md5 equals the movie header's `romChecksum`.
// - The movie's two Start presses (movie frames 43 and 85) are timing sensitive. Played from frame 0
//   the title screen is never left: all 23776 frames end on the title. A blank lead-in changes the
//   outcome without a pattern (frame 600 for leads 0/6/12 is the title, 1 and 2 and 8 are other title
//   states, 3 and 4 are in the game); leads 3 and 4 start the game, which is replayed here. Power-on RAM contents
//   (zero, $FF, FCEUX's pattern) change nothing: the game clears RAM.
// - With a 3-frame lead the game then plays: frame 1000 is the Promenade room with 4 of 5 lives left (`$06BB`)
//   and no keys. The player dies at frames 966, 1283 and 1866 (the lives byte drops), runs out of lives before frame 3000,
//   and the game falls back to the title screen, where it sits for the remaining ~19800 frames (final
//   hash below). A published run does not do that, so the first divergence is at or before frame 966.
//   With a 4-frame lead the run gets to Broken Bridge with 1 life at frame 6000. The ending is never
//   reached in either, so no RAM predicate for it was found.
// - The movie holds no resets, power cycles or second controller; the cause of the divergence is not
//   isolated (candidates: FCEUX's power-on CPU/PPU alignment, which shifts which frame the game's
//   input poll lands in; the game's own RNG seeded from frame timing).
//
// DIVERGENCE MATRIX (measured on the release build). Reset alignment
// (`Nes::new_aligned`, 0..=3) against blank lead-in (0..=4), full 23776-frame replay, lives byte
// `$06BB`. Frames are movie frames (lead-in subtracted). "First lives drop" is the first frame the
// byte falls; "game over" is the first frame it reads 0 after the game started; "== a0/l3" says the
// whole per-frame lives trace equals the (align 0, lead 3) run the pins use. The byte reads 255
// during the intro, so "started" is the movie frame it first leaves 0 (161 in every run that gets
// there). No ground truth exists for the real run's lives, so "first divergence" can only be bounded
// above by the first lives drop: a published run does not lose its lives.
//
// | align | lead | game started | first lives drop | game over | ends on title | lives trace        |
// |-------|------|--------------|------------------|-----------|---------------|--------------------|
// | 0,1,2 | 0    | no           | -                | -         | no (no start) | stuck pre-game     |
// | 0,1,2 | 1    | no           | -                | -         | no (no start) | same as lead 0     |
// | 0,1,2 | 2    | yes          | 554              | 4551      | yes           | differs from a0/l3 |
// | 0,1,2 | 3    | yes          | 963              | 2557      | yes           | == a0/l3 (AE_LEAD_IN) |
// | 0,1,2 | 4    | yes          | 2545             | 6366      | no            | differs from a0/l3 |
// | 3     | 0    | yes          | none             | none      | no            | lives 9, never drops; unexplained |
// | 3     | 1    | no           | -                | -         | no            | stuck pre-game     |
// | 3     | 2    | no           | -                | -         | no            | stuck pre-game     |
// | 3     | 3    | yes          | 554              | 4551      | yes           | == a0/l2           |
// | 3     | 4    | yes          | 963              | 2557      | yes           | == a0/l3           |
//
// Row command (prints one `ROW` line; any align 0..=3, lead 0..=4):
//   AE_ALIGN=<a> AE_LEAD=<l> cargo test --release --test it tasvideos_movies:: -- --ignored --nocapture alter_ego_sweep_row
// Wall time: one row is about 90 s alone (the full `alter_ego_movie_reaches_the_ending` measured 89 s
// on this machine); all 20 ran as parallel threads in 233 s. The table was produced by that parallel
// sweep with the same per-run logic as the row test.
//
// FINDINGS
// - Alignments 0, 1 and 2 are indistinguishable at every lead: identical lives traces, 15 of 15
//   cells. Alignment 3 is exactly one lead-in frame: (a3, lead n) == (a0, lead n-1) for n = 3, 4.
//   So the 0..=3 alignment is not a fine knob here; it either does nothing or moves the input by one
//   whole frame, which the lead-in already does.
// - No pair removes the divergence. Every pair that starts the game loses its lives (first drop 554,
//   963 or 2545) and, except (a3, lead 0) which is unexplained, ends in game over. Lead 4 only moves
//   it later (first drop 2545, game over 6366, never reaches the title by the end).
// - Lead 0 does not work for alignments 0..=2 (the game never starts). Alignment 3 with lead 0 does
//   start it, but takes a different path (lives 9, no drops, not the title), which is not evidence of
//   a correct replay: no ending predicate exists yet.
// - Results move non-monotonically with a one-frame change of input timing (lead 2, 3, 4 give three
//   different trajectories from the same movie). That is how a game behaves whose outcome depends on
//   which frame its input or RNG is sampled in, so the divergence is frame-phase, not a drifting
//   sub-frame error.
//
// ANSWERS
// 1. Not power-on alignment in the sub-frame sense: 0/1/2 change nothing and 3 is a one-frame shift.
//    What this sweep can say is that the outcome is set by which whole frame the Start presses (and
//    hence the title's RNG/input sampling) land in. It cannot separate RNG timing from input-poll
//    timing: both are frame-quantised, and no RNG address was identified. That split is open.
// 2. No alignment in 0..=3 makes lead 0 survive (see table). The lead-in compensates for the
//    emulator reaching the title input window about 3 frames before FCEUX (the same offset the Non
//    Human notes above found with a different game), i.e. a systematic early start, not per-game luck.
//    What produces those 3 frames (PPU warm-up / vblank-wait phase at power-on?) is not measured.
// 3. Nothing here proves a real CPU/PPU emulation difference, and nothing rules one out: the movie
//    dies after the game starts at any frame offset tried. If no fixed offset exists, that is a
//    spec risk (a) and needs the user to accept re-pinned goldens first.
//    Suggested next step: diff RAM between (a0, l3) and (a0, l4) around movie frame 161 to find the
//    bytes the two runs disagree on (RNG seed vs input latch), then test which one the movie needs.
//
// The non-ignored tests pin what does work; they are a canary for our own output, not proof of
// TAS-exact playback.

const AE_ROM: &str = "roms/alter_ego.nes";
const AE_MOVIE: &str = "roms/movies/aiqiyou-alterego.fm2";
/// Blank frames in front of the movie so the first Start press lands after the title is ready.
const AE_LEAD_IN: usize = 3;
/// Alter Ego's lives counter.
const AE_LIVES: usize = 0x06BB;

fn alter_ego_movie(lead_in: usize) -> Movie {
    let text = String::from_utf8(read(AE_MOVIE)).expect("FM2 is UTF-8");
    let mut movie = parse_fm2(&text).expect("movie parses");
    for _ in 0..lead_in {
        movie.frames.insert(
            0,
            MovieFrame {
                commands: 0,
                pads: [0, 0],
            },
        );
    }
    movie
}

#[test]
fn alter_ego_rom_matches_the_movie_header() {
    let movie = alter_ego_movie(0);
    assert_eq!(movie.frames.len(), 23776);
    assert!(
        movie.frames.iter().all(|f| f.commands == 0),
        "power-on start, no reset commands"
    );
    let header = |key: &str| {
        movie
            .header
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("palFlag").as_deref(), Some("0"), "NTSC movie");
    assert_eq!(
        header("emuVersion").as_deref(),
        Some("22020"),
        "FCEUX 2.2.3"
    );
    let data = tendy_nes::cart::split(&read(AE_ROM)).expect("ROM parses");
    assert_eq!(
        Some(tendy_nes::movie::rom_checksum_for(&data)),
        header("romChecksum"),
        "the downloaded ROM is not the one the movie was recorded on"
    );
}

#[test]
fn alter_ego_movie_starts_the_game() {
    let mut nes = Nes::new(&read(AE_ROM)).expect("ROM loads");
    let mut player = Player::new(alter_ego_movie(AE_LEAD_IN));
    let mut frames = 0usize;
    while frames < 1000 && player.apply(&mut nes).expect("replay error") {
        nes.run_frame();
        frames += 1;
    }
    let at_1000 = fnv1a64(&nes.bus.ppu.framebuffer);
    let lives_at_1000 = nes.bus.ram[AE_LIVES];
    // Promenade, one life already lost at frame 966 (see the notes above).
    assert_eq!(lives_at_1000, 4, "lives at frame 1000");
    assert_eq!(
        at_1000, 0x269c_875b_8c19_5a88,
        "frame 1000 framebuffer hash changed"
    );
}

#[test]
#[ignore = "replay desyncs (see the notes above): the run dies and returns to the title"]
fn alter_ego_movie_reaches_the_ending() {
    let mut nes = Nes::new(&read(AE_ROM)).expect("ROM loads");
    let mut player = Player::new(alter_ego_movie(AE_LEAD_IN));
    let mut frames = 0usize;
    while player.apply(&mut nes).expect("replay error") {
        nes.run_frame();
        frames += 1;
    }
    assert_eq!(frames, 23776 + AE_LEAD_IN, "every movie frame was played");
    // What the replay measures today: the title screen. A real run does not end there;
    // replace this with the ending predicate once 103b fixes the desync (this test fails until then).
    assert_ne!(
        fnv1a64(&nes.bus.ppu.framebuffer),
        0xd439778c2687b279,
        "replay still ends on the title screen"
    );
}

#[test]
#[ignore = "measurement: prints one row of the divergence matrix; set AE_ALIGN and AE_LEAD"]
fn alter_ego_sweep_row() {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    };
    let (align, lead) = (env("AE_ALIGN") as u8, env("AE_LEAD"));
    let mut nes = Nes::new_aligned(&read(AE_ROM), align).expect("ROM loads");
    let mut player = Player::new(alter_ego_movie(lead));
    let mut lives = Vec::new();
    while player.apply(&mut nes).expect("replay error") {
        nes.run_frame();
        lives.push(nes.bus.ram[AE_LIVES]);
    }
    let m = &lives[lead..];
    let start = m.iter().position(|&v| v > 0);
    let drop = (1..m.len()).find(|&i| m[i] < m[i - 1] && m[i - 1] > 0);
    let over = (1..m.len()).find(|&i| m[i] == 0 && m[i - 1] > 0);
    let title = fnv1a64(&nes.bus.ppu.framebuffer) == 0xd439778c2687b279;
    println!(
        "ROW align={align} lead={lead} start={start:?} first_drop={drop:?} game_over={over:?} ends_on_title={title}"
    );
}

// BizHawk .bk2: NES Banana Nana by ShesChardcore, publication 5016M.
//
// A freely downloadable homebrew (Morphcat Games, Ludum Dare 32), mapper 0 / NROM, BizHawk 2.8 NesHawk,
// power-on start (no Core.bin), 1615 frames. Unlike Non Human above, this one replays to the end:
//
// ```text
// python tools/fetch_test_roms.py     # roms/banana_nana.nes + roms/movies/sheschardcore-banananana.bk2
// ```
//
// What the movie's own files say, and what we did about it:
// - SyncSettings.json: `InitialWRamStatePattern: null`, `Famicom: false`, both ports `ControllerNES`,
//   `RegionOverride: 0` (auto, NTSC for this ROM). Nothing to match beyond the default console. RAM
//   pattern does not matter for this game: all-zero and all-$FF work RAM give identical frames
//   (`ram_pattern_does_not_change_the_run`).
// - Header.txt SHA1 is 70EF3427..., the SHA-1 of the ROM BizHawk loaded ("Banana_Nana_LD32.nes"). The
//   Internet Archive copy the fetch tool pins (No-Intro name, iNES PRG 32K / CHR 8K) has a different
//   SHA-1 (255d074d... over PRG+CHR), so it is another build of the same game, not the exact file. The
//   movie still plays to the same place, which a 1615-frame glitch run would not do on a divergent ROM.
// - Result: at the last input frame the screen is the final room (the boss), and 120 idle frames later
//   it is the ending text ("NANA! YOU DID IT! NOW, YOU ARE THE QUEEN."). Both framebuffer hashes are pinned.
//   That is the movie's known end state; the hashes are our own output at that state (a regression pin),
//   and the two screens were looked at before they were pinned.

const BK2_ROM: &str = "roms/banana_nana.nes";
const BK2_MOVIE: &str = "roms/movies/sheschardcore-banananana.bk2";
const BK2_FRAMES: usize = 1615;
/// Idle frames after the movie ends until the ending text is on screen.
const BK2_ENDING_WAIT: usize = 120;

fn bk2_movie() -> Movie {
    let path = crate::common::rom_path(BK2_MOVIE);
    assert!(
        path.exists(),
        "missing {}: run `python tools/fetch_test_roms.py` first",
        path.display()
    );
    parse_bk2(&path).expect("the .bk2 imports (power-on start, NesHawk LogKey)")
}

/// Play the whole movie on a console with `ram` power-on work RAM, then `BK2_ENDING_WAIT` idle frames.
/// Returns the framebuffer hash after the last input frame and after the wait.
fn play_bk2(ram: RamPowerOn) -> (u64, u64) {
    let mut nes = Nes::new_with_power_on(&read(BK2_ROM), ram, 0).expect("ROM loads (mapper 0)");
    let mut player = Player::new(bk2_movie());
    let mut frames = 0usize;
    while player.apply(&mut nes).expect("replay error") {
        nes.run_frame();
        frames += 1;
    }
    assert_eq!(frames, BK2_FRAMES, "every movie frame was played");
    let at_end = fnv1a64(&nes.bus.ppu.framebuffer);
    for _ in 0..BK2_ENDING_WAIT {
        nes.run_frame();
    }
    (at_end, fnv1a64(&nes.bus.ppu.framebuffer))
}

#[test]
fn banana_nana_bk2_reaches_the_ending() {
    let movie = bk2_movie();
    assert_eq!(movie.frames.len(), BK2_FRAMES);
    assert!(
        movie.frames.iter().all(|f| f.commands == 0),
        "power-on start, no reset commands"
    );
    let (at_end, ending) = play_bk2(RamPowerOn::Zero);
    assert_eq!(
        at_end, 0x0c8e_f23c_1601_88b2,
        "final room at the last input frame changed"
    );
    assert_eq!(ending, 0xaa48_2984_2cf6_995a, "ending text screen changed");
}

#[test]
fn ram_pattern_does_not_change_the_run() {
    assert_eq!(play_bk2(RamPowerOn::Zero), play_bk2(RamPowerOn::Ones));
}
