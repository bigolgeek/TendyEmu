use std::path::Path;
use std::sync::LazyLock;

/// An RGB palette: one `(r, g, b)` per entry, in the order the source gives them — 64 for a
/// 64-colour .pal or the built-in table, 512 for `palette_512` and `rgb_table`.
pub type PalTable = Vec<(u8, u8, u8)>;

/// A 512-entry RGBA colour table, indexed by `(emph << 6) | colour`: one frame pixel's four bytes,
/// already packed, ready to copy.
pub type RgbaTable = [[u8; 4]; 512];

/// The standard 2C02 NTSC palette: RGB for each of the 64 colour indices, 16 per row.
pub const NES_PALETTE: [(u8, u8, u8); 64] = [
    (84, 84, 84),
    (0, 30, 116),
    (8, 16, 144),
    (48, 0, 136),
    (68, 0, 100),
    (92, 0, 48),
    (84, 4, 0),
    (60, 24, 0),
    (32, 42, 0),
    (8, 58, 0),
    (0, 64, 0),
    (0, 60, 0),
    (0, 50, 60),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (152, 150, 152),
    (8, 76, 196),
    (48, 50, 236),
    (92, 30, 228),
    (136, 20, 176),
    (160, 20, 100),
    (152, 34, 32),
    (120, 60, 0),
    (84, 90, 0),
    (40, 114, 0),
    (8, 124, 0),
    (0, 118, 40),
    (0, 102, 120),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (236, 238, 236),
    (76, 154, 236),
    (120, 124, 236),
    (176, 98, 236),
    (228, 84, 236),
    (236, 88, 180),
    (236, 106, 100),
    (212, 136, 32),
    (160, 170, 0),
    (116, 196, 0),
    (76, 208, 32),
    (56, 204, 108),
    (56, 180, 204),
    (60, 60, 60),
    (0, 0, 0),
    (0, 0, 0),
    (236, 238, 236),
    (168, 204, 236),
    (188, 188, 236),
    (212, 178, 236),
    (236, 174, 236),
    (236, 174, 212),
    (236, 180, 176),
    (228, 196, 144),
    (204, 210, 120),
    (180, 222, 120),
    (168, 226, 144),
    (152, 226, 180),
    (160, 214, 228),
    (160, 162, 160),
    (0, 0, 0),
    (0, 0, 0),
];

/// Emphasis attenuation: a set PPUMASK emphasis bit scales the two other channels by 209/256 (~0.816).
pub const EMPH_NUM: u32 = 209;

/// Attenuate one colour by emphasis bits `emph` = PPUMASK >> 5 (bit 0 red,
/// bit 1 green, bit 2 blue): a set bit dims the two channels it does not name
/// by 209/256 (~0.816); columns $xE/$xF of index `c` stay as authored.
pub fn emphasise(rgb: (u8, u8, u8), c: u8, emph: u8) -> (u8, u8, u8) {
    if c & 0x0E == 0x0E {
        return rgb;
    }
    let (mut r, mut g, mut b) = rgb;
    let dim = |v: u8| (u32::from(v) * EMPH_NUM / 256) as u8;
    if emph & 1 != 0 {
        g = dim(g);
        b = dim(b);
    }
    if emph & 2 != 0 {
        r = dim(r);
        b = dim(b);
    }
    if emph & 4 != 0 {
        r = dim(r);
        g = dim(g);
    }
    (r, g, b)
}

/// RGB for palette index `c` (0..=63) under emphasis bits `emph` = PPUMASK >> 5
/// (bit 0 red, bit 1 green, bit 2 blue), from the built-in palette.
pub fn emphasized_rgb(c: u8, emph: u8) -> (u8, u8, u8) {
    emphasise(NES_PALETTE[usize::from(c & 0x3F)], c, emph)
}

/// 512-entry emphasis expansion of 64 base colours: index = (emph << 6) | c.
pub fn emphasis_table(base: &[(u8, u8, u8)]) -> PalTable {
    (0..512u16)
        .map(|i| {
            emphasise(
                base[usize::from(i & 0x3F)],
                (i & 0x3F) as u8,
                (i >> 6) as u8,
            )
        })
        .collect()
}

/// 512-entry palette: index = (emph << 6) | c.
pub fn palette_512() -> PalTable {
    emphasis_table(NES_PALETTE.as_slice())
}

/// Pack an RGB table (`palette_512`, `rgb_table`, …) into the RGBA form `to_rgba_pal` copies:
/// alpha 255, and any entry the source leaves out stays opaque black.
pub fn rgba_table(rgb: &[(u8, u8, u8)]) -> RgbaTable {
    let mut out = [[0, 0, 0, 255]; 512];
    for (px, &(r, g, b)) in out.iter_mut().zip(rgb) {
        *px = [r, g, b, 255];
    }
    out
}

/// The built-in palette packed for `to_rgba_emph`; built on first use.
static BUILTIN_RGBA: LazyLock<RgbaTable> = LazyLock::new(|| rgba_table(palette_512().as_slice()));

/// The built-in palette (every emphasis state) as a packed [`RgbaTable`].
pub fn builtin_rgba_table() -> &'static RgbaTable {
    &BUILTIN_RGBA
}

/// Convert a frame of palette indices (0..=63) to RGBA bytes (4 per pixel, alpha 255) under a
/// per-pixel emphasis buffer (same length as `frame`), through the built-in packed table: the
/// emphasis maths is pre-computed, so each pixel is one 4-byte copy.
pub fn to_rgba_emph(frame: &[u8], emph: &[u8], out: &mut [u8]) {
    to_rgba_pal(frame, emph, builtin_rgba_table(), out);
}

/// Like `to_rgba_emph`, reading a pre-built packed `table` (index = (emphasis << 6) | colour), so a
/// loaded .pal can override the default colours. `out` must be 4 times as long as `frame`.
///
/// The table's fixed 512 entries prove the index in range, so the loop carries no bounds check and
/// writes each pixel with one 4-byte copy.
pub fn to_rgba_pal(frame: &[u8], emph: &[u8], table: &RgbaTable, out: &mut [u8]) {
    for ((px, &c), &e) in out.as_chunks_mut::<4>().0.iter_mut().zip(frame).zip(emph) {
        *px = table[usize::from(e & 7) << 6 | usize::from(c & 0x3F)];
    }
}

/// Byte counts a .pal file may hold: 64 colours x 3 bytes and 512 colours x 3.
pub const PAL_BYTES_64: usize = 64 * 3;
pub const PAL_BYTES_512: usize = 512 * 3;

/// Split a .pal body into RGB triplets (3 bytes each, red first, then green,
/// then blue); the length must be a multiple of 3.
pub fn pal_triplets(bytes: &[u8]) -> PalTable {
    bytes
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| (t[0], t[1], t[2]))
        .collect()
}

/// Parse the bytes of a .pal file: 192 bytes (64 colours) or 1536 bytes
/// (512 colours, the 8 emphasis states of 64 colours) — the FCEUX format, raw
/// RGB triplets. Any other size is rejected with the reason the caller can
/// print, so the built-in palette stays in use.
pub fn parse_pal(bytes: &[u8]) -> Result<PalTable, String> {
    match bytes.len() {
        PAL_BYTES_64 | PAL_BYTES_512 => Ok(pal_triplets(bytes)),
        other => Err(format!(
            "expected {PAL_BYTES_64} (64 colours) or {PAL_BYTES_512} (512 colours) bytes, got {other}"
        )),
    }
}

/// The 512-entry RGB table a .pal implies, as `emphasis_table` orders it: a
/// 512-colour file supplies every emphasis state verbatim; a 64-colour file
/// gets the emphasis variants from the emulator's attenuation rule.
pub fn rgb_table(entries: &[(u8, u8, u8)]) -> PalTable {
    match entries.len() {
        512 => entries.to_vec(),
        _ => emphasis_table(entries),
    }
}

/// Read and parse a .pal file (see `parse_pal`); the error names the file and
/// the reason, so the caller can print it and keep the palette in use.
pub fn load_pal_file(path: &Path) -> Result<PalTable, String> {
    std::fs::read(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
        .and_then(|bytes| {
            parse_pal(bytes.as_slice()).map_err(|reason| format!("{}: {reason}", path.display()))
        })
}

/// Load `path` and return the 512-entry RGB table plus a phrase describing the
/// file; the error says why the file was unusable.
pub fn load_pal_table(path: &Path) -> Result<(PalTable, String), String> {
    load_pal_file(path).map(|entries| {
        let colours = entries.len();
        (
            rgb_table(entries.as_slice()),
            format!("{} ({} colours)", path.display(), colours),
        )
    })
}

/// The 64-entry palette generated from the NTSC signal model — Bisqwit's
/// derivation from the nesdev wiki "NTSC video" page
/// (https://www.nesdev.org/wiki/NTSC_video), ported from the pseudocode there,
/// not copied from any GPL source. Each colour index is read as a 12-spot
/// burst: signal level `hi` on the six "on" spots, `lo` on the six "off"
/// spots; averaging the burst over the 12 sub-carrier phases (p in 0..12, at
/// angle `PI*p/6`) gives the luma `y` and the chroma components `i` (in-phase)
/// and `q` (quadrature). The YIQ channels are converted to RGB and put through
/// the display gamma fix `x^(2.2/1.8)` before scaling to 8 bits.
pub fn ntsc_palette() -> PalTable {
    // Signal levels per brightness level: [0..3] = low, [4..7] = high
    // (indexed by level + 4), per the wiki derivation.
    const LEVELS: [f64; 8] = [0.350, 0.518, 0.962, 1.550, 1.094, 1.506, 1.962, 1.962];
    const BLACK: f64 = 0.518;
    const WHITE: f64 = 1.962;
    let mut out = Vec::with_capacity(64);
    for c in 0..64u8 {
        let color = c & 0x0F;
        let level = if color < 0x0E { (c >> 4) & 3 } else { 1 };
        let lo = LEVELS[usize::from(level + if color == 0 { 4 } else { 0 })];
        let hi = LEVELS[usize::from(level + if color < 0x0D { 4 } else { 0 })];
        let (mut y, mut i, mut q) = (0f64, 0f64, 0f64);
        for p in 0..12u32 {
            let spot = if (u32::from(color) + p + 8) % 12 < 6 {
                hi
            } else {
                lo
            };
            let v = (spot - BLACK) / (WHITE - BLACK) / 12.0;
            y += v;
            i += v * (std::f64::consts::PI * f64::from(p) / 6.0).cos();
            q += v * (std::f64::consts::PI * f64::from(p) / 6.0).sin();
        }
        out.push((
            ntsc_channel(y + 0.946882 * i + 0.623557 * q),
            ntsc_channel(y - 0.274788 * i - 0.635691 * q),
            ntsc_channel(y - 1.108545 * i + 1.709007 * q),
        ));
    }
    out
}

/// One 8-bit channel of an NTSC-generated colour: the gamma fix `x^(2.2/1.8)`
/// (negatives clamp to 0) scaled to 0..=255.
fn ntsc_channel(x: f64) -> u8 {
    let f = if x < 0.0 { 0.0 } else { x.powf(2.2 / 1.8) };
    ((255.0 * f) as i32).clamp(0, 255) as u8
}

// ---------------------------------------------------------------------------
// RGB PPUs: RP2C03, RP2C04-000x and RP2C05
// ---------------------------------------------------------------------------
//
// The 2C03/2C04/2C05 PPUs drive an RGBS output from a 64x9-bit lookup ROM: three
// 3-bit DAC digits per palette entry, red first. Every table below is
// transcribed from the nesdev wiki page "PPU palettes"
// (https://www.nesdev.org/wiki/PPU_palettes), revision 24257
// (13 September 2026), sections "2C03 and 2C05" and "2C04"; the Vs. PPU type
// numbering is from "NES 2.0" (https://www.nesdev.org/wiki/NES_2.0), revision
// 24464, section "Vs. System Type".
//
// Modelled here: `rgb_emphasis_table` builds the 512-entry table for these chips from the DAC
// digits, where each PPUMASK emphasis bit drives its own channel to full brightness instead of
// dimming the other two. The PPUMASK monochrome bit needs no separate path: the renderer ANDs
// the colour index with $30 on every PPU (`render.rs`, the 2C02's own rule), so a 2C04 already
// remaps each row to that row's $0X column, which is what the page says it does.

/// A 64-entry RGB PPU palette as 3-bit DAC digits per channel (red, green, blue).
pub type DacPalette = [[u8; 3]; 64];

/// The RP2C03 / RC2C03 / RP2C05 lookup table: 64 entries of 3-bit DAC digits.
///
/// NES 2.0 Vs. PPU type $0 ("any RP2C03/RC2C03 variant") and types $8-$B
/// (RC2C05-01..04) use it: the 2C05 "has the same colors as the 2C03" (Vs.
/// System page), and both render hue $D as black rather than dark grey. The
/// wiki notes the duplicate colours $0B = $1A (040) and $2B = $3B (276).
pub const RP2C03_DAC: DacPalette = [
    [3, 3, 3],
    [0, 1, 4],
    [0, 0, 6],
    [3, 2, 6],
    [4, 0, 3],
    [5, 0, 3],
    [5, 1, 0],
    [4, 2, 0],
    [3, 2, 0],
    [1, 2, 0],
    [0, 3, 1],
    [0, 4, 0],
    [0, 2, 2],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [5, 5, 5],
    [0, 3, 6],
    [0, 2, 7],
    [4, 0, 7],
    [5, 0, 7],
    [7, 0, 4],
    [7, 0, 0],
    [6, 3, 0],
    [4, 3, 0],
    [1, 4, 0],
    [0, 4, 0],
    [0, 5, 3],
    [0, 4, 4],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [7, 7, 7],
    [3, 5, 7],
    [4, 4, 7],
    [6, 3, 7],
    [7, 0, 7],
    [7, 3, 7],
    [7, 4, 0],
    [7, 5, 0],
    [6, 6, 0],
    [3, 6, 0],
    [0, 7, 0],
    [2, 7, 6],
    [0, 7, 7],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [7, 7, 7],
    [5, 6, 7],
    [6, 5, 7],
    [7, 5, 7],
    [7, 4, 7],
    [7, 5, 5],
    [7, 6, 4],
    [7, 7, 2],
    [7, 7, 3],
    [5, 7, 2],
    [4, 7, 3],
    [2, 7, 6],
    [4, 6, 7],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
];

/// The RP2C04-0001 lookup table (NES 2.0 Vs. PPU type `$2`): 64 entries of
/// 3-bit DAC digits, one of the four copy-protection permutations of the
/// 2C04 master palette.
pub const RP2C04_0001_DAC: DacPalette = [
    [7, 5, 5],
    [6, 3, 7],
    [7, 0, 0],
    [4, 4, 7],
    [0, 4, 4],
    [1, 2, 0],
    [2, 2, 2],
    [7, 0, 4],
    [7, 7, 7],
    [3, 3, 3],
    [7, 5, 0],
    [5, 0, 3],
    [4, 0, 3],
    [6, 6, 0],
    [3, 2, 0],
    [7, 7, 7],
    [3, 5, 7],
    [6, 5, 3],
    [3, 1, 0],
    [3, 6, 0],
    [4, 6, 7],
    [6, 5, 7],
    [7, 6, 4],
    [0, 2, 7],
    [7, 6, 0],
    [2, 7, 6],
    [0, 0, 0],
    [2, 0, 0],
    [6, 6, 6],
    [4, 4, 4],
    [7, 0, 7],
    [0, 1, 4],
    [0, 0, 3],
    [5, 6, 7],
    [7, 5, 7],
    [0, 7, 0],
    [0, 7, 7],
    [0, 2, 2],
    [0, 5, 3],
    [5, 0, 7],
    [0, 0, 0],
    [4, 2, 0],
    [7, 4, 7],
    [5, 1, 0],
    [4, 0, 7],
    [0, 0, 6],
    [7, 4, 0],
    [0, 0, 0],
    [0, 0, 0],
    [1, 4, 0],
    [5, 5, 5],
    [0, 3, 1],
    [5, 7, 2],
    [3, 2, 6],
    [7, 7, 0],
    [6, 3, 0],
    [0, 2, 0],
    [0, 3, 6],
    [0, 4, 0],
    [1, 1, 1],
    [7, 7, 3],
    [7, 3, 7],
    [4, 3, 0],
    [4, 7, 3],
];

/// The RP2C04-0002 lookup table (NES 2.0 Vs. PPU type `$3`): 64 entries of
/// 3-bit DAC digits, one of the four copy-protection permutations of the
/// 2C04 master palette.
pub const RP2C04_0002_DAC: DacPalette = [
    [0, 0, 0],
    [7, 5, 0],
    [4, 3, 0],
    [5, 7, 2],
    [4, 7, 3],
    [7, 3, 7],
    [0, 4, 4],
    [5, 6, 7],
    [7, 0, 0],
    [4, 0, 7],
    [7, 7, 3],
    [7, 4, 7],
    [7, 7, 7],
    [6, 3, 7],
    [4, 6, 7],
    [0, 4, 0],
    [0, 2, 0],
    [3, 5, 7],
    [5, 1, 0],
    [6, 6, 6],
    [0, 5, 3],
    [3, 6, 0],
    [2, 0, 0],
    [4, 4, 7],
    [2, 2, 2],
    [7, 0, 7],
    [0, 0, 3],
    [2, 7, 6],
    [6, 5, 7],
    [3, 2, 0],
    [0, 0, 0],
    [3, 2, 6],
    [4, 0, 3],
    [7, 6, 4],
    [7, 4, 0],
    [7, 5, 7],
    [0, 3, 6],
    [3, 1, 0],
    [5, 5, 5],
    [0, 0, 6],
    [5, 0, 7],
    [7, 6, 0],
    [3, 3, 3],
    [1, 2, 0],
    [0, 2, 7],
    [0, 0, 0],
    [6, 6, 0],
    [7, 7, 7],
    [6, 5, 3],
    [1, 1, 1],
    [0, 7, 0],
    [6, 3, 0],
    [0, 2, 2],
    [0, 1, 4],
    [7, 0, 4],
    [1, 4, 0],
    [0, 0, 0],
    [0, 7, 7],
    [4, 2, 0],
    [7, 7, 0],
    [7, 5, 5],
    [5, 0, 3],
    [0, 3, 1],
    [4, 4, 4],
];

/// The RP2C04-0003 lookup table (NES 2.0 Vs. PPU type `$4`): 64 entries of
/// 3-bit DAC digits, one of the four copy-protection permutations of the
/// 2C04 master palette.
pub const RP2C04_0003_DAC: DacPalette = [
    [5, 0, 7],
    [7, 3, 7],
    [4, 7, 3],
    [5, 5, 5],
    [0, 4, 0],
    [7, 7, 7],
    [5, 6, 7],
    [1, 2, 0],
    [0, 1, 4],
    [0, 0, 0],
    [7, 6, 4],
    [3, 2, 0],
    [7, 0, 4],
    [6, 6, 6],
    [6, 5, 3],
    [4, 6, 7],
    [4, 4, 7],
    [0, 4, 4],
    [5, 0, 3],
    [0, 2, 7],
    [1, 4, 0],
    [4, 3, 0],
    [6, 3, 0],
    [0, 5, 3],
    [3, 3, 3],
    [3, 2, 6],
    [0, 0, 0],
    [0, 0, 6],
    [7, 0, 0],
    [5, 1, 0],
    [7, 4, 7],
    [7, 5, 5],
    [6, 3, 7],
    [0, 2, 0],
    [0, 0, 3],
    [7, 7, 0],
    [1, 1, 1],
    [7, 5, 0],
    [7, 4, 0],
    [7, 7, 7],
    [3, 6, 0],
    [4, 0, 3],
    [3, 5, 7],
    [7, 0, 7],
    [0, 3, 6],
    [4, 4, 4],
    [0, 0, 0],
    [3, 1, 0],
    [0, 7, 7],
    [2, 0, 0],
    [5, 7, 2],
    [7, 5, 7],
    [4, 2, 0],
    [0, 7, 0],
    [6, 6, 0],
    [2, 2, 2],
    [0, 3, 1],
    [0, 0, 0],
    [6, 5, 7],
    [7, 7, 3],
    [4, 0, 7],
    [2, 7, 6],
    [7, 6, 0],
    [0, 2, 2],
];

/// The RP2C04-0004 lookup table (NES 2.0 Vs. PPU type `$5`): 64 entries of
/// 3-bit DAC digits, one of the four copy-protection permutations of the
/// 2C04 master palette.
pub const RP2C04_0004_DAC: DacPalette = [
    [4, 3, 0],
    [3, 2, 6],
    [0, 4, 4],
    [6, 6, 0],
    [0, 0, 0],
    [7, 5, 5],
    [0, 1, 4],
    [6, 3, 0],
    [5, 5, 5],
    [3, 1, 0],
    [0, 7, 0],
    [0, 0, 3],
    [7, 6, 4],
    [7, 7, 0],
    [0, 4, 0],
    [5, 7, 2],
    [7, 3, 7],
    [2, 0, 0],
    [0, 2, 7],
    [7, 4, 7],
    [0, 0, 0],
    [2, 2, 2],
    [5, 1, 0],
    [7, 4, 0],
    [6, 5, 3],
    [0, 5, 3],
    [4, 4, 7],
    [1, 4, 0],
    [4, 0, 3],
    [0, 0, 0],
    [4, 7, 3],
    [3, 5, 7],
    [5, 0, 3],
    [0, 3, 1],
    [4, 2, 0],
    [0, 0, 6],
    [4, 0, 7],
    [5, 0, 7],
    [3, 3, 3],
    [7, 0, 4],
    [0, 2, 2],
    [6, 6, 6],
    [0, 3, 6],
    [0, 2, 0],
    [1, 1, 1],
    [7, 7, 3],
    [4, 4, 4],
    [7, 0, 7],
    [7, 5, 7],
    [7, 7, 7],
    [3, 2, 0],
    [7, 0, 0],
    [7, 6, 0],
    [2, 7, 6],
    [7, 7, 7],
    [4, 6, 7],
    [0, 0, 0],
    [7, 5, 0],
    [6, 3, 7],
    [5, 6, 7],
    [3, 6, 0],
    [6, 5, 7],
    [0, 7, 7],
    [1, 2, 0],
];

/// One DAC digit triple to 8-bit RGB. The page gives `C = 255 * DAC / 7` for a
/// 3-bit channel value `DAC`; this rounds to the nearest integer, i.e.
/// `(255 * DAC + 3) / 7`, which yields 0, 36, 73, 109, 146, 182, 219, 255.
pub fn dac_to_rgb(dac: [u8; 3]) -> (u8, u8, u8) {
    let channel = |d: u8| ((255 * u32::from(d & 7) + 3) / 7) as u8;
    (channel(dac[0]), channel(dac[1]), channel(dac[2]))
}

/// The 64-entry table of the RGB PPU named by the NES 2.0 `vs_ppu_type` field
/// (header byte 13 low nibble), or `None` for $1/$6/$7/$C-$F, which the header
/// specification leaves reserved.
///
/// $0 is any RP2C03/RC2C03 variant; $2-$5 are RP2C04-0001..0004; $8-$B are
/// RC2C05-01..04, which share the 2C03 colours.
pub fn vs_palette_dac(vs_ppu_type: u8) -> Option<&'static DacPalette> {
    match vs_ppu_type {
        0x0 | 0x8..=0xB => Some(&RP2C03_DAC),
        0x2 => Some(&RP2C04_0001_DAC),
        0x3 => Some(&RP2C04_0002_DAC),
        0x4 => Some(&RP2C04_0003_DAC),
        0x5 => Some(&RP2C04_0004_DAC),
        _ => None,
    }
}

/// The 64-entry RGB table for a NES 2.0 `vs_ppu_type`, or `None` when that
/// value is not an RGB PPU. See `vs_palette_dac` for the mapping.
pub fn vs_palette(vs_ppu_type: u8) -> Option<Vec<(u8, u8, u8)>> {
    vs_palette_dac(vs_ppu_type).map(|table| table.iter().copied().map(dac_to_rgb).collect())
}

/// 512-entry table for an RGB PPU, index = (emph << 6) | c, from its 64 DAC entries.
///
/// The RGB chips emphasise differently from the composite 2C02 — nesdev "PPU palettes", section
/// "2C03 and 2C05": "unlike the 2C02, emphasis on the RGB chips works differently; rather than
/// \"darkening\" the specific color chosen, it sets the corresponding channel to full brightness
/// instead." Each set PPUMASK bit ($20 red, $40 green, $80 blue) therefore drives its own DAC
/// digit to 7 and leaves the rest of the entry alone. No column is exempt the way $xE/$xF are on
/// the composite table, and all three bits together give white — nesdev "Colour emphasis":
/// "Instead of darkening other RGB components, it forces one component to maximum brightness. A
/// few games, which set all three tint bits to darken all colors, are unplayable on these PPUs
/// because they instead turn the entire screen white."
pub fn rgb_emphasis_table(dac: &DacPalette) -> PalTable {
    (0..512u16)
        .map(|i| {
            let emph = (i >> 6) as u8;
            let entry = dac[usize::from(i & 0x3F)];
            let full = |d: u8, bit: u8| if emph & bit != 0 { 7 } else { d };
            dac_to_rgb([full(entry[0], 1), full(entry[1], 2), full(entry[2], 4)])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_red_bit_attenuates_green_and_blue() {
        assert_eq!(emphasized_rgb(0x20, 1), (236, 194, 192));
    }

    #[test]
    fn emphasis_green_bit_attenuates_red_and_blue() {
        assert_eq!(emphasized_rgb(0x20, 2), (192, 238, 192));
    }

    #[test]
    fn emphasis_blue_bit_attenuates_red_and_green() {
        assert_eq!(emphasized_rgb(0x20, 4), (192, 194, 236));
    }

    #[test]
    fn emphasis_all_bits() {
        assert_eq!(emphasized_rgb(0x20, 7), (156, 158, 156));
    }

    #[test]
    fn emphasis_on_column_0() {
        assert_eq!(emphasized_rgb(0x00, 3), (68, 68, 55));
    }

    #[test]
    fn emphasis_on_index_1() {
        assert_eq!(emphasized_rgb(0x01, 4), (0, 24, 116));
    }

    #[test]
    fn black_columns_are_never_attenuated() {
        assert_eq!(emphasized_rgb(0x0F, 7), (0, 0, 0));
        assert_eq!(emphasized_rgb(0x1E, 7), (0, 0, 0));
        assert_eq!(emphasized_rgb(0x2F, 7), (0, 0, 0));
    }

    #[test]
    fn no_emphasis_is_the_plain_palette() {
        for c in 0..64u8 {
            assert_eq!(emphasized_rgb(c, 0), NES_PALETTE[usize::from(c)]);
        }
    }

    #[test]
    fn palette_512_is_indexed_by_emphasis() {
        let p = palette_512();
        assert_eq!(p.len(), 512);
        assert_eq!(p[usize::from((7u16 << 6) | 0x20u16)], (156, 158, 156));
        for emph in 0..8u8 {
            for c in 0..64u8 {
                let idx = usize::from((u16::from(emph) << 6) | u16::from(c));
                assert_eq!(p[idx], emphasized_rgb(c, emph));
            }
        }
    }

    #[test]
    fn the_composite_512_table_is_byte_for_byte_unchanged() {
        // The RGB-PPU table must not disturb the 2C02 one: pin all 1536 RGB bytes with a digest.
        let bytes: Vec<u8> = palette_512()
            .iter()
            .copied()
            .flat_map(|(r, g, b)| [r, g, b])
            .collect();
        assert_eq!(bytes.len(), 1536);
        assert_eq!(crate::state::fnv1a64(&bytes), 0x36a3_8530_8d0e_2919);
    }

    #[test]
    fn zero_emphasis_converts_to_the_bare_palette() {
        let frame = [0x30u8, 0x0F];
        let emph = [0u8, 0];
        let mut out = [0u8; 8];
        to_rgba_emph(&frame, &emph, &mut out);
        assert_eq!(out, [236, 238, 236, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn emphasise_matches_the_builtin_rule_on_custom_colours() {
        assert_eq!(emphasise((236, 238, 236), 0x20, 7), (156, 158, 156));
        assert_eq!(emphasise((10, 120, 200), 0x00, 1), (10, 97, 163));
    }

    #[test]
    fn emphasis_columns_stay_as_authored() {
        assert_eq!(emphasise((100, 150, 200), 0x0F, 7), (100, 150, 200));
    }

    #[test]
    fn emphasis_table_expands_any_64_colours() {
        let base = [(20, 40, 60); 64];
        let table = emphasis_table(base.as_slice());
        assert_eq!(table.len(), 512);
        assert_eq!(
            table[usize::from((3u16 << 6) | 0x25u16)],
            emphasise((20, 40, 60), 0x25, 3)
        );
    }

    #[test]
    fn pal_file_of_64_colours_loads_in_rgb_order() {
        let bytes: Vec<u8> = (0..PAL_BYTES_64).map(|i| (i % 256) as u8).collect();
        assert_eq!(bytes.len(), PAL_BYTES_64);
        let entries = parse_pal(bytes.as_slice()).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(entries.len(), 64);
        assert_eq!(entries[0], (0, 1, 2));
        assert_eq!(entries[63], (189, 190, 191));
    }

    #[test]
    fn pal_file_of_512_colours_loads() {
        let bytes: Vec<u8> = (0..PAL_BYTES_512).map(|i| (i % 256) as u8).collect();
        assert_eq!(bytes.len(), PAL_BYTES_512);
        let entries = parse_pal(bytes.as_slice()).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(entries.len(), 512);
        assert_eq!(entries[511], (253, 254, 255));
    }

    #[test]
    fn pal_file_of_the_wrong_size_is_rejected() {
        for size in [0, 3, 90, 191, 193, 1535, 1537] {
            let bytes = vec![0u8; size];
            match parse_pal(bytes.as_slice()) {
                Err(reason) => assert!(reason.contains("expected 192")),
                Ok(entries) => panic!("size {size} was accepted as {} colours", entries.len()),
            }
        }
    }

    #[test]
    fn a_512_colour_file_is_used_verbatim() {
        let bytes: Vec<u8> = (0..PAL_BYTES_512).map(|i| (i % 256) as u8).collect();
        let entries = parse_pal(bytes.as_slice()).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(rgb_table(entries.as_slice()), entries);
    }

    #[test]
    fn a_64_colour_file_gets_the_emphasis_variants_generated() {
        // A bright, non-NES base: emphasis 7 dims every colour, columns E/F stay put.
        let base = [(100, 150, 200); 64];
        let table = rgb_table(base.as_slice());
        assert_eq!(table.len(), 512);
        assert_eq!(table[0], (100, 150, 200));
        assert_eq!(table[64], emphasise((100, 150, 200), 0x00, 1));
        assert_eq!(
            table[usize::from(7u16 << 6)],
            emphasise((100, 150, 200), 0x00, 7)
        );
        assert_eq!(table[usize::from((7u16 << 6) | 0x0Eu16)], (100, 150, 200));
    }

    #[test]
    fn built_in_palette_round_trips_through_the_512_table() {
        let bytes: Vec<u8> = NES_PALETTE
            .iter()
            .copied()
            .flat_map(|(r, g, b)| [r, g, b])
            .collect();
        assert_eq!(bytes.len(), PAL_BYTES_64);
        let entries = parse_pal(bytes.as_slice()).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(entries.as_slice(), NES_PALETTE.as_slice());
        assert_eq!(rgb_table(entries.as_slice()), palette_512());
    }

    #[test]
    fn builtin_rgba_table_matches_the_emphasis_rule() {
        // The packed table is what every frame copies from, so pin all 512 entries to the
        // per-colour emphasis rule rather than to another converter.
        let table = builtin_rgba_table();
        for emph in 0..8u8 {
            for c in 0..64u8 {
                let idx = usize::from((u16::from(emph) << 6) | u16::from(c));
                let (r, g, b) = emphasized_rgb(c, emph);
                assert_eq!(table[idx], [r, g, b, 255], "entry ${idx:03X}");
            }
        }
    }

    #[test]
    fn a_loaded_palette_replaces_the_rendered_colours() {
        let base = [(0xAA, 0xBB, 0xCC); 64];
        let table = rgba_table(rgb_table(base.as_slice()).as_slice());
        let frame = [0x01];
        let emph = [0];
        let mut out = [0u8; 4];
        to_rgba_pal(&frame, &emph, &table, &mut out);
        assert_eq!(out, [0xAA, 0xBB, 0xCC, 255]);
    }

    #[test]
    fn load_pal_file_reads_a_192_byte_file() {
        let dir = std::env::temp_dir().join("nes_pal_64");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("custom.pal");
        let bytes: Vec<u8> = (0..PAL_BYTES_64).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file, bytes.as_slice()).unwrap();
        let entries = load_pal_file(&file).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(entries.len(), 64);
        assert_eq!(entries[0], (0, 1, 2));
    }

    #[test]
    fn load_pal_file_rejects_a_wrong_size_file() {
        let dir = std::env::temp_dir().join("nes_pal_bad_size");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bad.pal");
        std::fs::write(&file, vec![0u8; 100]).unwrap();
        match load_pal_file(&file) {
            Err(reason) => {
                assert!(reason.contains("expected 192"));
                assert!(reason.contains("bad.pal"));
            }
            Ok(entries) => panic!("a 100-byte file was accepted as {} colours", entries.len()),
        }
    }

    #[test]
    fn load_pal_table_expands_a_192_byte_file() {
        let dir = std::env::temp_dir().join("nes_pal_table_64");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("custom.pal");
        let bytes: Vec<u8> = (0..PAL_BYTES_64).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file, bytes.as_slice()).unwrap();
        let (table, shown) =
            load_pal_table(&file).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(table.len(), 512);
        assert!(shown.contains("64 colours"));
        assert_eq!(table[0], (0, 1, 2));
        assert_eq!(table[64], emphasise((0, 1, 2), 0x00, 1));
    }

    #[test]
    fn load_pal_table_keeps_a_1536_byte_file_verbatim() {
        let dir = std::env::temp_dir().join("nes_pal_table_512");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("emphasis.pal");
        let bytes: Vec<u8> = (0..PAL_BYTES_512).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file, bytes.as_slice()).unwrap();
        let (table, shown) =
            load_pal_table(&file).unwrap_or_else(|reason| panic!("test: {reason}"));
        assert_eq!(table.len(), 512);
        assert!(shown.contains("512 colours"));
        assert_eq!(table[usize::from((7u16 << 6) | 0x3Fu16)], (253, 254, 255));
    }

    /// The loaders' wording is their interface: the frontend prints these strings, so rewriting
    /// the chain must keep them word for word.
    #[test]
    fn pal_loader_messages_are_unchanged() {
        let dir = std::env::temp_dir().join("nes_pal_wording");
        std::fs::create_dir_all(&dir).unwrap();
        let short = dir.join("short.pal");
        std::fs::write(&short, vec![0u8; 191]).unwrap();
        assert_eq!(
            load_pal_file(&short).unwrap_err(),
            format!(
                "{}: expected 192 (64 colours) or 1536 (512 colours) bytes, got 191",
                short.display()
            )
        );
        let missing = dir.join("missing.pal");
        let err = load_pal_file(&missing).unwrap_err();
        assert!(
            err.starts_with(&format!("cannot read {}: ", missing.display())),
            "{err}"
        );

        let good = dir.join("good.pal");
        std::fs::write(&good, vec![7u8; PAL_BYTES_64]).unwrap();
        let (table, shown) = load_pal_table(&good).unwrap();
        assert_eq!(shown, format!("{} (64 colours)", good.display()));
        assert_eq!(table.len(), 512);
        assert_eq!(table[0], (7, 7, 7));
    }

    /// Expected NTSC values come from a reference run of the algorithm; allow
    /// +/-1 per channel for float rounding.
    fn close(a: u8, b: u8) -> bool {
        (i32::from(a) - i32::from(b)).abs() <= 1
    }

    fn close_rgb(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
        close(a.0, b.0) && close(a.1, b.1) && close(a.2, b.2)
    }

    #[test]
    fn ntsc_palette_has_64_entries() {
        assert_eq!(ntsc_palette().len(), 64);
    }

    #[test]
    fn ntsc_black_and_white() {
        let p = ntsc_palette();
        assert_eq!(p[0x0F], (0, 0, 0));
        assert_eq!(p[0x0D], (0, 0, 0));
        assert!(close_rgb(p[0x20], (255, 255, 255)));
        assert!(close_rgb(p[0x30], (255, 255, 255)));
    }

    #[test]
    fn ntsc_greys() {
        let p = ntsc_palette();
        assert!(close_rgb(p[0x00], (82, 82, 82)));
        assert!(close_rgb(p[0x3D], (169, 169, 169)));
    }

    #[test]
    fn ntsc_hues() {
        let p = ntsc_palette();
        assert!(close_rgb(p[0x16], (130, 46, 36))); // red
        assert!(close_rgb(p[0x1A], (26, 107, 5))); // green
        assert!(close_rgb(p[0x12], (56, 55, 188))); // blue
        assert!(close_rgb(p[0x2C], (86, 179, 192)));
    }

    #[test]
    fn ntsc_columns_e_and_f_are_black() {
        let p = ntsc_palette();
        for (c, rgb) in p
            .iter()
            .enumerate()
            .filter(|(c, _)| c & 0x0F == 0x0E || c & 0x0F == 0x0F)
        {
            assert_eq!(*rgb, (0, 0, 0), "index 0x{c:02X}");
        }
    }
}

/// Unit tests for the RGB PPU tables: the mapping from NES 2.0 `vs_ppu_type`,
/// the DAC->RGB formula, spot checks against the cited page revision (nesdev
/// "PPU palettes", revision 24257, sections "2C03 and 2C05" and "RP2C04-000x"),
/// and the permutation relationship the page describes for the four 2C04
/// versions.
#[cfg(test)]
mod rgb_ppu_tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The 2C04 "master" ordering printed on the same page ("no version of the
    /// 2C04 was ever made with the below ordering"); it is the reference that
    /// the four shipped tables permute.
    const MASTER: DacPalette = [
        [3, 3, 3],
        [0, 1, 4],
        [0, 0, 6],
        [3, 2, 6],
        [4, 0, 3],
        [5, 0, 3],
        [5, 1, 0],
        [4, 2, 0],
        [3, 2, 0],
        [1, 2, 0],
        [0, 3, 1],
        [0, 4, 0],
        [0, 2, 2],
        [1, 1, 1],
        [0, 0, 3],
        [0, 2, 0],
        [5, 5, 5],
        [0, 3, 6],
        [0, 2, 7],
        [4, 0, 7],
        [5, 0, 7],
        [7, 0, 4],
        [7, 0, 0],
        [6, 3, 0],
        [4, 3, 0],
        [1, 4, 0],
        [0, 4, 0],
        [0, 5, 3],
        [0, 4, 4],
        [2, 2, 2],
        [2, 0, 0],
        [3, 1, 0],
        [7, 7, 7],
        [3, 5, 7],
        [4, 4, 7],
        [6, 3, 7],
        [7, 0, 7],
        [7, 3, 7],
        [7, 4, 0],
        [7, 5, 0],
        [6, 6, 0],
        [3, 6, 0],
        [0, 7, 0],
        [2, 7, 6],
        [0, 7, 7],
        [4, 4, 4],
        [0, 0, 0],
        [0, 0, 0],
        [7, 7, 7],
        [5, 6, 7],
        [6, 5, 7],
        [7, 5, 7],
        [7, 4, 7],
        [7, 5, 5],
        [7, 6, 4],
        [7, 7, 0],
        [7, 7, 3],
        [5, 7, 2],
        [4, 7, 3],
        [2, 7, 6],
        [4, 6, 7],
        [6, 6, 6],
        [6, 5, 3],
        [7, 6, 0],
    ];

    fn counts(table: &DacPalette) -> BTreeMap<[u8; 3], usize> {
        let mut out = BTreeMap::new();
        for entry in table {
            *out.entry(*entry).or_insert(0) += 1;
        }
        out
    }

    fn colours(table: &DacPalette) -> BTreeSet<(u8, u8, u8)> {
        table.iter().copied().map(dac_to_rgb).collect()
    }

    fn table_of(vs_ppu_type: u8) -> &'static DacPalette {
        vs_palette_dac(vs_ppu_type)
            .unwrap_or_else(|| panic!("vs_ppu_type ${vs_ppu_type:02X} has no table"))
    }

    #[test]
    fn dac_digits_scale_by_the_published_formula() {
        // C = 255 * DAC / 7, rounded to nearest.
        assert_eq!(dac_to_rgb([0, 0, 0]), (0, 0, 0));
        assert_eq!(dac_to_rgb([1, 1, 1]), (36, 36, 36));
        assert_eq!(dac_to_rgb([3, 3, 3]), (109, 109, 109));
        assert_eq!(dac_to_rgb([4, 4, 4]), (146, 146, 146));
        assert_eq!(dac_to_rgb([6, 6, 6]), (219, 219, 219));
        assert_eq!(dac_to_rgb([7, 7, 7]), (255, 255, 255));
        assert_eq!(dac_to_rgb([7, 4, 0]), (255, 146, 0));
    }

    #[test]
    fn every_rgb_ppu_type_maps_to_a_64_entry_table() {
        for vs_ppu_type in [0x0u8, 0x2, 0x3, 0x4, 0x5, 0x8, 0x9, 0x0A, 0x0B] {
            let table = vs_palette(vs_ppu_type)
                .unwrap_or_else(|| panic!("vs_ppu_type ${vs_ppu_type:02X} should be an RGB PPU"));
            assert_eq!(table.len(), 64, "vs_ppu_type ${vs_ppu_type:02X}");
        }
    }

    #[test]
    fn reserved_ppu_types_have_no_palette() {
        for vs_ppu_type in [0x1u8, 0x6, 0x7, 0xC, 0xD, 0xE, 0xF, 0x10, 0xFF] {
            assert!(
                vs_palette(vs_ppu_type).is_none(),
                "vs_ppu_type ${vs_ppu_type:02X} is reserved but got a table"
            );
        }
    }

    #[test]
    fn rp2c03_spot_checks_against_the_wiki() {
        let p = vs_palette(0x0).unwrap_or_else(|| panic!("type $0 has a table"));
        assert_eq!(p[0x00], (109, 109, 109)); // 333
        assert_eq!(p[0x0D], (0, 0, 0)); // 000, hue $D is black
        assert_eq!(p[0x16], (255, 0, 0)); // 700
        assert_eq!(p[0x20], (255, 255, 255)); // 777
        assert_eq!(p[0x3C], (146, 219, 255)); // 467
    }

    #[test]
    fn rp2c04_spot_checks_against_the_wiki() {
        // Type $2 = RP2C04-0001
        let p = table_of(0x2);
        assert_eq!(dac_to_rgb(p[0x00]), (255, 182, 182)); // 755
        assert_eq!(dac_to_rgb(p[0x08]), (255, 255, 255)); // 777
        assert_eq!(dac_to_rgb(p[0x1F]), (0, 36, 146)); // 014
        assert_eq!(dac_to_rgb(p[0x2D]), (0, 0, 219)); // 006
        assert_eq!(dac_to_rgb(p[0x3C]), (255, 255, 109)); // 773

        // Type $3 = RP2C04-0002
        let p = table_of(0x3);
        assert_eq!(dac_to_rgb(p[0x00]), (0, 0, 0)); // 000
        assert_eq!(dac_to_rgb(p[0x08]), (255, 0, 0)); // 700
        assert_eq!(dac_to_rgb(p[0x1F]), (109, 73, 219)); // 326
        assert_eq!(dac_to_rgb(p[0x2D]), (0, 0, 0)); // 000
        assert_eq!(dac_to_rgb(p[0x3C]), (255, 182, 182)); // 755

        // Type $4 = RP2C04-0003
        let p = table_of(0x4);
        assert_eq!(dac_to_rgb(p[0x00]), (182, 0, 255)); // 507
        assert_eq!(dac_to_rgb(p[0x08]), (0, 36, 146)); // 014
        assert_eq!(dac_to_rgb(p[0x1F]), (255, 182, 182)); // 755
        assert_eq!(dac_to_rgb(p[0x2D]), (146, 146, 146)); // 444
        assert_eq!(dac_to_rgb(p[0x3C]), (146, 0, 255)); // 407

        // Type $5 = RP2C04-0004
        let p = table_of(0x5);
        assert_eq!(dac_to_rgb(p[0x00]), (146, 109, 0)); // 430
        assert_eq!(dac_to_rgb(p[0x08]), (182, 182, 182)); // 555
        assert_eq!(dac_to_rgb(p[0x1F]), (109, 182, 255)); // 357
        assert_eq!(dac_to_rgb(p[0x2D]), (255, 255, 109)); // 773
        assert_eq!(dac_to_rgb(p[0x3C]), (109, 219, 0)); // 360
    }

    #[test]
    fn rc2c05_variants_use_the_2c03_colours() {
        let base = vs_palette(0x0).unwrap_or_else(|| panic!("type $0 has a table"));
        for vs_ppu_type in [0x8u8, 0x9, 0x0A, 0x0B] {
            assert_eq!(
                vs_palette(vs_ppu_type).as_ref(),
                Some(&base),
                "${vs_ppu_type:02X}"
            );
        }
    }

    #[test]
    fn every_2c04_table_covers_the_master_colours() {
        let master = colours(&MASTER);
        assert_eq!(
            master.len(),
            60,
            "the 2C04 master palette has 60 distinct colours"
        );
        for vs_ppu_type in [0x2u8, 0x3, 0x4, 0x5] {
            let table = colours(table_of(vs_ppu_type));
            assert_eq!(table.len(), 60, "vs_ppu_type ${vs_ppu_type:02X}");
            assert_eq!(
                table, master,
                "vs_ppu_type ${vs_ppu_type:02X} is not a permutation of the master"
            );
        }
    }

    #[test]
    fn the_four_2c04_tables_are_permutations_of_each_other() {
        // The page's duplicates (000, 040, 276, 777) mean the tables agree as
        // multisets of colours without each index being distinct; that is what
        // the page says a LUT may collapse ("repeating colors such as 000 and
        // 777 may index into the same entry").
        let base = counts(table_of(0x2));
        assert_eq!(base[&[0, 0, 0]], 4);
        assert_eq!(base[&[0, 4, 0]], 1);
        assert_eq!(base[&[2, 7, 6]], 1);
        assert_eq!(base[&[7, 7, 7]], 2);
        for vs_ppu_type in [0x3u8, 0x4, 0x5] {
            assert_eq!(
                counts(table_of(vs_ppu_type)),
                base,
                "vs_ppu_type ${vs_ppu_type:02X} is not a permutation of RP2C04-0001"
            );
        }
        // And no two versions are the same table.
        let tables: Vec<&DacPalette> = [0x2u8, 0x3, 0x4, 0x5]
            .iter()
            .map(|&t| table_of(t))
            .collect();
        for i in 0..tables.len() {
            for j in (i + 1)..tables.len() {
                assert_ne!(
                    tables[i], tables[j],
                    "2C04 versions {i} and {j} are identical"
                );
            }
        }
    }

    #[test]
    fn rp2c03_keeps_the_documented_duplicate_colours() {
        // "some of the colors are duplicates: $0B and $1A = 040, $2B and $3B = 276"
        let p = table_of(0x0);
        assert_eq!(p[0x0B], [0, 4, 0]);
        assert_eq!(p[0x1A], [0, 4, 0]);
        assert_eq!(p[0x2B], [2, 7, 6]);
        assert_eq!(p[0x3B], [2, 7, 6]);
        // "missing the greys in entries $2D and $3D"
        assert_eq!(p[0x2D], [0, 0, 0]);
        assert_eq!(p[0x3D], [0, 0, 0]);
        assert_eq!(colours(&RP2C03_DAC).len(), 50);
    }

    #[test]
    fn vs_palette_returns_rgb_entries() {
        let p = vs_palette(0x2).unwrap_or_else(|| panic!("type $2 has a table"));
        assert_eq!(p.len(), 64);
        assert_eq!(p[0], (255, 182, 182));
        assert_eq!(p[0x3F], (146, 255, 109)); // 473
    }

    #[test]
    fn rgb_emphasis_sets_the_named_channel_to_full_brightness() {
        // Nesdev "PPU palettes" (2C03 and 2C05): an emphasis bit "sets the corresponding channel
        // to full brightness", leaving the other two as the entry has them — the opposite of the
        // composite rule. A 2C03 and a 2C04 table, each single bit and all three together.
        for vs_ppu_type in [0x0u8, 0x2] {
            let dac = table_of(vs_ppu_type);
            let table = rgb_emphasis_table(dac);
            assert_eq!(table.len(), 512);
            let plain: PalTable = dac.iter().copied().map(dac_to_rgb).collect();
            assert_eq!(table[..64].to_vec(), plain, "no emphasis is the bare table");
            for emph in 1..8u8 {
                for c in 0..64u8 {
                    let base = dac_to_rgb(dac[usize::from(c)]);
                    let mut want = [base.0, base.1, base.2];
                    for (channel, bit) in [(0usize, 1u8), (1, 2), (2, 4)] {
                        if emph & bit != 0 {
                            want[channel] = 255;
                        }
                    }
                    let idx = usize::from((u16::from(emph) << 6) | u16::from(c));
                    assert_eq!(
                        table[idx],
                        (want[0], want[1], want[2]),
                        "type ${vs_ppu_type:02X} emph {emph} entry ${c:02X}"
                    );
                }
            }
        }
    }

    #[test]
    fn rgb_emphasis_spot_checks_by_literal_colour() {
        // RP2C04-0001 entry $00 is 755 = (255, 182, 182): green emphasis ($40) lifts green to
        // 775, red and blue stay. The composite rule would have dimmed red and blue instead.
        let p = rgb_emphasis_table(table_of(0x2));
        assert_eq!(p[usize::from(2u16 << 6)], (255, 255, 182));
        // RP2C03 entry $0D is 000 (black): red emphasis makes it pure red, not black.
        let q = rgb_emphasis_table(table_of(0x0));
        assert_eq!(q[usize::from((1u16 << 6) | 0x0Du16)], (255, 0, 0));
        // All three bits: the whole screen goes white (nesdev "Colour emphasis").
        for c in 0..64u8 {
            assert_eq!(
                p[usize::from((7u16 << 6) | u16::from(c))],
                (255, 255, 255),
                "entry ${c:02X}"
            );
        }
    }

    #[test]
    fn the_2c04_monochrome_bit_remaps_each_row_to_its_first_column() {
        // Nesdev "PPU palettes" (2C04): the PPUMASK monochrome bit "has the same implementation as
        // on the 2C02 ... rather than forcing colors to grayscale, it instead forces them to the
        // first column", since the renderer ANDs the index with $30 on every PPU (`render.rs`).
        let dac = table_of(0x2);
        let table = rgb_emphasis_table(dac);
        let shown = |c: u8| table[usize::from(c & 0x30)];
        for row in 0..4u8 {
            let first = dac_to_rgb(dac[usize::from(row << 4)]);
            for col in 0..16u8 {
                let c = (row << 4) | col;
                assert_eq!(
                    shown(c),
                    first,
                    "index ${c:02X} must show its row's $0X colour"
                );
            }
        }
        // And that is not grey: RP2C04-0001's $10 is 357.
        assert_eq!(shown(0x15), (109, 182, 255));
    }
}
