//! blargg mmc3_test_2 ROMs (H6000 harness, 600 frames).

use tendy_nes::nes::Nes;

use crate::common::rom_file;

/// Run a blargg ROM image until it reports through $6000 (signature DE B0 61, status below $80).
fn run_blargg_bytes(bytes: &[u8]) -> (u8, String) {
    run_blargg_opt(bytes, false).0
}

/// `run_blargg_bytes` with the sprite limit optionally removed; also returns a digest of the
/// CPU cycle count and work RAM after the run, so IRQ timing differences cannot hide.
fn run_blargg_opt(bytes: &[u8], unlimited: bool) -> ((u8, String), u64) {
    let mut nes = Nes::new(bytes).unwrap();
    nes.bus.ppu.sprite_limit_removed = unlimited;
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
    let digest = nes.bus.ram.iter().fold(nes.cpu.cycles, |h, &b| {
        h.wrapping_mul(31).wrapping_add(u64::from(b))
    });
    ((nes.bus.cart_mut().cpu_read(0x6000), text), digest)
}

fn run_blargg(name: &str) -> (u8, String) {
    run_blargg_bytes(&rom_file(name))
}

/// NES 2.0 header with submapper 4 (MMC3 rev A).
fn patch_submapper4(v: &mut [u8]) {
    v[7] = (v[7] & 0xF0) | 0x08;
    v[8] = 0x40;
}

#[test]
fn mmc3_1_clocking() {
    let (s, t) = run_blargg("1-clocking.nes");
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_2_details() {
    let (s, t) = run_blargg("2-details.nes");
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_3_a12_clocking() {
    let (s, t) = run_blargg("3-A12_clocking.nes");
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_4_scanline_timing() {
    let (s, t) = run_blargg("4-scanline_timing.nes");
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_5_mmc3_rev_b() {
    let (s, t) = run_blargg("5-MMC3.nes");
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_6_alt_with_submapper_4() {
    let mut v = rom_file("6-MMC3_alt.nes");
    patch_submapper4(&mut v);
    let (s, t) = run_blargg_bytes(&v);
    assert_eq!(s, 0, "{t}");
}

#[test]
fn mmc3_6_alt_unpatched_fails_as_rev_b() {
    let (s, _) = run_blargg("6-MMC3_alt.nes");
    assert_ne!(s, 0, "rev B must fail the rev A test");
}

/// The extra-sprite fetch uses `chr_peek`, so MMC3 IRQ timing is untouched with the sprite limit
/// removed. These ROMs count and time IRQs against the PPU's A12 edges; the verdicts and the final
/// machine state must equal the limit-on run.
#[test]
fn mmc3_irq_timing_is_unchanged_with_the_sprite_limit_removed() {
    for name in [
        "1-clocking.nes",
        "3-A12_clocking.nes",
        "4-scanline_timing.nes",
        "5-MMC3.nes",
    ] {
        let bytes = rom_file(name);
        let off = run_blargg_opt(&bytes, false);
        let on = run_blargg_opt(&bytes, true);
        assert_eq!(on, off, "{name}");
    }
}
