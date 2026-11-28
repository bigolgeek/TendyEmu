//! DESIGN §8 (#25): the parity table is true of the running app.
//!
//! `docs/parity/gb.md` is the checklist a player can read. This test parses it and checks every
//! `yes` row against the thing it names: the drawer's real rows, the real window keys, the real
//! picture size, the real bar. A row that stops being reachable fails here, so the document cannot
//! drift away from the app.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::menu::{Level, Module, Rows};
use tendyemu::paths::Paths;

/// The checklist under test.
const DOC: &str = include_str!("../../../docs/parity/gb.md");
/// The window, for the keys it binds itself (the drawer, `Tab`, `Escape`).
const MAIN: &str = include_str!("../src/main.rs");

/// A scratch config root, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-parity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch root");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    fn rom(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the ROM");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A 32 KiB monochrome cartridge with a valid header checksum.
fn gb_rom(colour: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"PARITY  ");
    if colour {
        rom[0x143] = 0x80;
    }
    rom[0x148] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A 32 KiB Advance cartridge: a branch at the entry point and a sound driver's ID strings.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"PARITY!!");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// The labels on one page of the drawer, its own rows first and then the modules'.
fn rows_on(app: &App, level: Level) -> Vec<String> {
    let rows = Rows::collect(app);
    let mut out: Vec<String> = rows
        .own(level)
        .iter()
        .map(|row| row.label.clone())
        .collect();
    for group in rows.groups() {
        if group.level == level && group.module != Module::Menu {
            out.extend(group.rows.iter().map(|row| row.label.clone()));
        }
    }
    out
}

fn page(title: &str) -> Option<Level> {
    Level::ALL.into_iter().find(|level| level.title() == title)
}

/// One row of the document.
struct Check {
    line: usize,
    feature: String,
    status: String,
    at: String,
}

fn checks() -> Vec<Check> {
    let mut out = Vec::new();
    for (index, line) in DOC.lines().enumerate() {
        let line = line.trim();
        if !line.starts_with('|') || line.starts_with("| ---") || line.starts_with("| Feature") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(
            cells.len(),
            3,
            "gb.md:{}: a row is Feature | status | where, got {line}",
            index + 1
        );
        assert!(
            cells[1] == "yes" || cells[1] == "gap",
            "gb.md:{}: status `{}` is neither `yes` nor `gap`",
            index + 1,
            cells[1]
        );
        out.push(Check {
            line: index + 1,
            feature: cells[0].to_string(),
            status: cells[1].to_string(),
            at: cells[2].to_string(),
        });
    }
    assert!(
        out.len() > 40,
        "the parity table is too short to be the whole app"
    );
    out
}

#[test]
fn gb_parity_rows_reachable() {
    let scratch = Scratch::new("rows");
    let gb_path = scratch.rom("parity.gb", &gb_rom(false));
    let colour_path = scratch.rom("misleading.gb", &gb_rom(true));
    let gba_path = scratch.rom("parity.gba", &gba_rom());

    let mut gb = App::new(scratch.paths());
    gb.open(&gb_path).expect("the monochrome cartridge opens");
    let mut cgb = {
        let mut app = App::new(scratch.paths());
        app.open(&colour_path).expect("the colour cartridge opens");
        app
    };
    let mut gba = {
        let mut app = App::new(scratch.paths());
        app.open(&gba_path).expect("the Advance cartridge opens");
        app
    };

    // The picture each run draws, taken once: `frame()` needs the machine mutably.
    let pictures: Vec<(u32, u32)> = [&mut gb, &mut cgb, &mut gba]
        .into_iter()
        .filter_map(|app| {
            app.machine_mut().map(|machine| {
                let frame = machine.frame();
                (frame.width, frame.height)
            })
        })
        .collect();

    // A drawer row is reachable if one of the three runs holds it: a Game Boy-only row and an
    // Advance-only row both count.
    let drawer = [&gb, &cgb, &gba];
    let has_row = |title: &str, label: &str| {
        let Some(level) = page(title) else {
            return false;
        };
        drawer
            .iter()
            .any(|app| rows_on(app, level).iter().any(|held| held == label))
    };

    // A key in config.toml is reported on the Emulation page rather than stopping the app.
    let ignored = {
        let scratch = Scratch::new("ignored");
        fs::write(scratch.paths().config_file(), "[app]\nbogus = 1\n").expect("the file");
        let app = App::new(scratch.paths());
        rows_on(&app, Level::Emulation)
            .iter()
            .any(|label| label == "Ignored")
    };

    let mut failures: Vec<String> = Vec::new();
    let mut keys: Vec<(usize, String, String)> = Vec::new();

    for check in checks() {
        if check.status != "yes" {
            continue;
        }
        for token in check.at.split(", ") {
            let token = token.trim();
            if let Some(name) = token.strip_prefix("key ") {
                keys.push((check.line, check.feature.clone(), name.to_string()));
                continue;
            }
            let pass = if let Some(rest) = token.strip_prefix("absent ") {
                match rest.split_once(" > ") {
                    Some((title, label)) => {
                        let Some(level) = page(title) else {
                            failures.push(format!("{}: no page `{title}`", check.line));
                            continue;
                        };
                        let on_advance = rows_on(&gba, level).iter().any(|held| held == label);
                        let on_gameboy = rows_on(&gb, level).iter().any(|held| held == label);
                        on_gameboy && !on_advance
                    }
                    None => false,
                }
            } else if let Some(rest) = token.strip_prefix("frame ") {
                match rest.split_once('x') {
                    Some((w, h)) => match (w.parse::<u32>(), h.parse::<u32>()) {
                        (Ok(w), Ok(h)) => pictures.contains(&(w, h)),
                        _ => false,
                    },
                    None => false,
                }
            } else if let Some(rest) = token.strip_prefix("console ") {
                drawer.iter().any(|app| {
                    app.machine()
                        .is_some_and(|machine| machine.console_name().contains(rest))
                })
            } else if let Some(rest) = token.strip_prefix("bar ") {
                drawer.iter().any(|app| app.bar().system_label() == rest)
            } else if token == "cart" {
                drawer.iter().any(|app| !app.bar().cart.is_empty())
            } else if token == "ignored-key" {
                ignored
            } else if token.starts_with("argument ") {
                // The one argument is a path: the run was opened from a real file on disk.
                gb.rom().is_some_and(Path::is_file) && gb.system() == Some(SystemKind::Gb)
            } else if token == "mouse click" {
                // The drawer's click path starts with the window reading a left click.
                MAIN.contains("is_mouse_button_pressed")
            } else if let Some((title, label)) = token.split_once(" > ") {
                has_row(title, label)
            } else {
                failures.push(format!(
                    "{}: `{}` is not a locator I know",
                    check.line, token
                ));
                continue;
            };
            if !pass {
                failures.push(format!("{}: `{}` — `{}`", check.line, check.feature, token));
            }
        }
    }

    // Keys last: `P` pauses and `F5` writes a state, so the picture checks read the live app first.
    for (line, feature, name) in keys {
        let bound = gb.hotkey(&name) || MAIN.contains(&format!("KeyCode::{name}"));
        if !bound {
            failures.push(format!("{line}: `{feature}` — key `{name}` is not bound"));
        }
    }

    assert!(
        failures.is_empty(),
        "the parity table names {} thing(s) the app does not have:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
