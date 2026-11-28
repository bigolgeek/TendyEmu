//! From a detected file to a running machine (DESIGN §2.10, §3.1).
//!
//! `tendy_detect` says which system a file is; this maps that answer onto each core's constructor.
//! Every refusal keeps the core's own words, prefixed so the whole app has one refusal line.

use std::path::{Path, PathBuf};

use tendy_core::{Machine, SystemKind};
use tendy_detect::{Detected, Entry, Format};

/// Where the bytes came from: the file the user opened, and the FDS BIOS the settings name.
/// A test, or an entry no file stands behind, passes neither.
///
/// `rom` is the file the entry was read from (an archive included), so the soft patch beside it
/// (SPEC §3, #30) and the FDS BIOS search both have a folder to look in. `fds_bios` is the path
/// the `[nes] fds_bios` setting holds, empty or `None` when the user has not chosen one.
#[derive(Clone, Copy, Default)]
pub struct Sources<'a> {
    pub rom: Option<&'a Path>,
    pub fds_bios: Option<&'a Path>,
}

/// The one line every FDS BIOS refusal ends with: the setting to fix and where it lives.
const FDS_BIOS_SETTING: &str = "set \"FDS BIOS\" in Main menu > Emulation";

/// Builds the core `detected` names from the bytes `entry` carries.
pub fn build(entry: &Entry, detected: &Detected) -> Result<Box<dyn Machine>, String> {
    build_with(entry, detected, Sources::default())
}

/// `build`, after applying the soft patch that sits next to `sources.rom` (SPEC §3, #30): the
/// `<stem>.ips`, `.bps` or `.ups` beside the file the user opened is applied before anything
/// parses the bytes, so the header and the game database see the patched image. An archive
/// entry, or a test with no file, has no patch beside it and is built as it came.
///
/// A patch that exists but cannot be applied refuses the open with its own reason: running the
/// unpatched ROM would silently hide a wrong revision or a corrupted download.
pub fn build_with(
    entry: &Entry,
    detected: &Detected,
    sources: Sources<'_>,
) -> Result<Box<dyn Machine>, String> {
    let rom = patched_bytes(entry, sources.rom)?;
    let machine: Box<dyn Machine> = match detected.system {
        SystemKind::Gb => Box::new(
            tendy_gb::GbMachine::new(
                rom,
                tendy_gb::GbOptions {
                    model: tendy_gb::GbModel::Auto,
                    boot_rom: None,
                    sgb_sound_rom: None,
                },
            )
            .map_err(refused)?,
        ),
        SystemKind::Gba => Box::new(tendy_gba::GbaMachine::new(rom, None).map_err(refused)?),
        SystemKind::Nes => {
            let image = match detected.format {
                Format::Qd => tendy_nes::NesImage::Disk { qd: true },
                Format::FdsHeadered | Format::FdsRaw => tendy_nes::NesImage::Disk { qd: false },
                Format::Nsf => tendy_nes::NesImage::Nsf { song: 0 },
                Format::Ines | Format::Nes20 | Format::Unif => tendy_nes::NesImage::Cartridge,
                // Detection never pairs another system's layout with the NES; a stray one is a
                // cartridge and the core refuses it if it is not.
                _ => tendy_nes::NesImage::Cartridge,
            };
            // A disk boots in the FDS BIOS, so it needs one before the core is built; a
            // cartridge or an NSF carries everything it needs.
            let bios = match image {
                tendy_nes::NesImage::Disk { .. } => Some(fds_bios(sources.fds_bios, sources.rom)?),
                _ => None,
            };
            Box::new(
                tendy_nes::NesMachine::new(
                    rom,
                    image,
                    bios,
                    tendy_nes::console::ConsoleConfig::default(),
                )
                .map_err(refused)?,
            )
        }
        SystemKind::Snes => Box::new(tendy_snes::SnesMachine::new(rom).map_err(refused)?),
    };
    Ok(machine)
}

/// The bytes to build from: `entry`'s own, with the patch beside `rom` applied when `rom` is
/// the file the entry came from (not an archive the entry was found in).
fn patched_bytes(entry: &Entry, rom: Option<&Path>) -> Result<Vec<u8>, String> {
    let direct = rom.filter(|path| {
        path.file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(&entry.name))
    });
    let Some(patch) = direct.and_then(tendy_detect::patch::beside) else {
        return Ok(entry.bytes.clone());
    };
    tendy_detect::patch::apply_path(&entry.bytes, &patch)
        .map_err(|why| refused(format!("cannot apply {}: {why}", patch.display())))
}

/// `refused: ...` once: a core's text may or may not already carry the prefix.
fn refused(why: String) -> String {
    if why.starts_with("refused: ") {
        why
    } else {
        format!("refused: {why}")
    }
}

/// The FDS BIOS bytes for a disk image: the file the `[nes] fds_bios` setting names, else the
/// `disksys.rom` dump the old NES app looked for beside the image and in a `roms/` folder there.
/// A disk the user supplied no BIOS for refuses in one line that names the setting: without one
/// the disk cannot boot at all, so running it half-loaded would only hide the real problem.
fn fds_bios(setting: Option<&Path>, rom: Option<&Path>) -> Result<Vec<u8>, String> {
    let named = setting.filter(|path| !path.as_os_str().is_empty());
    let candidates: Vec<PathBuf> = match named {
        Some(path) => vec![path.to_path_buf()],
        None => rom.map(search_beside).unwrap_or_default(),
    };
    let needed = tendy_nes::mapper::fds::BIOS_LEN;
    for path in &candidates {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() >= needed => return Ok(bytes),
            Ok(bytes) => {
                return Err(refused(format!(
                    "the FDS BIOS {} is {} bytes, not {needed} — {FDS_BIOS_SETTING}",
                    path.display(),
                    bytes.len()
                )));
            }
            // A search candidate that is not there is the next candidate's business; a file the
            // setting names is the user's answer, so a missing one is a refusal.
            Err(why) if named.is_none() && why.kind() == std::io::ErrorKind::NotFound => {}
            Err(why) => {
                return Err(refused(format!(
                    "cannot read the FDS BIOS {}: {why} — {FDS_BIOS_SETTING}",
                    path.display()
                )));
            }
        }
    }
    Err(refused(format!(
        "an FDS disk needs the FDS BIOS — {FDS_BIOS_SETTING}"
    )))
}

/// Where the old NES app looked beside the image: its own folder, then a `roms/` folder there.
fn search_beside(rom: &Path) -> Vec<PathBuf> {
    let Some(dir) = rom.parent() else {
        return Vec::new();
    };
    vec![
        dir.join("disksys.rom"),
        dir.join("roms").join("disksys.rom"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest cartridge the Game Boy core accepts: 32 KiB, ROM only, a valid header checksum.
    fn gb_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        rom[0x134..0x13C].copy_from_slice(b"SHELLGB ");
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

    /// One IPS record at `offset`, ready to go between the magic and the EOF marker.
    fn ips_record(offset: u32, data: &[u8]) -> Vec<u8> {
        let mut record = vec![
            (offset >> 16) as u8,
            (offset >> 8) as u8,
            offset as u8,
            (data.len() >> 8) as u8,
            data.len() as u8,
        ];
        record.extend_from_slice(data);
        record
    }

    fn ips(records: &[Vec<u8>]) -> Vec<u8> {
        let mut patch = b"PATCH".to_vec();
        for record in records {
            patch.extend_from_slice(record);
        }
        patch.extend_from_slice(b"EOF");
        patch
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tendyemu-loader-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch folder is writable");
        dir
    }

    /// An 8 KB FDS BIOS: `LDA #$00` at $E000 and a reset vector to it, so the disk boots.
    fn fds_bios() -> Vec<u8> {
        let mut bios = vec![0u8; tendy_nes::mapper::fds::BIOS_LEN];
        bios[0] = 0xA9;
        bios[0x1FFC] = 0x00;
        bios[0x1FFD] = 0xE0;
        bios
    }

    /// One side of a disk image with a plausible block structure: disk info, file count, one file
    /// header and four bytes of file data.
    fn fds_image() -> Vec<u8> {
        let mut side = vec![0u8; tendy_nes::mapper::fds::SIDE_LEN];
        side[0] = 0x01;
        side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
        side[0x38] = 0x02;
        side[0x39] = 1;
        side[0x3A] = 0x03;
        side[0x3A + 0x0B] = 0x00;
        side[0x3A + 0x0C] = 0x60;
        side[0x3A + 0x0D] = 0x04;
        side[0x4A] = 0x04;
        side[0x4B..0x4F].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mut disk = Vec::from(tendy_nes::mapper::fds::FDS_MAGIC);
        disk.push(1);
        disk.extend_from_slice(&[0u8; 11]);
        disk.extend_from_slice(&side);
        disk
    }

    /// Builds the image as a disk behind `sources`, or the refusal text.
    fn build_disk(sources: Sources<'_>) -> Result<Box<dyn Machine>, String> {
        let (entry, detected) =
            tendy_detect::open("game.fds", fds_image()).expect("a .fds image detects");
        build_with(&entry, &detected, sources)
    }

    #[test]
    fn a_disk_boots_on_its_first_side_when_the_settings_name_a_bios() {
        let dir = scratch("fds-bios");
        let bios = dir.join("disksys.rom");
        std::fs::write(&bios, fds_bios()).expect("the BIOS is written");
        let machine = build_disk(Sources {
            fds_bios: Some(&bios),
            ..Sources::default()
        })
        .expect("the disk opens");
        assert_eq!(machine.kind(), SystemKind::Nes);
        assert_eq!(machine.cart_storage(), "FDS disk");
        assert_eq!(machine.disk_side(), Some((0, 1)), "side A is mounted");
        assert!(
            machine.capabilities().has(tendy_core::Capability::DiskSwap),
            "a disk reports disk swap"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disk_without_a_bios_refuses_in_one_line_naming_the_setting() {
        let why = match build_disk(Sources::default()) {
            Ok(_) => panic!("a disk with no BIOS must be refused"),
            Err(why) => why,
        };
        assert!(why.starts_with("refused: "), "{why}");
        assert!(why.contains("FDS BIOS"), "{why}");
        assert!(why.contains("Emulation"), "the line says where: {why}");
        assert!(!why.contains('\n'), "one line: {why}");
    }

    #[test]
    fn a_short_bios_is_refused_rather_than_mounted() {
        let dir = scratch("fds-short");
        let bios = dir.join("short.bin");
        std::fs::write(&bios, [0u8; 100]).expect("the short BIOS is written");
        let why = match build_disk(Sources {
            fds_bios: Some(&bios),
            ..Sources::default()
        }) {
            Ok(_) => panic!("a short BIOS must be refused"),
            Err(why) => why,
        };
        assert!(why.contains("is 100 bytes"), "{why}");
        assert!(why.contains("FDS BIOS"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disksys_rom_beside_the_image_is_found_when_the_setting_is_empty() {
        let dir = scratch("fds-beside");
        let disk = dir.join("game.fds");
        std::fs::write(&disk, fds_image()).expect("the disk is written");
        // An empty setting is "not chosen", not a path.
        let empty = Path::new("");
        assert!(
            build_disk(Sources {
                rom: Some(&disk),
                fds_bios: Some(empty),
            })
            .is_err(),
            "an empty setting falls back to the search, and finds nothing yet"
        );
        std::fs::write(dir.join("disksys.rom"), fds_bios()).expect("the BIOS is written");
        let machine = build_disk(Sources {
            rom: Some(&disk),
            fds_bios: Some(empty),
        })
        .expect("the BIOS beside the image is found");
        assert_eq!(machine.disk_side(), Some((0, 1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rom_image_becomes_a_machine() {
        let bytes = gb_rom();
        let (entry, detected) = tendy_detect::open("test.gb", bytes).expect("a GB cart detects");
        let mut machine = build(&entry, &detected).expect("the core accepts it");
        assert_eq!(machine.kind(), SystemKind::Gb);
        assert_eq!(machine.frame().width, 160);
    }

    #[test]
    fn a_core_refusal_reads_as_one_line() {
        let (entry, detected) =
            tendy_detect::open("junk.gba", vec![0u8; 0x200]).expect("the extension decides");
        let why = match build(&entry, &detected) {
            Ok(_) => panic!("a blank GBA image must be refused"),
            Err(why) => why,
        };
        assert!(why.starts_with("refused: "), "{why}");
    }

    #[test]
    fn a_patch_beside_the_rom_reaches_the_core() {
        let dir = scratch("beside");
        let path = dir.join("hack.gb");
        std::fs::write(&path, gb_rom()).expect("the ROM is written");
        let (entry, detected) = tendy_detect::open("hack.gb", gb_rom()).expect("a GB cart detects");
        // The clean image builds; the patched one must not. Two records: `$0101` breaks the
        // Pro Action Replay boot signature this helper's entry bytes happen to match (that
        // unlicensed board is picked ahead of the type byte), and `$0147` sets a type no mapper
        // answers. Only an applied patch can make the core refuse, so the refusal proves the
        // core saw the patched bytes.
        assert!(
            build_with(
                &entry,
                &detected,
                Sources {
                    rom: Some(&path),
                    ..Sources::default()
                },
            )
            .is_ok()
        );
        std::fs::write(
            path.with_extension("ips"),
            ips(&[ips_record(0x0101, &[0x00]), ips_record(0x0147, &[0x04])]),
        )
        .expect("the patch is written");
        let patched = patched_bytes(&entry, Some(&path)).expect("the patch applies");
        assert_eq!(
            patched[0x0147], 0x04,
            "the patched byte reached the build input"
        );
        let why = match build_with(
            &entry,
            &detected,
            Sources {
                rom: Some(&path),
                ..Sources::default()
            },
        ) {
            Ok(_) => panic!("the patched ROM must be refused"),
            Err(why) => why,
        };
        assert!(why.starts_with("refused: "), "{why}");
        assert!(why.contains("unsupported cartridge type $04"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_patch_refuses_the_open_instead_of_running_the_clean_rom() {
        let dir = scratch("bad");
        let path = dir.join("hack.gb");
        std::fs::write(&path, gb_rom()).expect("the ROM is written");
        // A corrupt BPS file: its own CRC no longer matches.
        std::fs::write(path.with_extension("bps"), b"BPS1garbage").expect("the patch is written");
        let (entry, detected) = tendy_detect::open("hack.gb", gb_rom()).expect("a GB cart detects");
        let why = match build_with(
            &entry,
            &detected,
            Sources {
                rom: Some(&path),
                ..Sources::default()
            },
        ) {
            Ok(_) => panic!("a corrupted patch must refuse the open"),
            Err(why) => why,
        };
        assert!(why.starts_with("refused: cannot apply "), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
