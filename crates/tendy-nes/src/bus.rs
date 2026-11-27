use crate::apu::Apu;
use crate::apu::dmc::DmaKind;
use crate::cart::{CartHooks, Mapper, Region};
use crate::cdl::{self, Cdl};
use crate::cheats::RawRamCheats;
use crate::controller::{Controller, FourScore, Paddle, PowerPad, Zapper};
use crate::cpu::Bus;
use crate::events::{BusEvent, EventKind, EventLog};
use crate::expansion::ExpansionDevice;
use crate::ppu::{FIRST_WRITE_GLITCH_DOT, Ppu};
use crate::rng::{SeedDomain, derive};

/// What the 2KB work RAM holds when the console is switched on. A real console's RAM is
/// garbage: whatever the cartridge before it left behind, never a clean zeroed page. Zero is the
/// emulator default (and what every baseline test pins), Ones is the other extreme, Random is
/// pseudo-random contents deterministic from a seed, so a given seed always replays the same page
/// and tests stay reproducible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RamPowerOn {
    #[default]
    Zero,
    Ones,
    Random,
}

impl RamPowerOn {
    /// The spelling `--ram-init`, `ram_init` and the settings overlay use.
    pub fn name(self) -> &'static str {
        match self {
            RamPowerOn::Zero => "zero",
            RamPowerOn::Ones => "ones",
            RamPowerOn::Random => "random",
        }
    }

    /// The mode a spelling names, or `None` for anything else. The callers own the error text,
    /// because the flag names a flag and the file names a line.
    pub fn from_name(name: &str) -> Option<RamPowerOn> {
        match name {
            "zero" => Some(RamPowerOn::Zero),
            "ones" => Some(RamPowerOn::Ones),
            "random" => Some(RamPowerOn::Random),
            _ => None,
        }
    }
}

/// Fill `ram` the way a power-on would have left it. Zero -> 0x00, Ones -> 0xFF, Random -> the
/// seeded splitmix64 stream (`rng::derive(seed, SeedDomain::Ram)`), in the page's byte shape
/// [`SplitMix64::fill_bytes`](crate::rng::SplitMix64::fill_bytes) defines: eight bytes per 64-bit
/// step, least-significant byte first. Deterministic in `seed`: same seed, same bytes. No
/// dependencies, no allocator, no RNG tables.
pub fn power_on_ram(ram: &mut [u8; 0x0800], mode: RamPowerOn, seed: u64) {
    match mode {
        RamPowerOn::Zero => *ram = [0x00; 0x0800],
        RamPowerOn::Ones => *ram = [0xFF; 0x0800],
        // `SeedDomain::Ram` is the raw seed, so this is byte-for-byte the page the hand-rolled
        // copy here produced before the module existed.
        RamPowerOn::Random => derive(seed, SeedDomain::Ram).fill_bytes(ram),
    }
}

/// What a Vs. System cabinet drives onto the upper bits of the `$4016`/`$4017` reads, from
/// the two bit tables on nesdev "Vs. System":
///
/// ```text
/// $4016: PCCD DS0B   $4017: DDDD DD0B
/// ```
///
/// Bit 0 of each port is the stick, which the controller port answers and this struct leaves
/// alone, and bit 1 reads 0 on both. The cabinet adds the coin switches (`$4016` bits 5 and 6),
/// the service button (bit 2), DIP switches 1 and 2 (`$4016` bits 3 and 4) and DIP switches 8 down
/// to 3 (`$4017` bits 7 to 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VsInputs {
    /// DIP switches 1 to 8 as one byte, bit 0 = switch 1.
    pub dip: u8,
    /// The coin acceptor contact switches as held now, bit 0 = slot 1 (`$4016` bit 5) and bit 1 =
    /// slot 2 (bit 6). The slots read separately - a game that sees both grants two credits - so
    /// they are never coalesced (nesdev "Vs. System").
    pub coins: u8,
    /// The service button (`$4016` bit 2), which most games accept as a credit.
    pub service: bool,
}

impl VsInputs {
    /// The port byte the cabinet drives for `port` (0 = `$4016`, 1 = `$4017`), keeping only bit 0
    /// of `stick`, which the controller port supplied.
    fn port_bits(&self, port: usize, stick: u8) -> u8 {
        let stick = stick & 0x01;
        if port == 0 {
            stick
                | ((self.dip & 0x03) << 3)
                | (u8::from(self.service) << 2)
                | ((self.coins & 0x03) << 5)
        } else {
            stick | (self.dip & 0xFC)
        }
    }
}

/// The CPU's view of the NES: RAM, PPU registers, APU registers, joypads, and the cartridge.
/// Every CPU read or write first runs one CPU cycle of the rest of the system (3 PPU dots, or 4 on every
/// fifth PAL cycle, and one APU clock), so the PPU and APU stay in step with each bus access.
pub struct SysBus {
    pub ram: [u8; 0x0800],
    /// The cartridge. Private because the per-CPU-cycle hook flags below are cached from it: a
    /// direct assignment would leave them stale, so a swap goes through [`SysBus::set_cart`] and
    /// everything else reaches the board through [`SysBus::cart`] / [`SysBus::cart_mut`].
    cart: Box<dyn Mapper>,
    /// Which per-CPU-cycle hooks the cartridge implements, read from the board when it is set
    /// ([`SysBus::new`], [`SysBus::set_cart`]). `clock` runs once per CPU cycle, so a hook the board
    /// leaves at its default is not called through the vtable at all.
    cart_hooks: CartHooks,
    /// Whether the cartridge has expansion audio, taken from `expansion_audio_name()`: all six
    /// boards that implement `expansion_audio` name their chip, and every other board returns the 0.0
    /// default. `Apu::expansion_pulse` starts at 0.0 and is only ever written here, so a skipped call
    /// leaves it at exactly the level the call would have produced.
    cart_audio: bool,
    pub ppu: Ppu,
    pub apu: Apu,
    /// Video timing of the machine (NTSC unless the NES 2.0 header asks for PAL or Dendy).
    pub region: Region,
    /// Joypads on $4016 (port 1) and $4017 (port 2).
    pub pads: [Controller; 2],
    /// Vaus (Arkanoid) paddles: `Some` on a port that has a paddle plugged in, which then answers
    /// that port's reads instead of the joypad there. The device set is configuration, not
    /// emulated state: the frontend attaches one, and a save state records only the knob and the
    /// button the attached device holds.
    pub paddles: [Option<Paddle>; 2],
    /// Zapper light guns: `Some` on a port that has a gun plugged in, which then answers that
    /// port's reads instead of the joypad there. Attaching one is configuration, not emulated
    /// state: the frontend decides it, and a save state records only the aim and trigger the
    /// attached gun holds. A port carries one device, and a paddle is asked first, so attaching
    /// both to one port leaves the gun silent.
    pub zappers: [Option<Zapper>; 2],
    /// A Four Score adapter: `Some` when plugged into both ports, adding pads 3 and 4 and the
    /// 24-bit report with its signature byte to each joypad port. A paddle or Zapper on a port
    /// takes precedence there. Configuration like the other devices; a save state records its pads
    /// and read counters.
    pub four_score: Option<FourScore>,
    /// Power Pads: `Some` on a port that has a mat plugged in, which then answers that port's
    /// reads on D3/D4 instead of the joypad there. Configuration like the other devices; a save
    /// state records the buttons and read position the attached mat holds. The frontend refuses a
    /// mat beside a Zapper, a paddle or the Four Score on its port, so a port carries one device.
    pub power_pads: [Option<PowerPad>; 2],
    /// The Vs. System's coin slots, service button and DIP switches: `Some` on a Vs. board,
    /// whose `$4016`/`$4017` upper bits then come from the cabinet instead of the open bus. Not
    /// emulation state and not in a save state: a DIP switch is a startup setting and a coin reads
    /// high only while its key is held, so the layout and `STATE_VERSION` stay where they are.
    pub vs: Option<VsInputs>,
    /// The expansion-port device: its D1 (`$4016`) and D1-D4 (`$4017`) bits are ORed into
    /// the port reads and it sees the `$4016` OUT bits. `None` by default, which leaves every read
    /// unchanged. Configuration; a save state records its kind and payload.
    pub expansion: ExpansionDevice,
    /// Last value seen on the data bus; returned by reads of unmapped addresses.
    pub open_bus: u8,
    /// Set when the CPU read `$4016` or `$4017` since the frontend last cleared it (lag
    /// counter). Transient per-frame state, not part of save states.
    pub frame_pads_polled: bool,
    /// CPU cycles the last OAM DMA ($4014) took: 513, or 514 when it started on an odd cycle.
    pub dma_cycles: u32,
    /// Page(s) written to $4014; queued OAM DMA requests until the CPU's read cycle.
    pub oam_page: Option<u8>,
    /// Second $4014 request made before the first DMA ran; chained after it.
    pub oam_next: Option<u8>,
    /// CPU cycles since power-on.
    pub cycles: u64,
    /// An NMI edge from the PPU that the CPU has not taken yet.
    pub nmi_latch: bool,
    /// `nmi_latch` as it was before the current cycle: what the CPU sees if this is an instruction's last cycle.
    pub nmi_before: bool,
    /// IRQ line (APU frame counter or DMC, or the cartridge) after the current cycle.
    pub irq_level: bool,
    /// `irq_level` as it was before the current cycle.
    pub irq_before: bool,
    /// Set to true when a DMA cycle (OAM or DMC) just completed; cleared on the next CPU cycle.
    /// Used to detect unstable opcode behavior (e.g., SHx drops `& (H+1)` term when DMA lands on previous cycle).
    pub dma_just_occurred: bool,
    /// Buffered $4016 write: (value, CPU cycles until it reaches the pads).
    strobe_pending: Option<(u8, u8)>,
    /// CPU cycle of the last `$2007` read the PPU actually acted on, or `u64::MAX` for none yet.
    /// A `$2007` read on the very next cycle is not performed - see `access_read`.
    pub last_2007: u64,
    /// Joypad /OE lines asserted on the current cycle (bit 0: $4016, bit 1: $4017). Only a DMA run
    /// tracks this: outside DMA no two adjacent reads hold the same port's /OE. Not part of save states.
    joy_oe: u8,
    /// Joypad /OE lines held from the previous DMA cycle: a read on a held line does not clock the pad.
    joy_hold: u8,
    /// Open-bus bits of a `$4016` read (`Quirks::port1_open_bus_mask`); `$4017` is always `0xE0`.
    /// Configuration, not state: it is set by the `Nes` constructors and never saved.
    pub port1_open_mask: u8,
    /// Event viewer log; disabled by default and not part of save states.
    pub events: EventLog,
    /// Game Genie codes the frontend loaded: each read of `$8000-$FFFF` that comes
    /// through `access_read` — the CPU's own reads and every DMA fetch of that range,
    /// DMC sample bytes included, because that is what the real cartridge connector sees —
    /// is walked through `Code::apply`. Empty by default (zero hot-path cost: the one
    /// `is_empty()` branch short-circuits), and NOT part of save states: codes are frontend
    /// configuration, not emulation state, so the state format is unchanged and it is the
    /// frontend's job to call [`set_genie_codes`](SysBus::set_genie_codes) again on every
    /// machine it rebuilds (like `events` and `cheats`). Debug peeks and the tracer go through
    /// the same substitution (`cart_read`/`cart_peek`), so they show the bytes the CPU runs.
    genie: Vec<crate::genie::Code>,
    /// Frozen raw-RAM addresses. Empty by default (zero hot-path cost: one `false`
    /// branch in `access_write`), and not part of save states, like `events`. Private: frontends
    /// drive the engine through the `cheat_*` methods below and read state back through
    /// `cheats_armed` / `frozen_count` / `cheat_snapshot` / `cheat_apply`.
    cheats: RawRamCheats,
    /// Code/data log. `None` by default, which costs one `is_some()` branch in `read` and
    /// nothing else. Not part of save states, like `events`; the frontend turns it on with
    /// [`SysBus::enable_cdl`].
    cdl: Option<Box<Cdl>>,
    /// Set by `note_dummy`: the next read is an instruction fetch or a dummy read, so it does not
    /// mark data. Only ever set while `cdl` is `Some`.
    cdl_skip: bool,
}

/// State of one DMA run: the CPU read at `addr` is stolen by a DMC fetch and/or an OAM copy.
struct DmaRun {
    addr: u16,
    halted: bool,
    /// DMC unit stage: 0 idle, 1 halt cycle due, 2 dummy cycle due, 3 waiting for a get cycle.
    dmc: u8,
    /// A chained OAM DMA's halt cycle is due.
    oam_setup: bool,
    oam: Option<u8>,
    count: u16,
    oam_val: Option<u8>,
}

impl SysBus {
    pub fn new(cart: Box<dyn Mapper>) -> SysBus {
        let (cart_hooks, cart_audio) = Self::cart_flags(cart.as_ref());
        SysBus {
            ram: [0; 0x0800],
            cart,
            cart_hooks,
            cart_audio,
            ppu: Ppu::new(),
            apu: Apu::new(),
            region: Region::Ntsc,
            pads: [Controller::default(), Controller::default()],
            paddles: [None, None],
            zappers: [None, None],
            four_score: None,
            power_pads: [None, None],
            vs: None,
            expansion: ExpansionDevice::None,
            open_bus: 0,
            frame_pads_polled: true,
            dma_cycles: 0,
            oam_page: None,
            oam_next: None,
            cycles: 0,
            nmi_latch: false,
            nmi_before: false,
            irq_level: false,
            irq_before: false,
            dma_just_occurred: false,
            strobe_pending: None,
            last_2007: u64::MAX,
            joy_oe: 0,
            joy_hold: 0,
            port1_open_mask: 0xE0,
            events: EventLog::default(),
            genie: Vec::new(),
            cheats: RawRamCheats::new(),
            cdl: None,
            cdl_skip: false,
        }
    }

    /// The flags `clock` reads instead of calling the board: the per-cycle hooks it
    /// implements and whether it has expansion audio at all.
    fn cart_flags(cart: &dyn Mapper) -> (CartHooks, bool) {
        (cart.hooks(), cart.expansion_audio_name().is_some())
    }

    /// Replace the cartridge, re-reading the per-cycle hook flags cached from it. The `cart`
    /// field is private, so this is the only way to swap a board: a direct assignment would leave
    /// `cart_hooks` and `cart_audio` describing the board that was removed.
    ///
    /// A swap from an expansion-audio board to one *without* audio clears the mix immediately.
    /// Two boards *with* audio are the residual: the level the old chip left stays in the
    /// mix until the next mixer run asks the new board for its own (`expansion_due`: one CPU cycle
    /// in about ten at 44.1 kHz), because nothing else writes the level. `tests/expansion_audio.rs`
    /// pins that bound.
    pub fn set_cart(&mut self, cart: Box<dyn Mapper>) {
        (self.cart_hooks, self.cart_audio) = Self::cart_flags(cart.as_ref());
        if !self.cart_audio {
            // the level the board being removed left behind is not the new board's. Nothing
            // writes `expansion_pulse` again for a board without expansion audio (the fetch is
            // skipped), so without this the old chip's last value stays in the mix for the rest of
            // the session.
            self.apu.expansion_pulse = 0.0;
        }
        self.cart = cart;
    }

    /// The cartridge, for the board queries that take `&self` (`mirroring`, `prg_ram`, the hook
    /// flags, expansion audio). A swap or a board call that writes state needs [`SysBus::cart_mut`]
    /// or [`SysBus::set_cart`].
    pub fn cart(&self) -> &dyn Mapper {
        self.cart.as_ref()
    }

    /// The cartridge, for a board call that writes state (`cpu_read`, `ppu_write`, the loaders).
    /// Does not touch the cached hook flags; only [`SysBus::set_cart`] replaces the board itself.
    pub fn cart_mut(&mut self) -> &mut dyn Mapper {
        self.cart.as_mut()
    }

    /// The PPU and the cartridge together: `Ppu::tick` and `Ppu::advance_dots` need both at once and
    /// the two borrows must coexist, which the single-field accessors cannot give.
    pub fn ppu_and_cart(&mut self) -> (&mut Ppu, &mut dyn Mapper) {
        (&mut self.ppu, self.cart.as_mut())
    }

    /// Give the whole machine a region's timing: PPU frame geometry and the APU's rate tables and CPU
    /// clock. Called once at power-on from the ROM header; NTSC is the default.
    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.ppu.set_region(region);
        self.apu.set_region(region);
    }

    /// Appends an event stamped with the current PPU position. Callers check `events.enabled` first.
    #[cold]
    #[inline(never)]
    fn record(&mut self, kind: EventKind, addr: u16, val: u8) {
        let e = BusEvent {
            frame: self.ppu.frame,
            scanline: self.ppu.scanline,
            dot: self.ppu.dot,
            cycle: self.cycles,
            kind,
            addr,
            val,
        };
        self.events.push(e);
    }

    /// One CPU cycle of everything except the CPU: 3 PPU dots (4 on every fifth PAL cycle) and one APU
    /// clock, then the interrupt lines.
    fn clock(&mut self) {
        self.nmi_before = self.nmi_latch;
        self.irq_before = self.irq_level;
        // OAM decay is measured on the emulated CPU cycle counter, which is itself serialised
        // The PPU does not own the clock (spec §3.1), so it is handed this cycle's
        // value before it ticks. Gated, so the default path does not even store the field.
        if self.ppu.quirks.oam_decay {
            self.ppu.cpu_cycle = self.cycles;
        }
        // A cart IRQ that rises on the first dot of the cycle is already on the line when the CPU polls at
        // the end of it (the same rule NMI edges follow below); the poll would otherwise see it one CPU
        // cycle late. `irq_before` itself is updated after the event check so the event log still stamps
        // the rise on this cycle.
        let mut irq_early = false;
        // NTSC and Dendy run exactly 3 PPU dots per CPU cycle. The PAL PPU divides the master clock by 5
        // and its CPU by 16, so a PAL CPU cycle is 3.2 dots: a 4th dot on every fifth cycle (0-4, 5-9, ...).
        // That dot runs after the loop (its index, 3, is past NMI_EARLY_DOTS anyway) so the NTSC hot path
        // keeps a literal three-iteration loop the compiler can unroll.
        for dot in 0..3 {
            self.ppu.tick(self.cart.as_mut());
            if dot < Self::NMI_EARLY_DOTS && self.cart_hooks.irq && self.cart.irq_pending() {
                irq_early = true;
            }
            if self.ppu.nmi_out {
                self.ppu.nmi_out = false;
                if self.events.enabled && !self.nmi_latch {
                    self.record(EventKind::Nmi, 0, 0);
                }
                self.nmi_latch = true;
                // An edge on the first dot of the cycle is already visible to this cycle's interrupt poll.
                if dot < Self::NMI_EARLY_DOTS {
                    self.nmi_before = true;
                }
            }
        }
        if self.pal_fourth_dot() {
            self.pal_fourth_dot_tick();
        }
        if let Some((val, delay)) = self.strobe_pending {
            if delay <= 1 {
                self.strobe_devices(val);
                self.strobe_pending = None;
            } else {
                self.strobe_pending = Some((val, delay - 1));
            }
        }
        // `Apu` reads `expansion_pulse` only inside the mixer, which runs on about one CPU
        // cycle in ten, so the board is asked for its level only on those cycles. `expansion_due`
        // asks `mix`'s own question, so the value it reads is the one the old per-cycle fetch had
        // put there.
        if self.cart_audio && self.apu.expansion_due() {
            self.apu.expansion_pulse = self.cart.expansion_audio();
        }
        if !self.ppu.in_overclock_lines() {
            self.apu.clock();
        }
        if self.cart_hooks.cpu_clock {
            self.cart.cpu_clock();
        }
        self.irq_level = self.apu.irq() || (self.cart_hooks.irq && self.cart.irq_pending());
        if self.events.enabled && self.irq_level && !self.irq_before {
            self.record(EventKind::Irq, 0, 0);
        }
        if irq_early {
            self.irq_before = true;
        }
        self.cycles += 1;
    }

    const NMI_EARLY_DOTS: usize = 1;

    /// Whether this CPU cycle runs a 4th PPU dot: PAL only, on every fifth cycle (its CPU is 16 master
    /// clocks per cycle, its PPU 5 per dot, so 16/5 = 3.2 dots per cycle). NTSC and Dendy return false on
    /// the region compare alone, so the check costs one predictable branch.
    #[inline]
    fn pal_fourth_dot(&self) -> bool {
        self.region == Region::Pal && self.cycles % 5 == 4
    }

    /// The 4th PPU dot of a PAL CPU cycle (index 3, past `NMI_EARLY_DOTS`, so it never counts as an early
    /// dot for the interrupt-poll rules). Out of line on purpose: inlined, `clock` had two `Ppu::tick`
    /// call sites and the doubled hot-loop code cost NTSC a few percent.
    #[inline(never)]
    fn pal_fourth_dot_tick(&mut self) {
        self.ppu.tick(self.cart.as_mut());
        if self.ppu.nmi_out {
            self.ppu.nmi_out = false;
            if self.events.enabled && !self.nmi_latch {
                self.record(EventKind::Nmi, 0, 0);
            }
            self.nmi_latch = true;
        }
    }

    /// The read itself, without advancing time.
    fn access_read(&mut self, addr: u16) -> u8 {
        // a `$2007` read on the CPU cycle right after one the PPU acted on is not performed. The
        // PPU cannot start a second PPUDATA read 3 dots after the first, so the second cycle's /RD finds
        // the previous access still running: the bus shows PPU open bus, and neither the read buffer, nor
        // `v`, nor the stamp moves. A page-crossing `LDA $20xx,X` makes exactly that pair: its dummy read
        // lands on `$2007` and its real read on the `$2007` mirror one CPU cycle later.
        if (0x2000..=0x3FFF).contains(&addr) && addr & 7 == 7 {
            if self.cycles == self.last_2007.wrapping_add(1) {
                // Only a read the PPU acted on stamps `last_2007`; this one must not re-arm the rule.
                let v = self.ppu.io_latch;
                if self.events.enabled {
                    self.record(EventKind::Read, addr, v);
                }
                self.open_bus = v;
                return v;
            }
            self.last_2007 = self.cycles;
        }
        let v = match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => {
                let v = self.ppu.read_register(self.cart.as_mut(), addr);
                if self.ppu.nmi_cancel {
                    self.ppu.nmi_cancel = false;
                    self.cancel_nmi();
                }
                v
            }
            // $4015 drives every bit except bit 5, which stays open bus.
            0x4015 => self.apu.read_status() | (self.open_bus & 0x20),
            // Joypad reads: flush any pending strobe write before reading, then shift the selected
            // port's report out one bit at a time. `$4016` is port 1 and `$4017` port 2, and they are
            // one path: the /OE rule and the strobe flush are the same statements for
            // both, and the two copies had each needed the same fix twice.
            0x4016 | 0x4017 => {
                let port = usize::from(addr & 1);
                let mask = 1u8 << port;
                self.frame_pads_polled = true;
                // Commit any pending strobe immediately before read (for test compatibility).
                if let Some((val, _)) = self.strobe_pending.take() {
                    self.strobe_devices(val);
                }
                // a read on a /OE line held over from the previous DMA cycle does not clock the pad.
                self.joy_oe |= mask;
                let held = self.joy_hold & mask != 0;
                let bit = if held {
                    self.peek_port(port)
                } else {
                    self.read_port(port)
                };
                // The expansion port drives D1 on `$4016` and D1-D4 on `$4017`, and only a fresh /OE
                // pulse clocks it, like the pad above.
                let bit = bit
                    | if held {
                        if port == 0 {
                            self.expansion.peek_4016_d1()
                        } else {
                            self.expansion.peek_4017_d1_d4()
                        }
                    } else if port == 0 {
                        self.expansion.read_4016_d1()
                    } else {
                        self.expansion.read_4017_d1_d4()
                    };
                // Port 1's open-bus bits are configurable (`Quirks::port1_open_bus_mask`); `$4017`
                // always shows $E0.
                let open = if port == 0 {
                    self.port1_open_mask
                } else {
                    0xE0
                };
                let v = (self.open_bus & open) | bit;
                // a Vs. cabinet drives bits 1-7 of both ports itself (nesdev "Vs. System"),
                // so its bits replace the open bus instead of ORing into it.
                match &self.vs {
                    Some(vs) => vs.port_bits(port, v),
                    None => v,
                }
            }
            0x4000..=0x401F => self.open_bus,
            // The cartridge's register window ($4020-$5FFF; the FDS decoder covers $4020-$4097):
            // boards with registers there answer, the rest float the open-bus value.
            0x4020..=0x5FFF => self.cart.ext_read(addr).unwrap_or(self.open_bus),
            // $4020-$5FFF is the mapper register window, $6000-$7FFF cartridge WRAM; the
            // Game Genie (which sits between connector and cartridge) substitutes only
            // reads of $8000-$FFFF. DMA fetches land in the same arm through
            // `access_read` (the DMC's sample byte at `dma_dmc_get`, an OAM source in
            // $8000+ at `dma_oam_get_inner`), so DMA sees codes too, as the device does.
            _ => self.cart_read(addr),
        };
        if self.events.enabled && (0x2000..=0x401F).contains(&addr) {
            self.record(EventKind::Read, addr, v);
        }
        // $4015 is internal to the APU and doesn't drive the external data bus.
        if addr != 0x4015 {
            self.open_bus = v;
        }
        v
    }
    /// The byte a cartridge read of `addr` returns: the board's own read with the Game Genie
    /// codes applied. Shared by the CPU's `access_read` and by the side-effect-free
    /// `peek`, so the trace and the debugger show the bytes the CPU actually runs.
    fn cart_read(&mut self, addr: u16) -> u8 {
        let v = self.cart.cpu_read(addr);
        self.genie_apply(addr, v)
    }
    /// [`SysBus::cart_read`] without the board's read side effects (MMC5's NMI vector
    /// acknowledges its scanline IRQ on a normal read; a peek must not).
    fn cart_peek(&mut self, addr: u16) -> u8 {
        let v = self.cart.cpu_peek(addr);
        self.genie_apply(addr, v)
    }
    /// Each loaded code that matches `addr` rewrites the byte. No codes loaded is the
    /// one predictable `false` branch, nothing else.
    fn genie_apply(&self, addr: u16, mut v: u8) -> u8 {
        if !self.genie.is_empty() && (0x8000..=0xFFFF).contains(&addr) {
            for code in &self.genie {
                v = code.apply(addr, v);
            }
        }
        v
    }
    /// One DMA engine for both sources: get cycles read, put cycles write.
    fn run_dma(&mut self, addr: u16, get: u64) -> u8 {
        let polled = (self.nmi_before, self.irq_before);
        let start = self.cycles;
        let mut run = DmaRun {
            addr,
            halted: false,
            dmc: 0,
            oam_setup: false,
            oam: self.oam_page.take(),
            count: 0,
            oam_val: None,
        };
        match self.apu.dmc.dma_req {
            Some(DmaKind::Abort) => {} // handled by the halt cycle
            Some(_) => run.dmc = 1,
            None => {}
        }
        let did_oam = run.oam.is_some();
        // /OE contiguity is tracked one DMA cycle at a time. The halt cycle always starts a fresh run.
        self.joy_oe = 0;
        self.joy_hold = 0;
        // The two DMA units run independently: each cycle the DMC get has priority, the units' halt, dummy and
        // alignment cycles overlap each other and the OAM copy, and the OAM copy only waits when the DMC reads.
        while run.dmc != 0 || run.oam.is_some() || !run.halted {
            // Parity of this cycle's number, taken before the clock; the clock then runs before the access,
            // exactly as in Bus::read/write.
            let parity = self.cycles % 2;
            // Carry the previous cycle's /OE lines into this one: a re-read of the same port does not clock.
            self.joy_hold = self.joy_oe;
            self.joy_oe = 0;
            self.clock();
            if !run.halted {
                self.dma_halt(&mut run);
            } else {
                let mut busy = false;
                let mut dmc_got = false;
                if run.dmc == 3 && parity == get {
                    self.dma_dmc_get(&mut run);
                    dmc_got = true;
                    busy = true;
                } else if run.oam_setup {
                    run.oam_setup = false;
                    busy = true;
                } else if parity == get && run.oam.is_some() && run.oam_val.is_none() {
                    let page = run.oam.unwrap();
                    self.dma_oam_get(&mut run, page);
                    busy = true;
                } else if parity != get && run.oam.is_some() && run.oam_val.is_some() {
                    self.dma_oam_put(&mut run);
                    busy = true;
                }
                if !dmc_got && (run.dmc == 1 || run.dmc == 2) {
                    run.dmc += 1;
                }
                if !busy {
                    self.dma_idle(&mut run);
                }
            }
            let req = self.apu.dmc.dma_req;
            match req {
                None => run.dmc = 0, // cancelled (or still in load delay): drop the stale request
                Some(DmaKind::Abort) => self.apu.dmc.dma_req = None, // overlaps the running DMA entirely
                Some(_) => {
                    if run.dmc == 0 {
                        run.dmc = 1;
                    }
                }
            }
        }
        if did_oam {
            self.dma_cycles = (self.cycles - start) as u32;
        }
        // The halted read finally happens. It is /OE-contiguous only if the last DMA cycle read the same port.
        self.joy_hold = self.joy_oe;
        self.joy_oe = 0;
        self.clock();
        let result = self.access_read(addr);
        self.joy_hold = 0;
        self.joy_oe = 0;
        // The DMA's own reads poll the interrupt flags; put the CPU's poll state back so the
        // resumed instruction sees the state it had before the DMA stole its read.
        (self.nmi_before, self.irq_before) = polled;
        // Mark that a DMA cycle just occurred (for unstable opcode detection).
        self.dma_just_occurred = true;
        result
    }

    /// Halt cycle: the CPU's own read cycle is stolen.
    fn dma_halt(&mut self, run: &mut DmaRun) {
        run.halted = true;
        if self.apu.dmc.dma_req == Some(DmaKind::Abort) {
            // Aborted DMA: the halt cycle is all it costs.
            self.apu.dmc.dma_req = None;
            run.dmc = 0;
        } else if run.dmc != 0 {
            run.dmc = 2;
        }
        self.access_read(run.addr);
    }

    /// DMC get cycle: fetch the sample byte. If the request vanished, the cycle is a plain dummy read.
    fn dma_dmc_get(&mut self, run: &mut DmaRun) {
        match self.apu.dmc.fetch_addr() {
            Some(a) => {
                // The DMA byte only reaches the external data bus; during a CPU read of $4015 the
                // internal bus (which supplies bit 5 of the result) must not pick it up.
                let saved = self.open_bus;
                let dval = self.access_read(a);
                if run.addr == 0x4015 {
                    self.open_bus = saved;
                }
                // Bus conflict: CPU address bus in $4000-$401F means the APU/IO register mirror
                // selected by the low 5 bits of the sample address is read simultaneously.
                // $4016/$4017 merge: bits 5-7 come from the sample byte (open_bus now holds it);
                // $4015 reads clear the frame IRQ flag without driving the bus.
                if (0x4000..=0x401F).contains(&run.addr) {
                    let r = 0x4000 | (a & 0x1F);
                    if matches!(r, 0x4015..=0x4017) {
                        self.access_read(r);
                    }
                }
                if self.events.enabled {
                    self.record(EventKind::DmcDma, a, dval);
                }
                self.apu.dmc.fill(dval);
            }
            None => {
                // Disabled after the DMA was scheduled: the get is a plain dummy read and the request is spent.
                self.apu.dmc.dma_req = None;
                self.access_read(run.addr);
            }
        }
        run.dmc = 0;
    }

    /// OAM get cycle: read the next source byte of the page.
    fn dma_oam_get(&mut self, run: &mut DmaRun, page: u8) {
        self.dma_oam_get_inner(run, page);
        if self.events.enabled {
            let src = (u16::from(page) << 8) | run.count;
            let val = run.oam_val.unwrap_or(0);
            self.record(EventKind::OamDma, src, val);
        }
    }

    fn dma_oam_get_inner(&mut self, run: &mut DmaRun, page: u8) {
        let src = (u16::from(page) << 8) | run.count;
        // The APU/IO registers answer only while the CPU address bus is in $4000-$401F.
        let active = (0x4000..=0x401F).contains(&run.addr);
        let unmapped = (0x4000..=0x5FFF).contains(&src);
        if !active && (0x4000..=0x401F).contains(&src) {
            // The OAM address bus alone can't reach them: open bus.
            run.oam_val = Some(self.open_bus);
            return;
        }
        if !active || (0x2000..=0x3FFF).contains(&src) {
            run.oam_val = Some(self.access_read(src));
            return;
        }
        // Active: the register mirror selected by the low 5 bits is read at the same time as the source.
        let r = 0x4000 | (src & 0x1F);
        let ext = if unmapped {
            self.open_bus
        } else {
            self.access_read(src)
        };
        run.oam_val = Some(if matches!(r, 0x4015..=0x4017) {
            self.open_bus = ext;
            let reg = self.access_read(r);
            // $4015 drives all but bit 5. Mapped memory drives the $4016/$4017 bus over the pads (which still clock).
            if r == 0x4015 {
                // Within the DMA the status value stays on the bus for the next read.
                self.open_bus = reg;
                reg
            } else if unmapped {
                reg
            } else {
                self.open_bus = ext;
                ext
            }
        } else {
            ext
        });
    }

    /// OAM put cycle: write the byte to OAM at OAMADDR. After 256 bytes, a queued $4014 page chains on.
    fn dma_oam_put(&mut self, run: &mut DmaRun) {
        let a = self.ppu.oam_addr;
        // DMA writes through $2004, so they refresh the row with the decay quirk on.
        self.ppu.oam_write_live(a, run.oam_val.take().unwrap());
        self.ppu.oam_addr = a.wrapping_add(1);
        run.count += 1;
        if run.count == 256 {
            run.oam = self.oam_next.take();
            if run.oam.is_some() {
                run.count = 0;
                run.oam_setup = true;
            }
        }
    }

    /// DMC dummy or alignment cycle: a plain read at the CPU's address.
    fn dma_idle(&mut self, run: &mut DmaRun) {
        self.access_read(run.addr);
    }

    fn access_write(&mut self, addr: u16, val: u8) {
        // The open bus takes the written value first, in its original place: no write handler reads
        // `open_bus` and the value is in place before `write` returns. Taking the old value on the
        // same line is the only thing the first-write glitch needs of the store.
        let prev_open_bus = self.open_bus;
        self.open_bus = val;
        if self.events.enabled && addr >= 0x2000 {
            self.record(EventKind::Write, addr, val);
        }
        match addr {
            0x0000..=0x1FFF => {
                let idx = (addr & 0x07FF) as usize;
                self.ram[idx] = val;
                // a frozen RAM address re-asserts its value before the guest can read it
                // back. With no cheat frozen this is the single predictable `false` branch -
                // nothing else runs (the same gate the event log's `enabled` flag uses).
                if self.cheats.is_armed() {
                    self.cheats.enforce_ram(&mut self.ram, idx);
                }
            }
            0x2000..=0x3FFF => {
                // First-write-glitch bus plumbing (spec §3.3): the glitch copies the
                // open bus on the same dot as the write, so the PPU needs the value the CPU drove
                // *before* it — `prev_open_bus`, already in a register, so the default path pays no
                // load and no extra branch. Cheapest test first: the dot compare is against the
                // PPU's hot counter and fails on 340 dots out of 341, so a default console (and a
                // quirk-on one off the copy dot) never reads the quirk byte.
                if self.ppu.dot == FIRST_WRITE_GLITCH_DOT && self.ppu.quirks.first_write_glitch {
                    self.ppu.cpu_bus = prev_open_bus;
                }
                self.ppu.write_register(self.cart.as_mut(), addr, val);
                if self.ppu.nmi_cancel {
                    self.ppu.nmi_cancel = false;
                    self.cancel_nmi();
                }
            }
            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, val),
            // $4014 queues the request; it runs on the CPU's next read cycle.
            0x4014 => {
                // A second write before the halt (RMW INC $4014) replaces the page: one DMA only.
                self.oam_page = Some(val);
            }
            0x4016 => {
                // a mapper 99 board latches bit 2 of this write as its bank register at the
                // write itself - not after the strobe delay, which only the pads wait out.
                self.cart.write_4016(val);
                let delay = if self.cycles.is_multiple_of(2) { 2 } else { 1 };
                self.strobe_pending = Some((val, delay));
            }
            0x4000..=0x401F => {}
            // Expansion area: a board's own registers (the FDS's $4020-$4097) claim what they
            // recognise; the rest goes to the board's ordinary write path.
            0x4020..=0x5FFF => {
                if !self.cart.ext_write(addr, val) {
                    self.cart.cpu_write(addr, val);
                }
            }
            _ => self.cart.cpu_write(addr, val),
        }
    }
}

impl Bus for SysBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.dma_just_occurred = false;
        let v = if self.oam_page.is_some() || self.apu.dmc.dma_req.is_some() {
            self.run_dma(addr, 0)
        } else {
            self.clock();
            self.access_read(addr)
        };
        if self.cdl.is_some() {
            self.cdl_read(addr);
        }
        v
    }

    fn note_code(&mut self, pc: u16) {
        let Some(log) = self.cdl.as_deref_mut() else {
            return;
        };
        if let Some(off) = self.cart.prg_rom_offset(pc) {
            log.mark(off, cdl::CODE);
        }
    }

    fn note_dummy(&mut self) {
        if self.cdl.is_some() {
            self.cdl_skip = true;
        }
    }

    fn read_unpolled(&mut self, addr: u16) -> u8 {
        // The cycle clocks normally (PPU/APU/mapper, the DMA hijack path and the IRQ event stamp are all
        // unchanged); only the poll snapshot the CPU consumes is carried over from the previous cycle.
        let irq = self.irq_before;
        let polled = self.frame_pads_polled;
        let v = self.read(addr);
        self.irq_before = irq;
        self.frame_pads_polled = polled;
        v
    }

    fn write(&mut self, addr: u16, val: u8) {
        self.dma_just_occurred = false;
        // An aborted DMC DMA whose halt lands on a write cycle does not happen at all.
        if self.apu.dmc.dma_req == Some(DmaKind::Abort) {
            self.apu.dmc.dma_req = None;
        }
        self.clock();
        self.access_write(addr, val);
    }

    fn nmi_poll(&mut self) -> bool {
        if self.nmi_before {
            self.cancel_nmi();
            true
        } else {
            false
        }
    }

    fn irq_poll(&self) -> Option<bool> {
        Some(self.irq_before)
    }

    fn dma_just_occurred(&self) -> bool {
        self.dma_just_occurred
    }

    /// A BRK/IRQ hijack takes the same edge `nmi_poll` takes, so it consumes the same flags: one
    /// copy of the rule keeps the two from drifting apart.
    fn nmi_hijack(&mut self) -> bool {
        self.nmi_poll()
    }

    fn peek(&mut self, addr: u16) -> Option<u8> {
        match addr {
            0x0000..=0x1FFF => Some(self.ram[(addr & 0x07FF) as usize]),
            // the byte the CPU would read — Game Genie codes applied — and without
            // the board's read side effects (an MMC5 vector read acknowledges its IRQ).
            0x6000..=0xFFFF => Some(self.cart_peek(addr)),
            _ => None, // I/O registers stay untouched: peeks must not side-effect.
        }
    }
}

impl SysBus {
    /// Drop the NMI edge and the latch: what consuming the edge means, wherever it is consumed —
    /// the end-of-instruction poll, a BRK/IRQ hijack, and the `$2000-$3FFF` accesses that make the
    /// PPU report `nmi_cancel`.
    pub(crate) fn cancel_nmi(&mut self) {
        self.nmi_latch = false;
        self.nmi_before = false;
    }

    /// Start the code/data log for a PRG-ROM of `prg_len` bytes, replacing any earlier one.
    /// A board without `prg_rom_offset` maps no address, so its log stays all zero.
    pub fn enable_cdl(&mut self, prg_len: usize) {
        self.cdl = Some(Box::new(Cdl::new(prg_len)));
        self.cdl_skip = false;
    }

    /// The code/data log, or `None` while it is off.
    pub fn cdl(&self) -> Option<&Cdl> {
        self.cdl.as_deref()
    }

    /// Mark a CPU read as data, unless `note_dummy` said it is a fetch or a dummy read. Only called
    /// with the log on; out of line so the off path stays one branch.
    #[inline(never)]
    fn cdl_read(&mut self, addr: u16) {
        if std::mem::take(&mut self.cdl_skip) {
            return;
        }
        if let (Some(log), Some(off)) = (self.cdl.as_deref_mut(), self.cart.prg_rom_offset(addr)) {
            log.mark(off, cdl::DATA);
        }
    }

    /// Map a RAM address to the work-RAM index it is allowed to touch: work RAM ($0000-$07FF) or
    /// any of its mirrors ($0800-$1FFF). Anything else on the bus ($2004, $4014, a mapper register
    /// at $8000, a cheatdb code up to $FFFF) must NOT be masked into the 2KB page - masking would
    /// make it land on a different guest address entirely - so out-of-window addresses are
    /// rejected. Shared by the cheat tools and the RAM watch (`watch::RamWatch::add`), so the two
    /// frontends cannot disagree about what a RAM address means. Policy: `cheat_poke` /
    /// `cheat_freeze` ignore out-of-window addresses (their signatures cannot report failure);
    /// `cheat_unfreeze` and `RamWatch::add` return `false`.
    pub fn ram_index(addr: u16) -> Option<usize> {
        if addr <= 0x1FFF {
            Some((addr & 0x07FF) as usize)
        } else {
            None
        }
    }

    /// A one-time write of `val` to work RAM at `addr` (or any of its mirrors). For a value
    /// the guest must not change, [`cheat_freeze`](SysBus::cheat_freeze). Codes outside
    /// $0000-$1FFF (see [`ram_index`](SysBus::ram_index)) are ignored.
    pub fn cheat_poke(&mut self, addr: u16, val: u8) {
        if let Some(idx) = Self::ram_index(addr) {
            self.ram[idx] = val;
        }
    }

    /// Freeze `addr` at `val`. Pokes it now, then re-asserts it after every later CPU write
    /// to that address or any of its mirrors (see the `cheats` module docs for the per-write vs
    /// per-frame choice). Freezing an already-frozen address re-freezes it at the new value.
    /// Codes outside $0000-$1FFF (see [`ram_index`](SysBus::ram_index)) are ignored.
    pub fn cheat_freeze(&mut self, addr: u16, val: u8) {
        if let Some(idx) = Self::ram_index(addr) {
            self.cheats.freeze(idx, val);
            self.ram[idx] = val;
        }
    }

    /// Stop freezing `addr`; returns true when it had been frozen. Codes outside $0000-$1FFF
    /// (see [`ram_index`](SysBus::ram_index)) are rejected with `false`.
    pub fn cheat_unfreeze(&mut self, addr: u16) -> bool {
        match Self::ram_index(addr) {
            Some(idx) => self.cheats.unfreeze(idx),
            None => false,
        }
    }

    /// Clear every freeze; the write hook disarms back to zero cost.
    pub fn cheat_clear(&mut self) {
        self.cheats.clear();
    }

    /// Whether any RAM address is currently frozen.
    pub fn cheats_armed(&self) -> bool {
        self.cheats.is_armed()
    }

    /// How many RAM addresses are currently frozen.
    pub fn frozen_count(&self) -> usize {
        self.cheats.len()
    }

    /// A snapshot of the 2KB work RAM for cheat searches; index = RAM address $0000-$07FF. The
    /// caller owns the snapshot it keeps between search steps (see `watch::narrow_between`).
    pub fn cheat_snapshot(&self) -> &[u8; 0x0800] {
        &self.ram
    }

    /// Re-assert every frozen value over the RAM page once - the frame-boundary application some
    /// frontends prefer (`cheats::RawRamCheats::apply_all`). Per-write enforcement still covers
    /// every guest write; callers that use both interleave freely on the same table.
    pub fn cheat_apply(&mut self) {
        self.cheats.apply_all(&mut self.ram);
    }

    /// Install the Game Genie codes the frontend holds (see `launch::genie_from_args`).
    /// Replaces the current set wholesale, so the frontend has one method for the initial
    /// install and for a power-cycle rebuild's fresh machine. Configuration, not emulation
    /// state: `load_state` never touches this list.
    pub fn set_genie_codes(&mut self, codes: Vec<crate::genie::Code>) {
        self.genie = codes;
    }

    /// The Game Genie codes currently installed, in the order the frontend gave them.
    pub fn genie_codes(&self) -> &[crate::genie::Code] {
        &self.genie
    }

    /// Write a `$4016` strobe to both ports. A port with a paddle attached strobes the paddle;
    /// the joypad field stays untouched so detaching the paddle leaves a sane pad behind. A
    /// Zapper has no shift register, so a strobe has nothing to latch there.
    fn strobe_devices(&mut self, val: u8) {
        self.pads[0].write(val);
        self.pads[1].write(val);
        for paddle in self.paddles.iter_mut().flatten() {
            paddle.write(val);
        }
        if let Some(fs) = self.four_score.as_mut() {
            fs.write(val);
        }
        for pad in self.power_pads.iter_mut().flatten() {
            pad.write(val);
        }
        self.expansion.write_out(val);
    }

    /// One /OE pulse on `port` (0 = `$4016`, 1 = `$4017`): the paddle's /D3-/D4 byte when one is
    /// plugged in, then the byte a Zapper drives there, otherwise the joypad's next shift bit.
    ///
    /// The Zapper's light sense is sampled here, at the moment of the read, from the last rendered
    /// frame: the gun does not clock and holds no sample, so two reads of the same port with the
    /// same aim return the same byte. See [`crate::controller::Zapper`] for why the beam is not
    /// modelled.
    fn read_port(&mut self, port: usize) -> u8 {
        if let Some(paddle) = self.paddles[port].as_mut() {
            return paddle.read();
        }
        if let Some(pad) = self.power_pads[port].as_mut() {
            return pad.read();
        }
        match self.zappers[port].as_ref() {
            Some(zapper) => {
                let bright = self.picture_bright(zapper.x, zapper.y);
                zapper.read(bright)
            }
            None => match self.four_score.as_mut() {
                Some(fs) => fs.read(port, &mut self.pads[port]),
                None => self.pads[port].read(),
            },
        }
    }

    /// The byte a read of `port` would return without clocking it: what a read on a /OE line still
    /// asserted from the previous cycle produces. A Zapper does not clock either, so its arm takes
    /// the same sample `read_port` takes.
    fn peek_port(&self, port: usize) -> u8 {
        if let Some(paddle) = self.paddles[port].as_ref() {
            return paddle.peek();
        }
        if let Some(pad) = self.power_pads[port].as_ref() {
            return pad.peek();
        }
        match self.zappers[port].as_ref() {
            Some(zapper) => zapper.read(self.picture_bright(zapper.x, zapper.y)),
            None => match self.four_score.as_ref() {
                Some(fs) => fs.peek(port, &self.pads[port]),
                None => self.pads[port].peek(),
            },
        }
    }

    /// Whether the picture pixel at `(x, y)` of the last rendered frame trips a Zapper's sensor.
    ///
    /// The framebuffer holds a 2C02 colour index per pixel, so the index is looked up in the
    /// built-in palette with the pixel's emphasis applied and its luma compared with
    /// [`BRIGHT_LUMA`](crate::controller::BRIGHT_LUMA). The built-in table is what the sensor
    /// answers to, not `--palette`: a replacement palette is frontend configuration the bus never
    /// sees, and the flash every gun game aims with is `$30`, which both tables call white. A
    /// coordinate outside the 256x240 picture reads dark — the gun is pointing at the bezel.
    fn picture_bright(&self, x: u16, y: u16) -> bool {
        let (x, y) = (usize::from(x), usize::from(y));
        if x >= 256 || y >= 240 {
            return false;
        }
        let i = y * 256 + x;
        let Some(&colour) = self.ppu.framebuffer.get(i) else {
            return false;
        };
        let emphasis = self.ppu.emphasis.get(i).copied().unwrap_or(0) & 7;
        crate::controller::is_bright(crate::ppu::palette::emphasized_rgb(colour, emphasis))
    }

    /// Plug a Vaus (Arkanoid) paddle into `port` (0 = `$4016`, 1 = `$4017`), replacing the joypad
    /// there. Configuration, not emulated state: a save state records the knob and button the
    /// attached device holds, and the frontend calls this again after building a machine.
    pub fn attach_paddle(&mut self, port: usize) {
        if let Some(slot) = self.paddles.get_mut(port) {
            *slot = Some(Paddle::default());
        }
    }

    /// Unplug the paddle on `port`, leaving a standard joypad behind.
    pub fn detach_paddle(&mut self, port: usize) {
        if let Some(slot) = self.paddles.get_mut(port) {
            *slot = None;
        }
    }

    /// Plug a Power Pad into `port` (0 = `$4016`, 1 = `$4017`), where it answers that port's reads
    /// on D3/D4 instead of the joypad. Configuration, not emulated state.
    pub fn attach_power_pad(&mut self, port: usize) {
        if let Some(slot) = self.power_pads.get_mut(port) {
            *slot = Some(PowerPad::default());
        }
    }

    /// Unplug the Power Pad on `port`, leaving a standard joypad behind.
    pub fn detach_power_pad(&mut self, port: usize) {
        if let Some(slot) = self.power_pads.get_mut(port) {
            *slot = None;
        }
    }

    /// The Power Pad on `port`, when one is attached.
    pub fn power_pad(&self, port: usize) -> Option<&PowerPad> {
        self.power_pads.get(port).and_then(|slot| slot.as_ref())
    }

    /// A frontend call, once per frame: set `port`'s Power Pad to the held `buttons` (bit `n - 1` is
    /// button `n`). Does nothing on a port with no Power Pad attached.
    pub fn set_power_pad(&mut self, port: usize, buttons: u16) {
        if let Some(Some(pad)) = self.power_pads.get_mut(port) {
            pad.buttons = buttons & 0x0FFF;
        }
    }

    /// Plug a Zapper light gun into `port` (0 = `$4016`, 1 = `$4017`), where it answers that port's
    /// reads instead of the joypad. Configuration, not emulated state: a save state records the aim
    /// and trigger the attached gun holds, and the frontend calls this again after building a
    /// machine or power-cycling one.
    pub fn attach_zapper(&mut self, port: usize) {
        if let Some(slot) = self.zappers.get_mut(port) {
            *slot = Some(Zapper::default());
        }
    }

    /// Unplug the Zapper on `port`, leaving a standard joypad behind.
    pub fn detach_zapper(&mut self, port: usize) {
        if let Some(slot) = self.zappers.get_mut(port) {
            *slot = None;
        }
    }

    /// Plug a Four Score into both ports. Configuration, not emulated state: the frontend
    /// calls this again after building or power-cycling a machine.
    pub fn attach_four_score(&mut self) {
        self.four_score = Some(FourScore::default());
    }

    /// Unplug the Four Score, leaving the two standard joypads.
    pub fn detach_four_score(&mut self) {
        self.four_score = None;
    }

    /// Plug the Vs. System's coin slots, service button and DIP switches in, with `dip` on
    /// its eight switches. Configuration, not emulated state: the console calls this for a Vs.
    /// header, and until it does the two ports read as a plain NES's.
    pub fn attach_vs_inputs(&mut self, dip: u8) {
        self.vs = Some(VsInputs {
            dip,
            ..VsInputs::default()
        });
    }

    /// Set the Vs. DIP switches on a machine that has the cabinet attached; a plain NES has
    /// no switches and ignores it.
    pub fn set_vs_dip(&mut self, dip: u8) {
        if let Some(vs) = self.vs.as_mut() {
            vs.dip = dip;
        }
    }

    /// The Vs. coin slots (`coins`, bit 0 = slot 1) and service button as held now, set once
    /// per game frame like the pads. Ignored on a machine with no cabinet attached.
    pub fn set_vs_inputs(&mut self, coins: u8, service: bool) {
        if let Some(vs) = self.vs.as_mut() {
            vs.coins = coins & 0x03;
            vs.service = service;
        }
    }

    /// A frontend call, once per frame: set pad 3 (`pad` 0) or pad 4 (`pad` 1) buttons. Does
    /// nothing with no Four Score attached.
    pub fn set_four_score_buttons(&mut self, pad: usize, buttons: u8) {
        if let Some(fs) = self.four_score.as_mut()
            && let Some(c) = fs.pads.get_mut(pad)
        {
            c.set_buttons(buttons);
        }
    }

    /// The paddle on `port`, when one is attached.
    pub fn paddle(&self, port: usize) -> Option<&Paddle> {
        self.paddles.get(port).and_then(|p| p.as_ref())
    }

    /// A frontend call, once per frame: move `port`'s paddle knob to `position` (`0x00` left ..
    /// `0xFF` right) and set its fire button. Does nothing on a port with no paddle attached.
    pub fn set_paddle(&mut self, port: usize, position: u8, fire: bool) {
        if let Some(Some(paddle)) = self.paddles.get_mut(port) {
            paddle.position = position;
            paddle.fire = fire;
        }
    }

    /// The Zapper on `port`, when one is attached.
    pub fn zapper(&self, port: usize) -> Option<&Zapper> {
        self.zappers.get(port).and_then(|slot| slot.as_ref())
    }

    /// A frontend call, once per frame: aim `port`'s gun at picture pixel `(x, y)` and set its
    /// trigger. Does nothing on a port with no Zapper attached.
    pub fn set_zapper(&mut self, port: usize, x: u16, y: u16, trigger: bool) {
        if let Some(Some(zapper)) = self.zappers.get_mut(port) {
            zapper.x = x;
            zapper.y = y;
            zapper.trigger = trigger;
        }
    }

    /// Writes the whole bus: RAM, cartridge, PPU, APU, pads and devices, then the bus's own latches
    /// and counters.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.bytes(&self.ram);
        self.cart.save_state(w);
        self.ppu.save_state(w);
        self.apu.save_state(w);
        self.pads[0].save_state(w);
        self.pads[1].save_state(w);
        w.u8(self.open_bus);
        w.u32(self.dma_cycles);
        w.opt_u8(self.oam_page);
        w.opt_u8(self.oam_next);
        w.u64(self.cycles);
        w.bool(self.nmi_latch);
        w.bool(self.nmi_before);
        w.bool(self.irq_level);
        w.bool(self.irq_before);
        w.bool(self.dma_just_occurred);
        match self.strobe_pending {
            Some((val, delay)) => {
                w.u8(1);
                w.u8(val);
                w.u8(delay);
            }
            None => w.u8(0),
        }
        w.u64(self.last_2007);
        // Device state appended last, as the layout convention asks
        // (`docs/spec-console-quirks.md` §4): one presence byte plus the device's own fields per port,
        // paddles and then Zappers. A detached port records a default device, so the layout does not
        // depend on what is plugged in - the same rule the PPU quirks follow.
        for slot in &self.paddles {
            w.u8(u8::from(slot.is_some()));
            slot.clone().unwrap_or_default().save_state(w);
        }
        for slot in &self.zappers {
            w.u8(u8::from(slot.is_some()));
            slot.unwrap_or_default().save_state(w);
        }
        // The Four Score block (`STATE_VERSION` 6): a presence byte plus pads 3/4 and the
        // read counters, default when detached.
        w.u8(u8::from(self.four_score.is_some()));
        self.four_score.clone().unwrap_or_default().save_state(w);
        // The Power Pad block (`STATE_VERSION` 11): per port a presence byte plus the mat's
        // fields, default when detached. It sits before the expansion block, which stays last
        // (its kind byte and length frame it, so a later kind needs no bump).
        for slot in &self.power_pads {
            w.u8(u8::from(slot.is_some()));
            slot.clone().unwrap_or_default().save_state(w);
        }
        // The expansion-port block (`STATE_VERSION` 7): kind byte, u16 length, payload.
        self.expansion.save_state(w);
    }

    /// Mirror of `save_state`. Cheats are session state, not part of the snapshot (like
    /// `events`): the restored RAM is the guest's, so a frontend that applies pokes per frame
    /// should call `cheat_apply()` after `load_state()` to re-impose its cheat set, while frozen
    /// addresses re-assert themselves on the write path as soon as the guest stores to them.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        r.bytes_into(&mut self.ram)?;
        self.cart.load_state(r)?;
        self.ppu.load_state(r)?;
        self.apu.load_state(r)?;
        self.pads[0].load_state(r)?;
        self.pads[1].load_state(r)?;
        self.open_bus = r.u8()?;
        self.dma_cycles = r.u32()?;
        self.oam_page = r.opt_u8()?;
        self.oam_next = r.opt_u8()?;
        self.cycles = r.u64()?;
        self.nmi_latch = r.bool()?;
        self.nmi_before = r.bool()?;
        self.irq_level = r.bool()?;
        self.irq_before = r.bool()?;
        self.dma_just_occurred = r.bool()?;
        self.strobe_pending = if r.bool()? {
            let val = r.u8()?;
            let delay = r.u8()?;
            Some((val, delay))
        } else {
            None
        };
        self.last_2007 = r.u64()?;
        for slot in &mut self.paddles {
            let attached = r.u8()? == 1;
            let mut paddle = Paddle::default();
            paddle.load_state(r)?;
            *slot = attached.then_some(paddle);
        }
        for slot in &mut self.zappers {
            let attached = r.u8()? == 1;
            let mut zapper = Zapper::default();
            zapper.load_state(r)?;
            *slot = attached.then_some(zapper);
        }
        let attached = r.u8()? == 1;
        let mut fs = FourScore::default();
        fs.load_state(r)?;
        self.four_score = attached.then_some(fs);
        for slot in &mut self.power_pads {
            let attached = r.u8()? == 1;
            let mut pad = PowerPad::default();
            pad.load_state(r)?;
            *slot = attached.then_some(pad);
        }
        self.expansion.load_state(r)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apu::dmc::DmaKind;
    use crate::bus::{RamPowerOn, power_on_ram};
    use crate::controller::{Controller, Zapper};
    use crate::mapper::load;

    /// A 16 KB NROM image whose PRG is all NOPs.
    fn tiny_rom() -> Vec<u8> {
        let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        v.resize(16 + 16_384 + 8_192, 0xEA);
        v
    }

    /// On an NES-001 the pad's /OE stays asserted across adjacent reads of the same port, so the
    /// three halted re-reads of a DMC DMA that lands on `lda $4016` clock the pad once. Halt and the
    /// resumed read are the only clocks: exactly 2 bits of the shift register, not 4. Both ports are
    /// run: they are one arm of `access_read`, so both must obey the rule.
    #[test]
    fn dmc_dma_holds_joy_oe_across_halted_joypad_reads() {
        const BUTTONS: u8 = 0xAA;
        for port in 0..2usize {
            let addr = 0x4016 + port as u16;
            let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
            bus.pads[port].buttons = BUTTONS;
            bus.pads[port].write(1); // strobe high reloads the shift register
            bus.pads[port].write(0); // strobe low starts the shift-out

            // Arm a DMC reload: one byte left, empty buffer, sample at $C000 (no bus conflict).
            bus.apu.dmc.current_addr = 0xC000;
            bus.apu.dmc.bytes_remaining = 1;
            bus.apu.dmc.buffer = None;
            bus.apu.dmc.load_delay = 0;
            bus.apu.dmc.dma_req = Some(DmaKind::Reload);

            // From cycle 0 with `get = 1` the halted run is: halt (the port, clocks), dummy (the port,
            // held), alignment (the port, held), get ($C000), resumed read (the port, clocks).
            bus.cycles = 0;
            bus.run_dma(addr, 1);

            // The pad behaves exactly like a controller clocked twice.
            let mut model = Controller::default();
            model.buttons = BUTTONS;
            model.write(1);
            model.write(0);
            model.read();
            model.read();
            let expected: Vec<u8> = (0..8).map(|_| model.read() & 1).collect();
            let got: Vec<u8> = (0..8).map(|_| bus.read(addr) & 1).collect();
            assert_eq!(got, expected, "pad {port} should advance by exactly 2 bits");

            // ...and not by 4, which is what clocking every halted re-read produced before the /OE rule.
            let mut four = Controller::default();
            four.buttons = BUTTONS;
            four.write(1);
            four.write(0);
            for _ in 0..4 {
                four.read();
            }
            let four_bits: Vec<u8> = (0..8).map(|_| four.read() & 1).collect();
            assert_ne!(
                got, four_bits,
                "stop-clock behaviour must be distinguishable on port {port}"
            );
        }
    }

    #[test]
    fn zero_power_on_leaves_ram_clear() {
        let mut ram = [0xAB; 0x0800];
        power_on_ram(&mut ram, RamPowerOn::Zero, 0x1234_5678_9ABC_DEF0);
        assert_eq!(ram, [0x00; 0x0800]);
    }

    #[test]
    fn ones_power_on_fills_ram_with_ones() {
        let mut ram = [0xAB; 0x0800];
        power_on_ram(&mut ram, RamPowerOn::Ones, 0x1234_5678_9ABC_DEF0);
        assert_eq!(ram, [0xFF; 0x0800]);
    }

    /// The one spelling table the flag, the file and the menu read. Every mode round-trips
    /// and nothing else is accepted, so a renamed value cannot be half-renamed.
    #[test]
    fn ram_power_on_names_round_trip() {
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            assert_eq!(
                RamPowerOn::from_name(mode.name()),
                Some(mode),
                "{}",
                mode.name()
            );
        }
        assert_eq!(RamPowerOn::from_name("garbage"), None);
        assert_eq!(
            RamPowerOn::from_name("Zero"),
            None,
            "the spelling is lowercase"
        );
        assert_eq!(RamPowerOn::from_name(""), None);
    }

    #[test]
    fn random_power_on_is_deterministic_per_seed() {
        const SEED: u64 = 0xDECA_FACE_BEEF_0001;
        let mut a = [0x00; 0x0800];
        let mut b = [0x00; 0x0800];
        power_on_ram(&mut a, RamPowerOn::Random, SEED);
        power_on_ram(&mut b, RamPowerOn::Random, SEED);
        assert_eq!(a, b);
        // A different seed moves at least the first byte: the mix is not a constant function.
        let mut c = [0x00; 0x0800];
        power_on_ram(&mut c, RamPowerOn::Random, SEED + 1);
        assert_ne!(a, c);
    }

    #[test]
    fn random_power_on_differs_from_zero() {
        let mut random = [0x00; 0x0800];
        let mut zero = [0x00; 0x0800];
        power_on_ram(&mut random, RamPowerOn::Random, 1);
        power_on_ram(&mut zero, RamPowerOn::Zero, 1);
        assert_ne!(random, zero);
        assert!(random.iter().any(|byte| *byte != 0));
    }

    /// Before S1 this fill had its own splitmix64 copy; it now draws from
    /// `rng::derive(seed, SeedDomain::Ram)`. These are the first 32 bytes the old copy
    /// produced, captured from the pre-S1 tree, so the page is byte-for-byte unchanged.
    /// The full suite would not notice a shift here (`--ram-init random` is opt-in and
    /// every baseline runs with zeroed RAM), which is why the bytes are pinned.
    #[test]
    fn random_power_on_page_is_byte_for_byte_unchanged() {
        const EXPECTED: [(u64, &str); 4] = [
            (
                0,
                "afcd1d7b39a820e2f465b9a16a9e786e4f450980185dc406ec814c72a8b88bf8",
            ),
            (
                0x1234_5678_9ABC_DEF0,
                "e850ce45c6221916607b69a1af0c76ad0da52c9044ff0135df31d826a8b97c41",
            ),
            (
                0xDECA_FACE_BEEF_0001,
                "1465fee91752452a26c0160b16c86cc7a8d1cd5519da71f70ead6cc39841b1eb",
            ),
            (
                u64::MAX,
                "202c651b7771d9e4c982f6db67f89fe9e98172b24cf82f38d282a9cb6cb31d6d",
            ),
        ];
        for (seed, expected) in EXPECTED {
            let mut ram = [0x00; 0x0800];
            power_on_ram(&mut ram, RamPowerOn::Random, seed);
            let got: String = ram[..32].iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(
                got, expected,
                "power-on page head changed for seed {seed:#x}"
            );
        }
    }

    /// A frozen RAM address survives the guest's own store through the real write path.
    #[test]
    fn frozen_ram_survives_a_processor_write() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.cheat_freeze(0x0300, 0x42);
        assert!(bus.cheats_armed());
        bus.write(0x0300, 0x7F);
        assert_eq!(bus.ram[0x0300], 0x42);
    }

    /// Every mirror of $0000-$07FF lands on the same frozen RAM slot.
    #[test]
    fn mirror_writes_honour_the_freeze() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.cheat_freeze(0x0005, 0x30);
        for mirror in [0x0005u16, 0x0805, 0x1005, 0x1805] {
            bus.write(mirror, 0xEE);
            assert_eq!(bus.ram[0x0005], 0x30, "write to ${mirror:04X}");
        }
        assert_eq!(bus.frozen_count(), 1);
    }

    /// A poke alone never arms the hook, and dropping the last freeze disarms it again.
    #[test]
    fn pokes_and_unfrozen_writes_run_normally() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.cheat_poke(0x0045, 0x22);
        assert!(!bus.cheats_armed());
        bus.write(0x0045, 0x33);
        assert_eq!(
            bus.ram[0x0045], 0x33,
            "a poke is one-shot: the guest value wins"
        );
        bus.cheat_freeze(0x0044, 0x11);
        assert!(bus.cheat_unfreeze(0x0044));
        assert!(!bus.cheats_armed());
        bus.write(0x0044, 0x44);
        assert_eq!(bus.ram[0x0044], 0x44);
    }

    /// The cheat_* entry points accept only work RAM or one of its mirrors. A code
    /// naming anything else on the bus ($2004, $4014, $8000, $91D9 - cheats that cheatdb codes
    /// can reach) must not mask into the 2KB page, or it writes a different guest address.
    #[test]
    fn cheat_addresses_outside_work_ram_are_rejected() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        let before = bus.ram;
        for addr in [0x2004u16, 0x4014, 0x8000, 0x91D9] {
            bus.cheat_freeze(addr, 0x55);
            bus.cheat_poke(addr, 0x77);
            assert!(
                !bus.cheat_unfreeze(addr),
                "cheat_unfreeze({addr:04X}) must report nothing frozen"
            );
            assert!(!bus.cheats_armed(), "code at {addr:04X} must not arm");
            assert_eq!(bus.frozen_count(), 0, "code at {addr:04X} must not freeze");
        }
        // Nothing masked in and nothing masked out: RAM is exactly as it was.
        assert_eq!(bus.ram, before);
        // The guard's window is exactly work RAM and its mirrors.
        for addr in [0x0000u16, 0x07FF, 0x0805, 0x1005, 0x1805, 0x1FFF] {
            assert!(
                SysBus::ram_index(addr).is_some(),
                "{addr:04X} is inside the window"
            );
        }
        for addr in [
            0x2000u16, 0x2004, 0x4014, 0x4016, 0x5FFF, 0x6000, 0x8000, 0xFFFF,
        ] {
            assert!(
                SysBus::ram_index(addr).is_none(),
                "{addr:04X} is outside the window"
            );
        }
        // The accepted ends of that window really do land on work RAM.
        bus.cheat_freeze(0x0000, 0x11);
        bus.cheat_freeze(0x1FFF, 0x22);
        assert!(bus.cheats_armed());
        assert_eq!(bus.ram[0x0000], 0x11);
        assert_eq!(bus.ram[0x07FF], 0x22);
    }

    /// The gun answers its port instead of the joypad. Bit 3 reads 0 over a bright picture
    /// pixel, bit 4 reads 0 while the trigger is held, and the byte is the same on every read —
    /// there is no shift register inside a Zapper to clock.
    #[test]
    fn zapper_reads_light_and_trigger_on_its_port() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.attach_zapper(1);
        bus.set_zapper(1, 10, 20, false);
        // The framebuffer is what the gun samples, so a test can paint the aimed pixel directly.
        bus.ppu.framebuffer[20 * 256 + 10] = 0x30; // $30: the white guns aim with
        assert_eq!(
            bus.read(0x4017) & 0x18,
            0x10,
            "a bright pixel pulls the light sense low"
        );
        assert_eq!(
            bus.read(0x4017) & 0x18,
            0x10,
            "no clocking: the same byte comes back"
        );

        bus.set_zapper(1, 10, 20, true);
        assert_eq!(
            bus.read(0x4017) & 0x18,
            0x00,
            "a held trigger pulls /D4 low as well"
        );

        bus.ppu.framebuffer[20 * 256 + 10] = 0x0F; // $0F: black
        assert_eq!(
            bus.read(0x4017) & 0x18,
            0x08,
            "a dark pixel leaves the light sense high"
        );
        bus.set_zapper(1, 10, 20, false);
        assert_eq!(
            bus.read(0x4017) & 0x18,
            0x18,
            "dark and released: both lines idle high"
        );
    }

    /// The sense really is taken at the aimed pixel. A bright pixel one column or one row away
    /// must not count, and an aim outside the picture reads dark.
    #[test]
    fn zapper_samples_the_aimed_pixel() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.attach_zapper(0);
        // One bright pixel, at (10, 20).
        bus.ppu.framebuffer[20 * 256 + 10] = 0x30;
        for &(x, y, want) in &[
            (10u16, 20u16, 0x00u8),
            (11, 20, 0x08),
            (9, 20, 0x08),
            (10, 21, 0x08),
            (10, 19, 0x08),
            (0, 0, 0x08),
            (255, 239, 0x08),
            (256, 20, 0x08),
            (10, 240, 0x08),
        ] {
            bus.set_zapper(0, x, y, false);
            assert_eq!(bus.read(0x4016) & 0x08, want, "aim at ({x}, {y})");
        }
    }

    /// A port with no gun attached keeps answering as a joypad, and the two ports are
    /// independent: attaching on one leaves the other alone.
    #[test]
    fn a_port_without_a_zapper_stays_a_joypad() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.pads[1].buttons = 0x01; // A held
        bus.pads[1].write(1);
        bus.pads[1].write(0);
        bus.attach_zapper(0);
        assert_eq!(bus.zapper(1), None, "only port 1 has a gun");
        assert_eq!(
            bus.read(0x4017) & 0x01,
            0x01,
            "port 2 still shifts the pad's A out"
        );

        bus.detach_zapper(0);
        assert_eq!(bus.zapper(0), None);
        assert_eq!(
            bus.read(0x4016) & 0x18,
            0x00,
            "a joypad drives neither bit 3 nor bit 4"
        );
    }

    /// The gun's aim and trigger are emulated state and survive a round trip; a port left
    /// empty stays empty, so a state cannot invent a device.
    #[test]
    fn zapper_state_round_trips() {
        let mut bus = SysBus::new(load(&tiny_rom()).unwrap());
        bus.attach_zapper(0);
        bus.set_zapper(0, 33, 44, true);
        let mut w = crate::state::StateWriter::new();
        bus.save_state(&mut w);
        let data = w.finish();

        let mut fresh = SysBus::new(load(&tiny_rom()).unwrap());
        let mut r = crate::state::StateReader::new(&data);
        fresh.load_state(&mut r).expect("load");
        assert_eq!(
            fresh.zapper(0),
            Some(&Zapper {
                x: 33,
                y: 44,
                trigger: true
            })
        );
        assert_eq!(
            fresh.zapper(1),
            None,
            "a port with no gun in the state has none after loading"
        );
    }
}
