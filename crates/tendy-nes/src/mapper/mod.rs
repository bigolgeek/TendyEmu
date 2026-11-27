pub mod action52;
pub mod axrom;
pub mod bnrom;
pub mod camerica;
pub mod cnrom;
pub mod cnrom_security;
pub mod cnrom_unrom;
pub mod color_dreams;
pub mod cprom;
pub mod fds;
pub mod flash;
pub mod gnrom_moved;
pub mod gtrom;
pub mod gxrom;
pub mod j87;
pub mod mmc1;
pub mod mmc2_mmc4;
pub mod mmc3;
pub mod mmc5;
pub mod nina;
pub mod nrom;
pub mod sunsoft_latch;
pub mod unrom_variants;
pub mod unrom512;
pub mod uxrom;
pub mod vrc2_4;
pub mod vrc6;
pub mod vrc7;
pub mod vs;
pub mod fme7;
pub mod n163;

use crate::cart::{CartError, Mapper, RomData, split};
use action52::Action52;
use axrom::Axrom;
use bnrom::Bnrom;
use camerica::Camerica;
use cnrom::Cnrom;
use cnrom_security::CnromSecurity;
use cnrom_unrom::CnromUnrom;
use color_dreams::ColorDreams;
use cprom::Cprom;
use fme7::Fme7;
use gnrom_moved::GnromMoved;
use gtrom::Gtrom;
use gxrom::Gxrom;
use j87::J87;
use mmc1::Mmc1;
use mmc2_mmc4::Mmc2Mmc4;
use mmc3::Mmc3;
use mmc5::Mmc5;
use n163::{Namco163, Namco340};
use nina::Nina;
use nrom::Nrom;
use sunsoft_latch::{Sunsoft1, Sunsoft2};
use unrom_variants::{Un1rom, UnromInverted};
use unrom512::Unrom512;
use uxrom::Uxrom;
use vrc2_4::Vrc2_4;
use vrc6::Vrc6;
use vrc7::Vrc7;
use vs::Vs;

/// Parse an iNES file and build the mapper for it (mappers 0-5, 7, 9-11, 13, 19, 21-26, 30, 34, 38,
/// 66, 69, 71, 78, 79, 85, 87, 93, 94, 99, 111, 140, 152, 180, 184, 185, 210 and 228).
pub fn load(bytes: &[u8]) -> Result<Box<dyn Mapper>, CartError> {
    build(split(bytes)?)
}

/// Build the board for an already-parsed cartridge, whose header the game database has had its say
/// over. Split out of [`load`] so the frontend can parse a ROM once and use both the parsed
/// data and the board it makes.
pub fn build(rom: RomData) -> Result<Box<dyn Mapper>, CartError> {
    // a board with no program ROM has nothing to map, and the 32 KB-window boards count
    // their banks as `prg.len() / 0x8000`, which is zero for a single 16 KB bank; the first read
    // would panic on the divide. Checked here, after `split` and the game database, because a
    // corrected header may name a different board than the file did.
    if rom.prg.is_empty() {
        return Err(CartError::NoPrg);
    }
    if matches!(rom.header.mapper, 7 | 11 | 34 | 38 | 66 | 79 | 140 | 185) && rom.prg.len() < 0x8000
    {
        return Err(CartError::PrgTooSmall);
    }
    match rom.header.mapper {
        0 => Ok(Box::new(Nrom::new(rom))),
        1 => Ok(Box::new(Mmc1::new(rom))),
        2 => Ok(Box::new(Uxrom::new(rom))),
        3 => Ok(Box::new(Cnrom::new(rom))),
        4 => Ok(Box::new(Mmc3::new(rom))),
        5 => Ok(Box::new(Mmc5::new(rom))),
        7 => Ok(Box::new(Axrom::new(rom))),
        9 => Ok(Box::new(Mmc2Mmc4::new(rom, false))),
        10 => Ok(Box::new(Mmc2Mmc4::new(rom, true))),
        11 => Ok(Box::new(ColorDreams::new(rom))),
        21 | 22 | 23 | 25 => Ok(Box::new(Vrc2_4::new(rom))),
        24 | 26 => Ok(Box::new(Vrc6::new(rom))),
        30 => Ok(Box::new(Unrom512::new(rom))),
        19 => Ok(Box::new(Namco163::new(rom))),
        69 => Ok(Box::new(Fme7::new(rom))),
        85 => Ok(Box::new(Vrc7::new(rom))),
        210 => Ok(Box::new(Namco340::new(rom))),
        34 => Ok(Box::new(Bnrom::new(rom))),
        38 => Ok(Box::new(GnromMoved::new(rom, gnrom_moved::Kind::BitCorp))),
        66 => Ok(Box::new(Gxrom::new(rom))),
        71 => Ok(Box::new(Camerica::new(rom))),
        // Mapper 78: Holy Diver (NES 2.0 submapper 3) muxes H/V, Cosmo Carrier (submapper 1, and what
        // a submapper-0 header leaves us to guess, per nesdev) switches one screen.
        78 => {
            let hv = rom.header.submapper == 3;
            Ok(Box::new(CnromUnrom::new(rom, cnrom_unrom::Kind::M78, hv)))
        }
        79 => Ok(Box::new(Nina::new(rom))),
        228 => Ok(Box::new(Action52::new(rom))),
        13 => Ok(Box::new(Cprom::new(rom))),
        87 => Ok(Box::new(J87::new(rom))),
        93 => Ok(Box::new(Sunsoft2::new(rom))),
        94 => Ok(Box::new(Un1rom::new(rom))),
        99 => Ok(Box::new(Vs::new(rom))),
        // nesdev "INES Mapper 111" gives the number to two unrelated boards: GTROM, whose board has
        // CHR-RAM (the PRG image carries no CHR at all), and a Chinese MMC1 variant whose translation
        // has CHR-ROM. CHR-ROM therefore means the MMC1 variant; the two cannot be confused, because
        // GTROM has no CHR-ROM to read.
        111 if rom.chr_is_ram => Ok(Box::new(Gtrom::new(rom))),
        111 => Ok(Box::new(Mmc1::new(rom))),
        140 => Ok(Box::new(GnromMoved::new(rom, gnrom_moved::Kind::JalecoJf))),
        152 => Ok(Box::new(CnromUnrom::new(
            rom,
            cnrom_unrom::Kind::M152,
            false,
        ))),
        180 => Ok(Box::new(UnromInverted::new(rom))),
        184 => Ok(Box::new(Sunsoft1::new(rom))),
        185 => Ok(Box::new(CnromSecurity::new(rom))),
        m => Err(CartError::UnsupportedMapper(m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Mirroring;

    fn rom(prg_banks: u8, chr_banks: u8, flags6: u8, flags7: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, flags6, flags7, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v.resize(
            16 + prg_banks as usize * 16384 + chr_banks as usize * 8192,
            0,
        );
        v
    }

    #[test]
    fn loads_nrom() {
        let mut v = rom(1, 1, 0, 0);
        v[16] = 0xEA;
        v[16 + 16384] = 0x5A;

        let mut m = load(&v).unwrap();

        assert_eq!(m.cpu_read(0x8000), 0xEA);
        assert_eq!(m.cpu_read(0xC000), 0xEA);
        assert_eq!(m.ppu_read(0x0000), 0x5A);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn vertical_mirroring_passes_through() {
        let m = load(&rom(1, 1, 0x01, 0)).unwrap();
        assert_eq!(m.mirroring(), Mirroring::Vertical);
    }

    #[test]
    fn unsupported_mapper() {
        assert!(matches!(
            load(&rom(1, 1, 0x60, 0)),
            Err(CartError::UnsupportedMapper(6))
        ));
    }

    #[test]
    fn loads_mapper_30() {
        // iNES 1.0 mapper 30: flags6 high nibble $E, flags7 high nibble $1. Byte 6 bit 3 is this
        // board's one-screen configuration and bit 0 the mirroring ($08 + $00), and the battery bit
        // ($02) makes it flashable, so the PRG is a chip and the register does not conflict.
        let mut v = rom(16, 0, 0xEA, 0x10);
        v[16] = 0x11; // bank 0, the window at power-on
        v[16 + 0x4000] = 0x22; // bank 1
        v[16 + 15 * 0x4000] = 0x99; // the last bank, which $C000 fixes on

        let mut m = load(&v).unwrap();

        assert_eq!(m.cpu_read(0x8000), 0x11);
        assert_eq!(m.cpu_read(0xC000), 0x99);
        assert_eq!(m.mirroring(), Mirroring::SingleScreenLower);
        m.cpu_write(0xC100, 0x81); // register bit 7 picks the upper half
        assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
        m.cpu_write(0xC100, 0x01); // and the low five bits the PRG bank
        assert_eq!(m.cpu_read(0x8000), 0x22);
    }

    #[test]
    fn loads_vs_mapper_99() {
        // iNES mapper 99 is flags6 >> 4 = 3 with flags7 & $F0 = $60; flags7 bit 0 also marks the
        // image as a Vs. one. Every 8 KiB PRG bank starts with its own number.
        let mut v = rom(2, 1, 0x30, 0x61);
        for bank in 0..4 {
            v[16 + bank * 0x2000] = bank as u8;
        }
        v[16 + 0x8000] = 0x5A; // first CHR byte, right after the four PRG banks
        let mut m = load(&v).unwrap();

        // $8000-$9FFF is the first 8 KiB bank and the fixed 24 KiB above it banks 1 to 3.
        assert_eq!(m.cpu_read(0x8000), 0);
        assert_eq!(m.cpu_read(0xA000), 1);
        assert_eq!(m.cpu_read(0xC000), 2);
        assert_eq!(m.cpu_read(0xE000), 3);
        assert_eq!(m.ppu_read(0x0000), 0x5A);
        // The Vs. System always has 4 KiB of nametable RAM: four screens, whatever the header says.
        assert_eq!(m.mirroring(), Mirroring::FourScreen);
    }

    #[test]
    fn loads_mmc5() {
        let mut m = load(&rom(2, 1, 0x50, 0)).unwrap();
        // PRG mode 3 and $5117 = $FF at power-on: the reset vector's bank is the last one.
        assert_eq!(m.cpu_read(0xE000), 0x00);
        assert_eq!(m.mirroring(), Mirroring::FourScreen);
    }

    /// Mapper 111 with no CHR-ROM is GTROM (a flash-backed 32 KB window), and the same
    /// number with CHR-ROM is the unrelated Chinese MMC1 translation (nesdev "INES Mapper 111").
    #[test]
    fn loads_gtrom_and_the_mmc1_variant_under_mapper_111() {
        let mut v = rom(32, 0, 0xF0, 0x60);
        v[16] = 0xEA;
        v[16 + 0x8000] = 0x5A;
        let mut m = load(&v).unwrap();
        assert_eq!(m.cpu_read(0x8000), 0xEA);
        assert_eq!(m.mirroring(), Mirroring::FourScreen);
        m.cpu_write(0x5000, 0x01);
        assert_eq!(
            m.cpu_read(0x8000),
            0x5A,
            "the register selects the 32 KB flash page"
        );
        m.ppu_write(0x0000, 0x42);
        assert_eq!(
            m.ppu_read(0x0000),
            0x42,
            "GTROM's pattern tables are CHR-RAM"
        );

        let mut v = rom(32, 1, 0xF0, 0x60);
        v[16 + 32 * 16384] = 0x5A;
        let mut m = load(&v).unwrap();
        assert_eq!(
            m.ppu_read(0x0000),
            0x5A,
            "CHR-ROM in the image means the MMC1 variant"
        );
    }

    #[test]
    fn expansion_audio_names_only_the_boards_that_play_expansion_audio() {
        // NROM has no expansion channel, so the APU viewer draws no expansion row for it.
        assert_eq!(load(&rom(1, 1, 0, 0)).unwrap().expansion_audio_name(), None);
        // One name per board that does play one, so the row says which chip it is.
        assert_eq!(
            load(&rom(2, 1, 0x50, 0)).unwrap().expansion_audio_name(),
            Some("MMC5")
        );
        assert_eq!(
            load(&rom(2, 1, 0x30, 0x10)).unwrap().expansion_audio_name(),
            Some("N163")
        );
        assert_eq!(
            load(&rom(2, 1, 0x80, 0x10)).unwrap().expansion_audio_name(),
            Some("VRC6")
        );
        assert_eq!(
            load(&rom(2, 1, 0x50, 0x50)).unwrap().expansion_audio_name(),
            Some("VRC7")
        );
        assert_eq!(
            load(&rom(2, 1, 0x50, 0x40)).unwrap().expansion_audio_name(),
            Some("FME-7")
        );
    }

    #[test]
    fn loads_six_mapper_ids() {
        // iNES headers these six ids build for, power-on state: PRG bank 0 at $8000
        // and CHR bank 0, which is what every one of these latches holds after a reset.
        for (mapper, flags6, flags7) in [
            (38u16, 0x60u8, 0x20u8),
            (78, 0xE0, 0x40),
            (79, 0xF0, 0x40),
            (140, 0xC0, 0x80),
            (152, 0x80, 0x90),
            (185, 0x90, 0xB0),
        ] {
            let mut v = rom(2, 1, flags6, flags7);
            v[16] = 0xEA;
            v[16 + 2 * 16384] = 0x5A;

            let mut m = load(&v).unwrap_or_else(|e| panic!("mapper {mapper}: {e:?}"));

            assert_eq!(m.cpu_read(0x8000), 0xEA, "mapper {mapper} PRG bank 0");
            assert_eq!(m.ppu_read(0x0000), 0x5A, "mapper {mapper} CHR bank 0");
        }
    }

    #[test]
    fn bad_file_is_error() {
        assert!(matches!(load(&[0u8; 8]), Err(CartError::TooShort)));
    }

    #[test]
    fn nes2_chr_ram_below_8k_mirrors_through_ppu_reads() {
        // Byte 11 shift 5 -> 2 KiB of CHR-RAM. Addresses above the real size must wrap there
        // ($0800 and $1000 mirror $0000) instead of reading uninitialised memory.
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x05, 0x00, 0, 0, 0,
        ];
        v.resize(16 + 16384, 0);

        let mut m = load(&v).unwrap();
        m.ppu_write(0x0000, 0x5A);
        assert_eq!(m.ppu_read(0x0000), 0x5A);
        assert_eq!(m.ppu_read(0x0800), 0x5A);
        assert_eq!(m.ppu_read(0x1000), 0x5A);
        m.ppu_write(0x0801, 0xA5);
        assert_eq!(m.ppu_read(0x0001), 0xA5);
    }

    #[test]
    fn power_on_random_default_hook_is_a_noop() {
        // Mappers that don't override the hook must stay byte-identical to power-on.
        for (flags6, flags7) in [
            (0x00u8, 0x00u8), // 0 Nrom
            (0x20u8, 0x00u8), // 2 Uxrom
            (0x30u8, 0x00u8), // 3 Cnrom
            (0x70u8, 0x00u8), // 7 Axrom
            (0x90u8, 0x00u8), // 9 Mmc2
            (0xA0u8, 0x00u8), // 10 Mmc4
            (0xB0u8, 0x00u8), // 11 ColorDreams
            (0x20u8, 0x20u8), // 34 Bnrom: mapper = (flags7 & 0xF0) | (flags6 >> 4)
            (0x20u8, 0x40u8), // 66 Gxrom
            (0x70u8, 0x40u8), // 71 Camerica
        ] {
            for seed in [0u64, u64::MAX, 0xDEAD_BEEF_CAFE_F00D] {
                let mut m = match load(&rom(2, 1, flags6, flags7)) {
                    Ok(m) => m,
                    Err(e) => {
                        println!("NOOP-FAIL ({:02X},{:02X}): {:?}", flags6, flags7, e);
                        panic!("instrumented load failure");
                    }
                };
                let want = (m.mirroring(), m.cpu_read(0x8000), m.ppu_read(0x1FFF));
                m.power_on_random(seed);
                assert_eq!(
                    (m.mirroring(), m.cpu_read(0x8000), m.ppu_read(0x1FFF)),
                    want,
                    "default hook changes nothing"
                );
            }
        }
    }

    #[test]
    fn power_on_random_never_panics_via_dyn() {
        // The two overriding boards, through the Box<dyn Mapper>: garbage registers must stay
        // garbage-but-valid on every read address.
        for (flags6, flags7) in [(0x10u8, 0x00u8), (0x40u8, 0x00u8)] {
            for seed in [0u64, u64::MAX, 0xDEAD_BEEF_CAFE_F00D, 12345] {
                let mut m = load(&rom(2, 1, flags6, flags7)).unwrap();
                m.power_on_random(seed);
                for addr in [0x6000u16, 0x7FFF, 0x8000, 0xA000, 0xBFFF, 0xC000, 0xFFFF] {
                    m.cpu_read(addr);
                }
                for addr in [0x0000u16, 0x0FFF, 0x1000, 0x1FFF] {
                    m.ppu_read(addr);
                }
                m.mirroring();
            }
        }
    }

    /// A NES 2.0 image with no CHR-ROM and a CHR-RAM size of 0 (byte 11 shift counts are 0): the
    /// board has no CHR memory, so pattern fetches read 0 and writes are dropped instead of
    /// panicking.
    #[test]
    fn nes2_board_without_chr_memory_does_not_panic() {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0, 0, 0,
        ];
        v.resize(16 + 16384, 0);

        let mut m = load(&v).unwrap();

        assert_eq!(m.ppu_read(0x0000), 0);
        assert_eq!(m.ppu_read(0x1FFF), 0);
        m.ppu_write(0x0000, 0x5A);
        assert_eq!(m.ppu_read(0x0000), 0);
    }

    #[test]
    fn power_on_ram_default_hook_is_a_noop() {
        use crate::bus::RamPowerOn;
        // Boards that don't override the hook must stay byte-identical to power-on.
        for (flags6, flags7) in [
            (0x30u8, 0x00u8), // 3 Cnrom
            (0x70u8, 0x00u8), // 7 Axrom
            (0x90u8, 0x00u8), // 9 Mmc2
            (0xA0u8, 0x00u8), // 10 Mmc4
            (0xB0u8, 0x00u8), // 11 ColorDreams
            (0x20u8, 0x20u8), // 34 Bnrom
            (0x20u8, 0x40u8), // 66 Gxrom
            (0x70u8, 0x40u8), // 71 Camerica
        ] {
            let mut m = load(&rom(2, 1, flags6, flags7)).unwrap();
            let want = (
                m.prg_ram().to_vec(),
                m.chr_ram().to_vec(),
                m.cpu_read(0x8000),
                m.ppu_read(0x1FFF),
            );
            m.power_on_ram(RamPowerOn::Random, 7);
            assert_eq!(
                (
                    m.prg_ram().to_vec(),
                    m.chr_ram().to_vec(),
                    m.cpu_read(0x8000),
                    m.ppu_read(0x1FFF)
                ),
                want
            );
        }
    }

    #[test]
    fn power_on_ram_never_panics_on_any_mapper() {
        use crate::bus::RamPowerOn;
        // Every mapper number the loader knows, with CHR-ROM and with CHR-RAM, every mode.
        let mut built = 0;
        for mapper in 0u16..=255 {
            for chr_banks in [0u8, 1] {
                let flags6 = ((mapper & 0x0F) as u8) << 4;
                let flags7 = (mapper & 0xF0) as u8;
                let Ok(mut m) = load(&rom(2, chr_banks, flags6, flags7)) else {
                    continue;
                };
                built += 1;
                for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
                    for seed in [0u64, u64::MAX, 0xDEAD_BEEF_CAFE_F00D] {
                        m.power_on_ram(mode, seed);
                        for addr in [0x6000u16, 0x7FFF, 0x8000, 0xFFFF] {
                            m.cpu_read(addr);
                        }
                        for addr in [0x0000u16, 0x1FFF] {
                            m.ppu_read(addr);
                        }
                    }
                }
            }
        }
        assert!(built > 10, "the sweep reached {built} boards");
    }
}
