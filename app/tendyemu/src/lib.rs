//! The library half of the one binary: everything except the window loop and the device calls, so
//! the app's tests and the modules' unit tests run headless (DESIGN §6.1).

pub mod app;
pub mod audio;
pub mod avi;
pub mod battery;
pub mod caps;
pub mod config;
pub mod display;
pub mod draw;
pub mod file_pick;
pub mod first_run;
pub mod gamepad;
pub mod games;
pub mod keys;
pub mod loader;
pub mod menu;
pub mod paths;
pub mod recents;
pub mod record;
pub mod rewind;
pub mod screenshot;
pub mod settings;
pub mod states;
pub mod status_bar;
pub mod switch;
pub mod thumb;
pub mod timing;

// Declared but not used yet: cpal arrives with the device half of `audio.rs`, png with the slot
// thumbnails, tendy-spc with the cores' sound coprocessor (§6.1, all owned by later issues).
use cpal as _;
use png as _;
use tendy_spc as _;
