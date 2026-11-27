//! PNG screenshots (DESIGN §6.1, §6.3): a frame written to
//! `<system>/screenshots/<stem>-<n>.png`, the name's stem and number never overwriting an earlier
//! shot.
//!
//! The PNG is written by hand so the crate list gains nothing: IDAT is a zlib stream (RFC 1950:
//! two header bytes, the stored blocks, then adler32) built only from stored deflate blocks
//! (RFC 1951), so the file is a valid PNG any decoder opens. The size always comes from the
//! machine's own frame, never from a constant here.

use std::path::{Path, PathBuf};

use crate::app::App;
use crate::menu::{Row, RowAct, RowKind};

/// The largest payload one stored deflate block can carry: its 16-bit length field.
const STORED_BLOCK_MAX: usize = 0xFFFF;

/// The 8 bytes every PNG file starts with (the signature chunk).
const PNG_SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// Reflected IEEE polynomial, the one PNG chunks use.
const CRC_POLY: u32 = 0xEDB8_8320;

/// CRC-32 (IEEE) of `bytes`.
fn crc32(bytes: &[u8]) -> u32 {
    crc32_continue(0, bytes)
}

/// Continue a CRC-32 over more bytes; `crc` is the running value as [`crc32`] returned it.
fn crc32_continue(crc: u32, bytes: &[u8]) -> u32 {
    let mut state = crc ^ 0xFFFF_FFFF;
    for &byte in bytes {
        state ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (state & 1).wrapping_neg();
            state = (state >> 1) ^ (CRC_POLY & mask);
        }
    }
    state ^ 0xFFFF_FFFF
}

/// Adler32 for the zlib trailer, over the same bytes IDAT deflates.
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b): (u32, u32) = (1, 0);
    for &v in bytes {
        a = (a + u32::from(v)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Append one PNG chunk: 4-byte length, 4-byte type, data, then the CRC32 of type and data.
fn append_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(kind);
    out.extend(data);
    out.extend(crc32_continue(crc32(&kind), data).to_be_bytes());
}

/// The frame as PNG scanlines: every row prefixed with filter byte 0 (None).
fn scanlines(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    let mut rows = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks(stride) {
        rows.push(0);
        rows.extend_from_slice(row);
    }
    rows
}

/// PNG file bytes for an RGBA frame: 4 bytes per pixel, row-major, `width` x `height`. IHDR
/// declares 8-bit colour type 6 (RGBA); IDAT is a zlib stream of stored deflate blocks.
pub fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "the RGBA buffer must be width x height x 4 bytes"
    );
    let rows = scanlines(width, height, rgba);
    let mut idat = Vec::with_capacity(rows.len() + 46);
    idat.extend([0x78, 0x01]); // zlib header: 32 KiB window, no flags
    if rows.is_empty() {
        idat.extend([0x01, 0x00, 0x00, 0xFF, 0xFF]); // one empty final stored block
    }
    let blocks = rows.len().div_ceil(STORED_BLOCK_MAX);
    for block in 0..blocks {
        let start = block * STORED_BLOCK_MAX;
        let end = (start + STORED_BLOCK_MAX).min(rows.len());
        idat.push(if end == rows.len() { 0x01 } else { 0x00 }); // stored block, final flag
        let length = (end - start) as u16;
        idat.extend(length.to_le_bytes());
        idat.extend((length ^ 0xFF_FF).to_le_bytes()); // one's-complement check length
        idat.extend_from_slice(&rows[start..end]);
    }
    idat.extend(adler32(&rows).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8-bit RGBA, then default compression/filter/interlace

    let mut png = PNG_SIGNATURE.to_vec();
    append_chunk(&mut png, *b"IHDR", &ihdr);
    append_chunk(&mut png, *b"IDAT", &idat);
    append_chunk(&mut png, *b"IEND", &[]);
    png
}

/// The ROM's stem, for the file name.
fn stem(rom: &Path) -> String {
    rom.file_stem().map_or_else(
        || "screenshot".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

/// `<dir>/<rom stem>-<n>.png`.
pub fn screenshot_path(dir: &Path, rom: &Path, n: u32) -> PathBuf {
    dir.join(format!("{}-{n}.png", stem(rom)))
}

/// The next free index for `rom` in `dir`: one past the highest `-n.png`, so a session's
/// screenshots never overwrite each other.
pub fn next_index(rom: &Path, dir: &Path) -> u32 {
    let prefix = format!("{}-", stem(rom));
    let mut next = 1;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(tail) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(number) = tail.strip_suffix(".png") else {
                continue;
            };
            if let Ok(n) = number.parse::<u32>() {
                next = next.max(n + 1);
            }
        }
    }
    next
}

/// Write `rgba` to `path` as a PNG, creating the parent directory on demand. A failure names the
/// path.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|why| format!("cannot create {}: {why}", dir.display()))?;
    }
    std::fs::write(path, png_rgba(width, height, rgba))
        .map_err(|why| format!("cannot write {}: {why}", path.display()))
}

/// Write the running game's current picture as its next screenshot, under the system's folder.
pub fn save(app: &mut App) -> Result<PathBuf, String> {
    let system = app.system().ok_or("no game is running")?;
    let rom = app
        .rom()
        .map(Path::to_path_buf)
        .ok_or("the game was not opened from a file, so it has no screenshot name")?;
    let dir = app.paths().screenshots_dir(system);
    let path = screenshot_path(&dir, &rom, next_index(&rom, &dir));
    let machine = app.machine_mut().ok_or("no game is running")?;
    let frame = machine.frame();
    write_png(&path, frame.width, frame.height, frame.rgba)?;
    Ok(path)
}

/// The screenshot row.
pub fn rows(app: &App) -> Vec<Row> {
    if app.machine().is_none() {
        return Vec::new();
    }
    vec![Row {
        label: "Take screenshot".to_string(),
        value: String::new(),
        kind: RowKind::Action,
        desc: "Write the picture as a PNG",
        hotkey: "",
        requires: None,
        act: None,
    }]
}

/// Enter on the row writes one PNG and says where it went.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    if index != 0 || act != RowAct::Activate {
        return false;
    }
    match save(app) {
        Ok(path) => {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            app.show_notice(format!("screenshot saved {name}"), false);
        }
        Err(why) => app.show_notice(format!("screenshot not saved: {why}"), true),
    }
    true
}

/// A screenshot is a command, not a running capture: there is no per-frame state.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder under the system temp dir, removed at the end.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tendyemu-screenshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_png_starts_with_the_signature_and_ends_with_iend() {
        let rgba = [0u8; 4 * 4 * 4];
        let png = png_rgba(4, 4, &rgba);
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 4);
    }

    #[test]
    fn a_full_frame_becomes_a_four_block_png() {
        // 160x144 RGBA is 92 236 bytes of scanlines, so two stored blocks.
        let frame = vec![0x9Bu8; 160 * 144 * 4];
        let png = png_rgba(160, 144, &frame);
        assert!(png.len() > 160 * 144 * 4);
    }

    #[test]
    fn the_name_is_the_stem_and_the_index() {
        let path = screenshot_path(Path::new("roms"), Path::new("Tetris.gb"), 3);
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "Tetris-3.png");
    }

    #[test]
    fn write_png_creates_the_folder_and_writes_the_file() {
        let dir = scratch("write");
        let path = dir.join("nested/frame.png");
        let rgba = vec![0xFFu8; 4 * 4 * 4];
        write_png(&path, 4, 4, &rgba).expect("the nested folder is created");
        let written = std::fs::read(&path).expect("the PNG is written");
        assert_eq!(&written[..8], &PNG_SIGNATURE);
        assert_eq!(
            u32::from_be_bytes([written[16], written[17], written[18], written[19]]),
            4
        );
        assert_eq!(
            u32::from_be_bytes([written[20], written[21], written[22], written[23]]),
            4
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
