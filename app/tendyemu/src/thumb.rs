//! Slot thumbnails (DESIGN §6.1, §6.3). #22 owns this file: the PNG beside each state file, drawn
//! in the slot's row and written from the frame the state was taken on.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows: a thumbnail is drawn inside a slot row, not on one of its own.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to capture yet.
pub fn on_frame(_app: &mut App) {}
