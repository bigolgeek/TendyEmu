//! The workspace wrapper over the source's Advance core: [`GbaMachine`] implements
//! [`tendy_core::Machine`].
//!
//! Nothing here emulates. Every method delegates to the source's own API — the
//! [`System`](tendy_gb::System) trait for the running machine — and the only work done here is the
//! boundary conversion the contract asks for: RGB555 words to RGBA8888, the logical pad to the
//! source's eight-button matrix plus its two shoulders, and the source's state error into the
//! workspace's refusal type.

use tendy_core::{
    Buttons as Logical, Capabilities, Capability, CartIdentity, Frame, StateRefusal, SystemKind,
};
use tendy_gb::joypad::{Buttons, Shoulders};
use tendy_gb::palette;
use tendy_gb::system::System;

use crate::cart::Cartridge;
use crate::{Gba, HEIGHT, WIDTH};

/// The logical pad's bits, in the source's eight-button matrix order.
const PAD: [(Logical, Buttons); 8] = [
    (Logical::RIGHT, Buttons::RIGHT),
    (Logical::LEFT, Buttons::LEFT),
    (Logical::UP, Buttons::UP),
    (Logical::DOWN, Buttons::DOWN),
    (Logical::A, Buttons::A),
    (Logical::B, Buttons::B),
    (Logical::SELECT, Buttons::SELECT),
    (Logical::START, Buttons::START),
];

/// The two shoulder buttons, which the source keeps beside the eight-button matrix.
const SHOULDERS: [(Logical, Shoulders); 2] = [
    (Logical::L, Shoulders::LEFT),
    (Logical::R, Shoulders::RIGHT),
];

/// The channel names [`tendy_core::Machine::set_channel_mask`] controls, bit 0 first: the four PSG
/// voices, the same four rows the source's menu offers.
const CHANNELS: [&str; 4] = ["Pulse 1", "Pulse 2", "Wave", "Noise"];

/// The contract's pad as the source's two pad types: the eight matrix buttons and the two
/// shoulders, and nothing for X and Y, which a Game Boy Advance does not have.
pub fn native_buttons(buttons: Logical) -> (Buttons, Shoulders) {
    let mut pad = Buttons::NONE;
    for (logical, native) in PAD {
        if buttons.contains(logical) {
            pad |= native;
        }
    }
    let mut shoulders = Shoulders::NONE;
    for (logical, native) in SHOULDERS {
        if buttons.contains(logical) {
            shoulders |= native;
        }
    }
    (pad, shoulders)
}

/// The source's Advance machine plus the boundary state the contract needs.
pub struct GbaMachine {
    core: Gba,
    /// The last frame, converted; `frame()` refills it.
    rgba: Vec<u8>,
    /// The rate `drain_audio` produces at.
    sample_rate: u32,
}

impl GbaMachine {
    /// Builds the machine. The error text is the source's own refusal text, from
    /// [`Cartridge::new`].
    ///
    /// The source runs a high-level BIOS and has no input for a BIOS image, so `bios` cannot be
    /// installed: the parameter rides the contract's signature and is otherwise unread. A cartridge
    /// that does not parse is refused before the machine is built.
    pub fn new(rom: Vec<u8>, bios: Option<Vec<u8>>) -> Result<GbaMachine, String> {
        let _ = bios;
        let cart = Cartridge::new(rom)?;
        Ok(GbaMachine {
            core: Gba::new(cart),
            rgba: Vec::new(),
            sample_rate: 44_100,
        })
    }

    /// The source machine, for the app's Advance module (`as_any` gets here too).
    pub fn system(&self) -> &dyn System {
        &self.core
    }

    /// The source machine, mutably, for features `tendy_core::Machine` does not carry.
    pub fn system_mut(&mut self) -> &mut dyn System {
        &mut self.core
    }
}

impl tendy_core::Machine for GbaMachine {
    fn kind(&self) -> SystemKind {
        SystemKind::Gba
    }

    fn console_name(&self) -> &'static str {
        self.core.console_name()
    }

    fn cart_identity(&self) -> CartIdentity {
        let id = self.core.cart_identity();
        CartIdentity {
            title: id.title,
            crc32: id.crc32,
        }
    }

    fn cart_storage(&self) -> String {
        self.core.cartridge_storage().to_owned()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE
            .with(Capability::ChannelMask)
            .with(Capability::Cheats)
            .with(Capability::Link)
            .with(Capability::Debugger)
            .with(Capability::Trace)
    }

    fn run_frame(&mut self) -> u32 {
        self.core.run_frame()
    }

    fn frames(&self) -> u64 {
        self.core.frames()
    }

    fn frame_rate(&self) -> f64 {
        crate::fps()
    }

    fn reset(&mut self) {
        self.core.reset();
    }

    fn frame(&mut self) -> Frame<'_> {
        self.rgba.clear();
        for &word in self.core.frame().colours {
            self.rgba.extend_from_slice(&palette::rgb555(word));
        }
        Frame::new(WIDTH as u32, HEIGHT as u32, &self.rgba)
    }

    fn frame_checksum(&self) -> u64 {
        self.core.frame_checksum()
    }

    fn set_sample_rate(&mut self, hz: u32) {
        self.sample_rate = hz;
        self.core.set_sample_rate(hz);
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        self.core.drain_audio(out)
    }

    fn channel_names(&self) -> &'static [&'static str] {
        &CHANNELS
    }

    fn set_channel_mask(&mut self, mask: u8) {
        self.core.set_channel_mask(mask);
    }

    fn set_buttons(&mut self, buttons: Logical) {
        let (pad, shoulders) = native_buttons(buttons);
        self.core.set_buttons(pad);
        self.core.set_shoulders(shoulders);
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        crate::save::encode_battery(&self.core.bus.save, self.core.bus.rtc.as_ref())
    }

    fn load_battery_body(&mut self, bytes: &[u8], _now_ms: u64) -> Result<bool, String> {
        crate::save::load_battery(&mut self.core.bus.save, &mut self.core.bus.rtc, bytes)
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.core.save_state()
    }

    fn load_state(&mut self, blob: &[u8]) -> Result<(), StateRefusal> {
        self.core
            .load_state(blob)
            .map_err(|why| StateRefusal::Core(format!("{why:?}")))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
