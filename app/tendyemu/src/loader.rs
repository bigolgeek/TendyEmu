//! From a detected file to a running machine (DESIGN §2.10, §3.1).
//!
//! `tendy_detect` says which system a file is; this maps that answer onto each core's constructor.
//! Every refusal keeps the core's own words, prefixed so the whole app has one refusal line.

use tendy_core::{Machine, SystemKind};
use tendy_detect::{Detected, Entry, Format};

/// Builds the core `detected` names from the bytes `entry` carries.
pub fn build(entry: &Entry, detected: &Detected) -> Result<Box<dyn Machine>, String> {
    let rom = entry.bytes.clone();
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
}
