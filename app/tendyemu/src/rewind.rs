//! Rewind (DESIGN §6.1, §6.3): a ring of `Machine::save_state` snapshots in memory, so one
//! implementation works for every core.
//!
//! Nothing here touches the disk. A snapshot is taken every [`SNAPSHOT_SPACING`] machine frames —
//! ten a second at any system's rate — and the ring keeps the newest ones inside a byte budget.
//! Stepping back pops the newest snapshot and loads it, which is exact to the byte: the ring stores
//! what the core wrote and hands the same bytes back.
//!
//! Snapshots are stored as the difference from the one after them when that is smaller, which is
//! usually true for a console that redraws most of its memory once a frame. The newest snapshot is
//! always whole, because a step back hands it out first and the chain has to start somewhere.
//!
//! The ring lives in a thread-local session rather than an `App` field: the app's struct belongs to
//! the shell, and every module keeps its own state this way. The session forgets itself when the
//! game changes or the machine's frame counter goes backwards (a reset), which is when a rewind
//! from the old timeline would be wrong.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use tendy_core::SystemKind;

use crate::app::App;
use crate::menu::{Row, RowAct, RowKind};

/// Machine frames between two snapshots: ten snapshots per second of game time.
pub const SNAPSHOT_SPACING: u64 = 6;

/// Snapshots one second of game time holds.
pub const SNAPSHOTS_PER_SEC: usize = 10;

/// Bytes of snapshot the ring may hold at once. A 32 KiB save chip Game Boy frame is ~100 KiB and
/// a Super NES frame ~250 KiB, so this holds ten seconds of either with room to spare.
pub const BYTE_BUDGET: usize = 128 * 1024 * 1024;

/// The span §6.2 asks for when the settings file says nothing (`rewind_seconds`).
pub const DEFAULT_SECONDS: u64 = 10;

/// Bytes of header a [`Entry::Patch`] spends on each run: a `u32` offset and a `u16` length.
const RUN_HEADER: usize = 6;

/// One held snapshot, in the form that costs fewer bytes.
///
/// Both variants answer the same question — *given the snapshot after me, what was I?* — which is
/// the direction [`Ring::pop`] walks.
#[derive(Clone, Debug)]
enum Entry {
    /// The snapshot's own bytes.
    Full(Vec<u8>),
    /// The older bytes of every run that differs from the snapshot after it, in wire order:
    /// `u32` offset, `u16` length, then that many bytes of the older snapshot.
    Patch(Vec<u8>),
}

impl Entry {
    /// Bytes this entry occupies.
    fn len(&self) -> usize {
        match self {
            Entry::Full(bytes) | Entry::Patch(bytes) => bytes.len(),
        }
    }

    /// The snapshot this entry came from, rebuilt from the one after it.
    fn into_snapshot(self, newer: &[u8]) -> Vec<u8> {
        match self {
            Entry::Full(bytes) => bytes,
            Entry::Patch(patch) => {
                let mut older = newer.to_vec();
                let mut at = 0;
                // A malformed run can only come from memory that was scribbled on after it was
                // written; skipping it leaves the rest of the frame intact rather than panicking
                // in a rewind.
                while at + RUN_HEADER <= patch.len() {
                    let offset = u32::from_le_bytes([
                        patch[at],
                        patch[at + 1],
                        patch[at + 2],
                        patch[at + 3],
                    ]) as usize;
                    let len = u16::from_le_bytes([patch[at + 4], patch[at + 5]]) as usize;
                    let start = at + RUN_HEADER;
                    let end = start + len;
                    if end > patch.len() {
                        break;
                    }
                    if offset + len <= older.len() {
                        older[offset..offset + len].copy_from_slice(&patch[start..end]);
                    }
                    at = end;
                }
                older
            }
        }
    }
}

/// The difference that turns `newer` back into `older`: the runs where the two differ, carrying
/// `older`'s bytes. Runs are found eight bytes at a time, so a run can carry up to seven unchanged
/// bytes at either end — worth a few bytes, and it still rebuilds the frame exactly. Runs are capped
/// at `u16::MAX` so a header stays six bytes; a longer run is written as several.
fn reverse_patch(older: &[u8], newer: &[u8]) -> Vec<u8> {
    debug_assert_eq!(older.len(), newer.len());
    let mut patch = Vec::new();
    let mut i = 0;
    while i < older.len() {
        let mut at = i;
        while at + 8 <= older.len() && older[at..at + 8] == newer[at..at + 8] {
            at += 8;
        }
        while at < older.len() && older[at] == newer[at] {
            at += 1;
        }
        if at == older.len() {
            break;
        }
        let start = at;
        while at + 8 <= older.len() && older[at..at + 8] != newer[at..at + 8] {
            at += 8;
        }
        while at < older.len() && older[at] != newer[at] {
            at += 1;
        }
        let mut written = start;
        while written < at {
            let len = (at - written).min(usize::from(u16::MAX));
            patch.extend_from_slice(&(written as u32).to_le_bytes());
            patch.extend_from_slice(&(len as u16).to_le_bytes());
            patch.extend_from_slice(&older[written..written + len]);
            written += len;
        }
        i = at;
    }
    patch
}

/// A first-in-first-out queue of snapshots, bounded by count and by total bytes.
///
/// The newest snapshot is held whole in [`Ring::head`], which is what [`Ring::pop`] hands out:
/// rewinding walks backwards through time one snapshot at a time, and every step is the newest
/// one. The older snapshots are [`Entry`]s behind it. Eviction drops from the front (the oldest).
#[derive(Clone, Debug)]
pub struct Ring {
    entries: VecDeque<Entry>,
    /// The newest snapshot, materialised.
    head: Option<Vec<u8>>,
    bytes: usize,
    cap: usize,
    budget: usize,
    /// Store an older snapshot as a difference from the one after it.
    delta: bool,
}

impl Ring {
    /// A ring holding at most `cap` snapshots and at most `budget` bytes of them.
    pub fn new(cap: usize, budget: usize) -> Ring {
        Ring {
            entries: VecDeque::new(),
            head: None,
            bytes: 0,
            cap,
            budget,
            delta: true,
        }
    }

    /// Snapshots the ring may hold.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Bytes of snapshot the ring may hold.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Snapshots held.
    pub fn len(&self) -> usize {
        self.entries.len() + usize::from(self.head.is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Bytes the held snapshots occupy, as they are stored: a whole frame plus the runs of every
    /// snapshot behind it.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether snapshots are stored as differences from the one after them.
    pub fn stores_deltas(&self) -> bool {
        self.delta
    }

    /// Ask for difference storage. Ignored once a snapshot is held: the entries already there
    /// describe a chain in the form they were written in.
    pub fn set_delta(&mut self, delta: bool) {
        if self.is_empty() {
            self.delta = delta;
        }
    }

    /// Keep `snap`, then evict the oldest snapshots until both limits hold.
    pub fn push(&mut self, snap: Vec<u8>) {
        let Some(older) = self.head.replace(snap) else {
            // The first snapshot is the chain's only anchor, so it is held whole.
            self.bytes = self.head.as_ref().map_or(0, Vec::len);
            return;
        };
        let entry = self.entry_for(&older, self.head.as_deref().unwrap_or_default());
        self.bytes =
            self.bytes - older.len() + entry.len() + self.head.as_ref().map_or(0, Vec::len);
        self.entries.push_back(entry);
        self.evict();
    }

    /// The cheapest exact form that turns `newer` back into `older`.
    ///
    /// A difference only pays when both snapshots are the same length (a save chip detected
    /// mid-run changes the payload's shape) and when it is smaller than the bytes it describes;
    /// otherwise the older frame is kept whole.
    fn entry_for(&self, older: &[u8], newer: &[u8]) -> Entry {
        if self.delta && older.len() == newer.len() {
            let patch = reverse_patch(older, newer);
            if patch.len() < older.len() {
                return Entry::Patch(patch);
            }
        }
        Entry::Full(older.to_vec())
    }

    /// The newest snapshot, taken off the ring, or `None` when it is empty.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let newest = self.head.take()?;
        self.bytes -= newest.len();
        if let Some(entry) = self.entries.pop_back() {
            self.bytes -= entry.len();
            let older = entry.into_snapshot(&newest);
            self.bytes += older.len();
            self.head = Some(older);
        }
        Some(newest)
    }

    /// Forget every snapshot.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.head = None;
        self.bytes = 0;
    }

    fn evict(&mut self) {
        // The newest snapshot is always kept, even alone and over budget: there is no useful
        // rewind buffer that restores nothing, and the next push evicts it anyway.
        while self.len() > 1 && (self.len() > self.cap || self.bytes > self.budget) {
            if let Some(front) = self.entries.pop_front() {
                self.bytes -= front.len();
            }
        }
    }
}

/// The session's ring, the cadence, and the game it belongs to.
#[derive(Clone, Debug)]
struct Session {
    /// The file the ring belongs to; a new file starts a new timeline.
    rom: Option<PathBuf>,
    ring: Ring,
    /// Seconds of game time asked for by `[app] rewind_seconds`.
    seconds: u64,
    /// Machine frames since the last snapshot, `0..SNAPSHOT_SPACING`.
    since: u64,
    /// The machine's own frame counter at the last look, to count frames actually run.
    last_frame: u64,
    /// Whether the player asked for snapshots.
    enabled: bool,
}

impl Session {
    fn new() -> Session {
        Session {
            rom: None,
            ring: Ring::new(DEFAULT_SECONDS as usize * SNAPSHOTS_PER_SEC, BYTE_BUDGET),
            seconds: DEFAULT_SECONDS,
            since: 0,
            last_frame: 0,
            enabled: true,
        }
    }

    /// A new timeline: forget the history and start counting from `frames`.
    fn restart(&mut self, rom: Option<PathBuf>, frames: u64) {
        self.rom = rom;
        self.ring.clear();
        self.since = 0;
        self.last_frame = frames;
    }

    /// Point the ring at a new span, dropping the history that was sized for the old one.
    fn set_seconds(&mut self, seconds: u64) {
        if seconds != self.seconds {
            self.seconds = seconds;
            self.ring = Ring::new(seconds as usize * SNAPSHOTS_PER_SEC, BYTE_BUDGET);
            self.since = 0;
        }
    }
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::new());
}

/// The span the settings ask for: `rewind_seconds`, or [`DEFAULT_SECONDS`] until #21 reads the file.
fn wanted_seconds(app: &App) -> u64 {
    let system = app.system().unwrap_or(SystemKind::Gb);
    app.setting(system, "rewind_seconds")
        .and_then(|word| word.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS)
}

/// A module row: the drawer sends its activation back to [`on_row`].
fn row(
    label: impl Into<String>,
    value: impl Into<String>,
    kind: RowKind,
    desc: &'static str,
) -> Row {
    Row {
        label: label.into(),
        value: value.into(),
        kind,
        desc,
        hotkey: "",
        requires: None,
        act: None,
    }
}

/// The rewind rows: the on/off switch and one step back.
pub fn rows(app: &App) -> Vec<Row> {
    if app.machine().is_none() {
        return Vec::new();
    }
    let (enabled, seconds, held) = SESSION.with(|cell| {
        let session = cell.borrow();
        (session.enabled, session.seconds, session.ring.len())
    });
    let toggle = if enabled {
        format!("On — {seconds} s")
    } else {
        "Off".to_string()
    };
    vec![
        row(
            "Rewind",
            toggle,
            RowKind::Toggle,
            "Keep a buffer of the last seconds",
        ),
        row(
            "Step back",
            format!("{held} held"),
            RowKind::Action,
            "Restore the newest snapshot",
        ),
    ]
}

/// Handles the rewind rows: index 0 toggles, index 1 steps back one snapshot.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    match index {
        0 => {
            if !matches!(act, RowAct::Activate | RowAct::Step(_)) {
                return false;
            }
            let seconds = wanted_seconds(app);
            SESSION.with(|cell| {
                let mut session = cell.borrow_mut();
                session.enabled = !session.enabled;
                session.set_seconds(seconds);
                if session.enabled {
                    session.since = 0;
                } else {
                    session.ring.clear();
                }
            });
            true
        }
        1 => {
            if !matches!(act, RowAct::Activate | RowAct::Step(_)) {
                return false;
            }
            step_back(app)
        }
        _ => false,
    }
}

/// Restores the newest snapshot. `false` when there is none, or when the core refuses it.
fn step_back(app: &mut App) -> bool {
    let Some(machine) = app.machine_mut() else {
        return false;
    };
    SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        let Some(snapshot) = session.ring.pop() else {
            return false;
        };
        if machine.load_state(&snapshot).is_err() {
            return false;
        }
        session.last_frame = machine.frames();
        session.since = 0;
        true
    })
}

/// One per-frame hook: take a snapshot every [`SNAPSHOT_SPACING`] frames the machine ran.
pub fn on_frame(app: &mut App) {
    let seconds = wanted_seconds(app);
    let rom = app.rom().map(PathBuf::from);
    let Some(machine) = app.machine_mut() else {
        SESSION.with(|cell| cell.borrow_mut().restart(None, 0));
        return;
    };
    let frames = machine.frames();
    SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        if session.rom != rom || frames < session.last_frame {
            // A new game, or a reset that started a new timeline.
            session.restart(rom.clone(), frames);
        }
        if !session.enabled {
            session.last_frame = frames;
            return;
        }
        session.set_seconds(seconds);
        let delta = frames.saturating_sub(session.last_frame);
        session.last_frame = frames;
        session.since += delta;
        if session.since >= SNAPSHOT_SPACING {
            session.since = 0;
            session.ring.push(machine.save_state());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cheap deterministic byte source: no crate, and a shape a test can name when it fails.
    fn noise(seed: &mut u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            *seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            out.push((*seed >> 33) as u8);
        }
        out
    }

    #[test]
    fn a_patch_rebuilds_the_older_frame_whatever_changed() {
        let mut seed = 0x5EED_1234_5678_9ABC;
        for len in [0usize, 1, 7, 8, 9, 300, 70_000] {
            let older = noise(&mut seed, len);
            for edits in [0, 1, 3, len / 2] {
                let mut newer = older.clone();
                for edit in 0..edits.min(len) {
                    let at = (edit * 7_919) % len.max(1);
                    newer[at] ^= 0xFF;
                }
                let patch = reverse_patch(&older, &newer);
                assert_eq!(
                    Entry::Patch(patch).into_snapshot(&newer),
                    older,
                    "len {len}, {edits} edits"
                );
            }
            // A frame that moved on entirely: the patch is the whole frame, which the ring refuses
            // in favour of storing it whole, but it still has to rebuild it.
            let newer = noise(&mut seed, len);
            let patch = reverse_patch(&older, &newer);
            assert_eq!(Entry::Patch(patch).into_snapshot(&newer), older);
        }
    }

    #[test]
    fn a_truncated_patch_does_not_panic_or_scribble_outside_the_frame() {
        let older = vec![0u8; 64];
        let newer = vec![0xFFu8; 64];
        let mut patch = reverse_patch(&older, &newer);
        patch.truncate(patch.len() - 3);
        let rebuilt = Entry::Patch(patch).into_snapshot(&newer);
        assert_eq!(rebuilt.len(), 64);
    }

    #[test]
    fn eviction_keeps_a_chain_that_still_walks_back() {
        // Dropping the oldest snapshot drops one difference and nothing else: the chain from the
        // newest down to the new oldest is still whole.
        let mut ring = Ring::new(3, 1 << 20);
        ring.set_delta(true);
        let frames: Vec<Vec<u8>> = (0..6u8)
            .map(|n| {
                let mut frame = vec![0u8; 4_096];
                frame[0] = n;
                frame[1_000] = n.wrapping_mul(3);
                frame
            })
            .collect();
        for frame in &frames {
            ring.push(frame.clone());
        }
        assert_eq!(ring.len(), 3);
        assert!(
            ring.bytes() < 3 * 4_096,
            "three differences, not three frames"
        );
        for n in (3..6).rev() {
            assert_eq!(ring.pop(), Some(frames[n].clone()), "snapshot {n}");
        }
        assert!(ring.pop().is_none());
    }

    #[test]
    fn a_difference_is_taken_only_when_it_is_smaller() {
        // Two frames that differ in every byte: the patch carries the same bytes plus six bytes a
        // run, so the ring must store the frame whole.
        let mut ring = Ring::new(10, 1 << 20);
        ring.set_delta(true);
        ring.push(vec![0u8; 100]);
        ring.push(vec![7u8; 100]);
        assert_eq!(ring.bytes(), 200, "two whole frames, not two patches");
        assert_eq!(ring.pop(), Some(vec![7u8; 100]));
        assert_eq!(ring.pop(), Some(vec![0u8; 100]));
    }
}
