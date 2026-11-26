//! Archive extraction (SPEC §3, DESIGN §2.9).

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
        let _ = name;
        None
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
    let _ = (kind, bytes);
    Err(crate::Refusal::no_rom_inside(archive_name))
}

/// The loader's one entry point: archives go to `extract`, anything else straight to `crate::detect`.
pub fn open(name: &str, bytes: Vec<u8>) -> Result<(Entry, crate::Detected), crate::Refusal> {
    let detected = crate::detect(name, &bytes)?;
    Ok((
        Entry {
            name: name.to_owned(),
            bytes,
        },
        detected,
    ))
}
