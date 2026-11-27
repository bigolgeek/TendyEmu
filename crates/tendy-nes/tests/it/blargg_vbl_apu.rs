//! blargg ppu_vbl_nmi 1-10 (except 04, in clocked_bus.rs) and apu_test 4-6, run on the clocked bus.

use crate::common::run_blargg;

#[test]
fn blargg_vbl_basics() {
    let (status, text) = run_blargg("01-vbl_basics.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_vbl_set_time() {
    let (status, text) = run_blargg("02-vbl_set_time.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_vbl_clear_time() {
    let (status, text) = run_blargg("03-vbl_clear_time.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_nmi_timing() {
    let (status, text) = run_blargg("05-nmi_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_suppression() {
    let (status, text) = run_blargg("06-suppression.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_nmi_on_timing() {
    let (status, text) = run_blargg("07-nmi_on_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_nmi_off_timing() {
    let (status, text) = run_blargg("08-nmi_off_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_even_odd_frames() {
    let (status, text) = run_blargg("09-even_odd_frames.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_even_odd_timing() {
    let (status, text) = run_blargg("10-even_odd_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
    // The four subtests print the frame phase they measured; `08 08 09 07` is the ROM's answer key,
    // so pin it instead of only the pass byte.
    assert!(
        text.contains("08 08 09 07"),
        "expected prints \"08 08 09 07\", got: {text:?}"
    );
}

#[test]
fn blargg_apu_jitter() {
    let (status, text) = run_blargg("4-jitter.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_apu_len_timing() {
    let (status, text) = run_blargg("5-len_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
}

#[test]
fn blargg_apu_irq_flag_timing() {
    let (status, text) = run_blargg("6-irq_flag_timing.nes", 600);
    assert_eq!(status, 0, "{text}");
}
