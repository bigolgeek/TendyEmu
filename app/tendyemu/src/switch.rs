//! Switching the running core in-process (DESIGN §6.1, §6.4). #20 owns this file: the steps that
//! are not `App::open` itself, and the refusal path that keeps the old game running.
//!
//! `App::try_open` already does the steps that need the `App`'s private fields: it detects and
//! builds the new core before the old one goes (a refusal never costs the running game), writes the
//! old game's battery, swaps the machine, sets the new frame rate and loads the new game's battery
//! (§6.4 steps 1, 3, 4 and 5). The modules that keep per-game state — the rewind ring, an open
//! recording, the audio converter — compare their own `rom` with `App::rom` on their next hook and
//! drop the old timeline (#24, #19), which is step 3's close and step 4's drop. What has no other
//! home is the cartridge's wall clock: §6.4 step 5 says `tick_rtc` each frame, and this is the one
//! per-frame hook the shell does not already run for another reason.

use crate::app::App;
use crate::battery;
use crate::menu::{Row, RowAct};

/// No rows: a switch is a file open, not a setting.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No rows, so no row can be activated.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// One frame: the cartridge's real-time clock follows the wall clock, as the Game Boy front end
/// ticks it before every frame. Cores without a clock answer `tick_rtc` with nothing, so this is
/// free for four systems out of five.
///
/// The hook runs while the game is held too (drawer open, paused): the clock on the cartridge keeps
/// counting whether a frame is emulated or not.
pub fn on_frame(app: &mut App) {
    if let Some(machine) = app.machine_mut() {
        machine.tick_rtc(battery::now_ms());
    }
}
