//! Session history: every frame's pads and reset command since the session began,
//! plus a full `save_state()` keyframe every 600 frames, so an earlier frame can be reloaded and
//! replayed forward.
//!
//! WHY an input log instead of a snapshot per frame: one `save_state()` is ~136 KiB on a small
//! cartridge, so a snapshot per frame at ~60 Hz is ~8 MiB/s and ~480 MiB/min — a few minutes of
//! play would exhaust a 64 MiB budget, and a seek would have nothing left to load. The input log is
//! the FM2 per-frame shape, [`MovieFrame`]: three bytes per frame, ~10 KiB a minute. A keyframe
//! every [`DEFAULT_KEYFRAME_INTERVAL`] frames is what turns a seek into a bounded replay (at most
//! 599 frames, a few milliseconds) without making the log large, and the same 64 MiB then covers
//! about 80 minutes of session instead of under 8 s.
//!
//! WHY the log is the movie frame shape rather than a new struct: the pads are already recorded
//! per frame for FM2 movies ([`crate::movie::Recorder`]) and the reset command is already a bit in
//! that shape ([`crate::movie::Player`] acts on it), so a session log can be turned into a movie
//! and a movie can seed a session log with no second format and no second set of conventions.
//!
//! WHY nothing here enters `save_state()`: a history is a property of the *session*, not of the
//! machine, exactly like the rewind ring (`crate::rewind`). Loading a state from disk must not
//! resurrect a log that does not describe the machine the state was saved on, so the log lives
//! outside the state, `STATE_VERSION` does not move, and a state loaded from a file is a jump the
//! history reports as [`History::position`] disagreeing with [`History::end`] — see
//! [`History::step`], which starts a fresh log at that point rather than pretending.
//!
//! WHAT a seek is: load the nearest keyframe at or before the target, then set the pads (and do a
//! soft reset, if the log says so) and run every frame up to the target. The result is
//! byte-identical to a straight run, because a save state holds every piece of machine state and
//! the replay feeds exactly the inputs the original session fed. APU sample *output* is not part of
//! the state; a caller that wants the seeked stretch's audio must discard the samples it drained
//! before the seek, which is what a frontend showing the seeked frame does anyway.
//!
//! WHAT the budget does: past [`History::budget`] the oldest keyframes go, oldest first, and the
//! inputs they covered go with them, so the history then starts at the oldest kept keyframe. That
//! is a window that slides forward, never a log that grows without bound, and it is why
//! [`History::seek`] can fail with [`SeekError::TooOld`] on a long session — the frame asked for is
//! simply no longer in the log. One keyframe always survives, even when a single state is bigger
//! than the whole budget, because a history with no keyframe could not seek at all (the same rule
//! `crate::rewind::kept_frames` follows).
//!
//! WHAT is left to the frontend: the startup line that says where the history begins (83b), and the
//! choice to call [`History::truncate_after`] when the user resumes from a past frame.

use std::collections::VecDeque;
use std::fmt;

use crate::movie::MovieFrame;
use crate::nes::Nes;

/// Frames between keyframes: 600, about ten seconds at the NES's ~60 Hz frame rate. It bounds the
/// replay a seek costs (599 frames worst case) and sets the budget's granularity: a smaller
/// interval seeks faster and spends more memory, a larger one the other way round.
pub const DEFAULT_KEYFRAME_INTERVAL: usize = 600;

/// Bytes the keyframes may hold by default: 64 MiB. At ~136 KiB per state that is about 480
/// keyframes, i.e. about 288 000 frames or 80 minutes of session.
/// The input log itself is 3 bytes a frame (~10 KiB a minute, under 1 % of the keyframe
/// bytes per interval) and is not charged against the budget.
pub const DEFAULT_BUDGET: usize = 64 * 1024 * 1024;

/// FM2 command bit 0: soft reset. A frame carrying it is preceded by [`Nes::reset`], both when it is
/// recorded and when the log is replayed.
pub const COMMAND_RESET: u8 = 0b01;

/// FM2 command bit 1: power cycle. Recorded as given but never replayed here: a power cycle builds a
/// new machine (fresh RAM contents, a fresh power-on phase), which a `load_state` cannot reproduce,
/// so the frontend starts a new history on the fresh `Nes` instead of logging it as a command.
pub const COMMAND_POWER_CYCLE: u8 = 0b10;

/// Why [`History::seek`] could not put the machine at the frame asked for. Nothing has been touched
/// when a seek fails, so the caller can report the reason and stay where it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekError {
    /// Nothing is recorded yet: the first [`History::step`] (or [`History::attach`]) starts the log.
    Empty,
    /// The frame is older than the oldest retained keyframe. The budget dropped the keyframes (and
    /// inputs) that could still have replayed it.
    TooOld { asked: usize, oldest: usize },
    /// The frame has not been run yet: the history ends before it.
    BeyondHistory { asked: usize, end: usize },
    /// The stored keyframe did not load. The log is this process's own, so this means the state
    /// body was truncated or written by another build; `load_state` rolls a damaged body back, so
    /// the machine stays where it was.
    Load(crate::state::StateError),
}

impl fmt::Display for SeekError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SeekError::Empty => write!(f, "no session history yet"),
            SeekError::TooOld { asked, oldest } => write!(
                f,
                "frame {asked} is older than the history, which starts at frame {oldest}"
            ),
            SeekError::BeyondHistory { asked, end } => {
                write!(
                    f,
                    "frame {asked} has not been played yet (the history ends at {end})"
                )
            }
            SeekError::Load(e) => write!(f, "a keyframe did not load: {e:?}"),
        }
    }
}

/// One full `save_state()` and the absolute frame it restores.
///
/// The state is the machine *before* `frame` runs, so reloading it and replaying `frame .. target`
/// lands on the target. Keeping the frame number beside the bytes is what lets the keyframes be
/// dropped from the front without renumbering anything.
#[derive(Clone)]
pub struct Keyframe {
    frame: usize,
    state: Vec<u8>,
}

impl Keyframe {
    /// The frame this state restores: run it and the machine is at position `frame`.
    pub fn frame(&self) -> usize {
        self.frame
    }

    /// The `save_state()` bytes themselves.
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    /// Size of the state in bytes: what this keyframe costs against the budget.
    pub fn len(&self) -> usize {
        self.state.len()
    }

    /// A state is never zero bytes (it starts with a magic and a version), so this is false for
    /// every keyframe this module stores. It exists because clippy asks for it beside `len`.
    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }
}

impl fmt::Debug for Keyframe {
    /// The frame and the size, never the bytes: a debug line must not turn into a 136 KiB dump.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyframe")
            .field("frame", &self.frame)
            .field("bytes", &self.state.len())
            .finish()
    }
}

/// A session's input log plus its keyframes: see the module docs for why both, and why neither is in
/// `save_state()`.
pub struct History {
    /// Command bits and pads for frames `start .. start + inputs.len()`, oldest first.
    inputs: VecDeque<MovieFrame>,
    /// Full states, oldest first, each a multiple of `interval` frames apart.
    keyframes: VecDeque<Keyframe>,
    /// Absolute frame index of `inputs[0]`: 0 at [`History::attach`], and the oldest kept
    /// keyframe's frame after the budget has dropped the front.
    start: usize,
    /// Absolute frame the machine is at: 0 at attach, `end()` after a step, the seeked frame after a
    /// seek.
    position: usize,
    /// Frames between keyframes; 0 means "keyframe only at attach", which is a log that can seek but
    /// replays all of it.
    interval: usize,
    /// Bytes the keyframes may hold.
    budget: usize,
    /// Bytes the keyframes hold right now.
    bytes: usize,
}

impl History {
    /// A history with the defaults: a keyframe every 600 frames under a 64 MiB budget.
    pub fn new() -> History {
        History::with_limits(DEFAULT_KEYFRAME_INTERVAL, DEFAULT_BUDGET)
    }

    /// A history under a custom byte budget, 600 frames between keyframes. A smaller budget is how a
    /// test (or a frontend with less to spare) exercises the drop-the-oldest path without playing 80
    /// minutes.
    pub fn with_budget(budget: usize) -> History {
        History::with_limits(DEFAULT_KEYFRAME_INTERVAL, budget)
    }

    /// A history with both bounds spelled out. An `interval` of 0 stores one keyframe at attach and
    /// never another: every seek then replays from the start of the retained log.
    pub fn with_limits(interval: usize, budget: usize) -> History {
        History {
            inputs: VecDeque::new(),
            keyframes: VecDeque::new(),
            start: 0,
            position: 0,
            interval,
            budget,
            bytes: 0,
        }
    }

    // -- shape ---------------------------------------------------------------------------------

    /// Frames between keyframes, as asked for (the budget can drop keyframes, never space them out).
    pub fn keyframe_interval(&self) -> usize {
        self.interval
    }

    /// Bytes the keyframes may hold.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes the keyframes hold right now: what the history is costing.
    pub fn keyframe_bytes(&self) -> usize {
        self.bytes
    }

    /// Keyframes held right now.
    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// The frames the retained keyframes restore, oldest first: frame 0 at attach, then every
    /// `interval` frames, minus whatever the budget dropped from the front.
    pub fn keyframe_frames(&self) -> Vec<usize> {
        self.keyframes.iter().map(Keyframe::frame).collect()
    }

    /// The keyframes themselves, oldest first.
    pub fn keyframes(&self) -> impl Iterator<Item = &Keyframe> {
        self.keyframes.iter()
    }

    /// The keyframe a [`History::seek`] to `frame` would load: the newest one at or before it, or
    /// `None` when the frame is outside the retained log. Asking this is how a frontend can say how
    /// far a seek will have to replay.
    pub fn keyframe_for(&self, frame: usize) -> Option<&Keyframe> {
        if frame < self.start() || frame > self.end() {
            return None;
        }
        self.keyframes.iter().rev().find(|k| k.frame <= frame)
    }

    /// The oldest frame the log can still replay: 0 at attach, the oldest kept keyframe's frame once
    /// the budget has dropped the front.
    pub fn start(&self) -> usize {
        self.start
    }

    /// One past the last recorded frame: the frame the next [`History::step`] would record.
    pub fn end(&self) -> usize {
        self.start + self.inputs.len()
    }

    /// The frame the machine is at, as this history sees it. It differs from [`History::end`] only
    /// between a [`History::seek`] and the next step (or after a state was loaded from outside).
    pub fn position(&self) -> usize {
        self.position
    }

    /// Recorded frames held: `end() - start()`.
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// True before the first step, when nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.keyframes.is_empty()
    }

    /// The logged frame `frame`, or `None` when it is outside the retained log.
    pub fn input(&self, frame: usize) -> Option<MovieFrame> {
        if frame < self.start || frame >= self.end() {
            return None;
        }
        self.inputs.get(frame - self.start).cloned()
    }

    /// Every retained frame's input with its absolute frame number, oldest first.
    pub fn inputs(&self) -> impl Iterator<Item = (usize, MovieFrame)> + '_ {
        let start = self.start;
        self.inputs
            .iter()
            .enumerate()
            .map(move |(i, f)| (start + i, f.clone()))
    }

    // -- recording -----------------------------------------------------------------------------

    /// Start a history at the machine's current frame, discarding anything recorded before.
    ///
    /// This takes the frame-0 keyframe, so a log always has one and [`History::seek`] always has
    /// something to load for a frame it still covers. Called at power-on it logs the session from
    /// power-on; called later (a state loaded from a file, a `.nes` that replaced the cartridge) it
    /// logs the session from there, which is the only thing the machine can honour.
    pub fn attach(&mut self, nes: &Nes) {
        self.inputs.clear();
        self.keyframes.clear();
        self.bytes = 0;
        self.start = 0;
        self.position = 0;
        self.push_keyframe(nes);
    }

    /// Record this frame's pads, run it, and keyframe when due: the one call a frontend makes per
    /// frame, after setting `nes.bus.pads`. Equivalent to `step_with(nes, 0)`.
    pub fn step(&mut self, nes: &mut Nes) {
        self.step_with(nes, 0);
    }

    /// [`History::step`] with this frame's FM2 command bits: [`COMMAND_RESET`] makes the history do
    /// the soft reset itself (before running the frame, as an FM2 player does), so the log holds
    /// everything a replay needs. [`COMMAND_POWER_CYCLE`] is stored but not performed; see its docs.
    ///
    /// Resuming after a [`History::seek`]: the position is behind the log, so the future it is about
    /// to overwrite is dropped first, exactly as [`History::truncate_after`] would. A position ahead
    /// of the log (a state loaded from a file, or a machine swapped underneath) cannot be reconciled
    /// with the inputs on record, so the log restarts at that frame instead of lying about it.
    pub fn step_with(&mut self, nes: &mut Nes, commands: u8) {
        if self.keyframes.is_empty() {
            self.attach(nes);
        }
        if self.position < self.end() {
            self.truncate_after(self.position);
        } else if self.position > self.end() {
            self.attach(nes);
        }
        let frame = MovieFrame {
            commands,
            pads: [nes.bus.pads[0].buttons, nes.bus.pads[1].buttons],
        };
        self.inputs.push_back(frame);
        if commands & COMMAND_RESET != 0 {
            nes.reset();
        }
        nes.run_frame();
        self.position += 1;
        if self.interval > 0 && self.position.is_multiple_of(self.interval) {
            self.push_keyframe(nes);
        }
    }

    /// Drop every recorded frame after `frame`: what a session does when the user resumes from a
    /// past frame, so the new future replaces the old one instead of appending after it. Returns the
    /// number of recorded frames dropped.
    ///
    /// Call it after [`History::seek`], which is the one case where the machine really is at
    /// `frame`. The keyframe *at* `frame` survives — it restores the state before `frame` runs, so it
    /// is still reachable — while keyframes past it go with the frames they would have restored.
    pub fn truncate_after(&mut self, frame: usize) -> usize {
        // Frames before the retained log are already gone, so the earliest frame that can be kept is
        // `start`: clamping there is what stops a mistyped 0 from emptying the log.
        let frame = frame.max(self.start);
        let keep = frame.saturating_sub(self.start);
        let dropped = self.inputs.len().saturating_sub(keep);
        self.inputs.truncate(keep);
        while self.keyframes.back().is_some_and(|k| k.frame > frame) {
            if let Some(keyframe) = self.keyframes.pop_back() {
                self.bytes -= keyframe.len();
            }
        }
        dropped
    }

    // -- seeking -------------------------------------------------------------------------------

    /// Put the machine at `frame`: load the newest keyframe at or before it and replay the logged
    /// inputs (pads and reset commands) up to it. Byte-identical to having run straight there.
    ///
    /// Fails without touching the machine when the frame is outside the retained log — see
    /// [`SeekError`]. The log itself is unchanged: [`History::truncate_after`] is what forgets the
    /// future once the user resumes.
    pub fn seek(&mut self, nes: &mut Nes, frame: usize) -> Result<(), SeekError> {
        let Some(oldest) = self.keyframes.front().map(Keyframe::frame) else {
            return Err(SeekError::Empty);
        };
        if frame < oldest {
            return Err(SeekError::TooOld {
                asked: frame,
                oldest,
            });
        }
        let end = self.end();
        if frame > end {
            return Err(SeekError::BeyondHistory { asked: frame, end });
        }
        let index = self
            .keyframes
            .iter()
            .rposition(|k| k.frame <= frame)
            .expect("the oldest keyframe is at or before every frame in range");
        let (from, state) = {
            let keyframe = &self.keyframes[index];
            (keyframe.frame(), keyframe.state())
        };
        nes.load_state(state).map_err(SeekError::Load)?;
        let start = self.start;
        for at in from..frame {
            let input = &self.inputs[at - start];
            nes.bus.pads[0].buttons = input.pads[0];
            nes.bus.pads[1].buttons = input.pads[1];
            if input.commands & COMMAND_RESET != 0 {
                nes.reset();
            }
            nes.run_frame();
        }
        self.position = frame;
        Ok(())
    }

    // -- internals -----------------------------------------------------------------------------

    /// Store the machine's state as the keyframe for the current position, then enforce the budget.
    fn push_keyframe(&mut self, nes: &Nes) {
        let state = nes.save_state();
        self.bytes += state.len();
        self.keyframes.push_back(Keyframe {
            frame: self.position,
            state,
        });
        // Oldest first: the newest keyframe is the frame on screen, so it is the last to go, and
        // `len() > 1` keeps one even when a single state is larger than the budget.
        while self.bytes > self.budget && self.keyframes.len() > 1 {
            let dropped = self.keyframes.pop_front().expect("more than one keyframe");
            self.bytes -= dropped.len();
        }
        // "Past the budget the oldest keyframes *and their inputs* go": the log now starts at the
        // oldest kept keyframe, so a seek can no longer reach the frames before it.
        if let Some(front) = self.keyframes.front() {
            let new_start = front.frame();
            if new_start > self.start {
                let n = (new_start - self.start).min(self.inputs.len());
                self.inputs.drain(..n);
                self.start += n;
            }
        }
    }
}

impl Default for History {
    fn default() -> History {
        History::new()
    }
}

impl fmt::Debug for History {
    /// One line per history: the frame window, how many keyframes and what they cost. Prints no
    /// state bytes, because a debug line must not turn into a 64 MiB dump.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("History")
            .field("start", &self.start)
            .field("end", &self.end())
            .field("position", &self.position)
            .field("keyframes", &self.keyframe_frames())
            .field("keyframe_bytes", &self.bytes)
            .field("budget", &self.budget)
            .finish()
    }
}
