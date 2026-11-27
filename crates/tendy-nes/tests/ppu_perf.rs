//! Where `Ppu::tick`'s per-dot cost goes: the per-dot ablation, and the tables recorded in
//! `docs/ppu-profile.md`.
//!
//! Three of these print tables and are `#[ignore]`d so CI stays green; the fourth is the
//! `save_state()` pin and runs with the rest of the suite:
//!
//! ```text
//! cargo test --release --test ppu_perf -- --ignored --nocapture --test-threads=1
//! cargo test --release --test ppu_perf --features ppu-profile -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 1. [`ablation_matrix`] — four ROMs at four `$2001` settings: ms/frame and ns per PPU dot.
//! 2. [`phase_split`] — the four `tick` buckets and each one's share (needs `--features ppu-profile`).
//! 3. [`line_classes`] — ns per dot for a visible line, the post-render and vblank lines, and the
//!    pre-render line.
//! 4. [`default_build_state_is_unchanged`] — the `save_state()` digest pin for the default build.
//! 5. [`pixel_path_frame_time`] — the pixel-path before/after bench: two real ROMs at `$1E`.
//! 6. [`pixel_split`] — the six buckets inside `Phase::Pixel` (needs `--features ppu-profile`).
//!
//! ## Why the CPU does not run while a row is measured
//!
//! The ablation's whole point is that its four rows differ in `$2001` and in nothing else. Every
//! ROM's own code rewrites `$2001` (that is how a picture appears at all), so a run that let the CPU
//! clock would have to fight the ROM for the register. Instead the mask is **set once and never
//! touched again**: each run lifts the warmed PPU out of its console (`Ppu: Clone`) and ticks it
//! directly, with no CPU, APU or bus clock in the loop. Nothing left in the loop can write a
//! register, and every run asserts its mask is still the value it was given.
//!
//! What the PPU fetches still comes from a board in the console's own state: each run ticks a fresh
//! board loaded from the same image, and [`Rig::warm`] asserts that board's `Mapper::save_state()`
//! is byte for byte the console's own board (bank registers, PRG RAM, CHR RAM included). For the two
//! NROM images here that is trivially true; the assertion is what keeps it true for an image a later
//! image adds.
//!
//! Each ROM is warmed once and forked: the four settings and all five passes start from the same PPU
//! and board state, so a row differs from its neighbour in `$2001` and in nothing else.
//!
//! The price of that choice: these are PPU dots only, not whole-console frames, so `ms/frame` below
//! is the tick path's own frame time — the quantity being split — and not the 3.84 ms/frame
//! measured for CPU, bus and PPU together.

use std::time::{Duration, Instant};

use tendy_nes::cart::Mapper;
use tendy_nes::mapper;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::{DOTS_PER_LINE, LINES_PER_FRAME, Ppu};
use tendy_nes::state::fnv1a64;

/// Frames of warm-up before a measurement: enough for the PPU pipeline, the ROM's setup and the
/// mapper's bank state to reach the shape the rows are measured in.
const WARMUP_FRAMES: usize = 60;
/// Frames per timed pass. A pass is under a second of `Ppu::tick`, so 300 frames keeps the fixed
/// per-pass costs (a fork, the clock) out of the number while still being long enough that the
/// timer's own resolution is irrelevant.
const MEASURED_FRAMES: usize = 300;
/// Timed passes per row; the row reports their median. This host's timings move by more than some of
/// the differences between two `$2001` settings, so one pass is not enough to hang a decision on.
const PASSES: usize = 5;

/// The four `$2001` settings of the ablation. D3 enables the background, D4 the sprites, and D1/D2
/// are the left-8-column clips, which stay on in the setting that enables their layer: the ablation
/// separates the layers, not the clips.
const SETTINGS: [Setting; 4] = [
    Setting {
        name: "off",
        mask: 0x00,
    },
    Setting {
        name: "background only",
        mask: 0x0A,
    },
    Setting {
        name: "sprites only",
        mask: 0x14,
    },
    Setting {
        name: "both",
        mask: 0x1E,
    },
];

/// One row of the ablation: a name for the table and the `$2001` value written before the first dot.
struct Setting {
    name: &'static str,
    mask: u8,
}

/// What one timed run of [`MEASURED_FRAMES`] frames cost.
#[derive(Clone, Copy)]
struct Pass {
    elapsed: Duration,
    frames: usize,
    /// PPU dots ticked. A frame is 341 x 262 dots, less the dot an odd frame's pre-render line skips
    /// while rendering is on, so this is the honest denominator for ns/dot.
    dots: u64,
}

impl Pass {
    fn ms_per_frame(&self) -> f64 {
        self.elapsed.as_secs_f64() * 1e3 / self.frames as f64
    }

    fn ns_per_dot(&self) -> f64 {
        self.elapsed.as_nanos() as f64 / self.dots as f64
    }
}

/// The time and the dot count of one scanline index, summed over a run: the line-class table's unit.
#[derive(Clone, Copy, Default)]
struct LineCost {
    time: Duration,
    dots: u32,
}

impl LineCost {
    fn ns_per_dot(&self) -> f64 {
        if self.dots == 0 {
            0.0
        } else {
            self.time.as_nanos() as f64 / f64::from(self.dots)
        }
    }
}

/// A warmed PPU and the image it was warmed from: the state every row of a ROM starts from.
struct Rig {
    ppu: Ppu,
    image: Vec<u8>,
}

impl Rig {
    /// Run `walk` (the row's warm-up) on a fresh console, then lift the PPU out of it.
    ///
    /// The board a run ticks is a fresh load of the same image; this is where the harness proves it
    /// is the same board, by comparing its serialised board state with the console's own. A board
    /// whose state differs is a different workload, not an ablation.
    fn warm(image: &[u8], walk: fn(&mut Nes)) -> Rig {
        let mut nes = Nes::new(image).expect("the ROM loads");
        walk(&mut nes);
        let cart = mapper::load(image).expect("the board loads");
        assert_eq!(
            board_state(&*cart),
            board_state(nes.bus.cart()),
            "a fresh board is not the warmed console's own board"
        );
        Rig {
            ppu: nes.bus.ppu.clone(),
            image: image.to_vec(),
        }
    }

    /// A copy of the warmed machine for one run: the same PPU state and a board in the same state.
    fn fork(&self) -> (Ppu, Box<dyn Mapper>) {
        (
            self.ppu.clone(),
            mapper::load(&self.image).expect("the board loads"),
        )
    }
}

/// A board's mutable state, as the save-state writer serialises it (never ROM).
fn board_state(cart: &dyn Mapper) -> Vec<u8> {
    let mut w = tendy_nes::state::StateWriter::new();
    cart.save_state(&mut w);
    w.finish()
}

/// Tick `frames` frames of `rig`'s PPU with `$2001` pinned at `mask`.
///
/// The machine is driven one PPU dot at a time; `per_line` (when given) collects each scanline
/// index's time and dot count over every frame, which is what the line-class table needs.
fn run(rig: &Rig, mask: u8, frames: usize, mut per_line: Option<&mut [LineCost]>) -> Pass {
    let (mut ppu, mut cart) = rig.fork();
    ppu.mask = mask;
    // The warmed PPU stops at the start of vblank, so the rest of *that* frame is not a frame: tick
    // it away before the clock starts, or 300 "frames" would be 299 frames and a 21-line stub.
    let partial = ppu.frame;
    while ppu.frame == partial {
        ppu.tick(&mut *cart);
    }
    let mut dots = 0u64;
    let start = Instant::now();
    for _ in 0..frames {
        let frame = ppu.frame;
        while ppu.frame == frame {
            let line = ppu.scanline;
            // One clock read per scanline, and only when the caller asked for line classes: 262
            // reads a frame is well under a per-mille of the work they measure.
            let clock = per_line.as_ref().map(|_| Instant::now());
            let mut ticks = 0u32;
            while ppu.scanline == line && ppu.frame == frame {
                ppu.tick(&mut *cart);
                ticks += 1;
            }
            dots += u64::from(ticks);
            if let (Some(t0), Some(lines)) = (clock, per_line.as_mut()) {
                let lines: &mut [LineCost] = lines;
                let cost = &mut lines[usize::from(line)];
                cost.time += t0.elapsed();
                cost.dots += ticks;
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(
        ppu.mask, mask,
        "the mask changed under the run: this is not an ablation"
    );
    Pass {
        elapsed,
        frames,
        dots,
    }
}

/// [`PASSES`] runs of one setting from forks of the same warmed machine, median by elapsed time.
fn measure(rig: &Rig, mask: u8) -> Pass {
    let mut passes: Vec<Pass> = (0..PASSES)
        .map(|_| run(rig, mask, MEASURED_FRAMES, None))
        .collect();
    passes.sort_by_key(|pass| pass.elapsed);
    passes[PASSES / 2]
}

/// A test ROM from `roms/`, with the fetch hint every suite uses.
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

/// One frame driven exactly as `Nes::run_frame` drives it.
fn run_frame(nes: &mut Nes) {
    nes.bus.ppu.frame_ready = false;
    while !nes.bus.ppu.frame_ready {
        nes.step();
    }
}

/// [`WARMUP_FRAMES`] frames of whatever the ROM does on its own.
fn warm_up(nes: &mut Nes) {
    for _ in 0..WARMUP_FRAMES {
        run_frame(nes);
    }
}

/// AccuracyCoin boots into a menu that waits for input, spinning in a `JMP` loop. This is the same
/// Start press `tests/accuracycoin.rs::press` makes — A held for two frames, released for twelve —
/// that starts page 1 of the suite, so the rows measured after it are the ROM's real test code.
fn accuracycoin_page_one(nes: &mut Nes) {
    warm_up(nes);
    nes.bus.pads[0].buttons = tendy_nes::controller::BUTTON_A;
    for _ in 0..2 {
        run_frame(nes);
    }
    nes.bus.pads[0].buttons = 0;
    for _ in 0..12 {
        run_frame(nes);
    }
}

/// Put `bytes` at `addr` in a 32KB PRG image based at `$8000`.
fn put(prg: &mut [u8], addr: u16, bytes: &[u8]) {
    let o = usize::from(addr - 0x8000);
    prg[o..o + bytes.len()].copy_from_slice(bytes);
}

/// A busy 8KB CHR-ROM: all 1024 tiles of both pattern tables are non-zero patterns, so whatever the
/// background or the sprites fetch draws real pixels (a blank tile takes the same fetch path but
/// skips the colour lookup's non-zero arms).
fn busy_chr() -> Vec<u8> {
    (0..8192u32)
        .map(|i| if i % 16 < 8 { 0xB5 } else { 0xCA })
        .collect()
}

/// NROM-256 iNES image: 32KB PRG, 8KB CHR-ROM, reset/IRQ at the start of `code`, vectors last.
fn nrom_image(code: &[(u16, &[u8])], chr: Vec<u8>) -> Vec<u8> {
    let mut prg = vec![0xEAu8; 0x8000];
    for &(addr, bytes) in code {
        put(&mut prg, addr, bytes);
    }
    // NMI, reset, IRQ.
    prg[0x7FFA..0x8000].copy_from_slice(&[0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.extend(prg);
    v.extend(chr);
    v
}

/// Synthetic 1: a full-screen background of busy tiles that keeps scrolling — `$2005` is rewritten
/// from a zero-page counter on every vblank, so every frame fetches a different part of the same
/// nametable through the same four-fetch pattern.
///
/// Hand-assembled, so the labels and branch offsets are spelled out: `$8005`/`$800A` are the two
/// vblank waits, `$803B`/`$803D` and `$804A`/`$804C` are the nametable-copy loops (three pages of
/// tile 1, then the attribute page of 0), `$805F` is the scroll loop and the last `JMP` closes it.
fn scrolling_background_image() -> Vec<u8> {
    let code: &[u8] = &[
        0x78, // SEI
        0xD8, // CLD
        0xA2, 0xFF, // LDX #$FF
        0x9A, // TXS
        0x2C, 0x02, 0x20, // $8005: BIT $2002
        0x10, 0xFB, // BPL $8005  (wait for the first vblank)
        0x2C, 0x02, 0x20, // $800A: BIT $2002
        0x10, 0xFB, // BPL $800A  (and the second: the register warm-up is over)
        0xA9, 0x3F, // LDA #$3F
        0x8D, 0x06, 0x20, // STA $2006
        0xA9, 0x00, // LDA #$00
        0x8D, 0x06, 0x20, // STA $2006
        0xA9, 0x0F, // LDA #$0F
        0x8D, 0x07, 0x20, // STA $2007  ($3F00 backdrop)
        0xA9, 0x01, // LDA #$01
        0x8D, 0x07, 0x20, // STA $2007  ($3F01)
        0xA9, 0x21, // LDA #$21
        0x8D, 0x07, 0x20, // STA $2007  ($3F02)
        0xA9, 0x31, // LDA #$31
        0x8D, 0x07, 0x20, // STA $2007  ($3F03)
        0xA9, 0x20, // LDA #$20
        0x8D, 0x06, 0x20, // STA $2006
        0xA9, 0x00, // LDA #$00
        0x8D, 0x06, 0x20, // STA $2006  (v = $2000)
        0xA9, 0x01, // LDA #$01
        0xA0, 0x03, // LDY #$03
        0xA2, 0x00, // $803B: LDX #$00
        0x8D, 0x07, 0x20, // $803D: STA $2007  (tile 1, three pages of it)
        0xCA, // DEX
        0xD0, 0xFA, // BNE $803D
        0x88, // DEY
        0xD0, 0xF5, // BNE $803B
        0xA9, 0x00, // LDA #$00
        0xA0, 0x01, // LDY #$01
        0xA2, 0x00, // $804A: LDX #$00
        0x8D, 0x07, 0x20, // $804C: STA $2007  (attribute page, palette 0)
        0xCA, // DEX
        0xD0, 0xFA, // BNE $804C
        0x88, // DEY
        0xD0, 0xF5, // BNE $804A
        0xA9, 0x00, // LDA #$00
        0x8D, 0x00, 0x20, // STA $2000  (background table 0, nametable 0, no NMI)
        0xA9, 0x0A, // LDA #$0A
        0x8D, 0x01, 0x20, // STA $2001  (background on, left-8 on)
        0x2C, 0x02, 0x20, // $805F: BIT $2002  (wait for vblank)
        0x10, 0xFB, // BPL $805F
        0xE6, 0x00, // INC $00
        0xA5, 0x00, // LDA $00
        0x8D, 0x05, 0x20, // STA $2005  (X scroll)
        0xA5, 0x00, // LDA $00
        0x4A, // LSR A
        0x8D, 0x05, 0x20, // STA $2005  (Y scroll)
        0x4C, 0x5F, 0x80, // JMP $805F
    ];
    nrom_image(&[(0x8000, code)], busy_chr())
}

/// Synthetic 2: as many sprites in range on a line as an OAM allows — 8x16 sprites, eight of them
/// at each of `Y = 0, 16, ... 112`, so every line of the picture's top 128 has eight in range.
///
/// 64 sprites cannot put eight in range on *every* line: eight 8x16 sprites cover 128 lines, and
/// covering the other 112 would need 56 more sprites than the PPU has. This is therefore the densest
/// sprite workload the hardware can be driven into — at eight in range the evaluation writes all 32
/// secondary-OAM bytes, freezes its address and runs the overflow path, and the fetch window after
/// dot 256 walks all eight slots.
fn sprite_parade_image() -> Vec<u8> {
    let code: &[u8] = &[
        0x78, // SEI
        0xD8, // CLD
        0xA2, 0xFF, // LDX #$FF
        0x9A, // TXS
        0x2C, 0x02, 0x20, // $8005: BIT $2002
        0x10, 0xFB, // BPL $8005
        0x2C, 0x02, 0x20, // $800A: BIT $2002
        0x10, 0xFB, // BPL $800A
        0xA9, 0x00, // LDA #$00
        0x8D, 0x03, 0x20, // STA $2003  (OAMADDR 0)
        0xA2, 0x00, // LDX #$00
        0xBD, 0x00, 0x84, // $8016: LDA $8400,X  (the OAM shadow, copied in by hand)
        0x8D, 0x04, 0x20, // STA $2004
        0xE8, // INX
        0xD0, 0xF7, // BNE $8016
        0xA9, 0x20, // LDA #$20
        0x8D, 0x00, 0x20, // STA $2000  (8x16 sprites, table 0)
        0xA9, 0x14, // LDA #$14
        0x8D, 0x01, 0x20, // STA $2001  (sprites on, left-8 on)
        0x4C, 0x29,
        0x80, // JMP $8029  (OAM never changes again, so the CPU has nothing to do)
    ];
    // OAM: eight 8x16 sprites in each 16-line band, spread across the picture's width.
    let oam: Vec<u8> = (0..64usize)
        .flat_map(|i| [(i / 8) as u8 * 16, 1, 0, (i % 8) as u8 * 32])
        .collect();
    nrom_image(&[(0x8000, code), (0x8400, &oam[..])], busy_chr())
}

/// One corpus entry: a label for the tables, the iNES image, and the warm-up its row asks for.
type Rom = (&'static str, Vec<u8>, fn(&mut Nes));

/// The corpus: the two real ROMs this file names and the two synthetic workloads.
fn corpus() -> [Rom; 4] {
    [
        (
            "AccuracyCoin (page 1)",
            rom("AccuracyCoin.nes"),
            accuracycoin_page_one,
        ),
        ("nestest", rom("nestest.nes"), warm_up),
        (
            "synthetic: scrolling background",
            scrolling_background_image(),
            warm_up,
        ),
        (
            "synthetic: 8 sprites a line",
            sprite_parade_image(),
            warm_up,
        ),
    ]
}

/// The ablation matrix: for each ROM, what a PPU-only frame costs with rendering off, with the
/// background alone, with the sprites alone, and with both.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn ablation_matrix() {
    let frame_dots = f64::from(DOTS_PER_LINE) * f64::from(LINES_PER_FRAME);
    let mut measured: Vec<(&'static str, Vec<Pass>)> = Vec::with_capacity(4);
    for (name, image, walk) in corpus() {
        let rig = Rig::warm(&image, walk);
        let mut passes = Vec::with_capacity(SETTINGS.len());
        for setting in &SETTINGS {
            let pass = measure(&rig, setting.mask);
            let dots_per_frame = pass.dots as f64 / pass.frames as f64;
            assert!(
                (frame_dots - 2.0..=frame_dots).contains(&dots_per_frame),
                "{name}: {dots_per_frame:.1} dots/frame is not an NTSC frame"
            );
            assert!(pass.elapsed > Duration::ZERO, "{name}: nothing was timed");
            passes.push(pass);
        }
        measured.push((name, passes));
    }
    assert_eq!(measured.len(), 4, "four ROMs");

    println!(
        "\n## Ablation: median of {PASSES} passes of {MEASURED_FRAMES} frames after {WARMUP_FRAMES} warm-up \
         frames, PPU dots only (no CPU), every pass forked from one warm-up\n"
    );
    println!("| ROM | $2001 | setting | ms/frame | ns/dot | vs both |");
    println!("|---|---:|---|---:|---:|---:|");
    for (name, passes) in &measured {
        let both = passes[3].ns_per_dot();
        for (setting, pass) in SETTINGS.iter().zip(passes) {
            println!(
                "| {name} | ${:02X} | {} | {:.3} | {:.2} | {:+.1}% |",
                setting.mask,
                setting.name,
                pass.ms_per_frame(),
                pass.ns_per_dot(),
                100.0 * (pass.ns_per_dot() - both) / both
            );
        }
    }

    println!(
        "\n### What each layer costs, in ns per dot (the difference the ablation exists for)\n"
    );
    println!(
        "| ROM | rendering off | background only | sprites only | both | background | sprites | background + sprites | both - (off + bg + sprites) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (name, passes) in &measured {
        let (off, bg, spr, both) = (
            passes[0].ns_per_dot(),
            passes[1].ns_per_dot(),
            passes[2].ns_per_dot(),
            passes[3].ns_per_dot(),
        );
        let (bg_cost, spr_cost) = (bg - off, spr - off);
        println!(
            "| {name} | {off:.2} | {bg:.2} | {spr:.2} | {both:.2} | {bg_cost:+.2} | {spr_cost:+.2} | {:+.2} | {:+.2} |",
            both - off,
            both - (off + bg_cost + spr_cost)
        );
        // A layer that is switched on cannot be cheaper than the same dots with it off: if these
        // hold, the rows are measuring the layers and not the noise between passes.
        assert!(
            bg > off,
            "{name}: background only ({bg:.2}) is not above off ({off:.2})"
        );
        assert!(
            spr > off,
            "{name}: sprites only ({spr:.2}) is not above off ({off:.2})"
        );
        assert!(
            both > off,
            "{name}: both ({both:.2}) is not above off ({off:.2})"
        );
    }
    println!();
}

/// The four buckets of `Ppu::tick`, and each one's share of the time a run spent in it.
///
/// This is the `ppu-profile` build's table. The same rows in the default build ([`ablation_matrix`])
/// are the uninstrumented cost, so running this test binary twice — once with the feature and once
/// without — measures the counters' own overhead as well as the bucket split.
#[cfg(feature = "ppu-profile")]
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn phase_split() {
    use tendy_nes::ppu::profile::{self, Phase};

    println!(
        "
## Phase counters (`--features ppu-profile`), {MEASURED_FRAMES} frames per run

Each row is two runs of the same 300 frames: one with the counters live, one paused. The paused run
is the same loop with no clock reads in it, so `paused` is what a dot really costs and `live` is
what the probe makes it cost. `read` is solved from conservation — the five phases have to add up to
`paused`, so whatever a sample's span carries over that is the clock read — and the second table
subtracts it.
"
    );

    struct Row {
        rom: &'static str,
        mask: u8,
        /// Raw per-phase ns/dot: the sample's span, clock read included.
        raw: [f64; 5],
        /// The probe's in-place calibration of that read, for comparison with the solved `read`.
        clock: f64,
        /// The same 300 frames with the counters paused: what the instrumented loop costs bare.
        paused: f64,
        /// The same 300 frames with the counters live.
        live: f64,
        samples: u64,
    }
    let mut rows = Vec::new();
    for (name, image, walk) in corpus() {
        let rig = Rig::warm(&image, walk);
        for setting in [&SETTINGS[3], &SETTINGS[0]] {
            // Three (paused, live) pairs, median by the live run: one 300-frame pass of a hot loop is
            // noisier than the difference between two settings, and the paused run is the number the
            // buckets are solved against.
            let mut pairs = Vec::with_capacity(3);
            for _ in 0..3 {
                profile::pause();
                let paused = run(&rig, setting.mask, MEASURED_FRAMES, None);
                profile::resume();
                profile::reset();
                let live = run(&rig, setting.mask, MEASURED_FRAMES, None);
                pairs.push((paused.ns_per_dot(), live.ns_per_dot(), profile::take()));
            }
            pairs.sort_by(|a, b| a.1.total_cmp(&b.1));
            let (paused, live, totals) = pairs[1];
            let mut raw = [0.0f64; 5];
            let mut samples = 0;
            for (i, phase) in Phase::ALL.into_iter().enumerate() {
                assert!(
                    totals.samples(phase) > 0,
                    "{name}: no sample for {phase:?}: {totals:?}"
                );
                samples += totals.samples(phase);
                raw[i] = totals.ns(phase) as f64 / totals.samples(phase) as f64;
            }
            rows.push(Row {
                rom: name,
                mask: setting.mask,
                raw,
                clock: totals.clock_cost_ns(),
                paused,
                live,
                samples,
            });
        }
    }
    assert_eq!(rows.len(), 8, "four ROMs at two settings");

    println!(
        "| ROM | $2001 | bookkeeping head | background fetch/shift | pixel output | sprite eval | bookkeeping tail | sum | clock read (in place) | paused ns/dot | live ns/dot | samples |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        let sum: f64 = row.raw.iter().sum();
        println!(
            "| {} | ${:02X} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {sum:.2} | {:.1} | {:.2} | {:.2} | {} |",
            row.rom,
            row.mask,
            row.raw[0],
            row.raw[1],
            row.raw[2],
            row.raw[3],
            row.raw[4],
            row.clock,
            row.paused,
            row.live,
            row.samples
        );
    }

    println!("\n### The four buckets, as §Method asks for them (bookkeeping = head + tail)\n");
    println!(
        "| ROM | $2001 | read solved | background fetch/shift | pixel output | sprite eval | tick bookkeeping | buckets ns/dot | paused ns/dot | probe adds |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        let paused = row.paused;
        // Conservation: the five phases partition the dot, so the overhead each sample's span carries
        // is whatever has to come out of them for their sum to be what a bare dot costs.
        let read = (row.raw.iter().sum::<f64>() - paused) / 5.0;
        assert!(
            read > 0.0,
            "{}: a negative clock read ({read:.2} ns) makes no sense",
            row.rom
        );
        let corrected: Vec<f64> = row.raw.iter().map(|raw| (raw - read).max(0.0)).collect();
        let (head, bg, pixel, sprite, tail) = (
            corrected[0],
            corrected[1],
            corrected[2],
            corrected[3],
            corrected[4],
        );
        let book = head + tail;
        let total = bg + pixel + sprite + book;
        let share = |ns: f64| 100.0 * ns / total;
        // What the probe costs the loop, per dot: the whole difference between the two runs.
        let inflation = 100.0 * (row.live - paused) / paused;
        println!(
            "| {} | ${:02X} | {read:.1} | {:.1}% ({bg:.2}) | {:.1}% ({pixel:.2}) | {:.1}% ({sprite:.2}) | {:.1}% ({book:.2}) | {total:.2} | {paused:.2} | {inflation:+.0}% |",
            row.rom,
            row.mask,
            share(bg),
            share(pixel),
            share(sprite),
            share(book)
        );
        // Conservation is the gate: with the solved read, the buckets must add up to the bare dot.
        assert!(
            (total - paused).abs() < 0.02 * paused,
            "{}: the buckets ({total:.2}) do not add up to the paused dot ({paused:.2})",
            row.rom
        );
        // The in-place calibration and the solved read have to agree roughly, or the subtraction is
        // measuring the machine rather than the code.
        assert!(
            (row.clock - read).abs() < 0.25 * read,
            "{}: in-place clock {:.1} ns and the solved {read:.1} ns disagree",
            row.rom,
            row.clock
        );
    }
    println!();
}

/// The two ROMs `docs/ppu-pixel.md` measures: AccuracyCoin page 1 (real code, a busy picture) and the
/// densest sprite workload an OAM allows.
fn pixel_corpus() -> [Rom; 2] {
    [
        (
            "AccuracyCoin (page 1)",
            rom("AccuracyCoin.nes"),
            accuracycoin_page_one as fn(&mut Nes),
        ),
        (
            "synthetic: 8 sprites a line",
            sprite_parade_image(),
            warm_up as fn(&mut Nes),
        ),
    ]
}

/// The pixel-path bench: the same two ROMs, `$1E` only, median of [`PASSES`] x [`MEASURED_FRAMES`].
///
/// This is the before/after row of `docs/ppu-pixel.md`: one number per ROM that two builds can both
/// be run through, with the same forked-machine discipline as the ablation. It is a separate test
/// from [`ablation_matrix`] because that one measures sixteen rows to answer a different question,
/// and a change's before/after should not depend on it.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn pixel_path_frame_time() {
    let frame_dots = f64::from(DOTS_PER_LINE) * f64::from(LINES_PER_FRAME);
    println!(
        "\n## Pixel path: PPU-only frame time at $1E, median of {PASSES} passes of {MEASURED_FRAMES} frames\n"
    );
    println!("| ROM | ms/frame | ns/dot |");
    println!("|---|---:|---:|");
    let mut rows = 0;
    for (name, image, walk) in pixel_corpus() {
        let rig = Rig::warm(&image, walk);
        let pass = measure(&rig, 0x1E);
        let dots_per_frame = pass.dots as f64 / pass.frames as f64;
        assert!(
            (frame_dots - 2.0..=frame_dots).contains(&dots_per_frame),
            "{name}: {dots_per_frame:.1} dots/frame is not an NTSC frame"
        );
        println!(
            "| {name} | {:.3} | {:.2} |",
            pass.ms_per_frame(),
            pass.ns_per_dot()
        );
        rows += 1;
    }
    assert_eq!(rows, 2, "the two real ROMs");
    println!();
}

/// The sub-buckets inside `Phase::Pixel`: what one output dot costs, split six ways.
///
/// Three runs per ROM, and each instrument is silent while the other measures, so neither inflates
/// the other's spans:
///
/// 1. **paused** — both probes off: what a dot really costs.
/// 2. **phases** — the outer probe alone: the outer split, whose pixel bucket is what the six
///    sub-buckets have to add up to (the conservation reference).
/// 3. **buckets** — the sub-probe alone: one bucket of one output dot in eight.
///
/// As in [`phase_split`], the read each sample carries is solved from conservation rather than
/// assumed, and the in-place calibration is printed beside it for comparison.
#[cfg(feature = "ppu-profile")]
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn pixel_split() {
    use tendy_nes::ppu::profile::{self, Bucket, Phase};

    println!(
        "\n## Pixel sub-buckets (`--features ppu-profile`), {MEASURED_FRAMES} frames per run\n"
    );
    struct Row {
        rom: &'static str,
        /// Raw ns per sampled dot of each body bucket, and of the finalize bucket.
        raw: [f64; 6],
        samples: [u64; 6],
        clock: f64,
        paused: f64,
        live: f64,
        /// The outer pixel bucket, per output dot, from the phases run.
        pixel_per_dot: f64,
        /// Body samples, finalize samples.
        body_samples: u64,
        fin_samples: u64,
    }
    let mut rows = Vec::new();
    for (name, image, walk) in pixel_corpus() {
        let rig = Rig::warm(&image, walk);
        // The conservation reference: the outer split, with the sub-probe silent.
        let mut pairs = Vec::with_capacity(3);
        for _ in 0..3 {
            profile::pixel_pause();
            profile::pause();
            let paused = run(&rig, 0x1E, MEASURED_FRAMES, None);
            profile::resume();
            profile::reset();
            let live = run(&rig, 0x1E, MEASURED_FRAMES, None);
            pairs.push((paused.ns_per_dot(), live.ns_per_dot(), profile::take()));
        }
        pairs.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (paused, _live, totals) = pairs[1];
        let sum_raw: f64 = Phase::ALL
            .iter()
            .map(|p| totals.ns(*p) as f64 / totals.samples(*p) as f64)
            .sum();
        let read = (sum_raw - paused) / Phase::ALL.len() as f64;
        let pixel_raw = totals.ns(Phase::Pixel) as f64 / totals.samples(Phase::Pixel) as f64;
        // Per frame dot -> per output dot: the pixel phase runs on 61 440 of the frame's 89 342.
        let dots_per_frame = 341.0 * 262.0;
        let output_dots = 240.0 * 256.0;
        let pixel_per_dot = (pixel_raw - read) * dots_per_frame / output_dots;

        // The sub-buckets, with the outer probe silent.
        profile::pause();
        profile::pixel_resume();
        let _ = profile::pixel_take();
        let live_buckets = run(&rig, 0x1E, MEASURED_FRAMES, None);
        let buckets = profile::pixel_take();
        let mut raw = [0.0f64; 6];
        let mut samples = [0u64; 6];
        for (i, bucket) in Bucket::ALL.into_iter().enumerate() {
            samples[i] = buckets.samples(bucket);
            raw[i] = if samples[i] == 0 {
                0.0
            } else {
                buckets.ns(bucket) as f64 / samples[i] as f64
            };
        }
        // Every body bucket is armed on the same dots; the ones that fire conditionally (a sprite
        // winning) have fewer samples, so they are normalised by the population, not by their own
        // count. `BgPixel` always fires on a body sample, so it is the denominator.
        let body_samples = samples[Bucket::BgPixel as usize];
        assert!(body_samples > 0, "{name}: no body samples");
        assert!(
            samples[Bucket::Finalize as usize] > 0,
            "{name}: no finalize samples"
        );
        rows.push(Row {
            rom: name,
            raw,
            samples,
            clock: buckets.clock_cost_ns(),
            paused,
            live: live_buckets.ns_per_dot(),
            pixel_per_dot,
            body_samples,
            fin_samples: samples[Bucket::Finalize as usize],
        });
    }
    assert_eq!(rows.len(), 2, "the two real ROMs");

    println!("Raw spans per sampled dot (each carries one clock read), and the sample counts:\n");
    println!(
        "| ROM | bg_pixel | bg palette | sprite_pixel | sprite palette | lag push | finalize | clock read (in place) | samples (body / finalize) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        println!(
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.1} | {} / {} |",
            row.rom,
            row.raw[0],
            row.raw[1],
            row.raw[2],
            row.raw[3],
            row.raw[4],
            row.raw[5],
            row.clock,
            row.body_samples,
            row.fin_samples
        );
    }

    println!(
        "\nSamples taken per bucket. A body bucket with fewer samples than `bg_pixel` fires only on some\n\
              dots: `sprite palette` is the lookup on the dots where a sprite won, so a zero is \"the eight\n\
              slots drew nothing on any sampled dot of this ROM\", not a missing measurement.\n"
    );
    println!(
        "| ROM | bg_pixel | bg palette | sprite_pixel | sprite palette | lag push | finalize |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            row.rom,
            row.samples[0],
            row.samples[1],
            row.samples[2],
            row.samples[3],
            row.samples[4],
            row.samples[5]
        );
    }

    println!(
        "\nAfter the solved read, as ns per output dot (the six must add up to the outer pixel bucket):\n"
    );
    println!(
        "| ROM | bg_pixel | bg palette | sprite_pixel | sprite palette | lag push | finalize | sub-buckets | outer pixel bucket | read solved |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for row in &rows {
        // Each bucket's contribution to the dot is its total time over the population the buckets
        // share, minus one read per sample it took. A bucket that only fires when a sprite wins
        // (the sprite palette lookup) took fewer samples than the others, so it is normalised by the
        // population and carries correspondingly fewer reads: `A + C - read * (B + D)`.
        let body = row.body_samples as f64;
        let fin = row.fin_samples as f64;
        let total_ns = |i: usize| row.raw[i] * row.samples[i] as f64;
        let (a, b): (f64, f64) = (0..5).fold((0.0, 0.0), |(a, b), i| {
            (a + total_ns(i) / body, b + row.samples[i] as f64 / body)
        });
        let (c, d) = (total_ns(5) / fin, row.samples[5] as f64 / fin);
        let read = (a + c - row.pixel_per_dot) / (b + d);
        let mut corrected = [0.0f64; 6];
        for i in 0..5 {
            corrected[i] = (total_ns(i) - row.samples[i] as f64 * read) / body;
        }
        corrected[5] = (total_ns(5) - row.samples[5] as f64 * read) / fin;
        let total: f64 = corrected.iter().sum();
        println!(
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {total:.2} | {:.2} | {read:.2} |",
            row.rom,
            corrected[0],
            corrected[1],
            corrected[2],
            corrected[3],
            corrected[4],
            corrected[5],
            row.pixel_per_dot
        );
        assert!(
            read > 0.0,
            "{}: a negative clock read ({read:.2} ns) makes no sense",
            row.rom
        );
        assert!(
            (row.clock - read).abs() < 0.25 * read,
            "{}: in-place clock {:.1} ns and the solved {read:.1} ns disagree",
            row.rom,
            row.clock
        );
        assert!(
            (total - row.pixel_per_dot).abs() < 0.10 * row.pixel_per_dot,
            "{}: the sub-buckets ({total:.2}) do not add up to the outer pixel bucket ({:.2})",
            row.rom,
            row.pixel_per_dot
        );
    }
    println!(
        "\nEach sample costs one clock read, so `read solved` is what comes out of the six buckets for\n\
              them to add up to the outer pixel bucket; `clock read (in place)` is the probe's own\n\
              calibration of the same read, and the two have to agree (the test asserts 25%).\n"
    );
    println!("| ROM | paused ns/dot | sub-probe live ns/dot | probe adds |");
    println!("|---|---:|---:|---:|");
    for row in &rows {
        println!(
            "| {} | {:.2} | {:.2} | {:+.0}% |",
            row.rom,
            row.paused,
            row.live,
            100.0 * (row.live - row.paused) / row.paused
        );
    }
    println!();
}

/// ns per dot for each kind of scanline: the number the line-class table and H4 turn on.
#[test]
#[ignore = "perf harness: prints tables and takes minutes; run by hand under --nocapture"]
fn line_classes() {
    println!(
        "\n## Line classes: ns per dot by scanline ({MEASURED_FRAMES} frames, median of {PASSES} passes)\n"
    );
    println!(
        "| ROM | $2001 | visible 0-239 | line 240 (post-render) | vblank 241-260 | pre-render 261 |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    let mut rows = 0;
    for (name, image, walk) in corpus() {
        // One extra setting for the first ROM: the same visible lines with rendering forced off, so
        // the visible column can be read as "the pipeline" and "bookkeeping only" side by side.
        let settings: &[&Setting] = if name.starts_with("AccuracyCoin") {
            &[&SETTINGS[3], &SETTINGS[0]]
        } else {
            &[&SETTINGS[3]]
        };
        let rig = Rig::warm(&image, walk);
        for setting in settings {
            let mut passes: Vec<(Pass, Vec<LineCost>)> = Vec::with_capacity(PASSES);
            for _ in 0..PASSES {
                let mut lines = vec![LineCost::default(); usize::from(LINES_PER_FRAME)];
                let pass = run(&rig, setting.mask, MEASURED_FRAMES, Some(&mut lines));
                passes.push((pass, lines));
            }
            passes.sort_by_key(|(pass, _)| pass.elapsed);
            let (pass, lines) = &passes[PASSES / 2];
            let visible = mean(&lines[0..240]);
            let post_render = lines[240].ns_per_dot();
            let vblank = mean(&lines[241..261]);
            let pre_render = lines[261].ns_per_dot();
            println!(
                "| {name} | ${:02X} | {visible:.2} | {post_render:.2} | {vblank:.2} | {pre_render:.2} |",
                setting.mask
            );
            // H4 asks whether the idle dots cost what a visible dot costs. They cannot: the fetch,
            // sprite and pixel blocks are all gated on the line. The assertion is what makes the
            // verdict a measurement instead of a reading of the code.
            assert!(
                vblank < visible,
                "{name}: vblank {vblank:.2} is not below visible {visible:.2}"
            );
            assert!(pass.dots > 0, "{name}: nothing was ticked");
            rows += 1;
        }
    }
    assert_eq!(rows, 5, "four ROMs, plus rendering off for one of them");
    println!();
}

/// The ns/dot of a set of scanlines, weighted by their dots (an odd frame's pre-render line has one
/// dot fewer).
fn mean(lines: &[LineCost]) -> f64 {
    let dots: u32 = lines.iter().map(|l| l.dots).sum();
    let time: Duration = lines.iter().map(|l| l.time).sum();
    if dots == 0 {
        0.0
    } else {
        time.as_nanos() as f64 / f64::from(dots)
    }
}

/// `save_state()` after [`MEASURED_FRAMES`] frames of `nestest`, pinned from the build before the
/// profiler: the digest and the state's length. The profiler changes no emulated behaviour, and with the
/// `ppu-profile` feature off it changes no code that runs at all, so this must stay exactly here.
/// It moved once, for `STATE_VERSION` 12 (MMC1 SOROM/SXROM PRG-RAM size): the serialised version word
/// is part of the digest and nothing else changed - the
/// length is the same and the pre-bump build reproduces 0x55f7_9152_facb_0738 exactly.
const NESTEST_300_STATE: (u64, usize) = (0x0490_7675_064c_0adb, 138_372);

/// The pin that proves the counters are compiled out of the default build: the same 300 frames, the
/// same serialised machine, byte for byte.
#[test]
fn default_build_state_is_unchanged() {
    let image = rom("nestest.nes");
    let mut nes = Nes::new(&image).expect("nestest loads");
    for _ in 0..MEASURED_FRAMES {
        run_frame(&mut nes);
    }
    let state = nes.save_state();
    let digest = fnv1a64(&state);
    println!(
        "nestest, {MEASURED_FRAMES} frames: save_state() = {} bytes, fnv1a64 = {digest:#018x}",
        state.len()
    );
    assert_eq!(
        (digest, state.len()),
        NESTEST_300_STATE,
        "the default build's save_state() no longer matches the pinned state"
    );
}
