//! A jammed CPU must keep the clocked bus running, or `run_frame` never returns.

use tendy_nes::nes::Nes;

/// NROM image whose reset vector points at a JAM opcode ($02) at $8000.
fn jam_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16] = 0x02;
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

#[test]
fn jammed_cpu_keeps_the_bus_running() {
    let mut nes = Nes::new(&jam_rom()).unwrap();
    for _ in 0..10 {
        nes.step();
    }
    assert!(nes.cpu.jammed);
    let before = nes.bus.cycles;
    assert_eq!(nes.step(), 1);
    assert_eq!(nes.bus.cycles, before + 1);
}

#[test]
fn run_frame_returns_when_jammed() {
    let mut nes = Nes::new(&jam_rom()).unwrap();
    for _ in 0..3 {
        nes.run_frame();
    }
    assert!(nes.cpu.jammed);
    assert!(nes.bus.ppu.frame >= 2);
}
