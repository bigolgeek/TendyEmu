//! `config.toml` (DESIGN §6.2). #21 owns this file: the hand-written parser, the sections and the
//! atomic write. Until then every key takes its default, which is what `App::setting` says.

use crate::app::App;
use crate::menu::{Row, RowAct};

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
