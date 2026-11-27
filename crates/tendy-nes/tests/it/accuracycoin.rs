//! AccuracyCoin scoreboard: runs every scored test page of AccuracyCoin headlessly, reads each test's
//! result byte from RAM ($0400-$04FF) and fails if a test listed in `accuracycoin_baseline.txt` no
//! longer passes. Set ACCURACYCOIN_WRITE_BASELINE=1 to rewrite the baseline from the current results.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tendy_nes::controller::{BUTTON_A, BUTTON_B, BUTTON_DOWN, BUTTON_RIGHT, BUTTON_UP};
use tendy_nes::nes::Nes;

use crate::common::{page_complete, rom_file, wait_page_complete};

/// Every scored test as (menu page, result byte address, name), from `accuracycoin_tests.txt`
/// (lines "page address name", address in hex).
fn tests() -> Vec<(usize, u16, String)> {
    include_str!("../accuracycoin_tests.txt")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut parts = l.splitn(3, ' ');
            let page = parts.next().unwrap().parse().unwrap();
            let addr = u16::from_str_radix(parts.next().unwrap(), 16).unwrap();
            (page, addr, parts.next().unwrap().to_string())
        })
        .collect()
}

/// Tests that hang the emulator today, as (page, row): they are marked skipped (B) before their page runs.
const HANGS: [(usize, usize); 0] = [];

/// Hold `button` on joypad 1 for 2 frames, then release it for 12.
fn press(nes: &mut Nes, button: u8) {
    nes.bus.pads[0].buttons = button;
    for _ in 0..2 {
        nes.run_frame();
    }
    nes.bus.pads[0].buttons = 0;
    for _ in 0..12 {
        nes.run_frame();
    }
}

/// Frames a page's result bytes must stay unchanged before the wait trusts them, and the ceiling on
/// frames spent waiting for them. `settle` gets the same ceiling as its fallback.
const RECHECK_FRAMES: usize = 60;
const SETTLE_LIMIT: usize = 12000;

/// Run frames until the picture has not changed for 900 frames in a row, or `limit` frames pass.
///
/// Only the fallback: `run_page` ends a page when its result bytes settle (`wait_page_complete`),
/// which costs 60 frames instead of 900. A page whose tests hang never settles its bytes, and the
/// picture-still rule still ends it - which is why this exists at all.
fn settle(nes: &mut Nes, limit: usize) {
    let mut last = Vec::new();
    let mut stable = 0;
    for _ in 0..limit {
        nes.run_frame();
        if nes.bus.ppu.framebuffer == last {
            stable += 1;
            if stable == 900 {
                return;
            }
        } else {
            stable = 0;
            last.clone_from(&nes.bus.ppu.framebuffer);
        }
    }
}

/// Boot, open `page`, mark its hanging tests skipped, run every test on the page and wait for it to finish.
fn run_page(rom: &[u8], page: usize) -> Nes {
    let mut nes = Nes::new(rom).unwrap();
    for _ in 0..180 {
        nes.run_frame();
    }
    for _ in 1..page {
        press(&mut nes, BUTTON_RIGHT);
    }
    let mut row = 0;
    for &(_, hang_row) in HANGS.iter().filter(|(p, _)| *p == page) {
        while row < hang_row {
            press(&mut nes, BUTTON_DOWN);
            row += 1;
        }
        press(&mut nes, BUTTON_B);
    }
    while row > 0 {
        press(&mut nes, BUTTON_UP);
        row -= 1;
    }
    press(&mut nes, BUTTON_A);
    // End the page when every result byte of it has been written, not after 900 still
    // frames. `0` is "not run", so the bytes settle when the last test on the page finishes.
    let addrs: Vec<u16> = tests()
        .iter()
        .filter(|t| t.0 == page)
        .map(|t| t.1)
        .collect();
    let (_, confirmed) = wait_page_complete(SETTLE_LIMIT, RECHECK_FRAMES, || {
        nes.run_frame();
        addrs
            .iter()
            .map(|&addr| nes.bus.ram[usize::from(addr)])
            .collect()
    });
    if !confirmed {
        settle(&mut nes, SETTLE_LIMIT);
    }
    nes
}

/// How AccuracyCoin reports a result byte: $FF skipped, 0 not run, bit 0 set = pass, otherwise the error code is byte >> 2.
fn status(byte: u8) -> String {
    match byte {
        0xFF => "SKIP".to_string(),
        0 => "NOT RUN".to_string(),
        b if b & 1 == 1 => "PASS".to_string(),
        b => format!("FAIL {:X}", b >> 2),
    }
}

#[test]
fn accuracycoin_scoreboard() {
    let rom = rom_file("AccuracyCoin.nes");
    let tests = tests();
    // Every page boots its own `Nes` and the pages share nothing, so run them on a
    // thread pool (the pattern `tasvideos_scoreboard` uses) and read the results in page order.
    // The `Nes` holds a `Box<dyn Mapper>`, which is not `Send`, so the machine stays in its
    // thread: only that page's result bytes are handed back.
    let pages: Vec<usize> = (1..=22).filter(|&p| p != 16).collect();
    let next = AtomicUsize::new(0);
    let page_bytes: Mutex<Vec<Option<Vec<u8>>>> = Mutex::new(pages.iter().map(|_| None).collect());
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= pages.len() {
                        break;
                    }
                    let nes = run_page(&rom, pages[i]);
                    let bytes: Vec<u8> = tests
                        .iter()
                        .filter(|t| t.0 == pages[i])
                        .map(|(_, addr, _)| nes.bus.ram[usize::from(*addr)])
                        .collect();
                    page_bytes.lock().unwrap()[i] = Some(bytes);
                }
            });
        }
    });
    let page_bytes: Vec<Vec<u8>> = page_bytes
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|b| b.unwrap())
        .collect();
    let mut results = Vec::new();
    for (&page, bytes) in pages.iter().zip(&page_bytes) {
        for ((p, addr, name), &byte) in tests.iter().filter(|t| t.0 == page).zip(bytes) {
            results.push((*p, *addr, name.as_str(), byte));
        }
    }
    assert_eq!(results.len(), 147);

    let passing: Vec<u16> = results
        .iter()
        .filter(|r| r.3 & 1 == 1 && r.3 != 0xFF)
        .map(|r| r.1)
        .collect();
    for &(page, addr, name, byte) in &results {
        println!("page {page:2} ${addr:04X} {:8} {name}", status(byte));
    }
    println!("AccuracyCoin: {} / {} pass", passing.len(), results.len());

    let baseline_path = format!(
        "{}/tests/accuracycoin_baseline.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var("ACCURACYCOIN_WRITE_BASELINE").is_ok() {
        let mut text =
            String::from("# AccuracyCoin tests that must keep passing: result address and name.\n");
        for &(_, addr, name, byte) in &results {
            if byte & 1 == 1 && byte != 0xFF {
                text.push_str(&format!("{addr:04X} {name}\n"));
            }
        }
        std::fs::write(&baseline_path, text).unwrap();
    }
    let baseline = std::fs::read_to_string(&baseline_path).unwrap();
    let mut regressions = Vec::new();
    let mut expected = Vec::new();
    for line in baseline
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let addr = u16::from_str_radix(&line[..4], 16).unwrap();
        expected.push(addr);
        if !passing.contains(&addr) {
            regressions.push(line.to_string());
        }
    }
    for &(_, addr, name, _) in results
        .iter()
        .filter(|r| passing.contains(&r.1) && !expected.contains(&r.1))
    {
        println!("newly passing (add to the baseline): {addr:04X} {name}");
    }
    assert!(
        regressions.is_empty(),
        "tests that passed before now fail: {regressions:?}"
    );
}

#[test]
fn page_complete_needs_every_result_byte_written() {
    assert!(!page_complete(&[]), "an empty page is not a finished page");
    assert!(!page_complete(&[1, 0]), "0 means the test has not run");
    assert!(!page_complete(&[0xFF, 0]));
    assert!(
        page_complete(&[1, 0xF2]),
        "odd is a pass, an error code is not"
    );
    assert!(
        page_complete(&[0xFF]),
        "a skipped test writes 0xFF, so the byte is written"
    );
}

#[test]
fn wait_page_complete_rechecks_after_the_last_byte_is_written() {
    // Every byte reads written from the first frame, so the wait ends exactly when the recheck
    // does: one sampling frame plus 60 frames unchanged.
    let (frames, confirmed) = wait_page_complete(1000, 60, || vec![1, 2, 3]);
    assert!(confirmed);
    assert_eq!(frames, 61);
}

#[test]
fn wait_page_complete_rechecks_again_when_a_byte_changes() {
    // Complete from frame 1, but byte 1 is rewritten 2 -> 3 on frame 10: the first recheck breaks
    // (8 quiet frames), and the wait only ends after 60 quiet frames at the new value.
    let mut frame_no = 0;
    let (frames, confirmed) = wait_page_complete(1000, 60, || {
        frame_no += 1;
        vec![1, if frame_no < 10 { 2 } else { 3 }, 3]
    });
    assert!(confirmed);
    assert_eq!(frames, 71);
}

#[test]
fn wait_page_complete_stays_bounded_when_a_page_keeps_flapping() {
    // Complete on every frame, but a byte changes every 30 frames, so no recheck of 60 ever
    // succeeds. The wait must still stop at `limit` frames plus the recheck it was in the middle
    // of - not run `limit` rechecks of 60 frames each (that is 30 000 frames here).
    let mut frame_no = 0;
    let (frames, confirmed) = wait_page_complete(1000, 60, || {
        frame_no += 1;
        vec![1, 2 + (frame_no / 30) as u8]
    });
    assert!(!confirmed);
    assert_eq!(frames, frame_no);
    assert!(frames <= 1000 + 60, "ran {frames} frames");
}

#[test]
fn wait_page_complete_gives_up_at_the_limit() {
    // A test that never writes its result byte: 0 means "not run", so the page never completes and
    // the caller's settle takes over.
    let mut frame_no = 0;
    let (frames, confirmed) = wait_page_complete(500, 60, || {
        frame_no += 1;
        vec![1, 0]
    });
    assert!(!confirmed);
    assert_eq!((frames, frame_no), (500, 500));
}
