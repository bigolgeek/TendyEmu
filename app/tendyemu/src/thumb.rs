//! Slot thumbnails (DESIGN §6.1, §6.3). #22 owns this file: the PNG beside each state file,
//! written from the frame the state was taken on.
//!
//! A slot was a blind guess — `Slot 3` and nothing else. The picture the machine already holds is
//! written beside the state when the slot is saved, and read back for the drawer to draw in the
//! slot's row. The file is a PNG through the workspace's `png` dependency, so it opens in any
//! image viewer and costs no hand-written encoder.
//!
//! Reading is deliberately all-or-nothing: a thumbnail that was never written, was deleted, was
//! cut short by a crash or was not written by this build decodes to `None` and the drawer draws
//! nothing at all. A broken picture must never be a crash or a notice.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;

use crate::app::App;
use crate::menu::{Row, RowAct};

/// One decoded thumbnail: the frame as RGBA8888, 4 bytes per pixel, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The thumbnail that belongs beside `state`, named as its source app names it (§6.3): GB and GBA
/// use `<stem>-<slot>.thumb.png`, NES and SNES the state's own name with `.png` for `.state`.
pub fn thumb_path(state: &Path, system: SystemKind) -> PathBuf {
    match system {
        SystemKind::Gb | SystemKind::Gba => state.with_extension("thumb.png"),
        SystemKind::Nes | SystemKind::Snes => state.with_extension("png"),
    }
}

/// The picture beside `state`, or `None` when there is nothing to draw. Every failure — no file, a
/// directory where the file should be, a truncated or foreign PNG — is the same answer.
pub fn picture(state: &Path, system: SystemKind) -> Option<Image> {
    let bytes = std::fs::read(thumb_path(state, system)).ok()?;
    decode(&bytes)
}

/// Writes `rgba` beside `state`. The error names the path and the reason, so a caller can put it in
/// front of the player unchanged.
pub fn write(
    state: &Path,
    system: SystemKind,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<PathBuf, String> {
    let path = thumb_path(state, system);
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(format!(
            "cannot write {}: the picture is {} bytes, not {expected}",
            path.display(),
            rgba.len()
        ));
    }
    let bytes = encode(width, height, rgba)
        .map_err(|why| format!("cannot write {}: {why}", path.display()))?;
    std::fs::write(&path, bytes)
        .map_err(|why| format!("cannot write {}: {why}", path.display()))?;
    Ok(path)
}

/// A PNG of `rgba`. The encoder writes into memory first, so a failed write never leaves half a
/// file behind.
fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(rgba)?;
    }
    Ok(out)
}

/// The picture in `bytes`, or `None` when they are not an 8-bit RGBA PNG.
fn decode(bytes: &[u8]) -> Option<Image> {
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let mut rgba = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut rgba).ok()?;
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    rgba.truncate(frame.buffer_size());
    Some(Image {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

/// No rows: a thumbnail is drawn inside a slot row, not on one of its own.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything: the slot rows belong to `states.rs` and ask for the picture.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to capture: the picture is written when a state is saved.
pub fn on_frame(_app: &mut App) {}
