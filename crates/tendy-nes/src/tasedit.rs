//! TAS edit core: a movie plus a greenzone of save states, so an edit at frame N
//! costs a replay from the nearest still-valid state instead of from power-on.
//!
//! WHY the greenzone is plain `Nes::save_state` bytes: the same argument `rewind.rs` makes — every
//! piece of emulated state already round-trips through that vector, so no second state format is
//! needed here and `STATE_VERSION` does not move.
//!
//! Indexing: an entry keyed `k` is the machine after `k` movie frames have played — the state
//! immediately *before* frame `k`'s pads are applied, which is the same "frames played" count
//! [`Player::pos`] keeps. An edit to frame index `N` changes what frame `N` and every later frame
//! does, so every entry `> N` is stale: entry `N + 1` is the first one the edit can invalidate,
//! while entry `N` is the machine before frame `N`'s pads exist to it and stays valid. Keeping it is
//! what makes "edit the frame being looked at, then seek back to it" no replay at all.
//!
//! The budget drops the lowest-numbered (oldest) greenzone entries first, exactly like `Rewind`, and
//! the entry just stored always survives. The power-on state (`origin`) is held outside the budget:
//! it is the one state every seek can fall back on, and it is a single state, so it cannot grow.

use std::collections::BTreeMap;

use crate::movie::{Movie, MovieError, MovieFrame, Player};
use crate::nes::Nes;

/// Default greenzone budget: 128 MiB, the same as the rewind ring.
pub const DEFAULT_GREENZONE_BUDGET: usize = crate::rewind::DEFAULT_REWIND_BUDGET;

/// Saved machine states keyed by frames played, bounded by a byte budget.
///
/// The keys are what make an edit cheap: see the module docs for why key `k` is the state *before*
/// frame `k`'s input is applied, and therefore why only keys above an edited frame go stale.
pub struct Greenzone {
    states: BTreeMap<usize, Vec<u8>>,
    budget: usize,
    bytes: usize,
}

impl Greenzone {
    /// An empty greenzone whose held entries may total `budget` bytes.
    pub fn new(budget: usize) -> Greenzone {
        Greenzone {
            states: BTreeMap::new(),
            budget,
            bytes: 0,
        }
    }

    /// Entries held right now, i.e. how many frames a seek can start from without replaying.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// True when no checkpoint is held, so every seek replays from power-on.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Bytes the held entries occupy: what this feature is costing right now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte budget this greenzone was built with.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// True when there is a checkpoint for exactly `frame` frames played.
    pub fn contains(&self, frame: usize) -> bool {
        self.states.contains_key(&frame)
    }

    /// Frame numbers held, ascending.
    pub fn frames(&self) -> Vec<usize> {
        self.states.keys().copied().collect()
    }

    /// The newest entry at or before `frame`: where a seek to `frame` starts replaying.
    pub fn nearest(&self, frame: usize) -> Option<(usize, &[u8])> {
        self.states
            .range(..=frame)
            .next_back()
            .map(|(k, v)| (*k, v.as_slice()))
    }

    /// Store the state for `frame`, then drop the oldest entries until the budget holds. The
    /// entry just stored is never dropped, so a state larger than the budget still survives.
    pub fn insert(&mut self, frame: usize, state: Vec<u8>) {
        self.bytes += state.len();
        if let Some(old) = self.states.insert(frame, state) {
            self.bytes -= old.len();
        }
        while self.bytes > self.budget {
            let Some(oldest) = self.states.keys().copied().find(|k| *k != frame) else {
                break;
            };
            if let Some(old) = self.states.remove(&oldest) {
                self.bytes -= old.len();
            }
        }
    }

    /// Drop the entry for exactly `frame`, if held (a checkpoint that failed to load).
    fn remove(&mut self, frame: usize) {
        if let Some(old) = self.states.remove(&frame) {
            self.bytes -= old.len();
        }
    }

    /// Drop every entry keyed strictly after `frame` — the checkpoints an edit to frame `frame`
    /// makes stale. The entry for `frame` itself survives: it is the machine before that frame's
    /// pads are applied, so the edit cannot have changed it (see the module docs).
    pub fn invalidate_after(&mut self, frame: usize) {
        let dropped = self.states.split_off(&(frame + 1));
        self.bytes -= dropped.values().map(Vec::len).sum::<usize>();
    }
}

/// A movie being edited, with the states that make seeking cheap.
pub struct TasProject {
    /// The movie being edited. Public for inspection and serialisation, but it is only safe to
    /// mutate through this type's edit methods: a direct change to `frames` leaves greenzone
    /// entries describing a movie that no longer exists, and a seek would then replay the wrong run.
    pub movie: Movie,
    /// The checkpoints that make a seek cheap, ordered by frames played (see [`Greenzone`]).
    pub greenzone: Greenzone,
    /// The power-on state that every seek can fall back on, held outside the byte budget.
    origin: Vec<u8>,
}

impl TasProject {
    /// A project for `movie` with the default byte budget. `power_on` must be a freshly built
    /// machine — that state becomes the frame-0 checkpoint every seek can fall back on.
    pub fn new(movie: Movie, power_on: &Nes) -> TasProject {
        TasProject::with_budget(movie, power_on, DEFAULT_GREENZONE_BUDGET)
    }

    /// A project whose checkpoints may total `budget` bytes; a budget smaller than one state still
    /// keeps the newest checkpoint, so seeking stays possible. The power-on state is taken here and
    /// is never trimmed, because dropping it would leave a seek with nowhere to start.
    pub fn with_budget(movie: Movie, power_on: &Nes, budget: usize) -> TasProject {
        TasProject {
            movie,
            greenzone: Greenzone::new(budget),
            origin: power_on.save_state(),
        }
    }

    /// Set (`on`) or clear `mask` bits of pad `pad` (0 or 1) on `frame`, then drop the checkpoints
    /// the change invalidates. False when the frame or the pad does not exist, and then nothing is
    /// touched — no edit, no invalidation.
    pub fn set_button(&mut self, frame: usize, pad: usize, mask: u8, on: bool) -> bool {
        let Some(f) = self.movie.frames.get_mut(frame) else {
            return false;
        };
        let Some(p) = f.pads.get_mut(pad) else {
            return false;
        };
        if on {
            *p |= mask;
        } else {
            *p &= !mask;
        }
        self.greenzone.invalidate_after(frame);
        true
    }

    /// Flip `mask` bits of pad `pad` on `frame`, then drop the checkpoints the change invalidates.
    /// False when the frame or the pad does not exist, and then nothing is touched.
    pub fn toggle_button(&mut self, frame: usize, pad: usize, mask: u8) -> bool {
        let Some(f) = self.movie.frames.get_mut(frame) else {
            return false;
        };
        let Some(p) = f.pads.get_mut(pad) else {
            return false;
        };
        *p ^= mask;
        self.greenzone.invalidate_after(frame);
        true
    }

    /// Insert a blank frame (no buttons, no commands) before `frame`, then drop the checkpoints the
    /// shift invalidates; `frame == len` appends and so invalidates nothing. False when `frame` is
    /// past the end.
    pub fn insert_blank(&mut self, frame: usize) -> bool {
        if frame > self.movie.frames.len() {
            return false;
        }
        self.movie.frames.insert(
            frame,
            MovieFrame {
                commands: 0,
                pads: [0, 0],
            },
        );
        self.greenzone.invalidate_after(frame);
        true
    }

    /// Delete `frame`, shifting later frames up, then drop the checkpoints the shift invalidates.
    /// False when `frame` is not in the movie.
    pub fn delete_frame(&mut self, frame: usize) -> bool {
        if frame >= self.movie.frames.len() {
            return false;
        }
        self.movie.frames.remove(frame);
        self.greenzone.invalidate_after(frame);
        true
    }

    /// Put `nes` in the state after `frame` movie frames have played: restore the nearest checkpoint
    /// at or before `frame`, then replay with [`Player`] semantics — the same pad and soft-reset
    /// handling `--movie` playback uses — storing a checkpoint after every frame run, so the next
    /// seek starts closer. Returns the frame reached, which is below `frame` only when the movie ends
    /// first. A power-cycle command stops the replay with [`MovieError::PowerCycleUnsupported`],
    /// exactly as playback does, leaving `nes` at the last frame that did play.
    pub fn seek(&mut self, nes: &mut Nes, frame: usize) -> Result<usize, MovieError> {
        // A checkpoint that will not load (stale, from another ROM build, or damaged) is dropped and
        // the next older one tried, so a bad entry costs replay time rather than a panic. A failed
        // load leaves `nes` as it was (`Nes::load_state` restores its backup).
        let start = loop {
            match self
                .greenzone
                .nearest(frame)
                .map(|(played, state)| (played, state.to_vec()))
            {
                Some((played, state)) => {
                    if nes.load_state(&state).is_ok() {
                        break played;
                    }
                    self.greenzone.remove(played);
                }
                None => {
                    // The power-on state is the one every checkpoint descends from; it was taken
                    // from this project's machine in `new`, so failing to load it is a caller
                    // error (a different ROM), not a stale entry.
                    nes.load_state(&self.origin)
                        .expect("the power-on state loads into the machine it was taken from");
                    break 0;
                }
            }
        };
        let mut player = Player::new(std::mem::take(&mut self.movie));
        player.pos = start;
        let result = loop {
            if player.pos >= frame {
                break Ok(player.pos);
            }
            match player.apply(nes) {
                Ok(true) => {
                    nes.run_frame();
                    // Record on the way: what a seek replays is exactly what the next edit would
                    // otherwise have to replay again.
                    self.greenzone.insert(player.pos, nes.save_state());
                }
                // The movie ran out first; `pos` is the number of frames that really played.
                Ok(false) => break Ok(player.pos),
                Err(error) => break Err(error),
            }
        };
        // Hand the movie back on the error path too: the project must not be left holding an empty
        // movie because a replay stopped early.
        self.movie = player.movie;
        result
    }
}
