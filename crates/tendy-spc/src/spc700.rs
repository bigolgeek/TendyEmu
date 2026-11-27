//! The SPC700: its register file, its status byte, the bus one instruction can touch and the 256
//! opcodes. Owned by the APU lane.
//!
//! Sources: fullsnes "SNES APU SPC700 CPU Overview" and its three command tables (the `Clk` column is
//! the cycle count), "SNES APU SPC700 Load/Store Commands", the ALU/bit/jump tables, "SNES APU
//! SPC700 I/O Ports" and "SNES APU Memory and I/O Map".

use crate::APU_RAM_SIZE;
use crate::dsp::Dsp;
use crate::state::{StateRead, StateWrite};

/// The SPC700 status register. Bit 5 is the direct-page select, the one bit a 6502 programmer does
/// not expect.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Psw(pub u8);

impl Psw {
    pub const N: u8 = 0x80;
    pub const V: u8 = 0x40;
    pub const P: u8 = 0x20;
    pub const B: u8 = 0x10;
    pub const H: u8 = 0x08;
    pub const I: u8 = 0x04;
    pub const Z: u8 = 0x02;
    pub const C: u8 = 0x01;

    pub fn n(self) -> bool {
        self.0 & Psw::N != 0
    }
    pub fn set_n(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::N, on);
    }
    pub fn v(self) -> bool {
        self.0 & Psw::V != 0
    }
    pub fn set_v(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::V, on);
    }
    pub fn p(self) -> bool {
        self.0 & Psw::P != 0
    }
    pub fn set_p(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::P, on);
    }
    pub fn b(self) -> bool {
        self.0 & Psw::B != 0
    }
    pub fn set_b(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::B, on);
    }
    pub fn h(self) -> bool {
        self.0 & Psw::H != 0
    }
    pub fn set_h(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::H, on);
    }
    pub fn i(self) -> bool {
        self.0 & Psw::I != 0
    }
    pub fn set_i(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::I, on);
    }
    pub fn z(self) -> bool {
        self.0 & Psw::Z != 0
    }
    pub fn set_z(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::Z, on);
    }
    pub fn c(self) -> bool {
        self.0 & Psw::C != 0
    }
    pub fn set_c(&mut self, on: bool) {
        set_bit(&mut self.0, Psw::C, on);
    }
}

fn set_bit(byte: &mut u8, bit: u8, on: bool) {
    if on {
        *byte |= bit;
    } else {
        *byte &= !bit;
    }
}

/// How many events the SPC700's diagnosis ring can hold.
///
/// A frame is 17 038 SPC cycles, i.e. ~8 500 instructions, so this keeps the newest fifteen frames
/// of program flow — enough to see a driver's whole command loop, not just the instruction it is
/// standing on when the harness asks.
pub const TRACE_LEN: usize = 131_072;

/// How many `$F4`-`$F7` writes one event can carry: two, because `MOVW $F4,YA` (opcode `$FA`) is the
/// only way an instruction writes more than one of them.
pub const TRACE_PORTS: usize = 2;

/// `$4C`, the DSP's `KON` register (fullsnes "DSP: Key On").
pub const DSP_KON: u8 = 0x4C;

/// `$5C`, the DSP's `KOFF` register (fullsnes "DSP: Key Off").
pub const DSP_KOFF: u8 = 0x5C;

/// Which side of a `$F4`-`$F7` latch a write came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PortSide {
    /// The S-CPU wrote `$214x`: the SPC reads it at `$F4`-`$F7`.
    #[default]
    Scpu,
    /// The SPC wrote `$F4`-`$F7`: the S-CPU reads it at `$214x`.
    Spc,
}

impl PortSide {
    /// The word the diagnosis harness prints.
    pub fn name(self) -> &'static str {
        match self {
            PortSide::Scpu => "scpu",
            PortSide::Spc => "spc",
        }
    }
}

/// One write to a `$F4`-`$F7` latch, from either side.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PortWrite {
    pub side: PortSide,
    /// 0-3: `$F4`/`$2140` … `$F7`/`$2143`.
    pub index: u8,
    pub value: u8,
}

/// Which kind of event a [`SpcEvent`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EventKind {
    /// One instruction the SPC700 executed. While `$F1` bit 7 is set and `PC` is in the window,
    /// this is one pass of the boot ROM's own loop (`Apu::ipl_step`), not a RAM instruction.
    #[default]
    Step,
    /// One write the S-CPU made to `$2140`-`$2143`, recorded between steps because the S-CPU does
    /// not share the SPC's clock: the boot ROM reads the latches from software, so *when* the byte
    /// arrived relative to the SPC's own reads is the question this row answers.
    ScpuPort,
}

impl EventKind {
    /// The word the diagnosis harness prints.
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Step => "step",
            EventKind::ScpuPort => "scpu-port",
        }
    }
}

/// One row of the SPC700 diagnosis ring.
///
/// Recorded only while [`Spc700::enable_trace`] has been called — a harness, never the frame path —
/// so a machine that is not being traced is unchanged. `pc` and `bytes` are read **before** the
/// instruction; the registers and the timers are read **after** it, so a row is "here is what ran,
/// and here is what the machine looked like once it had".
#[derive(Clone, Copy, Default)]
pub struct SpcEvent {
    pub kind: EventKind,
    /// Set by the harness that owns the ring; the SPC does not know what a frame is.
    pub frame: u32,
    /// `pc` before the instruction, and the three bytes there — the opcode and its two possible
    /// operands. Operand lengths are not re-implemented: a row names the bytes the PC saw.
    pub pc: u16,
    pub bytes: [u8; 3],
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub psw: u8,
    /// `$F1`: bit 7 the boot-ROM window, bits 0-2 the timer enables.
    pub control: u8,
    /// `$FD`-`$FF`, the timer outputs, as the SPC would read them.
    pub t0out: u8,
    pub t1out: u8,
    pub t2out: u8,
    /// `$FA`-`$FC`, the timer prescalers.
    pub t0div: u8,
    pub t1div: u8,
    pub t2div: u8,
    /// The `$F4`-`$F7` writes this event made, and how many of the two slots are used.
    pub ports: [PortWrite; TRACE_PORTS],
    pub port_count: u8,
    /// A DSP `KON` (`$4C`) or `KOFF` (`$5C`) write this step made: the register and the value, with
    /// `$FF` in `dsp_reg` when the step made none (`$80` and above are no DSP register).
    pub dsp_reg: u8,
    pub dsp_value: u8,
    /// SPC700 cycles retired when the event was recorded.
    pub cycles: u64,
}

impl SpcEvent {
    /// The port writes this event carries.
    pub fn port_writes(&self) -> &[PortWrite] {
        &self.ports[..usize::from(self.port_count).min(TRACE_PORTS)]
    }
}

/// The SPC700's register file.
#[derive(Clone)]
pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub psw: Psw,
    /// `$F2`, the DSP register index.
    pub dspaddr: u8,
    /// `$F0`.
    pub test: u8,
    /// `$F1`: bit 7 = IPL ROM enabled, 4-0 timer enables.
    pub control: u8,
    /// `$FA`-`$FC`, the timer prescalers.
    pub t0div: u8,
    pub t1div: u8,
    pub t2div: u8,
    /// `$FD`-`$FF`, the 4-bit counters.
    pub t0out: u8,
    pub t1out: u8,
    pub t2out: u8,
    /// Which of the DSP's 32 clock slots the timers are at.
    pub stage: u8,
    /// The down-counters behind `t0div`-`t2div`: §7.1 declares the dividers but a timer needs both
    /// the reload value and the countdown (DESIGN amendment).
    pub t0pre: u8,
    pub t1pre: u8,
    pub t2pre: u8,
    pub cycles: u64,
    /// The diagnosis ring. `None` until a harness calls
    /// [`Spc700::enable_trace`]; every recording site checks it, so a machine that is not being
    /// traced is unchanged. Not saved: it is not machine state.
    pub trace: Option<Box<[SpcEvent]>>,
    /// Events recorded since `enable_trace`, including any the ring has overwritten.
    pub trace_len: u64,
    /// Set by the harness that owns the ring; the SPC does not know what a frame is.
    pub trace_frame: u32,
    /// The `$F4`-`$F7` writes the instruction being stepped has made, staged for the ring by
    /// [`Spc700::stage_port`]. Two bytes and a count, so the staging costs no allocation and no
    /// branch on a path whose writer already knows it is writing a port.
    step_ports: [PortWrite; TRACE_PORTS],
    step_port_count: u8,
    /// The DSP `KON`/`KOFF` write the instruction being stepped has made: `$FF` for none.
    step_dsp_reg: u8,
    step_dsp_value: u8,
}

/// Everything one instruction can touch, built by `Apu::tick` from its own fields so no borrow of
/// the whole `Apu` escapes and the DSP and RAM stay disjoint.
pub struct SpcBus<'a> {
    pub ram: &'a mut [u8; APU_RAM_SIZE],
    pub dsp: &'a mut Dsp,
    /// What the S-CPU last wrote to `$2140-$2143`: the input latches the SPC reads at `$F4-$F7`.
    /// Mutable because `$F1` bits 4/5 reset those latches to `$00`.
    pub to_apu: &'a mut [u8; 4],
    /// What the SPC writes here is what the S-CPU reads at `$2140-$2143`.
    pub from_apu: &'a mut [u8; 4],
}

/// The 12 operand forms of the six 8-bit ALU groups, from fullsnes's ALU table (`x+08`, `x+06`,
/// `x+04`, `x+14`, `x+05`, `x+15`, `x+16`, `x+17`, `x+07`, `x+09`, `x+18`, `x+19`).
const ALU_FORMS: [u8; 12] = [
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
];

// `new()` is the constructor §7.1 fixes; a `Default` impl is not part of the design's surface.
#[allow(clippy::new_without_default)]
impl Spc700 {
    /// Power-on: `PC` from the boot ROM's reset vector, `SP = $EF`, `PSW = $02`, `$F1` = `$B0`
    /// (ROM enabled, every timer disabled — fullsnes `$F1`: "On power on or reset, it seems to be
    /// set to B0h"), `TEST = $0A` (fullsnes: "Default setting is 0Ah").
    pub fn new() -> Spc700 {
        Spc700 {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xEF,
            pc: 0xFFC0,
            psw: Psw(Psw::Z),
            dspaddr: 0,
            test: 0x0A,
            control: 0xB0,
            t0div: 0,
            t1div: 0,
            t2div: 0,
            t0out: 0,
            t1out: 0,
            t2out: 0,
            stage: 0,
            t0pre: 0,
            t1pre: 0,
            t2pre: 0,
            cycles: 0,
            trace: None,
            trace_len: 0,
            trace_frame: 0,
            step_ports: [PortWrite::default(); TRACE_PORTS],
            step_port_count: 0,
            step_dsp_reg: 0xFF,
            step_dsp_value: 0,
        }
    }

    /// One instruction, charging its `Clk` value. Returns the cycles.
    ///
    /// While `PC` is inside the boot ROM with `$F1` bit 7 set, the ROM is the IPL handshake — a state
    /// machine in `Apu` (§7.3) — so the SPC waits there and the ROM's own 2-cycle instruction is what
    /// is charged.
    pub fn step(&mut self, bus: &mut SpcBus<'_>) -> u32 {
        if self.pc >= 0xFFC0 && self.control & 0x80 != 0 {
            return 2;
        }
        let opcode = self.fetch(bus);
        let base = opcode & 0xE0;
        let form = opcode & 0x1F;
        if matches!(base, 0x00 | 0x20 | 0x40 | 0x60 | 0x80 | 0xA0) && ALU_FORMS.contains(&form) {
            return self.alu(bus, base, form);
        }
        match opcode {
            0x00 => 2, // NOP
            0x0F => self.brk(bus),
            // CLRP / SETP / CLRC / SETC / EI / DI / CLRV / NOTC / XCN / DAA / DAS / DIV / MUL.
            0x20 => {
                self.psw.set_p(false);
                2
            }
            0x40 => {
                self.psw.set_p(true);
                2
            }
            0x60 => {
                self.psw.set_c(false);
                2
            }
            0x80 => {
                self.psw.set_c(true);
                2
            }
            0xA0 => {
                self.psw.set_i(true);
                3
            }
            0xC0 => {
                self.psw.set_i(false);
                3
            }
            0xE0 => {
                self.psw.set_v(false);
                self.psw.set_h(false);
                2
            }
            0xED => {
                let c = self.psw.c();
                self.psw.set_c(!c);
                3
            }
            0x9F => {
                let a = self.a.rotate_left(4);
                self.a = a;
                self.nz(a);
                5
            }
            0xDF => {
                self.daa();
                3
            }
            0xBE => {
                self.das();
                3
            }
            0x9E => self.div(),
            0xCF => self.mul(),
            0xEF | 0xFF => {
                // SLEEP/STOP halt the processor (fullsnes: "will hang the CPU forever").
                self.pc = self.pc.wrapping_sub(1);
                3
            }

            // --- loads and stores -------------------------------------------------------
            0xE8 => {
                let v = self.fetch(bus);
                self.a = v;
                self.nz(v);
                2
            }
            0xCD => {
                let v = self.fetch(bus);
                self.x = v;
                self.nz(v);
                2
            }
            0x8D => {
                let v = self.fetch(bus);
                self.y = v;
                self.nz(v);
                2
            }
            0x7D => {
                self.a = self.x;
                let v = self.a;
                self.nz(v);
                2
            }
            0x5D => {
                self.x = self.a;
                let v = self.a;
                self.nz(v);
                2
            }
            0xDD => {
                self.a = self.y;
                let v = self.a;
                self.nz(v);
                2
            }
            0xFD => {
                self.y = self.a;
                let v = self.a;
                self.nz(v);
                2
            }
            0x9D => {
                self.x = self.sp;
                let v = self.x;
                self.nz(v);
                2
            }
            // fullsnes "SNES APU SPC700 Load/Store Commands": `MOV SP,X BD 2 SP=X`. A driver that
            // wants its stack out of the way of a RAM clear moves it here; treating this as a no-op
            // leaves SP at the IPL's `$EF`, so a later `CALL`'s return address lands at `$01ED-$01EF`
            // — inside the region a clear loop zeroes — and the `RET` pops `$0000`.
            0xBD => {
                self.sp = self.x;
                2
            }
            0xE4 => {
                let addr = self.fetch_dp(bus);
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                3
            }
            0xF4 => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                4
            }
            0xE5 => {
                let addr = self.fetch16(bus);
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                4
            }
            0xF5 => {
                let addr = self.fetch16(bus).wrapping_add(u16::from(self.x));
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                5
            }
            0xF6 => {
                let addr = self.fetch16(bus).wrapping_add(u16::from(self.y));
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                5
            }
            0xE6 => {
                // (X): the 8-bit index selects a byte of the direct page the P flag names.
                let addr = self.dp_addr(self.x);
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                3
            }
            0xBF => {
                // MOV A,(X)+ — same page rule, and the index wraps inside that page.
                let addr = self.dp_addr(self.x);
                self.x = self.x.wrapping_add(1);
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                4
            }
            0xF7 => {
                let base = self.fetch_dp(bus);
                let ptr = self.read16(bus, base);
                let addr = ptr.wrapping_add(u16::from(self.y));
                let v = self.read(bus, addr);
                self.a = v;
                self.nz(v);
                6
            }
            0xE7 => {
                let base = self.fetch_dp_indexed(bus, self.x);
                let ptr = self.read16(bus, base);
                let v = self.read(bus, ptr);
                self.a = v;
                self.nz(v);
                6
            }
            0xF8 => {
                let addr = self.fetch_dp(bus);
                let v = self.read(bus, addr);
                self.x = v;
                self.nz(v);
                3
            }
            0xF9 => {
                let addr = self.fetch_dp_indexed(bus, self.y);
                let v = self.read(bus, addr);
                self.x = v;
                self.nz(v);
                4
            }
            0xE9 => {
                let addr = self.fetch16(bus);
                let v = self.read(bus, addr);
                self.x = v;
                self.nz(v);
                4
            }
            0xEB => {
                let addr = self.fetch_dp(bus);
                let v = self.read(bus, addr);
                self.y = v;
                self.nz(v);
                3
            }
            0xFB => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                let v = self.read(bus, addr);
                self.y = v;
                self.nz(v);
                4
            }
            0xEC => {
                let addr = self.fetch16(bus);
                let v = self.read(bus, addr);
                self.y = v;
                self.nz(v);
                4
            }
            0xBA => {
                // MOVW YA,dp — fullsnes "16bit ALU Operations": `MOVW YA,aa  MOVW YA,[aa]  BA aa  5
                // ...  N.....Z.`; the wiki's row is `MOVW YA, dp | BA | 2 | 5 | N.....Z.`. A 16-bit
                // load, so N is bit 15 of the word and Z is the whole word being zero — not Y's
                // byte, which left Z set for a word like `$0034`.
                let addr = self.fetch_dp(bus);
                let v = self.read16(bus, addr);
                self.a = v as u8;
                self.y = (v >> 8) as u8;
                self.nz16(v);
                5
            }
            0x8F => {
                let v = self.fetch(bus);
                let addr = self.fetch_dp(bus);
                self.read(bus, addr); // the store's dummy read
                self.write(bus, addr, v);
                5
            }
            0xFA => {
                let src = self.fetch_dp(bus);
                let dst = self.fetch_dp(bus);
                let v = self.read(bus, src);
                self.write(bus, dst, v);
                5
            }
            0xC4 => {
                let addr = self.fetch_dp(bus);
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                4
            }
            0xD8 => {
                let addr = self.fetch_dp(bus);
                self.read(bus, addr);
                let x = self.x;
                self.write(bus, addr, x);
                4
            }
            0xCB => {
                let addr = self.fetch_dp(bus);
                self.read(bus, addr);
                let y = self.y;
                self.write(bus, addr, y);
                4
            }
            0xD4 => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                5
            }
            0xDB => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                self.read(bus, addr);
                let y = self.y;
                self.write(bus, addr, y);
                5
            }
            0xD9 => {
                let addr = self.fetch_dp_indexed(bus, self.y);
                self.read(bus, addr);
                let x = self.x;
                self.write(bus, addr, x);
                5
            }
            0xC5 => {
                let addr = self.fetch16(bus);
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                5
            }
            0xC9 => {
                let addr = self.fetch16(bus);
                self.read(bus, addr);
                let x = self.x;
                self.write(bus, addr, x);
                5
            }
            0xCC => {
                let addr = self.fetch16(bus);
                self.read(bus, addr);
                let y = self.y;
                self.write(bus, addr, y);
                5
            }
            0xD5 => {
                let addr = self.fetch16(bus).wrapping_add(u16::from(self.x));
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                6
            }
            0xD6 => {
                let addr = self.fetch16(bus).wrapping_add(u16::from(self.y));
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                6
            }
            0xAF => {
                // MOV (X)+,A: no dummy read, unlike the other stores, and the same page rule.
                let addr = self.dp_addr(self.x);
                let x = self.x.wrapping_add(1);
                let a = self.a;
                self.write(bus, addr, a);
                self.x = x;
                4
            }
            0xC6 => {
                // MOV (X),A — fullsnes "Addressing Modes": `(X) [X] ; (or at [0100..01FF] when
                // flag P=1)`. Writing page 0 regardless sends the store to a byte the program
                // never named: a driver that clears its own variable with P set writes its
                // command byte instead.
                let addr = self.dp_addr(self.x);
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                4
            }
            0xD7 => {
                let base = self.fetch_dp(bus);
                let ptr = self.read16(bus, base);
                let addr = ptr.wrapping_add(u16::from(self.y));
                self.read(bus, addr);
                let a = self.a;
                self.write(bus, addr, a);
                7
            }
            0xC7 => {
                let base = self.fetch_dp_indexed(bus, self.x);
                let ptr = self.read16(bus, base);
                self.read(bus, ptr);
                let a = self.a;
                self.write(bus, ptr, a);
                7
            }
            0xDA => {
                let addr = self.fetch_dp(bus);
                self.read(bus, addr); // the dummy read of the LSB only
                let a = self.a;
                let y = self.y;
                self.write(bus, addr, a);
                self.write(bus, addr.wrapping_add(1), y);
                5
            }

            // --- push and pop -----------------------------------------------------------
            0x2D => {
                let a = self.a;
                self.push(bus, a);
                4
            }
            0x4D => {
                let x = self.x;
                self.push(bus, x);
                4
            }
            0x6D => {
                let y = self.y;
                self.push(bus, y);
                4
            }
            0x0D => {
                let p = self.psw.0;
                self.push(bus, p);
                4
            }
            // "POP A (X,Y, PSW)" — the manual's flag column is all dots for `POP A`/`POP X`/
            // `POP Y` (snesmusic's `spc700_apu_manual.txt`: "POP A AE 1 4 pop A from stack ........";
            // SNESdev wiki, "SPC-700 instruction set", "Differences from the 6502": "POP instructions
            // do not modify flags (except POP PSW)"). Only `POP PSW` restores flags. A driver that
            // does `PUSH A : MOV A,$48 : AND A,$1D : POP A : BNE` — Nintendo's sound drivers do
            // exactly this to test a mute mask while keeping the value in A — reads the AND's Z flag;
            // setting N/Z from the popped byte makes that branch test the value instead.
            0xAE => {
                self.a = self.pop(bus);
                4
            }
            0xCE => {
                self.x = self.pop(bus);
                4
            }
            0xEE => {
                self.y = self.pop(bus);
                4
            }
            0x8E => {
                self.psw = Psw(self.pop(bus));
                4
            }

            // --- increment / decrement / shift -----------------------------------------
            0x1C | 0x3C | 0x5C | 0x7C => {
                let value = self.a;
                let (result, carry) = shift_by(opcode & 0xE0, value, self.psw.c());
                self.a = result;
                if let Some(c) = carry {
                    self.psw.set_c(c);
                }
                self.nz(result);
                2
            }
            0x9C => {
                self.a = self.a.wrapping_sub(1);
                let v = self.a;
                self.nz(v);
                2
            }
            0xBC => {
                self.a = self.a.wrapping_add(1);
                let v = self.a;
                self.nz(v);
                2
            }

            // --- compares against X and Y ------------------------------------------------
            0xC8 => {
                let v = self.fetch(bus);
                self.cmp_reg(self.x, v);
                2
            }
            0xAD => {
                let v = self.fetch(bus);
                self.cmp_reg(self.y, v);
                2
            }
            0x3E => {
                let addr = self.fetch_dp(bus);
                let v = self.read(bus, addr);
                self.cmp_reg(self.x, v);
                3
            }
            0x7E => {
                let addr = self.fetch_dp(bus);
                let v = self.read(bus, addr);
                self.cmp_reg(self.y, v);
                3
            }
            0x1E => {
                let addr = self.fetch16(bus);
                let v = self.read(bus, addr);
                self.cmp_reg(self.x, v);
                4
            }
            0x5E => {
                let addr = self.fetch16(bus);
                let v = self.read(bus, addr);
                self.cmp_reg(self.y, v);
                4
            }
            0x3D => {
                self.x = self.x.wrapping_add(1);
                let v = self.x;
                self.nz(v);
                2
            }
            0x1D => {
                self.x = self.x.wrapping_sub(1);
                let v = self.x;
                self.nz(v);
                2
            }
            0xFC => {
                self.y = self.y.wrapping_add(1);
                let v = self.y;
                self.nz(v);
                2
            }
            0xDC => {
                self.y = self.y.wrapping_sub(1);
                let v = self.y;
                self.nz(v);
                2
            }
            0x0B | 0x2B | 0x4B | 0x6B | 0x8B | 0xAB => {
                let addr = self.fetch_dp(bus);
                self.rmw_shift(bus, opcode & 0xE0, addr);
                4
            }
            0x1B | 0x3B | 0x5B | 0x7B | 0x9B | 0xBB => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                self.rmw_shift(bus, opcode & 0xE0, addr);
                5
            }
            0x0C | 0x2C | 0x4C | 0x6C | 0x8C | 0xAC => {
                let addr = self.fetch16(bus);
                self.rmw_shift(bus, opcode & 0xE0, addr);
                5
            }

            // --- 16-bit ALU -------------------------------------------------------------
            0x7A => self.addw(bus),
            0x9A => self.subw(bus),
            0x5A => {
                let addr = self.fetch_dp(bus);
                let v = self.read16(bus, addr);
                self.cmpw(v);
                4
            }
            0x3A => {
                let addr = self.fetch_dp(bus);
                let v = self.read16(bus, addr);
                let result = v.wrapping_add(1);
                self.write_word(bus, addr, result);
                self.nz16(result);
                6
            }
            0x1A => {
                let addr = self.fetch_dp(bus);
                let v = self.read16(bus, addr);
                let result = v.wrapping_sub(1);
                self.write_word(bus, addr, result);
                self.nz16(result);
                6
            }

            // --- 1-bit ALU --------------------------------------------------------------
            // fullsnes's "1bit ALU Operations" gives every one of these an `AA aa ba` byte column —
            // three bytes, so one 16-bit operand — and "Addressing Modes" says what it holds:
            // `aaa.b  [aaa].b  ;-Bit0..7 of address [0000..1FFF] (13bit addr)`. Bits 0-12 are the
            // address and bits 13-15 the bit number, so there is no separate bit byte: the old
            // decode read the address as a full 16 bits and then fetched the next opcode as the bit
            // number, which both addressed the wrong byte whenever the bit number was non-zero and
            // left PC one byte late for every following instruction.
            0x0A => self.or1(bus, false),
            0x2A => self.or1(bus, true),
            0x4A => self.and1(bus, false),
            0x6A => self.and1(bus, true),
            0x8A => {
                // `EOR1 C,aaa.b  XOR  C,[aaa].b  8A aa ba  5  C=C XOR [aaa].bit_b`.
                let (addr, bit) = self.fetch_aaa_b(bus);
                let value = self.read(bus, addr);
                let c = self.psw.c();
                self.psw.set_c(c ^ bit_of(value, bit));
                5
            }
            0xEA => {
                // `NOT1 aaa.b  NOT  [aaa].b  EA aa ba  5  invert [aaa].bit_b`.
                let (addr, bit) = self.fetch_aaa_b(bus);
                let value = self.read(bus, addr);
                self.write(bus, addr, value ^ (1u8 << bit));
                5
            }
            0xAA => {
                // `MOV1 C,aaa.b  MOV  C,[aaa].b  AA aa ba  4  C=[aaa].bit_b`.
                let (addr, bit) = self.fetch_aaa_b(bus);
                let value = self.read(bus, addr);
                self.psw.set_c(bit_of(value, bit));
                4
            }
            0xCA => {
                // `MOV1 aaa.b,C  MOV  [aaa].b,C  CA aa ba  6  [aaa].bit_b=C`.
                let (addr, bit) = self.fetch_aaa_b(bus);
                let value = self.read(bus, addr);
                let mask = 1u8 << bit;
                let c = self.psw.c();
                let result = if c { value | mask } else { value & !mask };
                self.write(bus, addr, result);
                6
            }
            0x0E => {
                // TSET1: [aaaa] = [aaaa] OR A, and N/Z as if CMP A,[aaaa] on the old value.
                // fullsnes "Special ALU Operations": `TSET1 !aaaa  TSET [aaaa],A  0E aa aa  6
                // [aaaa]=[aaaa]OR A  N.....Z.`, and below the table: "For TCLR/TSET, Z+N flags are
                // set as for CMP A,[aaaa] (before [aaaa] gets changed)". The flag column is
                // `N.....Z.` — C is not in it — so the equality test must not touch C the way
                // `CMP` does.
                let addr = self.fetch16(bus);
                let value = self.read(bus, addr);
                let diff = self.a.wrapping_sub(value);
                self.nz(diff);
                self.write(bus, addr, value | self.a);
                6
            }
            0x4E => {
                // TCLR1: [aaaa] = [aaaa] AND NOT A; `TCLR1 !aaaa  TCLR [aaaa],A  4E aa aa  6
                // [aaaa]=[aaaa]AND NOT A  N.....Z.`, same N/Z-only equality test as TSET1.
                let addr = self.fetch16(bus);
                let value = self.read(bus, addr);
                let diff = self.a.wrapping_sub(value);
                self.nz(diff);
                self.write(bus, addr, value & !self.a);
                6
            }

            // --- TCALL, SET1/CLR1, BBS/BBC ----------------------------------------------
            _ if opcode & 0x0F == 0x01 => {
                // TCALL n: push PC, PC = Word[$FFDE - n*2].
                let hi = (self.pc >> 8) as u8;
                let lo = self.pc as u8;
                self.push(bus, hi);
                self.push(bus, lo);
                let vector = 0xFFDEu16.wrapping_sub(u16::from(opcode >> 4) * 2);
                self.pc = self.read16(bus, vector);
                8
            }
            _ if opcode & 0x1F == 0x02 => {
                // SET1 dp.b. fullsnes "SNES APU SPC700 CPU ALU Commands", 1bit ALU Operations:
                // `SET1 aa.b  SET [aa].b  b*20+02 aa  4  [aa].bit_b=1`. The opcode is
                // `b*20h+02h`, so the family's invariant is its low **five** bits: CLR1 is
                // `b*20h+12h` and differs in bit 4. A four-bit mask (`& 0x0F == 0x02`) also
                // matches every CLR1 opcode — $12, $32 … $F2 all end in `2` — which is what made
                // the CLR1 arm below unreachable and turned `CLR1 dp.n` into `SET1 dp.n`.
                let addr = self.fetch_dp(bus);
                let bit = opcode >> 5;
                let value = self.read(bus, addr);
                self.write(bus, addr, value | (1u8 << bit));
                4
            }
            _ if opcode & 0x1F == 0x12 => {
                // CLR1 dp.b: `CLR1 aa.b  CLR [aa].b  b*20+12 aa  4  [aa].bit_b=0`.
                let addr = self.fetch_dp(bus);
                let bit = opcode >> 5;
                let value = self.read(bus, addr);
                self.write(bus, addr, value & !(1u8 << bit));
                4
            }
            _ if opcode & 0x1F == 0x03 || opcode & 0x1F == 0x13 => {
                // BBS dp.b (0x?3) / BBC dp.b (0x?3 with the 0x10 bit).
                let addr = self.fetch_dp(bus);
                let offset = self.fetch(bus) as i8;
                let bit = opcode >> 5;
                let value = self.read(bus, addr);
                let set = value & (1 << bit) != 0;
                let wanted = opcode & 0x1F == 0x03;
                if set == wanted {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    7
                } else {
                    5
                }
            }

            // --- conditional branches ---------------------------------------------------
            _ if opcode & 0x1F == 0x10 => {
                let offset = self.fetch(bus) as i8;
                let kind = opcode >> 5;
                let taken = match kind >> 1 {
                    0 => self.psw.n() == (kind & 1 != 0),
                    1 => self.psw.v() == (kind & 1 != 0),
                    2 => self.psw.c() == (kind & 1 != 0),
                    _ => self.psw.z() == (kind & 1 != 0),
                };
                if taken {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    4
                } else {
                    2
                }
            }

            // --- jumps and calls --------------------------------------------------------
            0x2F => {
                let offset = self.fetch(bus) as i8;
                self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                4
            }
            0x5F => {
                self.pc = self.fetch16(bus);
                3
            }
            0x1F => {
                let base = self.fetch16(bus).wrapping_add(u16::from(self.x));
                self.pc = self.read16(bus, base);
                6
            }
            0x3F => {
                let target = self.fetch16(bus);
                self.push_call(bus);
                self.pc = target;
                8
            }
            0x4F => {
                let target = 0xFF00 | u16::from(self.fetch(bus));
                self.push_call(bus);
                self.pc = target;
                6
            }
            0x6F => {
                let lo = self.pop(bus);
                let hi = self.pop(bus);
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
                5
            }
            0x7F => {
                self.psw = Psw(self.pop(bus));
                let lo = self.pop(bus);
                let hi = self.pop(bus);
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
                6
            }
            0x2E => {
                let addr = self.fetch_dp(bus);
                let offset = self.fetch(bus) as i8;
                let value = self.read(bus, addr);
                if self.a != value {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    7
                } else {
                    5
                }
            }
            0xDE => {
                let addr = self.fetch_dp_indexed(bus, self.x);
                let offset = self.fetch(bus) as i8;
                let value = self.read(bus, addr);
                if self.a != value {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    8
                } else {
                    6
                }
            }
            0xFE => {
                let offset = self.fetch(bus) as i8;
                self.y = self.y.wrapping_sub(1);
                if self.y != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    6
                } else {
                    4
                }
            }
            0x6E => {
                let addr = self.fetch_dp(bus);
                let offset = self.fetch(bus) as i8;
                let value = self.read(bus, addr).wrapping_sub(1);
                self.write(bus, addr, value);
                if value != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    7
                } else {
                    5
                }
            }

            // Every opcode is covered above; a hole here would be a decode bug.
            _ => 2,
        }
    }

    /// The six 8-bit ALU groups over their twelve operand forms (fullsnes "8bit ALU Operations").
    fn alu(&mut self, bus: &mut SpcBus<'_>, base: u8, form: u8) -> u32 {
        match form {
            0x08 => {
                let value = self.fetch(bus);
                self.apply(base, value);
                2
            }
            0x09 => {
                let src = self.fetch_dp(bus);
                let dst = self.fetch_dp(bus);
                let value = self.read(bus, src);
                let old = self.read(bus, dst);
                let result = self.apply_to(base, old, value);
                self.store(bus, base, dst, result);
                6
            }
            0x18 => {
                let value = self.fetch(bus);
                let dst = self.fetch_dp(bus);
                let old = self.read(bus, dst);
                let result = self.apply_to(base, old, value);
                self.store(bus, base, dst, result);
                5
            }
            0x19 => {
                // `(X) <op>= (Y)`: both indices pick a byte of the direct page the P flag names.
                let src = self.dp_addr(self.y);
                let dst = self.dp_addr(self.x);
                let value = self.read(bus, src);
                let old = self.read(bus, dst);
                let result = self.apply_to(base, old, value);
                self.store(bus, base, dst, result);
                5
            }
            _ => {
                let (cycles, addr) = match form {
                    0x06 => (3, self.dp_addr(self.x)),
                    0x04 => {
                        let a = self.fetch_dp(bus);
                        (3, a)
                    }
                    0x14 => {
                        let a = self.fetch_dp_indexed(bus, self.x);
                        (4, a)
                    }
                    0x05 => (4, self.fetch16(bus)),
                    0x15 => {
                        let a = self.fetch16(bus).wrapping_add(u16::from(self.x));
                        (5, a)
                    }
                    0x16 => {
                        let a = self.fetch16(bus).wrapping_add(u16::from(self.y));
                        (5, a)
                    }
                    0x17 => {
                        let base = self.fetch_dp(bus);
                        let ptr = self.read16(bus, base).wrapping_add(u16::from(self.y));
                        (6, ptr)
                    }
                    _ => {
                        let base = self.fetch_dp_indexed(bus, self.x);
                        let ptr = self.read16(bus, base);
                        (6, ptr)
                    }
                };
                let value = self.read(bus, addr);
                self.apply(base, value);
                cycles
            }
        }
    }

    /// Writes a memory-destination ALU result back, except for `CMP`.
    ///
    /// fullsnes "SNES APU SPC700 CPU ALU Commands" lists the group as `CMP a,b / a-b / N.....ZC` —
    /// the flags alone, with no `a=<result>` the way `OR` has `a=a OR b` — and its `cmd aa,bb`
    /// operand row (`x+09`) is the same word-for-word operation for all six groups. A compare
    /// therefore has no store cycle. Writing the unchanged destination back is invisible in RAM but
    /// not at `$00F0-$00FF`: fullsnes's `MOV aa,A` row reads "(read)", i.e. these forms have bus
    /// side effects, so a driver's `CMP $F7,$00` port poll would echo the S-CPU's byte back to it
    /// before the SPC had read the data byte that came with it.
    fn store(&mut self, bus: &mut SpcBus<'_>, base: u8, dst: u16, result: u8) {
        if base != 0x60 {
            self.write(bus, dst, result);
        }
    }

    /// Applies one ALU operation to `A`, the six forms whose destination is the accumulator
    /// (`A <op> #imm` and `A <op> mem`). A compare leaves `A` unchanged.
    fn apply(&mut self, base: u8, value: u8) -> u8 {
        let result = self.apply_to(base, self.a, value);
        if base != 0x60 {
            self.a = result;
        }
        result
    }

    /// Applies one ALU operation whose destination is memory: `lhs` is the destination's current
    /// value, `rhs` the operand, and the returned value is what the RMW form writes back. The
    /// `dp,dp`, `dp,#imm` and `(X),(Y)` forms are `mem <op>= value` — they must not read or write
    /// `A`, which is what every one of them did while it shared [`Spc700::apply`].
    fn apply_to(&mut self, base: u8, lhs: u8, rhs: u8) -> u8 {
        match base {
            0x00 => {
                let r = lhs | rhs;
                self.nz(r);
                r
            }
            0x20 => {
                let r = lhs & rhs;
                self.nz(r);
                r
            }
            0x40 => {
                let r = lhs ^ rhs;
                self.nz(r);
                r
            }
            0x60 => {
                self.cmp(lhs, rhs);
                lhs
            }
            0x80 => self.adc(lhs, rhs),
            _ => self.sbc(lhs, rhs),
        }
    }

    /// `lhs + rhs + C`, returning the result.
    fn adc(&mut self, lhs: u8, rhs: u8) -> u8 {
        let carry = u16::from(self.psw.c());
        let sum = u16::from(lhs) + u16::from(rhs) + carry;
        let result = sum as u8;
        self.psw.set_c(sum > 0xFF);
        self.psw.set_v((lhs ^ result) & (rhs ^ result) & 0x80 != 0);
        self.psw
            .set_h((lhs & 0x0F) + (rhs & 0x0F) + carry as u8 > 0x0F);
        self.nz(result);
        result
    }

    /// `lhs - rhs - !C`, returning the result.
    ///
    /// H is the carry out of bit 3, not a half-borrow: fullsnes "Flags (PSW)" gives it as
    /// `Bit3  H  Half-carry  (0=Borrow, or no-carry, 1=Carry, or no-borrow)`. `DAS` reads it, and
    /// its half-borrow test is `!H` to match.
    fn sbc(&mut self, lhs: u8, rhs: u8) -> u8 {
        let borrow = u16::from(!self.psw.c());
        let diff = i16::from(lhs) - i16::from(rhs) - borrow as i16;
        let result = diff as u8;
        self.psw.set_c(diff >= 0);
        self.psw.set_v((lhs ^ rhs) & (lhs ^ result) & 0x80 != 0);
        let half = (rhs & 0x0F) + borrow as u8;
        self.psw.set_h((lhs & 0x0F) >= half);
        self.nz(result);
        result
    }

    /// `CMP lhs,rhs`: N and Z from `lhs - rhs`, C from the borrow.
    fn cmp(&mut self, lhs: u8, rhs: u8) {
        let diff = i16::from(lhs) - i16::from(rhs);
        let result = diff as u8;
        self.psw.set_c(diff >= 0);
        self.nz(result);
    }

    /// `CMP X,...` / `CMP Y,...` (fullsnes's three extra compare forms).
    fn cmp_reg(&mut self, reg: u8, value: u8) {
        let diff = i16::from(reg) - i16::from(value);
        let result = diff as u8;
        self.psw.set_c(diff >= 0);
        self.nz(result);
    }

    fn cmpw(&mut self, value: u16) {
        let ya = self.ya();
        let diff = i32::from(ya) - i32::from(value);
        self.psw.set_c(diff >= 0);
        let result = diff as u16;
        self.psw.set_n(result & 0x8000 != 0);
        self.psw.set_z(result == 0);
    }

    fn addw(&mut self, bus: &mut SpcBus<'_>) -> u32 {
        let addr = self.fetch_dp(bus);
        let value = self.read16(bus, addr);
        let ya = self.ya();
        let sum = u32::from(ya) + u32::from(value);
        let result = sum as u16;
        self.psw.set_c(sum > 0xFFFF);
        self.psw
            .set_v((ya ^ result) & (value ^ result) & 0x8000 != 0);
        // "For ADDW/SUBW, H is carry from bit11 to bit12."
        self.psw.set_h((ya & 0x0FFF) + (value & 0x0FFF) > 0x0FFF);
        self.set_ya(result);
        self.nz16(result);
        5
    }

    fn subw(&mut self, bus: &mut SpcBus<'_>) -> u32 {
        let addr = self.fetch_dp(bus);
        let value = self.read16(bus, addr);
        let ya = self.ya();
        let diff = i32::from(ya) - i32::from(value);
        let result = diff as u16;
        self.psw.set_c(diff >= 0);
        self.psw.set_v((ya ^ value) & (ya ^ result) & 0x8000 != 0);
        // fullsnes "16bit ALU Operations": "For ADDW/SUBW, H is carry from bit11 to bit12." A carry
        // out, so H is set when the subtraction of the low twelve bits does not borrow.
        self.psw
            .set_h(i32::from(ya & 0x0FFF) >= i32::from(value & 0x0FFF));
        self.set_ya(result);
        self.nz16(result);
        5
    }

    fn div(&mut self) -> u32 {
        let ya = self.ya();
        let divisor = u16::from(self.x);
        // "Division by zero returns Result=FFFFh, Remainder=Dividend."
        let quotient = ya.checked_div(divisor).unwrap_or(0xFFFF);
        let remainder = ya.checked_rem(divisor).unwrap_or(ya);
        self.a = quotient as u8;
        self.y = remainder as u8;
        self.psw.set_v(quotient > 0xFF);
        self.psw.set_h((self.y & 0x0F) >= (self.x & 0x0F));
        self.nz(self.a);
        12
    }

    fn mul(&mut self) -> u32 {
        let product = u16::from(self.y) * u16::from(self.a);
        self.a = product as u8;
        self.y = (product >> 8) as u8;
        // "YA=Y*A, NZ on Y only".
        self.nz(self.y);
        9
    }

    fn daa(&mut self) {
        let mut a = self.a;
        if self.psw.c() || a > 0x99 {
            a = a.wrapping_add(0x60);
            self.psw.set_c(true);
        }
        if self.psw.h() || (a & 0x0F) > 0x09 {
            a = a.wrapping_add(0x06);
        }
        self.a = a;
        self.nz(a);
    }

    fn das(&mut self) {
        // fullsnes "Flags (PSW)": `Bit0  C  Carry Flag  (0=Borrow, or no-carry, 1=Carry, or
        // no-borrow)`. `DAS` adjusts *after* a subtraction, so the tens digit it corrects for is
        // one it borrowed from — C **clear** — and the borrow it can carry out of the tens digit is
        // reported the same way. "Special ALU Operations" gives `DAS A  DAS A  BE  3  BCD adjust
        // after SBC  N.....ZC`, so C is in the flag column, and C is written only here: taking the
        // `$60` out of the tens digit *is* the borrow being reported, and a run that corrects
        // nothing has no borrow to invent — `SBC`'s C already describes the tens digit.
        let mut a = self.a;
        if !self.psw.c() || a > 0x99 {
            a = a.wrapping_sub(0x60);
            self.psw.set_c(false);
        }
        // fullsnes "Flags (PSW)": `Bit3  H  Half-carry  (0=Borrow, or no-carry, 1=Carry, or
        // no-borrow)`, so the half-borrow `DAS` adjusts for is H **clear**. `sbc` now reports that
        // documented sense, and this test is its companion: a `SBC : DAS` pair subtracts the same
        // 6 it always did.
        if !self.psw.h() || (a & 0x0F) > 0x09 {
            a = a.wrapping_sub(0x06);
        }
        self.a = a;
        self.nz(a);
    }

    fn brk(&mut self, bus: &mut SpcBus<'_>) -> u32 {
        let hi = (self.pc >> 8) as u8;
        let lo = self.pc as u8;
        self.push(bus, hi);
        self.push(bus, lo);
        let p = self.psw.0;
        self.push(bus, p);
        // "Push $+1,PSW,PC=[FFDE]" with B set and I clear.
        self.psw.set_b(true);
        self.psw.set_i(false);
        self.pc = self.read16(bus, 0xFFDE);
        8
    }

    fn push_call(&mut self, bus: &mut SpcBus<'_>) {
        let hi = (self.pc >> 8) as u8;
        let lo = self.pc as u8;
        self.push(bus, hi);
        self.push(bus, lo);
    }

    fn or1(&mut self, bus: &mut SpcBus<'_>, invert: bool) -> u32 {
        // `OR1  C,aaa.b  OR   C,[aaa].b  0A aa ba  5  C=C OR [aaa].bit_b`, and `OR1 C,/aaa.b`
        // (2A) the same with `not[aaa].bit_b`.
        let (addr, bit) = self.fetch_aaa_b(bus);
        let value = self.read(bus, addr);
        let flag = bit_of(value, bit) != invert;
        let c = self.psw.c();
        self.psw.set_c(c || flag);
        5
    }

    fn and1(&mut self, bus: &mut SpcBus<'_>, invert: bool) -> u32 {
        // `AND1 C,aaa.b  AND  C,[aaa].b  4A aa ba  4  C=C AND [aaa].bit_b`, and `AND1 C,/aaa.b`
        // (6A) the same with `not[aaa].bit_b`.
        let (addr, bit) = self.fetch_aaa_b(bus);
        let value = self.read(bus, addr);
        let flag = bit_of(value, bit) != invert;
        let c = self.psw.c();
        self.psw.set_c(c && flag);
        4
    }

    /// ASL/ROL/LSR/ROR/DEC/INC on a memory address, as the RMW shape fullsnes describes.
    fn rmw_shift(&mut self, bus: &mut SpcBus<'_>, base: u8, addr: u16) {
        let value = self.read(bus, addr);
        if base == 0x80 {
            let result = value.wrapping_sub(1);
            self.write(bus, addr, result);
            self.nz(result);
            return;
        }
        if base == 0xA0 {
            let result = value.wrapping_add(1);
            self.write(bus, addr, result);
            self.nz(result);
            return;
        }
        let (result, carry) = shift_by(base, value, self.psw.c());
        if let Some(c) = carry {
            self.psw.set_c(c);
        }
        self.write(bus, addr, result);
        self.nz(result);
    }

    /// A read through the SPC700's memory map (fullsnes "SNES APU Memory and I/O Map").
    pub fn read(&mut self, bus: &mut SpcBus<'_>, addr: u16) -> u8 {
        match addr {
            0x00F0..=0x00FF => self.read_io(bus, addr),
            // `$FFC0-$FFFF` is the boot ROM when `$F1` bit 7 is set ("writes do always go to RAM"),
            // and its 64 bytes are what a read of the window returns — fullsnes "SNES APU Memory
            // and I/O Map": `FFC0h..FFFFh 64-byte Boot ROM or RAM`. Zeros here would make
            // `Word[$FFDE]` read `$0000` where hardware has `E4 F5`.
            0xFFC0..=0xFFFF if self.control & 0x80 != 0 => {
                crate::apu::IPL_ROM[usize::from(addr - 0xFFC0)]
            }
            _ => bus.ram[usize::from(addr)],
        }
    }

    /// A write through the SPC700's memory map. A write to `$00F0-$00FF` both reaches the port and
    /// lands in RAM, which is why the ports read back their own last value.
    pub fn write(&mut self, bus: &mut SpcBus<'_>, addr: u16, value: u8) {
        if (0x00F0..=0x00FF).contains(&addr) {
            self.write_io(bus, addr, value);
        }
        // "writes do always go to RAM", the boot-ROM window included.
        bus.ram[usize::from(addr)] = value;
    }

    fn read_io(&mut self, bus: &mut SpcBus<'_>, addr: u16) -> u8 {
        match addr {
            0x00F2 => self.dspaddr,
            0x00F3 => bus.dsp.read(self.dspaddr),
            // The SPC reads the input latches the S-CPU wrote.
            0x00F4..=0x00F7 => bus.to_apu[usize::from(addr - 0x00F4)],
            // "Incremented at the rate selected via TnDIV (reset to 0 after reading)".
            0x00FD => {
                let value = self.t0out;
                self.t0out = 0;
                value
            }
            0x00FE => {
                let value = self.t1out;
                self.t1out = 0;
                value
            }
            0x00FF => {
                let value = self.t2out;
                self.t2out = 0;
                value
            }
            // Everything else is write-only or unused here and reads back its RAM mirror.
            _ => bus.ram[usize::from(addr)],
        }
    }

    fn write_io(&mut self, bus: &mut SpcBus<'_>, addr: u16, value: u8) {
        match addr {
            0x00F0 => self.test = value,
            0x00F1 => {
                // "4 Reset Port 00F4h/00F5h Input-Latches (0=No change, 1=Reset to 00h)", "5 Reset
                // Port 00F6h/00F7h Input-Latches (0=No change, 1=Reset to 00h)", "Note: The CPUIO
                // inputs are latched inside of the SPC700 (at time when the Main CPU writes them),
                // above two bits allow to reset these latches." A driver that has consumed a byte
                // clears the latch so the same byte is not read again as a second command.
                if value & 0x10 != 0 {
                    bus.to_apu[0] = 0;
                    bus.to_apu[1] = 0;
                }
                if value & 0x20 != 0 {
                    bus.to_apu[2] = 0;
                    bus.to_apu[3] = 0;
                }
                // "0-2 Timer 0-2 Enable (0=Disable, set TnOUT=0 & reload divider, 1=Enable)".
                let old = self.control;
                self.control = value;
                if value & 0x01 != 0 && old & 0x01 == 0 {
                    self.t0out = 0;
                    self.t0pre = reload(self.t0div);
                }
                if value & 0x02 != 0 && old & 0x02 == 0 {
                    self.t1out = 0;
                    self.t1pre = reload(self.t1div);
                }
                if value & 0x04 != 0 && old & 0x04 == 0 {
                    self.t2out = 0;
                    self.t2pre = reload(self.t2div);
                }
            }
            0x00F2 => self.dspaddr = value,
            0x00F3 => {
                bus.dsp.write(self.dspaddr, value);
                self.stage_dsp(self.dspaddr, value);
            }
            // What the SPC writes at `$F4`-`$F7` is what the S-CPU reads at `$214x`.
            0x00F4..=0x00F7 => {
                let index = (addr - 0x00F4) as u8;
                bus.from_apu[usize::from(index)] = value;
                self.stage_port(index, value);
            }
            0x00FA => self.t0div = value,
            0x00FB => self.t1div = value,
            0x00FC => self.t2div = value,
            _ => {}
        }
    }

    /// One 64 kHz timer tick: the timers increment "at T1 and T17" of the DSP's 32-slot sample loop
    /// (fullsnes "SNES APU Low Level Timings"), so `Apu` calls this twice a sample and passes the
    /// running tick number.
    ///
    /// fullsnes "SNES Timing Oscillators" gives the sources as "SPC700 timer 0+1 8000Hz, timer 2
    /// 64000Hz", i.e. one 8 kHz pulse every eighth 64 kHz tick.
    pub fn timer_clock(&mut self, tick: u64) {
        if self.test & 0x01 != 0 {
            return; // "$F0 bit 0 Timer-Enable (0=Normal, 1=Timers don't work)"
        }
        if self.control & 0x01 != 0 && tick.is_multiple_of(8) {
            let (pre, pulse) = prescale(self.t0pre, self.t0div);
            self.t0pre = pre;
            if pulse {
                self.t0out = bump_counter(self.t0out);
            }
        }
        if self.control & 0x02 != 0 && tick.is_multiple_of(8) {
            let (pre, pulse) = prescale(self.t1pre, self.t1div);
            self.t1pre = pre;
            if pulse {
                self.t1out = bump_counter(self.t1out);
            }
        }
        if self.control & 0x04 != 0 {
            let (pre, pulse) = prescale(self.t2pre, self.t2div);
            self.t2pre = pre;
            if pulse {
                self.t2out = bump_counter(self.t2out);
            }
        }
    }

    /// `$EA`-style addressing: the direct page is `00xx` or `01xx` by PSW bit 5.
    fn dp_addr(&self, raw: u8) -> u16 {
        if self.psw.p() {
            0x0100 | u16::from(raw)
        } else {
            u16::from(raw)
        }
    }

    /// Fetches a direct-page operand and resolves the page bit (two steps, so the mutable fetch and
    /// the immutable `dp_addr` do not overlap).
    fn fetch_dp(&mut self, bus: &mut SpcBus<'_>) -> u16 {
        let raw = self.fetch(bus);
        self.dp_addr(raw)
    }

    /// A direct-page operand with an index — `dp+X`, `dp+Y`, `[dp+X]`.
    ///
    /// fullsnes "SNES APU SPC700 CPU Overview", Addressing Modes:
    ///
    /// ```text
    ///   aa+X           [aa+X]         ; (or at [0100..01FF] when flag P=1)
    ///   aa+Y           [aa+Y]         ; (aa+X and aa+Y wrap within that area,
    ///   (X)            [X]            ; ie. addr = (aa+X) AND 0FFh)
    /// ```
    ///
    /// The index is added to the **eight-bit offset**, before the page bit picks the page: with
    /// X=$20, `MOV A,$F0+X` reads `$0010`, not `$0110`. Adding the index to the finished address
    /// instead (`fetch_dp(..).wrapping_add(x)`) carries into the next page and reads a byte the
    /// instruction never named.
    fn fetch_dp_indexed(&mut self, bus: &mut SpcBus<'_>, index: u8) -> u16 {
        let raw = self.fetch(bus);
        self.dp_addr(raw.wrapping_add(index))
    }

    /// The 1-bit family's operand: fullsnes "Addressing Modes",
    /// `aaa.b  [aaa].b  ;-Bit0..7 of address [0000..1FFF] (13bit addr)`.
    ///
    /// One 16-bit word, bits 0-12 the address and bits 13-15 the bit number (the table's `AA aa ba`
    /// column: three bytes). The bit number is not a byte of its own.
    fn fetch_aaa_b(&mut self, bus: &mut SpcBus<'_>) -> (u16, u8) {
        let operand = self.fetch16(bus);
        (operand & 0x1FFF, (operand >> 13) as u8)
    }

    fn fetch(&mut self, bus: &mut SpcBus<'_>) -> u8 {
        let value = self.read(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn fetch16(&mut self, bus: &mut SpcBus<'_>) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    fn read16(&mut self, bus: &mut SpcBus<'_>, addr: u16) -> u16 {
        let lo = self.read(bus, addr);
        let hi = self.read(bus, addr.wrapping_add(1));
        u16::from(lo) | (u16::from(hi) << 8)
    }

    fn write_word(&mut self, bus: &mut SpcBus<'_>, addr: u16, value: u16) {
        self.write(bus, addr, value as u8);
        self.write(bus, addr.wrapping_add(1), (value >> 8) as u8);
    }

    /// `[SP] = value, SP = SP - 1`.
    fn push(&mut self, bus: &mut SpcBus<'_>, value: u8) {
        let addr = 0x0100 | u16::from(self.sp);
        self.write(bus, addr, value);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// `SP = SP + 1, value = [SP]`.
    fn pop(&mut self, bus: &mut SpcBus<'_>) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        let addr = 0x0100 | u16::from(self.sp);
        self.read(bus, addr)
    }

    fn ya(&self) -> u16 {
        u16::from(self.a) | (u16::from(self.y) << 8)
    }

    fn set_ya(&mut self, value: u16) {
        self.a = value as u8;
        self.y = (value >> 8) as u8;
    }

    fn nz(&mut self, value: u8) {
        self.psw.set_n(value & 0x80 != 0);
        self.psw.set_z(value == 0);
    }

    /// N and Z from a **16-bit** result. fullsnes's 16-bit rows are `ADDW`/`SUBW` `NV..H.ZC`,
    /// `INCW`/`DECW` and `MOVW YA,dp` `N.....Z.`, i.e. N is bit 15 of the word and Z is the whole
    /// word being zero — not the low byte, which is what `ADDW`/`SUBW` used and what
    /// `INCW`/`DECW` did not set at all.
    fn nz16(&mut self, value: u16) {
        self.psw.set_n(value & 0x8000 != 0);
        self.psw.set_z(value == 0);
    }

    /// Turns the SPC700's diagnosis ring on.
    ///
    /// Allocates once, here, and never on the frame path; a harness calls this on a machine it has
    /// just built or between frames. Off by default, so no cartridge can observe it.
    pub fn enable_trace(&mut self) {
        if self.trace.is_none() {
            self.trace = Some(vec![SpcEvent::default(); TRACE_LEN].into_boxed_slice());
        }
        self.trace_len = 0;
    }

    /// Turns the ring off and releases it.
    pub fn disable_trace(&mut self) {
        self.trace = None;
        self.trace_len = 0;
    }

    /// True when a harness is recording.
    pub fn tracing(&self) -> bool {
        self.trace.is_some()
    }

    /// The frame the harness wants stamped on rows recorded from now on. The SPC has no notion of a
    /// frame; `Machine::run_frame` is the S-CPU's, so the harness advances this once per frame.
    pub fn set_trace_frame(&mut self, frame: u32) {
        self.trace_frame = frame;
    }

    /// Events recorded since [`Spc700::enable_trace`], including ones the ring has overwritten.
    pub fn trace_len(&self) -> u64 {
        self.trace_len
    }

    /// The event with absolute index `n`, or `None` once the ring has overwritten it.
    pub fn trace_event(&self, n: u64) -> Option<&SpcEvent> {
        let ring = self.trace.as_ref()?;
        if n >= self.trace_len || self.trace_len - n > TRACE_LEN as u64 {
            return None;
        }
        Some(&ring[(n as usize) & (TRACE_LEN - 1)])
    }

    /// Clears the per-step staging: `Apu::step_cpu` calls this immediately before the instruction so
    /// a port write the previous step made cannot be attributed to this one.
    pub fn begin_step(&mut self) {
        self.step_ports = [PortWrite::default(); TRACE_PORTS];
        self.step_port_count = 0;
        self.step_dsp_reg = 0xFF;
        self.step_dsp_value = 0;
    }

    /// Stages one `$F4`-`$F7` write by the SPC for the step being recorded: the store's own value,
    /// even if it repeats the latch's previous one. Called from the two places that write those
    /// latches — [`Spc700::write_io`] for a program and `Apu::ipl_step` for the boot ROM's echo.
    pub fn stage_port(&mut self, index: u8, value: u8) {
        let slot = usize::from(self.step_port_count);
        if slot < TRACE_PORTS {
            self.step_ports[slot] = PortWrite {
                side: PortSide::Spc,
                index: index & 3,
                value,
            };
            self.step_port_count += 1;
        }
    }

    /// Stages the DSP `KON`/`KOFF` write the step made, for the ring.
    fn stage_dsp(&mut self, register: u8, value: u8) {
        if register == DSP_KON || register == DSP_KOFF {
            self.step_dsp_reg = register;
            self.step_dsp_value = value;
        }
    }

    /// Records the instruction `Apu::step_cpu` has just run: `pc` and `bytes` as they were before it,
    /// the register file and the timers as they are now, plus whatever the step staged on the ports.
    pub fn record_step(&mut self, pc: u16, bytes: [u8; 3]) {
        if self.trace.is_none() {
            return;
        }
        let event = SpcEvent {
            kind: EventKind::Step,
            frame: self.trace_frame,
            pc,
            bytes,
            a: self.a,
            x: self.x,
            y: self.y,
            sp: self.sp,
            psw: self.psw.0,
            control: self.control,
            t0out: self.t0out,
            t1out: self.t1out,
            t2out: self.t2out,
            t0div: self.t0div,
            t1div: self.t1div,
            t2div: self.t2div,
            ports: self.step_ports,
            port_count: self.step_port_count,
            dsp_reg: self.step_dsp_reg,
            dsp_value: self.step_dsp_value,
            cycles: self.cycles,
        };
        self.push_event(event);
    }

    /// Records one write the S-CPU made to `$2140`-`$2143`. Called from `Apu::write_port`, which the
    /// bus reaches on a `STA $214x`; a write while tracing is off is not recorded.
    pub fn record_scpu_port(&mut self, index: u8, value: u8) {
        if self.trace.is_none() {
            return;
        }
        let mut ports = [PortWrite::default(); TRACE_PORTS];
        ports[0] = PortWrite {
            side: PortSide::Scpu,
            index: index & 3,
            value,
        };
        let event = SpcEvent {
            kind: EventKind::ScpuPort,
            frame: self.trace_frame,
            pc: self.pc,
            ports,
            port_count: 1,
            dsp_reg: 0xFF,
            cycles: self.cycles,
            ..SpcEvent::default()
        };
        self.push_event(event);
    }

    fn push_event(&mut self, event: SpcEvent) {
        if let Some(ring) = self.trace.as_mut() {
            let slot = (self.trace_len as usize) & (TRACE_LEN - 1);
            ring[slot] = event;
            self.trace_len += 1;
        }
    }

    pub fn save_state<W: StateWrite>(&self, w: &mut W) {
        w.u8(self.a);
        w.u8(self.x);
        w.u8(self.y);
        w.u8(self.sp);
        w.u8(self.psw.0);
        w.u8(self.dspaddr);
        w.u8(self.test);
        w.u8(self.control);
        w.u16(self.pc);
        w.u8(self.t0div);
        w.u8(self.t1div);
        w.u8(self.t2div);
        w.u8(self.t0out);
        w.u8(self.t1out);
        w.u8(self.t2out);
        w.u8(self.stage);
        // The dividers' countdowns, which §7.1's field list implies and §9.2's row lacked.
        w.u8(self.t0pre);
        w.u8(self.t1pre);
        w.u8(self.t2pre);
    }

    pub fn load_state<R: StateRead>(&mut self, r: &mut R) -> Result<(), R::Error> {
        self.a = r.u8()?;
        self.x = r.u8()?;
        self.y = r.u8()?;
        self.sp = r.u8()?;
        self.psw = Psw(r.u8()?);
        self.dspaddr = r.u8()?;
        self.test = r.u8()?;
        self.control = r.u8()?;
        self.pc = r.u16()?;
        self.t0div = r.u8()?;
        self.t1div = r.u8()?;
        self.t2div = r.u8()?;
        self.t0out = r.u8()?;
        self.t1out = r.u8()?;
        self.t2out = r.u8()?;
        self.stage = r.u8()?;
        self.t0pre = r.u8()?;
        self.t1pre = r.u8()?;
        self.t2pre = r.u8()?;
        Ok(())
    }
}

/// ASL/ROL/LSR/ROR by the opcode's group, returning the result and the carry it sets.
fn shift_by(base: u8, value: u8, carry_in: bool) -> (u8, Option<bool>) {
    match base {
        0x00 => (value << 1, Some(value & 0x80 != 0)),
        0x20 => ((value << 1) | u8::from(carry_in), Some(value & 0x80 != 0)),
        0x40 => (value >> 1, Some(value & 0x01 != 0)),
        0x60 => (
            (value >> 1) | (u8::from(carry_in) << 7),
            Some(value & 0x01 != 0),
        ),
        // DEC/INC carry no carry flag; the caller handles them without this helper.
        _ => (value, None),
    }
}

/// A bit of a memory byte. The 1-bit family's bit number is the operand's top three bits, so it is
/// already 0-7 and the mask is a plain shift.
fn bit_of(value: u8, bit: u8) -> bool {
    value & (1 << bit) != 0
}

/// One timer prescaler tick: `div` of 0 means 256 (fullsnes `TnDIV`: "00h=Divide by 256"). The
/// countdown starts at `div - 1`, so `div = 1` counts every source pulse.
fn prescale(pre: u8, div: u8) -> (u8, bool) {
    if pre == 0 {
        (reload(div), true)
    } else {
        (pre - 1, false)
    }
}

/// The reload value for a divider: `div - 1`, or 255 for "divide by 256".
fn reload(div: u8) -> u8 {
    if div == 0 { 255 } else { div - 1 }
}

/// The timer output counter: 4 bits, wrapping.
///
/// fullsnes "SPC700 I/O Ports", `00FDh - T0OUT`: "0-3 Incremented at the rate selected via TnDIV
/// (reset to 0 after reading) / 4-7 Not used (always zero)". There is no overflow bit: a counter
/// that has wrapped reads `$x0`-`$xF`, and a driver that turns `MOV Y,$T1OUT` into a loop count
/// (`DEC Y`/`BNE`) sees 0-15 iterations, not 128-143.
fn bump_counter(out: u8) -> u8 {
    ((out & 0x0F) + 1) & 0x0F
}
