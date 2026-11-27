//! A ROM-hash database of header fixups for the well-known bad iNES dumps.
//!
//! An iNES 1.0 header is written by whoever dumped the cartridge, and for a long list of
//! well-known ROM images it is wrong: the battery flag is missing (the game boots and then silently
//! never saves), an NROM/UNROM board declares its mirroring the wrong way round (status bars and
//! walls land in the wrong nametable), or the mapper nibble names a board the cartridge does not
//! have. Mesen and Nestopia answer that the way this module does: key a table on the ROM's own data
//! — never its file name or its header — and treat the table's board description as authoritative.
//!
//! The key is the CRC-32 of the PRG ROM followed by the CHR ROM, `crc32(prg ++ chr)`, with the
//! 16-byte header and any trainer excluded. Every database in this space uses that number, so a
//! hash can be looked up across all of them: Mesen's `MesenNesDB.txt`, the NewRisingSun NES 2.0
//! header database it is generated from, NesCartDB, and FCEUmm's `ines-correct.h`. Every row in
//! [`TABLE`] below appears in both the Mesen/NES 2.0 database and FCEUmm's table, and the two agree
//! on every field it carries; a dump that only one of them lists is left out.
//!
//! What a fixup may change is the three iNES 1.0 fields whose wrong value is invisible until the
//! game misbehaves: mapper, mirroring and the battery flag. Sizes are not touched, because a wrong
//! PRG/CHR size moves where the ROM's data starts — that dump would have to be redone, not
//! re-declared. A NES 2.0 header is never corrected either: it came from the same databases, so its
//! author already did this lookup.
//!
//! Applying a fixup is deliberately conservative. Only the field that actually differs is written,
//! so a ROM whose header is already right comes out of [`apply`] untouched — no field is assigned,
//! no stderr line is printed — and every change that is made is reported to stderr, one line per
//! ROM, so a user can tell why a game they know is mislabelled now behaves.

use crate::cart::{Header, Mirroring};
use crate::checksum::{crc32, crc32_continue};

/// One row of the database: the hash of the ROM's data and the board it really is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fixup {
    /// CRC-32 of PRG then CHR ([`rom_crc32`]).
    pub crc32: u32,
    /// The dump this row describes, for the stderr line and for anyone reading the table.
    pub name: &'static str,
    /// The board's mapper number.
    pub mapper: Option<u16>,
    /// The board's nametable arrangement.
    pub mirroring: Option<Mirroring>,
    /// Whether the board has battery-backed save RAM.
    pub battery: Option<bool>,
}

/// The database, sorted by [`Fixup::crc32`] so [`lookup`] can binary-search it; a test pins the
/// order. Adding a row means putting it in hash order.
pub const TABLE: &[Fixup] = &[
    // 0x1335CB05 — Crystalis (US). TKROM board; the common dump has no battery bit, so the save
    // file is never written.
    Fixup {
        crc32: 0x1335_CB05,
        name: "Crystalis (US)",
        mapper: Some(4),
        mirroring: None,
        battery: Some(true),
    },
    // 0x1D0F4D6B — The Black Bass (US), UNROM.
    Fixup {
        crc32: 0x1D0F_4D6B,
        name: "The Black Bass (US)",
        mapper: Some(2),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0x24598791 — Duck Hunt (World), RROM.
    Fixup {
        crc32: 0x2459_8791,
        name: "Duck Hunt (World)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0x2545214C — Dragon Warrior (US), SAROM with battery-backed save RAM.
    Fixup {
        crc32: 0x2545_214C,
        name: "Dragon Warrior (US)",
        mapper: Some(1),
        mirroring: None,
        battery: Some(true),
    },
    // 0x32FA246F — Tag Team Pro-Wrestling (Japan), NROM-3311.
    Fixup {
        crc32: 0x32FA_246F,
        name: "Tag Team Pro-Wrestling (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0x330DE468 — Obake no Q Tarou - Wanwan Panic (Japan), Bandai NROM-256.
    Fixup {
        crc32: 0x330D_E468,
        name: "Obake no Q Tarou (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0x3FE272FB — The Legend of Zelda (US), the PRG0 revision; SNROM, battery-backed.
    Fixup {
        crc32: 0x3FE2_72FB,
        name: "The Legend of Zelda (US, PRG0)",
        mapper: Some(1),
        mirroring: None,
        battery: Some(true),
    },
    // 0x43D30C2F — Ms. Pac-Man (Tengen, unlicensed), Tengen 800003 NROM board.
    Fixup {
        crc32: 0x43D3_0C2F,
        name: "Ms. Pac-Man (Tengen)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0x49AEB3A6 — Excitebike (Japan, USA), NROM-128.
    Fixup {
        crc32: 0x49AE_B3A6,
        name: "Excitebike (Japan, USA)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0x5112DC21 — Wild Gunman (World) (Rev 1), NROM-128.
    Fixup {
        crc32: 0x5112_DC21,
        name: "Wild Gunman (World)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0x57E220D0 — Final Fantasy III (Japan), TNROM with battery-backed save RAM.
    Fixup {
        crc32: 0x57E2_20D0,
        name: "Final Fantasy III (Japan)",
        mapper: Some(4),
        mirroring: None,
        battery: Some(true),
    },
    // 0x6D65CAC6 — Terra Cresta (Japan), HVC-UNROM.
    Fixup {
        crc32: 0x6D65_CAC6,
        name: "Terra Cresta (Japan)",
        mapper: Some(2),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0x9EA1DC76 — Rainbow Islands (USA), UNROM-09.
    Fixup {
        crc32: 0x9EA1_DC76,
        name: "Rainbow Islands (USA)",
        mapper: Some(2),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0xB3C30BEA — Xevious (Japan), Namcot 3303 NROM board.
    Fixup {
        crc32: 0xB3C3_0BEA,
        name: "Xevious (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0xBA322865 — Zelda II: The Adventure of Link (US), SKROM with battery-backed save RAM.
    Fixup {
        crc32: 0xBA32_2865,
        name: "Zelda II (US)",
        mapper: Some(1),
        mirroring: None,
        battery: Some(true),
    },
    // 0xC05A365B — Chou Fuyuu Yousai Exed Exes (Japan), HVC-NROM-256.
    Fixup {
        crc32: 0xC05A_365B,
        name: "Chou Fuyuu Yousai Exed Exes (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0xC4C3949A — Mario Bros. (World), HVC-NROM-128.
    Fixup {
        crc32: 0xC4C3_949A,
        name: "Mario Bros. (World)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0xCEBD2A31 — Final Fantasy (US), SNROM with battery-backed save RAM. The classic example:
    // the dump boots, saves nothing, and starts over from the beginning every session.
    Fixup {
        crc32: 0xCEBD_2A31,
        name: "Final Fantasy (US)",
        mapper: Some(1),
        mirroring: None,
        battery: Some(true),
    },
    // 0xE1B260DA — Argos no Senshi (Japan), HVC-UNROM.
    Fixup {
        crc32: 0xE1B2_60DA,
        name: "Argos no Senshi (Japan)",
        mapper: Some(2),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0xE28F2596 — Pac-Land (Japan), the [b2] bad dump that circulates with the wrong NROM board.
    Fixup {
        crc32: 0xE28F_2596,
        name: "Pac-Land (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0xE492D45A — Zippy Race (Japan), Irem NROM-128.
    Fixup {
        crc32: 0xE492_D45A,
        name: "Zippy Race (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
    // 0xE6A477B2 — 3-D WorldRunner (USA), UNROM-04.
    Fixup {
        crc32: 0xE6A4_77B2,
        name: "3-D WorldRunner (USA)",
        mapper: Some(2),
        mirroring: Some(Mirroring::Vertical),
        battery: Some(false),
    },
    // 0xEAF7ED72 — The Legend of Zelda (US), the PRG1 revision; SNROM, battery-backed.
    Fixup {
        crc32: 0xEAF7_ED72,
        name: "The Legend of Zelda (US)",
        mapper: Some(1),
        mirroring: None,
        battery: Some(true),
    },
    // 0xFCDACA80 — Elevator Action (Japan), Taito FC008 NROM-256.
    Fixup {
        crc32: 0xFCDA_CA80,
        name: "Elevator Action (Japan)",
        mapper: Some(0),
        mirroring: Some(Mirroring::Horizontal),
        battery: Some(false),
    },
];

/// CRC-32 of a cartridge's data as the databases key it: the PRG ROM followed by the CHR ROM.
pub fn rom_crc32(prg: &[u8], chr: &[u8]) -> u32 {
    crc32_continue(crc32(prg), chr)
}

/// The database row for a PRG+CHR hash, if the ROM is one this module knows.
pub fn lookup(crc32: u32) -> Option<&'static Fixup> {
    let index = TABLE
        .binary_search_by_key(&crc32, |fixup| fixup.crc32)
        .ok()?;
    Some(&TABLE[index])
}

/// The fields a fixup actually rewrote, with their old and new values, so the change can be logged
/// and tested; the field is `None` when the ROM's header already agreed with the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Correction {
    pub name: &'static str,
    pub crc32: u32,
    pub mapper: Option<(u16, u16)>,
    pub mirroring: Option<(Mirroring, Mirroring)>,
    pub battery: Option<(bool, bool)>,
}

impl Correction {
    /// True when the ROM's header already said what the database says, so nothing was written.
    pub fn is_empty(&self) -> bool {
        self.mapper.is_none() && self.mirroring.is_none() && self.battery.is_none()
    }

    /// The one-line report, e.g.
    /// `gamedb: "Final Fantasy (US)" (PRG+CHR CRC32 0xCEBD2A31): mapper 0 -> 1, battery off -> on`.
    pub fn describe(&self) -> String {
        let mut changes = Vec::new();
        if let Some((from, to)) = self.mapper {
            changes.push(format!("mapper {from} -> {to}"));
        }
        if let Some((from, to)) = self.mirroring {
            changes.push(format!("mirroring {from:?} -> {to:?}"));
        }
        if let Some((from, to)) = self.battery {
            changes.push(format!("battery {} -> {}", on_off(from), on_off(to)));
        }
        format!(
            "gamedb: \"{}\" (PRG+CHR CRC32 {:08X}): {}",
            self.name,
            self.crc32,
            changes.join(", ")
        )
    }

    /// Report the correction on stderr: a load is the only thing that prints this, and a ROM with a
    /// good header never reaches it.
    pub fn log(&self) {
        eprintln!("{}", self.describe());
    }
}

fn on_off(flag: bool) -> &'static str {
    if flag { "on" } else { "off" }
}

/// Correct `header` in place from the database, returning what changed — `None` for a ROM the
/// database does not know or whose header already matches it.
///
/// A NES 2.0 header is trusted as written: it comes from the same databases this table is drawn
/// from, and its author's lookup is newer than ours.
pub fn apply(header: &mut Header, prg: &[u8], chr: &[u8]) -> Option<Correction> {
    if header.nes2 {
        return None;
    }
    apply_in(TABLE, header, prg, chr)
}

/// The lookup-and-rewrite behind [`apply`], against an arbitrary table: the tests use it to drive
/// a fixup whose hash they build a fixture around.
pub fn apply_in(
    table: &[Fixup],
    header: &mut Header,
    prg: &[u8],
    chr: &[u8],
) -> Option<Correction> {
    let crc32 = rom_crc32(prg, chr);
    let index = table
        .binary_search_by_key(&crc32, |fixup| fixup.crc32)
        .ok()?;
    let fixup = &table[index];

    let mut correction = Correction {
        name: fixup.name,
        crc32,
        mapper: None,
        mirroring: None,
        battery: None,
    };
    if let Some(mapper) = fixup.mapper
        && mapper != header.mapper
    {
        correction.mapper = Some((header.mapper, mapper));
        header.mapper = mapper;
    }
    if let Some(mirroring) = fixup.mirroring
        && mirroring != header.mirroring
    {
        correction.mirroring = Some((header.mirroring, mirroring));
        header.mirroring = mirroring;
    }
    if let Some(battery) = fixup.battery
        && battery != header.battery
    {
        correction.battery = Some((header.battery, battery));
        header.battery = battery;
    }

    if correction.is_empty() {
        None
    } else {
        Some(correction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::split;

    /// A ROM image the size the header claims: 16-byte header, `prg_banks` 16 KB PRG banks and
    /// `chr_banks` 8 KB CHR banks of zeros.
    fn image(prg_banks: usize, chr_banks: usize, flags6: u8, flags7: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E,
            0x45,
            0x53,
            0x1A,
            prg_banks as u8,
            chr_banks as u8,
            flags6,
            flags7,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        v.resize(16 + prg_banks * 16384 + chr_banks * 8192, 0);
        v
    }

    /// XOR the four bytes at `at` with the value that makes `crc32(data)` equal `target`.
    ///
    /// CRC-32 is affine over GF(2): four free bytes inject 32 bits into the register, so they span
    /// the whole 32-bit difference space. Measure what each of those 32 bits does to the digest,
    /// then solve the 32x32 system over GF(2) by Gaussian elimination. This is what lets a test
    /// build a synthetic image carrying a real database row's hash — the lookup is exercised on
    /// the same number a real cartridge produces, not on a number invented for the test.
    fn pin_crc32(data: &mut [u8], at: usize, target: u32) {
        let base = crc32(data);
        let mut columns = [0u32; 32];
        for (bit, column) in columns.iter_mut().enumerate() {
            data[at + bit / 8] ^= 1 << (bit % 8);
            *column = crc32(data) ^ base;
            data[at + bit / 8] ^= 1 << (bit % 8);
        }

        // `basis[p]` holds a reduced column whose highest set bit is `p`, plus the mask of free
        // bits that produced it.
        let mut basis = [(0u32, 0u32); 32];
        for (bit, column) in columns.iter().enumerate() {
            let (mut value, mut mask) = (*column, 1u32 << bit);
            for pivot in (0..32).rev() {
                if value >> pivot & 1 == 0 {
                    continue;
                }
                if basis[pivot].0 == 0 {
                    basis[pivot] = (value, mask);
                    break;
                }
                value ^= basis[pivot].0;
                mask ^= basis[pivot].1;
            }
        }

        let (mut remainder, mut solution) = (target ^ base, 0u32);
        for pivot in (0..32).rev() {
            if remainder >> pivot & 1 == 1 {
                remainder ^= basis[pivot].0;
                solution ^= basis[pivot].1;
            }
        }
        assert_eq!(remainder, 0, "the four free bytes do not span every digest");

        for bit in 0..32 {
            if solution >> bit & 1 == 1 {
                data[at + bit / 8] ^= 1 << (bit % 8);
            }
        }
        assert_eq!(crc32(data), target, "pin_crc32 did not reach the target");
    }

    /// A 256 KB PRG / no CHR image (the SNROM shape of Final Fantasy) whose PRG+CHR hash is
    /// `target`. The last four PRG bytes are the free ones the hash is pinned with — the header is
    /// not part of the hash, so only the PRG slice is solved over.
    fn image_with_hash(target: u32, flags6: u8, flags7: u8) -> Vec<u8> {
        let mut v = image(16, 0, flags6, flags7);
        let prg = &mut v[16..];
        let at = prg.len() - 4;
        pin_crc32(prg, at, target);
        v
    }

    #[test]
    fn table_is_sorted_by_hash() {
        // `lookup` binary-searches, so a row added out of order would silently become invisible.
        for pair in TABLE.windows(2) {
            assert!(
                pair[0].crc32 < pair[1].crc32,
                "{} ({:08X}) is not before {} ({:08X})",
                pair[0].name,
                pair[0].crc32,
                pair[1].name,
                pair[1].crc32
            );
        }
    }

    #[test]
    fn every_row_is_reachable_by_its_own_hash() {
        for fixup in TABLE {
            assert_eq!(
                lookup(fixup.crc32),
                Some(fixup),
                "{} is not reachable",
                fixup.name
            );
        }
        assert_eq!(lookup(0x0000_0000), None);
        assert_eq!(lookup(0xFFFF_FFFF), None);
    }

    #[test]
    fn known_dumps_are_described_correctly() {
        // One row of each kind: a missing battery flag, wrong NROM mirroring, and a UNROM board
        // whose dump claims NROM.
        let final_fantasy = lookup(0xCEBD_2A31).expect("Final Fantasy is in the database");
        assert_eq!(
            (
                final_fantasy.mapper,
                final_fantasy.mirroring,
                final_fantasy.battery
            ),
            (Some(1), None, Some(true))
        );

        let excitebike = lookup(0x49AE_B3A6).expect("Excitebike is in the database");
        assert_eq!(
            (excitebike.mapper, excitebike.mirroring, excitebike.battery),
            (Some(0), Some(Mirroring::Vertical), Some(false))
        );

        let world_runner = lookup(0xE6A4_77B2).expect("3-D WorldRunner is in the database");
        assert_eq!(
            (
                world_runner.mapper,
                world_runner.mirroring,
                world_runner.battery
            ),
            (Some(2), Some(Mirroring::Vertical), Some(false))
        );
    }

    #[test]
    fn rom_crc32_is_the_hash_of_prg_then_chr() {
        // Both halves of the hash have to be chained, not hashed separately: the databases' number
        // is `crc32(prg ++ chr)`, so the continuation must equal the one-shot hash.
        let prg = b"PRG ROM bytes";
        let chr = b"CHR ROM bytes";
        let joined: Vec<u8> = prg.iter().chain(chr).copied().collect();
        assert_eq!(rom_crc32(prg, chr), crc32(&joined));
        assert_ne!(rom_crc32(prg, chr), crc32(prg) ^ crc32(chr));
    }

    #[test]
    fn bad_header_fixture_is_corrected_at_load() {
        // Final Fantasy (US) with the classic bad header: NROM, no battery bit. The image's hash is
        // the real one, so this goes through `TABLE` and then through `cart::split`, the load path.
        let rom = image_with_hash(0xCEBD_2A31, 0x00, 0x00);

        let loaded = split(&rom).expect("the fixture is a valid image");
        assert_eq!(
            loaded.header.mapper, 1,
            "mapper corrected from the database"
        );
        assert!(
            loaded.header.battery,
            "battery flag corrected from the database"
        );
        assert_eq!(loaded.prg.len(), 256 * 1024);
    }

    #[test]
    fn corrected_board_is_the_one_the_database_names() {
        // The same fixup seen through the board: a dump declaring NROM is built as UNROM, and with
        // the database's mirroring, because the correction happens before the mapper reads header.
        let rom = image_with_hash(0xE6A4_77B2, 0x00, 0x00);
        let mut board = crate::mapper::load(&rom).expect("the corrected board loads");
        assert_eq!(board.mirroring(), Mirroring::Vertical);
        assert_eq!(board.cpu_read(0xC000), 0);
    }

    #[test]
    fn good_header_is_left_alone() {
        // The same image with the header the database agrees with: no field is written and there is
        // nothing to report, which is what keeps a correct dump out of the stderr log.
        let rom = image_with_hash(0xCEBD_2A31, 0x12, 0x00); // SNROM = mapper 1 + battery
        let mut header = crate::cart::parse_header(&rom).expect("valid header");
        let before = header.clone();

        assert_eq!(apply(&mut header, &rom[16..], &[]), None);
        assert_eq!(header, before);
    }

    #[test]
    fn nes2_headers_are_trusted() {
        // NES 2.0 (flags 7 bits 2-3 = 0b10): the same data with the mapper left at NROM, which must
        // survive untouched — a NES 2.0 header's author already ran this lookup.
        let rom = image_with_hash(0xCEBD_2A31, 0x02, 0x08);
        let mut header = crate::cart::parse_header(&rom).expect("valid header");
        assert!(header.nes2);

        assert_eq!(apply(&mut header, &rom[16..], &[]), None);
        assert_eq!(header.mapper, 0);
        assert!(header.battery);
    }

    #[test]
    fn correction_line_names_every_changed_field() {
        let rom = image_with_hash(0xCEBD_2A31, 0x00, 0x00); // NROM, no battery
        let mut header = crate::cart::parse_header(&rom).expect("valid header");

        let correction = apply(&mut header, &rom[16..], &[]).expect("the header is corrected");
        assert_eq!(
            correction.describe(),
            "gamedb: \"Final Fantasy (US)\" (PRG+CHR CRC32 CEBD2A31): mapper 0 -> 1, battery off -> on"
        );

        // A mirroring-only fixup reports its own field and nothing else.
        let rom = image_with_hash(0x2459_8791, 0x00, 0x00); // Duck Hunt, image is Horizontal
        let mut header = crate::cart::parse_header(&rom).expect("valid header");
        let correction = apply(&mut header, &rom[16..], &[]).expect("the header is corrected");
        assert_eq!(
            correction.describe(),
            "gamedb: \"Duck Hunt (World)\" (PRG+CHR CRC32 24598791): mirroring Horizontal -> Vertical"
        );
    }

    #[test]
    fn unknown_rom_is_left_alone() {
        let rom = image(1, 1, 0x07, 0x00); // four-screen NROM, not in any database
        let mut header = crate::cart::parse_header(&rom).expect("valid header");
        let before = header.clone();

        assert_eq!(apply(&mut header, &rom[16..], &[]), None);
        assert_eq!(header, before);
    }
}
