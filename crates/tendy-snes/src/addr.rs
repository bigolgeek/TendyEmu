//! `Addr`: the 24-bit bus address, so no lane passes a bare `u32` and silently loses the bank
//! A bank wrap (`$00:FFFF` → `$00:0000`, not `$01:0000`) is the difference between
//! two frames of a picture and a corrupted state.

/// A 24-bit address on the SNES bus, `0x00_0000..=0xFF_FFFF`.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Addr(pub u32);

impl Addr {
    /// Builds an address from a bank and an offset.
    pub const fn new(bank: u8, offset: u16) -> Addr {
        Addr(((bank as u32) << 16) | offset as u32)
    }

    /// The bank, `$00`-`$FF`.
    pub fn bank(self) -> u8 {
        (self.0 >> 16) as u8
    }

    /// The offset inside the bank.
    pub fn offset(self) -> u16 {
        self.0 as u16
    }

    /// The address as a linear index, for tables.
    pub fn linear(self) -> usize {
        self.0 as usize
    }

    /// The same offset in `bank`.
    pub fn with_bank(self, bank: u8) -> Addr {
        Addr::new(bank, self.offset())
    }

    /// The next byte, wrapping inside the bank.
    pub fn next(self) -> Addr {
        Addr::new(self.bank(), self.offset().wrapping_add(1))
    }
}
