//! Game pads (DESIGN §6.1). #17 owns this file: XInput by hand through kernel32, no crate.
//!
//! The stub pads nothing, so a missing pad is simply no input.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the gamepad settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to poll yet.
pub fn on_frame(_app: &mut App) {}
