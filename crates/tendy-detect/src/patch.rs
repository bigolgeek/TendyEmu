//! Soft patches (IPS / BPS / UPS) shared by every core (SPEC §3, #30): turn the ROM file the
//! user has into the translated or hacked image they want to play, without touching the file
//! on disk.
//!
//! The decoders are pure `&[u8] -> Vec<u8>` functions, so they are tested on handcrafted
//! patches. [`beside`] and [`apply_path`] are the only functions that touch the filesystem.
//!
//! The behaviour is ported from reference implementations rather than invented:
//!
//! - **IPS** follows SourMesen/Mesen2 `Utilities/Patches/IpsPatcher.cpp`
//!   (`IpsPatcher::PatchBuffer`): `PATCH` magic, 3-byte big-endian offsets, 5-byte record
//!   headers (offset + 16-bit length), a zero length meaning an RLE record (16-bit repeat
//!   count + one value byte), records applied in file order and growing the image (new bytes
//!   zero-filled), the `EOF` terminator, and the optional 3-byte truncation length some rips
//!   append after `EOF`.
//! - **BPS** follows byuu's BPS v1 specification (public domain, <https://byuu.org/>), with the
//!   action loop and relative copy-offset arithmetic matching SourMesen/Mesen2
//!   `Utilities/Patches/BpsPatcher.cpp` (`BpsPatcher::PatchBuffer`): `BPS1` magic, variable-length
//!   numbers, SourceRead / TargetRead / SourceCopy / TargetCopy actions, then three big-endian
//!   CRC32 values (source, target, patch). The footer CRCs are verified - checking them is the
//!   point of the format - and a copy that leaves the image is an error instead of a silently
//!   clamped read.
//! - **UPS** follows byuu's UPS v1 specification as implemented by RomPatcher.js
//!   (`UPS.prototype.apply`) and Mesen2 `Utilities/Patches/UpsPatcher.cpp`: `UPS1` magic, input
//!   and output sizes as variable-length numbers, blocks of "skip" + XOR bytes terminated by a
//!   `0x00`, then three little-endian CRC32 values (input, output, patch). The `0x00` terminator
//!   counts against the target pointer but is not written. UPS is bi-linear: applying the same
//!   patch again restores the original file.
//!
//! Deliberately out of scope: IPS32 (`EEOF` with 32-bit offsets). It is reported as
//! [`PatchError::Unsupported`] instead of being half-applied.

use std::fmt;
use std::path::{Path, PathBuf};

/// The five bytes every IPS file starts with.
const IPS_MAGIC: &[u8] = b"PATCH";
/// The three bytes an IPS file ends its record list with.
const IPS_EOF: &[u8] = b"EOF";
/// The four bytes every BPS file starts with.
const BPS_MAGIC: &[u8] = b"BPS1";
/// Source CRC32, target CRC32 and patch CRC32: the fixed-size BPS footer.
const BPS_FOOTER: usize = 12;
/// The four bytes every UPS file starts with.
const UPS_MAGIC: &[u8] = b"UPS1";
/// Input CRC32, output CRC32 and patch CRC32: the fixed-size UPS footer (little-endian).
const UPS_FOOTER: usize = 12;
/// The patch extensions [`beside`] looks for, in the order a tie is broken.
const PATCH_EXTENSIONS: [&str; 3] = ["ips", "bps", "ups"];
/// The largest output a UPS patch may declare, in bytes. A ROM image is at most tens of MiB;
/// the cap keeps a hostile patch from asking for a huge allocation before its CRC is checked.
const UPS_MAX_TARGET: usize = 128 * 1024 * 1024;

/// Why a patch could not be applied. Every variant names the concrete damage instead of
/// collapsing to a bool, so the app can say which check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// No `PATCH`, `BPS1` or `UPS1` signature at the start of the file.
    NotAPatch,
    /// A patch format this module does not implement (IPS32).
    Unsupported(&'static str),
    /// The patch ended inside a header, record, action or metadata block.
    Truncated(&'static str),
    /// A variable-length number did not fit in 64 bits.
    BadVarint,
    /// The ROM is not the file this patch was built against (its size differs).
    SourceSize { expected: usize, actual: usize },
    /// The ROM is the wrong same-size file this patch was built against.
    SourceChecksum { expected: u32, actual: u32 },
    /// The patch file itself is corrupted: its own CRC32 does not match.
    PatchChecksum { expected: u32, actual: u32 },
    /// The patched image does not match the target the patch author recorded.
    TargetChecksum { expected: u32, actual: u32 },
    /// A record or action addresses bytes outside the image it reads or writes.
    OutOfRange(&'static str),
    /// Bytes are left over where the format says the patch ends.
    TrailingData(usize),
    /// Reading the patch file failed; the message carries the OS reason.
    Io(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::NotAPatch => write!(f, "not a patch file (no PATCH/BPS1/UPS1 signature)"),
            PatchError::Unsupported(format) => {
                write!(f, "{format} patches are not supported")
            }
            PatchError::Truncated(what) => write!(f, "patch ends in the middle of {what}"),
            PatchError::BadVarint => write!(f, "patch has a malformed variable-length number"),
            PatchError::SourceSize { expected, actual } => {
                write!(f, "ROM is {actual} bytes but the patch expects {expected}")
            }
            PatchError::SourceChecksum { expected, actual } => write!(
                f,
                "ROM CRC32 is {actual:08X} but the patch expects {expected:08X} \
                 (wrong ROM revision, or already patched)"
            ),
            PatchError::PatchChecksum { expected, actual } => write!(
                f,
                "patch CRC32 is {actual:08X} but the file records {expected:08X} \
                 (corrupted or truncated patch)"
            ),
            PatchError::TargetChecksum { expected, actual } => write!(
                f,
                "patched ROM CRC32 is {actual:08X} but the patch expects {expected:08X} \
                 (patch did not apply cleanly)"
            ),
            PatchError::OutOfRange(what) => write!(f, "patch addresses outside the image: {what}"),
            PatchError::TrailingData(count) => write!(f, "{count} bytes left after the patch ends"),
            PatchError::Io(message) => write!(f, "cannot read patch: {message}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Apply whichever patch format `patch` starts with (`PATCH` -> IPS, `BPS1` -> BPS, `UPS1` ->
/// UPS). Unknown data is rejected, never guessed at.
pub fn patch(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if patch.starts_with(IPS_MAGIC) {
        ips_patch(rom, patch)
    } else if patch.starts_with(BPS_MAGIC) {
        bps_patch(rom, patch)
    } else if patch.starts_with(UPS_MAGIC) {
        ups_patch(rom, patch)
    } else {
        Err(PatchError::NotAPatch)
    }
}

/// The patch file that sits next to `rom`: `<stem>.ips`, `<stem>.bps` or `<stem>.ups` in the
/// same folder. When more than one exists the first of those three wins.
pub fn beside(rom: &Path) -> Option<PathBuf> {
    PATCH_EXTENSIONS
        .iter()
        .map(|extension| rom.with_extension(extension))
        .find(|path| path.is_file())
}

/// Read the patch at `path` and apply it to `rom`. The only filesystem entry point next to
/// [`beside`]; the decoders themselves stay byte-slice pure.
pub fn apply_path(rom: &[u8], path: &Path) -> Result<Vec<u8>, PatchError> {
    let bytes =
        std::fs::read(path).map_err(|e| PatchError::Io(format!("{}: {e}", path.display())))?;
    patch(rom, &bytes)
}

/// Apply an IPS patch (the classic `PATCH ... EOF` format) to `rom`.
///
/// Records are applied in file order, so a later record wins where two overlap. A record that
/// reaches past the end of the image extends it with zero bytes - the standard behaviour for ROM
/// hacks that add banks or data.
pub fn ips_patch(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(IPS_MAGIC) {
        return Err(PatchError::NotAPatch);
    }

    let mut out = rom.to_vec();
    let mut pos = IPS_MAGIC.len();
    // The optional 3-byte truncation length that may follow `EOF` (Mesen2 reads it, so rips that
    // rely on it must work here too).
    let mut truncate: Option<usize> = None;

    loop {
        let rest = &patch[pos..];
        if rest.is_empty() {
            return Err(PatchError::Truncated("the IPS record list (no EOF marker)"));
        }
        // IPS32 shares the `PATCH` magic but ends with `EEOF` and 32-bit offsets.
        if rest.starts_with(b"EEOF") {
            return Err(PatchError::Unsupported("IPS32"));
        }
        if rest.starts_with(IPS_EOF) {
            pos += IPS_EOF.len();
            let tail = &patch[pos..];
            if !tail.is_empty() {
                // Some rips append exactly one 3-byte truncation length; anything else after
                // EOF is junk.
                if tail.len() != 3 {
                    return Err(PatchError::TrailingData(tail.len()));
                }
                truncate = Some(be24(tail));
                pos += 3;
            }
            break;
        }

        let header = patch
            .get(pos..pos + 5)
            .ok_or(PatchError::Truncated("an IPS record header"))?;
        let offset = be24(&header[..3]);
        let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
        pos += 5;

        if length == 0 {
            // RLE record: a 16-bit repeat count and the byte to repeat.
            let rle = patch
                .get(pos..pos + 3)
                .ok_or(PatchError::Truncated("an IPS RLE record"))?;
            let count = usize::from(u16::from_be_bytes([rle[0], rle[1]]));
            let value = rle[2];
            pos += 3;
            let end = offset
                .checked_add(count)
                .ok_or(PatchError::OutOfRange("IPS RLE record"))?;
            if out.len() < end {
                out.resize(end, 0);
            }
            out[offset..end].fill(value);
        } else {
            let bytes = patch
                .get(pos..pos + length)
                .ok_or(PatchError::Truncated("an IPS record's data"))?;
            pos += length;
            let end = offset + length;
            if out.len() < end {
                out.resize(end, 0);
            }
            out[offset..end].copy_from_slice(bytes);
        }
    }

    if pos != patch.len() {
        return Err(PatchError::TrailingData(patch.len() - pos));
    }
    // Mesen2 only truncates when the extension actually shortens the image.
    if let Some(length) = truncate
        && length < out.len()
    {
        out.truncate(length);
    }
    Ok(out)
}

/// Apply a BPS patch (byuu's BPS v1) to `rom`.
///
/// The footer's three CRC32 values are checked in this order: the patch itself (catches a
/// corrupted download before it can do anything), the source ROM (catches the wrong revision),
/// and finally the produced image. The source size is checked too, since `BPS1` stores it
/// explicitly.
pub fn bps_patch(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(BPS_MAGIC) {
        return Err(PatchError::NotAPatch);
    }
    if patch.len() < BPS_MAGIC.len() + BPS_FOOTER {
        return Err(PatchError::Truncated("the BPS header"));
    }

    // The last four bytes are the CRC32 of every byte before them.
    let recorded_patch_crc = be32(&patch[patch.len() - 4..]);
    let patch_crc = crc32(&patch[..patch.len() - 4]);
    if recorded_patch_crc != patch_crc {
        return Err(PatchError::PatchChecksum {
            expected: recorded_patch_crc,
            actual: patch_crc,
        });
    }

    let footer = &patch[patch.len() - BPS_FOOTER..];
    let recorded_source_crc = be32(&footer[..4]);
    let recorded_target_crc = be32(&footer[4..8]);

    // Everything between the magic and the footer is header numbers, metadata (skipped) and
    // actions.
    let mut body = Reader::new(&patch[BPS_MAGIC.len()..patch.len() - BPS_FOOTER]);
    let source_size = to_usize(body.varint()?)?;
    let target_size = to_usize(body.varint()?)?;
    let metadata_size = to_usize(body.varint()?)?;
    body.take(metadata_size)
        .map_err(|_| PatchError::Truncated("the BPS metadata"))?;

    if source_size != rom.len() {
        return Err(PatchError::SourceSize {
            expected: source_size,
            actual: rom.len(),
        });
    }
    let source_crc = crc32(rom);
    if recorded_source_crc != source_crc {
        return Err(PatchError::SourceChecksum {
            expected: recorded_source_crc,
            actual: source_crc,
        });
    }

    // The target size is an untrusted varint. Reserve only as much as a real patch can want - a
    // target is the source's size times a small factor - so a CRC-valid patch that declares 2^40
    // bytes is refused by the action walk below instead of aborting inside the allocator. The
    // vector grows past this if a patch really is bigger; `end > target_size` still bounds it.
    let reservation = target_size.min(rom.len().saturating_mul(4).max(1 << 20));
    let mut out: Vec<u8> = Vec::with_capacity(reservation);
    // Copy offsets are relative and signed; each action moves them from where the previous action
    // of the same kind left off (BPS spec, SourceCopy / TargetCopy).
    let mut source_offset: i64 = 0;
    let mut target_offset: i64 = 0;

    while out.len() < target_size {
        let start = out.len();
        let data = body.varint()?;
        let action = (data & 0x03) as u8;
        // The length is stored minus one, above the two-bit action field.
        let length = to_usize((data >> 2) + 1)?;
        let end = start
            .checked_add(length)
            .ok_or(PatchError::OutOfRange("a BPS action"))?;
        if end > target_size {
            return Err(PatchError::OutOfRange(
                "a BPS action that passes the target size",
            ));
        }

        match action {
            // SourceRead: copy the same positions from the source image.
            0 => {
                if end > rom.len() {
                    return Err(PatchError::OutOfRange(
                        "a BPS SourceRead past the end of the source",
                    ));
                }
                out.extend_from_slice(&rom[start..end]);
            }
            // TargetRead: literal bytes stored in the patch.
            1 => {
                let bytes = body.take(length)?;
                out.extend_from_slice(bytes);
            }
            // SourceCopy: seek in the source, then copy forwards.
            2 => {
                source_offset = step_offset(source_offset, body.varint()?)?;
                for _ in 0..length {
                    let index = offset_usize(source_offset)?;
                    let byte = *rom.get(index).ok_or(PatchError::OutOfRange(
                        "a BPS SourceCopy past the end of the source",
                    ))?;
                    out.push(byte);
                    source_offset += 1;
                }
            }
            // TargetCopy: seek in what has already been produced. Reading one byte at a time is
            // what lets a TargetCopy overlap its own output (the BPS replacement for RLE).
            3 => {
                target_offset = step_offset(target_offset, body.varint()?)?;
                for _ in 0..length {
                    let index = offset_usize(target_offset)?;
                    let byte = *out.get(index).ok_or(PatchError::OutOfRange(
                        "a BPS TargetCopy before the written target",
                    ))?;
                    out.push(byte);
                    target_offset += 1;
                }
            }
            _ => unreachable!("action is two bits"),
        }
    }

    if body.remaining() != 0 {
        return Err(PatchError::TrailingData(body.remaining()));
    }
    let target_crc = crc32(&out);
    if recorded_target_crc != target_crc {
        return Err(PatchError::TargetChecksum {
            expected: recorded_target_crc,
            actual: target_crc,
        });
    }
    Ok(out)
}

/// Apply a UPS patch (byuu's UPS v1) to `rom`.
///
/// The footer's three CRC32 values are checked in the same order as BPS: the patch itself, the
/// input ROM, then the produced image. Applying the same patch twice restores the input, which
/// is what the XOR delta is for.
pub fn ups_patch(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    if !patch.starts_with(UPS_MAGIC) {
        return Err(PatchError::NotAPatch);
    }
    if patch.len() < UPS_MAGIC.len() + UPS_FOOTER {
        return Err(PatchError::Truncated("the UPS header"));
    }

    // Footer order is input CRC, output CRC, patch CRC, each little-endian.
    let recorded_patch_crc = le32(&patch[patch.len() - 4..]);
    let patch_crc = crc32(&patch[..patch.len() - 4]);
    if recorded_patch_crc != patch_crc {
        return Err(PatchError::PatchChecksum {
            expected: recorded_patch_crc,
            actual: patch_crc,
        });
    }

    let footer = &patch[patch.len() - UPS_FOOTER..];
    let recorded_source_crc = le32(&footer[..4]);
    let recorded_target_crc = le32(&footer[4..8]);

    let mut body = Reader::new(&patch[UPS_MAGIC.len()..patch.len() - UPS_FOOTER]);
    let source_size = to_usize(body.varint()?)?;
    let target_size = to_usize(body.varint()?)?;
    if source_size != rom.len() {
        return Err(PatchError::SourceSize {
            expected: source_size,
            actual: rom.len(),
        });
    }
    let source_crc = crc32(rom);
    if recorded_source_crc != source_crc {
        return Err(PatchError::SourceChecksum {
            expected: recorded_source_crc,
            actual: source_crc,
        });
    }
    if target_size > UPS_MAX_TARGET {
        return Err(PatchError::OutOfRange("a UPS target larger than 128 MiB"));
    }

    // The output starts as the input, cut to the target size; bytes past the input's end are
    // zero. Growing lazily keeps a hostile declared size from allocating before a block is read.
    let mut out = rom.to_vec();
    out.truncate(target_size);
    let mut pos: usize = 0;
    while body.remaining() > 0 {
        let skip = to_usize(body.varint()?)?;
        pos = pos
            .checked_add(skip)
            .ok_or(PatchError::OutOfRange("a UPS block offset"))?;
        loop {
            let byte = body.take(1)?[0];
            if byte == 0 {
                // The terminator counts against the pointer but is left as it was.
                pos = pos
                    .checked_add(1)
                    .ok_or(PatchError::OutOfRange("a UPS block offset"))?;
                break;
            }
            if pos >= target_size {
                return Err(PatchError::OutOfRange("a UPS XOR block past the target"));
            }
            if pos >= out.len() {
                out.resize(pos + 1, 0);
            }
            out[pos] ^= byte;
            pos += 1;
        }
    }
    out.resize(target_size, 0);

    let target_crc = crc32(&out);
    if recorded_target_crc != target_crc {
        return Err(PatchError::TargetChecksum {
            expected: recorded_target_crc,
            actual: target_crc,
        });
    }
    Ok(out)
}

/// CRC32 (the reflected `0xEDB88320` polynomial) as the BPS and UPS footers define it; the check
/// value is `crc32("123456789") = 0xCBF43926`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Three big-endian bytes as an IPS offset or truncation length.
fn be24(bytes: &[u8]) -> usize {
    (usize::from(bytes[0]) << 16) | (usize::from(bytes[1]) << 8) | usize::from(bytes[2])
}

/// Four big-endian bytes as a CRC32 (the BPS footer order).
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Four little-endian bytes as a CRC32 (the UPS footer order).
fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn to_usize(value: u64) -> Result<usize, PatchError> {
    usize::try_from(value).map_err(|_| PatchError::OutOfRange("a patch size or length"))
}

/// A signed copy offset as an index; negative values are out of range by definition, so they map
/// to the same error a copy before the start would.
fn offset_usize(value: i64) -> Result<usize, PatchError> {
    usize::try_from(value).map_err(|_| PatchError::OutOfRange("a negative BPS copy offset"))
}

/// Apply one BPS relative-offset number: the low bit is the direction, the rest is the distance
/// (`offset += (negative ? -1 : +1) * (data >> 1)`).
fn step_offset(current: i64, code: u64) -> Result<i64, PatchError> {
    let distance = i64::try_from(code >> 1).map_err(|_| PatchError::BadVarint)?;
    let delta = if code & 1 != 0 { -distance } else { distance };
    current
        .checked_add(delta)
        .ok_or(PatchError::OutOfRange("a BPS copy offset"))
}

/// A cursor over the patch body that refuses to read past its end.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PatchError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(PatchError::Truncated("a patch block"))?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or(PatchError::Truncated("a patch block"))?;
        self.pos = end;
        Ok(bytes)
    }

    /// The BPS/UPS variable-length number: seven bits per byte, the high bit marks the last byte,
    /// and one is subtracted after every continuation byte so each value has exactly one encoding.
    fn varint(&mut self) -> Result<u64, PatchError> {
        let mut data: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or(PatchError::Truncated("a patch number"))?;
            self.pos += 1;
            let part = u64::from(byte & 0x7F)
                .checked_shl(shift)
                .ok_or(PatchError::BadVarint)?;
            data = data.checked_add(part).ok_or(PatchError::BadVarint)?;
            if byte & 0x80 != 0 {
                return Ok(data);
            }
            shift += 7;
            data = data
                .checked_add(1u64.checked_shl(shift).ok_or(PatchError::BadVarint)?)
                .ok_or(PatchError::BadVarint)?;
        }
    }
}
