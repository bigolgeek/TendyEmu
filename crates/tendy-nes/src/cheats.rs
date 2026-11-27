//! Raw-RAM cheats: freeze and poke the 2KB work RAM ($0000-$07FF) at the core level - the
//! way FCEUX's Cheat Search (https://fceux.com/web/help/CheatSearch.html) and Mesen's cheat tools
//! (https://www.mesen.ca/docs/tools.html) expose them. The value search over a RAM snapshot that
//! drives those tools lives with the RAM watch, [`crate::watch::search_between`] and
//! [`crate::watch::narrow_between`], so both tools share one `Condition`.
//!
//! The engine is core-level and the frontend stays thin: everything is driven through the
//! `cheat_*` methods on [`SysBus`](crate::bus::SysBus). A *poke* writes a byte once. A *freeze*
//! pokes it and then re-asserts the value after every later CPU write to that RAM address (or any
//! of its mirrors), so the guest cannot defeat it.
//!
//! Frozen values are enforced on the write path (`access_write`), not per frame:
//!
//! * The game reads freshly written RAM immediately (read-modify-write opcodes, mid-frame tables,
//!   $2007 tile-table construction), so per-write enforcement keeps the frozen value visible from
//!   the very next cycle, which is how FCEUX freezes behave.
//! * It needs no frame-boundary hook in `Nes::run_frame` and covers every writer there is: the
//!   guest's own stores are the only writes that reach work RAM, and they all flow through one
//!   code path.
//! * The event log stays a record of real CPU bus traffic - it keeps recording the value the CPU
//!   drove on the data bus, which is what the log means. No new `EventKind` is needed; if the
//!   frontend later wants frozen overrides visible in the history viewer, that is a new event
//!   kind layered on top, not a change of this engine.
//!
//! For frontends that prefer frame-boundary application (snapshot-then-reapply), [`RawRamCheats`]
//! also exposes [`RawRamCheats::apply_all`], and [`SysBus::cheat_apply`](crate::bus::SysBus)
//! wraps it; both kinds of freeze coexist on the same table.
//!
//! Cheats are deliberately not part of save states (the same rule `EventLog` follows): a
//! save/load restores the guest's memory and latches only; the cheat set is owned by the
//! frontend session that created it.

/// The frozen-address table. Sized for the 2KB page, so enforcement is one indexed lookup - no
/// allocation, no map, no lock. With no cheat armed the whole hot-path cost is the single
/// `is_armed()` branch in `access_write`, the same one-predictable-branch principle the event
/// log's `enabled` flag uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRamCheats {
    /// RAM index (idx = addr & 0x07FF): `Some(value)` pins `ram[idx]` at `value` after every write.
    frozen: [Option<u8>; 0x0800],
    /// Frozen slot count; 0 keeps the engine disarmed.
    count: usize,
}

impl RawRamCheats {
    /// An idle engine: nothing armed, zero hot-path cost.
    pub fn new() -> RawRamCheats {
        RawRamCheats {
            frozen: [None; 0x0800],
            count: 0,
        }
    }

    /// Whether at least one address is frozen; `access_write` gates on this one bool.
    pub fn is_armed(&self) -> bool {
        self.count != 0
    }

    /// How many addresses are frozen.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether this engine is empty (used by tests and diagnostics).
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The frozen value for `idx`, if it is frozen.
    pub fn frozen_value(&self, idx: usize) -> Option<u8> {
        if idx < 0x0800 { self.frozen[idx] } else { None }
    }

    /// Freeze `idx` (a RAM index in 0..0x800, already masked by the caller) at `val`. Freezing an
    /// already-frozen address re-freezes it at the new value without double-counting.
    pub fn freeze(&mut self, idx: usize, val: u8) {
        debug_assert!(
            idx < 0x0800,
            "freeze index must be masked into the 2KB page"
        );
        let idx = idx & 0x07FF;
        if self.frozen[idx].is_none() {
            self.count += 1;
        }
        self.frozen[idx] = Some(val);
    }

    /// Stop freezing `idx`. Returns true when it had been frozen. Unfreezing the last address
    /// disarms the engine, so `access_write`'s hot branch goes back to costing nothing.
    pub fn unfreeze(&mut self, idx: usize) -> bool {
        let idx = idx & 0x07FF;
        if self.frozen[idx].take().is_some() {
            self.count -= 1;
            true
        } else {
            false
        }
    }

    /// Clear every freeze and disarm the engine.
    pub fn clear(&mut self) {
        self.frozen = [None; 0x0800];
        self.count = 0;
    }

    /// The write-path hook: when `idx` is frozen, rewrite `ram[idx]` back to its frozen value
    /// right after the write. The caller gates on [`is_armed`], so with no cheats armed this never
    /// runs. Marked cold - both because it only runs when armed and to keep the always-run RAM
    /// store out of line.
    #[cold]
    #[inline(never)]
    pub fn enforce_ram(&self, ram: &mut [u8; 0x0800], idx: usize) {
        if let Some(v) = self.frozen[idx & 0x07FF] {
            ram[idx & 0x07FF] = v;
        }
    }

    /// Re-assert every frozen value over a whole RAM page in one pass - the frame-boundary
    /// application some frontends prefer (snapshot, let the frame run, reapply). A disarmed engine
    /// leaves the page untouched and returns without touching anything.
    pub fn apply_all(&self, ram: &mut [u8; 0x0800]) {
        if !self.is_armed() {
            return;
        }
        for (idx, slot) in self.frozen.iter().enumerate() {
            if let Some(v) = *slot {
                ram[idx] = v;
            }
        }
    }
}

impl Default for RawRamCheats {
    fn default() -> Self {
        RawRamCheats::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake bus with nothing but the 2KB work RAM, mirroring the RAM arm of
    /// `SysBus::access_write` exactly: the guest's store always lands, then the cheat hook runs
    /// when the engine is armed. This is the seam the real bus hooks into.
    struct FakeBus {
        ram: Box<[u8; 0x0800]>,
        cheats: RawRamCheats,
    }

    impl FakeBus {
        fn new() -> FakeBus {
            FakeBus {
                ram: Box::new([0x11; 0x0800]),
                cheats: RawRamCheats::new(),
            }
        }

        /// Poke: the one-time write the frontend asks for, same as `SysBus::cheat_poke`.
        fn cheat_poke(&mut self, idx: usize, val: u8) {
            self.ram[idx & 0x07FF] = val;
        }

        /// Freeze: poke now, then arm the hook - same shape as `SysBus::cheat_freeze`.
        fn cheat_freeze(&mut self, idx: usize, val: u8) {
            self.cheats.freeze(idx & 0x07FF, val);
            self.ram[idx & 0x07FF] = val;
        }

        /// The guest's own store, via the same guard order the real bus uses.
        fn write(&mut self, idx: usize, val: u8) {
            self.ram[idx & 0x07FF] = val;
            if self.cheats.is_armed() {
                self.cheats.enforce_ram(&mut self.ram, idx);
            }
        }
    }

    #[test]
    fn poke_replaces_exactly_one_byte() {
        let mut bus = FakeBus::new();
        bus.cheat_poke(0x0305, 0x42);
        assert_eq!(bus.ram[0x0305], 0x42);
        assert_eq!(bus.ram[0x0304], 0x11);
        assert_eq!(bus.ram[0x0306], 0x11);
        // A poke alone must not arm the write hook - it is one-shot by definition.
        assert!(!bus.cheats.is_armed());
    }

    #[test]
    fn freeze_pokes_immediately_and_survives_a_guest_write() {
        let mut bus = FakeBus::new();
        bus.cheat_freeze(0x0305, 0x42);
        assert!(bus.cheats.is_armed());
        // The freeze itself lands at once.
        assert_eq!(bus.ram[0x0305], 0x42);
        // The guest tries to overwrite it; the frozen value persists.
        bus.write(0x0305, 0x99);
        assert_eq!(bus.ram[0x0305], 0x42);
        // ...and neighbours are untouched by the hook.
        assert_eq!(bus.ram[0x0304], 0x11);
        assert_eq!(bus.ram[0x0306], 0x11);
        assert_eq!(bus.cheats.len(), 1);
    }

    #[test]
    fn unfreezing_lets_writes_through_again() {
        let mut bus = FakeBus::new();
        bus.cheats.freeze(0x0100, 0xAA);
        bus.write(0x0100, 0x01);
        assert_eq!(bus.ram[0x0100], 0xAA);
        assert!(bus.cheats.unfreeze(0x0100));
        assert!(!bus.cheats.is_armed());
        bus.write(0x0100, 0x55);
        assert_eq!(bus.ram[0x0100], 0x55);
    }

    #[test]
    fn unfreezing_an_unfrozen_address_reports_false() {
        let mut bus = FakeBus::new();
        assert!(!bus.cheats.unfreeze(0x0200));
        bus.cheats.freeze(0x0200, 0x7);
        assert!(bus.cheats.unfreeze(0x0200));
        assert!(!bus.cheats.unfreeze(0x0200));
    }

    #[test]
    fn refreezing_does_not_double_count() {
        let mut bus = FakeBus::new();
        bus.cheats.freeze(0x0040, 0x10);
        bus.cheats.freeze(0x0040, 0x20);
        assert_eq!(bus.cheats.len(), 1);
        bus.write(0x0040, 0x30);
        assert_eq!(bus.ram[0x0040], 0x20);
        // A second address (an index above the page is masked into it, like the bus does before
        // every hook call) lands in the neighbouring slot.
        bus.cheat_freeze(0x0841, 0x99);
        assert_eq!(bus.cheats.len(), 2);
        assert_eq!(bus.cheats.frozen_value(0x0040), Some(0x20));
        assert_eq!(bus.cheats.frozen_value(0x0041), Some(0x99));
    }

    #[test]
    fn clear_disarms_everything() {
        let mut bus = FakeBus::new();
        bus.cheats.freeze(0x0012, 0x02);
        bus.cheats.freeze(0x0674, 0x03);
        bus.cheats.clear();
        assert!(bus.cheats.is_empty());
        assert!(!bus.cheats.is_armed());
        bus.write(0x0012, 0xAA);
        assert_eq!(bus.ram[0x0012], 0xAA);
    }

    #[test]
    fn apply_all_reasserts_every_frozen_value() {
        let mut bus = FakeBus::new();
        bus.cheats.freeze(0x0012, 0x02);
        bus.cheats.freeze(0x0674, 0x03);
        // A guest frame ran without the write hook (per-frame application style): both bytes move.
        bus.cheat_poke(0x0012, 0xBB);
        bus.cheat_poke(0x0674, 0xCC);
        bus.cheat_poke(0x0013, 0xDD);
        bus.cheats.apply_all(&mut bus.ram);
        assert_eq!(bus.ram[0x0012], 0x02);
        assert_eq!(bus.ram[0x0674], 0x03);
        assert_eq!(bus.ram[0x0013], 0xDD);
    }

    #[test]
    fn apply_all_is_a_noop_when_disarmed() {
        let mut bus = FakeBus::new();
        bus.cheat_poke(0x0012, 0xBB);
        bus.cheats.apply_all(&mut bus.ram);
        assert_eq!(bus.ram[0x0012], 0xBB);
    }
}
