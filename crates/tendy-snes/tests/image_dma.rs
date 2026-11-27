//! The CPU lane's second pinned picture: a cartridge whose HDMA channel rewrites a background
//! scroll every line, so the picture depends on the per-line window, the table state machine and
//! the 8-clocks-a-byte rule together.
//!
//! Like `image_cpu` it is `#[ignore]`d because it pins a **picture**: the PPU lane (`src/ppu/*`) is
//! where pixels come from, and until it is merged every frame here is blank. The pin is filled from
//! the first green run after that merge (`cargo test --test image_dma -- --ignored`).

mod common;

use tendy_snes::Machine;
use tendy_snes::cart::Cartridge;

/// The running frame checksum of the image, recorded from the first green run after the PPU lane
/// merges. Zero is not a frame checksum, so an unfilled pin cannot pass by accident.
const PINNED: u64 = 0;
/// How many frames the picture is taken after.
const FRAMES: u64 = 90;

/// The HDMA table lives in the cartridge at `$00:9000`, which is ROM offset `$1000`.
const TABLE_ROM_OFFSET: usize = 0x1000;
/// An `88h` entry moves eight units on eight consecutive lines; the `00h` ends the channel for the
/// frame, which is the termination a picture needs in order to be stable.
const TABLE: &[u8] = &[0x88, 0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0, 0x00];

/// A program that points HDMA channel 0 at the table with `DMAP=$00` (mode 0, direct, incrementing)
/// and `BBAD=$0D` (BG1 horizontal scroll, one byte a line), then enables it in `$420C`.
///
/// It runs with the boot state's `DB = 0`, so absolute addressing reaches `$00:43xx` without a long
/// form, and the table is in ROM, so the image depends only on the transfer itself.
fn program() -> Vec<u8> {
    vec![
        0xC2, 0x30, // rep #$30          16-bit A
        0xE2, 0x20, // sep #$20          8-bit A for the register writes
        0xA9, 0x00, // lda #$00          dmap: mode 0, direct, incrementing
        0x8D, 0x00, 0x43, // sta $4300
        0xA9, 0x0D, // lda #$0D          bbad: $210D, BG1HOFS
        0x8D, 0x01, 0x43, // sta $4301
        0xA9, 0x00, // lda #$00          A1T low
        0x8D, 0x02, 0x43, // sta $4302
        0xA9, 0x90, // lda #$90          A1T high: $9000
        0x8D, 0x03, 0x43, // sta $4303
        0xA9, 0x00, // lda #$00          A1T bank: $00, where the ROM is
        0x8D, 0x04, 0x43, // sta $4304
        0xA9, 0x01, // lda #$01          enable channel 0
        0x8D, 0x0C, 0x42, // sta $420C
        0x4C, 0x03, 0x80, // jmp $8003         loop
    ]
}

#[test]
#[ignore = "pins a picture; the PPU lane (src/ppu/*) must land before a frame has pixels"]
fn dma_hdma_image_matches_the_pinned_checksum() {
    assert_ne!(
        PINNED, 0,
        "the pin is recorded from the first green run after the PPU lane merges (§11.1)"
    );
    let mut rom = common::synth_lorom(0x8000, &program());
    rom[TABLE_ROM_OFFSET..TABLE_ROM_OFFSET + TABLE.len()].copy_from_slice(TABLE);
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    for _ in 0..FRAMES {
        machine.run_frame();
    }
    assert_eq!(machine.frames, FRAMES);
    assert_eq!(
        machine.frame_checksum(),
        PINNED,
        "the {FRAMES}-frame HDMA image's checksum is pinned in this file"
    );
}
