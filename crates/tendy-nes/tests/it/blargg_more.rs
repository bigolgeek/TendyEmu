//! blargg $6000-protocol ROMs: cpu_interrupts_v2 rom_singles,
//! instr_misc, instr_timing rom_singles + the instr_timing aggregate, cpu_dummy_writes,
//! cpu_exec_space, oam_read, oam_stress, ppu_open_bus, ppu_read_buffer, sprdma_and_dmc_dma and
//! apu_test. Same harness as blargg_vbl_apu.rs, frame limit raised to 2400 (oam_stress needs
//! 1705 frames, the instr_timing aggregate needs 1302).

use crate::common::run_blargg;

#[test]
fn nmi_and_brk() {
    let (status, text) = run_blargg("2-nmi_and_brk.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn nmi_and_irq() {
    let (status, text) = run_blargg("3-nmi_and_irq.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn irq_and_dma() {
    let (status, text) = run_blargg("4-irq_and_dma.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn branch_delays_irq() {
    let (status, text) = run_blargg("5-branch_delays_irq.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn cpu_interrupts_multi() {
    let (status, text) = run_blargg("cpu_interrupts.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn instr_misc_multi() {
    let (status, text) = run_blargg("instr_misc.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn abs_x_wrap() {
    let (status, text) = run_blargg("01-abs_x_wrap.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn branch_wrap() {
    let (status, text) = run_blargg("02-branch_wrap.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn dummy_reads() {
    let (status, text) = run_blargg("03-dummy_reads.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn dummy_reads_apu() {
    let (status, text) = run_blargg("04-dummy_reads_apu.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn dummy_writes_oam() {
    let (status, text) = run_blargg("cpu_dummy_writes_oam.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn dummy_writes_ppumem() {
    let (status, text) = run_blargg("cpu_dummy_writes_ppumem.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn exec_space_apu() {
    let (status, text) = run_blargg("test_cpu_exec_space_apu.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn exec_space_ppuio() {
    let (status, text) = run_blargg("test_cpu_exec_space_ppuio.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn oam_read() {
    let (status, text) = run_blargg("oam_read.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn oam_stress() {
    let (status, text) = run_blargg("oam_stress.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn ppu_open_bus() {
    let (status, text) = run_blargg("ppu_open_bus.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn ppu_read_buffer() {
    let (status, text) = run_blargg("test_ppu_read_buffer.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn sprdma_and_dmc_dma() {
    let (status, text) = run_blargg("sprdma_and_dmc_dma.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn sprdma_and_dmc_dma_512() {
    let (status, text) = run_blargg("sprdma_and_dmc_dma_512.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn apu_test_multi() {
    let (status, text) = run_blargg("apu_test.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn instr_timing() {
    let (status, text) = run_blargg("1-instr_timing.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn branch_timing() {
    let (status, text) = run_blargg("2-branch_timing.nes", 2400);
    assert_eq!(status, 0, "{text}");
}

/// The aggregate build of instr_timing (`instr_timing/instr_timing.nes`, fetched by
/// tools/fetch_test_roms.py) runs both rom_singles in sequence: it needs 1302 frames and reports
/// through $6000 as usual. Asserting the trailing summary as well as the status keeps this from
/// passing on a build that ran only one of the two tests.
#[test]
fn instr_timing_aggregate() {
    let (status, text) = run_blargg("instr_timing.nes", 2400);
    assert_eq!(status, 0, "{text}");
    assert!(text.contains("All 2 tests passed"), "{text}");
}
