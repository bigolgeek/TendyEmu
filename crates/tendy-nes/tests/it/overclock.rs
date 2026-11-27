//! The overclock knob: extra idle post-render lines with the APU held through them.

use tendy_nes::cart::split;
use tendy_nes::console::ConsoleConfig;
use tendy_nes::nes::{Nes, PowerOn};
use tendy_nes::state::fnv1a64;

/// NROM whose program turns NMI on and then spins; the NMI handler increments zero-page RAM $02.
fn nmi_counter_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    // $8000: LDA #$80 / STA $2000 / JMP $8005 (spin).
    v[16..24].copy_from_slice(&[0xA9, 0x80, 0x8D, 0x00, 0x20, 0x4C, 0x05, 0x80]);
    // $8010: INC $02 / RTI.
    v[16 + 0x10..16 + 0x13].copy_from_slice(&[0xE6, 0x02, 0x40]);
    v[16 + 0x3FFA] = 0x10;
    v[16 + 0x3FFB] = 0x80;
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

fn machine(n: u16) -> Nes {
    let mut nes = Nes::new(&nmi_counter_rom()).unwrap();
    nes.set_overclock_lines(n);
    nes
}

/// CPU cycle counts at the first `count` NMIs, one per frame.
fn nmi_cycles(nes: &mut Nes, count: usize) -> Vec<u64> {
    let mut out = Vec::new();
    while out.len() < count {
        let before = nes.bus.ram[2];
        nes.step();
        if nes.bus.ram[2] != before {
            out.push(nes.bus.cycles);
        }
    }
    out
}

/// APU samples produced over `frames` whole frames.
fn samples_over(nes: &mut Nes, frames: usize) -> usize {
    nes.run_frame();
    nes.bus.apu.samples.clear();
    for _ in 0..frames {
        nes.run_frame();
    }
    nes.bus.apu.samples.len()
}

#[test]
fn zero_lines_is_the_stock_geometry() {
    let nes = machine(0);
    assert_eq!(nes.bus.ppu.lines_per_frame, 262);
    assert_eq!(nes.bus.ppu.vblank_line, 241);
    assert_eq!(nes.bus.ppu.pre_render_line, 261);
    assert!(!nes.bus.ppu.in_overclock_lines());
}

#[test]
fn fifty_lines_move_the_frame_geometry() {
    let nes = machine(50);
    assert_eq!(nes.bus.ppu.lines_per_frame, 312);
    assert_eq!(nes.bus.ppu.vblank_line, 291);
    assert_eq!(nes.bus.ppu.pre_render_line, 311);
}

#[test]
fn nmi_period_grows_by_the_extra_lines() {
    const FRAMES: usize = 10;
    let mut stock = machine(0);
    let mut over = machine(50);
    let s = nmi_cycles(&mut stock, FRAMES + 1);
    let o = nmi_cycles(&mut over, FRAMES + 1);
    let stock_span = (s[FRAMES] - s[0]) as i64;
    let over_span = (o[FRAMES] - o[0]) as i64;
    // 50 lines x 341 dots / 3 dots per CPU cycle, per frame. The NMI lands on an instruction boundary
    // of the spin loop, so allow a few cycles of jitter over the whole span.
    let expected = (FRAMES as i64) * 50 * 341 / 3;
    let grown = over_span - stock_span;
    assert!(
        (grown - expected).abs() <= 6,
        "grew {grown} cycles over {FRAMES} frames, expected {expected}"
    );
}

#[test]
fn apu_samples_per_frame_are_unchanged() {
    let mut stock = machine(0);
    let mut over = machine(50);
    stock.bus.apu.set_sample_rate(44_100.0);
    over.bus.apu.set_sample_rate(44_100.0);
    let a = samples_over(&mut stock, 20);
    let b = samples_over(&mut over, 20);
    assert!(a > 0, "the APU produced no samples");
    assert!(
        (a as i64 - b as i64).abs() <= 1,
        "stock {a} samples, overclocked {b}"
    );
}

#[test]
fn apu_is_held_only_inside_the_extra_lines() {
    let mut nes = machine(50);
    let mut held = 0u32;
    let mut lines = std::collections::BTreeSet::new();
    nes.run_frame();
    for _ in 0..40_000 {
        nes.step();
        if nes.bus.ppu.in_overclock_lines() {
            held += 1;
            lines.insert(nes.bus.ppu.scanline);
        }
    }
    assert!(held > 0);
    assert!(lines.iter().all(|l| (241..291).contains(l)), "{lines:?}");
}

#[test]
fn loading_an_overclocked_state_into_a_normal_machine_does_not_panic() {
    let mut over = machine(50);
    over.run_frame();
    // Park the beam inside the inserted lines (past the stock frame's 262 lines).
    while over.bus.ppu.scanline < 280 {
        over.step();
    }
    let state = over.save_state();
    let mut normal = machine(0);
    normal.load_state(&state).unwrap();
    assert!(normal.bus.ppu.scanline < normal.bus.ppu.lines_per_frame);
    for _ in 0..3 {
        normal.run_frame();
    }
}

/// A console built the way the frontend builds one, from a power-on option.
fn console(overclock: u16) -> Nes {
    let rom = nmi_counter_rom();
    let config = ConsoleConfig::new(PowerOn {
        overclock,
        ..PowerOn::default()
    });
    Nes::from_rom_data_console(split(&rom).expect("parse rom"), fnv1a64(&rom), &config)
        .expect("build console")
}

#[test]
fn the_power_on_option_builds_the_overclocked_geometry() {
    let nes = console(50);
    assert_eq!(nes.bus.ppu.lines_per_frame, 312);
    assert_eq!(nes.bus.ppu.vblank_line, 291);
    assert_eq!(nes.bus.ppu.pre_render_line, 311);
}

#[test]
fn the_default_power_on_keeps_the_stock_geometry() {
    assert_eq!(PowerOn::default().overclock, 0);
    for nes in [console(0), Nes::new(&nmi_counter_rom()).unwrap()] {
        assert_eq!(nes.bus.ppu.lines_per_frame, 262);
        assert_eq!(nes.bus.ppu.vblank_line, 241);
        assert_eq!(nes.bus.ppu.pre_render_line, 261);
    }
}
