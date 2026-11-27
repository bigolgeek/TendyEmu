//! `System`: the seventeen methods the front end is allowed to ask the machine, plus the frame and
//! the cartridge identity it hands back.
//!
//! `impl System for Machine` is delegation, one line per method, and no lane edits it.

use crate::cart::Board;
use crate::joypad::Buttons;
use crate::machine::Machine;
use crate::state::StateError;

/// One frame as the front end consumes it: 256 x 224 BGR555 words, top-left first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u16],
}

impl<'a> Frame<'a> {
    pub fn new(width: usize, height: usize, pixels: &'a [u16]) -> Frame<'a> {
        Frame {
            width,
            height,
            pixels,
        }
    }

    /// Pixels in the frame.
    pub fn len(&self) -> usize {
        self.width.saturating_mul(self.height)
    }

    /// No pixels at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0 || self.pixels.is_empty()
    }
}

impl std::ops::Deref for Frame<'_> {
    type Target = [u16];

    fn deref(&self) -> &[u16] {
        self.pixels
    }
}

/// What a cartridge says about itself: the two facts that key a `.srm` and a state file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CartIdentity {
    /// `Header::title`.
    pub title: String,
    /// `Cartridge::rom_crc32`.
    pub crc32: u32,
}

/// What the front end is allowed to ask the machine. Seventeen methods, no link, no second pad.
pub trait System {
    fn run_frame(&mut self) -> u32;
    fn frame(&self) -> Frame<'_>;
    fn drain_audio(&mut self, out: &mut [f32]) -> usize;
    fn set_buttons(&mut self, buttons: Buttons);
    fn frames(&self) -> u64;
    fn frame_checksum(&self) -> u64;
    /// "Super Nintendo (NTSC)".
    fn console_name(&self) -> &'static str;
    fn cart_identity(&self) -> CartIdentity;
    /// "LoROM", "HiROM + SRAM", "ExHiROM".
    fn cart_storage(&self) -> &'static str;
    fn save_state(&mut self) -> Vec<u8>;
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError>;
    fn reset(&mut self);
    fn set_sample_rate(&mut self, hz: u32);
    fn set_channel_mask(&mut self, mask: u8);
    fn battery_body(&self) -> Option<Vec<u8>>;
    fn load_battery_body(&mut self, bytes: &[u8]) -> Result<bool, String>;
    fn sample_rate(&self) -> u32;
}

impl System for Machine {
    fn run_frame(&mut self) -> u32 {
        Machine::run_frame(self)
    }

    fn frame(&self) -> Frame<'_> {
        Frame::new(crate::WIDTH, crate::HEIGHT, Machine::frame(self))
    }

    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        Machine::drain_audio(self, out)
    }

    fn set_buttons(&mut self, buttons: Buttons) {
        Machine::set_buttons(self, buttons)
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn frame_checksum(&self) -> u64 {
        Machine::frame_checksum(self)
    }

    fn console_name(&self) -> &'static str {
        "Super Nintendo (NTSC)"
    }

    fn cart_identity(&self) -> CartIdentity {
        CartIdentity {
            title: self.bus.cart.header.title.clone(),
            crc32: self.bus.cart.rom_crc32,
        }
    }

    fn cart_storage(&self) -> &'static str {
        let has_ram = !self.bus.cart.ram.is_empty();
        match (self.bus.cart.board, has_ram) {
            (Board::LoRom, _) => "LoROM",
            (Board::HiRom, false) => "HiROM",
            (Board::HiRom, true) => "HiROM + SRAM",
            (Board::ExHiRom, false) => "ExHiROM",
            (Board::ExHiRom, true) => "ExHiROM + SRAM",
        }
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.encode_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.decode_state(bytes)
    }

    fn reset(&mut self) {
        Machine::reset(self)
    }

    fn set_sample_rate(&mut self, hz: u32) {
        Machine::set_sample_rate(self, hz)
    }

    fn set_channel_mask(&mut self, mask: u8) {
        Machine::set_channel_mask(self, mask)
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        self.battery().map(<[u8]>::to_vec)
    }

    fn load_battery_body(&mut self, bytes: &[u8]) -> Result<bool, String> {
        Machine::load_battery(self, bytes)?;
        Ok(true)
    }

    fn sample_rate(&self) -> u32 {
        self.bus.apu.sample_rate
    }
}
