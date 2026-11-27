//! Screen-text ROMs: some blargg tests print only to nametable 0 (harness HSCR).

use tendy_nes::nes::Nes;

use crate::common::rom_file;

/// Nametable 0 as 30 text rows (blargg font: tile number = ASCII); other bytes become '.'.
fn screen_text(nes: &Nes) -> String {
    let mut s = String::new();
    for row in 0..30 {
        for col in 0..32 {
            let b = nes.bus.ppu.vram[row * 32 + col];
            s.push(if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        s.push('\n');
    }
    s
}

/// Run `name` for `frames` frames and return nametable 0 read as text.
fn run_screen(name: &str, frames: usize) -> String {
    let mut nes = Nes::new(&rom_file(name)).unwrap();
    for _ in 0..frames {
        nes.run_frame();
    }
    screen_text(&nes)
}

#[test]
fn cpu_dummy_reads() {
    let screen = run_screen("cpu_dummy_reads.nes", 300);
    assert!(screen.contains("Passed"), "screen text: {screen}");
}

#[test]
fn dma_2007_write() {
    let screen = run_screen("dma_2007_write.nes", 300);
    assert!(screen.contains("Passed"), "screen text: {screen}");
}

#[test]
fn read_write_2007() {
    let screen = run_screen("read_write_2007.nes", 300);
    assert!(screen.contains("Passed"), "screen text: {screen}");
}

#[test]
fn dma_2007_read() {
    let screen = run_screen("dma_2007_read.nes", 300);
    assert!(
        ["159A7A8F", "5E3DF9C4"]
            .iter()
            .any(|crc| screen.contains(crc)),
        "screen text: {screen}"
    );
}

#[test]
fn double_2007_read() {
    let screen = run_screen("double_2007_read.nes", 300);
    assert!(
        ["85CFD627", "F018C287", "440EF923", "E52F41A5"]
            .iter()
            .any(|crc| screen.contains(crc)),
        "screen text: {screen}"
    );
}

#[test]
fn dma_4016_read() {
    let screen = run_screen("dma_4016_read.nes", 300);
    assert!(screen.contains("Passed"), "screen text: {screen}");
}
