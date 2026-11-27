//! Exhaustive opcode matrix: every one of the 256 opcode bytes, run from two fixed machine
//! states against a bus that records every access, folded into one FNV-1a 64.
//!
//! `Cpu::execute` used to ask four group functions in turn whether an instruction was theirs
//! (`exec_basic`, `exec_alu`, `exec_flow`, `exec_unofficial`) and fall back to `unwrap_or(0)`, and
//! it re-tested a twenty-name `matches!` every instruction to decide the `writes` flag. The dispatch
//! is now one exhaustive `match`, and the flag lives in the opcode table. Neither change is meant
//! to alter what any instruction does, and this is the proof: every opcode — including the ones no
//! other unit test names — must produce the same cycle count, the same registers and the same bus
//! accesses as the tree the digest was recorded from.

use tendy_nes::cpu::*;
use tendy_nes::state::fnv1a64;

/// A bus that records every access as `(write, addr, value)` and never drives an interrupt line.
struct Recorder {
    mem: Vec<u8>,
    log: Vec<(bool, u16, u8)>,
}

impl Recorder {
    /// 64 KB of a byte pattern that varies with the address, so operand bytes, pointers, the stack
    /// and the vectors all read different values.
    fn new() -> Recorder {
        let mem = (0..0x1_0000)
            .map(|i: usize| (i as u8).wrapping_mul(7).wrapping_add(0x11))
            .collect();
        Recorder {
            mem,
            log: Vec::new(),
        }
    }
}

impl Bus for Recorder {
    fn read(&mut self, addr: u16) -> u8 {
        let v = self.mem[addr as usize];
        self.log.push((false, addr, v));
        v
    }
    fn write(&mut self, addr: u16, val: u8) {
        self.log.push((true, addr, val));
        self.mem[addr as usize] = val;
    }
}

/// One machine state to run the whole table from.
struct State {
    a: u8,
    x: u8,
    y: u8,
    p: u8,
}

/// The first state leaves every index at zero, so no indexed access crosses a page; the second puts
/// A, X, Y and the flags where indexed and indirect modes wrap and cross.
const STATES: [State; 2] = [
    State {
        a: 0x12,
        x: 0x00,
        y: 0x00,
        p: 0x24,
    },
    State {
        a: 0xF3,
        x: 0xF0,
        y: 0x0F,
        p: 0x65,
    },
];

#[test]
fn every_opcode_runs_as_before() {
    // Recorded from the tree as it stood before the opcode table.
    const BEFORE: &str = "145b5930bd5f5d2e";

    let mut bytes = Vec::new();
    let mut accesses = 0usize;
    for opcode in 0..=u8::MAX {
        for state in &STATES {
            let mut bus = Recorder::new();
            bus.mem[0x8000] = opcode;
            let mut cpu = Cpu::new();
            cpu.pc = 0x8000;
            cpu.sp = 0xFD;
            cpu.a = state.a;
            cpu.x = state.x;
            cpu.y = state.y;
            cpu.p = state.p;
            let cycles = cpu.step(&mut bus);
            bytes.extend_from_slice(&cycles.to_le_bytes());
            bytes.extend_from_slice(&[cpu.a, cpu.x, cpu.y, cpu.sp, cpu.p, u8::from(cpu.jammed)]);
            bytes.extend_from_slice(&cpu.pc.to_le_bytes());
            bytes.extend_from_slice(&(bus.log.len() as u32).to_le_bytes());
            for &(write, addr, val) in &bus.log {
                bytes.push(u8::from(write));
                bytes.extend_from_slice(&addr.to_le_bytes());
                bytes.push(val);
            }
            accesses += bus.log.len();
        }
    }
    // Non-vacuity: the two states together must have fetched every opcode and done something.
    assert!(
        accesses >= 512,
        "only {accesses} bus accesses over the whole matrix"
    );

    let digest = fnv1a64(&bytes);
    assert_eq!(format!("{digest:016x}"), BEFORE, "opcode matrix digest");
}
