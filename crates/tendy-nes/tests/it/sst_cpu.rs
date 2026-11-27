//! SingleStepTests nes6502: 100 cases per opcode with the CPU state before and after one instruction
//! and the exact bus access on every cycle (`roms/nes6502_sst.txt`, made by `tools/fetch_sst.py`).
//! Final registers and RAM must match for every opcode except the jams and the unstable ones.
//! Access lists are a ratchet: opcodes listed in `sst_access_ok.txt` must keep matching exactly.
//! Set SST_WRITE_ACCESS_OK=1 to rewrite that list from the current results.

use tendy_nes::cpu::{Bus, Cpu};

/// Cases in the data file: 256 opcodes x 100 cases each.
const CASES: usize = 25_600;

/// Opcodes whose final state is not checked: the 12 JAMs (they never finish an instruction) and the
/// unstable ones (ANE, LXA, SHA, SHS, SHY, SHX) whose results depend on analogue effects.
const SKIP_STATE: [u8; 19] = [
    0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xB2, 0xD2, 0xF2, 0x8B, 0xAB, 0x93, 0x9F,
    0x9B, 0x9C, 0x9E,
];

/// Flat 64 KB RAM that records every access as (address, value, is_write).
struct LoggingBus {
    mem: Vec<u8>,
    log: Vec<(u16, u8, bool)>,
}

impl Bus for LoggingBus {
    fn read(&mut self, addr: u16) -> u8 {
        let val = self.mem[usize::from(addr)];
        self.log.push((addr, val, false));
        val
    }

    fn write(&mut self, addr: u16, val: u8) {
        self.mem[usize::from(addr)] = val;
        self.log.push((addr, val, true));
    }
}

/// "pc s a x y p" in decimal.
fn regs(field: &str) -> Vec<u16> {
    field
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect()
}

/// "addr=value" pairs.
fn ram(field: &str) -> Vec<(u16, u8)> {
    field
        .split_whitespace()
        .map(|pair| {
            let (addr, val) = pair.split_once('=').unwrap();
            (addr.parse().unwrap(), val.parse().unwrap())
        })
        .collect()
}

/// "addr=value=r" or "addr=value=w", one per cycle.
fn cycles(field: &str) -> Vec<(u16, u8, bool)> {
    field
        .split_whitespace()
        .map(|item| {
            let mut parts = item.split('=');
            let addr = parts.next().unwrap().parse().unwrap();
            let val = parts.next().unwrap().parse().unwrap();
            (addr, val, parts.next() == Some("w"))
        })
        .collect()
}

/// Run one case. Returns (final state matches, access list and cycle count match).
fn run_case(line: &str) -> (bool, bool) {
    let fields: Vec<&str> = line.split(" | ").collect();
    let (start, end) = (regs(fields[0]), regs(fields[2]));
    let mut bus = LoggingBus {
        mem: vec![0; 0x10000],
        log: Vec::new(),
    };
    for (addr, val) in ram(fields[1]) {
        bus.mem[usize::from(addr)] = val;
    }
    let mut cpu = Cpu::new();
    cpu.pc = start[0];
    cpu.sp = start[1] as u8;
    cpu.a = start[2] as u8;
    cpu.x = start[3] as u8;
    cpu.y = start[4] as u8;
    cpu.p = start[5] as u8;
    let taken = cpu.step(&mut bus);
    let state_ok = [
        cpu.pc,
        u16::from(cpu.sp),
        u16::from(cpu.a),
        u16::from(cpu.x),
        u16::from(cpu.y),
        u16::from(cpu.p),
    ] == end[..]
        && ram(fields[3])
            .iter()
            .all(|&(addr, val)| bus.mem[usize::from(addr)] == val);
    let expected = cycles(fields[4]);
    let access_ok = bus.log == expected && taken as usize == expected.len();
    (state_ok, access_ok)
}

#[test]
fn single_step_tests() {
    let path = crate::common::rom_path("nes6502_sst.txt");
    // Not on CI: `tools/fetch_sst.py` pulls ~1 GB of JSON to build this file, which is too much
    // for a workflow (256 downloads per runner), so no CI runner has the asset and there is no
    // count to assert against. A missing file is a skip, like the commercial ROMs in
    // `game_snapshots.rs`; the SKIPPED line below is the only signal, and it reaches the CI log
    // only because the Integration step passes `--nocapture`.
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!(
            "sst: SKIPPED {CASES}/{CASES} cases ({} is absent; run `python tools/fetch_sst.py`)",
            path.display()
        );
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), CASES);

    let mut state_failures = Vec::new();
    let mut access_ok = Vec::new();
    for (opcode, cases) in lines.chunks(100).enumerate() {
        let opcode = opcode as u8;
        let results: Vec<(bool, bool)> = cases.iter().map(|line| run_case(line)).collect();
        let state_fails = results.iter().filter(|r| !r.0).count();
        if state_fails > 0 && !SKIP_STATE.contains(&opcode) {
            state_failures.push(format!("{opcode:02X} ({state_fails}/100)"));
        }
        if results.iter().all(|r| r.1) {
            access_ok.push(opcode);
        }
    }
    println!("access lists match for {} / 256 opcodes", access_ok.len());

    let list_path = format!("{}/tests/sst_access_ok.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("SST_WRITE_ACCESS_OK").is_ok() {
        let mut out =
            String::from("# Opcodes whose SingleStepTests bus accesses match on every cycle.\n");
        for opcode in &access_ok {
            out.push_str(&format!("{opcode:02X}\n"));
        }
        std::fs::write(&list_path, out).unwrap();
    }
    let listed: Vec<u8> = std::fs::read_to_string(&list_path)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| u8::from_str_radix(l.trim(), 16).unwrap())
        .collect();
    let regressions: Vec<String> = listed
        .iter()
        .filter(|op| !access_ok.contains(op))
        .map(|op| format!("{op:02X}"))
        .collect();
    for opcode in access_ok.iter().filter(|op| !listed.contains(op)) {
        println!("access list now matches (add to sst_access_ok.txt): {opcode:02X}");
    }
    assert!(
        state_failures.is_empty(),
        "final state differs: {state_failures:?}"
    );
    assert!(
        regressions.is_empty(),
        "access lists that matched before now differ: {regressions:?}"
    );
}
