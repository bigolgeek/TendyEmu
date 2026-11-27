//! Hand-written little-endian codec for save states (no external crates).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    RomMismatch,
    LengthMismatch { expected: usize, found: usize },
    BadValue,
}

pub const STATE_MAGIC: [u8; 4] = *b"NESS";
/// Layout version. Every layout change bumps this, so an old state fails with `BadVersion` before
/// anything is overwritten (spec `docs/spec-console-quirks.md` §4).
///
/// 1: the original layout.
/// 2: `Ppu.warmup` appended.
/// 3: `Ppu.oam_stamp` appended.
/// 4: `Ppu.glitch_2006` appended.
/// 5: the Zapper device block and the Vaus (Arkanoid) paddle block appended to
/// the bus state, paddles first.
/// 6: the Four Score block (presence byte, pads 3/4, read counters) appended after the
/// Zapper block.
/// 7: the expansion-port block (kind byte, `u16` payload length, payload) appended after
/// the Four Score block.
/// 8: `Ppu.vram` grew from 2 KB to 4 KB for four-screen boards.
/// 9: dead APU state removed: `Apu.frame_irq_new`, the Namco 163's two `u64` write-timestamp
/// words and the VRC7's 64-byte register copy no longer reach the file.
/// 10: expansion-chip save states stop carrying derived values: the VRC7's patch-derived
/// operator parameters and the VRC6/N163/5B cached `level`.
/// 11: the Power Pad block (per port a presence byte, the mat's buttons, latched buttons, read
/// count and strobe) inserted between the Four Score block and the expansion block.
/// 12: MMC1 SOROM/SXROM PRG-RAM grew from 8 KiB to 16/32 KiB, paged from the CHR register,
/// so the cartridge block's length differs.
pub const STATE_VERSION: u16 = 12;

/// 64-bit FNV-1a, used to tie a state to its ROM.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Default)]
pub struct StateWriter {
    buf: Vec<u8>,
}

impl StateWriter {
    pub fn new() -> Self {
        Self::default()
    }
    /// A writer that holds `capacity` bytes before its buffer has to grow.
    pub fn with_capacity(capacity: usize) -> Self {
        StateWriter {
            buf: Vec::with_capacity(capacity),
        }
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v)
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v))
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes())
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes())
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes())
    }
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits())
    }
    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits())
    }
    pub fn usize(&mut self, v: usize) {
        self.u64(v as u64)
    }
    /// Length (u32) then the bytes.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v)
    }
    pub fn opt_u8(&mut self, v: Option<u8>) {
        match v {
            Some(x) => {
                self.u8(1);
                self.u8(x)
            }
            None => self.u8(0),
        }
    }
    pub fn opt_bool(&mut self, v: Option<bool>) {
        match v {
            Some(x) => {
                self.u8(1);
                self.bool(x)
            }
            None => self.u8(0),
        }
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        StateReader { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], StateError> {
        if self.data.len() - self.pos < n {
            return Err(StateError::Truncated);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
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
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, StateError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, StateError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32, StateError> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn f64(&mut self) -> Result<f64, StateError> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub fn usize(&mut self) -> Result<usize, StateError> {
        Ok(self.u64()? as usize)
    }
    /// Reads a length-prefixed block that must be exactly `dst.len()` long.
    pub fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), StateError> {
        let n = self.u32()? as usize;
        if n != dst.len() {
            return Err(StateError::LengthMismatch {
                expected: dst.len(),
                found: n,
            });
        }
        dst.copy_from_slice(self.take(n)?);
        Ok(())
    }
    pub fn bytes_vec(&mut self) -> Result<Vec<u8>, StateError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub fn opt_u8(&mut self) -> Result<Option<u8>, StateError> {
        if self.bool()? {
            Ok(Some(self.u8()?))
        } else {
            Ok(None)
        }
    }
    pub fn opt_bool(&mut self) -> Result<Option<bool>, StateError> {
        if self.bool()? {
            Ok(Some(self.bool()?))
        } else {
            Ok(None)
        }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_types() {
        let mut w = StateWriter::new();
        w.u8(0xAB);
        w.bool(true);
        w.bool(false);
        w.u16(0xBEEF);
        w.u32(0xDEAD_BEEF);
        w.u64(0x0123_4567_89AB_CDEF);
        w.f32(1.5);
        w.f64(-2.25);
        w.usize(123_456);
        w.bytes(&[1, 2, 3, 4]);
        w.bytes(&[9, 8]);
        w.opt_u8(Some(7));
        w.opt_u8(None);
        w.opt_bool(Some(false));
        w.opt_bool(None);
        let n = w.len();
        assert!(!w.is_empty());
        let data = w.finish();
        assert_eq!(data.len(), n);

        let mut r = StateReader::new(&data);
        assert_eq!(r.u8(), Ok(0xAB));
        assert_eq!(r.bool(), Ok(true));
        assert_eq!(r.bool(), Ok(false));
        assert_eq!(r.u16(), Ok(0xBEEF));
        assert_eq!(r.u32(), Ok(0xDEAD_BEEF));
        assert_eq!(r.u64(), Ok(0x0123_4567_89AB_CDEF));
        assert_eq!(r.f32(), Ok(1.5));
        assert_eq!(r.f64(), Ok(-2.25));
        assert_eq!(r.usize(), Ok(123_456));
        let mut b = [0u8; 4];
        r.bytes_into(&mut b).unwrap();
        assert_eq!(b, [1, 2, 3, 4]);
        assert_eq!(r.bytes_vec(), Ok(vec![9, 8]));
        assert_eq!(r.opt_u8(), Ok(Some(7)));
        assert_eq!(r.opt_u8(), Ok(None));
        assert_eq!(r.opt_bool(), Ok(Some(false)));
        assert_eq!(r.opt_bool(), Ok(None));
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.u8(), Err(StateError::Truncated));
    }

    #[test]
    fn u16_is_little_endian() {
        let mut w = StateWriter::new();
        w.u16(0x1234);
        assert_eq!(w.finish(), vec![0x34, 0x12]);
    }

    #[test]
    fn truncated_u32() {
        let mut r = StateReader::new(&[1, 2, 3]);
        assert_eq!(r.u32(), Err(StateError::Truncated));
    }

    #[test]
    fn bad_bool() {
        let mut r = StateReader::new(&[2]);
        assert_eq!(r.bool(), Err(StateError::BadValue));
    }

    #[test]
    fn bytes_into_length_mismatch() {
        let mut w = StateWriter::new();
        w.bytes(&[1, 2, 3]);
        let data = w.finish();
        let mut r = StateReader::new(&data);
        let mut dst = [0u8; 4];
        assert_eq!(
            r.bytes_into(&mut dst),
            Err(StateError::LengthMismatch {
                expected: 4,
                found: 3
            })
        );
    }

    #[test]
    fn fnv1a64_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
