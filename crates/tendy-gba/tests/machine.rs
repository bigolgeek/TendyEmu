//! The wrapper: what [`tendy_core::Machine`] asks of it, and nothing beyond it (DESIGN §2.10, §8 #8).
//!
//! The frame test compares the wrapper with the source machine it wraps: a conversion that is only
//! a conversion shows up as equality, and any change to either side shows up as a difference.

mod common;

use tendy_core::{Buttons as Logical, Machine as _};
use tendy_gb::System;
use tendy_gb::joypad::{Buttons, Shoulders};
use tendy_gb::palette::rgb555;
use tendy_gba::GbaMachine;
use tendy_gba::cart::Cartridge;
use tendy_gba::{Gba, native_buttons};

/// The source machine on the same cartridge, for comparison.
fn core() -> Gba {
    let cart = Cartridge::new(common::mode3_image()).expect("the test image is a GBA cartridge");
    Gba::new(cart)
}

#[test]
fn gba_buttons_and_shoulders_map() {
    let pairs = [
        (Logical::UP, Buttons::UP),
        (Logical::DOWN, Buttons::DOWN),
        (Logical::LEFT, Buttons::LEFT),
        (Logical::RIGHT, Buttons::RIGHT),
        (Logical::A, Buttons::A),
        (Logical::B, Buttons::B),
        (Logical::SELECT, Buttons::SELECT),
        (Logical::START, Buttons::START),
    ];
    for (logical, native) in pairs {
        assert_eq!(
            native_buttons(logical),
            (native, Shoulders::NONE),
            "{logical:?} mapped wrong"
        );
    }
    assert_eq!(
        native_buttons(Logical::L),
        (Buttons::NONE, Shoulders::LEFT),
        "L is the source's left shoulder"
    );
    assert_eq!(
        native_buttons(Logical::R),
        (Buttons::NONE, Shoulders::RIGHT),
        "R is the source's right shoulder"
    );
    let together = Logical::UP
        .with(Logical::A)
        .with(Logical::START)
        .with(Logical::L)
        .with(Logical::R);
    assert_eq!(
        native_buttons(together),
        (
            Buttons::UP | Buttons::A | Buttons::START,
            Shoulders::LEFT | Shoulders::RIGHT
        )
    );
    // A Game Boy Advance has no X or Y: they press nothing.
    for extra in [Logical::X, Logical::Y] {
        assert_eq!(
            native_buttons(extra),
            (Buttons::NONE, Shoulders::NONE),
            "{extra:?} must not press"
        );
    }
    assert_eq!(
        native_buttons(Logical::NONE),
        (Buttons::NONE, Shoulders::NONE)
    );
}

#[test]
fn gba_frame_rgba_complete() {
    let mut machine =
        GbaMachine::new(common::mode3_image(), None).expect("the test image is a valid cartridge");
    let mut core = core();
    for _ in 0..2 {
        machine.run_frame();
        core.run_frame();
    }
    let words = core.frame().colours.to_vec();
    assert_eq!(
        words.len(),
        240 * 160,
        "the source frame is 240x160 colour words"
    );
    assert!(
        words.iter().any(|&word| word != 0),
        "the test image must draw something, or equality proves nothing"
    );
    let frame = machine.frame();
    assert_eq!((frame.width, frame.height), (240, 160));
    assert!(frame.is_complete(), "240x160 RGBA at four bytes a pixel");
    for (index, &word) in words.iter().enumerate() {
        assert_eq!(
            &frame.rgba[index * 4..index * 4 + 4],
            &rgb555(word),
            "pixel {index} (RGB555 {word:04x}) is not the source colour"
        );
    }
    assert!(
        frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 0xFF),
        "every pixel must be opaque"
    );
}
