//! VRAM DMA: the CGB's `GDMA` and `HDMA` (`$FF51-$FF55`), Pan Docs *CGB Registers*, "LCD VRAM DMA
//! Transfers".
//!
//! The unit is a state holder and the register rules; the bytes move in [`crate::bus::Bus`], which
//! owns the memory the transfer reads and writes, exactly as `Dma` (the OAM transfer) is split from
//! `Bus::tick_dma`.
//!
//! **Two modes, one field.** `$FF55` bit 7 picks general-purpose (`0`: the whole range at once, the
//! CPU halted until it is done) or HBlank (`1`: one 16-byte block in each HBlank, the CPU halted
//! for the block only). Both take about 8 µs per block — 8 M-cycles at normal speed, 16 at double
//! speed — which is what makes the CPU's halt the block in flight rather than a cycle count.
//!
//! **The source and destination masks** are Pan Docs' wording, not an emulator's: the source's
//! lower four bits are ignored and everything else is used (`0000-7FF0` or `A000-DFF0`, plus the
//! untested ranges), and the destination's "only bits 12-4 are respected; others are ignored", with
//! the `$8000` base of VRAM added. A `$7FF0` mask would destroy a source at `$A000`, which Pan Docs
//! names as a legal one, so the mask here is `$FFF0`.
//!
//! **What `$FF55` reads** (Pan Docs): mid-transfer the blocks left, minus one, with bit 7 clear; a
//! completed transfer `$FF`; a transfer the cartridge stopped with bit 7 **set** and the blocks that
//! were left in the low seven bits. The register has never read anything else here — post-boot, and
//! on a DMG cartridge, it is `$FF`.

/// Bytes in one VRAM DMA block (`$10`).
pub const BLOCK_BYTES: u8 = 16;

/// `$FF51`: source high.
pub const HDMA1: u16 = 0xFF51;
/// `$FF52`: source low.
pub const HDMA2: u16 = 0xFF52;
/// `$FF53`: destination high.
pub const HDMA3: u16 = 0xFF53;
/// `$FF54`: destination low.
pub const HDMA4: u16 = 0xFF54;
/// `$FF55`: length, mode and start.
pub const HDMA5: u16 = 0xFF55;

/// The five registers and the transfer they describe.
#[derive(Clone, Debug)]
pub struct VramDma {
    /// `HDMA1`/`HDMA2` as written; the low four bits are ignored when a block reads it. During a
    /// transfer this is the pointer itself, which advances a byte at a time.
    pub source: u16,
    /// `HDMA3`/`HDMA4` as written; only bits 12-4 survive, and the transfer runs from `$8000`.
    /// During a transfer this is the VRAM address, which advances a byte at a time.
    pub dest: u16,
    /// Blocks left to send, minus one — Pan Docs' "length divided by `$10`, minus 1", `$00-$7F`.
    pub remaining: u8,
    /// Bit 7 of the last `$FF55` write: `true` for HBlank DMA, `false` for general purpose.
    pub hblank: bool,
    /// True while a transfer is in flight. A general-purpose transfer is active from the write
    /// until its last block; an HBlank one from the write until its last block or a cancellation.
    pub active: bool,
    /// True when the cartridge stopped an HBlank transfer with a `$FF55` write: `$FF55` then reads
    /// bit 7 set with the blocks that were left.
    pub cancelled: bool,
    /// Bytes still to move in the block in flight; 0 between blocks. Non-zero is the CPU's halt.
    pub in_block: u8,
    /// Blocks moved since reset, for the report's instrumentation (`Machine::vram_dma_blocks`).
    pub blocks: u64,
    /// The ones that were HBlank blocks — one per mode 0 — rather than general-purpose.
    pub hblank_blocks: u64,
}

impl VramDma {
    /// Post-boot: no transfer, `$FF55` reads `$FF` (Pan Docs' "a value of `$FF` indicates that the
    /// transfer has completed"), and the address registers are write-only.
    pub fn new() -> VramDma {
        VramDma {
            source: 0,
            dest: 0,
            remaining: 0x7F,
            hblank: false,
            active: false,
            cancelled: false,
            in_block: 0,
            blocks: 0,
            hblank_blocks: 0,
        }
    }

    /// The source address a block reads from: Pan Docs, "The four lower bits of this address will
    /// be ignored and treated as 0."
    pub fn source_addr(&self) -> u16 {
        self.source & 0xFFF0
    }

    /// The VRAM address a block writes to: Pan Docs, "Only bits 12-4 are respected; others are
    /// ignored", from the `$8000` base.
    pub fn dest_addr(&self) -> u16 {
        0x8000 | (self.dest & 0x1FF0)
    }

    /// The value `$FF55` reads right now.
    pub fn hdma5(&self) -> u8 {
        if self.active {
            self.remaining & 0x7F
        } else if self.cancelled {
            0x80 | (self.remaining & 0x7F)
        } else {
            // Idle, or a transfer that ran to its end: "a value of $FF indicates that the transfer
            // has completed".
            0xFF
        }
    }

    /// Bytes left in the whole transfer, the block in flight included.
    pub fn bytes_left(&self) -> u16 {
        self.remaining as u16 * BLOCK_BYTES as u16 + self.in_block as u16
    }

    /// True while a block owns the bus, which is the CPU's halt (Pan Docs: "The execution of the
    /// program is halted during the separate transfers").
    pub fn halting(&self) -> bool {
        self.in_block > 0
    }

    /// A read of `$FF51-$FF55`. The four address registers are write-only and answer `$FF`.
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            HDMA5 => self.hdma5(),
            _ => 0xFF,
        }
    }

    /// A write of `$FF51-$FF54`: the register with its field stored raw, so a write and a read of
    /// the other byte of the pair see what was written.
    pub fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            HDMA1 => self.source = (self.source & 0x00FF) | ((value as u16) << 8),
            HDMA2 => self.source = (self.source & 0xFF00) | value as u16,
            HDMA3 => self.dest = (self.dest & 0x00FF) | ((value as u16) << 8),
            HDMA4 => self.dest = (self.dest & 0xFF00) | value as u16,
            _ => {}
        }
    }

    /// `$FF55` with an idle unit: latch the length and the mode and put the first block on the
    /// bus. The caller decides whether that block is due now or at the next HBlank.
    pub fn start(&mut self, value: u8) {
        self.remaining = value & 0x7F;
        self.hblank = value & 0x80 != 0;
        self.active = true;
        self.cancelled = false;
        self.in_block = 0;
        self.source = self.source_addr();
        self.dest = self.dest_addr();
    }

    /// Stop an HBlank transfer where it is; `$FF55` then reads bit 7 set and the blocks left.
    pub fn cancel(&mut self) {
        self.active = false;
        self.cancelled = true;
        self.in_block = 0;
    }

    /// Put the next block on the bus.
    pub fn begin_block(&mut self) {
        self.in_block = BLOCK_BYTES;
    }

    /// The last byte of a block has moved: either the transfer is over — no blocks left, or the
    /// destination would run past `$9FFF` ("If the transfer's destination address overflows, the
    /// transfer stops prematurely") — or the next block begins. A general-purpose transfer keeps
    /// the CPU halted across the join; an HBlank one waits for the next HBlank.
    pub fn end_block(&mut self) -> bool {
        self.blocks += 1;
        if self.hblank {
            self.hblank_blocks += 1;
        }
        let overflow = self.dest >= 0xA000;
        if self.remaining == 0 || overflow {
            self.active = false;
            self.cancelled = false;
            self.remaining = 0;
            return true;
        }
        self.remaining -= 1;
        if !self.hblank {
            self.begin_block();
        }
        false
    }

    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.u16(self.source);
        w.u16(self.dest);
        w.u8(self.remaining);
        w.bool(self.hblank);
        w.bool(self.active);
        w.bool(self.cancelled);
        w.u8(self.in_block);
        w.u64(self.blocks);
        w.u64(self.hblank_blocks);
    }

    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.source = r.u16()?;
        self.dest = r.u16()?;
        self.remaining = r.u8()?;
        self.hblank = r.bool()?;
        self.active = r.bool()?;
        self.cancelled = r.bool()?;
        self.in_block = r.u8()?;
        self.blocks = r.u64()?;
        self.hblank_blocks = r.u64()?;
        Ok(())
    }
}

impl Default for VramDma {
    fn default() -> Self {
        VramDma::new()
    }
}
