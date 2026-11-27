//! The SGB sound path's chip: the shared SPC700 + S-DSP (DESIGN §4).
//!
//! `tendy-gb`'s copy of the chip is gone; what stays here are the paths the sound path and its
//! tests already used. [`Apu::new`] now takes a [`Host`], and this host is `Host::Sgb`: ticks in SPC
//! cycles, RAM length-prefixed in state, `brr` written as a u16.

pub mod dsp;
pub mod echo;
pub mod spc700;
pub mod voice;

pub use tendy_spc::Host;
pub use tendy_spc::apu::*;
pub use tendy_spc::{APU_RAM_SIZE, AUDIO_STAGING, DSP_SAMPLE_HZ, MASTER_CLOCK_HZ, SPC_CLOCK_HZ};
