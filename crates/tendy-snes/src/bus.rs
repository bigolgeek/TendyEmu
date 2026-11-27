//! The 24-bit bus: the decode, the access costs, the line-event fan-out and the data bus's last
//! driven value.
//!
//! Sources: fullsnes "SNES Memory Map", "SNES Memory Control", "SNES Memory Work RAM Access", "SNES
//! I/O Map", "SNES CPU Clock Cycles", "SNES PPU Interrupts", "SNES Timing H/V Counters" and "SNES
//! DMA and HDMA Channel 0..7 Registers"; SNESdev `Timing` and `Memory map`.
//!
//! Three things live here and nowhere else:
//!
//! - **The decode.** One `match` on the bank, one on the system-area offset (§4.1, §4.2).
//! - **The cost.** [`Bus::access_ticks`] is §3.2's table, `MEMSEL` included.
//! - **The clock.** [`Bus::tick`] is the only place a device advances (§3.3), and
//!   [`Bus::on_line_events`] is what it does with the PPU's line events.
//!
//! The H/V-IRQ comparison is also here, because it is the one interrupt whose inputs are split
//! between devices: the counters are the PPU's and `HTIME`/`VTIME` are the S-CPU's (§3.7, §3.8).

use crate::addr::Addr;
use crate::apu::Apu;
use crate::cart::Cartridge;
use crate::dma::{BusPort, Dma};
use crate::io::CpuIo;
use crate::joypad::Joypad;
use crate::ppu::{LineEvents, Ppu};
use crate::{
    DOTS_PER_LINE, FAST_ACCESS, JOYPAD_ACCESS, LINES_PER_FRAME, REFRESH_TICKS, SLOW_ACCESS,
    VBLANK_START, WRAM_SIZE,
};

/// How many writes the diagnosis ring holds. A frame of ordinary code makes a few hundred (a stack
/// push is one, a direct-page `STA` is one), so this covers hundreds of frames; a run that fills it
/// keeps the newest, and a harness that needs an exact count drains it at every instruction.
pub const WRITE_LOG_LEN: usize = 65_536;

/// Where a logged write landed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WriteSpace {
    /// `$7E`/`$7F:0000-FFFF`: the 128 KiB of work RAM addressed by its own bank.
    #[default]
    Wram,
    /// The same array through the `$00-$3F`/`$80-$BF:0000-1FFF` mirror. Every direct-page store
    /// lands here (direct-page addressing is bank 0), and so does every stack push, so this arm is
    /// what makes "nothing wrote the cartridge's flag" a statement about the flag rather than about
    /// one of the two ways of naming it.
    Mirror,
    /// `$2180`, the work-RAM data port. Recorded at the byte the port's address counter pointed at,
    /// so a stream written through the port lines up with the direct stores to the same byte.
    Port,
    /// `$4200-$420D`, the S-CPU's own registers (fullsnes "SNES I/O Map").
    Io,
}

impl WriteSpace {
    /// The name the diagnosis harness prints.
    pub fn name(self) -> &'static str {
        match self {
            WriteSpace::Wram => "WRAM",
            WriteSpace::Mirror => "MIRR",
            WriteSpace::Port => "PORT",
            WriteSpace::Io => "IO",
        }
    }
}

/// One write [`Bus::write`] served into work RAM or into `$4200-$420D`.
///
/// Recorded only while [`Bus::enable_write_log`] has been called (a diagnosis harness, never the
/// frame path). `pb:pc` is the instruction that made the write, stamped by [`crate::cpu::Cpu::step`]
/// once per instruction; for a write a DMA made it is the instruction that started the transfer,
/// which is the only S-CPU code there is for it.
#[derive(Clone, Copy, Default)]
pub struct WriteRecord {
    pub space: WriteSpace,
    /// Stamped by the harness that owns the ring; the bus does not know what a frame is.
    pub frame: u32,
    pub pb: u8,
    pub pc: u16,
    /// The address the bus saw (for a [`WriteSpace::Port`] write, the work-RAM address the port's
    /// counter pointed at).
    pub bank: u8,
    pub offset: u16,
    pub value: u8,
    /// 1, or 2 for the low byte of a store the CPU made 16 bits wide: the two bytes of a `STA` that
    /// moved a word are one record each and both say 2.
    pub width: u8,
}

impl WriteRecord {
    /// The work-RAM byte this record wrote, as an index into the 128 KiB array, for a record that
    /// wrote work RAM at all. `$7E:0000` and `$00:0000` are the same byte and answer the same
    /// number, which is the whole point of recording both.
    pub fn wram_linear(self) -> Option<u32> {
        match self.space {
            WriteSpace::Wram | WriteSpace::Port => {
                Some(if self.bank == 0x7F { 0x1_0000 } else { 0 } + u32::from(self.offset))
            }
            WriteSpace::Mirror => Some(u32::from(self.offset)),
            WriteSpace::Io => None,
        }
    }
}

/// How many reads the diagnosis ring holds. One instruction fetches one to four bytes and reads at
/// most three operands, so this is a few thousand instructions; a harness that needs an exact
/// sequence drains it at every instruction and keeps the newest when a run outruns it.
pub const READ_LOG_LEN: usize = 65_536;

/// Where a logged read came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReadSpace {
    /// `$7E`/`$7F:0000-FFFF`: the 128 KiB of work RAM addressed by its own bank.
    #[default]
    Wram,
    /// The same array through the `$00-$3F`/`$80-$BF:0000-1FFF` mirror. Every direct-page read lands
    /// here (direct-page addressing is bank 0), including both bytes of a `(dp)`/`(dp),Y` pointer.
    Mirror,
    /// Cartridge ROM, by whichever window the board maps it into.
    Rom,
    /// Cartridge SRAM, by whichever window the board maps it into.
    Sram,
    /// A system-area register: `$2100-$21FF` (the PPU block, the APU ports, the `$2180` port),
    /// `$4000-$41FF` (the joypad), `$4200-$43FF` (the S-CPU's own registers and the DMA channels).
    Reg,
    /// An address nothing drives, so the value is whatever the data bus last carried (`Bus::mdr`).
    Open,
}

impl ReadSpace {
    /// The name the diagnosis harness prints.
    pub fn name(self) -> &'static str {
        match self {
            ReadSpace::Wram => "WRAM",
            ReadSpace::Mirror => "MIRR",
            ReadSpace::Rom => "ROM",
            ReadSpace::Sram => "SRAM",
            ReadSpace::Reg => "REG",
            ReadSpace::Open => "OPEN",
        }
    }
}

/// One read [`Bus::read`] served: the value the machine handed the cartridge, and where it came
/// from.
///
/// Recorded only while [`Bus::enable_read_log`] has been called (a diagnosis harness, never the
/// frame path). `pb:pc` is the instruction that made the read, stamped by [`crate::cpu::Cpu::step`]
/// once per instruction. Instruction fetches are reads too and are recorded: the fetch of the
/// opcode at `pb:pc` is the first record of every instruction, which is what makes "who read what,
/// in what order, around one instruction" a sequence rather than a list.
#[derive(Clone, Copy, Default)]
pub struct ReadRecord {
    pub space: ReadSpace,
    /// Stamped by the harness that owns the ring; the bus does not know what a frame is.
    pub frame: u32,
    pub pb: u8,
    pub pc: u16,
    /// The address the bus saw: the bank and offset of the read itself, before any window the board
    /// maps it through. `$C3:0920` on a LoROM board and `$00:8920` are the same ROM byte and two
    /// different records, because the cartridge asked for two different addresses.
    pub bank: u8,
    pub offset: u16,
    pub value: u8,
    /// 1, or 2 for the low byte of an operand the CPU read 16 bits wide (`LDA $0000,X`,
    /// `LDA $1234`): the two bytes of one read are one record each and both say 2. A pointer that
    /// the CPU assembles from two 8-bit reads (`(dp)`, `(dp),Y`) is two records of width 1, because
    /// that is the shape of the access.
    pub width: u8,
}

impl ReadRecord {
    /// The work-RAM byte this record read, as an index into the 128 KiB array. `$7E:0000` and
    /// `$00:0000` are the same byte and answer the same number.
    pub fn wram_linear(self) -> Option<u32> {
        match self.space {
            ReadSpace::Wram => {
                Some(if self.bank == 0x7F { 0x1_0000 } else { 0 } + u32::from(self.offset))
            }
            ReadSpace::Mirror => Some(u32::from(self.offset)),
            _ => None,
        }
    }
}

/// Every device, the clock and the data bus's last driven value.
#[derive(Clone)]
pub struct Bus {
    pub cart: Cartridge,
    /// 128 KiB, mirrored at `$00-$3F`/`$80-$BF`:0000-1FFF.
    pub wram: Box<[u8; WRAM_SIZE]>,
    pub ppu: Ppu,
    pub apu: Apu,
    pub dma: Dma,
    pub joypad: Joypad,
    /// `$4200-$421F`.
    pub io: CpuIo,
    /// Master clocks since reset, never wrapped.
    pub master: u64,
    /// The last value driven on the data bus.
    ///
    /// fullsnes "Open Bus": "The received data is the value being previously output on the data
    /// bus. In most cases, that is the last byte of the opcode (direct reads), or the upper byte of
    /// indirect address (indirect reads)". A fetch is a read and an operand is a read, so
    /// [`Bus::read`] loads this after every access, and a store drives the bus too, so
    /// [`Bus::write`] loads it as well. A DMA is the one exception: it is a second master and does
    /// not see the S-CPU's latch ([`Bus::dma_read`]).
    pub mdr: u8,
    /// The byte a general-purpose DMA last read from a driven A-bus address (`Bus::dma_read`).
    pub dma_open: u8,
    /// True while the first undriven address after a driven one still owes that byte.
    pub dma_open_latch: bool,
    /// The S-CPU's internal NMI flag.
    pub nmi_latch: bool,
    /// Master clocks the CPU still has to wait: the DRAM-refresh pause.
    pub stall: u32,
    /// True while a general-purpose DMA transfer has the CPU paused.
    pub dma_active: bool,
    /// The diagnosis write ring (§ design: off by default). `None` until a harness calls
    /// [`Bus::enable_write_log`]; `Bus::write` checks it once per access, so a machine nobody
    /// enabled is unchanged.
    pub write_log: Option<Box<[WriteRecord]>>,
    /// Writes recorded since `enable_write_log`, including any the ring has overwritten.
    pub write_log_len: u64,
    /// The diagnosis read ring (§ design: off by default). `None` until a harness calls
    /// [`Bus::enable_read_log`]; `Bus::read` checks it once per access, so a machine nobody enabled
    /// is unchanged.
    pub read_log: Option<Box<[ReadRecord]>>,
    /// Reads recorded since `enable_read_log`, including any the ring has overwritten.
    pub read_log_len: u64,
    /// The instruction the next read or write belongs to, stamped by [`crate::cpu::Cpu::step`] —
    /// the bus has no CPU and cannot see `PB:PC` itself.
    pub writer: (u8, u16),
    /// The width of the store about to happen, set by `Cpu::write_n` and reset by `Bus::write`.
    pub write_width: u8,
    /// The width of the operand read about to happen, set by `Cpu::read_n` and reset by `Bus::read`.
    pub read_width: u8,
}

impl Bus {
    /// A bus with every device at its power-on value.
    pub fn new(cart: Cartridge) -> Bus {
        Bus {
            cart,
            wram: Box::new([0; WRAM_SIZE]),
            ppu: Ppu::new(),
            apu: Apu::new(crate::apu::Host::Snes),
            dma: Dma::new(),
            joypad: Joypad::new(),
            io: CpuIo::default(),
            master: 0,
            mdr: 0,
            dma_open: 0xFF,
            dma_open_latch: false,
            nmi_latch: false,
            stall: 0,
            dma_active: false,
            write_log: None,
            write_log_len: 0,
            read_log: None,
            read_log_len: 0,
            writer: (0, 0),
            write_width: 1,
            read_width: 1,
        }
    }

    /// Serves the access, then advances the clock by its cost plus any stall.
    pub fn read(&mut self, addr: Addr) -> u8 {
        let value = self.peek(addr);
        self.mdr = value;
        if self.read_log.is_some() {
            self.record_read(addr, value);
        }
        self.read_width = 1;
        self.finish_access(addr);
        value
    }

    /// Serves the write at the start of the access, then advances the clock.
    pub fn write(&mut self, addr: Addr, value: u8) {
        self.mdr = value;
        if self.write_log.is_some() {
            self.record_write(addr, value);
        }
        self.write_width = 1;
        let bank = addr.bank();
        match bank {
            0x7E | 0x7F => {
                let i = addr.linear() & (WRAM_SIZE - 1);
                self.wram[i] = value;
            }
            _ if bank < 0x40 || (0x80..0xC0).contains(&bank) => {
                self.write_system_or_rom(addr, value);
            }
            _ => self.cart.write(addr, value),
        }
        self.finish_access(addr);
    }

    /// The same decode with **no** CPU cost and no stall: DMA, HDMA and tests use this.
    pub fn peek(&mut self, addr: Addr) -> u8 {
        let bank = addr.bank();
        match bank {
            0x7E | 0x7F => self.wram[addr.linear() & (WRAM_SIZE - 1)],
            _ if bank < 0x40 || (0x80..0xC0).contains(&bank) => self.read_system_or_rom(addr),
            _ => self.cart.read(addr),
        }
    }

    /// The decode with no cost, for a write.
    pub fn poke(&mut self, addr: Addr, value: u8) {
        let bank = addr.bank();
        match bank {
            0x7E | 0x7F => {
                let i = addr.linear() & (WRAM_SIZE - 1);
                self.wram[i] = value;
            }
            _ if bank < 0x40 || (0x80..0xC0).contains(&bank) => {
                self.write_system_or_rom(addr, value);
            }
            _ => self.cart.write(addr, value),
        }
    }

    /// The start of one general-purpose DMA channel's transfer.
    ///
    /// fullsnes "Open Bus for DMA": "DMA does always read FFh (High-Z) as open bus value", so the
    /// latch starts at `$FF` for every channel: one transfer's last driven byte is not the next
    /// channel's open bus.
    pub fn begin_dma_transfer(&mut self) {
        self.dma_open = 0xFF;
        self.dma_open_latch = false;
    }

    /// One A-bus byte served to a general-purpose DMA.
    ///
    /// Not [`Bus::read`]: the DMA is a second bus master, the S-CPU is paused while it runs, and
    /// the CPU's [`Bus::mdr`] is *not* the value on the bus. fullsnes "Open Bus for DMA": "For DMA
    /// reads, one may expect the same garbage as for CPU reads (ie. when starting a DMA via 'MOV
    /// [420Bh],A', one would expect 42h as open bus value). However, it takes a few cycles before
    /// the DMA transfer does actually start, during that time the hardware 'forgets' the 42h value,
    /// and instread, DMA does always read FFh (High-Z) as open bus value." The documented exception
    /// is the wrap: "If DMA wraps from an used to unused address (eg. from 1FFFh/WRAM to
    /// 2000h/unused) then the first 'unused' byte will be same as the last 'used' byte, thereafter,
    /// it forgets that value (and returns FFh on further unused addresses)."
    pub fn dma_read(&mut self, addr: Addr) -> u8 {
        if self.dma_a_bus_unused(addr) {
            if self.dma_open_latch {
                self.dma_open_latch = false;
                self.dma_open
            } else {
                0xFF
            }
        } else {
            let value = self.peek(addr);
            self.dma_open = value;
            self.dma_open_latch = true;
            value
        }
    }

    /// Whether a general-purpose DMA's A-bus read of `addr` is an address nothing drives.
    ///
    /// fullsnes "Open Bus for DMA": "2100h-21FFh Open Bus (when used as A-Bus) (of course they work
    /// as B-Bus)", "4000h-41FFh Open Bus (name 4016h/4017h cannot be read)", "4210h-421Fh These do
    /// work (the only I/O ports that are not open bus)" and "4300h-437Fh Special Open Bus (DMA
    /// registers, may return [PC] instead of FFh)". The rest is §4.2's unused map. `$6000-$7FFF` is
    /// left to the cartridge (the unpopulated-socket row), and `$8000-$FFFF` is ROM.
    fn dma_a_bus_unused(&self, addr: Addr) -> bool {
        let bank = addr.bank();
        if bank == 0x7E || bank == 0x7F {
            return false;
        }
        if !(bank < 0x40 || (0x80..0xC0).contains(&bank)) {
            return false;
        }
        matches!(
            addr.offset(),
            0x2000..=0x20FF
                | 0x2100..=0x21FF
                | 0x2200..=0x3FFF
                | 0x4000..=0x41FF
                | 0x4200..=0x420F
                | 0x4220..=0x42FF
                | 0x4300..=0x437F
                | 0x4380..=0x5FFF
        )
    }

    // ---- the write log (diagnosis only) ---------------------------------------------------------

    /// Turns the write ring on. Allocates once, here, and never on the frame path; a harness calls
    /// this on a machine it has just built. Off by default so no cartridge can observe it, and not
    /// serialised: it is a run's measurement, not a state.
    pub fn enable_write_log(&mut self) {
        if self.write_log.is_none() {
            self.write_log = Some(vec![WriteRecord::default(); WRITE_LOG_LEN].into_boxed_slice());
        }
        self.write_log_len = 0;
    }

    /// Turns the ring off and releases it.
    pub fn disable_write_log(&mut self) {
        self.write_log = None;
        self.write_log_len = 0;
    }

    /// Writes recorded since [`Bus::enable_write_log`], including ones the ring has overwritten.
    pub fn write_log_len(&self) -> u64 {
        self.write_log_len
    }

    /// The write with absolute index `n`, or `None` once the ring has overwritten it.
    pub fn write_event(&self, n: u64) -> Option<&WriteRecord> {
        let ring = self.write_log.as_ref()?;
        if n >= self.write_log_len || self.write_log_len - n > WRITE_LOG_LEN as u64 {
            return None;
        }
        Some(&ring[(n as usize) & (WRITE_LOG_LEN - 1)])
    }

    /// The same, mutably, so the harness can stamp the frame it belongs to.
    pub fn write_event_mut(&mut self, n: u64) -> Option<&mut WriteRecord> {
        let len = self.write_log_len;
        let ring = self.write_log.as_mut()?;
        if n >= len || len - n > WRITE_LOG_LEN as u64 {
            return None;
        }
        Some(&mut ring[(n as usize) & (WRITE_LOG_LEN - 1)])
    }

    /// Classifies one `Bus::write`. Only work RAM (by either of its two names), the `$2180` port
    /// and `$4200-$420D` are recorded; everything else is a different instrument's business.
    fn record_write(&mut self, addr: Addr, value: u8) {
        let bank = addr.bank();
        let offset = addr.offset();
        let system = bank < 0x40 || (0x80..0xC0).contains(&bank);
        let space = match bank {
            0x7E | 0x7F => WriteSpace::Wram,
            _ if system && offset < 0x2000 => WriteSpace::Mirror,
            _ if system && (0x4200..=0x420D).contains(&offset) => WriteSpace::Io,
            _ => return,
        };
        let (pb, pc) = self.writer;
        self.push_write(WriteRecord {
            space,
            frame: 0,
            pb,
            pc,
            bank,
            offset,
            value,
            width: self.write_width,
        });
    }

    /// `$2180`: a write through the work-RAM data port, recorded at the byte its counter pointed
    /// at. Called from [`Bus::write_b_bus`] before the counter moves, so a `$2180` stream and a
    /// direct store to the same byte cannot be told apart by address — which is the point.
    fn record_wram_port(&mut self, value: u8) {
        if self.write_log.is_none() {
            return;
        }
        let linear = self.io.wmadd & (WRAM_SIZE as u32 - 1);
        let bank = if linear < 0x1_0000 { 0x7E } else { 0x7F };
        let (pb, pc) = self.writer;
        self.push_write(WriteRecord {
            space: WriteSpace::Port,
            frame: 0,
            pb,
            pc,
            bank,
            offset: linear as u16,
            value,
            width: 1,
        });
    }

    fn push_write(&mut self, record: WriteRecord) {
        if let Some(ring) = self.write_log.as_mut() {
            let slot = (self.write_log_len as usize) & (WRITE_LOG_LEN - 1);
            ring[slot] = record;
            self.write_log_len += 1;
        }
    }

    // ---- the read log (diagnosis only) ----------------------------------------------------------

    /// Turns the read ring on. Allocates once, here, and never on the frame path; a harness calls
    /// this on a machine it has just built. Off by default so no cartridge can observe it, and not
    /// serialised: it is a run's measurement, not a state.
    pub fn enable_read_log(&mut self) {
        if self.read_log.is_none() {
            self.read_log = Some(vec![ReadRecord::default(); READ_LOG_LEN].into_boxed_slice());
        }
        self.read_log_len = 0;
    }

    /// Turns the ring off and releases it.
    pub fn disable_read_log(&mut self) {
        self.read_log = None;
        self.read_log_len = 0;
    }

    /// Reads recorded since [`Bus::enable_read_log`], including ones the ring has overwritten.
    pub fn read_log_len(&self) -> u64 {
        self.read_log_len
    }

    /// The read with absolute index `n`, or `None` once the ring has overwritten it.
    pub fn read_event(&self, n: u64) -> Option<&ReadRecord> {
        let ring = self.read_log.as_ref()?;
        if n >= self.read_log_len || self.read_log_len - n > READ_LOG_LEN as u64 {
            return None;
        }
        Some(&ring[(n as usize) & (READ_LOG_LEN - 1)])
    }

    /// The same, mutably, so the harness can stamp the frame it belongs to.
    pub fn read_event_mut(&mut self, n: u64) -> Option<&mut ReadRecord> {
        let len = self.read_log_len;
        let ring = self.read_log.as_mut()?;
        if n >= len || len - n > READ_LOG_LEN as u64 {
            return None;
        }
        Some(&mut ring[(n as usize) & (READ_LOG_LEN - 1)])
    }

    /// Classifies one `Bus::read` by where the decode sent it. The arms are [`Bus::peek`]'s, in its
    /// order, so a record cannot disagree with the decode that served the value.
    fn record_read(&mut self, addr: Addr, value: u8) {
        let space = self.read_space(addr);
        let (pb, pc) = self.writer;
        let record = ReadRecord {
            space,
            frame: 0,
            pb,
            pc,
            bank: addr.bank(),
            offset: addr.offset(),
            value,
            width: self.read_width,
        };
        if let Some(ring) = self.read_log.as_mut() {
            let slot = (self.read_log_len as usize) & (READ_LOG_LEN - 1);
            ring[slot] = record;
            self.read_log_len += 1;
        }
    }

    fn read_space(&self, addr: Addr) -> ReadSpace {
        let bank = addr.bank();
        let offset = addr.offset();
        if bank == 0x7E || bank == 0x7F {
            return ReadSpace::Wram;
        }
        if bank < 0x40 || (0x80..0xC0).contains(&bank) {
            // `Bus::read_system_or_rom`'s arms, one for one.
            match offset {
                0x0000..=0x1FFF => return ReadSpace::Mirror,
                0x2000..=0x20FF => return ReadSpace::Open,
                0x2100..=0x21FF => return ReadSpace::Reg,
                0x2200..=0x3FFF => return ReadSpace::Open,
                0x4000..=0x421F => return ReadSpace::Reg,
                0x4220..=0x42FF => return ReadSpace::Open,
                0x4300..=0x437F => return ReadSpace::Reg,
                0x4380..=0x43FF => return ReadSpace::Open,
                0x4400..=0x5FFF => return ReadSpace::Open,
                _ => {}
            }
        }
        self.cart_space(addr)
    }

    /// ROM, SRAM, or the open bus of an SRAM window that has no chip behind it.
    fn cart_space(&self, addr: Addr) -> ReadSpace {
        if self
            .cart
            .board
            .ram_offset(addr, self.cart.ram.len())
            .is_some()
        {
            if self.cart.ram.is_empty() {
                ReadSpace::Open
            } else {
                ReadSpace::Sram
            }
        } else {
            ReadSpace::Rom
        }
    }

    /// 6, 8 or 12 master clocks for this address, from the region and `MEMSEL` (§3.2).
    ///
    /// The two switchable rows are `$80-$BF:8000-FFFF` and `$C0-$FF:0000-FFFF`, both 6 or 8 by
    /// `MEMSEL` bit 0 (fullsnes "SNES Memory Control": "Access Cycle for Memory-2 Area
    /// (0=2.68MHz, 1=3.58MHz)").
    pub fn access_ticks(&self, addr: Addr) -> u32 {
        let bank = addr.bank();
        let offset = addr.offset();
        match bank {
            0x7E | 0x7F => SLOW_ACCESS,
            0x40..=0x7D => SLOW_ACCESS,
            0x00..=0x3F | 0x80..=0xBF => match offset {
                0x0000..=0x1FFF => SLOW_ACCESS,
                0x2000..=0x3FFF => FAST_ACCESS,
                0x4000..=0x41FF => JOYPAD_ACCESS,
                0x4200..=0x5FFF => FAST_ACCESS,
                0x6000..=0x7FFF => SLOW_ACCESS,
                // $8000-$FFFF: WS1 is fixed at 2.68 MHz in $00-$3F, WS2 switchable in $80-$BF.
                _ => {
                    if bank < 0x40 {
                        SLOW_ACCESS
                    } else {
                        self.rom_speed()
                    }
                }
            },
            _ => self.rom_speed(),
        }
    }

    /// The switchable ROM speed: 6 or 8 clocks by `MEMSEL` bit 0.
    fn rom_speed(&self) -> u32 {
        if self.io.memsel & 0x01 != 0 {
            FAST_ACCESS
        } else {
            SLOW_ACCESS
        }
    }

    fn finish_access(&mut self, addr: Addr) {
        let cost = self.access_ticks(addr);
        let stalled = std::mem::take(&mut self.stall);
        self.tick(cost + stalled);
    }

    /// One slice of the master clock. Fans out per §3.3; the only place a device advances.
    ///
    /// The order is fixed: the PPU first (it defines the line), then the events it raised, then the
    /// APU, the controller and the S-CPU's own timers.
    pub fn tick(&mut self, ticks: u32) {
        let line_before = self.ppu.line;
        let dot_before = self.ppu.dot;

        self.master += u64::from(ticks);
        let events = self.ppu.tick(ticks);

        self.on_line_events(events);
        self.sample_counters(line_before, dot_before, events.0 & LineEvents::START != 0);

        self.apu.tick(ticks);
        self.dma.tick(ticks);
        self.joypad.tick(ticks);
        self.io.auto_joy = self.joypad.auto;
        self.io.tick(ticks);
    }

    /// Acts on the PPU's line events (§3.3).
    ///
    /// This is public because it is the seam a lane without a renderer can exercise: `Ppu::tick` is
    /// the caller in production, and a test that needs "the PPU has reached dot 134 of this line"
    /// raises the one event instead of waiting for the dot clock to walk there.
    pub fn on_line_events(&mut self, events: LineEvents) {
        if events.0 & LineEvents::START != 0 {
            self.stall = 0; // a line starts with a fresh clock
            let line = self.ppu.line;
            self.start_line(line);
        }
        if events.0 & LineEvents::REFRESH != 0 {
            self.stall = REFRESH_TICKS; // the CPU waits 40 clocks (§3.2)
        }
        if events.0 & LineEvents::HDMA != 0 && self.ppu.line <= 224 {
            self.hdma_line(); // §6.3: lines 0-224 only, one unit a channel
        }
        if events.0 & LineEvents::VBLANK != 0 {
            self.vblank();
        }
        if events.0 & LineEvents::HBLANK != 0 {
            self.io.hblank = true; // $4212 bit 6
        }
        if events.0 & LineEvents::END != 0 {
            self.ppu.frame_ready = true; // the frame loop's only wait
            self.dma.begin_frame(); // HDMA table pointers reload (V=0)
        }
    }

    /// V=225: the NMI flag, the CPU's NMI latch, the joypad auto-read and the OAM reload.
    ///
    /// fullsnes "SNES PPU Interrupts": the NMI flag "gets set at begin of Vblank (this happens even
    /// if NMIs are disabled)" and the CPU's latch "gets set when [4200h].7 AND [4210h].7 changes
    /// from 0-to-1"; `$4200` bit 0 gates the auto-read; "only if not in Forced Blank mode" gates the
    /// OAM address reload (§3.3). The latch is taken on the flag's own rising edge — see the body.
    ///
    /// The latch belongs to the **CPU**, and the CPU is not running while a general-purpose DMA
    /// holds it: fullsnes "$420Bh - MDMAEN": "The CPU is paused during the transfer." So a v-blank
    /// that rises and falls entirely inside a transfer is not an NMI it can see — the flag is set
    /// by the PPU and cleared again at V=0 while the clock is stopped — and no latch is recorded
    /// here. `Cpu::step` takes the deferred NMI at the first boundary after the transfer when the
    /// level is still high (a short transfer that ends inside v-blank), which is the one case where
    /// the CPU can see it. Latching here unconditionally gave Zool's full-VRAM fill and Super Ninja
    /// Boy's loader an NMI they could not have seen on hardware, and both re-entered their handlers
    /// before the instruction that clears the handshake byte could run.
    fn vblank(&mut self) {
        // fullsnes "SNES PPU Interrupts": the latch "gets set when [4200h].7 AND [4210h].7 changes
        // from 0-to-1". Test the flag's own 0-to-1 edge before setting it: this function has two
        // callers for the same v-blank (the counter walk in [`Bus::sample_counters`] for a `tick`
        // that ends on the line boundary, and the PPU's own `VBLANK` event when its successor
        // processes dot 0 of V=225), and latching on both gave a game two NMIs in one frame — Super
        // Mario World re-entered `$00:816A` from inside its own handler on 5 of the 40 frames from
        // 100 to 140. `Cpu::step` covers the other half of the AND: enabling NMI while the flag is
        // already high is its own 0-to-1 edge on the level.
        let already_set = self.io.nmi_flag;
        self.io.nmi_flag = true;
        if !already_set && self.nmi_pending() && !self.dma_active {
            self.nmi_latch = true;
        }
        if self.io.nmitimen & 0x01 != 0 {
            self.joypad.begin_auto_read();
        }
        if self.ppu.regs.inidisp & 0x80 == 0 {
            self.ppu.reload_oamadd();
        }
    }

    /// The start of a scanline: H-blank ends, and V=0 does the frame's bookkeeping.
    ///
    /// `$4212` bit 6 "gets toggled in ALL scanlines" (fullsnes), so the clear belongs to the line
    /// start; the NMI flag is "reset automatically at end of Vblank", which is `V=0` (§3.8).
    fn start_line(&mut self, line: u16) {
        self.io.hblank = false;
        if line == 0 {
            self.io.nmi_flag = false;
            // fullsnes: "H=6, V=0 reload HDMA registers". `END` does this too; a second identical
            // reload at the same instant changes nothing, which is what makes both paths safe.
            self.dma.begin_frame();
        }
    }

    /// The H/V-IRQ comparison, off the PPU's counters.
    ///
    /// fullsnes `$4207`: the flag "gets set when the H-Counter gets equal to the H-Count register
    /// value", and `$4209` the same for the V counter. §3.8 places the H compare at `HTIME + 3.5`
    /// and the V compare at `H = 2.5, V = VTIME`, so in whole dots the H match is at `HTIME + 3` and
    /// the V match at dot 2, on every line for the H-only mode and on `VTIME`'s line otherwise.
    /// Detecting the crossing between the previous and the current counter position is what makes
    /// this an edge rather than a level: reading `$4211` acknowledges the flag and it must not be
    /// set again until the next match.
    fn sample_counters(&mut self, line_before: u16, dot_before: u16, start_raised: bool) {
        let before = u32::from(line_before) * DOTS_PER_LINE + u32::from(dot_before);
        let mut after = u32::from(self.ppu.line) * DOTS_PER_LINE + u32::from(self.ppu.dot);
        if after < before {
            after += DOTS_PER_LINE * LINES_PER_FRAME;
        }
        if after == before {
            return;
        }

        // A line change also runs the line's start-of-line work, unless the PPU raised `START`
        // itself: a renderer that raises the event has already had it acted on above.
        let first = before / DOTS_PER_LINE;
        let last = after / DOTS_PER_LINE;
        if last > first && !start_raised {
            for n in first + 1..=last {
                let line = (n % LINES_PER_FRAME) as u16;
                self.start_line(line);
                if line == VBLANK_START {
                    self.vblank();
                }
            }
        }

        let mode = self.io.nmitimen & 0x30;
        if mode == 0 {
            return;
        }
        for n in first..=last {
            let line = (n % LINES_PER_FRAME) as u16;
            let base = n * DOTS_PER_LINE;
            if mode & 0x10 != 0 {
                // H-only, or H+V which only fires on VTIME's line.
                let matches_line = mode != 0x30 || line == self.io.vtime;
                let point = base + u32::from(self.io.htime & 0x01FF) + 3;
                if matches_line && point > before && point <= after {
                    self.io.irq_flag = true;
                }
            }
            if mode & 0x20 != 0 && mode != 0x30 && line == self.io.vtime {
                // Mode 2 ("At V=V + H=0") fires on the V compare alone; mode 3 (H+V) waits for the
                // H compare on VTIME's line, which the arm above handles.
                let point = base + 2;
                if point > before && point <= after {
                    self.io.irq_flag = true;
                }
            }
        }
    }

    /// `io.nmi_enable && RDNMI.7`, with the enable bit read from `NMITIMEN` bit 7.
    pub fn nmi_pending(&self) -> bool {
        self.io.nmitimen & 0x80 != 0 && (self.io.nmi_flag || self.ppu.nmi)
    }

    /// `NMITIMEN` bits 5-4 non-zero and the H/V flag raised: the IRQ is level-driven until `$4211`
    /// is read or the mode is disabled (§3.8), which is what makes a handler that never
    /// acknowledges re-enter immediately after `RTI`.
    pub fn irq_pending(&self) -> bool {
        self.io.nmitimen & 0x30 != 0 && (self.io.irq_flag || self.ppu.irq || self.cart.irq())
    }

    /// H=278 of lines 0-224: the controller is the bus master for one line's units.
    fn hdma_line(&mut self) {
        let mut dma = std::mem::take(&mut self.dma);
        dma.hdma_line(self);
        self.dma = dma;
    }

    /// `$4212` HVBJOY: V-blank flag, H-blank flag and the auto-read's busy bit.
    ///
    /// Bits 5-1 are open bus: fullsnes "SNES Unpredictable Things" gives `4212h 3Eh HVBJOY  Bit5-1
    /// are open bus`, so they answer [`Bus::mdr`] rather than zero.
    fn hvbjoy(&mut self) -> u8 {
        let mut value = self.mdr & 0x3E;
        if self.ppu.line >= VBLANK_START {
            value |= 0x80;
        }
        if self.io.hblank {
            value |= 0x40;
        }
        if self.joypad.auto {
            value |= 0x01;
        }
        value
    }

    /// The whole system area in one function; the order of the arms is the decode.
    fn read_system_or_rom(&mut self, addr: Addr) -> u8 {
        let offset = addr.offset();
        match offset {
            0x0000..=0x1FFF => self.wram[offset as usize],
            0x2000..=0x20FF => self.mdr,
            0x2100..=0x21FF => self.read_b_bus((offset & 0xFF) as u8),
            0x2200..=0x3FFF => self.mdr,
            0x4000..=0x41FF => self.joypad.read_manual(addr, self.mdr),
            0x4200..=0x4211 => self.io.read(addr, self.mdr),
            0x4212 => self.hvbjoy(),
            0x4213..=0x4217 => self.io.read(addr, self.mdr),
            0x4218..=0x421F => self.joypad.read_auto(addr),
            0x4300..=0x437F => self.read_dma_reg(offset),
            0x4220..=0x42FF => self.mdr,
            0x4380..=0x43FF => self.mdr,
            // The `$4400-$5FFF` row: "open bus". The `_` arm below is the ROM window, and
            // `Cartridge::read` maps *every* offset of a LoROM/HiROM bank into the image, so letting
            // this range fall through handed the cartridge ROM bytes at an address the console does
            // not assert /CART for. Combatribes, The reads a block count at `$00:5000` and got code.
            0x4400..=0x5FFF => self.mdr,
            0x6000..=0x7FFF => self.cart.read(addr),
            _ => self.cart.read(addr),
        }
    }

    fn write_system_or_rom(&mut self, addr: Addr, value: u8) {
        let offset = addr.offset();
        match offset {
            0x0000..=0x1FFF => self.wram[offset as usize] = value,
            0x2100..=0x21FF => self.write_b_bus((offset & 0xFF) as u8, value),
            0x4000..=0x41FF => {
                self.io.joy_strobe = value;
                // The latch is the write's, not a read's: `Joypad::write_strobe`.
                self.joypad.write_strobe(value);
            }
            0x4200..=0x420A => self.io.write(addr, value),
            0x420B => self.start_gpdma(value),
            0x420C => self.dma.hdmaen = value,
            0x420D..=0x4217 => self.io.write(addr, value),
            0x4300..=0x437F => self.write_dma_reg(offset, value),
            _ => self.cart.write(addr, value),
        }
    }

    /// `$420B`: every enabled channel runs to completion inside this write (§3.7, §6.2).
    ///
    /// The controller is moved out for the duration, the trick §6.4 names: `Dma` is `Default`, so
    /// this is two copies of eight channels rather than an allocation.
    fn start_gpdma(&mut self, mask: u8) {
        let mut dma = std::mem::take(&mut self.dma);
        dma.mdmaen |= mask;
        self.dma_active = true;
        dma.run_gpdma(mask, self);
        self.dma_active = false;
        self.dma = dma;
    }

    /// One byte of a DMA write to the B-bus, charged at eight master clocks (fullsnes "SNES DMA and
    /// HDMA Notes": "fixed 8 master cycles per byte for DMA").
    fn dma_write(&mut self, dest: u8, value: u8) {
        self.write_b_bus(dest, value);
        self.tick(SLOW_ACCESS);
    }

    /// `$4300-$437F`: the eight channels' registers.
    fn read_dma_reg(&mut self, offset: u16) -> u8 {
        let index = usize::from(offset - 0x4300);
        let channel = index >> 4;
        let reg = index & 0x0F;
        let c = &self.dma.channels[channel];
        match reg {
            0x0 => c.dmap,
            0x1 => c.bbad,
            0x2 => c.a1t.offset() as u8,
            0x3 => (c.a1t.offset() >> 8) as u8,
            0x4 => c.a1t.bank(),
            0x5 => c.das as u8,
            0x6 => (c.das >> 8) as u8,
            0x7 => c.dasb,
            0x8 => c.a2a as u8,
            0x9 => (c.a2a >> 8) as u8,
            // fullsnes "SNES DMA and HDMA Channel 0..7 Registers": "43xAh - NTRLx - HDMA
            // Line-Counter (from current Table entry) (R/W)" and "43xBh - UNUSEDx - Unused Byte
            // (R/W)", with "43xFh - MIRRx - Read/Write-able mirror of 43xBh (R/W)". So $43xB and
            // $43xF are one storage byte and $43xC-$43xE are open bus.
            0xA => c.ntlt,
            0xB | 0xF => c.unused,
            _ => self.mdr,
        }
    }

    fn write_dma_reg(&mut self, offset: u16, value: u8) {
        let index = usize::from(offset - 0x4300);
        let channel = index >> 4;
        let reg = index & 0x0F;
        let c = &mut self.dma.channels[channel];
        match reg {
            0x0 => c.dmap = value,
            0x1 => c.bbad = value,
            0x2 => c.a1t = Addr::new(c.a1t.bank(), (c.a1t.offset() & 0xFF00) | u16::from(value)),
            0x3 => {
                c.a1t = Addr::new(
                    c.a1t.bank(),
                    (c.a1t.offset() & 0x00FF) | (u16::from(value) << 8),
                )
            }
            0x4 => c.a1t = Addr::new(value, c.a1t.offset()),
            0x5 => c.das = (c.das & 0xFF00) | u16::from(value),
            0x6 => c.das = (c.das & 0x00FF) | (u16::from(value) << 8),
            0x7 => c.dasb = value,
            0x8 => c.a2a = (c.a2a & 0xFF00) | u16::from(value),
            0x9 => c.a2a = (c.a2a & 0x00FF) | (u16::from(value) << 8),
            // "43xAh - NTRLx - HDMA Line-Counter (from current Table entry) (R/W)" and "43xBh -
            // UNUSEDx - Unused Byte (R/W)" with "43xFh ... Read/Write-able mirror of 43xBh"
            // (fullsnes "SNES DMA and HDMA Channel 0..7 Registers").
            0xA => c.ntlt = value,
            0xB | 0xF => c.unused = value,
            // $43xC-$43xE: "writing seems to have no effect" (fullsnes).
            _ => {}
        }
    }

    /// `$2100-$21FF`: the PPU block, the APU ports, the WRAM port, open bus elsewhere.
    fn read_b_bus(&mut self, reg: u8) -> u8 {
        match reg {
            0x00..=0x3F => {
                // $2137's read is the CPU's open bus; only its side effect belongs to the PPU, and
                // `WRIO` bit 7 is what allows the latch.
                if reg == 0x37 {
                    if self.io.wrio & 0x80 != 0 {
                        self.ppu.latch_hv();
                    }
                    return self.mdr;
                }
                let value = self.ppu.read(reg);
                value.unwrap_or(self.mdr)
            }
            // fullsnes "SNES Memory Map": "2144h..217Fh - APU Ports 2140-2143h mirrored to
            // 2144h..217Fh", and "SNES Unpredictable Things": "Ports 2144h..217Fh are APU mirrors,
            // NOT open bus." A write mirrors too, so a read of the mirror must answer the port and
            // never `self.mdr`.
            0x40..=0x7F => self.apu.read_port(usize::from(reg - 0x40)),
            0x80 => self.io.read_wram_data(&mut self.wram),
            _ => self.mdr,
        }
    }

    /// `$2100-$21FF`, write side.
    fn write_b_bus(&mut self, reg: u8, value: u8) {
        match reg {
            0x00..=0x3F => self.ppu.write(reg, value),
            0x40..=0x7F => self.apu.write_port(usize::from(reg - 0x40), value),
            0x80 => {
                self.record_wram_port(value);
                self.io.write_wram_data(&mut self.wram, value);
            }
            0x81..=0x83 => self.io.write_wram_port(0x2100 + u16::from(reg), value),
            _ => {}
        }
    }
}

impl BusPort for Bus {
    fn read(&mut self, addr: Addr) -> u8 {
        self.peek(addr)
    }

    fn read_dma(&mut self, addr: Addr) -> u8 {
        self.dma_read(addr)
    }

    fn begin_dma(&mut self) {
        self.begin_dma_transfer();
    }

    fn write_bbus(&mut self, dest: u8, value: u8) {
        self.dma_write(dest, value);
    }
}
