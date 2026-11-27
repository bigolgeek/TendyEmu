//! Code/data log: one flag byte per PRG-ROM byte saying how the CPU used it, in the FCEUX
//! `.cdl` layout so FCEUX and Mesen tooling can open the file.
//!
//! The file is the flag bytes and nothing else: its length equals the PRG-ROM size and byte `i`
//! describes PRG-ROM byte `i`. A byte the program never touched is `0`.
//!
//! This module is the table and the encoder. The marking happens in `SysBus` (`Bus::note_code`
//! for code, the read path for data); a board without `prg_rom_offset` maps nothing, so its log
//! stays all zero.

use std::io;
use std::path::Path;

/// Bit 0: the byte was fetched as an opcode or an operand.
pub const CODE: u8 = 0x01;
/// Bit 1: the byte was read as data (not by an instruction fetch, not by a dummy read).
pub const DATA: u8 = 0x02;
/// Bit 2: the byte was read as DMC sample data. Defined for the file layout; nothing sets it yet.
pub const PCM: u8 = 0x04;
/// Bit 6: code reached through an indirect jump or pointer. Defined for the layout; the hook
/// does not set it yet.
pub const INDIRECT_CODE: u8 = 0x40;
/// Bit 7: data read through an indirect pointer. Defined for the layout; the hook does not set it yet.
pub const INDIRECT_DATA: u8 = 0x80;

/// The flag bytes, indexed by PRG-ROM offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cdl {
    flags: Vec<u8>,
}

impl Cdl {
    /// An all-zero log for a PRG-ROM of `prg_len` bytes.
    pub fn new(prg_len: usize) -> Cdl {
        Cdl {
            flags: vec![0; prg_len],
        }
    }

    /// PRG-ROM size in bytes, which is also the encoded file's length.
    pub fn len(&self) -> usize {
        self.flags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.flags.is_empty()
    }

    /// OR `bits` into the flag at PRG-ROM `offset`. An offset past the end is ignored: a board
    /// that maps beyond the image the log was sized for must not panic the emulation.
    #[inline]
    pub fn mark(&mut self, offset: usize, bits: u8) {
        if let Some(f) = self.flags.get_mut(offset) {
            *f |= bits;
        }
    }

    /// The flags at `offset`, `0` past the end.
    pub fn get(&self, offset: usize) -> u8 {
        self.flags.get(offset).copied().unwrap_or(0)
    }

    /// The FCEUX encoding: the flag bytes as they are.
    pub fn as_bytes(&self) -> &[u8] {
        &self.flags
    }

    /// Write the encoded log to `path`.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, &self.flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_or_together_and_ignore_out_of_range() {
        let mut c = Cdl::new(4);
        c.mark(1, CODE);
        c.mark(1, DATA);
        c.mark(9, CODE);
        assert_eq!(c.as_bytes(), &[0, 3, 0, 0]);
        assert_eq!(c.get(9), 0);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn bit_layout_is_fceux() {
        assert_eq!(
            [CODE, DATA, PCM, INDIRECT_CODE, INDIRECT_DATA],
            [0x01, 0x02, 0x04, 0x40, 0x80]
        );
    }
}
