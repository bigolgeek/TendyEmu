//! tendy-nes: the NES core. The import (#9) fills this crate.

// The dependency graph is frozen by DESIGN §1.2; the import uses these crates.
use libm as _;
use png as _;
use tendy_core as _;
use zip as _;
