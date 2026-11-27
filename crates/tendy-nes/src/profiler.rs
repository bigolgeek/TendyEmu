//! A call-stack profiler: runs a `Nes` one instruction at a time and attributes every CPU cycle
//! to the routine that was executing it.
//!
//! Like the trace logger (`Tracer` in `src/trace.rs`) it wraps stepping and never touches emulation
//! state, so a profiled run is cycle-identical to an unprofiled one. Its one read per instruction
//! (the opcode at PC) goes through `SysBus::peek`, the tracer's path: it sees the byte the CPU runs,
//! Game Genie substitution included, and takes no cartridge read side effect.
//!
//! # Model
//!
//! A routine is keyed by its entry address. The call stack follows the instruction stream:
//!
//! * `JSR` (`$20`) pushes its target. The `JSR` itself is charged to the caller.
//! * `RTS` (`$60`) and `RTI` (`$40`) pop. The return instruction is charged to the routine it leaves.
//! * An interrupt entry (NMI via `$FFFA`, IRQ via `$FFFE`, including a hijacked vector) pushes a
//!   pseudo-routine keyed by the handler address the CPU lands on; the entry's own cycles are
//!   charged to it.
//! * The routine running when profiling starts is the root frame (keyed by that PC). It is never
//!   popped, so an unbalanced `RTS` cannot empty the stack.
//!
//! *Exclusive* cycles are those spent in a routine's own instructions; *inclusive* cycles also
//! include everything it called. A routine that appears several times on the stack (recursion) is
//! charged inclusive time once per instruction, so `inclusive <= total` always holds.
//!
//! # Known limits
//!
//! * The key is the **CPU address, not the bank**: two switchable-bank routines that share an entry
//!   address are reported as one.
//! * The stack is inferred from opcodes, not read from RAM. Code that fiddles with the hardware
//!   stack (pushing a return address and `RTS`ing to it, or discarding return addresses with
//!   `PLA`) or `JMP`s out of a routine leaves the profiler's stack out of step with the program's.
//!   Depth is capped at [`MAX_DEPTH`]; past it the oldest frame is dropped.
use crate::cpu::Bus;
use crate::nes::Nes;
use crate::symbols::Symbols;
use std::collections::{HashMap, HashSet};

const JSR: u8 = 0x20;
const RTS: u8 = 0x60;
const RTI: u8 = 0x40;

/// Deepest call stack tracked before the oldest frame is discarded.
pub const MAX_DEPTH: usize = 256;

/// Totals for one routine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoutineStats {
    /// Times the routine was entered (the root frame counts as one entry).
    pub calls: u64,
    /// Cycles spent in the routine's own instructions.
    pub exclusive: u64,
    /// Cycles spent in the routine and everything it called.
    pub inclusive: u64,
}

/// Cycle profiler. Drive it with [`Profiler::step`] / [`Profiler::run_frame`] instead of
/// `Nes::step` / `Nes::run_frame`.
#[derive(Clone, Debug, Default)]
pub struct Profiler {
    /// Per-routine totals keyed by entry address.
    pub routines: HashMap<u16, RoutineStats>,
    /// Every cycle returned by `step` since creation.
    pub total_cycles: u64,
    stack: Vec<u16>,
    /// Scratch set for [`Profiler::charge`], kept so a charge does not allocate.
    seen: HashSet<u16>,
}

impl Profiler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current call depth (the root frame counts); 0 before the first step.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Run one instruction (or interrupt entry) on `nes`, attribute its cycles, and return them.
    pub fn step(&mut self, nes: &mut Nes) -> u32 {
        if self.stack.is_empty() {
            self.push(nes.cpu.pc);
        }
        let interrupt = nes.cpu.next_interrupt_vector().is_some();
        let opcode = if interrupt {
            None
        } else {
            nes.bus.peek(nes.cpu.pc)
        };
        let cycles = nes.step();
        if interrupt {
            self.push(nes.cpu.pc);
        }
        self.charge(u64::from(cycles));
        match opcode {
            Some(JSR) => self.push(nes.cpu.pc),
            Some(RTS) | Some(RTI) if self.stack.len() > 1 => {
                self.stack.pop();
            }
            _ => {}
        }
        cycles
    }

    /// Run until the next vblank, like `Nes::run_frame`, profiling every instruction.
    pub fn run_frame(&mut self, nes: &mut Nes) {
        nes.bus.ppu.frame_ready = false;
        while !nes.bus.ppu.frame_ready {
            self.step(nes);
        }
    }

    fn push(&mut self, key: u16) {
        if self.stack.len() >= MAX_DEPTH {
            self.stack.remove(0);
        }
        self.stack.push(key);
        self.routines.entry(key).or_default().calls += 1;
    }

    fn charge(&mut self, cycles: u64) {
        self.total_cycles += cycles;
        let top = *self.stack.last().expect("charge follows a push");
        self.routines.entry(top).or_default().exclusive += cycles;
        self.seen.clear();
        for key in &self.stack {
            // A recursive routine is charged once, at its first (outermost) stack slot.
            if self.seen.insert(*key) {
                self.routines.entry(*key).or_default().inclusive += cycles;
            }
        }
    }

    /// Routines as `(entry, stats)`, most exclusive cycles first (ties by address).
    pub fn sorted(&self) -> Vec<(u16, RoutineStats)> {
        let mut v: Vec<(u16, RoutineStats)> = self.routines.iter().map(|(a, s)| (*a, *s)).collect();
        v.sort_by(|a, b| b.1.exclusive.cmp(&a.1.exclusive).then(a.0.cmp(&b.0)));
        v
    }

    /// A text table sorted by exclusive cycles. Routines with a symbol are named by it, others
    /// by `$ADDR`. The key is the CPU address, not the bank (see the module docs).
    pub fn report(&self, symbols: &Symbols) -> String {
        let total = self.total_cycles.max(1);
        let mut out = format!(
            "{:<24} {:>5} {:>10} {:>6} {:>10} {:>6} {:>8}\n",
            "routine", "addr", "excl", "excl%", "incl", "incl%", "calls"
        );
        for (addr, s) in self.sorted() {
            let name = symbols
                .name_at(addr)
                .map_or_else(|| format!("${addr:04X}"), str::to_string);
            out.push_str(&format!(
                "{:<24} ${:04X} {:>10} {:>5.1}% {:>10} {:>5.1}% {:>8}\n",
                name,
                addr,
                s.exclusive,
                s.exclusive as f64 * 100.0 / total as f64,
                s.inclusive,
                s.inclusive as f64 * 100.0 / total as f64,
                s.calls
            ));
        }
        out.push_str(&format!("total cycles: {}\n", self.total_cycles));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The charge loop before the fix, kept as the reference: a `contains` over the slots below
    /// each slot, so O(depth^2) per instruction.
    fn reference_inclusive(stack: &[u16], cycles: u64, routines: &mut HashMap<u16, RoutineStats>) {
        for (i, key) in stack.iter().enumerate() {
            if !stack[..i].contains(key) {
                routines.entry(*key).or_default().inclusive += cycles;
            }
        }
    }

    #[test]
    fn charge_matches_the_quadratic_reference_on_a_deep_recursive_stack() {
        let mut profiler = Profiler::new();
        // Full depth with recursion (keys repeat), so the once-per-routine rule is exercised.
        profiler.stack = (0..MAX_DEPTH)
            .map(|i| 0x8000 + (i % 37) as u16 * 3)
            .collect();
        let mut reference = HashMap::new();
        for cycles in [7u32, 2, 11] {
            profiler.charge(u64::from(cycles));
            reference_inclusive(&profiler.stack, u64::from(cycles), &mut reference);
        }
        let keys: HashSet<u16> = profiler
            .routines
            .keys()
            .chain(reference.keys())
            .copied()
            .collect();
        assert_eq!(keys.len(), 37, "every recursive routine is charged");
        for key in keys {
            let got = profiler.routines.get(&key).map_or(0, |s| s.inclusive);
            let want = reference.get(&key).map_or(0, |s| s.inclusive);
            assert_eq!(got, want, "inclusive for ${key:04X}");
        }
        assert_eq!(profiler.total_cycles, 20);
    }
}
