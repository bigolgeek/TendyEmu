//! Helpers shared by the integration tests in `tests/it/`.
//!
//! Those files are the modules of one test binary (`tests/it/main.rs`), so anything shared has to
//! live here, declared once by the crate root. `tests/it/common/` is a directory, not a `tests/*.rs`
//! file, so Cargo does not try to run it as a test binary of its own.
//!
//! `#![allow(dead_code)]`: the binary uses only some of these, and the rest would otherwise be a
//! warning - which is an error under `clippy --all-targets -D warnings`.

#![allow(dead_code)]

use tendy_nes::nes::Nes;

/// The folder the NES test ROMs live in (DESIGN §10: the ROMs are never committed).
///
/// `TENDY_NES_TEST_ROMS` first, then the shared `TENDY_TEST_ROMS`, then `<crate>/roms` for a
/// checkout that has fetched them there. The 195 public test ROMs are the source repository's
/// `roms/` folder; point a variable at a copy of it, or at the folder itself.
pub fn rom_root() -> std::path::PathBuf {
    std::env::var_os("TENDY_NES_TEST_ROMS")
        .or_else(|| std::env::var_os("TENDY_TEST_ROMS"))
        .map_or_else(
            || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("roms"),
            std::path::PathBuf::from,
        )
}

/// `name` under [`rom_root`], with a leading `roms/` accepted so the source's relative names work
/// unchanged.
pub fn rom_path(name: &str) -> std::path::PathBuf {
    rom_root().join(name.strip_prefix("roms/").unwrap_or(name))
}

/// The test ROM `name`, read from [`rom_root`].
///
/// Panics when the file is missing: right for the suites whose every case needs its ROM.
pub fn rom_file(name: &str) -> Vec<u8> {
    let path = rom_path(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (set TENDY_NES_TEST_ROMS to the NES test-ROM folder; see scripts/test-roms.txt)",
            path.display()
        )
    })
}

/// `rom_file` for a suite that reports an absent ROM as a skipped case rather than a failure
/// (the commercial-game suites, whose assets are never on CI).
pub fn rom_file_opt(name: &str) -> Option<Vec<u8>> {
    std::fs::read(rom_path(name)).ok()
}

/// AccuracyCoin's page-completion rule: every result byte of the page has been written.
///
/// `0` means "not run" in a result byte (`0xFF` skipped, bit 0 set pass, otherwise the error code
/// is `byte >> 2`, see `tests/accuracycoin.rs::status`), so a page whose bytes are all non-zero
/// has finished every test it will run.
pub fn page_complete(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|&b| b != 0)
}

/// Run `frame` until the page it reports is complete, then `recheck` more frames requiring the
/// bytes unchanged. Returns `(frames run, confirmed)`.
///
/// The recheck is what makes the rule safe rather than lucky: a byte is written when its test
/// finishes, so a page can read complete for one frame while another test is still mid-write.
/// `confirmed == false` means `limit` frames passed first, and the caller falls back to its own
/// settle. The bound is on frames, not on passes of the outer loop: a page whose bytes are
/// complete but keep changing would otherwise pay a whole recheck per pass (`limit * recheck`
/// frames - the alignment matrix's walk hit that and took hours instead of minutes).
pub fn wait_page_complete(
    limit: usize,
    recheck: usize,
    mut frame: impl FnMut() -> Vec<u8>,
) -> (usize, bool) {
    let mut frames = 0;
    while frames < limit {
        let bytes = frame();
        frames += 1;
        if !page_complete(&bytes) {
            continue;
        }
        let mut stable = 0;
        while stable < recheck {
            let sample = frame();
            frames += 1;
            if sample != bytes {
                break;
            }
            stable += 1;
        }
        if stable == recheck {
            return (frames, true);
        }
    }
    (frames, false)
}

/// Run the blargg `$6000`-protocol ROM `name` for at most `max_frames` frames; return
/// `(status, text)`.
///
/// The ROM signals completion by writing the `DE B0 61` signature to `$6001-$6003` with a
/// status below `$80` in `$6000`; `$6004` onwards is a NUL-terminated message. `max_frames`
/// differs per suite because some ROMs (oam_stress, the instr_timing aggregate) take far
/// longer than others to report.
pub fn run_blargg(name: &str, max_frames: usize) -> (u8, String) {
    let mut nes = Nes::new(&rom_file(name)).unwrap();
    for _ in 0..max_frames {
        nes.run_frame();
        if nes.bus.cart_mut().cpu_read(0x6001) == 0xDE
            && nes.bus.cart_mut().cpu_read(0x6002) == 0xB0
            && nes.bus.cart_mut().cpu_read(0x6003) == 0x61
            && nes.bus.cart_mut().cpu_read(0x6000) < 0x80
        {
            break;
        }
    }
    let mut text = String::new();
    let mut addr = 0x6004;
    while nes.bus.cart_mut().cpu_read(addr) != 0 && addr < 0x7000 {
        text.push(char::from(nes.bus.cart_mut().cpu_read(addr)));
        addr += 1;
    }
    (nes.bus.cart_mut().cpu_read(0x6000), text)
}

/// A minimal NROM image: 16 KB PRG and 8 KB CHR, a `JMP $8000` at the reset vector, so the CPU
/// spins on the spot forever. `flags7` and `timing` land in the iNES header byte 7 (mirroring
/// and the NES 2.0 flag) and byte 12 (TV system), which is how a test picks NTSC/PAL/Dendy.
pub fn loop_rom(flags7: u8, timing: u8) -> Vec<u8> {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, flags7, 0, 0, 0, 0, timing, 0, 0, 0,
    ];
    v.resize(16 + 16384 + 8192, 0);
    v[16] = 0x4C;
    v[17] = 0x00;
    v[18] = 0x80;
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

/// A minimal NROM image whose program at `base` starts the APU pulse, points the PPU at `$3F00`,
/// latches the controller into `$20`, and then loops forever writing the pad bits to `$0300`,
/// `$2007` and `$2005` and incrementing the frame counter at `$10`. The loop label is 30 bytes
/// in, which the `debug_assert` pins.
pub fn program(base: u16) -> Vec<u8> {
    let lo = |a: u16| (a & 0xFF) as u8;
    let hi = |a: u16| (a >> 8) as u8;
    let loop_at = base + 30;
    let mut p = vec![
        0xA9, 0x01, 0x8D, 0x15, 0x40, // LDA #$01 / STA $4015
        0xA9, 0xBF, 0x8D, 0x00, 0x40, // LDA #$BF / STA $4000
        0xA9, 0x40, 0x8D, 0x02, 0x40, // LDA #$40 / STA $4002
        0xA9, 0x01, 0x8D, 0x03, 0x40, // LDA #$01 / STA $4003
        0xA9, 0x3F, 0x8D, 0x06, 0x20, // LDA #$3F / STA $2006
        0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00 / STA $2006
    ];
    debug_assert_eq!(p.len(), 30, "loop label offset");
    p.extend_from_slice(&[
        0xA9,
        0x01,
        0x8D,
        0x16,
        0x40, // LDA #$01 / STA $4016   (strobe high)
        0xA9,
        0x00,
        0x8D,
        0x16,
        0x40, // LDA #$00 / STA $4016   (strobe low: shift mode)
        0xA2,
        0x08, // LDX #$08
        0xAD,
        0x16,
        0x40, // pad: LDA $4016
        0x4A, // LSR A
        0x66,
        0x20, // ROR $20
        0xCA, // DEX
        0xD0,
        0xF7, // BNE pad
        0xA5,
        0x20, // LDA $20
        0x8D,
        0x00,
        0x03, // STA $0300
        0xE6,
        0x10, // INC $10
        0xA5,
        0x10, // LDA $10
        0x8D,
        0x07,
        0x20, // STA $2007
        0x8D,
        0x05,
        0x20, // STA $2005
        0x4C,
        lo(loop_at),
        hi(loop_at), // JMP loop
    ]);
    p
}

/// NROM image with `code` at `$8000`, an `RTI` at `$8100`, and the reset vector at `$8000` with
/// NMI and IRQ pointed at the `RTI` - a bare guest that runs `code` and ignores interrupts.
pub fn program_code(code: &[u8]) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    v[16..16 + code.len()].copy_from_slice(code);
    v[16 + 0x100] = 0x40; // RTI at $8100
    v[16 + 0x3FFA] = 0x00; // NMI -> $8100
    v[16 + 0x3FFB] = 0x81;
    v[16 + 0x3FFC] = 0x00; // RESET -> $8000
    v[16 + 0x3FFD] = 0x80;
    v[16 + 0x3FFE] = 0x00; // IRQ -> $8100
    v[16 + 0x3FFF] = 0x81;
    v
}

/// The `nestest.log` line `line` reduced to the `trace_line()` format: the PC, the 25-character
/// register block, and `CYC`.
pub fn expected(line: &str) -> String {
    let at = line.find("A:").unwrap();
    let cyc = line.rsplit("CYC:").next().unwrap().trim();
    format!("{} {} CYC:{}", &line[0..4], &line[at..at + 25], cyc)
}

/// `src` with `//` line comments and `/* */` block comments removed, so a source-scanning test
/// cannot mistake a commented-out item for a real one. String literals are skipped, so a
/// `//` inside one stays; block comments nest, as Rust's do.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut block = 0usize;
    while let Some(c) = chars.next() {
        if block > 0 {
            if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block += 1;
            } else if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block -= 1;
            }
            continue;
        }
        match c {
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                block = 1;
            }
            '"' => {
                out.push(c);
                while let Some(n) = chars.next() {
                    out.push(n);
                    if n == '\\' {
                        if let Some(escaped) = chars.next() {
                            out.push(escaped);
                        }
                    } else if n == '"' {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}
