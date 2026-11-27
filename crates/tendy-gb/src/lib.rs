//! tendy-gb: the DMG, CGB and SGB core, behind [`tendy_core::Machine`].
//!
//! The emulation modules are the source core's, copied as they are; [`machine_impl`] is the only
//! new file, and it converts between that core and the workspace's contract. Every constant, every
//! module name and every re-export below matches the source so the moved tests and the machinery
//! that reaches through `as_any_mut` keep working.

/// Visible picture width in pixels.
pub const WIDTH: usize = 160;
/// Visible picture height in pixels.
pub const HEIGHT: usize = 144;
/// One frame of shade indices.
pub const FRAME_PIXELS: usize = WIDTH * HEIGHT;
/// Master clock in Hz.
pub const CLOCK_HZ: u32 = 4_194_304;
/// Dots (T-cycles) in one scanline.
pub const DOTS_PER_LINE: u32 = 456;
/// Lines in one frame, VBlank included.
pub const LINES_PER_FRAME: u32 = 154;
/// Dots in one frame.
pub const DOTS_PER_FRAME: u32 = DOTS_PER_LINE * LINES_PER_FRAME;
/// Machine cycles in one frame.
pub const MCYCLES_PER_FRAME: u32 = DOTS_PER_FRAME / 4;
/// Frame rate: `CLOCK_HZ / DOTS_PER_FRAME`.
pub const FPS: f64 = 59.727_500_569_605_83;
/// First VBlank line.
pub const VBLANK_LINE: u8 = 144;
/// One cartridge ROM bank.
pub const ROM_BANK_SIZE: usize = 0x4000;
/// Work RAM as a DMG cartridge sees it: the 8 KiB at `$C000-$DFFF`, one contiguous run.
pub const WRAM_SIZE: usize = 0x2000;
/// One of the CGB's work RAM banks: 4 KiB (Pan Docs, "FF70 — SVBK/WBK": "32 KiB of internal RAM …
/// divided into 8 banks of 4 KiB each").
pub const WRAM_BANK_SIZE: usize = 0x1000;
/// The CGB's eight work RAM banks, bank 0 first.
pub const WRAM_BANKS: usize = 8;
/// All of work RAM as the bus holds it: the DMG's 8 KiB *are* banks 0 and 1 of this, so a DMG
/// cartridge's `$C000-$DFFF` is the same 8 KiB run it has always been.
pub const WRAM_BYTES: usize = WRAM_BANKS * WRAM_BANK_SIZE;
/// Video RAM.
pub const VRAM_SIZE: usize = 0x2000;
/// Object attribute memory.
pub const OAM_SIZE: usize = 0xA0;
/// High RAM.
pub const HRAM_SIZE: usize = 0x7F;

pub mod apu;
pub mod boot;
pub mod bus;
pub mod cart;
pub mod cgb;
pub mod cheats;
pub mod checksum;
pub mod cpu;
pub mod debugger;
pub mod gba_types;
pub mod joypad;
pub mod link;
pub mod machine;
mod machine_impl;
pub mod palette;
pub mod ppu;
pub mod save;
pub mod serial;
pub mod sgb;
pub mod state;
pub mod system;
pub mod timer;
pub mod vdma;

pub use cart::Cartridge;
pub use joypad::Buttons;
pub use machine::Machine;
pub use machine_impl::{GbMachine, GbModel, GbOptions, native_buttons};
pub use system::{AdvanceRegs, CartIdentity, CpuRegs, CpuState, Frame, System};
