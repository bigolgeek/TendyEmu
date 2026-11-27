//! The five-frame window a `_TRN` command's 4 KiB takes.
//!
//! Pan Docs, *VRAM Transfers*, *Transfer Time*, is the whole rule:
//!
//! > Note that the transfer data should be prepared in VRAM **before** sending the transfer command
//! > packet. The actual transfer starts at the beginning of the next frame after the command has
//! > been sent, and the transfer ends at the end of the 5th frame after the command has been sent
//! > (not counting the frame in which the command has been sent). The displayed data must not be
//! > modified during the transfer, as the SGB reads it in multiple chunks.
//!
//! So a `_TRN` command does not hand over its data: it starts a read of the 4 KiB block that lasts
//! five frames, and the command's effect (the border, the palettes, the attribute files) is only
//! there once that read has finished. A cartridge that changes VRAM inside the window is read in
//! two halves — the chunks already taken keep the old bytes — which is the "multiple chunks"
//! sentence, and the reason the command cannot be applied when it arrives.
//!
//! shortcut: Pan Docs does not say where inside the five frames each chunk is read. This model
//! takes one fifth (820 bytes, 816 for the last) at the start of each of the five frames; upgrade
//! if a cartridge turns out to depend on a read landing mid-frame.
//!
//! Nothing here is save-state: like the 130-pulse receiver and the multi-packet area buffer, a
//! transfer in flight is dropped when a snapshot loads ([`crate::ppu::Ppu::load_state`]) and the
//! machine goes back to the state it was in — a state written between frames.

use crate::sgb::packets::{Command, Packet};

/// The block a `_TRN` command reads: Pan Docs, *VRAM Transfers*, *Transfer Data*: "larger data
/// blocks of 4KBytes can be transferred by using the video signal".
pub const TRN_BYTES: usize = 0x1000;

/// Frames the read lasts, the command's own frame not counted: "the transfer ends at the end of the
/// 5th frame after the command has been sent (not counting the frame in which the command has been
/// sent)".
pub const TRN_FRAMES: u8 = 5;

/// Bytes taken at each of those five frame starts: one fifth of the block, rounded up, so the last
/// read is the short one.
const TRN_CHUNK: usize = TRN_BYTES.div_ceil(TRN_FRAMES as usize);

/// Whether this packet is one whose 4 KiB the emulator consumes — `PAL_TRN`, `ATTR_TRN`, `CHR_TRN`,
/// `PCT_TRN` and `SOU_TRN`. Every one of them reads the block, so every one goes through the
/// five-frame window. (`SOU_TRN`'s block is the sound program and score the SGB asked to have
/// uploaded, parsed and not sent on. `DATA_TRN` is a VRAM transfer too, but it carries
/// SNES-side data that is dropped either way, so it never reaches here.)
pub fn is_trn(packet: &Packet) -> bool {
    matches!(
        Command::from_code(packet.command()),
        Command::PalTrn | Command::AttrTrn | Command::ChrTrn | Command::PctTrn | Command::SouTrn
    )
}

/// One `_TRN` transfer in flight: the packet to apply when the read finishes, the block to apply it
/// to, and how many frame starts have gone by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trn {
    /// The command packet, kept so completion is the same call the command would have made.
    packet: Packet,
    /// Where the block was when the command arrived: `$8000` (0) or `$8800` (0x800), from LCDC bit 4
    /// ([`crate::ppu::Ppu::apply_sgb_packet`]). Pan Docs tells the cartridge to leave the display
    /// alone during the transfer, and the five reads must see one block, so it is taken once.
    base: usize,
    /// What the SNES has read so far; the bytes before the window are zero until read.
    block: Box<[u8; TRN_BYTES]>,
    /// Frame starts since the command: 1..=5 read a chunk, 6 completes.
    frames: u8,
}

impl Trn {
    /// A transfer the command just started. Nothing is read yet: Pan Docs has the first read at the
    /// beginning of the *next* frame.
    pub fn new(packet: &Packet, base: usize) -> Trn {
        assert!(base + TRN_BYTES <= 0x2000, "the transfer block is in VRAM");
        Trn {
            packet: *packet,
            base,
            block: Box::new([0; TRN_BYTES]),
            frames: 0,
        }
    }

    /// The packet that started this transfer.
    pub fn packet(&self) -> &Packet {
        &self.packet
    }

    /// What has been read so far. Only the whole block once [`Trn::tick`] has returned `true`.
    pub fn block(&self) -> &[u8] {
        &self.block[..]
    }

    /// One frame start: take the next chunk of `vram`, and say whether the read is finished.
    ///
    /// `vram` is the console's 8 KiB. The chunk goes to the same offset in the block it came from,
    /// because Pan Docs has the SNES "re-produce the same ordering of bits and bytes".
    pub fn tick(&mut self, vram: &[u8]) -> bool {
        self.frames += 1;
        if self.frames > TRN_FRAMES {
            return true;
        }
        let start = usize::from(self.frames - 1) * TRN_CHUNK;
        let end = (start + TRN_CHUNK).min(TRN_BYTES);
        self.block[start..end].copy_from_slice(&vram[self.base + start..self.base + end]);
        false
    }
}
