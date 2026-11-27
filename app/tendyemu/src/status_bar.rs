//! The status bar: one line under the picture (DESIGN §6.1, §7.7).
//!
//! The words are the GB front end's, over whatever core is running. A narrow bar drops the title
//! first, then counts the stat fields it had to leave out (`+N`); `MUTED`/`PAUSED` and the notice
//! stay to the end.

use tendy_core::SystemKind;

/// The bar's height, taken off the bottom of the window.
pub const BAR_H: f32 = 24.0;
/// How long a plain notice stays up.
pub const NOTICE_SECS: f64 = 2.0;
/// The bar's left and right padding.
pub const PAD: f32 = 8.0;
/// The separator between two words.
pub const SEP_W: f32 = 13.0;
/// The gap before the right-hand notice slot.
pub const WORD_GAP: f32 = 12.0;
/// A field never gets narrower than this: `000.0 fps`, the widest a broken clock can print.
pub const FPS_MIN: &str = "000.0 fps";

/// Everything one frame's bar line is drawn from.
#[derive(Clone, Debug, PartialEq)]
pub struct BarState {
    /// The ROM's file name, empty when no game is open.
    pub title: String,
    pub fps: f64,
    /// The rate the pacer aims at: the running core's own.
    pub target_fps: f64,
    /// The session speed: 1.0 at 1x, 2.0-4.0 while fast-forwarding.
    pub speed: f64,
    /// The running system, drawn as one word. `None` before a game opens.
    pub system: Option<SystemKind>,
    /// The cartridge's storage as the machine names it (`SRAM`, `none`, `mapper 4`).
    pub cart: String,
    pub frame: u64,
    pub muted: bool,
    /// A real pause: the only thing that puts the `PAUSED` word on the bar.
    pub paused: bool,
    /// Held for any reason (a pause, the drawer, no game). The fps slot dashes for every held
    /// state; `PAUSED` does not.
    pub held: bool,
    /// The notice in the right-hand slot.
    pub notice: Option<String>,
    /// Whether the notice is an error (it gets the error chip).
    pub notice_error: bool,
}

impl Default for BarState {
    fn default() -> BarState {
        BarState {
            title: String::new(),
            fps: 0.0,
            target_fps: 60.0,
            speed: 1.0,
            system: None,
            cart: String::new(),
            frame: 0,
            muted: false,
            paused: false,
            held: true,
            notice: None,
            notice_error: false,
        }
    }
}

impl BarState {
    /// The speed slot's text: `1x`, `2x`, `3x`, `4x`.
    pub fn speed_label(&self) -> String {
        format!("{}x", self.speed.round() as i64)
    }

    /// The rate slot's text: empty with no game, dashes while a game's loop is held, else the
    /// rate.
    pub fn fps_label(&self) -> String {
        if self.system.is_none() {
            String::new()
        } else if self.held {
            "-000.0 fps".to_string()
        } else {
            format!("{:03.1} fps", self.fps.min(999.9))
        }
    }

    /// The system slot's word: `GB`, `GBA`, `NES`, `SNES`.
    pub fn system_label(&self) -> &'static str {
        match self.system {
            Some(SystemKind::Gb) => "GB",
            Some(SystemKind::Gba) => "GBA",
            Some(SystemKind::Nes) => "NES",
            Some(SystemKind::Snes) => "SNES",
            None => "",
        }
    }
}

/// What a word is for the drop rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordKind {
    /// The ROM title: the first thing dropped.
    Title,
    /// `MUTED`, `PAUSED`: never dropped before a stat field.
    Word,
    /// The right-hand notice slot.
    Notice,
    /// A stat field (`59.7 fps`, `1x`, ...).
    Stat,
}

/// One word of the bar line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub kind: WordKind,
}

/// The words of the left-hand group, in the order the bar draws them: title, `MUTED`, `PAUSED`,
/// then the stat fields. The notice is not here: it owns the right-hand slot.
pub fn status_words(state: &BarState) -> Vec<Word> {
    let mut words = Vec::new();
    if !state.title.is_empty() {
        words.push(Word {
            text: state.title.clone(),
            kind: WordKind::Title,
        });
    }
    if state.muted {
        words.push(Word {
            text: "MUTED".to_string(),
            kind: WordKind::Word,
        });
    }
    if state.paused {
        words.push(Word {
            text: "PAUSED".to_string(),
            kind: WordKind::Word,
        });
    }
    if let Some(system) = state.system {
        words.push(Word {
            text: system.display_name().to_string(),
            kind: WordKind::Stat,
        });
    }
    let fps = state.fps_label();
    if !fps.is_empty() {
        words.push(Word {
            text: fps,
            kind: WordKind::Stat,
        });
    }
    words.push(Word {
        text: state.speed_label(),
        kind: WordKind::Stat,
    });
    if !state.cart.is_empty() {
        words.push(Word {
            text: state.cart.clone(),
            kind: WordKind::Stat,
        });
    }
    words
}

/// The words that fit `width_px`, by the drop rule, and how many stat fields were left out.
///
/// One pass: everything; then the title goes; then stat fields from the right, each counted. A
/// `Word` and the notice are never dropped — a bar too narrow even for them overflows, which is
/// better than hiding `PAUSED`.
pub fn fitted(words: &[Word], width_px: f32, char_w: f32) -> (Vec<Word>, usize) {
    let char_w = if char_w > 0.0 { char_w } else { 7.0 };
    let width = |words: &[Word]| -> f32 {
        let chars: usize = words.iter().map(|word| word.text.chars().count()).sum();
        let gaps = words.len().saturating_sub(1) as f32 * WORD_GAP;
        chars as f32 * char_w + gaps
    };
    if width(words) <= width_px {
        return (words.to_vec(), 0);
    }
    let without_title: Vec<Word> = words
        .iter()
        .filter(|word| word.kind != WordKind::Title)
        .cloned()
        .collect();
    if width(&without_title) <= width_px {
        return (without_title, 0);
    }
    let mut kept = without_title;
    let mut dropped = 0;
    while width(&kept) > width_px {
        let Some(at) = kept.iter().rposition(|word| word.kind == WordKind::Stat) else {
            break;
        };
        kept.remove(at);
        dropped += 1;
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> BarState {
        BarState {
            title: "Zelda".to_string(),
            fps: 59.7,
            target_fps: 59.7275,
            speed: 1.0,
            system: Some(SystemKind::Snes),
            cart: "SRAM".to_string(),
            frame: 10,
            muted: false,
            paused: false,
            held: false,
            notice: None,
            notice_error: false,
        }
    }

    #[test]
    fn the_words_run_title_then_words_then_stats() {
        let words = status_words(&state());
        assert_eq!(words[0].kind, WordKind::Title);
        assert!(words.iter().any(|word| word.text == "59.7 fps"));
        assert!(words.iter().any(|word| word.text == "1x"));
        assert!(words.iter().any(|word| word.text == "Super NES"));
    }

    #[test]
    fn a_held_loop_dashes_the_rate_but_a_pause_adds_the_word() {
        let mut state = state();
        state.held = true;
        assert_eq!(state.fps_label(), "-000.0 fps");
        state.paused = true;
        assert!(
            status_words(&state)
                .iter()
                .any(|word| word.text == "PAUSED")
        );
    }

    #[test]
    fn no_game_leaves_the_rate_slot_empty() {
        let mut state = state();
        state.system = None;
        state.held = true;
        assert_eq!(state.fps_label(), "");
        let words = status_words(&state);
        let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert!(!texts.contains(&"-000.0 fps"), "{texts:?}");
        assert!(!texts.contains(&"000.0 fps"), "{texts:?}");
    }

    #[test]
    fn a_narrow_bar_drops_the_title_first() {
        let words = status_words(&state());
        let width: f32 = 210.0;
        let (kept, dropped) = fitted(&words, width, 7.0);
        assert_eq!(
            dropped, 0,
            "no stat is counted while the title alone can go"
        );
        assert!(!kept.iter().any(|word| word.kind == WordKind::Title));
        assert!(kept.iter().any(|word| word.text == "59.7 fps"));
    }

    #[test]
    fn a_narrower_bar_counts_the_stats_it_had_to_leave_out() {
        let words = status_words(&state());
        let (kept, dropped) = fitted(&words, 2.0, 7.0);
        assert_eq!(dropped, 4, "the system, rate, speed and storage fields");
        assert!(kept.is_empty());
    }

    #[test]
    fn the_muted_word_survives_a_bar_too_narrow_for_anything() {
        let mut state = state();
        state.muted = true;
        state.paused = true;
        let words = status_words(&state);
        let (kept, _) = fitted(&words, 0.0, 7.0);
        let texts: Vec<&str> = kept.iter().map(|word| word.text.as_str()).collect();
        assert!(texts.contains(&"MUTED"));
        assert!(texts.contains(&"PAUSED"));
    }
}
