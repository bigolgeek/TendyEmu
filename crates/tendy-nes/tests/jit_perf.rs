//! The numbers a JIT recompiler would be judged against, recorded in `docs/jit-baseline.md`.
//!
//! Three measurements, all `#[ignore]` so CI never pays for them (run them by hand):
//!
//! ```text
//! cargo test --release --test jit_perf -- --ignored --nocapture
//! ```
//!
//! 1. [`interp_cost_table`] — ms/frame, ns per CPU instruction, instructions/frame and machine
//!    cycles/frame for the corpus below, stepping exactly the way `Nes::run_frame` does.
//! 2. [`cpu_only_cost`] — the same two synthetic programs against `cpu::TestBus`, which has no PPU,
//!    APU, mapper or interrupt polling in it. The ratio `f_est` is the CPU-side share of frame time
//!    the JIT can attack, and it is a *low* estimate because a `TestBus` access is cheaper than a
//!    `SysBus` one (see the doc).
//! 3. [`instr_mix`] — the ten opcodes AccuracyCoin spends most of its instructions on, counted from
//!    the byte at PC before every step (`bus.peek(pc)`, so a Game Genie code would be counted as the
//!    opcode the CPU actually executes).
//!
//! Nothing here changes `src/`: the harness is read-only instrumentation of the interpreter.

use std::time::{Duration, Instant};

use tendy_nes::cpu::opcodes::OPS;
use tendy_nes::cpu::{Bus, Cpu, TestBus};
use tendy_nes::nes::Nes;

/// Frames of warm-up before a measurement: enough for the PPU/APU pipelines, the mapper and the
/// emulator's own branch predictors to reach their steady state.
const WARMUP_FRAMES: usize = 60;
/// Frames measured per ROM. A frame is ~1.79M CPU cycles, so 300 frames is > 8.9M cycles of work.
const MEASURED_FRAMES: usize = 300;
/// Timed passes of [`MEASURED_FRAMES`] frames; a row reports the median pass. One 300-frame pass is
/// only ~1.2s of CPU time and this desktop's frame times move by 20-30% between them (other
/// processes, clock ramping), which is far too much noise for a number a design decision hangs on.
const PASSES: usize = 5;
/// Timed passes of a `cpu_only_cost` run; the reported figure is their median.
const CPU_ONLY_RUNS: usize = 21;

/// Where the synthetic programs live in the NROM image.
const MAIN: u16 = 0x8000;
/// Where synthetic A's loop body starts (after the `LDA #$80 / STA $2000` that enables NMI).
const LOOP: u16 = 0x8005;
/// The `JSR` target of synthetic B.
const DELAY: u16 = 0x8010;
/// The NMI handler both synthetic programs point their vector at.
const NMI: u16 = 0x8020;

/// One row of the interpreter table: what [`MEASURED_FRAMES`] frames cost this console.
struct Cost {
    name: &'static str,
    frames: usize,
    instructions: u64,
    /// `SysBus::cycles`: the machine clock, DMA stalls included.
    machine_cycles: u64,
    /// `Cpu::cycles`: instruction cycles only. The gap between the two is DMA.
    cpu_cycles: u64,
    /// `$2001` at the end of the pass. Rendering is the biggest reason two ROMs' per-cycle costs
    /// differ, so every row records which of the two shapes it was measured in.
    mask: u8,
    elapsed: Duration,
}

impl Cost {
    fn ms_per_frame(&self) -> f64 {
        self.elapsed.as_secs_f64() * 1e3 / self.frames as f64
    }

    fn ns_per_instr(&self) -> f64 {
        self.elapsed.as_nanos() as f64 / self.instructions as f64
    }

    fn instr_per_frame(&self) -> f64 {
        self.instructions as f64 / self.frames as f64
    }

    fn cycles_per_frame(&self) -> f64 {
        self.machine_cycles as f64 / self.frames as f64
    }

    fn cpu_cycles_per_frame(&self) -> f64 {
        self.cpu_cycles as f64 / self.frames as f64
    }
}

/// One frame driven exactly as `Nes::run_frame` drives it, returning the number of instructions it
/// took. `Cpu::step` is the unit the JIT replaces, so that is the unit counted.
fn run_frame(nes: &mut Nes) -> u64 {
    nes.bus.ppu.frame_ready = false;
    let mut instructions = 0;
    while !nes.bus.ppu.frame_ready {
        nes.step();
        instructions += 1;
    }
    instructions
}

/// Put `bytes` at `addr` in a 32KB PRG image based at `$8000`.
fn put(prg: &mut [u8], addr: u16, bytes: &[u8]) {
    let o = usize::from(addr - 0x8000);
    prg[o..o + bytes.len()].copy_from_slice(bytes);
}

/// NROM-256 iNES image in the style of `tests/profiler.rs`: 32KB PRG, 8KB CHR, reset and IRQ at
/// `$8000`, the vectors last. `$8000` enables NMI and falls into `main`, which is placed at
/// [`LOOP`]; `delay` is placed at [`DELAY`] and may be empty; the NMI handler is a single `RTI`.
fn synthetic_image(main: &[u8], delay: &[u8]) -> Vec<u8> {
    let mut prg = vec![0xEAu8; 0x8000];
    // LDA #$80 / STA $2000: NMI on, so each frame carries one interrupt like a real game's.
    put(&mut prg, MAIN, &[0xA9, 0x80, 0x8D, 0x00, 0x20]);
    put(&mut prg, LOOP, main);
    put(&mut prg, DELAY, delay);
    put(&mut prg, NMI, &[0x40]);
    // NMI, reset, IRQ.
    prg[0x7FFA..0x8000].copy_from_slice(&[0x20, 0x80, 0x00, 0x80, 0x00, 0x80]);
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.extend(prg);
    v.extend(vec![0u8; 8192]);
    v
}

/// Synthetic A: `LDX #$FF / DEX / BNE` back to the `DEX`, then `JMP` back to the `LDX`. Three opcode
/// kinds (immediate, implied, relative branch) and one 258-cycle loop — the shape a block-finding
/// JIT sees in an idle game loop.
fn tight_loop_image() -> Vec<u8> {
    synthetic_image(&[0xA2, 0xFF, 0xCA, 0xD0, 0xFD, 0x4C, 0x05, 0x80], &[])
}

/// Synthetic B: `JSR $8010 / JMP $8005` with `$8010: RTS`, so a loop iteration is two instructions and
/// nine cycles of pure call overhead — the most call-heavy code a 6502 can run, and the shape that
/// pays the interpreter's per-instruction cost the most times per cycle.
fn call_loop_image() -> Vec<u8> {
    synthetic_image(&[0x20, 0x10, 0x80, 0x4C, 0x05, 0x80], &[0x60])
}

/// A test ROM from `roms/`, with the fetch hint `tests/it/common` uses.
fn rom(name: &str) -> Vec<u8> {
    let root = std::env::var_os("TENDY_NES_TEST_ROMS")
        .or_else(|| std::env::var_os("TENDY_TEST_ROMS"))
        .map_or_else(
            || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("roms"),
            std::path::PathBuf::from,
        );
    let path = root.join(name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (set TENDY_NES_TEST_ROMS)", path.display()))
}

/// AccuracyCoin as it boots is a menu that waits for input, spinning in a three-cycle `JMP` loop:
/// a faithful measurement of the ROM, but of its idle loop rather than of its tests. This is the
/// same Start press `tests/accuracycoin.rs::press` makes — A held for two frames, released for
/// twelve — which starts page 1 of the suite. Frames run after it are real test code.
fn accuracycoin_running() -> Nes {
    let mut nes = Nes::new(&rom("AccuracyCoin.nes")).unwrap();
    for _ in 0..WARMUP_FRAMES {
        run_frame(&mut nes);
    }
    nes.bus.pads[0].buttons = tendy_nes::controller::BUTTON_A;
    for _ in 0..2 {
        run_frame(&mut nes);
    }
    nes.bus.pads[0].buttons = 0;
    for _ in 0..12 {
        run_frame(&mut nes);
    }
    nes
}

/// The corpus, measured: AccuracyCoin twice (as booted, and with page 1 of the suite running),
/// then two test ROMs, a cycle-timing ROM and the CPU-only diagnostic, plus the two synthetic
/// extremes.
fn corpus_costs() -> Vec<Cost> {
    let mut costs = vec![measure("AccuracyCoin (menu)", &rom("AccuracyCoin.nes"))];
    costs.push(time_frames("AccuracyCoin (page 1)", accuracycoin_running));
    for (name, image) in [
        ("all_instrs", rom("all_instrs.nes")),
        ("instr_timing", rom("instr_timing.nes")),
        ("nestest", rom("nestest.nes")),
        ("synthetic A (LDX/DEX/BNE)", tight_loop_image()),
        ("synthetic B (JSR/RTS)", call_loop_image()),
    ] {
        costs.push(measure(name, &image));
    }
    costs
}

/// Warm up, then time [`MEASURED_FRAMES`] frames of `image`.
fn measure(name: &'static str, image: &[u8]) -> Cost {
    time_frames(name, || {
        let mut nes = Nes::new(image).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        for _ in 0..WARMUP_FRAMES {
            run_frame(&mut nes);
        }
        nes
    })
}

/// Time [`MEASURED_FRAMES`] frames of a warmed console, [`PASSES`] times, and keep the median pass.
/// Emulation is deterministic but the wall clock is not, and a console is not state-free: every
/// pass builds a fresh one rather than reusing the last, so a pass cannot measure a different
/// workload than the one before it (AccuracyCoin's first page ends and drops back to its menu, and
/// a reused console would report the menu as the page).
fn time_frames(name: &'static str, mut console: impl FnMut() -> Nes) -> Cost {
    let mut passes = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let mut nes = console();
        let machine0 = nes.bus.cycles;
        let cpu0 = nes.cpu.cycles;
        let start = Instant::now();
        let mut instructions = 0;
        for _ in 0..MEASURED_FRAMES {
            instructions += run_frame(&mut nes);
        }
        let elapsed = start.elapsed();
        passes.push(Cost {
            name,
            frames: MEASURED_FRAMES,
            instructions,
            machine_cycles: nes.bus.cycles - machine0,
            cpu_cycles: nes.cpu.cycles - cpu0,
            mask: nes.bus.ppu.mask,
            elapsed,
        });
    }
    passes.sort_by_key(|pass| pass.elapsed);
    passes
        .into_iter()
        .nth(PASSES / 2)
        .expect("at least one pass")
}

/// The interpreter's cost table — the baseline every later milestone is a ratio against.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn interp_cost_table() {
    println!(
        "\n## Interpreter cost per frame (median of {PASSES} passes of {MEASURED_FRAMES} frames, \
         after {WARMUP_FRAMES} warm-up frames)\n"
    );
    println!(
        "| ROM | ms/frame | ns/instruction | instructions/frame | cycles/frame | cpu cycles/frame | ppu $2001 |"
    );
    println!("|---|---:|---:|---:|---:|---:|---|");
    let costs = corpus_costs();
    for cost in &costs {
        println!(
            "| {} | {:.2} | {:.2} | {:.0} | {:.0} | {:.0} | ${:02X} |",
            cost.name,
            cost.ms_per_frame(),
            cost.ns_per_instr(),
            cost.instr_per_frame(),
            cost.cycles_per_frame(),
            cost.cpu_cycles_per_frame(),
            cost.mask,
        );
        // NTSC is 29,780.5 CPU cycles per frame; PAL test ROMs do not exist in this corpus.
        assert!(
            (29_000.0..30_600.0).contains(&cost.cycles_per_frame()),
            "{}: {:.0} cycles/frame is not an NTSC frame",
            cost.name,
            cost.cycles_per_frame()
        );
        assert!(
            cost.instructions > 0 && cost.elapsed > Duration::ZERO,
            "{}",
            cost.name
        );
        assert!(cost.ns_per_instr() > 1.0, "{}: implausibly fast", cost.name);
    }
    assert_eq!(
        costs.len(),
        7,
        "four ROMs, AccuracyCoin twice, two synthetics"
    );
}

/// `f_est` for one synthetic program: its CPU-side share of frame time, measured with a bus that
/// does nothing.
///
/// The program is copied to `$8000` and the CPU reset through the image's own vector, exactly as
/// `Nes::new` does; then the same number of instructions a real frame ran is stepped [`CPU_ONLY_RUNS`]
/// times and the median ns/instruction is kept.
fn cpu_only_ns_per_instr(image: &[u8], instructions: u64) -> f64 {
    // iNES header (16 bytes), then the 32KB PRG that maps to $8000-$FFFF.
    let mut bus = TestBus::new();
    bus.mem[0x8000..0x10000].copy_from_slice(&image[16..16 + 0x8000]);
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    assert_eq!(cpu.pc, MAIN, "the reset vector must land on $8000");
    assert!(!cpu.jammed);
    let mut runs = Vec::with_capacity(CPU_ONLY_RUNS);
    for _ in 0..CPU_ONLY_RUNS {
        let start = Instant::now();
        for _ in 0..instructions {
            cpu.step(&mut bus);
        }
        runs.push(start.elapsed().as_nanos() as f64 / instructions as f64);
    }
    runs.sort_by(f64::total_cmp);
    runs[CPU_ONLY_RUNS / 2]
}

/// The cost split §8's kill criterion needs: how much of a frame is `Cpu::step`'s own work.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn cpu_only_cost() {
    println!("\n## CPU-side share `f_est` (same instruction count, trivial bus)\n");
    println!("| ROM | real ns/instruction | cpu-only ns/instruction | f_est |");
    println!("|---|---:|---:|---:|");
    let mut measured = 0;
    for (name, image) in [
        ("synthetic A (LDX/DEX/BNE)", tight_loop_image()),
        ("synthetic B (JSR/RTS)", call_loop_image()),
    ] {
        let real = measure(name, &image);
        let cpu_only = cpu_only_ns_per_instr(&image, real.instructions / real.frames as u64);
        let f_est = cpu_only / real.ns_per_instr();
        println!(
            "| {name} | {:.2} | {:.2} | {f_est:.3} |",
            real.ns_per_instr(),
            cpu_only
        );
        assert!(
            f_est > 0.0 && f_est < 1.0,
            "{name}: f_est {f_est} is not a share"
        );
        measured += 1;
    }
    assert_eq!(measured, 2, "both synthetic programs are measured");
}

/// Where AccuracyCoin's instructions go: the ten most common opcodes by count, counted from the
/// byte at PC before every step. Returns the instructions counted and the share of them that were
/// `JMP Abs` — the menu loop's signature, and how the caller checks that a Start press took.
fn print_mix(title: &str, nes: &mut Nes) -> (u64, f64) {
    let mut counts = [0u64; 256];
    let mut total = 0u64;
    for _ in 0..MEASURED_FRAMES {
        nes.bus.ppu.frame_ready = false;
        while !nes.bus.ppu.frame_ready {
            // The opcode the CPU is about to execute, read the way the tracer reads it: a peek, so
            // no bus cycle is consumed and I/O is never touched.
            let opcode = usize::from(nes.bus.peek(nes.cpu.pc).unwrap_or(0));
            counts[opcode] += 1;
            nes.step();
            total += 1;
        }
    }
    let mut ranked: Vec<(usize, u64)> = counts
        .iter()
        .enumerate()
        .filter(|&(_, &n)| n > 0)
        .map(|(op, &n)| (op, n))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("\n## {title} ({MEASURED_FRAMES} frames, {total} instructions)\n");
    println!("| # | opcode | instruction | count | share |");
    println!("|---:|---|---|---:|---:|");
    let mut top = 0u64;
    for (i, &(op, n)) in ranked.iter().take(10).enumerate() {
        let entry = OPS[op];
        let mnemonic = format!("{:?}", entry.mn).to_uppercase();
        let share = 100.0 * n as f64 / total as f64;
        let rank = i + 1;
        println!(
            "| {rank} | `${op:02X}` | {mnemonic} {:?} | {n} | {share:.2}% |",
            entry.mode
        );
        top += n;
    }
    println!(
        "\nTop 10 cover {:.1}% of {} instructions.\n",
        100.0 * top as f64 / total as f64,
        total
    );
    assert!(total > 0, "{title}: no instructions counted");
    assert!(top <= total, "the top ten cannot exceed the whole");
    (total, 100.0 * counts[0x4C] as f64 / total as f64)
}

/// Where AccuracyCoin's instructions go, measured twice: the menu loop the ROM boots into, and the
/// suite's first page once Start is pressed. The page is the workload §8 is about; the menu is what
/// an un-driven `Nes::run_frame` would report.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn instr_mix() {
    let mut menu = Nes::new(&rom("AccuracyCoin.nes")).unwrap();
    for _ in 0..WARMUP_FRAMES {
        run_frame(&mut menu);
    }
    let (_, menu_jmp) = print_mix("AccuracyCoin as booted (menu loop)", &mut menu);
    let (_, page_jmp) = print_mix("AccuracyCoin, page 1 running", &mut accuracycoin_running());
    // The menu is a `JMP Abs` spin; a page runs its tests. If the Start press ever stopped landing,
    // the second table would silently become the first, and every number derived from it with it.
    assert!(
        menu_jmp > 95.0,
        "the menu loop should be nearly all JMP: {menu_jmp:.2}%"
    );
    assert!(
        page_jmp < 95.0,
        "page 1 is still spinning in the menu loop: {page_jmp:.2}% JMP"
    );
}
