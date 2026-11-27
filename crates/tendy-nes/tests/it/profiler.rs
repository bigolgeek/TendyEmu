use tendy_nes::genie::Code;
use tendy_nes::nes::Nes;
use tendy_nes::profiler::Profiler;
use tendy_nes::symbols::Symbols;

const DELAY: u16 = 0x8010;
const NMI: u16 = 0x8020;
const OUTER: u16 = 0x8030;

/// NROM image: reset -> $8000 enables NMI then loops `JSR delay`; the delay routine burns about
/// 1,300 cycles; the NMI handler is `INC $00 / RTI`.
fn rom() -> Vec<u8> {
    let mut prg = vec![0xEAu8; 0x8000];
    let mut put = |addr: u16, bytes: &[u8]| {
        let o = (addr - 0x8000) as usize;
        prg[o..o + bytes.len()].copy_from_slice(bytes);
    };
    // $8000: LDA #$80 / STA $2000 / loop: JSR $8010 / JMP loop
    put(
        0x8000,
        &[
            0xA9, 0x80, 0x8D, 0x00, 0x20, 0x20, 0x10, 0x80, 0x4C, 0x05, 0x80,
        ],
    );
    // delay: LDX #$FF / DEX / BNE -3 / RTS
    put(DELAY, &[0xA2, 0xFF, 0xCA, 0xD0, 0xFD, 0x60]);
    // nmi: INC $00 / RTI
    put(NMI, &[0xE6, 0x00, 0x40]);
    // outer: JSR outer (infinite recursion, used by the recursion test only)
    put(OUTER, &[0x20, 0x30, 0x80, 0x60]);
    prg[0x7FFA..0x8000].copy_from_slice(&[0x20, 0x80, 0x00, 0x80, 0x00, 0x80]); // NMI, reset, IRQ
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.extend(prg);
    v.extend(vec![0u8; 8192]);
    v
}

fn run(frames: usize) -> (Nes, Profiler, u64) {
    let mut nes = Nes::new(&rom()).unwrap();
    let start = nes.cpu.cycles;
    let mut p = Profiler::new();
    for _ in 0..frames {
        p.run_frame(&mut nes);
    }
    let ran = nes.cpu.cycles - start;
    (nes, p, ran)
}

#[test]
fn delay_routine_is_reported_first() {
    let (_, p, _) = run(5);
    assert_eq!(p.sorted()[0].0, DELAY);
    let syms = Symbols {
        labels: vec![(0x8000, "main".into()), (DELAY, "delay".into())],
    };
    let report = p.report(&syms);
    let first = report.lines().nth(1).unwrap();
    assert!(first.starts_with("delay"), "{report}");
    assert!(report.contains("main"), "{report}");
    assert!(
        report.contains("$8020"),
        "unnamed NMI routine shows its address: {report}"
    );
}

#[test]
fn totals_match_cycles_run() {
    let (_, p, ran) = run(5);
    assert_eq!(p.total_cycles, ran);
    let excl: u64 = p.routines.values().map(|s| s.exclusive).sum();
    assert_eq!(excl, ran);
}

#[test]
fn inclusive_is_at_least_exclusive() {
    let (_, p, ran) = run(5);
    for (addr, s) in &p.routines {
        assert!(s.inclusive >= s.exclusive, "${addr:04X}: {s:?}");
        assert!(s.inclusive <= ran, "${addr:04X}: {s:?}");
    }
    // The root frame includes the whole run.
    assert_eq!(p.routines[&0x8000].inclusive, ran);
    // main's inclusive covers the delay it calls; delay calls nothing.
    assert!(p.routines[&0x8000].inclusive > p.routines[&DELAY].inclusive);
    // An NMI landing inside the delay is charged to it inclusively.
    assert_eq!(
        p.routines[&DELAY].inclusive,
        p.routines[&DELAY].exclusive + p.routines[&NMI].inclusive
    );
}

#[test]
fn nmi_routine_counted_once_per_frame() {
    let (mut nes, mut p, _) = run(1);
    let before = p.routines.get(&NMI).map_or(0, |s| s.calls);
    for _ in 0..10 {
        p.run_frame(&mut nes);
    }
    assert_eq!(p.routines[&NMI].calls - before, 10);
    // Each NMI is 7 entry cycles + INC zp (5) + RTI (6).
    let s = p.routines[&NMI];
    assert_eq!(s.exclusive, s.calls * 18);
}

#[test]
fn recursion_charges_inclusive_once() {
    let mut nes = Nes::new(&rom()).unwrap();
    nes.cpu.pc = OUTER;
    let mut p = Profiler::new();
    for _ in 0..40 {
        p.step(&mut nes);
    }
    let s = p.routines[&OUTER];
    assert_eq!(s.inclusive, p.total_cycles);
    assert_eq!(s.exclusive, p.total_cycles);
    assert!(p.depth() <= tendy_nes::profiler::MAX_DEPTH);
}

/// The profiler fetches through the same path the CPU's fetch takes, so a Game Genie code
/// that rewrites the opcode at PC is what it sees. Reading the cartridge directly showed the
/// unpatched `LDA #$80` and missed the `JSR` the CPU actually executed.
#[test]
fn genie_bytes_are_what_the_profiler_fetches() {
    let rom = rom();
    let mut nes = Nes::new(&rom).unwrap();
    assert_eq!(
        nes.cpu.pc, 0x8000,
        "precondition: the reset vector lands on $8000"
    );
    // $8000 is `A9 80` (LDA #$80); the code turns that byte into `$20`, a JSR.
    nes.bus.set_genie_codes(vec![Code {
        addr: 0x8000,
        value: 0x20,
        compare: None,
    }]);
    let mut p = Profiler::new();
    p.step(&mut nes);
    assert_eq!(
        p.depth(),
        2,
        "the profiler missed the JSR the CPU ran: it read the cartridge, not the CPU's byte"
    );
}
