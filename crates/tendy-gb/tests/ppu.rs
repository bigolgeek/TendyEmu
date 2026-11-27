//! The PPU's tests.
//!
//! Most tests here drive `Ppu` directly; the ones that need a whole machine use `run_frame`, whose
//! CPU is the real one, so its step length — not one M-cycle per step — decides where a
//! frame boundary falls.

mod common;

use tendy_gb::bus::{IF_STAT, IF_VBLANK};
use tendy_gb::ppu::{Mode, Ppu};
use tendy_gb::{DOTS_PER_FRAME, DOTS_PER_LINE, MCYCLES_PER_FRAME, WIDTH};

// ---------------------------------------------------------------- helpers (this file only)

fn ppu() -> Ppu {
    Ppu::new()
}

/// Ticks until the PPU reports a finished frame, then releases it.
fn run_frame(p: &mut Ppu) {
    p.frame_ready = false;
    let mut guard = 0;
    while !p.frame_ready {
        p.tick(4);
        guard += 1;
        assert!(guard <= MCYCLES_PER_FRAME, "frame did not complete");
    }
    assert!(p.take_frame());
}

fn pixel(p: &Ppu, x: usize, y: usize) -> u8 {
    p.frame[y * WIDTH + x]
}

/// One 8x8 tile in the `$8000` block from its two bitplanes per line.
fn put_tile(p: &mut Ppu, tile: usize, rows: [(u8, u8); 8]) {
    for (line, (lo, hi)) in rows.iter().enumerate() {
        p.vram[tile * 16 + line * 2] = *lo;
        p.vram[tile * 16 + line * 2 + 1] = *hi;
    }
}

/// A tile whose every pixel is colour index `color`.
fn solid_tile(p: &mut Ppu, tile: usize, color: u8) {
    let lo = if color & 1 != 0 { 0xFF } else { 0x00 };
    let hi = if color & 2 != 0 { 0xFF } else { 0x00 };
    put_tile(p, tile, [(lo, hi); 8]);
}

/// A map entry; `map` 0 is `$9800`, 1 is `$9C00`.
fn set_map(p: &mut Ppu, map: usize, col: usize, row: usize, tile: u8) {
    let base = if map == 0 { 0x1800 } else { 0x1C00 };
    p.vram[base + row * 32 + col] = tile;
}

/// A sprite by screen position and attributes (`OAM`: stored Y is `screen y + 16`, X is `x + 8`).
fn put_sprite(p: &mut Ppu, index: usize, y: u8, x: u8, tile: u8, attr: u8) {
    p.oam[index * 4] = y.wrapping_add(16);
    p.oam[index * 4 + 1] = x.wrapping_add(8);
    p.oam[index * 4 + 2] = tile;
    p.oam[index * 4 + 3] = attr;
}

/// `BGP` with no remapping: index 0-3 becomes shade 0-3.
const IDENTITY: u8 = 0xE4;

// ---------------------------------------------------------------- timing

#[test]
fn frame_is_154_lines_of_456_dots() {
    assert_eq!(
        DOTS_PER_LINE * tendy_gb::LINES_PER_FRAME,
        tendy_gb::DOTS_PER_FRAME
    );

    let mut p = ppu();
    // 70 220 dots leave the frame unfinished; the last four dots of line 153 end it.
    for _ in 0..(tendy_gb::DOTS_PER_FRAME - 4) / 4 {
        p.tick(4);
    }
    assert_eq!((p.frames, p.frame_ready), (0, false));
    p.tick(4);
    assert_eq!((p.frames, p.frame_ready), (1, true));
    assert_eq!((p.ly, p.dot), (0, 0));
    p.take_frame();

    // A second frame changes LY exactly 154 times (0 -> 1 ... 153 -> 0).
    let mut lines = 0;
    let mut last = p.ly;
    for _ in 0..MCYCLES_PER_FRAME {
        p.tick(4);
        if p.ly != last {
            lines += 1;
            last = p.ly;
        }
    }
    assert_eq!(lines, 154);
    assert_eq!((p.ly, p.dot, p.frames), (0, 0, 2));
}

#[test]
fn mode_sequence_per_line() {
    let mut p = ppu();
    assert_eq!(p.mode, Mode::OamScan);
    p.tick(79);
    assert_eq!((p.ly, p.dot, p.mode), (0, 79, Mode::OamScan));
    p.tick(1);
    assert_eq!(
        (p.dot, p.mode),
        (80, Mode::Drawing),
        "mode 3 starts at dot 80"
    );
    p.tick(171);
    assert_eq!((p.dot, p.mode), (251, Mode::Drawing));
    p.tick(1);
    assert_eq!((p.dot, p.mode), (252, Mode::HBlank), "172 dots of mode 3");
    p.tick(203);
    assert_eq!((p.ly, p.dot, p.mode), (0, 455, Mode::HBlank));
    p.tick(1);
    assert_eq!((p.ly, p.dot, p.mode), (1, 0, Mode::OamScan));

    // SCX % 8 lengthens mode 3 and shortens mode 0 by the same amount (§4.4).
    let mut q = ppu();
    q.scx = 5;
    q.tick(256);
    assert_eq!((q.dot, q.mode), (256, Mode::Drawing));
    q.tick(1);
    assert_eq!((q.dot, q.mode), (257, Mode::HBlank));

    // Lines 144-153 are mode 1, all 456 dots of them (LY is at 1 here).
    for _ in 0..143 {
        p.tick(DOTS_PER_LINE);
    }
    assert_eq!((p.ly, p.dot, p.mode), (144, 0, Mode::VBlank));
    p.tick(455);
    assert_eq!(p.mode, Mode::VBlank);
}

#[test]
fn vblank_interrupt_at_line_144() {
    let mut p = ppu();
    for line in 0..143 {
        let bits = p.tick(DOTS_PER_LINE);
        assert_eq!(bits & IF_VBLANK, 0, "VBlank raised on line {line}");
    }
    assert_eq!(p.ly, 143);
    let bits = p.tick(DOTS_PER_LINE);
    assert_eq!(bits & IF_VBLANK, IF_VBLANK, "line 144 raises VBlank");
    assert_eq!((p.ly, p.dot, p.mode), (144, 0, Mode::VBlank));
    assert!(
        !p.frame_ready,
        "the frame ends at the line-153 wrap, not here"
    );

    // STAT's mode-1 select rides the same edge.
    let mut q = ppu();
    q.write(0xFF41, 0x10);
    for _ in 0..143 {
        let bits = q.tick(DOTS_PER_LINE);
        assert_eq!(bits & (IF_VBLANK | IF_STAT), 0);
    }
    let bits = q.tick(DOTS_PER_LINE);
    assert_eq!(bits & IF_VBLANK, IF_VBLANK);
    assert_eq!(bits & IF_STAT, IF_STAT);
}

#[test]
fn stat_lyc_interrupt_edges() {
    let mut p = ppu();
    p.write(0xFF45, 10); // LYC = 10
    p.write(0xFF41, 0x40); // the LYC=LY interrupt, nothing else
    assert_eq!(p.read(0xFF41) & 0x04, 0x00);

    let mut edge_lines = Vec::new();
    let mut flag_lines = Vec::new();
    for _ in 0..14 {
        let bits = p.tick(DOTS_PER_LINE);
        if bits & IF_STAT != 0 {
            edge_lines.push(p.ly);
        }
        if p.read(0xFF41) & 0x04 != 0 {
            flag_lines.push(p.ly);
        }
    }
    assert_eq!(
        edge_lines,
        vec![10],
        "one rising edge, on the line LYC names"
    );
    assert_eq!(flag_lines, vec![10], "STAT bit 2 is LY == LYC");
    assert!(!p.stat_line, "the condition falls once LY passes LYC");

    // Moving LYC produces one more edge, not a stuck line.
    p.write(0xFF45, 20);
    let mut second = Vec::new();
    for _ in 0..6 {
        let bits = p.tick(DOTS_PER_LINE);
        if bits & IF_STAT != 0 {
            second.push(p.ly);
        }
    }
    assert_eq!(second, vec![20]);
}

#[test]
fn stat_write_can_request_interrupt() {
    // UNVERIFIED: the exact cycle of DMG's spurious STAT interrupt; the model here is Pan Docs'
    // "as if $FF were written for one M-cycle, then the written value" (STAT, §4.5), settled by
    // mooneye acceptance/lcdon_timing-GS plus a manual check on Ocean's Road Rash.
    // The quirk only exists while the PPU runs: with the LCD off the mode field *reports* 0 rather
    // than the PPU being in HBlank, nothing is scanned, so the $FF cycle selects nothing and the
    // line stays low. Joe & Mac and Super Scrabble depend on that — both clear the display, write
    // STAT, and their STAT handler then waits for LY to advance.
    let mut off = ppu();
    off.write(0xFF40, 0x00);
    assert_eq!(off.mode, Mode::HBlank);
    assert_eq!(off.write(0xFF41, 0x00), 0, "the display is off: no request");

    // In HBlank with the display on, the $FF cycle enables the mode-0 condition: a write raises
    // STAT even when the written value selects nothing.
    let mut p = ppu();
    p.write(0xFF45, 5); // LY is 0, so LYC never matches
    p.tick(400);
    assert_eq!(p.mode, Mode::HBlank);
    assert_eq!(p.write(0xFF41, 0x00), IF_STAT, "writing $00 can fire it");

    // It takes a rising edge: a write while the line is already high requests nothing.
    let mut q = ppu();
    q.write(0xFF45, 5);
    q.tick(400);
    assert_eq!(q.mode, Mode::HBlank);
    assert_eq!(q.write(0xFF41, 0x08), IF_STAT);
    assert_eq!(q.write(0xFF41, 0x08), 0, "already high, no edge");

    // Mode 3 with LY != LYC is the one place $FF selects nothing: quiet.
    let mut r = ppu();
    r.write(0xFF45, 5); // LY is 0 in mode 3
    r.tick(80);
    assert_eq!(r.mode, Mode::Drawing);
    assert_eq!(r.write(0xFF41, 0x40), 0);
}

// ---------------------------------------------------------------- background and window

#[test]
fn scx_scy_wrap_around_the_map() {
    let mut p = ppu();
    for tile in 0..4 {
        solid_tile(&mut p, tile, tile as u8);
    }
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut p, 0, col, row, ((row + col) & 3) as u8);
        }
    }
    p.write(0xFF47, IDENTITY);
    p.write(0xFF43, 250); // SCX
    p.write(0xFF42, 255); // SCY
    run_frame(&mut p);

    for (x, y) in [
        (0usize, 0usize),
        (5, 0),
        (6, 0),
        (7, 3),
        (159, 143),
        (12, 0),
    ] {
        let map_col = ((250 + x) & 0xFF) >> 3;
        let map_row = ((255 + y) & 0xFF) >> 3;
        let expected = ((map_row + map_col) & 3) as u8;
        assert_eq!(pixel(&p, x, y), expected, "pixel ({x},{y})");
    }
    // The 256-pixel wrap: x = 5 is the last map column, x = 6 is column 0 again.
    assert_eq!(((250usize + 5) & 0xFF) >> 3, 31);
    assert_eq!(((250usize + 6) & 0xFF) >> 3, 0);
    // And the vertical one: line 1 is map row 0.
    assert_eq!(((255usize + 1) & 0xFF) >> 3, 0);
}

#[test]
fn window_starts_at_wx() {
    let mut p = ppu();
    solid_tile(&mut p, 0, 0);
    solid_tile(&mut p, 3, 3);
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut p, 0, col, row, 0);
            set_map(&mut p, 1, col, row, 3);
        }
    }
    p.write(0xFF47, IDENTITY);
    p.write(0xFF40, 0x91 | 0x20 | 0x40); // LCD on, window on, window map $9C00
    p.write(0xFF4A, 0); // WY
    p.write(0xFF4B, 50); // WX: the first window pixel is screen x = 43
    run_frame(&mut p);
    assert_eq!(pixel(&p, 42, 0), 0);
    assert_eq!(pixel(&p, 43, 0), 3);
    assert_eq!(pixel(&p, 159, 0), 3);
    assert_eq!(pixel(&p, 43, 143), 3, "the window is on every line");

    // WX = 7 is the left edge.
    let mut q = ppu();
    solid_tile(&mut q, 0, 0);
    solid_tile(&mut q, 3, 3);
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut q, 0, col, row, 0);
            set_map(&mut q, 1, col, row, 3);
        }
    }
    q.write(0xFF47, IDENTITY);
    q.write(0xFF40, 0x91 | 0x20 | 0x40);
    q.write(0xFF4B, 7);
    run_frame(&mut q);
    assert_eq!(pixel(&q, 0, 0), 3);
}

#[test]
fn window_row_counter_waits_for_wy() {
    let mut p = ppu();
    solid_tile(&mut p, 0, 0);
    solid_tile(&mut p, 1, 1);
    solid_tile(&mut p, 2, 2);
    for col in 0..32 {
        set_map(&mut p, 0, col, 0, 0);
        set_map(&mut p, 1, col, 0, 1); // window row 0: colour 1
        set_map(&mut p, 1, col, 1, 2); // window row 1: colour 2
        for row in 2..32 {
            set_map(&mut p, 1, col, row, 0);
        }
    }
    p.write(0xFF47, IDENTITY);
    p.write(0xFF40, 0x91 | 0x20 | 0x40);
    p.write(0xFF4A, 5); // WY
    p.write(0xFF4B, 7); // WX: left edge
    run_frame(&mut p);

    assert_eq!(pixel(&p, 0, 4), 0, "no window before WY");
    assert_eq!(pixel(&p, 0, 5), 1, "window row 0 on the line WY names");
    assert_eq!(
        pixel(&p, 0, 12),
        1,
        "window_line counts pixels: line 7 is still map row 0"
    );
    assert_eq!(
        pixel(&p, 0, 13),
        2,
        "window_line 8 is the second tile row of the map"
    );
    assert_eq!(pixel(&p, 0, 20), 2, "window_line 15 is still map row 1");
    assert_eq!(
        pixel(&p, 0, 21),
        0,
        "window_line 16 is map row 2, blank here"
    );
}

#[test]
fn window_disabled_when_wx_too_large() {
    let mut p = ppu();
    solid_tile(&mut p, 0, 0);
    solid_tile(&mut p, 3, 3);
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut p, 0, col, row, 0);
            set_map(&mut p, 1, col, row, 3);
        }
    }
    p.write(0xFF47, IDENTITY);
    p.write(0xFF40, 0x91 | 0x20 | 0x40);
    p.write(0xFF4B, 167); // WX > 166: no window at all
    run_frame(&mut p);
    assert!(
        (0..144).all(|y| (0..160).all(|x| pixel(&p, x, y) == 0)),
        "WX = 167 must show no window"
    );

    // WX = 166 is still visible: the window's left edge lands on the last pixel. `UNVERIFIED:`
    // the DMG then shows the window across the whole line, one scanline up (Window); v1 follows
    // the plain "first dot where the counter equals WX" rule and does not model that,
    // which is a granularity miss, not a decision this test can settle.
    let mut q = ppu();
    solid_tile(&mut q, 0, 0);
    solid_tile(&mut q, 3, 3);
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut q, 0, col, row, 0);
            set_map(&mut q, 1, col, row, 3);
        }
    }
    q.write(0xFF47, IDENTITY);
    q.write(0xFF40, 0x91 | 0x20 | 0x40);
    q.write(0xFF4B, 166);
    run_frame(&mut q);
    assert_eq!(pixel(&q, 158, 0), 0);
    assert_eq!(pixel(&q, 159, 0), 3);
}

#[test]
fn bgp_maps_indices_to_shades() {
    let mut p = ppu();
    // Tile 0, every row: colour indices 0,1,2,3,0,1,2,3 from left to right.
    put_tile(&mut p, 0, [(0x55, 0x33); 8]);
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut p, 0, col, row, 0);
        }
    }
    p.write(0xFF47, IDENTITY);
    run_frame(&mut p);
    assert_eq!(
        [
            pixel(&p, 0, 0),
            pixel(&p, 1, 0),
            pixel(&p, 2, 0),
            pixel(&p, 3, 0)
        ],
        [0, 1, 2, 3]
    );

    // 00 01 10 11: index 0 -> shade 3, index 1 -> 2, index 2 -> 1, index 3 -> 0.
    p.write(0xFF47, 0x1B);
    run_frame(&mut p);
    assert_eq!(
        [
            pixel(&p, 0, 0),
            pixel(&p, 1, 0),
            pixel(&p, 2, 0),
            pixel(&p, 3, 0)
        ],
        [3, 2, 1, 0]
    );
}

// ---------------------------------------------------------------- objects

#[test]
fn ten_sprites_per_line_in_oam_order() {
    let mut p = ppu();
    solid_tile(&mut p, 1, 1);
    solid_tile(&mut p, 2, 3);
    p.write(0xFF47, IDENTITY);
    p.write(0xFF48, IDENTITY); // OBP0
    p.write(0xFF40, 0x91 | 0x02); // objects on
    for i in 0..10usize {
        put_sprite(&mut p, i, 0, (i as u8) * 16, 1, 0);
    }
    // Two more objects on the same line, the first of them on top of object 0: OAM order stops at
    // ten, so neither is drawn.
    put_sprite(&mut p, 10, 0, 0, 2, 0);
    put_sprite(&mut p, 11, 0, 8, 2, 0);
    run_frame(&mut p);

    assert_eq!(pixel(&p, 0, 0), 1, "object 0, not the excluded object 10");
    assert_eq!(pixel(&p, 7, 0), 1);
    assert_eq!(pixel(&p, 16, 0), 1);
    assert_eq!(pixel(&p, 95, 0), 0, "the gap between objects 5 and 6");
    assert_eq!(pixel(&p, 144, 0), 1, "object 9 is the tenth and last");
    assert_eq!(pixel(&p, 152, 0), 0, "nothing beyond the tenth");
}

#[test]
fn sprite_priority_is_x_then_index() {
    let mut p = ppu();
    solid_tile(&mut p, 1, 1);
    solid_tile(&mut p, 2, 2);
    p.write(0xFF48, IDENTITY);
    p.write(0xFF40, 0x91 | 0x02);
    // Object 0 covers x 2..9, object 1 covers x 0..7: the lower X wins the overlap 2..7.
    put_sprite(&mut p, 0, 0, 2, 1, 0);
    put_sprite(&mut p, 1, 0, 0, 2, 0);
    run_frame(&mut p);
    assert_eq!(pixel(&p, 0, 0), 2, "object 1 has the lower X");
    assert_eq!(pixel(&p, 7, 0), 2);
    assert_eq!(pixel(&p, 8, 0), 1, "object 0 is the only one there");

    // Equal X: the lower OAM index wins.
    let mut q = ppu();
    solid_tile(&mut q, 1, 1);
    solid_tile(&mut q, 2, 2);
    q.write(0xFF48, IDENTITY);
    q.write(0xFF40, 0x91 | 0x02);
    put_sprite(&mut q, 0, 0, 4, 1, 0);
    put_sprite(&mut q, 1, 0, 4, 2, 0);
    run_frame(&mut q);
    assert_eq!(pixel(&q, 4, 0), 1, "same X: object 0 wins");
}

#[test]
fn attribute_bit7_puts_bg_over_sprite() {
    let mut p = ppu();
    solid_tile(&mut p, 1, 2); // the object's colour
    solid_tile(&mut p, 4, 0); // background, index 0
    solid_tile(&mut p, 5, 1); // background, index 1
    for row in 0..32 {
        for col in 0..32 {
            set_map(&mut p, 0, col, row, if col < 16 { 4 } else { 5 });
        }
    }
    p.write(0xFF47, IDENTITY);
    p.write(0xFF48, IDENTITY);
    p.write(0xFF40, 0x91 | 0x02);
    put_sprite(&mut p, 0, 0, 0, 1, 0x80); // attr 7 over background index 0
    put_sprite(&mut p, 1, 0, 132, 1, 0x80); // attr 7 over background index 1
    put_sprite(&mut p, 2, 0, 64, 1, 0x00); // attr 7 clear
    run_frame(&mut p);

    assert_eq!(pixel(&p, 0, 0), 2, "background index 0: the object shows");
    assert_eq!(pixel(&p, 132, 0), 1, "attribute bit 7 hands index 1-3 back");
    assert_eq!(pixel(&p, 64, 0), 2, "without bit 7 the object wins");
}

#[test]
fn eight_by_sixteen_ignores_tile_bit0() {
    let mut p = ppu();
    solid_tile(&mut p, 4, 1);
    solid_tile(&mut p, 5, 2);
    p.write(0xFF48, IDENTITY);
    p.write(0xFF40, 0x91 | 0x02 | 0x04); // 8x16 objects
    put_sprite(&mut p, 0, 0, 0, 5, 0); // odd tile index: the pair is (4, 5)
    run_frame(&mut p);

    assert_eq!(pixel(&p, 0, 0), 1, "top half is tile 4");
    assert_eq!(pixel(&p, 0, 7), 1);
    assert_eq!(pixel(&p, 0, 8), 2, "bottom half is tile 5");
    assert_eq!(pixel(&p, 0, 15), 2);
}

// ---------------------------------------------------------------- memory gates and LCD off

#[test]
fn vram_locked_outside_modes_0_to_2() {
    let mut p = ppu();
    assert_eq!(p.mode, Mode::OamScan);
    p.write_vram(0x8000, 0, 0x5A);
    assert_eq!(p.read_vram(0x8000, 0), 0x5A);
    assert_eq!(p.read_oam(0xFE00), 0xFF, "OAM is blocked in mode 2");

    // Mode 3 owns both memories.
    p.tick(80);
    assert_eq!(p.mode, Mode::Drawing);
    assert_eq!(p.read_vram(0x8000, 0), 0xFF);
    p.write_vram(0x8000, 0, 0x11);
    assert_eq!(p.vram[0], 0x5A, "the write was dropped");
    assert_eq!(p.read_oam(0xFE00), 0xFF);

    // Mode 0 frees VRAM and OAM again.
    p.tick(200);
    assert_eq!(p.mode, Mode::HBlank);
    assert_eq!(p.read_vram(0x8000, 0), 0x5A);
    p.write_vram(0x8000, 0, 0x22);
    assert_eq!(p.read_vram(0x8000, 0), 0x22);
    p.write_oam(0xFE00, 0x33);
    assert_eq!(p.read_oam(0xFE00), 0x33);

    // Mode 1 is free too.
    for _ in 0..144 {
        p.tick(DOTS_PER_LINE);
    }
    assert_eq!(p.mode, Mode::VBlank);
    assert_eq!(p.read_vram(0x8000, 0), 0x22);
    assert_eq!(p.read_oam(0xFE00), 0x33);
}

#[test]
fn lcd_off_blanks_and_frees_vram() {
    let mut p = ppu();
    solid_tile(&mut p, 1, 3);
    for col in 0..32 {
        set_map(&mut p, 0, col, 0, 1);
    }
    p.write(0xFF47, IDENTITY);
    run_frame(&mut p);
    assert_eq!(pixel(&p, 0, 0), 3, "the LCD drew something first");

    p.write(0xFF40, 0x00);
    assert_eq!((p.ly, p.dot, p.mode), (0, 0, Mode::HBlank));
    assert!(
        p.frame().iter().all(|&shade| shade == 0),
        "the picture blanks"
    );
    assert_eq!(
        p.read(0xFF41) & 0x03,
        0,
        "the mode reads 0 while the LCD is off"
    );
    assert_eq!(p.read(0xFF44), 0);

    // Freed: both memories take reads and writes again, whatever mode would have been.
    p.write_vram(0x8000, 0, 0x77);
    assert_eq!(p.read_vram(0x8000, 0), 0x77);
    p.write_oam(0xFE00, 0x66);
    assert_eq!(p.read_oam(0xFE00), 0x66);
}

/// The longest instruction the CPU can be inside when the PPU ends a frame: 24 dots is `CALL cc`
/// (or `LD (a16),SP`, `JP cc`), 6 M-cycles.
const LONGEST_INSTRUCTION_MCYCLES: u32 = 6;

#[test]
fn lcd_off_still_completes_frames() {
    // 17 556 M-cycles is *nominal* — a frame ends when the PPU says so, and the CPU
    // finishes the instruction it is in, so a frame may run up to one instruction long. With the
    // stub CPU (one M-cycle per step) frames divided exactly; the real CPU idles in a `jr -2`
    // (3 M-cycles) after a long NOP slide, so its frame boundaries land inside an instruction.
    // What must hold instead: every dot is counted once (no leak), no frame drifts by more than
    // one instruction, and the overshoot is carried in `lcd_off_dots` rather than accumulating.
    let mut m = common::machine(common::synth_rom(&common::entry(&[0x00])));
    m.bus.ppu.write(0xFF40, 0x00);
    let mut total = 0u32;
    for frame in 0..10 {
        let before = m.bus.ppu.lcd_off_dots;
        let cycles = m.run_frame();
        let after = m.bus.ppu.lcd_off_dots;
        total += cycles;

        // The off-period counter takes every dot exactly once: what it carried in, plus what the
        // CPU ticked, is one frame of dots plus what it carries out. A missed or repeated dot
        // breaks this even when the M-cycle count still looks plausible.
        assert_eq!(
            u64::from(before) + 4 * u64::from(cycles),
            u64::from(DOTS_PER_FRAME) + u64::from(after),
            "frame {frame}: the off-period counter lost or repeated dots"
        );
        // `frame_ready` is only set inside `tick`, so a frame can never end early; it may end up to
        // one instruction past its 17 556th M-cycle.
        assert!(
            cycles.abs_diff(MCYCLES_PER_FRAME) <= LONGEST_INSTRUCTION_MCYCLES,
            "frame {frame} took {cycles} M-cycles with the LCD off, more than one instruction from \
             {MCYCLES_PER_FRAME}"
        );
        assert!(
            m.frame().iter().all(|&shade| shade == 0),
            "frame {frame} is not blank"
        );
        // Whatever a frame overran by stays in `lcd_off_dots`, i.e. it is less than the one
        // instruction the next frame may have to finish.
        assert!(
            after < 4 * LONGEST_INSTRUCTION_MCYCLES,
            "frame {frame} carried {after} dots, more than one instruction"
        );
    }
    // Ten frames, so a per-frame overshoot that was not given back would show as 10x drift.
    assert!(
        total.abs_diff(10 * MCYCLES_PER_FRAME) <= LONGEST_INSTRUCTION_MCYCLES,
        "10 frames took {total} M-cycles, more than one instruction from {}",
        10 * MCYCLES_PER_FRAME
    );
    assert_eq!((m.frames, m.bus.ppu.frames), (10, 10));

    // Turning it back on draws again, starting with one blank frame (§4.4).
    m.bus.ppu.write(0xFF40, 0x91);
    assert!(m.bus.ppu.first_frame_blank);
    m.run_frame();
    assert!(
        m.frame().iter().all(|&shade| shade == 0),
        "first frame blank"
    );
    assert!(!m.bus.ppu.first_frame_blank);
    assert_eq!(m.frames, 11);
}

// ---------------------------------------------------------------- save state (§6)

#[test]
fn ppu_state_round_trips() {
    // Not a §9.2 name: this guards the §6 row the cartridge encodes, so a state saved mid-frame
    // reloads into a PPU that keeps drawing exactly like the original.
    use tendy_gb::state::{StateReader, StateWriter};

    let mut p = ppu();
    p.write(0xFF40, 0x00); // exercise the off-period counter too
    solid_tile(&mut p, 2, 3);
    p.write_oam(0xFE00, 0x20);
    p.tick(1000);
    p.write(0xFF40, 0x93);
    p.write(0xFF47, 0x1B);
    p.write(0xFF4B, 40);
    p.oam_dma_active = true;
    for _ in 0..500 {
        p.tick(4);
    }

    let mut w = StateWriter::new();
    p.save_state(&mut w);
    let bytes = w.finish();
    let mut q = ppu();
    let mut r = StateReader::new(&bytes);
    q.load_state(&mut r).expect("the state loads");
    assert_eq!(r.remaining(), 0, "the decoder consumed every byte");

    assert_eq!(
        (
            q.lcdc, q.stat, q.scy, q.scx, q.ly, q.lyc, q.bgp, q.obp0, q.obp1, q.wy, q.wx
        ),
        (
            p.lcdc, p.stat, p.scy, p.scx, p.ly, p.lyc, p.bgp, p.obp0, p.obp1, p.wy, p.wx
        )
    );
    assert_eq!((q.dot, q.lcd_off_dots), (p.dot, p.lcd_off_dots));
    assert_eq!(q.mode, p.mode);
    assert_eq!(
        (q.window_line, q.window_y, q.stat_line, q.frame_ready),
        (p.window_line, p.window_y, p.stat_line, p.frame_ready)
    );
    assert_eq!(
        (q.lyc_flag, q.line0_skip_mode2, q.mode3_len),
        (p.lyc_flag, p.line0_skip_mode2, p.mode3_len),
        "the STAT bit-2 latch, line 0's carve-out and the settled mode-3 length must survive a save"
    );
    assert_eq!(
        (q.oam_dma_active, q.first_frame_blank, q.frames),
        (p.oam_dma_active, p.first_frame_blank, p.frames)
    );
    assert_eq!(q.vram, p.vram);
    assert_eq!(q.oam, p.oam);
    assert_eq!(q.frame, p.frame);

    // And they stay in step: same interrupts, same picture.
    for _ in 0..2000 {
        assert_eq!(q.tick(4), p.tick(4));
    }
    assert_eq!((q.ly, q.dot, q.frames), (p.ly, p.dot, p.frames));
    assert_eq!(q.frame(), p.frame());
}

// ---------------------------------------------------------------- the LCD-on and latch rules

#[test]
fn stat_lyc_latch_survives_lcd_off() {
    // `stat_lyc_onoff` round 1: the comparison bit is a latch, and the clock that moves it stops
    // with the LCD, so neither LY = 0 nor a new LYC can change it until the LCD runs again.
    let mut p = ppu();
    p.write(0xFF45, 5); // LYC = 5
    p.write(0xFF41, 0x40); // the LYC = LY interrupt
    for _ in 0..5 {
        p.tick(DOTS_PER_LINE);
    }
    assert_eq!(p.read(0xFF41) & 0x04, 0x04, "LY == LYC on line 5");

    p.write(0xFF40, 0x00);
    assert_eq!(
        p.read(0xFF41) & 0x04,
        0x04,
        "the bit is retained while the LCD is off, not reset to 0"
    );
    p.write(0xFF45, 1);
    assert_eq!(
        p.read(0xFF41) & 0x04,
        0x04,
        "changing LYC has no effect: the comparison clock is not running"
    );
    p.tick(400);
    assert_eq!(p.read(0xFF41) & 0x04, 0x04, "and no dot moves it");

    // Turning it back on restarts the clock: LY = 0 against LYC = 1.
    assert_eq!(
        p.write(0xFF40, 0x91) & IF_STAT,
        0,
        "falling edge, no request"
    );
    assert_eq!(p.read(0xFF41) & 0x04, 0x00);

    // Round 4: the latch rising as the clock restarts does request the interrupt.
    let mut q = ppu();
    q.write(0xFF45, 5);
    q.write(0xFF41, 0x40);
    for _ in 0..20 {
        q.tick(DOTS_PER_LINE);
    }
    assert_eq!(q.read(0xFF41) & 0x04, 0x00);
    q.write(0xFF40, 0x00);
    q.write(0xFF45, 0x00); // LY = 0 and LYC = 0 once the clock restarts
    assert_eq!(
        q.write(0xFF40, 0x91) & IF_STAT,
        IF_STAT,
        "restarting the comparison clock can request the LYC interrupt"
    );

    // And when the comparison result does not change, there is no edge to request (round 2).
    let mut r = ppu();
    r.write(0xFF45, 1);
    r.write(0xFF41, 0x40);
    r.tick(DOTS_PER_LINE); // LY = 1 == LYC
    assert_eq!(r.read(0xFF41) & 0x04, 0x04);
    r.write(0xFF40, 0x00);
    r.write(0xFF45, 0x00); // LY = 0 and LYC = 0 when it comes back: still equal
    assert_eq!(r.write(0xFF40, 0x91) & IF_STAT, 0, "no edge, no request");
    assert_eq!(r.read(0xFF41) & 0x04, 0x04);
}

#[test]
fn vblank_start_raises_the_oam_condition() {
    // `vblank_stat_intr-GS`: the mode-2 select is also asserted for one dot at the start of
    // VBlank, so its interrupt lands with `IF_VBLANK`.
    let mut p = ppu();
    p.write(0xFF41, 0x20); // the OAM (mode 2) interrupt only
    for line in 0..143 {
        let bits = p.tick(DOTS_PER_LINE);
        assert_eq!(bits & IF_STAT, IF_STAT, "line {line} starts in mode 2");
    }
    let bits = p.tick(DOTS_PER_LINE);
    assert_eq!(
        bits & (IF_VBLANK | IF_STAT),
        IF_VBLANK | IF_STAT,
        "line 144 raises VBlank and the mode-2 condition together"
    );

    // It is one dot: the rest of VBlank matches neither mode 2 nor mode 0.
    for line in 145..=153 {
        let bits = p.tick(DOTS_PER_LINE);
        assert_eq!(bits & IF_STAT, 0, "line {line} raises no STAT interrupt");
    }
}

#[test]
fn lcd_on_restarts_line_zero_at_dot_zero() {
    // `lcdon_timing-GS`, `lcdon_write_timing-GS` and `stat_lyc_onoff` all enable the LCD into a
    // running frame: `LY` restarts at line 0, dot 0, line 0 never reports mode 2 and draws from
    // dot 80. Carrying the off-period phase into the enable instead (the dot clock "keeps
    // running", `dot = lcd_off_dots % 456`) fails all three ROMs.
    let mut p = ppu();
    p.write(0xFF40, 0x00);
    p.tick(4 * 10);
    p.write(0xFF40, 0x91);

    assert_eq!(
        (p.ly, p.dot, p.mode),
        (0, 0, Mode::HBlank),
        "the LCD restarts line 0 at dot 0, in mode 0"
    );
    assert_eq!(p.read(0xFF41) & 0x03, 0, "and STAT agrees");
    p.tick(79);
    assert_eq!(
        (p.ly, p.mode),
        (0, Mode::HBlank),
        "line 0 holds mode 0 through the mode-2 window"
    );
    p.tick(1);
    assert_eq!(
        (p.ly, p.dot, p.mode),
        (0, 80, Mode::Drawing),
        "line 0 draws from dot 80"
    );
    p.tick(376);
    assert_eq!(
        (p.ly, p.dot, p.mode),
        (1, 0, Mode::OamScan),
        "line 1 is normal"
    );

    // The first frame after LCD-on is blank, the second is drawn.
    let mut q = ppu();
    solid_tile(&mut q, 1, 3);
    for col in 0..32 {
        set_map(&mut q, 0, col, 0, 1);
    }
    q.write(0xFF47, IDENTITY);
    q.write(0xFF40, 0x00);
    q.tick(1000);
    q.write(0xFF40, 0x91);
    run_frame(&mut q);
    assert!(
        q.frame().iter().all(|&shade| shade == 0),
        "the first frame after LCD-on stays blank"
    );
    run_frame(&mut q);
    assert_eq!(pixel(&q, 0, 0), 3, "the next one draws");
}

// ------------------------------------------------- Dot-accurate STAT/LYC/SCX timing
//
// Each case fails if the fix it names is reverted, and names the Mooneye ROM that found it.

/// A PPU whose LCD has just been turned on: line 0, dot 0, with line 0's carve-outs in place.
///
/// `Ppu::new()` boots with the LCD already enabled (post-boot `LCDC`), so the LCD is switched off
/// first — otherwise the enable edge, and with it line 0's skipped mode 2, never happens.
fn lcd_on(lcdc: u8) -> Ppu {
    let mut p = Ppu::new();
    p.write(0xFF40, 0x00);
    p.write(0xFF40, 0x80 | lcdc);
    assert_eq!((p.ly, p.dot, p.mode), (0, 0, Mode::HBlank));
    p
}

/// Ticks one dot at a time until `line` reports `mode`, returning the dot that first did.
fn wait_mode(p: &mut Ppu, line: u8, mode: Mode) -> u32 {
    let mut guard = 0;
    loop {
        if p.ly == line && p.mode == mode {
            return p.dot;
        }
        p.tick(1);
        guard += 1;
        assert!(
            guard < DOTS_PER_LINE * 6,
            "line {line} never reached {mode:?}"
        );
    }
}

/// Ticks forward until the PPU sits on `dot` of `line`.
fn seek(p: &mut Ppu, line: u8, dot: u32) {
    let mut guard = 0;
    while !(p.ly == line && p.dot == dot) {
        p.tick(1);
        guard += 1;
        assert!(
            guard < DOTS_PER_LINE * 6,
            "dot {dot} of line {line} never reached"
        );
    }
}

/// `hblank_ly_scx_timing-GS`: mode 3 lasts `172 + SCX % 8` dots on a line with no objects, so
/// mode 0 begins at dot 252 + `SCX % 8`.
#[test]
fn mode3_length_follows_scx() {
    let mut p = lcd_on(0x00);
    p.write(0xFF43, 3);
    let start = wait_mode(&mut p, 1, Mode::Drawing);
    let end = wait_mode(&mut p, 1, Mode::HBlank);
    assert_eq!(start, 80);
    assert_eq!(end, 80 + 172 + 3, "mode 3 did not pay SCX % 8");
}

/// `intr_2_mode0_timing_sprites`: an object at `X=0` on the line pays eight dots, not the eleven
/// the documented formula gives, so one such object ends mode 3 at dot 260 (`172 + 8`).
///
/// UNVERIFIED: the three-dot difference is derived from the ROM's own 105-case NOP table, which
/// is calibrated against the same interrupt chain the sprite-less `intr_2_mode0_timing` pins; no
/// gbdev note explains it. See `Ppu::obj_penalty`.
#[test]
fn mode3_length_pays_the_object_penalty() {
    let mut p = lcd_on(0x02); // objects on
    // One object, eight pixels tall, covering line 1 (OAM Y is 16 dots above the line it starts on).
    p.oam[0] = 17;
    p.oam[1] = 0; // X = 0
    let start = wait_mode(&mut p, 1, Mode::Drawing);
    let end = wait_mode(&mut p, 1, Mode::HBlank);
    assert_eq!(start, 80);
    assert_eq!(
        end,
        80 + 172 + 8,
        "one X=0 object: 11 documented dots less 3"
    );
}

/// `lcdon_timing-GS`: the first line after the LCD comes on starts at dot 0, never reports mode 2
/// and enters mode 3 at dot 80; only that line differs.
#[test]
fn lcd_on_line_zero_skips_mode_two() {
    let mut p = lcd_on(0x00);
    assert_eq!((p.ly, p.dot, p.mode), (0, 0, Mode::HBlank));
    seek(&mut p, 0, 76);
    assert_eq!(p.mode, Mode::HBlank, "line 0 reported mode 2");
    seek(&mut p, 0, 80);
    assert_eq!(p.mode, Mode::Drawing, "line 0 did not draw at dot 80");
}

/// `lcdon_timing-GS`: `LY` steps four dots before the mode grid does — at dot 452 the register
/// already reads 1 while STAT still reports mode 0 for line 0.
#[test]
fn ly_steps_four_dots_before_the_mode_grid() {
    let mut p = lcd_on(0x00);
    seek(&mut p, 0, 448);
    assert_eq!(p.read(0xFF44), 0, "LY stepped early");
    seek(&mut p, 0, 452);
    assert_eq!(p.read(0xFF44), 1, "LY had not stepped by dot 452");
    assert_eq!(p.mode, Mode::HBlank);
}

/// `lcdon_timing-GS`: in those same four dots the STAT coincidence bit reads clear even though
/// the latch is set, because LY has already stepped but the comparison has not been re-run.
#[test]
fn stat_lyc_bit_reads_clear_before_the_line_ends() {
    let mut p = lcd_on(0x00);
    p.write(0xFF45, 1); // LYC = 1, the line the flag is set on
    seek(&mut p, 1, 448);
    assert_ne!(p.read(0xFF41) & 0x04, 0, "coincidence bit did not set");
    seek(&mut p, 1, 452);
    assert_eq!(
        p.read(0xFF41) & 0x04,
        0,
        "coincidence bit read set past dot 452"
    );
}

/// `lcdon_write_timing-GS` round 9: mode 2 owns OAM only for its first 76 dots — its last four
/// are the sprite sort, which does not touch OAM, so a write there lands while one at dot 72 does
/// not.
#[test]
fn oam_writes_land_in_the_last_four_dots_of_mode_two() {
    let mut p = lcd_on(0x02);
    seek(&mut p, 1, 72);
    assert_eq!(p.mode, Mode::OamScan);
    p.write_oam(0x0000, 0xAA);
    assert_ne!(p.oam[0], 0xAA, "dot 72 write was not blocked");
    seek(&mut p, 1, 76);
    p.write_oam(0x0000, 0x55);
    assert_eq!(p.oam[0], 0x55, "dot 76 write was blocked");
}

/// `lcdon_timing-GS` pass 2: the next line's object scan has already begun in the last four dots
/// of mode 0, so an OAM *read* returns `$FF` there while a read at dot 448 still answers.
#[test]
fn oam_reads_are_held_off_before_the_line_ends() {
    let mut p = lcd_on(0x02);
    p.oam[0] = 0x5A;
    seek(&mut p, 0, 448);
    assert_eq!(p.read_oam(0x0000), 0x5A);
    seek(&mut p, 0, 452);
    assert_eq!(p.mode, Mode::HBlank);
    assert_eq!(p.read_oam(0x0000), 0xFF);
}

/// `lcdon_timing-GS` pass 2 vs `lcdon_write_timing-GS` round 16: VRAM reads are held off for the
/// last four dots of mode 2 (the fetcher has started) but writes still land there.
#[test]
fn vram_read_is_held_off_at_the_end_of_mode_two_but_writes_land() {
    let mut p = lcd_on(0x00);
    p.vram[0] = 0x5A;
    seek(&mut p, 0, 76); // line 0: mode 0 thanks to the LCD-on carve-out
    assert_eq!(p.read_vram(0x8000, 0), 0x5A);
    seek(&mut p, 1, 76);
    assert_eq!(p.mode, Mode::OamScan);
    assert_eq!(
        p.read_vram(0x8000, 0),
        0xFF,
        "read at dot 76 of mode 2 was not blocked"
    );
    p.write_vram(0x8000, 0, 0xA5);
    assert_eq!(p.vram[0], 0xA5, "write at dot 76 of mode 2 was blocked");
}
