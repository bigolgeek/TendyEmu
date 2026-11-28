//! From a detected file to a running machine (DESIGN §2.10, §3.1).
//!
//! `tendy_detect` says which system a file is; this maps that answer onto each core's constructor.
//! Every refusal keeps the core's own words, prefixed so the whole app has one refusal line.

use std::path::Path;

use tendy_core::{Machine, SystemKind};
use tendy_detect::{Detected, Entry, Format};

/// Builds the core `detected` names from the bytes `entry` carries.
pub fn build(entry: &Entry, detected: &Detected) -> Result<Box<dyn Machine>, String> {
    build_with_rom(entry, detected, None)
}

/// `build`, after applying the soft patch that sits next to `rom` (SPEC §3, #30): the
/// `<stem>.ips`, `.bps` or `.ups` beside the file the user opened is applied before anything
/// parses the bytes, so the header and the game database see the patched image. An archive
/// entry, or a test with no file, has no patch beside it and is built as it came.
///
/// A patch that exists but cannot be applied refuses the open with its own reason: running the
/// unpatched ROM would silently hide a wrong revision or a corrupted download.
pub fn build_with_rom(
    entry: &Entry,
    detected: &Detected,
    rom: Option<&Path>,
) -> Result<Box<dyn Machine>, String> {
    let rom = patched_bytes(entry, rom)?;
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
            Box::new(
                tendy_nes::NesMachine::new(
                    rom,
                    image,
                    None,
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
        assert!(build_with_rom(&entry, &detected, Some(&path)).is_ok());
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
        let why = match build_with_rom(&entry, &detected, Some(&path)) {
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
        let why = match build_with_rom(&entry, &detected, Some(&path)) {
            Ok(_) => panic!("a corrupted patch must refuse the open"),
            Err(why) => why,
        };
        assert!(why.starts_with("refused: cannot apply "), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
