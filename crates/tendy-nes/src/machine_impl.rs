//! The `Machine` wrapper over the NES core's own `Nes` type (DESIGN §2.2, §2.10).
//!
//! The core's API stays exactly as the source wrote it; this file only adapts it to
//! [`tendy_core::Machine`]. Everything the source's front end reaches through `as_any_mut` keeps
//! working unchanged.

use tendy_core::{
    Buttons, Capabilities, Capability, CartIdentity, Frame, Machine, StateRefusal, SystemKind,
    fnv1a64,
};

use crate::apu::gain::{Channel, ChannelGain};
use crate::cart::{ConsoleType, Region};
use crate::console::ConsoleConfig;
use crate::nes::Nes;
use crate::ppu::palette::{self, RgbaTable};
use crate::{checksum, gamedb, nsf, save};

/// The image a [`NesMachine`] is built from (DESIGN §2.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NesImage {
    /// An iNES / NES 2.0 / UNIF cartridge image.
    Cartridge,
    /// A Famicom Disk System disk image; `qd` selects the Quick Disk container format.
    Disk {
        /// Read the image as a `.qd` file rather than a `.fds` header image.
        qd: bool,
    },
    /// An NSF music file; `song` is the one-based song to start with (0 = the file's own choice).
    Nsf {
        /// One-based song number, or 0 for the file's starting song.
        song: u8,
    },
}

/// A running NES (DESIGN §2.10).
pub struct NesMachine {
    /// The core's own machine, the source API unchanged.
    pub nes: Nes,
    /// The RGBA picture `frame()` fills.
    rgba: Vec<u8>,
    /// Frames run since power-on.
    frames: u64,
    /// The last channel mask, so unmuting can restore each channel's own gain.
    mask: u8,
    /// The per-channel gains to restore when a channel is unmuted.
    gains: ChannelGain,
    /// The display title; `new` cannot see the file name, so the caller sets it.
    title: String,
    /// The palette `frame()` renders through.
    palette: RgbaTable,
    /// CRC-32 of PRG‖CHR (the game database key), or of the whole image for FDS/NSF.
    crc32: u32,
    /// The cartridge board number, for `cart_storage`.
    mapper: u16,
    /// Whether the loaded cartridge is a Vs. System board.
    vs: bool,
    /// Whether the image is an NSF file rather than a cartridge.
    nsf: bool,
}

impl NesMachine {
    /// Build a machine from an image, its kind, an optional FDS BIOS (8192 bytes), and the console
    /// configuration (DESIGN §2.10).
    pub fn new(
        image: Vec<u8>,
        kind: NesImage,
        fds_bios: Option<Vec<u8>>,
        console: ConsoleConfig,
    ) -> Result<NesMachine, String> {
        let mut mapper = 0;
        let mut vs = false;
        let mut nsf = false;
        let (nes, crc32, title) = match kind {
            NesImage::Cartridge => {
                let data = crate::cart::split(&image).map_err(|e| format!("{e:?}"))?;
                mapper = data.header.mapper;
                vs = data.header.console_type == ConsoleType::VsSystem;
                let crc32 = gamedb::rom_crc32(&data.prg, &data.chr);
                let nes = Nes::from_rom_data_console(data, fnv1a64(&image), &console)
                    .map_err(|e| format!("{e:?}"))?;
                (nes, crc32, String::new())
            }
            NesImage::Disk { qd } => {
                let bios = fds_bios
                    .ok_or_else(|| "an FDS BIOS image is needed to load a disk".to_string())?;
                let nes = Nes::from_fds_console(&image, &bios, qd, &console)
                    .map_err(|e| format!("{e:?}"))?;
                (nes, checksum::crc32(&image), String::new())
            }
            NesImage::Nsf { song } => {
                nsf = true;
                let nes = Nes::from_nsf(&image, song, &console).map_err(|e| e.to_string())?;
                let title = nsf::NsfInfo::read(&image).map_or_else(|_| String::new(), |i| i.title);
                (nes, checksum::crc32(&image), title)
            }
        };
        Ok(NesMachine {
            rgba: vec![0; crate::SCREEN_WIDTH * crate::SCREEN_HEIGHT * 4],
            nes,
            frames: 0,
            mask: 0xFF,
            gains: ChannelGain::default(),
            title,
            palette: *palette::builtin_rgba_table(),
            crc32,
            mapper,
            vs,
            nsf,
        })
    }

    /// Sets the display title `cart_identity` reports. `new` takes bytes, not a path, so the file
    /// stem the design names is the caller's to supply.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Sets the palette `frame()` renders through (the built-in table, a `.pal` file, or the Vs.
    /// RGB PPU table).
    pub fn set_palette(&mut self, palette: RgbaTable) {
        self.palette = palette;
    }

    /// Whether the loaded cartridge is a Vs. System board.
    pub fn is_vs_system(&self) -> bool {
        self.vs
    }

    /// Whether an FDS disk image is loaded.
    pub fn is_disk(&self) -> bool {
        self.nes.bus.cart().disk_side().is_some()
    }
}

impl Machine for NesMachine {
    fn kind(&self) -> SystemKind {
        SystemKind::Nes
    }

    fn console_name(&self) -> &'static str {
        match self.nes.region() {
            Region::Ntsc => "NES (NTSC)",
            Region::Pal => "NES (PAL)",
            Region::Dendy => "NES (Dendy)",
        }
    }

    fn cart_identity(&self) -> CartIdentity {
        CartIdentity {
            title: self.title.clone(),
            crc32: self.crc32,
        }
    }

    fn cart_storage(&self) -> String {
        if self.is_disk() {
            return "FDS disk".to_string();
        }
        if self.nsf {
            return "NSF player".to_string();
        }
        let mut storage = format!("mapper {}", self.mapper);
        if self.nes.battery {
            storage.push_str(" + battery");
        }
        storage
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::NONE
            .with(Capability::ChannelMask)
            .with(Capability::Cheats)
            .with(Capability::Palettes)
            .with(Capability::Movies)
            .with(Capability::Tas)
            .with(Capability::RunAhead)
            .with(Capability::HdPacks)
            .with(Capability::CrtNtsc)
            .with(Capability::Shaders)
            .with(Capability::Debugger)
            .with(Capability::Trace)
            .with(Capability::MemoryView)
            .with(Capability::RetroAchievements);
        if self.is_disk() {
            caps = caps.with(Capability::DiskSwap);
        }
        if self.nsf {
            caps = caps.with(Capability::NsfPlayer);
        }
        if self.is_vs_system() {
            caps = caps.with(Capability::VsSystem);
        }
        caps
    }

    fn run_frame(&mut self) -> u32 {
        let before = self.nes.cpu.cycles;
        self.nes.run_frame();
        self.frames += 1;
        (self.nes.cpu.cycles - before) as u32
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn frame_rate(&self) -> f64 {
        // The values `nes-rust`'s app-side `src/timing.rs` paces with (that file is
        // ported later, so the two constants live here until then).
        const NES_FPS: f64 = 60.0988;
        const PAL_FPS: f64 = 50.0070;
        match self.nes.region() {
            Region::Ntsc => NES_FPS,
            Region::Pal | Region::Dendy => PAL_FPS,
        }
    }

    fn reset(&mut self) {
        self.nes.reset();
    }

    fn frame(&mut self) -> Frame<'_> {
        let ppu = &self.nes.bus.ppu;
        palette::to_rgba_pal(
            &ppu.framebuffer,
            &ppu.emphasis,
            &self.palette,
            &mut self.rgba,
        );
        Frame::new(
            crate::SCREEN_WIDTH as u32,
            crate::SCREEN_HEIGHT as u32,
            &self.rgba,
        )
    }

    fn frame_checksum(&self) -> u64 {
        let ppu = &self.nes.bus.ppu;
        let mut bytes = Vec::with_capacity(ppu.framebuffer.len() + ppu.emphasis.len());
        bytes.extend_from_slice(&ppu.framebuffer);
        bytes.extend_from_slice(&ppu.emphasis);
        fnv1a64(&bytes)
    }

    fn set_sample_rate(&mut self, hz: u32) {
        self.nes.bus.apu.set_sample_rate(f64::from(hz));
    }

    fn sample_rate(&self) -> u32 {
        self.nes.bus.apu.sample_rate as u32
    }

    fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        let apu = &mut self.nes.bus.apu;
        let (left, right): (&[f32], &[f32]) = if apu.samples_right.is_empty() {
            (&apu.samples, &apu.samples)
        } else {
            (&apu.samples, &apu.samples_right)
        };
        let frames = left.len().min(right.len()).min(out.len() / 2);
        for i in 0..frames {
            out[i * 2] = left[i];
            out[i * 2 + 1] = right[i];
        }
        apu.samples.clear();
        apu.samples_right.clear();
        frames
    }

    fn channel_names(&self) -> &'static [&'static str] {
        const NAMES: [&str; 6] = ["pulse1", "pulse2", "triangle", "noise", "dmc", "expansion"];
        &NAMES
    }

    fn set_channel_mask(&mut self, mask: u8) {
        for (index, channel) in Channel::ALL.into_iter().enumerate() {
            let bit = 1 << index;
            let enabled = mask & bit != 0;
            let was_enabled = self.mask & bit != 0;
            if was_enabled && !enabled {
                // Remember the channel's gain, then silence its mixer input.
                self.gains.set(channel, self.nes.bus.apu.gain.get(channel));
                self.nes.bus.apu.gain.set(channel, 0.0);
            } else if !was_enabled && enabled {
                self.nes.bus.apu.gain.set(channel, self.gains.get(channel));
            }
        }
        self.mask = mask;
    }

    fn set_buttons(&mut self, buttons: Buttons) {
        let mut bits = 0u8;
        for (button, value) in [
            (Buttons::A, 0x01),
            (Buttons::B, 0x02),
            (Buttons::SELECT, 0x04),
            (Buttons::START, 0x08),
            (Buttons::UP, 0x10),
            (Buttons::DOWN, 0x20),
            (Buttons::LEFT, 0x40),
            (Buttons::RIGHT, 0x80),
        ] {
            if buttons.contains(button) {
                bits |= value;
            }
        }
        self.nes.bus.pads[0].set_buttons(bits);
    }

    fn battery_body(&self) -> Option<Vec<u8>> {
        if self.nes.battery || self.nes.chr_battery {
            Some(save::battery_snapshot(&self.nes))
        } else {
            None
        }
    }

    fn load_battery_body(&mut self, bytes: &[u8], _now_ms: u64) -> Result<bool, String> {
        Ok(save::load_battery_body(&mut self.nes, bytes))
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.nes.save_state()
    }

    fn load_state(&mut self, blob: &[u8]) -> Result<(), StateRefusal> {
        self.nes
            .load_state(blob)
            .map_err(|why| StateRefusal::Core(format!("{why:?}")))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn disk_side(&self) -> Option<(usize, usize)> {
        self.nes.bus.cart().disk_side()
    }

    fn next_disk_side(&mut self) -> Option<(usize, usize)> {
        self.nes.bus.cart_mut().next_disk_side()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// iNES 1.0, NROM: one 16 KiB PRG bank of NOPs whose vectors point at $8000, 8 KiB CHR-RAM.
    fn nrom_image() -> Vec<u8> {
        let mut image = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut prg = vec![0xEAu8; 16_384];
        let n = prg.len();
        prg[n - 6..].copy_from_slice(&[0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
        image.extend(prg);
        image.resize(image.len() + 8192, 0);
        image
    }

    /// A one-song NSF whose PLAY routine starts pulse 1 on a $700 period; INIT is a bare RTS.
    fn nsf_image() -> Vec<u8> {
        let program: [u8; 22] = [
            0x60, 0xA9, 0xBF, 0x8D, 0x00, 0x40, 0xA9, 0x08, 0x8D, 0x01, 0x40, 0xA9, 0x40, 0x8D,
            0x02, 0x40, 0xA9, 0x00, 0x8D, 0x03, 0x40, 0x60,
        ];
        let mut nsf = vec![0u8; 0x80];
        nsf[0..5].copy_from_slice(b"NESM\x1A");
        nsf[5] = 1;
        nsf[6] = 1;
        nsf[7] = 1;
        nsf[8..10].copy_from_slice(&0x8000u16.to_le_bytes());
        nsf[10..12].copy_from_slice(&0x8000u16.to_le_bytes());
        nsf[12..14].copy_from_slice(&0x8001u16.to_le_bytes());
        nsf.extend_from_slice(&program);
        nsf
    }

    #[test]
    fn a_cartridge_machine_wraps_its_board() {
        let image = nrom_image();
        let data = crate::cart::split(&image).expect("the harness's own NROM image parses");
        let crc32 = gamedb::rom_crc32(&data.prg, &data.chr);
        let mut machine =
            NesMachine::new(image, NesImage::Cartridge, None, ConsoleConfig::default())
                .expect("the harness's own NROM image loads");
        assert_eq!(machine.kind(), SystemKind::Nes);
        assert_eq!(machine.console_name(), "NES (NTSC)");
        assert_eq!(machine.cart_storage(), "mapper 0");
        assert_eq!(machine.cart_identity().crc32, crc32);
        assert!(machine.capabilities().has(Capability::Cheats));
        assert!(!machine.capabilities().has(Capability::DiskSwap));
        assert!(!machine.capabilities().has(Capability::NsfPlayer));
        assert_eq!(machine.frames(), 0);
        machine.run_frame();
        assert_eq!(machine.frames(), 1);
        let frame = machine.frame();
        assert_eq!((frame.width, frame.height), (256, 240));
    }

    #[test]
    fn an_nsf_machine_reports_the_player() {
        let machine = NesMachine::new(
            nsf_image(),
            NesImage::Nsf { song: 1 },
            None,
            ConsoleConfig::default(),
        )
        .expect("the harness's own NSF loads");
        assert_eq!(machine.cart_storage(), "NSF player");
        assert!(machine.capabilities().has(Capability::NsfPlayer));
        assert!(!machine.capabilities().has(Capability::DiskSwap));
    }

    #[test]
    fn the_channel_mask_silences_and_restores_each_gain() {
        let mut machine = NesMachine::new(
            nsf_image(),
            NesImage::Nsf { song: 1 },
            None,
            ConsoleConfig::default(),
        )
        .expect("the harness's own NSF loads");
        machine.set_channel_mask(0);
        for channel in Channel::ALL {
            assert_eq!(machine.nes.bus.apu.gain.get(channel), 0.0);
        }
        machine.set_channel_mask(0xFF);
        for channel in Channel::ALL {
            assert_eq!(machine.nes.bus.apu.gain.get(channel), 1.0);
        }
    }
}
