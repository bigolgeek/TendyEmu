//! The SPC700 and the DSP, shared (issue #6, DESIGN §4).
//!
//! The chip lives in [`tendy_spc`]; these are the paths the SNES core and its tests have always
//! used, so the move did not rename anything above this line. The only visible change is that the
//! parts which differ between hosts are now arguments: [`Apu::new`] takes a [`Host`] (the SNES's
//! master clocks, its raw-RAM and u32-`brr` state), and [`Apu::save_state`] takes the host's
//! writer.

pub mod dsp;
pub mod echo;
pub mod spc700;
pub mod voice;

pub use tendy_spc::Host;
pub use tendy_spc::apu::*;
