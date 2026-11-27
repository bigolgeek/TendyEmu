use tendy_nes::bus::SysBus;
use tendy_nes::cpu::*;
use tendy_nes::mapper::load;

use crate::common::expected;
use crate::common::rom_file;

/// Run a blargg test ROM until it reports a result through $6000 (signature DE B0 61 at $6001-$6003,
/// status below $80), then return the status byte and the text message at $6004.
fn run_blargg(name: &str) -> (u8, String) {
    let rom = rom_file(name);
    let mut bus = SysBus::new(load(&rom).unwrap());
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    for step in 1..=20_000_000u32 {
        cpu.step(&mut bus);
        if step % 1000 == 0
            && bus.read(0x6001) == 0xDE
            && bus.read(0x6002) == 0xB0
            && bus.read(0x6003) == 0x61
            && bus.read(0x6000) < 0x80
        {
            break;
        }
    }
    let mut text = String::new();
    for addr in 0x6004..0x6400u16 {
        let c = bus.read(addr);
        if c == 0 {
            break;
        }
        text.push(char::from(c));
    }
    (bus.read(0x6000), text)
}

fn assert_passes(name: &str) {
    let (status, text) = run_blargg(name);
    assert_eq!(status, 0, "{name} failed: {text}");
}

#[test]
fn nestest_full_log_matches() {
    let rom = rom_file("nestest.nes");
    let log = String::from_utf8(rom_file("nestest.log")).unwrap();
    let mut bus = SysBus::new(load(&rom).unwrap());
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    cpu.pc = 0xC000;
    for (i, line) in log.lines().enumerate() {
        assert_eq!(
            cpu.trace_line(),
            expected(line),
            "nestest.log line {}",
            i + 1
        );
        cpu.step(&mut bus);
    }
    assert_eq!(log.lines().count(), 8991);
    assert_eq!(bus.ram[2], 0, "nestest official error code");
    assert_eq!(bus.ram[3], 0, "nestest unofficial error code");
}

#[test]
fn instr_01_basics() {
    assert_passes("01-basics.nes");
}

#[test]
fn instr_02_implied() {
    assert_passes("02-implied.nes");
}

#[test]
fn instr_03_immediate() {
    assert_passes("03-immediate.nes");
}

#[test]
fn instr_04_zero_page() {
    assert_passes("04-zero_page.nes");
}

#[test]
fn instr_05_zp_xy() {
    assert_passes("05-zp_xy.nes");
}

#[test]
fn instr_06_absolute() {
    assert_passes("06-absolute.nes");
}

#[test]
fn instr_07_abs_xy() {
    assert_passes("07-abs_xy.nes");
}

#[test]
fn instr_08_ind_x() {
    assert_passes("08-ind_x.nes");
}

#[test]
fn instr_09_ind_y() {
    assert_passes("09-ind_y.nes");
}

#[test]
fn instr_10_branches() {
    assert_passes("10-branches.nes");
}

#[test]
fn instr_11_stack() {
    assert_passes("11-stack.nes");
}

#[test]
fn instr_12_jmp_jsr() {
    assert_passes("12-jmp_jsr.nes");
}

#[test]
fn instr_13_rts() {
    assert_passes("13-rts.nes");
}

#[test]
fn instr_14_rti() {
    assert_passes("14-rti.nes");
}

#[test]
fn instr_15_brk() {
    assert_passes("15-brk.nes");
}

#[test]
fn instr_16_special() {
    assert_passes("16-special.nes");
}
