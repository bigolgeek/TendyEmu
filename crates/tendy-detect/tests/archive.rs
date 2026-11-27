//! The tendy-detect archive tests (DESIGN §8, #14).
//!
//! Every zip and 7z archive is built in code, so the suite needs no archive files. The entry-over-
//! cap case is a unit test in `src/archive.rs`: it needs a test-only cap, not a 64 MiB allocation.

use std::io::Write;

use tendy_core::SystemKind;
use tendy_detect::archive::{ArchiveKind, extract};
use tendy_detect::{Basis, Format, Refusal, open};

/// A zip holding `entries` in the given order; a name ending in `/` is a folder.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

/// The same for a 7-Zip archive.
fn sevenz_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    for (name, bytes) in entries {
        let folder = name.ends_with('/');
        let entry = if folder {
            sevenz_rust2::ArchiveEntry::new_directory(name)
        } else {
            sevenz_rust2::ArchiveEntry::new_file(name)
        };
        writer
            .push_archive_entry(entry, (!folder).then_some(*bytes))
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// One extension-led file per system (§3.3's table), with bytes no signature claims.
fn system_images() -> Vec<(&'static str, Vec<u8>, SystemKind, Format)> {
    vec![
        ("game.gb", vec![0x00; 0x150], SystemKind::Gb, Format::Gb),
        ("game.gba", vec![0x00; 0xC0], SystemKind::Gba, Format::Gba),
        ("game.nes", vec![0x00; 16], SystemKind::Nes, Format::Ines),
        ("game.fds", vec![0x00; 16], SystemKind::Nes, Format::FdsRaw),
        ("music.nsf", vec![0x00; 0x80], SystemKind::Nes, Format::Nsf),
        (
            "game.sfc",
            vec![0x00; 0x8000],
            SystemKind::Snes,
            Format::Snes,
        ),
    ]
}

/// Row 1's minimal iNES header.
fn ines() -> Vec<u8> {
    let mut rom = vec![0x00; 16];
    rom[0..4].copy_from_slice(b"NES\x1A");
    rom
}

/// Row 7: a Game Boy cartridge, zero apart from the logo.
fn gb() -> Vec<u8> {
    const GB_LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00,
        0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD,
        0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB,
        0xB9, 0x33, 0x3E,
    ];
    let mut rom = vec![0x00; 0x150];
    rom[0x104..0x134].copy_from_slice(&GB_LOGO);
    rom
}

#[test]
fn zip_first_known_extension_wins() {
    assert_eq!(ArchiveKind::from_name("GAME.ZIP"), Some(ArchiveKind::Zip));
    assert_eq!(ArchiveKind::from_name("game.gb"), None);

    // A readme sits first in stored order, so only pass 1 can pick the ROM behind it.
    for (name, bytes, system, format) in system_images() {
        let archive = zip_of(&[("readme.txt", b"the notes"), (name, &bytes)]);
        let (entry, detected) = extract(ArchiveKind::Zip, "games.zip", &archive).unwrap();
        assert_eq!(entry.name, name);
        assert_eq!(entry.bytes, bytes, "{name}");
        assert_eq!(detected.system, system, "{name}");
        assert_eq!(detected.format, format, "{name}");
        assert_eq!(detected.basis, Basis::Extension, "{name}");
    }
}

#[test]
fn zip_header_match_without_known_extension() {
    // `.bin` names no system, so pass 2 reads the bytes and finds the iNES header.
    let archive = zip_of(&[("readme.txt", b"the notes"), ("rom.bin", &ines())]);
    let (entry, detected) = extract(ArchiveKind::Zip, "games.zip", &archive).unwrap();
    assert_eq!(entry.name, "rom.bin");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Ines);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn zip_without_rom_refused() {
    let archive = zip_of(&[("readme.txt", b"the notes"), ("notes/", b"")]);
    assert_eq!(
        extract(ArchiveKind::Zip, "games.zip", &archive).unwrap_err(),
        Refusal::no_rom_inside("games.zip")
    );
}

#[test]
fn sevenz_first_known_extension_wins() {
    assert_eq!(
        ArchiveKind::from_name("GAMES.7Z"),
        Some(ArchiveKind::SevenZ)
    );

    for (name, bytes, system, format) in system_images() {
        let archive = sevenz_of(&[("readme.txt", b"the notes"), (name, &bytes)]);
        let (entry, detected) = extract(ArchiveKind::SevenZ, "games.7z", &archive).unwrap();
        assert_eq!(entry.name, name);
        assert_eq!(entry.bytes, bytes, "{name}");
        assert_eq!(detected.system, system, "{name}");
        assert_eq!(detected.format, format, "{name}");
        assert_eq!(detected.basis, Basis::Extension, "{name}");
    }
}

#[test]
fn sevenz_header_match_without_known_extension() {
    let archive = sevenz_of(&[("readme.txt", b"the notes"), ("rom.bin", &ines())]);
    let (entry, detected) = extract(ArchiveKind::SevenZ, "games.7z", &archive).unwrap();
    assert_eq!(entry.name, "rom.bin");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Ines);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn sevenz_without_rom_refused() {
    let archive = sevenz_of(&[("readme.txt", b"the notes"), ("notes/", b"")]);
    assert_eq!(
        extract(ArchiveKind::SevenZ, "games.7z", &archive).unwrap_err(),
        Refusal::no_rom_inside("games.7z")
    );
}

#[test]
fn nested_folder_entry_found() {
    let rom = gb();
    for (kind, archive) in [
        (
            ArchiveKind::Zip,
            zip_of(&[("roms/", b""), ("roms/deep/game.gb", &rom)]),
        ),
        (
            ArchiveKind::SevenZ,
            sevenz_of(&[("roms/", b""), ("roms/deep/game.gb", &rom)]),
        ),
    ] {
        let (entry, detected) = extract(kind, "games.7z", &archive).unwrap();
        assert_eq!(entry.name, "roms/deep/game.gb", "{kind:?}");
        assert_eq!(entry.bytes, rom, "{kind:?}");
        assert_eq!(detected.system, SystemKind::Gb, "{kind:?}");
        assert_eq!(detected.basis, Basis::Signature, "{kind:?}");
    }
}

#[test]
fn corrupt_archive_refused() {
    let garbage = b"neither a zip nor a 7z archive".to_vec();
    for (kind, name) in [
        (ArchiveKind::Zip, "games.zip"),
        (ArchiveKind::SevenZ, "games.7z"),
    ] {
        let refusal = extract(kind, name, &garbage).unwrap_err().to_string();
        let prefix = format!("refused: cannot read {name}: ");
        assert!(
            refusal.starts_with(&prefix) && refusal.len() > prefix.len(),
            "{kind:?}: {refusal}"
        );
    }
    // `open` sends an archive name straight to `extract`.
    assert!(
        open("games.zip", garbage)
            .unwrap_err()
            .to_string()
            .starts_with("refused: cannot read games.zip: ")
    );
}
