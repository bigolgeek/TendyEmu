//! The first-run offer (DESIGN §6.1, §6.5). #23 owns this file: the one-time offer to adopt the old
//! apps' files, and the `first-run.done` marker that answers it.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the offer exists.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}
