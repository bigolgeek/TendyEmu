//! The BIOS calls, reimplemented in our own code — and the BIOS's own code as a state machine.
//!
//! A Game Boy Advance cartridge calls the BIOS constantly: division, square roots, the affine
//! parameter sets, decompression, memory fills, waits. Every one of them is a `SWI` whose comment
//! field names the call, and on hardware the CPU traps to `$00000008` and the real BIOS does the
//! work. There is no BIOS image here and never will be one. What this module has instead is a Rust
//! function per call, dispatched from [`Cpu::software_interrupt`], plus the two places where the
//! BIOS's *own* code is observable and therefore has to be modelled:
//!
//! **The interrupt handler.** `$00000018` holds `b $00000128`, and the handler there saves six
//! registers, loads the user's handler pointer from `[$03007FFC]` and jumps to it — the sequence
//! GBATEK's *BIOS Interrupt handling* prints in full. A cartridge never sees that sequence as code,
//! but it does see its effects: the six registers saved on the IRQ stack, `R0` holding
//! `$04000000`, `LR_irq` holding the handler's return point, and — the reason this matters — the
//! *BIOS read* that a handler performs. That is modelled here as two steps of BIOS code, the
//! prologue at `$128` and the epilogue at `$138`, which is exactly how the hardware gets from the
//! vector to the user's handler and back.
//!
//! **Reading the BIOS region.** GBATEK, *Reading from BIOS Memory*: "the GBA allows to read opcodes
//! or data only if the program counter is located inside of the BIOS area. If the program counter
//! is not in the BIOS area, reading will return the most recent successfully fetched BIOS opcode
//! (eg. the opcode at `[00DCh+8]` after startup and SoftReset, the opcode at `[0134h+8]` during IRQ
//! execution, and opcode at `[013Ch+8]` after IRQ execution, and opcode at `[0188h+8]` after SWI
//! execution)." A cartridge's program counter is never inside our BIOS, so every read of the region
//! returns [`Bios::latch`], and the four points named above set it: post-boot, IRQ entry, IRQ
//! return and SWI return. The four words themselves are the four opcodes the public suite's
//! `bios.gba` requires (`$E129F000`, `$E25EF004`, `$E55EC002`, `$E3A02004`) — four instructions of
//! documented BIOS code, not an image of it.
//!
//! **The ABI.** Arguments in `R0`-`R3`, results in `R0`/`R1` (and `R3` for `Div`), everything else
//! preserved. The real BIOS pushes `SPSR_svc`, `R11`, `R12` and `R14` on the SVC stack and runs the
//! body in System mode (GBATEK, *How BIOS Processes SWIs*); that frame is invisible to a caller
//! that gets its registers back, so these calls run in Rust and leave the CPU in the mode they found
//! it in. The one call whose frame *is* visible — because the machine sleeps inside it — is
//! `IntrWait`, and that one is modelled as a suspended call: the caller's `PC` is already past the
//! `SWI`, the CPU halts, and the call finishes when a flag it is watching arrives.
//!
//! Everything is derived from GBATEK (*BIOS Functions*, *BIOS Interrupt handling*, *Reading from
//! BIOS Memory*) and from `roms/test/gba/src/`, the public suite's own source. No other emulator's
//! source was read for any of it.

use crate::bus::{self, Bus, DISPSTAT, IE, IF, IME, IO_BASE, SOUNDBIAS, WAITCNT};
use crate::cpu::{Cpu, MODE_SYS};

pub use tendy_gb::gba_types::SwiCall;

/// Where the BIOS's IRQ handler starts: `$00000018` is `b 128h` (GBATEK, *BIOS Interrupt handling*).
pub const IRQ_HANDLER: u32 = 0x0128;
/// The handler's epilogue: "00000138 ldmfd r13!,r0-r3,r12,r14 / 0000013C subs r15,r14,4h".
pub const IRQ_EPILOGUE: u32 = 0x0138;
/// The interrupt check flag the `IntrWait` functions read and clear: "3007FF8h 2 Interrupt Check
/// Flag (for IntrWait/VBlankIntrWait functions)".
pub const IRQ_FLAGS: u32 = 0x0300_7FF8;
/// The user IRQ handler's pointer: "3007FFCh 4 Pointer to user IRQ handler (32bit ARM code)".
pub const IRQ_HANDLER_POINTER: u32 = 0x0300_7FFC;
/// The mirror the BIOS handler actually loads through: "mov r0,4000000h / ldr r15,[r0,-4h]", which
/// is `$03FFFFFC` (GBATEK: "ptr+4 to 03FFFFFC (mirror of 03007FFC)").
pub const IRQ_HANDLER_MIRROR: u32 = 0x03FF_FFFC;

/// The word a read of the BIOS returns after the boot sequence: the opcode at `[00DCh+8]`.
pub const OPCODE_AFTER_BOOT: u32 = 0xE129_F000;
/// The word a read returns during an IRQ: the opcode at `[0134h+8]`, `subs r15,r14,4h`.
pub const OPCODE_DURING_IRQ: u32 = 0xE25E_F004;
/// The word a read returns after an IRQ: the opcode at `[013Ch+8]`.
pub const OPCODE_AFTER_IRQ: u32 = 0xE55E_C002;
/// The word a read returns after an SWI: the opcode at `[0188h+8]`, `mov r2,4h`.
pub const OPCODE_AFTER_SWI: u32 = 0xE3A0_2004;

/// `GetBiosChecksum`'s answer for a GBA or GBA SP (GBATEK, *SWI 0Dh*: "The checksum is BAAE187Fh").
pub const BIOS_CHECKSUM: u32 = 0xBAAE_187F;

/// `SOUNDBIAS`'s two levels: "0=Level 000h, any other value=Level 200h" (GBATEK, *SWI 19h*).
const SOUNDBIAS_LOW: u16 = 0x000;
const SOUNDBIAS_HIGH: u16 = 0x200;

/// The highest SWI number GBATEK's GBA table names. A smaller array than 256 is possible because a
/// call number above this is not a call, it is a fault, which is recorded separately.
const CALL_COUNT: usize = 0x2B;

/// The name GBATEK's BIOS-function table gives a call number, for a report that has to name what a
/// title asked for rather than print a hex number.
///
/// GBATEK, *BIOS Functions*: the named entries are `00h`-`19h`, `1Ah`-`1Fh` (the sound driver),
/// `20h`-`24h` ("-Undoc- (Sound Related ???)", which later documents call the MusicPlayer set),
/// `25h` MultiBoot, `26h`/`27h` (HardReset, CustomHalt), `28h`/`29h` (the driver's vsync pair) and
/// `2Ah` (the jump list). Above `2Ah` GBATEK says the BIOS "does not perform any range checks", so a
/// call there "will blindly lock up the GBA", which is why [`CALL_COUNT`] stops at `2Ah`.
pub fn call_name(number: u32) -> &'static str {
    match number {
        0x00 => "SoftReset",
        0x01 => "RegisterRamReset",
        0x02 => "Halt",
        0x03 => "Stop",
        0x04 => "IntrWait",
        0x05 => "VBlankIntrWait",
        0x06 => "Div",
        0x07 => "DivArm",
        0x08 => "Sqrt",
        0x09 => "ArcTan",
        0x0A => "ArcTan2",
        0x0B => "CpuSet",
        0x0C => "CpuFastSet",
        0x0D => "GetBiosChecksum",
        0x0E => "BgAffineSet",
        0x0F => "ObjAffineSet",
        0x10 => "BitUnPack",
        0x11 => "LZ77UnCompWram",
        0x12 => "LZ77UnCompVram",
        0x13 => "HuffUnComp",
        0x14 => "RLUnCompWram",
        0x15 => "RLUnCompVram",
        0x16 => "Diff8bitUnFilterWram",
        0x17 => "Diff8bitUnFilterVram",
        0x18 => "Diff16bitUnFilter",
        0x19 => "SoundBias",
        0x1A => "SoundDriverInit",
        0x1B => "SoundDriverMode",
        0x1C => "SoundDriverMain",
        0x1D => "SoundDriverVsync",
        0x1E => "SoundChannelClear",
        0x1F => "MidiKey2Freq",
        0x20..=0x24 => "MusicPlayer (undocumented)",
        0x25 => "MultiBoot",
        0x26 => "HardReset",
        0x27 => "CustomHalt",
        0x28 => "SoundDriverVsyncOff",
        0x29 => "SoundDriverVsyncOn",
        0x2A => "SoundGetJumpList",
        _ => "not a call",
    }
}

/// The BIOS as this slice implements it: what a read of it returns, what it has been asked to do,
/// and what it is waiting for.
///
/// Every field is plain, because a save state has to carry all of it (see
/// [`Bios::encode`]/[`Bios::decode`]) — except [`Bios::trace`], which is a diagnostic that a state
/// deliberately drops.
#[derive(Clone, Debug)]
pub struct Bios {
    /// The word a read of the BIOS region returns while the PC is outside it. See the module
    /// documentation for the four points that set it.
    pub latch: u32,
    /// How many times each call has been made, by number. The public suites' failure path is the
    /// only code in them that calls `Div`, so this is how a gate tells a passing run — whose only
    /// `SWI` is the BIOS test's own `Sqrt` — from a failing one.
    pub calls: [u32; CALL_COUNT],
    /// The first call number this slice does not implement (`$25`, `$26`, `$2A` and anything at all
    /// above `$2A`). Recorded rather than silently returned from.
    pub unimplemented: Option<u32>,
    /// The first BIOS address our own code does not implement, recorded for the same reason.
    pub unknown_entry: Option<u32>,
    /// The work area the sound driver was initialised with (`$1A`'s `R0`), while one is live.
    ///
    /// Everything else the driver needs is in that area, which is the caller's memory and therefore
    /// already part of a save state; the address itself has nowhere else to live, so it is carried
    /// here and encoded. See [`crate::sound_driver`].
    pub sound_work: Option<u32>,
    /// The flags an `IntrWait` is waiting for, while the CPU sleeps inside one.
    pub wait: Option<u16>,
    /// Where that call is parked: the caller's `PC` past the `SWI`, which is the instruction the
    /// BIOS's wait loop returns to. 0 while no call is waiting.
    ///
    /// The call holds *that* address and no other: a user handler may run code of its own in System
    /// mode, and an interrupt it takes while it does must not be mistaken for the sleeping caller.
    /// Part of a save state, with [`Bios::wait`].
    pub wait_pc: u32,
    /// The `RegisterRamReset` flags whose clears an interrupt cut short, while one is in flight: the
    /// regions the call had not reached yet. 0 when no call is parked, and part of a save state.
    ///
    /// A reset's clears run with the caller's interrupts open, so a V-Blank can be taken between
    /// two of them; the rest then runs after the handler returns. See [`poll_reset`].
    pub reset_pending: u32,
    /// Where that reset is parked — the caller's `PC` past the `SWI`, the address whose own return
    /// means the handler is done with the machine. 0 while no reset is parked.
    pub reset_pc: u32,
    /// How many calls [`Bios::trace`] may record: `--trace-swi N`'s `N`, 0 when the run is not
    /// tracing. A diagnostic field, so a save state does not carry it.
    pub trace_limit: usize,
    /// The calls recorded so far, in order, at most [`Bios::trace_limit`] of them.
    pub trace: Vec<SwiCall>,
}

impl Bios {
    /// The BIOS after the boot sequence: its opcode latch holds the word the boot code last fetched.
    pub fn new() -> Bios {
        Bios {
            latch: OPCODE_AFTER_BOOT,
            calls: [0; CALL_COUNT],
            unimplemented: None,
            unknown_entry: None,
            sound_work: None,
            wait: None,
            wait_pc: 0,
            reset_pending: 0,
            reset_pc: 0,
            trace_limit: 0,
            trace: Vec::new(),
        }
    }

    /// `--trace-swi N`: record the next `N` software interrupts, with their arguments.
    ///
    /// The record is a [`Vec`] rather than a fixed array because a run may ask for tens of thousands
    /// of calls and only a title being measured pays for it — a run that does not ask records
    /// nothing and allocates nothing.
    pub fn trace_calls(&mut self, limit: usize) {
        self.trace_limit = limit;
        self.trace = Vec::with_capacity(limit.min(65_536));
    }

    /// The bytes this slice's state needs. Written at the end of [`crate::Gba`]'s payload, so a
    /// save state reproduces the BIOS's record — including which call it refused, and the two calls
    /// that can be parked mid-flight (`IntrWait`'s and a `RegisterRamReset`'s).
    pub const STATE_LEN: usize = 4 + 4 * CALL_COUNT + 24 + 4;

    /// [`Bios::decode`]'s other half.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Bios::STATE_LEN);
        bytes.extend_from_slice(&self.latch.to_le_bytes());
        for count in self.calls {
            bytes.extend_from_slice(&count.to_le_bytes());
        }
        bytes.extend_from_slice(&self.unimplemented.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&self.unknown_entry.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&u32::from(self.wait.unwrap_or(u16::MAX)).to_le_bytes());
        bytes.extend_from_slice(&self.wait_pc.to_le_bytes());
        bytes.extend_from_slice(&self.sound_work.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&self.reset_pending.to_le_bytes());
        bytes.extend_from_slice(&self.reset_pc.to_le_bytes());
        bytes
    }

    /// Restore [`Bios::encode`]'s body.
    pub fn decode(&mut self, bytes: &[u8]) {
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        self.latch = word(0);
        for (index, count) in self.calls.iter_mut().enumerate() {
            *count = word(4 + index * 4);
        }
        let base = 4 + CALL_COUNT * 4;
        self.unimplemented = Some(word(base)).filter(|&value| value != u32::MAX);
        self.unknown_entry = Some(word(base + 4)).filter(|&value| value != u32::MAX);
        self.wait = Some(word(base + 8) as u16).filter(|&value| value != u16::MAX);
        self.wait_pc = word(base + 12);
        self.sound_work = Some(word(base + 16)).filter(|&value| value != u32::MAX);
        self.reset_pending = word(base + 20);
        self.reset_pc = word(base + 24);
    }

    /// How many times a call has been made.
    pub fn calls_to(&self, number: u32) -> u32 {
        self.calls.get(number as usize).copied().unwrap_or(0)
    }
}

impl Default for Bios {
    fn default() -> Bios {
        Bios::new()
    }
}

/// True when `pc` is inside the BIOS, which is where our own BIOS code lives.
pub fn in_bios(pc: u32) -> bool {
    bus::is_bios(pc)
}

/// One step of BIOS code: the two ends of the interrupt handler, and the vector's branch.
///
/// The handler's body — the jump to the user's routine — is what makes a cartridge's ISR run, and
/// it is entered from [`Cpu::step`] whenever the program counter is in BIOS range. An address here
/// that is neither end is recorded in [`Bios::unknown_entry`] and the core keeps running: it is the
/// machine's own exception vector table, not a fault in this core.
///
/// GBATEK, *Exception Vectors*: "when an exception arises, CPU is switched into ARM state, and the
/// program counter (PC) is loaded by the respective address", with the eight slots at
/// `BASE+00h..BASE+1Ch` — `BASE+04h` "Undefined Instruction", `BASE+08h` "Software Interrupt (SWI)",
/// `BASE+0Ch` "Prefetch Abort" — and the note that "there's only space for one ARM opcode at each of
/// the above addresses, it'd be usually recommended to deposit a Branch opcode into each vector,
/// which'd then redirect to the actual exception handlers address."
///
/// GBATEK does not print the opcode at `00000004h` and does not describe what the GBA's `$04`
/// handler does on an ordinary cartridge. Its only GBA-specific sentence about that handler is that
/// it "becomes unlocked, the handler then forwards these exceptions to the user handler in cartridge
/// ROM (entry point defined in 80000B4h)" *when* cartridge header `09Ch` has bits 2 and 7 both set —
/// and the entry point it names is the hardware debugger's DACS memory, which "normal cartridges do
/// not have ... nor any mirrors". So the handler's body is undescribed and this core does not invent
/// one, for `$04` or for any other entry: the address is recorded once, and the core is left running
/// on it. Nothing is fetched or branched to on the cartridge's behalf, so the program counter stays
/// where the exception put it; what changes is that the machine is no longer parked there, so
/// interrupts are taken under the usual gating and a census or a debugger sees a live core.
pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    match cpu.r[15] {
        // "00000018 b 128h ;IRQ vector: jump to actual BIOS handler". The exception entered the IRQ
        // mode at the vector, so this is the next step: three cycles for the branch itself.
        crate::cpu::VECTOR_IRQ => {
            cpu.r[15] = IRQ_HANDLER;
            3
        }
        IRQ_HANDLER => irq_prologue(cpu, bus),
        IRQ_EPILOGUE => irq_epilogue(cpu, bus),
        other => {
            let record = &mut bus.bios.unknown_entry;
            if record.is_none() {
                *record = Some(other);
            }
            1
        }
    }
}

/// "00000128 stmfd r13!,r0-r3,r12,r14 / 0000012C mov r0,4000000h / 00000130 add r14,r15,0h /
/// 00000134 ldr r15,[r0,-4h]" — the six registers saved on the IRQ stack, the pointer register set
/// up, the return point `$138` in `LR`, and the user's handler loaded from `[$03007FFC]`.
fn irq_prologue(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let saved = [0usize, 1, 2, 3, 12, 14];
    let mut sp = cpu.r[13].wrapping_sub(4 * saved.len() as u32);
    cpu.r[13] = sp;
    for register in saved {
        bus.write32(sp, cpu.r[register]);
        sp = sp.wrapping_add(4);
    }

    // The handler is the one place the BIOS sees `IF` before the user's routine acknowledges it, so
    // this is where the flags `IntrWait` watches are sampled. The documented contract has the user's
    // handler OR them into [$03007FF8]; sampling here means a handler that forgets still wakes a
    // waiting call, and a handler that does it properly sees the same bits.
    let ie = read16(bus, IO_BASE + IE as u32);
    let flags = read16(bus, IO_BASE + IF as u32);
    let seen = read16(bus, IRQ_FLAGS) | (ie & flags & 0x3FFF);
    write16(bus, IRQ_FLAGS, seen);

    cpu.r[0] = IO_BASE;
    cpu.r[14] = IRQ_EPILOGUE;
    let handler = read32(bus, IRQ_HANDLER_MIRROR);
    bus.bios.latch = OPCODE_DURING_IRQ;
    cpu.set_thumb(false);
    cpu.branch_to(handler);
    7
}

/// "00000138 ldmfd r13!,r0-r3,r12,r14 / 0000013C subs r15,r14,4h" — the registers back, and a return
/// to the interrupted instruction with the interrupted mode's status register.
fn irq_epilogue(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let saved = [0usize, 1, 2, 3, 12, 14];
    let mut sp = cpu.r[13];
    for register in saved {
        cpu.r[register] = read32(bus, sp);
        sp = sp.wrapping_add(4);
    }
    cpu.r[13] = sp;
    let target = cpu.r[14].wrapping_sub(4);
    cpu.restore_cpsr();
    bus.bios.latch = OPCODE_AFTER_IRQ;
    cpu.branch_to(target);
    // While an `IntrWait` is still waiting, the instruction this epilogue returns to is the call's
    // own parked continuation, not code the caller may run: `IntrWait` "Continues to wait in Halt
    // state until one (or more) of the specified interrupt(s) do occur" (GBATEK, *SWI 04h*), so an
    // interrupt it did not ask for ends the sleep, runs this handler, and puts the caller back to
    // sleep here. The flag `poll_wait` reads is what lets it through. Only the parked address is
    // held: a handler that runs the cartridge's own code in System mode returns to *its* caller,
    // not to the sleeping one, and must not be mistaken for it.
    if bus.bios.wait_pc != 0 && target == bus.bios.wait_pc {
        cpu.halted = true;
    }
    7
}

/// Whether an `IntrWait` should finish now: the flags it watches have arrived in `[$03007FF8]`.
///
/// GBATEK, *SWI 04h - IntrWait*: "Continues to wait in Halt state until one (or more) of the
/// specified interrupt(s) do occur"; "Return: No return value, the selected flag(s) are
/// automatically reset in BIOS Interrupt Flags value in RAM upon return." The test and the reset are
/// therefore against the RAM word the user's interrupt handler maintains, not against `IF`.
///
/// That sentence is also what says which interrupts end the call, and it is not `Halt`'s rule:
/// "Halt mode is terminated when any enabled interrupts are requested, that is when (IE AND IF) is
/// not zero" (GBATEK, *BIOS Halt Functions*), but `IntrWait` only continues until "one (or more) of
/// the specified interrupt(s)" does occur. An interrupt the call did not ask for therefore ends
/// that *sleep* — its handler runs through `$18`/`$128` like any other and returns through
/// [`irq_epilogue`] — and the BIOS then tests `[$03007FF8h]` again and halts again. The caller's
/// instruction after the `SWI` does not run until the watched flag is in the RAM word; the two
/// places that hold it back are [`irq_epilogue`], which is the only way a handler returns to it, and
/// [`crate::cpu::Cpu::step`]'s halted branch, which is where it would otherwise run. Without
/// them an unrelated request returns from the call early *and* leaves [`Bios::wait`] set, so the
/// next flag the user's handler writes is consumed by a call that has already returned.
///
/// The read and the clear go straight to work RAM rather than through the bus, because this is the
/// BIOS looking at its own bookkeeping between instructions, not a CPU access that would belong to
/// the next instruction's cycle count.
pub fn poll_wait(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let Some(watched) = bus.bios.wait else {
        return false;
    };
    let at = (IRQ_FLAGS & 0x7FFF) as usize;
    let seen = u16::from_le_bytes([bus.iwram[at], bus.iwram[at + 1]]);
    if seen & watched == 0 {
        return false;
    }
    let left = (seen & !watched).to_le_bytes();
    bus.iwram[at] = left[0];
    bus.iwram[at + 1] = left[1];
    bus.bios.wait = None;
    bus.bios.wait_pc = 0;
    cpu.halted = false;
    true
}

/// Dispatch a software interrupt's call number. Returns the cycles the call cost.
///
/// GBATEK, *How BIOS Processes SWIs*: "In ARM mode, only the upper 8bit of the 24bit comment field
/// are interpreted", which is why the number is the caller's state plus the comment — an ARM `SWI
/// 60000h` and a Thumb `SWI 06h` are the same call.
pub fn call(cpu: &mut Cpu, bus: &mut Bus, number: u32) -> u32 {
    // The caller's state is captured before the call touches `R0`-`R3` (several calls return there),
    // and the entry is pushed after it so it can say whether the dispatcher refused the number.
    let mut record = SwiCall {
        number,
        args: [cpu.r[0], cpu.r[1], cpu.r[2], cpu.r[3]],
        lr: cpu.r[14],
        ret: cpu.r[15],
        refused: false,
    };
    if let Some(count) = bus.bios.calls.get_mut(number as usize) {
        *count += 1;
    }
    let refusals_before = bus.bios.unimplemented;
    let cycles = match number {
        0x00 => soft_reset(cpu, bus),
        0x01 => register_ram_reset(cpu, bus),
        0x02 => halt(cpu),
        0x03 => stop(cpu),
        0x04 => intr_wait(cpu, bus, cpu.r[0], cpu.r[1]),
        0x05 => intr_wait(cpu, bus, 1, 1),
        0x06 => div(cpu, false),
        0x07 => div(cpu, true),
        0x08 => sqrt(cpu),
        0x09 => arc_tan(cpu),
        0x0A => arc_tan2(cpu),
        0x0B => cpu_set(cpu, bus, false),
        0x0C => cpu_set(cpu, bus, true),
        0x0D => {
            cpu.r[0] = BIOS_CHECKSUM;
            12
        }
        0x0E => bg_affine_set(cpu, bus),
        0x0F => obj_affine_set(cpu, bus),
        0x10 => bit_unpack(cpu, bus),
        0x11 => lz77(cpu, bus, false),
        0x12 => lz77(cpu, bus, true),
        0x13 => huffman(cpu, bus),
        0x14 => run_length(cpu, bus, false),
        0x15 => run_length(cpu, bus, true),
        0x16 => diff_filter(cpu, bus, false, false),
        0x17 => diff_filter(cpu, bus, false, true),
        0x18 => diff_filter(cpu, bus, true, false),
        0x19 => sound_bias(cpu, bus),
        0x1A => crate::sound_driver::init(cpu, bus),
        0x1B => crate::sound_driver::mode(cpu, bus),
        0x1C => crate::sound_driver::main(cpu, bus),
        0x1D => crate::sound_driver::vsync(cpu, bus),
        0x1E => crate::sound_driver::channel_clear(cpu, bus),
        0x1F => midi_key_2_freq(cpu, bus),
        0x20..=0x24 => crate::sound_driver::undocumented(cpu),
        0x28 => crate::sound_driver::vsync_off(cpu, bus),
        0x29 => crate::sound_driver::vsync_on(cpu, bus),
        other => refused(&mut bus.bios, other),
    };
    record.refused = bus.bios.unimplemented != refusals_before;
    if bus.bios.trace.len() < bus.bios.trace_limit {
        bus.bios.trace.push(record);
    }
    // The SWI handler's own return path is the last BIOS code an SWI executes, and it is what a read
    // of the BIOS sees afterwards: "the opcode at [0188h+8] after SWI execution".
    bus.bios.latch = OPCODE_AFTER_SWI;
    cycles
}

/// A call this slice declines to invent: recorded, and the machine stopped rather than left to run
/// on with a call that did nothing. `$25` (MultiBoot) and `$26` (HardReset) belong to G10 and to the
/// boot sequence that does not exist here. `$2A` (SoundGetJumpList) is the one sound call still
/// refused, and the reason is measured rather than assumed: no cartridge in the library asks for
/// it — 0 of 866, over 1 200 frames each, traced — and
/// GBATEK gives the call's shape (`120h` bytes of pointers) while naming none of the 36 entries, so
/// a table here would be 36 addresses we invented. Anything above `$2A` is not a call at all
/// (GBATEK: "2Bh+ Crash").
fn refused(bios: &mut Bios, number: u32) -> u32 {
    if bios.unimplemented.is_none() {
        bios.unimplemented = Some(number);
    }
    4
}

// ---------------------------------------------------------------------------- arithmetic

/// `Div` (`$06`) and `DivArm` (`$07`): signed division of `R0` by `R1`, quotient in `R0`, remainder
/// in `R1` and the quotient's magnitude in `R3`.
///
/// GBATEK: "incoming -1234, 10 should return -123, -4, +123". Division by zero is the case the
/// documentation says "usually gets caught in an endless loop"; a machine cannot loop forever in a
/// call, so this returns the ARM convention — quotient 0 and the numerator as the remainder — which
/// is what G1 did for the public suites' failure path.
fn div(cpu: &mut Cpu, swapped: bool) -> u32 {
    let (numerator, denominator) = if swapped {
        (cpu.r[1], cpu.r[0])
    } else {
        (cpu.r[0], cpu.r[1])
    };
    let (quotient, remainder) = if denominator == 0 {
        (0, numerator)
    } else {
        // `wrapping_div` is the one that agrees with the ARM7TDMI here: only `i32::MIN / -1`
        // overflows, and the hardware's shift-and-subtract loop answers `i32::MIN` with no
        // remainder rather than trapping. The registers are unsigned; the call's arithmetic is not.
        let (numerator, denominator) = (numerator as i32, denominator as i32);
        (
            numerator.wrapping_div(denominator) as u32,
            numerator.wrapping_rem(denominator) as u32,
        )
    };
    cpu.r[0] = quotient;
    cpu.r[1] = remainder;
    cpu.r[3] = (quotient as i32).unsigned_abs();
    if swapped { 24 } else { 20 }
}

/// `Sqrt` (`$08`): the integer square root of an unsigned 32-bit number, as a 16-bit result.
fn sqrt(cpu: &mut Cpu) -> u32 {
    cpu.r[0] = cpu.r[0].isqrt();
    26
}

/// Turn a 16-bit angle in turns into radians. `$10000` is a full turn (GBATEK's angle fields for the
/// affine sets, and the `C000h-4000h` range of `ArcTan`).
fn quarter_turns_to_radians(angle: u16) -> f64 {
    f64::from(angle) * std::f64::consts::TAU / 65536.0
}

/// `ArcTan` (`$09`): `R0` is a tangent in 1.14 fixed point, the result an angle in 1/65536ths of a
/// turn, in the range `C000h`-`4000h`.
///
/// The real function is a table lookup with a documented accuracy problem near ±π/4; this computes
/// the angle and rounds it, so it is never *less* accurate. Nothing in the public suite calls it.
fn arc_tan(cpu: &mut Cpu) -> u32 {
    let tangent = f64::from(cpu.r[0] as u16 as i16) / 16384.0;
    let turns = tangent.atan() * 65536.0 / std::f64::consts::TAU;
    cpu.r[0] = (turns.round() as i32 as u32) & 0xFFFF;
    30
}

/// `ArcTan2` (`$0A`): the angle of the vector (`X`, `Y`) in `R0`/`R1`, both 1.14 fixed point, as
/// `0000h`-`FFFFh` for one whole turn.
fn arc_tan2(cpu: &mut Cpu) -> u32 {
    let x = f64::from(cpu.r[0] as u16 as i16) / 16384.0;
    let y = f64::from(cpu.r[1] as u16 as i16) / 16384.0;
    let mut turns = y.atan2(x) * 65536.0 / std::f64::consts::TAU;
    if turns < 0.0 {
        turns += 65536.0;
    }
    cpu.r[0] = (turns.round() as i64 as u32) & 0xFFFF;
    36
}

// ---------------------------------------------------------------------------- rotation and scaling

/// The sine and cosine of a BIOS rotation angle in 1.0 units (256 = 1.0).
///
/// GBATEK: "the GBA BIOS recurses only the upper 8bit; the lower 8bit may contain a fractional
/// portion, but it is ignored by the BIOS" — so the angle is quantised to `$100` steps before
/// anything else happens, which is what makes a zero angle exact.
fn sin_cos(angle: u16) -> (i32, i32) {
    let radians = quarter_turns_to_radians(angle & 0xFF00);
    (
        (radians.sin() * 256.0).round() as i32,
        (radians.cos() * 256.0).round() as i32,
    )
}

/// The four affine parameters, in the order the video hardware wants them: `pa`, `pb`, `pc`, `pd`.
fn affine(scale_x: i16, scale_y: i16, angle: u16) -> [i16; 4] {
    let (sin, cos) = sin_cos(angle);
    let (sx, sy) = (i64::from(scale_x), i64::from(scale_y));
    let (sin64, cos64) = (i64::from(sin), i64::from(cos));
    [
        ((sx * cos64) >> 8) as i16,
        ((-sx * sin64) >> 8) as i16,
        ((sy * sin64) >> 8) as i16,
        ((sy * cos64) >> 8) as i16,
    ]
}

/// `BgAffineSet` (`$0E`): `R0` points at the source field, `R1` at the destination, `R2` counts the
/// entries.
fn bg_affine_set(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let (mut src, mut dest, count) = (cpu.r[0], cpu.r[1], cpu.r[2]);
    for _ in 0..count {
        let center_x = read32(bus, src) as i32;
        let center_y = read32(bus, src + 4) as i32;
        let display_x = read16(bus, src + 8) as i16 as i64;
        let display_y = read16(bus, src + 10) as i16 as i64;
        let scale_x = read16(bus, src + 12) as i16;
        let scale_y = read16(bus, src + 14) as i16;
        let angle = read16(bus, src + 16);
        let [pa, pb, pc, pd] = affine(scale_x, scale_y, angle);
        write16(bus, dest, pa as u16);
        write16(bus, dest + 2, pb as u16);
        write16(bus, dest + 4, pc as u16);
        write16(bus, dest + 6, pd as u16);
        // The start pixel is where the display's centre lands in source space:
        // `center - (display * parameters) / 256`.
        let start_x =
            i64::from(center_x) - ((display_x * i64::from(pa) + display_y * i64::from(pb)) >> 8);
        let start_y =
            i64::from(center_y) - ((display_x * i64::from(pc) + display_y * i64::from(pd)) >> 8);
        write32(bus, dest + 8, start_x as i32 as u32);
        write32(bus, dest + 12, start_y as i32 as u32);
        src += 20;
        dest += 16;
    }
    count * 60
}

/// `ObjAffineSet` (`$0F`): the same parameters, written `R3` bytes apart, without the start pixel.
fn obj_affine_set(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let (mut src, mut dest, count, offset) = (cpu.r[0], cpu.r[1], cpu.r[2], cpu.r[3]);
    for _ in 0..count {
        let scale_x = read16(bus, src) as i16;
        let scale_y = read16(bus, src + 2) as i16;
        let angle = read16(bus, src + 4);
        let [pa, pb, pc, pd] = affine(scale_x, scale_y, angle);
        for (index, parameter) in [pa, pb, pc, pd].into_iter().enumerate() {
            write16(bus, dest + index as u32 * offset, parameter as u16);
        }
        src += 6;
        dest += 4 * offset;
    }
    count * 40
}

// ---------------------------------------------------------------------------- memory
//
// "Note: On GBA, NDS7 and DSi7, these two functions will silently reject to do anything if the
// source start or end addresses are reaching into the BIOS area." — GBATEK, *BIOS Memory Copy*.

/// `CpuSet` (`$0B`, halfwords or words) and `CpuFastSet` (`$0C`, blocks of eight words).
fn cpu_set(cpu: &mut Cpu, bus: &mut Bus, fast: bool) -> u32 {
    let (src, dest) = (cpu.r[0], cpu.r[1]);
    let control = cpu.r[2];
    // "Bit 0-20 Wordcount", "Bit 24 Fixed Source Address (0=Copy, 1=Fill by WORD[r0])" and, for
    // `CpuSet`, "Bit 26 Datasize (0=16bit, 1=32bit)". `CpuFastSet` rounds its count up to a whole
    // 32-byte block ("otherwise the GBA is forcefully rounding-up the length").
    let mut count = control & 0x1F_FFFF;
    let fill = control & (1 << 24) != 0;
    let wide = fast || control & (1 << 26) != 0;
    if fast {
        count = count.div_ceil(8) * 8;
    }
    let width = if wide { 4 } else { 2 };
    if bus::is_bios(src) || bus::is_bios(src.wrapping_add(count * width)) {
        return 20;
    }
    // The source register's alignment: GBATEK's *BIOS Memory Copy* gives the contract ("r0 Source
    // address (must be aligned by 4 for 32bit, by 2 for 16bit)") and, decisively, the instruction each
    // shape of the call is built from. `CpuFastSet` is "Memcopy ... repeated LDMIA/STMIA [Rb]!,r2-r9
    // instructions. Memfill as single LDR followed by repeated STMIA [Rb]!,r2-r9"; `CpuSet` is
    // "Memcopy ... repeated LDMIA/STMIA [Rb]!,r3 or LDRH/STRH r3,[r0,r5] instructions. Memfill as
    // single LDMIA or LDRH followed by repeated STMIA [Rb]!,r3 or STRH r3,[r0,r5]". A caller that
    // breaks the contract gets the instruction's behaviour, and the instructions answer differently
    // (*ARM CPU Memory Alignments*): `LDM` ignores the low bits ("the memory access goes to a forcibly
    // aligned (rounded-down) memory address"), a single `LDR` rotates ("Reads from forcibly aligned
    // address ... and does then rotate the data"), and on the GBA "LDRH Rd,[odd] --> LDRH Rd,[odd-1]
    // ROR 8 ;read to bit0-7 and bit24-31".
    //
    // So every word read here is an `LDM` and starts at `r0 & !3` except one: `CpuFastSet`'s fill,
    // whose single `LDR` reads `r0` as given — `CpuSet`'s word fill is a single `LDMIA` and rounds
    // down like the copies. The halfword read is answered below, and `STRH`/`STMIA` force-align the
    // destination on their own, so the writes need nothing.
    let mut from = if wide && !(fast && fill) {
        src & !3
    } else {
        src
    };
    for index in 0..count {
        let to = dest.wrapping_add(index * width);
        if wide {
            let value = read32(bus, from);
            write32(bus, to, value);
        } else if from & 1 == 0 {
            let value = read16(bus, from);
            write16(bus, to, value);
        } else {
            // `LDRH` from an odd address: bits 0-7 hold the byte at `r0` and bits 24-31 the byte
            // below it, so the halfword the following `STRH` writes is that one odd byte with a zero
            // high byte. The instruction core models the same read at `thumb.rs`/`arm.rs`.
            let value = read16(bus, from & !1) >> 8;
            write16(bus, to, value);
        }
        if !fill {
            from = from.wrapping_add(width);
        }
    }
    8 + count * if wide { 4 } else { 2 }
}

/// `BitUnPack` (`$10`): expand source units of 1, 2, 4 or 8 bits into destination units of up to 32.
///
/// The unpack information is `u16 length in bytes`, `u8 source unit bits`, `u8 destination unit
/// bits`, `u32 offset | zero-flag`. The offset "is always added to all non-zero source units. If the
/// Zero Data Flag was set, it is also added to zero units."
fn bit_unpack(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let (src, dest, info) = (cpu.r[0], cpu.r[1], cpu.r[2]);
    let source_len = read16(bus, info) as u32;
    let source_bits = read8(bus, info + 2);
    let dest_bits = read8(bus, info + 3);
    let options = read32(bus, info + 4);
    let offset = options & 0x7FFF_FFFF;
    let zero_data = options & 0x8000_0000 != 0;
    if source_bits == 0 || dest_bits == 0 || source_bits > dest_bits {
        return 8;
    }
    let source_mask = (1u32 << source_bits) - 1;
    let source_bytes_remaining = source_len as usize;
    let units = (source_len * 8) / u32::from(source_bits);
    let mut word = 0u32;
    let mut bits_used = 0u32;
    let mut at = dest;
    for unit in 0..units {
        let bit = unit * u32::from(source_bits);
        let byte = bit / 8;
        if byte as usize >= source_bytes_remaining {
            break;
        }
        let shift = bit % 8;
        // Bits are taken from the least significant end of each byte, which is the order the
        // destination packing puts them back in.
        let mut raw = u32::from(read8(bus, src + byte)) >> shift;
        if shift + u32::from(source_bits) > 8 {
            raw |= u32::from(read8(bus, src + byte + 1)) << (8 - shift);
        }
        let value = raw & source_mask;
        let expanded = if value == 0 && !zero_data {
            0
        } else {
            (value + offset) & ((1u32 << dest_bits) - 1)
        };
        word |= expanded << bits_used;
        bits_used += u32::from(dest_bits);
        if bits_used >= 32 {
            write32(bus, at, word);
            at += 4;
            word = 0;
            bits_used = 0;
        }
    }
    if bits_used > 0 {
        // "The size of unpacked data must be a multiple of 4 bytes" — the tail is still a whole word
        // on hardware, and the unused bits stay zero.
        write32(bus, at, word);
    }
    8 + units
}

// ---------------------------------------------------------------------------- decompression

/// Where a decompressor's bytes land.
///
/// The `Wram` variants (`$11`, `$14`, `$16`) write bytes; the `Vram` ones (`$12`, `$15`, `$17`) write
/// halfwords, because VRAM has no 8-bit write. GBATEK's caution is what the halfword path has to
/// get right: "Writing 16bit units to [dest-1] instead of 8bit units to [dest] means that reading
/// from [dest-1] won't work", so an odd final byte is written as the *upper* half of the halfword
/// that ends at the next address, leaving the byte before it alone.
struct Sink {
    /// The halfword being assembled: the address of its low byte, which is also the low byte's own
    /// address while that byte waits for its partner. See `dest` for the address the *next* byte
    /// goes to — the two are not the same while a byte is pending, and LZ77's back-references are
    /// priced from the second.
    next: u32,
    /// True for a `Vram` variant.
    half: bool,
    /// The low byte of the halfword being assembled, while the pair is incomplete.
    pending: Option<u8>,
}

impl Sink {
    fn new(addr: u32, half: bool) -> Sink {
        Sink {
            next: addr,
            half,
            pending: None,
        }
    }

    /// Write one byte.
    fn put(&mut self, bus: &mut Bus, byte: u8) {
        if !self.half {
            bus.write8(self.next, byte);
            self.next += 1;
            return;
        }
        match self.pending.take() {
            Some(low) => {
                let at = self.next;
                bus.write16(at, u16::from_le_bytes([low, byte]));
                self.next += 2;
            }
            None => self.pending = Some(byte),
        }
    }

    /// The address the next byte will occupy — GBATEK's `Dest`, which its back-reference is priced
    /// from.
    ///
    /// A byte sink writes every byte as it arrives, so `Dest` is `next`. A halfword sink assembles
    /// pairs, and while the low half waits for its partner that byte has already been written as far
    /// as the stream is concerned — it is simply not in memory yet — so `Dest` is one past it.
    fn dest(&self) -> u32 {
        if self.pending.is_some() {
            self.next + 1
        } else {
            self.next
        }
    }

    /// The byte `back + 1` places before `Dest`, for LZ77's back-references.
    ///
    /// GBATEK, *Block Type 1 - Compressed*: "Copy N+3 Bytes from Dest-Disp-1 to Dest". `Dest` moves
    /// with every byte the run writes, and a run may copy bytes it has itself just written, which is
    /// why `Dest` is asked for again each time rather than once for the whole run.
    fn read_back(&self, bus: &mut Bus, back: u32) -> u8 {
        let at = self.dest().wrapping_sub(back + 1);
        // `back == 0` while a byte is pending names the pending byte's own slot, which memory does
        // not hold yet, so the byte the stream last wrote is served instead. GBATEK leaves the case
        // undefined for the 16-bit variant — "works only with disp=001h..FFFh, but not with
        // disp=000h" — so this keeps the answer the sink already gave and the case stays
        // deliberately undecided rather than being defined here.
        if self.pending.is_some() && at == self.next {
            return self.pending.unwrap_or(0);
        }
        bus.read8(at)
    }

    /// Finish the stream: an odd byte in a halfword sink is written as the lower half of a halfword
    /// at its own address (the destination is halfword-aligned, so the units sit on even addresses).
    fn finish(&mut self, bus: &mut Bus) {
        if let Some(byte) = self.pending.take() {
            let at = self.next;
            let high = bus.read8(at.wrapping_add(1));
            bus.write16(at, u16::from_le_bytes([byte, high]));
        }
    }
}

/// The header every compression stream starts with: "Data Header (32bit): Bit 0-3 size/type,
/// Bit 4-7 type, Bit 8-31 24bit size". Returns the decompressed length.
fn stream_header(bus: &mut Bus, src: u32) -> u32 {
    read32(bus, src) >> 8
}

/// `LZ77UnCompWram` (`$11`) and `LZ77UnCompVram` (`$12`).
fn lz77(cpu: &mut Cpu, bus: &mut Bus, half: bool) -> u32 {
    let (src, dest) = (cpu.r[0], cpu.r[1]);
    let length = stream_header(bus, src);
    let mut from = src + 4;
    let mut sink = Sink::new(dest, half);
    let mut written = 0u32;
    while written < length {
        let flags = read8(bus, from);
        from += 1;
        for bit in 0..8 {
            if written >= length {
                break;
            }
            if flags & (0x80 >> bit) == 0 {
                let byte = read8(bus, from);
                sink.put(bus, byte);
                from += 1;
                written += 1;
            } else {
                let first = read8(bus, from);
                let second = read8(bus, from + 1);
                from += 2;
                let count = u32::from(first >> 4) + 3;
                let displacement = (u32::from(first & 0x0F) << 8) | u32::from(second);
                for _ in 0..count {
                    if written >= length {
                        break;
                    }
                    let byte = sink.read_back(bus, displacement);
                    sink.put(bus, byte);
                    written += 1;
                }
            }
        }
    }
    sink.finish(bus);
    12 + length
}

/// `RLUnCompWram` (`$14`) and `RLUnCompVram` (`$15`): "Bit 0-6 Expanded Data Length (uncompressed
/// N-1, compressed N-3), Bit 7 Flag (0=uncompressed, 1=compressed)".
fn run_length(cpu: &mut Cpu, bus: &mut Bus, half: bool) -> u32 {
    let (src, dest) = (cpu.r[0], cpu.r[1]);
    let length = stream_header(bus, src);
    let mut from = src + 4;
    let mut sink = Sink::new(dest, half);
    let mut written = 0u32;
    while written < length {
        let flag = read8(bus, from);
        from += 1;
        let count = u32::from(flag & 0x7F);
        if flag & 0x80 == 0 {
            for _ in 0..=count {
                if written >= length {
                    break;
                }
                let byte = read8(bus, from);
                sink.put(bus, byte);
                from += 1;
                written += 1;
            }
        } else {
            let byte = read8(bus, from);
            from += 1;
            for _ in 0..count + 3 {
                if written >= length {
                    break;
                }
                sink.put(bus, byte);
                written += 1;
            }
        }
    }
    sink.finish(bus);
    12 + length
}

/// `HuffUnComp` (`$13`).
///
/// The tree is a byte table; a node's bits 0-5 are the offset to its children, bit 6 says the *one*
/// branch ends in data and bit 7 says the *zero* branch does. The bitstream is read one bit at a
/// time from the top of each 32-bit word (GBATEK: "Bit0-31 Node Bits (Bit31=First Bit)").
///
/// Where the bitstream begins is the byte after the tree, and GBATEK gives it as the tree-size
/// field's own meaning: "`Tree Size (8bit)` — Bit0-7 Size of Tree Table/2-1 (**ie. Offset to
/// Compressed Bitstream**)", with "Compressed Bitstream (stored in units of 32bits)" under it. The
/// offset is counted from that field, so it is `src + 4 + (tree_size + 1) * 2` — the size byte it is
/// measured from is part of the offset, not one byte before the tree's first node.
///
/// **The data element's width.** The header's low nibble is, verbatim, "Bit0-3 Data size in bit
/// units (normally 4 or 8)", and the tree's data nodes hold "Bit0-7 Data (upper bits should be zero
/// if Data Size is less than 8)" — so a 4-bit stream's symbols are one nibble each and an 8-bit
/// stream's are a byte each. The output is "Bit8-31 24bit size of decompressed data in **bytes**",
/// written "in units of 32bits": on this little-endian bus two nibbles share an output byte, the
/// **first symbol in the low half**. A 4-bit call therefore consumes twice `length` symbols and the
/// output is exactly `length` bytes — there is no half-written byte to flush, because the byte count
/// asks for an even number of symbols.
///
/// The document states the field but never the packing order, so the order is not taken from the
/// wording alone: it is measured. All 40 `SWI 13h` calls the five 4-bit cartridges in the library
/// (`0113`, `1223`, `1232`, `1910`, `1998`) make in 1 300 frames decode to a valid LZ77 header at
/// the destination when packed low-half-first, with a size the next `SWI 11h` then consumes from
/// that very buffer; packed the other way round none of them is a header at all.
fn huffman(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let (src, dest) = (cpu.r[0], cpu.r[1]);
    let header = read32(bus, src);
    let length = header >> 8;
    let unit = header & 0xF;
    let tree_size = u32::from(read8(bus, src + 4));
    let tree = src + 5;
    let bitstream = src + 4 + (tree_size + 1) * 2;
    let mut sink = Sink::new(dest, false);
    let mut node = tree;
    let mut word = 0u32;
    let mut bits_left = 0u32;
    let mut cursor = bitstream;
    let mut written = 0u32;
    // The low half of an output byte whose high half has not been decoded yet.
    let mut nibble: Option<u8> = None;
    while written < length {
        if bits_left == 0 {
            word = read32(bus, cursor);
            cursor += 4;
            bits_left = 32;
        }
        let bit = word >> 31;
        word <<= 1;
        bits_left -= 1;
        let flags = read8(bus, node);
        let offset = u32::from(flags & 0x3F);
        let child = (node & !1) + offset * 2 + 2 + bit;
        let ends = flags & if bit == 0 { 0x80 } else { 0x40 } != 0;
        if ends {
            let data = read8(bus, child);
            if unit == 4 {
                // Two symbols to the byte, the first in the low half.
                match nibble.take() {
                    None => nibble = Some(data & 0x0F),
                    Some(low) => {
                        sink.put(bus, low | ((data & 0x0F) << 4));
                        written += 1;
                    }
                }
            } else {
                sink.put(bus, data);
                written += 1;
            }
            node = tree;
        } else {
            node = child;
        }
    }
    sink.finish(bus);
    12 + length
}

/// `Diff8bitUnFilterWram` (`$16`), `Diff8bitUnFilterVram` (`$17`) and `Diff16bitUnFilter` (`$18`):
/// each data unit is the difference from the one before it, and the output has exactly the source's
/// size.
fn diff_filter(cpu: &mut Cpu, bus: &mut Bus, wide: bool, half: bool) -> u32 {
    let (src, dest) = (cpu.r[0], cpu.r[1]);
    let length = stream_header(bus, src);
    let mut from = src + 4;
    let mut sink = Sink::new(dest, half);
    let mut written = 0u32;
    let mut value = 0u32;
    while written < length {
        if wide {
            value = value.wrapping_add(read16(bus, from) as i16 as u32);
            value &= 0xFFFF;
            sink.put(bus, value as u8);
            sink.put(bus, (value >> 8) as u8);
            from += 2;
            written += 2;
        } else {
            value = (value.wrapping_add(read8(bus, from) as i8 as u32)) & 0xFF;
            sink.put(bus, value as u8);
            from += 1;
            written += 1;
        }
    }
    sink.finish(bus);
    12 + length
}

// ---------------------------------------------------------------------------- sound

/// `SoundBias` (`$19`): move `SOUNDBIAS`'s level to `000h` or `200h`, keeping the register's other
/// bits. GBATEK's GBA function has no delay parameter — "GBA uses a fixed delay count of 8".
fn sound_bias(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let level = if cpu.r[0] == 0 {
        SOUNDBIAS_LOW
    } else {
        SOUNDBIAS_HIGH
    };
    let stored = read16(bus, IO_BASE + SOUNDBIAS as u32);
    write16(bus, IO_BASE + SOUNDBIAS as u32, (stored & 0xFC00) | level);
    8 * 8
}

/// `MidiKey2Freq` (`$1F`): the frequency register value for a wave played at MIDI key `mk` with the
/// fine adjustment `fp`.
///
/// The wave's own `freq` field is defined as "sampling rate x 2^((180-original MIDI key)/12)", so the
/// value for key `mk` — with `fp` in 1/256ths of a semitone — is `freq * 2^((mk-180+fp/256)/12)`.
/// The real function is integer arithmetic over the BIOS's own table; this computes the same curve,
/// and may differ from it in the last unit.
fn midi_key_2_freq(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let wave = cpu.r[0];
    let key = f64::from((cpu.r[1] & 0xFF) as u8);
    let fine = f64::from((cpu.r[2] & 0xFF) as u8);
    let freq = read32(bus, wave + 4);
    let semitones = key - 180.0 + fine / 256.0;
    let value = f64::from(freq) * 2f64.powf(semitones / 12.0);
    cpu.r[0] = value.round() as u32;
    30
}

// ---------------------------------------------------------------------------- reset and power

/// `SoftReset` (`$00`): clear the 512 bytes of work RAM that hold the stacks and the BIOS's own
/// vectors, reset the three stack pointers, zero the registers and enter System mode at the address
/// `[$03007FFA]` names — "00h=8000000h (ROM), or 01h-FFh=2000000h (RAM), entered in ARM state".
fn soft_reset(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let flag = read8(bus, 0x0300_7FFA);
    let entry = if flag == 0 { 0x0800_0000 } else { 0x0200_0000 };
    for at in 0x0300_7E00..=0x0300_7FFF {
        bus.write8(at, 0);
    }
    // GBATEK's table: "GBA 3007FE0h 3007FA0h 3007F00h" for the three stack pointers.
    cpu.r = [0; 16];
    cpu.bank_sp_lr[crate::cpu::BANK_USER] = [crate::cpu::SP_USR, 0];
    cpu.bank_sp_lr[crate::cpu::BANK_IRQ] = [crate::cpu::SP_IRQ, 0];
    cpu.spsr = [0; crate::cpu::BANK_COUNT];
    cpu.set_thumb(false);
    cpu.write_cpsr_fields(MODE_SYS | 0xC0, 0xFF);
    cpu.r[13] = crate::cpu::SP_SVC;
    cpu.r[14] = entry;
    cpu.r[15] = entry;
    120
}

/// One memory region `RegisterRamReset` clears: the flag bit that names it, where it starts, and
/// how many bytes of it go.
struct Region {
    flag: u32,
    base: u32,
    bytes: usize,
}

/// The five memories a `RegisterRamReset` clears, in the order this slice clears them: the 256K
/// on-board WRAM first and the 32K on-chip WRAM **last**.
///
/// GBATEK prints the flags and no order (`G:95585-95606`), and mGBA's `_RegisterRamReset`
/// (`src/gba/bios.c`) clears the 256K before the 32K; that pair is the one a cartridge can observe,
/// because a cartridge keeps its interrupt handler in the 32K. 1896 Robot Wars erases its own
/// handler with this call and is cut by the next V-Blank 280 540 cycles into the 256K clear, so a
/// call that erased the 32K first would take that interrupt on erased code — not something a
/// shipped game does.
const REGIONS: [Region; 5] = [
    Region {
        flag: 0x01,
        base: bus::EWRAM_BASE,
        bytes: bus::EWRAM_SIZE,
    },
    Region {
        flag: 0x08,
        base: bus::VRAM_BASE,
        bytes: bus::VRAM_SIZE,
    },
    Region {
        flag: 0x10,
        base: bus::OAM_BASE,
        bytes: bus::OAM_SIZE,
    },
    Region {
        flag: 0x04,
        base: bus::PALETTE_BASE,
        bytes: bus::PALETTE_SIZE,
    },
    Region {
        flag: 0x02,
        base: bus::IWRAM_BASE,
        // "Clear 32K on-chip WRAM ;-excluding last 200h bytes".
        bytes: bus::IWRAM_SIZE - 0x200,
    },
];

/// Zero `bytes` of the region starting at `base`.
fn clear(bus: &mut Bus, base: u32, bytes: usize) {
    match base {
        bus::EWRAM_BASE => bus.ewram.fill(0),
        bus::IWRAM_BASE => bus.iwram[..bytes].fill(0),
        bus::VRAM_BASE => bus.vram.fill(0),
        bus::OAM_BASE => bus.oam.fill(0),
        _ => bus.palette.fill(0),
    }
}

/// What clearing `bytes` of the region at `base` costs: one 32-bit CPU store per word, at the
/// region's own row of GBATEK's table ([`Bus::store_cycles`], which is also what a store
/// instruction is charged from).
fn clear_cost(bus: &mut Bus, base: u32, bytes: usize) -> u32 {
    (bytes as u32 / 4) * bus.store_cycles(base, 4)
}

/// Clear the regions `flags` names, in [`REGIONS`]' order, spending each clear's time on the
/// machine clock — and stop, parking the rest, at the first clear after which an interrupt is due.
///
/// The stop is the documented half of an `SWI`'s behaviour: its body runs in System mode with the
/// caller's `I` bit, so "in some cases the BIOS may allow interrupts to be executed from inside of
/// the SWI procedure" (GBATEK, *How BIOS Processes SWIs*), and one that arrives during a clear is
/// taken before the next clear runs.
fn clear_regions(cpu: &mut Cpu, bus: &mut Bus, flags: u32) {
    let mut rest = flags;
    for region in REGIONS {
        if rest & region.flag == 0 {
            continue;
        }
        let cost = clear_cost(bus, region.base, region.bytes);
        bus.tick(cost);
        clear(bus, region.base, region.bytes);
        rest &= !region.flag;
        if rest != 0 && cpu.pending_interrupt(bus) {
            bus.bios.reset_pending = rest;
            bus.bios.reset_pc = cpu.r[15];
            return;
        }
    }
}

/// Finish a `RegisterRamReset` an interrupt took: the regions the call had not reached are cleared
/// here — after the handler has run, before the caller's instruction after the `SWI` — and the
/// time they take is spent on the clock like any other clear's.
///
/// The parked call holds *that* address and no other, for the reason [`Bios::wait_pc`] holds the
/// caller's: a handler may take further interrupts and run BIOS code of its own, and none of that
/// is the caller coming back.
pub fn poll_reset(cpu: &mut Cpu, bus: &mut Bus) {
    if bus.bios.reset_pc == 0 || cpu.pc() != bus.bios.reset_pc {
        return;
    }
    let pending = std::mem::take(&mut bus.bios.reset_pending);
    bus.bios.reset_pc = 0;
    clear_regions(cpu, bus, pending);
}

/// `RegisterRamReset` (`$01`): clear the memories the flags name, and force a blank screen.
///
/// GBATEK, *SWI 01h (GBA) - RegisterRamReset*: "Resets the I/O registers and RAM specified in
/// ResetFlags. However, it does not clear the CPU internal RAM area from 3007E00h-3007FFFh." The
/// flags it prints are `0 Clear 256K on-board WRAM`, `1 Clear 32K on-chip WRAM ;-excluding last
/// 200h bytes`, `2 Clear Palette`, `3 Clear VRAM`, `4 Clear OAM ;-zerofilled! does NOT disable
/// OBJs!`, `5 Reset SIO registers`, `6 Reset Sound registers`, `7 Reset all other registers
/// (except SIO, Sound)`; and "The function always switches the screen into forced blank by setting
/// DISPCNT=0080h (regardless of incoming R0, screen becomes white)" (`G:95585-95606`).
///
/// **What the clear is made of.** The call's own page prints no duration; the fill it makes is the
/// BIOS's memory fill, whose shape is documented: "Memory copy/fill in units of 32 bytes. Memcopy
/// is implemented as repeated LDMIA/STMIA [Rb]!,r2-r9 instructions. Memfill as single LDR followed
/// by repeated STMIA [Rb]!,r2-r9" (GBATEK, *SWI 0Ch - CpuFastSet*, `G:95390-95392`). A fill of that
/// shape writes each word with an ordinary 32-bit CPU store, so the floor for a word of any region
/// is one store at that region's row of *Address Bus Width and CPU Read/Write Access Widths*
/// (`G:591-598`) — the cheapest documented way to clear a word (the DMA row is dearer, below):
///
/// ```text
///   Work RAM 32K  32    8/16/32   8/16/32   1/1/1
///   OAM           32    8/16/32   16/32     1/1/1 *
///   Work RAM 256K 16    8/16/32   8/16/32   3/3/6 **
///   Palette RAM   16    8/16/32   16/32     1/1/2 *
///   VRAM          16    8/16/32   16/32     1/1/2 *
/// ```
///
/// `R0` bits 0-1 therefore cost 65 536 EWRAM words at 6 plus 8 064 IWRAM words at 1 = **401 280**
/// cycles, and bits 0-4 add the video memories at their rows: 451 200 with the screen force-blanked
/// and 476 288 while a visible line draws, the difference being the table's `*`, "Plus 1 cycle if
/// GBA accesses video memory at the same time" (`G:605`).
///
/// **Why the DMA row is not cheaper.** A fill driven by the DMA unit is priced by *Transfer
/// Rate/Timing*: "For n data units, the DMA transfer time is: `2N+2(n-1)S+xI`. Of which,
/// `1N+(n-1)S` are read cycles, and the other `1N+(n-1)S` are write cycles" (`G:2532-2539`). A
/// 32-bit unit into the 16-bit EWRAM bus is 6 cycles to write, and a fixed source address is 1 to
/// read, so 65 536 units cost 65 536 × 7 + 2 = 458 754 — dearer than the CPU's 393 216. The clear
/// has nothing to read that the CPU's own fill does not read, so the store row is the cheapest
/// documented way to clear a word and it is the price used here.
///
/// **The time is spent, not returned.** Each clear advances the machine clock ([`Bus::tick`]),
/// because the call is interruptible and its interrupts have to land where they land on hardware:
/// the SWI handler "switches into System mode" carrying the caller's `I` bit (GBATEK, *How BIOS
/// Processes SWIs*, `G:94887-94890`), and "In some cases the BIOS may allow interrupts to be
/// executed from inside of the SWI procedure" (`G:94892-94894`). A V-Blank that arrives during the
/// 256K clear is therefore taken inside this call, before the last of [`REGIONS`]' clears — which
/// is the clear that erases a cartridge's IRQ handler. The remaining clears are parked in
/// [`Bios::reset_pending`] and finished by [`poll_reset`] after the handler returns.
fn register_ram_reset(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let flags = cpu.r[0];
    // "The function always switches the screen into forced blank by setting DISPCNT=0080h
    // (regardless of incoming R0, screen becomes white)" — before the clears, so the video
    // memories are written with the screen blank and pay the table's plain `1/1/2`.
    bus.io[bus::DISPCNT] = 0x80;
    bus.io[bus::DISPCNT + 1] = 0;
    clear_regions(cpu, bus, flags & 0x1F);
    // Bits 5-7 reset groups of I/O registers. GBATEK prints the groups and no order, and the clears
    // run first here as they do in mGBA's `_RegisterRamReset` (`src/gba/bios.c`). The I/O page this
    // slice owns is small enough that the honest thing is to say which of them it clears rather
    // than pretend to a full register file.
    if flags & 0x80 != 0 {
        for offset in [DISPSTAT, WAITCNT, WAITCNT + 1, IE, IE + 1] {
            bus.io[offset] = 0;
        }
        bus.io[WAITCNT] = bus::WAITCNT_POST_BOOT as u8;
        bus.io[WAITCNT + 1] = (bus::WAITCNT_POST_BOOT >> 8) as u8;
    }
    // Nothing: the clears put their cycles on the machine clock themselves (see above), so there
    // is no total left for the instruction that issued the `SWI` to spend.
    0
}

/// `Halt` (`$02`): "Halts the CPU until an interrupt request occurs… the GBA locks up if that
/// condition doesn't get true", with the wake condition `(IE AND IF)` — the CPSR's IRQ mask and
/// `IME` are "don't care" (GBATEK, *BIOS Halt Functions*).
fn halt(cpu: &mut Cpu) -> u32 {
    cpu.halted = true;
    4
}

/// `Stop` (`$03`): the same wait, in the low-power mode that also stops the clock, video and sound.
///
/// This machine has one clock and no way for a test to tell a stopped one from a halted one, so the
/// model is `Halt`'s — the honest half of it, and the half a later slice can extend.
fn stop(cpu: &mut Cpu) -> u32 {
    cpu.halted = true;
    4
}

/// `IntrWait` (`$04`) and `VBlankIntrWait` (`$05`).
///
/// `R0` is "0=Return immediately if an old flag was already set, 1=Discard old flags, wait until a
/// NEW flag becomes set", and `R1` the flags to watch. The function "forcefully sets IME=1"; it
/// returns when the flag word at `[$03007FF8]` holds one of them, and clears the ones it watched.
fn intr_wait(cpu: &mut Cpu, bus: &mut Bus, discard: u32, watched: u32) -> u32 {
    let watched = (watched & 0x3FFF) as u16;
    bus.write16(IO_BASE + IME as u32, 1);
    if discard & 0xFFFF != 0 {
        write16(bus, IRQ_FLAGS, 0);
    }
    let seen = read16(bus, IRQ_FLAGS);
    if seen & watched != 0 {
        write16(bus, IRQ_FLAGS, seen & !watched);
        return 20;
    }
    bus.bios.wait = Some(watched);
    bus.bios.wait_pc = cpu.r[15];
    cpu.halted = true;
    20
}

// ---------------------------------------------------------------------------- the bus, as the
// ---------------------------------------------------------------------------- BIOS touches it

/// A word read the way a BIOS call makes one: through the bus, so that the access is charged to the
/// call and the open-bus latch behaves as it does for the CPU.
fn read32(bus: &mut Bus, addr: u32) -> u32 {
    bus.read32(addr)
}

/// A halfword read from a BIOS call.
fn read16(bus: &mut Bus, addr: u32) -> u16 {
    bus.read16(addr)
}

/// A byte read from a BIOS call.
fn read8(bus: &mut Bus, addr: u32) -> u8 {
    bus.read8(addr)
}

/// A word write from a BIOS call.
fn write32(bus: &mut Bus, addr: u32, value: u32) {
    bus.write32(addr, value);
}

/// A halfword write from a BIOS call.
fn write16(bus: &mut Bus, addr: u32, value: u16) {
    bus.write16(addr, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bios_latch_has_the_four_documented_words() {
        // The four opcodes GBATEK's *Reading from BIOS Memory* names by address, in the order the
        // public suite reads them: boot, during IRQ, after IRQ, after SWI.
        assert_eq!(OPCODE_AFTER_BOOT, 0xE129_F000);
        assert_eq!(OPCODE_DURING_IRQ, 0xE25E_F004, "subs r15,r14,4h");
        assert_eq!(OPCODE_AFTER_IRQ, 0xE55E_C002);
        assert_eq!(OPCODE_AFTER_SWI, 0xE3A0_2004, "mov r2,4h");
    }

    #[test]
    fn a_zero_angle_is_an_exact_identity() {
        // Scaling by 1.0 (256) with no rotation: pa = pd = 256, pb = pc = 0.
        assert_eq!(affine(256, 256, 0), [256, 0, 0, 256]);
        // And the sine table's quarter turn is (256, 0) at 90 degrees.
        assert_eq!(sin_cos(0x4000), (256, 0));
        assert_eq!(sin_cos(0x0000), (0, 256));
        // The low byte of the angle is ignored, as GBATEK says the BIOS ignores it.
        assert_eq!(sin_cos(0x40FF), sin_cos(0x4000));
    }

    #[test]
    fn the_state_round_trips() {
        let mut bios = Bios::new();
        bios.calls[6] = 3;
        bios.unimplemented = Some(0x25);
        bios.wait = Some(1);
        let bytes = bios.encode();
        let mut restored = Bios::new();
        restored.decode(&bytes);
        assert_eq!(restored.latch, OPCODE_AFTER_BOOT);
        assert_eq!(restored.calls[6], 3);
        assert_eq!(restored.unimplemented, Some(0x25));
        assert_eq!(restored.wait, Some(1));
    }
}
