//! NES emulator core. No window or audio-device types live here, so all of it is unit-testable.
//! Imported from `nes-rust` at `efc1e3c`; see `docs/DESIGN.md` §5.4.

/// Visible picture width in pixels.
pub const SCREEN_WIDTH: usize = 256;
/// Visible picture height in pixels.
pub const SCREEN_HEIGHT: usize = 240;
/// NTSC CPU clock in Hz (21.477272 MHz / 12).
pub const CPU_HZ: f64 = 1_789_773.0;
/// PAL (2A07) CPU clock in Hz (26.601712 MHz / 16). The PAL PPU runs at master/5, so one CPU cycle is
/// 3.2 PPU dots instead of 3.
pub const CPU_HZ_PAL: f64 = 1_662_607.0;
/// Dendy CPU clock in Hz (26.601712 MHz / 15). Its PPU is PAL-speed, so one CPU cycle stays 3 dots.
pub const CPU_HZ_DENDY: f64 = 1_773_448.0;

pub mod cart;
pub mod gamedb;
pub mod mapper;
pub mod cpu;
pub mod bus;
pub mod cdl;
pub mod rng;
pub mod cheats;
pub mod watch;

pub mod ra;
pub mod ppu;
pub mod console;
pub mod nes;
pub mod nsf;
pub mod controller;
pub mod expansion;
pub mod apu;
pub mod trace;
pub mod profiler;
pub mod events;
pub mod genie;
pub mod cheatdb;
pub mod debugger;
pub mod save;
pub mod unif;
pub mod state;
pub mod rewind;
pub mod tasedit;
pub mod history;
pub mod runahead;
pub mod checksum;
pub mod movie;
pub mod ntsc;
pub mod symbols;
pub mod hdpack;

pub mod machine_impl;

pub use machine_impl::{NesImage, NesMachine};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_size() {
        assert_eq!(SCREEN_WIDTH * SCREEN_HEIGHT, 61_440);
    }
}
