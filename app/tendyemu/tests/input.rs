//! Keyboard and gamepad input, headless (DESIGN §8; issue #17).
//!
//! The four tests the design names: the two proving bindings are stored and restored per system,
//! one proving every core's buttons can be reached from the keyboard, and one proving a pad and
//! the keys merge rather than replace each other. The ROM images are built in code.

use std::fs;
use std::path::PathBuf;

use tendy_core::{Buttons, SystemKind};
use tendyemu::app::App;
use tendyemu::gamepad;
use tendyemu::keys::{self, Keymap};
use tendyemu::menu::RowAct;
use tendyemu::paths::Paths;
use tendyemu::rewind;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-input-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    /// Writes `bytes` as `name` inside the scratch folder and hands back the path.
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

/// A 32 KiB Game Boy cartridge.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"INPUTGB ");
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
    rom[0xA0..0xA8].copy_from_slice(b"INPUTGBA");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An iNES cartridge: 16 KiB of PRG and 8 KiB of CHR.
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

/// A 32 KiB LoROM Super NES cartridge.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"INPUT SNES TEST GAME ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

/// The value column of the row labelled `label` on the Input page.
fn value(app: &App, label: &str) -> String {
    keys::rows(app)
        .into_iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("no {label} row"))
        .value
}

/// The index of the row labelled `label` on the Input page.
fn row_index(app: &App, label: &str) -> usize {
    keys::rows(app)
        .iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no {label} row"))
}

/// The logical buttons a core uses (§2.5): the Game Boy has eight, the Advance adds the
/// shoulders, the NES has eight, the Super NES has all twelve.
fn core_buttons(system: SystemKind) -> Vec<Buttons> {
    let mut buttons = vec![
        Buttons::UP,
        Buttons::DOWN,
        Buttons::LEFT,
        Buttons::RIGHT,
        Buttons::A,
        Buttons::B,
        Buttons::START,
        Buttons::SELECT,
    ];
    match system {
        SystemKind::Gb | SystemKind::Nes => {}
        SystemKind::Gba => buttons.extend([Buttons::L, Buttons::R]),
        SystemKind::Snes => buttons.extend([Buttons::X, Buttons::Y, Buttons::L, Buttons::R]),
    }
    buttons
}

#[test]
fn bindings_saved_per_system() {
    let scratch = Scratch::new("saved");
    let paths = scratch.paths();
    let mut app = App::new(paths.clone());

    // Rebind A on a Game Boy: the row the Input page shows writes `<root>/gb/keys.txt`.
    app.open_bytes("one.gb", gb_rom(), None)
        .expect("the Game Boy cart opens");
    let at = row_index(&app, "A");
    assert!(keys::on_row(&mut app, at, RowAct::Key("Q".into())));
    let gb_file = paths.keys_file(SystemKind::Gb);
    let gb = fs::read_to_string(&gb_file).expect("the Game Boy keys file was written");
    assert!(gb.contains("a = Q"), "{gb}");
    assert_eq!(value(&app, "A"), "Q", "the row shows the key just pressed");
    assert!(
        !paths.keys_file(SystemKind::Nes).exists(),
        "rebinding one system did not write another"
    );

    // The same logical button on an NES, to a different key, lands in its own file.
    app.open_bytes("one.nes", nes_rom(), None)
        .expect("the NES cart opens");
    let at = row_index(&app, "A");
    assert!(keys::on_row(&mut app, at, RowAct::Key("K".into())));
    let nes =
        fs::read_to_string(paths.keys_file(SystemKind::Nes)).expect("the NES file was written");
    assert!(nes.contains("a = K"), "{nes}");
    assert!(!nes.contains("a = Q"), "{nes}");
    assert_eq!(
        fs::read_to_string(&gb_file).expect("the Game Boy file is still there"),
        gb,
        "the Game Boy file did not change when the NES was rebound"
    );
    assert_eq!(value(&app, "A"), "K");
}

#[test]
fn bindings_restored_per_system() {
    let scratch = Scratch::new("restored");
    let paths = scratch.paths();

    // Two systems, two files, written in the shipped format.
    let mut gb = Keymap::builtin(SystemKind::Gb);
    gb.rebind(Buttons::A, "Q");
    keys::save(&paths, SystemKind::Gb, &gb).expect("the Game Boy file is writable");
    let mut snes = Keymap::builtin(SystemKind::Snes);
    snes.rebind(Buttons::A, "K");
    keys::save(&paths, SystemKind::Snes, &snes).expect("the Super NES file is writable");

    // A session starts on the file of whichever system it is running.
    let mut app = App::new(paths.clone());
    app.open_bytes("one.gb", gb_rom(), None)
        .expect("the Game Boy cart opens");
    assert_eq!(value(&app, "A"), "Q");
    app.open_bytes("one.sfc", snes_rom(), None)
        .expect("the Super NES cart opens");
    assert_eq!(value(&app, "A"), "K");
    // Back to a Game Boy image: its own file again, not the one just shown.
    app.open_bytes("two.gb", gb_rom(), None)
        .expect("a second Game Boy cart opens");
    assert_eq!(value(&app, "A"), "Q");

    // And each file still parses to the map that was saved.
    assert_eq!(
        keys::load(&paths, SystemKind::Gb)
            .expect("the Game Boy file parses")
            .map,
        gb
    );
    assert_eq!(
        keys::load(&paths, SystemKind::Snes)
            .expect("the Super NES file parses")
            .map,
        snes
    );
}

#[test]
fn every_core_button_reachable_from_keyboard() {
    for system in SystemKind::ALL {
        let map = Keymap::builtin(system);
        let buttons = core_buttons(system);
        let held: Vec<String> = buttons
            .iter()
            .map(|button| map.get(*button).to_string())
            .collect();

        // One physical key a button, so the whole pad can be held at once.
        let mut distinct = held.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), held.len(), "{} reuses a key", system.key());

        // Every one of those keys is a name the window table translates.
        for key in &held {
            assert!(Keymap::is_known(key), "{system:?}: {key}");
        }

        // Pressing them all reaches every button the core has.
        let pad = keys::pad(&held, &map);
        for button in &buttons {
            assert!(pad.contains(*button), "{system:?}: {button:?}");
        }
    }

    // The layouts really are per system: Game Boy and Super NES disagree on A and B, which is
    // the issue's reason to exist.
    let gb = Keymap::builtin(SystemKind::Gb);
    let snes = Keymap::builtin(SystemKind::Snes);
    assert_ne!(gb.get(Buttons::A), snes.get(Buttons::A));
    assert_ne!(gb.get(Buttons::B), snes.get(Buttons::B));
}

#[test]
fn gamepad_merges_with_keys() {
    // A keyboard A, and a pad B held at the same time: both reach the pad word.
    let map = Keymap::builtin(SystemKind::Gb);
    let keyboard = keys::pad(&[map.get(Buttons::A).to_string()], &map);
    assert_eq!(keyboard, Buttons::A);
    let pad = gamepad::GamepadState::from_xinput(0x2000, 0, 0);
    assert!(pad.is_down(gamepad::GamepadButton::B));
    let merged = gamepad::merge(keyboard, Some(&pad));
    assert!(
        merged.contains(Buttons::A),
        "the key the keyboard held is still down"
    );
    assert!(merged.contains(Buttons::B), "the pad's button joins it");
    assert!(!merged.contains(Buttons::UP));

    // No pad, or a stick push: the keyboard's bits are still there.
    assert_eq!(gamepad::merge(keyboard, None), keyboard);
    let stick = gamepad::GamepadState {
        left_x: 0.9,
        ..Default::default()
    };
    let merged = gamepad::merge(keyboard, Some(&stick));
    assert!(merged.contains(Buttons::A));
    assert!(
        merged.contains(Buttons::RIGHT),
        "the stick's direction joins in"
    );
}

#[test]
fn rebind_refuses_every_app_key() {
    let scratch = Scratch::new("app-keys");
    let rom = scratch.rom("Kwirk.gb", &gb_rom());
    let mut app = App::new(scratch.paths());
    app.open(&rom).expect("the built cartridge opens");
    let a = row_index(&app, "A");
    let default = value(&app, "A");
    let file = scratch.paths().keys_file(SystemKind::Gb);

    for &(key, what) in keys::RESERVED {
        assert!(keys::on_row(&mut app, a, RowAct::Key(key.to_string())));
        assert_eq!(value(&app, "A"), default, "{key} did not reach the button");
        let notice = app.notice().expect("a refusal notice").text.clone();
        assert!(notice.contains(key), "the notice names {key}: {notice}");
        assert!(
            notice.contains(what),
            "the notice says what {key} does: {notice}"
        );
        assert!(!file.exists(), "{key} was not written to the keys file");
    }
    assert!(!keys::claimed(&app, "F5"), "F5 is still the window's");

    // F4 and Backspace are not the app's, so they bind; Backspace stays Select on these layouts.
    for key in ["F4", "Backspace"] {
        assert!(keys::on_row(&mut app, a, RowAct::Key(key.to_string())));
        assert_eq!(value(&app, "A"), key, "{key} still binds");
    }
    let written = fs::read_to_string(&file).expect("the file was written");
    assert!(written.contains("a = Backspace"), "{written}");
}

#[test]
fn a_keys_file_naming_an_app_key_gets_the_default_and_one_notice() {
    let scratch = Scratch::new("file-app-key");
    let paths = scratch.paths();
    let file = paths.keys_file(SystemKind::Gb);
    fs::create_dir_all(file.parent().expect("the system folder")).expect("the folder is writable");
    fs::write(&file, "a = M\nb = K\n").expect("the keys file is writable");

    let loaded = keys::load(&paths, SystemKind::Gb).expect("the file parses");
    assert_eq!(
        loaded.map.get(Buttons::A),
        Keymap::builtin(SystemKind::Gb).get(Buttons::A),
        "A keeps its built-in key"
    );
    assert_eq!(
        loaded.map.get(Buttons::B),
        "K",
        "the other line still applies"
    );
    let notice = loaded.notice.expect("one notice names the ignored line");
    assert!(notice.contains("a = M"), "{notice}");
    assert!(notice.contains("opens the drawer"), "{notice}");

    // The app's own path: opening the game reads the file and puts that one line up once.
    let mut app = App::new(paths.clone());
    app.open_bytes("one.gb", gb_rom(), None)
        .expect("the Game Boy cart opens");
    assert_eq!(value(&app, "A"), "Z", "the row shows the built-in key");
    keys::on_frame(&mut app);
    let shown = app.notice().expect("the bar says why").text.clone();
    assert!(shown.contains("a = M"), "{shown}");
    app.dismiss_notice();
    keys::on_frame(&mut app);
    assert!(
        app.notice().is_none(),
        "the notice is put up once, not every frame"
    );
}

#[test]
fn no_default_layout_uses_an_app_key() {
    for system in SystemKind::ALL {
        let map = Keymap::builtin(system);
        for (button, label) in Buttons::ALL {
            let key = map.get(button);
            assert!(
                keys::reserved(key).is_none(),
                "{} binds {key} to {label}",
                system.key()
            );
        }
    }
    assert!(
        keys::reserved("Backspace").is_none(),
        "Backspace is Select on three layouts, not an app key"
    );
}

#[test]
fn f8_rewinds_on_every_default_layout() {
    let scratch = Scratch::new("rewind-key");
    let mut app = App::new(scratch.paths());
    for (system, name, bytes) in [
        (SystemKind::Gb, "rewind.gb", gb_rom()),
        (SystemKind::Gba, "rewind.gba", gba_rom()),
        (SystemKind::Nes, "rewind.nes", nes_rom()),
        (SystemKind::Snes, "rewind.sfc", snes_rom()),
    ] {
        app.open_bytes(name, bytes, None)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        assert_eq!(app.system(), Some(system), "{name}");

        // F8 is nobody's game button on any shipped layout, so it always reaches rewind (#78).
        assert!(!keys::claimed(&app, "F8"), "{} binds F8", system.key());
        let hotkeys = rewind::hotkeys(&app);
        assert!(
            hotkeys.contains(&"F8"),
            "{} rewinds on F8: {hotkeys:?}",
            system.key()
        );

        // Backspace rewinds exactly where no button holds it: the Game Boy, Advance and Super NES
        // layouts bind it to Select; only the NES leaves it free.
        let backspace = system != SystemKind::Nes;
        assert_eq!(
            keys::claimed(&app, "Backspace"),
            backspace,
            "{} and Backspace",
            system.key()
        );
        assert_eq!(hotkeys.contains(&"Backspace"), !backspace, "{hotkeys:?}");

        // The Rewind row names the keys that actually work on this system.
        let label = &rewind::rows(&app)[0].label;
        let expected = if backspace {
            "Rewind (hold F8)"
        } else {
            "Rewind (hold F8 or Backspace)"
        };
        assert_eq!(label, expected, "{} labels its rewind keys", system.key());
    }
}
