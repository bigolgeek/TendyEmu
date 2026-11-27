#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mirroring {
    #[default]
    Horizontal,
    Vertical,
    SingleScreenLower,
    SingleScreenUpper,
    FourScreen,
}

impl Mirroring {
    /// Save-state tag, Vertical = 0, Horizontal = 1: the numbering VRC2/4, VRC6, VRC7, FME-7 and
    /// Namco 163 have written since their states shipped. Every one of them carried its own copy of
    /// this match; the numbers are now fixed here so they cannot drift apart.
    pub fn to_tag(self) -> u8 {
        match self {
            Mirroring::Vertical => 0,
            Mirroring::Horizontal => 1,
            Mirroring::SingleScreenLower => 2,
            Mirroring::SingleScreenUpper => 3,
            Mirroring::FourScreen => 4,
        }
    }

    pub fn from_tag(tag: u8) -> Result<Mirroring, crate::state::StateError> {
        Ok(match tag {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleScreenLower,
            3 => Mirroring::SingleScreenUpper,
            4 => Mirroring::FourScreen,
            _ => return Err(crate::state::StateError::BadValue),
        })
    }

    /// MMC3's own save-state numbering: Horizontal = 0, Vertical = 1 — the other way round from
    /// [`Mirroring::to_tag`], and equally fixed, because changing it would load an existing state
    /// with the two nametables swapped.
    pub fn to_tag_horizontal_first(self) -> u8 {
        match self {
            Mirroring::Horizontal => 0,
            Mirroring::Vertical => 1,
            other => other.to_tag(),
        }
    }

    pub fn from_tag_horizontal_first(tag: u8) -> Result<Mirroring, crate::state::StateError> {
        Ok(match tag {
            0 => Mirroring::Horizontal,
            1 => Mirroring::Vertical,
            other => Mirroring::from_tag(other)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CartError {
    TooShort,
    BadMagic,
    Truncated,
    UnsupportedMapper(u16),
    /// NES 2.0 exponent-multiplier size too large to represent in a `usize` (2^E * (2*MM+1) overflows).
    UnsupportedSize,
    /// A cartridge image with no program ROM at all: every board's bank arithmetic divides by the
    /// PRG length, so there is nothing a board could map.
    NoPrg,
    /// A board that switches 32 KB at a time (mappers 7, 11, 34, 38, 66, 79, 140, 185) with less than
    /// 32 KB of program ROM: its bank count, `prg.len() / 0x8000`, is zero.
    PrgTooSmall,
    /// A UNIF image named a board this emulator has no mapper for; the string is its `MAPR` name.
    UnknownBoard(String),
    /// A UNIF image's `MIRR` chunk held a byte the format does not define: 0-4 name a fixed
    /// arrangement and 5 means "the board controls it". The byte is carried so the load error names
    /// it instead of calling the whole file a bad magic.
    BadMirroring(u8),
}

/// Console a ROM targets (header byte 7 bits 0-1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConsoleType {
    /// Plain NES / Famicom / Dendy.
    #[default]
    Nes,
    /// Vs. System (arcade); see `Header::vs_ppu_type` and `Header::vs_hardware_type`.
    VsSystem,
    PlayChoice10,
    /// NES 2.0 extended console type; see `Header::extended_console_type`.
    Extended,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// PRG-ROM size in 16 KB units, rounded up when an exponent-multiplier size is not a whole number of units.
    pub prg_banks: usize,
    /// CHR-ROM size in 8 KB units, rounded up likewise.
    pub chr_banks: usize,
    /// Exact PRG-ROM size in bytes (differs from `prg_banks * 16384` only for exponent-multiplier sizes).
    pub prg_rom_bytes: usize,
    /// Exact CHR-ROM size in bytes (differs from `chr_banks * 8192` only for exponent-multiplier sizes).
    pub chr_rom_bytes: usize,
    pub mapper: u16,
    pub mirroring: Mirroring,
    pub battery: bool,
    pub trainer: bool,
    /// NES 2.0 header (flags 7 bits 2-3 == 0b10).
    pub nes2: bool,
    pub submapper: u8,
    /// NES 2.0 RAM sizes in bytes (64 << shift, 0 when the shift is 0). All 0 for iNES 1.0.
    pub prg_ram_bytes: usize,
    pub prg_nvram_bytes: usize,
    pub chr_ram_bytes: usize,
    pub chr_nvram_bytes: usize,
    pub timing: Timing,
    /// Byte 7 bits 0-1. iNES 1.0 only defines bit 0 (Vs. Unisystem) and bit 1 (PlayChoice-10); a
    /// header whose byte 7 is junk (DiskDude! and the like, see `parse_header`) is always `Nes`.
    pub console_type: ConsoleType,
    /// NES 2.0, Vs. System only (byte 13 low nibble): Vs. PPU type (0 = RP2C03B, 2 = RP2C04-0001, ...). Else 0.
    pub vs_ppu_type: u8,
    /// NES 2.0, Vs. System only (byte 13 high nibble): Vs. hardware type (0 = Unisystem normal, ...). Else 0.
    pub vs_hardware_type: u8,
    /// NES 2.0, `ConsoleType::Extended` only (byte 13 low nibble): extended console type. Else 0.
    pub extended_console_type: u8,
    /// NES 2.0 byte 14 bits 0-1: number of miscellaneous ROM areas after CHR-ROM. 0 for iNES 1.0.
    pub misc_roms: u8,
    /// NES 2.0 byte 15 bits 0-5: default expansion device (0 = unspecified). 0 for iNES 1.0.
    pub default_expansion_device: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Timing {
    #[default]
    Ntsc,
    Pal,
    Multi,
    Dendy,
}

/// Console video timing a ROM asks for (NES 2.0 byte 12). `Multi` boots as NTSC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    Ntsc,
    Pal,
    Dendy,
}

impl From<Timing> for Region {
    /// Only a PAL- or Dendy-only header changes the machine's timing; NTSC and multi-region run as NTSC.
    fn from(timing: Timing) -> Region {
        match timing {
            Timing::Pal => Region::Pal,
            Timing::Dendy => Region::Dendy,
            Timing::Ntsc | Timing::Multi => Region::Ntsc,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomData {
    pub header: Header,
    pub prg: Vec<u8>,
    pub chr: Vec<u8>,
    pub chr_is_ram: bool,
    /// The 512-byte trainer (header flag 6 bit 2) that sits between the header and PRG-ROM; the
    /// console copies it to PRG RAM at `$7000` (see `Nes::from_rom_data_console`).
    pub trainer: Option<Vec<u8>>,
}

pub fn parse_header(bytes: &[u8]) -> Result<Header, CartError> {
    if bytes.len() < 16 {
        return Err(CartError::TooShort);
    }

    if bytes[0..4] != *b"NES\x1A" {
        return Err(CartError::BadMagic);
    }

    let flags6 = bytes[6];
    // Old dump tools wrote their names over the unused end of the header ("DiskDude!" over bytes
    // 7-15 is the common one), so byte 7 can hold text instead of mapper high bits. The iNES rule
    // of thumb: a header not marked NES 2.0 with anything in bytes 12-15 may have junk
    // there. Byte 7 is treated as zero (mapper from byte 6 alone, `nes2` false) only when its low
    // nibble also cannot be iNES 1.0, i.e. is not 0, 1 (Vs.) or 2 (PlayChoice): a real mapper high
    // nibble ahead of a tool tag ($40 on Batman - Return of the Joker, $e0 on Action 52) is kept.
    let junk7 = bytes[7] & 0x0C != 0x08 && bytes[7] & 0x0F > 2 && bytes[12..16] != [0; 4];
    let flags7 = if junk7 { 0 } else { bytes[7] };

    let mirroring = if flags6 & 0x08 != 0 {
        Mirroring::FourScreen
    } else if flags6 & 0x01 != 0 {
        Mirroring::Vertical
    } else {
        Mirroring::Horizontal
    };

    let battery = flags6 & 0x02 != 0;
    let trainer = flags6 & 0x04 != 0;
    let mut mapper = u16::from((flags7 & 0xF0) | (flags6 >> 4));
    let mut prg_banks = usize::from(bytes[4]);
    let mut chr_banks = usize::from(bytes[5]);
    let mut prg_rom_bytes = prg_banks * 16384;
    let mut chr_rom_bytes = chr_banks * 8192;

    // NES 2.0 (flags7 bits 2-3 == 0b10). Beyond the junk rule above, no header-cleanliness
    // heuristics are applied here.
    let nes2 = flags7 & 0x0C == 0x08;
    let mut submapper: u8 = 0;
    let mut prg_ram_bytes: usize = 0;
    let mut prg_nvram_bytes: usize = 0;
    let mut chr_ram_bytes: usize = 0;
    let mut chr_nvram_bytes: usize = 0;
    let mut timing = Timing::Ntsc;
    let mut vs_ppu_type = 0;
    let mut vs_hardware_type = 0;
    let mut extended_console_type = 0;
    let mut misc_roms = 0;
    let mut default_expansion_device = 0;

    // flags7 is already zero for a junk header, so that decodes as a plain NES.
    let console_type = if nes2 {
        [
            ConsoleType::Nes,
            ConsoleType::VsSystem,
            ConsoleType::PlayChoice10,
            ConsoleType::Extended,
        ][usize::from(flags7 & 3)]
    } else if flags7 & 1 != 0 {
        ConsoleType::VsSystem
    } else if flags7 & 2 != 0 {
        ConsoleType::PlayChoice10
    } else {
        ConsoleType::Nes
    };

    if nes2 {
        let b8 = bytes[8];
        let b9 = bytes[9];
        let b10 = bytes[10];
        let b11 = bytes[11];
        let b12 = bytes[12];

        mapper |= u16::from(b8 & 0x0F) << 8;
        submapper = b8 >> 4;
        (prg_banks, prg_rom_bytes) = nes2_rom_size(bytes[4], b9 & 0x0F, 16384)?;
        (chr_banks, chr_rom_bytes) = nes2_rom_size(bytes[5], b9 >> 4, 8192)?;

        let shift = |n: u8| if n == 0 { 0 } else { 64usize << n };
        prg_ram_bytes = shift(b10 & 15);
        prg_nvram_bytes = shift(b10 >> 4);
        chr_ram_bytes = shift(b11 & 15);
        chr_nvram_bytes = shift(b11 >> 4);

        timing = [Timing::Ntsc, Timing::Pal, Timing::Multi, Timing::Dendy][usize::from(b12 & 3)];

        match console_type {
            ConsoleType::VsSystem => {
                vs_ppu_type = bytes[13] & 0x0F;
                vs_hardware_type = bytes[13] >> 4;
            }
            ConsoleType::Extended => extended_console_type = bytes[13] & 0x0F,
            _ => {}
        }
        misc_roms = bytes[14] & 3;
        default_expansion_device = bytes[15] & 0x3F;
    }

    Ok(Header {
        prg_banks,
        chr_banks,
        prg_rom_bytes,
        chr_rom_bytes,
        mapper,
        mirroring,
        battery,
        trainer,
        nes2,
        submapper,
        prg_ram_bytes,
        prg_nvram_bytes,
        chr_ram_bytes,
        chr_nvram_bytes,
        timing,
        console_type,
        vs_ppu_type,
        vs_hardware_type,
        extended_console_type,
        misc_roms,
        default_expansion_device,
    })
}

/// NES 2.0 ROM size from its LSB byte (header byte 4 or 5) and MSB nibble (from byte 9), as
/// `(units, exact bytes)`. MSB nibble $F selects exponent-multiplier notation: the LSB is
/// `EEEEEEMM` and the size is `2^E * (2*MM + 1)` bytes. Otherwise the 12-bit value counts `unit`s.
/// `units` rounds up so mapper bank math stays valid; `split` pads the remainder.
fn nes2_rom_size(lsb: u8, msb: u8, unit: usize) -> Result<(usize, usize), CartError> {
    if msb != 0x0F {
        let units = usize::from(lsb) | usize::from(msb) << 8;
        return Ok((units, units * unit));
    }
    let bytes = 1usize
        .checked_shl(u32::from(lsb >> 2))
        .and_then(|p| p.checked_mul(usize::from(lsb & 3) * 2 + 1))
        .ok_or(CartError::UnsupportedSize)?;
    Ok((bytes.div_ceil(unit), bytes))
}

impl Header {
    /// PRG RAM a board should allocate: the NES 2.0 sizes, else 8 KB (the iNES 1.0 convention used today).
    pub fn prg_ram_size(&self) -> usize {
        if self.nes2 {
            self.prg_ram_bytes + self.prg_nvram_bytes
        } else {
            8192
        }
    }

    /// CHR RAM a board should allocate when the cartridge has no CHR-ROM: the NES 2.0 sizes, else
    /// 8 KB (the iNES 1.0 convention used today). A NES 2.0 header may legitimately ask for none.
    ///
    /// NES 2.0 splits CHR RAM and CHR NVRAM into separate nibbles and this returns their sum. A
    /// header that declares both CHR-ROM and CHR RAM is sized from the ROM alone; that is correct
    /// for every board implemented here, but boards that use both at once (74, 119/TQROM, 191-195)
    /// will need the RAM size kept separately when they are added.
    pub fn chr_ram_size(&self) -> usize {
        if self.nes2 {
            self.chr_ram_bytes + self.chr_nvram_bytes
        } else {
            8192
        }
    }
}

pub fn split(bytes: &[u8]) -> Result<RomData, CartError> {
    let mut header = parse_header(bytes)?;

    let prg_start: usize = 16 + if header.trainer { 512 } else { 0 };
    let prg_len = header.prg_rom_bytes;
    let chr_len = header.chr_rom_bytes;

    if prg_start
        .checked_add(prg_len)
        .and_then(|n| n.checked_add(chr_len))
        .is_none_or(|n| bytes.len() < n)
    {
        return Err(CartError::Truncated);
    }

    // an image with no program ROM has nothing to map — every board divides by the PRG length
    // on its first read — so it is a load error, not a panic inside a board.
    if prg_len == 0 {
        return Err(CartError::NoPrg);
    }

    let trainer = header.trainer.then(|| bytes[16..16 + 512].to_vec());

    // An exponent-multiplier size that is not a whole number of banks is padded up to the bank
    // with 0xFF (erased-ROM value) so mappers can index whole banks.
    let mut prg = bytes[prg_start..prg_start + prg_len].to_vec();
    prg.resize(header.prg_banks * 16384, 0xFF);

    let (chr, chr_is_ram) = if chr_len == 0 {
        (vec![0; header.chr_ram_size()], true)
    } else {
        let mut chr = bytes[prg_start + prg_len..prg_start + prg_len + chr_len].to_vec();
        chr.resize(header.chr_banks * 8192, 0xFF);
        (chr, false)
    };

    // a ROM the game database knows gets the board the databases describe, because its own
    // header is wrong for exactly the fields the table carries. A good header is left untouched and
    // prints nothing. The hash covers the ROM data only: a CHR-RAM board's zeros are this crate's
    // allocation, not part of the image the databases hashed.
    let rom_chr = if chr_is_ram { &[][..] } else { &chr[..] };
    if let Some(correction) = crate::gamedb::apply(&mut header, &prg, rom_chr) {
        correction.log();
    }

    Ok(RomData {
        header,
        prg,
        chr,
        chr_is_ram,
        trainer,
    })
}

/// Read CHR memory at `index`, wrapping to the size the board allocated. A NES 2.0 board with no
/// CHR memory at all reads as an open bus 0 instead of panicking.
///
/// Every real CHR image is a power of two, so the wrap is a mask on that path; `%` is the
/// fallback for an exponent-multiplier size that is not.
pub fn chr_read(chr: &[u8], index: usize) -> u8 {
    match chr.len() {
        0 => 0,
        n if n.is_power_of_two() => chr[index & (n - 1)],
        n => chr[index % n],
    }
}

/// Write CHR RAM at `index`, wrapping to the size the board allocated. Writes to a board with no
/// CHR memory are dropped.
pub fn chr_write(chr: &mut [u8], index: usize, val: u8) {
    let n = chr.len();
    if n != 0 {
        chr[index % n] = val;
    }
}

/// The CHR-ROM byte `chr_read` would read for `index`, for [`Mapper::chr_rom_offset`]:
/// `None` when the board's CHR is RAM (or absent), so the caller keys on the tile's bytes instead.
/// The wrap is the same one `chr_read` applies, so the two cannot name different bytes.
pub fn chr_rom_index(chr: &[u8], chr_is_ram: bool, index: usize) -> Option<usize> {
    if chr_is_ram || chr.is_empty() {
        return None;
    }
    Some(match chr.len() {
        n if n.is_power_of_two() => index & (n - 1),
        n => index % n,
    })
}

/// Number of 8 KB PRG banks a board can select, never zero: a bank register's `% n` needs a divisor
/// even for a malformed image.
pub fn prg_bank_count(prg: &[u8]) -> usize {
    (prg.len() / 0x2000).max(1)
}

/// Number of 1 KB CHR banks, never zero.
pub fn chr_bank_count(chr: &[u8]) -> usize {
    (chr.len() / 0x400).max(1)
}

/// Copy the prefix of `data` that fits into `dst`: the "load a battery file or state image that may
/// be shorter or longer than this board's RAM" step every `load_prg_ram`/`load_chr_ram` does.
pub fn copy_prefix(dst: &mut [u8], data: &[u8]) {
    let n = data.len().min(dst.len());
    dst[..n].copy_from_slice(&data[..n]);
}

/// The CHR-RAM a board hands to the battery save: its CHR bytes when the board has CHR RAM,
/// nothing when CHR is ROM.
pub fn chr_ram_slice(chr: &[u8], chr_is_ram: bool) -> &[u8] {
    if chr_is_ram { chr } else { &[] }
}

/// Fill a board's PRG RAM, then its CHR-RAM, for [`Mapper::power_on_ram`].
///
/// `Zero` leaves both untouched (boards are built zeroed); `Ones` writes 0xFF; `Random` draws one
/// `SeedDomain::CartRam` stream, eight bytes per step least-significant byte first
/// ([`crate::rng::SplitMix64::fill_bytes`]), PRG RAM first and CHR-RAM straight after it. Pass an
/// empty `chr_ram` for a board with CHR-ROM: ROM is never written.
pub fn fill_power_on_ram(
    prg_ram: &mut [u8],
    chr_ram: &mut [u8],
    mode: crate::bus::RamPowerOn,
    seed: u64,
) {
    use crate::bus::RamPowerOn;
    match mode {
        RamPowerOn::Zero => {}
        RamPowerOn::Ones => {
            prg_ram.fill(0xFF);
            chr_ram.fill(0xFF);
        }
        RamPowerOn::Random => {
            let mut rng = crate::rng::derive(seed, crate::rng::SeedDomain::CartRam);
            rng.fill_bytes(prg_ram);
            rng.fill_bytes(chr_ram);
        }
    }
}

/// The per-CPU-cycle hooks a board implements. `SysBus::clock` runs once per CPU cycle — about
/// 1.79 million times a second on NTSC — and the cartridge is a `Box<dyn Mapper>`, so every call on it
/// is a virtual call the compiler cannot inline. Most boards leave one or both of these hooks at their
/// empty defaults, and the bus skips a hook whose flag is clear instead of calling into the board.
///
/// A board that overrides [`Mapper::irq_pending`] or [`Mapper::cpu_clock`] must also override
/// [`Mapper::hooks`] to say so; if it does not, the bus never calls it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CartHooks {
    /// The board overrides [`Mapper::irq_pending`].
    pub irq: bool,
    /// The board overrides [`Mapper::cpu_clock`].
    pub cpu_clock: bool,
}

pub trait Mapper {
    fn cpu_read(&mut self, addr: u16) -> u8;
    fn cpu_write(&mut self, addr: u16, val: u8);
    /// Side-effect-free [`Mapper::cpu_read`] for the debugger and the tracer: the same byte,
    /// without any state a read can change. The default is a plain `cpu_read`, which is
    /// right for every board whose reads are pure; a board that does write state on read must
    /// override it, so that peeking (a trace line, a debugger view) cannot change the emulation.
    fn cpu_peek(&mut self, addr: u16) -> u8 {
        self.cpu_read(addr)
    }
    fn ppu_read(&mut self, addr: u16) -> u8;
    fn ppu_write(&mut self, addr: u16, val: u8);
    fn mirroring(&self) -> Mirroring;
    fn prg_ram(&self) -> &[u8];
    fn load_prg_ram(&mut self, data: &[u8]);
    /// Writable CHR memory (CHR-RAM, including NES 2.0 CHR-NVRAM) for the battery save.
    /// Boards with CHR-ROM, and boards that do not expose their CHR-RAM yet, return an empty slice.
    fn chr_ram(&self) -> &[u8] {
        &[]
    }
    /// Restore CHR-RAM from a battery save. Copies at most the board's CHR-RAM length;
    /// boards with CHR-ROM, or without support yet, ignore it.
    fn load_chr_ram(&mut self, _data: &[u8]) {}
    /// The bytes a battery `.sav` holds for this board. The default is PRG RAM, which is
    /// what a cartridge persists; the Famicom Disk System overrides it because its games save to
    /// the disk rather than to the program RAM the BIOS overwrites at the next boot.
    fn battery_data(&self) -> Vec<u8> {
        self.prg_ram().to_vec()
    }
    /// Restore what [`Mapper::battery_data`] wrote. The default is PRG RAM, so a `.sav`
    /// written with no magic still loads.
    fn load_battery_data(&mut self, data: &[u8]) {
        self.load_prg_ram(data);
    }
    /// What the frontend compares to decide whether an autosave has anything to write:
    /// [`Mapper::battery_data`] with the bytes the board's own hardware rewrites while it runs
    /// masked out. Those are not save data, and a chip that keeps moving them would otherwise make
    /// every autosave look changed and rewrite the `.sav`. The default is the data itself.
    fn battery_change_key(&self) -> Vec<u8> {
        self.battery_data()
    }
    /// Fill the board's volatile RAM the way a power-on would have left it: PRG RAM
    /// and, when the board has CHR-RAM, CHR-RAM. The default does nothing, so a board that has
    /// not opted in keeps its zeroed RAM. Runs at construction, before any battery `.sav` is
    /// loaded, so a save always wins over the fill.
    fn power_on_ram(&mut self, _mode: crate::bus::RamPowerOn, _seed: u64) {}
    /// Mutable board state only (bank registers, PRG RAM, CHR RAM); never ROM.
    fn save_state(&self, w: &mut crate::state::StateWriter);
    fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError>;
    /// Which per-CPU-cycle hooks this board implements, so the bus can skip the virtual calls
    /// for the ones it leaves at the defaults. The default — no hooks — is right for a board that does
    /// not override [`Mapper::irq_pending`] or [`Mapper::cpu_clock`].
    fn hooks(&self) -> CartHooks {
        CartHooks::default()
    }
    fn irq_pending(&self) -> bool {
        false
    }
    /// Called once per CPU cycle (M2), after the PPU dots and APU clock of that cycle.
    fn cpu_clock(&mut self) {}
    /// Cartridge expansion audio for the current CPU cycle, in the 2A03 mixer's scale (see
    /// `apu::expansion_pulse`). The bus hands this to `Apu::expansion_pulse` on the cycles the APU
    /// mixer runs, and the APU mixes it into the nonlinear mixer before the output filter.
    /// Boards without expansion audio keep the 0.0 default.
    fn expansion_audio(&self) -> f32 {
        0.0
    }
    /// Name of the cartridge's expansion-audio chip, for the frontend's APU viewer: `None`
    /// on a board without one, which is what makes the viewer draw no expansion row. A name, not a
    /// level: the level is [`Mapper::expansion_audio`]'s job, and a row labelled with the chip it
    /// came from cannot be mistaken for another board's channel.
    fn expansion_audio_name(&self) -> Option<&'static str> {
        None
    }
    /// A CPU read of a board register in `$4020-$5FFF` (the Namco 163's chip-RAM data port at
    /// `$4800-$4FFF`). `None` means the board does not decode the address and the bus shows the
    /// open-bus value it already held.
    fn ext_read(&mut self, _addr: u16) -> Option<u8> {
        None
    }
    /// A nametable byte at PPU `$2000-$3EFF` from board memory (the Namco 163 can map its CHR-ROM
    /// and its 2 KB of CIRAM there). `ciram` is the PPU's 4 KB of nametable RAM, so a board that
    /// banks CIRAM per window can index it directly.
    ///
    /// The default is the standard CIRAM mapping selected by [`Mapper::mirroring`], inline in this
    /// method: the PPU's fetch path used to call `nt_read` (which answered `None` here) and
    /// then `mirroring` itself, two indirect calls per nametable byte. `self.mirroring()` is a
    /// direct call inside this default body, so a board that does not override it pays one.
    fn nt_read(&mut self, addr: u16, ciram: &[u8]) -> Option<u8> {
        Some(ciram[crate::ppu::memory::nametable_index(addr, self.mirroring())])
    }
    /// A nametable write to board memory at PPU `$2000-$3EFF`; returns true when the board took it.
    /// The default writes CIRAM through [`Mapper::mirroring`], inline like `nt_read`.
    fn nt_write(&mut self, addr: u16, val: u8, ciram: &mut [u8]) -> bool {
        ciram[crate::ppu::memory::nametable_index(addr, self.mirroring())] = val;
        true
    }
    /// The PPU address bus changed without a memory access (e.g. $2006/$2007 updating v while not rendering).
    fn ppu_bus(&mut self, _addr: u16) {}
    /// Side-effect-free sprite-pattern fetch at `addr` (0x0000..=0x1FFF). Boards that keep separate
    /// background and sprite CHR banks (MMC5) override this; the default is a normal pattern read.
    fn ppu_read_sprite(&mut self, addr: u16) -> u8 {
        self.ppu_read(addr)
    }
    /// The PPU wrote one of its own registers at CPU address `addr` ($2000-$2007, not a mirror).
    /// Boards that decode the PPU's state from the bus (MMC5 detects 8x16 sprites and rendering
    /// this way) override it; the default does nothing.
    fn ppu_register_write(&mut self, _addr: u16, _val: u8) {}
    /// CPU address to the PRG-ROM byte it reads under the board's current bank registers,
    /// for the code/data log. `None` when the address is not PRG ROM, and the default for a board
    /// that has not implemented the mapping, so the log skips those bytes. Read-only.
    fn prg_rom_offset(&self, _addr: u16) -> Option<usize> {
        None
    }
    /// CHR address to the CHR-ROM byte the PPU currently sees there, for the tile
    /// trace and the HD-pack compositor. The read-only twin of [`Mapper::chr_peek`]: the same
    /// live banking (bank registers, MMC2/MMC4 latch state), no side effects. `None` for a board
    /// whose CHR is RAM, which is what tells the compositor to key on the tile's bytes instead.
    fn chr_rom_offset(&self, _addr: u16) -> Option<usize> {
        None
    }
    /// Side-effect-free read of the CHR byte the PPU currently sees at `addr` (0x0000..=0x1FFF).
    ///
    /// This is the debug-viewer door into pattern memory: it follows the board's *live* banking
    /// (CHR-RAM contents, bank registers, MMC2/MMC4 latch state) without the side effects of
    /// `ppu_read`/`mem_read` — no latch updates, no A12/IRQ counter clocks, no bus or open-bus
    /// state. A viewer can therefore snapshot the pattern tables mid-frame without perturbing
    /// emulation. Implemented for every board that owns CHR; the default answers 0 for a board
    /// that has no CHR view of its own.
    fn chr_peek(&self, addr: u16) -> u8 {
        let _ = addr;
        0
    }
    /// Side-effect-free read of the nametable byte the PPU currently sees at `addr`
    /// (`$2000-$3EFF`), for the debug viewer.
    ///
    /// This is [`nt_read`](Self::nt_read)'s read-only twin: same routing, but `&self` and with no
    /// board side effects. MMC5's `nt_read` clocks its split-screen fetch counter, so a viewer that
    /// read through it would perturb emulation; `nt_peek` must not. `ciram` is the PPU's 4 KB of
    /// nametable RAM, for boards that bank CIRAM per window. `None` means the standard CIRAM mapping
    /// selected by [`Mapper::mirroring`].
    fn nt_peek(&self, _addr: u16, _ciram: &[u8]) -> Option<u8> {
        None
    }
    /// Randomised power-on hook: boards override this to start in a seed-derived garbage
    /// bank state. The default no-op keeps every non-overriding mapper byte-identical to power-on.
    fn power_on_random(&mut self, seed: u64) {
        let _ = seed;
    }

    /// The CPU wrote `$4016`, the controller strobe register. Only a Vs. System board latches it:
    /// on mapper 99 its bit 2 is the cartridge's A13, i.e. the CHR/PRG bank register (nesdev
    /// "Vs. System"). Every other board - and a Vs. board that is not mapper 99 - drops it, which
    /// is what makes a plain NES's `$4016` write do nothing outside the ports.
    fn write_4016(&mut self, _val: u8) {}

    /// A board-mapped write in `$4020-$5FFF`. Returns true when the board claimed the address;
    /// false falls through to the board's ordinary `cpu_write`.
    fn ext_write(&mut self, addr: u16, val: u8) -> bool {
        let _ = (addr, val);
        false
    }

    /// FDS side swap: mount the next side of the inserted disk image, returning
    /// `(zero-based side, side count)`. `None` on every other board, so a frontend keybind can
    /// call it unconditionally.
    fn next_disk_side(&mut self) -> Option<(usize, usize)> {
        None
    }

    /// The mounted FDS side as `(zero-based side, side count)`, for a frontend overlay.
    fn disk_side(&self) -> Option<(usize, usize)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(prg_banks: u8, chr_banks: u8, flags6: u8, flags7: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, flags6, flags7, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v.resize(
            16 + prg_banks as usize * 16384 + chr_banks as usize * 8192,
            0,
        );
        v
    }

    #[test]
    fn header_basic() {
        assert_eq!(
            parse_header(&rom(2, 1, 0x01, 0x00)).unwrap(),
            Header {
                prg_banks: 2,
                chr_banks: 1,
                prg_rom_bytes: 32768,
                chr_rom_bytes: 8192,
                mapper: 0,
                mirroring: Mirroring::Vertical,
                battery: false,
                trainer: false,
                ..Default::default()
            }
        );
    }

    #[test]
    fn header_mapper_number() {
        assert_eq!(parse_header(&rom(1, 1, 0x10, 0x40)).unwrap().mapper, 0x41);
    }

    #[test]
    fn diskdude_filler_does_not_change_mapper() {
        // Crystalis (U): 256K PRG, 128K CHR, flags6 $42 (MMC3 + battery), then "DiskDude!" over
        // bytes 7-15, so byte 7 is $44 instead of mapper high bits.
        let mut v = rom(16, 16, 0x42, 0x44);
        v[7..16].copy_from_slice(b"DiskDude!");
        let h = parse_header(&v).unwrap();
        assert_eq!(h.mapper, 4);
        assert!(h.battery);
        assert!(!h.nes2);
    }

    #[test]
    fn diskdude_filler_keeps_low_mapper_nibble() {
        // Cybernoid (U): flags6 $31 -> CNROM (mapper 3), vertical mirroring; byte 7 is filler.
        let mut v = rom(2, 4, 0x31, 0x44);
        v[7..16].copy_from_slice(b"DiskDude!");
        let h = parse_header(&v).unwrap();
        assert_eq!(h.mapper, 3);
        assert_eq!(h.mirroring, Mirroring::Vertical);
        assert!(!h.nes2);
    }

    #[test]
    fn header_battery_and_trainer() {
        let h = parse_header(&rom(1, 1, 0x06, 0x00)).unwrap();
        assert!(h.battery);
        assert!(h.trainer);
        assert_eq!(h.mirroring, Mirroring::Horizontal);
    }

    #[test]
    fn header_four_screen() {
        assert_eq!(
            parse_header(&rom(1, 1, 0x09, 0x00)).unwrap().mirroring,
            Mirroring::FourScreen
        );
    }

    #[test]
    fn too_short() {
        assert_eq!(parse_header(&[0x4E, 0x45, 0x53]), Err(CartError::TooShort));
    }

    #[test]
    fn bad_magic() {
        let mut v = rom(1, 1, 0, 0);
        v[0] = 0;
        assert_eq!(parse_header(&v), Err(CartError::BadMagic));
    }

    #[test]
    fn split_sizes() {
        let r = split(&rom(2, 1, 0, 0)).unwrap();
        assert_eq!(r.prg.len(), 32768);
        assert_eq!(r.chr.len(), 8192);
        assert!(!r.chr_is_ram);
    }

    #[test]
    fn split_chr_ram() {
        let r = split(&rom(1, 0, 0, 0)).unwrap();
        assert_eq!(r.chr.len(), 8192);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn split_reads_prg_and_chr_bytes() {
        let mut v = rom(1, 1, 0, 0);
        v[16] = 0xAA;
        v[16 + 16384] = 0xBB;
        let r = split(&v).unwrap();
        assert_eq!(r.prg[0], 0xAA);
        assert_eq!(r.chr[0], 0xBB);
    }

    #[test]
    fn split_skips_trainer() {
        let mut v = rom(1, 1, 0x04, 0);
        v.extend(vec![0u8; 512]);
        v[16 + 512] = 0xCC;
        assert_eq!(split(&v).unwrap().prg[0], 0xCC);
    }

    #[test]
    fn split_keeps_trainer_bytes() {
        let mut v = rom(1, 1, 0x04, 0);
        v.splice(16..16, (0..512).map(|i| (i % 256) as u8));
        let r = split(&v).unwrap();
        assert_eq!(r.trainer.as_deref(), Some(&v[16..16 + 512]));
        assert_eq!(split(&rom(1, 1, 0, 0)).unwrap().trainer, None);
    }

    #[test]
    fn split_truncated() {
        let mut v = rom(1, 1, 0, 0);
        v.truncate(100);
        assert_eq!(split(&v), Err(CartError::Truncated));
    }

    fn header(bytes: &[u8]) -> Vec<u8> {
        let mut v = bytes.to_vec();
        v.resize(16, 0);
        v
    }

    #[test]
    fn nes2_timing_byte_maps_to_region() {
        for (byte, timing, region) in [
            (0u8, Timing::Ntsc, Region::Ntsc),
            (1, Timing::Pal, Region::Pal),
            (2, Timing::Multi, Region::Ntsc),
            (3, Timing::Dendy, Region::Dendy),
        ] {
            let h = parse_header(&header(&[
                0x4E, 0x45, 0x53, 0x1A, 0x01, 0x01, 0x00, 0x08, 0, 0, 0, 0, byte,
            ]))
            .unwrap();
            assert_eq!(h.timing, timing);
            assert_eq!(Region::from(h.timing), region);
        }
    }

    #[test]
    fn ines_header_is_ntsc_region() {
        assert_eq!(
            Region::from(parse_header(&rom(1, 1, 0, 0)).unwrap().timing),
            Region::Ntsc
        );
    }

    #[test]
    fn nes2_full() {
        let h = parse_header(&header(&[
            0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x42, 0x08, 0x40, 0x00, 0x70, 0x07, 0x01,
        ]))
        .unwrap();
        assert!(h.nes2);
        assert_eq!(h.mapper, 4);
        assert_eq!(h.submapper, 4);
        assert!(h.battery);
        assert_eq!(h.prg_ram_bytes, 0);
        assert_eq!(h.prg_nvram_bytes, 8192);
        assert_eq!(h.chr_ram_bytes, 8192);
        assert_eq!(h.chr_nvram_bytes, 0);
        assert_eq!(h.timing, Timing::Pal);
        assert_eq!(h.prg_ram_size(), 8192);
    }

    #[test]
    fn nes2_mapper_high_bits() {
        let h = parse_header(&header(&[
            0x4E, 0x45, 0x53, 0x1A, 0x01, 0x01, 0x00, 0x08, 0x21,
        ]))
        .unwrap();
        assert_eq!(h.mapper, 256);
        assert_eq!(h.submapper, 2);
    }

    #[test]
    fn nes2_prg_bank_extension() {
        let h = parse_header(&header(&[
            0x4E, 0x45, 0x53, 0x1A, 0x00, 0x01, 0x00, 0x08, 0x00, 0x01,
        ]))
        .unwrap();
        assert_eq!(h.prg_banks, 256);
    }

    /// NES 2.0 header with the given bytes 4, 5, 9 (sizes) and 13, 14, 15, byte 7 = `b7`.
    fn nes2_hdr(b7: u8, b4: u8, b5: u8, b9: u8, tail: [u8; 3]) -> Vec<u8> {
        header(&[
            0x4E, 0x45, 0x53, 0x1A, b4, b5, 0, b7, 0, b9, 0, 0, 0, tail[0], tail[1], tail[2],
        ])
    }

    #[test]
    fn nes2_exponent_size_whole_unit() {
        // E=14, MM=0 -> 16384 bytes = exactly one PRG bank.
        let h = parse_header(&nes2_hdr(0x08, 14 << 2, 1, 0x0F, [0; 3])).unwrap();
        assert_eq!((h.prg_banks, h.prg_rom_bytes), (1, 16384));
        assert_eq!((h.chr_banks, h.chr_rom_bytes), (1, 8192));
        let mut v = nes2_hdr(0x08, 14 << 2, 0, 0x0F, [0; 3]);
        v.resize(16 + 16384, 0);
        v[16] = 0x5A;
        let r = split(&v).unwrap();
        assert_eq!(r.prg.len(), 16384);
        assert_eq!(r.prg[0], 0x5A);
    }

    #[test]
    fn nes2_exponent_size_is_padded_to_unit() {
        // PRG: E=13, MM=1 -> 8192 * 3 = 24576 bytes -> 2 banks, last 8 KB padded with 0xFF.
        // CHR: E=11, MM=0 -> 2048 bytes -> 1 bank, padded.
        let mut v = nes2_hdr(0x08, 13 << 2 | 1, 11 << 2, 0xFF, [0; 3]);
        v.extend(vec![0x11; 24576]);
        v.extend(vec![0x22; 2048]);
        let r = split(&v).unwrap();
        assert_eq!((r.header.prg_banks, r.header.prg_rom_bytes), (2, 24576));
        assert_eq!((r.header.chr_banks, r.header.chr_rom_bytes), (1, 2048));
        assert_eq!(r.prg.len(), 32768);
        assert!(r.prg[..24576].iter().all(|&b| b == 0x11));
        assert!(r.prg[24576..].iter().all(|&b| b == 0xFF));
        assert_eq!(r.chr.len(), 8192);
        assert!(!r.chr_is_ram);
        assert!(r.chr[..2048].iter().all(|&b| b == 0x22));
        assert!(r.chr[2048..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn nes2_exponent_size_truncated_uses_exact_bytes() {
        let mut v = nes2_hdr(0x08, 13 << 2 | 1, 0, 0x0F, [0; 3]);
        v.resize(16 + 24575, 0);
        assert_eq!(split(&v), Err(CartError::Truncated));
        v.push(0);
        assert!(split(&v).is_ok());
    }

    #[test]
    fn nes2_exponent_size_overflow_is_unsupported() {
        // E=63 overflows usize.
        assert_eq!(
            parse_header(&nes2_hdr(0x08, 63 << 2 | 3, 0, 0x0F, [0; 3])),
            Err(CartError::UnsupportedSize)
        );
    }

    #[test]
    fn nes2_console_type_and_vs_fields() {
        let h = parse_header(&nes2_hdr(0x09, 1, 1, 0, [0x5A, 0, 0])).unwrap();
        assert_eq!(h.console_type, ConsoleType::VsSystem);
        assert_eq!(h.vs_ppu_type, 0x0A);
        assert_eq!(h.vs_hardware_type, 0x05);
        assert_eq!(h.extended_console_type, 0);

        let h = parse_header(&nes2_hdr(0x0A, 1, 1, 0, [0x5A, 0, 0])).unwrap();
        assert_eq!(h.console_type, ConsoleType::PlayChoice10);
        assert_eq!(
            (h.vs_ppu_type, h.vs_hardware_type, h.extended_console_type),
            (0, 0, 0)
        );

        let h = parse_header(&nes2_hdr(0x0B, 1, 1, 0, [0x5A, 0, 0])).unwrap();
        assert_eq!(h.console_type, ConsoleType::Extended);
        assert_eq!(h.extended_console_type, 0x0A);
        assert_eq!((h.vs_ppu_type, h.vs_hardware_type), (0, 0));

        let h = parse_header(&nes2_hdr(0x08, 1, 1, 0, [0x5A, 0, 0])).unwrap();
        assert_eq!(h.console_type, ConsoleType::Nes);
        assert_eq!(
            (h.vs_ppu_type, h.vs_hardware_type, h.extended_console_type),
            (0, 0, 0)
        );
    }

    #[test]
    fn nes2_misc_roms_and_expansion_device() {
        let h = parse_header(&nes2_hdr(0x08, 1, 1, 0, [0, 0xFE, 0xFF])).unwrap();
        assert_eq!(h.misc_roms, 2);
        assert_eq!(h.default_expansion_device, 0x3F);
        let h = parse_header(&nes2_hdr(0x08, 1, 1, 0, [0, 0, 0x23])).unwrap();
        assert_eq!((h.misc_roms, h.default_expansion_device), (0, 0x23));
    }

    #[test]
    fn ines1_console_type_bits() {
        assert_eq!(
            parse_header(&rom(1, 1, 0, 0x00)).unwrap().console_type,
            ConsoleType::Nes
        );
        assert_eq!(
            parse_header(&rom(1, 1, 0, 0x01)).unwrap().console_type,
            ConsoleType::VsSystem
        );
        assert_eq!(
            parse_header(&rom(1, 1, 0, 0x02)).unwrap().console_type,
            ConsoleType::PlayChoice10
        );
    }

    #[test]
    fn ines1_ignores_nes2_only_fields() {
        let mut v = rom(1, 1, 0, 0x01);
        v[13] = 0x5A;
        v[14] = 3;
        v[15] = 0x3F;
        let h = parse_header(&v).unwrap();
        assert_eq!(
            (h.vs_ppu_type, h.vs_hardware_type, h.extended_console_type),
            (0, 0, 0)
        );
        assert_eq!((h.misc_roms, h.default_expansion_device), (0, 0));
        assert_eq!((h.prg_rom_bytes, h.chr_rom_bytes), (16384, 8192));
    }

    #[test]
    fn diskdude_header_is_plain_nes_console() {
        let mut v = rom(1, 1, 0, 0);
        v[7..16].copy_from_slice(b"DiskDude!");
        let h = parse_header(&v).unwrap();
        assert_eq!(h.console_type, ConsoleType::Nes);
        assert!(!h.nes2);
        assert_eq!((h.misc_roms, h.default_expansion_device), (0, 0));
    }

    #[test]
    fn junk_in_bytes_12_to_15_clears_byte_7() {
        // Tools other than DiskDude! left text there too. "demiforce" over bytes 7-15 made
        // byte 7 $64, so a UxROM image (flags 6 $21) asked for mapper 98 and did not load.
        let mut v = rom(2, 1, 0x21, 0);
        v[7..16].copy_from_slice(b"demiforce");
        let h = parse_header(&v).unwrap();
        assert_eq!(
            (h.mapper, h.console_type, h.nes2),
            (2, ConsoleType::Nes, false)
        );
        // Junk in byte 7 alone ($43: high nibble 4, Vs. and PlayChoice bits) plus any non-zero
        // byte in 12-15 used to run the same image as a Vs. GxROM (mapper 66) game.
        let mut v = rom(2, 1, 0x21, 0x43);
        v[15] = b'!';
        let h = parse_header(&v).unwrap();
        assert_eq!(
            (h.mapper, h.console_type, h.nes2),
            (2, ConsoleType::Nes, false)
        );
        // Out of scope, pinned so a change is deliberate: the same junk byte 7 over a clean tail
        // is still trusted, as at base, and runs the image as a Vs. GxROM game.
        let h = parse_header(&rom(2, 1, 0x21, 0x43)).unwrap();
        assert_eq!(
            (h.mapper, h.console_type, h.nes2),
            (66, ConsoleType::VsSystem, false)
        );
    }

    #[test]
    fn a_valid_byte_7_with_a_tag_in_bytes_10_to_15_keeps_its_mapper() {
        // QA found: a real iNES 1.0 byte 7 (mapper high nibble, at most one console bit) ahead
        // of a tool tag is not junk. Clearing it decoded the low nibble only: Batman - Return of
        // the Joker 69 -> 5, Bee 52 / Fire Hawk / Stunt Kids 71 -> 7, Action 52 228 -> 4.
        let tags: [(usize, &[u8]); 3] = [(10, b"NI 1.3"), (15, &[0x01]), (12, b"!")];
        for (f6, f7, mapper, console) in [
            (0x50, 0x40, 69, ConsoleType::Nes), // Batman - Return of the Joker
            (0x71, 0x40, 71, ConsoleType::Nes), // Bee 52, Stunt Kids (Fire Hawk is $70)
            (0x41, 0xE0, 228, ConsoleType::Nes), // Action 52
            (0x30, 0x61, 99, ConsoleType::VsSystem), // a Vs. board keeps its console bit too
        ] {
            for (at, tag) in tags {
                let mut v = rom(2, 1, f6, f7);
                v[at..at + tag.len()].copy_from_slice(tag);
                let h = parse_header(&v).unwrap();
                let got = (h.mapper, h.console_type, h.nes2);
                assert_eq!(
                    got,
                    (mapper, console, false),
                    "flags 7 ${f7:02x}, tag at {at}"
                );
            }
        }
    }

    #[test]
    fn clean_diskdude_and_nes2_headers_decode_as_before() {
        // Equivalence pin: the headers the junk rule must leave alone. Every flags 6 / flags 7
        // pair over a zero tail (bytes 8-15), every NES 2.0 flags 7 again with four filled tails,
        // and a "DiskDude!" tail under every flags 6. FNV-1a 64 over each result's Debug text,
        // recorded at 1b4ac87.
        let mut digest: u64 = 0xCBF2_9CE4_8422_2325;
        let mut feed = |h: Result<Header, CartError>| {
            for b in format!("{h:?}").bytes() {
                digest = (digest ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
            }
        };
        let tails: [[u8; 8]; 4] = [
            [0x01, 0x21, 0x07, 0x70, 0x01, 0x5A, 0x03, 0x23],
            [0x10, 0x0F, 0x90, 0x09, 0x03, 0x12, 0x01, 0x3F],
            [0xA5, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
            [0xFF; 8],
        ];
        let mut v = rom(2, 1, 0, 0);
        for f6 in 0..=255u8 {
            v[6] = f6;
            for f7 in 0..=255u8 {
                v[7] = f7;
                v[8..16].fill(0);
                feed(parse_header(&v));
                if f7 & 0x0C == 0x08 {
                    for tail in &tails {
                        v[8..16].copy_from_slice(tail);
                        feed(parse_header(&v));
                    }
                }
            }
            v[7..16].copy_from_slice(b"DiskDude!");
            feed(parse_header(&v));
        }
        assert_eq!(digest, 0xB324_D7B2_19A1_6705);
    }

    #[test]
    fn ines_header_is_not_nes2() {
        let h = parse_header(&rom(2, 1, 0x01, 0x00)).unwrap();
        assert!(!h.nes2);
        assert_eq!(h.submapper, 0);
        assert_eq!(h.timing, Timing::Ntsc);
        assert_eq!(h.prg_ram_size(), 8192);
    }

    /// A NES 2.0 image with 16 KB PRG, no CHR-ROM, and byte 11 = `b11` (CHR-NVRAM high nibble,
    /// CHR-RAM low nibble).
    fn nes2_rom(b11: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, b11, 0x00, 0, 0, 0,
        ];
        v.resize(16 + 16384, 0);
        v
    }

    #[test]
    fn nes2_chr_ram_size_from_header() {
        assert_eq!(split(&nes2_rom(0x07)).unwrap().chr.len(), 8192);
        assert_eq!(split(&nes2_rom(0x09)).unwrap().chr.len(), 32768);
    }

    #[test]
    fn nes2_without_chr_memory_allocates_none() {
        let r = split(&nes2_rom(0x00)).unwrap();
        assert!(r.chr.is_empty());
        assert!(r.chr_is_ram);
    }

    #[test]
    fn nes2_chr_nvram_is_chr_ram() {
        let r = split(&nes2_rom(0x70)).unwrap();
        assert_eq!(r.header.chr_nvram_bytes, 8192);
        assert_eq!(r.chr.len(), 8192);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn nes2_chr_ram_and_chr_nvram_are_summed() {
        // b11 = 0x77: CHR-RAM shift 7 and CHR-NVRAM shift 7, so 8 KiB + 8 KiB. The sum is a
        // deliberate choice (both are writable CHR memory), so pin it.
        let r = split(&nes2_rom(0x77)).unwrap();
        assert_eq!(r.header.chr_ram_bytes, 8192);
        assert_eq!(r.header.chr_nvram_bytes, 8192);
        assert_eq!(r.chr.len(), 16384);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn nes2_chr_below_8k_is_believed() {
        // b11 = 0x05: 64 << 5 = 2 KiB, four times smaller than the iNES 1.0 default.
        let r = split(&nes2_rom(0x05)).unwrap();
        assert_eq!(r.header.chr_ram_bytes, 2048);
        assert_eq!(r.chr.len(), 2048);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn nes2_battery_chr_nvram_is_writable_ram() {
        // Same NVRAM header with the battery bit set: still CHR RAM today, and the .sav file carries
        // PRG RAM only. CHR-NVRAM gets its own `<name>.chr.sav` sidecar (`save::chr_save_path`).
        let mut v = nes2_rom(0x70);
        v[6] = 0x02;
        let r = split(&v).unwrap();
        assert!(r.header.battery);
        assert_eq!(r.chr.len(), 8192);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn nes2_chr_rom_ignores_chr_ram_size() {
        let mut v = nes2_rom(0x07);
        v[5] = 1;
        v.resize(16 + 16384 + 8192, 0);
        let r = split(&v).unwrap();
        assert_eq!(r.chr.len(), 8192);
        assert!(!r.chr_is_ram);
    }

    #[test]
    fn ines1_ignores_chr_size_bytes() {
        // Byte 11 is unused in iNES 1.0: the 8 KB CHR-RAM convention is kept even when it is set.
        let mut v = rom(1, 0, 0, 0);
        v[11] = 0x97;
        let r = split(&v).unwrap();
        assert_eq!(r.chr.len(), 8192);
        assert!(r.chr_is_ram);
    }

    #[test]
    fn mirroring_tags_keep_their_saved_numbering() {
        // Both numberings live in the save files of the boards that wrote them, so neither may
        // move.
        assert_eq!(Mirroring::Vertical.to_tag(), 0);
        assert_eq!(Mirroring::Horizontal.to_tag(), 1);
        assert_eq!(Mirroring::Horizontal.to_tag_horizontal_first(), 0);
        assert_eq!(Mirroring::Vertical.to_tag_horizontal_first(), 1);
        let all = [
            Mirroring::Horizontal,
            Mirroring::Vertical,
            Mirroring::SingleScreenLower,
            Mirroring::SingleScreenUpper,
            Mirroring::FourScreen,
        ];
        for m in all {
            assert_eq!(Mirroring::from_tag(m.to_tag()).unwrap(), m);
            assert_eq!(
                Mirroring::from_tag_horizontal_first(m.to_tag_horizontal_first()).unwrap(),
                m
            );
        }
        assert_eq!(
            Mirroring::SingleScreenUpper.to_tag(),
            3,
            "upper above lower is part of the tag"
        );
        assert_eq!(
            Mirroring::from_tag(5),
            Err(crate::state::StateError::BadValue)
        );
        assert_eq!(
            Mirroring::from_tag_horizontal_first(5),
            Err(crate::state::StateError::BadValue)
        );
    }

    #[test]
    fn chr_read_wraps_like_a_modulo() {
        // A power-of-two image takes the mask path; 24 KB takes the `%` fallback. Both wrap.
        for len in [8192usize, 24576] {
            let chr: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
            for index in [0usize, 1, len - 1, len, len + 5, 0x1FFF, 0x4000_0001] {
                assert_eq!(
                    chr_read(&chr, index),
                    chr[index % len],
                    "len {len} index {index}"
                );
            }
        }
        assert_eq!(chr_read(&[], 12345), 0, "no CHR memory reads as open bus");
    }

    /// The default nametable hooks answer with CIRAM itself, mapped by `mirroring`, so the
    /// PPU's fetch path pays one indirect call (`nt_read`) instead of two (`nt_read` + `mirroring`).
    #[test]
    fn the_default_nametable_hooks_map_ciram_by_mirroring() {
        // iNES flag 6 bit 0 set = vertical. The two names are `memory::nametable_index`'s:
        // horizontal mirrors $2000/$2400, vertical mirrors $2000/$2800.
        let mut ciram = [0u8; 4096];
        let mut horizontal = crate::mapper::load(&rom(1, 1, 0x00, 0)).unwrap();
        assert!(
            horizontal.nt_write(0x2000, 0x11, &mut ciram),
            "the default takes the write"
        );
        assert_eq!(
            horizontal.nt_read(0x2400, &ciram),
            Some(0x11),
            "horizontal: $2400 mirrors $2000"
        );
        assert!(horizontal.nt_write(0x2800, 0x22, &mut ciram));
        assert_eq!(
            horizontal.nt_read(0x2C00, &ciram),
            Some(0x22),
            "horizontal: $2C00 mirrors $2800"
        );
        assert_eq!(
            horizontal.nt_read(0x3EFF, &ciram),
            Some(ciram[crate::ppu::memory::nametable_index(0x3EFF, Mirroring::Horizontal)])
        );

        let mut ciram = [0u8; 4096];
        let mut vertical = crate::mapper::load(&rom(1, 1, 0x01, 0)).unwrap();
        assert!(vertical.nt_write(0x2400, 0x33, &mut ciram));
        assert!(vertical.nt_write(0x2000, 0x44, &mut ciram));
        assert_eq!(
            vertical.nt_read(0x2400, &ciram),
            Some(0x33),
            "vertical: $2400 is its own table"
        );
        assert_eq!(
            vertical.nt_read(0x2800, &ciram),
            Some(0x44),
            "vertical: $2800 mirrors $2000"
        );
        assert_eq!(
            vertical.nt_read(0x2C00, &ciram),
            Some(0x33),
            "vertical: $2C00 mirrors $2400"
        );
    }
}
