//! Per-game overrides (DESIGN §6.2, SPEC §6). #21 owns this file: `<system>/games/<crc32>.toml`,
//! read on open and written when a row changes.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// No rows until the per-game file is read.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}
