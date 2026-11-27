//! Sound (DESIGN §6.1). #19 owns this file: the device half (cpal), `negotiate_rate`, the ring
//! and the sinc converter from any core rate.
//!
//! Until then the app is silent, but the core's queue is still drained every frame so it cannot
//! grow without bound.

use crate::app::App;
use crate::menu::{Row, RowAct};

/// The rate the machine is told to produce at until #19 negotiates the device rate.
pub const SAMPLE_RATE: u32 = 48_000;

/// No rows until the audio settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Drains the frame's samples into nothing: silent, and the ring stays empty.
pub fn on_frame(app: &mut App) {
    let Some(machine) = app.machine_mut() else {
        return;
    };
    let mut buffer = [0.0f32; 4096];
    while machine.drain_audio(&mut buffer) > 0 {}
}
