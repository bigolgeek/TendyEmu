//! The Super NES parity checklist, checked against the real drawer (SPEC §6, DESIGN §8 #27).
//!
//! `docs/parity/snes.md` lists every feature of the SNES app this repository was merged from
//! and says whether it is reachable. This test parses that table and, for every `yes` row,
//! checks the place the row names: a drawer page and label, a session hotkey, a window-loop
//! key, the built-in SNES layout, the refusal screen, the first-run offer, or the test that
//! carries the claim. A row whose place disappears fails here, so the checklist cannot drift
//! from the code.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::{Buttons, SystemKind};
use tendyemu::app::App;
use tendyemu::keys::Keymap;
use tendyemu::menu::{Level, Rows};
use tendyemu::paths::Paths;

/// The checklist, embedded so a moved or renamed file fails to compile rather than passing.
const DOC: &str = include_str!("../../../docs/parity/snes.md");

/// The window loop, read as text: the keys it reads are its own table, not `App::hotkey`'s.
const MAIN: &str = include_str!("../src/main.rs");

struct Documented {
    feature: String,
    status: String,
    place: String,
}

/// The three cells of every table row in the checklist, with the markdown stripped.
fn documented() -> Vec<Documented> {
    let mut rows = Vec::new();
    for line in DOC.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 3 || cells[0] == "feature" || cells[0].starts_with("---") {
            continue;
        }
        rows.push(Documented {
            feature: cells[0].to_string(),
            status: cells[1].to_string(),
            place: cells[2].trim_matches('`').to_string(),
        });
    }
    assert!(!rows.is_empty(), "the checklist has rows");
    rows
}

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "tendyemu-snes-parity-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
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

/// A 32 KiB LoROM cartridge, the smallest image the SNES header score accepts.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"PARITY SNES CHECKER  ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

/// A fresh app with the synthetic cartridge open, so every module answers for a Super NES.
fn snes_app(scratch: &Scratch) -> App {
    let mut app = App::new(scratch.paths());
    let rom = scratch.rom("parity.sfc", &snes_rom());
    app.open(&rom).expect("the synthetic cartridge opens");
    app
}

/// The selectable labels of one drawer page, in drawing order.
fn labels(app: &App, level: Level) -> Vec<String> {
    Rows::collect(app)
        .groups()
        .iter()
        .filter(|group| group.level == level)
        .flat_map(|group| group.rows.iter())
        .map(|row| row.label.clone())
        .collect()
}

fn level_named(page: &str) -> Level {
    Level::ALL
        .iter()
        .copied()
        .find(|level| level.title() == page)
        .unwrap_or_else(|| panic!("the checklist names a drawer page that does not exist: {page}"))
}

fn button_named(name: &str) -> Buttons {
    match name {
        "Up" => Buttons::UP,
        "Down" => Buttons::DOWN,
        "Left" => Buttons::LEFT,
        "Right" => Buttons::RIGHT,
        "A" => Buttons::A,
        "B" => Buttons::B,
        "X" => Buttons::X,
        "Y" => Buttons::Y,
        "L" => Buttons::L,
        "R" => Buttons::R,
        "Start" => Buttons::START,
        "Select" => Buttons::SELECT,
        other => panic!("the checklist names a pad button that does not exist: {other}"),
    }
}

/// Reads a file named by a `tests:` row: a test file under the app, or one under `crates/`.
fn test_source(spec: &str) -> (PathBuf, String, String) {
    let (file, name) = spec
        .split_once("::")
        .unwrap_or_else(|| panic!("a tests: row is `<file>::<name>`, got {spec:?}"));
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = if file.starts_with("crates/") {
        package.join("../..").join(file)
    } else {
        package.join(file)
    };
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("{} cannot be read: {why}", path.display()));
    (path, text, name.to_string())
}

#[test]
fn snes_parity_rows_reachable() {
    let scratch = Scratch::new("rows");
    let app = snes_app(&scratch);

    for row in documented() {
        assert!(
            row.status == "yes" || row.status == "gap",
            "{}: status is yes or gap, not {:?}",
            row.feature,
            row.status
        );
        if row.status == "gap" {
            assert!(
                row.place.split_once(':').is_some(),
                "{}: a gap names its change and size",
                row.feature
            );
            continue;
        }

        let place = row.place.as_str();
        if let Some(rest) = place.strip_prefix("drawer: ") {
            let (page, wanted) = rest
                .split_once(" > ")
                .unwrap_or_else(|| panic!("{}: a drawer place is `page > row`", row.feature));
            let level = level_named(page);
            let labels = labels(&app, level);
            if let Some(list) = wanted.strip_prefix("* ") {
                let wanted: Vec<&str> = list.split(", ").collect();
                assert_eq!(
                    labels, wanted,
                    "{}: the {page} page's rows are exactly this list",
                    row.feature
                );
            } else {
                assert!(
                    labels.iter().any(|label| label == wanted),
                    "{}: {wanted:?} is a row of the {page} page; it has {labels:?}",
                    row.feature
                );
            }
        } else if let Some(key) = place.strip_prefix("hotkey: ") {
            let mut app = snes_app(&scratch);
            assert!(
                app.hotkey(key),
                "{}: {key} is a session hotkey",
                row.feature
            );
        } else if let Some(key) = place.strip_prefix("key: ") {
            assert!(
                MAIN.contains(&format!("KeyCode::{key}")),
                "{}: main.rs reads {key}",
                row.feature
            );
        } else if let Some(pairs) = place.strip_prefix("keys: ") {
            let map = Keymap::builtin(SystemKind::Snes);
            for pair in pairs.split(", ") {
                let (button, key) = pair
                    .split_once('=')
                    .unwrap_or_else(|| panic!("{}: a layout pair is `Button=Key`", row.feature));
                assert_eq!(
                    map.get(button_named(button)),
                    key,
                    "{}: the built-in Super NES layout puts {button} on {key}",
                    row.feature
                );
            }
        } else if place == "cli: file" {
            assert!(
                MAIN.contains("args_os().nth(1)"),
                "{}: main.rs opens the first argument",
                row.feature
            );
        } else if let Some(kind) = place.strip_prefix("screen: ") {
            let mut app = App::new(scratch.paths());
            let bytes = match kind {
                "coprocessor" => {
                    let mut rom = snes_rom();
                    rom[0x7FC0 + 0x16] = 0x13; // $FFD6 = SuperFX/GSU-1
                    rom
                }
                "unreadable" => vec![0u8; 64],
                other => panic!("{}: unknown screen kind {other:?}", row.feature),
            };
            let path = scratch.rom("refused.sfc", &bytes);
            let why = app.open(&path).expect_err("the image is refused");
            assert!(
                why.starts_with("refused: "),
                "{}: a refusal is one line, got {why:?}",
                row.feature
            );
            assert_eq!(app.refusal(), Some(why.as_str()));
            assert!(app.machine().is_none(), "{}: no game runs", row.feature);
        } else if let Some(kind) = place.strip_prefix("firstrun: ") {
            assert_eq!(
                kind, "snes-rust",
                "{}: first run is the snes-rust folder",
                row.feature
            );
            let parent = std::env::temp_dir().join(format!(
                "tendyemu-snes-parity-firstrun-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&parent);
            let root = parent.join("tendyemu");
            fs::create_dir_all(&root).expect("the scratch root is writable");
            fs::create_dir_all(parent.join("snes-rust")).expect("an old folder is writable");
            let fresh = App::new(Paths::new(root));
            let offered = labels(&fresh, Level::Emulation);
            let _ = fs::remove_dir_all(&parent);
            assert!(
                offered.iter().any(|label| label == "Copy old files"),
                "{}: an old snes-rust folder opens the copy offer; Emulation has {offered:?}",
                row.feature
            );
        } else if let Some(spec) = place.strip_prefix("tests: ") {
            let (path, text, name) = test_source(spec);
            assert!(
                text.contains(&format!("fn {name}(")),
                "{}: {} carries `{name}`",
                row.feature,
                path.display()
            );
        } else if place == "all-rows: desc" {
            for level in Level::ALL {
                for group in Rows::collect(&app)
                    .groups()
                    .iter()
                    .filter(|group| group.level == level)
                {
                    for entry in &group.rows {
                        assert!(
                            !entry.desc.is_empty(),
                            "{}: {:?} on the {} page explains itself",
                            row.feature,
                            entry.label,
                            level.title()
                        );
                    }
                }
            }
        } else {
            panic!(
                "{}: unknown place {place:?}; the checklist's locator vocabulary is drawer, hotkey, key, keys, cli, screen, firstrun, all-rows and tests",
                row.feature
            );
        }
    }
}
