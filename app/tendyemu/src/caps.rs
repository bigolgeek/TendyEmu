//! Which rows a game's capabilities allow (DESIGN §2.3, §6.1).

use tendy_core::{Capabilities, Capability};

use crate::app::App;
use crate::menu::{Row, RowAct};

/// A row's capability gate: `None` means the row applies to every game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowGate {
    pub requires: Option<Capability>,
}

/// Whether `gate` lets the row into the drawer for a machine with `caps`.
pub fn visible(gate: &RowGate, caps: Capabilities) -> bool {
    gate.requires.is_none_or(|cap| caps.has(cap))
}

/// This module adds no rows of its own: it decides which of everyone else's are drawn.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame; the gate is read when the rows are collected.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_without_a_gate_always_shows() {
        let gate = RowGate::default();
        assert!(visible(&gate, Capabilities::NONE));
    }

    #[test]
    fn a_gated_row_needs_its_capability() {
        let gate = RowGate {
            requires: Some(Capability::SgbBorder),
        };
        assert!(!visible(&gate, Capabilities::NONE));
        assert!(visible(
            &gate,
            Capabilities::NONE.with(Capability::SgbBorder)
        ));
    }
}
