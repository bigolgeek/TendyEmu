//! Cartridge expansion audio: the five sound chips the boards in this crate carry.
//!
//! Every chip here is clocked once per CPU cycle by its board's [`crate::cart::Mapper::cpu_clock`]
//! and reports a level through its `output` method, which the board forwards from
//! [`crate::cart::Mapper::expansion_audio`] (the FDS reports a mixer-scale amplitude through
//! `mix_level` and the board converts it, see [`fds`]). The bus hands that level to the APU as
//! `Apu::expansion_pulse`, which `Apu::mix` adds as a term of its own after the 2A03's two nonlinear
//! sums and *before* the DC-blocking/output filter: the chip's level is decoded through the pulse
//! curve on its own, the way a cartridge's own DAC drives into the console's summing node, so
//! expansion audio goes through exactly the same sample path as the five native channels without
//! being compressed by their sum.
//!
//! `output` is already normalised to the 2A03 mixer's scale (a full-strength 2A03 pulse pair is
//! about 0.26 and the whole APU peaks near 0.75), so a board's audio sits at a plausible level
//! against the native channels without a per-game fudge factor. The scales are pinned by unit
//! tests and documented per chip.
//!
//! A chip's timing lives in its own dividers (the N163's channel schedule, the VRC7's internal
//! sample, the 5B's tone counters), advanced once per CPU cycle by its `clock` method; no board has
//! to stamp a write with a cycle count.

pub mod fds;
pub mod n163;
pub mod opll;
pub mod vrc6;
pub mod ym2149;

pub use n163::Namco163Audio;
pub use opll::Opll;
pub use vrc6::Vrc6Audio;
pub use ym2149::Ym2149;
