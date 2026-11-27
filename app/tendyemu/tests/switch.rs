//! Switching systems mid-session (DESIGN §6.1, §6.4, §8; issue #20).
//!
//! One window, one audio stream, one running core: opening a ROM of another system writes the
//! running game's battery, drops what belonged to the old cartridge, and builds the new core without
//! a restart. The order in the issue's acceptance — GB → SNES → NES → GBA — is the order below.
//! Every image is built in code, so no test needs a personal ROM library.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::battery;
use tendyemu::menu::RowAct;
use tendyemu::paths::Paths;
use tendyemu::record;
use tendyemu::rewind;
use tendyemu::settings::Direction;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-switch-{name}-{}", std::process::id()));
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
    rom[0x134..0x13C].copy_from_slice(b"SWITCHGB");
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

/// A 32 KiB LoROM cartridge. With `battery`, the header claims ROM+RAM+battery and 8 KiB of SRAM.
fn snes_rom(battery: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 16].copy_from_slice(b"SWITCH SNES TEST");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = if battery { 0x02 } else { 0x00 };
    rom[at + 0x17] = 0x05;
    rom[at + 0x18] = if battery { 0x03 } else { 0x00 };
    rom
}

/// An iNES cartridge: 16 KiB of PRG (NOPs, with the reset vector at $8000) and 8 KiB of CHR. With
/// `battery`, flags 6 bit 1 is set, so the 8 KiB PRG RAM is the battery body.
fn nes_rom(battery: bool) -> Vec<u8> {
    let mut rom = vec![b'N', b'E', b'S', 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom[6] = if battery { 0x02 } else { 0x00 };
    rom.extend(std::iter::repeat_n(0xEAu8, 16 * 1024));
    rom.extend(std::iter::repeat_n(0x00u8, 8 * 1024));
    let vectors = 16 + 16 * 1024 - 6;
    for at in [vectors, vectors + 2, vectors + 4] {
        rom[at] = 0x00;
        rom[at + 1] = 0x80;
    }
    rom
}

/// A 32 KiB Game Boy Advance cartridge: the ARM branch and header the shell test uses, plus the
/// `SRAM_V` signature the core scans for, so the cart has a 32 KiB battery chip.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"SWITCHGB");
    rom[0x100..0x109].copy_from_slice(b"SRAM_V113");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

fn tick_n(app: &mut App, ticks: u32) {
    for _ in 0..ticks {
        app.tick(1.0 / 60.0);
    }
}

/// How many snapshots the rewind ring holds, read from the row the drawer shows.
fn held(app: &App) -> usize {
    let rows = rewind::rows(app);
    assert_eq!(rows.len(), 2, "the rewind rows are there while a game runs");
    rows[1]
        .value
        .strip_suffix(" held")
        .expect("the step-back row shows `N held`")
        .parse()
        .expect("a count")
}

/// A wrong file in the place a save belongs, so a switch that writes nothing is caught.
fn write_wrong_save(paths: &Paths, system: SystemKind, rom: &Path, len: usize) {
    let save = battery::save_path(paths, system, rom);
    if let Some(dir) = save.parent() {
        fs::create_dir_all(dir).expect("the saves folder is writable");
    }
    fs::write(&save, vec![0xAA; len]).expect("the wrong save is writable");
}

/// The running game's body, then a wrong file written over its save, then the switch. The save must
/// come back as the machine's bytes, not the wrong file that was there a moment before.
fn switch_writing_battery(
    app: &mut App,
    paths: &Paths,
    system: SystemKind,
    rom: &Path,
    next: &Path,
) {
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .unwrap_or_else(|| panic!("{} has a battery body", system.display_name()));
    assert!(
        !body.is_empty(),
        "{}'s body is not empty",
        system.display_name()
    );
    write_wrong_save(paths, system, rom, body.len());
    app.open(next).unwrap_or_else(|why| {
        panic!(
            "{} opens after {}: {why}",
            next.display(),
            system.display_name()
        )
    });
    let save = battery::save_path(paths, system, rom);
    let written =
        fs::read(&save).unwrap_or_else(|why| panic!("{} is written: {why}", save.display()));
    assert_eq!(
        written,
        body,
        "{} wrote its battery on the switch",
        system.display_name()
    );
}

#[test]
fn gb_snes_nes_gba_switch_writes_battery_each_time() {
    let scratch = Scratch::new("battery");
    let paths = scratch.paths();
    let mut app = App::new(paths.clone());
    let gb = scratch.rom("battery.gb", &gb_rom(true));
    let snes = scratch.rom("battery.sfc", &snes_rom(true));
    let nes = scratch.rom("battery.nes", &nes_rom(true));
    let gba = scratch.rom("battery.gba", &gba_rom());

    assert_eq!(app.open(&gb).expect("the GB cart opens"), SystemKind::Gb);
    tick_n(&mut app, 5);
    switch_writing_battery(&mut app, &paths, SystemKind::Gb, &gb, &snes);
    assert_eq!(app.system(), Some(SystemKind::Snes));

    tick_n(&mut app, 5);
    switch_writing_battery(&mut app, &paths, SystemKind::Snes, &snes, &nes);
    assert_eq!(app.system(), Some(SystemKind::Nes));

    tick_n(&mut app, 5);
    switch_writing_battery(&mut app, &paths, SystemKind::Nes, &nes, &gba);
    assert_eq!(app.system(), Some(SystemKind::Gba));

    // The last game writes on the way out too, as quitting always has.
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .expect("the SRAM cart has a battery body");
    write_wrong_save(&paths, SystemKind::Gba, &gba, body.len());
    app.close();
    assert!(app.machine().is_none());
    let save = battery::save_path(&paths, SystemKind::Gba, &gba);
    let written = fs::read(&save).expect("the GBA save is written on close");
    assert_eq!(written, body, "the GBA cart wrote its battery on close");
}

#[test]
fn refused_open_keeps_running_game() {
    let scratch = Scratch::new("refused");
    let mut app = App::new(scratch.paths());
    let gb = scratch.rom("running.gb", &gb_rom(true));
    app.open(&gb).expect("the GB cart opens");
    tick_n(&mut app, 20);
    let frames = app.machine().expect("a game runs").frames();

    let junk = scratch.rom("not-a-rom.bin", &[0x5Au8; 1_024]);
    let why = app
        .open(&junk)
        .expect_err("a file with no signature is refused");
    assert!(why.contains("refused"), "{why}");

    assert_eq!(
        app.system(),
        Some(SystemKind::Gb),
        "the old core still runs"
    );
    assert_eq!(
        app.rom(),
        Some(gb.as_path()),
        "the old game is still the game"
    );
    assert_eq!(
        app.machine().expect("a game still runs").frames(),
        frames,
        "no frame was run by the refusal"
    );
    assert!(app.refusal().is_some(), "the refusal screen has its text");

    // The refusal did not poison the next open: another system still switches.
    let nes = scratch.rom("next.nes", &nes_rom(false));
    assert_eq!(app.open(&nes).expect("the NES cart opens"), SystemKind::Nes);
    assert!(app.refusal().is_none(), "a good open clears the refusal");
}

#[test]
fn switch_drops_rewind_history() {
    let scratch = Scratch::new("rewind");
    let mut app = App::new(scratch.paths());
    let gb = scratch.rom("history.gb", &gb_rom(false));
    let nes = scratch.rom("other.nes", &nes_rom(false));

    app.open(&gb).expect("the GB cart opens");
    tick_n(&mut app, 60);
    let before = held(&app);
    assert!(before > 0, "the GB game built a rewind history: {before}");

    app.open(&nes).expect("the NES cart opens");
    tick_n(&mut app, 1);
    app.open(&gb).expect("the GB cart opens again");
    tick_n(&mut app, 1);

    assert_eq!(
        held(&app),
        0,
        "the snapshots of the first GB run did not survive the switch"
    );
}

#[test]
fn switch_closes_recording() {
    let scratch = Scratch::new("recording");
    let mut app = App::new(scratch.paths());
    let gb = scratch.rom("capture.gb", &gb_rom(false));
    let nes = scratch.rom("other.nes", &nes_rom(false));

    app.open(&gb).expect("the GB cart opens");
    tick_n(&mut app, 3);
    assert!(record::on_row(&mut app, 0, RowAct::Step(Direction::Right)));
    assert!(record::on_row(&mut app, 1, RowAct::Activate));
    tick_n(&mut app, 5);

    let dir = app.paths().recordings_dir(SystemKind::Gb);
    let gif = fs::read_dir(&dir)
        .expect("the recordings folder exists")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "gif"))
        .expect("the content run opened a GIF");

    app.open(&nes).expect("the NES cart opens");
    tick_n(&mut app, 1);

    let bytes = fs::read(&gif).unwrap_or_else(|why| panic!("{} is written: {why}", gif.display()));
    assert!(
        bytes.len() > 13,
        "the GIF has a header and at least one frame"
    );
    assert_eq!(&bytes[..6], b"GIF89a", "the file is a GIF");
    assert_eq!(
        bytes.last(),
        Some(&0x3B),
        "the switch finished the GIF with its trailer"
    );
    assert_eq!(
        record::rows(&app)[1].label,
        "Start recording",
        "no capture is running after the switch"
    );
}
