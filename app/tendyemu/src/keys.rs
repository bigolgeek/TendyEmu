//! The default keyboard map (DESIGN §6.1).
//!
//! #17 owns the real thing: per-system bindings from `<system>/keys.txt`, the `keys.txt` dialog and
//! a rebind row. Until then this is a fixed map of the names `main.rs` reports, so a game can be
//! played and the pad is never stuck down.

use tendy_core::Buttons;

use crate::app::App;
use crate::menu::{Row, RowAct};

/// The pad bits for the names held down. The display names are the core's own (`Buttons::ALL`).
pub fn pad(held: &[String]) -> Buttons {
    let mut pad = Buttons::NONE;
    for (button, name) in Buttons::ALL {
        if held.iter().any(|down| down == name) {
            pad = pad.with(button);
        }
    }
    pad
}

/// Hands the pressed pad to the machine once per frame.
pub fn on_frame(app: &mut App) {
    let pressed = pad(app.held_buttons());
    if let Some(machine) = app.machine_mut() {
        machine.set_buttons(pressed);
    }
}

/// No rows until the input settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_names_press_two_bits_and_nothing_presses_nothing() {
        assert_eq!(pad(&[]), Buttons::NONE);
        let pad = pad(&["up".to_string(), "a".to_string()]);
        assert!(pad.contains(Buttons::UP));
        assert!(pad.contains(Buttons::A));
        assert!(!pad.contains(Buttons::B));
    }
}
