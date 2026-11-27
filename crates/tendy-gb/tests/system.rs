//! The `System` interface: one door into the machine, and the adapter behind it.
//!
//! The gate for that leg is that no number moves, which `acid2`, Mooneye, the sweep and the rest of
//! `tests/` prove; these two prove the adapter itself is wired to the same machine and is not a
//! parallel path that would be free to drift.

mod common;

use tendy_gb::system::System;
use tendy_gb::{Cartridge, Machine};

/// A cartridge that writes a pattern into VRAM, sets the background palette and then parks: the
/// frames it draws have structure, so a frame comparison compares a picture rather than 23 040
/// copies of the same shade.
fn pattern_rom() -> Vec<u8> {
    common::synth_rom(&common::entry(&[
        0x3E, 0xAA, // ld a,$AA
        0x21, 0x00, 0x80, // ld hl,$8000
        0x06, 0x10, // ld b,16
        0x22, // ld (hl+),a
        0x05, // dec b
        0x20, 0xFC, // jr nz,-4
        0x3E, 0xE4, // ld a,$E4
        0xE0, 0x47, // ldh ($47),a   BGP
        0x18, 0xFE, // jr -2
    ]))
}

/// The interface, built as `Box<dyn System>` the way `main.rs` builds it, reports exactly what the
/// `Machine` it wraps reports: same frame counter, same running checksum, same pixels, every frame.
#[test]
fn the_boxed_interface_is_the_machine_it_wraps() {
    let mut direct = Machine::new(Cartridge::new(pattern_rom()).expect("synthesized ROM"));
    let mut boxed: Box<dyn System> = Box::new(Machine::new(
        Cartridge::new(pattern_rom()).expect("synthesized ROM"),
    ));

    for frame in 0..40 {
        direct.run_frame();
        boxed.run_frame();
        assert_eq!(
            boxed.frames(),
            direct.frames,
            "frame {frame}: the interface counts frames the machine's way"
        );
        assert_eq!(
            boxed.frame_checksum(),
            direct.frame_checksum(),
            "frame {frame}: the running checksum is the same digest"
        );
        assert_eq!(
            boxed.frame().pixels,
            direct.frame(),
            "frame {frame}: the interface shows the machine's picture"
        );
    }

    // The picture is named once, with its own size, instead of rebuilt as a tuple per call site.
    let frame = boxed.frame();
    assert_eq!(
        (frame.width, frame.height),
        (tendy_gb::WIDTH, tendy_gb::HEIGHT)
    );
    assert_eq!(frame.len(), tendy_gb::FRAME_PIXELS);

    // The two facts that key a `.sav`, a profile and a cheat section.
    let identity = boxed.cart_identity();
    assert_eq!(identity.title, "GB-TEST");
    assert_eq!(identity.crc32, direct.bus.cart.rom_crc32);
    assert_eq!(boxed.console_name(), "Game Boy (DMG)");

    // The accessors the debugger panel reads answer through the trait as well. `pc` and `sp` are
    // 32 bits wide (H3): a Game Boy's are widened into the low half, never truncated.
    assert_eq!(boxed.pc(), u32::from(direct.cpu.pc));
    assert_eq!(boxed.sp(), u32::from(direct.cpu.sp));
    // The same four facts `--trace-pc` prints, taken from a Game Boy: no link register, no modes,
    // ARM/Thumb does not apply.
    assert_eq!(
        boxed.cpu_state(),
        tendy_gb::CpuState {
            pc: u32::from(direct.cpu.pc),
            sp: u32::from(direct.cpu.sp),
            lr: None,
            mode: "none",
            thumb: false,
        }
    );
    assert_eq!(
        boxed.cpu_regs(),
        tendy_gb::system::CpuRegs {
            a: direct.cpu.a,
            f: direct.cpu.f.0,
            b: direct.cpu.b,
            c: direct.cpu.c,
            d: direct.cpu.d,
            e: direct.cpu.e,
            h: direct.cpu.h,
            l: direct.cpu.l,
        }
    );
    assert_eq!(boxed.cycles(), direct.bus.cycles);
    assert_eq!(boxed.booting(), direct.booting());
}

/// `save_state` → `load_state` through the trait restores the same bytes it saved, and a state that
/// is short or corrupt is refused with an error rather than a panic.
#[test]
fn a_state_round_trips_through_the_interface_and_a_short_one_is_refused() {
    let mut system: Box<dyn System> = Box::new(Machine::new(
        Cartridge::new(pattern_rom()).expect("synthesized ROM"),
    ));
    system.set_sample_rate(48_000);
    for _ in 0..20 {
        system.run_frame();
    }

    let saved = system.save_state();
    for _ in 0..5 {
        system.run_frame();
    }
    assert_ne!(
        system.save_state(),
        saved,
        "five frames later is a different timeline"
    );

    system
        .load_state(&saved)
        .expect("the interface's own state loads");
    assert_eq!(
        system.save_state(),
        saved,
        "the state round-trips byte for byte, so a rewind step cannot drift"
    );

    // Every prefix of a state is truncated or corrupt, and each one is an error rather than a panic.
    for cut in [0, 4, 13, saved.len() / 2, saved.len() - 1] {
        assert!(
            system.load_state(&saved[..cut]).is_err(),
            "a state cut to {cut} of {} bytes was accepted",
            saved.len()
        );
    }
    // Not a state at all: the magic is refused before anything is read out of it.
    assert!(system.load_state(&[0u8; 64]).is_err());
    // None of that touched the running machine.
    assert_eq!(system.save_state(), saved);
}
