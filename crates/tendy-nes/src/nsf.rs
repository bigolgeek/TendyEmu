//! NSF (NES Sound Format) playback: the file header, the board the tune's own 6502 code runs on,
//! and the driver stub that board maps in `$4100-$41FF`.
//!
//! An NSF file is a 128-byte header followed by ripped music code and data. It has no reset or NMI
//! vector and nothing at `$8000` that knows how to start itself, so a player has to do four things
//! on the file's behalf: place the data at the load address, clear RAM and the APU the way the
//! format expects, call `INIT` with the song number and the region, and call `PLAY` once per frame.
//! This module supplies all four. [`NsfBoard`] is a [`Mapper`] whose `$8000-$FFFF` window is the
//! file's 4 KiB banks (or its flat image), whose `$6000-$7FFF` window is 8 KiB of work RAM, and
//! whose `$4100-$41FF` page — served through [`Mapper::ext_read`], the `$4020-$5FFF` register
//! window — is a small driver stub. The stub is ordinary 6502 code, so a plain
//! [`Nes::from_nsf`](crate::nes::Nes::from_nsf) console starts making music with no host-side
//! scheduling: reset enters the stub, the stub clears the machine and calls `INIT`, and the NMI
//! handler it installs calls `PLAY` every vblank.
//!
//! Scope:
//!
//! * **2A03 only.** A file whose chip flags ask for VRC6, VRC7, FDS, MMC5, Namco 163, Sunsoft 5B
//!   or VT02+ audio is refused with [`NsfError::UnsupportedChip`] rather than played without the
//!   missing channels.
//! * **Standard-speed files only.** `PLAY` hangs off the PPU's vblank NMI, i.e. it runs at the
//!   region's frame rate (60.0988 Hz NTSC, 50.0070 Hz PAL). A play period further than 1 % from
//!   that frame period needs a timer to call `PLAY`, which this core does not have, so it is
//!   refused with [`NsfError::UnsupportedSpeed`].
//! * **No rendering.** The stub leaves the PPU's rendering disabled (`$2001` untouched, `$2000 =
//!   $80` for the NMI alone), which is what the format assumes: an NSF is sound code, its pattern
//!   and nametable accesses are not meant to be drawn.
//! * **Version 1.** NSF2's extra header semantics are not read.

use crate::cart::{Mapper, Mirroring, Region, chr_read, chr_write, copy_prefix};
use crate::state::{StateError, StateReader, StateWriter};
use std::fmt;

/// Length of the NSF header every file starts with; the music program follows it.
pub const HEADER_LEN: usize = 0x80;
/// The five magic bytes at offset 0 of every NSF file.
pub const MAGIC: [u8; 5] = *b"NESM\x1A";

/// Where the driver stub is mapped: the bus hands `$4020-$5FFF` to [`Mapper::ext_read`], so this
/// page must stay clear of `$4800` (the Namco 163 data port) and `$5FF8-$5FFF` (the bank
/// registers below it).
pub const DRIVER_BASE: u16 = 0x4100;
/// Length of the driver page.
pub const DRIVER_LEN: usize = 0x100;
/// The stub's reset entry, and the value the reset vector (`$FFFC/$FFFD`) answers with.
pub const RESET_ENTRY: u16 = DRIVER_BASE;
/// The stub's NMI entry, and the value the NMI vector (`$FFFA/$FFFB`) answers with: it calls the
/// tune's `PLAY` routine.
pub const NMI_ENTRY: u16 = DRIVER_BASE + 0xF0;
/// The stub's IRQ/BRK entry: a bare `RTI`, so a stray interrupt cannot run the tune twice.
pub const IRQ_ENTRY: u16 = DRIVER_BASE + 0xF4;

/// The NMI handler is a `JSR`/`RTI` pair, so the IRQ entry has to sit clear of it.
const _: () = assert!(IRQ_ENTRY >= NMI_ENTRY + 4);

/// Video timing an NSF declares at `$07A` bits 0-1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NsfRegion {
    /// NTSC only (`INIT` sees X = 0).
    Ntsc,
    /// PAL only (`INIT` sees X = 1).
    Pal,
    /// Both, played as NTSC — the format's own convention for a dual-region file on a player that
    /// has no reason to prefer PAL.
    Dual,
}

impl NsfRegion {
    /// The console video timing this region runs as.
    pub fn region(self) -> Region {
        match self {
            NsfRegion::Pal => Region::Pal,
            NsfRegion::Ntsc | NsfRegion::Dual => Region::Ntsc,
        }
    }

    /// The `X` the driver passes to `INIT`: 0 for NTSC, 1 for PAL.
    pub fn init_flag(self) -> u8 {
        u8::from(self.region() == Region::Pal)
    }
}

/// The 128-byte NSF header, as far as this reader reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NsfHeader {
    /// Format version (`$005`). Only 1 is accepted.
    pub version: u8,
    /// Number of songs in the file (`$006`).
    pub song_count: u8,
    /// The song the file wants played first, one-based (`$007`).
    pub start_song: u8,
    /// Where the music program is placed (`$008`); also gives the bank padding for a bank-switched
    /// file (its low 12 bits).
    pub load_addr: u16,
    /// The tune's `INIT` routine (`$00A`).
    pub init_addr: u16,
    /// The tune's `PLAY` routine (`$00C`); 0 means the file has none, and the NMI handler is a
    /// bare `RTI`.
    pub play_addr: u16,
    /// Requested `PLAY` period in microseconds on NTSC (`$06E`); 0 means "the region's frame rate".
    pub ntsc_speed: u16,
    /// Requested `PLAY` period in microseconds on PAL (`$078`); 0 means "the region's frame rate".
    pub pal_speed: u16,
    /// `$07A` bits 0-1.
    pub region: NsfRegion,
    /// `$07B`: expansion-audio chip flags. Must be 0 for this core.
    pub chip_flags: u8,
    /// `$070-$077`: initial bank number for each 4 KiB window at `$8000-$FFFF` (`$5FF8-$5FFF`).
    /// All zero means the file is not bank-switched.
    pub bank_init: [u8; 8],
}

impl NsfHeader {
    /// Whether the header asks for bank switching: any of the eight initial bank values non-zero.
    pub fn bankswitched(&self) -> bool {
        self.bank_init.iter().any(|&bank| bank != 0)
    }

    /// The play period this header asks for on `region`.
    pub fn speed(&self, region: Region) -> u16 {
        match region {
            Region::Pal => self.pal_speed,
            Region::Ntsc | Region::Dendy => self.ntsc_speed,
        }
    }
}

/// Why an NSF file could not be loaded or started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NsfError {
    /// Fewer than [`HEADER_LEN`] bytes.
    TooShort,
    /// The file does not start with [`MAGIC`].
    BadMagic,
    /// Not the format version this reader reads (1).
    UnsupportedVersion(u8),
    /// The chip flags ask for expansion audio, which this core does not emulate.
    UnsupportedChip(u8),
    /// A play period that is not the region's frame rate; honouring it would need a timer.
    UnsupportedSpeed(u16),
    /// A request for a song the file does not have.
    BadSong(u8),
    /// A load address below `$6000`, which the board cannot map.
    UnsupportedLoad(u16),
    /// The header declares more program bytes than the file holds.
    Truncated,
}

impl fmt::Display for NsfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NsfError::TooShort => {
                write!(f, "not an NSF file: fewer than {HEADER_LEN} header bytes")
            }
            NsfError::BadMagic => write!(f, "not an NSF file: the NESM signature is missing"),
            NsfError::UnsupportedVersion(version) => {
                write!(f, "NSF version {version} is not supported (version 1 only)")
            }
            NsfError::UnsupportedChip(flags) => write!(
                f,
                "expansion audio (chip flags ${flags:02X}) is not supported: this slice is 2A03 only"
            ),
            NsfError::UnsupportedSpeed(period) => write!(
                f,
                "play period {period} us is not the region's frame rate; only standard-speed NSF files are supported"
            ),
            NsfError::BadSong(song) => write!(f, "song {song} is outside this file's songs"),
            NsfError::UnsupportedLoad(addr) => write!(
                f,
                "load address ${addr:04X} is below $6000, which the board cannot map"
            ),
            NsfError::Truncated => {
                write!(
                    f,
                    "the program data ends before the length the header declares"
                )
            }
        }
    }
}

impl std::error::Error for NsfError {}

/// Two-byte little-endian field at `at`; the caller has already checked the length.
fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// Whether a requested play period is the region's own frame rate, which the vblank-driven `PLAY`
/// call already provides.
///
/// The standard NTSC period is 16639 us and the standard PAL one 19997 us, but rips in the wild
/// write 16666 and 20000 often enough that a 1 % band is the honest test: within it, the drift is
/// a fraction of a frame per minute and no tune can hear it. A zero field means "the default".
fn standard_speed(period: u16, region: Region) -> bool {
    if period == 0 {
        return true;
    }
    let frame_us = match region {
        Region::Pal => 19_997,
        Region::Ntsc | Region::Dendy => 16_639,
    };
    period.abs_diff(frame_us) <= frame_us / 100
}

/// Parse and validate the 128-byte header.
///
/// Rejects: a short file, a bad signature, a version other than 1, any expansion-audio chip flag,
/// and a play period that is not the region's frame rate. It does not look at the program data;
/// [`Nsf::parse`] does that.
pub fn parse_header(bytes: &[u8]) -> Result<NsfHeader, NsfError> {
    if bytes.len() < HEADER_LEN {
        return Err(NsfError::TooShort);
    }
    if bytes[0..5] != MAGIC {
        return Err(NsfError::BadMagic);
    }
    let version = bytes[5];
    if version != 1 {
        return Err(NsfError::UnsupportedVersion(version));
    }
    if bytes[6] == 0 {
        return Err(NsfError::BadSong(0));
    }
    let chip_flags = bytes[0x7B];
    if chip_flags != 0 {
        return Err(NsfError::UnsupportedChip(chip_flags));
    }
    let region_bits = bytes[0x7A];
    let region = if region_bits & 0x02 != 0 {
        NsfRegion::Dual
    } else if region_bits & 0x01 != 0 {
        NsfRegion::Pal
    } else {
        NsfRegion::Ntsc
    };
    let mut bank_init = [0u8; 8];
    bank_init.copy_from_slice(&bytes[0x70..0x78]);
    let header = NsfHeader {
        version,
        song_count: bytes[6],
        start_song: bytes[7],
        load_addr: word(bytes, 0x08),
        init_addr: word(bytes, 0x0A),
        play_addr: word(bytes, 0x0C),
        ntsc_speed: word(bytes, 0x6E),
        pal_speed: word(bytes, 0x78),
        region,
        chip_flags,
        bank_init,
    };
    if !standard_speed(header.speed(region.region()), region.region()) {
        return Err(NsfError::UnsupportedSpeed(header.speed(region.region())));
    }
    Ok(header)
}

/// What the player screen shows about a tune: the header's three 32-byte text fields and its song
/// numbers. Read straight from the bytes, so it needs no board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NsfInfo {
    pub title: String,
    pub artist: String,
    pub copyright: String,
    pub song_count: u8,
    /// One-based starting song, at least 1.
    pub start_song: u8,
}

/// A NUL-padded header text field as a string (stopping at the first NUL).
fn text_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

impl NsfInfo {
    /// Parse the whole file as [`Nsf::parse`] does (so a refused file is refused here, with the
    /// same error) and read its text fields.
    pub fn read(bytes: &[u8]) -> Result<NsfInfo, NsfError> {
        let nsf = Nsf::parse(bytes)?;
        Ok(NsfInfo {
            title: text_field(&bytes[0x0E..0x2E]),
            artist: text_field(&bytes[0x2E..0x4E]),
            copyright: text_field(&bytes[0x4E..0x6E]),
            song_count: nsf.song_count(),
            start_song: nsf.header.start_song.max(1),
        })
    }

    /// The one-based song a request selects: `None` and `Some(0)` are the file's starting song
    /// (the same rule as `Nes::from_nsf`).
    /// A request past the song count is [`NsfError::BadSong`].
    pub fn pick_song(&self, requested: Option<u8>) -> Result<u8, NsfError> {
        let song = match requested {
            None | Some(0) => self.start_song,
            Some(song) => song,
        };
        if song > self.song_count {
            return Err(NsfError::BadSong(song));
        }
        Ok(song)
    }

    /// The song after `song` in `step` direction (+1/-1), clamped to `1..=song_count`.
    pub fn step_song(&self, song: u8, step: i8) -> u8 {
        song.saturating_add_signed(step)
            .clamp(1, self.song_count.max(1))
    }

    /// The lines of the plain title screen for `song`.
    pub fn screen_lines(&self, song: u8) -> [String; 4] {
        [
            self.title.clone(),
            self.artist.clone(),
            self.copyright.clone(),
            format!("song {song} of {}", self.song_count),
        ]
    }
}

/// A parsed NSF file: its header and the music program that follows it.
#[derive(Debug)]
pub struct Nsf {
    pub header: NsfHeader,
    /// The program bytes: the file after the 128-byte header, cut to the length the header declares
    /// when `$07D-$07F` is non-zero (NSF2's backward-compatible length field).
    pub program: Vec<u8>,
}

impl Nsf {
    /// Parse a whole NSF image.
    ///
    /// Besides [`parse_header`]'s checks, a flat (non-bank-switched) file whose load address is
    /// below `$6000` is refused: the board only decodes `$6000-$FFFF`, so the data would be
    /// unreachable. A load address of `$6000-$7FFF` is accepted and mapped into the board's work
    /// RAM window, which the driver's RAM clear leaves alone (the board write-protects the loaded
    /// image, so the program survives the clear that the format asks for before `INIT`).
    pub fn parse(bytes: &[u8]) -> Result<Nsf, NsfError> {
        let header = parse_header(bytes)?;
        let declared =
            u32::from(bytes[0x7D]) | u32::from(bytes[0x7E]) << 8 | u32::from(bytes[0x7F]) << 16;
        let available = (bytes.len() - HEADER_LEN) as u32;
        let len = if declared == 0 { available } else { declared };
        if len > available {
            return Err(NsfError::Truncated);
        }
        let program = bytes[HEADER_LEN..HEADER_LEN + len as usize].to_vec();
        if !header.bankswitched() && header.load_addr < 0x6000 {
            return Err(NsfError::UnsupportedLoad(header.load_addr));
        }
        Ok(Nsf { header, program })
    }

    /// The console video timing the file runs as.
    pub fn region(&self) -> Region {
        self.header.region.region()
    }

    /// Number of songs the file holds.
    pub fn song_count(&self) -> u8 {
        self.header.song_count
    }

    /// The zero-based song index for a one-based `song`, as the driver passes it to `INIT`.
    ///
    /// `song` 0 selects the file's own starting song (with 0 meaning the first), which is what a
    /// caller who does not know the file wants to hear should pass.
    pub fn song_index(&self, song: u8) -> Result<u8, NsfError> {
        let one_based = if song == 0 {
            self.header.start_song.max(1)
        } else {
            song
        };
        if self.song_count() == 0 || one_based > self.song_count() {
            return Err(NsfError::BadSong(song));
        }
        Ok(one_based - 1)
    }

    /// Build the board for `song` (one-based; 0 selects the file's starting song).
    pub fn board(&self, song: u8) -> Result<NsfBoard, NsfError> {
        Ok(NsfBoard::new(
            &self.header,
            &self.program,
            self.song_index(song)?,
        ))
    }
}

/// The cartridge the tune runs on: the file's program at `$8000-$FFFF` (bank-switched in 4 KiB
/// windows when the header asks for it), 8 KiB of work RAM at `$6000-$7FFF`, 8 KiB of CHR-RAM, and
/// the [`DRIVER_BASE`] stub the reset and NMI vectors point into.
pub struct NsfBoard {
    /// Flat program image for a non-bank-switched file.
    program: Vec<u8>,
    /// `load_addr` of that flat image.
    load_addr: u16,
    /// Padding followed by the program, in 4 KiB banks, for a bank-switched file. Empty otherwise.
    bank_pool: Vec<u8>,
    /// Whether the header asked for bank switching; when false `bank_pool` is unused.
    bankswitched: bool,
    /// `$5FF8-$5FFF`: the bank mapped into each 4 KiB window of `$8000-$FFFF`.
    bank_select: [u8; 8],
    /// `$6000-$7FFF` work RAM.
    prg_ram: Vec<u8>,
    /// 8 KiB of pattern memory. An NSF never renders, but the PPU's `$2007` port and the debug
    /// views still reach it, and the tune's own code is free to poke it.
    chr_ram: Vec<u8>,
    /// The driver page at [`DRIVER_BASE`].
    driver: [u8; DRIVER_LEN],
}

impl NsfBoard {
    /// Build the board for `song`, the zero-based number the driver loads into A before `INIT`.
    ///
    /// A bank-switched file starts with the header's own bank assignment at `$5FF8-$5FFF`; the
    /// driver writes the same eight values again before `INIT`, as the format's pre-INIT sequence
    /// asks.
    pub fn new(header: &NsfHeader, program: &[u8], song: u8) -> NsfBoard {
        let bankswitched = header.bankswitched();
        let bank_pool = if bankswitched {
            // The low twelve bits of the load address are the padding in front of bank 0.
            let mut pool = vec![0xFF; usize::from(header.load_addr & 0x0FFF)];
            pool.extend_from_slice(program);
            pool
        } else {
            Vec::new()
        };
        NsfBoard {
            program: if bankswitched {
                Vec::new()
            } else {
                program.to_vec()
            },
            load_addr: header.load_addr,
            bank_pool,
            bankswitched,
            bank_select: header.bank_init,
            prg_ram: vec![0; 0x2000],
            chr_ram: vec![0; 0x2000],
            driver: build_driver(header, song),
        }
    }

    /// The driver page, for a frontend's debugger or a test that wants to disassemble it.
    pub fn driver(&self) -> &[u8] {
        &self.driver
    }

    /// The eight `$5FF8-$5FFF` bank registers.
    pub fn bank_select(&self) -> [u8; 8] {
        self.bank_select
    }

    /// Whether an address inside `$6000-$7FFF` holds the loaded program image (a flat file whose
    /// load address is below `$8000`) rather than board RAM.
    fn in_program(&self, addr: u16) -> bool {
        if self.bankswitched || self.load_addr >= 0x8000 {
            return false;
        }
        let offset = usize::from(addr) as i32 - i32::from(self.load_addr);
        offset >= 0 && (offset as usize) < self.program.len()
    }

    /// A byte of the program window at `$8000-$FFFF`: banked or flat.
    fn window_read(&self, addr: u16) -> u8 {
        if self.bankswitched {
            let window = usize::from((addr - 0x8000) >> 12);
            let offset = usize::from(addr & 0x0FFF);
            let bank = usize::from(self.bank_select[window]);
            self.bank_pool
                .get(bank * 0x1000 + offset)
                .copied()
                .unwrap_or(0xFF)
        } else {
            let offset = usize::from(addr) as i32 - i32::from(self.load_addr);
            if offset < 0 {
                return 0xFF;
            }
            self.program.get(offset as usize).copied().unwrap_or(0xFF)
        }
    }

    /// One of the three vectors the board answers, as the byte at `addr`.
    fn vector(&self, addr: u16) -> u8 {
        let entry = match addr {
            0xFFFA | 0xFFFB => NMI_ENTRY,
            0xFFFC | 0xFFFD => RESET_ENTRY,
            _ => IRQ_ENTRY,
        };
        if addr & 1 == 0 {
            entry as u8
        } else {
            (entry >> 8) as u8
        }
    }
}

impl Mapper for NsfBoard {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            // The driver page, also reachable through `ext_read` from the bus.
            DRIVER_BASE..=0x41FF => self.driver[usize::from(addr - DRIVER_BASE)],
            // The board owns the vectors: an NSF image has no ROM at $FFFA-$FFFF to take them from.
            0xFFFA..=0xFFFF => self.vector(addr),
            // The flat program image may sit in the work-RAM window; it wins over RAM there.
            0x6000..=0x7FFF => {
                if self.in_program(addr) {
                    self.program[usize::from(addr - self.load_addr)]
                } else {
                    self.prg_ram[usize::from(addr - 0x6000)]
                }
            }
            0x8000..=0xFFFF => self.window_read(addr),
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if self.ext_write(addr, val) {
            return;
        }
        if (0x6000..=0x7FFF).contains(&addr) && !self.in_program(addr) {
            self.prg_ram[usize::from(addr - 0x6000)] = val;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        chr_read(&self.chr_ram, usize::from(addr & 0x1FFF))
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        chr_write(&mut self.chr_ram, usize::from(addr & 0x1FFF), val);
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr_ram, usize::from(addr & 0x1FFF))
    }

    /// An NSF has no nametables of its own and never renders, so any fixed arrangement will do.
    fn mirroring(&self) -> Mirroring {
        Mirroring::Horizontal
    }

    fn prg_ram(&self) -> &[u8] {
        &self.prg_ram
    }

    fn load_prg_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.prg_ram, data);
    }

    fn chr_ram(&self) -> &[u8] {
        &self.chr_ram
    }

    fn load_chr_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.chr_ram, data);
    }

    /// The driver page and the bank registers, both `$4020-$5FFF` addresses the bus routes here.
    fn ext_read(&mut self, addr: u16) -> Option<u8> {
        if (DRIVER_BASE..DRIVER_BASE + DRIVER_LEN as u16).contains(&addr) {
            Some(self.driver[usize::from(addr - DRIVER_BASE)])
        } else {
            None
        }
    }

    fn ext_write(&mut self, addr: u16, val: u8) -> bool {
        if (0x5FF8..=0x5FFF).contains(&addr) {
            self.bank_select[usize::from(addr - 0x5FF8)] = val;
            return true;
        }
        false
    }

    /// Board state only: work RAM, the bank registers and CHR-RAM. The program and the driver are
    /// ROM, and the driver has the song number baked in, so neither is saved.
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        w.bytes(&self.bank_select);
        w.bytes(&self.chr_ram);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.prg_ram)?;
        r.bytes_into(&mut self.bank_select)?;
        r.bytes_into(&mut self.chr_ram)?;
        Ok(())
    }
}

/// A two-pass-free 6502 emitter for the driver page: each helper appends one instruction, and
/// [`Stub::branch`] resolves a backward relative jump against the current length.
struct Stub {
    code: Vec<u8>,
}

impl Stub {
    fn new() -> Stub {
        Stub { code: Vec::new() }
    }

    fn len(&self) -> usize {
        self.code.len()
    }

    fn byte(&mut self, byte: u8) {
        self.code.push(byte);
    }

    fn word_op(&mut self, opcode: u8, addr: u16) {
        self.code
            .extend_from_slice(&[opcode, addr as u8, (addr >> 8) as u8]);
    }

    fn lda_imm(&mut self, value: u8) {
        self.code.extend_from_slice(&[0xA9, value]);
    }

    fn ldx_imm(&mut self, value: u8) {
        self.code.extend_from_slice(&[0xA2, value]);
    }

    fn sta_abs(&mut self, addr: u16) {
        self.word_op(0x8D, addr);
    }

    fn sta_abs_x(&mut self, addr: u16) {
        self.word_op(0x9D, addr);
    }

    fn jsr(&mut self, addr: u16) {
        self.word_op(0x20, addr);
    }

    fn jmp(&mut self, addr: u16) {
        self.word_op(0x4C, addr);
    }

    /// A relative branch to `target`, an offset from the start of the page.
    fn branch(&mut self, opcode: u8, target: usize) {
        self.code.push(opcode);
        let next = self.code.len() + 1;
        let offset = target as isize - next as isize;
        debug_assert!((-128..=127).contains(&offset), "branch out of range");
        self.code.push(offset as u8);
    }
}

/// Build the driver page for `header` with the zero-based `song` in A before `INIT`.
///
/// The reset entry follows the format's pre-`INIT` sequence (NESdev wiki, "Initializing a tune"):
/// clear `$0000-$07FF` and `$6000-$7FFF` (the board write-protects a tune loaded below `$8000`),
/// zero `$4000-$4013`, write `$4015 = $00` then `$0F`, `$4017 = $40` (4-step, frame IRQ off),
/// load the header's bank assignment into `$5FF8-$5FFF` for a bank-switched file, then `INIT` with
/// A = song and X = region. It finishes by writing `$2000 = $80`: the vblank NMI on, rendering
/// off, whatever `INIT` did to the register. The NMI entry calls `PLAY` and returns; a file with
/// no `PLAY` (`play_addr == 0`) gets a bare `RTI`.
fn build_driver(header: &NsfHeader, song: u8) -> [u8; DRIVER_LEN] {
    let mut s = Stub::new();
    s.byte(0x78); // SEI
    s.byte(0xD8); // CLD
    s.ldx_imm(0xFF);
    s.byte(0x9A); // TXS
    s.lda_imm(0x00);
    s.ldx_imm(0x00);
    // Eight STA $page00,X cover the whole 2 KB of work RAM, stack page included.
    let clear = s.len();
    for page in 0..8u16 {
        s.sta_abs_x(page << 8);
    }
    s.byte(0xE8); // INX
    s.branch(0xD0, clear); // BNE clear
    // APU: silence $4000-$4013, then enable the four 2A03 channels with the frame counter's IRQ off.
    s.lda_imm(0x00);
    s.ldx_imm(0x13);
    let apu = s.len();
    s.sta_abs_x(0x4000);
    s.byte(0xCA); // DEX
    s.branch(0x10, apu); // BPL apu
    s.sta_abs(0x4015); // A is still 0
    s.lda_imm(0x0F);
    s.sta_abs(0x4015);
    s.lda_imm(0x40);
    s.sta_abs(0x4017);
    if header.bankswitched() {
        for (window, &bank) in header.bank_init.iter().enumerate() {
            s.lda_imm(bank);
            s.sta_abs(0x5FF8 + window as u16);
        }
    }
    s.lda_imm(song);
    s.ldx_imm(header.region.init_flag());
    s.jsr(header.init_addr);
    s.lda_imm(0x80);
    s.sta_abs(0x2000);
    let idle = DRIVER_BASE + s.len() as u16;
    s.jmp(idle);
    assert!(
        s.len() <= usize::from(NMI_ENTRY - DRIVER_BASE),
        "driver reset code overruns the NMI entry"
    );

    let mut page = [0u8; DRIVER_LEN];
    page[..s.len()].copy_from_slice(&s.code);
    let nmi = usize::from(NMI_ENTRY - DRIVER_BASE);
    if header.play_addr != 0 {
        page[nmi] = 0x20; // JSR
        page[nmi + 1] = header.play_addr as u8;
        page[nmi + 2] = (header.play_addr >> 8) as u8;
    }
    page[nmi + 3] = 0x40; // RTI
    page[usize::from(IRQ_ENTRY - DRIVER_BASE)] = 0x40; // RTI
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An NSF header with the given addresses and `program` after it.
    fn nsf(load: u16, init: u16, play: u16, program: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.push(1); // version
        v.push(1); // songs
        v.push(1); // starting song
        v.extend_from_slice(&load.to_le_bytes());
        v.extend_from_slice(&init.to_le_bytes());
        v.extend_from_slice(&play.to_le_bytes());
        v.resize(0x6E, 0);
        v.extend_from_slice(&16_639u16.to_le_bytes());
        v.resize(0x78, 0);
        v.extend_from_slice(&19_997u16.to_le_bytes());
        v.push(0); // NTSC
        v.push(0); // 2A03 only
        v.resize(HEADER_LEN, 0);
        v.extend_from_slice(program);
        v
    }

    #[test]
    fn header_fields_are_read() {
        let mut bytes = nsf(0x8000, 0x8123, 0x8456, &[0u8; 16]);
        bytes[6] = 3;
        bytes[7] = 2;
        bytes[0x70] = 1;
        bytes[0x77] = 4;
        let h = parse_header(&bytes).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!((h.song_count, h.start_song), (3, 2));
        assert_eq!(
            (h.load_addr, h.init_addr, h.play_addr),
            (0x8000, 0x8123, 0x8456)
        );
        assert_eq!((h.ntsc_speed, h.pal_speed), (16_639, 19_997));
        assert_eq!(h.region, NsfRegion::Ntsc);
        assert!(h.bankswitched());
        assert_eq!(h.bank_init, [1, 0, 0, 0, 0, 0, 0, 4]);
    }

    #[test]
    fn region_flags_map_to_timing_and_init_flag() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        assert_eq!(parse_header(&bytes).unwrap().region, NsfRegion::Ntsc);
        bytes[0x7A] = 1;
        let pal = parse_header(&bytes).unwrap();
        assert_eq!(pal.region, NsfRegion::Pal);
        assert_eq!(pal.region.region(), Region::Pal);
        assert_eq!(pal.region.init_flag(), 1);
        bytes[0x7A] = 2;
        let dual = parse_header(&bytes).unwrap();
        assert_eq!(dual.region, NsfRegion::Dual);
        assert_eq!(dual.region.region(), Region::Ntsc);
        assert_eq!(dual.region.init_flag(), 0);
        bytes[0x7A] = 3;
        assert_eq!(parse_header(&bytes).unwrap().region, NsfRegion::Dual);
    }

    #[test]
    fn bad_magic_is_an_error() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        bytes[0] = b'X';
        assert_eq!(parse_header(&bytes), Err(NsfError::BadMagic));
        assert_eq!(Nsf::parse(&bytes).unwrap_err(), NsfError::BadMagic);
    }

    #[test]
    fn short_file_is_an_error() {
        assert_eq!(parse_header(&[0u8; 0x7F]), Err(NsfError::TooShort));
    }

    #[test]
    fn version_2_is_unsupported() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        bytes[5] = 2;
        assert_eq!(parse_header(&bytes), Err(NsfError::UnsupportedVersion(2)));
    }

    #[test]
    fn expansion_chips_are_unsupported() {
        for flags in [1u8, 0x02, 0x20, 0x80] {
            let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
            bytes[0x7B] = flags;
            assert_eq!(parse_header(&bytes), Err(NsfError::UnsupportedChip(flags)));
        }
    }

    #[test]
    fn non_standard_play_period_is_unsupported() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        bytes[0x6E..0x70].copy_from_slice(&10_000u16.to_le_bytes());
        assert_eq!(
            parse_header(&bytes),
            Err(NsfError::UnsupportedSpeed(10_000))
        );
        bytes[0x6E..0x70].copy_from_slice(&0u16.to_le_bytes());
        assert!(parse_header(&bytes).is_ok(), "0 means the region default");
    }

    #[test]
    fn pal_file_checks_the_pal_period() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        bytes[0x7A] = 1;
        bytes[0x6E..0x70].copy_from_slice(&10_000u16.to_le_bytes());
        assert!(
            parse_header(&bytes).is_ok(),
            "the NTSC field is unused on a PAL file"
        );
        bytes[0x78..0x7A].copy_from_slice(&10_000u16.to_le_bytes());
        assert_eq!(
            parse_header(&bytes),
            Err(NsfError::UnsupportedSpeed(10_000))
        );
    }

    #[test]
    fn play_period_within_one_percent_is_standard() {
        for speed in [16_639u16, 16_666, 16_600, 16_700] {
            let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
            bytes[0x6E..0x70].copy_from_slice(&speed.to_le_bytes());
            assert!(
                parse_header(&bytes).is_ok(),
                "{speed} us is the NTSC frame rate"
            );
        }
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[]);
        bytes[0x7A] = 1;
        bytes[0x78..0x7A].copy_from_slice(&20_000u16.to_le_bytes());
        assert!(
            parse_header(&bytes).is_ok(),
            "20000 us is within 1 % of the PAL frame rate"
        );
    }

    #[test]
    fn song_index_is_zero_based_and_bounds_checked() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0x60]);
        bytes[6] = 3;
        bytes[7] = 2;
        let nsf = Nsf::parse(&bytes).unwrap();
        assert_eq!(nsf.song_index(1), Ok(0));
        assert_eq!(nsf.song_index(3), Ok(2));
        assert_eq!(
            nsf.song_index(0),
            Ok(1),
            "0 selects the file's starting song"
        );
        assert_eq!(nsf.song_index(4), Err(NsfError::BadSong(4)));
    }

    #[test]
    fn zero_song_count_is_refused() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0x60]);
        bytes[6] = 0;
        assert_eq!(Nsf::parse(&bytes).unwrap_err(), NsfError::BadSong(0));
        assert_eq!(NsfInfo::read(&bytes).unwrap_err(), NsfError::BadSong(0));
    }

    #[test]
    fn program_length_field_is_respected() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0xAA, 0xBB, 0xCC, 0xDD]);
        bytes[0x7D] = 2;
        assert_eq!(Nsf::parse(&bytes).unwrap().program, vec![0xAA, 0xBB]);
        bytes[0x7D] = 5;
        assert_eq!(Nsf::parse(&bytes).unwrap_err(), NsfError::Truncated);
    }

    #[test]
    fn flat_load_below_6000_is_unsupported() {
        let bytes = nsf(0x4000, 0x4000, 0x4000, &[0x60]);
        assert_eq!(
            Nsf::parse(&bytes).unwrap_err(),
            NsfError::UnsupportedLoad(0x4000)
        );
    }

    #[test]
    fn reset_vector_points_at_the_driver() {
        let bytes = nsf(0x8000, 0x8000, 0x8000, &[0u8; 0x2000]);
        let header = parse_header(&bytes).unwrap();
        let mut board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        assert_eq!(board.cpu_read(0xFFFC), RESET_ENTRY as u8);
        assert_eq!(board.cpu_read(0xFFFD), (RESET_ENTRY >> 8) as u8);
        assert_eq!(board.cpu_read(0xFFFA), NMI_ENTRY as u8);
        assert_eq!(board.cpu_read(0xFFFB), (NMI_ENTRY >> 8) as u8);
        assert_eq!(board.cpu_read(0xFFFE), IRQ_ENTRY as u8);
        assert_eq!(
            board.ext_read(RESET_ENTRY),
            Some(0x78),
            "the driver starts with SEI"
        );
        assert_eq!(
            board.cpu_read(RESET_ENTRY),
            0x78,
            "and is readable as a fetch would see it"
        );
    }

    #[test]
    fn driver_ends_with_the_init_call_and_song_number() {
        let bytes = nsf(0x8000, 0x8123, 0x8456, &[0u8; 0x2000]);
        let header = parse_header(&bytes).unwrap();
        let board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 6);
        let page = board.driver();
        let lda = page
            .windows(2)
            .rposition(|w| w[0] == 0xA9 && w[1] == 6)
            .expect("LDA #song");
        assert_eq!(
            &page[lda + 2..lda + 7],
            &[0xA2, 0x00, 0x20, 0x23, 0x81],
            "LDX #$00, JSR $8123"
        );
        let nmi = usize::from(NMI_ENTRY - DRIVER_BASE);
        assert_eq!(
            &page[nmi..nmi + 4],
            &[0x20, 0x56, 0x84, 0x40],
            "JSR PLAY / RTI"
        );
        assert_eq!(
            page[usize::from(IRQ_ENTRY - DRIVER_BASE)],
            0x40,
            "IRQ is a bare RTI"
        );
    }

    #[test]
    fn bank_registers_map_four_kib_windows() {
        let mut bytes = nsf(0x8000, 0x8000, 0x0000, &[]);
        bytes[0x70] = 1; // bank-switched: bank 1 first at $8000
        let mut program = vec![0x11u8; 0x1000];
        program.extend(vec![0x22u8; 0x1000]);
        bytes.extend_from_slice(&program);
        let header = parse_header(&bytes).unwrap();
        let mut board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        assert_eq!(board.cpu_read(0x8000), 0x22, "header bank 1 at $8000");
        assert_eq!(board.cpu_read(0x8FFF), 0x22);
        assert_eq!(
            board.cpu_read(0x9000),
            0x11,
            "the next window is still bank 0"
        );
        board.cpu_write(0x5FF8, 0);
        assert_eq!(board.cpu_read(0x8000), 0x11, "$5FF8 names bank 0");
        board.cpu_write(0x5FFB, 1);
        assert_eq!(board.cpu_read(0xB000), 0x22, "$5FFB is the fourth window");
        assert_eq!(
            board.cpu_read(0xA000),
            0x11,
            "the third window is unchanged"
        );
        assert_eq!(board.bank_select(), [0, 0, 0, 1, 0, 0, 0, 0]);
        assert!(board.ext_write(0x5FFE, 1));
        assert_eq!(board.cpu_read(0xE000), 0x22);
        assert_eq!(
            board.ext_read(0x4800),
            None,
            "the driver page does not leak into other ports"
        );
    }

    #[test]
    fn bank_pool_pads_by_the_load_address_low_bits() {
        let mut bytes = nsf(0x8100, 0x8100, 0x0000, &[]);
        bytes[0x70] = 2; // bank-switched
        bytes.extend_from_slice(&[0xAB; 0x1000]);
        let header = parse_header(&bytes).unwrap();
        // Window 1 holds bank 0, which begins 0x100 bytes into the pool; window 2 holds bank 2
        // (outside the pool) and reads as erased ROM.
        let mut board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        assert_eq!(
            board.cpu_read(0x8000),
            0xFF,
            "bank 2 is past the data: erased ROM"
        );
        board.cpu_write(0x5FF8, 0);
        assert_eq!(
            board.cpu_read(0x8000),
            0xFF,
            "bank 0 starts with 0x100 bytes of padding"
        );
        assert_eq!(board.cpu_read(0x8100), 0xAB);
        assert_eq!(board.cpu_read(0x8FFF), 0xAB);
        assert_eq!(
            board.cpu_read(0x9000),
            0xFF,
            "bank 0's second window starts in the padding again"
        );
    }

    #[test]
    fn flat_program_in_work_ram_survives_a_ram_clear() {
        let bytes = nsf(0x6000, 0x6000, 0x0000, &[0xA9, 0x01, 0x60]);
        let header = parse_header(&bytes).unwrap();
        let mut board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        assert_eq!(board.cpu_read(0x6000), 0xA9);
        for addr in 0x6000..=0x7FFFu16 {
            board.cpu_write(addr, 0);
        }
        assert_eq!(
            board.cpu_read(0x6000),
            0xA9,
            "the loaded program is write-protected"
        );
        assert_eq!(board.cpu_read(0x6003), 0, "the rest of the window is RAM");
        board.cpu_write(0x6003, 0x5A);
        assert_eq!(board.cpu_read(0x6003), 0x5A);
    }

    #[test]
    fn save_state_round_trips_ram_and_banks() {
        let mut bytes = nsf(0x8000, 0x8000, 0x0000, &[]);
        bytes[0x70] = 1;
        bytes.extend_from_slice(&[0x11; 0x2000]);
        let header = parse_header(&bytes).unwrap();
        let mut board = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        board.cpu_write(0x7FFF, 0x77);
        board.cpu_write(0x5FF8, 0);
        board.ppu_write(0x0123, 0x99);
        let saved = {
            let mut w = StateWriter::new();
            board.save_state(&mut w);
            w.finish()
        };
        let mut fresh = NsfBoard::new(&header, &bytes[HEADER_LEN..], 0);
        fresh.cpu_write(0x7FFF, 0);
        fresh.cpu_write(0x5FF8, 1);
        fresh.ppu_write(0x0123, 0);
        let mut r = StateReader::new(&saved);
        fresh.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(fresh.cpu_read(0x7FFF), 0x77);
        assert_eq!(fresh.bank_select()[0], 0);
        assert_eq!(fresh.chr_peek(0x0123), 0x99);
    }

    #[test]
    fn info_reads_the_text_fields_and_picks_songs() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0x60]);
        bytes[6] = 3;
        bytes[7] = 2;
        bytes[0x0E..0x0E + 5].copy_from_slice(b"Title");
        bytes[0x2E..0x2E + 6].copy_from_slice(b"Artist");
        bytes[0x4E..0x4E + 4].copy_from_slice(b"(c) ");
        let info = NsfInfo::read(&bytes).unwrap();
        assert_eq!(
            (
                info.title.as_str(),
                info.artist.as_str(),
                info.copyright.as_str()
            ),
            ("Title", "Artist", "(c)")
        );
        assert_eq!(info.screen_lines(2)[3], "song 2 of 3");
        assert_eq!(info.pick_song(None), Ok(2));
        assert_eq!(info.pick_song(Some(3)), Ok(3));
        assert_eq!(info.pick_song(Some(4)), Err(NsfError::BadSong(4)));
        assert_eq!(
            (
                info.step_song(1, -1),
                info.step_song(3, 1),
                info.step_song(2, 1)
            ),
            (1, 3, 3)
        );
    }

    #[test]
    fn pick_song_treats_zero_as_the_files_start_song() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0x60]);
        bytes[6] = 5;
        bytes[7] = 4;
        let info = NsfInfo::read(&bytes).unwrap();
        assert_eq!(info.pick_song(Some(0)), Ok(4));
        assert_eq!(info.pick_song(None), Ok(4));
        assert_eq!(info.pick_song(Some(5)), Ok(5));
        assert_eq!(info.pick_song(Some(6)), Err(NsfError::BadSong(6)));
    }

    #[test]
    fn info_refuses_what_parse_refuses() {
        let mut bytes = nsf(0x8000, 0x8000, 0x8000, &[0x60]);
        bytes[0x7B] = 0x01;
        assert_eq!(NsfInfo::read(&bytes), Err(NsfError::UnsupportedChip(1)));
    }
}
