//! Save-state wire format: magic, version, fields in declaration order, then a checksum.
//!
//! The writer and reader are real; the `save_state`/`load_state` bodies of the devices are scaffold
//! stubs and grow with their lanes.

/// Why a state could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadChecksum,
    BadValue,
}

/// First four bytes of every state.
pub const STATE_MAGIC: [u8; 4] = *b"GBST";
/// Wire version of the payload. Version 2 added the CGB's mode and speed registers;
/// version 3 added the CGB's second VRAM bank, its palette RAM and a frame drawn in colour
///; version 4 added the VRAM DMA unit's registers and its block in flight;
/// version 5 grew work RAM from the DMG's 8 KiB to the CGB's 32 KiB, all eight banks and the
/// `SVBK` value that says which one is mapped; version 6 added the Super Game Boy's 45
/// attribute files and the live 20×18 map, 4 140 bytes; version 7 added the SGB's four
/// visible palettes, its 512 system palettes and its `PAL_PRI` byte under an `enabled` flag,
/// 4 130 bytes; version 8 added the SGB's `MASK_EN` state, a tag byte plus the word a
/// blanked window fills with, 3 bytes; version 9 added the SGB border's 256 tiles, its
/// 32×28 map and its three palettes, 10 089 bytes; version 10 added the SGB sound chip —
/// its presence, 64 KiB of APU RAM, the SPC700, the DSP and the clock remainder — when there is one
///. An older state is refused rather than misread, and the front end says so.
pub const STATE_VERSION: u16 = 10;

/// FNV-1a, 64 bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_continue(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continue an FNV-1a 64 hash over more bytes, so a frame can be folded into a running checksum
/// without hashing everything again.
pub fn fnv1a64_continue(hash: u64, bytes: &[u8]) -> u64 {
    let mut h = hash;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// FNV-1a 64 over a frame's colour words, both bytes of each word little-endian — the fold a CGB
/// frame is digested with, so the picture's colours are in the checksum and not only its indices.
/// (`fnv1a64` over a DMG frame's shade bytes is the DMG fold, and is unchanged.)
pub fn fnv1a64_colours(words: &[u16]) -> u64 {
    fnv1a64_colours_continue(0xcbf2_9ce4_8422_2325, words)
}

/// [`fnv1a64_colours`], continued over another frame.
pub fn fnv1a64_colours_continue(hash: u64, words: &[u16]) -> u64 {
    let mut h = hash;
    for &w in words {
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// Little-endian field appender.
#[derive(Clone, Debug, Default)]
pub struct StateWriter {
    data: Vec<u8>,
}

#[allow(clippy::len_without_is_empty)]
impl StateWriter {
    pub fn new() -> Self {
        StateWriter { data: Vec::new() }
    }

    pub fn u8(&mut self, v: u8) {
        self.data.push(v);
    }

    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    pub fn u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.data.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    pub fn usize(&mut self, v: usize) {
        self.u64(v as u64);
    }

    /// Length-prefixed byte run.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.data.extend_from_slice(v);
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

/// Little-endian field reader over a borrowed slice.
#[derive(Clone, Debug)]
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        StateReader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], StateError> {
        let end = self.pos.checked_add(n).ok_or(StateError::Truncated)?;
        if end > self.data.len() {
            return Err(StateError::Truncated);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, StateError> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, StateError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StateError::BadValue),
        }
    }

    pub fn u16(&mut self) -> Result<u16, StateError> {
        let b: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u16::from_le_bytes(b))
    }

    pub fn u32(&mut self) -> Result<u32, StateError> {
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn u64(&mut self) -> Result<u64, StateError> {
        let b: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u64::from_le_bytes(b))
    }

    pub fn i32(&mut self) -> Result<i32, StateError> {
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(i32::from_le_bytes(b))
    }

    pub fn f64(&mut self) -> Result<f64, StateError> {
        Ok(f64::from_bits(self.u64()?))
    }

    pub fn usize(&mut self) -> Result<usize, StateError> {
        usize::try_from(self.u64()?).map_err(|_| StateError::BadValue)
    }

    /// Reads a length-prefixed run and copies it into a buffer of exactly that length.
    pub fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), StateError> {
        let len = self.u32()? as usize;
        if len != dst.len() {
            return Err(StateError::BadValue);
        }
        dst.copy_from_slice(self.take(len)?);
        Ok(())
    }

    /// Reads a length-prefixed run as an owned buffer. The complement of [`StateWriter::bytes`]
    /// for a field whose length is not fixed, such as the boot ROM (256 or 2048 bytes).
    pub fn bytes(&mut self) -> Result<Vec<u8>, StateError> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    /// Bytes left unread.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}

/// §4.2's shared state functions reach the SGB writer through this: the same bytes the sound path
/// wrote before the chip moved to `tendy-spc` — RAM length-prefixed, `brr` two bytes wide.
impl tendy_spc::StateWrite for StateWriter {
    fn u8(&mut self, v: u8) {
        self.data.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.data.extend_from_slice(&(v.len() as u32).to_le_bytes());
        self.data.extend_from_slice(v);
    }
}

/// The read half of the same contract: same widths, same order, `StateError` on a short body.
impl tendy_spc::StateRead for StateReader<'_> {
    type Error = StateError;

    fn u8(&mut self) -> Result<u8, StateError> {
        let b = self.take(1)?;
        Ok(b[0])
    }

    fn u16(&mut self) -> Result<u16, StateError> {
        let b: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Result<u32, StateError> {
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64, StateError> {
        let b: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| StateError::Truncated)?;
        Ok(u64::from_le_bytes(b))
    }

    fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), StateError> {
        let len = u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| StateError::Truncated)?,
        ) as usize;
        if len != dst.len() {
            return Err(StateError::BadValue);
        }
        dst.copy_from_slice(self.take(len)?);
        Ok(())
    }
}
