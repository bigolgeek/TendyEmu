//! CPU debugger core: breakpoints and a step engine, no UI.
//!
//! Parked by decision: the cheat database is wired into the launch path
//! (`launch::rom_cheats`), leaving this core unreached from the frontend. A
//! debugger UI is a feature of its own; nothing here is wired into `main`/`launch` yet.
//!
//! Like the trace logger (`Tracer` in `src/trace.rs`), the debugger wraps stepping from the
//! outside instead of hooking the CPU core: every instrumented run routes bus accesses through
//! `Watch`, a thin `Bus` adapter that delegates each access unchanged and consults the read/write
//! breakpoints as it does. `enabled: false` short-circuits each instruction to a plain
//! `Cpu::step`, so a disabled debugger performs no breakpoint work at all.
//!
//! Stop semantics:
//! - An `Exec` breakpoint fires *before* the instruction at its address runs; the machine is left
//!   sitting on that PC and the instruction has not executed. The stop arms a one-shot resume
//!   latch, so the next `step` executes that instruction instead of stopping on it again — without
//!   it, a breakpoint inside a loop could never be stepped off. The breakpoint fires again when the
//!   program really comes back to the address.
//! - An interrupt entry counts as the instruction that runs. When a pending NMI, or an IRQ the I
//!   flag does not mask, will hijack the next `Cpu::step` (it enters the vector instead of
//!   executing the opcode at PC), the `Exec` check is skipped: nothing at PC is about to run, so
//!   the breakpoint stays silent until an `RTI` returns to it. `executed` counts the entry and
//!   `state.pc` is the handler's first instruction.
//! - A `Read`/`Write` breakpoint fires *after* the instruction that made the access completes
//!   (the access is only observed, the bus still has to answer); PC points at the next
//!   instruction. Every matching hit of that instruction is reported in `StepReport::hits`.
//! - Opcode and operand fetches are bus reads, so a read breakpoint whose range covers executed
//!   code also fires when the code byte is fetched.
//!
//! Addresses are compared raw, exactly as they appear on the bus: a breakpoint on `$2007` does not
//! fire for `$200F`/`$3FFF`, and one on `$0000` misses the `$0800` mirror. Folding mirrors needs a
//! per-region map of what the console actually mirrors; left out until something asks for it (TODO).
//! Accesses the DMA engine makes inside `SysBus` (OAM DMA page reads, DMC fetches) are invisible
//! here for the same reason the CPU cannot see them — only the `$4014`/`$4010` writes reach the bus.
//!
//! Frame-boundary debug logging is left out: the trace logger already does that better.

use crate::cpu::{Bus, Cpu};
use std::mem;

/// What a breakpoint watches: `Exec` runs at the address; `Read`/`Write` are the bus accesses of
/// the same name (including opcode/operand fetches for `Read`); `Any` matches both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Exec,
    Read,
    Write,
    Any,
}

/// One breakpoint: an inclusive address range (`lo == hi` for a single address), the access to
/// watch, an optional byte compare and a hit counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    pub lo: u16,
    pub hi: u16,
    pub access: Access,
    /// Optional byte compare: for `Exec` the opcode byte at the address must equal it; for
    /// `Read`/`Write`/`Any` the byte on the bus must equal it.
    pub value: Option<u8>,
    /// Fires since the breakpoint was added (or since `reset_hits`): one per matching access, one
    /// per landing on the PC for `Exec`.
    pub hits: u64,
}

impl Breakpoint {
    /// Single-address breakpoint.
    pub fn at(access: Access, addr: u16) -> Breakpoint {
        Breakpoint {
            lo: addr,
            hi: addr,
            access,
            value: None,
            hits: 0,
        }
    }

    /// Breakpoint over an inclusive address range.
    pub fn over(access: Access, lo: u16, hi: u16) -> Breakpoint {
        Breakpoint {
            lo,
            hi,
            access,
            value: None,
            hits: 0,
        }
    }

    /// Breakpoint with a byte compare attached.
    pub fn matching(access: Access, addr: u16, value: u8) -> Breakpoint {
        Breakpoint {
            value: Some(value),
            ..Breakpoint::at(access, addr)
        }
    }

    /// Does this breakpoint apply to an `access` of `addr`?
    fn access_matches(&self, access: Access, addr: u16) -> bool {
        (self.lo <= addr && addr <= self.hi)
            && (self.access == Access::Any || self.access == access)
    }
}

/// One read or write breakpoint firing: its index into `Debugger::breakpoints`, the address and
/// the byte on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessHit {
    pub bp: usize,
    pub addr: u16,
    pub value: u8,
}

/// Registers an `access` of `addr` with bus byte `val` against `brs`, pushing one entry into
/// `hits` for every breakpoint that fires on it.
fn log_access(brs: &[Breakpoint], hits: &mut Vec<AccessHit>, access: Access, addr: u16, val: u8) {
    for (i, bp) in brs.iter().enumerate() {
        if bp.access_matches(access, addr) && bp.value.is_none_or(|v| v == val) {
            hits.push(AccessHit {
                bp: i,
                addr,
                value: val,
            });
        }
    }
}

/// Thin `Bus` adapter that watches every delegated read and write for access breakpoints. Each of
/// the trait's hooks is forwarded unconditionally, so an instrumented run behaves exactly like a
/// plain one.
struct Watch<'a, 'b, B: Bus> {
    inner: &'a mut B,
    brs: &'b [Breakpoint],
    hits: &'b mut Vec<AccessHit>,
}

impl<B: Bus> Bus for Watch<'_, '_, B> {
    fn read(&mut self, addr: u16) -> u8 {
        let v = self.inner.read(addr);
        log_access(self.brs, self.hits, Access::Read, addr, v);
        v
    }

    fn read_unpolled(&mut self, addr: u16) -> u8 {
        let v = self.inner.read_unpolled(addr);
        log_access(self.brs, self.hits, Access::Read, addr, v);
        v
    }

    fn write(&mut self, addr: u16, val: u8) {
        log_access(self.brs, self.hits, Access::Write, addr, val);
        self.inner.write(addr, val);
    }

    fn nmi_poll(&mut self) -> bool {
        self.inner.nmi_poll()
    }

    fn irq_poll(&self) -> Option<bool> {
        self.inner.irq_poll()
    }

    fn dma_just_occurred(&self) -> bool {
        self.inner.dma_just_occurred()
    }

    fn nmi_hijack(&mut self) -> bool {
        self.inner.nmi_hijack()
    }

    fn note_code(&mut self, pc: u16) {
        self.inner.note_code(pc);
    }

    fn note_dummy(&mut self) {
        self.inner.note_dummy();
    }

    fn peek(&mut self, addr: u16) -> Option<u8> {
        self.inner.peek(addr)
    }
}

/// The minimal CPU state a step report carries: PC, the byte at PC (`None` for an I/O address,
/// per `Bus::peek`), A/X/Y/SP/P, the cycle counter and the interrupt state the UI needs to explain
/// what the next step will do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuState {
    pub pc: u16,
    pub opcode: Option<u8>,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub p: u8,
    /// Total CPU cycles since power-on.
    pub cycles: u64,
    /// An NMI edge is latched and the next step enters `$FFFA` instead of PC.
    pub nmi_pending: bool,
    /// Level of the IRQ line as of the last poll.
    pub irq_line: bool,
    /// Vector the next step enters instead of executing the instruction at PC (`$FFFA` for NMI,
    /// `$FFFE` for an unmasked IRQ), or `None` when PC runs. `Cpu::next_interrupt_vector` is the
    /// same decision `Cpu::step` makes, so this is not a guess.
    pub next_interrupt: Option<u16>,
}

impl CpuState {
    fn take(cpu: &Cpu, bus: &mut impl Bus) -> CpuState {
        CpuState {
            pc: cpu.pc,
            opcode: bus.peek(cpu.pc),
            a: cpu.a,
            x: cpu.x,
            y: cpu.y,
            sp: cpu.sp,
            p: cpu.p,
            cycles: cpu.cycles,
            nmi_pending: cpu.nmi_pending,
            irq_line: cpu.irq_line,
            next_interrupt: cpu.next_interrupt_vector(),
        }
    }
}

/// Why a `step` returned early, naming the breakpoint that did it (its index in
/// `Debugger::breakpoints`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// An `Exec` breakpoint stopped the run before the instruction at PC.
    Exec(usize),
    /// A `Read`/`Write` breakpoint fired during the instruction that has now completed.
    Access(usize),
}

/// Result of `Debugger::step(cpu, bus, n)`.
#[derive(Clone, Debug)]
pub struct StepReport {
    /// Instructions run before returning; `n` unless a breakpoint stopped the run earlier. An
    /// interrupt entry counts as one instruction.
    pub executed: u32,
    pub stop: Option<Stop>,
    /// Every read/write breakpoint that fired during the instruction that ended the run, in
    /// `breakpoints` order: `Stop::Access` names only the first one, this keeps the address and
    /// byte of each (a range or `Any` breakpoint can match several in the same instruction).
    /// Empty unless `stop` is `Stop::Access`.
    pub hits: Vec<AccessHit>,
    /// Where the machine stands (on an `Exec` stop, before the instruction at PC).
    pub state: CpuState,
}

/// The breakpoint list, the enable flag and the resume latch that lets a step leave an `Exec` stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Debugger {
    pub enabled: bool,
    pub breakpoints: Vec<Breakpoint>,
    /// PC the last `Exec` stop parked on and that the next `step` must run *through* instead of
    /// re-checking the breakpoints there. One shot: `step` clears it after that instruction, and
    /// `add_breakpoint`/`add_breakpoint_range`/`clear_breakpoints` clear it, so a changed breakpoint
    /// list stops again (push into `breakpoints` directly and it does not — use the methods).
    resume_pc: Option<u16>,
}

impl Debugger {
    pub fn new(enabled: bool) -> Debugger {
        Debugger {
            enabled,
            breakpoints: Vec::new(),
            resume_pc: None,
        }
    }

    /// Adds a single-address breakpoint; returns its index in `breakpoints`.
    pub fn add_breakpoint(&mut self, addr: u16, access: Access) -> usize {
        self.breakpoints.push(Breakpoint::at(access, addr));
        self.resume_pc = None;
        self.breakpoints.len() - 1
    }

    /// Adds a breakpoint over an inclusive address range; returns its index in `breakpoints`.
    pub fn add_breakpoint_range(&mut self, lo: u16, hi: u16, access: Access) -> usize {
        self.breakpoints.push(Breakpoint::over(access, lo, hi));
        self.resume_pc = None;
        self.breakpoints.len() - 1
    }

    /// Drops every breakpoint.
    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
        self.resume_pc = None;
    }

    /// Zeroes every hit counter.
    pub fn reset_hits(&mut self) {
        for bp in &mut self.breakpoints {
            bp.hits = 0;
        }
    }

    /// Runs up to `n` instructions, stopping early when a breakpoint fires (module docs above
    /// give the exact stop semantics). An interrupt entry counts as an instruction: a pending NMI
    /// or unmasked IRQ that preempts PC is run as one step, and an `Exec` breakpoint sitting on PC
    /// does not fire for it — the breakpointed instruction itself has not run.
    pub fn step(&mut self, cpu: &mut Cpu, bus: &mut impl Bus, n: u32) -> StepReport {
        let mut executed = 0;
        let mut stop = None;
        let mut stop_hits = Vec::new();
        if !self.enabled {
            // Zero-overhead path: no breakpoint checks, no adapter, just the plain CPU step. The
            // resume latch dies here too, so re-enabling cannot skip an exec check.
            self.resume_pc = None;
            while executed < n {
                cpu.step(bus);
                executed += 1;
            }
        } else {
            while executed < n {
                // A previous call stopped on this PC and the caller asked to proceed, so this
                // instruction runs without re-checking the exec breakpoints. The latch is
                // one-shot (cleared here, re-armed by the next stop), which is what lets a loop
                // back to the same address fire the breakpoint again.
                let resuming = self.resume_pc == Some(cpu.pc);
                self.resume_pc = None;
                let stop_at = if resuming {
                    None
                } else {
                    self.exec_stop(cpu, bus)
                };
                if let Some(i) = stop_at {
                    self.breakpoints[i].hits += 1;
                    stop = Some(Stop::Exec(i));
                    self.resume_pc = Some(cpu.pc);
                    // The breakpoint instruction has not run, so it is not counted.
                    break;
                }
                if self.breakpoints.iter().any(|bp| bp.access != Access::Exec) {
                    let mut brs = mem::take(&mut self.breakpoints);
                    let mut hits = Vec::new();
                    let mut watch = Watch {
                        inner: bus,
                        brs: &brs,
                        hits: &mut hits,
                    };
                    cpu.step(&mut watch);
                    for hit in &hits {
                        brs[hit.bp].hits += 1;
                    }
                    self.breakpoints = brs;
                    executed += 1;
                    if let Some(first) = hits.first() {
                        // The instruction that made the access has completed; count it, then stop.
                        stop = Some(Stop::Access(first.bp));
                        stop_hits = hits;
                        break;
                    }
                } else {
                    // Exec breakpoints only: no access breakpoint to watch.
                    cpu.step(bus);
                    executed += 1;
                }
            }
        }
        StepReport {
            executed,
            stop,
            hits: stop_hits,
            state: CpuState::take(cpu, bus),
        }
    }

    /// First `Exec` breakpoint whose range covers PC and whose byte compare (the opcode byte at
    /// PC) matches, if any. `None` when the next `step` will enter an interrupt vector instead of
    /// executing at PC: the breakpoint watches instruction execution, and an entry is not it.
    fn exec_stop(&self, cpu: &Cpu, bus: &mut impl Bus) -> Option<usize> {
        if cpu.next_interrupt_vector().is_some() {
            return None;
        }
        let mut opcode_at_pc: Option<Option<u8>> = None; // fetched from the bus at most once
        self.breakpoints.iter().enumerate().find_map(|(i, bp)| {
            if bp.access != Access::Exec || cpu.pc < bp.lo || cpu.pc > bp.hi {
                return None;
            }
            if let Some(v) = bp.value {
                if opcode_at_pc.is_none() {
                    opcode_at_pc = Some(bus.peek(cpu.pc));
                }
                if opcode_at_pc != Some(Some(v)) {
                    return None;
                }
            }
            Some(i)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{FLAG_I, TestBus};

    const START: u16 = 0x0400;

    /// A machine whose program sits at `START` and whose reset vector points there.
    fn machine(code: &[u8]) -> (Cpu, TestBus) {
        let mut bus = TestBus::new();
        for (i, b) in code.iter().enumerate() {
            bus.mem[START as usize + i] = *b;
        }
        bus.mem[0xFFFC] = START as u8;
        bus.mem[0xFFFD] = (START >> 8) as u8;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        (cpu, bus)
    }

    /// Self-jump bytes (`JMP $target`). `at` must equal START + code length, i.e. the position
    /// the jump occupies, so the program ends in an endless loop *after* its instructions.
    fn self_loop(at: u16) -> [u8; 3] {
        [0x4C, at as u8, (at >> 8) as u8]
    }

    /// LDA #$01 / LDX #$02 / LDY #$03 / NOP / LDA $3000; pair with `self_loop(START + len)`.
    fn mix_code() -> Vec<u8> {
        vec![0xA9, 0x01, 0xA2, 0x02, 0xA0, 0x03, 0xEA, 0xAD, 0x00, 0x30]
    }

    #[test]
    fn exec_breakpoint_stops_before_the_instruction() {
        // LDA #$01 / LDA #$02 / JMP $0402 (the second LDA).
        let code = vec![0xA9, 0x01, 0xA9, 0x02, 0x4C, 0x02, 0x04];
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(0x0402, Access::Exec);

        let r = dbg.step(&mut cpu, &mut bus, 10);

        // Only the first LDA ran; the machine is parked on the breakpoint PC, which did not
        // execute.
        assert_eq!(r.executed, 1);
        assert_eq!(r.stop, Some(Stop::Exec(i)));
        assert_eq!(r.state.pc, 0x0402);
        assert_eq!(r.state.opcode, Some(0xA9));
        assert_eq!(r.state.a, 0x01);
        assert_eq!(r.state.p, 0x24);
        assert_eq!(cpu.pc, 0x0402);
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn read_breakpoint_fires_on_that_2007_address_only() {
        // LDA $2002 / LDA $2007 / STA $2008 / self-jump.
        let mut code = vec![
            0xAD, 0x02, 0x20, // LDA $2002
            0xAD, 0x07, 0x20, // LDA $2007
            0x8D, 0x08, 0x20, // STA $2008
        ];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(0x2007, Access::Read);

        let r = dbg.step(&mut cpu, &mut bus, 10);

        // The $2002 read did not fire it; the $2007 read did, and the instruction that made the
        // access completed before the stop (PC is at the $2008 store).
        assert_eq!(r.executed, 2);
        assert_eq!(r.stop, Some(Stop::Access(i)));
        assert_eq!(r.state.pc, 0x0406);
        assert_eq!(dbg.breakpoints[i].hits, 1);

        // The $2007 read also answered the CPU: the $2008 store runs, then the self-jump, and
        // nothing re-fires the breakpoint.
        let r2 = dbg.step(&mut cpu, &mut bus, 20);
        assert_eq!(r2.executed, 20);
        assert!(r2.stop.is_none());
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn condition_compare_fires_only_when_the_byte_matches() {
        // LDA $3000 (reads 0x55) / LDA #$12 / STA $3000 / LDA $3000 (reads 0x12) / self-jump.
        let mut code = vec![
            0xAD, 0x00, 0x30, // LDA $3000
            0xA9, 0x12, // LDA #$12
            0x8D, 0x00, 0x30, // STA $3000
            0xAD, 0x00, 0x30, // LDA $3000
        ];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        bus.mem[0x3000] = 0x55;
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(0x3000, Access::Read);
        dbg.breakpoints[i].value = Some(0x12);

        let r = dbg.step(&mut cpu, &mut bus, 10);

        // The 0x55 read and the store to $3000 (a write) did not fire it; the 0x12 read did.
        assert_eq!(r.executed, 4);
        assert_eq!(r.stop, Some(Stop::Access(i)));
        assert_eq!(cpu.a, 0x12);
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn write_breakpoint_fires_on_that_address_with_the_written_byte() {
        // STA $4001 (0x00) / LDA #$BB / STA $4000 (0xBB) / self-jump.
        let mut code = vec![
            0xA9, 0x00, // LDA #$00
            0x8D, 0x01, 0x40, // STA $4001
            0xA9, 0xBB, // LDA #$BB
            0x8D, 0x00, 0x40, // STA $4000
        ];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(0x4000, Access::Write);
        dbg.breakpoints[i].value = Some(0xBB);

        let r = dbg.step(&mut cpu, &mut bus, 10);

        // The $4001 store did not fire it; the $4000 store of 0xBB did.
        assert_eq!(r.executed, 4);
        assert_eq!(r.stop, Some(Stop::Access(i)));
        assert_eq!(dbg.breakpoints[i].hits, 1);

        // Nothing in the self-jump writes $4000: after the reset_hits, the run stays clean.
        dbg.reset_hits();
        let r2 = dbg.step(&mut cpu, &mut bus, 10);
        assert_eq!(r2.executed, 10);
        assert!(r2.stop.is_none());
        assert_eq!(dbg.breakpoints[i].hits, 0);
    }

    #[test]
    fn step_n_advances_n_instructions_and_reports_state() {
        let mut code = mix_code();
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);

        let r = dbg.step(&mut cpu, &mut bus, 3);

        assert_eq!(r.executed, 3);
        assert!(r.stop.is_none());
        assert_eq!(cpu.pc, 0x0406);
        assert_eq!((cpu.a, cpu.x, cpu.y), (0x01, 0x02, 0x03));

        // The report mirrors the CPU's registers, with the byte at PC peeked side-effect-free.
        assert_eq!(r.state.pc, cpu.pc);
        assert_eq!(r.state.opcode, bus.peek(cpu.pc));
        assert_eq!(r.state.a, 0x01);
        assert_eq!(r.state.x, 0x02);
        assert_eq!(r.state.y, 0x03);
        assert_eq!(r.state.sp, cpu.sp);
        assert_eq!(r.state.p, cpu.p);

        // Counting goes on from where the last `step` left off.
        let r2 = dbg.step(&mut cpu, &mut bus, 2);
        assert_eq!(r2.executed, 2);
        assert_eq!(cpu.a, 0x00); // LDA $3000 loaded the zeroed RAM byte
    }

    #[test]
    fn disabled_debugger_is_zero_overhead() {
        // 50 plain steps against 50 steps through a *disabled* debugger that even has a
        // breakpoint parked in its list: same machine, identical end state and cycles.
        let mut code = mix_code();
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut base_cpu, mut base_bus) = machine(&code);
        let (mut dbg_cpu, mut dbg_bus) = machine(&code);

        for _ in 0..50 {
            base_cpu.step(&mut base_bus);
        }

        let mut dbg = Debugger::new(false);
        dbg.add_breakpoint(START, Access::Exec);

        let r = dbg.step(&mut dbg_cpu, &mut dbg_bus, 50);

        assert_eq!(r.executed, 50);
        assert!(r.stop.is_none());
        assert_eq!(dbg.breakpoints[0].hits, 0);
        assert_eq!(dbg_cpu.a, base_cpu.a);
        assert_eq!(dbg_cpu.x, base_cpu.x);
        assert_eq!(dbg_cpu.y, base_cpu.y);
        assert_eq!(dbg_cpu.sp, base_cpu.sp);
        assert_eq!(dbg_cpu.p, base_cpu.p);
        assert_eq!(dbg_cpu.pc, base_cpu.pc);
        assert_eq!(dbg_cpu.cycles, base_cpu.cycles);
    }

    #[test]
    fn enabled_with_no_breakpoints_changes_nothing() {
        let mut code = mix_code();
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut base_cpu, mut base_bus) = machine(&code);
        let (mut dbg_cpu, mut dbg_bus) = machine(&code);

        for _ in 0..50 {
            base_cpu.step(&mut base_bus);
        }

        let mut dbg = Debugger::new(true);
        let r = dbg.step(&mut dbg_cpu, &mut dbg_bus, 50);

        assert_eq!(r.executed, 50);
        assert!(r.stop.is_none());
        assert_eq!(dbg_cpu.cycles, base_cpu.cycles);
        assert_eq!(
            (
                dbg_cpu.a, dbg_cpu.x, dbg_cpu.y, dbg_cpu.sp, dbg_cpu.p, dbg_cpu.pc
            ),
            (
                base_cpu.a,
                base_cpu.x,
                base_cpu.y,
                base_cpu.sp,
                base_cpu.p,
                base_cpu.pc
            )
        );
    }

    #[test]
    fn read_breakpoint_fires_on_the_opcode_fetch_too() {
        // NOP / self-jump.
        let mut code = vec![0xEA];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(0x0400, Access::Read);

        // Fetching the NOP's opcode is a bus read, so a read breakpoint on a code address fires
        // the same way a data read does.
        let r = dbg.step(&mut cpu, &mut bus, 10);
        assert_eq!(r.executed, 1);
        assert_eq!(r.stop, Some(Stop::Access(i)));
        assert_eq!(dbg.breakpoints[i].hits, 1);
        assert_eq!(cpu.pc, 0x0401);

        // The self-jump then keeps re-reading $0405 (its own opcode byte), never $0400.
        let r2 = dbg.step(&mut cpu, &mut bus, 10);
        assert_eq!(r2.executed, 10);
        assert!(r2.stop.is_none());
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn clear_and_range_breakpoints_behave() {
        // Just the self-jump itself (JMP $0400 at $0400): an exec-range breakpoint covering the
        // code stops the run on first landing.
        let (mut cpu, mut bus) = machine(&self_loop(START));
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint_range(0x0400, 0x0407, Access::Exec);
        let r = dbg.step(&mut cpu, &mut bus, 5);
        // The very first check already lands: the JMP loop's own PC is in the range, so the run
        // stops before executing anything.
        assert_eq!(r.executed, 0);
        assert_eq!(r.stop, Some(Stop::Exec(i)));
        assert_eq!(r.state.pc, START);
        assert_eq!(dbg.breakpoints[i].hits, 1);

        dbg.clear_breakpoints();
        assert!(dbg.breakpoints.is_empty());
        let r2 = dbg.step(&mut cpu, &mut bus, 5);
        assert_eq!(r2.executed, 5);
        assert!(r2.stop.is_none());
    }

    #[test]
    fn exec_breakpoint_with_condition_compares_the_opcode() {
        // NOP / self-jump.
        let mut code = vec![0xEA];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        // Exec breakpoint on the self-jump, matching the JMP opcode byte.
        let i = dbg.add_breakpoint(0x0401, Access::Exec);
        dbg.breakpoints[i].value = Some(0x4C);

        let r = dbg.step(&mut cpu, &mut bus, 3);
        // The NOP ran, then the machine parked on the self-jump whose opcode matched.
        assert_eq!(r.executed, 1);
        assert_eq!(r.stop, Some(Stop::Exec(i)));
        assert_eq!(r.state.pc, 0x0401);
        assert_eq!(dbg.breakpoints[i].hits, 1);

        // A mismatching compare (0xAD) keeps the self-jump loop going all five steps.
        let mut dbg2 = Debugger::new(true);
        let j = dbg2.add_breakpoint(0x0401, Access::Exec);
        dbg2.breakpoints[j].value = Some(0xAD);
        let r2 = dbg2.step(&mut cpu, &mut bus, 5);
        assert_eq!(r2.executed, 5);
        assert!(r2.stop.is_none());
        assert_eq!(dbg2.breakpoints[j].hits, 0);
    }

    #[test]
    fn exec_breakpoint_can_be_stepped_past_and_re_fires_on_the_next_lap() {
        // INX / JMP $0400: the loop's first instruction carries an exec breakpoint, so a step
        // engine that re-checks it at the top of every call can never execute anything.
        let code = vec![0xE8, 0x4C, 0x00, 0x04];
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(START, Access::Exec);

        let r = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r.executed, 0);
        assert_eq!(r.stop, Some(Stop::Exec(i)));
        assert_eq!(r.state.pc, START);
        assert_eq!(dbg.breakpoints[i].hits, 1);
        assert_eq!(cpu.x, 0);

        // Proceeding from the stop executes the instruction at the breakpoint PC instead of
        // stopping on it again.
        let r2 = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r2.executed, 1);
        assert!(r2.stop.is_none());
        assert_eq!(cpu.x, 1);
        assert_eq!(cpu.pc, START + 1);
        assert_eq!(dbg.breakpoints[i].hits, 1);

        // The JMP closes the loop, so the breakpoint fires again on the next lap — once per real
        // landing, not once per `step` call.
        let r3 = dbg.step(&mut cpu, &mut bus, 10);
        assert_eq!(r3.executed, 1);
        assert_eq!(r3.stop, Some(Stop::Exec(i)));
        assert_eq!(r3.state.pc, START);
        assert_eq!(dbg.breakpoints[i].hits, 2);
        assert_eq!(cpu.x, 1);

        // And it leaves the machine parked the same way, so the loop keeps stepping.
        let r4 = dbg.step(&mut cpu, &mut bus, 2);
        assert_eq!(r4.executed, 2);
        assert!(r4.stop.is_none());
        assert_eq!(cpu.x, 2);
    }

    #[test]
    fn exec_breakpoint_does_not_fire_when_a_pending_nmi_is_entered_instead() {
        // JMP $0400 at $0400, with the NMI vector pointing at an RTI at $0500.
        let (mut cpu, mut bus) = machine(&self_loop(START));
        bus.mem[0xFFFA] = 0x00;
        bus.mem[0xFFFB] = 0x05;
        bus.mem[0x0500] = 0x40; // RTI
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(START, Access::Exec);
        cpu.nmi_pending = true;

        // The pending NMI is entered instead of the instruction at the breakpoint PC, so the
        // breakpoint must not fire; the entry counts as the executed instruction.
        let r = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r.executed, 1);
        assert!(r.stop.is_none());
        assert_eq!(dbg.breakpoints[i].hits, 0);
        assert_eq!(cpu.pc, 0x0500);
        assert_eq!(r.state.pc, 0x0500);
        assert!(!cpu.nmi_pending);

        // RTI returns to $0400.
        let r2 = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r2.executed, 1);
        assert!(r2.stop.is_none());
        assert_eq!(cpu.pc, START);

        // Now the breakpointed JMP really is about to run, so the breakpoint fires — exactly once
        // for one real execution, not twice (before and after the interrupt).
        let r3 = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r3.executed, 0);
        assert_eq!(r3.stop, Some(Stop::Exec(i)));
        assert_eq!(r3.state.pc, START);
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn exec_breakpoint_does_not_fire_when_a_pending_irq_is_entered_instead() {
        // JMP $0400 at $0400, with the IRQ vector pointing at an RTI at $0600.
        let (mut cpu, mut bus) = machine(&self_loop(START));
        bus.mem[0xFFFE] = 0x00;
        bus.mem[0xFFFF] = 0x06;
        bus.mem[0x0600] = 0x40; // RTI
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(START, Access::Exec);
        cpu.set_flag(FLAG_I, false);
        cpu.irq_line = true;

        // An unmasked IRQ line hijacks the next step the same way: the entry runs, the
        // breakpoint stays silent, and the entry is what the report accounts for.
        let r = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r.executed, 1);
        assert!(r.stop.is_none());
        assert_eq!(dbg.breakpoints[i].hits, 0);
        assert_eq!(cpu.pc, 0x0600);
        assert_eq!(r.state.pc, 0x0600);

        // A masked IRQ line does not hijack, so the breakpoint fires normally.
        cpu.irq_line = false;
        cpu.set_flag(FLAG_I, true);
        let r2 = dbg.step(&mut cpu, &mut bus, 20);
        assert_eq!(r2.executed, 1);
        assert_eq!(r2.stop, Some(Stop::Exec(i)));
        assert_eq!(dbg.breakpoints[i].hits, 1);
    }

    #[test]
    fn access_stop_keeps_every_hit_with_its_address_and_value() {
        // LDA $2007 (reads 0x5A) / self-jump, with a single-address and a range read breakpoint
        // both covering the load.
        let mut code = vec![0xAD, 0x07, 0x20];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        bus.mem[0x2007] = 0x5A;
        let mut dbg = Debugger::new(true);
        let a = dbg.add_breakpoint(0x2007, Access::Read);
        let b = dbg.add_breakpoint_range(0x2000, 0x2007, Access::Read);

        let r = dbg.step(&mut cpu, &mut bus, 10);

        // The stop names the first breakpoint, and the report keeps both hits (address and byte),
        // so a UI can say what actually fired.
        assert_eq!(r.stop, Some(Stop::Access(a)));
        assert_eq!(
            r.hits,
            vec![
                AccessHit {
                    bp: a,
                    addr: 0x2007,
                    value: 0x5A
                },
                AccessHit {
                    bp: b,
                    addr: 0x2007,
                    value: 0x5A
                },
            ]
        );
        assert_eq!(dbg.breakpoints[a].hits, 1);
        assert_eq!(dbg.breakpoints[b].hits, 1);

        // An exec stop reports no access hits.
        let (mut cpu2, mut bus2) = machine(&self_loop(START));
        let mut dbg2 = Debugger::new(true);
        dbg2.add_breakpoint(START, Access::Exec);
        let r2 = dbg2.step(&mut cpu2, &mut bus2, 1);
        assert!(r2.stop.is_some());
        assert!(r2.hits.is_empty());
    }

    #[test]
    fn disabling_the_debugger_drops_a_pending_resume_latch() {
        // INX / JMP $0400 with an exec breakpoint at $0400: stop on it, run with the debugger
        // disabled, then re-enable. The stale latch must not swallow the next landing.
        let code = vec![0xE8, 0x4C, 0x00, 0x04];
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);
        let i = dbg.add_breakpoint(START, Access::Exec);

        let r = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r.stop, Some(Stop::Exec(i)));

        dbg.enabled = false;
        let r2 = dbg.step(&mut cpu, &mut bus, 2);
        assert_eq!(r2.executed, 2);
        assert_eq!(cpu.pc, START);

        dbg.enabled = true;
        let r3 = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r3.executed, 0);
        assert_eq!(r3.stop, Some(Stop::Exec(i)));
        assert_eq!(dbg.breakpoints[i].hits, 2);
    }

    #[test]
    fn report_state_carries_cycles_and_the_next_interrupt_vector() {
        let mut code = vec![0xEA];
        code.extend_from_slice(&self_loop(START + code.len() as u16));
        let (mut cpu, mut bus) = machine(&code);
        let mut dbg = Debugger::new(true);

        let r = dbg.step(&mut cpu, &mut bus, 1);
        assert_eq!(r.state.cycles, cpu.cycles);
        assert!(!r.state.nmi_pending);
        assert!(!r.state.irq_line);
        assert_eq!(r.state.next_interrupt, None);

        // The field is the same decision `Cpu::step` takes: an unmasked IRQ line preempts PC, a
        // masked one does not.
        cpu.irq_line = true;
        cpu.set_flag(FLAG_I, false);
        assert_eq!(CpuState::take(&cpu, &mut bus).next_interrupt, Some(0xFFFE));
        cpu.set_flag(FLAG_I, true);
        assert_eq!(CpuState::take(&cpu, &mut bus).next_interrupt, None);
        cpu.nmi_pending = true;
        assert_eq!(CpuState::take(&cpu, &mut bus).next_interrupt, Some(0xFFFA));
    }
}
