//! `Mapper::hooks` is a promise about behaviour. A board that overrides `irq_pending` or
//! `cpu_clock` and does not list itself is never called, so the game silently loses IRQs or
//! CPU-cycle clocking — and before this guard, no ROM-free test noticed: removing MMC1's flags left
//! every test green, and only a commercial-ROM snapshot failed.
//!
//! Every test here is ROM-free. The first reads the board sources, so it covers every board the
//! crate constructs, including ones added later. The second drives a real MMC1 through the bus, so
//! the MMC1 half is proved by behaviour rather than by the constant it returns.
//!
//! The scanner itself is fixture-tested: a commented-out impl, a body running past its
//! closing brace, and a generic `impl<T> Mapper for` all have to read the way the compiler reads
//! them, or the count below and every per-board verdict are fiction.

use tendy_nes::bus::SysBus;
use tendy_nes::cart::Mirroring;
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::load;

/// One `impl Mapper for X` block: the source file, the board's name, and the block's text.
struct Board {
    file: String,
    name: String,
    body: String,
}

/// Every board the crate constructs: one entry per `impl ... Mapper for X` block in `src/mapper`,
/// so a file that holds two boards (the UN1ROM pair, the Sunsoft latches) is judged one board at a
/// time.
fn boards() -> Vec<Board> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mapper");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("src/mapper is readable")
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    paths.sort();
    let mut boards = Vec::new();
    for path in paths {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("board source is readable");
        boards.extend(impls_in(&file, &text));
    }
    boards
}

/// The `impl ... Mapper for X` blocks in one source file. Split out from [`boards`] so the
/// fixture tests below can feed it source text instead of a file, and parsed rather than split on a
/// literal: a split cannot tell a commented-out impl from a real one, cannot see an
/// `impl<T> Mapper for X<T>`, and runs a block's text to the end of the file.
fn impls_in(file: &str, src: &str) -> Vec<Board> {
    // Comments first, so a disabled board is not a board. Then whitespace collapsed, so a formatted
    // multi-line `CartHooks { .. }` or `where` clause reads as one line.
    let text = crate::common::strip_comments(src)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut boards = Vec::new();
    let mut from = 0usize;
    while let Some(at) = impl_at(&text, from) {
        let Some(open) = text[at..].find('{').map(|offset| at + offset) else {
            break;
        };
        from = open + 1;
        let header = &text[at..open];
        if !header.contains("Mapper for ") {
            continue; // `impl Helper`, `impl Mapper` in a return position, `trait Mapper`
        }
        let close = closing_brace(&text, open);
        boards.push(Board {
            file: file.to_string(),
            name: board_name(header),
            body: text[open + 1..close].to_string(),
        });
        from = close + 1;
    }
    boards
}

/// The next `impl` keyword at or after `from`, as a whole word: `reimplement` is not an impl.
fn impl_at(text: &str, from: usize) -> Option<usize> {
    fn ident(c: Option<char>) -> bool {
        c.is_some_and(|c| c.is_alphanumeric() || c == '_')
    }
    let mut at = from;
    while let Some(pos) = text[at..].find("impl").map(|offset| at + offset) {
        if !ident(text[..pos].chars().next_back()) && !ident(text[pos + 4..].chars().next()) {
            return Some(pos);
        }
        at = pos + 4;
    }
    None
}

/// The board's name: the first token after `for` in the impl header, so a generic impl is named for
/// its base type (`impl<T> Mapper for Banked<T>` is `Banked`).
fn board_name(header: &str) -> String {
    let after = header
        .split("Mapper for ")
        .nth(1)
        .unwrap_or("")
        .trim_start();
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        "?".to_string()
    } else {
        name
    }
}

/// The index of the `}` closing the `{` at `open`, counting nested blocks.
fn closing_brace(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + i;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// A commented-out impl is not a board. The scanner reads source text, so `//` and `/* */`
/// have to go first — otherwise a board someone disabled still counts towards the floor and its
/// body is judged.
#[test]
fn a_commented_out_impl_is_not_a_board() {
    let src = "impl Mapper for Real { fn cpu_read(&mut self, addr: u16) -> u8 { 0 } }
// impl Mapper for LineCommented { fn irq_pending(&self) -> bool { true } }
/* impl Mapper for BlockCommented { fn cpu_clock(&mut self) {} } */
";
    let names: Vec<_> = impls_in("fixture.rs", src)
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, ["Real"]);
}

/// A body ends at its closing brace. The old split ran a block's text to the next
/// `impl Mapper for`, or to the end of the file, so the next item's functions were credited to the
/// board above it.
#[test]
fn a_body_stops_at_its_closing_brace() {
    let src = "impl Mapper for A { fn hooks(&self) -> CartHooks { CartHooks { irq: true, cpu_clock: true } } }
struct Helper;

impl Helper {
    fn irq_pending(&self) -> bool { true }
}
";
    let boards = impls_in("fixture.rs", src);
    assert_eq!(boards.len(), 1, "one board");
    assert_eq!(boards[0].name, "A");
    assert!(
        !boards[0].body.contains("irq_pending"),
        "A's body ran past its closing brace: {}",
        boards[0].body
    );
    assert!(
        !boards[0].body.contains("Helper"),
        "the next item leaked into A: {}",
        boards[0].body
    );
}

/// `impl<T> Mapper for X<T>` is a board like any other, and its name is `X`.
#[test]
fn a_generic_impl_is_found() {
    let src =
        "impl<T: Clone> Mapper for Banked<T> { fn cpu_read(&mut self, addr: u16) -> u8 { 0 } }";
    let boards = impls_in("fixture.rs", src);
    assert_eq!(boards.len(), 1, "the generic impl was missed");
    assert_eq!(boards[0].name, "Banked");
}

/// A board whose block overrides `irq_pending`/`cpu_clock` must report that in `hooks()`.
///
/// The trait's default bodies live in `src/cart.rs`, so a `fn irq_pending` or `fn cpu_clock` inside
/// an `impl Mapper for` block is always an override — there is nothing else it can be.
#[test]
fn every_board_that_overrides_a_hook_declares_it_in_hooks() {
    let boards = boards();
    println!("cart_hook_flags: {} boards scanned", boards.len());
    // 26 on the fixed scanner. The floor is what the scan actually finds, so a board that
    // stops living in `src/mapper` fails here instead of quietly shrinking the sweep.
    assert!(
        boards.len() >= 26,
        "the scan found only {} boards; it is not looking where the boards live",
        boards.len()
    );
    let declaring = boards
        .iter()
        .filter(|b| b.body.contains("fn hooks"))
        .count();
    assert!(
        declaring >= 9,
        "only {declaring} boards declare hooks(); the other boards would all be judged trivially"
    );

    let mut failures = Vec::new();
    for b in &boards {
        if b.body.contains("fn irq_pending") && !b.body.contains("irq: true") {
            failures.push(format!(
                "{} ({}) overrides irq_pending but its hooks() does not set irq: true",
                b.name, b.file
            ));
        }
        if b.body.contains("fn cpu_clock") && !b.body.contains("cpu_clock: true") {
            failures.push(format!(
                "{} ({}) overrides cpu_clock but its hooks() does not set cpu_clock: true",
                b.name, b.file
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a board that forgets a hook flag is never called:\n  {}",
        failures.join("\n  ")
    );
}

/// An MMC1 image: mapper 1, 8 × 16 KiB PRG (the smallest MMC1 PRG window), 8 KiB CHR-ROM.
fn mmc1_rom() -> Vec<u8> {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 1, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0xEA);
    v.resize(16 + 8 * 16384 + 8192, 0);
    v
}

/// The MMC1 half: its serial port counts CPU cycles, so the bus must clock it every cycle or
/// the "ignore a write one cycle after another" rule never fires and every write is accepted.
///
/// Five writes to `$8000` (the control register's window) one cycle apart: the first shifts a bit
/// in and the other four must be ignored, so the register is untouched at five bits' worth of
/// writes. Four more bits, each two cycles after the last, complete it with five ones → control
/// `$1F` → horizontal mirroring. Without MMC1's `cpu_clock` flag the cycle counter never advances
/// and the first assertion sees horizontal mirroring straight away.
#[test]
fn mmc1_serial_port_is_clocked_by_the_bus() {
    let mut bus = SysBus::new(load(&mmc1_rom()).unwrap());
    assert!(
        bus.cart().hooks().cpu_clock,
        "MMC1 counts CPU cycles for its serial port"
    );

    for _ in 0..5 {
        bus.write(0x8000, 0x01);
    }
    assert_eq!(
        bus.cart().mirroring(),
        Mirroring::SingleScreenLower,
        "four of five back-to-back writes must be ignored, leaving control at its $0C power-on value"
    );

    for _ in 0..4 {
        bus.read(0x0000); // one CPU cycle: the next write is two cycles after the last
        bus.write(0x8000, 0x01);
    }
    assert_eq!(
        bus.cart().mirroring(),
        Mirroring::Horizontal,
        "five bits of 1 → control $1F"
    );
}
