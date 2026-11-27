//! Save-state wire types: the magic and version every state file starts with, the
//! FNV-1a folds `Machine` uses for its frame checksum, and a byte writer/reader pair.
//!
//! The field-by-field encode/decode of §9.2 belongs to each device; this file owns the wire
//! primitives and nothing else.

/// Why a state file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// The body ended before a field did.
    Truncated,
    /// The first four bytes are not `STATE_MAGIC`.
    BadMagic,
    /// A version this build does not know; refused rather than misread.
    BadVersion(u16),
    /// The trailing checksum does not match the body.
    BadChecksum,
    /// A field holds a value this build cannot accept (a different cartridge, for instance).
    BadValue,
}

/// The four bytes every state file starts with.
pub const STATE_MAGIC: [u8; 4] = *b"SNST";
/// The state layout version. Any field change bumps it.
pub const STATE_VERSION: u16 = 3;

/// FNV-1a, 64-bit, from the offset basis.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_continue(FNV_OFFSET, bytes)
}

/// FNV-1a, 64-bit, continuing from `hash`.
pub fn fnv1a64_continue(hash: u64, bytes: &[u8]) -> u64 {
    let mut h = hash;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// FNV-1a over `words` as little-endian bytes: one frame's checksum.
pub fn fnv1a64_slice(words: &[u16]) -> u64 {
    let mut h = FNV_OFFSET;
    for &w in words {
        for b in w.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A byte writer; `usize`, `u16` and the rest are appended in the order the design's field table
/// lists them.
#[derive(Default)]
pub struct StateWriter {
    data: Vec<u8>,
}

impl StateWriter {
    /// An empty writer.
    pub fn new() -> Self {
        StateWriter::default()
    }

    /// One byte.
    pub fn u8(&mut self, v: u8) {
        self.data.push(v);
    }

    /// One byte, `1` for true.
    pub fn bool(&mut self, v: bool) {
        self.data.push(u8::from(v));
    }

    /// Little-endian, two bytes.
    pub fn u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Little-endian, four bytes.
    pub fn u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Little-endian, eight bytes.
    pub fn u64(&mut self, v: u64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Little-endian, two bytes.
    pub fn i16(&mut self, v: i16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Little-endian, four bytes.
    pub fn i32(&mut self, v: i32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// Little-endian, eight bytes.
    pub fn f64(&mut self, v: f64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    /// A `usize` as eight little-endian bytes, so a 32-bit host cannot shorten a state file.
    pub fn usize(&mut self, v: usize) {
        self.u64(v as u64);
    }

    /// A byte string, no length prefix: the caller writes the length first where one is needed.
    pub fn bytes(&mut self, v: &[u8]) {
        self.data.extend_from_slice(v);
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Nothing written yet.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The finished buffer.
    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

/// The reader that mirrors [`StateWriter`].
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    /// A reader over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        StateReader { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], StateError> {
        let end = self.pos.checked_add(len).ok_or(StateError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(StateError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// One byte.
    pub fn u8(&mut self) -> Result<u8, StateError> {
        Ok(self.take(1)?[0])
    }

    /// One byte, `1` for true.
    pub fn bool(&mut self) -> Result<bool, StateError> {
        Ok(self.u8()? != 0)
    }

    /// Little-endian, two bytes.
    pub fn u16(&mut self) -> Result<u16, StateError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Little-endian, four bytes.
    pub fn u32(&mut self) -> Result<u32, StateError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Little-endian, eight bytes.
    pub fn u64(&mut self) -> Result<u64, StateError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Little-endian, two bytes.
    pub fn i16(&mut self) -> Result<i16, StateError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    /// Little-endian, four bytes.
    pub fn i32(&mut self) -> Result<i32, StateError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Little-endian, eight bytes.
    pub fn f64(&mut self) -> Result<f64, StateError> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// A `usize` from the eight bytes [`StateWriter::usize`] wrote.
    pub fn usize(&mut self) -> Result<usize, StateError> {
        Ok(self.u64()? as usize)
    }

    /// Fills `dst`, or `Truncated` if the body ends first.
    pub fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), StateError> {
        let len = dst.len();
        dst.copy_from_slice(self.take(len)?);
        Ok(())
    }

    /// Bytes left after the read position.
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

/// §4.2's shared state functions reach the SNES writer through this: the same bytes the APU wrote
/// before it moved to `tendy-spc` — RAM raw (this `bytes` adds no length), `brr` four bytes wide.
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
        self.data.extend_from_slice(v);
    }
}

/// The read half of the same contract: same widths, same order, `StateError` on a short body.
impl tendy_spc::StateRead for StateReader<'_> {
    type Error = StateError;

    fn u8(&mut self) -> Result<u8, StateError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, StateError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, StateError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, StateError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), StateError> {
        let len = dst.len();
        dst.copy_from_slice(self.take(len)?);
        Ok(())
    }
}
