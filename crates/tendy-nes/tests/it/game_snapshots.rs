//! Frame-hash ratchet over real commercial games (`roms/games/`).
//!
//! The accuracy suites (`accuracycoin`, blargg) prove the units; this proves that a real
//! game still draws the same picture after a rendering, mapper or timing change. It is a
//! ratchet, not an oracle: it only fails when a hash moves away from the committed baseline.
//!
//! Two hashes per game, both compared:
//! * `digest` - a rolling fold of the framebuffer on **every** frame of the run. This is the
//!   sensitive one: a difference in any single frame moves it, including frames the final
//!   screenshot does not show (a fade, a one-frame glitch, a scanline status bar).
//! * `screen` - FNV-1a 64 of the final framebuffer, so a failure says which game and gives a
//!   value you can compare with a regenerated baseline.
//!
//! * `tests/game_snapshots.txt` - the checkpoint table: `<rom file>` TAB `<frames>`.
//!   One fixed frame count per game. Emulation is deterministic, so the same ROM at the same
//!   frame count must always produce the same picture.
//! * `tests/game_snapshots_baseline.txt` - `<rom file>` TAB `<frames>` TAB `<digest>` TAB
//!   `<screen>` TAB `<distinct colours>`.
//!
//! Run with `-- --nocapture` to see the score line: a plain `cargo test` prints `ok`
//! whatever the score is. The `Integration tests` step in `.github/workflows/accuracy.yml` passes
//! `--nocapture` for exactly that reason.
//!
//! `GAME_SNAPSHOTS_WRITE_BASELINE=1` rewrites the baseline. Before committing a regenerated
//! baseline, look at the frames that changed: a hash move is either a deliberate rendering fix
//! or a regression, and the hash cannot tell you which.
//!
//! ROMs are copyrighted and are not in the repository, and nothing downloads them (they are
//! commercial titles, not the test ROMs in `tools/fetch_test_roms.py`), so every case on a CI
//! runner is skipped and there is no count to assert against. The suite therefore ends with a
//! single `game snapshots: SKIPPED n/m` line instead: with `--nocapture` it is the first
//! thing in the log, so "skipped everything" no longer reads the same as "ran everything".
//!
//! What this does *not* cover: these are boot screens (title screens, menus, attract demos)
//! reached without input. Gameplay needs a recorded input movie per game; the movie recorder
//! exists for that, and adding gameplay checkpoints is a separate step.

use tendy_nes::nes::Nes;

const GAMES: &str = include_str!("../game_snapshots.txt");
const BASELINE: &str = include_str!("../game_snapshots_baseline.txt");

struct Case {
    name: String,
    frames: u32,
}

struct Row {
    name: String,
    frames: u32,
    digest: u64,
    screen: u64,
    colours: usize,
}

fn parse_table(text: &str, min_fields: usize, parse: impl Fn(&[&str]) -> Option<Row>) -> Vec<Row> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let parts: Vec<&str> = l.split('\t').map(str::trim).collect();
            if parts.len() < min_fields {
                None
            } else {
                parse(&parts)
            }
        })
        .collect()
}

fn cases() -> Vec<Case> {
    parse_table(GAMES, 2, |p| {
        Some(Row {
            name: p[0].to_string(),
            frames: p[1].parse().ok()?,
            digest: 0,
            screen: 0,
            colours: 0,
        })
    })
    .into_iter()
    .map(|r| Case {
        name: r.name,
        frames: r.frames,
    })
    .collect()
}

fn baseline() -> Vec<Row> {
    parse_table(BASELINE, 5, |p| {
        Some(Row {
            name: p[0].to_string(),
            frames: p[1].parse().ok()?,
            digest: u64::from_str_radix(p[2], 16).ok()?,
            screen: u64::from_str_radix(p[3], 16).ok()?,
            colours: p[4].parse().ok()?,
        })
    })
}

fn rom_path(name: &str) -> std::path::PathBuf {
    crate::common::rom_root().join("games").join(name)
}

/// Fold one framebuffer into the rolling digest. The framebuffer is 256*240 = 61440 bytes, a
/// whole number of 8-byte chunks, and the fold is one multiply per chunk, so hashing every
/// frame of a 1200-frame run costs milliseconds rather than seconds.
fn fold(digest: u64, fb: &[u8]) -> u64 {
    let mut h = digest;
    for chunk in fb.as_chunks::<8>().0 {
        h ^= u64::from_le_bytes(*chunk);
        h = h.rotate_left(11).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    h
}

fn screen_hash(fb: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in fb {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn colours(fb: &[u8]) -> usize {
    let mut seen = [false; 64];
    for &px in fb {
        seen[(px & 0x3F) as usize] = true;
    }
    seen.iter().filter(|s| **s).count()
}

/// Boot `name`, run `frames` frames folding every frame, return (digest, screen, colours).
fn snapshot(name: &str, frames: u32) -> Result<(u64, u64, usize), String> {
    let rom = std::fs::read(rom_path(name)).map_err(|e| e.to_string())?;
    let mut nes = Nes::new(&rom).map_err(|e| format!("{e:?}"))?;
    let mut digest: u64 = 0x243f_6a88_85a3_08d3;
    for _ in 0..frames {
        nes.run_frame();
        digest = fold(digest, &nes.bus.ppu.framebuffer);
    }
    let fb = &nes.bus.ppu.framebuffer;
    Ok((digest, screen_hash(fb), colours(fb)))
}

#[test]
fn game_frames_match_baseline() {
    let cases = cases();
    assert!(!cases.is_empty(), "tests/game_snapshots.txt has no cases");

    let write = std::env::var("GAME_SNAPSHOTS_WRITE_BASELINE").is_ok_and(|v| v == "1");
    let known = baseline();

    let mut rows: Vec<String> = Vec::new();
    let mut pass = 0usize;
    let mut skipped = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();

    for case in &cases {
        let (digest, screen, cols) = match snapshot(&case.name, case.frames) {
            Ok(v) => v,
            Err(_) if !rom_path(&case.name).exists() => {
                skipped += 1;
                println!("game {:<56} SKIP (no rom)", case.name);
                continue;
            }
            Err(e) => {
                println!("game {:<56} FAIL (load: {e})", case.name);
                failed.push((case.name.clone(), format!("load error: {e}")));
                continue;
            }
        };
        rows.push(format!(
            "{}\t{}\t{:016x}\t{:016x}\t{}",
            case.name, case.frames, digest, screen, cols
        ));

        let want = known
            .iter()
            .find(|r| r.name == case.name && r.frames == case.frames);

        match want {
            Some(w) if w.digest == digest && w.screen == screen => {
                pass += 1;
                println!(
                    "game {:<56} {:>4}f PASS {digest:016x}/{screen:016x}",
                    case.name, case.frames
                );
            }
            Some(w) => {
                let mut why = String::new();
                if w.digest != digest {
                    why.push_str(&format!("digest {digest:016x} != {:016x}", w.digest));
                }
                if w.screen != screen {
                    if !why.is_empty() {
                        why.push_str(", ");
                    }
                    why.push_str(&format!("screen {screen:016x} != {:016x}", w.screen));
                }
                if w.colours > 1 && cols == 1 {
                    why.push_str(" (screen went blank)");
                }
                println!(
                    "game {:<56} {:>4}f FAIL {why} colours {cols}/{}",
                    case.name, case.frames, w.colours
                );
                failed.push((case.name.clone(), why));
            }
            None => {
                println!(
                    "game {:<56} {:>4}f NO BASELINE (new case or changed frame count)",
                    case.name, case.frames
                );
                failed.push((
                    case.name.clone(),
                    "no baseline row for this frame count".to_string(),
                ));
            }
        }
    }

    if write {
        let mut out = String::from(
            "# Generated by GAME_SNAPSHOTS_WRITE_BASELINE=1. Inspect changed frames before committing.\n\
             # <rom file>\t<frames>\t<digest of every frame>\t<final screen fnv1a-64>\t<distinct colours>\n",
        );
        for r in &rows {
            out.push_str(r);
            out.push('\n');
        }
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/game_snapshots_baseline.txt"),
            out,
        )
        .expect("write baseline");
        println!("wrote {} baseline rows", rows.len());
        return;
    }

    println!(
        "\ngame snapshots: {pass} / {} pass, {skipped} skipped, {} failed",
        cases.len(),
        failed.len()
    );
    if skipped > 0 {
        // One grep-able line, because a passing test's stdout is only in the CI log when the
        // workflow passes `--nocapture`.
        println!(
            "game snapshots: SKIPPED {skipped}/{} cases (roms/games/ absent; commercial roms are never committed)",
            cases.len()
        );
    }
    if !failed.is_empty() {
        let mut msg = String::from("frame-hash regressions:\n");
        for (n, why) in &failed {
            msg.push_str(&format!("  {n}: {why}\n"));
        }
        msg.push_str(
            "If the new frames are correct, rerun with GAME_SNAPSHOTS_WRITE_BASELINE=1 and inspect what moved.",
        );
        panic!("{msg}");
    }
    // Every asset absent (a CI runner: these ROMs are commercial and never committed) leaves
    // nothing to compare, so there is no ratchet to run. If only *some* are absent the counts
    // above say so, and the guard below still catches a table that compares nothing.
    if skipped == cases.len() {
        return;
    }
    assert!(pass > 0, "no case was actually compared");
}
