//! The tendy-detect signature table tests (DESIGN §3.2, #12).
//!
//! Every row builds its minimal image in code, so the suite needs no ROM files. The one exception
//! is `snes_threshold_survey`, which is `#[ignore]`d and reads the two ROM folders named by
//! `TENDY_TEST_ROMS` and `TENDY_NES_TEST_ROMS` to settle §3.2's UNVERIFIED score threshold.

use std::io::Read;
use std::path::Path;

use tendy_core::SystemKind;
use tendy_detect::signature::{SNES_SIGNATURE_MIN_SCORE, identify};
use tendy_detect::{Basis, Format, GbFlags, Refusal, detect};
use tendy_snes::cart::Board;
use tendy_snes::cart::header::{CANDIDATES, score};

/// Row 1: an iNES header whose byte 7 is `flag7`, with the rest zero.
fn ines(flag7: u8) -> Vec<u8> {
    let mut rom = vec![0x00; 16];
    rom[0..4].copy_from_slice(b"NES\x1A");
    rom[7] = flag7;
    rom
}

/// Row 4: a raw FDS/QD disk, from its block 1.
fn raw_disk() -> Vec<u8> {
    let mut disk = vec![0x00; 32];
    disk[0] = 0x01;
    disk[1..15].copy_from_slice(b"*NINTENDO-HVC*");
    disk
}

/// Row 6: a GBA header: an ARM branch, `$96` at `$B2` and the complement at `$BD`.
fn gba() -> Vec<u8> {
    let mut rom = vec![0x00; 0xC0];
    rom[0..4].copy_from_slice(&0xEA00_0000_u32.to_le_bytes()); // b
    rom[0xB2] = 0x96;
    rom[0xBD] = gba_complement(&rom);
    rom
}

/// GBATEK's complement check over `$0A0-$0BC` (`$0BD` included in neither side).
fn gba_complement(rom: &[u8]) -> u8 {
    let mut check: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        check = check.wrapping_sub(byte);
    }
    check.wrapping_sub(0x19)
}

/// Row 7: a GB cartridge, zero apart from the logo.
fn gb() -> Vec<u8> {
    let mut rom = vec![0x00; 0x150];
    rom[0x104..0x134].copy_from_slice(&GB_LOGO);
    rom
}

/// Row 8: a SNES image with `board`'s header at `at`: map nibble, speed bit and a matching
/// checksum pair. The title field stays zero, which the core's score reads as printable.
fn snes_rom(len: usize, at: usize, map_mode: u8) -> Vec<u8> {
    let mut rom = vec![0x00; len];
    rom[at + 0x15] = map_mode | 0x20;
    let sum = rom_sum(&rom, at);
    rom[at + 0x1E..at + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[at + 0x1C..at + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom
}

/// The sum the core's `score` computes: the four checksum bytes count as `FF FF 00 00`.
fn rom_sum(rom: &[u8], at: usize) -> u16 {
    let mut sum: u32 = 0;
    for (index, byte) in rom.iter().enumerate() {
        sum += match index.wrapping_sub(at) {
            0x1C | 0x1D => 0xFF,
            0x1E | 0x1F => 0x00,
            _ => u32::from(*byte),
        };
    }
    sum as u16
}

/// The 48-byte Nintendo logo at `$0104-$0133` (Pan Docs, "The Cartridge Header").
const GB_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

#[test]
fn ines_header_names_the_nes_core() {
    let detected = detect("game.nes", &ines(0)).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Ines);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn nes20_bit_three_alone_marks_nes20() {
    // DESIGN §3.6 C4: bit 3 set and bit 2 clear is NES 2.0; both bits clear or both set is iNES.
    for (flag7, format) in [
        (0x00, Format::Ines),
        (0x04, Format::Ines),
        (0x08, Format::Nes20),
        (0x0C, Format::Ines),
    ] {
        assert_eq!(
            identify(&ines(flag7)).map(|matched| matched.format),
            Some(format),
            "byte 7 = {flag7:#04X}"
        );
    }
}

#[test]
fn unif_header_names_the_nes_core() {
    let mut rom = vec![0x00; 32];
    rom[0..4].copy_from_slice(b"UNIF");
    let detected = detect("game.unf", &rom).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Unif);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn fds_header_names_the_nes_core() {
    let mut rom = vec![0x00; 16];
    rom[0..4].copy_from_slice(b"FDS\x1A");
    rom[4] = 1; // one side
    let detected = detect("game.fds", &rom).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::FdsHeadered);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn a_raw_disk_names_the_nes_core_and_a_qd_name_turns_it_into_qd() {
    let detected = detect("disk.fds", &raw_disk()).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::FdsRaw);
    assert_eq!(detected.basis, Basis::Signature);

    // DESIGN §3.3: the same bytes named `.qd` are a QD image.
    let detected = detect("disk.qd", &raw_disk()).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Qd);
    assert_eq!(detected.basis, Basis::Signature);
    assert_eq!(detected.notice, None);
}

#[test]
fn nsf_header_names_the_nes_core() {
    let mut rom = vec![0x00; 0x80];
    rom[0..5].copy_from_slice(b"NESM\x1A");
    let detected = detect("music.nsf", &rom).unwrap();
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Nsf);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn gba_header_names_the_gba_core() {
    let detected = detect("game.gba", &gba()).unwrap();
    assert_eq!(detected.system, SystemKind::Gba);
    assert_eq!(detected.format, Format::Gba);
    assert_eq!(detected.basis, Basis::Signature);
}

#[test]
fn gba_each_check_fails_on_its_own() {
    let good = gba();

    // 1. Too short for the header.
    let mut short = good.clone();
    short.truncate(0xBF);
    assert_eq!(identify(&short), None, "a GBA header needs $C0 bytes");

    // 2. The word at 0 is not an ARM branch (`bx lr`).
    let mut branch = good.clone();
    branch[0..4].copy_from_slice(&0xE12F_FF1E_u32.to_le_bytes());
    assert_eq!(
        identify(&branch),
        None,
        "the entry point is not an ARM branch"
    );

    // 3. `$B2` is not `$96`; recompute the complement so only that check fails.
    let mut fixed = good.clone();
    fixed[0xB2] = 0x95;
    fixed[0xBD] = gba_complement(&fixed);
    assert_eq!(identify(&fixed), None, "$B2 is not $96");

    // 4. `$BD` is not the complement.
    let mut complement = good.clone();
    complement[0xBD] = complement[0xBD].wrapping_add(1);
    assert_eq!(identify(&complement), None, "the complement check fails");
}

#[test]
fn gb_logo_names_the_game_boy_core() {
    let detected = detect("game.gb", &gb()).unwrap();
    assert_eq!(detected.system, SystemKind::Gb);
    assert_eq!(detected.format, Format::Gb);
    assert_eq!(detected.basis, Basis::Signature);
    assert_eq!(
        detected.gb,
        Some(GbFlags {
            cgb: false,
            sgb: false
        })
    );
}

#[test]
fn gb_flags_report_cgb_and_sgb() {
    let mut cgb = gb();
    cgb[0x143] = 0x80;
    assert_eq!(
        detect("game.gbc", &cgb).unwrap().gb,
        Some(GbFlags {
            cgb: true,
            sgb: false
        })
    );

    let mut sgb = gb();
    sgb[0x146] = 0x03;
    sgb[0x14B] = 0x33;
    assert_eq!(
        detect("game.sgb", &sgb).unwrap().gb,
        Some(GbFlags {
            cgb: false,
            sgb: true
        })
    );

    // Pan Docs: SGB functions need both bytes, so one alone is not an SGB cartridge.
    let mut half = gb();
    half[0x146] = 0x03;
    assert_eq!(
        detect("game.gb", &half).unwrap().gb,
        Some(GbFlags {
            cgb: false,
            sgb: false
        })
    );
}

#[test]
fn snes_lorom_names_the_super_nes_core() {
    let rom = snes_rom(0x8000, 0x7FC0, 0);
    assert!(score(&rom, 0x7FC0, Board::LoRom) >= SNES_SIGNATURE_MIN_SCORE);
    let detected = detect("game.sfc", &rom).unwrap();
    assert_eq!(detected.system, SystemKind::Snes);
    assert_eq!(detected.format, Format::Snes);
    assert_eq!(detected.basis, Basis::Signature);
    assert!(!detected.copier_header);
}

#[test]
fn snes_hirom_names_the_super_nes_core() {
    let rom = snes_rom(0x1_0000, 0xFFC0, 1);
    assert!(score(&rom, 0xFFC0, Board::HiRom) >= SNES_SIGNATURE_MIN_SCORE);
    // The LoROM candidate sees filler and stays below the threshold.
    assert!(score(&rom, 0x7FC0, Board::LoRom) < SNES_SIGNATURE_MIN_SCORE);
    assert_eq!(
        identify(&rom).map(|matched| matched.system),
        Some(SystemKind::Snes)
    );
}

#[test]
fn snes_exhirom_names_the_super_nes_core() {
    // The ExHiROM header sits at $40FFC0, so the image must reach past it.
    let rom = snes_rom(0x41_0000, 0x40FFC0, 5);
    assert!(score(&rom, 0x40FFC0, Board::ExHiRom) >= SNES_SIGNATURE_MIN_SCORE);
    assert_eq!(
        identify(&rom).map(|matched| matched.system),
        Some(SystemKind::Snes)
    );
}

#[test]
fn snes_copier_header_is_detected_and_skipped() {
    let mut rom = vec![0xFF; 512];
    rom.extend_from_slice(&snes_rom(0x8000, 0x7FC0, 0));
    assert_eq!(rom.len() % 1024, 512);
    // Unstripped, the header is not where any candidate looks.
    assert!(score(&rom, 0x7FC0, Board::LoRom) < SNES_SIGNATURE_MIN_SCORE);

    let detected = detect("game.smc", &rom).unwrap();
    assert_eq!(detected.system, SystemKind::Snes);
    assert!(detected.copier_header);
}

#[test]
fn a_tie_between_candidates_keeps_the_earlier_one() {
    // A 64 KiB image with a valid header at both $7FC0 and $FFC0: both score 11.
    let mut rom = vec![0x00; 0x1_0000];
    rom[0x7FD5] = 0x20; // LoROM map nibble plus the speed bit
    rom[0xFFD5] = 0x21; // HiROM map nibble plus the speed bit
    // Each written checksum pair sums to $1FE whatever its value, so one sum serves both headers.
    let sum = (rom.iter().map(|&byte| u32::from(byte)).sum::<u32>() + 0x3FC) as u16;
    for at in [0x7FC0, 0xFFC0] {
        rom[at + 0x1E..at + 0x20].copy_from_slice(&sum.to_le_bytes());
        rom[at + 0x1C..at + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    }

    let lorom = score(&rom, 0x7FC0, Board::LoRom);
    let hirom = score(&rom, 0xFFC0, Board::HiRom);
    assert_eq!(lorom, hirom);
    assert!(lorom >= SNES_SIGNATURE_MIN_SCORE);

    // Detection sees one Super NES signature; the core's own parse resolves the tie to LoROM.
    assert_eq!(
        identify(&rom).map(|matched| matched.system),
        Some(SystemKind::Snes)
    );
    let (_, board) = tendy_snes::cart::Header::parse(&rom).expect("the tie image parses");
    assert_eq!(board, Board::LoRom);
}

#[test]
fn short_files_are_refused() {
    for len in [0, 1, 4, 15, 0x7F, 0x7FDF] {
        let bytes = vec![0x00; len];
        assert!(
            identify(&bytes).is_none(),
            "{len} bytes are not a signature"
        );
        assert_eq!(
            detect("file.bin", &bytes).unwrap_err(),
            Refusal::not_a_rom("file.bin")
        );
    }
}

#[test]
fn an_all_same_byte_image_is_refused() {
    // A zero-filled 32 KiB file satisfies the LoROM map nibble and little else (§3.2 row 8).
    for byte in [0x00, 0xFF] {
        let rom = vec![byte; 0x8000];
        assert!(
            identify(&rom).is_none(),
            "a {byte:#04X}-filled image is not a signature"
        );
    }
}

#[test]
fn a_random_megabyte_is_refused() {
    let mut bytes = vec![0x00; 1 << 20];
    xorshift(&mut bytes, 0x9E37_79B9_7F4A_7C15);
    assert!(
        identify(&bytes).is_none(),
        "random bytes are not a signature"
    );
    assert_eq!(
        detect("random.bin", &bytes).unwrap_err(),
        Refusal::not_a_rom("random.bin")
    );
}

/// A simple xorshift64, so the random cases are fixed and repeatable.
fn xorshift(bytes: &mut [u8], mut state: u64) -> u64 {
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
    }
    state
}

/// The extensions the two test-ROM folders use for ROM images.
const ROM_EXTENSIONS: [&str; 13] = [
    "gb", "gbc", "gba", "nes", "unf", "unif", "fds", "qd", "nsf", "sfc", "smc", "swc", "fig",
];

/// Whether `name` names a ROM image, by its last extension.
fn is_rom_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ROM_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// Whether `name` names a Super NES image.
fn is_snes_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ["sfc", "smc", "swc", "fig"]
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// DESIGN §3.2 row 8's score for a whole image, as `identify` computes it.
fn snes_score_of(bytes: &[u8]) -> i32 {
    let rom = if bytes.len() % 1024 == 512 {
        &bytes[512..]
    } else {
        bytes
    };
    let mut best = i32::MIN;
    for (at, board) in CANDIDATES {
        best = best.max(score(rom, at, board));
    }
    best
}

/// Every ROM image under `folder`: zip members are read in memory.
fn images_in(
    folder: &Path,
    images: &mut Vec<(String, Vec<u8>)>,
    archives: &mut usize,
    skipped: &mut usize,
) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            images_in(&path, images, archives, skipped);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            *skipped += 1;
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            *skipped += 1;
            continue;
        };
        if name.to_ascii_lowercase().ends_with(".zip") {
            *archives += 1;
            let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
                continue;
            };
            for index in 0..zip.len() {
                let Ok(mut member) = zip.by_index(index) else {
                    *skipped += 1;
                    continue;
                };
                if member.is_dir() || !is_rom_name(member.name()) {
                    continue;
                }
                let member_name = member.name().to_owned();
                let mut data = Vec::with_capacity(member.size() as usize);
                if member.read_to_end(&mut data).is_ok() {
                    images.push((member_name, data));
                } else {
                    *skipped += 1;
                }
            }
        } else if is_rom_name(name) {
            images.push((name.to_owned(), bytes));
        } else {
            *skipped += 1;
        }
    }
}

/// DESIGN §3.2's UNVERIFIED item: 8 must separate SNES images from everything else.
///
/// Run it explicitly, with the two folders named:
/// `cargo test -p tendy-detect --release --test signature snes_threshold_survey -- --ignored --nocapture`.
/// It prints counts only; an image that breaks the rule is named with its score.
#[ignore = "surveys the folders named by TENDY_TEST_ROMS and TENDY_NES_TEST_ROMS"]
#[test]
fn snes_threshold_survey() {
    let mut images = Vec::new();
    let mut archives = 0;
    let mut skipped = 0;
    for variable in ["TENDY_TEST_ROMS", "TENDY_NES_TEST_ROMS"] {
        match std::env::var_os(variable) {
            Some(folder) => images_in(Path::new(&folder), &mut images, &mut archives, &mut skipped),
            None => println!("snes_threshold_survey: {variable} is not set"),
        }
    }

    let (mut snes, mut other) = (0usize, 0usize);
    let mut snes_below = Vec::new();
    let mut other_above = Vec::new();
    for (name, bytes) in &images {
        let value = snes_score_of(bytes);
        if is_snes_name(name) {
            snes += 1;
            if value < SNES_SIGNATURE_MIN_SCORE {
                snes_below.push((name.as_str(), value));
            }
        } else {
            other += 1;
            if value >= SNES_SIGNATURE_MIN_SCORE {
                other_above.push((name.as_str(), value));
            }
        }
    }

    const RANDOM_BUFFERS: usize = 1000;
    const RANDOM_SIZES: [usize; 6] = [0x1000, 0x8000, 0x1_0000, 0x2_0000, 0x8_0000, 0x10_0000];
    let mut random_above = 0usize;
    let mut random_best = i32::MIN;
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for index in 0..RANDOM_BUFFERS {
        let mut bytes = vec![0x00; RANDOM_SIZES[index % RANDOM_SIZES.len()]];
        state = xorshift(&mut bytes, state);
        let value = snes_score_of(&bytes);
        random_best = random_best.max(value);
        random_above += usize::from(value >= SNES_SIGNATURE_MIN_SCORE);
    }

    println!("snes_threshold_survey, threshold {SNES_SIGNATURE_MIN_SCORE} of 13:");
    println!(
        "  SNES images {snes}, below the threshold {}",
        snes_below.len()
    );
    println!(
        "  other images {other}, at or above the threshold {}",
        other_above.len()
    );
    println!("  archives read {archives}, files not ROM images {skipped}");
    println!(
        "  random buffers {RANDOM_BUFFERS}, at or above the threshold {random_above}, best {random_best}"
    );
    for (name, value) in &snes_below {
        println!("  SNES image below the threshold: {name} scored {value}");
    }
    for (name, value) in &other_above {
        println!("  non-SNES image at or above the threshold: {name} scored {value}");
    }

    assert!(
        snes > 0 && other > 0,
        "no images surveyed: set both folders"
    );
    assert!(
        snes_below.is_empty() && other_above.is_empty() && random_above == 0,
        "the threshold does not separate the sets"
    );
}
