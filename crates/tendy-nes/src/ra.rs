//! RetroAchievements, offline half: the MemAddr condition engine, the NES memory bridge
//! and the RetroAchievements game hash. No network, login or token handling: this module never
//! talks to the service, and it is not wired into a frame yet.
//!
//! The parser covers a deliberate subset of RetroAchievements' MemAddr text format:
//!
//! * sizes `0xH` (8-bit), `0x ` (16-bit little-endian), `0xX` (32-bit little-endian), `0xM` to
//!   `0xT` (single bits) and `0xL`/`0xU` (low/high nibble);
//! * operands: a memory address, `d` (delta: the previous frame's value), `p` (prior: the last
//!   value that differed) and constants (decimal, or `h` then hex);
//! * comparisons `=`, `!=`, `<`, `<=`, `>` and `>=`;
//! * hit targets written `.N.`, and flags `R:` ResetIf, `P:` PauseIf, `A:` AddSource,
//!   `B:` SubSource and `N:` AndNext;
//! * a core group and any number of alt groups, separated by `S`.
//!
//! Anything outside that subset is a [`ParseError`] naming the token: the 24-bit/BE/float/BitCount
//! sizes, `Measured`/`MeasuredIf`, `AddHits`/`SubHits`, `OrNext`, `AddAddress`, `Remember`,
//! `ResetNextIf`, indirect addressing, BCD/inversion prefixes and the legacy `(N)` hit syntax.
//! Parsing never panics and never drops a condition silently.
//!
//! `docs/ra-decision.md` is the reference: what is accepted and refused, why this is a subset
//! rather than a port of rcheevos (MIT, read and cited, not vendored), the evaluation order, the
//! NES memory map, the game hash and the one known difference from rcheevos.

use crate::bus::SysBus;
use crate::checksum;
use crate::cpu::Bus;
use std::fmt;

/// The MD5 RetroAchievements identifies a NES game by.
///
/// RA hashes the ROM with its iNES/NES 2.0 header removed: rcheevos strips 16 bytes when the file
/// starts with `NES\x1a`, and does the same for Famicom Disk System images that start with
/// `FDS\x1a`; a headerless file is hashed whole (`rc_hash_rom.c:208-224`). It does not skip a
/// 512-byte trainer, so this function does not either: matching RA's database byte for byte is
/// the point.
pub fn game_hash(rom: &[u8]) -> String {
    let header = rom.len() > 16 && (rom.starts_with(b"NES\x1a") || rom.starts_with(b"FDS\x1a"));
    let body = if header { &rom[16..] } else { rom };
    checksum::hex(&checksum::md5(body))
}

/// The CPU bus address a RetroAchievements NES address reads, or `None` when this emulator does
/// not expose that byte.
///
/// RA's NES map (`rc_consoleinfo.c:725-737`, following the NES CPU memory map) gives the console
/// 11 regions. `$0000-$07FF` is system RAM and `$0800-$1FFF` are its three mirrors, so all four
/// copies are the same byte; `$6000-$7FFF` is cartridge RAM and `$8000-$FFFF` cartridge ROM. The
/// PPU and APU register ranges (`$2000-$401F`) and `$4020-$5FFF` (mapper-dependent cartridge
/// data) have no side-effect-free read here, so they are `None`.
pub fn nes_bus_address(addr: u32) -> Option<u16> {
    match addr {
        0x0000..=0x1FFF => Some((addr & 0x07FF) as u16),
        0x6000..=0xFFFF => Some(addr as u16),
        _ => None,
    }
}

/// Read one byte for the achievement engine, through [`Bus::peek`]: RAM and cartridge only, with
/// no read side effects. Memory RA defines but this emulator does not expose reads 0, which is
/// what rcheevos reports for an address a core does not provide.
pub fn read_nes(bus: &mut SysBus, addr: u32) -> u8 {
    nes_bus_address(addr)
        .and_then(|bus_addr| bus.peek(bus_addr))
        .unwrap_or(0)
}

/// A MemAddr string that could not be parsed, naming the token it stopped at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The offending text, quoted from the input; `end of string` at the end.
    pub token: String,
    /// What was expected or why the token is out of scope.
    pub reason: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot parse `{}`: {}", self.token, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// A parsed achievement: the core group, its alt groups, and one entry per distinct memory
/// reference so every frame reads each address once.
#[derive(Debug)]
pub struct Achievement {
    core: Group,
    alts: Vec<Group>,
    memrefs: Vec<MemRef>,
    /// False until a frame has been evaluated in which the achievement did not fire.
    primed: bool,
    unlocked: bool,
}

impl Achievement {
    /// Parse a MemAddr string. The whole string must be in the subset documented on the module;
    /// everything else is a [`ParseError`].
    pub fn new(memaddr: &str) -> Result<Self, ParseError> {
        let mut parser = Parser::new(memaddr);
        let (core, alts) = parser.parse_trigger()?;
        Ok(Self {
            core,
            alts,
            memrefs: parser.memrefs,
            primed: false,
            unlocked: false,
        })
    }

    /// Evaluate the achievement once, as one frame. `read` is called with RetroAchievements NES
    /// addresses and returns the byte there; [`read_nes`] adapts a [`SysBus`]. Returns true on the
    /// frame the achievement unlocks and false on every other frame, including after it has
    /// unlocked.
    pub fn step(&mut self, mut read: impl FnMut(u32) -> u8) -> bool {
        if self.unlocked {
            return false;
        }

        for memref in &mut self.memrefs {
            let value = memref.read(&mut read);
            if value == memref.value {
                memref.changed = false;
            } else {
                memref.prior = memref.value;
                memref.value = value;
                memref.changed = true;
            }
        }

        let memrefs = &self.memrefs;
        let mut was_reset = false;
        let core_true = eval_group(&mut self.core, memrefs, &mut was_reset);
        let mut any_alt = self.alts.is_empty();
        for alt in &mut self.alts {
            any_alt |= eval_group(alt, memrefs, &mut was_reset);
        }

        if was_reset {
            clear_hits(&mut self.core);
            for alt in &mut self.alts {
                clear_hits(alt);
            }
        }

        let fired = !was_reset && core_true && any_alt;
        if fired {
            if !self.primed {
                // rcheevos' WAITING state: the first frame the conditions hold is thrown away,
                // along with the hits it tallied.
                clear_hits(&mut self.core);
                for alt in &mut self.alts {
                    clear_hits(alt);
                }
                return false;
            }
            self.unlocked = true;
            return true;
        }

        self.primed = true;
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Size {
    Bit(u8),
    Low,
    High,
    Byte,
    Word,
    Dword,
}

/// One distinct (address, size) pair, with the values delta and prior are defined against.
#[derive(Clone, Copy, Debug)]
struct MemRef {
    address: u32,
    size: Size,
    value: u32,
    prior: u32,
    changed: bool,
}

impl MemRef {
    fn read(&self, read: &mut impl FnMut(u32) -> u8) -> u32 {
        // A 32-bit read near $FFFFFFFF wraps; unreachable for NES (addresses <= $FFFF).
        let mut byte = |offset: u32| read(self.address.wrapping_add(offset)) as u32;
        match self.size {
            Size::Bit(bit) => (byte(0) >> bit) & 1,
            Size::Low => byte(0) & 0x0F,
            Size::High => byte(0) >> 4,
            Size::Byte => byte(0),
            Size::Word => byte(0) | byte(1) << 8,
            Size::Dword => byte(0) | byte(1) << 8 | byte(2) << 16 | byte(3) << 24,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperandKind {
    Mem,
    Delta,
    Prior,
    Value,
}

#[derive(Clone, Copy, Debug)]
struct Operand {
    kind: OperandKind,
    /// Index into `Achievement::memrefs`; meaningless for [`OperandKind::Value`].
    memref: usize,
    /// The constant; meaningless for the memory kinds.
    value: u32,
}

#[derive(Clone, Copy, Debug)]
enum Compare {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Compare {
    fn test(self, left: u32, right: u32) -> bool {
        match self {
            Self::Eq => left == right,
            Self::Ne => left != right,
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Gt => left > right,
            Self::Ge => left >= right,
        }
    }
}

#[derive(Clone, Debug)]
struct Condition {
    left: Operand,
    op: Compare,
    right: Operand,
    required_hits: u32,
    hits: u32,
}

#[derive(Clone, Copy, Debug)]
struct Source {
    sub: bool,
    operand: Operand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitKind {
    Pause,
    Reset,
    Hits,
    Other,
}

/// A condition together with the `A:`/`B:` sources and `N:` gates written before it. `N:` and
/// the source flags apply to the next condition, so parsing folds them into one evaluation unit.
#[derive(Debug)]
struct Unit {
    sources: Vec<Source>,
    gates: Vec<Condition>,
    target: Condition,
    kind: UnitKind,
}

#[derive(Debug, Default)]
struct Group {
    units: Vec<Unit>,
}

fn operand_value(operand: &Operand, memrefs: &[MemRef]) -> u32 {
    let memref = match operand.kind {
        OperandKind::Value => return operand.value,
        _ => &memrefs[operand.memref],
    };
    match operand.kind {
        OperandKind::Delta if memref.changed => memref.prior,
        OperandKind::Prior => memref.prior,
        _ => memref.value,
    }
}

/// Evaluate one condition, applying `gate` (the accumulated `N:` result) and `offset` (the
/// accumulated `A:`/`B:` total, added to the left side only).
fn eval_condition(condition: &mut Condition, memrefs: &[MemRef], gate: bool, offset: u32) -> bool {
    let left = operand_value(&condition.left, memrefs).wrapping_add(offset);
    let right = operand_value(&condition.right, memrefs);
    let mut valid = condition.op.test(left, right) && gate;

    if condition.required_hits == 0 {
        return valid;
    }

    if valid {
        condition.hits = condition
            .hits
            .saturating_add(1)
            .min(condition.required_hits);
        valid = condition.hits == condition.required_hits;
    } else if condition.hits > 0 {
        // Once the target is met the condition stays true until a ResetIf clears the hits.
        valid = condition.hits == condition.required_hits;
    }
    valid
}

fn eval_unit(unit: &mut Unit, memrefs: &[MemRef]) -> bool {
    let mut gate = true;
    for gate_condition in &mut unit.gates {
        gate = eval_condition(gate_condition, memrefs, gate, 0);
    }

    let mut offset = 0u32;
    for source in &unit.sources {
        let value = operand_value(&source.operand, memrefs);
        offset = if source.sub {
            offset.wrapping_sub(value)
        } else {
            offset.wrapping_add(value)
        };
    }

    eval_condition(&mut unit.target, memrefs, gate, offset)
}

/// Evaluate one group. `was_reset` is trigger-wide: rcheevos evaluates a group's `ResetIf`s even
/// when an earlier group already reset, and their hits are all cleared together afterwards.
fn eval_group(group: &mut Group, memrefs: &[MemRef], was_reset: &mut bool) -> bool {
    // PauseIf first: any true pause stops the group before hit counts move or a reset is seen.
    for unit in group.units.iter_mut().filter(|u| u.kind == UnitKind::Pause) {
        if eval_unit(unit, memrefs) {
            return false;
        }
    }

    for unit in group.units.iter_mut().filter(|u| u.kind == UnitKind::Reset) {
        if eval_unit(unit, memrefs) {
            *was_reset = true;
            break;
        }
    }

    let mut is_true = !*was_reset;
    if !*was_reset {
        // Hit-target conditions are evaluated every frame so their counts keep moving.
        for unit in group.units.iter_mut().filter(|u| u.kind == UnitKind::Hits) {
            is_true &= eval_unit(unit, memrefs);
        }
    }

    if is_true {
        for unit in group.units.iter_mut().filter(|u| u.kind == UnitKind::Other) {
            if !eval_unit(unit, memrefs) {
                is_true = false;
                break;
            }
        }
    }

    is_true
}

fn clear_hits(group: &mut Group) {
    for unit in &mut group.units {
        unit.target.hits = 0;
        for gate in &mut unit.gates {
            gate.hits = 0;
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    memrefs: Vec<MemRef>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            memrefs: Vec::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + offset).copied()
    }

    fn at_alt(&self) -> bool {
        matches!(self.peek(), Some(b'S' | b's'))
    }

    /// The text from `start` up to the next separator, for an error message.
    fn token_at(&self, start: usize) -> String {
        let rest = &self.src[start.min(self.src.len())..];
        let end = rest
            .find(['_', '=', '!', '<', '>', ' ', '\t'])
            .unwrap_or(rest.len());
        if end == 0 {
            "end of string".to_string()
        } else {
            rest[..end].to_string()
        }
    }

    fn err(&self, start: usize, reason: &'static str) -> ParseError {
        ParseError {
            token: self.token_at(start),
            reason,
        }
    }

    fn parse_trigger(&mut self) -> Result<(Group, Vec<Group>), ParseError> {
        if self.src.trim().is_empty() {
            return Err(self.err(0, "empty condition string"));
        }

        let core = if self.at_alt() {
            // A blank core group is allowed: the achievement is any one alt group.
            Group::default()
        } else {
            let start = self.pos;
            let core = self.parse_group()?;
            if core.units.is_empty() {
                return Err(self.err(start, "expected a condition"));
            }
            core
        };

        let mut alts = Vec::new();
        while self.at_alt() {
            self.pos += 1;
            let start = self.pos;
            let alt = self.parse_group()?;
            if alt.units.is_empty() {
                return Err(self.err(start, "expected a condition in the alt group"));
            }
            alts.push(alt);
        }

        if self.pos != self.src.len() {
            return Err(self.err(
                self.pos,
                "expected `_` between conditions or `S` before an alt group",
            ));
        }

        Ok((core, alts))
    }

    fn parse_group(&mut self) -> Result<Group, ParseError> {
        let mut units = Vec::new();
        let mut sources = Vec::new();
        let mut gates = Vec::new();

        loop {
            if self.peek().is_none() || self.at_alt() {
                if !sources.is_empty() || !gates.is_empty() {
                    return Err(self.err(
                        self.pos,
                        "an AddSource, SubSource or AndNext has no condition to modify",
                    ));
                }
                break;
            }

            self.parse_clause(&mut sources, &mut gates, &mut units)?;

            match self.peek() {
                Some(b'_') => {
                    self.pos += 1;
                    if self.peek().is_none() {
                        return Err(self.err(self.pos, "expected a condition after `_`"));
                    }
                }
                // End of text and `S` fall to the loop top, which closes the group and names a
                // dangling AddSource/SubSource/AndNext there.
                Some(_) if !self.at_alt() => {
                    return Err(self.err(
                        self.pos,
                        "expected `_` between conditions or `S` before an alt group",
                    ));
                }
                _ => {}
            }
        }

        Ok(Group { units })
    }

    fn parse_clause(
        &mut self,
        sources: &mut Vec<Source>,
        gates: &mut Vec<Condition>,
        units: &mut Vec<Unit>,
    ) -> Result<(), ParseError> {
        let start = self.pos;
        let flag = self.parse_flag()?;

        let (target, kind) = match flag {
            Some(b'A' | b'B') => {
                let operand = self.parse_operand()?;
                if !matches!(self.peek(), None | Some(b'_' | b'S' | b's')) {
                    return Err(
                        self.err(start, "modifiers on AddSource/SubSource are not supported")
                    );
                }
                sources.push(Source {
                    sub: flag == Some(b'B'),
                    operand,
                });
                return Ok(());
            }
            Some(b'N') => {
                let condition = self.parse_comparison(start)?;
                gates.push(condition);
                return Ok(());
            }
            Some(b'P') => (self.parse_comparison(start)?, UnitKind::Pause),
            Some(b'R') => (self.parse_comparison(start)?, UnitKind::Reset),
            None => {
                let condition = self.parse_comparison(start)?;
                let kind = if condition.required_hits > 0 {
                    UnitKind::Hits
                } else {
                    UnitKind::Other
                };
                (condition, kind)
            }
            Some(_) => unreachable!("parse_flag only returns supported flags"),
        };

        units.push(Unit {
            sources: std::mem::take(sources),
            gates: std::mem::take(gates),
            target,
            kind,
        });
        Ok(())
    }

    /// An optional two-letter flag at the cursor, as its uppercase letter.
    fn parse_flag(&mut self) -> Result<Option<u8>, ParseError> {
        let Some(letter) = self.peek() else {
            return Ok(None);
        };
        if self.peek_at(1) != Some(b':') {
            return Ok(None);
        }
        let start = self.pos;
        let upper = letter.to_ascii_uppercase();
        match upper {
            b'A' | b'B' | b'N' | b'P' | b'R' => {
                self.pos += 2;
                Ok(Some(upper))
            }
            _ => Err(self.err(
                start,
                "only R: (ResetIf), P: (PauseIf), A: (AddSource), B: (SubSource) and N: (AndNext) are supported",
            )),
        }
    }

    /// One `left op right` comparison with its optional hit target.
    fn parse_comparison(&mut self, start: usize) -> Result<Condition, ParseError> {
        let left = self.parse_operand()?;
        let op = self.parse_compare().ok_or_else(|| {
            self.err(
                start,
                "expected a comparison: `=`, `!=`, `<`, `<=`, `>` or `>=`",
            )
        })?;
        let right = self.parse_operand()?;
        let required_hits = self.parse_hits(start)?;
        Ok(Condition {
            left,
            op,
            right,
            required_hits,
            hits: 0,
        })
    }

    fn parse_compare(&mut self) -> Option<Compare> {
        match self.peek()? {
            b'=' => {
                self.pos += 1;
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                }
                Some(Compare::Eq)
            }
            b'!' => {
                if self.peek_at(1) != Some(b'=') {
                    return None;
                }
                self.pos += 2;
                Some(Compare::Ne)
            }
            b'<' => {
                self.pos += 1;
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Some(Compare::Le)
                } else {
                    Some(Compare::Lt)
                }
            }
            b'>' => {
                self.pos += 1;
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Some(Compare::Ge)
                } else {
                    Some(Compare::Gt)
                }
            }
            _ => None,
        }
    }

    fn parse_hits(&mut self, start: usize) -> Result<u32, ParseError> {
        match self.peek() {
            Some(b'.') => {
                self.pos += 1;
                let hits = self.parse_decimal()?;
                if self.peek() != Some(b'.') {
                    return Err(self.err(start, "expected `.` after the hit count"));
                }
                self.pos += 1;
                Ok(hits)
            }
            Some(b'(') => Err(self.err(
                start,
                "the legacy `(N)` hit syntax is not supported; write `.N.`",
            )),
            _ => Ok(0),
        }
    }

    fn parse_decimal(&mut self) -> Result<u32, ParseError> {
        let start = self.pos;
        let digits_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(self.err(start, "expected a decimal number"));
        }
        self.src[digits_start..self.pos]
            .parse()
            .map_err(|_| self.err(start, "number does not fit in 32 bits"))
    }

    fn parse_operand(&mut self) -> Result<Operand, ParseError> {
        let start = self.pos;

        let kind = match self.peek() {
            Some(b'd' | b'D') if self.memref_starts_at(1) => {
                self.pos += 1;
                OperandKind::Delta
            }
            Some(b'p' | b'P') if self.memref_starts_at(1) => {
                self.pos += 1;
                OperandKind::Prior
            }
            Some(b'0') if self.starts_memref() => OperandKind::Mem,
            Some(b'h' | b'H') => {
                self.pos += 1;
                return self.parse_hex_value(start);
            }
            Some(c) if c.is_ascii_digit() => {
                return Ok(Operand {
                    kind: OperandKind::Value,
                    memref: 0,
                    value: self.parse_decimal()?,
                });
            }
            _ => {
                return Err(self.err(
                    start,
                    "expected a memory address, a delta/prior address or a value",
                ));
            }
        };

        let (address, size) = self.parse_memref()?;
        Ok(Operand {
            kind,
            memref: self.memref(address, size),
            value: 0,
        })
    }

    fn parse_hex_value(&mut self, start: usize) -> Result<Operand, ParseError> {
        let digits_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(self.err(start, "expected hexadecimal digits after `h`"));
        }
        let value = u32::from_str_radix(&self.src[digits_start..self.pos], 16)
            .map_err(|_| self.err(start, "value does not fit in 32 bits"))?;
        Ok(Operand {
            kind: OperandKind::Value,
            memref: 0,
            value,
        })
    }

    fn starts_memref(&self) -> bool {
        self.memref_starts_at(0)
    }

    fn memref_starts_at(&self, offset: usize) -> bool {
        self.peek_at(offset) == Some(b'0') && matches!(self.peek_at(offset + 1), Some(b'x' | b'X'))
    }

    fn parse_memref(&mut self) -> Result<(u32, Size), ParseError> {
        let start = self.pos;
        if !self.starts_memref() {
            return Err(self.err(start, "expected a memory address starting with `0x`"));
        }
        self.pos += 2;

        let size = match self.peek() {
            Some(b'h' | b'H') => Size::Byte,
            Some(b'x' | b'X') => Size::Dword,
            Some(b' ') => Size::Word,
            Some(b'l' | b'L') => Size::Low,
            Some(b'u' | b'U') => Size::High,
            Some(c) if (b'm'..=b't').contains(&c.to_ascii_lowercase()) => {
                Size::Bit(c.to_ascii_lowercase() - b'm')
            }
            _ => {
                return Err(self.err(
                    start,
                    "only the sizes 0xH, 0x (16-bit), 0xX, 0xM-0xT, 0xL and 0xU are supported",
                ));
            }
        };
        self.pos += 1;

        let digits_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(self.err(start, "expected a hexadecimal address after the size"));
        }
        let address = u32::from_str_radix(&self.src[digits_start..self.pos], 16)
            .map_err(|_| self.err(start, "address does not fit in 32 bits"))?;
        Ok((address, size))
    }

    fn memref(&mut self, address: u32, size: Size) -> usize {
        if let Some(index) = self
            .memrefs
            .iter()
            .position(|m| m.address == address && m.size == size)
        {
            return index;
        }
        self.memrefs.push(MemRef {
            address,
            size,
            value: 0,
            prior: 0,
            changed: false,
        });
        self.memrefs.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_ram(pairs: &[(u32, u8)], addr: u32) -> u8 {
        pairs
            .iter()
            .find(|(address, _)| *address == addr)
            .map(|(_, value)| *value)
            .unwrap_or(0)
    }

    fn frame(achievement: &mut Achievement, pairs: &[(u32, u8)]) -> bool {
        achievement.step(|addr| read_ram(pairs, addr))
    }

    #[test]
    fn a_condition_must_see_false_once_before_it_fires() {
        let mut achievement = Achievement::new("0xH0010=1").unwrap();
        for _ in 0..3 {
            assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        }
        assert!(!frame(&mut achievement, &[(0x0010, 0)]));
        assert!(frame(&mut achievement, &[(0x0010, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
    }

    #[test]
    fn hit_targets_accumulate_while_true() {
        let mut achievement = Achievement::new("0xH0010=1.3.").unwrap();
        assert!(!frame(&mut achievement, &[(0x0010, 0)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(frame(&mut achievement, &[(0x0010, 1)]));
    }

    #[test]
    fn a_met_hit_target_keeps_the_condition_true() {
        let mut achievement = Achievement::new("0xH0010=1.2._0xH0011=1").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        // The first condition's comparison is now false, but its hit target is met.
        assert!(frame(&mut achievement, &[(0x0011, 1)]));
    }

    #[test]
    fn reset_if_clears_hits_and_blocks_the_trigger() {
        let mut achievement = Achievement::new("0xH0010=1.2._R:0xH0011=1").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1), (0x0011, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(frame(&mut achievement, &[(0x0010, 1)]));

        // A reset also blocks a trigger that has no hit counts to clear.
        let mut blocked = Achievement::new("0xH0010=1_R:0xH0011=1").unwrap();
        assert!(!frame(&mut blocked, &[]));
        assert!(!frame(&mut blocked, &[(0x0010, 1), (0x0011, 1)]));
        assert!(frame(&mut blocked, &[(0x0010, 1)]));
    }

    #[test]
    fn pause_if_freezes_hits_without_clearing_them() {
        let mut achievement = Achievement::new("0xH0010=1.2._P:0xH0011=1").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1), (0x0011, 1)]));
        assert!(frame(&mut achievement, &[(0x0010, 1)]));
    }

    #[test]
    fn add_source_offsets_the_left_operand() {
        let mut achievement = Achievement::new("A:0xH0010_0xH0011>h0E").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 5), (0x0011, 9)]));
        assert!(frame(&mut achievement, &[(0x0010, 5), (0x0011, 10)]));
    }

    #[test]
    fn sub_source_offsets_the_left_operand() {
        let mut achievement = Achievement::new("B:0xH0010_0xH0011=h05").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 5), (0x0011, 9)]));
        assert!(frame(&mut achievement, &[(0x0010, 5), (0x0011, 10)]));
    }

    #[test]
    fn and_next_gates_the_following_condition() {
        let mut achievement = Achievement::new("N:0xH0010=1_0xH0011=2").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 0), (0x0011, 2)]));
        assert!(frame(&mut achievement, &[(0x0010, 1), (0x0011, 2)]));
    }

    #[test]
    fn alt_groups_are_or_ed_under_a_core_group() {
        let mut achievement = Achievement::new("0xH0010=1_S0xH0011=2_S0xH0012=3").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(!frame(&mut achievement, &[(0x0010, 1), (0x0011, 0)]));
        assert!(!frame(
            &mut achievement,
            &[(0x0010, 0), (0x0011, 0), (0x0012, 3)]
        ));
        assert!(frame(
            &mut achievement,
            &[(0x0010, 1), (0x0011, 0), (0x0012, 3)]
        ));
    }

    #[test]
    fn a_blank_core_group_needs_only_one_alt() {
        let mut achievement = Achievement::new("S0xH0010=1_S0xH0011=2").unwrap();
        assert!(!frame(&mut achievement, &[]));
        assert!(frame(&mut achievement, &[(0x0011, 2)]));
    }

    #[test]
    fn delta_is_the_previous_frames_value() {
        let mut achievement = Achievement::new("d0xH0010=1").unwrap();
        assert!(!frame(&mut achievement, &[(0x0010, 0)]));
        assert!(!frame(&mut achievement, &[(0x0010, 1)]));
        assert!(frame(&mut achievement, &[(0x0010, 1)]));
    }

    #[test]
    fn prior_is_the_last_value_that_changed() {
        let mut achievement = Achievement::new("p0xH0010=h05").unwrap();
        assert!(!frame(&mut achievement, &[(0x0010, 5)]));
        assert!(frame(&mut achievement, &[(0x0010, 6)]));
    }

    #[test]
    fn every_supported_size_reads_its_bytes() {
        let cases: [(&str, &[(u32, u8)]); 6] = [
            ("0xM0010=1", &[(0x0010, 0x01)]),
            ("0xT0010=1", &[(0x0010, 0x80)]),
            ("0xL0010=hF", &[(0x0010, 0x3F)]),
            ("0xU0010=h3", &[(0x0010, 0x3F)]),
            ("0x 0010=h1234", &[(0x0010, 0x34), (0x0011, 0x12)]),
            (
                "0xX0010=h12345678",
                &[
                    (0x0010, 0x78),
                    (0x0011, 0x56),
                    (0x0012, 0x34),
                    (0x0013, 0x12),
                ],
            ),
        ];
        for (memaddr, pairs) in cases {
            let mut achievement = Achievement::new(memaddr).unwrap();
            assert!(
                !achievement.step(|addr| read_ram(&[], addr)),
                "{memaddr} must not fire before it is primed"
            );
            assert!(
                frame(&mut achievement, pairs),
                "{memaddr} did not fire on its value"
            );
        }
    }

    #[test]
    fn parse_errors_name_the_token() {
        let cases = [
            ("0xW1234=1", "0xW1234"),
            ("0xI1234=1", "0xI1234"),
            ("0xK1234=1", "0xK1234"),
            ("M:0xH1234=1", "M:0xH1234"),
            ("O:0xH1234=1", "O:0xH1234"),
            ("A:0xH1234/2_0xH0010=1", "A:0xH1234"),
            ("0xH1234=1(3)", "0xH1234"),
            ("0xH1234", "0xH1234"),
            ("0x1234=1", "0x1234"),
            ("d5=1", "d5"),
            ("0xH1234=1.3", "0xH1234"),
            ("0xH1234=1_", "end of string"),
            ("0xH1234=1_0xH1235", "0xH1235"),
            ("é0xH1234=1", "é"),
            ("", "end of string"),
        ];
        for (memaddr, token) in cases {
            let error = Achievement::new(memaddr).expect_err(&format!("{memaddr} must not parse"));
            assert!(
                error.to_string().contains(token),
                "{memaddr}: `{error}` does not name `{token}`"
            );
        }
    }

    #[test]
    fn a_dangling_flag_or_an_empty_alt_group_is_refused() {
        // rcheevos reads a trailing A:/B:/N: as always-true and accepts an empty condset; both
        // are named errors here, never a silently dropped condition.
        for memaddr in [
            "0xH0010=1_A:0xH1234",
            "0xH0010=1_B:0xH1234",
            "0xH0010=1_N:0xH1235=1",
        ] {
            let error = Achievement::new(memaddr).expect_err(&format!("{memaddr} must not parse"));
            assert!(
                error.to_string().contains("has no condition to modify"),
                "{memaddr}: `{error}`"
            );
        }
        let error = Achievement::new("0xH0010=1S").expect_err("a trailing `S` must not parse");
        assert!(
            error.to_string().contains("alt group"),
            "`{error}` does not name the empty alt group"
        );
    }

    #[test]
    fn game_hash_strips_the_nes_and_fds_headers() {
        let payload = b"\x01\x02\x03\x04 a payload";
        let expected = checksum::hex(&checksum::md5(payload));

        let mut nes = b"NES\x1a".to_vec();
        nes.extend_from_slice(&[0u8; 12]);
        nes.extend_from_slice(payload);
        assert_eq!(game_hash(&nes), expected);

        let mut fds = b"FDS\x1a".to_vec();
        fds.extend_from_slice(&[0u8; 12]);
        fds.extend_from_slice(payload);
        assert_eq!(game_hash(&fds), expected);

        // Headerless files are hashed whole.
        assert_eq!(game_hash(payload), expected);

        // A file that is exactly one header is hashed whole: rcheevos only strips when it has
        // more than 16 bytes.
        let header_only = b"NES\x1a"
            .iter()
            .copied()
            .chain([0u8; 12])
            .collect::<Vec<_>>();
        assert_eq!(
            game_hash(&header_only),
            checksum::hex(&checksum::md5(&header_only))
        );
    }

    #[test]
    fn nes_addresses_mirror_ram_and_stop_at_the_cartridge() {
        assert_eq!(nes_bus_address(0x0000), Some(0x0000));
        assert_eq!(nes_bus_address(0x07FF), Some(0x07FF));
        assert_eq!(nes_bus_address(0x0800), Some(0x0000));
        assert_eq!(nes_bus_address(0x1234), Some(0x0234));
        assert_eq!(nes_bus_address(0x1FFF), Some(0x07FF));
        assert_eq!(nes_bus_address(0x2000), None);
        assert_eq!(nes_bus_address(0x401F), None);
        assert_eq!(nes_bus_address(0x6000), Some(0x6000));
        assert_eq!(nes_bus_address(0xFFFF), Some(0xFFFF));
        assert_eq!(nes_bus_address(0x1_0000), None);
    }
}
