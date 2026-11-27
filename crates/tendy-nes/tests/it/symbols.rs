use std::path::PathBuf;
use tendy_nes::symbols::Symbols;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/symbols")
        .join(name)
}

fn rows(s: &Symbols) -> Vec<(u16, &str)> {
    s.labels.iter().map(|(a, n)| (*a, n.as_str())).collect()
}

#[test]
fn sample_dbg_imports_three_labels() {
    let s = Symbols::load(&[fixture("sample.dbg")]).unwrap();
    assert_eq!(
        rows(&s),
        vec![
            (0x0010, "screen_x"),
            (0x8000, "main"),
            (0x8020, "irq_handler")
        ]
    );
}

#[test]
fn sample_nl_imports_three_labels() {
    let s = Symbols::load(&[fixture("sample.nl")]).unwrap();
    assert_eq!(
        rows(&s),
        vec![
            (0x0010, "screen_x"),
            (0x8000, "main"),
            (0x8020, "irq_handler")
        ]
    );
}

#[test]
fn broken_dbg_warns_and_skips_the_bad_line() {
    let s = Symbols::load(&[fixture("broken.dbg")]).expect("a bad line is not a load error");
    assert!(
        s.labels.iter().all(|(_, n)| n != "broken"),
        "{:?}",
        s.labels
    );
}

#[test]
fn unreadable_file_is_still_an_error() {
    let e = Symbols::load(&[fixture("does-not-exist.dbg")]).unwrap_err();
    assert!(e.contains("does-not-exist.dbg"), "{e}");
}

#[test]
fn merged_load_is_sorted_and_deduped() {
    // sample.dbg and sample.nl name the same three labels (so merging them dedupes), and other.nl
    // is disjoint (so the union really is a union): six distinct labels, sorted by address.
    let s = Symbols::load(&[
        fixture("sample.dbg"),
        fixture("sample.nl"),
        fixture("other.nl"),
    ])
    .unwrap();
    assert_eq!(
        rows(&s),
        vec![
            (0x0010, "screen_x"),
            (0x0030, "frame_ready"),
            (0x8000, "main"),
            (0x8020, "irq_handler"),
            (0x8100, "nmi_handler"),
            (0x8200, "oam_dma_done"),
        ]
    );
}

#[test]
fn other_nl_adds_three_disjoint_labels() {
    let s = Symbols::load(&[fixture("other.nl")]).unwrap();
    assert_eq!(
        rows(&s),
        vec![
            (0x0030, "frame_ready"),
            (0x8100, "nmi_handler"),
            (0x8200, "oam_dma_done")
        ]
    );
}

#[test]
fn name_at_finds_irq_handler() {
    let s = Symbols::load(&[fixture("sample.dbg"), fixture("sample.nl")]).unwrap();
    assert_eq!(s.name_at(0x8020), Some("irq_handler"));
    assert_eq!(s.name_at(0x8021), None);
}

#[test]
fn unknown_extension_is_rejected() {
    let e = Symbols::load(&[fixture("labels.txt")]).unwrap_err();
    assert!(e.contains("unknown symbol file extension"), "{e}");
}

#[test]
fn parse_dbg_works_without_a_path() {
    let s = Symbols::parse_dbg("sym\tname=\"main\",val=0x8000,type=lab\n").unwrap();
    assert_eq!(s.name_at(0x8000), Some("main"));
}

/// asm6f's `-n` option writes FCEUX-format `.nl` files (`<rom>.0.nl`, `<rom>.ram.nl`), so a
/// build assembled with it loads through the same `Symbols::load` as a hand-made fixture.
#[test]
fn asm6f_nl_imports_its_labels() {
    let s = Symbols::load(&[fixture("asm6f.nl")]).unwrap();
    assert_eq!(
        rows(&s),
        vec![(0x0010, "FrameCount"), (0xC000, "Reset"), (0xC020, "Sub")]
    );
}
