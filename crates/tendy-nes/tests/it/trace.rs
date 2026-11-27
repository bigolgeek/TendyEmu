use tendy_nes::nes::Nes;
use tendy_nes::trace::{Tracer, disasm, trace_line_disasm};

use crate::common::expected;
use crate::common::rom_file;

/// (scanline, dot) from the PPU: column of a trace or nestest.log line.
fn ppu_of(line: &str) -> (u32, u32) {
    let after = line.rsplit("PPU:").next().unwrap();
    let pair = &after[0..after.find("CYC:").unwrap()];
    let comma = pair.find(",").unwrap();
    (
        pair[0..comma].trim().parse().unwrap(),
        pair[comma + 1..].trim().parse().unwrap(),
    )
}

/// Our trace line without the PPU column: PC, the 25-char block, CYC.
fn reduced(line: &str) -> String {
    let ppu_at = line.find(" PPU:").unwrap();
    let cyc_at = line.find("CYC:").unwrap();
    format!("{} {}", &line[0..ppu_at], &line[cyc_at..])
}

/// Runs the 8991 nest instructions with the Tracer over a Vec<u8>; returns the whole trace text.
fn traced(nes: &mut Nes) -> String {
    let buf: Vec<u8> = Vec::new();
    let mut tr = Tracer::new(buf);
    while tr.lines < 8991 {
        tr.step(nes).unwrap();
    }
    String::from_utf8(tr.out).unwrap()
}

/// Same run but with the disassembly lines (trace_line_disasm) instead of bare trace_line.
fn traced_disasm(nes: &mut Nes) -> String {
    let mut text = String::new();
    for _ in 0..8991 {
        text.push_str(&trace_line_disasm(nes));
        text.push('\n');
        nes.step();
    }
    text
}

#[test]
fn trace_first_line_format() {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    let text = traced(&mut nes);
    let mut n = 0;
    for line in text.lines() {
        if n == 0 {
            assert_eq!(line, "C000 A:00 X:00 Y:00 P:24 SP:FD PPU:  0,  6 CYC:7");
        }
        n += 1;
    }
    assert_eq!(n, 8991);
}

#[test]
fn trace_ppu_position_matches_nestest_log() {
    // Our PPU column lags nestest by exactly 15 dots, measured constant on all 8991 lines.
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    let text = traced(&mut nes);
    let log = String::from_utf8(rom_file("nestest.log")).unwrap();
    for (i, (o, l)) in text.lines().zip(log.lines()).enumerate() {
        let (o_sl, o_dot) = ppu_of(o);
        let (l_sl, l_dot) = ppu_of(l);
        assert_eq!(
            o_sl * 341 + o_dot + 15,
            l_sl * 341 + l_dot,
            "nestest.log line {}",
            i + 1
        );
    }
}

#[test]
fn trace_registers_and_cyc_match_nestest_log() {
    // A:..SP block and CYC: same comparison as `crate::common::expected()`.
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    let text = traced(&mut nes);
    let log = String::from_utf8(rom_file("nestest.log")).unwrap();
    for (i, (o, l)) in text.lines().zip(log.lines()).enumerate() {
        assert_eq!(reduced(o), expected(l), "nestest.log line {}", i + 1);
    }
}

/// The bytes column (chars 6..14, trimmed) shared by nestest.log lines and ours.
fn bytes_col(line: &str) -> &str {
    &line[6..14]
}

#[test]
fn disasm_nestest_first_line() {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    assert_eq!(disasm(&mut nes, 0xC000), "4C F5 C5  JMP $C5F5");
}

/// Our 32-character disassembly field: the mnemonic + operand column of a trace_line_disasm line.
fn our_disasm(line: &str) -> &str {
    line[16..48].trim_end()
}

/// nestest.log's disassembly field reduced to the mnemonic + operand text we produce:
/// the ` = value` memory annotation and the ` @ effective-address` suffix nestest puts on
/// indexed memory touches are dropped (src/trace.rs produces neither, it must not touch I/O).
fn nestest_disasm(line: &str) -> &str {
    let mut text = line[16..48].trim_end();
    if let Some(at) = text.find(" @ ") {
        text = &text[..at];
    }
    if let Some(at) = text.find(" = ") {
        text = &text[..at];
    }
    text
}

#[test]
fn disasm_column_matches_nestest_log() {
    // Our disassembly column vs nestest.log's, all 8991 lines, nestest's own padding.
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    let text = traced_disasm(&mut nes);
    let log = String::from_utf8(rom_file("nestest.log")).unwrap();
    assert_eq!(
        text.lines().count(),
        log.lines().count(),
        "trace and nestest.log line counts"
    );
    for (i, (o, l)) in text.lines().zip(log.lines()).enumerate() {
        // The 16-character prefix: PC, two spaces, the 9-character byte column and the
        // star-or-space column nestest puts its illegal-opcode marker in.
        assert_eq!(&o[0..16], &l[0..16], "nestest.log line {} prefix", i + 1);
        assert_eq!(
            our_disasm(o),
            nestest_disasm(l),
            "nestest.log line {}",
            i + 1
        );
    }
}

#[test]
fn disasm_bytes_column_matches_nestest_log() {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    nes.cpu.pc = 0xC000;
    let log = String::from_utf8(rom_file("nestest.log")).unwrap();
    for (i, l) in log.lines().enumerate().take(100) {
        let line = trace_line_disasm(&mut nes);
        assert_eq!(
            bytes_col(&line).trim(),
            bytes_col(l).trim(),
            "nestest.log line {}",
            i + 1
        );
        nes.step();
    }
}
