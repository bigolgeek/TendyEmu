//! Composite-video (NTSC) output filter, written from the signal theory.
//!
//! A real NES does not hand the TV an RGB picture. It emits one composite wave per scanline: the
//! brightness (luma, Y) with the colour (chroma) riding on top as a phase-and-amplitude modulated
//! 3.58 MHz subcarrier. The TV then has to pull the two apart again, and where the picture changes
//! faster than the TV can tell luma detail from chroma, the separation fails: fine luma detail
//! turns into false colour and fine colour into false brightness. That crosstalk is what the
//! TASVideos `tvpassfail` test ROM draws on purpose, and what a plain palette lookup can never show.
//!
//! This module models the chain end to end, with no table of "known artefacts":
//!
//! 1. **Colour space.** Each palette entry's RGB is converted to Y, I and Q with the NTSC matrix:
//!    `Y = 0.299R + 0.587G + 0.114B`, `I = 0.596R - 0.274G - 0.322B`, `Q = 0.211R - 0.523G + 0.312B`.
//! 2. **Encode.** The wave is sampled at [`SAMPLES_PER_PIXEL`] = 3 samples per NES pixel with the
//!    subcarrier advancing 1/3 cycle (120 degrees) per sample, so one pixel spans exactly one
//!    subcarrier cycle: `composite(t) = Y + I cos(wt) + Q sin(wt)`.
//! 3. **Decode.** Luma is the low-pass of the composite (a 3-sample box, whose response is exactly
//!    zero at the subcarrier). Chroma is the band-pass, defined as composite minus that low-pass.
//!    The chroma is multiplied by `2cos(wt)` and `2sin(wt)` (synchronous demodulation) and each
//!    product is low-passed (two box passes) into I and Q. Y, I and Q go back to RGB through the
//!    inverse matrix.
//!
//! Because the box filters are exact nulls at the subcarrier and at twice the subcarrier, a flat
//! colour survives the round trip to within the rounding of the matrix constants. Anything that
//! changes within a pixel or two does not, which is the point.
//!
//! The decoded wave is reduced back to 256 pixels per row by averaging the three samples of each
//! pixel, so the output keeps the caller's 256x240 geometry and can replace the plain palette
//! lookup without touching the texture size or the display rectangle. Every scanline starts the
//! subcarrier at phase zero (the hardware's line-to-line phase shift is not modelled), which makes
//! the output a pure function of the frame.

use crate::ppu::palette::emphasized_rgb;
use crate::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// Composite samples per NES pixel. Three samples at 120 degrees each make one subcarrier cycle
/// per pixel.
pub const SAMPLES_PER_PIXEL: usize = 3;

/// Pixels of the edge colour added on each side of a scanline before filtering. The picture's
/// first and last pixels are then filtered against more of the same colour (as on hardware, where
/// the line is surrounded by the border colour) instead of against a repeated single sample, which
/// would break the subcarrier's phase and tint the edge columns.
const PAD: usize = 4;

/// Samples per scanline in the composite wave, padding included.
const LINE_SAMPLES: usize = (SCREEN_WIDTH + 2 * PAD) * SAMPLES_PER_PIXEL;

/// Subcarrier advance per sample, in radians (120 degrees).
const STEP: f32 = 2.0 * std::f32::consts::PI / 3.0;

/// RGB (0..=255) to YIQ.
fn rgb_to_yiq(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        0.299 * r + 0.587 * g + 0.114 * b,
        0.596 * r - 0.274 * g - 0.322 * b,
        0.211 * r - 0.523 * g + 0.312 * b,
    )
}

/// YIQ to RGB with the inverse matrix, rounded and clamped to a byte.
fn yiq_to_rgb(y: f32, i: f32, q: f32) -> [u8; 3] {
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [
        c(y + 0.956 * i + 0.621 * q),
        c(y - 0.272 * i - 0.647 * q),
        c(y - 1.106 * i + 1.703 * q),
    ]
}

/// 3-sample box low-pass with the edge samples repeated. It has a zero at 1/3 cycle per sample,
/// the subcarrier, and at 2/3, its second harmonic.
fn box3(src: &[f32], dst: &mut [f32]) {
    debug_assert_eq!(src.len(), dst.len());
    // Two samples minimum: both edges repeat their only neighbour, so a shorter input would index
    // past the end where the old clamp formula was total. The one caller passes a whole line.
    debug_assert!(
        src.len() >= 2,
        "box3 needs at least two samples, got {}",
        src.len()
    );
    let last = src.len() - 1;
    // The two edges repeat their only neighbour; the middle is a plain 3-tap, so the loop has no
    // per-sample index clamp and `windows(3)` proves the bound to the compiler.
    dst[0] = (src[0] + src[0] + src[1]) / 3.0;
    for (d, w) in dst[1..last].iter_mut().zip(src.windows(3)) {
        *d = (w[0] + w[1] + w[2]) / 3.0;
    }
    dst[last] = (src[last - 1] + src[last] + src[last]) / 3.0;
}

/// The composite NTSC filter. Holds the encoder's per-colour (Y, I, Q) table and the subcarrier
/// lookup, so `apply` does no trigonometry per pixel.
#[derive(Clone, Debug)]
pub struct NtscFilter {
    /// `(emphasis << 6) | colour` -> (Y, I, Q), derived from the crate's own RGB palette.
    yiq: Vec<(f32, f32, f32)>,
    /// `cos` and `sin` of the subcarrier phase for the three sample slots of a pixel.
    carrier: [(f32, f32); SAMPLES_PER_PIXEL],
}

impl Default for NtscFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl NtscFilter {
    /// A filter over the built-in palette, honouring PPUMASK emphasis like `to_rgba_emph`.
    pub fn new() -> Self {
        let rgb: Vec<(u8, u8, u8)> = (0..512usize)
            .map(|i| emphasized_rgb((i & 0x3F) as u8, (i >> 6) as u8))
            .collect();
        Self::with_table(&rgb)
    }

    /// A filter over a 512-entry RGB table (index `(emphasis << 6) | colour`), for a loaded .pal.
    pub fn with_table(table: &[(u8, u8, u8)]) -> Self {
        assert!(table.len() >= 512, "NTSC filter needs a 512-entry palette");
        let yiq = table
            .iter()
            .map(|&(r, g, b)| rgb_to_yiq(f32::from(r), f32::from(g), f32::from(b)))
            .collect();
        let mut carrier = [(0.0, 0.0); SAMPLES_PER_PIXEL];
        for (k, c) in carrier.iter_mut().enumerate() {
            let phase = STEP * k as f32;
            *c = (phase.cos(), phase.sin());
        }
        NtscFilter { yiq, carrier }
    }

    /// Encode one scanline of palette indices to its padded composite wave (`LINE_SAMPLES` samples).
    fn encode_line(&self, idx: &[u8], emph: &[u8], wave: &mut [f32]) {
        for p in 0..SCREEN_WIDTH + 2 * PAD {
            // Padding pixels repeat the nearest picture pixel.
            let x = p.saturating_sub(PAD).min(SCREEN_WIDTH - 1);
            let (c, e) = (idx[x], emph[x]);
            let (y, i, q) = self.yiq[usize::from(e & 7) << 6 | usize::from(c & 0x3F)];
            for (k, &(cos, sin)) in self.carrier.iter().enumerate() {
                wave[p * SAMPLES_PER_PIXEL + k] = y + i * cos + q * sin;
            }
        }
    }

    /// Decode a composite wave into RGB24 for one scanline, averaging each pixel's three samples.
    /// Every scratch buffer is a stack array: the six `Vec`s this used to heap-allocate per line
    /// were 1,440 allocations and 4.5 MB of zero fill per frame.
    fn decode_line(&self, wave: &[f32], out: &mut [u8]) {
        let mut luma = [0.0f32; LINE_SAMPLES];
        box3(wave, &mut luma);
        let (mut ri, mut rq) = ([0.0f32; LINE_SAMPLES], [0.0f32; LINE_SAMPLES]);
        for n in 0..LINE_SAMPLES {
            let (cos, sin) = self.carrier[n % SAMPLES_PER_PIXEL];
            let chroma = wave[n] - luma[n];
            ri[n] = 2.0 * chroma * cos;
            rq[n] = 2.0 * chroma * sin;
        }
        let (mut tmp, mut i_lp, mut q_lp) = (
            [0.0f32; LINE_SAMPLES],
            [0.0f32; LINE_SAMPLES],
            [0.0f32; LINE_SAMPLES],
        );
        box3(&ri, &mut tmp);
        box3(&tmp, &mut i_lp);
        box3(&rq, &mut tmp);
        box3(&tmp, &mut q_lp);
        for x in 0..SCREEN_WIDTH {
            let s = (x + PAD) * SAMPLES_PER_PIXEL;
            let mean = |v: &[f32]| (v[s] + v[s + 1] + v[s + 2]) / 3.0;
            let rgb = yiq_to_rgb(mean(&luma), mean(&i_lp), mean(&q_lp));
            out[x * 3..x * 3 + 3].copy_from_slice(&rgb);
        }
    }

    /// Filter a whole frame. `indices` and `emphasis` are the PPU's `SCREEN_WIDTH * SCREEN_HEIGHT`
    /// framebuffer and per-pixel emphasis buffers. `out` is resized to `SCREEN_WIDTH * SCREEN_HEIGHT * 3`
    /// and filled with RGB24 rows, 256 pixels wide: the three samples per pixel are averaged back
    /// down, so the geometry matches the plain palette path. Rows are independent.
    pub fn apply(&self, indices: &[u8], emphasis: &[u8], out: &mut Vec<u8>) {
        assert_eq!(indices.len(), SCREEN_WIDTH * SCREEN_HEIGHT);
        assert_eq!(emphasis.len(), indices.len());
        out.clear();
        out.resize(SCREEN_WIDTH * SCREEN_HEIGHT * 3, 0);
        let mut wave = [0.0f32; LINE_SAMPLES];
        for y in 0..SCREEN_HEIGHT {
            let r = y * SCREEN_WIDTH..(y + 1) * SCREEN_WIDTH;
            self.encode_line(&indices[r.clone()], &emphasis[r], &mut wave);
            self.decode_line(
                &wave,
                &mut out[y * SCREEN_WIDTH * 3..(y + 1) * SCREEN_WIDTH * 3],
            );
        }
    }

    /// Like [`apply`](Self::apply) but expanded to RGBA with alpha 255, ready for a texture upload.
    pub fn apply_rgba(&self, indices: &[u8], emphasis: &[u8], rgb: &mut Vec<u8>, rgba: &mut [u8]) {
        self.apply(indices, emphasis, rgb);
        for (px, c) in rgba
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(rgb.as_chunks::<3>().0)
        {
            px.copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = SCREEN_WIDTH * SCREEN_HEIGHT;

    /// Run the filter over a frame built from `f(x, y)` -> palette index, no emphasis.
    fn run(f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let idx: Vec<u8> = (0..N)
            .map(|i| f(i % SCREEN_WIDTH, i / SCREEN_WIDTH))
            .collect();
        let mut out = Vec::new();
        NtscFilter::new().apply(&idx, &vec![0; N], &mut out);
        out
    }

    fn px(out: &[u8], x: usize, y: usize) -> (f32, f32, f32) {
        let o = (y * SCREEN_WIDTH + x) * 3;
        (
            f32::from(out[o]),
            f32::from(out[o + 1]),
            f32::from(out[o + 2]),
        )
    }

    /// Chroma magnitude sqrt(I^2 + Q^2) of one output pixel.
    fn chroma(out: &[u8], x: usize, y: usize) -> f32 {
        let (r, g, b) = px(out, x, y);
        let (_, i, q) = rgb_to_yiq(r, g, b);
        i.hypot(q)
    }

    /// A flat field of any colour decodes back to that colour. Pinned because the encoder and the
    /// decoder are meant to be inverses on anything slower than a pixel; the tolerance of 3 levels
    /// per channel is the rounding of the 3-digit matrix constants, not filter error.
    #[test]
    fn flat_field_round_trips_colour() {
        for c in 0..64u8 {
            let out = run(|_, _| c);
            let (r, g, b) = emphasized_rgb(c, 0);
            // Sample the middle of the frame, away from the clamped edges.
            let (or, og, ob) = px(&out, 100, 100);
            for (got, want) in [(or, r), (og, g), (ob, b)] {
                assert!(
                    (got - f32::from(want)).abs() <= 3.0,
                    "colour {c:#04x}: got {got} want {want}"
                );
            }
        }
    }

    /// A flat field also round-trips right up to the left and right edges, where the filters
    /// repeat the edge sample: the picture border must not tint.
    #[test]
    fn flat_field_edges_do_not_tint() {
        let out = run(|_, _| 0x16);
        let (r, g, b) = emphasized_rgb(0x16, 0);
        for x in [0, SCREEN_WIDTH - 1] {
            let (or, og, ob) = px(&out, x, 7);
            assert!(
                (or - f32::from(r)).abs() <= 3.0
                    && (og - f32::from(g)).abs() <= 3.0
                    && (ob - f32::from(b)).abs() <= 3.0
            );
        }
    }

    /// A grey ramp (palette column 0 plus the black/white entries, all with I = Q = 0 in the
    /// source palette) stays grey: decoded R, G and B stay within 3 levels of each other.
    #[test]
    fn grey_ramp_stays_grey() {
        let greys = [0x0Fu8, 0x00, 0x10, 0x2D, 0x3D, 0x20, 0x30];
        // A slow ramp: each grey is a 36-pixel wide band, so no band edge is near the sampled centre.
        let out = run(|x, _| greys[(x / 37).min(greys.len() - 1)]);
        for band in 0..greys.len() {
            let x = band * 37 + 18;
            let (r, g, b) = px(&out, x, 50);
            let (_, i, q) = rgb_to_yiq(r, g, b);
            assert!(i.abs() < 3.0 && q.abs() < 3.0, "band {band}: I {i} Q {q}");
        }
    }

    /// The crosstalk a TV shows on the TASVideos test ROM: a 1-pixel checkerboard of black and
    /// white has no chroma in its source, yet the decoder reads the luma edges as subcarrier and
    /// produces colour at every pixel. The same two colours held flat produce none. Pinned: the
    /// checkerboard's chroma amplitude is above 10 on a 0..255 scale, the flat fields' is below 2.
    #[test]
    fn checkerboard_makes_chroma_and_flat_does_not() {
        let board = run(|x, y| if (x + y) % 2 == 0 { 0x30 } else { 0x0F });
        let mut peak = 0.0f32;
        for y in 40..44 {
            for x in 100..108 {
                peak = peak.max(chroma(&board, x, y));
            }
        }
        assert!(peak > 10.0, "checkerboard chroma peak {peak}");
        for flat in [0x30u8, 0x0F] {
            let out = run(|_, _| flat);
            for y in 40..44 {
                for x in 100..108 {
                    assert!(
                        chroma(&out, x, y) < 2.0,
                        "flat {flat:#04x} chroma {}",
                        chroma(&out, x, y)
                    );
                }
            }
        }
    }

    /// One black/white boundary leaks chroma only next to the boundary column: it is a
    /// band-limited edge, so far from it the decoded colour is grey again.
    #[test]
    fn edge_chroma_is_local() {
        let out = run(|x, _| if x < 128 { 0x0F } else { 0x30 });
        let near = (125..131)
            .map(|x| chroma(&out, x, 60))
            .fold(0.0f32, f32::max);
        assert!(near > 5.0, "edge chroma {near}");
        assert!(chroma(&out, 60, 60) < 2.0 && chroma(&out, 200, 60) < 2.0);
    }

    /// Two runs over the same frame are bit-identical: no randomness, no state kept between frames.
    #[test]
    fn output_is_deterministic() {
        let idx: Vec<u8> = (0..N).map(|i| ((i * 7 + i / 256) % 64) as u8).collect();
        let emph: Vec<u8> = (0..N).map(|i| (i / 1000 % 8) as u8).collect();
        let f = NtscFilter::new();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        f.apply(&idx, &emph, &mut a);
        NtscFilter::new().apply(&idx, &emph, &mut b);
        assert_eq!(a, b);
        assert_eq!(a.len(), N * 3);
    }

    /// Emphasis reaches the encoder: the same colour under red emphasis decodes differently from
    /// without it, and matches the plain `emphasized_rgb` within the flat-field tolerance.
    #[test]
    fn emphasis_is_honoured() {
        let idx = vec![0x20u8; N];
        let mut plain = Vec::new();
        let mut red = Vec::new();
        let f = NtscFilter::new();
        f.apply(&idx, &vec![0; N], &mut plain);
        f.apply(&idx, &vec![1; N], &mut red);
        let (r, g, b) = emphasized_rgb(0x20, 1);
        let o = (100 * SCREEN_WIDTH + 100) * 3;
        assert_ne!(plain[o..o + 3], red[o..o + 3]);
        assert!((f32::from(red[o + 1]) - f32::from(g)).abs() <= 3.0);
        assert!(
            (f32::from(red[o]) - f32::from(r)).abs() <= 3.0
                && (f32::from(red[o + 2]) - f32::from(b)).abs() <= 3.0
        );
    }
}
