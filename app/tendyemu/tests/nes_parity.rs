//! The NES parity checklist is true (docs/parity/nes.md; DESIGN §6.1, §8; issue #26).
//!
//! Every `yes` row of the checklist is read back out of the document and checked against the
//! app itself: the drawer page it names really holds that row, the key it names really acts,
//! the loader really opens the file kinds it names, and the pad really reaches the machine.
//! A checklist row that drifts from the code fails here.

use std::fs;
use std::path::PathBuf;

use tendy_core::{Buttons, SystemKind};
use tendyemu::app::App;
use tendyemu::gamepad::{self, GamepadState};
use tendyemu::keys;
use tendyemu::menu::{Entry, Level, Menu, Rows};
use tendyemu::paths::Paths;
use tendyemu::rewind;

/// The checklist, compiled in: a `yes` row no code backs fails the test.
const DOC: &str = include_str!("../../../docs/parity/nes.md");
/// The window's key handling, for the keys the drawer never sees.
const MAIN: &str = include_str!("../src/main.rs");
/// The frame loop, for the gamepad hook.
const APP: &str = include_str!("../src/app.rs");

/// One table row of the checklist.
struct Checklist {
    feature: String,
    status: String,
    note: String,
}

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-nes-parity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the scratch image is writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
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

/// A one-song NSF whose PLAY routine starts pulse 1; INIT is a bare RTS.
fn nsf_image() -> Vec<u8> {
    let program: [u8; 22] = [
        0x60, // $8000 INIT: rts
        0xA9, 0xBF, 0x8D, 0x00, 0x40, // PLAY: lda #$bf; sta $4000
        0xA9, 0x08, 0x8D, 0x01, 0x40, // lda #$08; sta $4001
        0xA9, 0x40, 0x8D, 0x02, 0x40, // lda #$40; sta $4002
        0xA9, 0x00, 0x8D, 0x03, 0x40, // lda #$00; sta $4003
        0x60, // rts
    ];
    let mut nsf = vec![0u8; 0x80];
    nsf[0..5].copy_from_slice(b"NESM\x1A");
    nsf[5] = 1;
    nsf[6] = 1;
    nsf[7] = 1;
    nsf[8..10].copy_from_slice(&0x8000u16.to_le_bytes());
    nsf[10..12].copy_from_slice(&0x8000u16.to_le_bytes());
    nsf[12..14].copy_from_slice(&0x8001u16.to_le_bytes());
    nsf[0x0E..0x17].copy_from_slice(b"parityNES");
    nsf.extend_from_slice(&program);
    nsf
}

/// A running NES app, opened from a real file so save states have a folder.
fn nes_app(scratch: &Scratch) -> App {
    let path = scratch.write("parity.nes", &nes_rom());
    let mut app = App::new(scratch.paths());
    app.open(&path)
        .expect("the NES core accepts the built-in cart");
    assert_eq!(app.system(), Some(SystemKind::Nes));
    app
}

/// Every table row of the checklist, in document order.
fn checklist() -> Vec<Checklist> {
    let mut rows = Vec::new();
    for line in DOC.lines() {
        let line = line.trim();
        if !line.starts_with('|') || line.starts_with("|---") || line.starts_with("| Feature ") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 3, "feature | status | note, got: {line}");
        rows.push(Checklist {
            feature: cells[0].to_string(),
            status: cells[1].to_string(),
            note: cells[2].to_string(),
        });
    }
    rows
}

/// The backticked names in a note, in order.
fn names(note: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = note;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let close = rest
            .find('`')
            .unwrap_or_else(|| panic!("unclosed backtick: {note}"));
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// The drawer page a note names, by its header title.
fn page_named(page: &str) -> Level {
    Level::ALL
        .into_iter()
        .find(|level| level.title() == page)
        .unwrap_or_else(|| panic!("the drawer has no page called `{page}`"))
}

/// The page has an enabled row with exactly this label.
fn assert_row(app: &App, page: &str, label: &str) {
    let mut menu = Menu::new();
    menu.set_rows(Rows::collect(app));
    menu.set_level(page_named(page));
    let entries = menu.entries();
    let found = entries
        .iter()
        .any(|entry| matches!(entry, Entry::Item(item) if item.enabled && item.row.label == label));
    if !found {
        let labels: Vec<&str> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(item.row.label.as_str()),
                _ => None,
            })
            .collect();
        panic!("`{page} > {label}` is not an enabled row; the page holds {labels:?}");
    }
}

/// The key still does what the checklist says it does.
fn assert_key(app: &mut App, key: &str) {
    match key {
        "P" => {
            let before = app.paused();
            assert!(app.hotkey("P"), "P is a session key");
            assert_ne!(app.paused(), before, "P pauses or resumes");
            if app.paused() != before {
                app.hotkey("P");
            }
        }
        "F2" => {
            let before = app.speed();
            assert!(app.hotkey("F2"), "F2 is a session key");
            assert_ne!(app.speed(), before, "F2 steps the speed");
            app.set_speed(1.0);
        }
        "F3" => {
            let before = app.speed();
            assert!(app.hotkey("F3"), "F3 is a session key");
            assert_ne!(app.speed(), before, "F3 steps the speed back");
            app.set_speed(1.0);
        }
        "F5" => assert!(app.hotkey("F5"), "F5 saves the selected state slot"),
        "F7" => assert!(app.hotkey("F7"), "F7 loads the selected state slot"),
        "F8" => assert!(
            rewind::hotkeys(app).contains(&"F8"),
            "F8 still holds rewind for this key map"
        ),
        "Tab" => {
            assert!(
                MAIN.contains("app.set_fast_forward(is_key_down(KeyCode::Tab)"),
                "the window still holds fast-forward on Tab"
            );
            app.set_fast_forward(true);
            assert_eq!(app.effective_speed(), 4.0, "fast-forward runs at 4x");
            app.set_fast_forward(false);
        }
        "Esc" => assert!(
            MAIN.contains(
                "is_key_pressed(KeyCode::Escape) && !tendyemu::keys::claimed(app, \"Escape\")"
            ),
            "the window still opens the drawer on Escape"
        ),
        "M" => assert!(
            MAIN.contains("is_key_pressed(KeyCode::M) && !tendyemu::keys::claimed(app, \"M\")"),
            "the window still opens the drawer on M"
        ),
        "F1" => assert!(
            MAIN.contains("is_key_pressed(KeyCode::F1) && !tendyemu::keys::claimed(app, \"F1\")"),
            "the window still opens the drawer on F1"
        ),
        other => panic!("the checklist names a key the test does not know: `{other}`"),
    }
}

/// The loader opens this kind of image as a running NES machine.
fn assert_file(kind: &str) {
    let scratch = Scratch::new(&format!("file-{kind}"));
    let (name, bytes) = match kind {
        "nes" => ("parity.nes", nes_rom()),
        "nsf" => ("parity.nsf", nsf_image()),
        other => panic!("the checklist names a file kind the test does not know: `{other}`"),
    };
    let path = scratch.write(name, &bytes);
    let mut app = App::new(scratch.paths());
    app.open(&path)
        .unwrap_or_else(|why| panic!("the loader opens a .{kind}: {why}"));
    assert_eq!(app.system(), Some(SystemKind::Nes));
    assert!(
        app.machine().is_some(),
        "a .{kind} reaches a running machine"
    );
}

/// The pad path reaches the machine, and its buttons merge onto the keyboard's.
fn assert_gamepad(app: &App) {
    assert!(
        APP.contains("gamepad::on_frame(self)"),
        "the frame loop still polls the pad"
    );
    assert!(
        keys::current_pad(app).is_some(),
        "there is a keyboard pad to merge the gamepad onto"
    );
    let pad = GamepadState {
        buttons: 1,
        left_x: 0.0,
        left_y: 0.0,
    };
    let merged = gamepad::merge(Buttons::NONE, Some(&pad));
    assert_ne!(
        merged.0 & Buttons::A.0,
        0,
        "the pad's A reaches the machine's A"
    );
}

#[test]
fn nes_parity_rows_reachable() {
    let rows = checklist();
    assert!(
        rows.len() >= 60,
        "the checklist keeps its rows, found {}",
        rows.len()
    );
    for row in &rows {
        assert!(
            row.status == "yes" || row.status == "gap",
            "`{}` has status `{}`, not yes or gap",
            row.feature,
            row.status
        );
    }

    let scratch = Scratch::new("rows");
    let mut app = nes_app(&scratch);
    let yes: Vec<&Checklist> = rows.iter().filter(|row| row.status == "yes").collect();
    assert!(!yes.is_empty(), "the checklist says something is reachable");

    // Rows first: a label can change with state (Pause becomes Resume while paused).
    for row in &yes {
        let names = names(&row.note);
        assert!(
            !names.is_empty(),
            "`{}` says yes but not where",
            row.feature
        );
        for name in &names {
            if let Some((page, label)) = name.split_once(" > ") {
                assert_row(&app, page, label);
            }
        }
    }

    for row in &yes {
        for name in names(&row.note) {
            if name.contains(" > ") {
                continue;
            }
            match name.as_str() {
                "gamepad" => assert_gamepad(&app),
                file if file.starts_with("file:") => assert_file(&file[5..]),
                key => assert_key(&mut app, key),
            }
        }
    }
}
