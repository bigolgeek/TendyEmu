use crate::bus::{RamPowerOn, SysBus, power_on_ram};
use crate::cart::{CartError, ConsoleType, Region, RomData, split};
use crate::console::{ConsoleConfig, Quirks};
use crate::cpu::Cpu;
use crate::mapper::build;
use crate::ppu::Ppu;
use crate::rng::{SeedDomain, derive};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes `Nes::save_state` reserves beyond the previous state's length. A state's length
/// moves by a byte or two between frames (its `Option` fields), so this keeps a snapshot to one
/// allocation; a bigger jump only costs that snapshot a reallocation.
const STATE_LEN_SLACK: usize = 64;

/// The CPU/PPU power-on alignment a run picked: `Exact(p)` pins one of the 4
/// relative phases the CPU and PPU can start in at power-on (0 is the default the
/// baselines pin); `Random` derives the alignment from the run's seed, so repeating
/// a seed replays it exactly. The derivation is
/// `rng::derive(seed, SeedDomain::Align).next_below(4)` — i.e. splitmix64(seed ^ 0x000A_116E) % 4 —
/// where the `0x000A_116E` ("ALIGN") tag keeps this stream disjoint from any other
/// seed-derived stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseChoice {
    /// A pinned alignment: the PPU's initial dot counter is offset by `p` (0..=3).
    Exact(u8),
    /// The alignment comes from the seed.
    Random,
}

impl PhaseChoice {
    /// The resolved alignment (0..=3) this choice and seed produce. Documented
    /// exactly so a different frontend or a replay tool derives the same phase.
    pub fn resolve(self, seed: u64) -> u8 {
        match self {
            PhaseChoice::Exact(phase) => phase,
            // The first step of the alignment stream, folded into 0..=3. The step is
            // `rng`'s splitmix64, so a replay tool can derive the same phase from
            // `SeedDomain::Align`'s tag alone.
            PhaseChoice::Random => derive(seed, SeedDomain::Align).next_below(4) as u8,
        }
    }

    /// The spelling `--align`, `--reset-align`, `align` and the settings overlay use: a pinned
    /// phase as `0`..`3`, or `random`. Adding or renaming a value changes this one function.
    pub fn name(self) -> String {
        match self {
            PhaseChoice::Exact(phase) => phase.to_string(),
            PhaseChoice::Random => "random".to_owned(),
        }
    }

    /// The choice a spelling names, or `None` for anything else. Only `0`..`3` and `random` are
    /// the words the flag and the file offer; the callers own the error text. [`from_header_value`]
    /// takes the whole `u8` range instead, because a save state can pin any dot count.
    ///
    /// [`from_header_value`]: PhaseChoice::from_header_value
    pub fn from_name(name: &str) -> Option<PhaseChoice> {
        match name {
            "0" | "1" | "2" | "3" => name.parse().ok().map(PhaseChoice::Exact),
            "random" => Some(PhaseChoice::Random),
            _ => None,
        }
    }

    /// How a movie header spells this choice: `random`, or the pinned number — the
    /// same values `--align` and `--reset-align` accept.
    pub fn to_header_value(self) -> String {
        self.name()
    }

    /// The choice a header value names: `random` or a decimal `u8`.
    ///
    /// The command line only ever produces 0..=3, but [`Nes::reset_aligned`] accepts any dot
    /// count, so the header carries the whole `u8` range and a configuration built through the
    /// API round-trips exactly instead of being refused.
    pub fn from_header_value(text: &str) -> Result<PhaseChoice, String> {
        if text == "random" {
            return Ok(PhaseChoice::Random);
        }
        text.parse::<u8>()
            .map(PhaseChoice::Exact)
            .map_err(|_| format!("{text:?} is not 0-255 or random"))
    }
}

/// Where the trainer lands in PRG RAM: `$7000` is offset `$1000` of the `$6000`-based window.
const TRAINER_RAM_OFFSET: usize = 0x1000;

/// Every power-on option the frontend exposes plus the seed all
/// the random ones draw from. The default is exactly the behaviour before the
/// struct existed — zeroed work RAM, alignment 0, no mapper randomisation, seed
/// unused — so no flag at all cannot move a baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerOn {
    /// What the 2 KB work RAM held at power-on.
    pub ram: RamPowerOn,
    /// The CPU/PPU power-on alignment.
    pub phase: PhaseChoice,
    /// Boards that override `power_on_random(seed)` start in seed-derived garbage
    /// bank state; MMC1 and MMC3 use the hook.
    pub random_mapper: bool,
    /// The seed RAM randomness, a randomised alignment and a randomised mapper share.
    pub seed: u64,
    /// The seed was generated here (current time in nanoseconds — non-reproducible
    /// across runs) rather than being given with `--seed`; the frontend prints it
    /// on stderr so the run can be repeated.
    pub seed_auto: bool,
    /// The CPU/PPU realignment applied on every reset-button press (#127b): `Exact(0)`
    /// leaves the PPU's phase untouched, which is today's behaviour and the default, so
    /// no flag cannot move a baseline. `Random` draws a fresh phase per reset from the
    /// seed — see [`PowerOn::reset_phase_for`].
    pub reset_phase: PhaseChoice,
    /// Idle post-render scanlines added per frame: `0` is the stock PPU timing. Power-on PPU
    /// timing, like the region — recorded in a movie header, not serialised in a save state.
    pub overclock: u16,
}

impl Default for PowerOn {
    fn default() -> Self {
        PowerOn {
            ram: RamPowerOn::Zero,
            phase: PhaseChoice::Exact(0),
            random_mapper: false,
            seed: 0,
            seed_auto: false,
            reset_phase: PhaseChoice::Exact(0),
            overclock: 0,
        }
    }
}

impl PowerOn {
    /// The PPU realignment in dots for the `reset_index`-th reset-button press since
    /// power-on (#127b), for [`Nes::reset_aligned`].
    ///
    /// `reset_index` is **zero-based**: the first reset after power-on passes `0`, the
    /// next `1`, and so on. It is the caller's counter — the frontend keeps it next to
    /// the machine and restarts it at a fresh power-on (a movie power cycle) — and it is
    /// the only per-reset input, so `(seed, reset_index)` alone reproduces the phase.
    ///
    /// - `Exact(k)` always realigns by `k` dots, so the default `Exact(0)` makes every
    ///   reset byte-identical to [`Nes::reset`].
    /// - `Random` takes the `reset_index + 1`-th draw of this seed's
    ///   [`SeedDomain::ResetAlign`] stream, folded into `0..=3`: a fresh phase per reset,
    ///   and the same phase for the same `(seed, index)` — which is what a movie replay
    ///   needs. The stream is separate from the power-on [`SeedDomain::Align`] one,
    ///   so a reset alignment is independent of the alignment the console started at.
    pub fn reset_phase_for(&self, reset_index: u32) -> u8 {
        match self.reset_phase {
            PhaseChoice::Exact(dots) => dots,
            PhaseChoice::Random => {
                let mut rng = derive(self.seed, SeedDomain::ResetAlign);
                let mut phase = rng.next_below(4) as u8;
                for _ in 0..reset_index {
                    phase = rng.next_below(4) as u8;
                }
                phase
            }
        }
    }
}

/// FM2 header keys a recording uses to remember the power-on options it ran with.
///
/// `nesrust_`-prefixed because FM2 allows unknown keys — FCEUX and BizHawk ignore them — and the
/// format has no names for these. [`PowerOn::to_header`] writes all six (seed, RAM, alignments, mapper, overclock); a header without them
/// (every movie recorded before this) is [`PowerOn::default()`].
pub const HEADER_RAM_INIT: &str = "nesrust_ram_init";
/// See [`HEADER_RAM_INIT`].
pub const HEADER_ALIGN: &str = "nesrust_align";
/// See [`HEADER_RAM_INIT`].
pub const HEADER_RESET_ALIGN: &str = "nesrust_reset_align";
/// See [`HEADER_RAM_INIT`].
pub const HEADER_RANDOM_MAPPER: &str = "nesrust_random_mapper";
/// See [`HEADER_RAM_INIT`].
pub const HEADER_SEED: &str = "nesrust_seed";
/// See [`HEADER_RAM_INIT`]. Idle post-render lines per frame; absent reads as 0.
pub const HEADER_OVERCLOCK: &str = "nesrust_overclock";

/// The `0`/`1` a header uses for a boolean flag.
fn flag_to_header_value(flag: bool) -> &'static str {
    if flag { "1" } else { "0" }
}

/// The boolean a header flag names; a repeated or foreign flag is an error rather than a guess.
fn flag_from_header_value(text: &str) -> Result<bool, String> {
    match text {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(format!("{other:?} is not 0 or 1")),
    }
}

impl PowerOn {
    /// The FM2 header keys recording this power-on configuration: what the work RAM held, the
    /// CPU/PPU alignment, the reset-button realignment, the mapper randomisation and the seed —
    /// every option `--ram-init`, `--align`, `--reset-align`, `--random-mapper` and `--seed`
    /// select.
    ///
    /// `seed_auto` is not recorded on purpose: it says the frontend generated the seed (and
    /// printed it), not how the machine behaves, so a replay — where the seed is always known —
    /// reads back as `false` with the seed itself intact.
    pub fn to_header(&self) -> Vec<(String, String)> {
        vec![
            (HEADER_RAM_INIT.to_owned(), self.ram.name().to_owned()),
            (HEADER_ALIGN.to_owned(), self.phase.to_header_value()),
            (
                HEADER_RESET_ALIGN.to_owned(),
                self.reset_phase.to_header_value(),
            ),
            (
                HEADER_RANDOM_MAPPER.to_owned(),
                flag_to_header_value(self.random_mapper).to_owned(),
            ),
            (HEADER_SEED.to_owned(), self.seed.to_string()),
            (HEADER_OVERCLOCK.to_owned(), self.overclock.to_string()),
        ]
    }

    /// The power-on configuration an FM2 header names.
    ///
    /// A header without these keys is [`PowerOn::default()`] — the movie predates those keys, or
    /// another tool wrote it — and a key that is present but unreadable is an error naming it.
    /// Keys are applied in file order and a repeated one keeps the last value, matching the
    /// command-line parser; unknown keys are ignored.
    pub fn from_header(header: &[(String, String)]) -> Result<PowerOn, String> {
        let mut power_on = PowerOn::default();
        for (key, value) in header {
            match key.as_str() {
                HEADER_RAM_INIT => {
                    power_on.ram = RamPowerOn::from_name(value).ok_or_else(|| {
                        format!("{HEADER_RAM_INIT}: {value:?} is not zero, ones or random")
                    })?;
                }
                HEADER_ALIGN => {
                    power_on.phase = PhaseChoice::from_header_value(value)
                        .map_err(|e| format!("{HEADER_ALIGN}: {e}"))?;
                }
                HEADER_RESET_ALIGN => {
                    power_on.reset_phase = PhaseChoice::from_header_value(value)
                        .map_err(|e| format!("{HEADER_RESET_ALIGN}: {e}"))?;
                }
                HEADER_RANDOM_MAPPER => {
                    power_on.random_mapper = flag_from_header_value(value)
                        .map_err(|e| format!("{HEADER_RANDOM_MAPPER}: {e}"))?;
                }
                HEADER_SEED => {
                    power_on.seed = value
                        .parse::<u64>()
                        .map_err(|_| format!("{HEADER_SEED}: {value:?} is not a decimal u64"))?;
                }
                HEADER_OVERCLOCK => {
                    power_on.overclock = value.parse::<u16>().map_err(|_| {
                        format!("{HEADER_OVERCLOCK}: {value:?} is not a decimal u16")
                    })?;
                }
                _ => {}
            }
        }
        // A movie's seed is always known, so nothing is "auto" here.
        power_on.seed_auto = false;
        Ok(power_on)
    }
}

/// The whole console: CPU plus the system bus (RAM, PPU, APU, cartridge).
pub struct Nes {
    pub cpu: Cpu,
    pub bus: SysBus,
    /// The cartridge's PRG storage should be saved: the iNES battery flag (flags 6 bit 1) is set, the
    /// board is MMC1, whose PRG RAM is battery-backed on most carts and whose dumps often lack the
    /// flag, or the board is GTROM (mapper 111 with CHR-RAM), whose PRG flash *is* the save.
    pub battery: bool,
    /// The cartridge's CHR memory should be saved: the NES 2.0 header declares CHR-NVRAM
    /// (byte 11 high nibble non-zero). The nibble itself says the memory is non-volatile, so this
    /// does not also require the battery flag.
    pub chr_battery: bool,
    /// FNV-1a 64 of the .nes file.
    pub rom_hash: u64,
    /// The hardware quirks this console was built with. Configuration, not emulated
    /// state: it is never saved (`save_state` writes the machine only) and `Quirks::default()` is
    /// exactly the behaviour of the console before the field existed. `Nes::reset` consults
    /// `ppu_reset_on_reset` here; the PPU reads its own copy of the rest.
    pub quirks: Quirks,
    /// Length of the last state `save_state` wrote, which sizes the next one's buffer.
    /// A hint, not machine state: it is never saved, and a stale value only costs a reallocation.
    state_len_hint: AtomicUsize,
}

impl Nes {
    /// Load an iNES image and reset the CPU with all-zero work RAM at the default (phase 0)
    /// CPU/PPU alignment: the emulator default, and what the baselines pin.
    pub fn new(rom: &[u8]) -> Result<Nes, CartError> {
        Nes::build(rom, RamPowerOn::Zero, 0, 0)
    }

    /// Load an iNES image, give the 2KB work RAM its power-on contents (zeros, ones, or the
    /// seed's pseudo-random stream), then reset the CPU. RAM is filled before the reset, so the
    /// reset sequence already reads whatever garbage a real power-on left behind.
    pub fn new_with_power_on(rom: &[u8], mode: RamPowerOn, seed: u64) -> Result<Nes, CartError> {
        Nes::build(rom, mode, seed, 0)
    }

    /// Load an iNES image and reset the CPU at a chosen CPU/PPU power-on alignment: `phase`
    /// (0..=3) offsets the PPU's initial dot counter, selecting one of the 4 relative phases
    /// the CPU and PPU can start in at power-on.
    pub fn new_aligned(rom: &[u8], phase: u8) -> Result<Nes, CartError> {
        Nes::build(rom, RamPowerOn::Zero, 0, phase)
    }

    /// The single constructor behind the three public ones: parse the image (which is where the
    /// game database corrects a known-bad header), then RAM contents, PPU phase, CPU reset.
    fn build(rom: &[u8], mode: RamPowerOn, seed: u64, phase: u8) -> Result<Nes, CartError> {
        Nes::from_rom_data_console(
            split(rom)?,
            crate::state::fnv1a64(rom),
            &ConsoleConfig::new(PowerOn {
                ram: mode,
                seed,
                phase: PhaseChoice::Exact(phase),
                ..PowerOn::default()
            }),
        )
    }

    /// Build the console honouring every power-on option: work-RAM
    /// contents, CPU/PPU alignment, and — when `random_mapper` is set — the board's
    /// seed-randomised power-on bank state. The hook runs on the cartridge BEFORE
    /// `Cpu::reset`, so the reset-vector reads already happen over the randomised
    /// banks, exactly as on hardware.
    ///
    /// The parts of power-on the `&PowerOn` shape does not carry stay the same: the
    /// battery state and video timing come from the (already corrected) header, and
    /// `rom_hash` is the hash of the whole image the data was parsed from; save
    /// states pin it. The frontend parses a ROM once and needs the parsed data for
    /// the movie checksum as well as the machine.
    pub fn from_rom_data_power_on(
        rom: RomData,
        rom_hash: u64,
        power_on: &PowerOn,
    ) -> Result<Nes, CartError> {
        Nes::from_rom_data_console(rom, rom_hash, &ConsoleConfig::new(*power_on))
    }

    /// The power-on steps every console shares, in the order hardware takes them and all before
    /// `Cpu::reset`: PPU alignment and quirks, region, APU power-on, then the work-RAM and the
    /// cartridge/PPU RAM fills. One copy so a new power-on step cannot land in one constructor and
    /// be forgotten in the other.
    fn power_on_bus(bus: &mut SysBus, config: &ConsoleConfig, region: Region) {
        let power_on = &config.power_on;
        bus.ppu = Ppu::new_aligned(power_on.phase.resolve(power_on.seed));
        bus.ppu.quirks = config.quirks.ppu(region);
        // The write warm-up is armed before the reset sequence, so the reset-vector reads and the
        // first instructions run against the gated registers (spec §2.3).
        bus.ppu.warmup = config.quirks.ppu_warmup;
        bus.port1_open_mask = config.quirks.port1_open_bus_mask();
        bus.set_region(region);
        if power_on.overclock != 0 {
            bus.ppu.set_overclock_lines(power_on.overclock);
        }
        // Power-on: the frame counter starts 3 cycles into its sequence, standing in for hardware's
        // virtual `$4017 = $00` write before the first instruction (see `POWER_ON_FRAME_LEAD`). Before
        // `Cpu::reset`, so the reset-vector reads are the first frame-counter clocks.
        bus.apu.power_on();
        power_on_ram(&mut bus.ram, power_on.ram, power_on.seed);
        // Cartridge PRG RAM / CHR-RAM (a no-op for boards without the hook, the FDS included):
        // before the `.sav` load (main.rs, after the console is built), so a battery save
        // overwrites the fill rather than the other way round.
        bus.cart_mut().power_on_ram(power_on.ram, power_on.seed);
        bus.ppu.power_on_ram(power_on.ram, power_on.seed);
    }

    /// Build the console from a parsed cartridge with every power-on option **and** the hardware
    /// quirks. This is the constructor the frontend and the quirk tests use;
    /// `from_rom_data_power_on` is this with default quirks, so a caller that only has options
    /// keeps its signature.
    ///
    /// `ConsoleConfig::default()` (zeroed RAM, alignment 0, no mapper randomisation, seed unused,
    /// default quirks) is byte-for-byte the console `Nes::new` builds, and a default `Quirks`
    /// changes nothing: each quirk is off at its default, and the defaults are the values
    /// the baselines pin.
    pub fn from_rom_data_console(
        rom: RomData,
        rom_hash: u64,
        config: &ConsoleConfig,
    ) -> Result<Nes, CartError> {
        let power_on = &config.power_on;
        // A self-flashed PRG is the save. nesdev "UNROM 512" § Flash ROM Save gates
        // its `$8000-$BFFF` cycle register on the battery bit — submapper 1's "The Battery bit is
        // only set if the game makes use of the self-flashing capability" — so the header flag that
        // is already here covers mapper 30. nesdev "GTROM" § Self-flashing: the GTROM board is
        // self-flashable by construction, so mapper 111 (with CHR-RAM; the number's other board is
        // MMC1, see `mapper::build`) always keeps its flash.
        let battery = rom.header.battery
            || rom.header.mapper == 1
            || (rom.header.mapper == 111 && rom.chr_is_ram);
        let chr_battery = rom.header.chr_nvram_bytes > 0;
        let region = Region::from(rom.header.timing);
        let vs = rom.header.console_type == ConsoleType::VsSystem;
        let trainer = rom.trainer.clone();
        let mapper = rom.header.mapper;
        let cart = build(rom)?;
        let mut bus = SysBus::new(cart);
        Nes::power_on_bus(&mut bus, config, region);
        // a Vs. System ROM's `$4016`/`$4017` upper bits come from the cabinet, so the coin,
        // service and DIP inputs are plugged in here. The switches themselves are the frontend's
        // (a config key) and start at 0; `SysBus::set_vs_dip` fills them in.
        if vs {
            bus.attach_vs_inputs(0);
        }
        if power_on.random_mapper {
            bus.cart_mut().power_on_random(power_on.seed);
        }
        // A trainer is 512 bytes the cartridge copies to $7000-$71FF before the game runs, i.e.
        // offset $1000 of the $6000-based PRG RAM. After the power-on randomisation so the
        // trainer is not scrambled; a battery .sav is loaded later by the frontend and still
        // wins. Boards with under $1200 bytes of PRG RAM have nowhere to put it. MMC5 and FME-7
        // bank their $6000 window (ROM or banked RAM), so offset $1000 is not $7000 there.
        if let Some(trainer) = trainer.filter(|_| !matches!(mapper, 5 | 69)) {
            let mut ram = bus.cart().prg_ram().to_vec();
            if ram.len() >= TRAINER_RAM_OFFSET + trainer.len() {
                ram[TRAINER_RAM_OFFSET..TRAINER_RAM_OFFSET + trainer.len()]
                    .copy_from_slice(&trainer);
                bus.cart_mut().load_prg_ram(&ram);
            }
        }
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        Ok(Nes {
            cpu,
            bus,
            battery,
            chr_battery,
            rom_hash,
            quirks: config.quirks,
            state_len_hint: AtomicUsize::new(0),
        })
    }

    /// Load a Famicom Disk System image. `disk` is the `.fds`/`.qd` file (with or without the
    /// fwNES header; `qd` selects the 65536-byte sides with block CRCs) and `bios` the 8 KB
    /// `disksys.rom` dump — the frontend resolves and validates that file before calling here, so
    /// a missing BIOS is reported to the user with the paths it tried rather than as an emulation
    /// fault. An FDS always runs as NTSC and always has battery-backed save data (the disk itself:
    /// the game writes its progress to the sides, see [`crate::cart::Mapper::battery_data`]).
    pub fn from_fds(disk: &[u8], bios: &[u8], qd: bool) -> Result<Nes, String> {
        Nes::from_fds_power_on(disk, bios, qd, &PowerOn::default())
    }

    /// `from_fds` honouring the power-on options the frontend accepts: the work-RAM
    /// contents and CPU/PPU alignment come from `power_on`. A disk system has no
    /// switchable-bank board state to randomise, so `random_mapper` finds the default
    /// no-op hook here.
    pub fn from_fds_power_on(
        disk: &[u8],
        bios: &[u8],
        qd: bool,
        power_on: &PowerOn,
    ) -> Result<Nes, String> {
        Nes::from_fds_console(disk, bios, qd, &ConsoleConfig::new(*power_on))
    }

    /// `from_fds_power_on` plus the hardware quirks. A disk system is not special-cased:
    /// the same `Quirks` reach the PPU, and the quirks that describe the CPU-side console behaviour
    /// (the reset button, the `$2004` read, the write warm-up) apply to an FDS the same way they do to
    /// a cartridge console. `ppu_reset_on_reset` is honoured by `Nes::reset`, which both machines share.
    pub fn from_fds_console(
        disk: &[u8],
        bios: &[u8],
        qd: bool,
        config: &ConsoleConfig,
    ) -> Result<Nes, String> {
        let image = crate::mapper::fds::parse_image(disk, qd)?;
        let cart = crate::mapper::fds::Fds::new(image, bios)
            .map_err(|e| format!("cannot mount the disk image: {e:?}"))?;
        let mut bus = SysBus::new(Box::new(cart));
        Nes::power_on_bus(&mut bus, config, Region::Ntsc);
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        // A disk system has no CHR-NVRAM (its pattern tables are the PPU's own CHR-RAM), so the
        // CHR sidecar never applies to it.
        Ok(Nes {
            cpu,
            bus,
            battery: true,
            chr_battery: false,
            rom_hash: crate::state::fnv1a64(disk),
            quirks: config.quirks,
            state_len_hint: AtomicUsize::new(0),
        })
    }

    /// Load an NSF file and start it: the tune's own 6502 code runs on a
    /// [`crate::nsf::NsfBoard`] whose driver stub sits in `$4100-$41FF`, so the console plays
    /// itself — reset clears RAM and the APU, calls the tune's `INIT`, and the NMI the driver
    /// installs calls `PLAY` every vblank. `song` is one-based, as a frontend shows it (0
    /// selects the file's own starting song); `INIT` sees it minus one, per the format.
    ///
    /// Video timing comes from the file's own region flags — a PAL-only file runs at PAL speed and
    /// `INIT` sees X = 1, a dual-region file runs as NTSC — and everything else from `config`, the
    /// same power-on options and quirks a cartridge console gets: power-on is `Nes::power_on_bus`,
    /// shared with every other constructor. Rendering stays off: the driver
    /// leaves `$2001` alone and only turns the vblank NMI on. No battery: an NSF carries no save
    /// data.
    ///
    /// Errors are [`crate::nsf::NsfError`]: a file this core cannot honour (no signature, an
    /// unsupported version, expansion audio, a play period that is not the frame rate) is refused
    /// rather than played wrong.
    pub fn from_nsf(
        bytes: &[u8],
        song: u8,
        config: &ConsoleConfig,
    ) -> Result<Nes, crate::nsf::NsfError> {
        let nsf = crate::nsf::Nsf::parse(bytes)?;
        let board = nsf.board(song)?;
        let region = nsf.region();
        let mut bus = SysBus::new(Box::new(board));
        Nes::power_on_bus(&mut bus, config, region);
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        Ok(Nes {
            cpu,
            bus,
            battery: false,
            chr_battery: false,
            rom_hash: crate::state::fnv1a64(bytes),
            quirks: config.quirks,
            state_len_hint: AtomicUsize::new(0),
        })
    }

    /// Video timing the cartridge asks for with a NES 2.0 header: NTSC, PAL or Dendy.
    pub fn region(&self) -> Region {
        self.bus.region
    }

    /// Overclock: insert `n` idle post-render lines (the APU is held through them), giving the CPU
    /// `n` x 341/3 more cycles per frame. Power-on configuration like the region, not serialised.
    pub fn set_overclock_lines(&mut self, n: u16) {
        self.bus.ppu.set_overclock_lines(n);
    }

    /// The console's reset button: silence the APU channels, restart the APU frame counter from its
    /// power-on phase (the `$4017` step mode is kept), drop pending interrupts and restart the CPU from
    /// the reset vector. RAM, cartridge RAM and PPU memory keep their contents, as on hardware.
    ///
    /// This is [`Nes::reset_aligned`]`(0)`: today's behaviour, with the PPU's phase untouched.
    pub fn reset(&mut self) {
        self.reset_aligned(0);
    }

    /// The console's reset button with the CPU/PPU relative phase moved by `extra_dots` PPU dots
    /// first: the PPU clock is advanced `extra_dots` dots (0..=3 is the range the four
    /// power-on phases cover, but any count is a valid realignment) before the CPU restarts, so it
    /// resumes at a different point of the PPU's dot/scanline cycle. Only the PPU runs during the
    /// advance — the CPU and APU do not, since only the relative phase changes.
    ///
    /// This is **realignment, not a PPU reset**: `$2000`/`$2001`/`w`, the `$2007` buffer, VRAM, OAM
    /// and the palette keep their values unless the opt-in `ppu_reset_on_reset` quirk is on,
    /// in which case the PPU takes the reset line's registers back to power-on
    /// first and the advance then counts from the top of the picture. `reset_aligned(0)` on a
    /// default console is byte-identical to [`Nes::reset`]. An NMI edge the advance crosses
    /// is dropped, because the reset sequence drops pending interrupts anyway.
    pub fn reset_aligned(&mut self, extra_dots: u8) {
        // The front-loader PPU half of the reset button, first, so the realignment below counts
        // from the top of the picture (spec §2.1 call site).
        if self.quirks.ppu_reset_on_reset {
            self.bus.ppu.reset();
        }
        if extra_dots > 0 {
            let bus = &mut self.bus;
            let (ppu, cart) = bus.ppu_and_cart();
            ppu.advance_dots(cart, extra_dots);
            // The advance is bookkeeping, not a run of the machine: a vblank it crossed must not
            // leave an NMI raised for the first CPU cycle after the reset.
            bus.ppu.nmi_out = false;
        }
        self.bus.apu.write_register(0x4015, 0x00);
        self.bus.apu.soft_reset();
        self.bus.cancel_nmi();
        self.cpu.nmi_pending = false;
        self.cpu.irq_line = false;
        self.cpu.poll_i = None;
        self.cpu.soft_reset(&mut self.bus);
    }

    /// Run one CPU instruction (or interrupt entry). The bus advances the PPU and APU on every access,
    /// so nothing has to catch up afterwards. Returns the instruction's CPU cycles.
    pub fn step(&mut self) -> u32 {
        self.cpu.step(&mut self.bus)
    }

    /// Run until the PPU reaches the start of vblank (one frame).
    pub fn run_frame(&mut self) {
        self.bus.ppu.frame_ready = false;
        while !self.bus.ppu.frame_ready {
            self.step();
        }
    }

    /// Header (magic, version, rom_hash) then CPU then bus. Taken between instructions.
    ///
    /// The buffer starts at the previous state's length: rewind takes one of these every
    /// frame, and a buffer grown from empty cost 9-10 reallocations per state and kept 10-42% more
    /// heap than the state's length in the rewind ring.
    pub fn save_state(&self) -> Vec<u8> {
        let hint = self.state_len_hint.load(Ordering::Relaxed);
        let mut w = crate::state::StateWriter::with_capacity(hint + STATE_LEN_SLACK);
        for b in crate::state::STATE_MAGIC {
            w.u8(b);
        }
        w.u16(crate::state::STATE_VERSION);
        w.u64(self.rom_hash);
        self.cpu.save_state(&mut w);
        self.bus.save_state(&mut w);
        let state = w.finish();
        self.state_len_hint.store(state.len(), Ordering::Relaxed);
        state
    }

    /// Checks magic/version/rom hash before touching anything, then the body, and undoes a body that
    /// turns out damaged: a truncated or corrupt state leaves the machine exactly as it was. Costs
    /// one extra snapshot per load (the picture buffer and RAMs, roughly 70-100 KB), which is
    /// nothing even when rewinding every frame.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), crate::state::StateError> {
        let mut r = crate::state::StateReader::new(data);
        Self::check_state_header(&mut r, self.rom_hash)?;
        // A body that stops half way has already overwritten the fields it read (the bus loads
        // cart, PPU and APU one after the other). Take the machine's own snapshot and replay it
        // before reporting the error; this build just wrote it, so loading it back cannot fail.
        let backup = self.save_state();
        if let Err(err) = self.load_state_body(&mut r) {
            let mut backup_reader = crate::state::StateReader::new(&backup);
            if Self::check_state_header(&mut backup_reader, self.rom_hash).is_ok() {
                let _ = self.load_state_body(&mut backup_reader);
            }
            return Err(err);
        }
        Ok(())
    }

    /// The header of a save state: magic, layout version and ROM hash. Nothing is written to the
    /// machine, so a state rejected here leaves a running game untouched.
    fn check_state_header(
        r: &mut crate::state::StateReader,
        rom_hash: u64,
    ) -> Result<(), crate::state::StateError> {
        use crate::state::{STATE_MAGIC, STATE_VERSION, StateError};
        for b in STATE_MAGIC {
            if r.u8()? != b {
                return Err(StateError::BadMagic);
            }
        }
        let version = r.u16()?;
        if version != STATE_VERSION {
            return Err(StateError::BadVersion(version));
        }
        if r.u64()? != rom_hash {
            return Err(StateError::RomMismatch);
        }
        Ok(())
    }

    /// The body of a save state, from a reader already past the header: CPU then bus. Can fail half
    /// way through a damaged state.
    fn load_state_body(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        use crate::state::StateError;
        self.cpu.load_state(r)?;
        self.bus.load_state(r)?;
        if r.remaining() != 0 {
            return Err(StateError::LengthMismatch {
                expected: 0,
                found: r.remaining(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal NROM image: 1 PRG bank, 1 CHR bank, everything zero.
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
    fn new_aligned_selects_ppu_phase() {
        // The CPU reset may consume PPU dots; the phase offset must survive it untouched, so
        // compare the resulting dot positions against the default construction.
        let rom = rom(1, 1, 0, 0);
        let base = Nes::new(&rom).unwrap().bus.ppu.dot;
        for phase in 1..=3 {
            let nes = Nes::new_aligned(&rom, phase).unwrap();
            assert_eq!(nes.bus.ppu.dot, base + phase as u16);
        }
    }

    /// An image with `mapper` (low nibble in flags 6 / high in flags 7), a trainer of
    /// `(i % 251) + 1` bytes when `with_trainer`, 2 PRG banks and CHR-RAM.
    fn trainer_rom(mapper: u8, with_trainer: bool) -> Vec<u8> {
        let flags6 = (mapper << 4) | if with_trainer { 0x04 } else { 0 };
        let mut v = vec![
            0x4E,
            0x45,
            0x53,
            0x1A,
            2,
            0,
            flags6,
            mapper & 0xF0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        if with_trainer {
            v.extend((0..512).map(|i| (i % 251) as u8 + 1));
        }
        v.resize(v.len() + 2 * 16384, 0xEA);
        v
    }

    #[test]
    fn trainer_is_copied_to_7000() {
        for mapper in [0u8, 1, 4] {
            let mut nes = Nes::new(&trainer_rom(mapper, true)).unwrap();
            // MMC1 and MMC3 power on with PRG RAM enabled, so $7000 reads straight through.
            for i in 0..512u16 {
                assert_eq!(
                    nes.bus.cart_mut().cpu_read(0x7000 + i),
                    (i % 251) as u8 + 1,
                    "mapper {mapper} +{i:#x}"
                );
            }
            assert_eq!(
                nes.bus.cart_mut().cpu_read(0x6FFF),
                0,
                "mapper {mapper} below"
            );
            assert_eq!(
                nes.bus.cart_mut().cpu_read(0x7200),
                0,
                "mapper {mapper} above"
            );
        }
    }

    #[test]
    fn no_trainer_leaves_7000_untouched() {
        for mapper in [0u8, 1, 4] {
            let mut nes = Nes::new(&trainer_rom(mapper, false)).unwrap();
            assert_eq!(nes.bus.cart_mut().cpu_read(0x7000), 0, "mapper {mapper}");
        }
    }

    #[test]
    fn trainer_is_skipped_without_enough_prg_ram() {
        // NES 2.0, no PRG RAM at all: the trainer has nowhere to go and construction still works.
        let mut v = trainer_rom(0, true);
        v[7] = 0x08;
        assert!(Nes::new(&v).is_ok());
    }

    #[test]
    fn new_delegates_with_phase_zero() {
        let rom = rom(1, 1, 0, 0);
        assert_eq!(
            Nes::new(&rom).unwrap().bus.ppu.dot,
            Nes::new_aligned(&rom, 0).unwrap().bus.ppu.dot
        );
    }

    /// `name` / `from_name` are the one spelling table `--align`, `--reset-align` and the
    /// file read: `0`..`3` and `random`, and nothing else.
    #[test]
    fn phase_choice_names_round_trip() {
        for phase in 0u8..4 {
            let choice = PhaseChoice::Exact(phase);
            assert_eq!(
                PhaseChoice::from_name(&choice.name()),
                Some(choice),
                "{}",
                choice.name()
            );
        }
        assert_eq!(PhaseChoice::from_name("random"), Some(PhaseChoice::Random));
        assert_eq!(PhaseChoice::Random.name(), "random");
        // Only the flag's own range: a hand-built `Exact` can name more, but the words do not.
        for other in ["4", "255", "-1", "Random", "", " 0"] {
            assert_eq!(PhaseChoice::from_name(other), None, "{other:?}");
        }
    }

    /// Build from `rom` with `power_on`, run `frames` frames, return the save state.
    fn run_state(rom: &[u8], power_on: &PowerOn, frames: usize) -> Vec<u8> {
        let mut nes =
            Nes::from_rom_data_power_on(split(rom).unwrap(), crate::state::fnv1a64(rom), power_on)
                .unwrap();
        for _ in 0..frames {
            nes.run_frame();
        }
        nes.save_state()
    }

    fn phase_choice_test() {
        // splitmix64(seed ^ 0x000A_116E) % 4, documented on PhaseChoice, spelled out:
        let seed: u64 = 42;
        let state = seed ^ 0x000A_116E;
        let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let expected = ((z ^ (z >> 31)) % 4) as u8;
        assert_eq!(PhaseChoice::Random.resolve(seed), expected);
        for phase in 0u8..4 {
            assert_eq!(PhaseChoice::Exact(phase).resolve(seed), phase);
        }
    }

    #[test]
    fn default_power_on_equals_the_plain_constructor() {
        let rom = rom(1, 1, 0, 0);
        let mut plain = Nes::new(&rom).unwrap();
        for _ in 0..2 {
            plain.run_frame();
        }
        assert_eq!(run_state(&rom, &PowerOn::default(), 2), plain.save_state());
    }

    /// #127b: the reset-button phase is a function of `(seed, reset index)` alone — the same
    /// pair always gives the same phase, the phase stays in `0..=3`, and the default `PowerOn`
    /// never moves it (so the default reset path is untouched). The draws are spelled out
    /// against the `ResetAlign` stream so a replay tool can reproduce them without this code.
    #[test]
    fn reset_phase_for_is_a_function_of_seed_and_index() {
        // Default: no realignment on any reset, however many have happened.
        let default = PowerOn::default();
        assert_eq!(default.reset_phase, PhaseChoice::Exact(0));
        assert!((0..16).all(|n| default.reset_phase_for(n) == 0));

        // `Exact(k)` is the same phase on every reset.
        let exact = PowerOn {
            reset_phase: PhaseChoice::Exact(3),
            ..PowerOn::default()
        };
        assert!((0..8).all(|n| exact.reset_phase_for(n) == 3));

        // Random: a fresh draw per reset, reproducible from the seed and index.
        let random = PowerOn {
            reset_phase: PhaseChoice::Random,
            seed: 42,
            ..PowerOn::default()
        };
        let phases: Vec<u8> = (0..16).map(|n| random.reset_phase_for(n)).collect();
        assert_eq!(
            phases,
            (0..16)
                .map(|n| random.reset_phase_for(n))
                .collect::<Vec<u8>>(),
            "same (seed, index) must give the same phase"
        );
        let distinct: std::collections::BTreeSet<u8> = phases.iter().copied().collect();
        assert!(
            distinct.len() >= 2,
            "16 resets landed on one phase: {phases:?}"
        );
        assert!(
            phases.iter().all(|&p| p < 4),
            "phases must stay in 0..=3: {phases:?}"
        );
        // Reset 0 is the stream's first draw, reset n its n-th after that.
        let mut rng = derive(42, SeedDomain::ResetAlign);
        for (n, &phase) in phases.iter().enumerate() {
            assert_eq!(phase, rng.next_below(4) as u8, "reset index {n}");
        }

        // A different seed is a different stream (not merely a shifted one).
        let other = PowerOn {
            reset_phase: PhaseChoice::Random,
            seed: 43,
            ..PowerOn::default()
        };
        assert_ne!(
            phases,
            (0..16)
                .map(|n| other.reset_phase_for(n))
                .collect::<Vec<u8>>()
        );
    }

    /// #127b: `reset_phase` is a reset-button option only — it must not reach construction.
    #[test]
    fn reset_phase_does_not_change_power_on() {
        let rom = rom(1, 1, 0, 0);
        for reset_phase in [PhaseChoice::Exact(2), PhaseChoice::Random] {
            let power_on = PowerOn {
                reset_phase,
                seed: 42,
                seed_auto: true,
                ..PowerOn::default()
            };
            assert_eq!(
                run_state(&rom, &power_on, 2),
                run_state(&rom, &PowerOn::default(), 2),
                "reset_phase {reset_phase:?} moved the power-on state"
            );
        }
    }

    #[test]
    fn same_power_on_and_seed_replay_identically() {
        // A preset with every random option on: RAM contents, alignment and mapper
        // bank state are all seed-derived. Same seed twice must land on byte-identical
        // state after the same frames.
        let rom = rom(1, 1, 0x10, 0);
        let preset = PowerOn {
            ram: RamPowerOn::Random,
            phase: PhaseChoice::Random,
            random_mapper: true,
            seed: 42,
            seed_auto: true,
            reset_phase: PhaseChoice::Random,
            overclock: 0,
        };
        let first = run_state(&rom, &preset, 2);
        let second = run_state(&rom, &preset, 2);
        assert_eq!(first, second);
        phase_choice_test();
    }

    #[test]
    fn seed_changing_changes_the_power_on_state() {
        let rom = rom(1, 1, 0, 0);
        let a = Nes::from_rom_data_power_on(
            split(&rom).unwrap(),
            crate::state::fnv1a64(&rom),
            &PowerOn {
                ram: RamPowerOn::Random,
                seed: 42,
                ..PowerOn::default()
            },
        )
        .unwrap();
        let b = Nes::from_rom_data_power_on(
            split(&rom).unwrap(),
            crate::state::fnv1a64(&rom),
            &PowerOn {
                ram: RamPowerOn::Random,
                seed: 43,
                ..PowerOn::default()
            },
        )
        .unwrap();
        assert_ne!(a.bus.ram, b.bus.ram);
    }

    #[test]
    fn random_mapper_wires_the_power_on_random_hook() {
        // MMC1 oracle (the mapper's own power_on_random test): seed 42 leaves
        // shift = 42 & 0x1F and control = (42 >> 8) & 0x1F, while a plain power-on
        // starts shift = 0, control = 0x0C. With zero RAM and the same alignment
        // everything else in the saved state matches, so the states differ exactly
        // when the hook ran — and the hook sits before `Cpu::reset` in
        // `from_rom_data_power_on`, so the reset-vector reads happen over the
        // randomised banks.
        let rom = rom(1, 1, 0x10, 0);
        let default_run = run_state(&rom, &PowerOn::default(), 2);
        let random_run = run_state(
            &rom,
            &PowerOn {
                random_mapper: true,
                seed: 42,
                ..PowerOn::default()
            },
            2,
        );
        assert_ne!(default_run, random_run);
    }

    // -----------------------------------------------------------------------------------------
    // The power-on options as FM2 header keys.
    // -----------------------------------------------------------------------------------------

    /// Every option combination the header has to carry: all three RAM modes, both alignment
    /// choices of both phases, both mapper flags and a seed at each edge of the range.
    fn power_on_matrix() -> Vec<PowerOn> {
        let mut all = Vec::new();
        for ram in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            for phase in [
                PhaseChoice::Exact(0),
                PhaseChoice::Exact(1),
                PhaseChoice::Exact(3),
                PhaseChoice::Exact(7),
                PhaseChoice::Random,
            ] {
                for reset_phase in [
                    PhaseChoice::Exact(0),
                    PhaseChoice::Exact(2),
                    PhaseChoice::Random,
                ] {
                    for random_mapper in [false, true] {
                        for seed in [0, 1, u64::MAX] {
                            all.push(PowerOn {
                                ram,
                                phase,
                                random_mapper,
                                seed,
                                seed_auto: false,
                                reset_phase,
                                overclock: 0,
                            });
                        }
                    }
                }
            }
        }
        all
    }

    #[test]
    fn every_power_on_combination_round_trips_through_the_header() {
        let matrix = power_on_matrix();
        // 3 RAM modes x 5 alignments x 3 reset alignments x 2 mapper flags x 3 seeds.
        assert_eq!(matrix.len(), 270);
        for power_on in matrix {
            let header = power_on.to_header();
            assert_eq!(
                PowerOn::from_header(&header),
                Ok(power_on),
                "{power_on:?} -> {header:?}"
            );
        }
    }

    #[test]
    fn a_header_without_the_keys_is_the_default_power_on() {
        // Back-compat: every movie recorded before those keys existed says nothing about the console.
        assert_eq!(PowerOn::from_header(&[]), Ok(PowerOn::default()));
        let foreign = vec![
            ("version".to_owned(), "3".to_owned()),
            ("palFlag".to_owned(), "0".to_owned()),
        ];
        assert_eq!(PowerOn::from_header(&foreign), Ok(PowerOn::default()));
        // A partial header fills the rest in from the default, so a build that wrote only some
        // keys still replays.
        let partial = vec![(HEADER_SEED.to_owned(), "9".to_owned())];
        assert_eq!(
            PowerOn::from_header(&partial),
            Ok(PowerOn {
                seed: 9,
                ..PowerOn::default()
            })
        );
    }

    #[test]
    fn the_header_spells_the_options_like_the_command_line() {
        let power_on = PowerOn {
            ram: RamPowerOn::Random,
            phase: PhaseChoice::Random,
            reset_phase: PhaseChoice::Exact(2),
            random_mapper: true,
            seed: 7,
            seed_auto: false,
            overclock: 0,
        };
        let header = power_on.to_header();
        let value = |key: &str| {
            header
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing {key}"))
        };
        assert_eq!(value(HEADER_RAM_INIT), "random");
        assert_eq!(value(HEADER_ALIGN), "random");
        assert_eq!(value(HEADER_RESET_ALIGN), "2");
        assert_eq!(value(HEADER_RANDOM_MAPPER), "1");
        assert_eq!(value(HEADER_SEED), "7");
        assert_eq!(value(HEADER_OVERCLOCK), "0");
        // Every key is namespaced, and the default console's keys are all present and explicit.
        let defaults = PowerOn::default().to_header();
        for (key, _) in &defaults {
            assert!(key.starts_with("nesrust_"), "{key} is not namespaced");
        }
        assert_eq!(defaults.len(), 6);
        assert_eq!(
            defaults.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(),
            vec!["zero", "0", "0", "0", "0", "0"]
        );
    }

    #[test]
    fn seed_auto_is_not_recorded() {
        // `seed_auto` says the frontend generated the seed; a replay always knows it. The emulated
        // configuration is what the header carries, so the two spellings are identical.
        let generated = PowerOn {
            ram: RamPowerOn::Random,
            seed: 5,
            seed_auto: true,
            ..PowerOn::default()
        };
        let explicit = PowerOn {
            seed_auto: false,
            ..generated
        };
        assert_eq!(generated.to_header(), explicit.to_header());
        let read = PowerOn::from_header(&generated.to_header()).unwrap();
        assert!(!read.seed_auto, "a replayed seed is never auto-generated");
        assert_eq!(read.seed, 5);
    }

    #[test]
    fn a_repeated_key_keeps_the_last_value_and_unknown_keys_are_ignored() {
        let header = vec![
            (HEADER_SEED.to_owned(), "1".to_owned()),
            ("nesrust_future_option".to_owned(), "on".to_owned()),
            (HEADER_SEED.to_owned(), "2".to_owned()),
        ];
        assert_eq!(
            PowerOn::from_header(&header),
            Ok(PowerOn {
                seed: 2,
                ..PowerOn::default()
            })
        );
    }

    #[test]
    fn an_unreadable_header_value_is_an_error_naming_its_key() {
        for (key, value) in [
            (HEADER_RAM_INIT, "maybe"),
            (HEADER_ALIGN, "4x"),
            (HEADER_ALIGN, "random "),
            (HEADER_RESET_ALIGN, "-1"),
            (HEADER_RANDOM_MAPPER, "yes"),
            (HEADER_SEED, "0x10"),
            (HEADER_SEED, "-1"),
        ] {
            let header = vec![(key.to_owned(), value.to_owned())];
            let err = PowerOn::from_header(&header).expect_err("a bad value must be refused");
            assert!(err.contains(key), "{key}: the error names the key: {err}");
            assert!(
                err.contains(value),
                "{key}: the error names the value: {err}"
            );
        }
    }
}
