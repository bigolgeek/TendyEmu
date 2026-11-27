//! The drawer's settings rows (DESIGN §6.1, §6.2). #21 owns this file: one row per §6.2 key, the
//! value text from the loaded config, and the write-through when a value changes.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// Which way an arrow key steps a choice row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

/// No rows until the config is read.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}
