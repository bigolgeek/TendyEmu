//! UNIF (Universal NES Image Format) container parser.
//!
//! A UNIF image is a 32-byte header (`UNIF`, a little-endian revision, 24 reserved bytes) followed
//! by Type+Length+Value chunks: a 4-byte type, a little-endian 32-bit length, then that many data
//! bytes. The format is a superseded alternative to iNES whose selling point was naming the exact
//! board (`MAPR`) instead of quoting a mapper number. References: <https://www.nesdev.org/wiki/UNIF>
//! and the original specification, REV 7c, published with libunif
//! (<https://github.com/eteran/libunif>, `UNIF_current.txt`).
//!
//! The emulator core understands iNES only ([`crate::cart::split`]), so this module turns a UNIF
//! image into the same [`RomData`] that path produces, and [`to_ines`] wraps that data in a
//! synthesised 16-byte iNES header — which is what the frontend loads, so a `.unf` file takes the
//! ordinary path through `mapper::build` unchanged.
//!
//! Board names come from the nesdev "UNIF to NES 2.0 Mapping" table
//! (<https://www.nesdev.org/wiki/UNIF_to_NES_2.0_Mapping>, revision 24185). Only boards whose
//! mapper this crate implements are in [`BOARDS`]; any other `MAPR` is [`CartError::UnknownBoard`],
//! which names the board rather than guessing a mapper.
//!
//! Chunks handled: `MAPR` (the board), `PRG0`-`PRGF` and `CHR0`-`CHRF` (the ROM halves, concatenated
//! in hex-digit order), `MIRR` (hardwired mirroring), `BATR` (battery-backed RAM) and `NAME` (the
//! game's name, kept for diagnostics). Everything else — `PCK`/`CCK` checksums, `DINF`, `READ`,
//! `WRTR`, `CTRL`, `TVCI`, `VROR`, and any type this crate has never heard of — is skipped by its
//! length, which is what the format asks a reader to do.

use crate::cart::{CartError, Header, Mirroring, RomData};

/// The four bytes every UNIF image starts with.
pub const MAGIC: &[u8; 4] = b"UNIF";

/// Bytes in the UNIF file header: magic, revision, then 24 reserved bytes.
pub const HEADER_LEN: usize = 32;

/// iNES bank sizes in bytes: PRG banks are 16 KB, CHR banks 8 KB.
const PRG_BANK: usize = 16 * 1024;
const CHR_BANK: usize = 8 * 1024;

/// CHR RAM a UNIF board with no `CHR` chunk gets: the iNES 1.0 convention this crate already uses
/// for a CHR-ROM-less header (see [`crate::cart::split`]).
const CHR_RAM_LEN: usize = 8 * 1024;

/// One Type+Length+Value block, borrowing its data from the image it was parsed out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// The 4-byte type, e.g. `b"MAPR"`.
    pub id: [u8; 4],
    /// The chunk's data, exactly `length` bytes; no padding follows it in the format.
    pub data: &'a [u8],
}

impl Chunk<'_> {
    /// The chunk type as text, for logs and error messages.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.id).into_owned()
    }
}

/// A parsed UNIF image: the board it names, the game name when the file carries one, and the
/// cartridge data the rest of the emulator works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The `MAPR` board name as the file spells it, trimmed of surrounding whitespace.
    pub board: String,
    /// The `NAME` chunk without its null terminator, when present.
    pub name: Option<String>,
    /// The cartridge, in the shape [`crate::cart::split`] hands to `mapper::build`.
    pub rom: RomData,
}

/// True when `bytes` starts with the UNIF magic.
pub fn is_unif(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC
}

/// Split an image into its chunks, in file order.
///
/// The header's revision and reserved bytes are not checked: no reader needs them, and old dumpers
/// wrote nothing meaningful there. A magic-less image is [`CartError::BadMagic`], a header shorter
/// than 32 bytes [`CartError::TooShort`], and a chunk whose length runs past the end of the file
/// [`CartError::Truncated`].
pub fn chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>, CartError> {
    if !is_unif(bytes) {
        return Err(CartError::BadMagic);
    }
    if bytes.len() < HEADER_LEN {
        return Err(CartError::TooShort);
    }
    let mut out = Vec::new();
    let mut at = HEADER_LEN;
    while at < bytes.len() {
        // A partial chunk header can only be a truncated file.
        if bytes.len() - at < 8 {
            return Err(CartError::Truncated);
        }
        let id = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        let start = at + 8;
        let end = start
            .checked_add(len as usize)
            .ok_or(CartError::Truncated)?;
        if end > bytes.len() {
            return Err(CartError::Truncated);
        }
        out.push(Chunk {
            id,
            data: &bytes[start..end],
        });
        at = end;
    }
    Ok(out)
}

/// `MAPR` names that map onto a board this crate implements, with that board's iNES mapper.
///
/// The names are the prefix-stripped stems of the nesdev "UNIF to NES 2.0 Mapping" table (revision
/// 24185): that table lists `*-NROM`, `*-SNROM`, `*-TLROM` and so on because dumps disagree about
/// the vendor prefix, so [`board_mapper`] tries the exact name first and then each prefix in
/// [`VENDOR_PREFIXES`].
const BOARDS: &[(&str, u16)] = &[
    // Mapper 0: NROM, and the other boards with 8/16/32 KB of PRG and no banking.
    ("NROM", 0),
    ("NROM-128", 0),
    ("NROM-256", 0),
    ("HROM", 0),
    ("RROM", 0),
    ("RTROM", 0),
    ("SROM", 0),
    ("STROM", 0),
    ("FAMILYBASIC", 0),
    ("JALECO-JF01", 0),
    ("JALECO-JF02", 0),
    ("JALECO-JF03", 0),
    ("JALECO-JF04", 0),
    ("NAMCOT-3301", 0),
    ("NAMCOT-3302", 0),
    ("NAMCOT-3303", 0),
    ("NAMCOT-3304", 0),
    ("NAMCOT-3305", 0),
    ("NAMCOT-3311", 0),
    ("NAMCOT-3312", 0),
    // Mapper 1: MMC1.
    ("SAROM", 1),
    ("SBROM", 1),
    ("SCROM", 1),
    ("SC1ROM", 1),
    ("SEROM", 1),
    ("SFROM", 1),
    ("SF1ROM", 1),
    ("SFEXPROM", 1),
    ("SGROM", 1),
    ("SHROM", 1),
    ("SH1ROM", 1),
    ("SIROM", 1),
    ("SJROM", 1),
    ("SKROM", 1),
    ("SLROM", 1),
    ("SL1ROM", 1),
    ("SL2ROM", 1),
    ("SL3ROM", 1),
    ("SLRROM", 1),
    ("SMROM", 1),
    ("SNROM", 1),
    ("SNWEPROM", 1),
    ("SOROM", 1),
    ("SUROM", 1),
    ("SXROM", 1),
    // Mapper 2: UxROM.
    ("UNROM", 2),
    ("UOROM", 2),
    ("JALECO-JF15", 2),
    ("JALECO-JF18", 2),
    // Mapper 3: CNROM.
    ("CNROM", 3),
    ("NAMCOT-CNROM+WRAM", 3),
    ("BANDAI-PT-554", 3),
    // Mapper 4: MMC3.
    ("B4", 4),
    ("HKROM", 4),
    ("TBROM", 4),
    ("TEROM", 4),
    ("TFROM", 4),
    ("TGROM", 4),
    ("TKROM", 4),
    ("TK1ROM", 4),
    ("TKEPROM", 4),
    ("TLROM", 4),
    ("TL1ROM", 4),
    ("TL2ROM", 4),
    ("TNROM", 4),
    ("TR1ROM", 4),
    ("TSROM", 4),
    ("TVROM", 4),
    ("ACCLAIM-MC-ACC", 4),
    // Mapper 5: MMC5.
    ("EKROM", 5),
    ("ELROM", 5),
    ("ETROM", 5),
    ("EWROM", 5),
    // Mapper 7: AxROM.
    ("AMROM", 7),
    ("ANROM", 7),
    ("AN1ROM", 7),
    ("AOROM", 7),
    // Mapper 9: MMC2.
    ("PNROM", 9),
    ("PEEOROM", 9),
    // Mapper 10: MMC4.
    ("FJROM", 10),
    ("FKROM", 10),
    // Mapper 13: CPROM.
    ("CPROM", 13),
    // Mapper 19: Namco 163.
    ("NAMCOT-129", 19),
    ("NAMCOT-163", 19),
    // Mapper 34: BNROM.
    ("BNROM", 34),
    // Mapper 79: the Sachen SA-016-1M row of the mapping table, plus the NINA-03/NINA-06 MAPR names
    // AVE dumps carry for the same board (the table lists one stem per function, not per board).
    ("SA-016-1M", 79),
    ("NINA-03", 79),
    ("NINA-06", 79),
    // Mapper 85: VRC7.
    ("VRC7", 85),
    // Mapper 93: Sunsoft-2 on the Sunsoft-3R board (the table's only name for it).
    ("SUNSOFT_UNROM", 93),
    // Mapper 185: CNROM with the security diodes (the table's `*-CNROM+SECURITY`).
    ("CNROM+SECURITY", 185),
];

/// Vendor prefixes UNIF dumps put in front of a board stem, longest first so `NAMCOT-CNROM+WRAM`
/// survives [`board_mapper`]'s first, exact lookup.
const VENDOR_PREFIXES: &[&str] = &[
    "NES-", "HVC-", "BTL-", "BMC-", "UNL-", "KONAMI-", "NAMCOT-", "SUNSOFT-", "JALECO-", "BANDAI-",
    "ACCLAIM-",
];

/// The iNES mapper a UNIF board name maps to, when this crate implements that board.
///
/// The name is trimmed and upper-cased first (real dumps carry both), then matched exactly, then
/// with one vendor prefix removed. `None` means "no board this crate can build" — [`parse`] turns
/// that into [`CartError::UnknownBoard`].
pub fn board_mapper(board: &str) -> Option<u16> {
    let name = board.trim().to_ascii_uppercase();
    let lookup = |stem: &str| BOARDS.iter().find(|(b, _)| *b == stem).map(|(_, m)| *m);
    if let Some(mapper) = lookup(&name) {
        return Some(mapper);
    }
    for prefix in VENDOR_PREFIXES {
        if let Some(stem) = name.strip_prefix(prefix)
            && let Some(mapper) = lookup(stem)
        {
            return Some(mapper);
        }
    }
    None
}

/// The chunk type's data as null-terminated UTF-8 text (the encoding `MAPR` and `NAME` use).
fn text(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).trim().to_string()
}

/// The bank number a `PRGn`/`CHRn` type names, from its one hex digit (`0`-`9`, `A`-`F`).
fn bank_index(id: [u8; 4]) -> Option<usize> {
    match id[3] {
        b'0'..=b'9' => Some(usize::from(id[3] - b'0')),
        b'A'..=b'F' => Some(usize::from(id[3] - b'A') + 10),
        b'a'..=b'f' => Some(usize::from(id[3] - b'a') + 10),
        _ => None,
    }
}

/// Parse a UNIF image into the [`RomData`] the rest of the emulator runs.
///
/// `PRG` chunks are concatenated in bank order (`PRG0`, `PRG1`, ... `PRGF`), as are `CHR` chunks;
/// a board with no `CHR` chunk gets 8 KB of CHR RAM. Both halves are padded up to whole banks with
/// `0xFF`, the erased-ROM value, exactly as [`crate::cart::split`] pads an iNES image.
pub fn parse(bytes: &[u8]) -> Result<Image, CartError> {
    let chunks = chunks(bytes)?;
    let mut board: Option<String> = None;
    let mut name: Option<String> = None;
    let mut mirroring = Mirroring::Horizontal;
    let mut battery = false;
    let mut prg: [Option<&[u8]>; 16] = [None; 16];
    let mut chr: [Option<&[u8]>; 16] = [None; 16];

    for chunk in &chunks {
        if &chunk.id == b"MAPR" {
            board = Some(text(chunk.data));
        } else if &chunk.id == b"NAME" {
            name = Some(text(chunk.data));
        } else if &chunk.id == b"MIRR" {
            // One byte, the hardwired arrangement (spec REV 7c, and the iNES flags it mirrors):
            // 0 horizontal, 1 vertical, 2 single-screen $2000, 3 single-screen $2400, 4 four-screen.
            // 5 means "the mapper controls it", so the header value never matters.
            let value = *chunk.data.first().ok_or(CartError::Truncated)?;
            mirroring = match value {
                0 | 5 => Mirroring::Horizontal,
                1 => Mirroring::Vertical,
                2 => Mirroring::SingleScreenLower,
                3 => Mirroring::SingleScreenUpper,
                4 => Mirroring::FourScreen,
                _ => return Err(CartError::BadMirroring(value)),
            };
        } else if &chunk.id == b"BATR" {
            // The chunk's presence is the flag; its one data byte says nothing.
            battery = true;
        } else if chunk.id[..3] == *b"PRG" {
            if let Some(index) = bank_index(chunk.id) {
                prg[index] = Some(chunk.data);
            }
        } else if chunk.id[..3] == *b"CHR"
            && let Some(index) = bank_index(chunk.id)
        {
            chr[index] = Some(chunk.data);
        }
    }

    // `MAPR` is required: without it there is no board to build. The format calls it optional,
    // but an image with no board name cannot run here, so it is an image without a board rather
    // than a guess.
    let board = board.ok_or_else(|| CartError::UnknownBoard("<missing MAPR>".to_string()))?;
    let mapper = board_mapper(&board).ok_or(CartError::UnknownBoard(board.clone()))?;

    let mut prg_bytes: Vec<u8> = Vec::new();
    for bank in prg.iter().flatten() {
        prg_bytes.extend_from_slice(bank);
    }
    if prg_bytes.is_empty() {
        return Err(CartError::Truncated);
    }
    let mut chr_bytes: Vec<u8> = Vec::new();
    for bank in chr.iter().flatten() {
        chr_bytes.extend_from_slice(bank);
    }

    let prg_rom_bytes = prg_bytes.len();
    let prg_banks = prg_rom_bytes.div_ceil(PRG_BANK);
    prg_bytes.resize(prg_banks * PRG_BANK, 0xFF);

    let chr_rom_bytes = chr_bytes.len();
    let chr_banks = chr_rom_bytes.div_ceil(CHR_BANK);
    let (chr, chr_is_ram) = if chr_bytes.is_empty() {
        (vec![0; CHR_RAM_LEN], true)
    } else {
        chr_bytes.resize(chr_banks * CHR_BANK, 0xFF);
        (chr_bytes, false)
    };

    let header = Header {
        prg_banks,
        chr_banks: if chr_is_ram { 0 } else { chr_banks },
        prg_rom_bytes,
        chr_rom_bytes,
        mapper,
        mirroring,
        battery,
        ..Header::default()
    };
    Ok(Image {
        board,
        name,
        rom: RomData {
            header,
            prg: prg_bytes,
            chr,
            chr_is_ram,
            trainer: None,
        },
    })
}

/// The board name upper-cased with one vendor prefix removed, the way [`board_mapper`] looks it up:
/// the NES 2.0 lookups below match this stem so `ACCLAIM-MC-ACC` and `NES-HKROM` both land.
fn board_stem(board: &str) -> String {
    let name = board.trim().to_ascii_uppercase();
    match VENDOR_PREFIXES.iter().find_map(|p| name.strip_prefix(p)) {
        Some(stem) => stem.to_string(),
        None => name,
    }
}

/// The NES 2.0 PRG-RAM size shift for a board that carries more than the 8 KiB every iNES 1.0
/// image answers with: SOROM has 16 KiB, SXROM 32 KiB (NESdev MMC1 "PRG RAM capacity").
fn nes2_prg_ram_shift(board: &str) -> Option<u8> {
    match board_stem(board).as_str() {
        "SOROM" => Some(8), // 64 << 8 = 16 KiB
        "SXROM" => Some(9), // 64 << 9 = 32 KiB
        _ => None,
    }
}

/// The NES 2.0 submapper a board needs where its MMC3 variant differs from a plain MMC3:
/// HKROM is MMC6 (submapper 1: 1 KiB of protected PRG RAM), and ACCLAIM-MC-ACC counts A12 rises
/// instead of falls (submapper 3).
fn nes2_submapper(board: &str) -> Option<u8> {
    match board_stem(board).as_str() {
        "HKROM" => Some(1),
        "MC-ACC" => Some(3),
        _ => None,
    }
}

/// Parse a UNIF image and wrap the result in a 16-byte header, so the frontend can hand the bytes
/// to [`crate::cart::split`] exactly like a `.nes` file. SOROM and SXROM get a NES 2.0 header, the
/// only one that carries their 16/32 KiB of PRG RAM, as do the MMC3 variants whose submapper iNES
/// 1.0 cannot express (MC-ACC and HKROM); every other board gets iNES 1.0.
///
/// The image must fit iNES 1.0: at most 255 PRG banks, 255 CHR banks and mapper 255. A UNIF board
/// with CHR RAM is written with no CHR banks, which `split` reads back as the same 8 KB of CHR RAM.
pub fn to_ines(bytes: &[u8]) -> Result<Vec<u8>, CartError> {
    let image = parse(bytes)?;
    // SOROM and SXROM are the MMC1 boards with more PRG RAM than iNES 1.0 can express:
    // `Header::prg_ram_size` reports 8 KiB to every iNES 1.0 image, so their size only survives in
    // a NES 2.0 header, as a shift of 64 bytes (NESdev: "PRG-RAM size has to be assumed; 32 KiB of
    // it are sufficient"). The MMC3 variants are the same story for their submapper. Every other
    // board keeps the byte-identical iNES 1.0 header.
    let prg_ram_shift = nes2_prg_ram_shift(&image.board);
    let submapper = nes2_submapper(&image.board);
    let nes2 = prg_ram_shift.is_some() || submapper.is_some();
    let rom = image.rom;
    let header = &rom.header;
    if header.prg_banks > 0xFF || header.chr_banks > 0xFF {
        return Err(CartError::UnsupportedSize);
    }
    if header.mapper > 0xFF {
        return Err(CartError::UnsupportedMapper(header.mapper));
    }

    let mut out = Vec::with_capacity(16 + rom.prg.len() + rom.chr.len());
    out.extend_from_slice(b"NES\x1A");
    out.push(header.prg_banks as u8);
    out.push(header.chr_banks as u8);
    let mut flags6 = (header.mapper as u8 & 0x0F) << 4;
    flags6 |= match header.mirroring {
        Mirroring::Vertical => 0x01,
        // iNES 1.0 has no flag for the two single-screen arrangements; every board that runs them
        // controls mirroring itself, so the header value is that board's starting arrangement.
        Mirroring::Horizontal | Mirroring::SingleScreenLower | Mirroring::SingleScreenUpper => 0x00,
        Mirroring::FourScreen => 0x08,
    };
    if header.battery {
        flags6 |= 0x02;
    }
    out.push(flags6);
    out.push(header.mapper as u8 & 0xF0 | if nes2 { 0x08 } else { 0x00 });
    if nes2 {
        // NES 2.0: byte 8 is submapper | mapper MSB, byte 9 the ROM-size MSBs, byte 10 the
        // PRG-RAM (low nibble) / battery-backed PRG-NVRAM (high nibble) shifts, byte 11 the
        // CHR-RAM shift, then timing, console, miscellaneous and expansion bytes.
        out.push(submapper.unwrap_or(0) << 4);
        out.push(0x00);
        // A board with no larger size keeps iNES 1.0's 8 KiB, which NES 2.0 spells as shift 7.
        let ram_shift = prg_ram_shift.unwrap_or(7);
        out.push(if header.battery {
            ram_shift << 4
        } else {
            ram_shift
        });
        out.push(if rom.chr_is_ram { 0x07 } else { 0x00 }); // 64 << 7 = 8 KiB CHR RAM
        out.extend_from_slice(&[0; 4]);
    } else {
        out.extend_from_slice(&[0; 8]);
    }
    out.extend_from_slice(&rom.prg);
    if !rom.chr_is_ram {
        out.extend_from_slice(&rom.chr);
    }
    Ok(out)
}
