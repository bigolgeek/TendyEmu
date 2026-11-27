//! The SM83: registers, flags and the step entry point.
//!
//! `step` is the only entry point: it ticks once per machine cycle it spends — through
//! `Bus::read`, `Bus::write` and `Bus::tick`, never by advancing a clock of its own (§2) — and
//! returns that count. The ALU lives in [`alu`], the opcode dispatch and the cycle tables in
//! [`opcodes`].

use crate::bus::Bus;
use crate::state::{StateError, StateReader, StateWriter};

pub mod alu;
pub mod opcodes;

/// The four flags, in F's high nibble.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(pub u8);

impl Flags {
    pub const Z: u8 = 0x80;
    pub const N: u8 = 0x40;
    pub const H: u8 = 0x20;
    pub const C: u8 = 0x10;

    pub fn z(self) -> bool {
        self.0 & Flags::Z != 0
    }

    pub fn set_z(&mut self, on: bool) {
        self.0 = if on {
            self.0 | Flags::Z
        } else {
            self.0 & !Flags::Z
        };
    }

    pub fn n(self) -> bool {
        self.0 & Flags::N != 0
    }

    pub fn set_n(&mut self, on: bool) {
        self.0 = if on {
            self.0 | Flags::N
        } else {
            self.0 & !Flags::N
        };
    }

    pub fn h(self) -> bool {
        self.0 & Flags::H != 0
    }

    pub fn set_h(&mut self, on: bool) {
        self.0 = if on {
            self.0 | Flags::H
        } else {
            self.0 & !Flags::H
        };
    }

    pub fn c(self) -> bool {
        self.0 & Flags::C != 0
    }

    pub fn set_c(&mut self, on: bool) {
        self.0 = if on {
            self.0 | Flags::C
        } else {
            self.0 & !Flags::C
        };
    }
}

/// The CPU register file and its four control latches.
#[derive(Clone)]
pub struct Cpu {
    pub a: u8,
    pub f: Flags,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
    pub ime: bool,
    /// EI arms this; the next instruction runs before IME goes true.
    pub ime_pending: bool,
    pub halted: bool,
    /// The next opcode byte is read twice (`halt.html`).
    pub halt_bug: bool,
    /// STOP, low-power halt until a P10-P13 line reads low.
    pub stopped: bool,
    /// An illegal opcode: `step` returns 1 M-cycle forever.
    pub locked: bool,
}

impl Cpu {
    /// All zero, PC = `$0000`.
    pub fn new() -> Cpu {
        Cpu {
            a: 0,
            f: Flags(0),
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            sp: 0,
            pc: 0,
            ime: false,
            ime_pending: false,
            halted: false,
            halt_bug: false,
            stopped: false,
            locked: false,
        }
    }

    /// Post-boot: A=01 B=00 C=13 D=00 E=D8 H=01 L=4D SP=FFFE PC=0100, and F from the cartridge's
    /// `$014D` byte — `$00` gives `$80`, anything else `$B0`.
    pub fn post_boot(header_checksum: u8) -> Cpu {
        Cpu::post_boot_at(header_checksum, 0x0100)
    }

    /// [`Cpu::post_boot`] with the entry address chosen by the cartridge: every licensed cartridge
    /// starts at `$0100`, the unlicensed boards whose images carry no logo at `$0000` (§5.5).
    pub fn post_boot_at(header_checksum: u8, pc: u16) -> Cpu {
        Cpu {
            a: 0x01,
            f: Flags(if header_checksum == 0 { 0x80 } else { 0xB0 }),
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc,
            ..Cpu::new()
        }
    }

    /// The register file the CGB hands a cartridge at `$0100`. Pan Docs (*CGB Registers*,
    /// "Detecting CGB (and GBA) functions") makes A = `$11` the way a cartridge knows it is on a
    /// CGB — and B's bit 0 the way it tells a CGB from a GBA, so B = 0 here. The rest is what the
    /// CGB boot ROM leaves in the registers (`F` = `$80`, `DE` = `$FF56`, `HL` = `$000D`); the
    /// cartridge's `$014D` checksum does not reach `F` on a CGB, because the CGB boot ROM computes
    /// it but jumps to `$0100` with its own flags. C4 replaces this with our own boot ROM; until
    /// then this is the post-boot state CGB code is written against.
    pub fn post_boot_cgb(pc: u16) -> Cpu {
        Cpu {
            a: 0x11,
            f: Flags(0x80),
            b: 0x00,
            c: 0x00,
            d: 0xFF,
            e: 0x56,
            h: 0x00,
            l: 0x0D,
            sp: 0xFFFE,
            pc,
            ..Cpu::new()
        }
    }

    /// The register file the SGB's boot ROM leaves at `$0100`. Pan Docs, *Power Up
    /// Sequence*, the CPU registers table's **SGB** column: "A $01, F Z=0 N=0 H=0 C=0, B $00,
    /// C $14, D $00, E $00, H $C0, L $60, PC $0100, SP $FFFE".
    ///
    /// SGB is the one monochrome console whose `F` is fixed: the SGB boot ROM "performs no checks",
    /// so unlike the DMG's it never computed the header checksum, and A = `$01` is passed through
    /// the same `$FF50` write as the DMG's rather than the MGB's `$FF`. `A`, `B`, `D`, `PC` and `SP`
    /// are the DMG's values too; `C`, `F`, `E`, `H` and `L` are not.
    pub fn post_boot_sgb(pc: u16) -> Cpu {
        Cpu {
            a: 0x01,
            f: Flags(0x00),
            b: 0x00,
            c: 0x14,
            d: 0x00,
            e: 0x00,
            h: 0xC0,
            l: 0x60,
            sp: 0xFFFE,
            pc,
            ..Cpu::new()
        }
    }

    /// Executes one instruction (or one halt cycle, or one interrupt dispatch), ticking once per
    /// M-cycle it spends. The return value is that count (§2 rule 3), measured from `Bus::cycles`
    /// so it can never disagree with the ticks that actually happened.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        let start = bus.cycles;
        self.step_once(bus);
        bus.cycles.wrapping_sub(start) as u32
    }

    /// §3.4's five cases, in order.
    fn step_once(&mut self, bus: &mut Bus) {
        // Published for the VRAM DMA unit, which holds its HBlank blocks while the CPU is halted
        // (Pan Docs, *CGB Registers*, "Upon halting the CPU ... the transfer will also be halted").
        bus.cpu_halted = self.halted;
        if self.locked {
            bus.tick();
            return;
        }
        if bus.vdma.halting() {
            // A VRAM DMA block owns the bus: "The execution of the program is halted during the
            // separate transfers, but the program execution continues during the 'spaces' between
            // each data block." One M-cycle passes; no fetch, no dispatch.
            bus.tick();
            return;
        }
        if bus.cgb.switching() {
            // The CGB's speed switch holds the CPU for 2 050 machine cycles (Pan Docs, *FF4D —
            // KEY1/SPD*). `Bus::tick` runs the pause out and flips the speed; no interrupt is
            // dispatched and no instruction fetch happens until it is over.
            bus.tick();
            return;
        }
        if self.stopped {
            bus.tick();
            // STOP ends when a P10-P13 line goes low (§2). `Joypad::read` is the matrix the ROM
            // sees, so a button the currently selected row does not cover cannot wake it.
            if bus.joypad.read() & 0x0F != 0x0F {
                self.stopped = false;
            }
            return;
        }
        let pending = bus.pending();
        if self.ime && pending != 0 {
            // The dispatch re-samples the request after its upper-byte push: `ie_push` needs that
            // push to be able to cancel the dispatch.
            self.service_interrupt(bus);
            return;
        }
        if self.halted {
            bus.tick();
            // Sampled after the tick: a flag raised by the PPU or timer work that ran during this
            // idle M-cycle must wake the CPU on that same cycle. Re-testing the `pending` taken
            // *before* the tick left `halt_ime0_nointr_timing` waking one M-cycle late.
            if bus.pending() != 0 {
                self.halted = false;
            }
            return;
        }
        let arm_ime = self.ime_pending;
        let op = self.fetch_opcode(bus);
        if opcodes::is_illegal(op) {
            // SPEC R1: an illegal opcode locks the CPU. `locked` is the report: it stays set so the
            // front end can say so once, and `step` then costs one M-cycle forever.
            self.locked = true;
            return;
        }
        self.execute(bus, op);
        if arm_ime && self.ime_pending {
            // EI's IME is armed only after the instruction that follows it has run (§3.4).
            self.ime = true;
            self.ime_pending = false;
        }
    }

    /// Five M-cycles, the lowest pending bit.
    pub fn service_interrupt(&mut self, bus: &mut Bus) -> u32 {
        let start = bus.cycles;
        self.ime = false;
        self.ime_pending = false;
        self.halted = false;
        // The dispatch takes the place of the byte the halt bug was sitting on: `halt.html`'s
        // `ei; halt` case services the handler and returns to the `halt` itself, so the handler's
        // first opcode is read once and PC advances normally. Leaving the latch set made the
        // handler's first byte be re-read as its own operand.
        self.halt_bug = false;
        bus.tick();
        bus.tick();
        let pc = self.pc;
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, (pc >> 8) as u8);
        // The request is sampled after the *upper* byte push — not after both. `ie_push` round 3
        // puts `SP` at `$0001`, so the upper push lands in ROM but the lower push writes `IE`;
        // the ROM expects that write to be "too late to cancel the interrupt dispatch" and the
        // SERIAL handler to run, so the choice made here is latched. Round 1 (`SP=$0000`, upper
        // push over `IE=$04` with `$02`) and round 4 (upper push over `IE=$03` with `$02`) both
        // expect this sample to decide the vector: round 1 finds nothing pending and resumes at
        // `$0000` with `IF` untouched, round 4 dispatches bit 1 (STAT) and clears only that bit.
        let pending = bus.ie & bus.iflag & 0x1F;
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, pc as u8);
        if pending != 0 {
            let bit = pending.trailing_zeros() as u8;
            bus.iflag &= !(1u8 << (bit & 7));
            self.pc = 0x40 + (bit as u16 & 0x1F) * 8;
        } else {
            self.pc = 0x0000;
        }
        bus.tick();
        bus.cycles.wrapping_sub(start) as u32
    }

    pub fn af(&self) -> u16 {
        (self.a as u16) << 8 | self.f.0 as u16
    }

    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f = Flags((v & 0xF0) as u8);
    }

    pub fn bc(&self) -> u16 {
        (self.b as u16) << 8 | self.c as u16
    }

    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }

    pub fn de(&self) -> u16 {
        (self.d as u16) << 8 | self.e as u16
    }

    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }

    pub fn hl(&self) -> u16 {
        (self.h as u16) << 8 | self.l as u16
    }

    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.a);
        w.u8(self.f.0);
        w.u8(self.b);
        w.u8(self.c);
        w.u8(self.d);
        w.u8(self.e);
        w.u8(self.h);
        w.u8(self.l);
        w.u16(self.sp);
        w.u16(self.pc);
        w.bool(self.ime);
        w.bool(self.ime_pending);
        w.bool(self.halted);
        w.bool(self.halt_bug);
        w.bool(self.stopped);
        w.bool(self.locked);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.a = r.u8()?;
        self.f = Flags(r.u8()?);
        self.b = r.u8()?;
        self.c = r.u8()?;
        self.d = r.u8()?;
        self.e = r.u8()?;
        self.h = r.u8()?;
        self.l = r.u8()?;
        self.sp = r.u16()?;
        self.pc = r.u16()?;
        self.ime = r.bool()?;
        self.ime_pending = r.bool()?;
        self.halted = r.bool()?;
        self.halt_bug = r.bool()?;
        self.stopped = r.bool()?;
        self.locked = r.bool()?;
        Ok(())
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}
