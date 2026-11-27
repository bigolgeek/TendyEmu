//! Save states (DESIGN §6.1, §2.7). #22 owns this file: the slot rows, the container from
//! `tendy_core::state`, and the battery-adjacent rules of §6.3.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the slot naming arrives.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}
