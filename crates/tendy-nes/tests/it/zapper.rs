//! A synthetic ROM reads the Zapper's port on a real machine. The gun is plugged in through
//! the frontend API and the program stores what it reads, so attach, aim, framebuffer sampling,
//! port read and CPU are all exercised together — the bus-level unit tests cannot show that a
//! game-shaped read sequence sees the gun at all.

use tendy_nes::controller::{ZAPPER_LIGHT_SENSE, ZAPPER_TRIGGER};
use tendy_nes::nes::Nes;

/// Colours the program puts in `$3F00`. With rendering off every pixel of the frame is the
/// backdrop, so the gun's whole field of view is one colour: `$0F` the palette's true black, `$30`
/// the white every Zapper game flashes to aim with.
const BLACK: u8 = 0x0F;
const WHITE: u8 = 0x30;

/// A 16 KB NROM image: the program at `$C000` (its only bank, mapped at `$C000-$FFFF`) with all
/// three vectors pointing at it.
fn zapper_rom(backdrop: u8) -> Vec<u8> {
    let base = 0xC000u16;
    let prg_len = 0x4000usize;
    let chr_len = 0x2000usize;
    // NROM-128: one 16 KB PRG bank, one 8 KB CHR bank (all zeros: nothing is rendered).
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.resize(16 + prg_len + chr_len, 0);
    let code = program(base, backdrop);
    rom[16..16 + code.len()].copy_from_slice(&code);
    for vector in [0x3FFA, 0x3FFC, 0x3FFE] {
        rom[16 + vector] = (base & 0xFF) as u8;
        rom[16 + vector + 1] = (base >> 8) as u8;
    }
    rom
}

/// The program: put `backdrop` in `$3F00`, point `v` back at `$3F00`, turn rendering off (so the
/// backdrop fills the picture), then loop strobing the ports and storing what port 1 and port 2
/// read, plus a second read of port 1 that must not change because a Zapper has no shift register
/// to clock.
///
/// The second `$2006` pair is what makes the flash white on a console that implements the backdrop
/// override: with forced blank and `v` left in palette RAM, the PPU draws the entry
/// `v` addresses instead of entry 0. A `$2007` write leaves `v` one past the byte it wrote, so
/// without this the screen is `palette[1]` - uninitialised black - and the gun sees no light over
/// what this test calls a flash of white.
fn program(base: u16, backdrop: u8) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend([0xA9, 0x3F, 0x8D, 0x06, 0x20]); // LDA #$3F ; STA $2006
    p.extend([0xA9, 0x00, 0x8D, 0x06, 0x20]); // LDA #$00 ; STA $2006
    p.extend([0xA9, backdrop, 0x8D, 0x07, 0x20]); // LDA #backdrop ; STA $2007
    // Backdrop override: during forced blank the PPU draws `palette[v & 0x1F]`, and the
    // write above left `v` at $3F01. Put it back at $3F00 so the frame really is `backdrop`.
    p.extend([0xA9, 0x3F, 0x8D, 0x06, 0x20]); // LDA #$3F ; STA $2006
    p.extend([0xA9, 0x00, 0x8D, 0x06, 0x20]); // LDA #$00 ; STA $2006
    p.extend([0xA9, 0x00, 0x8D, 0x01, 0x20]); // LDA #$00 ; STA $2001 (rendering off)
    let loop_at = base + p.len() as u16;
    p.extend([0xA9, 0x01, 0x8D, 0x16, 0x40]); // LDA #$01 ; STA $4016 (strobe high)
    p.extend([0xA9, 0x00, 0x8D, 0x16, 0x40]); // LDA #$00 ; STA $4016 (strobe low)
    p.extend([0xAD, 0x16, 0x40, 0x85, 0x00]); // LDA $4016 ; STA $00
    p.extend([0xAD, 0x17, 0x40, 0x85, 0x01]); // LDA $4017 ; STA $01
    p.extend([0xAD, 0x16, 0x40, 0x85, 0x02]); // LDA $4016 ; STA $02
    p.extend([0x4C, (loop_at & 0xFF) as u8, (loop_at >> 8) as u8]); // JMP loop
    p
}

/// Boot the ROM with a gun on port 1 aimed at `(x, y)`, run two frames, and return the three bytes
/// the program stored: port 1, port 2, and port 1 again.
fn run(backdrop: u8, x: u16, y: u16, trigger: bool) -> (u8, u8, u8) {
    let rom = zapper_rom(backdrop);
    let mut nes = Nes::new(&rom).expect("build console");
    nes.bus.attach_zapper(0);
    nes.bus.set_zapper(0, x, y, trigger);
    for _ in 0..2 {
        nes.run_frame();
    }
    (nes.bus.ram[0x00], nes.bus.ram[0x01], nes.bus.ram[0x02])
}

/// The gun's two lines are the top bits of the low nibble; a joypad never drives either, so
/// masking with `0x18` is what a game does and what these tests compare.
const LINES: u8 = ZAPPER_LIGHT_SENSE | ZAPPER_TRIGGER;

#[test]
fn a_gun_over_a_dark_picture_reads_both_lines_high() {
    let (p1, p2, again) = run(BLACK, 100, 50, false);
    assert_eq!(p1 & LINES, LINES, "an idle gun over black reads $18");
    assert_eq!(
        again, p1,
        "a Zapper does not clock: the second read is the same byte"
    );
    assert_eq!(
        p2 & LINES,
        0,
        "port 2 has no gun, so the joypad there answers instead"
    );
}

#[test]
fn the_white_flash_pulls_the_light_sense_low() {
    let (p1, _, _) = run(WHITE, 100, 50, false);
    assert_eq!(p1 & LINES, ZAPPER_TRIGGER, "a lit pixel pulls /D3 low");
}

#[test]
fn the_trigger_pulls_its_own_line_low() {
    let (dark, _, _) = run(BLACK, 100, 50, true);
    assert_eq!(
        dark & LINES,
        ZAPPER_LIGHT_SENSE,
        "over black a held trigger leaves /D3 high"
    );
    let (lit, _, _) = run(WHITE, 100, 50, true);
    assert_eq!(
        lit & LINES,
        0,
        "over white with the trigger held, both lines read low"
    );
}

#[test]
fn a_gun_aimed_outside_the_picture_sees_no_light() {
    let (p1, _, _) = run(WHITE, 300, 50, false);
    assert_eq!(
        p1 & LINES,
        LINES,
        "the bezel is dark even while the screen flashes"
    );
    let (p1, _, _) = run(WHITE, 100, 300, false);
    assert_eq!(p1 & LINES, LINES, "below the picture is dark too");
}
