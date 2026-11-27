//! Save states, end to end and headless (DESIGN §2.7, §6.3, §8; issue #22).
//!
//! Every test drives the same path the drawer does: `states::save_slot` writes the core's own blob
//! inside the shared container, `states::load_slot` unwraps it again, and the thumbnail beside it
//! is read back through the same decoder the drawer uses. The ROM images are built in code, so no
//! test needs a personal ROM library.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendy_core::state::{MAGIC, VERSION, unwrap};
use tendyemu::app::App;
use tendyemu::paths::Paths;
use tendyemu::states;
use tendyemu::thumb;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-states-{name}-{}", std::process::id()));
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

/// A 32 KiB Game Boy cartridge, ROM only, with a valid header checksum.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"STATE GB");
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
    rom[0xA0..0xA8].copy_from_slice(b"STATEGBA");
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
    rom[at..at + 21].copy_from_slice(b"STATE SNES TEST GAME!");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

fn all_roms() -> Vec<(SystemKind, &'static str, Vec<u8>)> {
    vec![
        (SystemKind::Gb, "state.gb", gb_rom()),
        (SystemKind::Gba, "state.gba", gba_rom()),
        (SystemKind::Nes, "state.nes", nes_rom()),
        (SystemKind::Snes, "state.sfc", snes_rom()),
    ]
}

#[test]
fn own_states_round_trip_byte_identical_each_system() {
    let scratch = Scratch::new("round-trip");
    let paths = scratch.paths();
    for (system, name, bytes) in all_roms() {
        let rom = scratch.rom(name, &bytes);
        let mut app = App::new(paths.clone());
        app.open(&rom)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        for _ in 0..3 {
            app.tick(1.0 / 60.0);
        }
        let before = app
            .machine_mut()
            .expect("an open game has a machine")
            .save_state();

        let notice =
            states::save_slot(&mut app, 1).unwrap_or_else(|why| panic!("{name} saves: {why}"));
        assert!(
            notice.starts_with("state saved to slot 1"),
            "{name}: {notice}"
        );

        // The file is the container, and the blob inside it is the core's own bytes, untouched.
        let file = states::state_path(&paths, system, &rom, 1);
        let written = fs::read(&file).unwrap_or_else(|why| panic!("{}: {why}", file.display()));
        assert_eq!(&written[..4], &MAGIC, "{name}");
        assert_eq!(u16::from_le_bytes([written[4], written[5]]), VERSION);
        assert_eq!(&written[6..10], &system.tag(), "{name}");
        let inner = unwrap(&written, system).unwrap_or_else(|why| panic!("{name}: {why}"));
        assert_eq!(inner, &before[..], "{name}: the blob is the core's own");

        // A fresh machine takes the state and saves the same bytes back.
        let mut fresh = App::new(paths.clone());
        fresh
            .open(&rom)
            .unwrap_or_else(|why| panic!("{name} reopens: {why}"));
        states::load_slot(&mut fresh, 1).unwrap_or_else(|why| panic!("{name} loads: {why}"));
        let after = fresh
            .machine_mut()
            .expect("an open game has a machine")
            .save_state();
        assert_eq!(
            after, before,
            "{name}: its own state round-trips byte-identically"
        );
    }
}

#[test]
fn slot_files_named_as_source_apps() {
    let paths = Paths::new("scratch");
    let cases = [
        (
            SystemKind::Gb,
            "roms/Pokemon.gb",
            3,
            "Pokemon-3.state",
            "Pokemon-3.thumb.png",
        ),
        (
            SystemKind::Gba,
            "roms/Fusion.gba",
            9,
            "Fusion-9.state",
            "Fusion-9.thumb.png",
        ),
        (SystemKind::Nes, "roms/SMB.nes", 1, "SMB.state", "SMB.png"),
        (
            SystemKind::Snes,
            "roms/Zelda.sfc",
            5,
            "Zelda.slot5.state",
            "Zelda.slot5.png",
        ),
    ];
    for (system, rom, slot, state_name, thumb_name) in cases {
        let state = states::state_path(&paths, system, Path::new(rom), slot);
        assert_eq!(
            state,
            paths.states_dir(system).join(state_name),
            "{} state file",
            system.display_name()
        );
        assert_eq!(
            thumb::thumb_path(&state, system),
            paths.states_dir(system).join(thumb_name),
            "{} thumbnail",
            system.display_name()
        );
    }
    // The counts are the source apps' own.
    assert_eq!(states::slot_count(SystemKind::Gb), 9);
    assert_eq!(states::slot_count(SystemKind::Gba), 9);
    assert_eq!(states::slot_count(SystemKind::Nes), 1);
    assert_eq!(states::slot_count(SystemKind::Snes), 5);
    // The stem survives a cartridge file with a longer extension.
    assert_eq!(
        states::state_path(&paths, SystemKind::Gb, Path::new("roms/Metal Gear.gbc"), 9)
            .file_name()
            .unwrap()
            .to_string_lossy(),
        "Metal Gear-9.state"
    );
}

#[test]
fn thumbnail_beside_slot() {
    let scratch = Scratch::new("thumb-beside");
    let paths = scratch.paths();
    let rom = scratch.rom("Kwirk.gb", &gb_rom());
    let mut app = App::new(paths.clone());
    app.open(&rom).expect("the cartridge opens");
    app.tick(1.0 / 60.0);
    let frame = app.frame().expect("an open game has a picture");
    let (width, height, rgba) = (frame.width, frame.height, frame.rgba.to_vec());

    let notice = states::save_slot(&mut app, 2).expect("the state is written");
    assert_eq!(notice, "state saved to slot 2");

    let state = states::state_path(&paths, SystemKind::Gb, &rom, 2);
    let picture = thumb::thumb_path(&state, SystemKind::Gb);
    assert_eq!(
        picture.parent(),
        state.parent(),
        "the picture sits beside the state"
    );
    assert!(picture.is_file(), "{} is written", picture.display());
    let image = thumb::picture(&state, SystemKind::Gb).expect("the picture it just wrote");
    assert_eq!((image.width, image.height), (width, height));
    assert_eq!(
        image.rgba, rgba,
        "the picture is the frame the state was taken on"
    );
}

#[test]
fn the_thumbnail_is_the_state_name_with_thumb_png() {
    // Beside the state, same stem, same slot: `<stem>-<slot>.thumb.png` next to
    // `<stem>-<slot>.state`, the GB front end's own names.
    let paths = Paths::new("scratch");
    let rom = Path::new("roms/Tetris.gb");
    let state = states::state_path(&paths, SystemKind::Gb, rom, 3);
    let picture = thumb::thumb_path(&state, SystemKind::Gb);
    assert_eq!(
        picture.file_name().unwrap().to_string_lossy(),
        "Tetris-3.thumb.png"
    );
    assert_eq!(
        picture.parent(),
        state.parent(),
        "the two files sit in one folder"
    );
    assert_ne!(
        thumb::thumb_path(
            &states::state_path(&paths, SystemKind::Gb, rom, 1),
            SystemKind::Gb
        ),
        thumb::thumb_path(
            &states::state_path(&paths, SystemKind::Gb, rom, 2),
            SystemKind::Gb
        )
    );
}

#[test]
fn a_save_writes_the_state_and_its_thumbnail() {
    let scratch = Scratch::new("both-files");
    let paths = scratch.paths();
    let rom = scratch.rom("Kwirk.gb", &gb_rom());
    let mut app = App::new(paths.clone());
    app.open(&rom).expect("the cartridge opens");
    let frame = app.frame().expect("an open game has a picture");
    let rgba = frame.rgba.to_vec();
    let before = app
        .machine_mut()
        .expect("an open game has a machine")
        .save_state();

    let notice = states::save_slot(&mut app, 2).expect("the state is written");
    assert_eq!(notice, "state saved to slot 2");

    // The state is the bytes the machine encoded, untouched inside the container.
    let state = states::state_path(&paths, SystemKind::Gb, &rom, 2);
    let written = fs::read(&state).expect("the state file");
    assert_eq!(unwrap(&written, SystemKind::Gb), Ok(&before[..]));

    // The picture is a PNG this build's own reader turns back into the same pixels.
    let png = fs::read(thumb::thumb_path(&state, SystemKind::Gb)).expect("the thumbnail file");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let image = thumb::picture(&state, SystemKind::Gb).expect("the picture it just wrote");
    assert_eq!(image.rgba, rgba);

    // Another slot stays unadorned: only the slot that was saved has a file.
    let other = states::state_path(&paths, SystemKind::Gb, &rom, 3);
    assert!(thumb::picture(&other, SystemKind::Gb).is_none());
}

#[test]
fn a_thumbnail_that_cannot_be_written_leaves_the_state_and_names_the_path() {
    let scratch = Scratch::new("thumb-refused");
    let paths = scratch.paths();
    let rom = scratch.rom("Kwirk.gb", &gb_rom());
    let mut app = App::new(paths.clone());
    app.open(&rom).expect("the cartridge opens");
    let before = app
        .machine_mut()
        .expect("an open game has a machine")
        .save_state();

    // A folder where the picture should go: the write fails on every file system.
    let state = states::state_path(&paths, SystemKind::Gb, &rom, 1);
    let blocked = thumb::thumb_path(&state, SystemKind::Gb);
    fs::create_dir_all(&blocked).expect("the blocking folder");

    let notice = states::save_slot(&mut app, 1).expect("the state is still written");
    assert!(notice.starts_with("state saved to slot 1"), "{notice}");
    assert!(notice.contains("no thumbnail"), "{notice}");
    assert!(
        notice.contains(&blocked.display().to_string()),
        "the notice names the file it could not write: {notice}"
    );
    assert!(notice.contains("cannot write"), "{notice}");

    // The state is whole: written first, on its own, still the bytes that were handed over.
    let written = fs::read(&state).expect("the state file");
    assert_eq!(unwrap(&written, SystemKind::Gb), Ok(&before[..]));
    assert_eq!(&written[..4], &MAGIC);
    assert!(thumb::picture(&state, SystemKind::Gb).is_none());
}

#[test]
fn a_missing_or_truncated_thumbnail_draws_nothing() {
    let scratch = Scratch::new("thumb-absent");
    let paths = scratch.paths();
    let rom = scratch.rom("Kwirk.gb", &gb_rom());
    let mut app = App::new(paths.clone());
    app.open(&rom).expect("the cartridge opens");
    let state = states::state_path(&paths, SystemKind::Gb, &rom, 4);

    // Never saved: no file at all.
    assert!(thumb::picture(&state, SystemKind::Gb).is_none());

    // Saved once, then deleted: the same answer.
    states::save_slot(&mut app, 4).expect("the state is written");
    let path = thumb::thumb_path(&state, SystemKind::Gb);
    let whole = fs::read(&path).expect("the thumbnail just written");
    assert!(thumb::picture(&state, SystemKind::Gb).is_some());
    fs::remove_file(&path).expect("delete it");
    assert!(thumb::picture(&state, SystemKind::Gb).is_none());

    // A file cut short by a crash, at several lengths: every one is absent, none panics.
    for cut in [0, 8, 20, whole.len() / 2, whole.len() - 12] {
        fs::write(&path, &whole[..cut]).expect("a truncated file");
        assert!(
            thumb::picture(&state, SystemKind::Gb).is_none(),
            "{cut} bytes decoded"
        );
    }

    // A foreign file, and a folder where the picture should be, are the same nothing.
    fs::write(&path, b"not a png at all").expect("garbage");
    assert!(thumb::picture(&state, SystemKind::Gb).is_none());
    fs::remove_file(&path).expect("garbage");
    fs::create_dir_all(&path).expect("a folder in its place");
    assert!(thumb::picture(&state, SystemKind::Gb).is_none());
}
