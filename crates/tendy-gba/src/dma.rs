//! The Game Boy Advance's four DMA channels: their registers, and what one store means.
//!
//! GBATEK, *GBA DMA Transfers*, is the whole of the hardware. Each of the four channels owns three
//! registers at `$040000B0 + n*12`:
//!
//! - `DMAnSAD` (source address);
//! - `DMAnDAD` (destination address);
//! - `DMAnCNT_L` (transfer count: 14 bits on DMA0-2, 16 on DMA3, and `0` means its maximum);
//! - `DMAnCNT_H` (control, below).
//!
//! **The control word.** GBATEK's GBA bit table is not the one the DS uses, and reading it with the
//! DS layout turns an immediate 32-bit copy into a V-Blank halfword copy: F-Zero's own handler copy
//! is `$8400`-shaped — enable, 32-bit units, start immediately — and only decodes correctly as such
//! under this layout.
//!
//! ```text
//! bits 5-6   destination control (0 increment, 1 decrement, 2 fixed, 3 increment/reload)
//! bits 7-8   source control (0 increment, 1 decrement, 2 fixed, 3 prohibited)
//! bit  9     repeat
//! bit  10    unit size (0 = 16-bit, 1 = 32-bit)
//! bit  11    Game Pak DRQ (DMA3 only)
//! bits 12-13 start timing (0 immediately, 1 V-Blank, 2 H-Blank, 3 special)
//! bit  14    interrupt when the transfer ends
//! bit  15    enable
//! ```
//!
//! **Where the state lives.** All of it is register storage, so it lives in the I/O page and travels
//! in the save state with every other register; this module only gives those bytes their meaning.
//! The transfer itself — the reads, the writes and the cycles it costs — belongs to
//! [`crate::bus`], because memory access lives there.
//!
//! **The registers never move.** GBATEK, *Source and Destination Address and Word Count Registers*:
//! "The hardware does NOT change the content of these registers during or after the transfer"; the
//! walking addresses are internal pointers, reloaded from `SAD`/`DAD`/`CNT_L` when the enable bit
//! rises from 0 to 1. A read of `DMAnDAD` therefore returns the address the channel was *given*,
//! not the one it reached.
//!
//! **Start timing 3.** GBATEK's GBA table gives it per channel — "DMA0=Prohibited, DMA1/DMA2=Sound
//! FIFO, DMA3=Video Capture" — so nothing here starts a transfer on it: the sound FIFOs are G5's and
//! video capture is not implemented. (The `$04000000` word that starts a channel belongs to the DS,
//! not the GBA; GBATEK's GBA chapter has no such trigger.)
//!
//! **The address mask.** GBATEK: "The most significant address bits are ignored, only the least
//! significant 27 or 28 bits are used (max 07FFFFFFh internal memory, or max 0FFFFFFFh any memory)".
//! The channel's own heading says which it is: DMA0's source and DMA0-2's destinations are
//! "internal memory" (27 bits), the others "any memory" (28 bits).

/// How many DMA channels the GBA has.
pub const CHANNELS: usize = 4;

/// `DMA0SAD`: the first channel's source address. `$040000B0`.
pub const BASE: usize = 0xB0;

/// The stride from one channel's registers to the next.
pub const STRIDE: usize = 0xC;

/// Offsets of the four registers inside a channel's block.
pub const SAD: usize = 0x0;
pub const DAD: usize = 0x4;
pub const COUNT: usize = 0x8;
pub const CONTROL: usize = 0xA;

/// `DMAnCNT_H` bits (GBATEK, *40000BAh - DMA0CNT_H*).
///
/// Destination control, bits 5-6: `0` increment, `1` decrement, `2` fixed, `3` increment and reload
/// (the destination returns to its written value after each unit, which is the mode a repeating
/// block copy needs).
pub const DEST_MASK: u16 = 0b11 << 5;
/// Source control, bits 7-8: `0` increment, `1` decrement, `2` fixed. `3` is prohibited by GBATEK
/// and is treated as fixed here.
pub const SRC_MASK: u16 = 0b11 << 7;
/// Bit 9: repeat on the next trigger; only meaningful with V-Blank/H-Blank timing.
pub const REPEAT: u16 = 1 << 9;
/// Bit 10: 32-bit units instead of 16-bit ones.
pub const WIDTH32: u16 = 1 << 10;
/// Bit 11: Game Pak DRQ, DMA3 only (no cartridge here drives it).
pub const DRQ: u16 = 1 << 11;
/// Bits 12-13: what starts the transfer.
pub const TIMING_MASK: u16 = 0b11 << 12;
/// Bit 14: raise the channel's interrupt when the transfer ends.
pub const IRQ: u16 = 1 << 14;
/// Bit 15: enable.
pub const ENABLE: u16 = 1 << 15;
/// Every bit of the control word has a meaning on the GBA.
pub const CONTROL_MASK: u16 = 0xFFFF;

/// The address bits a 27-bit ("internal memory") register keeps.
pub const ADDRESS_MASK_INTERNAL: u32 = 0x07FF_FFFF;

/// The address bits a 28-bit ("any memory") register keeps.
pub const ADDRESS_MASK_ANY: u32 = 0x0FFF_FFFF;

/// The largest count a DMA0-2 count register holds (14 bits: "1..4000h").
pub const COUNT_MAX_012: u32 = 0x4000;

/// The largest count DMA3 holds (16 bits: "1..10000h").
pub const COUNT_MAX_3: u32 = 0x1_0000;

/// What starts a transfer, from `DMAnCNT_H` bits 12-13.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// `0`: the store that sets the enable bit runs it.
    Immediate,
    /// `1`: the display's V-Blank.
    VBlank,
    /// `2`: the display's H-Blank.
    HBlank,
    /// `3`: the channel's own special source — the sound FIFOs (DMA1/2) or video capture (DMA3).
    Special,
}

impl Timing {
    /// The timing `control` selects.
    pub fn from_control(control: u16) -> Timing {
        match (control & TIMING_MASK) >> 12 {
            0 => Timing::Immediate,
            1 => Timing::VBlank,
            2 => Timing::HBlank,
            _ => Timing::Special,
        }
    }
}

/// One channel's four registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Channel {
    /// `DMAnSAD`: where the transfer reads from.
    pub sad: u32,
    /// `DMAnDAD`: where it writes to.
    pub dad: u32,
    /// `DMAnCNT_L`: the transfer count as written; `0` means the channel's maximum.
    pub count: u16,
    /// `DMAnCNT_H`.
    pub control: u16,
    /// The internal source pointer the hardware walks. Not a register: GBATEK says the address
    /// registers keep their initial values, so the walking address lives here. It is reloaded from
    /// `sad` when the enable bit rises, and it keeps walking across a repeat (a repeating H-Blank
    /// channel streams through its source).
    pub cursor_sad: u32,
    /// The internal destination pointer, reloaded from `dad` on enable and, for a repeat in
    /// "increment and reload" mode, after each transfer.
    pub cursor_dad: u32,
}

impl Channel {
    /// True while the enable bit is set.
    pub fn enabled(&self) -> bool {
        self.control & ENABLE != 0
    }

    /// What starts this channel's transfer.
    pub fn timing(&self) -> Timing {
        Timing::from_control(self.control)
    }

    /// Bytes in one unit: 4 with bit 10 set, else 2.
    pub fn unit(&self) -> u32 {
        if self.control & WIDTH32 != 0 { 4 } else { 2 }
    }

    /// How many units the transfer moves, `index` being the channel: GBATEK's count is 14 bits on
    /// DMA0-2 and 16 on DMA3, and a written `0` means that maximum.
    pub fn units(&self, index: usize) -> u32 {
        let max = if index == CHANNELS - 1 {
            COUNT_MAX_3
        } else {
            COUNT_MAX_012
        };
        match u32::from(self.count) {
            0 => max,
            count => count.min(max),
        }
    }

    /// How far the source address moves after each unit.
    pub fn source_step(&self) -> u32 {
        match (self.control & SRC_MASK) >> 7 {
            0 => self.unit(),
            1 => self.unit().wrapping_neg(),
            _ => 0,
        }
    }

    /// How far the destination address moves after each unit.
    pub fn destination_step(&self) -> u32 {
        match (self.control & DEST_MASK) >> 5 {
            0 => self.unit(),
            1 => self.unit().wrapping_neg(),
            3 => self.unit(),
            _ => 0,
        }
    }

    /// True when the destination returns to its written value after each unit.
    pub fn destination_reloads(&self) -> bool {
        (self.control & DEST_MASK) >> 5 == 3
    }

    /// True when the transfer repeats on the next trigger.
    pub fn repeats(&self) -> bool {
        self.control & REPEAT != 0 && self.timing() != Timing::Immediate
    }

    /// True when the end of the transfer raises the channel's interrupt.
    pub fn wants_irq(&self) -> bool {
        self.control & IRQ != 0
    }
}

/// The four channels.
#[derive(Clone, Copy, Debug, Default)]
pub struct Dma {
    pub channels: [Channel; CHANNELS],
}

impl Dma {
    /// Four disabled channels with every register zero.
    pub fn new() -> Dma {
        Dma::default()
    }

    /// True for an I/O offset that belongs to a DMA channel.
    pub fn owns(offset: usize) -> bool {
        (BASE..BASE + CHANNELS * STRIDE).contains(&offset)
    }

    /// One channel.
    pub fn channel(&self, index: usize) -> Channel {
        self.channels[index]
    }

    /// The mask a channel's source address register keeps.
    pub fn source_mask(index: usize) -> u32 {
        if index == 0 {
            ADDRESS_MASK_INTERNAL
        } else {
            ADDRESS_MASK_ANY
        }
    }

    /// The mask a channel's destination address register keeps.
    pub fn destination_mask(index: usize) -> u32 {
        if index == CHANNELS - 1 {
            ADDRESS_MASK_ANY
        } else {
            ADDRESS_MASK_INTERNAL
        }
    }

    /// One byte of a channel register, `offset` being `$0B0`-`$0DF`'s offset in the I/O page.
    pub fn read8(&self, offset: usize) -> u8 {
        let index = (offset - BASE) / STRIDE;
        let within = offset - BASE - index * STRIDE;
        let channel = &self.channels[index];
        match within {
            SAD..=3 => (channel.sad >> (8 * within)) as u8,
            4..=7 => (channel.dad >> (8 * (within - 4))) as u8,
            8 => channel.count as u8,
            9 => (channel.count >> 8) as u8,
            10 => channel.control as u8,
            _ => (channel.control >> 8) as u8,
        }
    }

    /// One store to a channel register. Returns the channel whose enable bit this store set, which
    /// is what tells the bus to start a transfer — `None` for every other store.
    pub fn write8(&mut self, offset: usize, value: u8) -> Option<usize> {
        let index = (offset - BASE) / STRIDE;
        let within = offset - BASE - index * STRIDE;
        let channel = &mut self.channels[index];
        match within {
            SAD => channel.sad = (channel.sad & !0xFF) | u32::from(value),
            1 => channel.sad = (channel.sad & !0xFF00) | (u32::from(value) << 8),
            2 => channel.sad = (channel.sad & !0xFF_0000) | (u32::from(value) << 16),
            3 => channel.sad = (channel.sad & 0x00FF_FFFF) | (u32::from(value) << 24),
            x if (DAD..DAD + 4).contains(&x) => {
                let shift = 8 * (x - DAD);
                channel.dad = (channel.dad & !(0xFFu32 << shift)) | (u32::from(value) << shift);
            }
            COUNT => channel.count = (channel.count & 0xFF00) | u16::from(value),
            x if x == COUNT + 1 => {
                // DMA0-2's count is 14 bits: the top two bits of its high byte hold nothing.
                let mask = if index == CHANNELS - 1 { 0xFF } else { 0x3F };
                channel.count = (channel.count & 0x00FF) | (u16::from(value & mask) << 8);
            }
            x if x == CONTROL => channel.control = (channel.control & 0xFF00) | u16::from(value),
            _ => channel.control = (channel.control & 0x00FF) | (u16::from(value) << 8),
        }
        // GBATEK's 27/28-bit rule: a store cannot put an address past the channel's reach, and a
        // read afterwards returns what the channel will use, not what was written.
        channel.sad &= Self::source_mask(index);
        channel.dad &= Self::destination_mask(index);
        // The enable bit is the store that starts a transfer; the high byte is where it lives. A
        // rising enable reloads the internal pointers from the registers (GBATEK: "Upon DMA Enable
        // (Bit 15) changing from 0 to 1: Reloads SAD, DAD, CNT_L").
        let started = within == CONTROL + 1 && value & 0x80 != 0;
        if started {
            channel.cursor_sad = channel.sad;
            channel.cursor_dad = channel.dad;
        }
        started.then_some(index)
    }

    /// The internal pointers of the four channels, 8 bytes each — `cursor_sad` then `cursor_dad`,
    /// little endian. A state taken mid-transfer needs them: the address registers keep their
    /// written values, so without the cursors a resumed transfer would walk from the start again.
    pub fn encode(&self) -> [u8; CHANNELS * 8] {
        let mut bytes = [0u8; CHANNELS * 8];
        for (index, channel) in self.channels.iter().enumerate() {
            let at = index * 8;
            bytes[at..at + 4].copy_from_slice(&channel.cursor_sad.to_le_bytes());
            bytes[at + 4..at + 8].copy_from_slice(&channel.cursor_dad.to_le_bytes());
        }
        bytes
    }

    /// Restore [`Dma::encode`]'s body.
    pub fn decode(&mut self, bytes: &[u8; CHANNELS * 8]) {
        for (index, channel) in self.channels.iter_mut().enumerate() {
            let at = index * 8;
            channel.cursor_sad =
                u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            channel.cursor_dad =
                u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        }
    }

    /// Apply what the end of a transfer does to its registers and pointers.
    ///
    /// GBATEK, *DMA Repeat bit*: with repeat set "the Enable bit remains set after the transfer, and
    /// the transfer will be restarted each time when the Start condition becomes true"; with it
    /// clear "the Enable bit is automatically cleared". The address and count registers themselves
    /// are never touched — only the internal pointers, which are reloaded on enable and repeat.
    pub fn finish(&mut self, index: usize, original: Channel, sad: u32, dad: u32) {
        let channel = &mut self.channels[index];
        channel.cursor_sad = sad;
        if original.repeats() {
            // "Upon Repeat: Reloads CNT_L, and optionally DAD (Increment+Reload)".
            channel.cursor_dad = if original.destination_reloads() {
                original.dad
            } else {
                dad
            };
        } else {
            channel.cursor_dad = dad;
            channel.control &= !ENABLE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DMA register offset for `channel`'s register `within`.
    fn at(channel: usize, within: usize) -> usize {
        BASE + channel * STRIDE + within
    }

    /// Write a 32-bit register one byte at a time, as the bus does.
    fn put_word(dma: &mut Dma, offset: usize, value: u32) {
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            dma.write8(offset + index, byte);
        }
    }

    #[test]
    fn the_control_word_decodes_the_gba_bit_table() {
        // F-Zero's own handler copy: enable, 32-bit, immediate, count 15 (15 units).
        let mut dma = Dma::new();
        dma.write8(at(3, COUNT), 0x0F);
        dma.write8(at(3, CONTROL), 0x00);
        dma.write8(at(3, CONTROL + 1), 0x84);
        let channel = dma.channel(3);
        assert!(channel.enabled());
        assert_eq!(channel.timing(), Timing::Immediate);
        assert_eq!(channel.unit(), 4, "bit 10 is the unit size");
        assert_eq!(channel.units(3), 15, "the count is the number of units");
        assert!(!channel.repeats());

        // A V-Blank repeating block copy: repeat (bit 9), 16-bit units, dest fixed (bits 5-6 = 2),
        // start on V-Blank (bits 12-13 = 1), interrupt on end (bit 14), enable (bit 15).
        let mut dma = Dma::new();
        dma.write8(at(1, CONTROL), 0b0100_0000);
        dma.write8(at(1, CONTROL + 1), 0b1101_0010);
        let channel = dma.channel(1);
        assert_eq!(channel.timing(), Timing::VBlank);
        assert!(channel.repeats());
        assert_eq!(channel.unit(), 2);
        assert_eq!(channel.destination_step(), 0, "destination fixed");
        assert!(channel.wants_irq(), "bit 14");
    }

    #[test]
    fn a_count_of_zero_means_the_channels_maximum() {
        let mut dma = Dma::new();
        dma.write8(at(0, COUNT), 0);
        dma.write8(at(0, COUNT + 1), 0);
        assert_eq!(dma.channel(0).units(0), 0x4000, "DMA0-2 hold 14 bits");
        assert_eq!(dma.channel(3).units(3), 0x1_0000, "DMA3 holds 16");
        dma.write8(at(0, COUNT), 0xFF);
        dma.write8(at(0, COUNT + 1), 0xFF);
        assert_eq!(
            dma.channel(0).units(0),
            0x3FFF,
            "the two unused bits of DMA0-2's high byte do not store"
        );
        dma.write8(at(3, COUNT), 0x10);
        assert_eq!(dma.channel(3).units(3), 0x10);
        assert_eq!(dma.read8(at(3, COUNT)), 0x10);
    }

    #[test]
    fn the_address_registers_keep_the_bits_their_channel_can_reach() {
        let mut dma = Dma::new();
        put_word(&mut dma, at(0, SAD), 0x0800_0000);
        assert_eq!(
            dma.channel(0).sad,
            0,
            "DMA0's source is internal memory: 27 bits"
        );
        put_word(&mut dma, at(1, SAD), 0x0800_0000);
        assert_eq!(
            dma.channel(1).sad,
            0x0800_0000,
            "DMA1's source is any memory"
        );
        put_word(&mut dma, at(0, DAD), 0x0300_0000);
        assert_eq!(dma.channel(0).dad, 0x0300_0000);
        put_word(&mut dma, at(3, DAD), 0x0800_0000);
        assert_eq!(
            dma.channel(3).dad,
            0x0800_0000,
            "DMA3 can write the Game Pak"
        );
        put_word(&mut dma, at(3, DAD), 0x1000_0000);
        assert_eq!(dma.channel(3).dad, 0, "28 bits, not 32");
    }

    #[test]
    fn the_enable_store_is_the_one_that_reports_a_channel() {
        let mut dma = Dma::new();
        assert_eq!(dma.write8(at(2, CONTROL), 0x00), None);
        assert_eq!(dma.write8(at(2, CONTROL + 1), 0x40), None, "bit 15 clear");
        assert_eq!(
            dma.write8(at(2, CONTROL + 1), 0x80),
            Some(2),
            "the store that sets enable names its channel"
        );
        assert_eq!(dma.write8(at(2, SAD), 0x01), None);
    }

    #[test]
    fn the_address_steps_follow_the_control_bits() {
        let channel = Channel {
            control: ENABLE | WIDTH32,
            ..Channel::default()
        };
        assert_eq!(channel.unit(), 4);
        assert_eq!(channel.source_step(), 4);
        assert_eq!(channel.destination_step(), 4);

        // Decrement on both sides: bits 5-6 = 1 and bits 7-8 = 1.
        let channel = Channel {
            control: ENABLE | (0b01 << 5) | (0b01 << 7),
            ..Channel::default()
        };
        assert_eq!(channel.destination_step(), (-2i32) as u32);
        assert_eq!(channel.source_step(), (-2i32) as u32);

        // Fixed on both sides: bits 5-6 = 2 and bits 7-8 = 2.
        let fixed = Channel {
            control: ENABLE | (0b10 << 5) | (0b10 << 7),
            ..Channel::default()
        };
        assert_eq!(fixed.source_step(), 0);
        assert_eq!(fixed.destination_step(), 0);
    }

    #[test]
    fn a_one_shot_transfer_clears_its_enable_and_a_repeating_one_keeps_it() {
        let mut dma = Dma::new();
        dma.write8(at(1, CONTROL), 0x00);
        dma.write8(at(1, CONTROL + 1), 0x80);
        dma.write8(at(1, COUNT), 4);
        let original = dma.channel(1);
        dma.finish(1, original, 0x0300_0100, 0x0300_0108);
        assert!(!dma.channel(1).enabled(), "a one-shot ends disarmed");
        assert_eq!(
            dma.channel(1).count,
            4,
            "the count register keeps what was written: hardware does not change it"
        );
        assert_eq!(dma.channel(1).cursor_dad, 0x0300_0108);

        let mut dma = Dma::new();
        dma.write8(at(1, CONTROL), 0b0100_0000); // dest fixed
        dma.write8(at(1, CONTROL + 1), 0b1001_0010); // enable, V-Blank, repeat
        dma.write8(at(1, COUNT), 4);
        put_word(&mut dma, at(1, DAD), 0x0300_0000);
        let original = dma.channel(1);
        assert!(original.repeats());
        dma.finish(1, original, 0x0300_0010, 0x0300_0008);
        assert!(dma.channel(1).enabled(), "a repeating channel stays armed");
        assert_eq!(
            dma.channel(1).cursor_sad,
            0x0300_0010,
            "a repeat keeps walking through the source"
        );
        assert_eq!(
            dma.channel(1).cursor_dad,
            0x0300_0008,
            "a fixed destination is left where it was"
        );
    }

    #[test]
    fn the_four_timings_come_from_bits_twelve_and_thirteen() {
        for (bits, timing) in [
            (0b00, Timing::Immediate),
            (0b01, Timing::VBlank),
            (0b10, Timing::HBlank),
            (0b11, Timing::Special),
        ] {
            assert_eq!(Timing::from_control(bits << 12), timing);
        }
    }
}
