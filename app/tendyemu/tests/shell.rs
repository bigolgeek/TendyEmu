//! The shell, end to end and headless (DESIGN §6.1, §8; issue #15).
//!
//! Every test here drives the same code `tendyemu <file>` and the drawer drive: `App::open` is the
//! argument path, and the drawer's rows produce the requests the window loop performs. The ROM
//! images are built in code, so no test needs a personal ROM library.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::menu::{self, Entry, ItemAct, Level, Request};
use tendyemu::paths::Paths;
use tendyemu::recents;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-shell-{name}-{}", std::process::id()));
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

/// A 32 KiB Game Boy cartridge. With `battery`, an MBC1+RAM+BATTERY cart with 8 KiB of save RAM.
fn gb_rom(battery: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"SHELLGB ");
    rom[0x147] = if battery { 0x03 } else { 0x00 };
    rom[0x148] = 0x00;
    rom[0x149] = if battery { 0x02 } else { 0x00 };
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
    rom[0xA0..0xA8].copy_from_slice(b"SHELLGBA");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An iNES cartridge: 16 KiB of PRG (NOPs, with the reset vector at $8000) and 8 KiB of CHR.
fn nes_rom() -> Vec<u8> {
    let mut rom = vec![b'N', b'E', b'S', 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.extend(std::iter::repeat_n(0xEAu8, 16 * 1024));
    rom.extend(std::iter::repeat_n(0x00u8, 8 * 1024));
    let vectors = 16 + 16 * 1024 - 6;
    // NMI, reset and IRQ all point at $8000.
    for at in [vectors, vectors + 2, vectors + 4] {
        rom[at] = 0x00;
        rom[at + 1] = 0x80;
    }
    rom
}

/// A 32 KiB LoROM cartridge: a header at $7FC0 whose map nibble names LoROM, so it scores as a
/// cartridge without a checksum this test would have to recompute.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"SHELL SNES TEST GAME ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

fn all_roms() -> Vec<(SystemKind, &'static str, Vec<u8>)> {
    vec![
        (SystemKind::Gb, "shell.gb", gb_rom(false)),
        (SystemKind::Gba, "shell.gba", gba_rom()),
        (SystemKind::Nes, "shell.nes", nes_rom()),
        (SystemKind::Snes, "shell.sfc", snes_rom()),
    ]
}

/// Opens the recent row that points at `path`, the way a click or Enter on it does.
fn open_from_recent_row(app: &mut App, path: &Path) {
    let index = app
        .recents()
        .iter()
        .position(|recent| recent.path == path)
        .expect("the ROM is in the recents list");
    app.menu_mut().open();
    app.refresh();
    app.menu_mut().set_level(Level::RecentRoms);
    app.refresh();
    let entries = app.menu().entries();
    let at = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                Entry::Item(item)
                    if matches!(item.act, ItemAct::Own(Request::OpenRecent(i)) if i == index)
            )
        })
        .expect("the recents page has a row for the ROM");
    app.menu_mut().select(at);
    assert_eq!(app.menu().cursor(), at);
    assert!(menu::enter(app.menu_mut()), "Enter records a request");
    let request = app.take_request().expect("a request is waiting");
    assert!(app.request(request), "the request is performed");
}

#[test]
fn open_each_system_by_argument() {
    // `tendyemu <file>` is `App::open(path)` with argv[1]; the window loop adds nothing.
    let scratch = Scratch::new("argument");
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        let opened = app
            .open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        assert_eq!(opened, system, "{name}");
        assert_eq!(app.system(), Some(system), "{name}");
        assert_eq!(
            app.machine().map(|machine| machine.kind()),
            Some(system),
            "{name}"
        );
        assert!(!app.held(), "{name} runs");
    }
}

#[test]
fn open_each_system_from_menu_row() {
    let scratch = Scratch::new("menu-row");
    // Open each file once so the recents list holds one row per system, then shut them down.
    let mut app = App::new(scratch.paths());
    let mut paths = Vec::new();
    for (_, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        paths.push(path);
    }
    app.close();
    assert!(app.machine().is_none());

    for path in paths {
        open_from_recent_row(&mut app, &path);
        let recent = app
            .recents()
            .iter()
            .find(|recent| recent.path == path)
            .expect("still remembered");
        assert_eq!(app.system(), Some(recent.system), "{}", path.display());
        assert!(app.machine().is_some(), "{}", path.display());
    }

    // The top row itself records the dialog request; a headless test never opens one.
    app.menu_mut().open();
    app.refresh();
    let entries = app.menu().entries();
    let at = entries
        .iter()
        .position(|entry| {
            matches!(entry, Entry::Item(item) if matches!(item.act, ItemAct::Own(Request::OpenRom)))
        })
        .expect("the drawer's first row is Open ROM");
    app.menu_mut().select(at);
    assert!(menu::enter(app.menu_mut()));
    assert_eq!(app.take_request(), Some(Request::OpenRom));
    assert!(!app.request(Request::OpenRom), "no dialog without a window");
}

#[test]
fn recents_mix_systems_and_reopen_right_core() {
    let scratch = Scratch::new("recents");
    let paths = scratch.paths();
    let mut app = App::new(paths.clone());
    let mut opened: Vec<(SystemKind, PathBuf)> = Vec::new();
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        opened.push((system, path));
    }

    // Newest first, one line per ROM: `<system key> TAB <file>`.
    let text = fs::read_to_string(paths.recents_file()).expect("recents.txt exists");
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 4, "{text}");
    for (line, (system, path)) in lines.iter().zip(opened.iter().rev()) {
        let (key, file) = line.split_once('\t').expect("a tab between key and file");
        assert_eq!(key, system.key(), "{line}");
        assert_eq!(Path::new(file), path.as_path(), "{line}");
        assert_eq!(recents::system_from_key(key), Some(*system));
    }

    let loaded = recents::load(&paths);
    assert_eq!(loaded.len(), 4);
    assert!(loaded.iter().all(|recent| !recent.gone));

    // Every core reopens from its own line, and re-opening moves it to the front.
    for (system, path) in opened.iter().rev() {
        app.close();
        app.open(path)
            .unwrap_or_else(|why| panic!("{} reopens: {why}", path.display()));
        assert_eq!(app.system(), Some(*system));
        assert_eq!(
            app.recents().first().map(|recent| recent.path.clone()),
            Some(path.clone())
        );
    }

    // The list is shared by the drawer, and the system word comes from the entry.
    app.refresh();
    app.menu_mut().open();
    app.menu_mut().set_level(Level::RecentRoms);
    app.refresh();
    let entries = app.menu().entries();
    let labels: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) if item.enabled => Some(item.row.label.clone()),
            _ => None,
        })
        .collect();
    assert!(
        labels.iter().any(|label| label.starts_with("Super NES")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label.starts_with("Game Boy —")),
        "{labels:?}"
    );
}

#[test]
fn refusal_shows_detect_text() {
    let scratch = Scratch::new("refusal");
    let mut app = App::new(scratch.paths());
    let good = scratch.rom("good.gb", &gb_rom(false));
    app.open(&good).expect("the good ROM opens");

    let junk = scratch.rom("junk.xyz", &[0u8; 64]);
    let why = app.open(&junk).expect_err("64 bytes of nothing is refused");
    assert!(why.starts_with("refused: "), "{why}");

    // §6.4: the refusal goes to the bar, and the running game keeps running.
    assert_eq!(app.system(), Some(SystemKind::Gb));
    assert!(app.machine().is_some());
    assert_eq!(
        app.refusal(),
        Some(why.as_str()),
        "the refusal screen keeps it whole"
    );
    app.show_notice(why.clone(), true);
    assert_eq!(app.bar().notice.as_deref(), Some(why.as_str()));
    assert!(app.bar().notice_error);
    app.tick(1.0 / 60.0);
    assert!(app.notice().is_some(), "an error notice stays up");
    assert_eq!(app.system(), Some(SystemKind::Gb));

    // A good file clears the refusal screen.
    let good2 = scratch.rom("good2.gb", &gb_rom(false));
    app.open(&good2).expect("the second good ROM opens");
    assert!(app.refusal().is_none());

    // A file that cannot be read at all is refused the same way.
    let missing = scratch.root.join("does-not-exist.gba");
    let why = app.open(&missing).expect_err("a missing file is refused");
    assert!(why.starts_with("refused: cannot read"), "{why}");
}

#[test]
fn battery_written_on_exit() {
    let scratch = Scratch::new("battery");
    let paths = scratch.paths();
    let mut app = App::new(paths.clone());
    let cart = scratch.rom("battery.gb", &gb_rom(true));
    app.open(&cart).expect("a battery cart opens");
    for _ in 0..30 {
        app.tick(1.0 / 60.0);
    }
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .expect("an MBC1+RAM cart has a battery body");

    app.quit_app();
    assert!(app.quit_requested());
    let saved = paths.saves_dir(SystemKind::Gb).join("battery.sav");
    let written =
        fs::read(&saved).unwrap_or_else(|why| panic!("{} is written: {why}", saved.display()));
    assert_eq!(written.len(), body.len());
    assert_eq!(written, body);
}
