//! The headless runner and the library sweep, end to end (DESIGN §6.1, §8; issue #28).
//!
//! The first test builds one small cartridge per system in code and runs it through the same
//! detection, the same core and the same frame function the window uses, so no ROM library is
//! needed. The second sweeps the maintainers' library when `TENDY_ROM_LIBRARY` names it and
//! compares the per-system counts with the last sweep each source project published; with the
//! variable unset it prints its skip sentence and passes, because the library is not in the tree.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::headless::{self, Options, Verdict};

/// A scratch folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-headless-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn rom(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the scratch ROM is writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A 32 KiB Game Boy cartridge: a jump at the entry point and a valid header checksum.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"HEADGB  ");
    rom[0x147] = 0x00;
    rom[0x148] = 0x00;
    rom[0x149] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A 32 KiB Game Boy Advance cartridge: an ARM branch at $08000000 and a sound header.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // `b $080000C0`: the offset is (0xC0 - 8) / 4 = 0x2E.
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"HEADGBA ");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An iNES cartridge: 16 KiB of PRG (NOPs, reset vector at $8000) and 8 KiB of CHR.
fn nes_rom() -> Vec<u8> {
    let mut rom = vec![b'N', b'E', b'S', 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.extend(std::iter::repeat_n(0xEAu8, 16 * 1024));
    rom.extend(std::iter::repeat_n(0x00u8, 8 * 1024));
    let vectors = 16 + 16 * 1024 - 6;
    for at in [vectors, vectors + 2, vectors + 4] {
        rom[at] = 0x00;
        rom[at + 1] = 0x80;
    }
    rom
}

/// A 32 KiB LoROM cartridge: a header at $7FC0 whose map byte names LoROM.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"HEAD SNES TEST GAME  ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

#[test]
fn headless_runs_frames_each_system() {
    let scratch = Scratch::new("frames");
    let roms = [
        ("headless.gb", gb_rom(), SystemKind::Gb, (160, 144)),
        ("headless.gba", gba_rom(), SystemKind::Gba, (240, 160)),
        ("headless.nes", nes_rom(), SystemKind::Nes, (256, 240)),
        ("headless.sfc", snes_rom(), SystemKind::Snes, (256, 224)),
    ];
    let options = Options {
        frames: 3,
        long: 3,
        any_frame: false,
    };
    for (name, bytes, system, (width, height)) in roms {
        let path = scratch.rom(name, &bytes);
        let first = headless::run_file(&path, &options);
        assert_eq!(
            first.system,
            Some(system),
            "{name}: detection names the system"
        );
        assert!(
            !matches!(first.verdict, Verdict::Refused(_) | Verdict::Crashed(_)),
            "{name}: {}",
            first.verdict.detail()
        );
        assert_eq!(first.frames, 3, "{name}: exactly the frames asked for");
        assert_eq!(
            (first.width, first.height),
            (width, height),
            "{name}: native size"
        );
        // The same file twice is the same run: the sweep's numbers are comparable between runs.
        let again = headless::run_file(&path, &options);
        assert_eq!(
            first.checksum, again.checksum,
            "{name}: two runs, one checksum"
        );
        assert_eq!(
            first.verdict, again.verdict,
            "{name}: two runs, one verdict"
        );
    }

    // The same four files as a folder: the walk finds each one and each system gets its row.
    let report = headless::sweep(&scratch.root, &options).expect("the scratch folder is readable");
    assert_eq!(report.outcomes.len(), 4, "{}", report);
    assert_eq!(
        report.tally(None).total(),
        0,
        "every file detected: {report}"
    );
    for system in SystemKind::ALL {
        assert_eq!(
            report.tally(Some(system)).total(),
            1,
            "{}: {report}",
            system.key()
        );
    }
}

/// One system's library and the last sweep its source project published.
///
/// These are measured numbers, not targets: the cores are pinned to the source commits, so a
/// difference is a regression in detection or in the core, never a reason to edit the row.
struct Source {
    /// The folder inside the library, matched case-insensitively.
    folder: &'static str,
    system: SystemKind,
    frames: u32,
    long: u32,
    any_frame: bool,
    draws: usize,
    blank: usize,
    refused: usize,
}

const SOURCES: &[Source] = &[
    // gb-rust, 1,200 frames and 1,300 before a row is called blank: 1,589 drawing, 12 blank,
    // 4 boards it refuses by name, 4 files that are not cartridges (3 boot ROMs, Game Genie).
    Source {
        folder: "gb",
        system: SystemKind::Gb,
        frames: 1200,
        long: 1300,
        any_frame: false,
        draws: 1589,
        blank: 12,
        refused: 8,
    },
    // gb-rust's colour library, every cartridge forced to Color in the source sweep.
    Source {
        folder: "gbc",
        system: SystemKind::Gb,
        frames: 1200,
        long: 1300,
        any_frame: false,
        draws: 481,
        blank: 9,
        refused: 0,
    },
    // gb-rust's Advance library at 1,200 frames, with a frame a sixth of a second later.
    Source {
        folder: "gba",
        system: SystemKind::Gba,
        frames: 1200,
        long: 1210,
        any_frame: false,
        draws: 809,
        blank: 57,
        refused: 0,
    },
    // snes-rust at 600 frames, one-colour rows re-run to 1,800, and a cartridge that drew at any
    // frame counting as drawing: 614 ok + 117 phase = 731.
    Source {
        folder: "snes",
        system: SystemKind::Snes,
        frames: 600,
        long: 1800,
        any_frame: true,
        draws: 731,
        blank: 2,
        refused: 30,
    },
];

/// `root/name`, matched case-insensitively so the same library layouts work on every platform.
fn subfolder(root: &Path, name: &str) -> Option<PathBuf> {
    let exact = root.join(name);
    if exact.is_dir() {
        return Some(exact);
    }
    fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|found| found.to_str())
                    .is_some_and(|found| found.eq_ignore_ascii_case(name))
        })
}

/// The first `limit` file names in a cell, for a mismatch message.
fn names(rows: &[&tendyemu::headless::Outcome], limit: usize) -> String {
    let shown: Vec<&str> = rows
        .iter()
        .take(limit)
        .map(|row| row.file.as_str())
        .collect();
    if rows.len() > limit {
        format!("{} … and {} more", shown.join(", "), rows.len() - limit)
    } else {
        shown.join(", ")
    }
}

#[test]
#[ignore = "the folder sweep runner: scripts/sweep.sh sets TENDY_SWEEP_DIR and drives it"]
fn sweep_a_folder() {
    // The same code `tendyemu --headless` runs, reached through the CLI so this runner proves the
    // parsing and the report as well as the sweep. The folder is an argument, never a constant:
    // the tree is public and a cartridge collection is not.
    let Some(dir) = std::env::var_os("TENDY_SWEEP_DIR") else {
        println!("# set TENDY_SWEEP_DIR to the folder to sweep");
        return;
    };
    let mut args: Vec<std::ffi::OsString> = vec!["--headless".into()];
    if let Some(frames) = std::env::var_os("TENDY_SWEEP_FRAMES") {
        args.push("--frames".into());
        args.push(frames);
    }
    if let Some(blank) = std::env::var_os("TENDY_SWEEP_BLANK_FRAMES") {
        args.push("--blank-frames".into());
        args.push(blank);
    }
    if std::env::var_os("TENDY_SWEEP_SAMPLE").is_some_and(|set| set != "0") {
        args.push("--sample".into());
    }
    args.push(dir);
    assert_eq!(headless::cli(&args), 0, "the sweep ran");
}

#[test]
fn sweep_counts_match_sources() {
    let Some(root) = std::env::var_os("TENDY_ROM_LIBRARY") else {
        println!("# skipped: TENDY_ROM_LIBRARY is not set (no library swept)");
        return;
    };
    let root = PathBuf::from(root);
    let mut compared = 0;
    for source in SOURCES {
        let Some(dir) = subfolder(&root, source.folder) else {
            println!(
                "# skipped: no {} folder under TENDY_ROM_LIBRARY",
                source.folder
            );
            continue;
        };
        let options = Options {
            frames: source.frames,
            long: source.long,
            any_frame: source.any_frame,
        };
        let report = headless::sweep(&dir, &options)
            .unwrap_or_else(|why| panic!("{}: {why}", dir.display()));
        let tally = report.tally(Some(source.system));
        println!(
            "{}: draws {} blank {} refused {} crash {} total {}",
            source.folder,
            tally.draws,
            tally.blank,
            tally.refused,
            tally.crashed,
            tally.total()
        );
        let unnamed = report.tally(None);
        assert_eq!(
            unnamed.total(),
            0,
            "{}: {} file(s) detection could not name: {}",
            source.folder,
            unnamed.total(),
            names(&report.files(None, headless::VerdictKind::Refused), 20)
        );
        assert_eq!(
            tally.draws,
            source.draws,
            "{}: the source last swept {} drawing; naming the rows that differ: {}",
            source.folder,
            source.draws,
            names(
                &report.files(Some(source.system), headless::VerdictKind::Draws),
                20
            )
        );
        assert_eq!(
            tally.blank,
            source.blank,
            "{}: the source last swept {} blank: {}",
            source.folder,
            source.blank,
            names(
                &report.files(Some(source.system), headless::VerdictKind::Blank),
                20
            )
        );
        assert_eq!(
            tally.refused + tally.crashed,
            source.refused,
            "{}: the source last swept {} not loading: {} refused, {} crashed",
            source.folder,
            source.refused,
            names(
                &report.files(Some(source.system), headless::VerdictKind::Refused),
                20
            ),
            names(
                &report.files(Some(source.system), headless::VerdictKind::Crashed),
                20
            )
        );
        assert_eq!(
            tally.total(),
            source.draws + source.blank + source.refused,
            "{}: the folder holds a different number of files from the source sweep",
            source.folder
        );
        compared += 1;
    }
    assert!(
        compared > 0,
        "TENDY_ROM_LIBRARY names a folder with none of the library's subfolders"
    );
}
