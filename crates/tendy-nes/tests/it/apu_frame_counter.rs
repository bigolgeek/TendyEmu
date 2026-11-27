use tendy_nes::apu::{Apu, POWER_ON_FRAME_LEAD};
use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;

use crate::common::rom_file;
use crate::common::run_blargg;

#[test]
fn frame_irq_after_4_step_sequence() {
    let mut a = Apu::new();

    for _ in 0..29827 {
        a.clock();
    }
    assert!(!a.frame_irq);

    a.clock();
    assert!(a.frame_irq);
    assert!(a.irq());
}

#[test]
fn reading_status_clears_frame_irq() {
    let mut a = Apu::new();

    for _ in 0..30000 {
        a.clock();
    }

    assert_eq!(a.read_status(), 0x40);
    a.clock(); // the flag clear takes effect one clock after the read
    assert_eq!(a.read_status(), 0x00);
    assert!(!a.irq());
}

#[test]
fn irq_inhibit_blocks_frame_irq() {
    let mut a = Apu::new();

    a.write_register(0x4017, 0x40);

    for _ in 0..30000 {
        a.clock();
    }

    assert!(!a.frame_irq);
}

#[test]
fn five_step_mode_has_no_irq() {
    let mut a = Apu::new();

    a.write_register(0x4017, 0x80);

    for _ in 0..40000 {
        a.clock();
    }

    assert!(!a.frame_irq);
}

#[test]
fn status_reports_length_counters() {
    let mut a = Apu::new();

    a.write_register(0x4015, 0x0F);
    a.write_register(0x4003, 0x08);
    a.write_register(0x4007, 0x08);
    a.write_register(0x400B, 0x08);
    a.write_register(0x400F, 0x08);

    assert_eq!(a.read_status(), 0x0F);

    a.write_register(0x4015, 0x00);
    assert_eq!(a.read_status(), 0x00);
}

#[test]
fn length_expires_on_second_half_frame() {
    let mut a = Apu::new();

    a.write_register(0x4015, 0x01);
    a.write_register(0x4003, 0x18);

    let mut n = 0;
    while a.read_status() & 1 != 0 {
        a.clock();
        n += 1;
    }

    assert_eq!(n, 29829);
}

#[test]
fn five_step_write_clocks_half_frame() {
    let mut a = Apu::new();

    a.write_register(0x4015, 0x01);
    a.write_register(0x4003, 0x18);
    a.write_register(0x4017, 0x80);

    for _ in 0..4 {
        a.clock();
    }

    assert_eq!(a.pulse1.length.value, 1);
}

#[test]
fn blargg_len_ctr() {
    let (status, text) = run_blargg("1-len_ctr.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_len_table() {
    let (status, text) = run_blargg("2-len_table.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_irq_flag() {
    let (status, text) = run_blargg("3-irq_flag.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn dmc_enable_even_write_request_after_2_cycles() {
    let mut a = Apu::new();
    assert_eq!(a.cycle % 2, 0);
    a.write_register(0x4015, 0x10);
    assert_eq!(a.dmc.fetch_addr(), None);
    a.clock();
    assert_eq!(a.dmc.fetch_addr(), None);
    a.clock();
    assert_eq!(a.dmc.fetch_addr(), Some(0xC000));
}

#[test]
fn dmc_enable_odd_write_request_after_3_cycles() {
    let mut a = Apu::new();
    a.clock();
    a.write_register(0x4015, 0x10);
    a.clock();
    a.clock();
    assert_eq!(a.dmc.fetch_addr(), None);
    a.clock();
    assert_eq!(a.dmc.fetch_addr(), Some(0xC000));
}

#[test]
fn dmc_disable_during_delay_never_exposes_fetch_addr() {
    let mut a = Apu::new();
    a.write_register(0x4015, 0x10);
    a.clock();
    a.write_register(0x4015, 0x00);
    assert_eq!(a.dmc.load_delay, 0);
    for _ in 0..10 {
        assert_eq!(a.dmc.fetch_addr(), None);
        a.clock();
    }
}

#[test]
fn dmc_disable_cancels_visible_request() {
    let mut a = Apu::new();
    a.write_register(0x4015, 0x10);
    a.clock();
    a.clock();
    assert!(a.dmc.fetch_addr().is_some());
    a.write_register(0x4015, 0x00);
    assert_eq!(a.dmc.fetch_addr(), None);
}

/// Power-on alone carries the frame-counter lead: `Apu::new` keeps phase 0 (the frame-counter clocks
/// counted above), and a console applies `POWER_ON_FRAME_LEAD` before its reset-vector reads, so a
/// fresh `Nes` is 2 cycles further in — 2 more APU clocks from `Cpu::reset`'s two vector reads.
#[test]
fn power_on_leads_the_frame_counter_by_three() {
    assert_eq!(POWER_ON_FRAME_LEAD, 3);
    assert_eq!(Apu::new().frame_cycle, 0);

    let mut a = Apu::new();
    a.power_on();
    assert_eq!(a.frame_cycle, POWER_ON_FRAME_LEAD);

    let nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    assert_eq!(nes.bus.apu.frame_cycle, POWER_ON_FRAME_LEAD + 2);
}

#[test]
fn soft_reset_clears_the_irq_state_and_keeps_the_step_mode() {
    let mut a = Apu::new();

    // Into the 4-step sequence past the frame IRQ, then switch to 5-step: the IRQ flag is still set
    // and the mode is what a reset must preserve.
    for _ in 0..29828 {
        a.clock();
    }
    assert!(a.frame_irq);
    a.write_register(0x4017, 0x80);
    assert!(a.five_step);

    a.soft_reset();

    assert!(!a.frame_irq);
    assert!(!a.irq());
    assert!(!a.irq_inhibit);
    assert_eq!(a.reset_delay, 0);
    assert_eq!(a.frame_cycle, POWER_ON_FRAME_LEAD);
    assert!(a.five_step, "$4017 mode is unchanged by a reset");
    assert_eq!(a.read_status() & 0x40, 0);
}

#[test]
fn soft_reset_restarts_the_sequence_from_the_power_on_phase() {
    let mut a = Apu::new();
    for _ in 0..10000 {
        a.clock();
    }
    // A $4017 write would reset the sequence 3-4 clocks later and the IRQ inhibit would suppress the
    // next frame IRQ: a reset drops that pending write and clears the inhibit.
    a.write_register(0x4017, 0x40);
    assert!(a.reset_delay > 0);

    a.soft_reset();

    let mut n = 0;
    while !a.frame_irq {
        a.clock();
        n += 1;
    }
    assert_eq!(n, 29828 - POWER_ON_FRAME_LEAD);
}

/// NROM image whose program is `JMP $8000`, so the machine only idles in the frame counter.
fn idle_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16..19].copy_from_slice(&[0x4C, 0x00, 0x80]);
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

#[test]
fn nes_reset_clears_the_frame_irq_flag() {
    let mut nes = Nes::new(&idle_rom()).unwrap();
    while !nes.bus.apu.frame_irq {
        nes.step();
    }

    nes.reset();

    assert!(
        !nes.bus.apu.frame_irq,
        "the flag must be clear at the first $4015 read after a reset"
    );
    // The CPU's two reset-vector reads clock the APU twice after the soft reset, so the phase is the
    // power-on lead plus those two clocks -- not the phase the machine had before the reset.
    assert_eq!(nes.bus.apu.frame_cycle, POWER_ON_FRAME_LEAD + 2);
    assert_eq!(nes.bus.apu.reset_delay, 0);
    assert_eq!(nes.bus.read(0x4015) & 0x40, 0x00);
    assert_eq!(nes.cpu.pc, 0x8000);
}
