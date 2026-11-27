//! The APU: the SPC700, the DSP and the four communication latches.
//!
//! The clock is 1 024 000 SPC cycles per 21 477 272 master clocks, carried as an integer remainder
//! (§3.4); the DSP takes one sample every 32 of those cycles, which is where 532.5 samples a frame
//! comes from. The IPL boot ROM is implemented as its state machine (§7.3), not as 64 shipped bytes,
//! and the state machine advances one pass per SPC cycle budget — the ports are read from software,
//! as the ROM reads them, never at the instant the S-CPU writes.

pub use crate::dsp::Dsp;
pub use crate::spc700::{EventKind, PortSide, PortWrite, Spc700, SpcBus, SpcEvent};

use crate::state::{StateRead, StateWrite};
use crate::{APU_RAM_SIZE, AUDIO_STAGING, DSP_SAMPLE_HZ, Host};

/// `$CC`, the one byte the boot ROM's `CMP $F4,#$CC` accepts as the first kick.
const IPL_KICK: u8 = 0xCC;

/// The 64 bytes of the IPL boot ROM at `$FFC0-$FFFF`, which the SPC reads while `$F1` bit 7 is set.
///
/// The handshake itself is [`Ipl`]'s state machine, not these bytes; this table is what a *read* of
/// the window returns, and what a jump into the window would execute. Returning zeros for the window
/// (what this did before) makes `Word[$FFDE]` read `$0000`, where hardware has `E4 F5` (`MOV A,$F5`)
/// there and a `BRK`/`TCALL 0` in the window therefore reaches `$F5E4`. fullsnes "SNES APU Memory
/// and I/O Map" (`FFC0h..FFFFh 64-byte Boot ROM or RAM`) and its IPL ROM listing.
pub const IPL_ROM: [u8; 64] = [
    0xCD, 0xEF, // $FFC0 MOV X,#$EF
    0xBD, // $FFC2 MOV SP,X
    0xE8, 0x00, // $FFC3 MOV A,#$00
    0xC6, // $FFC5 MOV (X),A
    0x1D, // $FFC6 DEC X
    0xD0, 0xFC, // $FFC7 BNE $FFC5
    0x8F, 0xAA, 0xF4, // $FFC9 MOV $F4,#$AA
    0x8F, 0xBB, 0xF5, // $FFCC MOV $F5,#$BB
    0x78, 0xCC, 0xF4, // $FFCF CMP $F4,#$CC
    0xD0, 0xFB, // $FFD2 BNE $FFCF
    0x2F, 0x19, // $FFD4 BRA $FFEF
    0xEB, 0xF4, // $FFD6 MOV Y,$F4
    0xD0, 0xFC, // $FFD8 BNE $FFD6
    0x7E, 0xF4, // $FFDA CMP Y,$F4
    0xD0, 0x0B, // $FFDC BNE $FFE9
    0xE4, 0xF5, // $FFDE MOV A,$F5
    0xCB, 0xF4, // $FFE0 MOV $F4,Y
    0xD7, 0x00, // $FFE2 MOV [$00]+Y,A
    0xFC, // $FFE4 INC Y
    0xD0, 0xF3, // $FFE5 BNE $FFDA
    0xAB, 0x01, // $FFE7 INC $01
    0x10, 0xEF, // $FFE9 BPL $FFDA
    0x7E, 0xF4, // $FFEB CMP Y,$F4
    0x10, 0xEB, // $FFED BPL $FFEB
    0xBA, 0xF6, // $FFEF MOVW YA,$F6
    0xDA, 0x00, // $FFF1 MOVW $00,YA
    0xBA, 0xF4, // $FFF3 MOVW YA,$F4
    0xC4, 0xF4, // $FFF5 MOV $F4,A
    0xDD, // $FFF7 MOV A,Y
    0x5D, // $FFF8 MOV X,A
    0xD0, 0xDB, // $FFF9 BNE $FFD6
    0x1F, 0x00, 0x00, // $FFFB JMP [$0000+X]
    0xC0, 0xFF, // $FFFE reset vector = $FFC0
];

/// One pass of `CMP $F4,#$CC` / `BNE` while the handshake waits for the first kick: 5 + 4 clocks.
const IPL_KICK_POLL_CYCLES: u32 = 9;

/// One pass of `Trans: MOV Y,$F4` / `BNE Trans`: 3 + 4 clocks.
const IPL_BLOCK_POLL_CYCLES: u32 = 7;

/// One pass of `CMP Y,$F4` / `BNE` in the transfer loop: 3 + 4 clocks.
const IPL_BYTE_POLL_CYCLES: u32 = 7;

/// The pass that stores a byte and echoes its index: `CMP Y,$F4` (3), `BNE` (2), `MOV A,$F5` (3),
/// `MOV $F4,Y` (4), `MOV [$00]+Y,A` (7), `INC Y` (2), `BNE` (4).
const IPL_STORE_CYCLES: u32 = 25;

/// The main loop with a transfer command: `MOVW YA,$F6` (5), `MOVW $00,YA` (5), `MOVW YA,$F4` (5),
/// `MOV $F4,A` (4), `MOV A,Y` (2), `MOV X,A` (2), `BNE` (4).
const IPL_COMMAND_CYCLES: u32 = 27;

/// The main loop with the entry command: the same minus the taken branch, plus `JMP [$0000+X]` (6).
const IPL_ENTRY_CYCLES: u32 = 31;

/// The audio processing unit.
#[derive(Clone)]
pub struct Apu {
    /// Which host drives the chip: what `tick`'s argument counts, and how §9.2's state is laid out.
    pub host: Host,
    pub ram: Box<[u8; APU_RAM_SIZE]>,
    pub cpu: Spc700,
    pub dsp: Dsp,
    /// The four latches, in the direction the S-CPU sees them.
    pub to_apu: [u8; 4],
    pub from_apu: [u8; 4],
    pub ipl: Ipl,
    /// The boot ROM's `Y`: the index the transfer loop expects next on `$2140`.
    pub ipl_index: u8,
    /// Phase of the 1.024 MHz SPC700 clock against the master clock.
    pub spc_acc: u32,
    /// Phase of the 32 kHz sample clock against the SPC700 clock, 0..32.
    pub sample_acc: u32,
    /// Interleaved L,R staging, `AUDIO_STAGING` f32, allocated once.
    pub out: Vec<f32>,
    pub read_pos: usize,
    pub len: usize,
    pub sample_rate: u32,
    /// The eight CHANNELS rows: bit n silences voice n. Not saved.
    pub mask: u8,
    /// SPC700 cycles since reset.
    pub cycles: u64,
}

// `new()` is the constructor §2.6 fixes; a `Default` impl is not part of the design's surface.
#[allow(clippy::new_without_default)]
impl Apu {
    /// Power-on: zeroed RAM, the IPL handshake armed with its `AA BB` greeting, the DSP's `FLG` at
    /// `$E0` (§4.5).
    pub fn new(host: Host) -> Apu {
        let mut apu = Apu {
            host,
            ram: Box::new([0; APU_RAM_SIZE]),
            cpu: Spc700::new(),
            dsp: Dsp::new(),
            to_apu: [0; 4],
            from_apu: [0; 4],
            ipl: Ipl::default(),
            ipl_index: 0,
            // A zero ledger: `tick`'s accumulator is stored biased (see `Host::acc_bias`).
            spc_acc: host.acc_bias() as u32,
            sample_acc: 0,
            out: Vec::with_capacity(AUDIO_STAGING),
            read_pos: 0,
            len: 0,
            sample_rate: DSP_SAMPLE_HZ,
            mask: 0,
            cycles: 0,
        };
        // The boot ROM's first two instructions are `mov [F4],AA` / `mov [F5],BB`.
        apu.greet();
        apu
    }

    /// Advances the SPC700 by however many of its cycles fall inside these ticks, and the DSP by
    /// however many samples fall out of that (§3.4).
    ///
    /// The unit of `ticks` is the host's: master clocks on the SNES, SPC cycles on the SGB. The
    /// ratio is exact either way: `spc_acc` carries the remainder of the division by the host's
    /// spend, so 357 368 master clocks a frame give 17 038 SPC **cycles** — 8 519 two-cycle
    /// instructions — and 532 or 533 samples, with no drift over any number of frames.
    ///
    /// An instruction has to be charged its own `Clk` value, so the accumulator is a signed ledger
    /// in units of `1/spend` of a cycle: `ticks * earn` is credited, and each instruction spends
    /// `cycles * spend`. The ledger is stored biased, because §9.2's `spc_acc` is a `u32` and one
    /// instruction may overrun the change owed by up to twelve cycles.
    /// (the pseudocode steps one instruction per accumulated `MASTER_CLOCK_HZ`, which
    /// charges the instruction's clocks *and* keeps the one-instruction-per-clock rate, running the
    /// SPC 2-8x fast; reported as a design defect.)
    pub fn tick(&mut self, ticks: u32) {
        let bias = self.host.acc_bias();
        let mut acc = i64::from(self.spc_acc) - bias + i64::from(ticks) * self.host.earn();
        while acc > 0 {
            let cycles = i64::from(self.step_instruction());
            acc -= cycles * self.host.spend();
        }
        self.spc_acc = (acc + bias) as u32;
    }

    /// Exactly one instruction, charged its own clocks and followed by the timer slots and DSP
    /// samples those clocks contain — the step [`Apu::tick`] takes, with the ledger left alone.
    ///
    /// This is also the SPC700 diagnosis instrument's clock: a harness that steps the SPC one
    /// instruction at a time through here advances time the way the machine does, so a driver that
    /// paces itself on `T0OUT` moves instead of looking wedged.
    pub fn step_instruction(&mut self) -> u32 {
        self.step_cpu()
    }

    /// A byte as the SPC700's memory map would read it, for the trace's "bytes at PC": the boot ROM
    /// while `$F1` bit 7 is set and the address is in the window, RAM everywhere else.
    fn apu_peek(&self, addr: u16) -> u8 {
        if addr >= 0xFFC0 && self.cpu.control & 0x80 != 0 {
            IPL_ROM[usize::from(addr - 0xFFC0)]
        } else {
            self.ram[usize::from(addr)]
        }
    }

    /// Turns the SPC700's diagnosis ring on. Off by default; see [`Spc700::enable_trace`].
    pub fn enable_trace(&mut self) {
        self.cpu.enable_trace();
    }

    /// Turns it off again.
    pub fn disable_trace(&mut self) {
        self.cpu.disable_trace();
    }

    /// The frame stamped on rows recorded from now on.
    pub fn set_trace_frame(&mut self, frame: u32) {
        self.cpu.set_trace_frame(frame);
    }

    /// Rows recorded since [`Apu::enable_trace`], including ones the ring has overwritten.
    pub fn trace_len(&self) -> u64 {
        self.cpu.trace_len()
    }

    /// The row with absolute index `n`, or `None` once the ring has overwritten it.
    pub fn trace_event(&self, n: u64) -> Option<&SpcEvent> {
        self.cpu.trace_event(n)
    }

    /// One SPC700 instruction, then its cycles split into DSP slots and timer ticks. Returns the
    /// instruction's `Clk` value.
    ///
    /// While `PC` sits in `$FFC0-$FFFF` with `$F1` bit 7 set the boot ROM is in the window, and there
    /// the "instruction" is one pass of the boot ROM's own loop, run by [`Apu::ipl_step`] (§7.3). That
    /// is the whole point of §7.3's design defect fix: the handshake has to advance on **SPC** clocks,
    /// so that a byte the S-CPU wrote after its index is the byte that gets stored.
    fn step_cpu(&mut self) -> u32 {
        let in_rom = self.cpu.pc >= 0xFFC0 && self.cpu.control & 0x80 != 0;
        // The ring clears its staging before the step and reads the row's instruction before it
        // runs: `pc` and the bytes there. A pass of the boot ROM names the instruction in the ROM
        // that pass stands for, not the `$FFC0` the state machine's `pc` is parked on.
        let tracing = self.cpu.tracing();
        let mut pc = self.cpu.pc;
        self.cpu.begin_step();
        if in_rom && self.ipl == Ipl::Running {
            // "If you ever want to go back to this uploader, simply jump to $FFC0 with IPL enabled
            // and the P flag clear (or $FFC9 to skip resetting the stack and page 0)" (Anomie,
            // "IPL BOOT ROM", also quoted by fullsnes).
            if self.cpu.pc == 0xFFC0 {
                self.ram[..=0xEF].fill(0); // `MOV X,#$EF : - MOV (X),A : DEC X : BNE -`
                self.cpu.sp = 0xEF;
            }
            self.ipl = Ipl::AwaitKick;
            self.greet();
        }
        let cycles = if in_rom {
            if self.ipl != Ipl::Running {
                // The pass this row records is the ROM instruction the state stands for.
                pc = self.ipl.pc();
            }
            self.ipl_step()
        } else {
            let mut bus = SpcBus {
                ram: &mut self.ram,
                dsp: &mut self.dsp,
                to_apu: &mut self.to_apu,
                from_apu: &mut self.from_apu,
            };
            self.cpu.step(&mut bus)
        };
        self.cpu.cycles += u64::from(cycles);
        for _ in 0..cycles {
            self.cpu.stage = (self.cpu.stage + 1) & 31;
            if self.cpu.stage == 1 || self.cpu.stage == 17 {
                // The timers increment "in sync with DSP timings: at T1 and T17".
                let tick = self.dsp.samples * 2 + u64::from(self.cpu.stage / 16);
                self.cpu.timer_clock(tick);
            }
            self.sample_acc += 1;
            if self.sample_acc >= 32 {
                self.sample_acc = 0;
                let (l, r) = self.dsp.sample(&mut self.ram);
                self.push_sample(l, r);
            }
        }
        self.cycles += u64::from(cycles);
        if tracing {
            let bytes = [
                self.apu_peek(pc),
                self.apu_peek(pc.wrapping_add(1)),
                self.apu_peek(pc.wrapping_add(2)),
            ];
            self.cpu.record_step(pc, bytes);
        }
        cycles
    }

    /// One pass of the boot ROM's control flow, charged what the instructions it stands for cost.
    ///
    /// The ROM polls the ports from software: it reads `$F4`, compares it with `Y`, and only *then*
    /// reads `$F5`. Modelling that as a reaction to the S-CPU's write instead puts the data byte one
    /// store too early — an S-CPU's 16-bit `STA $2140` writes `$2140` before `$2141`, so at the
    /// instant `$2140` changes `$2141` still holds the previous byte, and every block ends up shifted
    /// by one with the command byte at its head. So a byte is stored on the pass *after* its index was
    /// seen ([`Ipl::ReadByte`]): one poll pass is 7 SPC clocks, longer than the S-CPU's whole 16-bit
    /// store, which is the margin the real polling loop has.
    fn ipl_step(&mut self) -> u32 {
        match self.ipl {
            // `CMP $F4,#$CC` / `BNE -` at `$FFCF`: only the documented kick starts a command.
            Ipl::AwaitKick => {
                if self.to_apu[0] == IPL_KICK {
                    self.ipl_command()
                } else {
                    IPL_KICK_POLL_CYCLES
                }
            }
            // `Trans: MOV Y,$F4` / `BNE Trans`: a block always starts at index zero.
            Ipl::AwaitFirstByte => {
                if self.to_apu[0] == 0 {
                    self.ipl_index = 0;
                    self.ipl = Ipl::ReadByte;
                }
                IPL_BLOCK_POLL_CYCLES
            }
            // `CMP Y,$F4` / `BNE +` / `BPL -`: wait for the next index; anything at or past the
            // expected one is the uploader's "next block" or "jump" command.
            Ipl::Transfer => {
                let seen = self.to_apu[0];
                if seen == self.ipl_index {
                    self.ipl = Ipl::ReadByte;
                    IPL_BYTE_POLL_CYCLES
                } else if (seen.wrapping_sub(self.ipl_index) as i8) < 0 {
                    IPL_BYTE_POLL_CYCLES
                } else {
                    self.ipl_command()
                }
            }
            // `MOV A,$F5` / `MOV $F4,Y` / `MOV [$00]+Y,A` / `INC Y`: store, echo, and step the
            // pointer — `INC $01` too when the index wraps, so a block longer than 256 bytes walks
            // memory the way the ROM does.
            Ipl::ReadByte => {
                let index = self.ipl_index;
                let base = u16::from(self.ram[0]) | (u16::from(self.ram[1]) << 8);
                let addr = base.wrapping_add(u16::from(index));
                self.ram[usize::from(addr)] = self.to_apu[1];
                self.from_apu[0] = index;
                // The ROM's own echo, `MOV $F4,Y`: one of the four latches, SPC side.
                self.cpu.stage_port(0, index);
                self.ipl_index = self.ipl_index.wrapping_add(1);
                if self.ipl_index == 0 {
                    self.ram[1] = self.ram[1].wrapping_add(1);
                }
                self.ipl = Ipl::Transfer;
                IPL_STORE_CYCLES
            }
            // Not reachable through `step_cpu`, which re-arms the handshake before it gets here.
            Ipl::Running => {
                self.ipl = Ipl::AwaitKick;
                self.greet();
                IPL_KICK_POLL_CYCLES
            }
        }
    }

    /// The boot ROM's greeting: `A = $AA -> $F4`, `A = $BB -> $F5`.
    fn greet(&mut self) {
        let mut bus = SpcBus {
            ram: &mut self.ram,
            dsp: &mut self.dsp,
            to_apu: &mut self.to_apu,
            from_apu: &mut self.from_apu,
        };
        self.cpu.write(&mut bus, 0x00F4, 0xAA);
        self.cpu.write(&mut bus, 0x00F5, 0xBB);
    }

    /// `$2140`+index: the value the SPC last wrote.
    pub fn read_port(&mut self, index: usize) -> u8 {
        self.from_apu[index & 3]
    }

    /// `$2140`+index: into `to_apu`. The latches are all the S-CPU does; the boot ROM picks the
    /// value up when the SPC runs (§7.3). While a harness is tracing, the write is also a ring row:
    /// *when* it arrives relative to the SPC's own `$F4` reads is what the port handshake turns on.
    pub fn write_port(&mut self, index: usize, value: u8) {
        self.to_apu[index & 3] = value;
        self.cpu.record_scpu_port((index & 3) as u8, value);
    }

    /// `Start:` — `MOVW YA,$F6` / `MOVW $00,YA` / `MOVW YA,$F4` / `MOV $F4,A` / `MOV A,Y` /
    /// `MOV X,A` / `BNE Trans`, then either the transfer loop or `JMP [$0000+X]`.
    ///
    /// `$00`/`$01` become the write pointer (or the entry point), `A` is echoed to `$F4` — the
    /// acknowledgement the uploader waits for — and `$2141` is the mode: non-zero starts a block, zero
    /// enters the uploaded program.
    ///
    /// The entry is `JMP [$0000+X]` in the ROM's Start loop, and the three instructions before it are
    /// what a driver's first state is made of: `MOVW YA,$F4` leaves `Y = $F5`, `MOV A,Y` then `MOV X,A`
    /// leave `A = X = 0` on this path (it is only taken when `$F5` is zero), and `JMP` touches nothing
    /// else. **`SP` and the rest of `PSW` are not written.** fullsnes "SNES APU IPL ROM" has the
    /// listing; Anomie's "After power on, on entry to the first user code loaded the registers have
    /// the following values" (`A=0, X=0, Y=0, SP=$EF, PSW=$02`) describes the *first* entry, where
    /// `$EF`/`$02` are what the ROM's own reset path (`$FFC0`'s `MOV X,#$EF` / `MOV SP,X`) and the
    /// SPC's power-on state already left — not something the entry itself writes. A driver that
    /// jumps back into the window to ask for another block (`JMP $FFC9`, which fullsnes documents as
    /// "skip resetting the stack") keeps its stack, and a later `CALL`-style entry expects the return
    /// address it pushed to still be there.
    fn ipl_command(&mut self) -> u32 {
        self.ram[0] = self.to_apu[2];
        self.ram[1] = self.to_apu[3];
        self.from_apu[0] = self.to_apu[0];
        // `MOV $F4,A` in the ROM's Start loop: the acknowledgement the uploader polls for.
        self.cpu.stage_port(0, self.to_apu[0]);
        if self.to_apu[1] != 0 {
            self.ipl = Ipl::AwaitFirstByte;
            IPL_COMMAND_CYCLES
        } else {
            self.cpu.pc = u16::from(self.ram[0]) | (u16::from(self.ram[1]) << 8);
            // `MOV A,Y` / `MOV X,A` with `Y = $F5 = 0`: N and Z come from that zero, and nothing
            // else in PSW or SP moves.
            self.cpu.a = 0;
            self.cpu.x = 0;
            self.cpu.y = 0;
            self.cpu.psw.set_n(false);
            self.cpu.psw.set_z(true);
            self.ipl = Ipl::Running;
            IPL_ENTRY_CYCLES
        }
    }

    /// One DSP sample as interleaved f32, the machine's one float conversion (§7.5).
    fn push_sample(&mut self, l: i16, r: i16) {
        if self.out.len() + 2 > AUDIO_STAGING {
            // The front end is not draining: drop the backlog instead of allocating on the frame
            // path (§7.5 caps the staging buffer at `AUDIO_STAGING`).
            self.out.clear();
            self.read_pos = 0;
        }
        self.out.push(f32::from(l) / 32768.0);
        self.out.push(f32::from(r) / 32768.0);
        self.len = self.out.len();
    }

    /// Game > Reset: the APU, including its RAM, returns to power-on.
    pub fn reset(&mut self) {
        let mask = self.mask;
        let rate = self.sample_rate;
        *self = Apu::new(self.host);
        self.mask = mask;
        self.sample_rate = rate;
        self.dsp.set_mask(mask);
    }

    /// Interleaved stereo frames copied out; returns how many stereo frames were written.
    pub fn read_samples(&mut self, out: &mut [f32]) -> usize {
        let available = (self.out.len() - self.read_pos) & !1;
        let want = (out.len() & !1).min(available);
        out[..want].copy_from_slice(&self.out[self.read_pos..self.read_pos + want]);
        self.read_pos += want;
        if self.read_pos >= self.out.len() {
            self.out.clear();
            self.read_pos = 0;
        }
        self.len = self.out.len();
        want / 2
    }

    pub fn set_sample_rate(&mut self, hz: u32) {
        self.sample_rate = hz;
    }

    pub fn set_channel_mask(&mut self, mask: u8) {
        self.mask = mask;
        self.dsp.set_mask(mask);
    }

    /// §9.2's `Apu` and `Spc700` and `Dsp` rows, in the table's order.
    ///
    /// The layout is the host's, so the two cores keep the bytes they wrote before the move: the
    /// SNES writes RAM raw and `brr` as a `u32`, the SGB length-prefixes RAM and writes `brr` as a
    /// `u16` (§4.2's D2 and D3). Everything else is identical, signed words included: both hosts
    /// write a DSP `i16` as its two's-complement `u16`.
    pub fn save_state<W: StateWrite>(&self, w: &mut W) {
        w.bytes(&*self.ram);
        w.bytes(&self.to_apu);
        w.bytes(&self.from_apu);
        w.u8(self.ipl as u8);
        w.u8(self.ipl_index);
        w.u32(self.spc_acc);
        w.u32(self.sample_acc);
        w.u32(self.sample_rate);
        w.u8(self.mask);
        w.u64(self.cycles);
        self.cpu.save_state(w);
        self.dsp.save_state(w, self.host);
    }

    /// The inverse of [`Apu::save_state`], with the same host-dependent layout.
    pub fn load_state<R: StateRead>(&mut self, r: &mut R) -> Result<(), R::Error> {
        r.bytes_into(&mut *self.ram)?;
        r.bytes_into(&mut self.to_apu)?;
        r.bytes_into(&mut self.from_apu)?;
        self.ipl = Ipl::from_u8(r.u8()?);
        self.ipl_index = r.u8()?;
        self.spc_acc = r.u32()?;
        self.sample_acc = r.u32()?;
        self.sample_rate = r.u32()?;
        self.mask = r.u8()?;
        self.cycles = r.u64()?;
        self.cpu.load_state(r)?;
        self.dsp.load_state(r, self.host)?;
        self.dsp.set_mask(self.mask);
        Ok(())
    }
}

/// The IPL boot ROM's behaviour, as a state machine. The 64 bytes are a data sheet, not a
/// cartridge; each variant is a slice of the ROM's own control flow (§7.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ipl {
    /// `$FFCF`: `CMP $F4,#$CC` / `BNE -`, waiting for the uploader's kick.
    #[default]
    AwaitKick,
    /// `$FFD6`: `MOV Y,$F4` / `BNE Trans`, waiting for index 0 to start a block.
    AwaitFirstByte,
    /// The transfer loop's `CMP Y,$F4` / `BNE +` / `BPL -`: waiting for the next index.
    Transfer,
    /// The loop's store: `MOV A,$F5` / `MOV $F4,Y` / `MOV [$00]+Y,A`, one pass after the index.
    ReadByte,
    /// `JMP [$0000+X]`: the SPC700 is running cartridge code and the ports are the program's.
    Running,
}

impl Ipl {
    /// The address in the boot-ROM window whose instruction this pass stands for. The state machine
    /// [`Ipl`] replaces is a slice of the ROM's own control flow, so a trace row should name the
    /// instruction *that flow* was at, not `$FFC0` for every pass: `$FFCF` `CMP $F4,#$CC`,
    /// `$FFD6` `MOV Y,$F4`, `$FFDA` `CMP Y,$F4`, `$FFDE` `MOV A,$F5`, and `$FFC0` while the ROM is
    /// not in the window at all. fullsnes "SNES APU IPL ROM" has the listing these addresses are
    /// read from (see [`crate::apu::IPL_ROM`]).
    pub fn pc(self) -> u16 {
        match self {
            Ipl::AwaitKick => 0xFFCF,
            Ipl::AwaitFirstByte => 0xFFD6,
            Ipl::Transfer => 0xFFDA,
            Ipl::ReadByte => 0xFFDE,
            Ipl::Running => 0xFFC0,
        }
    }

    /// The state as §9.2's one byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// The inverse of [`Ipl::to_u8`]; an unknown value re-arms the handshake.
    pub fn from_u8(value: u8) -> Ipl {
        match value {
            1 => Ipl::AwaitFirstByte,
            2 => Ipl::Transfer,
            3 => Ipl::ReadByte,
            4 => Ipl::Running,
            _ => Ipl::AwaitKick,
        }
    }
}
