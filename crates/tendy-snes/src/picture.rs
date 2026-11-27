//! BGR555 → RGBA: the expansion, the 5-bit ramp and the row stride.
//!
//! The one place the picture becomes pixels, shared by the window, `screenshot.rs` and `thumb.rs`.
//! A SNES produces RGB — the PPU hands out 15-bit words and the colours are the cartridge's — so
//! there is no palette table here and no `palette.rs` in the crate.

/// Bytes one pixel takes in the RGBA buffer.
pub const BYTES_PER_PIXEL: usize = 4;

/// Expands `words` into `out` as RGBA8888, `(v << 3) | (v >> 2)` per channel and `0xFF` for alpha.
///
/// `(v << 3) | (v >> 2)` is the standard 5-bit to 8-bit expansion: 0 maps to 0, 31 to 255 and the
/// ramp between is even. A CGRAM word is `0bbbbbgg gggrrrrr` (fullsnes, "CGRAM: 15 bit RGB"), so
/// red is the low five bits. An `out` shorter than the picture is filled as far as it goes and an
/// `out` longer than the picture is left alone past its last word — the caller sizes the buffer,
/// as §10.6's window loop does.
pub fn bgr555_to_rgba(words: &[u16], out: &mut [u8]) {
    let count = (out.len() / BYTES_PER_PIXEL).min(words.len());
    for (i, &word) in words.iter().enumerate().take(count) {
        let r = (word & 0x1F) as u8;
        let g = ((word >> 5) & 0x1F) as u8;
        let b = ((word >> 10) & 0x1F) as u8;
        let at = i * BYTES_PER_PIXEL;
        out[at] = (r << 3) | (r >> 2);
        out[at + 1] = (g << 3) | (g >> 2);
        out[at + 2] = (b << 3) | (b >> 2);
        out[at + 3] = 0xFF;
    }
}

/// Bytes one 256x224 frame takes as RGBA: 229 376 (§10.2's table).
pub fn rgba_len() -> usize {
    crate::FRAME_PIXELS * BYTES_PER_PIXEL
}
