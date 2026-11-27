//! R4: the clocked bus. Every CPU read or write runs one CPU cycle of the PPU (3 dots) and APU, and
//! the CPU polls for interrupts at the end of each instruction with what the bus saw before its last cycle.

use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;

use crate::common::run_blargg;

/// NROM image: `code` at $8000, reset vector $8000, NMI and IRQ vectors at $8100 (an RTI).
fn program(code: &[u8]) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16..16 + code.len()].copy_from_slice(code);
    v[16 + 0x100] = 0x40;
    v[16 + 0x3FFA] = 0x00;
    v[16 + 0x3FFB] = 0x81;
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v[16 + 0x3FFE] = 0x00;
    v[16 + 0x3FFF] = 0x81;
    v
}

#[test]
fn bus_advances_one_cycle_per_access() {
    // LDX #$FF; LDA $02F0,X (page cross, 5 cycles); STA $0300; INC $10; JMP $8000
    let code = [
        0xA2, 0xFF, 0xBD, 0xF0, 0x02, 0x8D, 0x00, 0x03, 0xE6, 0x10, 0x4C, 0x00, 0x80,
    ];
    let mut nes = Nes::new(&program(&code)).unwrap();
    let expected = [2, 5, 4, 5, 3];
    for &want in expected.iter().chain(expected.iter()) {
        let before = nes.bus.cycles;
        let dot_before = u64::from(nes.bus.ppu.scanline) * 341 + u64::from(nes.bus.ppu.dot);
        let got = nes.step();
        assert_eq!(got, want);
        assert_eq!(nes.bus.cycles - before, u64::from(want));
        let dot_after = u64::from(nes.bus.ppu.scanline) * 341 + u64::from(nes.bus.ppu.dot);
        assert_eq!(dot_after - dot_before, 3 * u64::from(want));
    }
}

#[test]
fn oam_dma_takes_513_or_514_cycles() {
    let mut nes = Nes::new(&program(&[0x4C, 0x00, 0x80])).unwrap();
    // Start DMAs after 0 to 3 extra one-cycle reads, so they start on both even and odd cycles.
    let mut seen = Vec::new();
    for extra in 0..4 {
        for _ in 0..extra {
            nes.bus.read(0x0000);
        }
        nes.bus.write(0x4014, 0x02);
        nes.bus.read(0x0000);
        seen.push(nes.bus.dma_cycles);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, vec![513, 514]);
}

#[test]
fn blargg_cli_latency() {
    let (status, text) = run_blargg("1-cli_latency.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_nmi_control() {
    let (status, text) = run_blargg("04-nmi_control.nes", 600);
    assert_eq!(status, 0, "{text}");
}
