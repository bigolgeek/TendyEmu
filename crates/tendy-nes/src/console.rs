//! Console configuration: the hardware-behaviour toggles a run can opt into,
//! the PPU-facing subset of them, and the bundle the `Nes` constructors take.
//!
//! Two rules make this module safe to land before any of the behaviours it will switch on:
//!
//! * **The default is today's behaviour, byte for byte.** [`Quirks::default()`] is the hybrid
//!   current `main` implements — no PPU reset when the reset button is pressed, `$2004` readable,
//!   no write warm-up, no OAM decay, neither scroll glitch — and [`PpuQuirks::default()`] is the
//!   same set as the PPU sees it. Each field is read where its behaviour lives, so a run that never
//!   builds a non-default `Quirks` emulates exactly what it did before.
//! * **Quirks are configuration, not emulated state.** They are never written to a save state
//!   (`Nes::save_state`/`Ppu::save_state` do not touch them), so a state records what the machine
//!   was doing and not which hardware the user asked to emulate. Keeping the flags consistent
//!   across a save/load is the caller's job; movies record them (#131c).
//!
//! The split exists because the two halves of the machine need different sets: `Nes` owns the
//! reset button, so `ppu_reset_on_reset` lives only in [`Quirks`], while the register, rendering and
//! OAM behaviours live in [`PpuQuirks`] where the PPU can read them without going through the bus.

use crate::cart::Region;
use crate::nes::PowerOn;

/// The prefix every FM2 header key this crate records about the console starts with.
/// FM2 allows unknown header keys — FCEUX, BizHawk and Mesen ignore them — so the
/// keys are namespaced rather than taken from the format's own vocabulary.
pub const HEADER_PREFIX: &str = "nesrust_";

/// The opt-in hardware behaviours a console can have (revisions, PPU glitches).
///
/// Every field is a toggle away from the default, which is the behaviour the baselines pin. The
/// defaults are therefore not `bool::default()` for all fields and are written out by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quirks {
    /// The reset button also resets the PPU: `$2000`/`$2001` cleared, the `$2005`/`$2006` latch
    /// reset, the `$2007` read buffer cleared. Off = today's behaviour: the reset
    /// button never touches the PPU, which is what the Famicom and both top-loaders do.
    pub ppu_reset_on_reset: bool,
    /// `$2004` reads return OAM. Off models the early-PPU models where the register is write-only
    /// and a read returns the open-bus latch.
    pub ppu_2004_readable: bool,
    /// Writes to `$2000`/`$2001`/`$2005`/`$2006` are ignored until the `$2000`... reset signal is
    /// cleared at the first pre-render line after power-on or PPU reset (~29658 CPU clocks on
    /// NTSC). Off = no warm-up.
    pub ppu_warmup: bool,
    /// OAM (sprite) DRAM decays when rendering has been off for a while.
    pub oam_decay: bool,
    /// A `$2006` second write that lands on the same dot as a scroll increment copies a garbled
    /// address into `v`, producing a wrong-scroll frame.
    pub scroll_glitch_2006: bool,
    /// A first write to `$2000`/`$2005`/`$2006` while rendering is enabled garbles the `t` update.
    pub first_write_glitch: bool,
    /// The console is a NES-101 top-loader: bit 2 of a `$4016` read is not driven (the Famicom
    /// microphone line is absent), so it floats and keeps the open-bus bit instead of reading 0.
    /// Composes with `famicom_port` by OR-ing mask bits.
    pub nes101_port: bool,
    /// Famicom-style port: bits 3-4 of a `$4016` read are not driven (the `$4017`-only D3/D4 lines
    /// are not wired to port 1), so they float and keep the open-bus bits.
    pub famicom_port: bool,
}

impl Default for Quirks {
    /// Today's behaviour, which is not `bool::default()` for every field: `$2004` is readable.
    fn default() -> Quirks {
        Quirks {
            ppu_reset_on_reset: false,
            ppu_2004_readable: true,
            ppu_warmup: false,
            oam_decay: false,
            scroll_glitch_2006: false,
            first_write_glitch: false,
            nes101_port: false,
            famicom_port: false,
        }
    }
}

impl Quirks {
    /// The subset the PPU reads, resolved against the console's video timing.
    ///
    /// `oam_pal_refresh` is the one derived field: OAM rows are refreshed on scanlines 265-310,
    /// which PAL has and NTSC does not, so the PPU's decay bookkeeping needs to know whether the
    /// refresh window exists. Resolving it here — once, at construction — keeps the PPU from
    /// having to ask about the region on every decay check.
    pub fn ppu(self, region: Region) -> PpuQuirks {
        PpuQuirks {
            ppu_2004_readable: self.ppu_2004_readable,
            ppu_warmup: self.ppu_warmup,
            oam_decay: self.oam_decay,
            oam_pal_refresh: self.oam_decay && region == Region::Pal,
            scroll_glitch_2006: self.scroll_glitch_2006,
            first_write_glitch: self.first_write_glitch,
        }
    }

    /// The open-bus mask of a `$4016` read: the bits the controller port does not drive, which
    /// keep the last bus byte. `0xE0` (bits 5-7) by default; `nes101-port` adds `0x04` and
    /// `famicom-port` adds `0x18`. `$4017` always uses `0xE0`.
    pub fn port1_open_bus_mask(&self) -> u8 {
        let mut mask = 0xE0;
        if self.nes101_port {
            mask |= 0x04;
        }
        if self.famicom_port {
            mask |= 0x18;
        }
        mask
    }

    /// Every quirk name `--quirks` / `--quirk` accept and a movie header records,
    /// in the canonical order the error messages and the header value use.
    ///
    /// Each name moves one toggle away from [`Quirks::default()`] and no name moves one back,
    /// which is what lets a `--console` preset and any number of names compose in either order.
    pub const NAMES: [&'static str; 8] = [
        "ppu-reset",
        "no-2004-read",
        "ppu-warmup",
        "oam-decay",
        "scroll-glitch",
        "first-write-glitch",
        "nes101-port",
        "famicom-port",
    ];

    /// The FM2 header key a recording uses for the quirks it ran with.
    pub const HEADER_KEY: &'static str = "nesrust_quirks";

    /// The field a quirk name controls, and the value that means "on" for it (`false` for
    /// `no-2004-read`, whose field is the inverse). `None` when `name` is not a quirk. The one
    /// place a name maps to a field: `set_name`, `clear_name` and `names` all go through it.
    fn slot(&mut self, name: &str) -> Option<(&mut bool, bool)> {
        Some(match name {
            "ppu-reset" => (&mut self.ppu_reset_on_reset, true),
            "no-2004-read" => (&mut self.ppu_2004_readable, false),
            "ppu-warmup" => (&mut self.ppu_warmup, true),
            "oam-decay" => (&mut self.oam_decay, true),
            "scroll-glitch" => (&mut self.scroll_glitch_2006, true),
            "first-write-glitch" => (&mut self.first_write_glitch, true),
            "nes101-port" => (&mut self.nes101_port, true),
            "famicom-port" => (&mut self.famicom_port, true),
            _ => return None,
        })
    }

    /// Set the toggle `name` names, returning false when it names no quirk.
    pub fn set_name(&mut self, name: &str) -> bool {
        match self.slot(name) {
            Some((field, on)) => *field = on,
            None => return false,
        }
        true
    }

    /// Clear the toggle `name` names — the inverse of [`Quirks::set_name`] — returning false when
    /// it names no quirk.
    ///
    /// The settings overlay needs this: a quirk is a row that turns on *and* off, while a
    /// `--quirks` name only ever turns one on, so the mapping from a name to its toggle belongs
    /// here next to `set_name` rather than duplicated in the menu.
    pub fn clear_name(&mut self, name: &str) -> bool {
        match self.slot(name) {
            Some((field, on)) => *field = !on,
            None => return false,
        }
        true
    }

    /// The names of the toggles this set has moved away from the default, in
    /// [`Quirks::NAMES`] order — the set's spelling as a `--quirks` value.
    pub fn names(&self) -> Vec<&'static str> {
        // `slot` takes `&mut self`; `Quirks` is `Copy`, so read through a copy.
        let mut copy = *self;
        Self::NAMES
            .into_iter()
            .filter(|name| copy.slot(name).is_some_and(|(field, on)| *field == on))
            .collect()
    }

    /// The header keys this set records: one, [`Quirks::HEADER_KEY`], with the names of the
    /// toggles that are on — empty for the default console.
    pub fn to_header(&self) -> Vec<(String, String)> {
        vec![(Quirks::HEADER_KEY.to_owned(), self.names().join(","))]
    }

    /// The quirks an FM2 header names. A header without [`Quirks::HEADER_KEY`] — every movie
    /// recorded before those keys existed — is [`Quirks::default()`], today's behaviour.
    ///
    /// Keys this build does not know are ignored, so a movie from a newer build still replays
    /// with everything both sides agree on; a name this build does not know is an error, because
    /// dropping it would run a console the recording was not made on.
    pub fn from_header(header: &[(String, String)]) -> Result<Quirks, String> {
        let mut quirks = Quirks::default();
        for (key, value) in header {
            if key != Quirks::HEADER_KEY {
                continue;
            }
            // A repeated key keeps the last value, like every single-valued command-line flag.
            quirks = Quirks::default();
            for name in value.split(',') {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if !quirks.set_name(name) {
                    return Err(format!(
                        "{} {name:?} is not a quirk name ({})",
                        Quirks::HEADER_KEY,
                        Quirks::NAMES.join(", ")
                    ));
                }
            }
        }
        Ok(quirks)
    }
}

/// The [`Quirks`] the PPU reads, as `Ppu::quirks`. Set by the `Nes` constructors; a `Ppu` built on
/// its own (`Ppu::new()`, `Ppu::new_aligned()`, as the PPU tests do) gets the default set, which is
/// today's behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpuQuirks {
    /// `$2004` reads return OAM (default `true`).
    pub ppu_2004_readable: bool,
    /// `$2000`/`$2001`/`$2005`/`$2006` writes are ignored until the first pre-render line.
    pub ppu_warmup: bool,
    /// OAM rows decay while rendering is off.
    pub oam_decay: bool,
    /// The PAL refresh window (scanlines 265-310) refreshes every OAM row. Derived from
    /// [`Quirks::oam_decay`] and the region, so it is only ever `true` where `oam_decay` is.
    pub oam_pal_refresh: bool,
    /// The `$2006` second-write scroll glitch.
    pub scroll_glitch_2006: bool,
    /// The first-write scroll glitch.
    pub first_write_glitch: bool,
}

impl Default for PpuQuirks {
    /// Matches [`Quirks::default()`] with no region-specific refresh, i.e. NTSC.
    fn default() -> PpuQuirks {
        PpuQuirks {
            ppu_2004_readable: true,
            ppu_warmup: false,
            oam_decay: false,
            oam_pal_refresh: false,
            scroll_glitch_2006: false,
            first_write_glitch: false,
        }
    }
}

/// Everything a console comes up with: the power-on options and the hardware
/// quirks. `ConsoleConfig::default()` is a plain default console, so a caller that
/// only wants the options keeps writing `PowerOn` and a caller that wants both passes this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsoleConfig {
    pub power_on: PowerOn,
    pub quirks: Quirks,
}

impl ConsoleConfig {
    /// A console with these power-on options and today's hardware behaviour.
    pub fn new(power_on: PowerOn) -> ConsoleConfig {
        ConsoleConfig {
            power_on,
            quirks: Quirks::default(),
        }
    }

    /// The same power-on options with quirks from the caller.
    pub fn with_quirks(power_on: PowerOn, quirks: Quirks) -> ConsoleConfig {
        ConsoleConfig { power_on, quirks }
    }

    /// Every FM2 header key this configuration records, power-on options first.
    pub fn to_header(&self) -> Vec<(String, String)> {
        let mut header = self.power_on.to_header();
        header.extend(self.quirks.to_header());
        header
    }

    /// The console an FM2 header names, or `None` when it names none.
    ///
    /// `None` is the answer for every movie recorded before those keys existed: FM2 has no such keys,
    /// so the header says nothing about the console and the caller keeps the one it already has —
    /// which is exactly the behaviour those movies had. When the header does carry a key the
    /// configuration is built from the header alone, with every absent field left at its default,
    /// so a movie from a build that recorded only some of the keys still replays.
    ///
    /// A value this build cannot read is an error naming the key, never a silent default: a
    /// replay on a console other than the recording's desyncs.
    pub fn from_header(header: &[(String, String)]) -> Result<Option<ConsoleConfig>, String> {
        if !header.iter().any(|(key, _)| key.starts_with(HEADER_PREFIX)) {
            return Ok(None);
        }
        Ok(Some(ConsoleConfig {
            power_on: PowerOn::from_header(header)?,
            quirks: Quirks::from_header(header)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::RamPowerOn;
    use crate::nes::PhaseChoice;

    #[test]
    fn defaults_are_todays_behaviour() {
        // Pinned field by field: a stray `#[derive(Default)]` must not turn `$2004` reads off.
        let quirks = Quirks::default();
        assert!(
            !quirks.ppu_reset_on_reset,
            "the reset button does not touch the PPU today"
        );
        assert!(quirks.ppu_2004_readable, "$2004 is readable today");
        assert!(!quirks.ppu_warmup);
        assert!(!quirks.oam_decay);
        assert!(!quirks.scroll_glitch_2006);
        assert!(!quirks.first_write_glitch);

        let ppu = PpuQuirks::default();
        assert_eq!(ppu, Quirks::default().ppu(Region::Ntsc));
        assert!(ppu.ppu_2004_readable);
        assert!(!ppu.oam_pal_refresh);
    }

    #[test]
    fn console_config_default_is_the_plain_console() {
        let config = ConsoleConfig::default();
        assert_eq!(config.power_on, PowerOn::default());
        assert_eq!(config.quirks, Quirks::default());
        assert_eq!(ConsoleConfig::new(PowerOn::default()), config);
    }

    #[test]
    fn ppu_subset_carries_every_ppu_behaviour() {
        let all = Quirks {
            ppu_reset_on_reset: true,
            ppu_2004_readable: false,
            ppu_warmup: true,
            oam_decay: true,
            scroll_glitch_2006: true,
            first_write_glitch: true,
            nes101_port: true,
            famicom_port: true,
        };
        let nt = all.ppu(Region::Ntsc);
        assert!(!nt.ppu_2004_readable);
        assert!(nt.ppu_warmup);
        assert!(nt.oam_decay);
        assert!(!nt.oam_pal_refresh, "NTSC has no 265-310 refresh window");
        assert!(nt.scroll_glitch_2006);
        assert!(nt.first_write_glitch);
        // `ppu_reset_on_reset` stays Nes-level: it is not part of the PPU's set.
        assert!(
            all.ppu(Region::Pal).oam_pal_refresh,
            "PAL refreshes OAM rows 265-310"
        );
        assert!(
            !all.ppu(Region::Dendy).oam_pal_refresh,
            "Dendy is PAL-speed but the refresh window follows Region::Pal (spec §3.1)"
        );
    }

    #[test]
    fn pal_refresh_needs_oam_decay() {
        let on = Quirks {
            oam_decay: true,
            ..Quirks::default()
        };
        assert!(on.ppu(Region::Pal).oam_pal_refresh);
        assert!(!on.ppu(Region::Ntsc).oam_pal_refresh);
        let off = Quirks::default();
        assert!(
            !off.ppu(Region::Pal).oam_pal_refresh,
            "no decay, no refresh window"
        );
    }

    #[test]
    fn quirks_survive_a_copy() {
        // `Copy` is what lets the constructors hand the set to both the `Nes` and the `Ppu`
        // without borrow gymnastics, and `PowerOn` is `Copy` for the same reason.
        let config = ConsoleConfig::with_quirks(
            PowerOn {
                ram: RamPowerOn::Ones,
                phase: PhaseChoice::Exact(2),
                ..PowerOn::default()
            },
            Quirks {
                ppu_warmup: true,
                ..Quirks::default()
            },
        );
        let copy = config;
        assert_eq!(copy, config);
        assert!(copy.quirks.ppu_warmup);
        assert_eq!(copy.power_on.ram, RamPowerOn::Ones);
        assert_eq!(copy.power_on.phase, PhaseChoice::Exact(2));
    }

    // -----------------------------------------------------------------------------------------
    // The quirks and the console as FM2 header keys.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn every_quirk_combination_round_trips_through_the_header() {
        // All 256 subsets of the eight toggles: the header value must name exactly the toggles that
        // are on, and read back to the same set.
        for bits in 0u16..256 {
            let all = Quirks {
                ppu_reset_on_reset: bits & 1 != 0,
                ppu_2004_readable: bits & 2 == 0,
                ppu_warmup: bits & 4 != 0,
                oam_decay: bits & 8 != 0,
                scroll_glitch_2006: bits & 16 != 0,
                first_write_glitch: bits & 32 != 0,
                nes101_port: bits & 64 != 0,
                famicom_port: bits & 128 != 0,
            };
            let header = all.to_header();
            assert_eq!(
                Quirks::from_header(&header),
                Ok(all),
                "{all:?} -> {header:?}"
            );
            assert_eq!(all.names().len(), bits.count_ones() as usize, "{all:?}");
        }
    }

    #[test]
    fn the_quirk_names_are_the_flag_names_in_a_fixed_order() {
        // One table for the flags, the error messages and the header value.
        assert_eq!(
            Quirks::NAMES.join(", "),
            "ppu-reset, no-2004-read, ppu-warmup, oam-decay, scroll-glitch, first-write-glitch, nes101-port, famicom-port"
        );
        let all = Quirks {
            ppu_reset_on_reset: true,
            ppu_2004_readable: false,
            ppu_warmup: true,
            oam_decay: true,
            scroll_glitch_2006: true,
            first_write_glitch: true,
            nes101_port: true,
            famicom_port: true,
        };
        assert_eq!(all.names(), Quirks::NAMES.to_vec());
        for name in Quirks::NAMES {
            let mut quirks = Quirks::default();
            assert!(quirks.set_name(name), "{name}");
            assert_eq!(
                quirks.names(),
                vec![name],
                "{name} moved more than one toggle"
            );
            assert!(!quirks.set_name("no-such-quirk"));
        }
        // The default console names nothing: the header value is empty, and the key is still
        // written so the recording says so explicitly.
        assert_eq!(Quirks::default().names(), Vec::<&str>::new());
        assert_eq!(
            Quirks::default().to_header(),
            vec![(Quirks::HEADER_KEY.to_owned(), String::new())]
        );
    }

    #[test]
    fn a_header_without_the_quirks_key_is_the_default() {
        assert_eq!(Quirks::from_header(&[]), Ok(Quirks::default()));
        // A default recording's empty value names no quirk, so it round-trips too.
        let empty = vec![(Quirks::HEADER_KEY.to_owned(), String::new())];
        assert_eq!(Quirks::from_header(&empty), Ok(Quirks::default()));
        // Space after a comma is accepted, so a hand-edited movie still reads.
        let listed = vec![(
            Quirks::HEADER_KEY.to_owned(),
            "oam-decay, ppu-warmup".to_owned(),
        )];
        assert_eq!(
            Quirks::from_header(&listed),
            Ok(Quirks {
                oam_decay: true,
                ppu_warmup: true,
                ..Quirks::default()
            })
        );
    }

    #[test]
    fn an_unknown_quirk_name_in_a_header_is_an_error() {
        // Dropping it would run a console the recording was not made on.
        let header = vec![(
            Quirks::HEADER_KEY.to_owned(),
            "oam-decay,no-such-quirk".to_owned(),
        )];
        let err = Quirks::from_header(&header).expect_err("an unknown name must be refused");
        assert!(err.contains("no-such-quirk"), "{err}");
        assert!(
            err.contains("first-write-glitch"),
            "the error lists the names: {err}"
        );
    }

    #[test]
    fn a_console_header_carries_both_halves_and_needs_one_of_its_keys() {
        let config = ConsoleConfig::with_quirks(
            PowerOn {
                ram: RamPowerOn::Random,
                seed: 3,
                ..PowerOn::default()
            },
            Quirks {
                oam_decay: true,
                ..Quirks::default()
            },
        );
        let header = config.to_header();
        assert_eq!(header.len(), 7, "six power-on keys and one quirks key");
        assert!(
            header.iter().all(|(key, _)| key.starts_with(HEADER_PREFIX)),
            "{header:?}"
        );
        assert_eq!(ConsoleConfig::from_header(&header), Ok(Some(config)));
        // No key at all: the header names no console, so the caller keeps the one it has.
        assert_eq!(
            ConsoleConfig::from_header(&[("version".to_owned(), "3".to_owned())]),
            Ok(None)
        );
        // A value the build cannot read is an error, never a silent default.
        let bad = vec![(Quirks::HEADER_KEY.to_owned(), "no-such-quirk".to_owned())];
        assert!(ConsoleConfig::from_header(&bad).is_err());
    }

    /// The settings overlay turns a quirk row off again, so `clear_name` has to be the exact
    /// inverse of `set_name` for every name the file format knows.
    #[test]
    fn every_quirk_name_can_be_cleared_again() {
        for name in Quirks::NAMES {
            let mut quirks = Quirks::default();
            assert!(quirks.set_name(name), "{name}");
            assert_ne!(quirks, Quirks::default(), "{name} set no toggle");
            assert_eq!(quirks.names(), vec![name], "{name}");
            assert!(quirks.clear_name(name), "{name}");
            assert_eq!(quirks, Quirks::default(), "{name} did not clear");
        }
        // An unknown name is refused rather than silently ignored, like `set_name`.
        let mut quirks = Quirks::default();
        assert!(!quirks.clear_name("no-such-quirk"));
        assert_eq!(quirks, Quirks::default());
    }
}
