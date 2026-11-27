//! The workspace wrapper over the source core: [`GbMachine`] implements [`tendy_core::Machine`].
//!
//! Nothing here emulates. Every method delegates to the source's own API — the [`System`] trait for
//! the running machine, [`Machine`] for the counters and buffers it keeps outside that trait — and
//! the only work done here is the boundary conversion the contract asks for: shade indices or
//! RGB555 words to RGBA8888, the logical pad to the `$FF00` matrix, and the source's state blob into
//! the workspace's refusal type.

use tendy_core::{
    Buttons as Logical, Capabilities, Capability, CartIdentity, Frame, StateRefusal, SystemKind,
};

use crate::machine::{BOOT_ROM_SIZE, CGB_BOOT_ROM_SIZE, Machine};
use crate::palette::{self, PaletteName};
use crate::sgb::{SGB_FRAME_HEIGHT, SGB_FRAME_WIDTH};
use crate::system::{SgbScreen, System};
use crate::{HEIGHT, WIDTH};

/// The Game Boy model to run, as the source's console setting does today (`--cgb`, `--sgb`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GbModel {
    /// A CGB console when `$0143` bit 7 is set, a DMG console otherwise. Never SGB.
    #[default]
    Auto,
    /// A DMG console, whatever the cartridge says.
    Dmg,
    /// A CGB console, whatever the cartridge says.
    Cgb,
    /// A Super Game Boy.
    Sgb,
}

/// What the source's launch path hands the machine today.
#[derive(Clone, Debug, Default)]
pub struct GbOptions {
    /// Model; `Auto` picks DMG or CGB from `$0143` bit 7, never SGB.
    pub model: GbModel,
    /// A user boot ROM: 256 bytes for the DMG page, 2048 for the CGB page.
    pub boot_rom: Option<Vec<u8>>,
    /// The SGB cartridge for SGB sound: 262144 bytes, `Super GAMEBOY` at `$7FC0`.
    pub sgb_sound_rom: Option<Vec<u8>>,
}

/// The channel names [`tendy_core::Machine::set_channel_mask`] controls, bit 0 first.
const CHANNELS: [&str; 4] = ["Pulse 1", "Pulse 2", "Wave", "Noise"];

/// The logical pad's bits, in the source's `$FF00` order.
const PAD: [(Logical, crate::joypad::Buttons); 8] = [
    (Logical::RIGHT, crate::joypad::Buttons::RIGHT),
    (Logical::LEFT, crate::joypad::Buttons::LEFT),
    (Logical::UP, crate::joypad::Buttons::UP),
    (Logical::DOWN, crate::joypad::Buttons::DOWN),
    (Logical::A, crate::joypad::Buttons::A),
    (Logical::B, crate::joypad::Buttons::B),
    (Logical::SELECT, crate::joypad::Buttons::SELECT),
    (Logical::START, crate::joypad::Buttons::START),
];

/// The contract's pad as the source's joypad matrix: the eight buttons both pads share, and nothing
/// for the four the Game Boy lacks.
pub fn native_buttons(buttons: Logical) -> crate::joypad::Buttons {
    let mut pad = crate::joypad::Buttons::NONE;
    for (logical, native) in PAD {
        if buttons.contains(logical) {
            pad |= native;
        }
    }
    pad
}

/// The cartridge type byte of a board with an accelerometer (Pan Docs, "The Cartridge Header").
const MBC7: u8 = 0x22;

/// The source's machine plus the boundary state the contract needs.
pub struct GbMachine {
    core: Machine,
    /// The last frame, converted; `frame()` refills it.
    rgba: Vec<u8>,
    /// The Super Game Boy's 256x224 composition buffer.
    screen: SgbScreen,
    palette: PaletteName,
    /// Whether `frame()` shows the SGB border.
    border: bool,
    /// The rate `drain_audio` produces at.
    sample_rate: u32,
}

impl GbMachine {
    /// Builds the machine. The error text is the source's own refusal text.
    pub fn new(rom: Vec<u8>, options: GbOptions) -> Result<GbMachine, String> {
        if let Some(boot) = options.boot_rom.as_deref()
            && boot.len() != BOOT_ROM_SIZE
            && boot.len() != CGB_BOOT_ROM_SIZE
        {
            return Err(format!(
                "boot ROM is {} bytes; a DMG boot ROM is {BOOT_ROM_SIZE} and a CGB one is \
                 {CGB_BOOT_ROM_SIZE}",
                boot.len()
            ));
        }
        let cart = crate::cart::Cartridge::new(rom)?;
        let sgb = options.model == GbModel::Sgb;
        let console_cgb = !sgb
            && match options.model {
                GbModel::Auto => cart.header.cgb_flag & 0x80 != 0,
                GbModel::Cgb => true,
                GbModel::Dmg | GbModel::Sgb => false,
            };
        let mut core = match (sgb, options.boot_rom) {
            (true, Some(boot)) => Machine::with_boot_rom_sgb(cart, boot),
            (true, None) => Machine::new_sgb(cart),
            (false, Some(boot)) => Machine::with_boot_rom_on(cart, boot, console_cgb),
            (false, None) => Machine::new_on(cart, console_cgb),
        };
        // The chip exists only inside a Super Game Boy; a machine that is not one ignores the call.
        if sgb && let Some(rom) = options.sgb_sound_rom {
            let _ = core.set_sgb_sound(rom);
        }
        Ok(GbMachine {
            core,
            rgba: Vec::new(),
            screen: SgbScreen::new(),
            palette: PaletteName::default(),
            border: false,
            sample_rate: 44_100,
        })
    }

    /// The source machine, for the app's Game Boy module (`as_any` gets here too).
    pub fn system(&self) -> &dyn System {
        &self.core
    }

    /// The source machine, mutably, for features `tendy_core::Machine` does not carry.
    pub fn system_mut(&mut self) -> &mut dyn System {
        &mut self.core
    }

    /// The palette the DMG faces are expanded through.
    pub fn set_palette(&mut self, name: PaletteName) {
        self.palette = name;
    }

    /// True when the machine is a Super Game Boy console.
    fn is_sgb(&self) -> bool {
        self.core.bus.ppu.sgb.enabled()
    }
}

impl tendy_core::Machine for GbMachine {
    fn kind(&self) -> SystemKind {
        SystemKind::Gb
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
        let mut caps = Capabilities::NONE
            .with(Capability::ChannelMask)
            .with(Capability::Cheats)
            .with(Capability::BootRom)
            .with(Capability::Palettes)
            .with(Capability::LcdFilter)
            .with(Capability::Link)
            .with(Capability::Debugger)
            .with(Capability::Trace);
        if self.is_sgb() {
            caps = caps.with(Capability::SgbBorder);
            if self.core.sgb_rom.is_some() {
                caps = caps.with(Capability::SgbSound);
            }
        }
        if self.core.bus.cart.header.cart_type == MBC7 {
            caps = caps.with(Capability::Tilt);
        }
        caps
    }

    fn run_frame(&mut self) -> u32 {
        self.core.run_frame()
    }

    fn frames(&self) -> u64 {
        self.core.frames
    }

    fn frame_rate(&self) -> f64 {
        f64::from(crate::CLOCK_HZ) / f64::from(crate::DOTS_PER_FRAME)
    }

    fn reset(&mut self) {
        self.core.reset();
    }

    fn frame(&mut self) -> Frame<'_> {
        self.rgba.clear();
        if self.border && self.is_sgb() {
            if let Some(words) = self.screen.compose(&self.core) {
                for &word in words {
                    self.rgba.extend_from_slice(&palette::rgb555(word));
                }
            }
            return Frame::new(SGB_FRAME_WIDTH as u32, SGB_FRAME_HEIGHT as u32, &self.rgba);
        }
        // The source decides when the picture is colour words rather than shades: a CGB, or an SGB
        // whose scanline renderer resolved every pixel through the console's own palettes.
        if System::frame(&self.core).in_colour() {
            for &word in self.core.colours() {
                self.rgba.extend_from_slice(&palette::rgb555(word));
            }
        } else {
            let table = palette::table(self.palette);
            for &shade in self.core.frame() {
                self.rgba.extend_from_slice(&table[usize::from(shade & 3)]);
            }
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
        self.core.set_buttons(native_buttons(buttons));
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        crate::save::encode_save(&self.core)
    }

    fn load_battery_body(&mut self, bytes: &[u8], now_ms: u64) -> Result<bool, String> {
        crate::save::load_save(&mut self.core, bytes, now_ms)
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.core.encode_state()
    }

    fn load_state(&mut self, blob: &[u8]) -> Result<(), StateRefusal> {
        self.core
            .decode_state(blob)
            .map_err(|why| StateRefusal::Core(format!("{why:?}")))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn state_refusal(&self, blob: &[u8]) -> Option<String> {
        // The refusal text is whatever `load_state` would say, so probe a copy: a refusal leaves the
        // running machine untouched, so the copy is exactly what the caller would get.
        let mut probe = self.core.clone();
        probe.decode_state(blob).err().map(|why| format!("{why:?}"))
    }

    fn tick_rtc(&mut self, now_ms: u64) {
        self.core.tick_rtc(now_ms);
    }

    fn set_sgb_border(&mut self, on: bool) {
        self.border = on;
    }

    fn set_tilt(&mut self, x: i8, y: i8) {
        self.core.set_tilt(x, y);
    }
}
