//! Switching the running core in-process (DESIGN §6.1, §6.4). #20 owns this file: the steps that
//! are not `App::open` itself, and the refusal path that keeps the old game running.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the switch settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}
