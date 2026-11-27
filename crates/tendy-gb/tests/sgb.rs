//! One gate for the SGB module: adding `src/sgb/` must not move a pixel.
//!
//! The number below was measured **before** `src/sgb/` existed, on source identical to `main`
//! (`4c00d5f`): a synthesized DMG cartridge, a fixed 16-byte tile written straight into VRAM, a
//! 32×32 map of tiles 0-3, `BGP = $E4`, three frames.
//!
//! The machine this test builds is not given an SGB console, so it holds `Sgb::off()` — the state
//! every build produces. If wiring SGB, or a command's `unimplemented!()`
//! replacement, ever reaches a DMG frame, this fails.

mod common;

/// `main`'s running checksum over the three frames.
const MAIN_RUNNING: u64 = 0xf146_549e_efd3_9f45;
/// `main`'s checksum of the third frame alone.
const MAIN_LAST_FRAME: u64 = 0x443d_bc2b_6f6e_4c85;
/// How many of the 23 040 pixels of that frame are not shade 0, so a blank frame cannot pass.
const MAIN_NONZERO: usize = 3_600;

/// The fixed picture, drawn into a machine that has never heard of the Super Game Boy.
fn picture() -> tendy_gb::Machine {
    let mut m = common::machine(common::synth_rom(&[]));
    for (i, byte) in m.bus.ppu.vram.iter_mut().enumerate().take(16) {
        *byte = (i as u8).wrapping_mul(17);
    }
    for i in 0..32 * 32 {
        m.bus.ppu.vram[0x1800 + i] = [0u8, 1, 2, 3][(i / 5) % 4];
    }
    m.bus.ppu.bgp = 0xE4;
    for _ in 0..3 {
        m.run_frame();
    }
    m
}

#[test]
fn an_sgb_off_machine_draws_main_frame() {
    let m = picture();
    assert!(!m.bus.ppu.sgb.enabled(), "this machine never turns SGB on");
    let nonzero = m.frame().iter().filter(|&&b| b != 0).count();
    assert_eq!(
        m.frame_checksum(),
        MAIN_RUNNING,
        "the running checksum moved off main's {MAIN_RUNNING:016x}"
    );
    assert_eq!(
        m.last_frame_checksum(),
        MAIN_LAST_FRAME,
        "the last frame moved off main's {MAIN_LAST_FRAME:016x}"
    );
    assert_eq!(
        nonzero, MAIN_NONZERO,
        "the picture is not the one main draws ({nonzero} non-zero pixels)"
    );
}
