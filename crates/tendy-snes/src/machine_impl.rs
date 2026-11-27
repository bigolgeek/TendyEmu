//! The `tendy-core` wrapper over the SNES machine (DESIGN §2.10): the one place the front end
//! meets the core, with the frame converted to RGBA8888 and the pad mapped to the source's bits.
//!
//! Nothing here changes the machine: every method delegates, so the inertness pins taken from the
//! source (`bf3c60b`) still hold.

use std::any::Any;

use tendy_core::{
    Buttons, Capabilities, Capability, CartIdentity, Frame, Machine, StateRefusal, SystemKind,
};

use crate::cart::Cartridge;
use crate::machine::Machine as Core;
use crate::picture::{BYTES_PER_PIXEL, bgr555_to_rgba};
use crate::system::System as Source;
use crate::{FPS, FRAME_PIXELS, HEIGHT, WIDTH};

/// The eight DSP voices, as the source's CHANNELS rows name them (`snes src/settings.rs`).
const CHANNEL_NAMES: [&str; 8] = [
    "Voice 1", "Voice 2", "Voice 3", "Voice 4", "Voice 5", "Voice 6", "Voice 7", "Voice 8",
];

/// The logical pad to the source's `joypad::Buttons` bits (DESIGN §2.5).
const BUTTON_MAP: [(Buttons, crate::joypad::Buttons); 12] = [
    (Buttons::UP, crate::joypad::Buttons::UP),
    (Buttons::DOWN, crate::joypad::Buttons::DOWN),
    (Buttons::LEFT, crate::joypad::Buttons::LEFT),
    (Buttons::RIGHT, crate::joypad::Buttons::RIGHT),
    (Buttons::A, crate::joypad::Buttons::A),
    (Buttons::B, crate::joypad::Buttons::B),
    (Buttons::X, crate::joypad::Buttons::X),
    (Buttons::Y, crate::joypad::Buttons::Y),
    (Buttons::L, crate::joypad::Buttons::L),
    (Buttons::R, crate::joypad::Buttons::R),
    (Buttons::START, crate::joypad::Buttons::START),
    (Buttons::SELECT, crate::joypad::Buttons::SELECT),
];

/// The [`Machine`] wrapper over snes's machine (`src/machine.rs`) plus its RGBA buffer.
pub struct SnesMachine {
    /// The source machine, public so the SNES UI module (#27) and the tests reach its own API.
    pub core: Core,
    rgba: Vec<u8>,
}

impl SnesMachine {
    /// Builds the machine; the error is snes `src/cart/refusal.rs` text, starting `refused: `.
    pub fn new(rom: Vec<u8>) -> Result<SnesMachine, String> {
        let cart = Cartridge::new(rom).map_err(|why| why.to_string())?;
        Ok(SnesMachine {
            core: Core::new(cart),
            rgba: vec![0; FRAME_PIXELS * BYTES_PER_PIXEL],
        })
    }
}

impl Machine for SnesMachine {
    fn kind(&self) -> SystemKind {
        SystemKind::Snes
    }

    fn console_name(&self) -> &'static str {
        Source::console_name(&self.core)
    }

    fn cart_identity(&self) -> CartIdentity {
        let identity = Source::cart_identity(&self.core);
        CartIdentity {
            title: identity.title,
            crc32: identity.crc32,
        }
    }

    fn cart_storage(&self) -> String {
        Source::cart_storage(&self.core).to_owned()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE.with(Capability::ChannelMask)
    }

    fn run_frame(&mut self) -> u32 {
        self.core.run_frame()
    }

    fn frames(&self) -> u64 {
        self.core.frames
    }

    fn frame_rate(&self) -> f64 {
        FPS
    }

    fn reset(&mut self) {
        self.core.reset();
    }

    fn frame(&mut self) -> Frame<'_> {
        bgr555_to_rgba(self.core.frame(), &mut self.rgba);
        Frame::new(WIDTH as u32, HEIGHT as u32, &self.rgba)
    }

    fn frame_checksum(&self) -> u64 {
        self.core.frame_checksum()
    }

    fn set_sample_rate(&mut self, hz: u32) {
        self.core.set_sample_rate(hz);
    }

    fn sample_rate(&self) -> u32 {
        Source::sample_rate(&self.core)
    }

    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        self.core.drain_audio(out)
    }

    fn channel_names(&self) -> &'static [&'static str] {
        &CHANNEL_NAMES
    }

    fn set_channel_mask(&mut self, mask: u8) {
        // The source's DSP register is a mute mask (`dsp.rs:239`: bit n set silences voice n);
        // the boundary contract is the opposite (`tendy-core`: bit n set means channel n plays).
        self.core.set_channel_mask(!mask);
    }

    fn set_buttons(&mut self, buttons: Buttons) {
        let mut pad = crate::joypad::Buttons::NONE;
        for (logical, source) in BUTTON_MAP {
            if buttons.contains(logical) {
                pad.0 |= source.0;
            }
        }
        self.core.set_buttons(pad);
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        Source::battery_body(&self.core)
    }

    fn load_battery_body(&mut self, bytes: &[u8], _now_ms: u64) -> Result<bool, String> {
        self.core.load_battery(bytes)?;
        Ok(true)
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.core.encode_state()
    }

    fn load_state(&mut self, blob: &[u8]) -> Result<(), StateRefusal> {
        self.core
            .decode_state(blob)
            .map_err(|why| StateRefusal::Core(format!("{why:?}")))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
