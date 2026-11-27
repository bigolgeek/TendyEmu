//! §5.1's falsifier, built before the renderer: one hand-built cartridge image that writes `$2100`
//! **inside** a visible line, so the user can look at one picture on a real console and see whether
//! a mid-line register write becomes visible mid-line (dot-exact hardware) or from the next line
//! boundary (v1's line-snapshot model).
//!
//! What the image does, in order:
//! 1. sets `$2100` to `$00` — forced blank off, brightness 0, so the whole screen is black;
//! 2. writes CGRAM 0 (the backdrop, which every mode displays) to white `$7FFF`, so that raising
//!    the brightness is visible;
//! 3. waits for 120 H-blank edges by polling `$4212` bit 6 (`$78` = 120 falling edges), which puts
//!    the CPU in dot 1 of a visible line;
//! 4. burns a counted delay of about 600 master clocks (twenty `DEY`/`BNE` turns), roughly dot 150 —
//!    well inside dots 22-277 — and
//! 5. writes `$2100` = `$0F` (brightness 15) exactly once, then spins.
//!
//! What the user should see on a console: black above, white below, and on the one line where the
//! write lands **either** a split (black for the first ~128 pixels, white for the rest) — which
//! falsifies the line-snapshot model — **or** a whole-line change, which confirms it. Either answer
//! settles the `UNVERIFIED:` rows of §4.6 and §5.11; §12.2 says which.
//!
//! The test only assembles and pins the image: it needs no CPU lane (the scaffold's stub CPU is
//! never asked to run it) and no PPU lane.

mod common;

use std::path::PathBuf;
use tendy_snes::cart::Cartridge;

/// The program, byte for byte. Assembled by hand for the 65816's reset state (emulation mode, `A`
/// 8-bit, `X`/`Y` 16-bit), addresses absolute, branches resolved on paper — see the block comment.
const PROGRAM: [u8; 57] = [
    0x78, // sei
    0xA9, 0x00, // lda #$00
    0x8D, 0x00, 0x21, // sta $2100   ; forced blank off, brightness 0 = black
    0xA9, 0x00, // lda #$00
    0x8D, 0x21, 0x21, // sta $2121   ; CGADD = 0 (the backdrop)
    0xA9, 0xFF, // lda #$FF
    0x8D, 0x22, 0x21, // sta $2122   ; CGDATA low
    0xA9, 0x7F, // lda #$7F
    0x8D, 0x22, 0x21, // sta $2122   ; CGDATA high -> backdrop white $7FFF
    0xA2, 0x00, 0x00, // ldx #$0000  ; the line counter
    0xAD, 0x12, 0x42, // lda $4212   ; count: wait for the H-blank flag
    0x29, 0x40, // and #$40
    0xF0, 0xF9, // beq back to the load
    0xAD, 0x12, 0x42, // lda $4212   ; wait_clear: and for it to fall
    0x29, 0x40, // and #$40
    0xD0, 0xF9, // bne back to the load
    0xE8, // inx
    0xE0, 0x78, 0x00, // cpx #$0078  ; 120 lines
    0xD0, 0xEC, // bne back to the counter's load
    0xA0, 0x14, 0x00, // ldy #$0014  ; 20 turns of the next two = about 600 clocks
    0x88, // dey
    0xD0, 0xFD, // bne back to the dey
    0xA9, 0x0F, // lda #$0F
    0x8D, 0x00, 0x21, // sta $2100   ; brightness 15, exactly once, mid-line
    0x80, 0xFE, // bra *
];

/// `target/brightness-probe.sfc`, beside the build, as §12.2 names it.
fn probe_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target");
    let _ = std::fs::create_dir_all(&path);
    path.push("brightness-probe.sfc");
    path
}

/// The byte pair `needle` occurs exactly `n` times in `hay`.
fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

mod probe_brightness {
    use super::*;

    /// §11.2, §12.2. Not a renderer test: it pins the image, its header and the three properties the
    /// console run depends on, and leaves the file for hand-over.
    #[test]
    fn mid_line_brightness_image_is_built_for_the_console() {
        assert_eq!(PROGRAM.len(), 57, "the program is 57 bytes (~64)");

        let rom = common::synth_lorom(0x8000, &PROGRAM);
        let cart = Cartridge::new(rom.clone()).expect("the probe image loads as a LoROM cartridge");

        // §8.2: a valid header — the checksum and its complement, both present and XORing to $FFFF.
        let header = 0x7FC0;
        let sum = u16::from_le_bytes([rom[header + 0x1E], rom[header + 0x1F]]);
        let not = u16::from_le_bytes([rom[header + 0x1C], rom[header + 0x1D]]);
        assert_ne!(sum, 0, "the probe image's checksum is written");
        assert_eq!(
            sum ^ not,
            0xFFFF,
            "the checksum complement pairs with the checksum (fullsnes \"SNES Header\")"
        );
        assert!(
            !cart.header.title.trim().is_empty(),
            "the header carries a title"
        );

        // The program, at $00:8000+3 behind `common::entry`'s `jmp $8003`.
        let code = &rom[3..3 + PROGRAM.len()];
        assert_eq!(code, &PROGRAM[..], "the program is in the image at $8003");

        // §12.2 (1): the line counter compares against 120, not 119 or 121.
        assert_eq!(
            count(code, &[0xE0, 0x78, 0x00]),
            1,
            "the probe counts 120 lines: `cpx #$0078`"
        );
        // §12.2 (2): the delay loop is twenty turns — roughly 600 master clocks, about dot 150.
        assert_eq!(
            count(code, &[0xA0, 0x14, 0x00]),
            1,
            "the delay is 20 turns of DEY/BNE = about 600 master clocks"
        );
        // §12.2 (3): brightness 0 on entry and brightness 15 exactly once, mid-line. Two stores to
        // `$2100` in the whole program, and the second is the `$0F` one.
        assert_eq!(
            count(code, &[0x8D, 0x00, 0x21]),
            2,
            "the program writes $2100 once at setup and once inside the line"
        );
        assert_eq!(
            count(code, &[0xA9, 0x0F, 0x8D, 0x00, 0x21]),
            1,
            "brightness 0 -> 15 is written exactly once, mid-line"
        );
        assert_eq!(
            count(code, &[0xA9, 0x00, 0x8D, 0x00, 0x21]),
            1,
            "the setup write is brightness 0 with forced blank off, so the screen starts black"
        );
        // White backdrop: CGRAM 0 = $7FFF, so raising the brightness is visible on every mode.
        assert_eq!(
            count(
                code,
                &[0xA9, 0xFF, 0x8D, 0x22, 0x21, 0xA9, 0x7F, 0x8D, 0x22, 0x21]
            ),
            1,
            "CGRAM 0 is set to white $7FFF (the backdrop every mode displays)"
        );

        // The picture the user looks at: black above, white below, and a split on the write's line
        // if the hardware is dot-exact. Written out for the hand-over.
        let path = probe_path();
        std::fs::write(&path, &rom).expect("the probe image is written to target/");
        let size = std::fs::metadata(&path)
            .expect("the probe image exists")
            .len();
        assert_eq!(size as usize, rom.len(), "the file is the whole image");
        let read_back = std::fs::read(&path).expect("the probe image reads back");
        assert_eq!(read_back, rom, "the file on disk is the image in memory");
        Cartridge::new(read_back).expect("the written image loads");
        println!(
            "brightness probe written to {} — black above, white below, and on the write's line a \
             mid-line split means dot-exact hardware (falsifies the next-line rule)",
            path.display()
        );
    }
}
