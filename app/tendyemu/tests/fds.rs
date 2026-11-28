//! FDS disks open with the BIOS the settings name (docs/parity/nes.md; DESIGN §2.10, §6.2;
//! issue #86).
//!
//! A disk is the one NES image that is not self-contained: its first instruction is in the FDS
//! BIOS, so a `.fds` or `.qd` image needs an 8192-byte dump of the console's own ROM. These tests
//! build both ends in code — a synthetic BIOS that paints the screen, and a one-side disk with a
//! plausible block layout — so no copyrighted file is needed to prove a disk boots on side A.
//! The refusal path is checked too: a disk with no BIOS says which setting to fix, in one line.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::config::{self, Config};
use tendyemu::menu::{Module, Rows};
use tendyemu::paths::Paths;
use tendyemu::settings::{self, Row};

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!("tendyemu-fds-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the scratch file is writable");
        path
    }

    /// The config file the drawer writes: `[nes] fds_bios` names the BIOS at `bios`.
    fn choose_bios(&self, bios: &Path) {
        let mut config = Config::new();
        config.set(
            "nes",
            settings::key(Row::NesFdsBios),
            bios.to_string_lossy(),
        );
        config::save(&self.paths(), &config).expect("the config file is writable");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// An 8 KB FDS BIOS that boots, paints the screen white and loops: the reset vector points at
/// code that sets palette entry 1 to white, fills pattern tile 0 with it, turns the background on
/// and jumps to itself. A real `disksys.rom` does far more, but a frame with pixels in it can
/// only happen if the CPU ran from the BIOS, which is what "reaches its first frame" means here.
fn fds_bios() -> Vec<u8> {
    let mut program: Vec<u8> = vec![
        0x78, // sei
        0xA2, 0xFF, // ldx #$FF
        0x9A, // txs
        0xA9, 0x00, // lda #$00
        0x8D, 0x00, 0x20, // sta $2000 — NMI off
        0x8D, 0x01, 0x20, // sta $2001 — rendering off
        // Palette: $3F00 black, $3F01 white.
        0xA9, 0x3F, 0x8D, 0x06, 0x20, // lda #$3F; sta $2006
        0xA9, 0x00, 0x8D, 0x06, 0x20, // lda #$00; sta $2006
        0xA9, 0x0F, 0x8D, 0x07, 0x20, // lda #$0F; sta $2007
        0xA9, 0x30, 0x8D, 0x07, 0x20, // lda #$30; sta $2007
        // Pattern tile 0 at $0000: plane 0 all ones, plane 1 all zeroes → colour 1.
        0xA9, 0x00, 0x8D, 0x06, 0x20, // lda #$00; sta $2006
        0xA9, 0x00, 0x8D, 0x06, 0x20, // lda #$00; sta $2006
    ];
    for _ in 0..8 {
        program.extend_from_slice(&[0xA9, 0xFF, 0x8D, 0x07, 0x20]);
    }
    for _ in 0..8 {
        program.extend_from_slice(&[0xA9, 0x00, 0x8D, 0x07, 0x20]);
    }
    // Show the background, leftmost eight pixels included.
    program.extend_from_slice(&[0xA9, 0x0A, 0x8D, 0x01, 0x20]);
    // jmp to itself.
    let here = 0xE000u16 + program.len() as u16;
    program.push(0x4C);
    program.extend_from_slice(&here.to_le_bytes());

    let mut bios = vec![0u8; tendy_nes::mapper::fds::BIOS_LEN];
    bios[..program.len()].copy_from_slice(&program);
    for vector in [0x1FFA, 0x1FFC, 0x1FFE] {
        bios[vector..vector + 2].copy_from_slice(&0xE000u16.to_le_bytes());
    }
    bios
}

/// One side of a `.fds` disk: disk info, file count, one file header and four bytes of file data.
fn fds_image() -> Vec<u8> {
    let mut side = vec![0u8; tendy_nes::mapper::fds::SIDE_LEN];
    side[0] = 0x01;
    side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
    side[0x38] = 0x02;
    side[0x39] = 1;
    side[0x3A] = 0x03;
    side[0x3A + 0x0B] = 0x00;
    side[0x3A + 0x0C] = 0x60;
    side[0x3A + 0x0D] = 4;
    side[0x4A] = 0x04;
    side[0x4B..0x4F].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let mut disk = Vec::from(tendy_nes::mapper::fds::FDS_MAGIC);
    disk.push(1);
    disk.extend_from_slice(&[0u8; 11]);
    disk.extend_from_slice(&side);
    disk
}

/// The same single side as a `.qd` image: 65536-byte sides, with the two CRC bytes after every
/// block that the board strips on the way in.
fn qd_image() -> Vec<u8> {
    let mut side = vec![0u8; tendy_nes::mapper::fds::QD_SIDE_LEN];
    let mut at = 0;
    side[at] = 0x01;
    side[at + 1..at + 15].copy_from_slice(b"*NINTENDO-HVC*");
    at += 0x38 + 2;
    side[at] = 0x02;
    side[at + 1] = 1;
    at += 0x02 + 2;
    side[at] = 0x03;
    side[at + 0x0B] = 0x00;
    side[at + 0x0C] = 0x60;
    side[at + 0x0D] = 4;
    at += 0x10 + 2;
    side[at] = 0x04;
    side[at + 1..at + 5].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    side
}

/// A stored (uncompressed) zip holding one file: the archive the user drops a disk in.
fn stored_zip(name: &str, bytes: &[u8]) -> Vec<u8> {
    let crc = tendy_nes::checksum::crc32(bytes);
    let size = bytes.len() as u32;
    let name = name.as_bytes();
    let mut zip = Vec::new();
    // Local file header.
    zip.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&crc.to_le_bytes());
    zip.extend_from_slice(&size.to_le_bytes());
    zip.extend_from_slice(&size.to_le_bytes());
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(name);
    zip.extend_from_slice(bytes);
    // Central directory.
    let central_at = zip.len() as u32;
    zip.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&crc.to_le_bytes());
    zip.extend_from_slice(&size.to_le_bytes());
    zip.extend_from_slice(&size.to_le_bytes());
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(name);
    let central_len = zip.len() as u32 - central_at;
    // End of central directory.
    zip.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&central_len.to_le_bytes());
    zip.extend_from_slice(&central_at.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip
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

/// The smallest Game Boy cartridge the core accepts: 32 KiB, ROM only, a valid header checksum.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"SHELLGB ");
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A scratch folder holding both ends of a bootable disk, with the BIOS chosen in the config.
fn disk_app(scratch: &Scratch) -> (App, PathBuf) {
    let bios = scratch.write("disksys.rom", &fds_bios());
    scratch.choose_bios(&bios);
    let disk = scratch.write("game.fds", &fds_image());
    (App::new(scratch.paths()), disk)
}

#[test]
fn an_fds_image_reaches_its_first_frame() {
    let scratch = Scratch::new("first-frame");
    let (mut app, disk) = disk_app(&scratch);
    app.open(&disk).expect("the disk opens with a BIOS set");
    assert_eq!(app.system(), Some(SystemKind::Nes));

    let machine = app.machine_mut().expect("a machine is running");
    assert_eq!(machine.cart_storage(), "FDS disk");
    assert_eq!(machine.disk_side(), Some((0, 1)), "side A is mounted");
    machine.run_frame();
    assert_eq!(machine.frames(), 1, "the disk reached its first frame");

    let frame = machine.frame();
    assert_eq!((frame.width, frame.height), (256, 240));
    assert!(frame.is_complete(), "the frame is whole");
    let painted = frame
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[0] > 0x80 && pixel[1] > 0x80 && pixel[2] > 0x80);
    assert!(painted, "the BIOS ran and painted the screen");
}

#[test]
fn a_disk_without_a_bios_refuses_and_names_the_setting() {
    let scratch = Scratch::new("no-bios");
    let disk = scratch.write("game.fds", &fds_image());
    let mut app = App::new(scratch.paths());
    let why = app
        .open(&disk)
        .expect_err("a disk with no BIOS must be refused");
    assert!(why.starts_with("refused: "), "{why}");
    assert!(
        why.contains("FDS BIOS"),
        "the refusal names the setting: {why}"
    );
    assert!(why.contains("Emulation"), "and where it lives: {why}");
    assert!(!why.contains('\n'), "one line: {why}");
    assert_eq!(app.system(), None, "a refusal opens nothing");
    assert_eq!(app.refusal(), Some(why.as_str()));
}

#[test]
fn the_bios_beside_the_image_is_used_when_the_setting_is_empty() {
    let scratch = Scratch::new("beside");
    scratch.write("disksys.rom", &fds_bios());
    let disk = scratch.write("game.fds", &fds_image());
    let mut app = App::new(scratch.paths());
    app.open(&disk)
        .expect("the disksys.rom beside the image is found, as the old NES app found it");
    let machine = app.machine_mut().expect("a machine is running");
    assert_eq!(machine.disk_side(), Some((0, 1)));
}

#[test]
fn a_zip_holding_an_fds_image_opens() {
    let scratch = Scratch::new("zip");
    let bios = scratch.write("disksys.rom", &fds_bios());
    scratch.choose_bios(&bios);
    let archive = scratch.write("disks.zip", &stored_zip("game.fds", &fds_image()));
    let mut app = App::new(scratch.paths());
    app.open(&archive).expect("a .fds inside a zip opens");
    assert_eq!(app.system(), Some(SystemKind::Nes));
    let machine = app.machine_mut().expect("a machine is running");
    assert_eq!(machine.disk_side(), Some((0, 1)), "side A is mounted");
    machine.run_frame();
    assert_eq!(machine.frames(), 1);
}

#[test]
fn a_qd_image_opens_with_the_same_bios() {
    let scratch = Scratch::new("qd");
    let bios = scratch.write("disksys.rom", &fds_bios());
    scratch.choose_bios(&bios);
    let disk = scratch.write("game.qd", &qd_image());
    let mut app = App::new(scratch.paths());
    app.open(&disk).expect("a .qd image opens");
    let machine = app.machine_mut().expect("a machine is running");
    assert_eq!(machine.disk_side(), Some((0, 1)));
    machine.run_frame();
    assert_eq!(machine.frames(), 1);
}

#[test]
fn the_fds_bios_row_is_on_the_nes_menu_only() {
    let scratch = Scratch::new("row");
    let mut app = App::new(scratch.paths());
    let nes = scratch.write("cart.nes", &nes_rom());
    app.open(&nes).expect("a cartridge opens");
    let labels: Vec<String> = Rows::collect(&app)
        .of(Module::Settings)
        .iter()
        .map(|row| row.label.clone())
        .collect();
    assert!(
        labels.iter().any(|label| label == "FDS BIOS"),
        "the NES menu holds the row: {labels:?}"
    );

    let gb = scratch.write("cart.gb", &gb_rom());
    app.open(&gb).expect("a Game Boy cartridge opens");
    let labels: Vec<String> = Rows::collect(&app)
        .of(Module::Settings)
        .iter()
        .map(|row| row.label.clone())
        .collect();
    assert!(
        !labels.iter().any(|label| label == "FDS BIOS"),
        "another system's menu does not: {labels:?}"
    );
}

#[test]
fn the_row_and_its_key_never_disagree() {
    let scratch = Scratch::new("key");
    let bios = scratch.write("disksys.rom", &fds_bios());
    scratch.choose_bios(&bios);
    let loaded = config::load(&scratch.paths()).config;
    let shown = settings::text(&loaded, Some(SystemKind::Nes), Row::NesFdsBios);
    assert_eq!(shown, "disksys.rom", "the row shows the file the key holds");
    assert_eq!(settings::key(Row::NesFdsBios), "fds_bios");
    assert_eq!(
        loaded.value(SystemKind::Nes, "fds_bios"),
        Some(bios.to_string_lossy().as_ref()),
        "the row's key is the key on disk"
    );
}
