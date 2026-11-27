//! The write/read pair the shared state functions are generic over (DESIGN §4.2).
//!
//! §9.2's state layout is identical on both hosts except for two things the host owns: how RAM is
//! framed (the SNES writes it raw, the SGB prefixes its length) and how wide `brr` is (u32 against
//! u16). So the shared code never names a concrete writer or reader; it asks for these traits, and
//! each host's own `StateWriter`/`StateReader` implements them against its existing byte layout.
//!
//! The traits hold only the methods both hosts already have — `u8`, `u16`, `u32`, `u64`, `bytes` —
//! and no `i16`. A DSP signed word is written `u16(v as u16)` on both hosts, which is the same two
//! little-endian two's-complement bytes the SNES's `i16` method wrote, so the bytes do not move.
//!
//! Reading is fallible: a truncated blob is the host's error, so each reader names its own through
//! [`StateRead::Error`] and the shared functions return it unchanged.

/// A byte sink with the host's own layout, one method per width the state uses.
pub trait StateWrite {
    /// One byte.
    fn u8(&mut self, v: u8);
    /// Two bytes, little-endian.
    fn u16(&mut self, v: u16);
    /// Four bytes, little-endian.
    fn u32(&mut self, v: u32);
    /// Eight bytes, little-endian.
    fn u64(&mut self, v: u64);
    /// A byte string, framed the way this host frames one.
    fn bytes(&mut self, v: &[u8]);
}

/// A byte source matching [`StateWrite`], with the host's own error type.
pub trait StateRead {
    /// The host's state error, so a truncated blob is reported as that host reports it.
    type Error;

    /// One byte.
    fn u8(&mut self) -> Result<u8, Self::Error>;
    /// Two bytes, little-endian.
    fn u16(&mut self) -> Result<u16, Self::Error>;
    /// Four bytes, little-endian.
    fn u32(&mut self) -> Result<u32, Self::Error>;
    /// Eight bytes, little-endian.
    fn u64(&mut self) -> Result<u64, Self::Error>;
    /// `dst.len()` bytes, from the framing this host wrote.
    fn bytes_into(&mut self, dst: &mut [u8]) -> Result<(), Self::Error>;
}
