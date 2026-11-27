//! Super Game Boy: the packet receiver, the palettes, the attribute map and the border.
//!
//! **The receiver is wired.** `Bus::write_io`/`read_io` hand `$FF00` to [`Responder`] when
//! [`Sgb::enabled`], and a decoded packet reaches [`Sgb::apply`]. Nothing turns SGB on by itself:
//! only `Machine::new_sgb` (the `--sgb` flag or the `console = "sgb"` row) does, so a
//! `Machine` that is not given an SGB console draws exactly the frame it drew before this module
//! existed (`tests/sgb.rs` pins the number).
//!
//! **The split.** Pan Docs puts the SGB conversation on `JOYP` bits 4 and 5, *not* on the serial
//! port: "Command packets are transferred from the Game Boy to the SNES by using bits 4 and 5 of
//! of the `JOYP` register" (*Command Packet Transfers*). So there are two values, in two places:
//!
//! - [`Sgb`] is the state the picture depends on — the four visible palettes, the 20×18 attribute
//!   map and `MASK_EN`'s state. It lives in [`crate::ppu::Ppu`] beside `palettes`, so the scanline
//!   renderer can read it without a new argument through `Ppu::tick`.
//! - [`Responder`] is the 130-pulse receiver that turns writes to `$FF00` into [`Packet`]s. It
//!   belongs to [`crate::bus::Bus`] beside `joypad` and `serial`, and it is *not* a field of
//!   [`crate::serial::Serial`]: an SGB game never writes `SB`/`SC` for a packet, so a responder
//!   that only watched the serial registers would never see a byte. `Bus::write_io`'s `$FF00` arm
//!   is the one hook, and it is two lines: `write_io` feeds the receiver, `read_io` takes its byte.
//!
//! The SNES side of sound is a third thing, and it is neither of these: the chip
//! ([`SgbSound`], `gb::sgb::spc`) lives in [`crate::apu::Apu`] beside the Game Boy's four channels,
//! because it is audio and the mixer has to reach it every sample. It exists only when the
//! console is an SGB *and* the user has pointed `Emulation > SGB ROM file...` at their own SGB BIOS
//! image; with no file there is no chip and every sample is what it was before. When there is one,
//! [`crate::Machine`] boots the cartridge's own driver into it and hands it each frame's `SOUND` and
//! `SOU_TRN`.
//!
//! Neither value is created unless the console is an SGB. The encoding grows one leg at
//! a time, each leg adding its own two lines at the end of `Ppu::save_state`/`load_state` and
//! bumping `state::STATE_VERSION`. Version 6 is the attribute map's 4 140 bytes, version
//! 7 the palettes' 4 130 and version 8 `MASK_EN`'s 3 and version 9 the border's
//! tiles, map and palettes:
//!
//! ```text
//! w.bool(self.sgb.enabled());          // version 6 -> 7
//! self.sgb.palettes.save_state(w);     // 32 + 4096 bytes
//! self.sgb.attr.save_state(w);         // 4050 + 90 bytes, version 6
//! self.sgb.mask.save_state(w);         // 1 + 2 bytes, version 8
//! self.sgb.border.save_state(w);       // 1 + 8 KiB + 1792 + 96 bytes, version 9
//! ```
//!
//! Separate calls on purpose: a leg that lands first must not call a function another leg has not
//! filled yet. `Machine::encode_state`/`decode_state` keep their own copy of the same field list
//! rather than calling `Ppu::save_state`; the line had to be added in both places, and the
//! two lists must stay in step.

pub mod areas;
pub mod attr;
pub mod border;
pub mod packets;
pub mod palettes;
pub mod sound;
pub mod spc;
pub mod transfer;

pub use areas::Areas;
pub use attr::{ATF_BYTES, ATF_FILES, AttrMap, CELLS_DOWN, CELLS_WIDE};
pub use border::{
    Border, MaskState, SGB_FRAME_HEIGHT, SGB_FRAME_PIXELS, SGB_FRAME_WIDTH, SgbFrame,
    WINDOW_HEIGHT, WINDOW_WIDTH, WINDOW_X, WINDOW_Y,
};
pub use packets::{Command, Packet, Responder, SoundRequest};
pub use palettes::{DEFAULT_RAMP, SYSTEM_PALETTES, SgbPalettes};
pub use sound::{SgbSound, SoundTransfer};
pub use transfer::{TRN_BYTES, TRN_FRAMES, Trn};

/// The SGB state a frame is drawn from: whether SGB is on, the four visible palettes, the live
/// attribute map, the `MASK_EN` state and the border.
///
/// A DMG console with SGB off holds `Sgb::off()`, every field at its neutral value, and
/// [`Sgb::colour`] returns `None` for every pixel; the DMG renderer then keeps its own shade path
/// and no frame moves.
#[derive(Clone, PartialEq, Eq)]
pub struct Sgb {
    /// False unless the console is an SGB: `Machine::new_sgb` is the only caller that sets it.
    enabled: bool,
    /// The four visible palettes.
    palettes: SgbPalettes,
    /// The 45 uploaded attribute files and the live 20×18 map.
    attr: AttrMap,
    /// The four area commands' multi-packet buffer. Session state: not encoded, and
    /// dropped by [`Sgb::clear_transfer`] when a state loads.
    areas: Areas,
    /// `MASK_EN`'s state.
    mask: MaskState,
    /// The 256×224 border: our art, or the cartridge's after `CHR_TRN`+`PCT_TRN`.
    border: Border,
    /// The last `SOUND` (`$08`) command a cartridge sent. A machine whose console is an SGB
    /// *and* whose user set an SGB ROM file has a chip, and the machine hands this to it once
    /// ([`Sgb::take_sound`]); with no chip it is the whole of what the request is answered
    /// with ([`Sgb::sound`]). Session state, like `areas`.
    sound: Option<SoundRequest>,
    /// The last `SOU_TRN` (`$09`) block a cartridge sent, parsed and not yet uploaded:
    /// `docs/SGB-SOUND.md` says what the SGB program uploads, and the machine hands it to the chip once
    /// ([`Sgb::take_sound_transfer`]). Session state, like `sound`.
    sound_transfer: Option<sound::SoundTransfer>,
}

impl Sgb {
    /// An SGB that is off: what every non-SGB machine holds.
    pub fn off() -> Sgb {
        Sgb {
            enabled: false,
            palettes: SgbPalettes::new(),
            attr: AttrMap::blank(),
            areas: Areas::new(),
            mask: MaskState::Active,
            border: Border::new(),
            sound: None,
            sound_transfer: None,
        }
    }

    /// An SGB that is on, with our clean-room default palettes and a blank attribute map.
    pub fn on() -> Sgb {
        Sgb {
            enabled: true,
            ..Sgb::off()
        }
    }

    /// Whether the renderer should ask this value for colours.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The four visible palettes.
    pub fn palettes(&self) -> &SgbPalettes {
        &self.palettes
    }

    /// The attribute map.
    pub fn attr(&self) -> &AttrMap {
        &self.attr
    }

    /// The attribute map, for the state decoder: `Ppu::load_state` restores it in place.
    pub fn attr_mut(&mut self) -> &mut AttrMap {
        &mut self.attr
    }

    /// The palettes, for the state decoder: `Ppu::load_state` restores them in place.
    pub fn palettes_mut(&mut self) -> &mut SgbPalettes {
        &mut self.palettes
    }

    /// `MASK_EN`'s state, for the state decoder: `Ppu::load_state` restores it in place.
    pub fn mask_mut(&mut self) -> &mut MaskState {
        &mut self.mask
    }

    /// The border: our art until a cartridge sends `PCT_TRN`, then its own picture.
    pub fn border(&self) -> &Border {
        &self.border
    }

    /// The border, for the state decoder: `Ppu::load_state` restores it in place.
    pub fn border_mut(&mut self) -> &mut Border {
        &mut self.border
    }

    /// Whether a multi-packet area transfer is still expecting continuation packets.
    pub fn receiving(&self) -> bool {
        self.areas.receiving()
    }

    /// The last `SOUND` (`$08`) command a cartridge sent, `None` until one arrives.
    ///
    /// It is what the cartridge's sound request is answered with. A machine with a sound chip (an SGB
    /// console *and* the user's own SGB ROM file) hands it to the chip, which plays it and echoes
    /// the ports; with no chip the four Game Boy channels are all the audio there is, and
    /// the request stays recorded here for whoever asks ([`Sgb::sound`]).
    pub fn sound(&self) -> Option<SoundRequest> {
        self.sound
    }

    /// Take the last `SOUND` (`$08`) request, for the sound chip to play.
    ///
    /// A request is delivered once: the chip's echo on `$2140-$2143`, not this value, is how the
    /// cartridge learns it was taken (`docs/SGB-SOUND.md` §2). `None` when nothing new has arrived
    /// since the last take.
    pub fn take_sound(&mut self) -> Option<SoundRequest> {
        self.sound.take()
    }

    /// The last `SOU_TRN` (`$09`) block a cartridge sent, parsed into `(APU address, bytes)` packets
    /// and the jump address that ends the list, `None` until one arrives.
    ///
    /// It is what the chip is given when there is one: the payloads go into APU RAM and
    /// the SPC700 starts at the jump address. `refused` is set when the block was not a whole
    /// transfer, and holds whatever parsed before the fault — such a block is never delivered.
    pub fn sound_transfer(&self) -> Option<&sound::SoundTransfer> {
        self.sound_transfer.as_ref()
    }

    /// Take the last `SOU_TRN` (`$09`) block, for the sound chip to upload. Delivered
    /// once, like [`Sgb::take_sound`].
    pub fn take_sound_transfer(&mut self) -> Option<sound::SoundTransfer> {
        self.sound_transfer.take()
    }

    /// Drop any multi-packet transfer in flight; a loaded state starts with none.
    pub fn clear_transfer(&mut self) {
        self.areas = Areas::new();
    }

    /// Turn SGB on or off — the state decoder's mirror of [`Sgb::enabled`].
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// `MASK_EN`'s state.
    pub fn mask(&self) -> MaskState {
        self.mask
    }

    /// Replace `MASK_EN`'s state.
    pub fn set_mask(&mut self, mask: MaskState) {
        self.mask = mask;
    }

    /// The RGB555 colour for one pixel of the 160×144 window, or `None` when SGB is off.
    ///
    /// `cell_x` and `cell_y` are the attribute map's cell — screen pixel ÷ 8, **not** tile-map
    /// coordinates — and `index` is the pixel's 0-3 colour index. The colour is
    /// `attr.palette_at(cell_x, cell_y)` looked up in `palettes.colour`.
    pub fn colour(&self, cell_x: usize, cell_y: usize, index: u8) -> Option<u16> {
        if !self.enabled {
            return None;
        }
        let palette = self.attr.palette_at(cell_x, cell_y);
        Some(self.palettes.colour(palette, index))
    }

    /// Apply one decoded packet.
    ///
    /// `vram` is the 4 KiB block a `_TRN` command reads: `$8000-$8FFF`, or `$8800-$97FF` when
    /// LCDC bit 4 selects the `$8800` character data area ([`crate::ppu::Ppu::apply_sgb_packet`]
    /// picks it). `PAL_TRN`, `ATTR_TRN`, `CHR_TRN` and `PCT_TRN` are the commands that look at it.
    /// Those four do not reach here when they arrive: [`crate::ppu::Ppu`] holds them for the five
    /// frames Pan Docs says the transfer takes and calls this with the block that was read
    /// ([`crate::sgb::transfer`]).
    ///
    /// The commands this project implements are PAL01/PAL23/PAL03/PAL12 and PAL_SET, PAL_TRN,
    /// PAL_PRI, ATTR_TRN and ATTR_SET, MASK_EN, the four area
    /// commands ATTR_BLK/ATTR_LIN/ATTR_DIV/ATTR_CHR, CHR_TRN/PCT_TRN, SOUND
    /// and SOU_TRN. `MLT_REQ` and `ICON_EN` never reach here: they are the
    /// receiver's own, and [`Responder::deliver`] keeps them. `SOUND`'s four bytes and `SOU_TRN`'s
    /// parsed block are kept here for the machine to hand to the sound chip at the frame's end
    ///. Everything else — DATA_SND, DATA_TRN, JUMP, OBJ_TRN, TEST_EN, ATRC_EN — is
    /// decoded and dropped.
    pub fn apply(&mut self, packet: &Packet, vram: &[u8]) {
        // A continuation packet of a multi-packet area command carries data, not a header, so it
        // has to be recognised before the dispatch decodes `bytes[0]` as a command code.
        if self.areas.receiving() {
            self.areas.apply(packet, &mut self.attr);
            return;
        }
        match Command::from_code(packet.command()) {
            Command::Pal01 => self.palettes.set_pair(0, 1, packet),
            Command::Pal23 => self.palettes.set_pair(2, 3, packet),
            Command::Pal03 => self.palettes.set_pair(0, 3, packet),
            Command::Pal12 => self.palettes.set_pair(1, 2, packet),
            Command::PalTrn => self.palettes.pal_trn(vram),
            Command::PalPri => self.palettes.set_priority(packet.bytes[1]),
            Command::PalSet => {
                self.palettes.pal_set(packet);
                let flags = packet.bytes[9];
                // Pan Docs, PAL_SET: bit 6 cancels MASK_EN, bit 7 applies an attribute file.
                if flags & 0x40 != 0 {
                    self.set_mask(MaskState::Active);
                }
                if flags & 0x80 != 0 {
                    self.attr.apply_file(flags & 0x3F);
                }
            }
            Command::AttrTrn => self.attr.attr_trn(vram),
            Command::AttrSet => {
                if packet.bytes[1] & 0x40 != 0 {
                    self.set_mask(MaskState::Active);
                }
                self.attr.apply_file(packet.bytes[1] & 0x3F);
            }
            Command::MaskEn => self.set_mask(match packet.bytes[1] {
                0 => MaskState::Active,
                1 => MaskState::Frozen,
                2 => MaskState::Blanked(0x0000),
                3 => MaskState::Blanked(self.palettes.colour(0, 0)),
                _ => MaskState::Active,
            }),
            Command::AttrBlk | Command::AttrLin | Command::AttrDiv | Command::AttrChr => {
                self.areas.apply(packet, &mut self.attr);
            }
            // The cartridge's own border. Byte 1 bit 0 picks which 128 tiles the 4 KiB
            // of VRAM holds; `PCT_TRN` carries the map and palettes that place them.
            Command::ChrTrn => self.border.chr_trn(packet.bytes[1], vram),
            Command::PctTrn => self.border.pct_trn(vram),
            // SGB sound. The machine hands it to the chip at the frame's end when there is
            // one; with no chip it is recorded and nothing is played.
            Command::Sound => self.sound = Some(SoundRequest::from_packet(packet)),
            // `SOU_TRN`'s 4 KiB, parsed into the packets it holds. The chip takes them at
            // the frame's end, if there is one.
            Command::SouTrn => self.sound_transfer = Some(sound::parse_transfer(vram)),
            _ => {}
        }
    }
}

impl Default for Sgb {
    fn default() -> Self {
        Sgb::off()
    }
}
