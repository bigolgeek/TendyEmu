//! Rewind (DESIGN §6.1). #24 owns this file: the ring of `Machine::save_state` snapshots, so one
//! implementation works for any core.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the rewind ring exists.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to snapshot yet.
pub fn on_frame(_app: &mut App) {}
