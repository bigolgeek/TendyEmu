//! Capture end to end, headless (DESIGN §6.1, §8; issue #24).
//!
//! One test per feature, each looping over all four systems: the frame comes from
//! `Machine::frame`, so nothing here reaches into a core's own pixel format. ROM images are built
//! in code, so no test needs a personal ROM library.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::menu::{RowAct, RowKind};
use tendyemu::paths::Paths;
use tendyemu::{avi, record, rewind, screenshot};

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-capture-{name}-{}", std::process::id()));
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

/// A 32 KiB Game Boy cartridge with a valid header and checksum.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"CAPTURE ");
    rom[0x148] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A 32 KiB Game Boy Advance cartridge: a branch at $08000000 and a sound header.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"CAPTURE!");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An iNES cartridge: NOPs in PRG, an empty CHR bank and the vectors at $8000.
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

/// A 32 KiB LoROM cartridge whose header names LoROM.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"CAPTURE SNES GAME    ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

fn all_roms() -> Vec<(SystemKind, &'static str, Vec<u8>)> {
    vec![
        (SystemKind::Gb, "capture.gb", gb_rom()),
        (SystemKind::Gba, "capture.gba", gba_rom()),
        (SystemKind::Nes, "capture.nes", nes_rom()),
        (SystemKind::Snes, "capture.sfc", snes_rom()),
    ]
}

/// Runs `seconds` of emulated frames, the way the window loop's tick does.
fn run(app: &mut App, seconds: f64) {
    let per = 1.0 / 60.0;
    let ticks = (seconds / per).round() as u32;
    for _ in 0..ticks {
        app.tick(per);
    }
}

/// The file kinds a folder holds, newest name last, so a test reads the file the row just made.
fn files_in(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|why| panic!("{} is readable: {why}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|seen| seen.eq_ignore_ascii_case(extension))
        })
        .collect();
    files.sort();
    files
}

/// The width and height in a PNG's IHDR, read without decoding the image.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "the PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR", "the first chunk is IHDR");
    let read = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    (read(16), read(20))
}

#[test]
fn screenshot_native_size_each_system() {
    let scratch = Scratch::new("screenshot");
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        run(&mut app, 1.0);
        let (width, height) = {
            let machine = app.machine_mut().expect("a game is running");
            let frame = machine.frame();
            (frame.width, frame.height)
        };

        let rows = screenshot::rows(&app);
        assert_eq!(rows.len(), 1, "{name}");
        assert_eq!(rows[0].kind, RowKind::Action, "{name}");
        assert!(
            screenshot::on_row(&mut app, 0, RowAct::Activate),
            "{name}: the row is taken"
        );

        let dir = scratch.paths().screenshots_dir(system);
        let files = files_in(&dir, "png");
        assert_eq!(files.len(), 1, "{name}: one PNG in {}", dir.display());
        let png = fs::read(&files[0]).expect("the PNG is readable");
        assert_eq!(png_size(&png), (width, height), "{name}");
        assert!(
            png.windows(4).any(|window| window == b"IEND"),
            "{name}: the file is closed"
        );
    }
}

#[test]
fn rewind_steps_back_each_system() {
    let scratch = Scratch::new("rewind");
    for (_system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        // Two seconds of frames so the ring holds a few snapshots at 6 frames apart.
        run(&mut app, 2.0);

        let rows = rewind::rows(&app);
        assert_eq!(rows.len(), 2, "{name}");
        // Rewind starts on, like the Game Boy front end it comes from.
        assert_eq!(rows[0].value, "On — 10 s", "{name}");
        assert!(
            rewind::on_row(&mut app, 0, RowAct::Activate),
            "{name}: the toggle is taken"
        );
        assert_eq!(rewind::rows(&app)[0].value, "Off", "{name}");
        assert!(
            rewind::on_row(&mut app, 0, RowAct::Activate),
            "{name}: the toggle is taken again"
        );
        assert_eq!(rewind::rows(&app)[0].value, "On — 10 s", "{name}");
        run(&mut app, 2.0);
        // Three more frames, so the newest snapshot is behind the running state: stepping back
        // has somewhere to go. The snapshot cadence is six frames.
        run_frames(&mut app, 3);
        let now = state_digest(&mut app);
        let held: usize = rewind::rows(&app)[1]
            .value
            .split(' ')
            .next()
            .and_then(|count| count.parse().ok())
            .expect("the step row counts what it holds");
        assert!(held > 10, "{name}: {held} snapshots held");

        assert!(
            rewind::on_row(&mut app, 1, RowAct::Activate),
            "{name}: there is a snapshot to step back to"
        );
        let first = state_digest(&mut app);
        assert_ne!(first, now, "{name}: the machine state moved back");
        assert!(
            rewind::on_row(&mut app, 1, RowAct::Activate),
            "{name}: stepping back again works"
        );
        let second = state_digest(&mut app);
        assert_ne!(second, first, "{name}: a second step goes further back");
        // The ring is still usable after the loads: the next frames keep the session going.
        run(&mut app, 0.5);
        assert!(
            rewind::on_row(&mut app, 1, RowAct::Activate),
            "{name}: the ring refills after a rewind"
        );
    }
}

/// Runs exactly `frames` emulated frames, bypassing the pacer.
fn run_frames(app: &mut App, frames: u32) {
    let per = app
        .machine()
        .map(|machine| 1.0 / machine.frame_rate())
        .expect("a game is running");
    for _ in 0..frames {
        app.tick(per);
    }
}

/// A digest of everything `save_state` carries: two runs of the same game state hash the same.
fn state_digest(app: &mut App) -> u64 {
    let bytes = app.machine_mut().expect("a game is running").save_state();
    tendy_core::fnv1a64(&bytes)
}

#[test]
fn wav_records_each_system() {
    let scratch = Scratch::new("wav");
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        let hz = app.sample_rate();
        // The mode row cycles Off -> GIF -> WAV -> GIF+WAV -> MP4. Pick WAV by its label, because
        // the choice is a session preference shared by every game.
        for _ in 0..5 {
            if record::rows(&app)[0].value == "WAV" {
                break;
            }
            assert!(record::on_row(
                &mut app,
                0,
                RowAct::Step(tendyemu::settings::Direction::Right)
            ));
        }
        assert_eq!(record::rows(&app)[0].value, "WAV", "{name}");
        assert!(
            record::on_row(&mut app, 1, RowAct::Activate),
            "{name}: the start row is taken"
        );
        run(&mut app, 1.0);
        assert!(
            record::on_row(&mut app, 1, RowAct::Activate),
            "{name}: the stop row is taken"
        );

        let dir = scratch.paths().recordings_dir(system);
        let files = files_in(&dir, "wav");
        assert_eq!(files.len(), 1, "{name}: one WAV in {}", dir.display());
        let wav = fs::read(&files[0]).expect("the WAV is readable");
        assert_eq!(&wav[..4], b"RIFF", "{name}");
        assert_eq!(&wav[8..12], b"WAVE", "{name}");
        assert_eq!(&wav[12..16], b"fmt ", "{name}");
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().expect("four bytes")) as usize,
            wav.len() - 8,
            "{name}: the RIFF size is the file size"
        );
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "{name}: PCM");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2, "{name}: stereo");
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().expect("four bytes")),
            hz,
            "{name}: the device rate"
        );
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "{name}: 16-bit");
        assert_eq!(&wav[36..40], b"data", "{name}");
        let samples = u32::from_le_bytes(wav[40..44].try_into().expect("four bytes")) as usize;
        assert_eq!(samples, wav.len() - 44, "{name}: the data size is the file");
        assert!(
            samples > 0 && samples.is_multiple_of(4),
            "{name}: {samples} bytes"
        );
    }
}

#[test]
fn gif_records_each_system() {
    let scratch = Scratch::new("gif");
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        // The mode row cycles Off -> GIF -> WAV -> GIF+WAV -> MP4. Pick GIF by its label, because
        // the choice is a session preference shared by every game.
        for _ in 0..5 {
            if record::rows(&app)[0].value == "GIF" {
                break;
            }
            assert!(record::on_row(
                &mut app,
                0,
                RowAct::Step(tendyemu::settings::Direction::Right)
            ));
        }
        assert_eq!(record::rows(&app)[0].value, "GIF", "{name}");
        assert!(
            record::on_row(&mut app, 1, RowAct::Activate),
            "{name}: the start row is taken"
        );
        run(&mut app, 1.0);
        assert!(
            record::on_row(&mut app, 1, RowAct::Activate),
            "{name}: the stop row is taken"
        );

        let dir = scratch.paths().recordings_dir(system);
        let files = files_in(&dir, "gif");
        assert_eq!(files.len(), 1, "{name}: one GIF in {}", dir.display());
        let gif = fs::read(&files[0]).expect("the GIF is readable");
        assert_eq!(&gif[..6], b"GIF89a", "{name}");
        assert_eq!(&gif[gif.len() - 1..], &[0x3B], "{name}: the trailer");
        // The logical screen descriptor carries the frame size, little-endian.
        let width = u16::from_le_bytes([gif[6], gif[7]]);
        let height = u16::from_le_bytes([gif[8], gif[9]]);
        assert!(width > 0 && height > 0, "{name}: {width}x{height}");
    }
}

#[test]
fn avi_records_each_system() {
    let scratch = Scratch::new("avi");
    for (system, name, bytes) in all_roms() {
        let path = scratch.rom(name, &bytes);
        let mut app = App::new(scratch.paths());
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        assert_eq!(avi::rows(&app).len(), 1, "{name}");
        assert!(
            avi::on_row(&mut app, 0, RowAct::Activate),
            "{name}: the start row is taken"
        );
        run(&mut app, 1.0);
        assert!(
            avi::on_row(&mut app, 0, RowAct::Activate),
            "{name}: the stop row is taken"
        );

        let dir = scratch.paths().recordings_dir(system);
        let files = files_in(&dir, "avi");
        assert_eq!(files.len(), 1, "{name}: one AVI in {}", dir.display());
        let avi = fs::read(&files[0]).expect("the AVI is readable");
        assert_eq!(&avi[..4], b"RIFF", "{name}");
        assert_eq!(&avi[8..12], b"AVI ", "{name}");
        assert_eq!(
            u32::from_le_bytes(avi[4..8].try_into().expect("four bytes")) as usize,
            avi.len() - 8,
            "{name}: the RIFF size is the file size"
        );
        // Every frame is in the index, and the index count matches `avih.dwTotalFrames`.
        let idx = avi
            .windows(4)
            .rposition(|window| window == b"idx1")
            .unwrap_or_else(|| panic!("{name}: the index is there"));
        let entries =
            u32::from_le_bytes(avi[idx + 4..idx + 8].try_into().expect("four bytes")) / 16;
        assert!(entries > 30, "{name}: {entries} chunks indexed");
        assert_eq!(
            &avi[idx + 8..idx + 12],
            b"00db",
            "{name}: the first indexed chunk is a frame"
        );
    }
}
