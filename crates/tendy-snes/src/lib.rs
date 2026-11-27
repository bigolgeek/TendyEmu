//! tendy-snes: the Super NES core. Imported from `snes-rust` at `bf3c60b` (DESIGN §5.5).
//!
//! The constants, the machine, the bus, the 65816, the PPU, the APU and the cartridge boards are
//! the source's, byte for byte. The front end and the loader stay in the source until #27 ports
//! them; [`machine_impl::SnesMachine`] is the `tendy-core` wrapper this crate adds (§2.10).

/// Picture width in pixels.
pub const WIDTH: usize = 256;
/// Picture height in pixels (NTSC, no overscan).
pub const HEIGHT: usize = 224;
/// Picture size in pixels: 57 344.
pub const FRAME_PIXELS: usize = WIDTH * HEIGHT;
/// The master clock, 21.47727 MHz.
pub const MASTER_CLOCK_HZ: u32 = 21_477_272;
/// Dots in a line: 1364 master clocks / 4.
pub const DOTS_PER_LINE: u32 = 341;
/// Lines in an NTSC frame.
pub const LINES_PER_FRAME: u32 = 262;
/// Dots in a frame: 89 342.
pub const DOTS_PER_FRAME: u32 = DOTS_PER_LINE * LINES_PER_FRAME;
/// Master clocks in a frame: 262 * 1364 = 357 368.
pub const MASTER_TICKS_PER_FRAME: u32 = 357_368;
/// Frame rate: `MASTER_CLOCK_HZ / MASTER_TICKS_PER_FRAME`, 60.098 475 52.
pub const FPS: f64 = 60.098_475_52;
/// The first visible dot.
pub const VISIBLE_DOT: u32 = 22;
/// Visible lines.
pub const VISIBLE_LINES: u32 = 224;
/// The first V-blank line.
pub const VBLANK_START: u16 = 225;
/// The last line of an NTSC frame.
pub const LAST_LINE: u16 = 261;
/// The DRAM-refresh pause, in master clocks.
pub const REFRESH_TICKS: u32 = 40;
/// A fast-region access, in master clocks.
pub const FAST_ACCESS: u32 = 6;
/// A slow-region access, in master clocks.
pub const SLOW_ACCESS: u32 = 8;
/// A manual-joypad-port access, in master clocks.
pub const JOYPAD_ACCESS: u32 = 12;
/// The APU oscillator.
pub const APU_OSC_HZ: u32 = 24_576_000;
/// The SPC700 clock: 24.576 MHz / 24.
pub const SPC_CLOCK_HZ: u32 = 1_024_000;
/// The DSP sample rate: 24.576 MHz / 24 / 32.
pub const DSP_SAMPLE_HZ: u32 = 32_000;
/// Work RAM, 128 KiB.
pub const WRAM_SIZE: usize = 0x20000;
/// The 8 KiB mirror seen at `$00-$3F`/`$80-$BF`:0000.
pub const WRAM_MIRROR: usize = 0x2000;
/// VRAM, 64 KiB byte-addressed: 32 768 words, so a 15-bit word address spans the whole array.
pub const VRAM_SIZE: usize = 0x10000;
/// OAM: 512 bytes plus the 32-byte high table.
pub const OAM_SIZE: usize = 0x220;
/// CGRAM: 256 BGR555 words.
pub const CGRAM_SIZE: usize = 0x200;
/// APU RAM, 64 KiB.
pub const APU_RAM_SIZE: usize = 0x10000;
/// The audio staging buffer, f32, allocated once per machine.
pub const AUDIO_STAGING: usize = 4096;

pub mod addr;
pub mod bus;
pub mod cpu;
pub mod dma;
pub mod io;
pub mod ppu;
pub mod apu;
pub mod cart;
pub mod joypad;
pub mod state;
pub mod machine;
pub mod system;
pub mod picture;
pub mod checksum;

pub mod machine_impl;

pub use cart::Cartridge;
pub use joypad::Buttons;
pub use machine::Machine;
pub use machine_impl::SnesMachine;
pub use system::{Frame, System};
