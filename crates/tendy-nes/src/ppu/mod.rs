pub mod memory;
pub mod palette;
pub mod render;
pub mod sprites;
mod state;
pub mod trace;
pub mod viewer;

use crate::bus::RamPowerOn;
use crate::cart::{Mapper, Region};
use crate::console::PpuQuirks;
use crate::rng::{SeedDomain, derive};
use memory::{nametable_index, palette_index};
use render::{Background, LagPixel, LagSource};
use sprites::{ExtraSprite, SpriteSlot};
use trace::TileTrace;

/// PPU dots per scanline.
pub const DOTS_PER_LINE: u16 = 341;
/// Scanlines per frame (NTSC), including vblank and the pre-render line 261.
pub const LINES_PER_FRAME: u16 = 262;
/// Scanlines per frame for PAL and Dendy, including the pre-render line 311.
pub const LINES_PER_FRAME_PAL: u16 = 312;

/// PPU I/O latch decay: about 20 frames (262 * 341 dots per frame) without any register access. Calibration.
pub const PPU_IO_DECAY_DOTS: u32 = 20 * 262 * 341;

/// OAM decay interval (spec `docs/spec-console-quirks.md` §3.1): a row of sprite DRAM
/// that has gone longer than this many CPU cycles without being refreshed has lost its contents.
/// Mesen2's `OamDecayCycleCount` (`NesPpu.h:36`); about 1.7 ms on NTSC, a little over one vblank.
/// The comparison is Mesen's: a read within `<= OAM_DECAY_CYCLES` refreshes the row, `> ` decays it.
pub const OAM_DECAY_CYCLES: u64 = 3000;

/// The one dot the first-write glitch can fire on (spec `docs/spec-console-quirks.md` §3.3):
/// Mesen's `_cycle == 257`, which is `self.dot == 258` here because `self.dot` is the next dot
/// to process (spec §0.2). A CPU write to `$2000`/`$2005`/`$2006` that lands on this dot collides with the
/// horizontal copy of `t` into `v`, so the copied bits come from the CPU data bus instead.
pub const FIRST_WRITE_GLITCH_DOT: u16 = 258;

/// Palette RAM at power-on: the 32 bytes the 2C02 leaves in `$3F00-$3F1F` before any code runs.
///
/// Real hardware does not come up with a zeroed palette; these are the power-up colours. The
/// values are the table `blargg_ppu_tests_2005.09.15b/power_up_palette` tests against and that
/// TASVideos treats as the canonical emulation target, and the same array Mesen2 uses
/// (`NesPpu.cpp`, `paletteRamBootValues`) when palette RAM is not randomised.
pub const POWER_UP_PALETTE: [u8; 32] = [
    0x09, 0x01, 0x00, 0x01, 0x00, 0x02, 0x02, 0x0D, 0x08, 0x10, 0x08, 0x24, 0x00, 0x00, 0x04,
    0x2C, //
    0x09, 0x01, 0x34, 0x03, 0x00, 0x04, 0x00, 0x14, 0x08, 0x3A, 0x00, 0x02, 0x00, 0x20, 0x2C,
    0x08, //
];

/// Which layers the frontend hides from the picture. The PPU still runs both in full, so
/// sprite 0 hit, overflow and every other observable are unchanged; only the colour written to the
/// framebuffer differs. Default: both visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerMask {
    pub hide_bg: bool,
    pub hide_sprites: bool,
}

#[derive(Clone, Debug)]
pub struct Ppu {
    /// Per-pixel provenance plane: `None` (the default) means nothing is recorded.
    /// A debug view, never serialised and never read by emulation. See `trace.rs`.
    trace: Option<Box<TileTrace>>,
    /// Display-only layer hiding: frontend configuration, never serialised and
    /// never consulted by anything but the colour choice in `finalize_lag_pixel`.
    pub layers: LayerMask,
    /// Display-only sprite-limit removal: when set, sprites beyond the eight in
    /// secondary OAM are also drawn. Frontend configuration, never serialised; the real evaluation,
    /// `$2002` overflow and sprite 0 hit are untouched.
    pub sprite_limit_removed: bool,
    /// The extra sprites for the line being drawn (see `sprite_limit_removed`); not serialised.
    pub extra_sprites: Vec<ExtraSprite>,
    /// $2000 PPUCTRL.
    pub ctrl: u8,
    /// $2001 PPUMASK.
    pub mask: u8,
    /// $2002 PPUSTATUS: bit 7 vblank, bit 6 sprite 0 hit, bit 5 sprite overflow.
    pub status: u8,
    /// $2003 OAMADDR.
    pub oam_addr: u8,
    pub oam: [u8; 256],
    /// OAM decay staleness stamps (spec `docs/spec-console-quirks.md` §3.1): one
    /// `SysBus::cycles` value per 8-byte OAM row, the last cycle the row was refreshed. Emulated
    /// state, so it is serialised unconditionally (the layout never depends on config), but
    /// only `PpuQuirks::oam_decay` ever writes it — a default console carries 32 zeros forever. A
    /// reset does not refresh DRAM, so [`Ppu::reset`] leaves these alone.
    pub oam_stamp: [u64; 32],
    /// The CPU cycle the PPU is currently inside, copied from `SysBus::cycles` at the top of
    /// `SysBus::clock` while the decay quirk is on. Transient and derived: never serialised, and `0`
    /// on a standalone `Ppu`, which therefore never decays anything.
    pub cpu_cycle: u64,
    /// Loopy registers: current VRAM address, temporary address, fine X scroll, write toggle.
    pub v: u16,
    pub t: u16,
    pub fine_x: u8,
    pub w: bool,
    /// $2007 read buffer.
    pub read_buffer: u8,
    /// Last value written to or read from a PPU register (PPU open bus).
    pub io_latch: u8,
    /// Dots until the PPU I/O latch decays to 0 (restarted by every register access; 0 = not decaying).
    pub io_decay: u32,
    /// The PPU write warm-up (spec `docs/spec-console-quirks.md` §2.3): while set,
    /// writes to `$2000`/`$2001`/`$2005`/`$2006` are ignored and the `$2007` read buffer is held
    /// clear, because the internal reset signal that clears the vblank flag also gates those
    /// registers. Armed by the `Nes` constructors from `PpuQuirks::ppu_warmup` and by
    /// [`Ppu::reset`], and cleared on the pre-render line's dot 1 by [`Ppu::tick`]. Power-on value
    /// `false`, so a standalone `Ppu` behaves exactly as it did before the field existed.
    pub warmup: bool,
    /// 4 KB of nametable RAM: four 1 KB tables, of which the first two are the only ones the
    /// mirroring modes below four-screen address.
    pub vram: [u8; 4096],
    pub palette: [u8; 32],
    /// Current position: scanline 0..=`lines_per_frame` - 1, dot 0..=340.
    pub scanline: u16,
    pub dot: u16,
    pub frame: u64,
    /// Scanlines per frame: 262 on NTSC, 312 on PAL and Dendy.
    pub lines_per_frame: u16,
    /// Scanline where vblank (and NMI) starts: 241, or 291 on Dendy.
    pub vblank_line: u16,
    /// The pre-render scanline: 261, or 311 on PAL and Dendy.
    pub pre_render_line: u16,
    /// Idle scanlines inserted after line 240 (see `set_overclock_lines`); 0 on a stock machine.
    pub overclock_lines: u16,
    /// Skip the last dot of an odd frame's pre-render line while rendering (NTSC only: the 2C07 PAL PPU
    /// has "no short line", and Dendy never skips it).
    pub odd_frame_skip: bool,
    /// The 2C07 (PAL) and the Dendy swap the red and green emphasis bits relative to NTSC and the
    /// RGB PPUs: bit 5 of `$2001` emphasizes green there and bit 6 red. Set by `set_region`.
    pub emphasis_swap: bool,
    /// Set when the PPU raises NMI; the system moves it to the CPU and clears it.
    pub nmi_out: bool,
    /// $2002 read one dot before vblank set: the flag (and NMI) is skipped this frame.
    pub suppress_vbl: bool,
    /// Access at dot 2..=3 (0-1 PPU clocks after vblank set): the pending NMI is cancelled.
    pub nmi_cancel: bool,
    /// Set at the start of vblank; the frontend clears it after taking the frame.
    pub frame_ready: bool,
    /// 256 x 240 palette indices (0..=63), row-major.
    pub framebuffer: Vec<u8>,
    /// PPUMASK emphasis bits for each pixel (bits 0-2 = red/green/blue), parallel to
    /// `framebuffer`; the red and green bits are already swapped on PAL and Dendy.
    pub emphasis: Vec<u8>,
    /// The last two pixels produced, oldest first: greyscale and emphasis reach the
    /// picture two dots after the pixel itself, so each entry waits here until dot x+3. Entries at
    /// and past `lag_len` are stale.
    pub lag_pipe: [LagPixel; 2],
    /// Where those two entries came from, entry for entry. Trace state, written only while the
    /// tracer is on: the per-dot path never touches it, and it is not part of `save_state` — the
    /// descriptors it holds are not machine state and emit nothing.
    pub lag_src: [LagSource; 2],
    /// Live entries in `lag_pipe` (0..=2).
    pub lag_len: u8,
    /// Background fetch latches and shift registers.
    pub bg: Background,
    /// Sprites selected for the line being drawn (at most 8, in OAM order).
    pub sprites: [SpriteSlot; 8],
    /// Secondary OAM (persists across lines).
    pub oam2: [u8; 32],
    pub eval_s: u8,
    /// $2004 "OAM buffer": what a read returns while rendering.
    pub oam_latch: u8,
    pub eval_in_range: bool,
    pub eval_done: bool,
    /// "OAM2 overflowed" flag: freezes the OAM2 fetch address at 0 (see `sprite_eval_dot`).
    pub oam2_frozen: bool,
    /// Secondary OAM address used by the sprite fetch window (dots 257-340).
    pub oam2_addr: u8,
    /// Rendering-enabled state of the previous two dots (the PPU reacts to $2001 about two dots late).
    pub rend_pipe: [bool; 5],
    /// OAM row latched when rendering was disabled; copied over row 0 when it resumes.
    pub oam_corrupt_pending: Option<u8>,
    sprite_active_prev: bool,
    pub eval_bug: u8,
    pub sprite0_in_oam2: bool,
    /// Consecutive dots rendering has been enabled (saturating).
    pub rend_run: u8,
    pub bg_idle: u8,
    /// Dots left until a deferred $2007 read-buffer update completes (0 = none).
    pub read_pending: u8,
    /// Byte the PPU read from memory in this tick's fetch (hardware data read).
    pub fetch_bus: Option<u8>,
    /// Last byte a fetch put on the data bus.
    pub bus_last: u8,
    /// (fetch dot, latch byte): replaces the low pattern-address byte at that dot.
    pub ale_latch: Option<(u16, u8)>,
    /// (ALE dot, old v) pending hybrid nametable read.
    pub hyb: Option<(u16, u16)>,
    /// (`$2006` landing dot, pre-write `v`) pending scroll-glitch update (spec `docs/spec-console-quirks.md` §3.2).
    /// Only `PpuQuirks::scroll_glitch_2006` ever arms it, so a
    /// default PPU carries `None` forever; it lives at most two dots but can straddle an instruction
    /// boundary, so it is emulated state and serialised unconditionally (`STATE_VERSION` 4). It does
    /// not touch the `hyb` path: the hybrid read still happens, the glitch only overwrites `v`/`t`
    /// two dots later.
    pub glitch_2006: Option<(u16, u16)>,
    /// The CPU data bus value **before** the register write being processed (spec
    /// `docs/spec-console-quirks.md` §3.3). A first write to `$2000`/`$2005`/`$2006` that lands on
    /// the dot-257 horizontal copy takes the copied bits from this stale bus value instead of `t`
    /// (the PPU's `v` is loaded from the bus while the CPU is still driving it). Transient and
    /// derived: set by `SysBus::access_write` immediately before the write when
    /// `PpuQuirks::first_write_glitch` is on, `0` on a standalone `Ppu`, never serialised. No state
    /// layout change, so `STATE_VERSION` stays 4.
    pub cpu_bus: u8,
    /// The hardware quirks this PPU runs with. Configuration, not emulated state:
    /// never serialised, and `PpuQuirks::default()` is exactly the behaviour before the field
    /// existed. The `Nes` constructors set it from `ConsoleConfig::quirks`.
    pub quirks: PpuQuirks,
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
    }
}

/// Dots between a $2001 write and the background fetches / scroll increments reacting to it.
const BG_DELAY: usize = 5;
const SPRITE_DELAY: usize = 3;
/// Dots from a $2007 read until the read buffer is updated while rendering.
const READ_DELAY: u8 = 4;
/// Dots from a second $2006 write until v changes while rendering.
const HYB_DELAY: u16 = 3;
/// Dots rendering must have been on for the dot-339 sprite counter reset to take effect.
const COUNT_RUN: u8 = 7;
/// Max dots the BG shifters keep clocking (serial input) after fetches stop.
const BG_IDLE_SHIFT: u8 = 10;

/// Cost counters for the tick path: the phases a `ppu-profile` build times inside [`Ppu::tick`].
///
/// The `ppu-profile` cargo feature is off by default, and every use of this module in `tick` carries
/// its own `#[cfg(feature = "ppu-profile")]`: a default build compiles none of it and pays not even a
/// branch for it, so the counters are compiled out (docs/ppu-profile.md).
///
/// A sample is one phase of one dot out of every eight. Both rotations matter:
///
/// - the **dot** rotates with the scanline, so every dot of an 8-dot block is sampled; a fixed phase
///   would sample only the nametable-fetch dot (or only a fetch-free one) and bias every share.
/// - the **phase** rotates with the sample counter, so all five phases are measured on the same
///   population of dots. One phase per sample is what keeps the clock out of the number: a sample
///   costs one `Instant::now()`, not five, and the probe measures that one read in place so the
///   report can take it back out (see `Totals::clock_cost_ns`; this host's read is ~28 ns).
#[cfg(feature = "ppu-profile")]
pub mod profile {
    use std::cell::Cell;
    use std::time::Instant;

    /// Dots one sample stands for: the probe arms on one dot in eight.
    pub const DOTS_PER_SAMPLE: u64 = 8;

    /// One of the five phases a dot is split into, in the order they run.
    ///
    /// The four buckets are the method split; `tick`'s bookkeeping is the one that happens
    /// twice in a dot, before the fetches and after them, so it is measured as two phases and added
    /// back together in the report.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Phase {
        /// The head of `tick`: the I/O latch decay, the rendering-state pipeline, the frame geometry.
        Bookhead = 0,
        /// The background fetches and the shifter load, plus the `$2006` glitch landing dot and the
        /// idle shifter clock that share the interval.
        Background = 1,
        /// The visible-line pixel path: `output_pixel` and the dots-257/258 finalize.
        Pixel = 2,
        /// Sprite evaluation, the fetch window and the dot-339 counter reset.
        Sprite = 3,
        /// The tail of `tick`: vblank and NMI, the PAL OAM refresh, the `$2007` read-buffer step and
        /// the dot/scanline counter.
        Booktail = 4,
    }

    impl Phase {
        /// The cycle, in the order `tick` marks its boundaries.
        pub const ALL: [Phase; 5] = [
            Phase::Bookhead,
            Phase::Background,
            Phase::Pixel,
            Phase::Sprite,
            Phase::Booktail,
        ];

        /// The phase that starts where this one ends.
        fn next(self) -> Phase {
            match self {
                Phase::Bookhead => Phase::Background,
                Phase::Background => Phase::Pixel,
                Phase::Pixel => Phase::Sprite,
                Phase::Sprite => Phase::Booktail,
                Phase::Booktail => Phase::Bookhead,
            }
        }

        fn index(self) -> usize {
            self as usize
        }
    }

    /// Nanoseconds summed per phase, and how many samples each phase got (they rotate, so the five
    /// counts are equal to within one).
    ///
    /// `clock_ns`/`clock_samples` are the probe's own calibration: an empty interval measured in the
    /// same context as a sample, i.e. the cost of the one `Instant::now()` each sample's span carries.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Totals {
        ns: [u64; 5],
        samples: [u64; 5],
        clock_ns: u64,
        clock_samples: u64,
    }

    impl Totals {
        /// Raw nanoseconds accumulated in `phase`.
        pub fn ns(&self, phase: Phase) -> u64 {
            self.ns[phase.index()]
        }

        /// Samples taken for `phase`.
        pub fn samples(&self, phase: Phase) -> u64 {
            self.samples[phase.index()]
        }

        /// The probe's own clock read, measured in place: the overhead every sample's span carries.
        pub fn clock_cost_ns(&self) -> f64 {
            if self.clock_samples == 0 {
                0.0
            } else {
                self.clock_ns as f64 / self.clock_samples as f64
            }
        }
    }

    thread_local! {
        /// Per-thread, so two test threads cannot mix their counters.
        static TOTALS: Cell<Totals> = const { Cell::new(Totals { ns: [0; 5], samples: [0; 5], clock_ns: 0, clock_samples: 0 }) };
        /// Whether the probe may sample: [`pause`] turns the counters off without a rebuild, which is
        /// how the harness measures what they cost.
        static ENABLED: Cell<bool> = const { Cell::new(true) };
    }

    /// Stop sampling: `tick`'s probe calls stay, but none of them reads the clock.
    pub fn pause() {
        ENABLED.with(|e| e.set(false));
    }

    /// Resume sampling.
    pub fn resume() {
        ENABLED.with(|e| e.set(true));
    }

    /// Zero the accumulators and return what they held.
    pub fn take() -> Totals {
        TOTALS.with(|t| t.replace(Totals::default()))
    }

    /// Zero the accumulators without reading them.
    pub fn reset() {
        TOTALS.with(|t| t.set(Totals::default()));
    }

    /// The sampling probe [`Ppu::tick`] carries: it times one phase of one dot in eight.
    pub struct Probe {
        /// The phase this sample times, or `None` when this dot is not sampled.
        phase: Option<Phase>,
        /// When the timed phase opened, once `tick` has reached it.
        t0: Option<Instant>,
    }

    impl Probe {
        /// Arm on one dot in every [`DOTS_PER_SAMPLE`], with the phase rotating per sample and the
        /// dot rotating with the scanline (see the module docs).
        #[inline]
        pub fn arm(dot: u16, scanline: u16) -> Probe {
            if !ENABLED.with(|e| e.get()) {
                return Probe {
                    phase: None,
                    t0: None,
                };
            }
            let at = u32::from(dot) + u32::from(scanline);
            let phase = if at % DOTS_PER_SAMPLE as u32 == 0 {
                Some(Phase::ALL[(at / DOTS_PER_SAMPLE as u32) as usize % Phase::ALL.len()])
            } else {
                None
            };
            // The bookkeeping head is the one phase whose span crosses a `tick` boundary: it runs from
            // the top of this tick to where the background block starts, so it opens here — a sample
            // that opened at the end of the *previous* tick would be thrown away with that tick's
            // probe.
            if phase.is_none() {
                return Probe { phase, t0: None };
            }
            // The probe's own calibration, measured in the same context as a sample: an empty
            // interval is one clock read, which is exactly what a sample's span carries in overhead.
            let a = Instant::now();
            let b = Instant::now();
            let overhead = b.duration_since(a).as_nanos() as u64;
            TOTALS.with(|t| {
                let mut totals = t.get();
                totals.clock_ns += overhead;
                totals.clock_samples += 1;
                t.set(totals);
            });
            let t0 = if phase == Some(Phase::Bookhead) {
                Some(Instant::now())
            } else {
                None
            };
            Probe { phase, t0 }
        }

        /// Mark the boundary where `phase` starts: called at the five boundaries of the cycle, once
        /// per dot plus the one at the end of `tick`.
        ///
        /// Two of those five calls matter to a sample — the one that opens its phase and the one that
        /// closes it — and every other call is two compares. The clock is read on those two only.
        #[inline]
        pub fn enter(&mut self, phase: Phase) {
            let Some(wanted) = self.phase else { return };
            if wanted == phase {
                self.t0 = Some(Instant::now());
            } else if wanted.next() == phase {
                let Some(t0) = self.t0.take() else { return };
                let ns = Instant::now().duration_since(t0).as_nanos() as u64;
                TOTALS.with(|t| {
                    let mut totals = t.get();
                    totals.ns[wanted.index()] += ns;
                    totals.samples[wanted.index()] += 1;
                    t.set(totals);
                });
            }
        }
    }

    /// One piece of the visible-line pixel path: the split of [`Phase::Pixel`] used by the
    /// profiler (docs/ppu-pixel.md §2).
    ///
    /// `output_pixel` rotates over [`Bucket::BODY`] and `finalize_lag_pixel` arms [`Bucket::FINALIZE`]
    /// on its own, so the five body buckets are sampled on the same population of dots and the
    /// finalize bucket on its own. The backdrop lookup (`$3F00` override) is not a bucket of its own:
    /// it is timed with the lag push it is part of.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Bucket {
        /// The shifter mux: the background pixel and its palette, before the D3/D1 gate.
        BgPixel = 0,
        /// The background palette RAM lookup.
        BgPalette = 1,
        /// `sprite_pixel`: the eight-slot scan, the halt counters and the shifter advance.
        SpritePixel = 2,
        /// The sprite palette RAM lookup, on the dots where a sprite won.
        SprPalette = 3,
        /// The `LagPixel` construction (backdrop lookup included) and the push into the two-dot
        /// pipeline.
        LagPush = 4,
        /// `finalize_lag_pixel`'s compose: the D3/D1 gate, the hidden layers, greyscale, emphasis and
        /// the two array writes. Its trace-plane write is outside the span, because it is off.
        Finalize = 5,
    }

    impl Bucket {
        /// The five buckets `output_pixel` rotates over.
        pub const BODY: [Bucket; 5] = [
            Bucket::BgPixel,
            Bucket::BgPalette,
            Bucket::SpritePixel,
            Bucket::SprPalette,
            Bucket::LagPush,
        ];
        /// The one bucket `finalize_lag_pixel` arms.
        pub const FINALIZE: [Bucket; 1] = [Bucket::Finalize];
        /// Every bucket, in index order.
        pub const ALL: [Bucket; 6] = [
            Bucket::BgPixel,
            Bucket::BgPalette,
            Bucket::SpritePixel,
            Bucket::SprPalette,
            Bucket::LagPush,
            Bucket::Finalize,
        ];

        fn index(self) -> usize {
            self as usize
        }
    }

    /// Nanoseconds and samples per [`Bucket`], plus the same in-place clock calibration the phase
    /// probe keeps.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct BucketTotals {
        ns: [u64; 6],
        samples: [u64; 6],
        clock_ns: u64,
        clock_samples: u64,
    }

    impl BucketTotals {
        /// Raw nanoseconds accumulated in `bucket`.
        pub fn ns(&self, bucket: Bucket) -> u64 {
            self.ns[bucket.index()]
        }

        /// Samples taken for `bucket`.
        pub fn samples(&self, bucket: Bucket) -> u64 {
            self.samples[bucket.index()]
        }

        /// The probe's own clock read, measured in place (as `Totals::clock_cost_ns`).
        pub fn clock_cost_ns(&self) -> f64 {
            if self.clock_samples == 0 {
                0.0
            } else {
                self.clock_ns as f64 / self.clock_samples as f64
            }
        }
    }

    thread_local! {
        /// Per-thread, like `TOTALS`.
        static BUCKET_TOTALS: Cell<BucketTotals> =
            const { Cell::new(BucketTotals { ns: [0; 6], samples: [0; 6], clock_ns: 0, clock_samples: 0 }) };
        /// Whether the pixel sub-probe may sample. Independent of `ENABLED` so a run can measure the
        /// outer phase split and the inner bucket split without one inflating the other.
        static PIXEL_ENABLED: Cell<bool> = const { Cell::new(true) };
    }

    /// Stop sampling the pixel sub-buckets (the outer phase probe is unaffected).
    pub fn pixel_pause() {
        PIXEL_ENABLED.with(|e| e.set(false));
    }

    /// Resume sampling the pixel sub-buckets.
    pub fn pixel_resume() {
        PIXEL_ENABLED.with(|e| e.set(true));
    }

    /// Zero the bucket accumulators and return what they held.
    pub fn pixel_take() -> BucketTotals {
        BUCKET_TOTALS.with(|t| t.replace(BucketTotals::default()))
    }

    /// The pixel sub-probe: samples one [`Bucket`] of one output dot in [`DOTS_PER_SAMPLE`].
    ///
    /// Same discipline as [`Probe`] — the dot rotates with the scanline and the bucket rotates with
    /// the sample — with one difference: the bucket is picked from the sample index rather than from
    /// a running counter, so the two arm sites on a dot (`output_pixel` and `finalize_lag_pixel`)
    /// agree and neither needs resetting between runs.
    pub struct PixelProbe {
        /// The bucket this sample times, or `None` when this dot is not sampled.
        bucket: Option<Bucket>,
    }

    impl PixelProbe {
        /// Arm on one output dot in every [`DOTS_PER_SAMPLE`], for one bucket out of `table`.
        #[inline]
        pub fn arm(dot: u16, scanline: u16, table: &'static [Bucket]) -> PixelProbe {
            if !PIXEL_ENABLED.with(|e| e.get()) {
                return PixelProbe { bucket: None };
            }
            let at = u32::from(dot) + u32::from(scanline);
            if at % DOTS_PER_SAMPLE as u32 != 0 {
                return PixelProbe { bucket: None };
            }
            // The same in-place calibration the phase probe takes, so the two instruments' read
            // estimates are comparable.
            let a = Instant::now();
            let b = Instant::now();
            let overhead = b.duration_since(a).as_nanos() as u64;
            BUCKET_TOTALS.with(|t| {
                let mut totals = t.get();
                totals.clock_ns += overhead;
                totals.clock_samples += 1;
                t.set(totals);
            });
            let index = (at / DOTS_PER_SAMPLE as u32) as usize % table.len();
            PixelProbe {
                bucket: Some(table[index]),
            }
        }

        /// Run `body` timed as `bucket` when this sample is that bucket, and plainly otherwise.
        #[inline]
        pub fn time<T>(&self, bucket: Bucket, body: impl FnOnce() -> T) -> T {
            if self.bucket != Some(bucket) {
                return body();
            }
            let t0 = Instant::now();
            let value = body();
            let ns = Instant::now().duration_since(t0).as_nanos() as u64;
            BUCKET_TOTALS.with(|t| {
                let mut totals = t.get();
                totals.ns[bucket.index()] += ns;
                totals.samples[bucket.index()] += 1;
                t.set(totals);
            });
            value
        }
    }
}

impl Ppu {
    pub fn new() -> Self {
        Ppu::new_aligned(0)
    }

    /// Power-on at a chosen CPU/PPU alignment: `phase` (0..=3) offsets the initial dot counter,
    /// selecting one of the 4 relative phases the CPU and PPU can be in at power-on. The offset
    /// wraps modulo the dots-per-scanline constant; phase 0 reproduces the default construction.
    pub fn new_aligned(phase: u8) -> Self {
        Ppu {
            trace: None,
            layers: LayerMask::default(),
            sprite_limit_removed: false,
            extra_sprites: Vec::new(),
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            oam: [0; 256],
            oam_stamp: [0; 32],
            cpu_cycle: 0,
            v: 0,
            t: 0,
            fine_x: 0,
            w: false,
            read_buffer: 0,
            io_latch: 0,
            io_decay: 0,
            warmup: false,
            vram: [0; 4096],
            palette: POWER_UP_PALETTE,
            scanline: 0,
            dot: (phase as u16) % DOTS_PER_LINE,
            frame: 0,
            lines_per_frame: LINES_PER_FRAME,
            vblank_line: 241,
            pre_render_line: 261,
            overclock_lines: 0,
            odd_frame_skip: true,
            emphasis_swap: false,
            nmi_out: false,
            suppress_vbl: false,
            nmi_cancel: false,
            frame_ready: false,
            framebuffer: vec![0; 256 * 240],
            emphasis: vec![0; 256 * 240],
            lag_pipe: [LagPixel::default(); 2],
            lag_src: [LagSource::default(); 2],
            lag_len: 0,
            bg: Background::default(),
            sprites: [SpriteSlot::default(); 8],
            oam2: [0xFF; 32],
            eval_s: 0,
            oam_latch: 0xFF,
            eval_in_range: false,
            eval_done: false,
            oam2_frozen: false,
            oam2_addr: 0,
            oam_corrupt_pending: None,
            sprite_active_prev: false,
            rend_pipe: [false; 5],
            eval_bug: 0,
            sprite0_in_oam2: false,
            rend_run: 0,
            bg_idle: 0,
            read_pending: 0,
            fetch_bus: None,
            bus_last: 0,
            ale_latch: None,
            hyb: None,
            glitch_2006: None,
            cpu_bus: 0,
            quirks: PpuQuirks::default(),
        }
    }

    /// The PPU half of the reset button (spec `docs/spec-console-quirks.md` §2.1).
    ///
    /// The NES-001 front loader wires its reset button to the PPU as well as the CPU, so the PPU
    /// comes out of a reset at the top of a picture whose frame is even, with the registers on that
    /// line back at their power-on values: `$2000` and `$2001` cleared, the `t` latch and the fine X
    /// scroll cleared, the `$2005`/`$2006` write toggle cleared and the `$2007` read buffer cleared.
    /// Everything the reset line does not touch keeps its value: `v`, `$2002`, OAMADDR, OAM, VRAM,
    /// palette RAM, the PPU I/O latch and the region timing. Famicoms and both top-loaders do not
    /// connect the button to the PPU at all — that is the default here, and the only path to this
    /// function is `Quirks::ppu_reset_on_reset` in [`crate::nes::Nes::reset_aligned`].
    ///
    /// The in-flight pipeline fields take the value a fresh PPU has, read from [`Ppu::new_aligned`]
    /// so the two lists cannot drift apart. `frame` keeps counting and only rounds up to even,
    /// because the first pre-render line after the reset must not skip its dot and `frame` is a
    /// monotonic counter that tests and the tracer read.
    ///
    /// OAM decay stamps are *not* touched: a reset does not refresh DRAM (spec `docs/spec-console-quirks.md` §3.1).
    pub fn reset(&mut self) {
        let fresh = Ppu::new_aligned(0);
        // Registers on the PPU's reset line (nesdev "PPU power up state"; Mesen2 `NesPpu::Reset`).
        self.ctrl = fresh.ctrl;
        self.mask = fresh.mask;
        self.t = fresh.t;
        self.fine_x = fresh.fine_x;
        self.w = fresh.w;
        self.read_buffer = fresh.read_buffer;
        // The reset signal that gates the registers comes back with the reset: the warm-up is
        // re-armed only here, so only when `ppu_reset_on_reset` reached this function (spec §2.3).
        self.warmup = self.quirks.ppu_warmup;
        // "The PPU comes out of power and reset at the top of the picture", on an even frame:
        // `frame & 1 == 1` is what makes the pre-render line skip its last dot.
        self.scanline = fresh.scanline;
        self.dot = fresh.dot;
        self.frame += self.frame & 1;
        // Everything in flight dies with the position jump: the NMI race latches, the rendering
        // pipeline, the sprite evaluation scratch, the delayed $2007/fetch artefacts.
        self.nmi_out = fresh.nmi_out;
        self.suppress_vbl = fresh.suppress_vbl;
        self.nmi_cancel = fresh.nmi_cancel;
        self.rend_pipe = fresh.rend_pipe;
        self.rend_run = fresh.rend_run;
        self.bg_idle = fresh.bg_idle;
        self.bg = fresh.bg;
        self.lag_pipe = fresh.lag_pipe;
        self.lag_src = fresh.lag_src;
        self.lag_len = fresh.lag_len;
        self.sprites = fresh.sprites;
        self.eval_s = fresh.eval_s;
        self.oam_latch = fresh.oam_latch;
        self.eval_in_range = fresh.eval_in_range;
        self.eval_done = fresh.eval_done;
        self.oam2_frozen = fresh.oam2_frozen;
        self.oam2_addr = fresh.oam2_addr;
        self.sprite_active_prev = fresh.sprite_active_prev;
        self.eval_bug = fresh.eval_bug;
        self.sprite0_in_oam2 = fresh.sprite0_in_oam2;
        self.read_pending = fresh.read_pending;
        self.fetch_bus = fresh.fetch_bus;
        self.bus_last = fresh.bus_last;
        self.ale_latch = fresh.ale_latch;
        self.hyb = fresh.hyb;
        self.glitch_2006 = fresh.glitch_2006;
    }

    /// Frame timing for a region's PPU: NTSC is 262 lines with vblank at 241 and the odd-frame dot skip;
    /// PAL is 312 lines with 70 vblank lines and no short line; Dendy is 312 lines with the same 311
    /// pre-render line but whose vblank (and NMI) starts at line 291.
    pub fn set_region(&mut self, region: Region) {
        // The PAL refresh window (scanlines 265-310) is part of the OAM-decay quirk, so it follows
        // the region: re-derive it here in case the timing changes after construction.
        self.quirks.oam_pal_refresh = self.quirks.oam_decay && region == Region::Pal;
        self.lines_per_frame = match region {
            Region::Ntsc => LINES_PER_FRAME,
            Region::Pal | Region::Dendy => LINES_PER_FRAME_PAL,
        };
        self.vblank_line = match region {
            Region::Dendy => 291,
            Region::Ntsc | Region::Pal => 241,
        };
        self.pre_render_line = match region {
            Region::Ntsc => 261,
            Region::Pal | Region::Dendy => 311,
        };
        self.odd_frame_skip = region == Region::Ntsc;
        // The 2C07 and the Dendy swap the red and green emphasis bits (NESdev PPU registers,
        // `$2001`: "Emphasize red (green on PAL/Dendy)"); the RGB PPUs and NTSC do not.
        self.emphasis_swap = matches!(region, Region::Pal | Region::Dendy);
        self.apply_overclock_lines();
    }

    /// Overclock knob: insert `n` idle post-render lines after line 240, so the CPU gets `n` x 341/3 more
    /// cycles per frame to run in. The lines render nothing and `vblank_line`, `pre_render_line` and
    /// `lines_per_frame` all move down by `n`. Power-on configuration like the region: it is not part of
    /// a save state (STATE_VERSION is unchanged), so call it right after `set_region` and before running.
    /// A state saved with the knob on and loaded with it off resumes without panicking: a saved scanline
    /// beyond the machine's frame wraps to line 0 (see `load_state`). Calling it again re-derives the
    /// geometry from the current region, so changing `n` mid-frame can leave the position past the end of
    /// the (shorter) frame; set it only at power-on.
    pub fn set_overclock_lines(&mut self, n: u16) {
        let region = if self.vblank_line - self.overclock_lines == 291 {
            Region::Dendy
        } else if self.lines_per_frame - self.overclock_lines == LINES_PER_FRAME_PAL {
            Region::Pal
        } else {
            Region::Ntsc
        };
        self.overclock_lines = n;
        self.set_region(region);
        if self.scanline >= self.lines_per_frame {
            self.scanline = 0;
        }
    }

    /// Adds `overclock_lines` to the region geometry `set_region` just wrote.
    fn apply_overclock_lines(&mut self) {
        self.lines_per_frame += self.overclock_lines;
        self.vblank_line += self.overclock_lines;
        self.pre_render_line += self.overclock_lines;
    }

    /// Whether the beam is on one of the inserted idle lines (lines 241 .. 241 + n), where the APU is held.
    #[inline]
    pub fn in_overclock_lines(&self) -> bool {
        self.overclock_lines != 0
            && self.scanline > 240
            && self.scanline < 241 + self.overclock_lines
    }

    /// Fill the PPU's volatile RAM the way a power-on would have left it: the 2 KB
    /// nametable RAM, the 256-byte primary OAM and the 32-byte secondary OAM. Palette RAM is not
    /// touched: its power-up contents are a fixed table on real consoles, not noise.
    ///
    /// - `Zero` is today's construction (`new_aligned`): nametables and OAM 0x00, secondary OAM 0xFF
    ///   (the value the sprite-evaluation clear writes there anyway).
    /// - `Ones` fills everything 0xFF.
    /// - `Random` draws one `SeedDomain::PpuRam` stream, eight bytes per step, least-significant
    ///   byte first: nametable RAM, then primary OAM, then secondary OAM.
    ///
    /// OAM attribute bytes (`index & 3 == 2`) lose bits 2-4 on hardware, the same `0xE3` mask
    /// `$2004` writes apply, so the fill never stores bits the chip cannot hold.
    pub fn power_on_ram(&mut self, mode: RamPowerOn, seed: u64) {
        match mode {
            RamPowerOn::Zero => {
                self.vram = [0x00; 4096];
                self.oam = [0x00; 256];
                self.oam2 = [0xFF; 32];
            }
            RamPowerOn::Ones => {
                self.vram = [0xFF; 4096];
                self.oam = [0xFF; 256];
                self.oam2 = [0xFF; 32];
            }
            RamPowerOn::Random => {
                let mut rng = derive(seed, SeedDomain::PpuRam);
                rng.fill_bytes(&mut self.vram);
                rng.fill_bytes(&mut self.oam);
                rng.fill_bytes(&mut self.oam2);
            }
        }
        for (i, b) in self.oam.iter_mut().enumerate() {
            if i & 3 == 2 {
                *b &= 0xE3;
            }
        }
    }

    /// All reads of primary OAM: attribute bits 2-4 do not physically exist.
    pub fn oam_read(&self, a: u8) -> u8 {
        let v = self.oam[a as usize];
        if a & 3 == 2 { v & 0xE3 } else { v }
    }

    /// An OAM read by the machine, with the decay bookkeeping (spec §3.1).
    ///
    /// With the quirk off this is [`Ppu::oam_read`] and nothing else — the default path and the
    /// first test here. With it on, a row read within [`OAM_DECAY_CYCLES`] of its stamp is refreshed;
    /// a staler row has lost its contents, so all eight bytes take the decay pattern and the stamp
    /// is deliberately *not* refreshed (Mesen2 `ReadSpriteRam`), which is what makes a decayed row
    /// keep reading as decayed until something writes it. Bits 2-4 of an attribute byte do not
    /// physically exist, so the pattern masks them.
    ///
    /// The viewer keeps using `oam_read`: reading OAM in a debugger must not refresh DRAM.
    pub fn oam_read_live(&mut self, a: u8) -> u8 {
        if !self.quirks.oam_decay {
            return self.oam_read(a);
        }
        let row = usize::from(a >> 3);
        if self.cpu_cycle.saturating_sub(self.oam_stamp[row]) <= OAM_DECAY_CYCLES {
            self.oam_stamp[row] = self.cpu_cycle;
        } else {
            for i in 0..8u8 {
                let b = (a & 0xF8) | i;
                self.oam[usize::from(b)] = if b & 3 == 2 { b & 0xE3 } else { b };
            }
        }
        self.oam_read(a)
    }

    /// An OAM write by the machine, with the decay bookkeeping (spec §3.1): store the
    /// byte, then stamp its row. With the quirk off this is the store and nothing else.
    pub fn oam_write_live(&mut self, a: u8, v: u8) {
        self.oam[usize::from(a)] = v;
        if self.quirks.oam_decay {
            self.oam_stamp[usize::from(a >> 3)] = self.cpu_cycle;
        }
    }

    fn in_render_window(&self) -> bool {
        self.rendering_enabled() && (self.scanline < 240 || self.scanline == self.pre_render_line)
    }

    /// An access 0-1 PPU clocks after the vblank flag sets (an access at dot 2..=3). Hardware still
    /// lets the CPU's NMI edge detector lose the pending edge there, so a `$2002` read and a
    /// `$2000` NMI-disable in this window both cancel it. Shared so the two paths cannot drift.
    fn in_nmi_cancel_window(&self) -> bool {
        self.scanline == self.vblank_line && (2..=3).contains(&self.dot)
    }

    fn vram_increment(&self) -> u16 {
        if self.ctrl & 0x04 != 0 { 32 } else { 1 }
    }

    /// A `$2007` read of palette memory: six bits from palette RAM, masked to the greyscale columns
    /// by `$2001` bit 0, with PPU open bus on the top two bits.
    fn palette_read(&mut self, cart: &mut dyn Mapper, addr: u16) -> u8 {
        let mut p = self.mem_read(cart, addr) & 0x3F;
        if self.mask & 1 != 0 {
            p &= 0x30;
        }
        p | (self.io_latch & 0xC0)
    }

    /// Read PPU address space: pattern tables from the cartridge, nametables, palette.
    pub fn mem_read(&mut self, cart: &mut dyn Mapper, addr: u16) -> u8 {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => cart.ppu_read(addr),
            0x2000..=0x3EFF => match cart.nt_read(addr, &self.vram) {
                Some(v) => v,
                // Unreachable for a board using the trait default, which maps CIRAM itself:
                // kept for one that answers `None` for an address it does not own.
                None => self.vram[nametable_index(addr, cart.mirroring())],
            },
            _ => self.palette[palette_index(addr)],
        }
    }

    /// Sprite-pattern read: same path as `mem_read`, but boards with separate sprite CHR banks
    /// (MMC5) get the sprite door instead of the background one.
    pub fn mem_read_sprite(&mut self, cart: &mut dyn Mapper, addr: u16) -> u8 {
        let addr = addr & 0x3FFF;
        if addr <= 0x1FFF {
            cart.ppu_read_sprite(addr)
        } else {
            self.mem_read(cart, addr)
        }
    }

    /// Write PPU address space. Palette entries keep only their low 6 bits.
    pub fn mem_write(&mut self, cart: &mut dyn Mapper, addr: u16, val: u8) {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => cart.ppu_write(addr, val),
            0x2000..=0x3EFF => {
                if !cart.nt_write(addr, val, &mut self.vram) {
                    self.vram[nametable_index(addr, cart.mirroring())] = val;
                }
            }
            _ => self.palette[palette_index(addr)] = val & 0x3F,
        }
    }

    /// CPU read of a PPU register; `reg` is the CPU address (only the low 3 bits matter).
    pub fn read_register(&mut self, cart: &mut dyn Mapper, reg: u16) -> u8 {
        let v = match reg & 7 {
            2 => {
                if self.scanline == self.vblank_line && self.dot == 1 {
                    self.suppress_vbl = true;
                } else if self.in_nmi_cancel_window() {
                    self.nmi_cancel = true;
                    self.nmi_out = false;
                }
                // Sprite flags are sampled at the end of the read, a couple of dots after the vblank flag.
                let st = if self.scanline == self.pre_render_line && self.dot == 1 {
                    self.status & 0x80
                } else {
                    self.status & 0xE0
                };
                let r = st | (self.io_latch & 0x1F);
                self.status &= !0x80;
                self.w = false;
                r
            }
            4 => {
                // Early PPUs (before the RP2C02G) cannot read OAMDATA back: the register is
                // write-only and a read returns PPU open bus. It does so inside the
                // render window too — the `oam_latch` path is skipped — and it must not touch
                // OAMADDR, OAM or the I/O latch (the `self.io_latch = v` after the match is a
                // no-op here). The write path is unchanged.
                if !self.quirks.ppu_2004_readable {
                    self.io_latch
                } else if self.in_render_window() {
                    self.oam_latch
                } else {
                    // Refresh-on-read is the whole of "rendering keeps OAM alive": with the decay
                    // quirk on, the machine's own reads stamp the row.
                    self.oam_read_live(self.oam_addr)
                }
            }
            7 => {
                let addr = self.v & 0x3FFF;
                let defer = self.in_render_window();
                let r = if defer {
                    // While rendering, the buffer is updated READ_DELAY dots later from the fetch bus.
                    let r = if addr >= 0x3F00 {
                        self.palette_read(cart, addr)
                    } else {
                        self.read_buffer
                    };
                    self.read_pending = READ_DELAY;
                    r
                } else if addr >= 0x3F00 {
                    // Palette reads are not buffered; the buffer is filled from the nametable underneath.
                    self.read_buffer = self.mem_read(cart, addr - 0x1000);
                    self.palette_read(cart, addr)
                } else {
                    let r = self.read_buffer;
                    self.read_buffer = self.mem_read(cart, addr);
                    r
                };
                if !defer {
                    self.v = self.v.wrapping_add(self.vram_increment()) & 0x7FFF;
                    cart.ppu_bus(self.v);
                    // The internal reset signal holds the read buffer clear (spec §2.3): a
                    // buffered `$2007` read during the warm-up returns 0, though `v` still steps and
                    // palette reads (unbuffered) still return palette data.
                    if self.warmup {
                        self.read_buffer = 0;
                    }
                }
                r
            }
            _ => self.io_latch,
        };
        self.io_latch = v;
        v
    }

    /// CPU write of a PPU register; `reg` is the CPU address (only the low 3 bits matter).
    pub fn write_register(&mut self, cart: &mut dyn Mapper, reg: u16, val: u8) {
        self.io_latch = val;
        self.io_decay = PPU_IO_DECAY_DOTS;
        // Boards that decode the PPU's state from the bus (MMC5) see the write with its CPU address;
        // a board that does not care ignores it. Register mirrors are filtered by the board.
        cart.ppu_register_write(reg, val);
        // The write warm-up (spec §2.3): the internal reset signal ignores `$2000`,
        // `$2001`, `$2005` and `$2006` until the pre-render line's dot 1, so the latch does not
        // toggle and no NMI edge or first-write glitch is seen. The latch update and the cart hook
        // above still happen, as on hardware. `$2002`/`$2003`/`$2004`/`$2007` are not gated.
        if self.warmup && matches!(reg & 7, 0 | 1 | 5 | 6) {
            return;
        }
        match reg & 7 {
            0 => {
                let was_enabled = self.ctrl & 0x80 != 0;
                self.ctrl = val;
                if was_enabled && val & 0x80 == 0 && self.in_nmi_cancel_window() {
                    self.nmi_cancel = true;
                    self.nmi_out = false;
                }
                self.t = (self.t & 0xF3FF) | (u16::from(val & 0x03) << 10);
                // $2000 first-write glitch (spec §3.3): a write that lands on the
                // dot-257 horizontal copy leaves `v`'s nametable-X bit taken from the CPU data bus
                // (the open bus before this write) instead of from `t`. `t` keeps its normal value;
                // only `v` is garbled. Cheapest test first: the dot compare is on a value this
                // function already has (`dot == 1` is tested in the NMI block below), so the quirk
                // byte is only read on the one dot in 341 that can glitch.
                if self.dot == FIRST_WRITE_GLITCH_DOT
                    && self.quirks.first_write_glitch
                    && self.in_render_window()
                {
                    let b = u16::from(self.cpu_bus);
                    self.v = (self.v & !0x0400) | ((b << 10) & 0x0400);
                }
                if !was_enabled
                    && val & 0x80 != 0
                    && self.status & 0x80 != 0
                    && !(self.scanline == self.pre_render_line && self.dot == 1)
                {
                    self.nmi_out = true;
                }
            }
            1 => self.mask = val,
            3 => self.oam_addr = val,
            4 => {
                if self.in_render_window() {
                    // Write glitch: no OAM write, only the high six bits of OAMADDR step.
                    self.oam_addr = self.oam_addr.wrapping_add(4) & 0xFC;
                } else {
                    self.oam_write_live(self.oam_addr, val);
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            }
            5 => {
                if self.w {
                    self.t = (self.t & 0x0C1F)
                        | (u16::from(val & 0x07) << 12)
                        | (u16::from(val & 0xF8) << 2);
                } else {
                    self.t = (self.t & 0x7FE0) | u16::from(val >> 3);
                    self.fine_x = val & 0x07;
                    // $2005 first-write glitch (spec §3.3): the dot-257 copy takes
                    // coarse X from the CPU data bus (bits 7-3) instead of `t`. Dot test first, as
                    // on the `$2000` path: a default console never touches the quirk byte.
                    if self.dot == FIRST_WRITE_GLITCH_DOT
                        && self.quirks.first_write_glitch
                        && self.in_render_window()
                    {
                        let b = u16::from(self.cpu_bus);
                        self.v = (self.v & !0x001F) | ((b >> 3) & 0x001F);
                    }
                }
                self.w = !self.w;
            }
            6 => {
                if self.w {
                    let old_v_w = self.v;
                    self.t = (self.t & 0x7F00) | u16::from(val);
                    self.v = self.t;
                    if !self.in_render_window() {
                        cart.ppu_bus(self.v);
                    }
                    let e = self.dot + HYB_DELAY - 1;
                    let in_window = self.in_render_window();
                    if in_window
                        && ((9..=257).contains(&e) || (321..=337).contains(&e))
                        && (e - 1).is_multiple_of(8)
                    {
                        self.hyb = Some((e, old_v_w));
                    }
                    // $2006 scroll glitch (spec §3.2): when the `v <- t` copy lands
                    // on a coarse-X increment or on the dot-257 horizontal copy, the address is
                    // ANDed with its pre-write value instead of taking `t` whole. The hybrid read
                    // above is untouched — this only schedules a second, later write to `v`/`t`.
                    if self.quirks.scroll_glitch_2006
                        && in_window
                        && (e == 257
                            || (e == 328 || e == 336)
                            || (e.is_multiple_of(8) && (8..=256).contains(&e)))
                    {
                        self.glitch_2006 = Some((e, old_v_w));
                    }
                } else {
                    self.t = (self.t & 0x00FF) | (u16::from(val & 0x3F) << 8);
                    // $2006 first-write glitch (spec §3.3): both nametable bits of
                    // `v` come from the CPU data bus (bits 3-2) instead of `t`. Dot test first, as
                    // on the `$2000` path.
                    if self.dot == FIRST_WRITE_GLITCH_DOT
                        && self.quirks.first_write_glitch
                        && self.in_render_window()
                    {
                        let b = u16::from(self.cpu_bus);
                        self.v = (self.v & !0x0C00) | ((b << 8) & 0x0C00);
                    }
                }
                self.w = !self.w;
            }
            7 => {
                let addr = self.v;
                self.mem_write(cart, addr, val);
                if self.in_render_window() {
                    // During rendering a $2007 access steps `v` the way the renderer does — a
                    // coarse X increment and a fine Y increment together, ignoring the increment
                    // bit (NESdev PPU scrolling, $2007: "During rendering (on the pre-render line
                    // and the visible lines 0-239, provided either background or sprite rendering
                    // is enabled), it will update v in an odd way, triggering a coarse X increment
                    // and a Y increment simultaneously (with normal wrapping behavior)"). The read
                    // path already does this at its four-dot expiry; without it the two disagree.
                    self.inc_coarse_x();
                    self.inc_y();
                } else {
                    self.v = self.v.wrapping_add(self.vram_increment()) & 0x7FFF;
                    cart.ppu_bus(self.v);
                }
            }
            _ => {}
        }
    }

    /// Secondary OAM address (0..=31) at the moment rendering is switched off; seeds OAM corruption.
    fn oam_corrupt_seed(&self) -> u8 {
        let d = self.dot + 2;
        match d {
            1..=64 => ((d - 1) >> 1) as u8,
            65..=256 => (self.eval_s.wrapping_add(3) & 0x1C) & 31,
            257..=320 => ((((d - 257) >> 3) * 4 + ((d - 257) & 7).min(3)) & 31) as u8,
            _ => 0,
        }
    }

    /// Advance the PPU clock by `dots` dots without touching the CPU or the APU. Used to
    /// realign the CPU/PPU relative phase when the reset button is pressed: only the PPU's own
    /// dot/scanline counter moves, so this is **realignment, not a PPU reset** — control, mask,
    /// `w`, the read buffer, VRAM, OAM and the palette are left exactly as they were.
    pub fn advance_dots(&mut self, cart: &mut dyn Mapper, dots: u8) {
        for _ in 0..dots {
            self.tick(cart);
        }
    }

    /// Advance one PPU dot: background fetches on visible and pre-render lines, pixel output on
    /// visible lines, sprite evaluation for the next line at dot 257, vblank start at `vblank_line` dot 1 (NMI
    /// when enabled), flags cleared at the pre-render line dot 1, and (NTSC) the skipped dot at the end of odd
    /// pre-render lines while rendering.
    pub fn tick(&mut self, cart: &mut dyn Mapper) {
        #[cfg(feature = "ppu-profile")]
        let mut probe = profile::Probe::arm(self.dot, self.scanline);
        // Frame geometry is read once into locals: these are compared on every dot, and re-loading them
        // through `&mut self` after each store in the body (the compiler cannot prove the body does not
        // write them) put several loads back into the innermost loop.
        let pre_render = self.pre_render_line;
        let vblank = self.vblank_line;
        let lines = self.lines_per_frame;
        let odd_frame_skip = self.odd_frame_skip;
        self.fetch_bus = None;
        if self.io_decay > 0 {
            self.io_decay -= 1;
            if self.io_decay == 0 {
                self.io_latch = 0;
            }
        }
        let render_line = self.scanline < 240 || self.scanline == pre_render;
        let cur = self.rendering_enabled();
        // rend_pipe[i] is the rendering state 5 - i dots ago.
        let active = self.rend_pipe[5 - BG_DELAY] && self.rend_pipe[5 - BG_DELAY + 1];
        let sprite_active = self.rend_pipe[5 - SPRITE_DELAY];
        self.rend_run = if cur {
            self.rend_run.saturating_add(1)
        } else {
            0
        };
        self.rend_pipe = [
            self.rend_pipe[1],
            self.rend_pipe[2],
            self.rend_pipe[3],
            self.rend_pipe[4],
            cur,
        ];
        #[cfg(feature = "ppu-profile")]
        probe.enter(profile::Phase::Background);
        if render_line && active {
            self.background_dot(cart);
        } else if render_line
            && self.rend_pipe[5 - BG_DELAY + 1]
            && (self.dot == 328 || self.dot == 336)
        {
            // Rendering was enabled inside the BG enable pipeline: the preload coarse-X increment still happens.
            self.inc_coarse_x();
        }
        // The $2006 scroll glitch's landing dot (spec §3.2). The background fetches
        // above — including the hybrid read the `hyb` path performs at this same dot — have already
        // run and are not touched: only `v` and `t` are overwritten here. `new = t` is stable because
        // no CPU write fits in the two dots since the second `$2006` write.
        if let Some((landing, old)) = self.glitch_2006
            && self.dot == landing
        {
            let new = self.t;
            if landing == 257 {
                // The dot-256 increments ran on `old`; the dot-257 horizontal copy then fills the
                // coarse-X/fine-X/nametable bits from `t` (= `new`).
                self.v = old;
                self.inc_coarse_x();
                self.inc_y();
                self.v = ((self.v & !0x041F) | (new & 0x041F)) & new;
            } else {
                // A coarse-X increment: only the 0x041F bits come from `old + 1`, the rest from `t`.
                // (At dot 256 the coarse-Y increment touches no 0x041F bit, so it drops out.)
                self.v = old;
                self.inc_coarse_x();
                self.v = (new & !0x041F) | (self.v & new & 0x041F);
            }
            self.t = self.v;
            self.glitch_2006 = None;
        }
        if active {
            self.bg_idle = 0;
        } else {
            self.bg_idle = self.bg_idle.saturating_add(1);
            if self.bg_idle <= BG_IDLE_SHIFT
                && render_line
                && ((2..=257).contains(&self.dot) || (321..=337).contains(&self.dot))
            {
                self.shift();
            }
        }
        #[cfg(feature = "ppu-profile")]
        probe.enter(profile::Phase::Pixel);
        if self.scanline < 240 {
            if self.dot == 1 {
                // Dots 257-258 drained the previous visible line's pipeline: start empty.
                self.lag_len = 0;
            }
            if (1..=256).contains(&self.dot) {
                self.output_pixel();
            } else if self.dot == 257 || self.dot == 258 {
                // Pixels 254 and 255 are still inside the two-dot pipeline: their greyscale/emphasis
                // lands on dots 257 and 258.
                self.finalize_lag_pixel();
            }
        }
        #[cfg(feature = "ppu-profile")]
        probe.enter(profile::Phase::Sprite);
        if self.sprite_active_prev && !sprite_active && render_line {
            let seed = self.oam_corrupt_seed();
            self.oam_corrupt_pending = Some(seed);
        }
        if render_line
            && sprite_active
            && let Some(seed) = self.oam_corrupt_pending.take()
        {
            let base = usize::from(seed) * 8;
            if base != 0 {
                for i in 0..8 {
                    self.oam[base + i] = self.oam[i];
                }
                self.oam2[usize::from(seed)] = self.oam2[0];
            }
        }
        self.sprite_active_prev = sprite_active;
        if render_line && sprite_active {
            self.sprite_eval_dot();
            self.sprite_fetch_dot(cart);
        }
        if render_line && self.dot == 339 && sprite_active && self.rend_run >= COUNT_RUN {
            self.sprites_start_counting();
        }
        #[cfg(feature = "ppu-profile")]
        probe.enter(profile::Phase::Booktail);
        // Both of these happen on dot 1 only, so the dot test comes first: one compare per dot instead
        // of two scanline compares.
        if self.dot == 1 {
            if self.scanline == vblank {
                if self.suppress_vbl {
                    self.suppress_vbl = false;
                } else {
                    self.status |= 0x80;
                    if self.ctrl & 0x80 != 0 {
                        self.nmi_out = true;
                    }
                }
                self.frame_ready = true;
            } else if self.scanline == pre_render {
                self.status &= 0x1F;
                // The same signal that clears the flags clears the reset signal the write warm-up
                // gates the registers on (spec §2.3): no per-region cycle constant, and
                // region-correct by construction.
                self.warmup = false;
            }
        }
        // The 2C07 refreshes OAM during lines 265-310 whether rendering is on or not:
        // one read per row per line, so a row already stale when the window opens decays here
        // exactly as Mesen's evaluation reads would, and fresh rows stay fresh. `oam_pal_refresh` is
        // derived (`oam_decay && PAL`), so this is a single false test on the default path and on
        // NTSC even with the quirk on.
        if self.quirks.oam_pal_refresh && self.dot == 1 && (265..=310).contains(&self.scanline) {
            for row in 0..32u8 {
                self.oam_read_live(row * 8);
            }
        }
        if let Some(b) = self.fetch_bus {
            self.bus_last = b;
        }
        if self.read_pending > 0 {
            self.read_pending -= 1;
            if self.read_pending == 0 {
                let n = self.dot + 1;
                if ((2..=257).contains(&n) || (321..=337).contains(&n))
                    && matches!((n - 1) % 8, 4 | 6)
                {
                    self.ale_latch = Some((n, self.bus_last));
                }
                self.read_buffer = self.fetch_bus.unwrap_or(self.bus_last);
                if self.rendering_enabled() {
                    self.inc_coarse_x();
                    self.inc_y();
                }
            }
        }
        self.dot += 1;
        // The skip dot is the rarest condition, so it is tested first: the geometry loads and the
        // odd-frame test then cost nothing on the other 340 dots.
        // the skip samples the rendering-enabled state one dot late (the state
        // during dot 338, `rend_pipe[3]`), not `$2001` as of dot 339 (blargg ppu_vbl_nmi
        // 10-even_odd_timing subtests 2-5; Mesen2 NesPpu: `_renderingEnabled` lags `$2001` by one
        // dot and that delayed flag is what the cycle-339 skip tests).
        if self.dot == 340
            && odd_frame_skip
            && self.scanline == pre_render
            && self.frame & 1 == 1
            && self.rend_pipe[3]
        {
            self.dot = DOTS_PER_LINE;
        }
        if self.dot == DOTS_PER_LINE {
            self.dot = 0;
            self.scanline += 1;
            if self.scanline == lines {
                self.scanline = 0;
                self.frame += 1;
            }
        }
        #[cfg(feature = "ppu-profile")]
        probe.enter(profile::Phase::Bookhead);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_aligned_offsets_initial_dot() {
        for phase in 0..=3 {
            let ppu = Ppu::new_aligned(phase);
            assert_eq!(ppu.dot, (phase as u16) % DOTS_PER_LINE);
            assert_eq!(ppu.scanline, 0);
            assert_eq!(ppu.frame, 0);
        }
    }

    /// The seed used by the power-on RAM tests.
    const RAM_SEED: u64 = 0x1234_5678_9ABC_DEF0;

    fn ram_bytes(p: &Ppu) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (p.vram.to_vec(), p.oam.to_vec(), p.oam2.to_vec())
    }

    #[test]
    fn power_on_ram_zero_is_the_constructed_state() {
        let fresh = Ppu::new_aligned(2);
        let mut ppu = Ppu::new_aligned(2);
        ppu.vram[5] = 9;
        ppu.oam[7] = 9;
        ppu.oam2[3] = 9;
        ppu.power_on_ram(RamPowerOn::Zero, RAM_SEED);
        assert_eq!(ram_bytes(&ppu), ram_bytes(&fresh));
        assert_eq!(ppu.palette, fresh.palette);
    }

    #[test]
    fn power_on_ram_ones_masks_the_missing_attribute_bits() {
        let mut ppu = Ppu::new();
        ppu.power_on_ram(RamPowerOn::Ones, 0);
        assert!(ppu.vram.iter().all(|&b| b == 0xFF));
        assert!(ppu.oam2.iter().all(|&b| b == 0xFF));
        for (i, &b) in ppu.oam.iter().enumerate() {
            assert_eq!(b, if i & 3 == 2 { 0xE3 } else { 0xFF }, "oam[{i}]");
        }
    }

    #[test]
    fn power_on_ram_random_is_deterministic_in_the_seed() {
        let make = |seed| {
            let mut p = Ppu::new();
            p.power_on_ram(RamPowerOn::Random, seed);
            p
        };
        let (a, b, c) = (make(RAM_SEED), make(RAM_SEED), make(RAM_SEED + 1));
        assert_eq!(ram_bytes(&a), ram_bytes(&b));
        assert_ne!(a.vram, c.vram);
        assert_ne!(a.oam, c.oam);
        assert_ne!(a.oam2, c.oam2);
        // Not a constant fill, and the attribute mask holds.
        assert!(a.vram.iter().any(|&x| x != a.vram[0]));
        assert!(
            a.oam
                .iter()
                .enumerate()
                .all(|(i, &x)| i & 3 != 2 || x & 0x1C == 0)
        );
        // Stream layout: nametable RAM first, from the PpuRam domain.
        let mut rng = derive(RAM_SEED, SeedDomain::PpuRam);
        let mut expect = [0u8; 4096];
        rng.fill_bytes(&mut expect);
        assert_eq!(a.vram, expect);
    }

    #[test]
    fn power_on_ram_leaves_palette_alone() {
        for mode in [RamPowerOn::Zero, RamPowerOn::Ones, RamPowerOn::Random] {
            let fresh = Ppu::new();
            let mut ppu = Ppu::new();
            ppu.power_on_ram(mode, RAM_SEED);
            assert_eq!(ppu.palette, fresh.palette, "{mode:?}");
        }
    }

    #[test]
    fn new_equals_new_aligned_zero() {
        let a = Ppu::new();
        let b = Ppu::new_aligned(0);
        assert_eq!(a.dot, b.dot);
        assert_eq!(a.scanline, b.scanline);
        assert_eq!(a.frame, b.frame);
        assert_eq!(a.ctrl, b.ctrl);
        assert_eq!(a.v, b.v);
        assert_eq!(a.lines_per_frame, b.lines_per_frame);
        assert_eq!(a.vblank_line, b.vblank_line);
        assert_eq!(a.pre_render_line, b.pre_render_line);
        assert_eq!(a.odd_frame_skip, b.odd_frame_skip);
        assert_eq!(a.oam, b.oam);
        assert_eq!(a.framebuffer, b.framebuffer);
    }

    /// `blargg_ppu_tests_2005.09.15b/power_up_palette` fails if `$3F00-$3F1F` is not this table at
    /// power-on, and a zeroed palette also blanks the boot frames of real games.
    #[test]
    fn palette_ram_holds_the_power_up_table() {
        let ppu = Ppu::new();
        assert_eq!(ppu.palette, POWER_UP_PALETTE);
        // The 6-bit values, the `$3F10`/`$3F14` mirrors and the low-6-bit store all come from the
        // same table, so a read of the boot state has to agree with it.
        assert_eq!(ppu.palette[palette_index(0x3F00)], POWER_UP_PALETTE[0]);
        assert_eq!(ppu.palette[palette_index(0x3F10)], POWER_UP_PALETTE[0]);
        assert_eq!(ppu.palette[palette_index(0x3F14)], POWER_UP_PALETTE[4]);
        assert!(POWER_UP_PALETTE.iter().all(|v| v & 0xC0 == 0));
    }
}
