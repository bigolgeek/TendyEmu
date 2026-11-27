//! The Game Boy Advance save chips: SRAM, Flash (64 KiB and 128 KiB), EEPROM (512 B and 8 KiB)
//! and the RTC cartridges, plus the one place a cartridge's save type is decided.
//!
//! Every byte of the protocol is written from GBATEK, *Backup Media* and *Backup IDs* — never from
//! another emulator. The public suites in `roms/test/gba/save/` pin the parts GBATEK leaves open
//! (the byte-lane behaviour of the 8-bit chips on the 16-bit bus, and the 0xFF fill of an erased or
//! never-written chip), and each is quoted where it is used.
//!
//! # Detection, documented rather than guessed
//!
//! A GBA header has **no save-type field**. GBATEK, *Backup IDs*: "Nintendo didn't include a
//! backup-type entry in the ROM header, however, the required type can be detected by ID strings in
//! the ROM-image." The strings (word-aligned, length a multiple of 4) are:
//!
//! ```text
//! EEPROM_Vnnn    EEPROM 512 bytes or 8 Kbytes (4Kbit or 64Kbit)
//! SRAM_Vnnn      SRAM 32 Kbytes (256Kbit)
//! FLASH_Vnnn     FLASH 64 Kbytes (512Kbit) (older files)
//! FLASH512_Vnnn  FLASH 64 Kbytes (512Kbit) (newer files)
//! FLASH1M_Vnnn   FLASH 128 Kbytes (1Mbit)
//! ```
//!
//! `SIIRTC_V` marks an RTC cart (the S-3511A clock), independently of the save chip. Detection scans
//! the whole ROM for these strings, so a ROM that carries none names no chip and the window reads
//! 0xFF, which is what `none.gba` tests. **A store into the window outranks the header's silence**:
//! a cartridge whose ROM names no type but which *writes* `$0E000000` plainly has a chip, so the
//! first store creates the 32 KiB SRAM chip (see [`Save::write8`] and its `provision_on_write`
//! sibling). Three cartridges in the reference library store a signature, read it back and retry
//! forever while the write is dropped. A read alone never creates a chip: with no chip `$0D000000`
//! is the Game Pak ROM mirror, and cartridges read it for their own reasons.
//!
//! **The one place the rule is genuinely undecidable** is the EEPROM size: `EEPROM_Vnnn` does not
//! say 512 B or 8 KiB, and GBATEK's *EEPROM* page says "There seems to be no autodection mechanism,
//! so that a hardcoded bus width must be used." The size is decided by the DMA request's bit count
//! on first use (9/73 bits is 512 B, 17/81 bits is 8 KiB), which is exactly what [`Eeprom::receive`]
//! does. No heuristic is invented: a request with the wrong count is refused and corrupts nothing.
//!
//! # The last bit of a read request is the chip's turn on the wire, not a field
//!
//! GBATEK, *Set Address (For Reading)* gives the read request as `2 bits "11"`, `n bits eeprom
//! address (MSB first, 6 or 14 bits, depending on EEPROM)`, `1 bit "0"`, and *Read Data* as
//! `4 bits - ignore these`, `64 bits - data`. That last bit is a **clock**, not a field: the chip
//! and the cartridge share one wire (GBATEK, *Addressing and Waitstates*: "The eeprom is connected
//! to Bit0 of the data bus"), and on a read the chip takes that wire over to drive its first
//! don't-care bit. What the cartridge leaves in the bit is its own buffer's business, and it leaves
//! its own data there: the Classic NES Series' twelve put bit 49 of the block they last wrote into
//! it, 52 of 52 write-then-read pairs (`tests/gba_cnes_save.rs`), and the three cartridges below
//! put their own leftovers there too — 0490 and 0785 each read block 0 twice a moment apart, once
//! with the documented `110000000` and once with `110000001`, and 0352 sends `110000001` from its
//! first read on.
//!
//! Enforcing the `0` there refused those reads, and a refused read is answered by the Ready line:
//! the cartridge's 68-bit data DMA came back all ones, its verify failed, and it rewrote the block
//! for the whole run — 12.7M window reads in 1 200 frames on each of the three, all of them a
//! consequence of the refusal. The **length** of the stream is what says "read" (9 or 17 bits), so
//! the chip now accepts any value in that last clock. A **write**'s trailing bit keeps its
//! documented `0`: it follows the 64 data bits, every write stream measured here ends with it, and
//! nothing is gained by relaxing a bit no cartridge has been seen to vary.
//!
//! # The 8-bit chips and the 16-bit bus
//!
//! SRAM and Flash are 8-bit chips on a 16-bit bus (GBATEK: SRAM "restricted to 8 bits, it should be
//! accessed by LDRB, LDRSB, and STRB opcodes only"; Flash "restricted to 16bit address and 8bit data
//! buswidths ... accessed only by 8bit read/write LDRB/STRB opcodes"). What the hardware does when a
//! wider opcode touches them is what the public suites pin (their `t004`..`t009` are byte-for-byte
//! identical across `sram.asm` and `flash.asm`):
//!
//! * A **write** of any width stores exactly one byte: the byte at the store's lowest address, taken
//!   from the little-endian lane that address selects (`strh $AABB` at an even address stores `$BB`,
//!   at an odd address stores `$AA`). The remaining bytes of the store are dropped.
//! * A **read** of any width returns the single byte the start address selects, replicated across
//!   every lane of the read (`ldrh` returns it twice, `ldr` four times).
//!
//! # The save file
//!
//! The battery body is the chip's raw bytes — SRAM 32 KiB, Flash 64/128 KiB, EEPROM 512 B/8 KiB — so
//! a save written by this build is read back byte-for-byte next run. A file whose size matches
//! neither the chip nor the chip + `GART` footer was written for a different save chip: a short one
//! is padded with 0xFF and a long one truncated to the chip, so a wrong chip never loses the data
//! that does fit, and neither case writes outside the chip. An RTC cart appends a fixed footer
//! (`GART`) after the chip bytes so the clock survives a session the way the Game Boy path's `RTCL`
//! footer does.

use tendy_gb::state::{StateError, StateReader, StateWriter};

/// SRAM's size: 32 KiB (256 Kbit), GBATEK *Backup SRAM* ("SRAM - 32 KBytes (256Kbit)").
pub const SRAM_SIZE: usize = 0x8000;
/// Flash 64 KiB (512 Kbit) and 128 KiB (1 Mbit), GBATEK *Backup Flash ROM*.
pub const FLASH64_SIZE: usize = 0x1_0000;
/// 128 KiB = 1 Mbit.
pub const FLASH128_SIZE: usize = 0x2_0000;
/// EEPROM 512 B (4 Kbit).
pub const EEPROM512_SIZE: usize = 0x200;
/// EEPROM 8 KiB (64 Kbit).
pub const EEPROM8K_SIZE: usize = 0x2000;

/// Master-clock cycles in one second: 16.78 MHz, used to advance the RTC.
pub const CYCLES_PER_SECOND: u64 = 16_777_216;

/// Where a save chip lives: `$0E000000` (GBATEK *GBA Memory Map*, "Game Pak SRAM"), mirrored across
/// `$0E000000-$0FFFFFFF`.
pub const SAVE_BASE: u32 = 0x0E00_0000;
/// The EEPROM's address, the first address of the upper half of the WS2 area (GBATEK *Backup EEPROM*,
/// "the eeprom can be then addressed at ... On carts with 16MB or smaller ROM, eeprom can be
/// alternately accessed anywhere at D000000h-DFFFFFFh").
pub const EEPROM_BASE: u32 = 0x0D00_0000;
/// The RTC/GPIO registers' address, in the ROM area (GBATEK *GBA Cart I/O Port (GPIO)*,
/// "mapped to a 6-byte region in the ROM-area at 80000C4h").
pub const GPIO_BASE: u32 = 0x0800_00C4;

/// A chip's fill byte: never-written SRAM/Flash/EEPROM reads 0xFF, which is what `sram.gba`'s `t001`
/// ("Uninitialized memory") and `none.gba` both assert, and what GBATEK's flash section says an
/// erased chip is filled with ("erased memory is FFh-filled").
pub const ERASED: u8 = 0xFF;

/// A cartridge's save type, as [`detect`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveType {
    /// No save chip: the window reads 0xFF and ignores writes.
    None,
    /// 32 KiB of battery-backed SRAM.
    Sram,
    /// 64 KiB flash (512 Kbit).
    Flash64,
    /// 128 KiB flash (1 Mbit).
    Flash128,
    /// 512-byte EEPROM (4 Kbit).
    Eeprom512,
    /// 8 KiB EEPROM (64 Kbit).
    Eeprom8k,
}

impl SaveType {
    /// The chip's name, in GBATEK's own words, for the bar and the drawer (G7).
    ///
    /// `EEPROM_Vnnn` does not say 512 bytes or 8 KiB (see this module's comment), so the detected
    /// type reads `EEPROM` until the first request's bit count decides it — the report is the truth,
    /// not a guess.
    pub fn label(self) -> &'static str {
        match self {
            SaveType::None => "none",
            SaveType::Sram => "SRAM",
            SaveType::Flash64 => "Flash 512 Kbit",
            SaveType::Flash128 => "Flash 1 Mbit",
            SaveType::Eeprom512 | SaveType::Eeprom8k => "EEPROM",
        }
    }

    /// The chip's size in bytes, `None` for no chip.
    pub fn bytes(self) -> Option<usize> {
        match self {
            SaveType::None => None,
            SaveType::Sram => Some(SRAM_SIZE),
            SaveType::Flash64 => Some(FLASH64_SIZE),
            SaveType::Flash128 => Some(FLASH128_SIZE),
            SaveType::Eeprom512 => Some(EEPROM512_SIZE),
            SaveType::Eeprom8k => Some(EEPROM8K_SIZE),
        }
    }
}

impl Save {
    /// The type this chip is, as [`detect`] would have named it. A 128 KiB flash says 1 Mbit, and an
    /// EEPROM whose request has not landed yet says `Eeprom512` — its size is still undecided.
    pub fn kind(&self) -> SaveType {
        match self {
            Save::None => SaveType::None,
            Save::Sram(_) => SaveType::Sram,
            Save::Flash(flash) => match flash.size {
                FlashSize::K64 => SaveType::Flash64,
                FlashSize::K128 => SaveType::Flash128,
            },
            Save::Eeprom(eeprom) => match eeprom.size {
                EepromSize::K4bit | EepromSize::Undetermined => SaveType::Eeprom512,
                EepromSize::K64kbit => SaveType::Eeprom8k,
            },
        }
    }
}

/// The one save chip a cartridge has, or a recorded refusal for a type that is recognised but not
/// implemented here. Every family is one variant, as the brief asks.
#[derive(Clone, Debug, Default)]
pub enum Save {
    /// No chip named by the ROM's ID strings. Reads of the window are 0xFF; the first **store** into
    /// it creates the SRAM chip the missing string would have named (see [`Save::write8`]).
    #[default]
    None,
    Sram(Sram),
    Flash(Flash),
    Eeprom(Eeprom),
}

/// Decode the save type from a ROM's ID strings, following GBATEK's *Backup IDs* table exactly.
///
/// `EEPROM_Vnnn` maps to an EEPROM of **undetermined** size (see [`Eeprom::receive`]); the `_V`
/// version digits are not read. The scan is the documented rule — a string anywhere in the ROM,
/// word-aligned — and no string names no chip, which a later store can still overturn (see
/// [`Save::write8`]).
pub fn detect(rom: &[u8]) -> SaveType {
    // A ROM that carries "FLASH1M_V" is 128 KiB; the "1M" string must win over the bare "FLASH_V"
    // (which would otherwise read 64 KiB), so the larger string is checked first.
    let has = |needle: &[u8]| -> bool {
        rom.windows(needle.len())
            .step_by(4)
            .any(|window| window == needle)
    };
    if has(b"FLASH1M_V") {
        SaveType::Flash128
    } else if has(b"FLASH512_V") || has(b"FLASH_V") {
        SaveType::Flash64
    } else if has(b"SRAM_V") {
        SaveType::Sram
    } else if has(b"EEPROM_V") {
        SaveType::Eeprom512
    } else {
        SaveType::None
    }
}

/// Whether a ROM carries the RTC's ID string (GBATEK: the S-3511A's library string `SIIRTC_V`).
pub fn has_rtc(rom: &[u8]) -> bool {
    rom.windows(b"SIIRTC_V".len())
        .step_by(4)
        .any(|window| window == b"SIIRTC_V")
}

/// Build the save chip a cartridge's ROM names. `Eeprom512` and `Eeprom8k` both start here as an
/// EEPROM whose size is decided by the first DMA request (see the module comment).
pub fn build(rom: &[u8]) -> Save {
    match detect(rom) {
        SaveType::None => Save::None,
        SaveType::Sram => Save::Sram(Sram::new()),
        SaveType::Flash64 => Save::Flash(Flash::new(FlashSize::K64)),
        SaveType::Flash128 => Save::Flash(Flash::new(FlashSize::K128)),
        SaveType::Eeprom512 | SaveType::Eeprom8k => Save::Eeprom(Eeprom::new()),
    }
}

/// The byte a chip returns for one lane of a wide read: the start address's byte, replicated. Both
/// `sram.gba` and `flash.gba` assert it (`t004`: `strb 1` then `ldrh` is `$0101`; `t005`: `ldr` is
/// `$01010101`).
fn replicate(byte: u8, width: usize) -> u64 {
    let mut value = 0u64;
    for _ in 0..width {
        value = (value << 8) | u64::from(byte);
    }
    value
}

// ---------------------------------------------------------------- SRAM

/// 32 KiB of 8-bit SRAM, with the wide-access byte-lane behaviour the suite pins.
#[derive(Clone, Debug)]
pub struct Sram {
    data: Box<[u8; SRAM_SIZE]>,
}

impl Sram {
    pub fn new() -> Sram {
        Sram {
            data: Box::new([ERASED; SRAM_SIZE]),
        }
    }

    /// The chip's raw bytes, for a save file.
    pub fn data(&self) -> &[u8; SRAM_SIZE] {
        &self.data
    }

    /// Load a save file's bytes; a wrong length is refused without touching the chip.
    pub fn load(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() != SRAM_SIZE {
            return Err(format!(
                "save is {} bytes, SRAM is {SRAM_SIZE}",
                bytes.len()
            ));
        }
        self.data.copy_from_slice(bytes);
        Ok(())
    }

    /// The 32 KiB chip is "repeated twice" inside the 64 KiB window (GBATEK *Memory Map*), and the
    /// window is mirrored across `$0E000000-$0FFFFFFF`, so the byte is `addr & $7FFF`.
    fn offset(addr: u32) -> usize {
        (addr & (SRAM_SIZE as u32 - 1)) as usize
    }

    pub fn read8(&self, addr: u32) -> u8 {
        self.data[Self::offset(addr)]
    }

    pub fn read16(&self, addr: u32) -> u16 {
        replicate(self.read8(addr), 2) as u16
    }

    pub fn read32(&self, addr: u32) -> u32 {
        replicate(self.read8(addr), 4) as u32
    }

    pub fn write8(&mut self, addr: u32, value: u8) {
        let at = Self::offset(addr);
        self.data[at] = value;
    }

    pub fn write16(&mut self, addr: u32, value: u16) {
        let at = Self::offset(addr);
        self.data[at] = (value >> (8 * (addr & 1))) as u8;
    }

    pub fn write32(&mut self, addr: u32, value: u32) {
        let at = Self::offset(addr);
        self.data[at] = (value >> (8 * (addr & 3))) as u8;
    }
}

impl Default for Sram {
    fn default() -> Sram {
        Sram::new()
    }
}

// ---------------------------------------------------------------- flash

/// A flash chip's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashSize {
    /// 64 KiB (512 Kbit).
    K64,
    /// 128 KiB (1 Mbit), two 64 KiB banks.
    K128,
}

/// The unlock sequence GBATEK's flash section gives: `[E005555h]=AAh, [E002AAAh]=55h`, then a
/// command byte. An erase command (`80h`) is followed by a second `AA`/`55` pair and then the erase
/// target (`10h` to `$5555` for a chip erase, `30h` to a sector address for a sector erase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unlock {
    /// Not in a sequence.
    Idle,
    /// `AA` was written to `$5555`.
    Aa,
    /// `AA`/`55` written; the next byte to `$5555` is a command.
    Aa55,
    /// `80` (erase command) was seen; awaiting the second `AA` to `$5555`.
    Erase,
    /// Erase armed, `AA` written; awaiting `55` to `$2AAA`.
    EraseAa,
    /// Erase armed, `AA`/`55` written; the next byte is the erase target (`10`/`30`).
    Erase55,
}

/// A pending erase: the chip is busy until `busy_until` cycles.
#[derive(Clone, Copy, Debug)]
enum PendingErase {
    Chip,
    Sector(u16),
}

/// A flash chip: 64 KiB or 128 KiB, with the command sequences GBATEK documents (unlock pair, chip
/// identification, erase chip / 4 KB sector, write byte, bank switch) and their "takes a while"
/// behaviour modelled as a cycle count rather than a sleep.
///
/// The write is instant: `flash.asm`'s `t004` reads the byte back on the very next instruction with
/// no poll, so the byte must be there at once. The erase is a cycle count: `t010`/`t011` poll until
/// the cell reads `$FF`, so the erase completes after [`ERASE_CHIP_CYCLES`]/[`ERASE_SECTOR_CYCLES`],
/// during which reads return `$00` (busy). GBATEK leaves the exact timings "unknown?", so these are
/// one cycle per byte, named and bounded by the suites' poll timeouts.
#[derive(Clone, Debug)]
pub struct Flash {
    data: Box<[u8]>,
    size: FlashSize,
    /// The bank for a 128 KiB chip, 0..1. GBATEK: "Specifies 64K bank number for read/write/erase".
    bank: u8,
    unlock: Unlock,
    id_mode: bool,
    program_armed: bool,
    bank_armed: bool,
    pending: Option<(PendingErase, u64)>,
}

/// A chip erase: 64 KiB, one cycle a byte (see [`Flash`]).
const ERASE_CHIP_CYCLES: u64 = 0x1_0000;
/// A 4 KiB sector erase: one cycle a byte.
const ERASE_SECTOR_CYCLES: u64 = 0x1000;

/// Flash command bytes (GBATEK *Backup Flash ROM*).
const CMD_ID: u8 = 0x90;
const CMD_ID_EXIT: u8 = 0xF0;
const CMD_ERASE: u8 = 0x80;
const CMD_ERASE_CHIP: u8 = 0x10;
const CMD_ERASE_SECTOR: u8 = 0x30;
const CMD_PROGRAM: u8 = 0xA0;
const CMD_BANK: u8 = 0xB0;

/// The unlock addresses, within the 64 KiB window.
const UNLOCK1: u16 = 0x5555;
const UNLOCK2: u16 = 0x2AAA;

impl Flash {
    pub fn new(size: FlashSize) -> Flash {
        let len = match size {
            FlashSize::K64 => FLASH64_SIZE,
            FlashSize::K128 => FLASH128_SIZE,
        };
        Flash {
            data: vec![ERASED; len].into_boxed_slice(),
            size,
            bank: 0,
            unlock: Unlock::Idle,
            id_mode: false,
            program_armed: false,
            bank_armed: false,
            pending: None,
        }
    }

    /// The chip's raw bytes, for a save file.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Load a save file's bytes; a wrong length is refused without touching the chip.
    pub fn load(&mut self, bytes: &[u8]) -> Result<(), String> {
        let want = self.data.len();
        if bytes.len() != want {
            return Err(format!("save is {} bytes, flash is {want}", bytes.len()));
        }
        self.data.copy_from_slice(bytes);
        Ok(())
    }

    /// The chip offset of `addr`: the 16-bit window offset, plus the bank for a 128 KiB chip.
    fn offset(&self, addr: u32) -> usize {
        (addr & 0xFFFF) as usize + usize::from(self.bank) * FLASH64_SIZE
    }

    /// The manufacturer/device ID pair the chip reports in ID mode (GBATEK's *Device Types* table,
    /// "Identification Codes MSB=Device Type, LSB=Manufacturer", with `man=[E000000h]`,
    /// `dev=[E000001h]`). Panasonic is the 64 KiB chip, Sanyo the 128 KiB one.
    fn id(&self) -> (u8, u8) {
        match self.size {
            FlashSize::K64 => (0x32, 0x1B),  // Panasonic 1B32h
            FlashSize::K128 => (0x62, 0x13), // Sanyo 1362h
        }
    }

    /// Apply a pending erase once its cycle deadline has passed.
    fn settle(&mut self, cycles: u64) {
        if let Some((erase, deadline)) = self.pending
            && cycles >= deadline
        {
            match erase {
                PendingErase::Chip => self.data.fill(ERASED),
                PendingErase::Sector(n) => {
                    let base = usize::from(n & 0xF000) + usize::from(self.bank) * FLASH64_SIZE;
                    let end = (base + 0x1000).min(self.data.len());
                    self.data[base..end].fill(ERASED);
                }
            }
            self.pending = None;
        }
    }

    /// One byte at `addr`; `$00` while an erase is still in progress.
    pub fn read8(&mut self, addr: u32, cycles: u64) -> u8 {
        self.settle(cycles);
        if self.pending.is_some() {
            return 0x00;
        }
        if self.id_mode {
            let at = (addr & 0xFFFF) as usize;
            return match at {
                0 => self.id().0,
                1 => self.id().1,
                _ => ERASED,
            };
        }
        self.data[self.offset(addr)]
    }

    pub fn read16(&mut self, addr: u32, cycles: u64) -> u16 {
        replicate(self.read8(addr, cycles), 2) as u16
    }

    pub fn read32(&mut self, addr: u32, cycles: u64) -> u32 {
        replicate(self.read8(addr, cycles), 4) as u32
    }

    /// A byte write: the command sequence, or the data byte a program/bank command expects.
    pub fn write8(&mut self, addr: u32, value: u8, cycles: u64) {
        self.settle(cycles);
        let at = (addr & 0xFFFF) as usize;
        match self.unlock {
            Unlock::Idle => {
                if at == UNLOCK1 as usize && value == 0xAA {
                    self.unlock = Unlock::Aa;
                } else if self.program_armed {
                    self.program_armed = false;
                    self.data[self.offset(addr)] = value;
                } else if self.bank_armed {
                    self.bank_armed = false;
                    if self.size == FlashSize::K128 {
                        self.bank = value & 1;
                    }
                }
            }
            Unlock::Aa => {
                if at == UNLOCK2 as usize && value == 0x55 {
                    self.unlock = Unlock::Aa55;
                } else {
                    self.unlock = Unlock::Idle;
                }
            }
            Unlock::Aa55 => {
                self.unlock = Unlock::Idle;
                if at == UNLOCK1 as usize {
                    match value {
                        CMD_ID => self.id_mode = true,
                        CMD_ID_EXIT => self.id_mode = false,
                        CMD_ERASE => self.unlock = Unlock::Erase,
                        CMD_PROGRAM => self.program_armed = true,
                        CMD_BANK => self.bank_armed = true,
                        _ => {}
                    }
                }
            }
            Unlock::Erase => {
                self.unlock = Unlock::Idle;
                if at == UNLOCK1 as usize && value == 0xAA {
                    self.unlock = Unlock::EraseAa;
                }
            }
            Unlock::EraseAa => {
                self.unlock = Unlock::Idle;
                if at == UNLOCK2 as usize && value == 0x55 {
                    self.unlock = Unlock::Erase55;
                }
            }
            Unlock::Erase55 => {
                self.unlock = Unlock::Idle;
                if value == CMD_ERASE_CHIP && at == UNLOCK1 as usize {
                    self.pending = Some((PendingErase::Chip, cycles + ERASE_CHIP_CYCLES));
                } else if value == CMD_ERASE_SECTOR {
                    self.pending = Some((
                        PendingErase::Sector(at as u16),
                        cycles + ERASE_SECTOR_CYCLES,
                    ));
                }
            }
        }
    }

    /// A halfword write: a program command's single data byte, taken from the lane `addr` selects.
    pub fn write16(&mut self, addr: u32, value: u16, cycles: u64) {
        self.settle(cycles);
        if self.program_armed {
            self.program_armed = false;
            self.data[self.offset(addr)] = (value >> (8 * (addr & 1))) as u8;
        }
    }

    /// A word write: a program command's single data byte, taken from the lane `addr` selects.
    pub fn write32(&mut self, addr: u32, value: u32, cycles: u64) {
        self.settle(cycles);
        if self.program_armed {
            self.program_armed = false;
            self.data[self.offset(addr)] = (value >> (8 * (addr & 3))) as u8;
        }
    }
}

// ---------------------------------------------------------------- EEPROM

/// An EEPROM's size. A chip detected from `EEPROM_V` starts undetermined and is locked by the first
/// DMA request's bit count; the two explicit sizes are what the protocol tests construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EepromSize {
    /// 512 bytes (4 Kbit): a 6-bit address, 9-bit read / 73-bit write request.
    K4bit,
    /// 8 KiB (64 Kbit): a 14-bit address (lower 10 used), 17-bit read / 81-bit write request.
    K64kbit,
    /// Not yet decided; locked by the first request.
    Undetermined,
}

impl EepromSize {
    /// Address bits in a request: 6 or 14 (GBATEK's "6 or 14 bits, depending on EEPROM").
    pub fn address_bits(self) -> usize {
        match self {
            EepromSize::K4bit => 6,
            EepromSize::K64kbit => 14,
            EepromSize::Undetermined => 6,
        }
    }

    /// The chip's size in bytes.
    pub fn bytes(self) -> usize {
        match self {
            EepromSize::K4bit => EEPROM512_SIZE,
            EepromSize::K64kbit => EEPROM8K_SIZE,
            EepromSize::Undetermined => 0,
        }
    }
}

/// What a decoded request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EepromOutcome {
    /// A read request: the 6- or 14-bit block address.
    Read(u16),
    /// A write request, programmed. The chip is busy for a while afterwards.
    Written,
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EepromError {
    /// The bit count does not match any documented request for this chip.
    BadBitCount,
    /// The leading two bits are not `11` (read) or `10` (write).
    BadCommand,
    /// A write request's trailing bit is not `0`. A read's trailing bit is not checked: it is the
    /// clock the chip drives its own line on (see this module's note).
    BadTerminator,
}

/// A serial EEPROM, driven by DMA bit streams: each halfword's bit 0 is one bit, and the *length* of
/// the transfer is the command (GBATEK *Backup EEPROM*). One 64-bit (8-byte) word per address.
///
/// The chip is one bit wide and answers a halfword read with the bit it is driving on bit 0 of the
/// bus, so the whole protocol is [`Eeprom::write_bit`] and [`Eeprom::read_bit`]: a request is clocked
/// in one bit at a time, and — for a read — shifted out one bit at a time. With nothing pending the
/// chip drives Ready, which is bit 0 set, and that is what a cartridge's timeout loop polls.
#[derive(Clone, Debug)]
pub struct Eeprom {
    size: EepromSize,
    data: Box<[u8]>,
    /// The cycle the last write finishes at (GBATEK: "ca. 108368 clock cycles (ca. 6.5ms)").
    busy_until: u64,
    /// The bits of a request clocked in since the chip last went idle.
    command: Vec<bool>,
    /// The bits a read request makes the chip shift out (GBATEK: "Read a stream of 68 bits").
    readout: Vec<bool>,
    /// How many of [`Eeprom::readout`] have gone.
    readout_pos: usize,
}

/// EEPROM write time: 108 368 cycles, GBATEK's own figure.
const EEPROM_WRITE_CYCLES: u64 = 108_368;

impl Eeprom {
    /// An EEPROM of a fixed size, for the protocol tests and for a chip whose size is known.
    pub fn with_size(size: EepromSize) -> Eeprom {
        let len = size.bytes();
        Eeprom {
            size,
            data: vec![ERASED; len].into_boxed_slice(),
            busy_until: 0,
            command: Vec::new(),
            readout: Vec::new(),
            readout_pos: 0,
        }
    }

    /// An EEPROM whose size is decided by the first request, the case `EEPROM_V` detection produces.
    pub fn new() -> Eeprom {
        Eeprom::with_size(EepromSize::Undetermined)
    }

    /// The chip's size as now known, `None` before the first request.
    pub fn known_size(&self) -> Option<EepromSize> {
        match self.size {
            EepromSize::Undetermined => None,
            size => Some(size),
        }
    }

    /// The chip's raw bytes, for a save file. `None` before the size is known.
    pub fn data(&self) -> Option<&[u8]> {
        match self.size {
            EepromSize::Undetermined => None,
            _ => Some(&self.data),
        }
    }

    /// Load a save file's bytes. An EEPROM whose size is still undecided is sized by the file's own
    /// length (512 B or 8 KiB); otherwise the length must match, and a mismatch is refused without
    /// touching the chip.
    pub fn load(&mut self, bytes: &[u8]) -> Result<(), String> {
        if matches!(self.size, EepromSize::Undetermined) {
            self.size = match bytes.len() {
                EEPROM512_SIZE => EepromSize::K4bit,
                EEPROM8K_SIZE => EepromSize::K64kbit,
                _ => return Err("EEPROM size cannot be decided from the save file".to_string()),
            };
            self.data = vec![ERASED; bytes.len()].into_boxed_slice();
        }
        if bytes.len() != self.data.len() {
            return Err(format!(
                "save is {} bytes, EEPROM is {}",
                bytes.len(),
                self.data.len()
            ));
        }
        self.data.copy_from_slice(bytes);
        Ok(())
    }

    /// True while a write is still programming (GBATEK: poll until the chip signals ready).
    pub fn busy(&self, cycles: u64) -> bool {
        cycles < self.busy_until
    }

    /// Decode the address bits (MSB first) of a request.
    fn decode_address(bits: &[bool], address_bits: usize) -> u16 {
        let mut addr = 0u16;
        for &bit in &bits[..address_bits] {
            addr = (addr << 1) | u16::from(bit);
        }
        addr
    }

    /// Decode the 64 data bits (MSB first) into 8 bytes, byte 0 first.
    fn decode_data(bits: &[bool]) -> [u8; 8] {
        let mut data = [0u8; 8];
        for (byte_index, byte) in data.iter_mut().enumerate() {
            for bit_index in 0..8 {
                if bits[byte_index * 8 + bit_index] {
                    *byte |= 1 << (7 - bit_index);
                }
            }
        }
        data
    }

    /// The 64 data bits of a block, MSB first (bit 7 of byte 0 first).
    fn block_bits(&self, addr: u16) -> Vec<bool> {
        let mut bits = Vec::with_capacity(64);
        for &byte in &self.data[addr as usize * 8..addr as usize * 8 + 8] {
            for bit in (0..8).rev() {
                bits.push(byte & (1 << bit) != 0);
            }
        }
        bits
    }

    /// Feed a DMA write's bit stream (one bit per halfword, bit 0) and decode the request. This is
    /// the single most error-prone protocol on the console, so the size is decided here and a
    /// request that does not match the documented shape is refused without touching the data.
    pub fn receive(&mut self, bits: &[bool], cycles: u64) -> Result<EepromOutcome, EepromError> {
        // The request length *is* the command (GBATEK: a 9-bit read or a 73-bit write is 512 B; a
        // 17-bit read or an 81-bit write is 8 KiB).
        let (read_len, write_len) = match self.size {
            EepromSize::K4bit => (9, 73),
            EepromSize::K64kbit => (17, 81),
            EepromSize::Undetermined => {
                // Decide the size from the bit count, then fall through with it locked in.
                self.size = match bits.len() {
                    9 | 73 => EepromSize::K4bit,
                    17 | 81 => EepromSize::K64kbit,
                    _ => return Err(EepromError::BadBitCount),
                };
                self.data = vec![ERASED; self.size.bytes()].into_boxed_slice();
                return self.receive(bits, cycles);
            }
        };

        let is_read = bits.len() == read_len;
        if !is_read && bits.len() != write_len {
            return Err(EepromError::BadBitCount);
        }
        if bits.len() < 2 {
            return Err(EepromError::BadBitCount);
        }
        // GBATEK: `2 bits "11" (Read Request)` / `2 bits "10" (Write Request)`.
        let command = (bits[0], bits[1]);
        let address_bits = self.size.address_bits();
        let header = 2 + address_bits;
        let addr = Self::decode_address(&bits[2..2 + address_bits], address_bits);
        match command {
            (true, true) => {
                // Read: `... 1 bit "0"`. The length says "read"; the trailing bit is the clock the
                // chip drives its first don't-care bit on, so its value is not sampled (see this
                // module's note on that bit).
                if bits.len() != header + 1 {
                    return Err(EepromError::BadBitCount);
                }
                Ok(EepromOutcome::Read(addr))
            }
            (true, false) => {
                // Write: `... 64 bits data ... 1 bit "0"`.
                if bits.len() != header + 64 + 1 {
                    return Err(EepromError::BadBitCount);
                }
                if bits[header + 64] {
                    return Err(EepromError::BadTerminator);
                }
                let data = Self::decode_data(&bits[header..header + 64]);
                let at = addr as usize * 8;
                self.data[at..at + 8].copy_from_slice(&data);
                self.busy_until = cycles + EEPROM_WRITE_CYCLES;
                Ok(EepromOutcome::Written)
            }
            _ => Err(EepromError::BadCommand),
        }
    }

    /// The 68-bit read response: "4 bits - ignore these, 64 bits - data (conventionally MSB first)".
    pub fn response(&self, addr: u16) -> Vec<bool> {
        let mut bits = vec![false; 4];
        bits.extend(self.block_bits(addr));
        bits
    }

    /// The request lengths a chip of this size answers: `(read, write)`. Before the size is decided
    /// the pair is the 8 KiB chip's, which is the longest stream the chip can be sent.
    fn request_lengths(&self) -> (usize, usize) {
        match self.size {
            EepromSize::K4bit => (9, 73),
            EepromSize::K64kbit => (17, 81),
            EepromSize::Undetermined => (17, 81),
        }
    }

    /// Whether the bits clocked in so far are a whole request whose length settles the chip's size
    /// on its own. With the size known that is the chip's own length. Before it is, only two lengths
    /// do: 17 bits *of a read* (`11`…`0`, the 8 KiB chip's read request) and 81 bits (its write).
    /// Seventeen bits of a write stream are the middle of the 73-bit 512-byte write, not a request.
    fn request_is_settled(&self) -> bool {
        let (read_len, write_len) = self.request_lengths();
        let len = self.command.len();
        if len < 2 {
            return false;
        }
        // GBATEK's two command prefixes: "11" is a read request, "10" a write request. A length on
        // its own does not settle anything — a 73-bit write passes through its 17th bit.
        let read = self.command[0] && self.command[1];
        let write = self.command[0] && !self.command[1];
        match self.size {
            EepromSize::K4bit | EepromSize::K64kbit => {
                (len == read_len && read) || (len == write_len && write)
            }
            EepromSize::Undetermined => (len == 17 && read) || (len == 81 && write),
        }
    }

    /// Whether the bits clocked in so far are a whole 512-byte chip's request — 9 bits for a read
    /// (`11`…`0`) or 73 for a write (`10`…`0`). Before the size is decided these are *also* the
    /// first bits of the 8 KiB chip's longer requests, so they are only a request once an access in
    /// the other direction says the stream has stopped.
    fn small_request_is_complete(&self) -> bool {
        match self.command.len() {
            9 => self.command[0] && self.command[1],
            73 => self.command[0] && !self.command[1],
            _ => false,
        }
    }

    /// Whether the bits clocked in so far are a whole 512-byte chip's write request — 73 bits
    /// (`10`…`0`) — which another write confirms as complete.
    fn small_write_is_pending(&self) -> bool {
        self.command.len() == 73 && self.command[0] && !self.command[1]
    }

    /// Decode the bits clocked in so far and let them go: a read request fills the read-out, a
    /// write programs its block, and a stream the chip refuses leaves the data alone.
    fn take_request(&mut self, cycles: u64) {
        let bits = std::mem::take(&mut self.command);
        if let Ok(EepromOutcome::Read(addr)) = self.receive(&bits, cycles) {
            self.readout = self.response(addr);
            self.readout_pos = 0;
        }
    }

    /// One bit clocked into the chip: a write of the window. GBATEK, *Using DMA*: "one halfword for
    /// each bit, bit1-15 of the halfwords are don't care, only bit0 is used".
    ///
    /// A write while a read-out is still shifting abandons the rest of it — the cartridge has begun
    /// a new request. A whole 512-byte write still waiting for the ready poll is programmed now if
    /// the cartridge has begun another request instead, which is the only way the chip can tell a
    /// 73-bit write from a 74th bit of an 81-bit one; and a stream longer than any documented
    /// request is dropped rather than accumulated forever.
    pub fn write_bit(&mut self, bit: bool, cycles: u64) {
        self.readout_pos = self.readout.len();
        if self.size == EepromSize::Undetermined && self.small_write_is_pending() {
            self.take_request(cycles);
        }
        self.command.push(bit);
        if self.request_is_settled() {
            self.take_request(cycles);
        } else if self.command.len() > self.request_lengths().1 {
            self.command.clear();
        }
    }

    /// One bit clocked out of the chip: a read of the window. While a read request's data is being
    /// shifted out that is the data; otherwise it is the chip's Ready line, which is what GBATEK,
    /// *Write Data to Address* tells the caller to poll: "keep reading from the chip, by normal LDRH
    /// ..., until Bit 0 of the returned data becomes 1 (Ready)".
    ///
    /// This is also where a 512-byte request is told apart from the start of an 8 KiB one: a read
    /// access means the cartridge has stopped clocking bits in, so the bits it left are a whole
    /// 512-byte request if they have one of that chip's shapes.
    pub fn read_bit(&mut self, cycles: u64) -> bool {
        if self.readout_pos >= self.readout.len()
            && (self.small_request_is_complete() || self.request_is_settled())
        {
            self.take_request(cycles);
        }
        if self.readout_pos < self.readout.len() {
            let bit = self.readout[self.readout_pos];
            self.readout_pos += 1;
            return bit;
        }
        !self.busy(cycles)
    }
}

impl Default for Eeprom {
    fn default() -> Eeprom {
        Eeprom::new()
    }
}

// ---------------------------------------------------------------- the RTC and GPIO

/// The RTC cart's GPIO port (GBATEK *GBA Cart I/O Port (GPIO)*): 4 data bits at `$80000C4`, 4
/// direction bits at `$80000C6`, and the read/write control at `$80000C8`. The bits GBATEK's
/// *Connection Examples* table pins for the RTC: bit 0 = SCK (clock), bit 1 = SIO (data), bit 2 =
/// CS (chip select).
#[derive(Clone, Copy, Debug, Default)]
pub struct Gpio {
    /// `$80000C4` bits 0-3: data (0=Low, 1=High).
    pub data: u16,
    /// `$80000C6` bits 0-3: direction (0=In, 1=Out).
    pub direction: u16,
    /// `$80000C8` bit 0: the port is read/write (1) or write-only (0).
    pub control: u16,
}

impl Gpio {
    /// A 16-bit read of `$80000C4`/`C6`/`C8`, in write-only mode reading 0.
    pub fn read(&self, offset: u16) -> u16 {
        let readable = self.control & 1 != 0;
        if !readable {
            return 0;
        }
        match offset {
            0 => self.data & 0xF,
            2 => self.direction & 0xF,
            4 => self.control & 0x1,
            _ => 0,
        }
    }

    /// A 16-bit write to `$80000C4`/`C6`/`C8`.
    pub fn write(&mut self, offset: u16, value: u16) {
        match offset {
            0 => self.data = value & 0xF,
            2 => self.direction = value & 0xF,
            4 => self.control = value & 0x1,
            _ => {}
        }
    }
}

/// The S-3511A RTC (GBATEK *GBA Cart Real-Time Clock (RTC)*): a control register, a 7-byte datetime
/// (year, month, day, day-of-week, hour, minute, second, BCD) and a 3-byte time register, accessed
/// over the GPIO's three wires. The clock is taken forward by the machine's own clock so a cartridge
/// that reads it never sees it stand still.
#[derive(Clone, Debug)]
pub struct Rtc {
    pub gpio: Gpio,
    /// Seconds since 2000-01-01 00:00:00, the RTC's wall clock. Advanced by [`Rtc::tick`].
    base_seconds: u64,
    /// Cycle accumulator, so a whole second is reached only once per 16 777 216 cycles.
    cycle_remainder: u64,
    /// The control register: bit 6 = 12/24-hour mode (1 = 24h), bit 7 = power-off (read, cleared).
    pub control: u8,
    /// Serial-interface state (command byte + data, shifted MSB first).
    cs: bool,
    sck: bool,
    command: Option<u8>,
    shift: u8,
    shift_bits: u8,
    /// The data byte being shifted out on a read.
    out_byte: u8,
    out_bits: u8,
    /// Bytes read/written so far for the selected register.
    reg_pos: u8,
    /// The register currently being read or written.
    write_pending: bool,
}

/// RTC register selectors: the command byte is `$60 | (register << 1) | read/write` (GBATEK's
/// *Comparision of RTC Registers* table, and the S-3511A's own command encoding).
const RTC_REG_RESET: u8 = 0;
const RTC_REG_CONTROL: u8 = 1;
const RTC_REG_DATETIME: u8 = 2;
const RTC_REG_TIME: u8 = 3;

impl Rtc {
    /// Power-on: GBATEK, "Setting after Battery-Shortcut is 82h" — the power-off flag is set, so the
    /// time is lost and starts at the epoch.
    pub fn new() -> Rtc {
        Rtc {
            gpio: Gpio::default(),
            base_seconds: 0,
            cycle_remainder: 0,
            control: 0x82,
            cs: false,
            sck: false,
            command: None,
            shift: 0,
            shift_bits: 0,
            out_byte: 0,
            out_bits: 0,
            reg_pos: 0,
            write_pending: false,
        }
    }

    /// The 7-byte datetime register in BCD: year (00-99), month (01-12), day (01-31), day-of-week
    /// (0=Monday..6=Sunday), hour (0-23), minute, second.
    pub fn datetime(&self) -> [u8; 7] {
        let (year, month, day) = civil_from_days(self.days() + days_from_civil(2000, 1, 1));
        let hour = (self.base_seconds % 86_400) / 3_600;
        let minute = (self.base_seconds % 3_600) / 60;
        let second = self.base_seconds % 60;
        [
            to_bcd((year % 100) as u8),
            to_bcd(month as u8),
            to_bcd(day as u8),
            self.day_of_week(),
            to_bcd(hour as u8),
            to_bcd(minute as u8),
            to_bcd(second as u8),
        ]
    }

    /// The 3-byte time register: hour, minute, second, BCD.
    pub fn time(&self) -> [u8; 3] {
        let dt = self.datetime();
        [dt[4], dt[5], dt[6]]
    }

    /// Days since the epoch (2000-01-01).
    fn days(&self) -> i64 {
        (self.base_seconds / 86_400) as i64
    }

    /// Day of week: 0=Monday..6=Sunday (2000-01-01 was a Saturday).
    fn day_of_week(&self) -> u8 {
        ((self.days() + 5).rem_euclid(7)) as u8
    }

    /// Take the clock forward over `cycles` master cycles, one second per 16 777 216 cycles.
    pub fn tick(&mut self, cycles: u64) {
        let total = self.cycle_remainder + cycles;
        self.base_seconds += total / CYCLES_PER_SECOND;
        self.cycle_remainder = total % CYCLES_PER_SECOND;
    }

    /// Read a whole second of wall clock (for the save footer's base time).
    pub fn base_seconds(&self) -> u64 {
        self.base_seconds
    }

    /// The bytes of one register, in shift-out order.
    fn register_bytes(&self, register: u8) -> Vec<u8> {
        match register {
            RTC_REG_CONTROL => vec![self.control],
            RTC_REG_DATETIME => self.datetime().to_vec(),
            RTC_REG_TIME => self.time().to_vec(),
            _ => vec![0xFF],
        }
    }

    /// Apply a write of one register's bytes.
    fn apply_write(&mut self, register: u8, bytes: &[u8]) {
        match register {
            RTC_REG_CONTROL => {
                if let Some(&value) = bytes.first() {
                    // Bit 7 (power-off) is read-only; it is cleared on read, not written.
                    self.control = value & 0x7F;
                }
            }
            RTC_REG_DATETIME if bytes.len() >= 7 => {
                let (hour, minute, second) =
                    (from_bcd(bytes[4]), from_bcd(bytes[5]), from_bcd(bytes[6]));
                let (year, month, day) = (
                    from_bcd(bytes[0]) as i64 + 2000,
                    from_bcd(bytes[1]) as u32,
                    from_bcd(bytes[2]) as u32,
                );
                let days = days_from_civil(year, month, day) - days_from_civil(2000, 1, 1);
                self.base_seconds = days as u64 * 86_400
                    + u64::from(hour) * 3600
                    + u64::from(minute) * 60
                    + u64::from(second);
                self.cycle_remainder = 0;
            }
            RTC_REG_TIME if bytes.len() >= 3 => {
                let days = self.days();
                let (hour, minute, second) =
                    (from_bcd(bytes[0]), from_bcd(bytes[1]), from_bcd(bytes[2]));
                self.base_seconds = days as u64 * 86_400
                    + u64::from(hour) * 3600
                    + u64::from(minute) * 60
                    + u64::from(second);
                self.cycle_remainder = 0;
            }
            RTC_REG_RESET => {
                // GBATEK: "reset all RTC registers (all used registers become 00h, except
                // day/month which become 01h)".
                self.control = 0;
                self.base_seconds = 0;
                self.cycle_remainder = 0;
            }
            _ => {}
        }
    }

    /// One GPIO data write (bit 0 = SCK, bit 1 = SIO, bit 2 = CS), advancing the serial protocol.
    /// Returns the bit the RTC drives on SIO for this cycle (used when the CPU reads SIO).
    pub fn gpio_write(&mut self, data: u16) -> bool {
        let cs = data & 0x4 != 0;
        let sck = data & 0x1 != 0;
        let sio = data & 0x2 != 0;
        let mut out = sio;
        if cs && !self.cs {
            // Rising CS begins a transaction.
            self.command = None;
            self.shift = 0;
            self.shift_bits = 0;
            self.reg_pos = 0;
            self.write_pending = false;
        }
        if cs && sck && !self.sck {
            // Rising SCK shifts one bit, MSB first.
            match self.command {
                None => {
                    self.shift = (self.shift << 1) | u8::from(sio);
                    self.shift_bits += 1;
                    if self.shift_bits == 8 {
                        self.command = Some(self.shift);
                        self.write_pending = self.shift & 1 == 0;
                        self.reg_pos = 0;
                        self.shift_bits = 0;
                    }
                }
                Some(command) => {
                    if self.write_pending {
                        self.shift = (self.shift << 1) | u8::from(sio);
                        self.shift_bits += 1;
                        if self.shift_bits == 8 {
                            let register = (command >> 1) & 0x7;
                            let bytes = self.register_bytes(register);
                            let pos = self.reg_pos as usize;
                            let mut new = bytes;
                            if pos < new.len() {
                                new[pos] = self.shift;
                            }
                            self.apply_write(register, &new);
                            self.reg_pos += 1;
                            self.shift = 0;
                            self.shift_bits = 0;
                        }
                    } else {
                        // Read: prepare the next byte to shift out.
                        if self.out_bits == 0 {
                            let register = (command >> 1) & 0x7;
                            let bytes = self.register_bytes(register);
                            let pos = self.reg_pos as usize;
                            self.out_byte = bytes.get(pos).copied().unwrap_or(0xFF);
                            self.reg_pos += 1;
                            self.out_bits = 8;
                        }
                        out = self.out_byte & 0x80 != 0;
                        self.out_byte <<= 1;
                        self.out_bits -= 1;
                    }
                }
            }
        }
        self.cs = cs;
        self.sck = sck;
        out
    }
}

impl Default for Rtc {
    fn default() -> Rtc {
        Rtc::new()
    }
}

/// Binary-coded decimal: `42` becomes `$42`.
fn to_bcd(value: u8) -> u8 {
    (value / 10) * 16 + (value % 10)
}

/// `$42` becomes `42`.
fn from_bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0xF)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------- the save window

/// The save window's read/write surface, shared by SRAM and Flash's data path, plus the open-bus
/// behaviour of a cartridge with no chip. The bus routes `$0E000000-$0FFFFFFF` here.
impl Save {
    /// The chip type, for the report and the tests.
    pub fn save_type(&self) -> SaveType {
        match self {
            Save::None => SaveType::None,
            Save::Sram(_) => SaveType::Sram,
            Save::Flash(flash) => match flash.size {
                FlashSize::K64 => SaveType::Flash64,
                FlashSize::K128 => SaveType::Flash128,
            },
            Save::Eeprom(eeprom) => match eeprom.size {
                EepromSize::K4bit | EepromSize::Undetermined => SaveType::Eeprom512,
                EepromSize::K64kbit => SaveType::Eeprom8k,
            },
        }
    }

    /// One byte of the window. `$FF` for no chip (what `none.gba` asserts) and for an EEPROM cart,
    /// whose chip is not in this window.
    pub fn read8(&mut self, addr: u32, cycles: u64) -> u8 {
        match self {
            Save::Sram(sram) => sram.read8(addr),
            Save::Flash(flash) => flash.read8(addr, cycles),
            Save::None | Save::Eeprom(_) => ERASED,
        }
    }

    pub fn read16(&mut self, addr: u32, cycles: u64) -> u16 {
        match self {
            Save::Sram(sram) => sram.read16(addr),
            Save::Flash(flash) => flash.read16(addr, cycles),
            Save::None | Save::Eeprom(_) => replicate(ERASED, 2) as u16,
        }
    }

    pub fn read32(&mut self, addr: u32, cycles: u64) -> u32 {
        match self {
            Save::Sram(sram) => sram.read32(addr),
            Save::Flash(flash) => flash.read32(addr, cycles),
            Save::None | Save::Eeprom(_) => replicate(ERASED, 4) as u32,
        }
    }

    pub fn write8(&mut self, addr: u32, value: u8, cycles: u64) {
        self.provision_on_write();
        match self {
            Save::Sram(sram) => sram.write8(addr, value),
            Save::Flash(flash) => flash.write8(addr, value, cycles),
            Save::None | Save::Eeprom(_) => {}
        }
    }

    pub fn write16(&mut self, addr: u32, value: u16, cycles: u64) {
        self.provision_on_write();
        match self {
            Save::Sram(sram) => sram.write16(addr, value),
            Save::Flash(flash) => flash.write16(addr, value, cycles),
            Save::None | Save::Eeprom(_) => {}
        }
    }

    pub fn write32(&mut self, addr: u32, value: u32, cycles: u64) {
        self.provision_on_write();
        match self {
            Save::Sram(sram) => sram.write32(addr, value),
            Save::Flash(flash) => flash.write32(addr, value, cycles),
            Save::None | Save::Eeprom(_) => {}
        }
    }

    /// A **store** into the window is proof that a chip is there, so a cartridge whose ROM names no
    /// backup type gets the 32 KiB chip GBATEK's *GBA Memory Map* puts at `0E000000-0E00FFFF` ("Game
    /// Pak SRAM (max 64 KBytes)") the first time it writes one.
    ///
    /// The header cannot be asked: GBATEK, *Backup IDs* — "Nintendo didn't include a backup-type
    /// entry in the ROM header, however, the required type can be detected by ID strings in the
    /// ROM-image." A ROM laid out without the string therefore resolves to [`SaveType::None`], and
    /// three cartridges in the reference library are in exactly that state: they store a signature
    /// into `$0E000000`, read it back, and retry forever while the store is dropped. A store is the
    /// only admissible evidence — a cartridge reads `$0D000000` for its own reasons, and with no chip
    /// that address is the Game Pak ROM mirror (GBATEK, *GBA Memory Map*: `0C000000-0DFFFFFF` Game
    /// Pak ROM), so a read never creates a chip.
    ///
    /// Only [`Save::None`] can be promoted: a cartridge whose own header named a chip keeps the chip
    /// it named, byte for byte, and an EEPROM's window is not this one.
    fn provision_on_write(&mut self) {
        if matches!(self, Save::None) {
            *self = Save::Sram(Sram::new());
        }
    }

    /// The EEPROM, for the DMA-driven protocol (the bus does not decode `$0D000000` by hand; G4's
    /// DMA does). `None` for a cartridge without one.
    pub fn eeprom(&mut self) -> Option<&mut Eeprom> {
        match self {
            Save::Eeprom(eeprom) => Some(eeprom),
            _ => None,
        }
    }

    /// The battery body: the chip's raw bytes, or `None` for a cartridge with none (or with an EEPROM
    /// whose size is still undecided). An RTC footer is appended by [`encode_battery`].
    pub fn body(&self) -> Option<Vec<u8>> {
        match self {
            Save::Sram(sram) => Some(sram.data().to_vec()),
            Save::Flash(flash) => Some(flash.data().to_vec()),
            Save::Eeprom(eeprom) => eeprom.data().map(<[u8]>::to_vec),
            Save::None => None,
        }
    }

    /// Read a battery body back. A wrong length is refused without touching the chip.
    pub fn load_body(&mut self, bytes: &[u8]) -> Result<bool, String> {
        match self {
            Save::Sram(sram) => sram.load(bytes).map(|()| true),
            Save::Flash(flash) => flash.load(bytes).map(|()| true),
            Save::Eeprom(eeprom) => eeprom.load(bytes).map(|()| true),
            Save::None => Err("cartridge has no save chip".to_string()),
        }
    }
}

// ---------------------------------------------------------------- state files and the battery

/// The RTC footer's marker, after the chip bytes (the Game Boy path's `RTCL` is its sibling).
const RTC_MAGIC: [u8; 4] = *b"GART";
/// The footer's version.
const RTC_VERSION: u8 = 1;

/// The whole battery body a `.sav` holds: the chip's raw bytes, then, for an RTC cart, a `GART`
/// footer with the clock's control register and base time. `None` for a cartridge with no chip and
/// no clock.
pub fn encode_battery(save: &Save, rtc: Option<&Rtc>) -> Option<Vec<u8>> {
    let chip = save.body();
    if chip.is_none() && rtc.is_none() {
        return None;
    }
    let mut body = chip.unwrap_or_default();
    if let Some(rtc) = rtc {
        body.extend_from_slice(&RTC_MAGIC);
        body.push(RTC_VERSION);
        body.push(rtc.control);
        body.extend_from_slice(&rtc.base_seconds.to_le_bytes());
    }
    Some(body)
}

/// Read [`encode_battery`]'s body back. A file whose size matches neither the chip nor the chip +
/// RTC footer was written for a different save chip: a short one is padded with 0xFF and a long one
/// truncated to the chip (one line each, and neither writes outside the chip). Returns whether
/// anything was loaded.
pub fn load_battery(save: &mut Save, rtc: &mut Option<Rtc>, bytes: &[u8]) -> Result<bool, String> {
    if bytes.is_empty() {
        return Ok(false);
    }
    // An EEPROM whose size is not yet decided is sized by the save file's own length (512 B or
    // 8 KiB); there is no RTC footer to split off in that case.
    if matches!(save, Save::Eeprom(eeprom) if eeprom.known_size().is_none()) {
        return save.load_body(bytes);
    }
    let chip_len = save.body().map_or(0, |body| body.len());
    if bytes.len() == chip_len {
        if chip_len > 0 {
            save.load_body(&bytes[..chip_len])?;
        }
        return Ok(true);
    }
    // A longer body carries the RTC footer: `GART`, version, control, base time.
    let footer_len = RTC_MAGIC.len() + 1 + 1 + 8;
    if bytes.len() == chip_len + footer_len
        && bytes[chip_len..chip_len + RTC_MAGIC.len()] == RTC_MAGIC
    {
        if bytes[chip_len + RTC_MAGIC.len()] != RTC_VERSION {
            return Err(format!(
                "save file has RTC format version {}, expected {RTC_VERSION}",
                bytes[chip_len + RTC_MAGIC.len()]
            ));
        }
        if chip_len > 0 {
            save.load_body(&bytes[..chip_len])?;
        }
        let control = bytes[chip_len + RTC_MAGIC.len() + 1];
        let base_seconds = u64::from_le_bytes(
            bytes[chip_len + RTC_MAGIC.len() + 2..chip_len + footer_len]
                .try_into()
                .map_err(|_| "save file is corrupt".to_string())?,
        );
        let mut new_rtc = Rtc::new();
        new_rtc.control = control;
        new_rtc.base_seconds = base_seconds;
        *rtc = Some(new_rtc);
        return Ok(true);
    }
    // A wrong size (a chip detected differently from the one that wrote the file): pad a short file
    // with 0xFF and truncate a long one to the chip.
    if chip_len == 0 {
        return Err("cartridge has no save chip".to_string());
    }
    let mut body = bytes.to_vec();
    if body.len() < chip_len {
        body.resize(chip_len, ERASED);
    } else {
        body.truncate(chip_len);
    }
    save.load_body(&body)?;
    Ok(true)
}

/// The save chip's and clock's state, written and read in one place so `Gba::encode`/`decode` stay a
/// list of device calls.
impl Save {
    /// The chip's state, in a form [`Save::decode`] reads back.
    pub fn encode(&self, w: &mut StateWriter) {
        match self {
            Save::None => w.u8(0),
            Save::Sram(sram) => {
                w.u8(1);
                w.bytes(sram.data());
            }
            Save::Flash(flash) => {
                w.u8(match flash.size {
                    FlashSize::K64 => 2,
                    FlashSize::K128 => 3,
                });
                w.bytes(flash.data());
                w.u8(flash.bank);
                w.u8(u8::from(flash.id_mode));
                w.u8(match flash.unlock {
                    Unlock::Idle => 0,
                    Unlock::Aa => 1,
                    Unlock::Aa55 => 2,
                    Unlock::Erase => 3,
                    Unlock::EraseAa => 4,
                    Unlock::Erase55 => 5,
                });
                w.u8(u8::from(flash.program_armed));
                w.u8(u8::from(flash.bank_armed));
                match flash.pending {
                    None => w.u8(0),
                    Some((PendingErase::Chip, deadline)) => {
                        w.u8(1);
                        w.u64(deadline);
                    }
                    Some((PendingErase::Sector(n), deadline)) => {
                        w.u8(2);
                        w.u16(n);
                        w.u64(deadline);
                    }
                }
            }
            Save::Eeprom(eeprom) => {
                w.u8(4);
                w.u8(match eeprom.size {
                    EepromSize::K4bit => 0,
                    EepromSize::K64kbit => 1,
                    EepromSize::Undetermined => 2,
                });
                match eeprom.data() {
                    Some(data) => w.bytes(data),
                    None => w.bytes(&[]),
                }
                w.u64(eeprom.busy_until);
            }
        }
    }

    /// [`Save::encode`]'s reader. Every field is read before anything is written, so a damaged state
    /// leaves the running machine untouched.
    pub fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let tag = r.u8()?;
        match tag {
            0 => *self = Save::None,
            1 => {
                let mut sram = Sram::new();
                r.bytes_into(&mut sram.data[..])?;
                *self = Save::Sram(sram);
            }
            2 | 3 => {
                let size = if tag == 2 {
                    FlashSize::K64
                } else {
                    FlashSize::K128
                };
                let mut flash = Flash::new(size);
                r.bytes_into(&mut flash.data[..])?;
                flash.bank = r.u8()?;
                flash.id_mode = r.bool()?;
                flash.unlock = match r.u8()? {
                    0 => Unlock::Idle,
                    1 => Unlock::Aa,
                    2 => Unlock::Aa55,
                    3 => Unlock::Erase,
                    4 => Unlock::EraseAa,
                    _ => Unlock::Erase55,
                };
                flash.program_armed = r.bool()?;
                flash.bank_armed = r.bool()?;
                flash.pending = match r.u8()? {
                    0 => None,
                    1 => Some((PendingErase::Chip, r.u64()?)),
                    _ => Some((PendingErase::Sector(r.u16()?), r.u64()?)),
                };
                *self = Save::Flash(flash);
            }
            4 => {
                let size = match r.u8()? {
                    0 => EepromSize::K4bit,
                    1 => EepromSize::K64kbit,
                    _ => EepromSize::Undetermined,
                };
                let mut eeprom = Eeprom::with_size(size);
                let mut data = vec![0u8; eeprom.data.len()];
                r.bytes_into(&mut data)?;
                eeprom.data.copy_from_slice(&data);
                eeprom.busy_until = r.u64()?;
                *self = Save::Eeprom(eeprom);
            }
            _ => return Err(StateError::BadValue),
        }
        Ok(())
    }
}

/// [`Rtc`]'s state: the clock and the GPIO ports. The serial interface's mid-transaction state is
/// transient and deliberately not saved, since a state is only ever written between frames.
impl Rtc {
    pub fn encode(&self, w: &mut StateWriter) {
        w.u64(self.base_seconds);
        w.u64(self.cycle_remainder);
        w.u8(self.control);
        w.u16(self.gpio.data);
        w.u16(self.gpio.direction);
        w.u16(self.gpio.control);
    }

    pub fn decode(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let base_seconds = r.u64()?;
        let cycle_remainder = r.u64()?;
        let control = r.u8()?;
        let data = r.u16()?;
        let direction = r.u16()?;
        let gpio_control = r.u16()?;
        self.base_seconds = base_seconds;
        self.cycle_remainder = cycle_remainder;
        self.control = control;
        self.gpio.data = data;
        self.gpio.direction = direction;
        self.gpio.control = gpio_control;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM with an ID string placed at a word-aligned offset, the way Nintendo's tools lay it out.
    fn rom_with_id(id: &[u8]) -> Vec<u8> {
        let mut rom = vec![0u8; 0x200];
        let at = 0x100;
        rom[at..at + id.len()].copy_from_slice(id);
        rom
    }

    // ---------------------------------------------------------------- detection

    #[test]
    fn the_id_strings_name_the_save_type() {
        for (id, want) in [
            (&b"SRAM_V001"[..], SaveType::Sram),
            (&b"FLASH_V001"[..], SaveType::Flash64),
            (&b"FLASH512_V001"[..], SaveType::Flash64),
            (&b"FLASH1M_V001"[..], SaveType::Flash128),
            (&b"EEPROM_V122"[..], SaveType::Eeprom512),
        ] {
            assert_eq!(
                detect(&rom_with_id(id)),
                want,
                "{}",
                String::from_utf8_lossy(id)
            );
        }
        // No string means no chip.
        assert_eq!(detect(&rom_with_id(b"HELLO")), SaveType::None);
    }

    #[test]
    fn flash1m_wins_over_the_bare_flash_string() {
        // A ROM carrying both strings is 128 KiB, not 64 KiB: the "1M" string is checked first.
        let mut rom = rom_with_id(b"FLASH_V001");
        rom[0x140..0x14C].copy_from_slice(b"FLASH1M_V002");
        assert_eq!(detect(&rom), SaveType::Flash128);
    }

    #[test]
    fn the_rtc_string_is_detected_apart_from_the_chip() {
        assert!(has_rtc(&rom_with_id(b"SIIRTC_V001")));
        assert!(!has_rtc(&rom_with_id(b"SRAM_V001")));
    }

    // ---------------------------------------------------------------- SRAM byte lanes

    #[test]
    fn sram_is_thirty_two_kib_filled_with_ff_and_mirrors() {
        let mut sram = Sram::new();
        assert_eq!(sram.read8(0x0E00_0000), 0xFF, "never written reads 0xFF");
        sram.write8(0x0E00_0000, 0x42);
        assert_eq!(sram.read8(0x0E00_0000), 0x42);
        assert_eq!(
            sram.read8(0x0E00_8000),
            0x42,
            "the 32 KiB chip repeats twice"
        );
        assert_eq!(
            sram.read8(0x0F00_0000),
            0x42,
            "and across the 32 MiB region"
        );
    }

    #[test]
    fn sram_wide_accesses_take_the_lane_the_suite_pins() {
        // `strb 1` then `ldrh` is `$0101`, and `ldr` is `$01010101` (sram.gba t004/t005).
        let mut sram = Sram::new();
        sram.write8(0x0E00_0000, 1);
        assert_eq!(sram.read16(0x0E00_0000), 0x0101);
        assert_eq!(sram.read32(0x0E00_0000), 0x0101_0101);
        // `strh $AABB` at an even address stores $BB, at an odd address stores $AA (t006).
        let mut sram = Sram::new();
        sram.write16(0x0E00_0000, 0xAABB);
        assert_eq!(sram.read8(0x0E00_0000), 0xBB);
        assert_eq!(
            sram.read8(0x0E00_0001),
            0xFF,
            "only the lane byte is written"
        );
        sram.write16(0x0E00_0001, 0xAABB);
        assert_eq!(sram.read8(0x0E00_0001), 0xAA);
        // `str $AABBCCDD` stores one byte per lane (t008/t009).
        let mut sram = Sram::new();
        sram.write32(0x0E00_0000, 0xAABB_CCDD);
        assert_eq!(sram.read8(0x0E00_0000), 0xDD);
        assert_eq!(sram.read8(0x0E00_0001), 0xFF);
        sram.write32(0x0E00_0002, 0xAABB_CCDD);
        assert_eq!(sram.read8(0x0E00_0002), 0xBB);
    }

    // ---------------------------------------------------------------- flash

    /// The unlock pair + a command byte, at cycle `cycles`.
    fn flash_cmd(flash: &mut Flash, cmd: u8, cycles: u64) {
        flash.write8(0x0E00_5555, 0xAA, cycles);
        flash.write8(0x0E00_2AAA, 0x55, cycles);
        flash.write8(0x0E00_5555, cmd, cycles);
    }

    #[test]
    fn flash_reads_ff_until_written_and_programs_a_byte_after_a0() {
        let mut flash = Flash::new(FlashSize::K64);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0xFF);
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_0000, 0x42, 0);
        assert_eq!(
            flash.read8(0x0E00_0000, 0),
            0x42,
            "the byte is there at once"
        );
        assert_eq!(flash.read16(0x0E00_0000, 0), 0x4242, "wide reads replicate");
    }

    #[test]
    fn flash_erase_chip_fills_ff_after_a_cycle_count() {
        let mut flash = Flash::new(FlashSize::K64);
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_0000, 0x00, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0x00);
        flash_cmd(&mut flash, 0x80, 0);
        flash_cmd(&mut flash, 0x10, 0);
        // Busy until the deadline: the cell reads 0x00, not 0xFF.
        assert_eq!(flash.read8(0x0E00_0000, 0), 0x00, "busy during the erase");
        assert_eq!(flash.read8(0x0E00_0000, ERASE_CHIP_CYCLES), 0xFF, "erased");
    }

    #[test]
    fn flash_erase_sector_fills_only_that_sector() {
        let mut flash = Flash::new(FlashSize::K64);
        // Program two bytes in different sectors.
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_0000, 0x00, 0);
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_1000, 0x00, 0);
        // Erase sector 0: AA/55/80, then a second AA/55, then 30 to the sector's address.
        flash_cmd(&mut flash, 0x80, 0);
        flash.write8(0x0E00_5555, 0xAA, 0);
        flash.write8(0x0E00_2AAA, 0x55, 0);
        flash.write8(0x0E00_0000, 0x30, 0);
        assert_eq!(
            flash.read8(0x0E00_0000, ERASE_SECTOR_CYCLES),
            0xFF,
            "sector 0 erased"
        );
        assert_eq!(
            flash.read8(0x0E00_1000, ERASE_SECTOR_CYCLES),
            0x00,
            "sector 1 untouched"
        );
    }

    #[test]
    fn flash_identifies_itself_and_exits() {
        let mut flash = Flash::new(FlashSize::K64);
        flash_cmd(&mut flash, 0x90, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0x32, "Panasonic manufacturer");
        assert_eq!(flash.read8(0x0E00_0001, 0), 0x1B, "device");
        flash_cmd(&mut flash, 0xF0, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0xFF, "back to the array");

        let mut flash = Flash::new(FlashSize::K128);
        flash_cmd(&mut flash, 0x90, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0x62, "Sanyo manufacturer");
        assert_eq!(flash.read8(0x0E00_0001, 0), 0x13, "device");
    }

    #[test]
    fn flash128_bank_switching_selects_the_other_half() {
        let mut flash = Flash::new(FlashSize::K128);
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_0000, 1, 0);
        // Switch to bank 1: the byte at the window's start is a different cell.
        flash_cmd(&mut flash, 0xB0, 0);
        flash.write8(0x0E00_0000, 1, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 0xFF, "bank 1 is empty");
        // Write there, switch back, and the old value is still in bank 0.
        flash_cmd(&mut flash, 0xA0, 0);
        flash.write8(0x0E00_0000, 2, 0);
        flash_cmd(&mut flash, 0xB0, 0);
        flash.write8(0x0E00_0000, 0, 0);
        assert_eq!(flash.read8(0x0E00_0000, 0), 1, "bank 0 kept its byte");
    }

    // ---------------------------------------------------------------- EEPROM

    /// The read request bits for a chip: `11`, the address (MSB first), `0`.
    fn read_request(addr: u16, address_bits: usize) -> Vec<bool> {
        let mut bits = vec![true, true];
        for bit in (0..address_bits).rev() {
            bits.push(addr & (1 << bit) != 0);
        }
        bits.push(false);
        bits
    }

    /// The write request bits: `10`, the address, 64 data bits (MSB first), `0`.
    fn write_request(addr: u16, data: [u8; 8], address_bits: usize) -> Vec<bool> {
        let mut bits = vec![true, false];
        for bit in (0..address_bits).rev() {
            bits.push(addr & (1 << bit) != 0);
        }
        for &byte in &data {
            for bit in (0..8).rev() {
                bits.push(byte & (1 << bit) != 0);
            }
        }
        bits.push(false);
        bits
    }

    #[test]
    fn eeprom_read_returns_four_dummy_bits_and_the_block_msb_first() {
        let mut eeprom = Eeprom::with_size(EepromSize::K4bit);
        // Write block 3 (the 8 bytes at offset 24).
        let data = *b"ABCDEFGH";
        assert_eq!(
            eeprom.receive(&write_request(3, data, 6), 0),
            Ok(EepromOutcome::Written)
        );
        let response = eeprom.response(3);
        assert_eq!(response.len(), 68, "4 dummy + 64 data bits");
        assert!(
            response[..4].iter().all(|&bit| !bit),
            "the dummy bits are 0"
        );
        // Reassemble the 64 data bits and compare with the block.
        let mut got = [0u8; 8];
        for (byte_index, byte) in got.iter_mut().enumerate() {
            for bit_index in 0..8 {
                if response[4 + byte_index * 8 + bit_index] {
                    *byte |= 1 << (7 - bit_index);
                }
            }
        }
        assert_eq!(got, data);
    }

    #[test]
    fn eeprom_read_request_decodes_the_address() {
        let mut eeprom = Eeprom::with_size(EepromSize::K4bit);
        assert_eq!(
            eeprom.receive(&read_request(0x2A, 6), 0),
            Ok(EepromOutcome::Read(0x2A))
        );
        let mut eeprom = Eeprom::with_size(EepromSize::K64kbit);
        assert_eq!(
            eeprom.receive(&read_request(0x2AA, 14), 0),
            Ok(EepromOutcome::Read(0x2AA))
        );
    }

    #[test]
    fn eeprom_decides_its_size_from_the_first_request() {
        let mut eeprom = Eeprom::new();
        assert_eq!(eeprom.known_size(), None);
        assert_eq!(
            eeprom.receive(&read_request(0, 6), 0),
            Ok(EepromOutcome::Read(0))
        );
        assert_eq!(eeprom.known_size(), Some(EepromSize::K4bit));

        let mut eeprom = Eeprom::new();
        assert_eq!(
            eeprom.receive(&read_request(0, 14), 0),
            Ok(EepromOutcome::Read(0))
        );
        assert_eq!(eeprom.known_size(), Some(EepromSize::K64kbit));
    }

    #[test]
    fn a_wrong_bit_count_or_torn_write_is_refused_without_touching_the_data() {
        let mut eeprom = Eeprom::with_size(EepromSize::K4bit);
        // A 512-byte chip expects a 9-bit read; 8 bits is not a request.
        let bad = vec![true, true, false, false, false, false, false, false];
        assert_eq!(eeprom.receive(&bad, 0), Err(EepromError::BadBitCount));
        // A torn write (a 73-bit request cut short) is refused and nothing is programmed.
        let mut request = write_request(0, *b"ABCDEFGH", 6);
        request.truncate(40);
        assert_eq!(eeprom.receive(&request, 0), Err(EepromError::BadBitCount));
        assert!(
            eeprom.response(0)[4..].iter().all(|&bit| bit),
            "fresh data is 0xFF"
        );
    }

    #[test]
    fn a_bad_command_or_terminator_is_refused() {
        let mut eeprom = Eeprom::with_size(EepromSize::K4bit);
        // Leading bits "00" are not a read or a write.
        let mut bad = read_request(0, 6);
        bad[0] = false;
        bad[1] = false;
        assert_eq!(eeprom.receive(&bad, 0), Err(EepromError::BadCommand));
        // A write request whose trailing bit is 1, not 0.
        let mut bad = write_request(0, *b"ABCDEFGH", 6);
        *bad.last_mut().unwrap() = true;
        assert_eq!(eeprom.receive(&bad, 0), Err(EepromError::BadTerminator));
        // A read request's trailing bit is the chip's own clock, so either value is a request.
        let mut ok = read_request(0, 6);
        *ok.last_mut().unwrap() = true;
        assert_eq!(eeprom.receive(&ok, 0), Ok(EepromOutcome::Read(0)));
    }

    // ---------------------------------------------------------------- RTC

    #[test]
    fn the_rtc_starts_at_the_epoch_in_bcd_and_ticks_forward() {
        let mut rtc = Rtc::new();
        // 2000-01-01 00:00:00, a Saturday (day-of-week 5 with 0=Monday); the power-off flag is set.
        assert_eq!(rtc.datetime(), [0x00, 0x01, 0x01, 5, 0x00, 0x00, 0x00]);
        assert_eq!(
            rtc.control, 0x82,
            "power-off flag set after a battery shortcut"
        );
        // One second is 16 777 216 master cycles.
        rtc.tick(CYCLES_PER_SECOND);
        assert_eq!(rtc.datetime()[6], 0x01, "the second is BCD 01");
        rtc.tick(CYCLES_PER_SECOND * 59);
        assert_eq!(rtc.datetime()[5], 0x01, "sixty seconds is one minute");
        assert_eq!(rtc.time(), [0x00, 0x01, 0x00]);
    }

    #[test]
    fn the_gpio_ports_hold_their_nibbles() {
        let mut gpio = Gpio {
            control: 1,
            ..Gpio::default()
        }; // read/write
        gpio.write(0, 0x5);
        gpio.write(2, 0xA);
        assert_eq!(gpio.read(0), 0x5);
        assert_eq!(gpio.read(2), 0xA);
        // Write-only mode reads 0.
        let mut gpio = Gpio::default();
        gpio.write(0, 0x5);
        assert_eq!(gpio.read(0), 0);
    }

    /// One full RTC serial transaction: the command byte, any write bytes, then any read bytes.
    fn rtc_transaction(rtc: &mut Rtc, command: u8, write: &[u8], read_bytes: usize) -> Vec<u8> {
        rtc.gpio_write(0x4); // CS rising begins the transaction
        for bit in (0..8).rev() {
            let sio = command & (1 << bit) != 0;
            rtc.gpio_write(0x4 | (u16::from(sio) << 1));
            rtc.gpio_write(0x5 | (u16::from(sio) << 1)); // rising SCK
            rtc.gpio_write(0x4 | (u16::from(sio) << 1));
        }
        for &byte in write {
            for bit in (0..8).rev() {
                let sio = byte & (1 << bit) != 0;
                rtc.gpio_write(0x4 | (u16::from(sio) << 1));
                rtc.gpio_write(0x5 | (u16::from(sio) << 1));
                rtc.gpio_write(0x4 | (u16::from(sio) << 1));
            }
        }
        let mut out = Vec::new();
        for _ in 0..read_bytes {
            let mut byte = 0u8;
            for bit in (0..8).rev() {
                rtc.gpio_write(0x4);
                let sio = rtc.gpio_write(0x5);
                if sio {
                    byte |= 1 << bit;
                }
            }
            out.push(byte);
        }
        rtc.gpio_write(0x0); // CS low
        out
    }

    #[test]
    fn the_rtc_time_is_read_back_over_the_serial_wires() {
        let mut rtc = Rtc::new();
        rtc.tick(CYCLES_PER_SECOND * 3661); // 01:01:01
        // Read the time register: command $67 = 0x66 | read bit.
        let time = rtc_transaction(&mut rtc, 0x67, &[], 3);
        assert_eq!(time, vec![0x01, 0x01, 0x01], "hour, minute, second in BCD");
        // Read the control register: command $63.
        let control = rtc_transaction(&mut rtc, 0x63, &[], 1);
        assert_eq!(control, vec![0x82]);
    }

    #[test]
    fn the_rtc_datetime_is_written_over_the_serial_wires() {
        let mut rtc = Rtc::new();
        // Write a datetime: command $64 (write), then 7 BCD bytes.
        let bcd = [0x20, 0x12, 0x31, 6, 0x23, 0x59, 0x58];
        rtc_transaction(&mut rtc, 0x64, &bcd, 0);
        let dt = rtc.datetime();
        assert_eq!(dt[0], 0x20, "year");
        assert_eq!(dt[1], 0x12, "month");
        assert_eq!(dt[2], 0x31, "day");
        assert_eq!(dt[4], 0x23, "hour");
        assert_eq!(dt[5], 0x59, "minute");
        assert_eq!(dt[6], 0x58, "second");
        // The day of week is derived from the date: 2020-12-31 was a Thursday.
        assert_eq!(dt[3], 3);
        assert_eq!(
            rtc.base_seconds(),
            (days_from_civil(2020, 12, 31) - days_from_civil(2000, 1, 1)) as u64 * 86_400
                + 23 * 3600
                + 59 * 60
                + 58
        );
    }

    // ---------------------------------------------------------------- the battery body

    #[test]
    fn the_battery_body_round_trips_the_chip_and_the_clock() {
        let mut save = Save::Sram(Sram::new());
        save.write8(0x0E00_0000, 0xAB, 0);
        let body = encode_battery(&save, None).unwrap();
        assert_eq!(body.len(), SRAM_SIZE);
        let mut reloaded = Save::Sram(Sram::new());
        load_battery(&mut reloaded, &mut None, &body).unwrap();
        assert_eq!(reloaded.read8(0x0E00_0000, 0), 0xAB);

        // An RTC cart appends the clock and it comes back.
        let mut rtc = Rtc::new();
        rtc.tick(CYCLES_PER_SECOND * 120);
        let body = encode_battery(&save, Some(&rtc)).unwrap();
        assert_eq!(body.len(), SRAM_SIZE + 14, "chip bytes + the GART footer");
        let mut reloaded = Save::Sram(Sram::new());
        let mut reloaded_rtc = None;
        load_battery(&mut reloaded, &mut reloaded_rtc, &body).unwrap();
        assert_eq!(reloaded.read8(0x0E00_0000, 0), 0xAB);
        assert_eq!(reloaded_rtc.unwrap().base_seconds(), 120);
    }

    #[test]
    fn a_wrong_sized_save_is_padded_or_truncated_without_writing_outside_the_chip() {
        // A short file is padded with 0xFF: the bytes that fit load at the start, the rest is erased.
        let mut save = Save::Sram(Sram::new());
        let loaded =
            load_battery(&mut save, &mut None, &[0x11, 0x22]).expect("padded, not refused");
        assert!(loaded);
        assert_eq!(save.read8(0x0E00_0000, 0), 0x11, "the short prefix loads");
        assert_eq!(save.read8(0x0E00_0001, 0), 0x22);
        assert_eq!(
            save.read8(0x0E00_0002, 0),
            0xFF,
            "the rest is padded with 0xFF"
        );
        assert_eq!(save.read8(0x0E00_7FFF, 0), 0xFF, "up to the chip's edge");

        // A long file is truncated to the chip: only the chip-size prefix loads, and the bytes past
        // it land nowhere.
        let mut save = Save::Sram(Sram::new());
        let mut long = vec![0xAAu8; SRAM_SIZE + 0x40];
        long[SRAM_SIZE..].fill(0xEE);
        let loaded = load_battery(&mut save, &mut None, &long).expect("truncated, not refused");
        assert!(loaded);
        assert_eq!(
            save.body().expect("body"),
            vec![0xAAu8; SRAM_SIZE],
            "the whole chip is the 0xAA prefix; the 0xEE tail wrote nothing"
        );
    }
}
