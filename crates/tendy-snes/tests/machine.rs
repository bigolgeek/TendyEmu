//! The `tendy-core::Machine` wrapper's own two tests (DESIGN §8, #10): the pad map and the RGBA
//! frame. The inertness of the emulation itself lives in `inertness.rs`.

mod common;

use tendy_core::{Buttons, Machine as _};
use tendy_snes::SnesMachine;
use tendy_snes::picture::bgr555_to_rgba;

/// Every logical button reaches exactly the source bit DESIGN §2.5 names, and no other.
#[test]
fn snes_buttons_map() {
    let mut wrapper = SnesMachine::new(common::synth_lorom(0x8000, &[0x80, 0xFE]))
        .expect("the synthesized LoROM loads");
    let expected = [
        (Buttons::UP, 1u16 << 11),
        (Buttons::DOWN, 1 << 10),
        (Buttons::LEFT, 1 << 9),
        (Buttons::RIGHT, 1 << 8),
        (Buttons::A, 1 << 7),
        (Buttons::B, 1 << 15),
        (Buttons::X, 1 << 6),
        (Buttons::Y, 1 << 14),
        (Buttons::L, 1 << 5),
        (Buttons::R, 1 << 4),
        (Buttons::START, 1 << 12),
        (Buttons::SELECT, 1 << 13),
    ];

    for (logical, bit) in expected {
        wrapper.set_buttons(logical);
        assert_eq!(
            wrapper.core.buttons().0,
            bit,
            "{logical:?} must reach bit {bit} of $4218 and nothing else"
        );
    }

    let all = expected
        .iter()
        .fold(Buttons::NONE, |pad, (b, _)| pad.with(*b));
    wrapper.set_buttons(all);
    assert_eq!(wrapper.core.buttons().0, 0xFFF0, "all twelve bits at once");
}

/// `frame()` hands over a complete 256x224 RGBA8888 buffer, converted from the native BGR555 frame
/// exactly as `picture::bgr555_to_rgba` does.
#[test]
fn snes_frame_rgba_complete() {
    let mut wrapper = SnesMachine::new(common::synth_lorom(0x8000, &[0x80, 0xFE]))
        .expect("the synthesized LoROM loads");
    wrapper.run_frame();
    let native: Vec<u16> = wrapper.core.frame().to_vec();
    let frame = wrapper.frame();

    assert_eq!((frame.width, frame.height), (256, 224));
    assert!(frame.is_complete(), "rgba is width * height * 4 bytes");
    let mut expected = vec![0u8; native.len() * 4];
    bgr555_to_rgba(&native, &mut expected);
    assert_eq!(
        frame.rgba, expected,
        "the boundary pixels are the native ones"
    );
    for pixel in frame.rgba.as_chunks::<4>().0 {
        assert_eq!(pixel[3], 0xFF, "every boundary pixel is opaque");
    }
}

/// The boundary's mask says "bit n plays"; the DSP's register says "bit n is silent". The wrapper
/// inverts, so a voice the app leaves on is heard.
#[test]
fn snes_channel_mask_is_inverted() {
    let mut wrapper = SnesMachine::new(common::synth_lorom(0x8000, &[0x80, 0xFE]))
        .expect("the synthesized LoROM loads");
    wrapper.set_channel_mask(0b0000_0010);
    assert_eq!(
        wrapper.core.bus.apu.dsp.mask, 0b1111_1101,
        "only voice 1 plays, so only voice 1 escapes the mute mask"
    );
}
