//! tendy-spc: the SPC700 and S-DSP, shared by the SNES APU and the SGB sound path.

/// Which host drives the SPC, and so which clock units and state layout apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// Super NES: ticks are master clocks; state writes `brr` as u32 and RAM with no length prefix.
    Snes,
    /// Super Game Boy sound: ticks are SPC cycles; state writes `brr` as u16 and RAM with a u32 length prefix.
    Sgb,
}
