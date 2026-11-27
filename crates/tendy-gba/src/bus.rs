//! The Game Boy Advance's memory map and its one piece of video hardware: the frame clock.
//!
//! GBATEK, *GBA Memory Map*, is the whole of it. Every region has a base, a size and a bus width,
//! and every one of them is decoded here, in `read8`/`write8` and the wider reads built on them:
//!
//! | Region | Address | Size | Bus |
//! |---|---|---|---|
//! | BIOS | `$00000000` | 16 KiB | 32-bit |
//! | On-board work RAM (EWRAM) | `$02000000` | 256 KiB | 16-bit |
//! | On-chip work RAM (IWRAM) | `$03000000` | 32 KiB | 32-bit |
//! | I/O | `$04000000` | 1 KiB | 32-bit |
//! | Palette RAM | `$05000000` | 1 KiB | 16-bit |
//! | Video RAM | `$06000000` | 96 KiB | 16-bit |
//! | OAM | `$07000000` | 1 KiB | 32-bit |
//! | Game Pak ROM, wait state 0 | `$08000000` | 32 MiB | 16-bit |
//! | Game Pak ROM, wait state 1 | `$0A000000` | 32 MiB (mirror) | 16-bit |
//! | Game Pak ROM, wait state 2 | `$0C000000` | 32 MiB (mirror) | 16-bit |
//! | Game Pak SRAM | `$0E000000` | 64 KiB window | 8-bit |
//!
//! **Mirrors.** The internal memories are mirrored across their whole 16 MiB region, because the
//! decoder only looks at the bits below the region's own size: EWRAM repeats every 256 KiB
//! (`$02040000` reads `$02000000`), IWRAM every 32 KiB (`$03008000` reads `$03000000`, and GBATEK's
//! own interrupt notes call `$03FFFFFC` "mirror of `$03007FFC`"), palette RAM every 1 KiB, VRAM
//! every 128 KiB and OAM every 1 KiB. GBATEK's *Classic NES Series* note is the hardware evidence
//! that games rely on it: those cartridges broke emulators by reading "RAM mirrors (eg. main RAM
//! accessed at 2F00000h instead of 2000000h)". The three ROM windows all show the cartridge from
//! its own offset zero, and an address past the ROM's size folds back into it (`offset % size`),
//! which is why those games found "ROM mirrors (instead of the usual increasing numbers in unused
//! ROM area)".
//!
//! **Open bus.** Reading an unmapped address, or the unused half of the I/O page, returns the last
//! value that was on the data bus; GBATEK describes it as "returning data mirrored from other
//! ports" and as the value left by the last fetch. One latch (`open_bus`) is set by every access
//! that reaches memory — a read holds what it read, a write holds what was written — and a read of
//! nothing returns the latch's byte for the address, leaving the latch alone. Because a wider read
//! goes through its bytes in little-endian order, reading a word from memory leaves that word in the
//! latch: write `$DEADBEEF` to work RAM and an unmapped word read hands back `$DEADBEEF`.
//!
//! **What each access costs.** GBATEK's *Address Bus Width and CPU Read/Write Access Widths* table
//! gives the cycles of an 8/16/32-bit access region by region, and this slice charges them: the
//! internal memories are one cycle, on-board work RAM is 3/3/6, palette RAM and VRAM are 1/1/2,
//! Game Pak SRAM is its `WAITCNT` wait plus one, and a Game Pak ROM access is the `WAITCNT` wait for
//! its window plus one — with a 32-bit ROM access split into two 16-bit fragments, the second of
//! which is *always* sequential (GBATEK: "of which, the second fragment is always sequential, even
//! if the first fragment was non-sequential"). The core's own `S`/`N` model counts one cycle per
//! access; what an access owes above that lands in [`Bus::waits`] and is added to the instruction's
//! cost, so a step's cycle count is the core's plus its memory's.
//!
//! **The prefetch buffer.** GBATEK, *GBA GamePak Prefetch*: "When prefetch buffer is enabled, the
//! GBA attempts to read opcodes from Game Pak ROM during periods when the CPU is not using the bus
//! (if any). Memory access is then performed with 0 Waits if the CPU requests data which is already
//! stored in the buffer. The prefetch buffer stores up to eight 16bit values." The eight halfwords
//! are modelled as the window the buffer's next fill covers: a fetch whose address lies in that
//! window costs one cycle and no waits, and a fetch outside it — a branch, a fetch from RAM or
//! BIOS ("Opcodes executed in RAM or BIOS are not affected by the prefetch feature"), a fetch from
//! another 128 KiB block — pays the window's non-sequential cost and refills. What the document
//! does not print is how many cycles an instruction leaves the bus free, so the fill itself is not
//! modelled: the buffer is taken to be primed to its eight entries after every fetch from the Game
//! Pak and empty after anything that breaks the sequence (a branch, a DMA read of the Game Pak, a
//! store to `WAITCNT`).
//!
//! **The 128 KiB boundary.** GBATEK, *GBA GamePak Prefetch → NOTES*: "The GBA forcefully uses
//! non-sequential timing at the beginning of each 128K-block of gamepak ROM, eg. "LDMIA
//! [801fff8h],r0-r7" will have non-sequential timing at 8020000h." A Game Pak ROM access that opens
//! a 128 KiB block pays the non-sequential cost even when it continues a sequence — for an
//! instruction fetch, a CPU data access and a DMA unit alike — and flushing the buffer is a
//! consequence rather than a separate rule.
//!
//! **The video-memory penalty.** GBATEK, *Address Bus Width and CPU Read/Write Access Widths*,
//! footnotes its `OAM 1/1/1 *`, `Palette RAM 1/1/2 *` and `VRAM 1/1/2 *` rows with "* Plus 1 cycle
//! if GBA accesses video memory at the same time", and *VRAM, OAM, and Palette RAM Access* says the
//! CPU "appears to be able to access VRAM/OAM/Palette at any time, a waitstate (one clock cycle)
//! being inserted automatically in case that the display controller was accessing memory
//! simultaneously." One more cycle is charged for a CPU access to VRAM, palette RAM or OAM while
//! the display is drawing a visible line; during H-Blank, V-Blank and forced blank the table's own
//! figure stands.
//!
//! **Byte stores to the video memories.** GBATEK's table gives palette RAM, VRAM and OAM no 8-bit
//! write, and the public suite pins what happens instead: a byte store to palette RAM or to VRAM in
//! a bitmap mode reaches the 16-bit bus on both halves, so the byte is written twice
//! (`memory.gba`'s tests 53 and 54); in a tiled mode and for OAM the store is dropped (tests 50 and
//! 52). Byte *reads* of all three work, so only the write path is specialised.
//!
//! **DMA.** The four channels' registers live in [`crate::dma`]; this module owns their
//! transfers, because a transfer is memory access and the memory lives here. An event-triggered
//! transfer (V-Blank, H-Blank) appends its cycles to the clock it was raised from, so a frame is
//! still exactly 228 lines of 1232 cycles while the CPU executes fewer instructions; an immediate
//! one is charged to [`Bus::waits`], so it lengthens the instruction that wrote the control word.
//!
//! **What is deliberately not here.** VRAM's HBlank-only access windows are G3's (GBATEK notes the CPU "appears to be able to
//! access VRAM/OAM/Palette at any time", so the window is not enforced — only the one-cycle penalty
//! for a simultaneous access is charged). The frame clock is here because the public test ROMs spin on `DISPSTAT`'s VBlank flag
//! before they report anything, so without it nothing at all can be proved — and because a VBlank
//! that raises `IF` is the one interrupt source the suite's BIOS test needs.

use crate::bios::Bios;
use crate::cart::Cartridge;
use crate::dma::{self, Dma, Timing};
use crate::irq;
use crate::save::{self, GPIO_BASE, Rtc, Save};
use crate::serial::{self, Serial};
use crate::sound::{self, Sound};
use crate::timer::Timers;
use tendy_gb::joypad::{Buttons, Shoulders};

/// On-board work RAM, 256 KiB (GBATEK: "`02000000-0203FFFF` WRAM - On-board Work RAM (256 KBytes)").
pub const EWRAM_SIZE: usize = 0x4_0000;
/// On-chip work RAM, 32 KiB.
pub const IWRAM_SIZE: usize = 0x8000;
/// The I/O page: `$04000000-$040003FF`.
pub const IO_SIZE: usize = 0x400;
/// Palette RAM: 1 KiB, 256 RGB555 words for backgrounds and 256 for objects.
pub const PALETTE_SIZE: usize = 0x400;
/// Video RAM: 96 KiB.
pub const VRAM_SIZE: usize = 0x1_8000;
/// OAM: 128 objects of 8 bytes.
pub const OAM_SIZE: usize = 0x400;
/// Game Pak SRAM as this slice holds it. The chip families themselves — SRAM, flash and EEPROM — are
/// [`save`]'s; the bus only maps the window.
pub const SRAM_SIZE: usize = save::SRAM_SIZE;
/// `IF` bit 0: the V-Blank interrupt, which is the one source this slice raises itself.
pub const VBLANK_IF: u16 = 1;

/// The save window's length (GBATEK: "`0E000000-0E00FFFF` Game Pak SRAM (max 64 KBytes)").
pub const SRAM_WINDOW: usize = 0x1_0000;

/// The BIOS is 16 KiB of system ROM (GBATEK, *GBA Memory Map*: "`00000000-00003FFF` BIOS").
pub const BIOS_LEN: u32 = 0x4000;
/// VRAM's addressable size: 96 KiB, the three 32 KiB blocks GBATEK's *LCD VRAM Overview* lists.
pub const VRAM_WINDOW: usize = 0x2_0000;

/// `WAITCNT` bit 14: the Game Pak prefetch buffer.
pub const WAITCNT_PREFETCH: u16 = 1 << 14;
/// How many halfwords the buffer holds. GBATEK, *GBA GamePak Prefetch*: "The prefetch buffer stores
/// up to eight 16bit values."
pub const PREFETCH_HALFWORDS: u8 = 8;
/// The Game Pak's address block for wait-state purposes, 128 KiB. GBATEK, *GBA GamePak Prefetch →
/// NOTES*: "The GBA forcefully uses non-sequential timing at the beginning of each 128K-block of
/// gamepak ROM, eg. "LDMIA [801fff8h],r0-r7" will have non-sequential timing at 8020000h."
pub const ROM_BLOCK_SIZE: u32 = 0x2_0000;
/// `WAITCNT` bits 11-12: the PHI terminal's output. Stored, not used.
pub const WAITCNT_PHI_MASK: u16 = 0b11 << 11;

/// The wait states a `WAITCNT` field can name, in cycles *waited*: "0..3 = 4,3,2,8 cycles" over an
/// access that is already one cycle long (GBATEK, *WAITCNT*).
const WAITS: [u32; 4] = [4, 3, 2, 8];
/// The second-access (sequential) wait for WS0: "0..1 = 2,1 cycles".
const WAITS_SECOND_WS0: [u32; 2] = [2, 1];
/// The second-access wait for WS1: "0..1 = 4,1 cycles; unlike above WS0".
const WAITS_SECOND_WS1: [u32; 2] = [4, 1];
/// The second-access wait for WS2: "0..1 = 8,1 cycles; unlike above WS0,WS1".
const WAITS_SECOND_WS2: [u32; 2] = [8, 1];

/// The post-boot `WAITCNT`: `4317h`, the setting GBATEK's *WAITCNT* entry gives for the cartridges
/// Nintendo ships — "WS0/ROM=3,1 clks; SRAM=8 clks; WS2/EEPROM: 8,8 clks; prefetch enabled; that is
/// WAITCNT=4317h". GBATEK's *power-on* default is `0000h`; the boot sequence is what leaves the
/// cartridge with the faster one, and this slice starts from where the boot sequence ends.
pub const WAITCNT_POST_BOOT: u16 = 0x4317;

/// Region bases, named once so a test can say what it is writing to.
pub const BIOS_BASE: u32 = 0x0000_0000;
pub const EWRAM_BASE: u32 = 0x0200_0000;
pub const IWRAM_BASE: u32 = 0x0300_0000;
pub const IO_BASE: u32 = 0x0400_0000;
pub const PALETTE_BASE: u32 = 0x0500_0000;
pub const VRAM_BASE: u32 = 0x0600_0000;
pub const OAM_BASE: u32 = 0x0700_0000;
pub const ROM_BASE: u32 = 0x0800_0000;
pub const ROM_WS1_BASE: u32 = 0x0A00_0000;
pub const ROM_WS2_BASE: u32 = 0x0C00_0000;
pub const SRAM_BASE: u32 = 0x0E00_0000;

/// I/O offsets this slice answers, from GBATEK's *GBA I/O Map*.
pub const DISPCNT: usize = 0x000;
pub const DISPSTAT: usize = 0x004;
pub const VCOUNT: usize = 0x006;
pub const SOUNDBIAS: usize = 0x088;
pub const KEYINPUT: usize = 0x130;
pub const KEYCNT: usize = 0x132;
pub const IE: usize = 0x200;
pub const IF: usize = 0x202;
pub const WAITCNT: usize = 0x204;
pub const IME: usize = 0x208;
pub const POSTFLG: usize = 0x300;
/// `HALTCNT` (`$04000301`): a store here stops the CPU (GBATEK, *4000301h - HALTCNT*).
pub const HALTCNT: usize = 0x301;

/// The high bytes of the three registers whose two halves are answered separately. Named because a
/// `match` arm is a pattern and `DISPSTAT + 1` is an expression.
const DISPSTAT_HI: usize = DISPSTAT + 1;
const VCOUNT_HI: usize = VCOUNT + 1;
const KEYINPUT_HI: usize = KEYINPUT + 1;
const KEYCNT_HI: usize = KEYCNT + 1;
const IF_HI: usize = IF + 1;
const WAITCNT_HI: usize = WAITCNT + 1;

/// One store a cartridge made to `KEYCNT` (`$04000132`), as the keypad-interrupt probe reports it:
/// what the register held after the store, when the store happened, and which keys were
/// down at that moment. A probe records these; nothing in the machine reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeycntWrite {
    /// The register after the store, as [`Bus::keycnt`] reads it.
    pub value: u16,
    /// The frame clock's count when the store landed, which is the frame the store is in.
    pub frame: u64,
    /// CPU cycles since power-on.
    pub cycles: u64,
    /// `KEYINPUT` at that moment: the keys that were pressed, active low, bits 0-9.
    pub keys: u16,
}

/// Cycles per scanline: 308 dots of 4 cycles (GBATEK, *LCD Dimensions and Timings*: "Total 308 dots,
/// 73.433 us, 1232 cycles").
pub const CYCLES_PER_LINE: u32 = 1232;
/// Scanlines per frame, VBlank included: "Total 228 lines, 16.743 ms, 280896 cycles".
pub const LINES_PER_FRAME: u16 = 228;
/// Visible scanlines: "Visible (*) 160 lines".
pub const VISIBLE_LINES: u16 = 160;
/// Cycles in one frame: 1232 x 228 = 280 896, which at the 16.78 MHz clock is 59.7275 Hz.
pub const CYCLES_PER_FRAME: u32 = 280_896;
/// The first VBlank line: `VCOUNT` 160.
pub const VBLANK_FIRST_LINE: u16 = 160;
/// The last VBlank line. GBATEK, *DISPSTAT*: the V-Blank flag is "set in line 160..226; not 227".
pub const VBLANK_LAST_LINE: u16 = 226;
/// Cycles into a line where the H-Blank flag rises. GBATEK: "Although the drawing time is only 960
/// cycles (240*4), the H-Blank flag is '0' for a total of 1006 cycles."
pub const HBLANK_DRAW_CYCLES: u32 = 1006;
/// The cycles a visible line actually spends drawing. GBATEK, *LCD Dimensions and Timings*: "Although
/// the drawing time is only 960 cycles (240*4), the H-Blank flag is '0' for a total of 1006 cycles."
/// The display controller reads video memory during these, which is what an access to it is charged
/// for ([`Bus::video_penalty`]).
pub const DRAW_CYCLES: u32 = 960;

/// True for a line that has an H-Blank at all. GBATEK, *4000004h - DISPSTAT*: the H-Blank flag is
/// "set in lines 2-159,162-225" — lines 0-1 and the V-Blank lines either side of them have none.
///
/// One predicate, called by both the `DISPSTAT` flag and the H-Blank interrupt/DMA trigger, so the
/// two cannot drift apart: the flag is this range *and* the dot, the event is this range at the dot
/// the flag rises on.
pub const fn hblank_line(line: u16) -> bool {
    (line >= 2 && line <= 159) || (line >= 162 && line <= 225)
}

/// The frame clock: `VCOUNT`, the sub-line cycle count, and how many frames the LCD has finished.
///
/// This is the whole of the video hardware in this slice. It counts the master clock exactly — one
/// frame is 228 lines of 1232 cycles, both divisors of 280 896 — so `VCOUNT` and the flags derived
/// from it land on the same cycles GBATEK publishes, at any instruction granularity the CPU offers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lcd {
    /// `VCOUNT`: the line being drawn, 0-159 visible, 160-227 in VBlank.
    pub line: u16,
    /// Cycles into the current line, 0-1231.
    pub line_cycle: u32,
    /// Frames finished since power-on.
    pub frames: u64,
    /// How many times the clock has entered V-Blank. A monotonic counter rather than a flag, so a
    /// tick that spans more than one line — or more than one frame — still tells its caller that the
    /// boundary was crossed. It is not part of the machine's state: nothing outside a tick can see
    /// it.
    pub vblank_entries: u64,
}

impl Lcd {
    /// Power-on: line 0, cycle 0.
    pub fn new() -> Lcd {
        Lcd::default()
    }

    /// True while `DISPSTAT`'s V-Blank flag is up: lines 160 to 226.
    pub fn in_vblank(&self) -> bool {
        (VBLANK_FIRST_LINE..=VBLANK_LAST_LINE).contains(&self.line)
    }

    /// True while `DISPSTAT`'s H-Blank flag is up: on a line that has one, the 226 cycles after
    /// the 960 drawing cycles.
    pub fn in_hblank(&self) -> bool {
        hblank_line(self.line) && self.line_cycle >= HBLANK_DRAW_CYCLES
    }

    /// Advance the LCD `cycles` master cycles, rolling lines and frames over as they end.
    pub fn tick(&mut self, mut cycles: u32) {
        while cycles > 0 {
            let left = CYCLES_PER_LINE - self.line_cycle;
            if cycles < left {
                self.line_cycle += cycles;
                return;
            }
            cycles -= left;
            self.line_cycle = 0;
            self.line += 1;
            if self.line == VBLANK_FIRST_LINE {
                self.vblank_entries += 1;
            }
            if self.line == LINES_PER_FRAME {
                self.line = 0;
                self.frames += 1;
            }
        }
    }
}

/// The machine's bus: the cartridge, every internal memory, the I/O page, and the frame clock.
#[derive(Clone)]
pub struct Bus {
    pub cart: Cartridge,
    /// 256 KiB of on-board work RAM (`$02000000`).
    pub ewram: Box<[u8; EWRAM_SIZE]>,
    /// 32 KiB of on-chip work RAM (`$03000000`).
    pub iwram: Box<[u8; IWRAM_SIZE]>,
    /// The I/O page's storage. Registers that are derived rather than stored — `DISPSTAT`'s status
    /// bits, `VCOUNT`, `KEYINPUT` — are answered in `io_read8`, and their read-only halves are
    /// kept clear here.
    pub io: [u8; IO_SIZE],
    /// Palette RAM (`$05000000`), 512 RGB555 words.
    pub palette: [u8; PALETTE_SIZE],
    /// 96 KiB of video RAM (`$06000000`). Nothing draws into it yet: the PPU is G3's.
    pub vram: Box<[u8; VRAM_SIZE]>,
    /// OAM (`$07000000`), 128 objects.
    pub oam: [u8; OAM_SIZE],
    /// Game Pak SRAM (`$0E000000`), the low 256 bytes of the 64 KiB window.
    pub save: Save,
    /// The RTC cart's clock, `None` for a cartridge without one (the `SIIRTC_V` string).
    pub rtc: Option<Rtc>,
    /// The frame clock.
    pub lcd: Lcd,
    /// The four timers: their counters and prescaler remainders, clocked by [`Bus::tick`]. Their
    /// registers are answered by [`Timers`] rather than kept in `io`, because a read of `TMxCNT_L`
    /// returns a counter that no store ever wrote.
    pub timers: Timers,
    /// The four DMA channels. Unlike the timers, every DMA register is storage, so it lives in `io`
    /// (as the hardware's does) and travels in the save state with the rest of the I/O page; this
    /// field is the decoded view of it.
    pub dma: Dma,
    /// The sound unit: two DMA-fed sample channels' FIFOs, their registers, and the mixer's output
    /// staging. Its registers are answered by [`Sound`] rather than kept in `io`, because a read of
    /// `SOUNDCNT_X` returns live status and the FIFOs are write-only.
    pub sound: Sound,
    /// The link port: the serial registers' meaning, the transfer engine and the cable (G9). Its
    /// registers *are* storage in `io` — as the hardware's are — so they travel in the save state;
    /// what lives here is the transfer in flight, which a state never holds.
    pub serial: Serial,
    /// The keypad as the front end last set it. GBATEK's `KEYINPUT` bits are active low, so this
    /// holds *pressed* buttons and `keyinput` inverts them.
    pub buttons: Buttons,
    /// The Advance's two shoulder buttons, which have no room in the Game Boy's one-byte
    /// [`Buttons`] (G7). `keyinput` puts them on bits 8 (R) and 9 (L).
    pub shoulders: Shoulders,
    /// Every `KEYCNT` store, when a probe asked for them ([`Bus::trace_keycnt`]). `None` in a
    /// normal run, so a store pays one test and nothing is allocated. A diagnostic, not machine
    /// state: it is not in the save state, the way [`Bios::trace`] is not.
    pub keycnt_writes: Option<Vec<KeycntWrite>>,
    /// The last value on the data bus, for reads of nothing.
    pub open_bus: u32,
    /// CPU cycles executed since power-on, which is also the LCD's clock.
    pub cycles: u64,
    /// The BIOS: the word a read of it returns, the calls it has answered, and the wait an
    /// `IntrWait` is in.
    pub bios: Bios,
    /// The wait cycles this instruction's accesses owe above the one cycle each the core's flat
    /// model charges for them. [`crate::cpu::Cpu::step`] adds this to the instruction's cost and
    /// clears it, so `bus.waits` is only ever non-zero inside a step.
    pub waits: u32,
    /// The Game Pak prefetch buffer: the ROM address of its oldest entry, `None` while it is empty
    /// (which is what it is with `WAITCNT`'s bit 14 clear, and after anything that breaks the fetch
    /// sequence).
    pub prefetch_next: Option<u32>,
    /// How many halfwords the buffer holds from [`Bus::prefetch_next`] on, at most
    /// [`PREFETCH_HALFWORDS`]. Together with the address it is the window a fetch may hit.
    pub prefetch_count: u8,
    /// The address just past the last opcode fetch: a fetch at exactly this address is sequential,
    /// anything else is a branch and refills the pipeline.
    pub last_fetch_end: Option<u32>,
    /// The address just past the last data access, for the same reason on the data side.
    pub last_data_end: Option<u32>,
    /// A store to `HALTCNT` was made by the instruction that just ran. The CPU stops between
    /// instructions, so the flag is consumed by the next [`crate::cpu::Cpu::step`]; it is in
    /// the save state because a frame can end between the store and that step.
    pub halt_request: bool,
}

impl Bus {
    /// Power-on: the cartridge in the slot, every memory zeroed, the LCD at line 0 cycle 0, and the
    /// I/O page at the values GBATEK gives for after the boot sequence.
    pub fn new(cart: Cartridge) -> Bus {
        let mut io = [0u8; IO_SIZE];
        // The boot sequence leaves both rotation/scaling matrices at identity — BG2PA and BG3PA are
        // 0100h and so are BG2PD and BG3PD. A bitmap-mode cartridge depends on it: the public test
        // ROM `roms/test/gba/ppu/hello.gba` selects mode 4 and never writes a matrix at all, because
        // on hardware it never has to. (G3's renderer reads these four.)
        for offset in [0x020, 0x026, 0x030, 0x036] {
            io[offset] = 0x00;
            io[offset + 1] = 0x01;
        }
        // GBATEK, *4000300h - POSTFLG*: "After initial reset, the GBA BIOS initializes the register
        // to 01h" — so a cartridge that has just started sees 1.
        io[POSTFLG] = 0x01;
        // GBATEK, *4000204h - WAITCNT*: the value the boot sequence leaves for the cartridge.
        io[WAITCNT] = WAITCNT_POST_BOOT as u8;
        io[WAITCNT + 1] = (WAITCNT_POST_BOOT >> 8) as u8;
        // The save chip and the clock come from the ROM's ID strings, before the ROM is moved into
        // the cartridge field below.
        let save = save::build(&cart.rom);
        let rtc = save::has_rtc(&cart.rom).then(Rtc::new);
        Bus {
            cart,
            ewram: Box::new([0; EWRAM_SIZE]),
            iwram: Box::new([0; IWRAM_SIZE]),
            io,
            palette: [0; PALETTE_SIZE],
            vram: Box::new([0; VRAM_SIZE]),
            oam: [0; OAM_SIZE],
            save,
            rtc,
            lcd: Lcd::new(),
            timers: Timers::new(),
            dma: Dma::new(),
            sound: Sound::new(),
            serial: Serial::new(),
            buttons: Buttons::NONE,
            shoulders: Shoulders::NONE,
            keycnt_writes: None,
            open_bus: 0,
            cycles: 0,
            bios: Bios::new(),
            waits: 0,
            prefetch_next: None,
            prefetch_count: 0,
            last_fetch_end: None,
            last_data_end: None,
            halt_request: false,
        }
    }

    /// Advance the master clock `cycles` cycles: what one step of the CPU cost.
    ///
    /// Time is spent in pieces, each ending at the next thing the display does — the start of the
    /// line's H-Blank, then the start of the next line — so an event runs on the cycle GBATEK puts
    /// it on rather than at whatever point the instruction that is executing happens to end. The
    /// events are the display's own: `DISPSTAT`'s H-Blank and V-Counter interrupts, and the DMA
    /// channels timed to H-Blank and the frame's V-Blank.
    ///
    /// A transfer an event starts **steals its cycles from the CPU**: the budget this call is
    /// spending grows by the transfer's cost, so the machine keeps its 228 lines of 1232 cycles and
    /// the CPU simply retires fewer instructions inside them. That is what GBATEK's *DMA Transfer
    /// Time* means by the CPU being halted for the duration.
    pub fn tick(&mut self, cycles: u32) {
        let mut budget = u64::from(cycles);
        let mut spent = 0u64;
        while spent < budget {
            // The nearer of the line's H-Blank and its end. Always at least one cycle away.
            let to_hblank = if self.lcd.line_cycle < HBLANK_DRAW_CYCLES {
                HBLANK_DRAW_CYCLES - self.lcd.line_cycle
            } else {
                CYCLES_PER_LINE - self.lcd.line_cycle
            };
            // A serial transfer ends on a cycle it can name in advance, so the clock stops there
            // rather than at the end of whichever instruction happens to be running: the busy bit
            // clears, and the interrupt rises, on the cycle the hardware would.
            let to_serial = self.serial.cycles_to_deadline().unwrap_or(u32::MAX).max(1);
            let chunk = (budget - spent)
                .min(u64::from(to_hblank))
                .min(u64::from(to_serial)) as u32;
            let before = self.lcd.line_cycle;
            self.clock(chunk);
            spent += u64::from(chunk);
            // The H-Blank begins exactly here.
            if before < HBLANK_DRAW_CYCLES && self.lcd.line_cycle == HBLANK_DRAW_CYCLES {
                budget += u64::from(self.hblank_event());
            }
            // The line rolled over, so a new one begins (and after line 227, a new frame).
            if self.lcd.line_cycle < before {
                budget += u64::from(self.line_event());
            }
            // A FIFO that fell to its refill level during this chunk asks DMA1/DMA2 for sixteen
            // more bytes now; the transfer's cycles are stolen from the CPU like an H-Blank one's.
            budget += u64::from(self.service_audio_dma());
        }
    }

    /// The clock itself: the LCD, the cartridge's clock and the four timers, plus the V-Blank
    /// request the display raises on the line it enters (when `DISPSTAT` bit 3 asks for one).
    fn clock(&mut self, cycles: u32) {
        let entries = self.lcd.vblank_entries;
        let started = self.cycles;
        self.cycles += u64::from(cycles);
        self.lcd.tick(cycles);
        if let Some(rtc) = &mut self.rtc {
            rtc.tick(u64::from(cycles));
        }
        // The timers are clocked by the same master clock, and an overflow that has its interrupt
        // bit set raises its request here — one of the ways a halted CPU can be woken.
        let overflowed = self.timers.tick(cycles);
        if overflowed != 0 {
            self.raise_interrupt(overflowed);
        }
        // The sound unit's sample clock is the selected timer's overflow, so it is handed the two
        // timers' periods after they have been clocked: a timer started this tick is already
        // running when the FIFO asks how fast to play. Which timer each FIFO selects is its own
        // `SOUNDCNT_H` bit — GBATEK, *4000082h - SOUNDCNT_H*: "Direct Sound A Sampling Rate
        // (0=Timer 0, 1=Timer 1)", and bit 14 the same for B.
        let periods = [
            self.fifo_timer_period(self.fifo_timer(0)),
            self.fifo_timer_period(self.fifo_timer(1)),
        ];
        self.sound.tick(cycles, periods);
        // The link port's own clock: a transfer that completes here asks for request 7 when its
        // `SIOCNT` bit 14 says so (GBATEK, *4000128h - SIOCNT*: "IRQ Enable ... Want IRQ upon
        // completion").
        if self
            .serial
            .tick(&mut self.io, cycles, started, self.lcd.frames)
        {
            self.raise(irq::SERIAL);
        }
        if self.lcd.vblank_entries != entries && self.io[DISPSTAT] & 0b1000 != 0 {
            self.raise(irq::VBLANK);
        }
    }

    /// The display's H-Blank begins, on `line_cycle` 1006. The interrupt follows [`hblank_line`];
    /// the channels timed to H-Blank follow the drawing lines alone. Returns the cycles a transfer
    /// stole.
    fn hblank_event(&mut self) -> u32 {
        if hblank_line(self.lcd.line) && self.io[DISPSTAT] & 0b1_0000 != 0 {
            self.raise(irq::HBLANK);
        }
        // H-Blank DMA runs on the 160 drawing lines and is suspended for the whole V-Blank, so a
        // repeating channel does not consume table rows during the hidden lines: hardware report on
        // gbadev.org ("hblank DMA does not trigger during vblank ... you need to stop the dma
        // transfer and reset the registers on the vblank period"), which is why the Classic NES
        // ports re-arm their 160-row per-scanline tables in V-Blank. mGBA gates the same way
        // (`video.c`, `vcount < 160`). GBATEK's DISPSTAT flag range is about the flag, not this.
        if self.lcd.line >= VBLANK_FIRST_LINE {
            return 0;
        }
        self.run_timed_dma(Timing::HBlank)
    }

    /// A line begins: the V-Counter comparison is taken here, and line 160 begins V-Blank, where the
    /// V-Blank channels run. Returns the cycles a transfer stole.
    fn line_event(&mut self) -> u32 {
        let line = self.lcd.line;
        let setting = u16::from_le_bytes([self.io[DISPSTAT], self.io[DISPSTAT + 1]]) >> 8;
        if (line & 0xFF) == setting && self.io[DISPSTAT] & 0b10_0000 != 0 {
            self.raise(irq::VCOUNT);
        }
        if line == VBLANK_FIRST_LINE {
            self.run_timed_dma(Timing::VBlank)
        } else {
            0
        }
    }

    /// Run every enabled channel whose start timing is `timing`, in channel order, returning the
    /// cycles they stole from the CPU.
    fn run_timed_dma(&mut self, timing: Timing) -> u32 {
        let mut stolen = 0;
        for index in 0..dma::CHANNELS {
            let channel = self.dma.channel(index);
            if channel.enabled() && channel.timing() == timing {
                stolen += self.transfer(index);
            }
        }
        stolen
    }

    /// A channel's control word was just written, so a channel it enabled with an immediate start
    /// runs now. Its cycles are charged to [`Bus::waits`], not to the clock: the transfer happens
    /// inside the instruction that asked for it, and makes that instruction longer.
    fn start_written_dma(&mut self, index: usize) {
        let channel = self.dma.channel(index);
        if channel.enabled() && channel.timing() == Timing::Immediate {
            self.waits += self.transfer(index);
        }
        // A Special-timing channel is started by its FIFO's request, which may already be up when
        // the enable bit rises: the FIFO fell to half full while the DMA was still off. Servicing
        // it here means the first refill is not delayed to the end of the next chunk.
        if channel.enabled() {
            self.waits += self.service_audio_dma();
        }
    }

    /// Which timer a FIFO takes its sample clock from: `SOUNDCNT_H` bit 10 for A and bit 14 for B
    /// ("0=Timer 0, 1=Timer 1", GBATEK *4000082h - SOUNDCNT_H*).
    fn fifo_timer(&self, index: usize) -> usize {
        let bit = if index == 0 {
            sound::FIFO_A_TIMER
        } else {
            sound::FIFO_B_TIMER
        };
        usize::from(self.sound.control_h() & bit != 0)
    }

    /// The master-clock period of the timer a FIFO selects, `None` while it is stopped or counting
    /// the timer below it — a cascaded timer does not overflow on the clock, so it cannot be a
    /// sample clock.
    fn fifo_timer_period(&self, index: usize) -> Option<u32> {
        let timer = self.timers.t[index];
        if !timer.running() || timer.cascaded() {
            return None;
        }
        Some(timer.prescaler() * (0x1_0000 - u32::from(timer.reload)))
    }

    /// Run the DMA a drained FIFO asks for: DMA1 for channel A, DMA2 for channel B, each only while
    /// it is enabled with the Special start timing and pointed at its own FIFO. Returns the cycles
    /// the transfers stole from the CPU.
    ///
    /// **How a drained FIFO asks.** The pop that drops the FIFO to sixteen bytes sets a latch;
    /// this method takes it (one transfer answers one request, so a block that lands at exactly the
    /// threshold does not ask again until the next pop) and runs the channel the hardware names.
    /// With no such channel enabled the request is dropped — the DAC keeps holding its last sample
    /// until a pop asks again.
    fn service_audio_dma(&mut self) -> u32 {
        let mut stolen = 0;
        for (index, fifo, bit) in [(1usize, sound::FIFO_A, 0usize), (2, sound::FIFO_B, 1)] {
            if !self.sound.fifo[bit].take_refill() {
                continue;
            }
            let channel = self.dma.channel(index);
            if !channel.enabled() || channel.timing() != Timing::Special {
                continue;
            }
            if channel.dad & dma::ADDRESS_MASK_ANY != IO_BASE + fifo as u32 {
                continue;
            }
            stolen += self.fifo_transfer(index);
        }
        stolen
    }

    /// One sound-FIFO transfer: GBATEK, *Sound DMA (FIFO Timing Mode)*: "Upon DMA request from
    /// sound controller, 4 units of 32bits (16 bytes) are transferred (both Word Count register and
    /// DMA Transfer Type bit are ignored) ... The destination address will not be incremented in
    /// FIFO mode."
    fn fifo_transfer(&mut self, index: usize) -> u32 {
        let original = self.dma.channel(index);
        let units = 4u32;
        let unit = 4u32;
        // GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: "The mis-aligned low bit(s)
        // are ignored, the memory access goes to a forcibly aligned (rounded-down) memory address".
        // A DMA unit is one of those accesses, so both cursors start on their size's boundary; the
        // count is untouched — this masks the address, not the length.
        let mut sad = original.cursor_sad & !(unit - 1);
        let dad = original.cursor_dad & !(unit - 1);
        let step = match (original.control & dma::SRC_MASK) >> 7 {
            1 => unit.wrapping_neg(),
            2 => 0,
            _ => unit,
        };
        let internal = if rom_window(sad).is_some() && rom_window(dad).is_some() {
            4
        } else {
            2
        };
        // The destination does not move in FIFO mode, so its four units are all the same address and
        // the first of them is the only non-sequential one; the source steps as the control word says.
        let cycles = self.dma_half_total(sad, unit as usize, step, units)
            + self.dma_half_total(dad, unit as usize, 0, units)
            + internal;
        // A DMA read takes the Game Pak bus, so the opcode prefetch buffer is refilled out of
        // sequence and whatever it held is gone.
        if rom_window(sad).is_some() {
            self.invalidate_prefetch();
        }
        for _ in 0..units {
            let mut word = 0u32;
            for lane in 0..unit {
                word |= u32::from(self.lane8(sad.wrapping_add(lane))) << (8 * lane);
            }
            for lane in 0..unit {
                self.store8(dad.wrapping_add(lane), (word >> (8 * lane)) as u8);
            }
            sad = sad.wrapping_add(step);
        }
        self.dma.finish(index, original, sad, dad);
        if original.wants_irq() {
            self.raise(irq::DMA0 + index);
        }
        cycles
    }

    /// Run one channel's transfer now: the copies, the register update the end of a transfer makes,
    /// its end interrupt, and the cycles it cost.
    ///
    /// GBATEK's *Transfer Rate/Timing*, "`2N+2(n-1)S+xI`": "Of which, `1N+(n-1)S` are read cycles,
    /// and the other `1N+(n-1)S` are write cycles, actual number of cycles depends on the waitstates
    /// and bus-width of the source and destination areas." The read half is therefore priced from
    /// the **source** region and the write half from the **destination** region — each with its
    /// first unit at that region's non-sequential cost (`N`) and every later unit at its sequential
    /// cost (`S`) — and the internal term is 2 (or 4 when both sides are in the Game Pak). The reads
    /// and writes go through the same decode every other access does, but are never charged to
    /// [`Bus::waits`] — the cost is returned, and the caller spends it as machine time.
    fn transfer(&mut self, index: usize) -> u32 {
        let original = self.dma.channel(index);
        let unit = original.unit();
        let units = original.units(index);
        // GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: each unit of a DMA is a
        // halfword or word access, and a mis-aligned one goes "to a forcibly aligned (rounded-down)
        // memory address". Both cursors therefore start on their unit boundary; `units` above is
        // still the count the cartridge wrote — this masks the address, not the length.
        let mut sad = original.cursor_sad & !(unit - 1);
        let mut dad = original.cursor_dad & !(unit - 1);
        let internal = if rom_window(sad).is_some() && rom_window(dad).is_some() {
            4
        } else {
            2
        };
        // Every unit but the first is the sequential one — unless it opens a 128 KiB Game Pak block,
        // which is non-sequential however the unit before it ended ([`Bus::dma_half_total`]).
        let cycles = self.dma_half_total(sad, unit as usize, original.source_step(), units)
            + self.dma_half_total(dad, unit as usize, original.destination_step(), units)
            + internal;
        // A DMA read takes the Game Pak bus, so the opcode prefetch buffer is refilled out of
        // sequence and whatever it held is gone.
        if rom_window(sad).is_some() {
            self.invalidate_prefetch();
        }
        for _ in 0..units {
            let mut word = 0u32;
            for lane in 0..unit {
                word |= u32::from(self.lane8(sad.wrapping_add(lane))) << (8 * lane);
            }
            for lane in 0..unit {
                self.store8(dad.wrapping_add(lane), (word >> (8 * lane)) as u8);
            }
            sad = sad.wrapping_add(original.source_step());
            dad = dad.wrapping_add(original.destination_step());
        }
        self.dma.finish(index, original, sad, dad);
        if original.wants_irq() {
            self.raise(irq::DMA0 + index);
        }
        cycles
    }

    /// Set an `IF` bit. GBATEK, *4000202h - IF*: writing a `1` to a bit *acknowledges* the
    /// interrupt and clears it, so this is the only way a flag rises.
    pub fn raise_interrupt(&mut self, bit: u16) {
        let flags = u16::from_le_bytes([self.io[IF], self.io[IF + 1]]) | (bit & 0x3FFF);
        self.io[IF..IF + 2].copy_from_slice(&flags.to_le_bytes());
    }

    /// Raise the request GBATEK's table numbers `index`, which is what every source here calls.
    pub fn raise(&mut self, index: usize) {
        self.raise_interrupt(irq::bit(index));
    }

    /// How many master cycles until the hardware does the next thing that could wake a halted CPU:
    /// the start of this line's HBlank, the start of the next line, or a timer overflow. Always at
    /// least one cycle, so a caller may add it to the clock without spinning.
    pub fn cycles_to_next_event(&self) -> u32 {
        let next = if self.lcd.line_cycle < HBLANK_DRAW_CYCLES {
            HBLANK_DRAW_CYCLES
        } else {
            CYCLES_PER_LINE
        };
        let display = (next - self.lcd.line_cycle).max(1);
        let timers = match self.timers.cycles_to_overflow() {
            Some(timer) => display.min(timer.max(1)),
            None => display,
        };
        match self.serial.cycles_to_deadline() {
            Some(serial) => timers.min(serial.max(1)),
            None => timers,
        }
    }

    /// `IE`: the requests allowed through to the CPU.
    pub fn ie(&self) -> u16 {
        u16::from_le_bytes([self.io[IE], self.io[IE + 1]])
    }

    /// `IF`: the requests themselves, before `IE` filters them.
    pub fn irq_flags(&self) -> u16 {
        u16::from_le_bytes([self.io[IF], self.io[IF + 1]])
    }

    /// `IME` bit 0: the master switch in front of `IE & IF`.
    pub fn ime(&self) -> bool {
        self.io[IME] & 1 != 0
    }

    /// The requests that survived `IE` and are asking for an interrupt, acknowledged or not.
    pub fn requested(&self) -> u16 {
        self.ie() & self.irq_flags() & irq::IMPLEMENTED
    }

    /// The highest-priority request among [`Bus::requested`], for a test or a diagnosis.
    pub fn highest_request(&self) -> Option<usize> {
        irq::highest(self.requested())
    }

    /// True when `IE & IF` names any implemented request — what wakes a halted CPU, whether or not
    /// `IME` is set (GBATEK, *HALTCNT*: "In Halt mode, the CPU is paused as long as (IE AND IF)=0").
    pub fn irq_requested(&self) -> bool {
        self.requested() != 0
    }

    /// `WAITCNT` as the CPU reads it: bits 13 and 15 are not writable, and bit 15 — the Game Pak
    /// type flag — reads 0 for a Game Boy Advance cartridge.
    pub fn waitcnt(&self) -> u16 {
        u16::from_le_bytes([self.io[WAITCNT], self.io[WAITCNT + 1]]) & 0x5FFF
    }

    /// True with the Game Pak prefetch buffer enabled (GBATEK: "14 Game Pak Prefetch Buffer").
    pub fn prefetch_enabled(&self) -> bool {
        self.waitcnt() & WAITCNT_PREFETCH != 0
    }

    /// True in one of the three bitmap BG modes, which is what decides how much of VRAM is
    /// addressable and what a byte store to it does.
    pub fn bitmap_mode(&self) -> bool {
        (3..=5).contains(&(self.io[DISPCNT] & 0b111))
    }

    /// The cycles GBATEK's table gives one access of `width` bytes at `addr`: the fragment costs of
    /// a Game Pak ROM access, or the region's own figure.
    fn access_cycles(&mut self, addr: u32, width: usize, fetch: bool) -> u32 {
        // GBATEK, *GBA GamePak Prefetch → NOTES*: "The GBA forcefully uses non-sequential timing at
        // the beginning of each 128K-block of gamepak ROM, eg. "LDMIA [801fff8h],r0-r7" will have
        // non-sequential timing at 8020000h." A ROM access that opens a block is therefore
        // non-sequential whatever the access before it ended with, and an instruction fetch there
        // cannot be a buffered one.
        let block_start = rom_window(addr).is_some() && addr.is_multiple_of(ROM_BLOCK_SIZE);
        let sequential = !block_start
            && if fetch {
                self.last_fetch_end == Some(addr)
            } else {
                self.last_data_end == Some(addr)
            };
        let mut total = 0;
        if let Some(window) = rom_window(addr) {
            let (first, second) = self.rom_costs(window);
            let buffer = self.prefetch_window();
            for fragment in 0..rom_fragments(width) {
                let at = addr + (fragment as u32) * 2;
                // "Memory access is then performed with 0 Waits if the CPU requests data which is
                // already stored in the buffer." The buffer's eight entries are the window it would
                // hand over next; a fetch of any address inside it is a hit, whether it continues
                // the sequence (the common case) or jumps forward within the window.
                let buffered = !block_start
                    && buffer.is_some_and(|(start, count)| {
                        at >= start && at < start + 2 * u32::from(count)
                    });
                if fetch && buffered {
                    total += 1;
                } else {
                    total += rom_fragment_cost(first, second, fragment, sequential);
                }
            }
        } else {
            total = self.internal_cycles(addr, width, true);
        }
        if fetch {
            // "Opcodes executed in RAM or BIOS are not affected by the prefetch feature." A fetch
            // from the Game Pak leaves the buffer primed to its eight entries (the fill itself is
            // not modelled — see the module note); anything else empties it.
            if self.prefetch_enabled() && rom_window(addr).is_some() {
                self.prefetch_next = Some(addr + width as u32);
                self.prefetch_count = PREFETCH_HALFWORDS;
            } else {
                self.invalidate_prefetch();
            }
            self.last_fetch_end = Some(addr + width as u32);
        } else {
            self.last_data_end = Some(addr + width as u32);
        }
        total
    }

    /// The buffer's entries as a window — the address of its oldest halfword and how many it holds —
    /// or `None` while it is empty. Empty means empty: with `WAITCNT`'s bit 14 clear the hardware
    /// fills nothing, and every access pays the window's own wait.
    fn prefetch_window(&self) -> Option<(u32, u8)> {
        match self.prefetch_next {
            Some(start) if self.prefetch_count > 0 && self.prefetch_enabled() => {
                Some((start, self.prefetch_count.min(PREFETCH_HALFWORDS)))
            }
            _ => None,
        }
    }

    /// Empty the prefetch buffer. Its entries belong to a fetch sequence that has been broken: the
    /// CPU executed elsewhere, a DMA read took the Game Pak bus, or `WAITCNT` was rewritten.
    fn invalidate_prefetch(&mut self) {
        self.prefetch_next = None;
        self.prefetch_count = 0;
    }

    /// True while the display controller is drawing a visible line, which is when an access to the
    /// video memories and the controller can collide. GBATEK, *VRAM, OAM, and Palette RAM Access*: a
    /// waitstate is "inserted automatically in case that the display controller was accessing memory
    /// simultaneously", and the table's footnote prices it: "* Plus 1 cycle if GBA accesses video
    /// memory at the same time." Drawing is the 960 cycles of a visible line before its H-Blank, and
    /// stops entirely while `DISPCNT`'s forced-blank bit is set — *LCD Dimensions and Timings*:
    /// "Although the drawing time is only 960 cycles (240*4) ...".
    fn video_penalty(&self) -> bool {
        !self.forced_blank()
            && self.lcd.line < VBLANK_FIRST_LINE
            && self.lcd.line_cycle < DRAW_CYCLES
    }

    /// `DISPCNT` bit 7: the display is forced blank, so the controller fetches nothing.
    fn forced_blank(&self) -> bool {
        self.io[DISPCNT] & 0x80 != 0
    }

    /// The two costs of a Game Pak ROM access in a window: the non-sequential one, and the
    /// sequential one that follows it.
    fn rom_costs(&self, window: usize) -> (u32, u32) {
        let waitcnt = self.waitcnt();
        let first = match window {
            0 => (waitcnt >> 2) & 3,
            1 => (waitcnt >> 5) & 3,
            _ => (waitcnt >> 8) & 3,
        };
        let second = match window {
            0 => WAITS_SECOND_WS0[usize::from(waitcnt >> 4 & 1)],
            1 => WAITS_SECOND_WS1[usize::from(waitcnt >> 7 & 1)],
            _ => WAITS_SECOND_WS2[usize::from(waitcnt >> 10 & 1)],
        };
        (1 + WAITS[first as usize], 1 + second)
    }

    /// The cost of one whole Game Pak access of `width` bytes: GBATEK's *Address Bus Width* table
    /// prices GamePak ROM `5/5/8` — one access cycle plus the waits — and its *GamePak ROM
    /// Waitstates* note splits a wide one into 16-bit fragments, "of which, the second fragment is
    /// always sequential, even if the first fragment was non-sequential". A non-sequential 32-bit
    /// read is therefore the `5` of its opening fragment plus the `3` of the sequential one after
    /// it — the table's `8` — and a sequential 32-bit read is `3+3` = `6`. The CPU's own path
    /// ([`Bus::access_cycles`]) and the DMA's ([`Bus::dma_half_costs`]) are priced from these two
    /// functions, so neither can read the width and the other drop it.
    fn rom_access_cost(&self, window: usize, width: usize, sequential: bool) -> u32 {
        let (first, second) = self.rom_costs(window);
        (0..rom_fragments(width))
            .map(|fragment| rom_fragment_cost(first, second, fragment, sequential))
            .sum()
    }

    /// The `(non-sequential, sequential)` costs of one DMA *half* at `addr`.
    ///
    /// GBATEK, *GBA DMA Transfers → Transfer Rate/Timing*: "For n data units, the DMA transfer time
    /// is: `2N+2(n-1)S+xI` … Of which, `1N+(n-1)S` are read cycles, and the other `1N+(n-1)S` are
    /// write cycles, actual number of cycles depends on the waitstates and bus-width of the
    /// **source and destination** areas." One half therefore takes its costs from the region it
    /// touches, and the two halves of a transfer are not interchangeable when the two ends are
    /// different kinds of memory.
    ///
    /// `width` is the width of one **unit**, and in the Game Pak it decides the figure as much as
    /// the waitstates do: a 16-bit unit is one fragment and costs the table's `5`/`3`, a 32-bit
    /// unit is two and costs `8`/`6` ([`Bus::rom_access_cost`]). A sound-FIFO transfer is four
    /// 32-bit units through this same function, so it is priced by the same rule.
    fn dma_half_costs(&self, addr: u32, width: usize) -> (u32, u32) {
        match rom_window(addr) {
            Some(window) => (
                self.rom_access_cost(window, width, false),
                self.rom_access_cost(window, width, true),
            ),
            None => {
                let cost = self.internal_cycles(addr, width, false);
                (cost, cost)
            }
        }
    }

    /// The cycles one side of a DMA transfer takes for `units` units of `width` bytes, starting at
    /// `addr` and stepping by `step`: the first unit at the region's non-sequential cost and every
    /// later one at its sequential cost — except a unit that opens a 128 KiB Game Pak block, which
    /// is non-sequential however the unit before it ended. GBATEK, *GBA GamePak Prefetch → NOTES*:
    /// "The GBA forcefully uses non-sequential timing at the beginning of each 128K-block of gamepak
    /// ROM, eg. "LDMIA [801fff8h],r0-r7" will have non-sequential timing at 8020000h."
    fn dma_half_total(&self, addr: u32, width: usize, step: u32, units: u32) -> u32 {
        let (first, sequential) = self.dma_half_costs(addr, width);
        (0..units)
            .map(|index| {
                let at = addr.wrapping_add(index.wrapping_mul(step));
                if index == 0 || (rom_window(at).is_some() && at.is_multiple_of(ROM_BLOCK_SIZE)) {
                    first
                } else {
                    sequential
                }
            })
            .sum()
    }

    /// The timing state a save state has to carry: the prefetch buffer's position and depth, and the
    /// two addresses that decide whether the next access is sequential. `u32::MAX` is `None`.
    pub fn encode_timing(&self) -> [u8; 20] {
        let mut bytes = [0u8; 20];
        for (index, value) in [
            self.prefetch_next.unwrap_or(u32::MAX),
            self.last_fetch_end.unwrap_or(u32::MAX),
            self.last_data_end.unwrap_or(u32::MAX),
            self.waits,
            u32::from(self.prefetch_count),
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Restore [`Bus::encode_timing`]'s body.
    pub fn decode_timing(&mut self, bytes: &[u8; 20]) {
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let none = |value: u32| Some(value).filter(|&value| value != u32::MAX);
        self.prefetch_next = none(word(0));
        self.last_fetch_end = none(word(4));
        self.last_data_end = none(word(8));
        self.waits = word(12);
        self.prefetch_count = word(16).min(u32::from(PREFETCH_HALFWORDS)) as u8;
    }

    /// The four timers as this state's payload holds them: reload, counter, control and prescaler
    /// remainder each, 8 bytes a timer. `TMxCNT_L` and `TMxCNT_H` are here rather than in the I/O
    /// page because [`Timers`] answers their reads.
    pub fn encode_timers(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for (index, timer) in self.timers.t.iter().enumerate() {
            let at = index * 8;
            bytes[at..at + 2].copy_from_slice(&timer.reload.to_le_bytes());
            bytes[at + 2..at + 4].copy_from_slice(&timer.counter.to_le_bytes());
            bytes[at + 4..at + 6].copy_from_slice(&timer.control.to_le_bytes());
            bytes[at + 6..at + 8].copy_from_slice(&(timer.sub as u16).to_le_bytes());
        }
        bytes
    }

    /// Restore [`Bus::encode_timers`]'s body.
    pub fn decode_timers(&mut self, bytes: &[u8; 32]) {
        for (index, timer) in self.timers.t.iter_mut().enumerate() {
            let at = index * 8;
            timer.reload = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            timer.counter = u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]);
            timer.control = u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]);
            timer.sub = u32::from(u16::from_le_bytes([bytes[at + 6], bytes[at + 7]]));
        }
    }

    /// The cycles of one access inside the machine: GBATEK's table row for the region, which for
    /// the 16-bit memories depends on the access's width and nowhere else — except for the three
    /// video memories, whose rows carry the table's `*` footnote. `video` is true on the CPU's own
    /// path; a DMA transfer is not the "GBA" the footnote's row is about, and is priced without it.
    fn internal_cycles(&self, addr: u32, width: usize, video: bool) -> u32 {
        // "* Plus 1 cycle if GBA accesses video memory at the same time." The display controller
        // reads palette RAM, VRAM and OAM while it draws (GBATEK, *VRAM, OAM, and Palette RAM
        // Access*: "a waitstate (one clock cycle) being inserted automatically in case that the
        // display controller was accessing memory simultaneously"), so an access to any of the
        // three costs one more cycle then.
        let contended = video && self.video_penalty();
        match addr >> 24 {
            // BIOS ROM 32-bit 1/1/1, Work RAM 32K 1/1/1, I/O 1/1/1, OAM 1/1/1 *.
            0x00 | 0x01 | 0x03 | 0x04 => 1,
            0x07 => 1 + u32::from(contended),
            // Work RAM 256K 16-bit: "3/3/6 **".
            0x02 => {
                if width >= 4 {
                    6
                } else {
                    3
                }
            }
            // Palette RAM and VRAM: "1/1/2 *".
            0x05 | 0x06 => {
                if width >= 4 {
                    2 + u32::from(contended)
                } else {
                    1 + u32::from(contended)
                }
            }
            // Game Pak SRAM, 8-bit: its `WAITCNT` field plus the one access cycle.
            0x0E => 1 + WAITS[usize::from(self.waitcnt() & 3)],
            // A read of nothing still takes a cycle.
            _ => 1,
        }
    }

    /// `DISPSTAT` as the CPU reads it: the stored bits 3-15 with the three status bits derived from
    /// the frame clock and the V-Count setting.
    pub fn dispstat(&self) -> u16 {
        let stored = u16::from_le_bytes([self.io[DISPSTAT], self.io[DISPSTAT + 1]]) & 0xFFF8;
        let mut value = stored;
        if self.lcd.in_vblank() {
            value |= 1 << 0;
        }
        if self.lcd.in_hblank() {
            value |= 1 << 1;
        }
        // Bit 2: the V-Counter flag, set while `VCOUNT` equals the V-Count setting (bits 8-15).
        let lyc = (stored >> 8) & 0xFF;
        if self.lcd.line & 0xFF == lyc {
            value |= 1 << 2;
        }
        value
    }

    /// `KEYINPUT`: bits 0-9 are the ten buttons, active low, and the six bits above them read 1
    /// (GBATEK, *4000130h - KEYINPUT*: "0-9 Key Status (0=Pressed, 1=Released)", the order being
    /// A, B, Select, Start, Right, Left, Up, Down, R, L).
    pub fn keyinput(&self) -> u16 {
        let b = self.buttons;
        let mut value = 0x03FFu16;
        for (bit, button) in [
            (0, Buttons::A),
            (1, Buttons::B),
            (2, Buttons::SELECT),
            (3, Buttons::START),
            (4, Buttons::RIGHT),
            (5, Buttons::LEFT),
            (6, Buttons::UP),
            (7, Buttons::DOWN),
        ] {
            if b.0 & button.0 != 0 {
                value &= !(1 << bit);
            }
        }
        // Bits 8 and 9 are R and L, which the Game Boy's `Buttons` has no room for (G7).
        if self.shoulders.0 & Shoulders::RIGHT.0 != 0 {
            value &= !(1 << 8);
        }
        if self.shoulders.0 & Shoulders::LEFT.0 != 0 {
            value &= !(1 << 9);
        }
        value
    }

    /// Set the pad the way the front end does, and let `KEYCNT` see the new state.
    pub fn set_buttons(&mut self, buttons: Buttons) {
        self.buttons = buttons;
        self.keypad_irq();
    }

    /// Set the two shoulder buttons (G7), and let `KEYCNT` see the new state.
    pub fn set_shoulders(&mut self, shoulders: Shoulders) {
        self.shoulders = shoulders;
        self.keypad_irq();
    }

    /// `KEYCNT` as the hardware reads it: bits 0-9 the keys that raise the request, bit 14 the
    /// request's enable, bit 15 its condition (GBATEK, *4000132h - KEYCNT*: "0-9 Keypad Button
    /// Mask", "14 Keypad Interrupt Enable", "15 Keypad Interrupt Condition (0=OR, 1=AND)"; the six
    /// bits between are unused and read 0).
    pub fn keycnt(&self) -> u16 {
        u16::from_le_bytes([self.io[KEYCNT], self.io[KEYCNT + 1]]) & 0xC3FF
    }

    /// `--trace-keycnt`: record every `KEYCNT` store from here on, with the frame it landed in and
    /// the keys that were held. The keypad-interrupt probe's instrument; a normal run
    /// never asks, and records nothing.
    pub fn trace_keycnt(&mut self) {
        self.keycnt_writes = Some(Vec::new());
    }

    /// A `KEYCNT` store: record it if a probe asked, then judge the condition.
    ///
    /// The record is taken before the condition is judged, so a probe sees the store as the
    /// cartridge made it whether or not it raised anything.
    fn keycnt_stored(&mut self) {
        let value = self.keycnt();
        let frame = self.lcd.frames;
        let cycles = self.cycles;
        let keys = self.keyinput();
        if let Some(writes) = self.keycnt_writes.as_mut() {
            writes.push(KeycntWrite {
                value,
                frame,
                cycles,
                keys,
            });
        }
        self.keypad_irq();
    }

    /// Raise the keypad request when `KEYCNT`'s condition is met.
    ///
    /// GBATEK's condition: with bit 15 clear "the interrupt is requested if any of the selected
    /// keys is pressed", with it set "if all of the selected keys are pressed". The compare is
    /// against the *pressed* state — `KEYINPUT` is active low, so the mask is inverted here — and a
    /// mask of none selects nothing, which no key can satisfy.
    fn keypad_irq(&mut self) {
        let control = self.keycnt();
        if control & 0x4000 == 0 {
            return;
        }
        let selected = control & 0x03FF;
        if selected == 0 {
            return;
        }
        let pressed = !self.keyinput() & 0x03FF;
        let matched = if control & 0x8000 != 0 {
            selected & pressed == selected
        } else {
            selected & pressed != 0
        };
        if matched {
            self.raise(irq::KEYPAD);
        }
    }

    /// True for an address in the save window, `$0E000000-$0FFFFFFF`.
    fn save_window(addr: u32) -> bool {
        matches!(addr >> 24, 0x0E | 0x0F)
    }

    /// True for the EEPROM's own window, `$0D000000-$0DFFFFFF`, on a cartridge whose save chip is
    /// an EEPROM — the one place that decides, as [`Bus::save_window`] is for the 8-bit chips.
    ///
    /// GBATEK, *GBA Cart Backup EEPROM*: "The eeprom is connected to Bit0 of the data bus, and to
    /// the upper 1 bit (or upper 17 bits in case of large 32MB ROM) of the cartridge ROM address
    /// bus", and "On carts with 16MB or smaller ROM, eeprom can be alternately accessed anywhere at
    /// D000000h-DFFFFFFh". A cartridge with any other save chip — and one with none — keeps the ROM
    /// mirror it always showed there, which is why this asks about the chip and not the address.
    fn eeprom_window(&self, addr: u32) -> bool {
        (addr >> 24) == 0x0D && matches!(self.save, Save::Eeprom(_))
    }

    /// One bit clocked out of the EEPROM by a read of its window.
    fn eeprom_read_bit(&mut self) -> bool {
        let cycles = self.cycles;
        match self.save.eeprom() {
            Some(eeprom) => eeprom.read_bit(cycles),
            // Unreachable: `eeprom_window` only routes here when the chip is an EEPROM. The Ready
            // bit reads 1 so that a poll can never hang on it.
            None => true,
        }
    }

    /// One bit clocked into the EEPROM by a write of its window.
    fn eeprom_write_bit(&mut self, bit: bool) {
        let cycles = self.cycles;
        if let Some(eeprom) = self.save.eeprom() {
            eeprom.write_bit(bit, cycles);
        }
    }

    /// The RTC cart's GPIO port value at `addr`, `None` if `addr` is not one of the three 16-bit
    /// ports (`$80000C4`, `C6`, `C8`) or the cartridge has no RTC.
    fn gpio_read(&self, addr: u32) -> Option<u16> {
        let rtc = self.rtc.as_ref()?;
        let rel = addr.wrapping_sub(GPIO_BASE);
        (rel < 6).then(|| rtc.gpio.read((rel & !1) as u16))
    }

    /// A write to the RTC cart's GPIO port, driving the serial interface on a data-port write.
    /// Returns whether `addr` was a GPIO port (so the caller knows not to touch ROM).
    fn gpio_write(&mut self, addr: u32, value: u16) -> bool {
        let Some(rtc) = self.rtc.as_mut() else {
            return false;
        };
        let rel = addr.wrapping_sub(GPIO_BASE);
        if rel >= 6 {
            return false;
        }
        let offset = (rel & !1) as u16;
        rtc.gpio.write(offset, value);
        if offset == 0 {
            // A data-port write clocks the serial interface; the RTC may drive SIO back, which the
            // next read of the port sees (bit 1).
            let sio = rtc.gpio_write(rtc.gpio.data);
            rtc.gpio.data = (rtc.gpio.data & !0x2) | (u16::from(sio) << 1);
        }
        true
    }

    /// One byte at `addr`. An address the hardware does not decode returns the open bus: the byte
    /// of the latch the address selects, and — as on hardware, where a read of nothing puts nothing
    /// new on the bus — leaves the latch as it was.
    pub fn read8(&mut self, addr: u32) -> u8 {
        let value = self.lane8(addr);
        self.charge(addr, 1, false);
        value
    }

    /// The byte an access at `addr` reads, before its cost is charged.
    fn lane8(&mut self, addr: u32) -> u8 {
        // GBATEK, *Reading from BIOS Memory (00000000-00003FFF)*: "the GBA allows to read opcodes or
        // data only if the program counter is located inside of the BIOS area. If the program
        // counter is not in the BIOS area, reading will return the most recent successfully fetched
        // BIOS opcode". Our program counter is never inside it — the calls are `bios.rs` — so every
        // read of the region hands back that word.
        if is_bios(addr) {
            return (self.bios.latch >> (8 * (addr & 3))) as u8;
        }
        match self.mapped8(addr) {
            Some(value) => {
                self.latch(addr, value);
                value
            }
            None => (self.open_bus >> (8 * (addr & 3))) as u8,
        }
    }

    /// Charge one access at `addr` of `width` bytes: the cycles GBATEK's table gives for it, less the
    /// one cycle the core's flat `S`/`N` model already counted.
    fn charge(&mut self, addr: u32, width: usize, fetch: bool) {
        let total = self.access_cycles(addr, width, fetch);
        self.waits += total.saturating_sub(1);
    }

    /// A store's share of the wait bill.
    ///
    /// GBATEK, *ARM CPU Instruction Cycle Times*, on splitting the figure between code and data:
    /// "1N is used for data access, plus (n-1)S for LDM/STM, the remaining S+N are code access" —
    /// one bus transfer is the data access. The store opcodes print `STR 2N`, and both of those
    /// cycles are that one transfer (the code cycle is the fetch, charged where it is made), so the
    /// region's documented cost replaces the two clock cycles the flat `2N` already counted.
    fn charge_store(&mut self, addr: u32, width: usize) {
        let total = self.access_cycles(addr, width, false);
        self.waits += total.saturating_sub(2);
    }

    /// The word at `addr` in the three RAMs that can hold code, read with no cost, no open-bus
    /// latch and no side effect: the pipeline's view of an opcode that is already in flight.
    /// Anything else (ROM cannot be written, I/O reads have effects) answers 0.
    pub fn peek_code(&self, addr: u32) -> u32 {
        let byte = |a: u32| -> u32 {
            u32::from(match a >> 24 {
                0x02 => self.ewram[(a as usize) & (EWRAM_SIZE - 1)],
                0x03 => self.iwram[(a as usize) & (IWRAM_SIZE - 1)],
                0x06 => self.vram_offset(a).map_or(0, |o| self.vram[o]),
                _ => 0,
            })
        };
        (0..4).fold(0, |word, i| word | byte(addr.wrapping_add(i)) << (8 * i))
    }

    /// The cycles GBATEK's table gives one store of `width` bytes at `addr` — the figure
    /// [`Bus::charge_store`] prices a store instruction's data access from, exposed for the code
    /// that clears a whole region and has to say what that many stores cost (a BIOS
    /// `RegisterRamReset`, whose clears are 65 536 word stores and up).
    pub fn store_cycles(&mut self, addr: u32, width: usize) -> u32 {
        self.access_cycles(addr, width, false)
    }

    /// An opcode fetch of a halfword, charged as a fetch rather than as data: this is what the Game
    /// Pak prefetch buffer helps, and the only reason the two paths differ.
    pub fn fetch16(&mut self, addr: u32) -> u16 {
        let lo = self.lane8(addr);
        let hi = self.lane8(addr.wrapping_add(1));
        self.charge(addr, 2, true);
        u16::from_le_bytes([lo, hi])
    }

    /// An opcode fetch of a word, for ARM state.
    pub fn fetch32(&mut self, addr: u32) -> u32 {
        let bytes = [
            self.lane8(addr),
            self.lane8(addr.wrapping_add(1)),
            self.lane8(addr.wrapping_add(2)),
            self.lane8(addr.wrapping_add(3)),
        ];
        self.charge(addr, 4, true);
        u32::from_le_bytes(bytes)
    }

    /// Put `value` on the data bus at the byte lane `addr` selects. A word access is four byte
    /// accesses in ascending order, so the four lanes of the latch end up holding that word.
    fn latch(&mut self, addr: u32, value: u8) {
        let shift = 8 * (addr & 3);
        let lane = 0xFFu32 << shift;
        self.open_bus = (self.open_bus & !lane) | (u32::from(value) << shift);
    }

    /// The byte `addr` decodes to, or `None` when nothing lives there. Every region of GBATEK's map
    /// is here, with its mirroring; the decoding itself lives in one place so a read and a write can
    /// never disagree about what a region's size is.
    fn mapped8(&mut self, addr: u32) -> Option<u8> {
        Some(match addr >> 24 {
            // BIOS: 16 KiB of system ROM. Reads of it are answered in `lane8` from the opcode
            // latch, which is what the hardware's read protection leaves a cartridge; the region
            // past it, and the rest of 00000000-01FFFFFF, is not decoded.
            0x00 | 0x01 => return None,
            // On-board work RAM, mirrored every 256 KiB.
            0x02 => self.ewram[(addr as usize) & (EWRAM_SIZE - 1)],
            // On-chip work RAM, mirrored every 32 KiB.
            0x03 => self.iwram[(addr as usize) & (IWRAM_SIZE - 1)],
            0x04 => {
                let offset = (addr & 0xFF_FFFF) as usize;
                if offset >= IO_SIZE {
                    return None;
                }
                self.io_read8(offset)
            }
            // Palette RAM, mirrored every 1 KiB.
            0x05 => self.palette[(addr as usize) & (PALETTE_SIZE - 1)],
            // VRAM, in the layout the current BG mode gives it.
            0x06 => self.vram[self.vram_offset(addr)?],
            // OAM, mirrored every 1 KiB.
            0x07 => self.oam[(addr as usize) & (OAM_SIZE - 1)],
            // The three ROM windows, each showing the cartridge from its own offset zero. The top
            // half of the third is the EEPROM's own window on a cartridge that has one.
            0x08 | 0x09 => return self.rom_byte((addr - ROM_BASE) as usize),
            0x0A | 0x0B => return self.rom_byte((addr - ROM_WS1_BASE) as usize),
            0x0C => return self.rom_byte((addr - ROM_WS2_BASE) as usize),
            // The EEPROM drives bit 0 of the bus and nothing else, so only the even byte of a
            // halfword carries it: a halfword read clocks exactly one bit out of the chip, and the
            // upper byte a halfword read also asks for is the don't-care half GBATEK describes.
            0x0D if self.eeprom_window(addr) => {
                return Some(if addr & 1 == 0 {
                    u8::from(self.eeprom_read_bit())
                } else {
                    0
                });
            }
            0x0D => return self.rom_byte((addr - ROM_WS2_BASE) as usize),
            // Game Pak SRAM: an 8-bit chip that sees the low byte of the address, mirrored across
            // `$0E000000-$0FFFFFFF` (GBATEK: "The 64K SRAM area is mirrored across the whole 32MB
            // area"). The chip behind it is `save`'s.
            0x0E | 0x0F => return Some(self.save.read8(addr, self.cycles)),
            // Everything else — the 4 KiB past the BIOS, the holes inside each region, and the whole
            // upper half of the address space — is not decoded by the hardware.
            _ => return None,
        })
    }

    /// Where in the 96 KiB of VRAM `addr` lands, or `None` if it lands in a hole.
    ///
    /// GBATEK, *LCD VRAM Overview*: tiled modes use `$00000-$0FFFF` for BG data and
    /// `$10000-$17FFF` for object tiles; bitmap modes use all 96 KiB linearly. The public suite pins
    /// the mirroring — `memory.gba`'s test 5 writes `$06010000` in a tiled mode and reads
    /// `$06018000`, so the 32 KiB object window repeats once at `$18000`, while its test 4 wants the
    /// whole of VRAM to repeat at `$20000` in a bitmap mode.
    fn vram_offset(&self, addr: u32) -> Option<usize> {
        let offset = (addr as usize) & (VRAM_WINDOW - 1);
        if !self.bitmap_mode() && offset >= VRAM_SIZE {
            // The object-tile window's mirror: $18000 shows $10000, which is where object tiles
            // live in a tiled mode.
            return Some(offset - 0x8000);
        }
        (offset < VRAM_SIZE).then_some(offset)
    }

    /// One byte at `addr`, with the video memories' 8-bit-write rules.
    ///
    /// Writes to cartridge ROM and to undecoded space are ignored, not panics. Palette RAM, VRAM and
    /// OAM have no 8-bit write (GBATEK's *Address Bus Width* table lists `16/32` under Write for all
    /// three), and what the hardware does instead is what `memory.gba` tests: the byte reaches the
    /// 16-bit bus on both halves, so it is written twice, except that in a tiled BG mode and in OAM
    /// nothing is written at all.
    pub fn write8(&mut self, addr: u32, value: u8) {
        match addr >> 24 {
            0x05 => {
                let half = u16::from_le_bytes([value, value]);
                self.store16(addr & !1, half);
            }
            0x06 => {
                if self.bitmap_mode() {
                    let half = u16::from_le_bytes([value, value]);
                    self.store16(addr & !1, half);
                } else if ((addr as usize) & (VRAM_WINDOW - 1)) < 0x1_0000 {
                    // A tiled mode's BG area is byte-addressable — the map and 4bpp tile data are
                    // byte-oriented, and `shades.gba` fills its tiles with `strb`. The object area
                    // above it is not: `memory.gba` test 52 writes `06010010h` a byte at a time in
                    // mode 0 and the halfword stays zero.
                    self.store8(addr, value);
                }
            }
            0x07 => {}
            _ => self.store8(addr, value),
        }
        self.latch(addr, value);
        self.charge_store(addr, 1);
    }

    /// The store `write8` performs when the region takes a byte.
    fn store8(&mut self, addr: u32, value: u8) {
        match addr >> 24 {
            0x02 => self.ewram[(addr as usize) & (EWRAM_SIZE - 1)] = value,
            0x03 => self.iwram[(addr as usize) & (IWRAM_SIZE - 1)] = value,
            0x04 => {
                let offset = (addr & 0xFF_FFFF) as usize;
                if offset < IO_SIZE {
                    self.io_write8(offset, value);
                }
            }
            0x05 => self.palette[(addr as usize) & (PALETTE_SIZE - 1)] = value,
            0x06 => {
                if let Some(offset) = self.vram_offset(addr) {
                    self.vram[offset] = value;
                }
            }
            0x07 => self.oam[(addr as usize) & (OAM_SIZE - 1)] = value,
            // The EEPROM is one bit wide and clocked by the byte lane that carries bit 0: a halfword
            // store lands as two byte stores, so exactly one bit reaches the chip. GBATEK, *Using
            // DMA*: "one halfword for each bit, bit1-15 of the halfwords are don't care, only bit0
            // is used".
            0x0D if self.eeprom_window(addr) => {
                if addr & 1 == 0 {
                    self.eeprom_write_bit(value & 1 != 0);
                }
            }
            0x0E | 0x0F => self.save.write8(addr, value, self.cycles),
            // ROM is read-only, and an address the hardware does not decode swallows the write.
            _ => {}
        }
    }

    /// A halfword store, which every region accepts.
    fn store16(&mut self, addr: u32, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        if (addr >> 24) == 0x04 && (addr & 0xFF_FFFF) as usize == KEYCNT {
            // `KEYCNT` is one register, so a halfword store to it is one access: both bytes land
            // and the condition is judged once. Judging the low byte against the old control word
            // would raise a request the completed store suppresses (a game disarming an AND watch
            // while the keys are held is the ordinary case).
            self.io[KEYCNT..KEYCNT + 2].copy_from_slice(&[lo, hi]);
            self.keycnt_stored();
            return;
        }
        // The link port's two-byte registers whose halves are one write: `SIOCNT`'s start bit must
        // not run before its mode bits have landed, and a `SIOMLT_SEND` halfword is one value on
        // the cable rather than two.
        if (addr >> 24) == 0x04 {
            let offset = (addr & 0xFF_FFFF) as usize;
            if serial::owns16(offset) {
                self.serial.write16(&mut self.io, offset, value);
                return;
            }
        }
        self.store8(addr, lo);
        self.store8(addr.wrapping_add(1), hi);
    }

    /// One halfword at `addr`. The save window returns its single byte replicated on both halves
    /// (the 8-bit chip on the 16-bit bus), and the RTC's GPIO ports answer in the ROM area.
    pub fn read16(&mut self, addr: u32) -> u16 {
        let value = if Self::save_window(addr) {
            let value = self.save.read16(addr, self.cycles);
            let byte = value as u8;
            self.latch(addr, byte);
            self.latch(addr.wrapping_add(1), byte);
            value
        } else if let Some(value) = self.gpio_read(addr) {
            self.latch(addr, value as u8);
            self.latch(addr.wrapping_add(1), (value >> 8) as u8);
            value
        } else {
            let lo = self.lane8(addr);
            let hi = self.lane8(addr.wrapping_add(1));
            u16::from_le_bytes([lo, hi])
        };
        self.charge(addr, 2, false);
        value
    }

    /// One word at `addr`. The save window replicates its byte across all four lanes.
    pub fn read32(&mut self, addr: u32) -> u32 {
        let value = if Self::save_window(addr) {
            let value = self.save.read32(addr, self.cycles);
            let byte = value as u8;
            for lane in 0..4 {
                self.latch(addr.wrapping_add(lane), byte);
            }
            value
        } else if let Some(lo) = self.gpio_read(addr) {
            // A word covers two ports: data and direction, or control and the next byte.
            let hi = self.gpio_read(addr.wrapping_add(2)).unwrap_or(0);
            let value = u32::from(lo) | (u32::from(hi) << 16);
            for lane in 0..4 {
                self.latch(addr.wrapping_add(lane), value.to_le_bytes()[lane as usize]);
            }
            value
        } else {
            let bytes = [
                self.lane8(addr),
                self.lane8(addr.wrapping_add(1)),
                self.lane8(addr.wrapping_add(2)),
                self.lane8(addr.wrapping_add(3)),
            ];
            u32::from_le_bytes(bytes)
        };
        self.charge(addr, 4, false);
        value
    }

    /// One halfword to `addr`. The save window takes the byte the address's lane selects (the 8-bit
    /// chip), and the RTC's GPIO ports answer in the ROM area. Everywhere else the write is
    /// force-aligned down to an even address (GBATEK, *Mis-aligned STR,STRH*), so the full address
    /// the CPU hands over is masked here — not in the CPU, which must keep the low bit for the save
    /// window's byte lane.
    pub fn write16(&mut self, addr: u32, value: u16) {
        if Self::save_window(addr) {
            self.save.write16(addr, value, self.cycles);
        } else if !self.gpio_write(addr, value) {
            self.store16(addr & !1, value);
        }
        let [lo, hi] = value.to_le_bytes();
        self.latch(addr, lo);
        self.latch(addr.wrapping_add(1), hi);
        self.charge_store(addr, 2);
    }

    /// One word to `addr`. The save window takes one byte; a GPIO word covers two ports. Everywhere
    /// else the write is force-aligned down to a four-byte boundary (GBATEK, *Mis-aligned STR,STRH*).
    pub fn write32(&mut self, addr: u32, value: u32) {
        let bytes = value.to_le_bytes();
        // Two halfword stores: GBATEK's instruction timing treats a word store as one bus access,
        // and every region this slice has takes a halfword wherever it takes a word.
        if Self::save_window(addr) {
            self.save.write32(addr, value, self.cycles);
        } else if self.gpio_write(addr, u16::from_le_bytes([bytes[0], bytes[1]])) {
            self.gpio_write(
                addr.wrapping_add(2),
                u16::from_le_bytes([bytes[2], bytes[3]]),
            );
        } else {
            let aligned = addr & !3;
            self.store16(aligned, u16::from_le_bytes([bytes[0], bytes[1]]));
            self.store16(
                aligned.wrapping_add(2),
                u16::from_le_bytes([bytes[2], bytes[3]]),
            );
        }
        for (index, byte) in bytes.into_iter().enumerate() {
            self.latch(addr.wrapping_add(index as u32), byte);
        }
        // A word store to `$04000000` is the display control word and nothing else on the GBA: the
        // `$04000000`-starts-a-channel rule is the DS's, not this machine's (GBATEK, *40000BAh*:
        // timing 3 is the sound FIFOs on DMA1/2 and video capture on DMA3).
        self.charge_store(addr, 4);
    }

    /// A byte of ROM at a cartridge offset, folding an offset past the end back into the ROM. The
    /// hardware does the same — GBATEK's *Cart Protections* note that games depend on those
    /// "ROM mirrors (instead of the usual increasing numbers in unused ROM area)".
    fn rom_byte(&self, offset: usize) -> Option<u8> {
        let len = self.cart.len();
        if len == 0 {
            return None;
        }
        Some(self.cart.read(offset % len))
    }

    /// One byte of the I/O page, with the derived registers answered instead of stored.
    fn io_read8(&mut self, offset: usize) -> u8 {
        match offset {
            DISPSTAT => self.dispstat() as u8,
            DISPSTAT_HI => (self.dispstat() >> 8) as u8,
            VCOUNT => self.lcd.line as u8,
            VCOUNT_HI => (self.lcd.line >> 8) as u8,
            KEYINPUT => self.keyinput() as u8,
            KEYINPUT_HI => (self.keyinput() >> 8) as u8,
            KEYCNT => self.keycnt() as u8,
            KEYCNT_HI => (self.keycnt() >> 8) as u8,
            // The link port answers its own registers: `SIOCNT`'s busy and line bits are live, and
            // a read of `JOY_RECV` clears a `JOYSTAT` flag.
            _ if serial::owns(offset) => self.serial.read8(&mut self.io, offset),
            // `TMxCNT_L` reads the *counter*, which no store ever wrote; the timers own both halves
            // of their register pair.
            _ if Timers::owns(offset) => self.timers.read8(offset),
            // The DMA channels' registers are storage in `io`; only their meaning is decoded.
            _ if Dma::owns(offset) => self.dma.read8(offset),
            // The sound block answers its own registers: the FIFOs are write-only and
            // `SOUNDCNT_X`'s low bits are live channel status.
            _ if Sound::owns(offset) => self.sound.read8(offset),
            _ => self.io[offset],
        }
    }

    /// One byte of the I/O page. The read-only registers stay as they were: `VCOUNT` and `KEYINPUT`
    /// ignore the write entirely, `DISPSTAT`'s three status bits are not storage, `IF` acknowledges
    /// by clearing, and `WAITCNT`'s bits 13 and 15 are not writable.
    fn io_write8(&mut self, offset: usize, value: u8) {
        match offset {
            VCOUNT | VCOUNT_HI | KEYINPUT | KEYINPUT_HI => {}
            // `KEYCNT` is storage, and a write can make its condition true while the keys are
            // already held — GBATEK: the request rises "when the condition is met", and arming the
            // watch is part of meeting it.
            KEYCNT | KEYCNT_HI => {
                self.io[offset] = value;
                self.keycnt_stored();
            }
            DISPSTAT => self.io[DISPSTAT] = value & 0xF8,
            // GBATEK, *4000202h - IF*: "Interrupts must be manually acknowledged by writing a "1" to
            // one of the IRQ bits, the IRQ bit will then be cleared."
            IF => self.io[IF] &= !value,
            IF_HI => self.io[IF_HI] &= !(value & 0x3F),
            // A rewrite of the wait-state control changes the waits the buffer was filled at, so what
            // it holds is no longer what the Game Pak would hand over; the hardware refills it.
            WAITCNT => {
                self.io[WAITCNT] = value;
                self.invalidate_prefetch();
            }
            WAITCNT_HI => {
                self.io[WAITCNT_HI] = value & 0x5F;
                self.invalidate_prefetch();
            }
            // The link port's registers: a store of `SIOCNT`'s start bit arms the engine, and a
            // multiplayer unit's send value goes onto the cable as it is written.
            _ if serial::owns(offset) => self.serial.write8(&mut self.io, offset, value),
            _ if Timers::owns(offset) => self.timers.write8(offset, value),
            // The sound block owns `$060-$0A7`: a FIFO store is a sample byte, and `SOUNDCNT_H`'s
            // reset bits clear a FIFO.
            _ if Sound::owns(offset) => self.sound.write8(offset, value),
            // A DMA control word's enable bit starts a transfer with an immediate start timing.
            _ if Dma::owns(offset) => {
                if let Some(index) = self.dma.write8(offset, value) {
                    self.start_written_dma(index);
                }
            }
            // GBATEK, *4000301h - HALTCNT*: "Writing to this register switches the GBA into battery
            // saving mode. In Halt mode, the CPU is paused as long as (IE AND IF)=0." The store is
            // recorded and the CPU stops between instructions; bit 7's Stop mode stops the CPU here
            // too, because nothing this slice has is clocked by something a Stop would switch off.
            HALTCNT => {
                self.io[HALTCNT] = value;
                self.halt_request = true;
            }
            _ => self.io[offset] = value,
        }
    }
}

/// True for an address inside the 16 KiB the BIOS owns.
pub fn is_bios(addr: u32) -> bool {
    addr < BIOS_LEN
}

/// The Game Pak ROM window `addr` lands in, 0-2, or `None` for anything else. The three windows are
/// the same cartridge at `$08000000`, `$0A000000` and `$0C000000` (GBATEK, *GBA Memory Map*).
fn rom_window(addr: u32) -> Option<usize> {
    match addr >> 24 {
        0x08 | 0x09 => Some(0),
        0x0A | 0x0B => Some(1),
        0x0C | 0x0D => Some(2),
        _ => None,
    }
}

/// The number of 16-bit fragments a Game Pak access of `width` bytes is split into.
///
/// GBATEK, *GBA Memory Map → GBA GamePak ROM Waitstates*: "GamePak uses 16bit data bus, so that a
/// 32bit access is split into TWO 16bit accesses (of which, the second fragment is always
/// sequential, even if the first fragment was non-sequential)."
fn rom_fragments(width: usize) -> usize {
    width.div_ceil(2)
}

/// The cost of one 16-bit fragment of a Game Pak access: `first` when it opens a non-sequential
/// access, `second` for every fragment after it — "the second fragment is always sequential, even
/// if the first fragment was non-sequential" ([`rom_fragments`]).
fn rom_fragment_cost(first: u32, second: u32, fragment: usize, sequential: bool) -> u32 {
    if fragment > 0 || sequential {
        second
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Bus {
        let rom = vec![0u8; 0x100];
        // A cartridge cannot be built from a zero header, so build the bytes directly: these tests
        // are about the map, not the header.
        let cart = Cartridge {
            rom,
            header: crate::cart::Header {
                entry: 0xEA00_002E,
                entry_point: ROM_BASE + 0xC0,
                title: "TEST".to_owned(),
                game_code: "TEST".to_owned(),
                maker: "00".to_owned(),
                version: 0,
                rom_bytes: 0x100,
                complement: 0,
                complement_ok: true,
                logo_ok: true,
            },
            crc32: 0,
        };
        Bus::new(cart)
    }

    #[test]
    fn one_frame_is_two_hundred_and_twenty_eight_lines_of_one_thousand_two_hundred_and_thirty_two_cycles()
     {
        assert_eq!(
            CYCLES_PER_LINE * u32::from(LINES_PER_FRAME),
            CYCLES_PER_FRAME
        );
        assert_eq!(CYCLES_PER_FRAME, 280_896);
        let mut lcd = Lcd::new();
        lcd.tick(CYCLES_PER_FRAME);
        assert_eq!(lcd.frames, 1);
        assert_eq!(lcd.line, 0);
        assert_eq!(lcd.line_cycle, 0);
        // Ticked a line at a time, the same thing arrives at the same place.
        let mut lcd = Lcd::new();
        for _ in 0..LINES_PER_FRAME {
            lcd.tick(CYCLES_PER_LINE);
        }
        assert_eq!(lcd.frames, 1);
        assert_eq!(lcd.line, 0);
    }

    #[test]
    fn the_vblank_flag_rises_on_line_160_and_falls_after_line_226() {
        let mut bus = bus();
        let mut seen = Vec::new();
        for _ in 0..LINES_PER_FRAME {
            let line = bus.lcd.line;
            seen.push((line, bus.dispstat() & 1));
            bus.lcd.tick(CYCLES_PER_LINE);
        }
        for (line, flag) in seen {
            let want = u16::from((160..=226).contains(&line));
            assert_eq!(flag, want, "VBlank flag on line {line}");
        }
        // And `VCOUNT` counts 0-227, then wraps.
        assert_eq!(bus.lcd.frames, 1);
        assert_eq!(bus.read16(IO_BASE + VCOUNT as u32), 0);
    }

    #[test]
    fn the_hblank_flag_and_the_hblank_event_share_one_line_range() {
        // GBATEK, *DISPSTAT*: the H-Blank flag is "set in lines 2-159,162-225". Lines 0-1 and the
        // V-Blank lines either side of them have neither the flag nor the event; the predicate is
        // shared, so this test pins both sides at once.
        let mut lcd = Lcd::new();
        for line in 0..LINES_PER_FRAME {
            lcd.line = line;
            lcd.line_cycle = HBLANK_DRAW_CYCLES;
            let want = matches!(line, 2..=159 | 162..=225);
            assert_eq!(lcd.in_hblank(), want, "H-Blank flag on line {line}");
            assert_eq!(hblank_line(line), want, "H-Blank event on line {line}");
            lcd.line_cycle = HBLANK_DRAW_CYCLES - 1;
            assert!(
                !lcd.in_hblank(),
                "the flag has not risen yet on line {line}"
            );
        }
    }

    #[test]
    fn vcount_counts_zero_to_two_hundred_and_twenty_seven() {
        let mut bus = bus();
        let mut seen = Vec::new();
        for _ in 0..LINES_PER_FRAME {
            seen.push(bus.read16(IO_BASE + VCOUNT as u32));
            bus.lcd.tick(CYCLES_PER_LINE);
        }
        assert_eq!(seen[0], 0);
        assert_eq!(seen[159], 159);
        assert_eq!(seen[160], 160);
        assert_eq!(seen[227], 227);
        assert_eq!(bus.read16(IO_BASE + VCOUNT as u32), 0, "wrapped");
    }

    #[test]
    fn ewram_is_two_hundred_and_fifty_six_kib_and_mirrors() {
        let mut bus = bus();
        bus.write8(EWRAM_BASE, 0x5A);
        assert_eq!(bus.read8(EWRAM_BASE), 0x5A);
        assert_eq!(
            bus.read8(EWRAM_BASE + 0x4_0000),
            0x5A,
            "mirrored every 256 KiB"
        );
        assert_eq!(bus.read8(EWRAM_BASE + 0xF_C0000), 0x5A, "and at $02FC0000");
        assert_eq!(
            bus.read8(EWRAM_BASE + 0x3_FFFF),
            0x00,
            "the last byte is its own byte"
        );
        // The last byte of each 256 KiB copy is the same byte, wherever the region is entered.
        bus.write8(EWRAM_BASE + 0x3_FFFF, 0xA5);
        assert_eq!(bus.read8(EWRAM_BASE + 0x3_FFFF), 0xA5);
        assert_eq!(
            bus.read8(0x02FF_FFFF),
            0xA5,
            "the top of the region is that byte"
        );
    }

    #[test]
    fn iwram_is_thirty_two_kib_and_mirrors_every_thirty_two_kib() {
        let mut bus = bus();
        bus.write8(IWRAM_BASE + 4, 0xAB);
        assert_eq!(bus.read8(IWRAM_BASE + 4), 0xAB);
        assert_eq!(
            bus.read8(IWRAM_BASE + 0x8004),
            0xAB,
            "mirrored every 32 KiB"
        );
        assert_eq!(bus.read8(IWRAM_BASE + 0xF_8004), 0xAB, "and at $030F8004");
        bus.write8(IWRAM_BASE + 0x7FFF, 0x12);
        assert_eq!(bus.read8(IWRAM_BASE + 0x7FFF), 0x12, "the last real byte");
        assert_eq!(
            bus.read8(0x03FF_FFFF),
            0x12,
            "the last byte at the top of the region"
        );
        assert_eq!(bus.read8(IWRAM_BASE + 0xFFFF), 0x12, "wraps to it");
    }

    #[test]
    fn vram_palette_and_oam_have_their_sizes() {
        let mut bus = bus();
        // Halfwords, because these three regions have no 8-bit write (see
        // `a_byte_store_to_palette_or_bitmap_vram_is_written_twice`).
        bus.write16(VRAM_BASE, 0x1111);
        bus.write16(VRAM_BASE + (VRAM_SIZE as u32 - 2), 0x2222);
        assert_eq!(bus.read16(VRAM_BASE), 0x1111);
        assert_eq!(bus.read16(VRAM_BASE + (VRAM_SIZE as u32 - 2)), 0x2222);
        assert_eq!(VRAM_SIZE, 0x1_8000, "96 KiB");

        bus.write16(PALETTE_BASE, 0x3333);
        bus.write16(PALETTE_BASE + 0x3FE, 0x4444);
        assert_eq!(bus.read16(PALETTE_BASE), 0x3333);
        assert_eq!(bus.read16(PALETTE_BASE + 0x3FE), 0x4444);
        assert_eq!(
            bus.read16(PALETTE_BASE + 0x400),
            0x3333,
            "mirrored every 1 KiB"
        );

        bus.write16(OAM_BASE, 0x5555);
        bus.write16(OAM_BASE + 0x3FE, 0x6666);
        assert_eq!(bus.read16(OAM_BASE), 0x5555);
        assert_eq!(bus.read16(OAM_BASE + 0x3FE), 0x6666);
        assert_eq!(bus.read16(OAM_BASE + 0x400), 0x5555, "mirrored every 1 KiB");
    }

    #[test]
    fn the_save_window_reads_ff_with_no_chip_until_a_store_creates_one() {
        let mut bus = bus();
        // The `bus()` helper's ROM carries no ID string, so no chip is named: the window reads 0xFF
        // and stays that way for as long as the cartridge only reads it, which is what `none.gba`
        // asserts (it never writes).
        assert_eq!(bus.read8(SRAM_BASE), 0xFF);
        assert_eq!(bus.read16(SRAM_BASE), 0xFFFF, "a wide read replicates 0xFF");
        assert_eq!(bus.read32(SRAM_BASE), 0xFFFF_FFFF);
        assert_eq!(bus.read8(0x0F00_0000), 0xFF, "mirrored at $0F000000");
        assert!(
            matches!(bus.save, Save::None),
            "a read never creates a chip"
        );
        // A store is the proof: the first one creates the 32 KiB SRAM chip, and the byte is there.
        bus.write8(SRAM_BASE, 0x77);
        assert_eq!(bus.save.kind(), save::SaveType::Sram);
        assert_eq!(bus.read8(SRAM_BASE), 0x77, "the store landed");
        assert_eq!(
            bus.read8(SRAM_BASE + 0x1_0000),
            0x77,
            "mirrored at $0E010000"
        );
        assert_eq!(bus.read8(0x0F00_0000), 0x77, "and at $0F000000");
    }

    #[test]
    fn a_write_to_rom_is_ignored() {
        let mut bus = bus();
        bus.write8(ROM_BASE, 0x99);
        assert_eq!(bus.read8(ROM_BASE), 0x00);
        bus.write32(ROM_BASE, 0xDEAD_BEEF);
        assert_eq!(bus.read32(ROM_BASE), 0x0000_0000);
    }

    #[test]
    fn the_three_rom_windows_show_the_cartridge_from_its_start() {
        let mut bus = bus();
        for base in [ROM_BASE, ROM_WS1_BASE, ROM_WS2_BASE] {
            assert_eq!(bus.read8(base), 0x00, "window at {base:#010X}");
            assert_eq!(bus.read8(base + 0x0F), 0x00);
        }
        // A ROM of 0x100 bytes mirrors every 0x100 through a 32 MiB window.
        bus.cart.rom[0x10] = 0x3C;
        assert_eq!(bus.read8(ROM_BASE + 0x10), 0x3C);
        assert_eq!(bus.read8(ROM_BASE + 0x100 + 0x10), 0x3C, "folded back");
        assert_eq!(bus.read8(ROM_WS1_BASE + 0x10), 0x3C);
        assert_eq!(bus.read8(ROM_WS2_BASE + 0x110), 0x3C);
    }

    #[test]
    fn a_read_of_nothing_returns_the_last_value_on_the_bus() {
        let mut bus = bus();
        // Nothing on the bus yet.
        assert_eq!(bus.read32(0x0400_0400), 0x0000_0000);
        // A word written to work RAM is still there for a read of undecoded space.
        bus.write32(EWRAM_BASE, 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0400_0400), 0xDEAD_BEEF);
        assert_eq!(
            bus.read8(0x1000_0000),
            0xEF,
            "the low byte is the last byte latched"
        );
        // A word read from memory is latched too, byte by byte.
        bus.write32(EWRAM_BASE, 0x1234_5678);
        assert_eq!(bus.read32(EWRAM_BASE), 0x1234_5678);
        assert_eq!(bus.read32(0x1000_0000), 0x1234_5678);
    }

    #[test]
    fn a_read_of_the_bios_returns_the_opcode_the_hardware_remembers() {
        let mut bus = bus();
        // G2: the BIOS region is read-protected, and what a cartridge sees is the last opcode the
        // BIOS fetched — not the data bus (GBATEK, *Reading from BIOS Memory*).
        bus.write32(EWRAM_BASE, 0xCAFE_F00D);
        assert_eq!(bus.read32(BIOS_BASE), crate::bios::OPCODE_AFTER_BOOT);
        assert_eq!(
            bus.read32(BIOS_BASE + 0x1C),
            crate::bios::OPCODE_AFTER_BOOT,
            "the debug vector reads the same word"
        );
        // The 4 KiB past the BIOS is not decoded and is open bus, as everywhere else.
        assert_eq!(bus.read32(0x0000_5000), 0xCAFE_F00D, "past the BIOS");
    }

    #[test]
    fn dispstat_stores_its_irq_enables_and_keeps_its_status_bits_read_only() {
        let mut bus = bus();
        // Post-boot the three status bits are 0, 0 and the V-Counter match (VCOUNT 0 = LYC 0).
        assert_eq!(bus.dispstat(), 0x0004, "post-boot");
        bus.write16(IO_BASE + DISPSTAT as u32, 0xFFFF);
        // Bits 3,4,5 store; bits 0-2 are status; bit 6 is unused; the V-Count setting is 0xFF.
        assert_eq!(bus.read16(IO_BASE + DISPSTAT as u32) & 0xFFF8, 0xFFF8);
        assert_eq!(
            bus.read16(IO_BASE + DISPSTAT as u32) & 0b111,
            0,
            "line 0, no match, drawing"
        );
        // A V-Count match sets bit 2: LYC 0 matches the line the LCD is on, LYC 5 does not.
        bus.write16(IO_BASE + DISPSTAT as u32, 0x0000);
        assert_eq!(
            bus.read16(IO_BASE + DISPSTAT as u32) & 0b100,
            0b100,
            "LYC = 0 matches line 0"
        );
        bus.write16(IO_BASE + DISPSTAT as u32, 5 << 8);
        assert_eq!(
            bus.read16(IO_BASE + DISPSTAT as u32) & 0b100,
            0,
            "LYC = 5, line 0"
        );
        bus.lcd.tick(CYCLES_PER_LINE * 5);
        assert_eq!(
            bus.read16(IO_BASE + DISPSTAT as u32) & 0b100,
            0b100,
            "line 5"
        );
    }

    #[test]
    fn the_keypad_reads_active_low() {
        let mut bus = bus();
        assert_eq!(
            bus.read16(IO_BASE + KEYINPUT as u32),
            0x03FF,
            "nothing pressed"
        );
        bus.buttons = Buttons::A | Buttons::START | Buttons::LEFT;
        assert_eq!(
            bus.read16(IO_BASE + KEYINPUT as u32),
            0x03FF & !0x0001 & !0x0008 & !0x0020
        );
        // KEYINPUT is read-only.
        bus.write16(IO_BASE + KEYINPUT as u32, 0);
        assert_eq!(bus.read16(IO_BASE + KEYINPUT as u32) & 0x03FF, 0x03D6);
    }

    #[test]
    fn the_io_page_has_its_post_boot_values() {
        let mut bus = bus();
        assert_eq!(bus.read16(IO_BASE + DISPCNT as u32), 0x0000);
        // `DISPSTAT` is 0 apart from the V-Counter flag, which is up because `VCOUNT` is 0 and the
        // V-Count setting is 0: hardware sets that flag whenever the two match.
        assert_eq!(bus.read16(IO_BASE + DISPSTAT as u32), 0x0004);
        assert_eq!(bus.read16(IO_BASE + SOUNDBIAS as u32), 0x0200);
        assert_eq!(bus.read8(IO_BASE + POSTFLG as u32), 0x01);
        assert_eq!(bus.read16(IO_BASE + IE as u32), 0x0000);
        assert_eq!(bus.read16(IO_BASE + IF as u32), 0x0000);
        assert_eq!(bus.read16(IO_BASE + IME as u32), 0x0000);
    }

    #[test]
    fn an_interrupt_request_rises_from_a_source_and_is_acknowledged_by_writing_one() {
        let mut bus = bus();
        assert_eq!(bus.irq_flags(), 0, "nothing has asked yet");
        bus.raise(irq::VBLANK);
        assert_eq!(bus.irq_flags(), irq::bit(irq::VBLANK));
        // `IE` decides what reaches the CPU: the request is up, but nothing is enabled.
        assert_eq!(bus.requested(), 0);
        assert_eq!(bus.highest_request(), None);
        bus.write16(
            IO_BASE + IE as u32,
            irq::bit(irq::VBLANK) | irq::bit(irq::HBLANK),
        );
        assert_eq!(bus.requested(), irq::bit(irq::VBLANK));
        assert_eq!(bus.highest_request(), Some(irq::VBLANK));
        // `IME` is the master switch in front of both, and does not touch the flags.
        assert!(!bus.ime());
        bus.write16(IO_BASE + IME as u32, 1);
        assert!(bus.ime());
        assert_eq!(bus.irq_flags(), irq::bit(irq::VBLANK));
        // Writing a `1` acknowledges that bit and leaves the others alone (GBATEK, *IF*).
        bus.raise(irq::HBLANK);
        bus.write16(IO_BASE + IF as u32, irq::bit(irq::VBLANK));
        assert_eq!(
            bus.irq_flags(),
            irq::bit(irq::HBLANK),
            "VBlank cleared, HBlank still up"
        );
        assert_eq!(bus.highest_request(), Some(irq::HBLANK));
        // A byte write acknowledges the low eight bits and only those.
        bus.write8(IO_BASE + IF as u32, 0xFF);
        assert_eq!(bus.irq_flags(), 0);
        // Bits 14 and 15 are not requests: they never read back as set.
        bus.raise_interrupt(0xFFFF);
        assert_eq!(bus.irq_flags(), irq::IMPLEMENTED);
        bus.write16(IO_BASE + IF as u32, 0xFFFF);
        assert_eq!(bus.irq_flags(), 0);
    }

    #[test]
    fn the_wider_reads_are_little_endian_and_time_advances_the_lcd() {
        let mut bus = bus();
        bus.write32(IWRAM_BASE, 0x1122_3344);
        assert_eq!(bus.read8(IWRAM_BASE), 0x44);
        assert_eq!(bus.read16(IWRAM_BASE), 0x3344);
        assert_eq!(bus.read32(IWRAM_BASE), 0x1122_3344);
        bus.tick(CYCLES_PER_LINE);
        assert_eq!(bus.cycles, u64::from(CYCLES_PER_LINE));
        assert_eq!(bus.lcd.line, 1);
    }

    /// Program one channel the way the cartridge does: the three registers, then the control word
    /// whose enable bit starts it.
    fn program_dma(bus: &mut Bus, index: usize, sad: u32, dad: u32, count: u16, control: u16) {
        let base = IO_BASE + (dma::BASE + index * dma::STRIDE) as u32;
        bus.write32(base, sad);
        bus.write32(base + dma::DAD as u32, dad);
        bus.write16(base + dma::COUNT as u32, count);
        bus.write16(base + dma::CONTROL as u32, control);
    }

    /// Eight rising bytes at `at`, so a copy is visible byte by byte.
    fn fill(bus: &mut Bus, at: usize, len: usize) {
        for index in 0..len {
            bus.ewram[at + index] = (index as u8) ^ 0x5A;
        }
    }

    /// An IWRAM source with a fixed destination. `DMAnCNT_L` counts *units*, so 16 is sixteen
    /// halfwords (a 32-byte source walk), and a fixed destination can only keep the last unit.
    #[test]
    fn an_iwram_copy_to_a_fixed_destination_lands_where_it_is_told() {
        let mut bus = bus();
        for index in 0..32usize {
            bus.iwram[0x400 + index] = 0x40 + index as u8;
        }
        program_dma(
            &mut bus,
            1,
            IWRAM_BASE + 0x400,
            IWRAM_BASE + 0x800,
            16,
            dma::ENABLE | (1 << 12) | (0b10 << 5),
        );
        bus.tick(CYCLES_PER_LINE * 161);
        assert_eq!(
            bus.iwram[0x400], 0x40,
            "the source is where the test put it"
        );
        assert_eq!(
            bus.iwram[0x800..0x802],
            [0x5E, 0x5F],
            "the fixed destination holds the last unit, the source walked past the rest"
        );
        assert_eq!(bus.dma.channel(1).cursor_sad, IWRAM_BASE + 0x420);
    }

    #[test]
    fn an_immediate_transfer_copies_the_block_and_is_charged_to_the_instruction() {
        let mut bus = bus();
        fill(&mut bus, 0x100, 16);
        bus.waits = 0;
        program_dma(
            &mut bus,
            0,
            EWRAM_BASE + 0x100,
            IWRAM_BASE + 0x40,
            8,
            dma::ENABLE,
        );
        assert_eq!(
            &bus.iwram[0x40..0x50],
            &bus.ewram[0x100..0x110],
            "an immediate start runs inside the store that enabled it"
        );
        assert!(
            bus.waits > 0,
            "the transfer's cycles are owed by the instruction that asked for it"
        );
        assert!(
            !bus.dma.channel(0).enabled(),
            "a one-shot transfer disarms itself"
        );
        assert_eq!(bus.dma.channel(0).count, 8);
        assert_eq!(
            bus.dma.channel(0).dad,
            IWRAM_BASE + 0x40,
            "the address register keeps what was written: the walking address is internal"
        );
    }

    #[test]
    fn a_word_transfer_copies_words_not_halfwords() {
        let mut bus = bus();
        bus.ewram[0x200..0x204].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        bus.ewram[0x204..0x208].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        program_dma(
            &mut bus,
            3,
            EWRAM_BASE + 0x200,
            IWRAM_BASE + 0x80,
            2,
            dma::ENABLE | dma::WIDTH32,
        );
        assert_eq!(bus.read32(IWRAM_BASE + 0x80), 0xDEAD_BEEF);
        assert_eq!(bus.read32(IWRAM_BASE + 0x84), 0x1234_5678);
    }

    /// GBATEK, *Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP*: "The mis-aligned low bit(s) are
    /// ignored, the memory access goes to a forcibly aligned (rounded-down) memory address". A DMA
    /// unit is a halfword or word access like any other, so a source whose bit 0 is set — which is
    /// what a cartridge hands the copy when the source is also a Thumb function pointer — reads the
    /// same bytes the aligned source does, and `DMAnCNT_L` still counts the units the cartridge
    /// wrote: this masks the address, not the length.
    #[test]
    fn a_dma_from_one_byte_past_the_block_reads_the_aligned_halfwords() {
        let mut bus = bus();
        // Eight bytes at $02000100: $10 $11 $12 $13 $14 $15 $16 $17.
        for index in 0..8usize {
            bus.ewram[0x100 + index] = 0x10 + index as u8;
        }
        // Aligned: four halfwords from $02000100 to $03000040, four units of two bytes.
        program_dma(
            &mut bus,
            0,
            EWRAM_BASE + 0x100,
            IWRAM_BASE + 0x40,
            4,
            dma::ENABLE,
        );
        let aligned: [u8; 8] = bus.iwram[0x40..0x48].try_into().expect("eight bytes");
        assert_eq!(aligned, [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);
        assert_eq!(bus.dma.channel(0).count, 4, "the cartridge's four units");
        assert_eq!(bus.dma.channel(0).cursor_sad, EWRAM_BASE + 0x108);

        // The odd source: $02000101 is one byte into the block, so its bit 0 is ignored and the
        // four halfwords are $02000100/$02000102/$02000104/$02000106 — the same eight bytes, in
        // the same order, with the cursor starting from the aligned address.
        program_dma(
            &mut bus,
            0,
            EWRAM_BASE + 0x101,
            IWRAM_BASE + 0x48,
            4,
            dma::ENABLE,
        );
        assert_eq!(
            &bus.iwram[0x48..0x50],
            aligned,
            "an odd source must deliver the aligned block's bytes, not the bytes from $…101"
        );
        assert_eq!(
            bus.dma.channel(0).cursor_sad,
            EWRAM_BASE + 0x108,
            "the walk started on the aligned boundary"
        );
        assert_eq!(
            bus.dma.channel(0).count,
            4,
            "the mask moves the address, never the count"
        );
    }

    /// The same rule on the destination and on a word unit: `$03000051` is rounded down to
    /// `$03000050`, so a 32-bit-unit copy covers eight bytes from there even though the register
    /// said `…51`.
    #[test]
    fn a_dma_to_an_odd_destination_writes_the_aligned_words() {
        let mut bus = bus();
        bus.ewram[0x200..0x208].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        program_dma(
            &mut bus,
            3,
            EWRAM_BASE + 0x200,
            IWRAM_BASE + 0x51,
            2,
            dma::ENABLE | dma::WIDTH32,
        );
        assert_eq!(
            &bus.iwram[0x50..0x58],
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            "two words at the word boundary below the odd destination"
        );
        assert_eq!(bus.dma.channel(3).count, 2, "two word units, as written");
        assert_eq!(bus.dma.channel(3).cursor_dad, IWRAM_BASE + 0x58);
    }

    #[test]
    fn a_channel_timed_to_vblank_runs_when_the_line_reaches_one_sixty() {
        let mut bus = bus();
        fill(&mut bus, 0x300, 16);
        // V-Blank start timing (bits 10-11 = 1), a count of four halfwords.
        program_dma(
            &mut bus,
            1,
            EWRAM_BASE + 0x300,
            IWRAM_BASE + 0x100,
            4,
            dma::ENABLE | (1 << 12),
        );
        assert_eq!(bus.iwram[0x100], 0x00, "nothing has run yet");
        // To line 159's end: still short of V-Blank.
        bus.tick(CYCLES_PER_LINE * 160);
        assert_eq!(bus.lcd.line, 160, "the display is in V-Blank");
        assert_eq!(
            &bus.iwram[0x100..0x108],
            &bus.ewram[0x300..0x308],
            "the transfer ran on entering V-Blank"
        );
    }

    #[test]
    fn a_channel_timed_to_vblank_again_waits_for_the_next_frame_when_it_does_not_repeat() {
        let mut bus = bus();
        fill(&mut bus, 0x300, 8);
        program_dma(
            &mut bus,
            1,
            EWRAM_BASE + 0x300,
            IWRAM_BASE + 0x100,
            4,
            dma::ENABLE | (1 << 12),
        );
        bus.tick(CYCLES_PER_LINE * 161);
        let copied = bus.iwram[0x100..0x108].to_vec();
        bus.iwram[0x100..0x108].fill(0);
        bus.tick(CYCLES_PER_LINE * 228);
        assert_eq!(
            bus.iwram[0x100..0x108],
            [0u8; 8],
            "the channel was disarmed by its one-shot transfer"
        );
        assert_eq!(copied, bus.ewram[0x300..0x308]);
    }

    #[test]
    fn a_repeating_channel_reloads_its_count_and_runs_every_frame() {
        let mut bus = bus();
        fill(&mut bus, 0x300, 16);
        program_dma(
            &mut bus,
            1,
            EWRAM_BASE + 0x300,
            IWRAM_BASE + 0x100,
            4,
            dma::ENABLE | (1 << 12) | dma::REPEAT,
        );
        bus.tick(CYCLES_PER_LINE * 160);
        assert_eq!(&bus.iwram[0x100..0x108], &bus.ewram[0x300..0x308]);
        bus.iwram[0x100..0x118].fill(0);
        bus.tick(CYCLES_PER_LINE * 228);
        // The count is reloaded, so four more units move; the destination pointer keeps walking,
        // because only "increment and reload" (bits 5-6 = 3) returns it to the written address.
        assert_eq!(
            &bus.iwram[0x108..0x110],
            &bus.ewram[0x308..0x310],
            "the repeat ran again on the next frame's V-Blank"
        );
        assert!(
            bus.dma.channel(1).enabled(),
            "a repeating channel stays armed"
        );
    }

    #[test]
    fn a_channel_timed_to_hblank_runs_once_per_drawing_line_and_not_in_v_blank() {
        let mut bus = bus();
        // One halfword per H-Blank into a fixed destination, repeating. H-Blank DMA runs on every
        // drawing line, line 0 included, and is suspended for the whole V-Blank: a hardware report
        // on gbadev.org has it not transferring again until the next frame's line 0, and the Classic
        // NES ports' 160-row per-scanline tables only line up if it fires exactly 160 times.
        for index in 0..8 {
            bus.iwram[0x500 + index * 2] = 0x10 + index as u8;
            bus.iwram[0x501 + index * 2] = 0x80;
        }
        program_dma(
            &mut bus,
            2,
            IWRAM_BASE + 0x500,
            IWRAM_BASE + 0x700,
            1,
            dma::ENABLE | (2 << 12) | dma::REPEAT | (0b10 << 5),
        );
        // Line 2's H-Blank is at 1006 cycles into that line: lines 0, 1 and 2 have each run it once.
        bus.tick(CYCLES_PER_LINE * 2 + HBLANK_DRAW_CYCLES);
        assert_eq!(
            bus.read16(IWRAM_BASE + 0x700),
            0x8012,
            "one transfer on each of the three drawing lines so far"
        );
        assert_eq!(bus.dma.channel(2).cursor_sad, IWRAM_BASE + 0x506);
        // Walk into V-Blank: the hidden lines move nothing, and the channel stays armed.
        bus.tick(CYCLES_PER_LINE * 160);
        let table = bus.read16(IWRAM_BASE + 0x700);
        let cursor = bus.dma.channel(2).cursor_sad;
        assert!(bus.lcd.line >= 160, "in V-Blank: line {}", bus.lcd.line);
        bus.tick(CYCLES_PER_LINE * 60);
        assert_eq!(
            bus.read16(IWRAM_BASE + 0x700),
            table,
            "no transfer during V-Blank"
        );
        assert_eq!(bus.dma.channel(2).cursor_sad, cursor);
        assert!(
            bus.dma.channel(2).enabled(),
            "the channel is still armed for the next frame"
        );
    }

    #[test]
    fn a_transfer_that_steals_cycles_adds_them_to_the_clock() {
        let mut bus = bus();
        fill(&mut bus, 0x300, 16);
        program_dma(
            &mut bus,
            1,
            EWRAM_BASE + 0x300,
            IWRAM_BASE + 0x100,
            8,
            dma::ENABLE | (1 << 12),
        );
        let started = bus.cycles;
        let asked = CYCLES_PER_LINE * 160;
        bus.tick(asked);
        let spent = bus.cycles - started;
        assert_eq!(bus.lcd.line, 160);
        assert!(
            spent > u64::from(asked),
            "the transfer's cycles were spent inside the tick ({spent} > {asked})"
        );
        // And the frame is still 228 lines: the stolen cycles are machine time, not a new line. The
        // cost is GBATEK's `1N+(n-1)S` read plus `1N+(n-1)S` write plus 2I: N = S = 3 reading the
        // on-board work RAM (3 + 7*3 = 24), N = S = 1 writing the 32K work RAM (1 + 7*1 = 8), and
        // I = 1 (IWRAM) — 34 cycles for eight halfwords.
        assert_eq!(spent - u64::from(asked), 34);
    }

    /// GBATEK, *GBA DMA Transfers → Transfer Rate/Timing*: "Of which, `1N+(n-1)S` are read cycles,
    /// and the other `1N+(n-1)S` are write cycles, actual number of cycles depends on the waitstates
    /// and bus-width of the source and destination areas." A 32-bit unit therefore costs its
    /// source's read figure *plus* its destination's write figure — 6 + 2 = 8 from Work RAM 256K to
    /// VRAM, and 2 + 6 = 8 back — and never 12, which is one region's figure billed to both halves.
    #[test]
    fn a_dma_unit_pays_its_source_read_and_its_destination_write() {
        /// `count` 32-bit units from `sad` to `dad`, programmed as a cartridge programs them, and
        /// the cycles the enabling store is charged — the transfer's whole cost: the store itself
        /// is one cycle's I/O access and is already counted by the core's flat model.
        fn cost(sad: u32, dad: u32, count: u16) -> u32 {
            let mut bus = bus();
            let base = IO_BASE + (dma::BASE + 3 * dma::STRIDE) as u32;
            bus.write32(base, sad);
            bus.write32(base + dma::DAD as u32, dad);
            bus.write16(base + dma::COUNT as u32, count);
            bus.waits = 0;
            bus.write16(base + dma::CONTROL as u32, dma::ENABLE | dma::WIDTH32);
            bus.waits
        }
        // One non-sequential unit and three sequential ones each way, plus the 2I internal term.
        assert_eq!(
            cost(EWRAM_BASE, VRAM_BASE, 4),
            6 + 3 * 6 + 2 + 3 * 2 + 2,
            "EWRAM -> VRAM: 6 read + 2 write per unit"
        );
        assert_eq!(
            cost(VRAM_BASE, EWRAM_BASE, 4),
            2 + 3 * 2 + 6 + 3 * 6 + 2,
            "VRAM -> EWRAM: 2 read + 6 write per unit, the same 8 and not 4"
        );
        // A same-region copy is what the old formula was right about: 6 + 6 per unit on both ends.
        assert_eq!(
            cost(EWRAM_BASE, EWRAM_BASE, 4),
            6 + 3 * 6 + 6 + 3 * 6 + 2,
            "EWRAM -> EWRAM: unchanged by the split"
        );
        // The 64 KiB upload the two cartridges make — 16 384 units of 4 bytes — is 16 384 * 8 cycles
        // plus 2I: the 131 074 GBATEK's table gives, not the 196 612 one region charged twice.
        assert_eq!(
            cost(EWRAM_BASE, VRAM_BASE, 16_384),
            16_384 * (6 + 2) + 2,
            "a 64 KiB EWRAM -> VRAM upload"
        );
    }

    /// GBATEK, *GBA Memory Map → Address Bus Width and CPU Read/Write Access Widths*:
    ///
    ///   `GamePak ROM   16    8/16/32   -         5/5/8 **/***`
    ///   `**  Default waitstate settings`
    ///
    /// — one access cycle plus 4 waits non-sequentially and plus 2 sequentially at `WAITCNT` `0000h`
    /// — and *GBA Memory Map → GBA GamePak ROM Waitstates*: "GamePak uses 16bit data bus, so that a
    /// 32bit access is split into TWO 16bit accesses (of which, the second fragment is always
    /// sequential, even if the first fragment was non-sequential)." A 32-bit DMA unit read from the
    /// Game Pak is therefore **two** fragments: `5+3 = 8` on the first unit — the table's `8`, not
    /// its `5` — and `3+3 = 6` on every later one. The transfer's total is the document's
    /// `2N+2(n-1)S+xI` with those figures: `8 + 3*6` read, `1 + 3*1` written to Work RAM 32K
    /// ("1/1/1"), and `2I`.
    #[test]
    fn a_32_bit_dma_unit_read_from_the_game_pak_is_two_16_bit_fragments() {
        /// The cycles the enabling store is charged for a DMA3 transfer of `count` units of `control`'s
        /// width from the Game Pak at `waitcnt` — the whole cost, since the store's own I/O access is
        /// folded into the core's flat model.
        fn cost(waitcnt: u16, control: u16, count: u16) -> u32 {
            let mut bus = bus();
            bus.write16(IO_BASE + WAITCNT as u32, waitcnt);
            let base = IO_BASE + (dma::BASE + 3 * dma::STRIDE) as u32;
            bus.write32(base, ROM_BASE);
            bus.write32(base + dma::DAD as u32, IWRAM_BASE);
            bus.write16(base + dma::COUNT as u32, count);
            bus.waits = 0;
            bus.write16(base + dma::CONTROL as u32, dma::ENABLE | control);
            bus.waits
        }
        // Power-on waits (`WAITCNT` `0000h`: 4,2). The 16-bit read is one fragment — the pair this
        // function already answered with — and the 32-bit read is that fragment plus the
        // sequential one behind it.
        let power_on = 0x0000;
        // The write half is Work RAM 32K, "1/1/1": one cycle each unit, four units.
        let write = 1 + 3;
        assert_eq!(
            cost(power_on, dma::WIDTH32, 4),
            8 + 3 * 6 + write + 2,
            "1N=8 and 1S=6 per 32-bit unit read from the Game Pak, plus 2I"
        );
        assert_eq!(
            cost(power_on, 0, 4),
            5 + 3 * 3 + write + 2,
            "a 16-bit unit is one fragment: 5 and 3, unchanged"
        );
        // The post-boot waits the boot sequence leaves (`4317h`: 3,1), so the same rule with a
        // different row: 1N = 4+2 = 6 and 1S = 2+2 = 4 for a 32-bit unit.
        assert_eq!(
            cost(WAITCNT_POST_BOOT, dma::WIDTH32, 4),
            6 + 3 * 4 + write + 2,
            "the fragment rule follows the waitstates, not the power-on row"
        );
        // And at the source of both halves: the FIFO path's four 32-bit units go through this same
        // pair, so ROM as a FIFO's source is priced 8/6 by construction rather than by a case.
        let mut bus = bus();
        bus.write16(IO_BASE + WAITCNT as u32, power_on);
        assert_eq!(bus.dma_half_costs(ROM_BASE, 4), (8, 6));
        assert_eq!(bus.dma_half_costs(ROM_BASE, 2), (5, 3));
    }

    #[test]
    fn a_dma_end_interrupt_is_the_channels_own_bit() {
        let mut bus = bus();
        fill(&mut bus, 0x300, 8);
        program_dma(
            &mut bus,
            2,
            EWRAM_BASE + 0x300,
            IWRAM_BASE + 0x100,
            4,
            dma::ENABLE | dma::IRQ,
        );
        assert_eq!(bus.irq_flags(), irq::bit(irq::DMA0 + 2));
    }

    #[test]
    fn the_hblank_interrupt_is_asked_for_by_dispstat_bit_four() {
        let mut with_irq = bus();
        let mut plain = bus();
        // Bit 4: H-Blank IRQ enable. No IE: this is the request, not the CPU's line.
        with_irq.write16(IO_BASE + DISPSTAT as u32, 1 << 4);
        with_irq.tick(CYCLES_PER_LINE * 3 + HBLANK_DRAW_CYCLES);
        assert_eq!(with_irq.lcd.line, 3);
        assert_eq!(
            with_irq.irq_flags(),
            irq::bit(irq::HBLANK),
            "one request per line, at the start of its H-Blank"
        );
        // Without the enable bit nothing asks.
        plain.tick(CYCLES_PER_LINE * 3 + HBLANK_DRAW_CYCLES);
        assert_eq!(plain.irq_flags(), 0);
    }

    #[test]
    fn the_vcounter_interrupt_fires_on_the_line_dispstat_names() {
        let mut bus = bus();
        // Bit 5: V-Counter IRQ enable, and VCOUNT = 5 in the high byte.
        bus.write16(IO_BASE + DISPSTAT as u32, (1 << 5) | (5 << 8));
        assert_eq!(bus.dispstat() >> 8, 5, "the setting lives in the high byte");
        bus.tick(CYCLES_PER_LINE * 4);
        assert_eq!(bus.lcd.line, 4);
        assert_eq!(bus.irq_flags(), 0, "line 4 is not the line named");
        bus.tick(CYCLES_PER_LINE);
        assert_eq!(bus.lcd.line, 5);
        assert_eq!(bus.irq_flags(), irq::bit(irq::VCOUNT));
    }

    #[test]
    fn the_vblank_interrupt_is_asked_for_by_dispstat_bit_three() {
        let mut with_irq = bus();
        let mut plain = bus();
        with_irq.write16(IO_BASE + DISPSTAT as u32, 1 << 3);
        with_irq.tick(CYCLES_PER_LINE * 160);
        assert_eq!(with_irq.lcd.line, 160);
        assert_eq!(with_irq.irq_flags(), irq::bit(irq::VBLANK));

        plain.tick(CYCLES_PER_LINE * 160);
        assert_eq!(
            plain.irq_flags(),
            0,
            "the flag rises, but no request is made without bit 3"
        );
    }
}
