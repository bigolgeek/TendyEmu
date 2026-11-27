//! The wrapper: what [`tendy_core::Machine`] asks of it, and nothing beyond it (DESIGN §2.10, §8 #7).
//!
//! Every assertion here compares the wrapper with the source machine it wraps. A rename or a
//! re-implementation that changes a byte shows up as a difference; a conversion that is only a
//! conversion does not.

mod common;

use tendy_core::{Buttons as Logical, Machine as _};
use tendy_gb::joypad::Buttons;
use tendy_gb::palette::{PaletteName, table};
use tendy_gb::{GbMachine, GbModel, GbOptions, native_buttons};

/// A machine booted from a ROM that does nothing: `common::entry(&[])` jumps to `$0150` and loops.
fn wrapper(model: GbModel) -> GbMachine {
    let rom = common::synth_rom(&common::entry(&[]));
    GbMachine::new(
        rom,
        GbOptions {
            model,
            ..GbOptions::default()
        },
    )
    .expect("a synthesized ROM is a valid cartridge")
}

#[test]
fn gb_machine_checksum_is_native() {
    let rom = common::synth_rom(&common::entry(&[]));
    let mut machine = GbMachine::new(rom.clone(), GbOptions::default()).expect("valid cartridge");
    let mut core = common::machine(rom);
    for _ in 0..3 {
        machine.run_frame();
        core.run_frame();
    }
    assert_eq!(
        machine.frame_checksum(),
        core.frame_checksum(),
        "the wrapper must report the source's frame checksum, not one of its own"
    );
}

#[test]
fn gb_buttons_map() {
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
        assert_eq!(native_buttons(logical), native, "{logical:?} mapped wrong");
    }
    let together = Logical::UP.with(Logical::A).with(Logical::START);
    assert_eq!(
        native_buttons(together),
        Buttons::UP | Buttons::A | Buttons::START
    );
    // The Game Boy has no X, Y, L or R: they press nothing.
    for extra in [Logical::X, Logical::Y, Logical::L, Logical::R] {
        assert_eq!(
            native_buttons(extra),
            Buttons::NONE,
            "{extra:?} must not press"
        );
    }
    assert_eq!(native_buttons(Logical::NONE), Buttons::NONE);
}

#[test]
fn gb_frame_rgba_complete() {
    let machine = wrapper(GbModel::Auto);
    let rom = common::synth_rom(&common::entry(&[]));
    let mut core = common::machine(rom);
    let mut machine = machine;
    machine.run_frame();
    core.run_frame();
    machine.run_frame();
    core.run_frame();
    let shades = core.frame().to_vec();
    let frame = machine.frame();
    assert_eq!((frame.width, frame.height), (160, 144));
    assert!(frame.is_complete(), "160x144 RGBA at four bytes a pixel");
    let table = table(PaletteName::default());
    for (index, &shade) in shades.iter().enumerate() {
        let want = table[usize::from(shade & 3)];
        assert_eq!(
            &frame.rgba[index * 4..index * 4 + 4],
            &want,
            "pixel {index} (shade {shade}) is not the palette's colour"
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

#[test]
fn gb_state_blob_is_native() {
    let rom = common::synth_rom(&common::entry(&[]));
    let mut machine = GbMachine::new(rom.clone(), GbOptions::default()).expect("valid cartridge");
    let mut core = common::machine(rom);
    for _ in 0..3 {
        machine.run_frame();
        core.run_frame();
    }
    let blob = machine.save_state();
    assert_eq!(
        &blob[..4],
        b"GBST",
        "the blob must be the source's own container, magic first"
    );
    assert!(
        blob == core.encode_state(),
        "the wrapper must hand the source's state through untouched"
    );
}

#[test]
fn sgb_border_switches_frame_size() {
    let mut machine = wrapper(GbModel::Sgb);
    machine.run_frame();
    let plain = machine.frame();
    assert_eq!((plain.width, plain.height), (160, 144));
    machine.set_sgb_border(true);
    let bordered = machine.frame();
    assert_eq!((bordered.width, bordered.height), (256, 224));
    assert!(bordered.is_complete(), "256x224 RGBA at four bytes a pixel");
    machine.set_sgb_border(false);
    let plain_again = machine.frame();
    assert_eq!((plain_again.width, plain_again.height), (160, 144));
}
