//! The 256×224 border, `SgbFrame::compose`, `MASK_EN`'s three states and the LCD edge.
//!
//! Every expectation below is read off Pan Docs' *PCT_TRN* sentence and this file's arithmetic, not
//! off `compose`'s output: "The 32×28 map entries correspond to 256×224 pixels of the Super NES
//! screen. The 20×18 entries in the center of the 32×28 area should be set to a blank (solid color
//! 0) tile as transparent space for the Game Boy window to be displayed inside." So the picture is
//! 256×224, the window is 160×144, and "in the center" puts it at `(256-160)/2, (224-144)/2` =
//! **48, 40**. The window's last pixel is therefore `(48+160-1, 40+144-1)` = **(207, 183)**, the
//! border pixel inside the last row/column at `(47, 39)`, and the first border pixel past the
//! window's bottom-right corner at `(208, 184)`.
//!
//! **Clean room.** Both borders in this file are generated here: `marker_border` is one word
//! repeated, and `gradient_border` is `x | y << 8` per pixel. Nothing traced, downloaded or dumped
//! is asserted on — the art is the repository's own design, asserted in `tests/sgb_art.rs`,
//! and no test here depends on its words.
//!
//! The tables are the gate: each row is a pixel or a `MASK_EN` value with a hand-written expected
//! word, so a compositor that shifted the window by a tile, transposed it, or wrote only part of a
//! row fails on a named coordinate rather than on a checksum.

mod common;

use tendy_gb::sgb::{
    Border, DEFAULT_RAMP, MaskState, Packet, SGB_FRAME_HEIGHT, SGB_FRAME_PIXELS, SGB_FRAME_WIDTH,
    Sgb, SgbFrame, WINDOW_HEIGHT, WINDOW_WIDTH, WINDOW_X, WINDOW_Y,
};
use tendy_gb::state::{StateError, StateReader, StateWriter};

// ---------------------------------------------------------------- the borders, generated here

/// The border word in the marker border, and the screen word in the marker screen.
const BORDER: u16 = 0x0011;
const SCREEN: u16 = 0x0022;

/// A border that is one word everywhere except the window, where it is `$0000` — the transparent
/// hole Pan Docs, *PCT_TRN* requires of a border's centre ("set to a blank (solid color 0) tile as
/// transparent space for the Game Boy window to be displayed inside"), so the screen shows through.
fn marker_border(word: u16) -> Border {
    let mut pixels = vec![word; SGB_FRAME_PIXELS];
    for y in WINDOW_Y..WINDOW_Y + WINDOW_HEIGHT {
        pixels[y * SGB_FRAME_WIDTH + WINDOW_X..][..WINDOW_WIDTH].fill(0);
    }
    Border::from_pixels(pixels)
}

/// A border whose every pixel is its own coordinate, `x | y << 8`: a copy of the art can be told
/// from a fill, and a pixel read from the wrong row cannot match. The window is the transparent
/// `$0000`, so the rectangle it covers is the screen's alone.
fn gradient_border() -> Border {
    let mut pixels = Vec::with_capacity(SGB_FRAME_PIXELS);
    for y in 0..SGB_FRAME_HEIGHT {
        for x in 0..SGB_FRAME_WIDTH {
            let in_window = (WINDOW_X..WINDOW_X + WINDOW_WIDTH).contains(&x)
                && (WINDOW_Y..WINDOW_Y + WINDOW_HEIGHT).contains(&y);
            pixels.push(if in_window {
                0
            } else {
                x as u16 | (y as u16) << 8
            });
        }
    }
    Border::from_pixels(pixels)
}

/// A screen of one word, 23 040 of them.
fn flat_gb(word: u16) -> Vec<u16> {
    vec![word; WINDOW_WIDTH * WINDOW_HEIGHT]
}

// ---------------------------------------------------------------- the geometry

/// The four constants and the empty frame they size (spec test 1).
#[test]
fn the_geometry_is_pan_docs_sentence_read_twice() {
    assert_eq!(SGB_FRAME_WIDTH, 256, "Pan Docs' 256 pixels of SNES screen");
    assert_eq!(SGB_FRAME_HEIGHT, 224, "Pan Docs' 224 pixels of SNES screen");
    assert_eq!(SGB_FRAME_PIXELS, 57_344, "256 x 224");
    assert_eq!(WINDOW_X, 48, "(256 - 160) / 2, tile 6 across");
    assert_eq!(WINDOW_Y, 40, "(224 - 144) / 2, tile 5 down");
    assert_eq!(WINDOW_WIDTH, 160);
    assert_eq!(WINDOW_HEIGHT, 144);

    let fresh = SgbFrame::new();
    assert_eq!(fresh.pixels().len(), 57_344);
    assert!(
        fresh.pixels().iter().all(|&w| w == 0),
        "a fresh frame is a black 256x224 picture"
    );
}

/// The composed picture's edges: the table is the leg's gate (spec test 2).
///
/// The border is `0x0011` everywhere and the screen is `0x0022`, so a row is one word's worth of
/// comparison and every coordinate below is written by hand from the geometry in the module
/// comment — `48 + 160 - 1 = 207`, `40 + 144 - 1 = 183`, `47 = 48 - 1`, `208 = 48 + 160`,
/// `39 = 40 - 1`, `184 = 40 + 144`, `255 = 256 - 1`, `223 = 224 - 1`.
#[test]
fn the_composed_picture_at_every_edge_the_geometry_names() {
    // (x, y, expected, what the coordinate is)
    const ROWS: [(usize, usize, u16, &str); 14] = [
        (0, 0, BORDER, "picture's top-left corner"),
        (255, 0, BORDER, "picture's top-right corner"),
        (0, 223, BORDER, "picture's bottom-left corner"),
        (255, 223, BORDER, "picture's bottom-right corner"),
        (
            48,
            40,
            SCREEN,
            "window's top-left pixel, the first of the screen",
        ),
        (207, 40, SCREEN, "window's top-right: 48 + 160 - 1"),
        (48, 183, SCREEN, "window's bottom-left: 40 + 144 - 1"),
        (207, 183, SCREEN, "the last window pixel"),
        (47, 40, BORDER, "one pixel left of the window"),
        (208, 40, BORDER, "one pixel right of the window: 48 + 160"),
        (48, 39, BORDER, "one pixel above the window: 40 - 1"),
        (48, 184, BORDER, "one pixel below the window: 40 + 144"),
        (
            47,
            39,
            BORDER,
            "the inside border corner, last row/column before the window",
        ),
        (
            208,
            184,
            BORDER,
            "the first border pixel past the window's far corner",
        ),
    ];

    let border = marker_border(BORDER);
    let mut frame = SgbFrame::new();
    frame.compose(&border, &flat_gb(SCREEN), MaskState::Active);

    for &(x, y, expected, what) in ROWS.iter() {
        assert_eq!(
            frame.pixels()[y * SGB_FRAME_WIDTH + x],
            expected,
            "({x}, {y}) is {what}: expected {expected:#06x}"
        );
    }
    assert_eq!(ROWS.len(), 14, "the table lost a row");
}

/// The window is the whole 160×144 rectangle at 48,40 — no row transposed, no column off by one.
///
/// Every screen word is its own coordinate (`x | y << 8`), and every pixel outside the rectangle
/// must still be the border's own `x | y << 8`: a compositor that copied the rows in the wrong
/// order, used the window's width as its stride, or painted past the rectangle fails here.
#[test]
fn the_window_is_exactly_the_rectangle_at_48_40() {
    let mut gb = vec![0u16; WINDOW_WIDTH * WINDOW_HEIGHT];
    for y in 0..WINDOW_HEIGHT {
        for x in 0..WINDOW_WIDTH {
            gb[y * WINDOW_WIDTH + x] = x as u16 | (y as u16) << 8;
        }
    }
    let mut frame = SgbFrame::new();
    frame.compose(&gradient_border(), &gb, MaskState::Active);

    let mut inside = 0;
    let mut outside = 0;
    for y in 0..SGB_FRAME_HEIGHT {
        for x in 0..SGB_FRAME_WIDTH {
            let got = frame.pixels()[y * SGB_FRAME_WIDTH + x];
            let in_window = (WINDOW_X..WINDOW_X + WINDOW_WIDTH).contains(&x)
                && (WINDOW_Y..WINDOW_Y + WINDOW_HEIGHT).contains(&y);
            if in_window {
                let screen = (x - WINDOW_X) as u16 | ((y - WINDOW_Y) as u16) << 8;
                assert_eq!(got, screen, "window pixel ({x}, {y})");
                inside += 1;
            } else {
                let art = x as u16 | (y as u16) << 8;
                assert_eq!(got, art, "border pixel ({x}, {y})");
                outside += 1;
            }
        }
    }
    assert_eq!(inside, 160 * 144, "the window is 23 040 pixels");
    assert_eq!(
        outside,
        57_344 - 160 * 144,
        "the border is the other 34 304"
    );
}

/// One whole row, written out by hand: the border's 48 pixels, the screen's 160, the border's 48.
#[test]
fn a_whole_row_is_border_then_screen_then_border() {
    let mut frame = SgbFrame::new();
    frame.compose(&marker_border(BORDER), &flat_gb(SCREEN), MaskState::Active);

    let mut expected = vec![BORDER; 256];
    expected[48..208].fill(SCREEN);

    // The first window row and the last, and the two rows immediately outside it.
    for y in [39usize, 40, 183, 184] {
        let row = &frame.pixels()[y * 256..(y + 1) * 256];
        if y == 39 || y == 184 {
            assert!(
                row.iter().all(|&w| w == BORDER),
                "row {y} is outside the window and must be all border"
            );
        } else {
            assert_eq!(row, &expected[..], "row {y} is border, screen, border");
        }
    }
}

/// Every pixel of the picture is written by one `compose`, and the counts are the geometry's.
///
/// The fresh frame is all zero and the two words are neither zero nor equal, so a pixel left
/// untouched — a partial row, a gap between window rows — shows up as a count that is short.
#[test]
fn every_pixel_of_the_picture_is_written() {
    let mut frame = SgbFrame::new();
    frame.compose(&marker_border(BORDER), &flat_gb(SCREEN), MaskState::Active);
    let pixels = frame.pixels();
    assert_eq!(
        pixels.iter().filter(|&&w| w == SCREEN).count(),
        160 * 144,
        "the screen is 23 040 words"
    );
    assert_eq!(
        pixels.iter().filter(|&&w| w == BORDER).count(),
        57_344 - 160 * 144,
        "the border is the other 34 304"
    );
    assert!(
        pixels.iter().all(|&w| w != 0),
        "a pixel kept the fresh frame's zero: compose left part of the picture unwritten"
    );
}

/// A screen of one colour still composes a border: no early-out skips the border (the
/// fourth case).
#[test]
fn a_one_colour_screen_still_composes_a_border() {
    let mut frame = SgbFrame::new();
    frame.compose(&marker_border(BORDER), &flat_gb(0x0044), MaskState::Active);
    for (x, y) in [(0, 0), (255, 0), (0, 223), (255, 223), (47, 40), (208, 184)] {
        assert_eq!(
            frame.pixels()[y * SGB_FRAME_WIDTH + x],
            BORDER,
            "({x}, {y}) must still be the border, not the screen's colour"
        );
    }
    assert_eq!(frame.pixels()[40 * 256 + 48], 0x0044);
}

// ---------------------------------------------------------------- MASK_EN, the three states

/// `MASK_EN` 1: the previous picture stays, byte for byte (spec test 3).
///
/// A frozen frame writes **nothing**: not the new border, not the new screen. The second compose
/// passes a different border (`0x0033`) and a different screen (`0x0044`), so a border repaint
/// would be visible even though the screen was not drawn.
#[test]
fn frozen_keeps_the_previous_picture_byte_for_byte() {
    let mut frame = SgbFrame::new();
    frame.compose(&marker_border(BORDER), &flat_gb(SCREEN), MaskState::Active);
    let kept = frame.pixels().to_vec();

    frame.compose(&marker_border(0x0033), &flat_gb(0x0044), MaskState::Frozen);
    assert_eq!(frame.pixels(), &kept[..], "a frozen frame moved");

    // And a frozen frame before anything was drawn leaves the fresh black picture alone.
    let mut fresh = SgbFrame::new();
    fresh.compose(&marker_border(0x0033), &flat_gb(0x0044), MaskState::Frozen);
    assert!(fresh.pixels().iter().all(|&w| w == 0));
}

/// `MASK_EN` 2 and 3: one colour across the window, the border untouched (spec test 4).
///
/// The expected window word is the `Blanked` payload, and the border is still `0x0011` at the
/// picture's corner and at `(47, 40)` — the pixel that proves the fill stopped at column 48.
#[test]
fn blanked_fills_the_window_and_keeps_the_border() {
    for (colour, why) in [
        (0x0000u16, "MASK_EN 2: black"),
        (0x1234, "MASK_EN 3's colour 0"),
    ] {
        let mut frame = SgbFrame::new();
        frame.compose(
            &marker_border(BORDER),
            &flat_gb(SCREEN),
            MaskState::Blanked(colour),
        );

        let mut window = 0;
        for y in 0..SGB_FRAME_HEIGHT {
            for x in 0..SGB_FRAME_WIDTH {
                let got = frame.pixels()[y * SGB_FRAME_WIDTH + x];
                let in_window = (WINDOW_X..WINDOW_X + WINDOW_WIDTH).contains(&x)
                    && (WINDOW_Y..WINDOW_Y + WINDOW_HEIGHT).contains(&y);
                if in_window {
                    assert_eq!(
                        got, colour,
                        "{why}: window pixel ({x}, {y}) is not the blank's colour"
                    );
                    window += 1;
                }
            }
        }
        assert_eq!(window, 160 * 144, "{why}: the whole window is blanked");
        assert_eq!(
            frame.pixels()[0],
            BORDER,
            "{why}: the picture's corner is border"
        );
        assert_eq!(
            frame.pixels()[40 * 256 + 47],
            BORDER,
            "{why}: (47, 40) is border"
        );
        assert_eq!(
            frame.pixels()[39 * 256 + 48],
            BORDER,
            "{why}: (48, 39) is border"
        );
    }
}

/// `MASK_EN` through the dispatch, five values on one table (spec test 5).
///
/// The header is `$17 << 3 | 1` = `0xB9`: command code `$17` is `MASK_EN` and the length is one
/// packet, Pan Docs' "Command Code × 8 + Length". `$17`'s byte 1 is the mask value: "0 Cancel Mask
/// (Display activated), 1 Freeze Screen (Keep displaying current picture), 2 Blank Screen (Black),
/// 3 Blank Screen (Color 0)"; 4 is undefined and the dispatch already cancels, so it is pinned
/// here as `Active` rather than left to drift.
#[test]
fn mask_en_through_the_dispatch() {
    const ROWS: [(u8, MaskState, &str); 5] = [
        (0, MaskState::Active, "0: cancel mask"),
        (1, MaskState::Frozen, "1: freeze screen"),
        (2, MaskState::Blanked(0x0000), "2: blank screen, black"),
        (
            3,
            MaskState::Blanked(0x7FFF),
            "3: blank screen, colour 0 = our grey ramp's first step",
        ),
        (4, MaskState::Active, "4: undefined, cancels"),
    ];
    assert_eq!(DEFAULT_RAMP[0], 0x7FFF, "the value row 3 expects");

    for &(value, expected, why) in ROWS.iter() {
        let mut bytes = [0u8; 16];
        bytes[0] = 0xB9; // $17 << 3 | 1
        bytes[1] = value;
        let packet = Packet { bytes };

        // From a frozen SGB, so `0` has an effect to show rather than agreeing by default.
        let mut sgb = Sgb::on();
        sgb.set_mask(MaskState::Frozen);
        sgb.apply(&packet, &[]);
        assert_eq!(sgb.mask(), expected, "MASK_EN {value} is {why}");
    }
}

/// The LCD edge: off freezes, on cancels — and only while SGB is on (spec test 6).
#[test]
fn the_lcd_edge_freezes_and_unfreezes_with_sgb_on() {
    let mut m = common::machine(common::synth_rom(&[]));
    assert_eq!(m.bus.ppu.lcdc, 0x91, "the PPU starts with the LCD on");
    m.bus.ppu.sgb = Sgb::on();

    m.bus.write(0xFF40, 0x11); // LCDC.7 1 -> 0
    assert_eq!(
        m.bus.ppu.sgb.mask(),
        MaskState::Frozen,
        "LCD off must freeze the window"
    );
    m.bus.write(0xFF40, 0x00); // still off: no edge, so no change
    assert_eq!(m.bus.ppu.sgb.mask(), MaskState::Frozen);

    m.bus.write(0xFF40, 0x91); // LCDC.7 0 -> 1
    assert_eq!(
        m.bus.ppu.sgb.mask(),
        MaskState::Active,
        "LCD on must cancel the mask"
    );
    m.bus.write(0xFF40, 0x93); // still on: no edge
    assert_eq!(m.bus.ppu.sgb.mask(), MaskState::Active);
}

/// With SGB off the same two writes change nothing: the mask stays `Active` and `tests/sgb.rs`'s
/// picture is still `main`'s.
///
/// The two writes are made **after** the three frames, so the checksum is the same three frames
/// `tests/sgb.rs` folds: a write before them would be an LCD off/on, whose blank first frame is the
/// PPU's documented behaviour and not this hook's — the test is about the hook, so it is placed
/// where only the hook could move the number.
#[test]
fn an_sgb_off_machine_ignores_the_lcd_edge() {
    let mut m = common::machine(common::synth_rom(&[]));
    assert!(!m.bus.ppu.sgb.enabled(), "this machine never turns SGB on");
    for (i, byte) in m.bus.ppu.vram.iter_mut().enumerate().take(16) {
        *byte = (i as u8).wrapping_mul(17);
    }
    for i in 0..32 * 32 {
        m.bus.ppu.vram[0x1800 + i] = [0u8, 1, 2, 3][(i / 5) % 4];
    }
    m.bus.ppu.bgp = 0xE4;
    for _ in 0..3 {
        m.run_frame();
    }
    assert_eq!(
        m.frame_checksum(),
        0xf146_549e_efd3_9f45,
        "the picture is not tests/sgb.rs's"
    );

    m.bus.write(0xFF40, 0x11);
    assert_eq!(
        m.bus.ppu.sgb.mask(),
        MaskState::Active,
        "SGB off: no freeze"
    );
    m.bus.write(0xFF40, 0x91);
    assert_eq!(m.bus.ppu.sgb.mask(), MaskState::Active);
    assert_eq!(
        m.frame_checksum(),
        0xf146_549e_efd3_9f45,
        "an SGB-off LCD write moved the frame checksum"
    );
}

// ---------------------------------------------------------------- state

/// `MaskState` on the wire: one tag byte, plus a word for `Blanked` (spec test 7).
#[test]
fn mask_state_round_trips_and_refuses_an_unknown_tag() {
    let states = [
        (MaskState::Active, 1usize),
        (MaskState::Frozen, 1),
        (MaskState::Blanked(0x0000), 3),
        (MaskState::Blanked(0x7FFF), 3),
        (MaskState::Blanked(0x1234), 3),
    ];
    for (state, bytes_expected) in states {
        let mut w = StateWriter::new();
        state.save_state(&mut w);
        assert_eq!(
            w.len(),
            bytes_expected,
            "{state:?} is {bytes_expected} bytes"
        );
        let bytes = w.finish();

        let mut decoded = MaskState::Active;
        decoded
            .load_state(&mut StateReader::new(&bytes))
            .expect("a state this file wrote must decode");
        assert_eq!(decoded, state, "{state:?} did not survive the round trip");
    }

    // An unknown tag is a corrupt state, not a value to guess at.
    let mut held = MaskState::Blanked(0x1234);
    assert_eq!(
        held.load_state(&mut StateReader::new(&[9u8])),
        Err(StateError::BadValue)
    );
    assert_eq!(
        held,
        MaskState::Blanked(0x1234),
        "a refused state must leave the value it held"
    );
    // A `Blanked` tag with its word cut off is truncated, not a wild word.
    let mut decoded = MaskState::Active;
    assert_eq!(
        decoded.load_state(&mut StateReader::new(&[2u8])),
        Err(StateError::Truncated)
    );
}

/// The mask rides in the machine's snapshot: the two lines `Ppu` and `Machine` each carry.
#[test]
fn the_mask_survives_a_machine_snapshot() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb.set_mask(MaskState::Blanked(0x1234));
    let state = m.encode_state();

    let mut other = common::machine(common::synth_rom(&[]));
    assert_eq!(other.bus.ppu.sgb.mask(), MaskState::Active);
    other.decode_state(&state).expect("our own snapshot");
    assert_eq!(other.bus.ppu.sgb.mask(), MaskState::Blanked(0x1234));
}
