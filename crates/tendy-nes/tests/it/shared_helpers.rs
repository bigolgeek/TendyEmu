//! Guard: the shared test helpers live once in `tests/it/common/mod.rs`, not copied into
//! every test file.
//!
//! It counts the definitions that were byte-identical copies and are now shared. Helpers whose
//! copies legitimately differ are left where they are and are deliberately not counted here:
//! `trace_log.rs` keeps its own `loop_rom`, `history.rs`/`runahead.rs`/`zapper.rs`/
//! `clocked_bus.rs` keep their own `program`, and `cpu_roms.rs`/`mmc3_roms.rs` keep their own
//! `run_blargg` (each builds a different ROM or polls through a different bus path).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SHARED: [&str; 4] = [
    "fn rom_file(",
    "fn rom_file_opt(",
    "fn fnv1a(",
    "fn expected(line: &str) -> String",
];

#[test]
fn shared_helpers_are_not_copied_into_test_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/it");
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file names the helpers in its own needles; it is not one of the copies.
        if path.file_name().and_then(|n| n.to_str()) == Some("shared_helpers.rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for needle in SHARED {
            *counts.entry(needle).or_default() += text.matches(needle).count();
        }
    }
    for needle in SHARED {
        assert_eq!(
            counts.get(needle).copied().unwrap_or(0),
            0,
            "{needle} is copied into a test file; use tests/it/common/mod.rs instead"
        );
    }

    // And the shared module really defines them: a tree with neither copies nor definitions
    // would otherwise pass this test.
    let common = fs::read_to_string(dir.join("common").join("mod.rs")).unwrap();
    for needle in [
        "pub fn rom_file(",
        "pub fn rom_file_opt(",
        "pub fn expected(", // (line: &str) -> String
    ] {
        assert_eq!(
            common.matches(needle).count(),
            1,
            "{needle} must be defined once in tests/it/common/mod.rs"
        );
    }
}
