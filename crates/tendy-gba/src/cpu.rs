//! The ARM7TDMI: both instruction sets, the banked registers, the pipeline and the exceptions.
//!
//! It replaces an earlier stub that only proved the bus carried instructions. The behaviour comes
//! from two documents only — GBATEK's *ARM CPU*
//! chapter and ARM's *ARM7TDMI-S Data Sheet* (ARM DDI 0084D, quoted by section in `arm.rs` and
//! `thumb.rs`) — and the public suites in `roms/test/gba/` are the gate.
//!
//! **The pipeline.** `r[15]` holds the address of the *next* instruction, already fetched; an
//! instruction that reads `R15` sees its own address plus eight in ARM state and plus four in
//! Thumb ([`Cpu::pc_read`]). Three things read one fetch later still, at +12 in ARM state
//! ([`Cpu::pc_read_late`]): both operands of a data-processing instruction whose second operand is
//! shifted *by a register* (4.5.5), the source register of `STR`/`STRH` (4.9.4, 4.10.4), and `R15`
//! in the list of an `STM` (4.11.1: "Whenever R15 is stored to memory the stored value is the
//! address of the STM instruction plus 12"). Thumb reads `R15` at +4 everywhere and stores it at
//! +6, which is the same rule written for a two-byte pipeline.
//!
//! **Banking.** The live `R13`/`R14` (and, in FIQ, `R8`-`R12`) sit in `r[]`; six register banks hold
//! the copies that are not live, and a mode change swaps them in and out — [`Cpu::switch_banks`].
//! That happens on `MSR` to the CPSR's control field exactly as on an exception, which is what makes
//! `MSR CPSR_c, MODE_FIQ` a real bank change and not a change of label (test 253 of `arm.gba`
//! depends on it).
//!
//! **Cycles.** GBATEK's instruction summary gives the ARM7TDMI's own pattern: one `S` cycle for a
//! plain instruction, one more for a register-specified shift or a write to `R15`, one `N` per extra
//! memory access, and a multiply's `m` internal cycles. `S`, `N` and `I` are each one cycle here,
//! because the GBA's wait states and prefetch buffer are G4's: this is the core's own timing with an
//! infinitely fast memory behind it.
//!
//! **State** is plain fields, each documented with what it holds, because G7 has to serialise all of
//! it into a save state.

use crate::bus::Bus;

/// `SP_svc` after the boot sequence (GBATEK, *Default memory usage at 03007FXX*: "SP_svc=03007FE0h").
pub const SP_SVC: u32 = 0x0300_7FE0;
/// `SP_irq` after the boot sequence: "SP_irq=03007FA0h".
pub const SP_IRQ: u32 = 0x0300_7FA0;
/// `SP_usr` (and `SP_sys`) after the boot sequence: "SP_usr=03007F00h".
pub const SP_USR: u32 = 0x0300_7F00;

/// CPSR mode field: User.
pub const MODE_USR: u32 = 0x10;
/// CPSR mode field: Fast Interrupt.
pub const MODE_FIQ: u32 = 0x11;
/// CPSR mode field: Interrupt.
pub const MODE_IRQ: u32 = 0x12;
/// CPSR mode field: Supervisor.
pub const MODE_SVC: u32 = 0x13;
/// CPSR mode field: Abort.
pub const MODE_ABT: u32 = 0x17;
/// CPSR mode field: Undefined.
pub const MODE_UND: u32 = 0x1B;
/// CPSR mode field: System.
pub const MODE_SYS: u32 = 0x1F;
/// The mode field of the CPSR.
pub const MODE_MASK: u32 = 0x1F;

/// CPSR bit 31, Negative.
pub const FLAG_N: u32 = 1 << 31;
/// CPSR bit 30, Zero.
pub const FLAG_Z: u32 = 1 << 30;
/// CPSR bit 29, Carry.
pub const FLAG_C: u32 = 1 << 29;
/// CPSR bit 28, Overflow.
pub const FLAG_V: u32 = 1 << 28;
/// CPSR bit 7, IRQ disable.
pub const FLAG_I: u32 = 1 << 7;
/// CPSR bit 6, FIQ disable.
pub const FLAG_F: u32 = 1 << 6;
/// CPSR bit 5, Thumb state.
pub const FLAG_T: u32 = 1 << 5;
/// The four condition flags together, the ones an `S`-less instruction must not disturb.
pub const FLAGS: u32 = FLAG_N | FLAG_Z | FLAG_C | FLAG_V;

/// The undefined instruction vector (GBATEK, *Exception Vectors*: "BASE+04h").
pub const VECTOR_UND: u32 = 0x04;
/// The software interrupt vector: "BASE+08h".
pub const VECTOR_SWI: u32 = 0x08;
/// The IRQ vector: "BASE+18h".
pub const VECTOR_IRQ: u32 = 0x18;
/// The FIQ vector: "BASE+1Ch".
pub const VECTOR_FIQ: u32 = 0x1C;

/// The CPSR a cartridge's first instruction sees: ARM state, System mode, FIQ masked.
///
/// GBATEK, *GBA Cartridge Header*, under the entry-point table: "Note: With all entry points, the
/// CPU is initially set into system mode." *SWI 00h - SoftReset* describes the state the reset
/// restores the same way: it "initializes system, supervisor, and irq stack pointers, sets R0-R12,
/// LR_svc, SPSR_svc, LR_irq, and SPSR_irq to zero, and enters system mode", with the table
/// `GBA 3007FE0h 3007FA0h 3007F00h` for `sp_svc`, `sp_irq`, `sp_sys`.
///
/// So System mode is not a detail: on hardware the cartridge's ordinary call chain runs on
/// `SP_sys` (`$03007F00`), and the 64-byte supervisor area (*Default memory usage at 03007FXX*:
/// "3007FA0h 64 Default area for SP_svc Supervisor Stack (4 words/time)") is used only by the
/// BIOS's own SWI frames, while the 160-byte interrupt area below it holds `SP_irq`'s frames.
/// Booting into SVC instead put both on one pointer: cartridges that never touch the CPSR ran
/// their whole stack down through the interrupt reservation, and four of them died on
/// the first V-Blank.
///
/// **IRQs are unmasked.** G1 masked them; the public BIOS suite says otherwise, because
/// `bios.gba`'s third test sets `DISPSTAT`'s V-Blank enable, `IE` and `IME` and expects its handler
/// to run without ever touching the CPSR — which can only be true if the boot sequence left IRQs
/// enabled. FIQ stays masked, which is unobservable: "the FIQ signal is shortcut to VDD35… there is
/// no way to generate a FIQ by hardware" (GBATEK, *GBA Interrupt Control*).
pub const POST_BOOT_CPSR: u32 = MODE_SYS | FLAG_F;

/// Register banks: one per privileged mode, plus the user/system pair, which is one set of
/// registers seen through two names.
pub const BANK_FIQ: usize = 0;
pub const BANK_IRQ: usize = 1;
pub const BANK_SVC: usize = 2;
pub const BANK_ABT: usize = 3;
pub const BANK_UND: usize = 4;
pub const BANK_USER: usize = 5;
/// How many register banks there are, and how many `SPSR`s (the user bank has none).
pub const BANK_COUNT: usize = 6;

/// The register file, the status registers and the banks the modes keep.
///
/// Every field is public: the machine's save state is written field by field rather than through a
/// trait, and the tests in `tests/gba.rs` drive the core by hand.
#[derive(Clone, Debug)]
pub struct Cpu {
    /// `R0-R15` of the running mode. Banked registers are swapped in and out of these slots on a
    /// mode change; `R15` always holds the address of the next instruction.
    pub r: [u32; 16],
    /// The current program status register: N/Z/C/V, the IRQ and FIQ masks, Thumb state, mode.
    pub cpsr: u32,
    /// One saved status register per bank — the `SPSR` of each mode that has one. `spsr[2]` is
    /// `SPSR_svc` whether or not the CPU is in SVC mode.
    pub spsr: [u32; BANK_COUNT],
    /// `R13`/`R14` of each bank: the copies that are *not* live. A mode change writes the live pair
    /// here and reads the new mode's pair into [`Cpu::r`]. Index 5 is the user/system pair.
    pub bank_sp_lr: [[u32; 2]; BANK_COUNT],
    /// `R8`-`R12` of FIQ mode, the five registers only FIQ has its own copy of.
    pub fiq_r8_r12: [u32; 5],
    /// `R8`-`R12` of the user bank, parked here while FIQ mode is live. They are the same registers
    /// FIQ mode's copy is separate from, which is why both sets have to be held.
    pub user_r8_r12: [u32; 5],
    /// True between a BIOS `Halt`/`Stop` and the next interrupt: the core waits rather than fetching,
    /// and [`Cpu::step`] charges a cycle without executing anything.
    pub halted: bool,
    /// The comment field of the last software interrupt the core did not service itself, so a caller
    /// can say *which* BIOS call stopped it rather than only that one did.
    pub swi: Option<u32>,
    /// How many software interrupts have been executed. The public suites' failure path is the only
    /// code in them that issues one, so zero here means "no test reported a failure" — the gate the
    /// brief describes by watching the digits, which the suites overwrite with their own scratch.
    pub swi_calls: u64,
    /// Instructions retired since power-on. "How fast is the core" is a question about this number
    /// and the wall clock, not a feeling.
    pub instructions: u64,
    /// Opcodes a store overwrote after the pipeline had already fetched them: `(address, the old
    /// opcode, the `instructions` count of the step that runs it)`. See [`Cpu::step`].
    stale: [(u32, u32, u64); 2],
}

impl Cpu {
    /// Power-on: every register zero, ARM state, System mode, IRQ and FIQ masked.
    pub fn new() -> Cpu {
        let mut cpu = Cpu {
            r: [0; 16],
            cpsr: POST_BOOT_CPSR,
            spsr: [0; BANK_COUNT],
            bank_sp_lr: [[0; 2]; BANK_COUNT],
            fiq_r8_r12: [0; 5],
            user_r8_r12: [0; 5],
            halted: false,
            swi: None,
            swi_calls: 0,
            instructions: 0,
            stale: [(0, 0, 0); 2],
        };
        // GBATEK, *Default memory usage at 03007FXX*: the boot sequence leaves a stack pointer in each
        // of the three modes a cartridge runs in, and `SP_usr` (`SP_sys`) is the live one — "The three
        // stack pointers are initially initialized at the TOP of the respective areas: SP_svc=03007FE0h,
        // SP_irq=03007FA0h, SP_usr=03007F00h". The supervisor pair is parked in its bank, not left
        // zeroed, so a cartridge that does switch to SVC still finds the documented stack: on hardware
        // this area is 64 bytes and serves the BIOS's own SWI frames.
        cpu.bank_sp_lr[BANK_USER] = [SP_USR, 0];
        cpu.bank_sp_lr[BANK_IRQ] = [SP_IRQ, 0];
        cpu.bank_sp_lr[BANK_SVC] = [SP_SVC, 0];
        cpu.r[13] = SP_USR;
        cpu
    }

    /// The state the boot sequence hands a cartridge: ARM state, System mode, `SP_sys` live, and `PC`
    /// at the entry point the header's branch names.
    pub fn post_boot(entry_point: u32) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.r[15] = entry_point;
        cpu
    }

    /// True in Thumb state (`CPSR` bit 5).
    pub fn thumb(&self) -> bool {
        self.cpsr & FLAG_T != 0
    }

    /// Switch instruction sets, as `BX` does.
    pub fn set_thumb(&mut self, thumb: bool) {
        if thumb {
            self.cpsr |= FLAG_T;
        } else {
            self.cpsr &= !FLAG_T;
        }
    }

    /// The CPSR's mode field.
    pub fn mode(&self) -> u32 {
        self.cpsr & MODE_MASK
    }

    /// The address of the next instruction: `R15` after the fetch.
    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    /// The program counter as the executing instruction sees it: the address of the instruction plus
    /// eight bytes in ARM state, plus four in Thumb (the ARM7TDMI's three-stage pipeline).
    pub fn pc_value(&self) -> u32 {
        self.pc_read()
    }

    /// What an instruction reads for `R15`: its own address plus 8 (ARM) or plus 4 (Thumb).
    pub(super) fn pc_read(&self) -> u32 {
        self.r[15] + if self.thumb() { 2 } else { 4 }
    }

    /// What the parts of an instruction that read `R15` one fetch later see: plus 12 in ARM state.
    ///
    /// DDI 0084D 4.5.5: "The PC value will be the address of the instruction, plus 8 or 12 bytes […]
    /// If a register is used to specify the shift amount the PC will be 12 bytes ahead." 4.9.4:
    /// "When R15 is the source register (Rd) of a register store (STR) instruction, the stored value
    /// will be address of the instruction plus 12." 4.11.1: "Whenever R15 is stored to memory the
    /// stored value is the address of the STM instruction plus 12." In Thumb the same rules land six
    /// bytes past the instruction, which is this expression with the two-byte pipeline.
    pub(super) fn pc_read_late(&self) -> u32 {
        self.r[15] + if self.thumb() { 4 } else { 8 }
    }

    /// A register operand: `R15` reads as the pipelined program counter, everything else as stored.
    pub(super) fn reg(&self, index: usize) -> u32 {
        if index == 15 {
            self.pc_read()
        } else {
            self.r[index]
        }
    }

    /// A register operand read one fetch later, as `STR`'s source and a register-shifted operand
    /// are. See [`Cpu::pc_read_late`].
    pub(super) fn reg_late(&self, index: usize) -> u32 {
        if index == 15 {
            self.pc_read_late()
        } else {
            self.r[index]
        }
    }

    /// Write a register. A write to `R15` is a branch, and is aligned: GBATEK, *Mis-aligned PC/R15* —
    /// "For ARM code […] R15 is forcibly aligned by clearing the lower two bits. For THUMB code […]
    /// by clearing the lower bit."
    pub(super) fn write_reg(&mut self, index: usize, value: u32) {
        if index == 15 {
            self.branch_to(value);
        } else {
            self.r[index] = value;
        }
    }

    /// Jump, aligning the target to the instruction set's width.
    pub(super) fn branch_to(&mut self, target: u32) {
        self.r[15] = if self.thumb() {
            target & !1
        } else {
            target & !3
        };
    }

    /// A register as a *user-bank* transfer sees it, for the `^` of an `LDM`/`STM` whose list does
    /// not name `R15` (DDI 0084D 4.11.4: "the User bank registers are transferred rather than the
    /// register bank corresponding to the current mode").
    pub(super) fn read_user_reg(&self, index: usize) -> u32 {
        match index {
            8..=12 => {
                if self.mode() == MODE_FIQ {
                    self.user_r8_r12[index - 8]
                } else {
                    self.r[index]
                }
            }
            13 | 14 => self.bank_sp_lr[BANK_USER][index - 13],
            15 => self.pc_read_late(),
            _ => self.r[index],
        }
    }

    /// Write a user-bank register, the other half of the `^` above.
    pub(super) fn write_user_reg(&mut self, index: usize, value: u32) {
        match index {
            8..=12 => {
                if self.mode() == MODE_FIQ {
                    self.user_r8_r12[index - 8] = value;
                } else {
                    self.r[index] = value;
                }
            }
            13 | 14 => self.bank_sp_lr[BANK_USER][index - 13] = value,
            15 => self.branch_to(value),
            _ => self.r[index] = value,
        }
    }

    /// The bank a mode's registers live in. USR, SYS and any value the mode field does not name share
    /// the user bank, which is what the hardware does with a reserved mode.
    pub(super) fn bank_of(mode: u32) -> usize {
        match mode {
            MODE_FIQ => BANK_FIQ,
            MODE_IRQ => BANK_IRQ,
            MODE_SVC => BANK_SVC,
            MODE_ABT => BANK_ABT,
            MODE_UND => BANK_UND,
            _ => BANK_USER,
        }
    }

    /// Swap the live `R13`/`R14` (and FIQ's `R8`-`R12`) for the new mode's.
    fn switch_banks(&mut self, old: u32, new: u32) {
        let old = Cpu::bank_of(old);
        let new = Cpu::bank_of(new);
        if old == new {
            return;
        }
        self.bank_sp_lr[old] = [self.r[13], self.r[14]];
        if old == BANK_FIQ {
            for (index, slot) in self.fiq_r8_r12.iter_mut().enumerate() {
                *slot = self.r[8 + index];
            }
            for (index, slot) in self.user_r8_r12.iter().enumerate() {
                self.r[8 + index] = *slot;
            }
        }
        [self.r[13], self.r[14]] = self.bank_sp_lr[new];
        if new == BANK_FIQ {
            for (index, slot) in self.user_r8_r12.iter_mut().enumerate() {
                *slot = self.r[8 + index];
            }
            for (index, slot) in self.fiq_r8_r12.iter().enumerate() {
                self.r[8 + index] = *slot;
            }
        }
    }

    /// Write the whole CPSR, banking if the mode field changed.
    fn set_cpsr(&mut self, value: u32) {
        self.switch_banks(self.mode(), value & MODE_MASK);
        self.cpsr = value;
    }

    /// `MSR`: write the fields the instruction selected, and carry out a change of mode.
    ///
    /// The `T` bit is not writable through `MSR` (GBATEK: "The T-bit may not be changed; for
    /// THUMB/ARM switching use BX instruction"), and in User mode only the flags byte may change.
    pub(super) fn write_cpsr_fields(&mut self, value: u32, mask: u32) {
        let mut mask = mask & !FLAG_T;
        if self.mode() == MODE_USR {
            mask &= FLAGS;
        }
        let value = (self.cpsr & !mask) | (value & mask);
        self.set_cpsr(value);
    }

    /// The `SPSR` of a mode, or `None` for User and System mode, where there is none.
    pub(super) fn spsr_of(&self, mode: u32) -> Option<u32> {
        match Cpu::bank_of(mode) {
            BANK_USER => None,
            bank => Some(self.spsr[bank]),
        }
    }

    /// Take an exception: save the return address and the status register, enter `mode` in ARM state
    /// with IRQs masked (and FIQs too, for FIQ), and jump to `vector`.
    ///
    /// GBATEK, *Actions performed by CPU when entering an exception*.
    fn enter_exception(&mut self, mode: u32, vector: u32, return_address: u32) {
        let old = self.cpsr;
        let bank = Cpu::bank_of(mode);
        if bank != BANK_USER {
            self.spsr[bank] = old;
        }
        let mut cpsr = (old & !(MODE_MASK | FLAG_T)) | mode | FLAG_I;
        if mode == MODE_FIQ {
            cpsr |= FLAG_F;
        }
        self.switch_banks(old & MODE_MASK, mode);
        self.cpsr = cpsr;
        self.r[14] = return_address;
        self.r[15] = vector;
    }

    /// Return from an exception: `CPSR = SPSR_<mode>`. Does nothing in User mode, which has no saved
    /// status register.
    pub(super) fn restore_cpsr(&mut self) {
        if let Some(spsr) = self.spsr_of(self.mode()) {
            self.set_cpsr(spsr);
        }
    }

    /// An instruction the core does not implement: the Undefined Instruction trap. `R14_und` is the
    /// address of the next instruction, which is where `R15` already points, so a handler returns
    /// with `MOVS PC, R14`.
    pub(super) fn undefined_instruction(&mut self) -> u32 {
        let return_address = self.r[15];
        self.enter_exception(MODE_UND, VECTOR_UND, return_address);
        // GBATEK, *Undefined Instruction*: "Execution time: 2S+1I+1N." — four cycles, not five.
        // Every caller (both instruction sets) returns this figure unchanged.
        4
    }

    /// Is the interrupt now due? One line, because the Advance has one.
    ///
    /// `IME` gates everything, `IE & IF` is the request, and the CPSR's `I` bit masks it.
    /// **There is no FIQ.** GBATEK, *Fast Interrupt (FIQ)*: "The FIQ signal is shortcut to VDD35
    /// … there is no way to generate a FIQ by hardware", and the interrupt controller's whole
    /// vocabulary is the one IRQ line at `$18`. An earlier version of this function sent requests
    /// on `IE`/`IF` bits 8-13 — DMA 0-3, keypad, Game Pak — to `$1C` "because they share the ARM's
    /// FIQ line"; the shared line exists on the die but is tied off, no cartridge can see it, and
    /// the BIOS has no entry at `$1C`, so every cartridge whose DMA or keypad interrupt fired was
    /// sent to a vector that halts the machine. The ARM7TDMI can still *enter* FIQ from software
    /// (`MSR CPSR_c, #MODE_FIQ`, `VECTOR_FIQ`, [`Cpu::enter_exception`]'s FIQ arm) — that is the
    /// core's own instruction set, and the register-bank tests use it. What it cannot do is have
    /// the controller raise one.
    ///
    /// G4 owns the sources that *set* `IF`; the dispatch is this core's, and a test raises `IF` by
    /// hand to prove it.
    ///
    /// `pub(crate)` because a BIOS call's body runs with the caller's `I` bit, so a call that takes
    /// real time — `RegisterRamReset`'s clears — asks this between its own stores.
    pub(crate) fn pending_interrupt(&self, bus: &Bus) -> bool {
        bus.ime()
            && bus.requested() != 0
            // `I` masks the IRQ line. `F` does not, and is not consulted: on this machine it masks
            // an interrupt source that does not exist.
            && self.cpsr & FLAG_I == 0
    }

    /// Take the interrupt [`Cpu::pending_interrupt`] said was due.
    ///
    /// `R14_irq` is the address of the *next* instruction plus four, so the handler's
    /// `SUBS PC, R14, #4` continues after the instruction that was interrupted — the offset the
    /// pipeline forces on every exception return.
    ///
    /// The IRQ goes to `$00000018`, which is `b $128`: the BIOS handler. [`crate::bios::step`]
    /// takes it from there on the next step, so a cartridge sees the vector it would see on hardware
    /// and the handler behind it is code we implement.
    fn take_interrupt(&mut self) -> u32 {
        let return_address = self.r[15].wrapping_add(4);
        self.enter_exception(MODE_IRQ, VECTOR_IRQ, return_address);
        7
    }

    /// Execute one instruction. Returns the cycles it cost; the caller advances the clock with
    /// [`Bus::tick`] so that a step taken by a debugger and a step taken by the frame loop charge
    /// the machine the same way.
    ///
    /// The cost is the core's own (`S`/`N`/`I` at one cycle each) plus whatever the instruction's
    /// memory accesses owed on top of that — [`Bus::waits`], which the bus fills as it is read and
    /// written and this takes as soon as the instruction is done.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        bus.waits = 0;
        // A store to `HALTCNT` stops the CPU between instructions (GBATEK, *4000301h - HALTCNT*).
        // Taking it here rather than inside the store keeps the write inside its own instruction.
        if bus.halt_request {
            bus.halt_request = false;
            self.halted = true;
        }
        // An `IntrWait` finishes here, between instructions, when the flag it watches has arrived:
        // the call's caller is already past the `SWI`, so all that is left is to wake the CPU.
        crate::bios::poll_wait(self, bus);
        if self.pending_interrupt(bus) {
            self.halted = false;
            let cycles = self.take_interrupt();
            return cycles + bus.waits;
        }
        // A `RegisterRamReset` the interrupt took: the regions it had not cleared yet are cleared
        // here, before the caller's instruction after the `SWI` runs — the V-Blank that arrived
        // inside the call has already been serviced, and the clears after it are the call's own.
        crate::bios::poll_reset(self, bus);
        // A watched flag that has not arrived keeps the call's caller asleep through every
        // interrupt it did not ask for: that handler ran above, and the CPU halts again instead of
        // running the instruction after the `SWI` — see [`crate::bios`]'s `poll_wait`.
        if self.halted {
            // A halted core waits for any *enabled* interrupt, whether or not the CPSR lets it in
            // and whether or not `IME` is set: "the state of CPUs IRQ disable bit in CPSR register,
            // and the IME register are don't care, Halt passes through even if either one has
            // disabled interrupts" (GBATEK, *BIOS Halt Functions*).
            //
            // A BIOS wait is not `Halt`, though: `IntrWait` "Continues to wait in Halt state until
            // one (or more) of the specified interrupt(s) do occur", so a request it did not ask
            // for never releases the caller — only the flag `poll_wait` reads does. The caller is
            // the instruction parked at `bios.wait_pc`; a handler running other code is not.
            if (bus.bios.wait_pc != 0 && self.r[15] == bus.bios.wait_pc)
                || !self.interrupt_requested(bus)
            {
                return 1;
            }
            self.halted = false;
        }
        if crate::bios::in_bios(self.r[15]) {
            // The BIOS is code we implement, not an image: an address inside it means one of the
            // documented operations, and nothing here executes `18h`'s bytes.
            let cycles = crate::bios::step(self, bus);
            return cycles + bus.waits;
        }
        self.instructions += 1;
        // The three-stage pipeline has already fetched the two opcodes after this one, so a store
        // that overwrites them must not change what runs: GBATEK, *GBA Cart Protections* (Classic
        // NES Series): "CPU pipeline (selfmodifying code that shall NOT affect prefetched opcodes)".
        // Only RAM can be overwritten, so only code running from RAM pays for the look-ahead.
        let (pc, size) = (self.r[15], if self.thumb() { 2 } else { 4 });
        let mask = if size == 2 { 0xFFFF } else { u32::MAX };
        let ahead = |bus: &Bus| {
            [
                bus.peek_code(pc.wrapping_add(size)) & mask,
                bus.peek_code(pc.wrapping_add(2 * size)) & mask,
            ]
        };
        let before = matches!(pc >> 24, 2 | 3 | 6).then(|| ahead(bus));
        let core = if self.thumb() {
            self.step_thumb(bus)
        } else {
            self.step_arm(bus)
        };
        if let Some(before) = before {
            // A branch refills the pipeline from memory, so only straight-line flow keeps it.
            if self.r[15] == pc.wrapping_add(size) {
                let after = ahead(bus);
                for slot in 0..2 {
                    if before[slot] != after[slot] {
                        let at = pc.wrapping_add(size * (slot as u32 + 1));
                        self.stale[slot] = (at, before[slot], self.instructions + 1 + slot as u64);
                    }
                }
            }
        }
        core + bus.waits
    }

    /// `fetched`, unless the pipeline held an older opcode for `pc` that a store has since replaced.
    pub(super) fn pipelined(&self, pc: u32, fetched: u32) -> u32 {
        self.stale
            .iter()
            .find(|&&(at, _, due)| at == pc && due == self.instructions)
            .map_or(fetched, |&(_, old, _)| old)
    }

    /// True when `IE & IF` names any implemented interrupt, which is what wakes a halted core.
    fn interrupt_requested(&self, bus: &Bus) -> bool {
        bus.irq_requested()
    }

    /// A software interrupt: the number picks a BIOS call, which [`crate::bios`] implements.
    ///
    /// GBATEK, *How BIOS Processes SWIs*: "SWIs can be called from both within THUMB and ARM mode. In
    /// ARM mode, only the upper 8bit of the 24bit comment field are interpreted" — a Thumb `SWI 06h`
    /// and an ARM `SWI 60000h` are the same call.
    ///
    /// The instruction's own cost is charged here rather than in either decoder, because this is the
    /// one function both instruction sets reach.
    pub(super) fn software_interrupt(&mut self, comment: u32, bus: &mut Bus) -> u32 {
        self.swi_calls += 1;
        let number = if self.thumb() {
            comment & 0xFF
        } else {
            (comment >> 16) & 0xFF
        };
        // GBATEK, *Software Interrupt (SWI/BKPT)*: "Execution Time: 2S+1N" — the same figure
        // *THUMB.17* prints for the Thumb encoding and *ARM CPU Instruction Cycle Times* repeats as
        // `SWI,trap 2S+1N`. Three bus cycles for the instruction, before the handler's own work.
        //
        // GBATEK, *How BIOS Processes SWIs*: "Each time when calling a BIOS function 4 words (SPSR,
        // R11, R12, R14) are saved on Supervisor stack (_svc)". That save is four 32-bit stores
        // below `SP_svc`, which the boot sequence leaves at `$03007FE0` ("SP_svc=03007FE0h",
        // *Default memory usage at 03007FXX*) inside the arena *GBA BIOS RAM Usage* labels
        // "3007FA0h 40h   Default Supervisor Stack (4 words/time) (sp_svc=03007FE0h)" — so the four
        // words land at `$03007FD0-$03007FDF`, SPSR last, at `$03007FDC-$03007FDF`.
        let mut cycles = 3 + self.save_supervisor_frame(bus);
        cycles += crate::bios::call(self, bus, number);
        if bus.bios.unimplemented == Some(number) {
            // A call nobody implemented: record it on the core as well, so a gate can name it, and
            // stop the machine rather than let it run on as if the call had worked.
            self.swi = Some(comment);
            self.halted = true;
        }
        cycles
    }

    /// The four words a BIOS call saves on the Supervisor stack, and what their stores cost.
    ///
    /// The order is the one an `STMFD` of GBATEK's list produces — the lowest-numbered register at
    /// the lowest address, `SPSR` at the top. `R14` is the return address the exception puts in
    /// `LR_svc`, which is the instruction after the `SWI` and so the value `R15` already holds here;
    /// the saved status register is the caller's `CPSR`, which is what `SPSR_svc` holds at that
    /// moment. The stack pointer is left where it was: the handler that saves is the handler that
    /// restores, and a caller gets its registers back.
    ///
    /// The stores go through [`Bus::write32`], whose `charge_store` prices whatever region the
    /// Supervisor stack is in from GBATEK's table — a cartridge may move that stack ("The user may
    /// redefine these addresses and move stacks into other locations"), and then the frame costs that
    /// region's row, not IWRAM's. `charge_store` adds only the region's excess over the `2N` of a
    /// store, so those two cycles are added here, once per word.
    fn save_supervisor_frame(&mut self, bus: &mut Bus) -> u32 {
        let sp = if self.mode() == MODE_SVC {
            self.r[13]
        } else {
            self.bank_sp_lr[BANK_SVC][0]
        };
        let frame = [self.r[11], self.r[12], self.r[15], self.cpsr];
        let base = sp.wrapping_sub(16);
        for (index, value) in frame.into_iter().enumerate() {
            bus.write32(base.wrapping_add(index as u32 * 4), value);
        }
        4 * 2 // four stores, `STR 2N` each
    }

    // ------------------------------------------------------------- conditions and flags

    /// Evaluate an ARM condition code against the flags. `0xF` is `NV`, never true on ARMv4.
    pub(super) fn condition(&self, cond: u32) -> bool {
        let n = self.cpsr & FLAG_N != 0;
        let z = self.cpsr & FLAG_Z != 0;
        let c = self.cpsr & FLAG_C != 0;
        let v = self.cpsr & FLAG_V != 0;
        match cond {
            0x0 => z,
            0x1 => !z,
            0x2 => c,
            0x3 => !c,
            0x4 => n,
            0x5 => !n,
            0x6 => v,
            0x7 => !v,
            0x8 => c && !z,
            0x9 => !c || z,
            0xA => n == v,
            0xB => n != v,
            0xC => !z && n == v,
            0xD => z || n != v,
            0xE => true,
            _ => false,
        }
    }

    /// Set or clear one CPSR flag.
    pub(super) fn set_flag(&mut self, flag: u32, set: bool) {
        if set {
            self.cpsr |= flag;
        } else {
            self.cpsr &= !flag;
        }
    }

    /// Set N and Z from a result.
    pub(super) fn set_nz(&mut self, value: u32) {
        self.set_flag(FLAG_N, value & 0x8000_0000 != 0);
        self.set_flag(FLAG_Z, value == 0);
    }

    /// `a + b + carry_in`, returning the result, the carry out and the overflow — the three things an
    /// `S` instruction needs. Nothing is written; the caller decides whether to keep it.
    pub(super) fn add_flags(&self, a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
        let c = u32::from(carry_in);
        let (sum, one) = a.overflowing_add(b);
        let (sum, two) = sum.overflowing_add(c);
        let overflow = (a ^ sum) & (b ^ sum) & 0x8000_0000 != 0;
        (sum, one || two, overflow)
    }

    /// `a - b - !carry_in`, returning the result, the carry out (a subtraction's carry is "no
    /// borrow") and the overflow.
    pub(super) fn sub_flags(&self, a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
        let borrow = u32::from(!carry_in);
        let (diff, one) = a.overflowing_sub(b);
        let (diff, two) = diff.overflowing_sub(borrow);
        let overflow = (a ^ b) & (a ^ diff) & 0x8000_0000 != 0;
        (diff, !(one || two), overflow)
    }

    // ---------------------------------------------------------------- the barrel shifter

    /// A shift by an immediate field, returning the result and the carry out.
    ///
    /// The zero amounts are the special ones (DDI 0084D 4.5.1; GBATEK, *Zero Shift Amount*):
    /// `LSL #0` moves nothing and leaves the carry alone, `LSR #0` is `LSR #32`, `ASR #0` is
    /// `ASR #32`, and `ROR #0` is `RRX`, which shifts the old carry in at the top.
    pub(super) fn shift_by_immediate(
        &self,
        value: u32,
        kind: u32,
        amount: u32,
        carry_in: bool,
    ) -> (u32, bool) {
        match (kind, amount) {
            (0, 0) => (value, carry_in),
            (0, n) => (value << n, (value >> (32 - n)) & 1 != 0),
            (1, 0) => (0, value & 0x8000_0000 != 0),
            (1, n) => (value >> n, (value >> (n - 1)) & 1 != 0),
            (2, 0) => (((value as i32) >> 31) as u32, value & 0x8000_0000 != 0),
            (2, n) => (((value as i32) >> n) as u32, (value >> (n - 1)) & 1 != 0),
            (3, 0) => ((value >> 1) | (u32::from(carry_in) << 31), value & 1 != 0),
            (_, n) => (value.rotate_right(n), (value >> (n - 1)) & 1 != 0),
        }
    }

    /// A shift whose amount is in a register. Only the low byte counts, and 32 and above are the
    /// documented extensions (DDI 0084D 4.5.2): `LSL #32` gives zero with bit 0 as the carry, more
    /// gives zero and no carry; `LSR #32` uses bit 31; `ASR` saturates on bit 31; `ROR` folds the
    /// amount back into 1-32.
    pub(super) fn shift_by_register(
        &self,
        value: u32,
        kind: u32,
        amount: u32,
        carry_in: bool,
    ) -> (u32, bool) {
        let n = amount & 0xFF;
        if n == 0 {
            return (value, carry_in);
        }
        match kind {
            0 if n < 32 => (value << n, (value >> (32 - n)) & 1 != 0),
            0 if n == 32 => (0, value & 1 != 0),
            0 => (0, false),
            1 if n < 32 => (value >> n, (value >> (n - 1)) & 1 != 0),
            1 if n == 32 => (0, value & 0x8000_0000 != 0),
            1 => (0, false),
            2 if n < 32 => (((value as i32) >> n) as u32, (value >> (n - 1)) & 1 != 0),
            2 => (((value as i32) >> 31) as u32, value & 0x8000_0000 != 0),
            _ => {
                let n = n % 32;
                if n == 0 {
                    (value, value & 0x8000_0000 != 0)
                } else {
                    (value.rotate_right(n), (value >> (n - 1)) & 1 != 0)
                }
            }
        }
    }

    /// How many internal cycles a multiply takes, from the multiplier's significant bytes: "m=1 for
    /// Bit 31-8, m=2 for Bit 31-16, m=3 for Bit 31-24, and m=4 otherwise" (GBATEK).
    pub(super) fn multiply_cycles(value: u32, signed: bool) -> u32 {
        let significant = |shift: u32| {
            let top = if signed {
                ((value as i32) >> shift) as u32
            } else {
                value >> shift
            };
            top == 0 || (signed && top == u32::MAX >> shift)
        };
        if significant(8) {
            1
        } else if significant(16) {
            2
        } else if significant(24) {
            3
        } else {
            4
        }
    }
}

impl Default for Cpu {
    fn default() -> Cpu {
        Cpu::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Bus, IE, IF, IME, IWRAM_BASE, ROM_BASE};
    use crate::cart::{Cartridge, Header};
    use crate::irq;

    /// A bus with `code` at the start of the ROM and an entry point of `$08000000`.
    fn bus_with(code: &[u32]) -> Bus {
        let mut rom = vec![0u8; 0x100];
        for (i, word) in code.iter().enumerate() {
            rom[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let cart = Cartridge {
            rom,
            header: Header {
                entry: 0xEA00_0000,
                entry_point: ROM_BASE,
                title: "TEST".to_owned(),
                game_code: "TEST".to_owned(),
                maker: "00".to_owned(),
                version: 0,
                rom_bytes: 0x100,
                complement: 0,
                complement_ok: true,
                logo_ok: true,
            },
            crc32: 0,
        };
        Bus::new(cart)
    }

    fn run(cpu: &mut Cpu, bus: &mut Bus, steps: usize) -> u32 {
        let mut cycles = 0;
        for _ in 0..steps {
            let n = cpu.step(bus);
            bus.tick(n);
            cycles += n;
        }
        cycles
    }

    fn bus_write(bus: &mut Bus, at: usize, word: u32) {
        let bytes = word.to_le_bytes();
        bus.cart.rom[at..at + 4].copy_from_slice(&bytes);
    }

    #[test]
    fn post_boot_is_arm_system_mode_with_the_cartridge_entry_point() {
        // GBATEK, *GBA Cartridge Header*: "With all entry points, the CPU is initially set into
        // system mode." The live stack pointer is therefore `SP_sys`, and the supervisor pair waits
        // in its own bank rather than being the pointer the cartridge runs on.
        let cpu = Cpu::post_boot(0x0800_00C0);
        assert!(!cpu.thumb(), "ARM state");
        assert_eq!(cpu.cpsr & 0x1F, MODE_SYS);
        assert_eq!(cpu.r[13], SP_USR);
        assert_eq!(cpu.bank_sp_lr[BANK_SVC], [SP_SVC, 0]);
        assert_eq!(cpu.r[15], 0x0800_00C0);
    }

    #[test]
    fn mov_and_add_immediate_run_and_set_their_flags() {
        let mut bus = bus_with(&[
            0xE3A0_0010, // mov r0,#16
            0xE280_1001, // add r1,r0,#1
            0xE280_2002, // add r2,r0,#2
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 3);
        assert_eq!(cpu.r[0], 16);
        assert_eq!(cpu.r[1], 17);
        assert_eq!(cpu.r[2], 18);
        assert_eq!(cpu.r[15], ROM_BASE + 12);
    }

    #[test]
    fn cmp_and_a_conditional_branch_take_the_branch_when_equal() {
        let mut bus = bus_with(&[
            0xE3A0_0005, // mov r0,#5
            0xE350_0005, // cmp r0,#5
            0x1A00_0002, // bne +8 (skipped)
            0xE3A0_1001, // mov r1,#1
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 4);
        assert_eq!(cpu.r[1], 1, "the branch was not taken");
        assert_eq!(cpu.r[15], ROM_BASE + 16);
        assert!(cpu.cpsr & FLAG_Z != 0);
    }

    #[test]
    fn a_branch_to_a_register_switches_instruction_sets() {
        // ldr r0,[pc,#0] / bx r0, with the literal $0800000D at $08000008: odd, so Thumb.
        let mut bus = bus_with(&[0xE59F_0000, 0xE12F_FF10, ROM_BASE + 0x0D]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 2);
        assert!(cpu.thumb(), "BX with bit 0 set selects Thumb");
        assert_eq!(cpu.r[15], ROM_BASE + 0x0C);
    }

    #[test]
    fn a_store_then_a_load_goes_through_the_bus() {
        let mut bus = bus_with(&[0; 4]);
        bus_write(&mut bus, 0, 0xE3A0_0301); // mov r0,#0x04000000
        bus_write(&mut bus, 4, 0xE3A0_1403); // mov r1,#0x03000000
        bus_write(&mut bus, 8, 0xE581_0000); // str r0,[r1]
        bus_write(&mut bus, 12, 0xE591_2000); // ldr r2,[r1]
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 4);
        assert_eq!(cpu.r[0], 0x0400_0000);
        assert_eq!(cpu.r[1], IWRAM_BASE);
        assert_eq!(bus.read32(IWRAM_BASE), 0x0400_0000);
        assert_eq!(cpu.r[2], 0x0400_0000);
    }

    #[test]
    fn thumb_mov_cmp_and_a_conditional_branch() {
        // Thumb: mov r0,#3 / cmp r0,#3 / beq +2 / mov r1,#9
        let mut bus = bus_with(&[]);
        let code: [u16; 4] = [0x2003, 0x2803, 0xD000, 0x2109];
        for (i, half) in code.iter().enumerate() {
            let bytes = half.to_le_bytes();
            bus.cart.rom[i * 2..i * 2 + 2].copy_from_slice(&bytes);
        }
        let mut cpu = Cpu::post_boot(ROM_BASE);
        cpu.set_thumb(true);
        run(&mut cpu, &mut bus, 4);
        assert_eq!(cpu.r[0], 3);
        assert_eq!(cpu.r[1], 0, "the move was branched over");
    }

    #[test]
    fn thumb_literal_load_reads_the_rom_through_the_bus() {
        // ldr r0,[pc,#0] loads the word at $08000004, which is the second instruction's encoding.
        let mut bus = bus_with(&[0x0000_4800, 0xDEAD_BEEF]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        cpu.set_thumb(true);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.r[0], 0xDEAD_BEEF);
    }

    // ------------------------------------------------ the barrel shifter, every shift type

    #[test]
    fn every_shift_sets_the_carry_from_the_bit_it_shifted_out() {
        let cpu = Cpu::new();
        // LSL: the last bit shifted out is bit (32 - n).
        assert_eq!(cpu.shift_by_immediate(0x8000_0001, 0, 1, false), (2, true));
        assert_eq!(
            cpu.shift_by_immediate(1, 0, 31, false),
            (0x8000_0000, false)
        );
        // LSR: bit (n - 1).
        assert_eq!(cpu.shift_by_immediate(2, 1, 1, false), (1, false));
        assert_eq!(cpu.shift_by_immediate(3, 1, 1, false), (1, true));
        // ASR keeps the sign and reports bit (n - 1).
        assert_eq!(
            cpu.shift_by_immediate(0x8000_0000, 2, 1, false),
            (0xC000_0000, false)
        );
        assert_eq!(
            cpu.shift_by_immediate(0x8000_0003, 2, 2, false),
            (0xE000_0000, true)
        );
        // ROR brings the shifted-out bits back in at the top; carry = bit (n - 1).
        assert_eq!(cpu.shift_by_immediate(1, 3, 1, false), (0x8000_0000, true));
        assert_eq!(cpu.shift_by_immediate(0x0000_0002, 3, 1, false), (1, false));
    }

    #[test]
    fn lsl_zero_leaves_the_carry_and_lsr_asr_zero_mean_thirty_two() {
        let cpu = Cpu::new();
        // LSL #0 moves nothing and passes the carry through unchanged.
        assert_eq!(
            cpu.shift_by_immediate(0x1234_5678, 0, 0, true),
            (0x1234_5678, true)
        );
        assert_eq!(
            cpu.shift_by_immediate(0x1234_5678, 0, 0, false),
            (0x1234_5678, false)
        );
        // LSR #0 is LSR #32: zero, and bit 31 becomes the carry.
        assert_eq!(cpu.shift_by_immediate(0x8000_0000, 1, 0, false), (0, true));
        assert_eq!(cpu.shift_by_immediate(0x7FFF_FFFF, 1, 0, false), (0, false));
        // ASR #0 is ASR #32: the whole register becomes the sign, and bit 31 is the carry.
        assert_eq!(
            cpu.shift_by_immediate(0x8000_0001, 2, 0, false),
            (0xFFFF_FFFF, true)
        );
        assert_eq!(cpu.shift_by_immediate(0x7FFF_FFFF, 2, 0, false), (0, false));
        // ROR #0 is RRX: the old carry goes in at bit 31, and bit 0 comes out.
        assert_eq!(cpu.shift_by_immediate(1, 3, 0, true), (0x8000_0000, true));
        assert_eq!(cpu.shift_by_immediate(0, 3, 0, false), (0, false));
    }

    #[test]
    fn a_register_shifted_by_32_has_its_own_rules_and_only_the_low_byte_counts() {
        let cpu = Cpu::new();
        // LSL by 32 is zero with bit 0 as the carry; by more, zero and no carry.
        assert_eq!(cpu.shift_by_register(1, 0, 32, true), (0, true));
        assert_eq!(cpu.shift_by_register(1, 0, 33, true), (0, false));
        // LSR by 32 is zero with bit 31 as the carry.
        assert_eq!(cpu.shift_by_register(0x8000_0000, 1, 32, true), (0, true));
        // ASR saturates on bit 31 at 32 and beyond.
        assert_eq!(
            cpu.shift_by_register(0x8000_0000, 2, 33, true),
            (0xFFFF_FFFF, true)
        );
        // ROR by 32 reproduces the value and uses bit 31 as the carry.
        assert_eq!(
            cpu.shift_by_register(0x8000_0001, 3, 32, true),
            (0x8000_0001, true)
        );
        // Only the low byte of Rs is the amount: $F10 is 16.
        assert_eq!(cpu.shift_by_register(1, 0, 0xF10, false), (0x1_0000, false));
        // A zero amount passes the old carry through.
        assert_eq!(cpu.shift_by_register(7, 3, 0, true), (7, true));
    }

    #[test]
    fn sbc_and_rsc_treat_the_carry_as_no_borrow() {
        let cpu = Cpu::new();
        // SBC R0, R1, R2 with the carry set: R1 - R2 - 0.
        assert_eq!(cpu.sub_flags(64, 32, true), (32, true, false));
        // With the carry clear: R1 - R2 - 1, which is 32 - 64 - 1 = -33.
        assert_eq!(cpu.sub_flags(64, 32, false), (31, true, false));
        assert_eq!(cpu.sub_flags(32, 64, false), (0xFFFF_FFDF, false, false));
        // 0 - 0 - 1 is the case that shows SBC's borrow leaving the carry clear.
        assert_eq!(cpu.sub_flags(0, 0, false), (0xFFFF_FFFF, false, false));
        // And the signed edge: $80000000 - 1 overflows.
        let (result, carry, overflow) = cpu.sub_flags(0x8000_0000, 1, true);
        assert_eq!(result, 0x7FFF_FFFF);
        assert!(carry, "no borrow");
        assert!(overflow, "positive overflow");
    }

    #[test]
    fn msr_to_the_flags_field_can_set_every_condition_and_clear_it_again() {
        let mut bus = bus_with(&[
            0xE3A0_0000, // mov r0,#0
            0xE128_F000, // msr cpsr_f, #0
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        cpu.cpsr |= FLAG_N | FLAG_Z | FLAG_C | FLAG_V;
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.cpsr & FLAGS, 0, "msr cpsr_f, #0 cleared all four");
    }

    // ---------------------------------------- the instructions the public suites cannot pin down

    #[test]
    fn tst_teq_cmp_and_cmn_set_flags_and_never_write_rd() {
        let mut bus = bus_with(&[
            0xE3A0_0005, // mov r0,#5
            0xE3A0_1005, // mov r1,#5
            0xE110_0001, // tst r0,r1
            0xE130_0001, // teq r0,r1
            0xE150_0001, // cmp r0,r1
            0xE170_0001, // cmn r0,r1
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 3);
        assert_eq!(cpu.cpsr & FLAG_Z, 0, "tst: 5 & 5 is not zero");
        assert_eq!(cpu.r[0], 5, "tst wrote its Rd");
        run(&mut cpu, &mut bus, 1);
        assert_ne!(cpu.cpsr & FLAG_Z, 0, "teq: 5 ^ 5 is zero");
        assert_eq!(cpu.r[0], 5, "teq wrote its Rd");
        run(&mut cpu, &mut bus, 1);
        assert_ne!(cpu.cpsr & FLAG_Z, 0, "cmp: 5 - 5 is zero");
        assert_ne!(cpu.cpsr & FLAG_C, 0, "cmp: 5 - 5 borrowed nothing");
        assert_eq!(cpu.r[0], 5, "cmp wrote its Rd");
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cpsr & (FLAG_Z | FLAG_C | FLAG_V), 0, "cmn: 5 + 5 is 10");
        assert_eq!(cpu.r[0], 5, "cmn wrote its Rd");
    }

    #[test]
    fn rsc_subtracts_the_complement_of_the_carry() {
        let mut bus = bus_with(&[
            0xE0F0_2001, // rscs r2,r0,r1  (r1 - r0 - !C)
            0xE0F0_3001, // rscs r3,r0,r1
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        cpu.cpsr |= FLAG_C;
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.r[2], 0, "0 - 0 - 0");
        assert_ne!(cpu.cpsr & FLAG_C, 0, "no borrow");
        cpu.cpsr &= !FLAG_C;
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.r[3], 0xFFFF_FFFF, "0 - 0 - 1");
        assert_eq!(cpu.cpsr & FLAG_C, 0, "the borrow cleared C");
    }

    #[test]
    fn ldm_with_the_base_in_its_list_keeps_the_loaded_value() {
        let mut bus = bus_with(&[
            0xE3A0_0403, // mov r0,#$03000000
            0xE8B0_0003, // ldmia r0!, {r0, r1}
        ]);
        bus.write32(IWRAM_BASE, 0x1111_1111);
        bus.write32(IWRAM_BASE + 4, 0x2222_2222);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.r[0], 0x1111_1111, "the loaded value, not the writeback");
        assert_eq!(cpu.r[1], 0x2222_2222);
    }

    #[test]
    fn stm_of_a_list_naming_r15_stores_the_instruction_address_plus_twelve() {
        let mut bus = bus_with(&[
            0xE3A0_0403, // mov r0,#$03000000  (at $08000000)
            0xE8A0_8000, // stmia r0!, {r15}   (at $08000004)
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 2);
        assert_eq!(
            bus.read32(IWRAM_BASE),
            ROM_BASE + 16,
            "the STM is at +4, so its +12 is +16"
        );
        assert_eq!(cpu.r[0], IWRAM_BASE + 4, "the base still wrote back");
    }

    #[test]
    fn a_misaligned_halfword_load_is_the_aligned_word_rotated_and_masked() {
        let mut bus = bus_with(&[
            0xE3A0_0403, // mov r0,#$03000000
            0xE3A0_1001, // mov r1,#1
            0xE190_20B1, // ldrh r2,[r0,r1]
            0xE190_30F1, // ldrsh r3,[r0,r1]
        ]);
        bus.write32(IWRAM_BASE, 0x1122_3344);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 4);
        assert_eq!(
            cpu.r[2], 0x4400_0033,
            "the aligned word rotated right 8 with bits 8-23 cleared"
        );
        assert_eq!(cpu.r[3], 0x33, "LDRSH from an odd address is LDRSB from it");
    }

    #[test]
    fn a_pre_decrement_strh_with_writeback_is_a_store_not_an_msr() {
        // P=1 U=0 W=1 puts bits 24-21 where MSR's are; only bits 7-4 tell them apart (GBATEK,
        // *Halfword ... Data Transfer*: bit 7 and bit 4 "must be set"). Found as Rayman's BPE decoder
        // initialising its table with `strh r12,[r6,#-2]!`.
        let mut bus = bus_with(&[
            0xE3A0_6403, // mov r6,#$03000000
            0xE286_6004, // add r6,r6,#4
            0xE3A0_C0AB, // mov r12,#$AB
            0xE166_C0B2, // strh r12,[r6,#-2]!
            0xE166_C0B2, // strh r12,[r6,#-2]!
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 5);
        assert_eq!(cpu.r[6], IWRAM_BASE, "the base walked down twice");
        assert_eq!(bus.read16(IWRAM_BASE), 0xAB);
        assert_eq!(bus.read16(IWRAM_BASE + 2), 0xAB);
    }

    #[test]
    fn msr_writes_a_banked_spsr_and_mrs_reads_the_same_one_back() {
        // The boot mode is System (GBATEK, *GBA Cartridge Header*: "With all entry points, the CPU
        // is initially set into system mode"), and System has no SPSR of its own, so the test asks
        // for Supervisor mode first — the SPSR under test is SPSR_svc.
        let mut bus = bus_with(&[
            0xE321_F013, // msr cpsr_c, #MODE_SVC
            0xE3A0_0011, // mov r0,#$11
            0xE161_F000, // msr spsr_c, r0      (SPSR_svc)
            0xE14F_1000, // mrs r1, spsr
            0xE321_F011, // msr cpsr_c, #MODE_FIQ
            0xE14F_2000, // mrs r2, spsr         (SPSR_fiq, untouched)
            0xE3A0_3022, // mov r3,#$22
            0xE161_F003, // msr spsr_c, r3       (SPSR_fiq)
            0xE321_F013, // msr cpsr_c, #MODE_SVC
            0xE14F_4000, // mrs r4, spsr         (SPSR_svc again)
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 10);
        assert_eq!(cpu.r[1], 0x11, "SPSR_svc held what MSR put there");
        assert_eq!(cpu.r[2], 0, "SPSR_fiq is a different register");
        assert_eq!(cpu.r[4], 0x11, "SPSR_svc was not disturbed by FIQ's");
        assert_eq!(cpu.spsr[BANK_FIQ], 0x22);
    }

    #[test]
    fn fiq_banks_r8_to_r12_and_a_later_irq_still_sees_the_user_bank() {
        let mut bus = bus_with(&[]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        for i in 0..5 {
            cpu.r[8 + i] = 0x1000 + i as u32;
        }
        cpu.write_cpsr_fields(MODE_FIQ, MODE_MASK);
        assert_eq!(cpu.mode(), MODE_FIQ);
        assert_eq!(cpu.r[8], 0, "FIQ's own bank starts zeroed");
        for i in 0..5 {
            cpu.r[8 + i] = 0x2000 + i as u32;
        }
        cpu.write_cpsr_fields(MODE_SVC, MODE_MASK);
        for i in 0..5 {
            assert_eq!(cpu.r[8 + i], 0x1000 + i as u32, "the user five are back");
        }
        // Raise an IRQ by hand: IRQ banks R13/R14 only, so its handler must still see the user five.
        bus.io[IME] = 1;
        bus.io[IE] = 1;
        bus.io[IF] = 1;
        cpu.cpsr &= !FLAG_I;
        let before = cpu.cpsr;
        cpu.step(&mut bus);
        assert_eq!(cpu.mode(), MODE_IRQ);
        for i in 0..5 {
            assert_eq!(cpu.r[8 + i], 0x1000 + i as u32, "IRQ sees the user bank");
        }
        cpu.restore_cpsr();
        assert_eq!(cpu.mode(), MODE_SVC);
        assert_eq!(cpu.spsr[BANK_IRQ], before);
        cpu.write_cpsr_fields(MODE_FIQ, MODE_MASK);
        for i in 0..5 {
            assert_eq!(cpu.r[8 + i], 0x2000 + i as u32, "FIQ's five survived both");
        }
    }

    /// GBATEK, *GBA Cart Protections*: Classic NES Series games rely on "CPU pipeline (selfmodifying
    /// code that shall NOT affect prefetched opcodes)". A store at `X` that rewrites `X+8` changes
    /// nothing the pipeline has already fetched, so the old opcode still runs; `X+12` is fetched
    /// after the store and does see it.
    #[test]
    fn a_store_into_the_two_prefetched_opcodes_does_not_change_what_runs() {
        let mut bus = bus_with(&[]);
        let mut cpu = Cpu::post_boot(IWRAM_BASE);
        let code = [
            0xE28F_E008, // add lr,pc,#8      lr = this + 16
            0xE1A0_2002, // mov r2,r2
            0xE58E_0000, // str r0,[lr]       rewrites the opcode at +16, eight past the store
            0xE3A0_10FF, // mov r1,#255
            0xE3A0_10FF, // mov r1,#255       <- rewritten to mov r1,#0 after it was fetched
            0xE3A0_30FF, // mov r3,#255       <- one further on: fetched after the store
            0xE3A0_40FF, // mov r4,#255
        ];
        for (i, word) in code.iter().enumerate() {
            bus.write32(IWRAM_BASE + 4 * i as u32, *word);
        }
        bus.write32(IWRAM_BASE + 20, 0xE3A0_3000); // mov r3,#0: unlike the line above, this one is seen
        cpu.r[0] = 0xE3A0_1000; // mov r1,#0
        cpu.r[15] = IWRAM_BASE;
        for _ in 0..6 {
            cpu.step(&mut bus);
        }
        assert_eq!(
            cpu.r[1], 255,
            "the opcode already in the pipeline still ran"
        );
        assert_eq!(cpu.r[3], 0, "an opcode fetched after the store sees it");
    }

    #[test]
    fn an_irq_raises_the_vector_with_the_return_address_four_past_the_pipeline() {
        let mut bus = bus_with(&[
            0xE3A0_0001, // mov r0,#1
            0xE3A0_1002, // mov r1,#2
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        bus.io[IME] = 1;
        bus.io[IE] = 1;
        bus.io[IF] = 1;
        cpu.cpsr |= FLAG_F; // boot leaves F set; the dispatch does not consult it
        cpu.cpsr &= !FLAG_I;
        let before = cpu.cpsr;
        let cycles = cpu.step(&mut bus);
        assert_eq!(cycles, 7);
        assert_eq!(cpu.mode(), MODE_IRQ);
        assert_eq!(cpu.r[15], VECTOR_IRQ);
        assert_eq!(
            cpu.r[14],
            ROM_BASE + 4,
            "LR_irq is the interrupted instruction's own address plus four"
        );
        assert_eq!(cpu.spsr[BANK_IRQ], before);
        assert_ne!(cpu.cpsr & FLAG_I, 0, "IRQ entry masks IRQs");
        assert_eq!(cpu.r[0], 0, "the interrupted instruction did not run");
    }

    /// The bug this pair of tests exists to prevent: every `IE`/`IF` bit is one IRQ line on the
    /// Advance. Bit 10 is DMA 2, and an earlier dispatch sent it — and every other bit in `$3F00` —
    /// to the FIQ vector `$1C`, which no cartridge installs and which the BIOS does not answer.
    #[test]
    fn a_dma_request_enters_irq_mode_at_18_and_never_fiq() {
        let mut bus = bus_with(&[
            0xE3A0_0001, // mov r0,#1   <- retires before the interrupt is raised
            0xE3A0_1002, // mov r1,#2   <- the instruction that would run next
            0xE3A0_2003, // mov r2,#3
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        run(&mut cpu, &mut bus, 1);
        bus.io[IME] = 1;
        // Bit 10 lives in the high byte of both registers, so write the halfword rather than the
        // byte an earlier test's bit 0 needed.
        let request = irq::bit(10); // DMA 2
        bus.io[IE..IE + 2].copy_from_slice(&request.to_le_bytes());
        bus.io[IF..IF + 2].copy_from_slice(&request.to_le_bytes());
        // Both masks clear: the `I` bit is what gates the IRQ, and `F` gates nothing on a machine
        // with no FIQ source — so an interrupt must come through with F clear and must not need it.
        cpu.cpsr &= !(FLAG_I | FLAG_F);
        let before = cpu.cpsr;
        let cycles = cpu.step(&mut bus);
        assert_eq!(cycles, 7);
        assert_eq!(
            cpu.mode(),
            MODE_IRQ,
            "DMA 2 is an IRQ on the Advance, not a FIQ"
        );
        assert_eq!(cpu.r[15], VECTOR_IRQ, "the vector is $18");
        // The interrupted instruction is the one at $08000004 and it did not run. Its own address
        // plus four is $08000008, which is what `R14_irq` must hold: `SUBS PC, R14, #4` in the
        // handler then returns to $08000004 and executes it.
        assert_eq!(cpu.r[14], ROM_BASE + 8);
        assert_eq!(cpu.spsr[BANK_IRQ], before);
        assert_eq!(cpu.r[1], 0, "the interrupted instruction did not run");
        assert_eq!(cpu.r[2], 0, "and nothing past it did either");
    }

    /// The mirror of the test above: an FIQ-mask bit in `IF` is not a way through a set `I`.
    #[test]
    fn f_clear_does_not_let_a_masked_interrupt_through() {
        let mut bus = bus_with(&[
            0xE3A0_0001, // mov r0,#1
            0xE3A0_1002, // mov r1,#2
        ]);
        let mut cpu = Cpu::post_boot(ROM_BASE);
        bus.io[IME] = 1;
        let request = irq::bit(10);
        bus.io[IE..IE + 2].copy_from_slice(&request.to_le_bytes());
        bus.io[IF..IF + 2].copy_from_slice(&request.to_le_bytes());
        cpu.cpsr |= FLAG_I;
        cpu.cpsr &= !FLAG_F;
        let cycles = cpu.step(&mut bus);
        assert_eq!(
            cpu.mode(),
            MODE_SYS,
            "no exception was taken, so the CPU is still in its boot mode"
        );
        assert_eq!(cpu.r[15], ROM_BASE + 4, "the instruction executed");
        assert_eq!(cpu.r[0], 1);
        assert_eq!(
            cycles,
            1 + bus.waits,
            "one S cycle and whatever the fetch owed"
        );
        assert_eq!(
            bus.ie() & bus.irq_flags() & 0x3FFF,
            irq::bit(10),
            "still pending"
        );
    }
}
