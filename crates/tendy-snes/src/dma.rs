//! The DMA and HDMA controller: eight channels, the transfer-unit table and the per-line HDMA tick
//! Sources: fullsnes "SNES DMA Transfers", "SNES DMA and HDMA Start/Enable Registers",
//! "SNES DMA and HDMA Channel 0..7 Registers", "SNES DMA and HDMA Notes" and "HDMA Table Formats".
//!
//! The controller is the bus master while it runs: it reaches the machine only through [`BusPort`],
//! reads with no CPU cost and charges eight master clocks for every byte it writes (`Bus::write_bbus`
//! implementation, §6.2), which is what keeps one ticking rule true with a second master (§3).

use crate::addr::Addr;
use crate::state::{StateError, StateReader, StateWriter};

/// One channel at `$4300 + n*0x10`.
#[derive(Clone, Default)]
pub struct Channel {
    pub dmap: u8,
    pub bbad: u8,
    /// DMA source, or the HDMA table's start.
    pub a1t: Addr,
    /// DMA byte counter.
    pub das: u16,
    /// HDMA indirect data bank.
    pub dasb: u8,
    /// HDMA table's current address.
    pub a2a: u16,
    /// HDMA line counter and repeat flag.
    pub ntlt: u8,
    /// `$43xB` / `$43xF`, readable/writable storage.
    pub unused: u8,
    /// HDMA only: units the current table entry still owes.
    pub remaining: u8,
    pub indirect: Addr,
    /// A `00h` entry ended this channel until the next frame.
    pub line_end: bool,
}

/// The controller: eight channels and the two enable masks.
///
/// `Default` is required by `Bus::hdma_line`, which moves the controller out for the duration of
/// the tick.
#[derive(Clone, Default)]
pub struct Dma {
    pub channels: [Channel; 8],
    pub mdmaen: u8,
    pub hdmaen: u8,
}

impl Dma {
    pub fn new() -> Dma {
        Dma::default()
    }

    /// `$420B`: runs every enabled channel in order 0-7, to completion, charging 8 clocks a byte.
    ///
    /// fullsnes "SNES DMA and HDMA Start/Enable Registers": "general purpose DMA will be started
    /// immediately ... The CPU is paused during the transfer"; "The MDMAEN bits are cleared
    /// automatically at transfer completion".
    pub fn run_gpdma(&mut self, mask: u8, bus: &mut dyn BusPort) {
        for channel in 0..8 {
            if mask & (1 << channel) == 0 {
                continue;
            }
            self.run_channel(channel, bus);
            self.mdmaen &= !(1 << channel);
        }
    }

    /// One channel's whole transfer. The counter is a **byte** counter, so a unit that would run
    /// past it moves only the bytes that remain (fullsnes: "len=5 would transfer one whole Unit,
    /// plus the first byte of the second Unit").
    fn run_channel(&mut self, channel: usize, bus: &mut dyn BusPort) {
        let c = &mut self.channels[channel];
        let unit = Self::unit_addresses(c.dmap & 0x07);
        // fullsnes `DMAPn`: "(0=Increment, 2=Decrement, 1/3=Fixed)" on bits 4-3.
        let step = match (c.dmap >> 3) & 0x03 {
            0 => 1i32,
            2 => -1,
            _ => 0,
        };
        // fullsnes "SNES DMA and HDMA Notes": a WRAM-to-WRAM transfer does nothing, because the
        // chip "is unable to process both at once".
        let wram_port = c.bbad == 0x80;
        if wram_port && matches!(c.a1t.bank(), 0x7E | 0x7F) {
            return;
        }
        let mut count = u32::from(c.das);
        if count == 0 {
            count = 0x1_0000;
        }
        let mut src = c.a1t;
        bus.begin_dma();
        while count > 0 {
            for &dest in unit {
                if count == 0 {
                    break;
                }
                let value = bus.read_dma(src);
                bus.write_bbus(c.bbad.wrapping_add(dest), value);
                src = Addr::new(src.bank(), src.offset().wrapping_add(step as u16));
                count -= 1;
            }
        }
        c.a1t = src;
        c.das = 0;
    }

    /// H=278 of lines 0-224: one unit per enabled channel.
    ///
    /// The table entry is fetched only when the previous entry's line count has run out; the `00h`
    /// entry ends the channel for the rest of the frame, and `81h..FFh` moves one unit on each of
    /// `X-80h` consecutive lines.
    pub fn hdma_line(&mut self, bus: &mut dyn BusPort) {
        for channel in 0..8 {
            if self.hdmaen & (1 << channel) == 0 {
                continue;
            }
            let c = &mut self.channels[channel];
            if c.line_end {
                continue;
            }
            let unit = Self::unit_addresses(c.dmap & 0x07);
            if c.ntlt & 0x7F == 0 {
                let entry = bus.read(Addr::new(c.a1t.bank(), c.a2a));
                c.a2a = c.a2a.wrapping_add(1);
                c.ntlt = entry;
                if entry == 0 {
                    c.line_end = true;
                    continue;
                }
                c.remaining = if entry & 0x80 != 0 { entry & 0x7F } else { 1 };
                if c.dmap & 0x40 != 0 {
                    let lo = bus.read(Addr::new(c.a1t.bank(), c.a2a));
                    let hi = bus.read(Addr::new(c.a1t.bank(), c.a2a.wrapping_add(1)));
                    c.a2a = c.a2a.wrapping_add(2);
                    // A table in `$7E`/`$7F` takes the data bank from the table itself and ignores
                    // `DASB` — the workaround games use for a WRAM table (§6.5's first row).
                    let bank = match c.a1t.bank() {
                        0x7E | 0x7F => c.a1t.bank(),
                        _ => c.dasb,
                    };
                    c.indirect = Addr::new(bank, u16::from_le_bytes([lo, hi]));
                }
            }
            if c.remaining > 0 {
                let mut src = if c.dmap & 0x40 != 0 {
                    c.indirect
                } else {
                    Addr::new(c.a1t.bank(), c.a2a)
                };
                for &dest in unit {
                    let value = bus.read(src);
                    bus.write_bbus(c.bbad.wrapping_add(dest), value);
                    src = src.next();
                }
                let step = unit.len() as u16;
                if c.dmap & 0x40 != 0 {
                    c.indirect =
                        Addr::new(c.indirect.bank(), c.indirect.offset().wrapping_add(step));
                } else {
                    c.a2a = c.a2a.wrapping_add(step);
                }
                c.remaining -= 1;
            }
            c.ntlt = c.ntlt.wrapping_sub(1) & 0x7F;
        }
    }

    /// V=0: reload every HDMA channel's table pointer from `A1T` and clear its counter, so the
    /// per-frame stepping restarts inside v-blank (fullsnes: "H=6, V=0 reload HDMA registers").
    pub fn begin_frame(&mut self) {
        for c in &mut self.channels {
            c.a2a = c.a1t.offset();
            c.ntlt = 0;
            c.remaining = 0;
            c.line_end = false;
        }
    }

    /// One slice of the master clock. Not in §6.4's list: the fan-out of §3.3 calls it. HDMA runs
    /// from [`Dma::hdma_line`] and the refresh stall is consumed by the CPU's own access, so there
    /// is nothing left for the controller to count.
    pub fn tick(&mut self, _ticks: u32) {}

    /// The B-bus offsets one unit of `mode` writes.
    pub fn unit_addresses(mode: u8) -> &'static [u8] {
        match mode {
            0 => &[0],
            1 => &[0, 1],
            2 => &[0, 0],
            3 => &[0, 0, 1, 1],
            4 => &[0, 1, 2, 3],
            5 => &[0, 1, 0, 1],
            6 => &[0, 0],
            7 => &[0, 0, 1, 1],
            _ => &[],
        }
    }

    /// How many bytes one unit of `mode` moves.
    pub fn unit_size(mode: u8) -> usize {
        Dma::unit_addresses(mode).len()
    }

    /// §9.2's `Dma` rows, in the design's order: the two masks, then eight channels.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.mdmaen);
        w.u8(self.hdmaen);
        for c in &self.channels {
            w.u8(c.dmap);
            w.u8(c.bbad);
            w.u8(c.dasb);
            w.u8(c.ntlt);
            w.u8(c.unused);
            w.u8(c.remaining);
        }
        for c in &self.channels {
            w.u32(c.a1t.0);
            w.u16(c.das);
            w.u16(c.a2a);
            w.u32(c.indirect.0);
            w.bool(c.line_end);
        }
    }

    /// The inverse of [`Dma::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        self.mdmaen = r.u8()?;
        self.hdmaen = r.u8()?;
        for c in &mut self.channels {
            c.dmap = r.u8()?;
            c.bbad = r.u8()?;
            c.dasb = r.u8()?;
            c.ntlt = r.u8()?;
            c.unused = r.u8()?;
            c.remaining = r.u8()?;
        }
        for c in &mut self.channels {
            c.a1t = Addr(r.u32()?);
            c.das = r.u16()?;
            c.a2a = r.u16()?;
            c.indirect = Addr(r.u32()?);
            c.line_end = r.bool()?;
        }
        Ok(())
    }
}

/// What a transfer needs from the bus, and nothing else. Implemented for `Bus`, so `Dma` never
/// names `Bus` and never borrows it for longer than one byte.
pub trait BusPort {
    /// The decode with no CPU cost: the controller is the bus master here.
    fn read(&mut self, addr: Addr) -> u8;
    /// One A-bus byte for a **general-purpose** DMA. The DMA's own open bus, not the S-CPU's last
    /// byte (fullsnes "Open Bus for DMA"); a bus that models only the decode keeps `read`'s answer.
    fn read_dma(&mut self, addr: Addr) -> u8 {
        self.read(addr)
    }
    /// The start of one general-purpose channel: the hook that resets a DMA open-bus latch.
    fn begin_dma(&mut self) {}
    /// Write to a B-bus port (`$2100 + dest`) and charge the byte's 8 master clocks.
    fn write_bbus(&mut self, dest: u8, value: u8);
}
