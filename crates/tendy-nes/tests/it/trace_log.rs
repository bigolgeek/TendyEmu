//! The trace logger: the `docs/spec-trace.md` §5 cases that pin the file format and the
//! `--trace-limit` stop (cases 1, 2, 5, 6).
//!
//! `tests/trace.rs` keeps guarding the legacy `trace_line` / `trace_line_disasm` one-liners
//! against `nestest.log`; this file covers only the §1.1 full-run format and `Tracer`'s limit.

use std::io::{BufWriter, Write};
use tendy_nes::bus::RamPowerOn;
use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;
use tendy_nes::trace::{FrameEnd, Tracer};

/// NROM image: 16KB PRG, 8KB CHR, reset vector -> $C000 (PRG offset 0 of the mirrored bank), so
/// `code` runs from `$C000` exactly as `tests/trace.rs` places nestest at `$C000`.
fn rom_with(code: &[u8]) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16..16 + code.len()].copy_from_slice(code);
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0xC0;
    v
}

/// §5 case 1's program: six instructions covering immediate, zero-page +X, absolute, a taken
/// relative branch, a JSR (the 3-byte byte column) and one illegal opcode (`$1A`, the `*` marker).
///
/// ```text
/// $C000  A9 42     LDA #$42      ; Z=0, so the branch below is taken
/// $C002  95 10     STA $10,X
/// $C004  8D 00 04  STA $0400
/// $C007  D0 02     BNE $C00B
/// $C00B  20 20 C0  JSR $C020
/// $C020  1A        *NOP          ; illegal, one byte
/// $C021  60        RTS
/// ```
fn format_rom() -> Vec<u8> {
    let mut prg = vec![0u8; 16384];
    prg[..9].copy_from_slice(&[0xA9, 0x42, 0x95, 0x10, 0x8D, 0x00, 0x04, 0xD0, 0x02]);
    prg[0x0B..0x0E].copy_from_slice(&[0x20, 0x20, 0xC0]);
    prg[0x20] = 0x1A;
    prg[0x21] = 0x60;
    rom_with(&prg)
}

/// An unbounded instruction stream for the limit test: `NOP` then `JMP $C001` forever.
fn loop_rom() -> Vec<u8> {
    rom_with(&[0xEA, 0x4C, 0x01, 0xC0])
}

/// A program that turns rendering on and keeps changing RAM, so the determinism test compares a
/// non-trivial framebuffer and non-trivial CPU/PPU state, not two blank machines.
fn determinism_rom() -> Vec<u8> {
    let code = [
        0xA9, 0x3F, 0x8D, 0x06, 0x20, // LDA #$3F / STA $2006        ($C000)
        0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00 / STA $2006        ($C005)
        0xA9, 0x30, 0x8D, 0x07, 0x20, // LDA #$30 / STA $2007  ($3F00 backdrop)
        0xA9, 0x0F, 0x8D, 0x07, 0x20, // LDA #$0F / STA $2007  ($3F01)
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E / STA $2001  (show bg + sprites)
        0xA5, 0x10, // LDA $10                ($C019)
        0xE6, 0x10, // INC $10                ($C01B)
        0x1A, // illegal NOP                    ($C01D)
        0x4C, 0x19, 0xC0, // JMP $C019        ($C01E)
    ];
    rom_with(&code)
}

/// §5 case 1: every column of the §1.1 line, byte for byte.
///
/// The literals below are pinned by hand against the §1.1 field table. Taking line 1:
/// frame `0` (row 1, unpadded) `:` scanline `  0` (row 2, right-aligned in 3) `:` dot `  6`
/// (row 3) ` ` (row 4, one space) `C000` (row 5, `%04X`) two spaces (row 6) `A9 42    `
/// (row 7, `{:<9}`) ` ` (row 8, one marker column, official) `LDA #$42` + 24 spaces (row 9,
/// `{:<32}`) then the 25-character register block with no separator (rows 10-11) and
/// ` CYC:7` (row 12). The registers start at char 58: the 10-character `0:  0:  6 ` prefix plus
/// the grid's 48-character position (row 10). Line 6 is the illegal-opcode case: marker `*` in
/// the same single column, `1A` padded to the 9-wide byte field.
#[test]
fn format_matches_pinned_column_layout() {
    let mut nes = Nes::new(&format_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::new(Vec::new());
    for _ in 0..6 {
        tracer.log_file_line(&mut nes).expect("write to a Vec");
    }
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "0:  0:  6 C000  A9 42     LDA #$42                        A:00 X:00 Y:00 P:24 SP:FD CYC:7",
            "0:  0: 12 C002  95 10     STA $10,X                       A:42 X:00 Y:00 P:24 SP:FD CYC:9",
            "0:  0: 24 C004  8D 00 04  STA $0400                       A:42 X:00 Y:00 P:24 SP:FD CYC:13",
            "0:  0: 36 C007  D0 02     BNE $C00B                       A:42 X:00 Y:00 P:24 SP:FD CYC:17",
            "0:  0: 45 C00B  20 20 C0  JSR $C020                       A:42 X:00 Y:00 P:24 SP:FD CYC:20",
            "0:  0: 63 C020  1A       *NOP                             A:42 X:00 Y:00 P:24 SP:FB CYC:26",
        ]
    );
    // The structural pins the literals encode: 10-char prefix, 16-char PC/bytes/marker group,
    // 32-char text group, registers at 58, and one marker column at line index 25.
    for line in &lines {
        assert_eq!(&line[58..60], "A:", "registers start at char 58: {line}");
        assert_eq!(
            &line[10..14],
            &line[10..14].to_uppercase(),
            "PC is upper hex: {line}"
        );
    }
    assert_eq!(
        lines[0].as_bytes()[25],
        b' ',
        "official opcode marker is a space"
    );
    assert_eq!(
        lines[5].as_bytes()[25],
        b'*',
        "illegal opcode marker is one `*`"
    );
    assert_eq!(lines[5].len(), 90);
}

/// §5 case 2: `--trace-limit` writes exactly N lines and stops without stepping an extra
/// instruction, so the machine sits at the same cycle as N hand-run steps of the same program.
#[test]
fn limit_stops_emulation_after_n_lines() {
    const N: u64 = 20;
    let mut logged = Nes::new(&loop_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::new(Vec::new()).stop_after(N);
    // The run loop's shape: trace one frame at a time until the budget is spent mid-frame.
    let mut end = FrameEnd::Frame;
    while end == FrameEnd::Frame {
        end = tracer.log_frame(&mut logged).expect("write to a Vec");
    }
    assert_eq!(
        end,
        FrameEnd::Limit,
        "a 20-line budget must stop mid-frame, not at a boundary"
    );
    assert_eq!(tracer.lines, N, "the counter counts logged lines");
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    assert_eq!(
        text.lines().count() as u64,
        N,
        "one line per logged instruction"
    );

    // No extra instruction: N hand-steps from the same power-on state leave the same machine.
    let mut hand = Nes::new(&loop_rom()).expect("synthetic NROM");
    for _ in 0..N {
        hand.step();
    }
    assert_eq!(
        logged.cpu.cycles, hand.cpu.cycles,
        "the limit must not step past the N-th line"
    );
    assert_eq!(logged.cpu.pc, hand.cpu.pc);
    assert_eq!(logged.save_state(), hand.save_state());
}

/// §5 case 5: logging changes nothing — same seeded power-on, same frame count, same state bytes
/// and same framebuffer whether or not a tracer drove the run (§4.3).
#[test]
fn trace_log_does_not_perturb_emulation() {
    const SEED: u64 = 0x5EED_0BAD_C0FF_EE00;
    const FRAMES: u32 = 3;
    let mut plain = Nes::new_with_power_on(&determinism_rom(), RamPowerOn::Random, SEED)
        .expect("synthetic NROM");
    let mut logged = Nes::new_with_power_on(&determinism_rom(), RamPowerOn::Random, SEED)
        .expect("synthetic NROM");
    let mut tracer = Tracer::new(Vec::new());
    for _ in 0..FRAMES {
        plain.run_frame();
        assert_eq!(
            tracer.log_frame(&mut logged).expect("write to a Vec"),
            FrameEnd::Frame
        );
    }
    assert!(tracer.lines > 0, "the traced run logged nothing");
    assert!(
        plain.bus.ppu.framebuffer.iter().any(|&b| b != 0),
        "the synthetic ROM should render something, or the digest compares two blank frames"
    );
    assert_eq!(
        plain.save_state(),
        logged.save_state(),
        "logging perturbed the machine state"
    );
    assert_eq!(
        fnv1a64(&plain.bus.ppu.framebuffer),
        fnv1a64(&logged.bus.ppu.framebuffer),
        "logging perturbed the picture"
    );
}

/// §5 case 6: the `BufWriter` is flushed at every frame boundary, so a run stopped mid-frame has
/// every completed frame's lines on disk and never a partial line (§2's killed-run rule).
#[test]
fn buffer_flushes_per_frame() {
    let path = std::env::temp_dir().join(format!("nes-trace-log-flush-{}.log", std::process::id()));
    let file = std::fs::File::create(&path).expect("temp trace log");
    let mut tracer = Tracer::new(BufWriter::new(file));
    let mut nes = Nes::new(&loop_rom()).expect("synthetic NROM");
    // Three completed frames, flushing at each boundary exactly as the run loop does.
    let mut lines_per_frame = Vec::new();
    for _ in 0..3 {
        assert_eq!(
            tracer.log_frame(&mut nes).expect("write to the temp file"),
            FrameEnd::Frame
        );
        tracer.out.flush().expect("flush at the frame boundary");
        lines_per_frame.push(tracer.lines);
    }
    // Mid-frame stop: log a few more instructions of frame 4 and never flush them.
    for _ in 0..5 {
        tracer
            .log_file_line(&mut nes)
            .expect("write to the temp file");
    }
    let on_disk = std::fs::read_to_string(&path).expect("read the temp trace log");
    let disk_lines = on_disk.lines().count() as u64;
    assert!(
        disk_lines >= lines_per_frame[2],
        "{disk_lines} lines on disk, but the last completed frame ended at {}",
        lines_per_frame[2]
    );
    assert!(
        disk_lines <= tracer.lines,
        "{disk_lines} lines on disk, {} logged",
        tracer.lines
    );
    assert!(
        on_disk.ends_with('\n'),
        "a mid-frame stop left a partial line: {on_disk:?}"
    );

    // The unflushed tail is still only buffered: one more flush brings the file fully up to date.
    tracer.out.flush().expect("flush at exit");
    let all = std::fs::read_to_string(&path).expect("read the temp trace log");
    assert_eq!(all.lines().count() as u64, tracer.lines);
    std::fs::remove_file(&path).ok();
}

// --------------------------------------------------------------------------------------------
// §5 later cases: the frame window (`--trace-from`), symbol
// annotation (`--symbols`) and the §2 combination table. The six earlier cases above are untouched.
// --------------------------------------------------------------------------------------------

use std::path::PathBuf;

fn symbol_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/symbols")
        .join(name)
}

/// The `frame` field of a §1.1 line: the decimal number before the first `:` (§1.1 row 1).
fn frame_of(line: &str) -> u64 {
    line.split(':')
        .next()
        .expect("frame field")
        .parse()
        .expect("numeric frame field")
}

/// §5 case 3: five frames with a window at frame 2. Every logged line has frame >= 2 and the
/// first logged line starts exactly on it; the emulation is unchanged, so the windowed run ends
/// in the same state as its un-windowed twin (the determinism invariant is a *state* claim), and
/// an un-windowed run of the same length still logs the earlier frames the filter really
/// filtered away.
#[test]
fn frame_window_starts_logging_at_frame_n() {
    const FROM: u64 = 2;
    const FRAMES: u32 = 5;

    let mut windowed = Nes::new(&loop_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::new(Vec::new()).from_frame(FROM);
    for _ in 0..FRAMES {
        assert_eq!(
            tracer.log_frame(&mut windowed).expect("write to a Vec"),
            FrameEnd::Frame
        );
    }
    assert!(tracer.lines > 0, "the windowed run logged nothing");
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    let first_frame = frame_of(lines[0]);
    assert_eq!(
        first_frame, FROM,
        "the first logged line starts exactly on frame {FROM}"
    );
    for line in &lines {
        assert!(
            frame_of(line) >= FROM,
            "a warmup line leaked into the log: {line}"
        );
    }

    // Warmup ran in full: the windowed machine sits exactly where its un-windowed twin does.
    let mut plain = Nes::new(&loop_rom()).expect("synthetic NROM");
    for _ in 0..FRAMES {
        plain.run_frame();
    }
    assert_eq!(
        windowed.save_state(),
        plain.save_state(),
        "the window perturbed emulation"
    );

    // Sanity that the filter really filtered: the un-windowed twin's same-length log includes
    // the frames < FROM the window kept out.
    let mut unwindowed = Nes::new(&loop_rom()).expect("synthetic NROM");
    let mut plain_tracer = Tracer::new(Vec::new());
    for _ in 0..FRAMES {
        assert_eq!(
            plain_tracer
                .log_frame(&mut unwindowed)
                .expect("write to a Vec"),
            FrameEnd::Frame
        );
    }
    let plain_text = String::from_utf8(plain_tracer.out).expect("ASCII lines");
    assert!(
        plain_text.lines().any(|line| frame_of(line) < FROM),
        "an un-windowed run of the same length logs no frame < {FROM}: the window never filtered anything"
    );
}

/// §5 case 4: a fixture naming the synthetic program's entry gives exactly its lines the
/// `; <name>` comment (appended after `CYC:`, spec §2), its neighbours none — and the
/// same-address collision case keeps `with_symbols`' first-record-wins behaviour through
/// `Symbols::load`'s sorted dedupe.
#[test]
fn symbols_annotate_trace_lines() {
    // Exactly the entry line is annotated; its neighbours stay clean.
    let entry = symbol_fixture("entry.nl");
    let labels = tendy_nes::symbols::Symbols::load(&[entry]).expect("entry.nl");
    let mut nes = Nes::new(&format_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::with_symbols(Vec::new(), labels.labels);
    for _ in 0..6 {
        tracer.log_file_line(&mut nes).expect("write to a Vec");
    }
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].ends_with("; reset"),
        "exact-PC line lacks the label: {}",
        lines[0]
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains("; ")).count(),
        1,
        "a neighbour was annotated"
    );
    // The comment rides after `CYC:` with no reflow: the pinned line 1 literal plus `; reset`.
    assert!(
        lines[0].ends_with("A:00 X:00 Y:00 P:24 SP:FD CYC:7 ; reset"),
        "the comment is not the plain trailing comment: {}",
        lines[0]
    );

    // The collision case: two labels at one address resolve to the first record, so the
    // comment names `first`, not `second`.
    let collision = symbol_fixture("collision.nl");
    let labels = tendy_nes::symbols::Symbols::load(&[collision]).expect("collision.nl");
    assert_eq!(
        labels.name_at(0xC000),
        Some("first"),
        "first record must win the dedupe"
    );
    let mut nes = Nes::new(&format_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::with_symbols(Vec::new(), labels.labels);
    for _ in 0..6 {
        tracer.log_file_line(&mut nes).expect("write to a Vec");
    }
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].ends_with("; first"),
        "collision lost the first record: {}",
        lines[0]
    );
    assert!(
        !text.contains("second"),
        "the losing label leaked into the log"
    );
}

/// §2 combination: `--symbols` with two files (the `[;more]` of the flag) — both label tables
/// merge into one lookup, so the entry line and the JSR-target line each gain their own comment
/// while every other line stays clean.
#[test]
fn symbols_from_two_files_annotate_both() {
    let files = [symbol_fixture("entry.nl"), symbol_fixture("extra.nl")];
    let labels = tendy_nes::symbols::Symbols::load(&files).expect("entry.nl;extra.nl");
    assert_eq!(labels.name_at(0xC000), Some("reset"));
    assert_eq!(labels.name_at(0xC020), Some("sub"));
    let mut nes = Nes::new(&format_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::with_symbols(Vec::new(), labels.labels);
    for _ in 0..6 {
        tracer.log_file_line(&mut nes).expect("write to a Vec");
    }
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].ends_with("; reset"),
        "file 1's label missing: {}",
        lines[0]
    );
    assert!(
        lines[5].ends_with("; sub"),
        "file 2's label missing: {}",
        lines[5]
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains("; ")).count(),
        2,
        "a neighbour was annotated"
    );
}

/// §2 combination: window + limit interplay — "`--trace-log` + `--trace-from` +
/// `--trace-limit`: warmup until frame N, then count lines toward the limit". Exactly the N
/// budgeted lines land in the log, none from a warmup frame, and the warmup still emulated in
/// full (the machine ran far past what N instructions cover).
#[test]
fn window_then_limit_counts_only_windowed_lines() {
    const FROM: u64 = 2;
    const N: u64 = 25;
    let mut nes = Nes::new(&loop_rom()).expect("synthetic NROM");
    let mut tracer = Tracer::new(Vec::new()).from_frame(FROM).stop_after(N);
    let mut end = FrameEnd::Frame;
    while end == FrameEnd::Frame {
        end = tracer.log_frame(&mut nes).expect("write to a Vec");
    }
    assert_eq!(end, FrameEnd::Limit);
    assert_eq!(
        tracer.lines, N,
        "warmup lines consumed the budget or the limit was undercounted"
    );
    let text = String::from_utf8(tracer.out).expect("ASCII lines");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len() as u64, N);
    for line in &lines {
        assert!(
            frame_of(line) >= FROM,
            "a warmup line leaked into the log: {line}"
        );
    }

    // Warmup happened: the machine executed the full frames before frame FROM, i.e. far more
    // CPU cycles than N instructions from power-on would spend. A 60 Hz frame is one NOP+JMP
    // lap every ~5 cycles, so frame >= 2 means more than (N/FRAMES) cycles above the budget:
    // compare against a hand-run of N instructions only as a floor the warmup must clear.
    let mut hand = Nes::new(&loop_rom()).expect("synthetic NROM");
    for _ in 0..N {
        hand.step();
    }
    assert!(
        nes.cpu.cycles > hand.cpu.cycles,
        "warmup frames did not run in full (window advanced the {} -instruction run)",
        N
    );
}
