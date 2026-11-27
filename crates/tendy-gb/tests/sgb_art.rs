//! The border art — one 256×224 picture, ours.
//!
//! Pan Docs fixes only the picture's size and the window's place (*PCT_TRN*: "The 32×28 map entries
//! correspond to 256×224 pixels of the Super NES screen", the Game Boy window 160×144 centred inside
//! it at 48,40). The art is this repository's: two 8-pixel rings — blue `$7C00` outermost, green
//! `$03E0` from 8 to 15 pixels in — four white `$7FFF` 16×16 corner blocks at (16,16), (224,16),
//! (16,184) and (224,184), and `$0000` everywhere else, the window included.
//!
//! Every expected word below is read off that design and its one rule — the distance of a pixel
//! `(x, y)` to the nearest edge is `min(x, 255 - x, y, 223 - y)` — not off `Border::new`'s output.
//! The points are named one at a time so a ring drawn a pixel out of place, an off-by-one at the
//! 8/16 boundary, or a corner block at the wrong offset fails on a coordinate rather than a checksum.

use tendy_gb::sgb::{
    Border, MaskState, SGB_FRAME_HEIGHT, SGB_FRAME_PIXELS, SGB_FRAME_WIDTH, SgbFrame,
};
use tendy_gb::state::fnv1a64_colours;

/// `(x, y)` as a row-major index, the layout every assertion below addresses.
fn at(x: usize, y: usize) -> usize {
    y * SGB_FRAME_WIDTH + x
}

/// The word at `(x, y)` of the border, or the panic naming the coordinate the art lacks.
fn word(border: &Border, x: usize, y: usize) -> u16 {
    border.pixels()[at(x, y)]
}

const BLUE: u16 = 0x7C00;
const GREEN: u16 = 0x03E0;
const WHITE: u16 = 0x7FFF;
const BLACK: u16 = 0x0000;

/// 1. The picture is 256×224 words: 57 344 of them, row-major, 256 to a row.
#[test]
fn the_picture_is_256_by_224_words() {
    let border = Border::new();
    assert_eq!(border.pixels().len(), 57_344);
    assert_eq!(border.pixels().len(), SGB_FRAME_PIXELS);
    assert_eq!(SGB_FRAME_WIDTH, 256);
    assert_eq!(SGB_FRAME_HEIGHT, 224);
}

/// 2. The two rings, at points on all four edges.
///
/// Blue: within 8 pixels of any edge, so `x = 0..=7` or `x = 248..=255` or `y = 0..=7` or
/// `y = 216..=223`. Green: 8 to 15 pixels in, so `x = 8..=15` or `x = 240..=247` or `y = 8..=15` or
/// `y = 208..=215`. Every point stays clear of the other axis so it tests one ring, not a corner.
#[test]
fn the_blue_ring_is_the_outermost_eight_pixels() {
    let border = Border::new();
    for (x, y) in [
        (0, 100),
        (7, 100),
        (248, 100),
        (255, 100),
        (100, 0),
        (100, 7),
        (100, 216),
        (100, 223),
    ] {
        assert_eq!(word(&border, x, y), BLUE, "({x},{y}) is in the blue ring");
    }
}

/// 2 (continued). The green ring: 8 to 15 pixels from the nearest edge.
#[test]
fn the_green_ring_is_pixels_eight_to_fifteen_in() {
    let border = Border::new();
    for (x, y) in [(8, 100), (15, 100), (240, 100), (100, 8), (100, 215)] {
        assert_eq!(word(&border, x, y), GREEN, "({x},{y}) is in the green ring");
    }
    // 16 pixels in is past both rings: plain black, on both axes.
    for (x, y) in [(16, 100), (100, 16), (100, 207)] {
        assert_eq!(word(&border, x, y), BLACK, "({x},{y}) is past both rings");
    }
}

/// 3. The four corner blocks: 16×16 white at (16,16), (224,16), (16,184) and (224,184).
///
/// The first block runs `x = 16..=31`, `y = 16..=31`; the top-right block's left edge is 256 - 32 =
/// 224 and its right edge 239; the bottom-left block's top edge is 224 - 40 = 184 and its bottom
/// edge 199. So each block's two corners are its first and last pixel, and one pixel outside each
/// of them must not be white.
#[test]
fn the_corner_blocks_are_sixteen_by_sixteen_white() {
    let border = Border::new();
    for (x, y) in [
        (16, 16),
        (31, 31),
        (224, 16),
        (239, 31),
        (16, 184),
        (31, 199),
        (224, 184),
        (239, 199),
    ] {
        assert_eq!(word(&border, x, y), WHITE, "({x},{y}) is a corner block");
    }
    // One pixel past each of a block's edges: the block stops dead at 16 pixels.
    for (x, y) in [(32, 16), (223, 16), (16, 32), (16, 183), (240, 16)] {
        assert_ne!(
            word(&border, x, y),
            WHITE,
            "({x},{y}) is outside every block"
        );
    }
}

/// 4. The window is untouched. Its first pixel is (48, 40) and its last (207, 183); the art leaves
///    the whole 160×144 rectangle black so a masked or frozen frame shows nothing of the border.
#[test]
fn the_game_boy_window_stays_black() {
    let border = Border::new();
    for (x, y) in [(47, 40), (48, 40), (128, 112), (207, 183), (208, 183)] {
        assert_eq!(
            word(&border, x, y),
            BLACK,
            "({x},{y}) is in or beside the window"
        );
    }
    // And the whole window, not just its corners: 160×144 of black.
    for y in 40..40 + 144 {
        for x in 48..48 + 160 {
            assert_eq!(word(&border, x, y), BLACK, "({x},{y}) is in the window");
        }
    }
}

/// 5. Deterministic, and a picture rather than a fill: the same call twice gives the same words,
///    and at least four distinct words are present.
#[test]
fn the_art_is_deterministic_and_not_one_colour() {
    assert!(Border::new() == Border::new(), "two calls, one picture");
    let border = Border::new();
    let mut distinct: Vec<u16> = border.pixels().to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() >= 4,
        "the picture has {} distinct words: {distinct:?}",
        distinct.len()
    );
}

/// 6. The regression pin: FNV-1a 64 over the picture's colour words, both bytes of each
///    little-endian. The number below was taken from this design with
///    `cargo test --test sgb_art -- --nocapture`; it is ours, not a Pan Docs value, and any later
///    change to the art must move it deliberately and update this line and the report.
#[test]
fn the_picture_hashes_to_the_pinned_value() {
    let border = Border::new();
    let hash = fnv1a64_colours(border.pixels());
    println!("fnv1a64_colours(Border::new()) = 0x{hash:016x}");
    assert_eq!(hash, 0xfe7a_a8b8_2e3c_0725);
}

/// 7. The compositor draws the art and the window over it: a screen of
///    `$0022` covers every window pixel, and the blue ring is still at (0,0).
#[test]
fn the_compositor_draws_the_art_around_the_window() {
    let mut frame = SgbFrame::new();
    frame.compose(&Border::new(), &vec![0x0022; 23_040], MaskState::Active);
    assert_eq!(frame.pixels()[at(48, 40)], 0x0022, "(48,40) is the window");
    assert_eq!(frame.pixels()[at(0, 0)], BLUE, "(0,0) is the blue ring");
}
