use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;

/// NROM: enable NMI, then loop on `LDA $2002 / BPL`, counting each vblank the loop sees in $10.
/// The NMI handler reads $2002 (clearing the flag) and returns, as Dragon Warrior 3's does.
fn race_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    let main = [
        0xA9, 0x80, 0x8D, 0x00, 0x20, 0xAD, 0x02, 0x20, 0x10, 0xFB, 0xE6, 0x10, 0x4C, 0x05, 0x80,
    ];
    v[16..16 + main.len()].copy_from_slice(&main);
    let nmi = [0x2C, 0x02, 0x20, 0x40];
    v[16 + 0x10..16 + 0x10 + nmi.len()].copy_from_slice(&nmi);
    v[16 + 0x3FFA] = 0x10;
    v[16 + 0x3FFB] = 0x80;
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

#[test]
fn vblank_poll_sees_flag_with_nmi_enabled() {
    let mut nes = Nes::new(&race_rom()).unwrap();
    for _ in 0..600 {
        nes.run_frame();
    }
    // Before the fix the NMI always cleared the flag first, so the loop saw 0 vblanks.
    assert!(nes.bus.ram[0x10] > 20, "{} vblanks seen", nes.bus.ram[0x10]);
}

#[test]
#[ignore = "needs your own copy of roms/Dragon Warrior 3 (U).nes"]
fn dragon_warrior_3_boots() {
    let path = crate::common::rom_path("Dragon Warrior 3 (U).nes");
    let mut nes = Nes::new(&std::fs::read(path).unwrap()).unwrap();
    for _ in 0..2400 {
        nes.run_frame();
    }
    assert_ne!(nes.bus.ppu.mask & 0x18, 0, "rendering never turned on");
    // Reviewed snapshot: the opening scene (hero at the volcano, dragon overhead).
    assert_eq!(fnv1a64(&nes.bus.ppu.framebuffer), 0x577f_885e_3d37_0a83);
}
