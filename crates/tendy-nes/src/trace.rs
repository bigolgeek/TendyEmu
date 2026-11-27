use crate::cpu::Bus;
use crate::cpu::opcodes::{Mn, Mode, OPS};
use crate::nes::Nes;
use std::fmt;
use std::io::{self, Write};

/// nestest.log field order without the disassembly:
/// "C000 A:00 X:00 Y:00 P:24 SP:FD PPU:  0,  6 CYC:7" (scanline and dot right-aligned in 3 columns).
pub fn trace_line(nes: &Nes) -> String {
    let c = &nes.cpu;
    format!(
        "{:04X} A:{:02X} X:{:02X} Y:{:02X} P:{:02X} SP:{:02X} PPU:{:3},{:3} CYC:{}",
        c.pc, c.a, c.x, c.y, c.p, c.sp, nes.bus.ppu.scanline, nes.bus.ppu.dot, c.cycles
    )
}

/// The full-run log line: `trace_line_disasm`'s grid with the `frame:scanline:dot` position
/// prefix prepended, exactly as pinned by §1.1 of `docs/spec-trace.md`:
///
/// ```text
/// <frame>:<scanline:3>:<dot:3> <PC>  <bytes:9><marker><text:32>A:<aa> X:<xx> Y:<yy> P:<pp> SP:<ss> CYC:<n>
/// ```
///
/// The grid is reproduced verbatim: §1.1 row 7 "chars 0..16 and 16..48 of it are the two column
/// groups `tests/trace.rs` already pins against `nestest.log`", row 4 "a single space after the
/// dot field, before the PC", row 6 "exactly two spaces after the PC", row 10 "no space is emitted
/// between [the register block and the text] ... with the 10-character `0:261:340 ` prefix the
/// first register character is line index 58". The position is §1.2's: the PPU's cached
/// frame/scanline/dot as they stood at the start of the instruction, read before any part of it
/// runs. `trace_line`'s old `PPU:{:3},{:3}` field is *replaced by* this prefix (§1.2); the legacy
/// one-liner keeps its exact output.
pub fn trace_log_line(nes: &mut Nes) -> String {
    let mut out = Vec::new();
    write_log_line(&mut out, nes).expect("writing to a Vec cannot fail");
    String::from_utf8(out).expect("ASCII trace line")
}

/// The §1.1 line written straight to `out` (no trailing newline), assembled from the position
/// prefix, the grid's 48-character PC/bytes/text group and a register block that never had the
/// `PPU:{:3},{:3}` pair in the first place — `trace_log_line`'s old build-then-cut. One `write!`
/// per field and no line-sized `String`, so a logged line costs no allocations.
fn write_log_line<W: Write>(out: &mut W, nes: &mut Nes) -> io::Result<()> {
    // §4.1: the logger only reads cached fields, so this is three plain field reads.
    let (frame, scanline, dot) = (nes.bus.ppu.frame, nes.bus.ppu.scanline, nes.bus.ppu.dot);
    write!(out, "{frame}:{scanline:>3}:{dot:>3} ")?;
    write_pc_group(out, nes)?;
    // §1.1 row 12 keeps "one space after the register block, as in `trace_line`" before `CYC:`.
    let c = &nes.cpu;
    write!(
        out,
        "A:{:02X} X:{:02X} Y:{:02X} P:{:02X} SP:{:02X} CYC:{}",
        c.a, c.x, c.y, c.p, c.sp, c.cycles
    )
}

/// Why a traced frame stopped (§6 PR-A: a `--trace-limit` stop must not step an extra
/// instruction, so the frame driver reports it instead of pretending the frame finished).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameEnd {
    /// The PPU reached the start of vblank: a whole frame ran.
    Frame,
    /// `Tracer::max_lines` was reached; no further instruction was run.
    Limit,
}

/// Writes one trace line before every instruction it runs.
pub struct Tracer<W: Write> {
    pub out: W,
    pub lines: u64,
    pub symbols: Vec<(u16, String)>,
    /// `--trace-limit`: stop after this many logged lines (§2, §6 PR-A). `None` logs the whole
    /// run. Enforced by the frame drivers (`run_frame` / `log_frame`), which check it *before*
    /// stepping, so the stop never runs an extra instruction; `step`/`log_file_line` stay raw
    /// single-instruction operations and leave the budget to their caller.
    pub max_lines: Option<u64>,
    /// `--trace-from <N>` (§2): only log instructions whose start-of-instruction frame field is
    /// `>= N`; earlier frames are warmup. `None` logs the whole run. Enforced in
    /// `log_file_line`, the one place the §1.1 file-format line is produced, so the frame driver
    /// `log_frame` gets the window for free. §2 pins warmup: frames before N still emulate in
    /// full (the un-windowed reads and the step run identically), they just write nothing and do
    /// not consume the budget ("warmup lines do not consume the budget").
    pub from_frame: Option<u64>,
}
impl<W: Write> Tracer<W> {
    pub fn new(out: W) -> Self {
        Tracer {
            out,
            lines: 0,
            symbols: Vec::new(),
            max_lines: None,
            from_frame: None,
        }
    }
    /// A tracer that appends "; name" to lines whose PC has an imported symbol.
    pub fn with_symbols(out: W, mut symbols: Vec<(u16, String)>) -> Self {
        symbols.sort_by_key(|(a, _)| *a);
        Tracer {
            out,
            lines: 0,
            symbols,
            max_lines: None,
            from_frame: None,
        }
    }
    /// Stop after `max_lines` logged lines: the frame drivers return `FrameEnd::Limit` as soon as
    /// `lines` reaches it, before running another instruction (§6 PR-A: "the stop must not step an
    /// extra instruction").
    pub fn stop_after(mut self, max_lines: u64) -> Self {
        self.max_lines = Some(max_lines);
        self
    }
    /// Only log from frame `from` on (§2 `--trace-from`): earlier frames still emulate in full
    /// (warmup — "the run reaches the same CPU/PPU state as an un-windowed run of the same
    /// length"), they just write nothing, and warmup lines do not consume the `--trace-limit`
    /// budget. "`--trace-from` never advances emulation state to reach it."
    pub fn from_frame(mut self, from: u64) -> Self {
        self.from_frame = Some(from);
        self
    }
    /// The `--trace-limit` budget is spent.
    pub fn at_limit(&self) -> bool {
        self.max_lines.is_some_and(|max| self.lines >= max)
    }
    /// End one line: the `; name` annotation when a symbol sits exactly at `pc`, then a
    /// newline. Both line writers end here, so the label lookup exists once.
    fn end_line(&mut self, pc: u16) -> io::Result<()> {
        match self.symbols.binary_search_by_key(&pc, |(a, _)| *a) {
            Ok(i) => writeln!(self.out, " ; {}", self.symbols[i].1),
            Err(_) => self.out.write_all(b"\n"),
        }
    }
    /// Trace and run one instruction; returns its CPU cycles.
    pub fn step(&mut self, nes: &mut Nes) -> io::Result<u32> {
        self.out.write_all(trace_line(nes).as_bytes())?;
        self.end_line(nes.cpu.pc)?;
        self.lines += 1;
        Ok(nes.step())
    }
    /// The §1.1 full-run line for the next instruction, written *before* it runs, then the
    /// instruction: `Tracer::step`'s order (write BEFORE execute, so the columns describe the
    /// machine at entry). Returns the instruction's CPU cycles.
    ///
    /// Symbol annotation (§2): labels are exact-PC, and the annotation is the trailing
    /// comment `; <name>` appended after `CYC:` on the line whose PC exactly equals a label
    /// address; no suffix on other lines ("the label table is applied to the *file-format* line
    /// in §1, not to the legacy one-liner"). Reuses `with_symbols`' binary search on sorted
    /// labels — a memory-side lookup, so it cannot perturb emulation (§4.1).
    ///
    /// Frame window (§2 `--trace-from`): the start-of-instruction frame field below N means
    /// warmup — the instruction is emulated in full through the exact same path (the cached
    /// reads and `Nes::step` run identically to an un-windowed run, §4), but nothing is written
    /// and no logged line is counted, so warmup cannot consume `--trace-limit` either.
    pub fn log_file_line(&mut self, nes: &mut Nes) -> io::Result<u32> {
        // §1.2: prefix and window test read the PPU's cached frame at the start of the
        // instruction, before any part of it runs. The window is tested *before* the line
        // is built, so a warmup instruction pays no disassembly and reads no cartridge bytes.
        let start_frame = nes.bus.ppu.frame;
        if self.from_frame.is_some_and(|from| start_frame < from) {
            return Ok(nes.step());
        }
        write_log_line(&mut self.out, nes)?;
        self.end_line(nes.cpu.pc)?;
        self.lines += 1;
        Ok(nes.step())
    }
    /// One frame driven through `one` per instruction, stopped before any further instruction
    /// once the `--trace-limit` budget is spent (§6 PR-A: "the stop must not step an extra
    /// instruction"). `log_frame` and `run_frame` differ only in the per-instruction function
    /// and in what they return, so the loop exists once.
    fn drive_frame(
        &mut self,
        nes: &mut Nes,
        one: fn(&mut Self, &mut Nes) -> io::Result<u32>,
    ) -> io::Result<FrameEnd> {
        if self.at_limit() {
            return Ok(FrameEnd::Limit);
        }
        nes.bus.ppu.frame_ready = false;
        while !nes.bus.ppu.frame_ready {
            if self.at_limit() {
                return Ok(FrameEnd::Limit);
            }
            one(self, nes)?;
        }
        Ok(FrameEnd::Frame)
    }
    /// Trace and run one frame with the §1.1 file format, stopping early at `max_lines`.
    ///
    /// Flushing is the caller's job: §2 wants the `BufWriter` flushed at every frame boundary, so
    /// the run loop flushes after each `FrameEnd::Frame` and at a `FrameEnd::Limit` stop.
    pub fn log_frame(&mut self, nes: &mut Nes) -> io::Result<FrameEnd> {
        self.drive_frame(nes, Self::log_file_line)
    }
    /// Trace and run until the next vblank (like `Nes::run_frame`), returning early when
    /// `max_lines` is reached: the limit is checked before each step, so no extra instruction runs.
    pub fn run_frame(&mut self, nes: &mut Nes) -> io::Result<()> {
        self.drive_frame(nes, Self::step).map(|_| ())
    }
}

/// How many operand bytes follow the opcode for `mode`.
fn operand_len(mode: Mode) -> usize {
    match mode {
        Mode::Imp | Mode::Acc => 0,
        Mode::Imm | Mode::Zp | Mode::Zpx | Mode::Zpy | Mode::Izx | Mode::Izy | Mode::Rel => 1,
        Mode::Abs | Mode::Abx | Mode::Aby | Mode::Ind => 2,
    }
}

/// The opcode and its operand bytes at `pc`, read through [`Bus::peek`](crate::cpu::Bus::peek):
/// RAM ($0000-$1FFF) and cartridge ($6000-$FFFF) only, so disassembly has no side effects, and
/// the bytes are the ones the CPU runs — Game Genie substitutions included, with no
/// mapper read side effects. Slots past the instruction's length stay `None`, and a
/// fixed array keeps the trace hot path free of per-instruction `Vec`s.
fn fetch_bytes(nes: &mut Nes, pc: u16) -> [Option<u8>; 3] {
    let mut bytes = [None; 3];
    bytes[0] = nes.bus.peek(pc);
    let len = bytes[0].map_or(0, |op| operand_len(OPS[op as usize].mode));
    for (i, slot) in bytes.iter_mut().enumerate().take(len + 1).skip(1) {
        *slot = nes.bus.peek(pc.wrapping_add(i as u16));
    }
    bytes
}

/// `Mn`'s Debug name, uppercased into a fixed stack buffer: the mnemonic column without a
/// per-instruction `String`. The longest mnemonic is three letters.
struct Upper<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl fmt::Write for Upper<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            *self.buf.get_mut(self.len).ok_or(fmt::Error)? = b.to_ascii_uppercase();
            self.len += 1;
        }
        Ok(())
    }
}

/// Writes one disassembly byte: two upper-case hex digits, or `??` when it cannot be read.
fn write_hex<W: Write>(out: &mut W, b: Option<u8>) -> io::Result<()> {
    match b {
        Some(v) => write!(out, "{v:02X}"),
        None => out.write_all(b"??"),
    }
}

/// `n` spaces.
fn pad<W: Write>(out: &mut W, n: usize) -> io::Result<()> {
    for _ in 0..n {
        out.write_all(b" ")?;
    }
    Ok(())
}

/// Writes the instruction's bytes (`AA BB CC`, one space between them).
fn write_bytes<W: Write>(out: &mut W, bytes: &[Option<u8>; 3], operand: usize) -> io::Result<()> {
    for (i, b) in bytes.iter().enumerate().take(operand + 1) {
        if i > 0 {
            out.write_all(b" ")?;
        }
        write_hex(out, *b)?;
    }
    Ok(())
}

/// `write_bytes` padded to nestest's 9-character column. Every byte is exactly two characters,
/// so the padding is arithmetic rather than a formatted `String`.
fn write_bytes_padded<W: Write>(
    out: &mut W,
    bytes: &[Option<u8>; 3],
    operand: usize,
) -> io::Result<()> {
    write_bytes(out, bytes, operand)?;
    pad(out, 9 - (3 * (operand + 1) - 1))
}

/// Writes the mnemonic and returns its length.
fn write_mnemonic<W: Write>(out: &mut W, mn: Mn) -> io::Result<usize> {
    let mut buf = [0u8; 8];
    let len = {
        let mut upper = Upper {
            buf: &mut buf,
            len: 0,
        };
        fmt::write(&mut upper, format_args!("{mn:?}"))
            .map_err(|_| io::Error::other("opcode mnemonic longer than the trace buffer"))?;
        upper.len
    };
    out.write_all(&buf[..len])?;
    Ok(len)
}

/// The operand column: `#$nn`, `$nn`, `$nn,X`, `$nn,Y`, `$nnnn`, `$nnnn,X`, `$nnnn,Y`, `($nnnn)`,
/// `($nn,X)`, `($nn),Y`, `A`, or the absolute branch target for Rel. Returns its length, the
/// leading space included. Every branch is a fixed length, even where a byte reads `??`.
fn write_operand<W: Write>(
    out: &mut W,
    mode: Mode,
    bytes: &[Option<u8>; 3],
    pc: u16,
) -> io::Result<usize> {
    let n = match mode {
        Mode::Imp => 0,
        Mode::Acc => {
            out.write_all(b" A")?;
            2
        }
        Mode::Imm => {
            out.write_all(b" #$")?;
            write_hex(out, bytes[1])?;
            5
        }
        Mode::Zp => {
            out.write_all(b" $")?;
            write_hex(out, bytes[1])?;
            4
        }
        Mode::Zpx | Mode::Zpy => {
            out.write_all(b" $")?;
            write_hex(out, bytes[1])?;
            out.write_all(if mode == Mode::Zpx { b",X" } else { b",Y" })?;
            6
        }
        Mode::Abs | Mode::Abx | Mode::Aby | Mode::Ind => {
            out.write_all(if mode == Mode::Ind {
                &b" ($"[..]
            } else {
                &b" $"[..]
            })?;
            // High byte first: $nnnn is bytes[2] then bytes[1].
            write_hex(out, bytes[2])?;
            write_hex(out, bytes[1])?;
            match mode {
                Mode::Abx => {
                    out.write_all(b",X")?;
                    8
                }
                Mode::Aby => {
                    out.write_all(b",Y")?;
                    8
                }
                Mode::Ind => {
                    out.write_all(b")")?;
                    8
                }
                _ => 6,
            }
        }
        Mode::Izx => {
            out.write_all(b" ($")?;
            write_hex(out, bytes[1])?;
            out.write_all(b",X)")?;
            8
        }
        Mode::Izy => {
            out.write_all(b" ($")?;
            write_hex(out, bytes[1])?;
            out.write_all(b"),Y")?;
            8
        }
        Mode::Rel => {
            // The absolute branch target: pc + 2 + sign-extended offset byte.
            let off = bytes[1].unwrap_or(0) as i8 as i32;
            write!(
                out,
                " ${:04X}",
                ((pc as u32).wrapping_add(2).wrapping_add(off as u32)) & 0xFFFF
            )?;
            6
        }
    };
    Ok(n)
}

/// "4C F5 C5  JMP $C5F5"-style bytes plus the mnemonic and a nestest.log-style operand for the
/// instruction at `pc`: `#$nn`, `$nn`, `$nn,X`, `$nn,Y`, `$nnnn`, `$nnnn,X`, `$nnnn,Y`, `($nnnn)`,
/// `($nn,X)`, `($nn),Y`, `A`, and the absolute branch target for Rel. Illegal opcodes get nestest's
/// `*` marker, e.g. "*NOP #$89". The ` = value` memory annotation and the ` @ ea` effective-address
/// suffix nestest puts on indexed memory touches are not produced. I/O bytes print as "??";
/// reads stay limited to RAM and cartridge and go through `SysBus::peek`, so disassembly has no
/// side effects and shows the bytes the CPU runs.
pub fn disasm(nes: &mut Nes, pc: u16) -> String {
    let mut out = Vec::new();
    write_disasm(&mut out, nes, pc).expect("writing to a Vec cannot fail");
    String::from_utf8(out).expect("ASCII disassembly")
}

/// `disasm` written straight to `out`, so the trace path needs no line-sized `String`.
fn write_disasm<W: Write>(out: &mut W, nes: &mut Nes, pc: u16) -> io::Result<()> {
    let bytes = fetch_bytes(nes, pc);
    let Some(op) = bytes[0] else {
        write_bytes(out, &bytes, 0)?;
        return out.write_all(b"  ??");
    };
    let entry = &OPS[op as usize];
    write_bytes(out, &bytes, operand_len(entry.mode))?;
    out.write_all(b"  ")?;
    if !entry.official {
        out.write_all(b"*")?;
    }
    write_mnemonic(out, entry.mn)?;
    write_operand(out, entry.mode, &bytes, pc)?;
    Ok(())
}

/// The PC/bytes/marker/text group of the trace grid (48 characters: `%04X`, two spaces, the
/// 9-wide byte column, the marker column, the 32-wide text), written straight to `out`.
fn write_pc_group<W: Write>(out: &mut W, nes: &mut Nes) -> io::Result<()> {
    let pc = nes.cpu.pc;
    let bytes = fetch_bytes(nes, pc);
    write!(out, "{pc:04X}  ")?;
    match bytes[0] {
        None => {
            write_bytes_padded(out, &bytes, 0)?;
            out.write_all(b" ")?;
            out.write_all(b"??")?;
            pad(out, 30)?;
        }
        Some(op) => {
            let entry = &OPS[op as usize];
            write_bytes_padded(out, &bytes, operand_len(entry.mode))?;
            // nestest's illegal-opcode marker sits in the space column after the bytes, so
            // "*NOP ..." renders as "80 89     *NOP ..." with the field starting at the mnemonic.
            out.write_all(if entry.official { b" " } else { b"*" })?;
            let text = write_mnemonic(out, entry.mn)? + write_operand(out, entry.mode, &bytes, pc)?;
            pad(out, 32usize.saturating_sub(text))?;
        }
    }
    Ok(())
}

/// `trace_line()` with a nestest.log-shaped disassembly column after the PC:
/// "C000  4C F5 C5  JMP $C5F5                       A:00 X:00 ...". The bytes are padded to
/// nestest's 9-character column and the mnemonic+operand to its 32-character one; nestest's
/// illegal-opcode marker sits in the space column after the bytes, so "*NOP ..." renders as
/// "80 89     *NOP ..." with the field itself starting at the mnemonic.
pub fn trace_line_disasm(nes: &mut Nes) -> String {
    let mut out = Vec::new();
    write_grid(&mut out, nes).expect("writing to a Vec cannot fail");
    String::from_utf8(out).expect("ASCII disassembly")
}

/// `trace_line_disasm` written straight to `out`.
fn write_grid<W: Write>(out: &mut W, nes: &mut Nes) -> io::Result<()> {
    write_pc_group(out, nes)?;
    let c = &nes.cpu;
    write!(
        out,
        "A:{:02X} X:{:02X} Y:{:02X} P:{:02X} SP:{:02X} PPU:{:3},{:3} CYC:{}",
        c.a, c.x, c.y, c.p, c.sp, nes.bus.ppu.scanline, nes.bus.ppu.dot, c.cycles
    )
}
