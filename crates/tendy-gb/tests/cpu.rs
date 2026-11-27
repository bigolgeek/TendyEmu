//! CPU tests: the opcode tables, flags, the four latches (HALT bug, EI delay,
//! illegal lock, STOP) and interrupt dispatch, plus Blargg's `cpu_instrs` as the ROM gate.
//!
//! Every case runs real instructions through `Cpu::step` on a real `Bus`: the code is copied into
//! WRAM and executed from `$C000`, so no fixture file is needed and the PPU/timer keep ticking.

mod common;

use common::{entry, machine, synth_rom};
use tendy_gb::DOTS_PER_LINE;
use tendy_gb::Machine;
use tendy_gb::bus::{IF_SERIAL, IF_STAT, IF_TIMER, IF_VBLANK};
use tendy_gb::cpu::alu;
use tendy_gb::cpu::opcodes::{CB_CYCLES, OPCODE_CYCLES, is_illegal, official_cycles};
use tendy_gb::cpu::{Cpu, Flags};
use tendy_gb::ppu::Mode;

/// A machine whose PC is `$C000`, with `code` copied into WRAM there.
fn harness(code: &[u8]) -> Machine {
    let mut m = machine(synth_rom(&entry(&[])));
    m.cpu = Cpu::post_boot(0);
    m.cpu.pc = 0xC000;
    m.bus.ie = 0;
    m.bus.iflag = 0;
    m.bus.wram[..code.len()].copy_from_slice(code);
    m
}

/// One loop reusing a single machine: registers, flags and PC are restored per iteration, so a
/// 10 000-case check does not build 10 000 cartridges.
struct Bench {
    m: Machine,
}

impl Bench {
    fn new(code: &[u8]) -> Bench {
        Bench { m: harness(code) }
    }

    fn run_n(&mut self, steps: u32, setup: impl FnOnce(&mut Machine)) -> &Machine {
        let m = &mut self.m;
        m.cpu.pc = 0xC000;
        m.cpu.sp = 0xFFFE;
        m.cpu.f = Flags(0);
        m.cpu.ime = false;
        m.cpu.ime_pending = false;
        m.cpu.halted = false;
        m.cpu.halt_bug = false;
        m.cpu.locked = false;
        m.cpu.stopped = false;
        m.bus.ie = 0;
        m.bus.iflag = 0;
        setup(m);
        for _ in 0..steps {
            m.cpu.step(&mut m.bus);
        }
        m
    }

    fn run(&mut self, setup: impl FnOnce(&mut Machine)) -> &Machine {
        self.run_n(1, setup)
    }
}

/// Steps the machine once on `code` with `setup` applied first.
fn run(code: &[u8], setup: impl FnOnce(&mut Machine)) -> Machine {
    let mut bench = Bench::new(code);
    bench.run(setup);
    bench.m
}

// ---------------------------------------------------------------------------------------------
// Cycle counts
// ---------------------------------------------------------------------------------------------

#[test]
fn official_opcodes_cycles() {
    for op in 0..=0xFFu8 {
        if is_illegal(op) {
            continue;
        }
        if op == 0xCB {
            // `CB_CYCLES` includes the prefix M-cycle; the base table's `1` is the prefix alone.
            let mut m = harness(&[0xCB, 0x00]);
            let cycles = m.cpu.step(&mut m.bus);
            assert_eq!(
                cycles, CB_CYCLES[0x00] as u32,
                "$CB $00 costs the CB table's count, prefix included"
            );
            assert_eq!(OPCODE_CYCLES[0xCB_usize], 1, "the prefix alone is 1");
            continue;
        }
        for flags in [0x00u8, 0x90u8] {
            // F = $00 and F = $90 make every condition taken in one run and not in the other.
            let mut m = harness(&[op, 0x00, 0x00, 0x00]);
            m.cpu.f = Flags(flags);
            let before = m.bus.cycles;
            let cycles = m.cpu.step(&mut m.bus);
            let taken = condition_taken(op, flags);
            assert_eq!(
                cycles,
                official_cycles(op, taken) as u32,
                "opcode ${op:02X} with F={flags:02X}: step ticked {cycles}"
            );
            assert_eq!(
                m.bus.cycles - before,
                cycles as u64,
                "opcode ${op:02X}: the return value must equal the ticks made"
            );
        }
    }
}

/// Whether the conditional opcode `op` takes its branch when the flags are `f`.
fn condition_taken(op: u8, f: u8) -> bool {
    let z = f & 0x80 != 0;
    let c = f & 0x10 != 0;
    match op {
        0x20 | 0xC0 | 0xC2 | 0xC4 => !z, // NZ
        0x28 | 0xC8 | 0xCA | 0xCC => z,  // Z
        0x30 | 0xD0 | 0xD2 | 0xD4 => !c, // NC
        0x38 | 0xD8 | 0xDA | 0xDC => c,  // C
        _ => true,
    }
}

#[test]
fn cb_opcodes_cycles() {
    for cb in 0..=0xFFu8 {
        let mut m = harness(&[0xCB, cb]);
        m.cpu.set_hl(0xC100);
        let cycles = m.cpu.step(&mut m.bus);
        assert_eq!(
            cycles, CB_CYCLES[cb as usize] as u32,
            "$CB ${cb:02X}: step ticked a count the table does not have"
        );
        assert_eq!(
            m.cpu.pc, 0xC002,
            "$CB ${cb:02X}: the pair is one instruction"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------------------------

/// One hand-computed ALU case: `code`, the register state before it, and the expected result.
struct Case {
    code: &'static [u8],
    a: u8,
    b: u8,
    hl: u16,
    f: u8,
    result: u8,
    flags: u8,
}

/// ALU cases whose expected values are written out by hand from the Pan Docs flag rules.
/// `result` is A, except for `INC B`/`DEC B`, where it is B.
const ALU_CASES: &[Case] = &[
    // ADD A,B
    Case {
        code: &[0x80],
        a: 0x0F,
        b: 0x01,
        hl: 0,
        f: 0x00,
        result: 0x10,
        flags: 0x20,
    },
    Case {
        code: &[0x80],
        a: 0xFF,
        b: 0x01,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0xB0,
    },
    Case {
        code: &[0x80],
        a: 0x00,
        b: 0x00,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x80,
    },
    // ADC A,B
    Case {
        code: &[0x88],
        a: 0x00,
        b: 0x00,
        hl: 0,
        f: 0x10,
        result: 0x01,
        flags: 0x00,
    },
    Case {
        code: &[0x88],
        a: 0x0F,
        b: 0x00,
        hl: 0,
        f: 0x10,
        result: 0x10,
        flags: 0x20,
    },
    Case {
        code: &[0x88],
        a: 0xFF,
        b: 0x00,
        hl: 0,
        f: 0x10,
        result: 0x00,
        flags: 0xB0,
    },
    // SUB B
    Case {
        code: &[0x90],
        a: 0x00,
        b: 0x01,
        hl: 0,
        f: 0x00,
        result: 0xFF,
        flags: 0x70,
    },
    Case {
        code: &[0x90],
        a: 0x10,
        b: 0x01,
        hl: 0,
        f: 0x00,
        result: 0x0F,
        flags: 0x60,
    },
    Case {
        code: &[0x90],
        a: 0x10,
        b: 0x10,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0xC0,
    },
    // SBC A,B
    Case {
        code: &[0x98],
        a: 0x00,
        b: 0x00,
        hl: 0,
        f: 0x10,
        result: 0xFF,
        flags: 0x70,
    },
    Case {
        code: &[0x98],
        a: 0x10,
        b: 0x00,
        hl: 0,
        f: 0x10,
        result: 0x0F,
        flags: 0x60,
    },
    // AND B / OR B / XOR B
    Case {
        code: &[0xA0],
        a: 0xF0,
        b: 0x0F,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0xA0,
    },
    Case {
        code: &[0xA0],
        a: 0xFF,
        b: 0x0F,
        hl: 0,
        f: 0x00,
        result: 0x0F,
        flags: 0x20,
    },
    Case {
        code: &[0xB0],
        a: 0x00,
        b: 0x00,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x80,
    },
    Case {
        code: &[0xB0],
        a: 0x01,
        b: 0x02,
        hl: 0,
        f: 0x00,
        result: 0x03,
        flags: 0x00,
    },
    Case {
        code: &[0xA8],
        a: 0xFF,
        b: 0xFF,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x80,
    },
    Case {
        code: &[0xA8],
        a: 0x0F,
        b: 0xF0,
        hl: 0,
        f: 0x00,
        result: 0xFF,
        flags: 0x00,
    },
    // CP B leaves A alone
    Case {
        code: &[0xB8],
        a: 0x10,
        b: 0x10,
        hl: 0,
        f: 0x00,
        result: 0x10,
        flags: 0xC0,
    },
    Case {
        code: &[0xB8],
        a: 0x10,
        b: 0x20,
        hl: 0,
        f: 0x00,
        result: 0x10,
        flags: 0x50,
    },
    // INC B / DEC B keep the carry
    Case {
        code: &[0x04],
        a: 0x00,
        b: 0x0F,
        hl: 0,
        f: 0x10,
        result: 0x10,
        flags: 0x30,
    },
    Case {
        code: &[0x04],
        a: 0x00,
        b: 0xFF,
        hl: 0,
        f: 0x10,
        result: 0x00,
        flags: 0xB0,
    },
    Case {
        code: &[0x05],
        a: 0x00,
        b: 0x10,
        hl: 0,
        f: 0x10,
        result: 0x0F,
        flags: 0x70,
    },
    Case {
        code: &[0x05],
        a: 0x00,
        b: 0x01,
        hl: 0,
        f: 0x10,
        result: 0x00,
        flags: 0xD0,
    },
    // ADD HL,BC keeps Z, clears N (`add_hl_flags` covers the pair)
    Case {
        code: &[0x09],
        a: 0x00,
        b: 0x00,
        hl: 0x0FFF,
        f: 0x10,
        result: 0x00,
        flags: 0x00,
    },
    Case {
        code: &[0x09],
        a: 0x00,
        b: 0x01,
        hl: 0x0FFF,
        f: 0x00,
        result: 0x00,
        flags: 0x20,
    },
    // DAA
    Case {
        code: &[0x27],
        a: 0x15,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x15,
        flags: 0x00,
    },
    Case {
        code: &[0x27],
        a: 0x1A,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x20,
        flags: 0x00,
    },
    Case {
        code: &[0x27],
        a: 0x99,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x99,
        flags: 0x00,
    },
    Case {
        code: &[0x27],
        a: 0x9A,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x90,
    },
    Case {
        code: &[0x27],
        a: 0xA0,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x90,
    },
    Case {
        code: &[0x27],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x50,
        result: 0xA0,
        flags: 0x50,
    },
    Case {
        code: &[0x27],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x40,
        result: 0x00,
        flags: 0xC0,
    },
    // RLCA RRCA RLA RRA CPL SCF CCF
    Case {
        code: &[0x07],
        a: 0x85,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x0B,
        flags: 0x10,
    },
    Case {
        code: &[0x07],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x00,
    },
    Case {
        code: &[0x0F],
        a: 0x01,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x80,
        flags: 0x10,
    },
    Case {
        code: &[0x17],
        a: 0x80,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x10,
    },
    Case {
        code: &[0x17],
        a: 0x40,
        b: 0,
        hl: 0,
        f: 0x10,
        result: 0x81,
        flags: 0x00,
    },
    Case {
        code: &[0x1F],
        a: 0x01,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x10,
    },
    Case {
        code: &[0x1F],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x10,
        result: 0x80,
        flags: 0x00,
    },
    Case {
        code: &[0x2F],
        a: 0x0F,
        b: 0,
        hl: 0,
        f: 0x10,
        result: 0xF0,
        flags: 0x70,
    },
    Case {
        // CPL leaves Z alone: with Z set the result keeps it. Blargg `09-op r,r` fails on exactly
        // this (`2F Failed`) when Z is dropped.
        code: &[0x2F],
        a: 0x0F,
        b: 0,
        hl: 0,
        f: 0x80,
        result: 0xF0,
        flags: 0xE0,
    },
    Case {
        code: &[0x37],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x80,
        result: 0x00,
        flags: 0x90,
    },
    Case {
        code: &[0x3F],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x10,
        result: 0x00,
        flags: 0x00,
    },
    Case {
        code: &[0x3F],
        a: 0x00,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0x10,
    },
    // The immediate forms
    Case {
        code: &[0xC6, 0x01],
        a: 0xFF,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0xB0,
    },
    Case {
        code: &[0xFE, 0x01],
        a: 0xFF,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0xFF,
        flags: 0x40,
    },
    Case {
        code: &[0xE6, 0x00],
        a: 0xFF,
        b: 0,
        hl: 0,
        f: 0x00,
        result: 0x00,
        flags: 0xA0,
    },
];

#[test]
fn official_opcodes_flags() {
    for case in ALU_CASES {
        let m = run(case.code, |m| {
            m.cpu.a = case.a;
            m.cpu.b = case.b;
            if case.code[0] == 0x09 {
                m.cpu.set_bc(case.b as u16); // ADD HL,BC adds the pair
            }
            m.cpu.set_hl(case.hl);
            m.cpu.f = Flags(case.f);
        });
        let name: String = case
            .code
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        if matches!(case.code[0], 0x04 | 0x05) {
            assert_eq!(m.cpu.b, case.result, "${name}: B after");
            assert_eq!(m.cpu.a, case.a, "${name}: INC/DEC must not touch A");
        } else {
            assert_eq!(
                m.cpu.a, case.result,
                "${name}: A after (started {:#04X})",
                case.a
            );
        }
        assert_eq!(
            m.cpu.f.0, case.flags,
            "${name}: F after (started {:#04X})",
            case.f
        );
    }

    // The whole LD block leaves the flags alone, and each `LD r,r'` moves the right byte.
    for op in 0x40..=0x7Fu8 {
        if op == 0x76 {
            continue;
        }
        let source = op & 7;
        let dest = (op >> 3) & 7;
        if source == 6 || dest == 6 {
            continue; // the (HL) forms have their own memory test
        }
        let m = run(&[op], |m| {
            m.cpu.f = Flags(0x90);
            m.cpu.b = 0x11;
            m.cpu.c = 0x22;
            m.cpu.d = 0x33;
            m.cpu.e = 0x44;
            m.cpu.h = 0x55;
            m.cpu.l = 0x66;
            m.cpu.a = 0x77;
        });
        let value = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x00, 0x77][source as usize];
        let got = [
            m.cpu.b, m.cpu.c, m.cpu.d, m.cpu.e, m.cpu.h, m.cpu.l, 0, m.cpu.a,
        ][dest as usize];
        assert_eq!(got, value, "LD ${op:02X}: wrong byte moved");
        assert_eq!(m.cpu.f.0, 0x90, "LD ${op:02X}: flags must not change");
    }
}

/// An independent statement of the DAA rule (Pan Docs, `CPU_Registers_and_Flags.html`).
fn daa_reference(a: u8, n: bool, h: bool, c: bool) -> (u8, u8) {
    let mut value = a;
    let mut carry = c;
    if !n {
        if carry || value > 0x99 {
            value = value.wrapping_add(0x60);
            carry = true;
        }
        if h || value & 0x0F > 0x09 {
            value = value.wrapping_add(0x06);
        }
    } else {
        if carry {
            value = value.wrapping_sub(0x60);
        }
        if h {
            value = value.wrapping_sub(0x06);
        }
    }
    let mut f = u8::from(n) << 6;
    if value == 0 {
        f |= Flags::Z;
    }
    if carry {
        f |= Flags::C;
    }
    (value, f)
}

/// Two-digit BCD, for the arithmetic identity DAA exists to provide.
fn bcd(value: u8) -> u8 {
    (value / 10) << 4 | (value % 10)
}

#[test]
fn daa_every_input() {
    // Every reachable flag input, against the reference above.
    let mut bench = Bench::new(&[0x27]);
    for a in 0..=0xFFu8 {
        for n in [false, true] {
            for h in [false, true] {
                for c in [false, true] {
                    let f = Flags(u8::from(n) << 6 | u8::from(h) << 5 | u8::from(c) << 4);
                    let m = bench.run(|m| {
                        m.cpu.a = a;
                        m.cpu.f = f;
                    });
                    let (value, flags) = daa_reference(a, n, h, c);
                    assert_eq!(m.cpu.a, value, "DAA ${a:02X} N={n} H={h} C={c}: A");
                    // Z is not an input; the reference's Z is the result's.
                    assert_eq!(
                        m.cpu.f.0 & 0xF0,
                        flags & 0xF0,
                        "DAA ${a:02X} N={n} H={h} C={c}: flags"
                    );
                }
            }
        }
    }

    // And the semantic proof: adding two BCD digits and adjusting gives the BCD sum.
    let mut bench = Bench::new(&[0x80, 0x27]); // add a,b ; daa
    for x in 0..100u8 {
        for y in 0..100u8 {
            let sum = x as u16 + y as u16;
            let m = bench.run_n(2, |m| {
                m.cpu.a = bcd(x);
                m.cpu.b = bcd(y);
            });
            assert_eq!(
                m.cpu.a,
                bcd((sum % 100) as u8),
                "DAA after ${:02X} + ${:02X}",
                bcd(x),
                bcd(y)
            );
            assert_eq!(
                m.cpu.f.c(),
                sum >= 100,
                "DAA carry after ${:02X} + ${:02X}",
                bcd(x),
                bcd(y)
            );
        }
    }

    // The subtraction direction: `SUB` then `DAA` is the BCD difference, borrowing as C.
    let mut bench = Bench::new(&[0x90, 0x27]); // sub b ; daa
    for x in 0..100u8 {
        for y in 0..100u8 {
            let m = bench.run_n(2, |m| {
                m.cpu.a = bcd(x);
                m.cpu.b = bcd(y);
            });
            let (value, carry) = if x >= y {
                (bcd(x - y), false)
            } else {
                (bcd(100 + x - y), true)
            };
            assert_eq!(
                m.cpu.a,
                value,
                "DAA after ${:02X} - ${:02X}",
                bcd(x),
                bcd(y)
            );
            assert_eq!(
                m.cpu.f.c(),
                carry,
                "DAA borrow after ${:02X} - ${:02X}",
                bcd(x),
                bcd(y)
            );
        }
    }
}

#[test]
fn add_hl_flags() {
    // A stride through all 65 536 HL values plus every carry boundary.
    let mut values: Vec<u16> = (0..=0xFFFFu16).step_by(53).collect();
    values.extend([
        0x0000, 0x0001, 0x0FF0, 0x0FFF, 0x1000, 0x7FFF, 0x8000, 0xFFF0, 0xFFFF,
    ]);
    let mut bench = Bench::new(&[0x09]); // add hl,bc
    for hl in values {
        for addend in [0x0001u16, 0x0FFF, 0x1000, 0x8000, 0xFFFF] {
            let m = bench.run(|m| {
                m.cpu.set_bc(addend);
                m.cpu.set_hl(hl);
                m.cpu.f = Flags(0xA0); // Z set: it must survive; H set: it must be recomputed
            });
            let sum = hl as u32 + addend as u32;
            assert_eq!(
                m.cpu.hl(),
                sum as u16,
                "ADD HL,${addend:04X} with HL=${hl:04X}"
            );
            let h = (hl & 0x0FFF) + (addend & 0x0FFF) > 0x0FFF;
            let c = sum > 0xFFFF;
            assert!(m.cpu.f.z(), "ADD HL: Z is preserved");
            assert!(!m.cpu.f.n(), "ADD HL: N is cleared");
            assert_eq!(m.cpu.f.h(), h, "ADD HL,${addend:04X} with HL=${hl:04X}: H");
            assert_eq!(m.cpu.f.c(), c, "ADD HL,${addend:04X} with HL=${hl:04X}: C");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// CB block flags
// ---------------------------------------------------------------------------------------------

/// The rotate/shift result and carry, written with bit arithmetic rather than a rotate call.
fn shift_reference(cb: u8, value: u8, carry_in: bool) -> (u8, bool) {
    let bit = |v: u8, i: u8| v >> i & 1 != 0;
    match (cb >> 3) & 7 {
        0 => ((value << 1) | u8::from(bit(value, 7)), bit(value, 7)), // RLC
        1 => ((value >> 1) | (u8::from(bit(value, 0)) << 7), bit(value, 0)), // RRC
        2 => ((value << 1) | u8::from(carry_in), bit(value, 7)),      // RL
        3 => ((value >> 1) | (u8::from(carry_in) << 7), bit(value, 0)), // RR
        4 => (value << 1, bit(value, 7)),                             // SLA
        5 => ((value >> 1) | (value & 0x80), bit(value, 0)),          // SRA
        6 => (value.rotate_right(4), false),                          // SWAP
        _ => (value >> 1, bit(value, 0)),                             // SRL
    }
}

#[test]
fn cb_opcodes_flags() {
    let mut bench = Bench::new(&[0xCB, 0x00]);
    for cb in 0..=0xFFu8 {
        for value in [0x00u8, 0x01, 0x0F, 0x80, 0x85, 0xF0, 0xFF] {
            for carry_in in [false, true] {
                let f_in = if carry_in { 0x10 } else { 0x00 };
                let m = bench.run(|m| {
                    m.bus.wram[1] = cb; // the byte after the $CB prefix
                    // Every register holds the same byte, so the opcode's own index picks the one
                    // it must transform. The `(HL)` forms read $C100 instead, which needs H and L
                    // to hold the pointer, not the operand.
                    m.cpu.a = value;
                    m.cpu.b = value;
                    m.cpu.c = value;
                    m.cpu.d = value;
                    m.cpu.e = value;
                    m.cpu.h = value;
                    m.cpu.l = value;
                    if cb & 7 == 6 {
                        m.cpu.set_hl(0xC100);
                        m.bus.wram[0x100] = value;
                    }
                    m.cpu.f = Flags(f_in);
                });
                let index = (cb & 7) as usize;
                let got = [
                    m.cpu.b,
                    m.cpu.c,
                    m.cpu.d,
                    m.cpu.e,
                    m.cpu.h,
                    m.cpu.l,
                    m.bus.wram[0x100],
                    m.cpu.a,
                ][index];
                let block = cb >> 6;
                let bit_number = (cb >> 3) & 7;
                match block {
                    0 => {
                        let (result, carry) = shift_reference(cb, value, carry_in);
                        assert_eq!(got, result, "CB ${cb:02X} on ${value:02X}: value");
                        assert_eq!(m.cpu.f.z(), result == 0, "CB ${cb:02X} on ${value:02X}: Z");
                        assert_eq!(m.cpu.f.c(), carry, "CB ${cb:02X} on ${value:02X}: C");
                        assert!(!m.cpu.f.n() && !m.cpu.f.h(), "CB ${cb:02X}: N and H clear");
                    }
                    1 => {
                        assert_eq!(got, value, "BIT ${cb:02X}: value untouched");
                        assert_eq!(
                            m.cpu.f.z(),
                            value >> bit_number & 1 == 0,
                            "BIT {bit_number},${value:02X}: Z is the complement"
                        );
                        assert!(m.cpu.f.h() && !m.cpu.f.n(), "BIT: H set, N clear");
                        assert_eq!(m.cpu.f.c(), carry_in, "BIT: C preserved");
                    }
                    2 => {
                        assert_eq!(
                            got,
                            value & !(1 << bit_number),
                            "RES {bit_number},${value:02X}"
                        );
                        assert_eq!(m.cpu.f.0, f_in, "RES: flags untouched");
                    }
                    _ => {
                        assert_eq!(
                            got,
                            value | (1 << bit_number),
                            "SET {bit_number},${value:02X}"
                        );
                        assert_eq!(m.cpu.f.0, f_in, "SET: flags untouched");
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The four latches
// ---------------------------------------------------------------------------------------------

#[test]
fn halt_bug_reads_the_next_byte_twice() {
    // IME = 0 with an interrupt pending: HALT does not halt, PC does not advance past it, and the
    // following instruction runs twice (`halt.html`). A starts at $01 (post-boot).
    let mut m = harness(&[0x76, 0x3C, 0x00]); // halt; inc a; nop
    m.bus.ie = 0x01;
    m.bus.iflag = 0x01;
    let a0 = m.cpu.a;
    assert_eq!(m.cpu.step(&mut m.bus), 1, "HALT itself is one M-cycle");
    assert!(!m.cpu.halted, "the halt bug means no halt");
    assert!(m.cpu.halt_bug, "the halt bug latch is set");
    m.cpu.step(&mut m.bus); // INC A, PC suppressed
    assert_eq!(m.cpu.a, a0 + 1);
    assert_eq!(
        m.cpu.pc, 0xC001,
        "PC did not advance for the duplicated byte"
    );
    m.cpu.step(&mut m.bus); // INC A again
    assert_eq!(m.cpu.a, a0 + 2, "the byte after HALT ran twice");
    assert_eq!(m.cpu.pc, 0xC002, "and only then did PC advance");

    // With no interrupt pending, HALT really halts and PC moves on normally.
    let mut m = harness(&[0x76, 0x3C]);
    m.cpu.step(&mut m.bus);
    assert!(m.cpu.halted);
    assert!(!m.cpu.halt_bug);
    assert_eq!(m.cpu.pc, 0xC001);
    assert_eq!(m.cpu.step(&mut m.bus), 1, "a halted step is one M-cycle");
    assert!(m.cpu.halted, "nothing is pending, so it stays halted");
    assert_eq!(m.cpu.a, a0, "a halted CPU executes nothing");
}

#[test]
fn ei_halt_dispatch_replaces_the_halt_bug_byte() {
    // `ei; halt` with an interrupt already pending: EI's delay leaves IME = 0 for the HALT, so the
    // halt bug latch is set, but the dispatch takes the pending byte's place — `halt.html` says the
    // handler is called and the interrupt returns to the HALT. The handler's first opcode must
    // therefore be read once, not re-read as its own operand.
    let mut m = harness(&[0xFB, 0x76, 0x3C]); // ei; halt; inc a
    m.bus.ie = 0x01;
    m.bus.iflag = 0x01;
    let a0 = m.cpu.a;
    m.cpu.step(&mut m.bus); // ei
    assert!(!m.cpu.ime, "EI is delayed by one instruction");
    m.cpu.step(&mut m.bus); // halt
    assert!(m.cpu.halt_bug, "IME was 0 with an interrupt pending");
    assert!(m.cpu.ime, "EI armed IME once the HALT had run");
    m.cpu.step(&mut m.bus); // the dispatch
    assert_eq!(m.cpu.pc, 0x0040, "the VBlank handler runs");
    assert!(!m.cpu.halt_bug, "the dispatch consumes the halt-bug latch");
    // $0040 in the synthesized ROM is a NOP.
    m.cpu.step(&mut m.bus);
    assert_eq!(m.cpu.pc, 0x0041, "the handler's first byte is not re-read");
    assert_eq!(m.cpu.a, a0, "no handler opcode ran twice");
}

#[test]
fn ei_delays_ime_by_one_instruction() {
    let mut m = harness(&[0xFB, 0x00, 0x00]); // ei; nop; nop
    m.bus.ie = 0x01;
    m.bus.iflag = 0x01;
    assert!(!m.cpu.ime);

    m.cpu.step(&mut m.bus); // EI
    assert!(!m.cpu.ime, "IME is not set by EI itself");
    assert!(m.cpu.ime_pending);

    m.cpu.step(&mut m.bus); // the instruction after EI runs before any dispatch
    assert_eq!(m.cpu.pc, 0xC002, "the NOP after EI executed");
    assert!(m.cpu.ime, "IME is armed after the following instruction");

    let cycles = m.cpu.step(&mut m.bus);
    assert_eq!(cycles, 5, "the dispatch costs five M-cycles");
    assert_eq!(m.cpu.pc, 0x40, "VBlank vector");
    assert!(!m.cpu.ime, "the dispatch clears IME");

    // EI immediately followed by DI leaves interrupts off.
    let mut m = harness(&[0xFB, 0xF3, 0x00]); // ei; di
    m.cpu.step(&mut m.bus);
    m.cpu.step(&mut m.bus);
    assert!(!m.cpu.ime, "DI after EI cancels the arming");
    assert!(!m.cpu.ime_pending);
}

#[test]
fn illegal_opcode_locks_once() {
    for op in [
        0xD3u8, 0xDB, 0xDD, 0xE3, 0xE4, 0xEB, 0xEC, 0xED, 0xF4, 0xFC, 0xFD,
    ] {
        let mut m = harness(&[op, 0x3C, 0x3C]); // illegal; inc a; inc a
        let a0 = m.cpu.a;
        let cycles = m.cpu.step(&mut m.bus);
        assert_eq!(cycles, 1, "${op:02X}: the illegal opcode costs one M-cycle");
        assert!(m.cpu.locked, "${op:02X} locks the CPU");
        let after = m.cpu.pc;
        for _ in 0..4 {
            assert_eq!(
                m.cpu.step(&mut m.bus),
                1,
                "${op:02X}: a locked step is one M-cycle"
            );
        }
        assert_eq!(m.cpu.a, a0, "${op:02X}: nothing executes while locked");
        assert_eq!(m.cpu.pc, after, "${op:02X}: PC stays put while locked");
        assert!(m.cpu.locked);
    }
    for op in 0..=0xFFu8 {
        if is_illegal(op) {
            assert_eq!(
                OPCODE_CYCLES[op as usize], 0,
                "${op:02X}: illegals are 0 in the table"
            );
        } else {
            assert_ne!(
                OPCODE_CYCLES[op as usize], 0,
                "${op:02X}: is legal, needs a count"
            );
        }
    }
}

#[test]
fn interrupt_dispatch_order() {
    for (iflag, expected_bit) in [(0x1Fu8, 0u16), (0x1C, 2), (0x18, 3), (0x10, 4), (0x02, 1)] {
        let mut m = harness(&[0x00]);
        m.bus.ie = 0x1F;
        m.bus.iflag = iflag;
        m.cpu.ime = true;
        let cycles = m.cpu.step(&mut m.bus);
        assert_eq!(cycles, 5, "dispatch costs five M-cycles");
        assert_eq!(
            m.cpu.pc,
            0x40 + expected_bit * 8,
            "IF=${iflag:02X}: the lowest pending bit wins"
        );
        assert_eq!(
            m.bus.iflag,
            iflag & !(1 << expected_bit),
            "only the serviced bit is cleared"
        );
        assert_eq!(m.cpu.sp, 0xFFFC, "two bytes pushed");
        assert_eq!(m.bus.hram[0x7C], 0x00, "the return address low byte");
        assert_eq!(m.bus.hram[0x7D], 0xC0, "the return address high byte");
    }

    // The push order is high byte first: SP+1 holds the high half of the return address.
    let mut m = harness(&[0x00]);
    m.cpu.pc = 0x1234;
    m.bus.ie = 0x01;
    m.bus.iflag = 0x01;
    m.cpu.ime = true;
    m.cpu.step(&mut m.bus);
    assert_eq!((m.bus.hram[0x7D], m.bus.hram[0x7C]), (0x12, 0x34));
}

#[test]
fn oam_dma_restart_keeps_the_bus_owned() {
    // `oam_dma_start.s`, "Expected timing (restarted DMA)": `M = 1: previous DMA is running (OAM
    // *not* accessible)`, `M = 2: new DMA starts`. A write to `$FF46` therefore never un-blocks
    // OAM; before the carried bit it did for two M-cycles and that ROM's round 2 executed the
    // `INC B` at `$FE00` (b = 1 where it wants 0).
    let mut m = harness(&[]);
    m.bus.write(0xFF46, 0xC0);
    m.bus.tick();
    m.bus.tick(); // M-cycle 2 of the first transfer: blocked
    assert!(m.bus.dma.blocks());
    m.bus.write(0xFF46, 0xC1); // restart while the first one runs
    assert!(
        m.bus.dma.blocks(),
        "a restart must not hand OAM back to the CPU"
    );
    m.bus.tick();
    assert!(m.bus.dma.blocks(), "still owned one M-cycle later");
    assert_eq!(m.bus.dma.index, 0, "the new transfer starts from byte 0");
}

#[test]
fn ie_keeps_all_eight_bits() {
    // `bits/unused_hwio-GS` is the exception to "unused bits and unused IO all return 1s": it
    // asserts `test IE %11100000 %00000000 %00000000` (write 0, read $00) *and*
    // `test IE %11100000 %11100000 %11100000` (write $E0, read $E0), so IE is plain eight-bit
    // storage — unlike IF, whose three unused bits do read as ones. Reading `$E0 | ie` while
    // masking writes to `$1F` failed the first of those two cases.
    let mut m = harness(&[]);
    m.bus.write(0xFFFF, 0x00);
    assert_eq!(m.bus.read(0xFFFF), 0x00, "IE read back a set unused bit");
    m.bus.write(0xFFFF, 0x1F);
    assert_eq!(m.bus.read(0xFFFF), 0x1F, "IE read back a set unused bit");
    m.bus.write(0xFFFF, 0xFF);
    assert_eq!(m.bus.ie, 0xFF, "writes keep every bit");
    m.bus.write(0xFF0F, 0xFF);
    assert_eq!(
        m.bus.pending(),
        0x1F,
        "only the five real bits can request an interrupt"
    );
}

#[test]
fn cpl_preserves_z_and_c_and_sets_n_h() {
    // `CPL` on hardware: N = H = 1, Z and C untouched (`CPU_Registers_and_Flags.html`).
    // Blargg's `09-op r,r` starts with NOP then CPL and pins both Z states through its
    // checksums, so preserving only C printed `2F Failed` on the pre-fix tree.
    let z = Flags::Z;
    let n = Flags::N;
    let h = Flags::H;
    let c = Flags::C;
    assert_eq!(alu::cpl(0x00, 0x00), (0xFF, n | h), "CPL $00, F=$00");
    assert_eq!(
        alu::cpl(0x00, 0xF0),
        (0xFF, z | n | h | c),
        "CPL $00, F=$F0"
    );
    assert_eq!(alu::cpl(0x0F, z), (0xF0, z | n | h), "CPL $0F, F=Z");
    assert_eq!(alu::cpl(0xFF, c), (0x00, n | h | c), "CPL $FF, F=C");
}

/// The transfer's M-cycle timeline, pinned to `oam_dma_start.s`'s "Expected timing":
///
/// - fresh DMA: `M = 0` is the write, `M = 1` blocks nothing ("OAM still accessible"), `M = 2` is
///   the first blocked cycle;
/// - `oam_dma_timing.s` wants the 160th blocked cycle to be the one that a read one cycle before
///   "the OAM DMA end" lands on, i.e. the window is M-cycles 2..=161 with 160 bytes moved.
#[test]
fn oam_dma_starts_at_cycle_two_and_blocks_for_160() {
    let mut m = harness(&[]);
    for i in 0..0xA0usize {
        m.bus.wram[0x100 + i] = (i as u8) ^ 0x5A;
    }
    m.bus.write(0xFF46, 0xC1); // the write is M-cycle 0
    assert!(
        !m.bus.dma.blocks(),
        "M-cycle 0: the write itself is not blocked"
    );
    assert_eq!(m.bus.dma.index, 0, "no byte moved during the write");

    let mut blocked = Vec::new();
    let mut copies = 0;
    for cycle in 1..=200 {
        if m.bus.dma.blocks() {
            blocked.push(cycle);
        }
        let before = m.bus.dma.index;
        m.bus.tick();
        if m.bus.dma.index != before {
            copies += 1;
        }
    }
    assert_eq!(
        blocked,
        (2..=161).collect::<Vec<_>>(),
        "the block runs from M-cycle 2 to M-cycle 161"
    );
    assert_eq!(copies, 0xA0, "one byte per blocked M-cycle");
    for (i, byte) in m.bus.ppu.oam.iter().enumerate() {
        assert_eq!(
            *byte,
            (i as u8) ^ 0x5A,
            "OAM byte {i} comes from $C1{i:02X}"
        );
    }
}

#[test]
fn oam_dma_blocks_only_oam() {
    // `jp_timing.s`/`call_timing.s` execute `jp`/`call` from `$FDFE` inside the transfer and
    // `oam_dma_start.s` runs `wait_dma_finish` from the `$0038` vector mid-transfer, so ROM, WRAM
    // and HRAM have to stay readable; only `$FE00-$FE9F` answers `$FF`.
    let mut m = harness(&[]);
    m.bus.wram[0x0000] = 0x11;
    m.bus.hram[0x00] = 0x22;
    m.bus.write(0xFF46, 0xC0);
    m.bus.tick();
    m.bus.tick(); // M-cycle 2: blocked from here
    assert!(m.bus.dma.blocks());
    assert_eq!(m.bus.read(0xFE00), 0xFF, "an OAM read is blocked");
    m.bus.write(0xFE01, 0xAB); // dropped: the DMA owns OAM
    assert_ne!(m.bus.ppu.oam[1], 0xAB, "an OAM write is dropped");
    assert_eq!(m.bus.read(0xC000), 0x11, "WRAM still answers");
    assert_eq!(m.bus.read(0xFF80), 0x22, "HRAM still answers");
    assert_ne!(m.bus.read(0x0100), 0xFF, "ROM still answers");
    assert_eq!(
        m.bus.read(0xFF46) & 0xC0,
        0xC0,
        "the DMA register still answers"
    );
}

#[test]
fn oam_dma_reads_a_source_past_dfff_through_wram() {
    // `oam_dma/sources-GS`, "including the area past $DFFF": transfers from $E000, $FE00 and $FF00
    // must produce the bytes at $C000, $DE00 and $DF00.
    for (source, expect) in [(0xE0u8, 0xC000u16), (0xFE, 0xDE00), (0xFF, 0xDF00)] {
        let mut m = harness(&[]);
        for i in 0..0xA0usize {
            m.bus.wram[(expect - 0xC000) as usize + i] = i as u8 | 0x80;
        }
        m.bus.write(0xFF46, source);
        while m.bus.dma.active {
            m.bus.tick();
        }
        for (i, byte) in m.bus.ppu.oam.iter().enumerate() {
            assert_eq!(
                *byte,
                i as u8 | 0x80,
                "source ${source:02X}00 byte {i} comes from ${expect:04X}"
            );
        }
    }
}

#[test]
fn oam_bytes_are_indexed_from_fe00() {
    // $FE00 is OAM byte 0, not byte $FE00 % $A0 = 64. With the LCD off, OAM is not behind the PPU's
    // mode gate, so this measures the address decode alone.
    let mut m = harness(&[]);
    m.bus.write(0xFF40, 0x00);
    assert_eq!(m.bus.read(0xFF40) & 0x80, 0, "the LCD is off");
    m.bus.write(0xFE00, 0x12);
    m.bus.write(0xFE9F, 0x34);
    assert_eq!(m.bus.ppu.oam[0], 0x12, "$FE00 is the first OAM byte");
    assert_eq!(m.bus.ppu.oam[0x9F], 0x34, "$FE9F is the last OAM byte");
    assert_eq!(m.bus.read(0xFE00), 0x12);
    assert_eq!(m.bus.read(0xFE9F), 0x34);
}

#[test]
fn mbc5_powers_on_with_bank_1_at_4000() {
    // Pan Docs `MBC5.html`: the ROM bank register powers on as 1. `oam_dma/sources-GS` starts with
    // `call disable_ppu_safe` into bank 1 before any banking write, so bank 0 at $4000 made it
    // fetch $FF and `RST $38` forever.
    let mut rom = vec![0u8; 0x8000];
    rom[0x147] = 0x1B; // MBC5+RAM+battery
    rom[0x149] = 0x02; // 8 KiB RAM
    rom[0x100] = 0xC3; // jp $0150
    rom[0x101] = 0x50;
    rom[0x102] = 0x01;
    rom[0x4000] = 0x42; // bank 1, byte 0
    rom[0x0000] = 0x99; // bank 0, byte 0
    let mut m = machine(rom);
    assert_eq!(m.bus.read(0x4000), 0x42, "$4000 reads bank 1 at power-on");
    assert_eq!(m.bus.read(0x0000), 0x99, "$0000 stays on bank 0");
    m.bus.write(0x2000, 0x00);
    assert_eq!(m.bus.read(0x4000), 0x99, "a write of 0 selects bank 0");
}

// ---------------------------------------------------------------------------------------------
// The ROM gate
// ---------------------------------------------------------------------------------------------

/// Blargg's `cpu_instrs`: every part it reports, read from the serial log.
///
/// `UNVERIFIED:` the ROM's report format is taken to be `NN:ok` per part with no failure marker;
/// that is this ROM's own convention, not something Pan Docs states. The shipped build reports 36
/// parts, so the test requires every `NN:` status it finds to be `ok` and at least eleven of them
/// (the eleven groups §9.1 names).
#[test]
#[ignore = "needs roms/test/cpu_instrs.gb (scripts/fetch-test-roms.sh)"]
fn blargg_cpu_instrs_passes_all_11() {
    let path = test_rom("cpu_instrs.gb");
    let rom =
        std::fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    let mut m = machine(rom);
    let mut text = String::new();
    let mut seen = 0;
    let mut quiet = 0;
    for _ in 0..30_000 {
        m.run_frame();
        text = String::from_utf8_lossy(&m.bus.serial.out).into_owned();
        if text.contains("Passed all tests") || text.contains("Failed") {
            break;
        }
        // The report ends when the ROM stops talking. A part can be silent for a long time before
        // it prints `:ok` (parts 10 and 11 are the quiet ones), so this waits 50 emulated seconds.
        if seen > 0 && m.bus.serial.out.len() == seen {
            quiet += 1;
            if quiet > 3_000 {
                break;
            }
        } else {
            seen = m.bus.serial.out.len();
            quiet = 0;
        }
    }
    println!("{text}");
    assert!(
        !text.contains("Failed"),
        "cpu_instrs reported a failure:\n{text}"
    );
    // Every `NN:` the ROM printed must be followed by `ok`, and the numbers must run 01, 02, …
    // with no gap or repeat: a ROM that restarted or skipped a part would break this.
    let bytes = text.as_bytes();
    let mut parts = 0u32;
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i].is_ascii_digit() && bytes[i + 1].is_ascii_digit() && bytes[i + 2] == b':' {
            let status = &text[i + 3..i + 5];
            assert_eq!(
                status,
                "ok",
                "cpu_instrs part {} reported {status}",
                &text[i..i + 2]
            );
            parts += 1;
            assert_eq!(
                text[i..i + 2].parse::<u32>().unwrap(),
                parts,
                "cpu_instrs part numbers must be sequential"
            );
        }
    }
    assert!(
        parts >= 11,
        "cpu_instrs reported {parts} parts, expected at least 11:\n{text}"
    );
    println!("{parts} parts ok");
}

// ---------------------------------------------------------------------------------------------
// The Mooneye acceptance table (report only)
// ---------------------------------------------------------------------------------------------

/// Mooneye's pass/fail protocol (mooneye-test-suite `README.markdown`, "Pass/fail reporting"): a
/// finished test writes the Fibonacci numbers 3/5/8/13/21/34 to B/C/D/E/H/L and then executes
/// `LD B, B` as a debug breakpoint — that opcode is the signal to stop emulating and read the
/// registers. A failing test writes `$42` to all six.
const MOONEYE_PASS: [u8; 6] = [3, 5, 8, 13, 21, 34];
const MOONEYE_FAIL: u8 = 0x42;

/// Where `scripts/fetch-test-roms.sh` unpacks the suite.
const MOONEYE_ROOT: &str = "roms/test/mooneye";

/// A test ROM: the folder `TENDY_TEST_ROMS` names when it is set, else the one the fetch script
/// writes.
fn test_rom(name: &str) -> std::path::PathBuf {
    match std::env::var_os("TENDY_TEST_ROMS") {
        Some(dir) => std::path::PathBuf::from(dir).join(name),
        None => std::path::Path::new("roms/test").join(name),
    }
}

/// The mooneye suite's folder: under `TENDY_TEST_ROMS` when it is set.
fn mooneye_root() -> std::path::PathBuf {
    match std::env::var_os("TENDY_TEST_ROMS") {
        Some(dir) => std::path::PathBuf::from(dir).join("mooneye"),
        None => std::path::PathBuf::from(MOONEYE_ROOT),
    }
}

/// True for the file names the suite says pass on one console model only (§"Test naming"): `-C`
/// (CGB/AGB/AGS), `-S` (SGB/SGB2), `-A` (AGB/AGS), `-mgb` (Pocket), `-cgb`, `-sgb2`, `-agb`, `-ags`.
/// `-GS` is both groups the DMG belongs to, and `-dmg0`/`-dmgABC` are DMG revisions, so all of those
/// run.
fn mooneye_other_model(path: &std::path::Path) -> bool {
    const OTHERS: [&str; 9] = ["C", "S", "A", "mgb", "cgb", "sgb", "sgb2", "agb", "ags"];
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    match stem.rsplit_once('-') {
        Some((_, suffix)) => OTHERS.contains(&suffix),
        None => false,
    }
}

/// Every `acceptance/**/*.gb` under `dir`, sorted, skipping the model-specific ones.
fn mooneye_roms(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "acceptance") {
                mooneye_acceptance(&path, out);
            } else {
                mooneye_roms(&path, out);
            }
        }
    }
}

/// The ROMs inside one `acceptance` tree.
fn mooneye_acceptance(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut here = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            mooneye_acceptance(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "gb") && !mooneye_other_model(&path) {
            here.push(path);
        }
    }
    here.sort();
    out.extend(here);
}

/// Runs one ROM until its `LD B, B` breakpoint: the six registers it reported, or `None` if it
/// never reached one inside `steps`.
///
/// The look-ahead uses `Bus::peek`, never `Bus::read`: a `read` would spend a machine cycle of its
/// own between every instruction and shift the DIV/PPU phase the timing tests are measuring, which
/// turns real passes into false failures.
fn mooneye_registers(m: &mut Machine, steps: u32) -> Option<[u8; 6]> {
    for _ in 0..steps {
        let pc = m.cpu.pc;
        // `LD B, B` (`$40`) is the suite's breakpoint, so the step to stop on is the one that
        // executes it — the registers already hold the result when it runs.
        if m.bus.peek(pc) == 0x40 {
            m.cpu.step(&mut m.bus);
            return Some([m.cpu.b, m.cpu.c, m.cpu.d, m.cpu.e, m.cpu.h, m.cpu.l]);
        }
        m.cpu.step(&mut m.bus);
    }
    None
}

/// One ROM, with the last instructions before it stopped when it hangs.
///
/// `MOONEYE_ROM=<path>`; optional `MOONEYE_STEPS=<n>` (default 8 000 000) and `MOONEYE_TAIL=<n>`
/// (default 24, `0` to print none). Diagnostic only; it asserts nothing.
#[test]
#[ignore = "diagnostic: MOONEYE_ROM=<path> cargo test --release --test cpu -- --ignored mooneye_one --nocapture"]
fn mooneye_one() {
    let Ok(path) = std::env::var("MOONEYE_ROM") else {
        println!("MOONEYE_ROM is unset: nothing to run (this test only drives a single ROM)");
        return;
    };
    let steps: u32 = std::env::var("MOONEYE_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8_000_000);
    let tail: usize = std::env::var("MOONEYE_TAIL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    // `MOONEYE_ON_PC=0038` captures the history only from the first time that PC is reached.
    let on_pc: Option<u16> = std::env::var("MOONEYE_ON_PC")
        .ok()
        .and_then(|s| u16::from_str_radix(&s, 16).ok());
    // `MOONEYE_STOP_PC=0038` ends the run the first time that PC is reached.
    let stop_pc: Option<u16> = std::env::var("MOONEYE_STOP_PC")
        .ok()
        .and_then(|s| u16::from_str_radix(&s, 16).ok());
    // `MOONEYE_DUMP=c000:20,1200:10` prints hex dumps at the end (address:length, hex).
    let dump: Vec<(u16, usize)> = std::env::var("MOONEYE_DUMP")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|part| {
                    let (addr, len) = part.split_once(':')?;
                    Some((
                        u16::from_str_radix(addr, 16).ok()?,
                        len.parse::<usize>().ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let rom = std::fs::read(&path).expect("ROM");
    let mut m = machine(rom);
    let mut history: Vec<String> = Vec::new();
    let mut capturing = on_pc.is_none();
    let mut outcome = None;
    for _ in 0..steps {
        let pc = m.cpu.pc;
        if m.bus.peek(pc) == 0x40 {
            m.cpu.step(&mut m.bus);
            outcome = Some([m.cpu.b, m.cpu.c, m.cpu.d, m.cpu.e, m.cpu.h, m.cpu.l]);
            break;
        }
        if on_pc == Some(pc) {
            capturing = true;
        }
        if stop_pc == Some(pc) {
            println!("stopped at pc {pc:04x}");
            break;
        }
        if tail > 0 && capturing {
            let op = m.bus.peek(pc);
            history.push(format!(
                "m{:>9} pc{:04x} op{:02x} a{:02x} f{:02x} sp{:04x} hl{:04x} div{:02x} tima{:02x} tac{:02x} if{:02x} dma[{}{}{}{}]",
                m.bus.cycles,
                pc,
                op,
                m.cpu.a,
                m.cpu.f.0,
                m.cpu.sp,
                m.cpu.hl(),
                m.bus.timer.read(0xFF04),
                m.bus.timer.tima,
                m.bus.timer.tac,
                m.bus.iflag,
                m.bus.dma.active as u8,
                m.bus.dma.start_delay,
                m.bus.dma.index,
                m.bus.dma.blocks() as u8,
            ));
            if on_pc.is_some() {
                // Capture the first `tail` instructions after the hit, then stop.
                if history.len() >= tail {
                    break;
                }
            } else if history.len() > tail {
                history.remove(0);
            }
        }
        m.cpu.step(&mut m.bus);
    }
    println!("{path}: {}", m.bus.cycles);
    println!("serial {:02x?}", m.bus.serial.out);
    for &(addr, len) in &dump {
        let mut line = format!("dump {addr:04x}:");
        for i in 0..len {
            let a = addr.wrapping_add(i as u16);
            line.push_str(&format!(" {:02x}", m.bus.peek(a)));
        }
        println!("{line}");
    }
    match outcome {
        Some(r) => println!(
            "registers {r:02x?}  b={:02x} c={:02x} d={:02x} e={:02x} h={:02x} l={:02x}",
            r[0], r[1], r[2], r[3], r[4], r[5]
        ),
        None => {
            println!(
                "no breakpoint in {steps} steps: pc {:04x} sp {:04x} a {:02x} f {:02x}",
                m.cpu.pc, m.cpu.sp, m.cpu.a, m.cpu.f.0
            );
            for line in &history {
                println!("{line}");
            }
        }
    }
}

/// The Mooneye acceptance suite: a pass/fail table, no assertions on the results.
///
/// `69 ROMs: 63 pass, 6 fail`. The six that stay failing are expected: they need a
/// boot ROM, which `SPEC.md` does not require —
///
/// ```text
/// acceptance/serial/boot_sclk_align-dmgABCmgb.gb
/// acceptance/boot_div-dmg0.gb
/// acceptance/boot_div-dmgABCmgb.gb
/// acceptance/boot_hwio-dmg0.gb
/// acceptance/boot_hwio-dmgABCmgb.gb
/// acceptance/boot_regs-dmg0.gb
/// ```
///
/// `REPORT ONLY:` this test asks for the table and explicitly not for fixes, so it only fails
/// if the suite is not there to run. A hang (no `LD B, B` within the step budget) is reported as
/// `hang`, not as a failure of the test.
#[test]
#[ignore = "needs roms/test/mooneye (unzip mooneye-test-suite.zip there)"]
fn mooneye_acceptance_table() {
    let root = mooneye_root();
    let mut roms = Vec::new();
    mooneye_roms(&root, &mut roms);
    assert!(
        !roms.is_empty(),
        "{} has no acceptance ROMs — unzip mooneye-test-suite.zip there",
        root.display()
    );

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut hung = 0u32;
    let mut failing = Vec::new();
    for path in &roms {
        let rom = std::fs::read(path).expect("acceptance ROM");
        let mut m = machine(rom);
        // Two emulated seconds is far more than any acceptance test needs; a ROM still running is
        // stuck rather than slow.
        let result = mooneye_registers(&mut m, 8_000_000);
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/");
        // Drop the suite's `mts-<date>-<hash>/` directory, so the table names the test itself.
        let name = match name.split_once('/') {
            Some((_, rest)) => rest.to_owned(),
            None => name,
        };
        let verdict = match result {
            Some(registers) if registers == MOONEYE_PASS => {
                passed += 1;
                "pass".to_owned()
            }
            Some(registers) if registers.iter().all(|&r| r == MOONEYE_FAIL) => {
                failed += 1;
                failing.push(name.clone());
                "FAIL".to_owned()
            }
            Some(registers) => {
                failed += 1;
                failing.push(name.clone());
                format!("FAIL {registers:02x?}")
            }
            None => {
                hung += 1;
                failing.push(name.clone());
                "hang".to_owned()
            }
        };
        println!("{verdict:16} {name}");
    }
    println!(
        "\n{} ROMs: {passed} pass, {failed} fail, {hung} hang",
        roms.len()
    );
    if !failing.is_empty() {
        println!("not passing:");
        for name in &failing {
            println!("  {name}");
        }
    }
}

// ------------------------------------------------------ Dispatch and HALT timing
//
// Each case fails if the fix it names is reverted, and names the Mooneye ROM that found it.

/// `bits/unused_hwio-GS` reads `IE` back with `%11100000` and expects `%00000000` after writing
/// `$00`, then `%11100000` after writing `$E0`: unlike every other `$FFxx` register, `IE` stores
/// all eight bits (its three unused ones are plain storage, not read-as-one).
#[test]
fn ie_stores_all_eight_bits() {
    let mut m = harness(&[]);
    m.bus.write(0xFFFF, 0x00);
    assert_eq!(m.bus.read(0xFFFF), 0x00, "IE read back a set unused bit");
    m.bus.write(0xFFFF, 0xE0);
    assert_eq!(m.bus.read(0xFFFF), 0xE0, "IE lost a written unused bit");
    m.bus.write(0xFFFF, 0x1F);
    assert_eq!(m.bus.read(0xFFFF), 0x1F, "IE invented unused bits");
}

/// The other half of the same ROM: `IF`'s three unused bits read as ones whatever was written
/// (`test IF %11100000 %00000000 %11100000`).
#[test]
fn if_unused_bits_read_as_ones() {
    let mut m = harness(&[]);
    m.bus.write(0xFF0F, 0x00);
    assert_eq!(m.bus.read(0xFF0F), 0xE0);
}

/// `ie_push` round 3: `SP=$0001`, so the *lower*-byte push is the one that lands on `IE`. That
/// write is too late — the ROM expects the SERIAL handler to run and its `IF` bit to be the one
/// cleared — because the dispatch settles its vector after the upper-byte push, not after both.
#[test]
fn dispatch_latches_its_request_after_the_upper_byte_push() {
    let mut m = harness(&[]);
    m.cpu.sp = 0x0001;
    m.cpu.pc = 0x0035; // the pushed low byte, which clears the serial bit in IE
    m.cpu.ime = true;
    m.bus.ie = IF_SERIAL;
    m.bus.iflag = IF_SERIAL;
    m.cpu.step(&mut m.bus);
    assert_eq!(m.cpu.pc, 0x0058, "SERIAL vector ($40 + 8 * bit 3)");
    assert_eq!(
        m.bus.iflag & IF_SERIAL,
        0,
        "the taken request was not cleared"
    );
    assert_eq!(m.bus.ie, 0x35, "the lower-byte push did not land on IE");
}

/// `ie_push` round 1: `SP=$0000` puts the upper-byte push on `IE`, cancelling the timer request
/// mid-dispatch. The ROM expects PC=$0000 with `IF` untouched — the cancelled dispatch consumes
/// the five M-cycles but jumps nowhere.
#[test]
fn dispatch_cancelled_by_its_own_push_resumes_at_zero() {
    let mut m = harness(&[]);
    m.cpu.sp = 0x0000;
    m.cpu.pc = 0x0202; // high byte $02 clears the timer bit in IE
    m.cpu.ime = true;
    m.bus.ie = IF_TIMER;
    m.bus.iflag = IF_TIMER;
    let cycles = m.cpu.step(&mut m.bus);
    assert_eq!(m.cpu.pc, 0x0000);
    assert_eq!(m.bus.iflag & IF_TIMER, IF_TIMER, "IF was modified");
    assert_eq!(m.bus.ie, 0x02);
    assert!(
        !m.cpu.ime,
        "IME must stay cleared after a cancelled dispatch"
    );
    assert_eq!(cycles, 5, "the dispatch costs five M-cycles either way");
}

/// `ie_push` round 4: the upper-byte push drops the VBLANK request but keeps STAT, and the ROM
/// expects the STAT handler — the vector comes from the re-sampled request, not the one the
/// dispatch started on.
#[test]
fn dispatch_re_samples_which_source_it_takes() {
    let mut m = harness(&[]);
    m.cpu.sp = 0x0000;
    m.cpu.pc = 0x0202; // high byte $02 clears bit 0 (VBLANK) in IE = %00000011
    m.cpu.ime = true;
    m.bus.ie = IF_STAT | IF_VBLANK;
    m.bus.iflag = IF_STAT | IF_VBLANK;
    m.cpu.step(&mut m.bus);
    assert_eq!(m.cpu.pc, 0x0048, "STAT vector ($40 + 8 * bit 1)");
    assert_eq!(
        m.bus.iflag, IF_VBLANK,
        "only the taken request may be cleared"
    );
}

/// `halt_ime0_nointr_timing`: a flag raised by the M-cycle HALT is idling on must wake the CPU on
/// *that* M-cycle. The request is sampled after the idle tick, so a source that fires during it
/// is seen; sampling before the tick costs the ROM a NOP of timing.
#[test]
fn halt_wakes_on_the_m_cycle_its_flag_appears() {
    let mut m = harness(&[]);
    m.bus.ie = IF_VBLANK;
    m.cpu.ime = false; // IME=0: the wake resumes at the instruction after HALT
    m.cpu.halted = true;
    // The PPU is one dot away from line 144, where VBlank — and IF bit 0 — begins, so the idle
    // tick raises the flag it is tested against.
    m.bus.ppu.write(0xFF40, 0x80);
    m.bus.ppu.ly = 143;
    m.bus.ppu.dot = DOTS_PER_LINE - 1;
    m.bus.ppu.mode = Mode::HBlank;
    m.cpu.step(&mut m.bus);
    assert!(
        !m.cpu.halted,
        "HALT did not wake on the cycle IF was raised"
    );
    assert!(m.bus.iflag & IF_VBLANK != 0);
}
