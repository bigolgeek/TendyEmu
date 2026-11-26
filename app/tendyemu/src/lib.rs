//! tendyemu: the library half of the one binary, so the app's tests can run headless.

// The dependency graph is frozen by DESIGN §1.2; the front end (#15) uses these crates.
use cpal as _;
use macroquad as _;
use png as _;
use rfd as _;
use tendy_core as _;
use tendy_detect as _;
use tendy_gb as _;
use tendy_gba as _;
use tendy_nes as _;
use tendy_snes as _;
use tendy_spc as _;
