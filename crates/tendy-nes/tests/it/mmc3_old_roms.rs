//! blargg's older MMC3 ROMs.
//!
//! `mmc3_test` (2005-era shell, H6000 harness): result in $6000, 0 = pass.
//! `mmc3_irq_tests` (2005 shell, no $6000 signature): result in RAM $00F8, 1 = pass.
//! The ROMs are saved as `mmc3_old_*` / `mmc3_irq_*` because their basenames collide
//! with mmc3_test_2 and sprite_overflow_tests.

use tendy_nes::nes::Nes;

use crate::common::rom_file;

/// H6000 harness: status at $6000 once the DE B0 61 signature is present and status < $80.
fn run_6000(bytes: &[u8]) -> (u8, String) {
    let mut nes = Nes::new(bytes).unwrap();
    for _ in 0..600 {
        nes.run_frame();
        if nes.bus.cart_mut().cpu_read(0x6001) == 0xDE
            && nes.bus.cart_mut().cpu_read(0x6002) == 0xB0
            && nes.bus.cart_mut().cpu_read(0x6003) == 0x61
            && nes.bus.cart_mut().cpu_read(0x6000) < 0x80
        {
            break;
        }
    }
    let mut text = String::new();
    let mut addr = 0x6004;
    while nes.bus.cart_mut().cpu_read(addr) != 0 && addr < 0x7000 {
        text.push(char::from(nes.bus.cart_mut().cpu_read(addr)));
        addr += 1;
    }
    (nes.bus.cart_mut().cpu_read(0x6000), text)
}

/// Older shell: result code in RAM $00F8 (1 = passed, n >= 2 = failed check n).
fn run_f8(bytes: &[u8]) -> u8 {
    let mut nes = Nes::new(bytes).unwrap();
    for _ in 0..600 {
        nes.run_frame();
    }
    nes.bus.ram[0xF8]
}

/// Nametable 0 as text rows, read the way `tests/tasvideos.rs` reads a `screen:` row
/// (blargg font: tile number = ASCII).
fn screen_rows(nes: &Nes) -> Vec<String> {
    (0..30)
        .map(|row| {
            (0..32)
                .map(|col| {
                    let b = nes.bus.ppu.vram[row * 32 + col];
                    if (0x20..0x7F).contains(&b) {
                        b as char
                    } else {
                        ' '
                    }
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Run to the 600-frame cap, stopping early once the ROM has drawn a verdict, and return the
/// non-empty screen rows joined by `" / "`.
fn run_screen(bytes: &[u8]) -> String {
    let mut nes = Nes::new(bytes).unwrap();
    let mut rows = Vec::new();
    for _ in 0..600 {
        nes.run_frame();
        rows = screen_rows(&nes);
        if rows
            .iter()
            .any(|r| r.contains("Passed") || r.contains("Failed"))
        {
            break;
        }
    }
    rows.iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// NES 2.0 header with submapper 4 (MMC3 rev A).
fn patch_submapper4(v: &mut [u8]) {
    v[7] = (v[7] & 0xF0) | 0x08;
    v[8] = 0x40;
}

fn old(name: &str) {
    let (s, t) = run_6000(&rom_file(name));
    assert_eq!(s, 0, "{t}");
}

fn irq(name: &str) {
    assert_eq!(run_f8(&rom_file(name)), 1, "{name}");
}

#[test]
fn old_1_clocking() {
    old("mmc3_old_1-clocking.nes");
}

#[test]
fn old_2_details() {
    old("mmc3_old_2-details.nes");
}

#[test]
fn old_3_a12_clocking() {
    old("mmc3_old_3-A12_clocking.nes");
}

#[test]
fn old_4_scanline_timing() {
    old("mmc3_old_4-scanline_timing.nes");
}

#[test]
fn old_5_mmc3() {
    old("mmc3_old_5-MMC3.nes");
}

/// NES 2.0 header with submapper 1 (MMC6).
fn patch_submapper1(v: &mut [u8]) {
    v[7] = (v[7] & 0xF0) | 0x08;
    v[8] = 0x10;
}

/// MMC6 has no PRG RAM at `$6000-$6FFF`, so the H6000 signature is unreachable: `$6000` reads 0
/// no matter what the ROM decided, and asserting on that zero-read passes without testing anything.
/// The ROM reports its verdict on screen instead (`6-MMC6 / Passed`).
#[test]
fn old_6_mmc6_with_submapper_1() {
    let mut v = rom_file("mmc3_old_6-MMC6.nes");
    patch_submapper1(&mut v);
    let screen = run_screen(&v);
    assert!(screen.contains("6-MMC6"), "not the MMC6 screen: {screen}");
    assert!(screen.contains("Passed"), "screen: {screen}");
    assert!(!screen.contains("Failed"), "screen: {screen}");
}

#[test]
fn old_6_mmc6_unpatched_fails_as_mmc3() {
    let (s, _) = run_6000(&rom_file("mmc3_old_6-MMC6.nes"));
    assert_ne!(s, 0, "plain MMC3 must fail the MMC6 test");
}

#[test]
fn irq_1_clocking() {
    irq("mmc3_irq_1.Clocking.nes");
}

#[test]
fn irq_2_details() {
    irq("mmc3_irq_2.Details.nes");
}

#[test]
fn irq_3_a12_clocking() {
    irq("mmc3_irq_3.A12_clocking.nes");
}

#[test]
fn irq_4_scanline_timing() {
    irq("mmc3_irq_4.Scanline_timing.nes");
}

#[test]
fn irq_5_rev_a_with_submapper_4() {
    let mut v = rom_file("mmc3_irq_5.MMC3_rev_A.nes");
    patch_submapper4(&mut v);
    assert_eq!(run_f8(&v), 1);
}

#[test]
fn irq_5_rev_a_unpatched_fails_as_rev_b() {
    assert_ne!(
        run_f8(&rom_file("mmc3_irq_5.MMC3_rev_A.nes")),
        1,
        "rev B must fail the rev A test"
    );
}

#[test]
fn irq_6_rev_b() {
    irq("mmc3_irq_6.MMC3_rev_B.nes");
}
