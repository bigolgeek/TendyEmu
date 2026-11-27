//! The CPU lane's pinned picture: a cartridge that exercises the ALU, the widths and the decimal
//! mode for a fixed number of frames, whose frame checksum is held against a recorded value
//!
//! The test is `#[ignore]`d because it pins a **picture**, and a picture needs a renderer: the PPU
//! lane (`src/ppu/*`) is a separate job and its renderer is not merged yet, so every frame this
//! machine produces is blank and its checksum says nothing about the CPU. The body runs the moment
//! the PPU lane lands, and the pin below is filled from that first green run
//! (`cargo test --test image_cpu -- --ignored`), which is §11.1's "recorded from the run that first
//! made them green".

mod common;

use tendy_snes::Machine;
use tendy_snes::cart::Cartridge;

/// The running frame checksum of the image, recorded from the first green run after the PPU lane
/// merges. Zero is not a frame checksum, so an unfilled pin cannot pass by accident.
const PINNED: u64 = 0;
/// How many frames the picture is taken after.
const FRAMES: u64 = 90;

/// A program that keeps the 65816 busy in a way a picture can show: 16-bit and 8-bit arithmetic,
/// a decimal sum and a shift, each stored into the WRAM buffer at `$00:20xx`, in a loop.
fn program() -> Vec<u8> {
    vec![
        0xC2, 0x30, // rep #$30      16-bit A and X
        0xA2, 0x00, 0x00, // ldx #$0000
        0xE8, // inx
        0x8E, 0x00, 0x20, // stx $2000     the counter
        0xA9, 0x99, 0x19, // lda #$1999
        0x18, // clc
        0x69, 0x01, 0x00, // adc #$0001    $199A, carry clear
        0x8D, 0x02, 0x20, // sta $2002
        0xE2, 0x20, // sep #$20      8-bit A
        0xF8, // sed
        0xA9, 0x09, // lda #$09
        0x18, // clc
        0x69, 0x01, // adc #$01      decimal: $10
        0x8D, 0x04, 0x20, // sta $2004
        0xD8, // cld
        0xC2, 0x20, // rep #$20
        0xA9, 0x00, 0x40, // lda #$4000
        0x0A, // asl A          $8000, carry set
        0x8D, 0x06, 0x20, // sta $2006
        0x4C, 0x03, 0x80, // jmp $8003     back to the top of the program
    ]
}

#[test]
#[ignore = "pins a picture; the PPU lane (src/ppu/*) must land before a frame has pixels"]
fn cpu_alu_image_matches_the_pinned_checksum() {
    assert_ne!(
        PINNED, 0,
        "the pin is recorded from the first green run after the PPU lane merges (§11.1)"
    );
    let rom = common::synth_lorom(0x8000, &program());
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    for _ in 0..FRAMES {
        machine.run_frame();
    }
    assert_eq!(machine.frames, FRAMES);
    assert_eq!(
        machine.frame_checksum(),
        PINNED,
        "the {FRAMES}-frame ALU image's checksum is pinned in this file"
    );
}
