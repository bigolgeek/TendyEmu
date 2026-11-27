//! Frame pacing and the run mode (DESIGN §6.1).
//!
//! The rate comes from `Machine::frame_rate`, not from a per-system constant, so one pacer runs
//! every core. Fast-forward is 2x, 3x or 4x only; there is no 1.5x.

/// The fast-forward choices: 1x, then 2x, 3x and 4x with nothing between.
pub const SPEEDS: [f64; 4] = [1.0, 2.0, 3.0, 4.0];

/// Frames the pacer may run in one tick before it gives up catching up.
pub const CATCH_UP_LIMIT: u32 = 4;

/// Whether the loop is running normally or fast-forwarding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    #[default]
    Normal,
    FastForward,
}

/// Whether a tick that had to catch up redraws its picture.
///
/// The machine runs every frame it is owed either way; what `Auto` drops is the texture upload of
/// a tick that already ran more than one emulated frame, so the picture is one tick older and the
/// front end gets the time back. Skipping the emulation instead would drift the sound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frameskip {
    /// Every tick redraws the picture.
    #[default]
    Off,
    /// A tick that ran more than one emulated frame keeps the previous picture.
    Auto,
}

impl Frameskip {
    /// The settings-file spelling.
    pub fn name(self) -> &'static str {
        match self {
            Frameskip::Off => "off",
            Frameskip::Auto => "auto",
        }
    }

    /// The settings-file spelling, or `None` for anything else.
    pub fn from_name(word: &str) -> Option<Frameskip> {
        match word {
            "off" => Some(Frameskip::Off),
            "auto" => Some(Frameskip::Auto),
            _ => None,
        }
    }

    /// The drawer row's value text.
    pub fn label(self) -> &'static str {
        match self {
            Frameskip::Off => "Off",
            Frameskip::Auto => "Auto",
        }
    }

    /// The other choice: the row has two, so a step is a flip.
    pub fn step(self) -> Frameskip {
        match self {
            Frameskip::Off => Frameskip::Auto,
            Frameskip::Auto => Frameskip::Off,
        }
    }

    /// Whether a tick that ran `frames_run` emulated frames redraws the picture.
    pub fn draw_due(self, frames_run: u32) -> bool {
        match self {
            Frameskip::Off => true,
            Frameskip::Auto => frames_run <= 1,
        }
    }
}

/// A fractional frame accumulator against a frame rate.
#[derive(Clone, Debug)]
pub struct FramePacer {
    fps: f64,
    acc: f64,
}

impl Default for FramePacer {
    fn default() -> Self {
        FramePacer::new(60.0)
    }
}

impl FramePacer {
    pub fn new(fps: f64) -> FramePacer {
        FramePacer { fps, acc: 0.0 }
    }

    /// The rate this pacer runs at.
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Sets the rate, dropping the accumulator: a system switch must not owe the old core frames.
    pub fn set_fps(&mut self, fps: f64) {
        self.fps = fps;
        self.acc = 0.0;
    }

    /// Frames to run for a `dt`-second tick at `speed`, never more than the catch-up limit.
    ///
    /// The accumulator is charged the whole amount even when the answer is clamped, so a slow
    /// tick does not leave a debt that the next one has to pay off in a burst.
    pub fn frames_due_at(&mut self, dt: f64, speed: f64) -> u32 {
        if self.fps <= 0.0 || dt <= 0.0 || speed <= 0.0 {
            return 0;
        }
        self.acc += dt * self.fps * speed;
        let due = self.acc.floor();
        if due < 1.0 {
            return 0;
        }
        self.acc -= due;
        if due > f64::from(CATCH_UP_LIMIT) {
            CATCH_UP_LIMIT
        } else {
            due as u32
        }
    }

    /// Throws away the fractional frame: what a pause or a drawer does, so closing it does not
    /// burst through the frames it was held for.
    pub fn reset(&mut self) {
        self.acc = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pacer_runs_one_frame_a_tick_at_its_rate() {
        let mut pacer = FramePacer::new(60.0);
        let mut ran = 0;
        for _ in 0..60 {
            ran += pacer.frames_due_at(1.0 / 60.0, 1.0);
        }
        assert_eq!(ran, 60);
    }

    #[test]
    fn fast_forward_runs_two_or_four_frames_a_tick() {
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due_at(1.0 / 60.0, 2.0), 2);
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due_at(1.0 / 60.0, 4.0), 4);
    }

    #[test]
    fn a_slow_tick_is_capped_and_leaves_no_debt() {
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due_at(1.0, 1.0), CATCH_UP_LIMIT);
        // The next ordinary tick owes one frame, not the rest of the backlog.
        assert_eq!(pacer.frames_due_at(1.0 / 60.0, 1.0), 1);
    }

    #[test]
    fn a_zero_tick_or_rate_runs_nothing() {
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due_at(0.0, 1.0), 0);
        assert_eq!(pacer.frames_due_at(1.0, 0.0), 0);
        let mut pacer = FramePacer::new(0.0);
        assert_eq!(pacer.frames_due_at(1.0, 1.0), 0);
    }

    #[test]
    fn a_rate_change_starts_from_zero() {
        let mut pacer = FramePacer::new(60.0);
        pacer.frames_due_at(0.5 / 60.0, 1.0);
        pacer.set_fps(59.7275);
        assert_eq!(pacer.fps(), 59.7275);
        assert_eq!(pacer.frames_due_at(1.0 / 59.7275, 1.0), 1);
    }

    #[test]
    fn frameskip_auto_redraws_only_a_tick_that_kept_time() {
        assert!(Frameskip::Off.draw_due(3));
        assert!(Frameskip::Auto.draw_due(1));
        assert!(Frameskip::Auto.draw_due(0));
        assert!(!Frameskip::Auto.draw_due(2));
    }

    #[test]
    fn the_two_frameskip_words_round_trip() {
        for skip in [Frameskip::Off, Frameskip::Auto] {
            assert_eq!(Frameskip::from_name(skip.name()), Some(skip));
            assert_eq!(skip.step().step(), skip);
        }
        assert_eq!(Frameskip::from_name("sometimes"), None);
    }
}
