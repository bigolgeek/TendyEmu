//! tendy-spc: the SPC700 and S-DSP, shared by the SNES APU and the SGB sound path.
//!
//! One copy of the chip, two hosts. The code is the SNES core's (DESIGN §4.1: it is the harder
//! user, and the SGB copy declared the same constants locally, which is where the code started).
//! What differs between the hosts is made explicit rather than duplicated:
//!
//! - **Clock.** [`Apu::tick`] takes the host's unit — master clocks on the SNES, SPC cycles on the
//!   SGB — and [`Host`] supplies the credit and the charge, so each host keeps the arithmetic it
//!   had.
//! - **State.** [`Apu::save_state`] and [`Apu::load_state`] are generic over a small write/read
//!   trait pair, which each host's own `StateWriter`/`StateReader` implements, so RAM and `brr`
//!   keep the width the host already wrote.
//!
//! Nothing in here knows what a SNES or a Game Boy is beyond those two facts, which is why
//! `tendy-spc` has no dependencies (DESIGN §1.2).

pub mod apu;
pub mod dsp;
pub mod echo;
pub mod spc700;
pub mod state;
pub mod voice;

pub use apu::{Apu, IPL_ROM, Ipl};
pub use dsp::Dsp;
pub use echo::Echo;
pub use spc700::{EventKind, PortSide, PortWrite, Spc700, SpcBus, SpcEvent};
pub use state::{StateRead, StateWrite};
pub use voice::{EnvState, Voice};

/// The chip's whole address space: 64 KiB of APU RAM.
pub const APU_RAM_SIZE: usize = 0x10000;

/// The SPC700's own clock, 1 024 000 cycles a second.
pub const SPC_CLOCK_HZ: u32 = 1_024_000;

/// The DSP's sample rate: one sample every 32 SPC cycles.
pub const DSP_SAMPLE_HZ: u32 = 32_000;

/// Stereo staging capacity in `f32`: 2 048 frames, 4 096 values.
pub const AUDIO_STAGING: usize = 4096;

/// The SNES master clock. `Host::Snes` counts in these and scales its accumulator bias by one, so
/// the chip's timing model needs the number even though the chip has no opinion about the machine
/// that clocks it.
pub const MASTER_CLOCK_HZ: u32 = 21_477_272;

/// Which host drives the SPC, and so which clock units and state layout apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// Super NES: ticks are master clocks; state writes `brr` as u32 and RAM with no length prefix.
    Snes,
    /// Super Game Boy sound: ticks are SPC cycles; state writes `brr` as u16 and RAM with a u32 length prefix.
    Sgb,
}

impl Host {
    /// The bias [`Apu::tick`]'s ledger is stored with, in the same units the ledger counts: 96 SPC
    /// cycles' worth, comfortably more than the longest step can overrun — a 25-cycle boot-ROM
    /// transfer pass, one pass per byte (`IPL_STORE_CYCLES`) — and more than the twelve-cycle
    /// overrun the longest SPC700 instruction can create.
    ///
    /// The SNES stores the ledger in `1/MASTER_CLOCK_HZ` of a cycle, so the bias carries that
    /// scale; the SGB stores it in whole cycles, so the bias is the plain 96. This is the same
    /// number in both units, which is what lets one loop serve both hosts bit for bit.
    pub const fn acc_bias(self) -> i64 {
        match self {
            Host::Snes => 96 * MASTER_CLOCK_HZ as i64,
            Host::Sgb => 96,
        }
    }

    /// What one `tick` earns the ledger, in its own unit: `MASTER_CLOCK_HZ` of them make an SPC
    /// cycle on the SNES; one SPC cycle is one unit on the SGB.
    pub const fn earn(self) -> i64 {
        match self {
            Host::Snes => SPC_CLOCK_HZ as i64,
            Host::Sgb => 1,
        }
    }

    /// What one SPC cycle costs the ledger: `MASTER_CLOCK_HZ` units on the SNES, one on the SGB.
    pub const fn spend(self) -> i64 {
        match self {
            Host::Snes => MASTER_CLOCK_HZ as i64,
            Host::Sgb => 1,
        }
    }
}
