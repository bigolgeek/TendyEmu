//! The pin-file reader against real files under `baselines/` and against hand-written text.

use std::path::PathBuf;

use tendy_core::pin::{Pin, parse};

/// The two-text run layout: comments and blanks are ignored, values keep their spaces.
#[test]
fn pin_parse_reads_runs_and_keys() {
    let text = "\
# what the source core printed at the pinned commit
[gb/dmg-acid2]
frame@60 = 0123456789abcdef
frame@300 = fedcba9876543210

[nes/cart]
frame@60 = 0000000000000001
refusal = refused: DSP-1, not in v1 ($FFD6 = $03)
";
    let pins = parse(text).expect("the text parses");
    assert_eq!(pins.len(), 4);
    assert_eq!(
        pins[0],
        Pin {
            run: "gb/dmg-acid2".into(),
            key: "frame@60".into(),
            value: "0123456789abcdef".into(),
        }
    );
    assert_eq!(pins[1].run, "gb/dmg-acid2");
    assert_eq!(pins[2].run, "nes/cart");
    assert_eq!(pins[3].key, "refusal");
    // The split is at the first `=`, so a refusal keeps its own `$FFD6 = $03`.
    assert_eq!(pins[3].value, "refused: DSP-1, not in v1 ($FFD6 = $03)");
}

/// A line that is neither a header, a pair, a comment nor blank is an error naming its line.
#[test]
fn pin_parse_rejects_line_without_equals() {
    let text = "[gb/dmg-acid2]\nframe@60 0123456789abcdef\n";
    let error = parse(text).expect_err("a pair without = is refused");
    assert!(error.contains("line 2"), "{error}");
    assert!(error.contains("key = value"), "{error}");
}

/// Every pin file committed under `baselines/` parses, and every pin names a run and a value.
#[test]
fn pin_files_parse() {
    let baselines = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../baselines");
    let mut files = 0;
    let mut pins = 0;
    for entry in std::fs::read_dir(&baselines).expect("baselines/ exists") {
        let path = entry.expect("the directory reads").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name != "pins.toml" && !name.ends_with(".pins") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a pin file reads");
        let parsed = parse(&text).unwrap_or_else(|why| panic!("{}: {why}", path.display()));
        assert!(
            !parsed.is_empty(),
            "{}: a pin file with no pins",
            path.display()
        );
        for pin in &parsed {
            assert!(!pin.run.is_empty());
            assert!(!pin.key.is_empty());
            assert!(!pin.value.is_empty());
        }
        files += 1;
        pins += parsed.len();
    }
    assert!(files > 0, "no pin files under {}", baselines.display());
    assert!(pins > 0);
}
