//! Recording (DESIGN §6.1, §6.3). #24 owns this file: the formats the GB front end records today,
//! under `<system>/recordings/`.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the capture settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to record yet.
pub fn on_frame(_app: &mut App) {}
