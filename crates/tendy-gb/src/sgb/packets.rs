//! `$FF00` bits 4-5: the 130-pulse packet receiver (Pan Docs, *Command Packet Transfers*).
//!
//! The framing, quoted from Pan Docs so the leg does not have to rediscover it:
//!
//! - "A command packet transfer must be initiated by setting `JOYP` bits 4 and 5 both to 0; this
//!   will reset and start the ICD2 packet receiving circuit."
//! - "Data is then transferred (LSB first), setting bit 4 to 0 will indicate a `0` bit, and setting
//!   bit 5 to 0 will indicate a `1` bit."
//! - "Each packet transfer is started by a "Start" pulse, then 16 bytes (128 bits) of data are
//!   transferred, the LSB of each byte first; and finally, a `0` bit must be transferred as a stop
//!   bit."
//! - "The structure of the first packet in a transmission is: 1 pulse: Start signal / 1 byte:
//!   Header byte (`Command Code` × 8 + `Length`) / 15 bytes: `Data` / 1 bit: Stop Bit (`0`)."
//! - "The above `Length` indicates the total number of packets (1-7, including the first packet)".
//!
//! A pulse is a write to `$FF00` with one of bits 4/5 low; the separation between pulses is a
//! write with both high. Pan Docs asks software to hold a pulse low for at least 5 M-cycles and
//! the space for at least 15, and says hardware accepts 2 M-cycle pulses; this receiver does not
//! time anything, it counts writes. That is a deliberate, named simplification: a game that writes
//! `$FF00` twice inside one pulse would be misread, and no game does that inside a transfer
//! ("Obviously, it'd be no good idea to modify the joypad register in the middle of a transfer").

use crate::state::{StateError, StateReader, StateWriter};

/// One 16-byte packet, as transferred.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Packet {
    /// The sixteen bytes in transfer order. `bytes[0]` is the header of the first packet of a
    /// transmission; bits arrive LSB-first within each byte, so bit 0 is sent first.
    pub bytes: [u8; 16],
}

impl Packet {
    /// The command code: `header >> 3` (Pan Docs: "Command Code × 8 + Length").
    pub fn command(&self) -> u8 {
        self.bytes[0] >> 3
    }

    /// How many packets the transmission holds, the first included: `header & 7`, Pan Docs'
    /// "total number of packets (1-7, including the first packet)".
    pub fn length(&self) -> u8 {
        self.bytes[0] & 0x07
    }
}

/// The SGB system command codes of Pan Docs' *Command Summary*, documented data rather than a
/// BIOS dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `$00` PAL01: palettes 0 and 1.
    Pal01,
    /// `$01` PAL23: palettes 2 and 3.
    Pal23,
    /// `$02` PAL03: palettes 0 and 3.
    Pal03,
    /// `$03` PAL12: palettes 1 and 2.
    Pal12,
    /// `$04` ATTR_BLK: rectangles of the 20×18 map. One or more data sets; three or more
    /// continue into further packets.
    AttrBlk,
    /// `$05` ATTR_LIN: whole horizontal or vertical lines of the map. Fifteen or more
    /// data sets continue into further packets.
    AttrLin,
    /// `$06` ATTR_DIV: the screen split in two at a line. Fixed at one packet.
    AttrDiv,
    /// `$07` ATTR_CHR: a run of cells in one direction. Forty-one or more data sets
    /// continue into further packets.
    AttrChr,
    /// `$08` SOUND: the SNES's own effects, by ID, pitch and volume (recorded; with no SGB ROM
    /// file there is no audio path, so nothing is played).
    Sound,
    /// `$09` SOU_TRN: sound program/data. Dropped; SGB sound is out of scope.
    SouTrn,
    /// `$0A` PAL_SET: four system-palette IDs, plus the ATF flag (with the map).
    PalSet,
    /// `$0B` PAL_TRN: 4 KiB of system palette data by VRAM transfer.
    PalTrn,
    /// `$0C` ATRC_EN: attraction mode. Dropped.
    AttrEn,
    /// `$0D` TEST_EN: the speed function. Dropped.
    TestEn,
    /// `$0E` ICON_EN: the icon/register-file disable bits. The receiver records it.
    IconEn,
    /// `$0F` DATA_SND: SNES work RAM write. Dropped.
    DataSnd,
    /// `$10` DATA_TRN: SNES program/data transfer. Dropped.
    DataTrn,
    /// `$11` MLT_REQ: one, two or four joypads (read back through `P1`).
    MltReq,
    /// `$12` JUMP: SNES program counter. Dropped.
    Jump,
    /// `$13` CHR_TRN: 128 border tiles from 4 KiB of VRAM; byte 1 bit 0 picks the half.
    ChrTrn,
    /// `$14` PCT_TRN: the 32×28 border map and palettes 4-6 from 4 KiB of VRAM.
    PctTrn,
    /// `$15` ATTR_TRN: 45 attribute files by VRAM transfer.
    AttrTrn,
    /// `$16` ATTR_SET: copy one attribute file to the screen.
    AttrSet,
    /// `$17` MASK_EN: cancel, freeze or blank the window.
    MaskEn,
    /// `$18` OBJ_TRN: SNES object mode. Dropped.
    ObjTrn,
    /// `$19` PAL_PRI: system palette priority. The bit is recorded and nothing is done with it.
    PalPri,
    /// A code Pan Docs' summary does not list: decoded, ignored.
    Unknown(u8),
}

impl Command {
    /// The command a packet's code selects (Pan Docs, *Command Summary*).
    pub fn from_code(code: u8) -> Command {
        match code {
            0x00 => Command::Pal01,
            0x01 => Command::Pal23,
            0x02 => Command::Pal03,
            0x03 => Command::Pal12,
            0x04 => Command::AttrBlk,
            0x05 => Command::AttrLin,
            0x06 => Command::AttrDiv,
            0x07 => Command::AttrChr,
            0x08 => Command::Sound,
            0x09 => Command::SouTrn,
            0x0A => Command::PalSet,
            0x0B => Command::PalTrn,
            0x0C => Command::AttrEn,
            0x0D => Command::TestEn,
            0x0E => Command::IconEn,
            0x0F => Command::DataSnd,
            0x10 => Command::DataTrn,
            0x11 => Command::MltReq,
            0x12 => Command::Jump,
            0x13 => Command::ChrTrn,
            0x14 => Command::PctTrn,
            0x15 => Command::AttrTrn,
            0x16 => Command::AttrSet,
            0x17 => Command::MaskEn,
            0x18 => Command::ObjTrn,
            0x19 => Command::PalPri,
            code => Command::Unknown(code),
        }
    }
}

/// The decoder Pan Docs' *SGB Command $08 — SOUND* gives: the SNES's own sound effects, two ports,
/// a pitch and a volume each, plus a music (BGM) code.
///
/// Pan Docs, *SGB Command $08 — SOUND*: byte 1 is "Sound Effect A (Port 1) Decrescendo 8-bit Sound
/// Code", byte 2 "Sound Effect B (Port 2) Sustain 8-bit Sound Code", byte 3 "Sound Effect
/// Attributes" with "Bit 0-1 - Sound Effect A Pitch (0..3=Low..High)", "Bit 2-3 - Sound Effect A
/// Volume (0..2=High..Low, 3=Mute on)", "Bit 4-5 - Sound Effect B Pitch (0..3=Low..High)" and
/// "Bit 6-7 - Sound Effect B Volume (0..2=High..Low, 3=Not used)", and byte 4 "Music Score Code
/// (must be zero if not used)".
///
/// **It is a record, not sound.** The SNES APU has no audio path in this emulator: the
/// effect IDs and the BGM code below are kept so a cartridge's request is understood and can be
/// read back, and nothing is played. The four Game Boy channels are unaffected and still sound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundRequest {
    /// Sound effect A (port 1): the 8-bit decrescendo effect code, `$00-$30` and `$80` (Pan Docs'
    /// *Sound Effect A IDs*).
    pub effect_a: u8,
    /// Sound effect B (port 2): the 8-bit sustain effect code (Pan Docs' *Sound Effect B IDs*).
    pub effect_b: u8,
    /// Byte 3: the two effects' pitches and volumes.
    pub attributes: u8,
    /// Byte 4: the music score code — Pan Docs' BGM flag, `$01-$0F` for the system's own BGM and
    /// `$80` to stop it.
    pub music: u8,
}

impl SoundRequest {
    /// The four bytes a `SOUND` packet carries.
    pub fn from_packet(packet: &Packet) -> SoundRequest {
        SoundRequest {
            effect_a: packet.bytes[1],
            effect_b: packet.bytes[2],
            attributes: packet.bytes[3],
            music: packet.bytes[4],
        }
    }
}

/// The packet receiver: writes to `$FF00` in, complete packets out ([`Responder::write_joyp`]
/// and [`Responder::read_joyp`]).
///
/// It is session state, not save-state: a state is only written between frames, and a packet in
/// flight when a state is written is a transfer the game will finish before its next state.
#[derive(Clone, Debug)]
pub struct Responder {
    /// True from a start pulse until the stop bit closes the packet.
    receiving: bool,
    /// Bits of the current packet received, 0..=128.
    received: u16,
    /// The packet under construction.
    packet: Packet,
    /// Bits 4-5 of the previous `$FF00` write, for `MLT_REQ`'s `P15` low-to-high edge.
    select: u8,
    /// The joypad `MLT_REQ` reports next: 0 is player 1.
    player: u8,
    /// `MLT_REQ`'s count: 1, 2 or 4.
    players: u8,
    /// `ICON_EN`'s disable byte. Bit 2 suppresses every later packet.
    icons: u8,
}

impl Responder {
    /// A receiver waiting for a start pulse.
    pub fn new() -> Responder {
        Responder {
            receiving: false,
            received: 0,
            packet: Packet::default(),
            select: 0x30,
            player: 0,
            players: 1,
            icons: 0,
        }
    }

    /// Forget a packet in flight (a reset, or a state load).
    pub fn reset(&mut self) {
        self.receiving = false;
        self.received = 0;
        self.packet = Packet::default();
        self.select = 0x30;
        self.player = 0;
        self.icons = 0;
    }

    /// `ICON_EN`'s disable byte: bit 0 built-in palettes, bit 1 controller set-up screen, bit 2
    /// register-file transfer.
    pub fn icons(&self) -> u8 {
        self.icons
    }

    /// Record `ICON_EN`'s byte. A set bit 2 stops every later packet from being decoded;
    /// the receiver still consumes the pulses.
    pub fn set_icons(&mut self, icons: u8) {
        self.icons = icons;
    }

    /// `MLT_REQ`'s count: 1, 2 or 4 joypads.
    pub fn players(&self) -> u8 {
        self.players
    }

    /// Record `MLT_REQ`'s count: 0 means one player, 1 two, 3 four. `2` is not a value
    /// Pan Docs defines, and is treated as one player.
    pub fn set_players(&mut self, control: u8) {
        self.players = match control & 0x03 {
            1 => 2,
            3 => 4,
            _ => 1,
        };
    }

    /// Feed one write of `$FF00`, returning the packet the write completed, if any.
    ///
    /// One write is one pulse (Pan Docs, *Command Packet Transfers*): bits 4 and 5 both low start
    /// (or restart) a packet, bit 4 low alone is a `0` bit, bit 5 low alone is a `1` bit, and both
    /// high is the space between pulses. After 128 bits the next pulse must be a `0`, and it is
    /// that pulse which completes the packet. While idle a single-line-low write is an ordinary
    /// joypad row select and never starts a packet.
    pub fn write_joyp(&mut self, value: u8) -> Option<Packet> {
        let lines = value & 0x30;
        // 1. Both lines low: reset and start the receiving circuit. A start pulse during a packet
        //    restarts it, so this arm comes before the `receiving` check.
        if lines == 0 {
            self.receiving = true;
            self.received = 0;
            self.packet = Packet::default();
            self.select = value;
            return None;
        }
        // 2. Inside a packet, a pulse is a bit, a stop bit, or (both lines high) nothing at all.
        if self.receiving {
            if lines == 0x20 && self.received == 128 {
                // The stop bit: a `0` completes the packet.
                self.receiving = false;
                self.select = value;
                return Some(self.packet);
            }
            if lines == 0x10 && self.received == 128 {
                // A `1` where the stop bit belongs. Pan Docs only says a `0` "must" be sent; this
                // receiver discards the packet.
                self.receiving = false;
                self.select = value;
                return None;
            }
            if lines == 0x10 || lines == 0x20 {
                // Pan Docs: "setting bit 4 to 0 will indicate a `0` bit, and setting bit 5 to 0
                // will indicate a `1` bit" — 0x10 is a 1, 0x20 is a 0, LSB of each byte first.
                let index = self.received as usize;
                let bit = u8::from(lines == 0x10) << (index % 8);
                self.packet.bytes[index / 8] |= bit;
                self.received += 1;
            }
            // 0x30 is the space between pulses: no bit, and the packet keeps its place.
            self.select = value;
            return None;
        }
        // 3. Idle: the pulse is a joypad row select. `MLT_REQ` counts P15's low-to-high edge here
        //    (Pan Docs: "The next joypad is automatically selected when P15 goes from LOW (0) to
        //    HIGH (1)").
        if self.players > 1 && self.select & 0x20 == 0 && value & 0x20 != 0 {
            self.player = (self.player + 1) % self.players;
        }
        self.select = value;
        None
    }

    /// Overlay the `MLT_REQ` joypad ID on a read of `$FF00`.
    ///
    /// Pan Docs: "First set P14 and P15 both HIGH (deselect both Buttons and Cursor keys), you can
    /// now read the lower 4 bits of `P1`, which indicate the joypad ID for the following joypad
    /// input": `$xF` player 1, `$xE` player 2, `$xD` player 3, `$xC` player 4. With one player the
    /// read passes through untouched.
    pub fn read_joyp(&self, value: u8) -> u8 {
        if self.players > 1 && value & 0x30 == 0x30 {
            (value & 0xF0) | (0x0F - self.player)
        } else {
            value
        }
    }

    /// Hand one completed packet to the machine.
    ///
    /// `MLT_REQ` and `ICON_EN` are the receiver's own — they change its player count and its
    /// disable byte — and those two return `true`, meaning *consumed*. Every other command returns
    /// `false` and the caller hands it to [`crate::ppu::Ppu::apply_sgb_packet`], which is where the
    /// palette,
    /// attribute and mask legs receive it. `ICON_EN` bit 2 (“SGB Register File Transfer (0=Receive,
    /// 1=Disable)”) makes every later packet consumed and dropped.
    pub fn deliver(&mut self, packet: &Packet) -> bool {
        match Command::from_code(packet.command()) {
            Command::MltReq => {
                self.set_players(packet.bytes[1]);
                true
            }
            Command::IconEn => {
                self.set_icons(packet.bytes[1]);
                true
            }
            _ if self.icons & 0x04 != 0 => true,
            _ => false,
        }
    }

    /// The player `MLT_REQ` currently reports, 0 for player 1.
    pub fn player(&self) -> u8 {
        self.player
    }

    /// Encode the receiver (keep the player pair, drop a packet in flight).
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.player);
        w.u8(self.players);
        w.u8(self.icons);
    }

    /// Decode the receiver.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let player = r.u8()?;
        let players = r.u8()?;
        let icons = r.u8()?;
        self.reset();
        self.player = player;
        self.players = players;
        self.icons = icons;
        Ok(())
    }
}

impl Default for Responder {
    fn default() -> Self {
        Responder::new()
    }
}
