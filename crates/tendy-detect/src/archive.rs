//! Archive extraction (SPEC §3, DESIGN §2.9).

use std::fmt;
use std::io::{Cursor, Read};

use crate::Refusal;
use crate::extension;
use crate::signature;

/// The archive formats SPEC §3 lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `.zip`, read with `zip` (deflate only).
    Zip,
    /// `.7z`, read with `sevenz-rust2`.
    SevenZ,
}

impl ArchiveKind {
    /// The kind a file name's extension names (case-insensitive), as all three sources decide today.
    pub fn from_name(name: &str) -> Option<ArchiveKind> {
        match extension::extension_of(name)? {
            ext if ext.eq_ignore_ascii_case("zip") => Some(ArchiveKind::Zip),
            ext if ext.eq_ignore_ascii_case("7z") => Some(ArchiveKind::SevenZ),
            _ => None,
        }
    }
}

/// The largest entry the loader reads, in bytes. `sevenz-rust2`'s memory limit covers only its own
/// buffers and dictionaries, not the size of what it extracts (docs.rs, sevenz-rust2 0.25.0,
/// `ArchiveReader::new_with_memory_limit`), so this cap is enforced by reading through `Read::take`.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// One file, taken from disk or out of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The file name, or the entry's path inside the archive.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// Picks the ROM inside an archive (SPEC §4 step 1).
/// - Pass 1: the first file entry, in stored order, whose extension `extension::system_of` knows.
/// - Pass 2: the first file entry whose bytes match a signature.
/// - Otherwise `Refusal::no_rom_inside`.
///
/// Folders are skipped, nested paths count, and entries over `MAX_ENTRY_BYTES` are skipped.
pub fn extract(
    kind: ArchiveKind,
    archive_name: &str,
    bytes: &[u8],
) -> Result<(Entry, crate::Detected), crate::Refusal> {
    extract_with(kind, archive_name, bytes, MAX_ENTRY_BYTES)
}

/// `extract` with the cap passed in, so a unit test need not allocate 64 MiB.
fn extract_with(
    kind: ArchiveKind,
    archive_name: &str,
    bytes: &[u8],
    cap: u64,
) -> Result<(Entry, crate::Detected), crate::Refusal> {
    let picked = match kind {
        ArchiveKind::Zip => pick_zip(archive_name, bytes, cap)?,
        ArchiveKind::SevenZ => pick_sevenz(archive_name, bytes, cap)?,
    };
    let Some(entry) = picked else {
        return Err(crate::Refusal::no_rom_inside(archive_name));
    };
    let detected = crate::detect(&entry.name, &entry.bytes)?;
    Ok((entry, detected))
}

/// The loader's one entry point: archives go to `extract`, anything else straight to `crate::detect`.
pub fn open(name: &str, bytes: Vec<u8>) -> Result<(Entry, crate::Detected), crate::Refusal> {
    if let Some(kind) = ArchiveKind::from_name(name) {
        return extract(kind, name, &bytes);
    }
    let detected = crate::detect(name, &bytes)?;
    Ok((
        Entry {
            name: name.to_owned(),
            bytes,
        },
        detected,
    ))
}

/// Both passes in one traversal: the first entry whose extension names a system wins outright;
/// the first entry whose bytes match a signature is kept only until such an entry appears.
fn pick_zip(archive_name: &str, bytes: &[u8], cap: u64) -> Result<Option<Entry>, Refusal> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| cannot_read(archive_name, e))?;
    let mut by_name = None;
    let mut by_signature = None;
    for index in 0..archive.len() {
        let mut member = archive
            .by_index(index)
            .map_err(|e| cannot_read(archive_name, e))?;
        if member.is_dir() {
            continue;
        }
        let name = member.name().to_owned();
        let Some(bytes) = bounded(&mut member, cap).map_err(|e| cannot_read(archive_name, e))?
        else {
            continue;
        };
        let entry = Entry { name, bytes };
        // Pass 1 wins wherever it lands, so a match ends the search.
        if extension::names_a_system(&entry.name) {
            by_name = Some(entry);
            break;
        }
        if by_signature.is_none() && signature::identify(&entry.bytes).is_some() {
            by_signature = Some(entry);
        }
    }
    Ok(by_name.or(by_signature))
}

/// `pick_zip` over a 7-Zip archive. `EntryDecision::Stop` returns the hit; a miss drains the entry,
/// which a solid block decodes in order anyway.
fn pick_sevenz(archive_name: &str, bytes: &[u8], cap: u64) -> Result<Option<Entry>, Refusal> {
    use sevenz_rust2::EntryDecision;

    let mut archive =
        sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
            .map_err(|e| cannot_read(archive_name, e))?;
    let mut by_name = None;
    let mut by_signature = None;
    archive
        .for_each_entries_with_decision(|entry, data| {
            if entry.is_directory() {
                return Ok(EntryDecision::Continue);
            }
            let name = entry.name().to_owned();
            let Some(bytes) = bounded(data, cap)? else {
                return Ok(EntryDecision::Continue);
            };
            let entry = Entry { name, bytes };
            if extension::names_a_system(&entry.name) {
                by_name = Some(entry);
                return Ok(EntryDecision::Stop);
            }
            if by_signature.is_none() && signature::identify(&entry.bytes).is_some() {
                by_signature = Some(entry);
            }
            Ok(EntryDecision::Continue)
        })
        .map_err(|e| cannot_read(archive_name, e))?;
    Ok(by_name.or(by_signature))
}

/// Reads an entry through `Read::take`, `None` when it is over `cap` bytes.
fn bounded(reader: &mut dyn Read, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut data = Vec::new();
    reader.take(cap + 1).read_to_end(&mut data)?;
    Ok((data.len() as u64 <= cap).then_some(data))
}

/// `refused: cannot read <archive>: <error>`, the wording gb and snes use today (§2.9).
fn cannot_read(archive_name: &str, error: impl fmt::Display) -> Refusal {
    Refusal {
        reason: format!("cannot read {archive_name}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tendy_core::SystemKind;

    use super::*;

    /// A zip holding one file entry per pair, in the given order.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn entry_over_cap_skipped() {
        // A test-only cap of 8 bytes: the 9-byte `.gb` entry is skipped unread past the cap, so the
        // 4-byte `.nes` entry behind it is the first the loader sees.
        let archive = zip_of(&[("big.gb", &[0x00; 9]), ("small.nes", b"NES\x1A")]);
        let (entry, detected) = extract_with(ArchiveKind::Zip, "games.zip", &archive, 8).unwrap();
        assert_eq!(entry.name, "small.nes");
        assert_eq!(detected.system, SystemKind::Nes);

        // At the real cap the first entry is under it and wins.
        let (entry, detected) =
            extract_with(ArchiveKind::Zip, "games.zip", &archive, MAX_ENTRY_BYTES).unwrap();
        assert_eq!(entry.name, "big.gb");
        assert_eq!(detected.system, SystemKind::Gb);
    }
}
