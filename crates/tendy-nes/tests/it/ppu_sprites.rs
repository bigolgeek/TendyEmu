use tendy_nes::cart::Mapper;
use tendy_nes::mapper::load;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::Ppu;

use crate::common::rom_file;

fn chr_ram_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384, 0);
    v
}

fn w(p: &mut Ppu, c: &mut dyn Mapper, r: u16, v: u8) {
    p.write_register(c, r, v);
}

/// CHR tile 1 has one pixel (colour 1) at the left of its top row; tile 2 is solid colour 1.
/// With `bg_tile`, nametable cell (column 2, row 1) holds tile 2 (pixels x 16..=23, lines 8..=15).
/// Backdrop 0x0F, background colour 0x16, sprite palette 4 colour 1 is 0x21.
/// OAM gets `sprites` (y, tile, attr, x) followed by hidden sprites (y = 0xFF).
/// Renders one full frame plus 241 lines and returns the PPU.
fn run(sprites: &[[u8; 4]], ctrl: u8, mask: u8, bg_tile: bool) -> Ppu {
    run_opt(sprites, ctrl, mask, bg_tile, false).0
}

/// `run` with the sprite limit optionally removed. Also returns the tick at which `$2002`
/// bit 6 (sprite 0 hit) and bit 5 (overflow) were first seen set.
fn run_opt(
    sprites: &[[u8; 4]],
    ctrl: u8,
    mask: u8,
    bg_tile: bool,
    unlimited: bool,
) -> (Ppu, Option<usize>, Option<usize>) {
    let mut cart = load(&chr_ram_rom()).unwrap();
    let mut p = Ppu::new();
    p.sprite_limit_removed = unlimited;
    w(&mut p, cart.as_mut(), 0x2006, 0x00);
    w(&mut p, cart.as_mut(), 0x2006, 0x10);
    w(&mut p, cart.as_mut(), 0x2007, 0x80);
    w(&mut p, cart.as_mut(), 0x2006, 0x00);
    w(&mut p, cart.as_mut(), 0x2006, 0x20);
    for _ in 0..8 {
        w(&mut p, cart.as_mut(), 0x2007, 0xFF);
    }
    if bg_tile {
        w(&mut p, cart.as_mut(), 0x2006, 0x20);
        w(&mut p, cart.as_mut(), 0x2006, 0x22);
        w(&mut p, cart.as_mut(), 0x2007, 0x02);
    }
    w(&mut p, cart.as_mut(), 0x2006, 0x3F);
    w(&mut p, cart.as_mut(), 0x2006, 0x00);
    w(&mut p, cart.as_mut(), 0x2007, 0x0F);
    w(&mut p, cart.as_mut(), 0x2007, 0x16);
    w(&mut p, cart.as_mut(), 0x2006, 0x3F);
    w(&mut p, cart.as_mut(), 0x2006, 0x11);
    w(&mut p, cart.as_mut(), 0x2007, 0x21);
    w(&mut p, cart.as_mut(), 0x2003, 0x00);
    for s in sprites {
        for &b in s {
            w(&mut p, cart.as_mut(), 0x2004, b);
        }
    }
    for _ in sprites.len()..64 {
        for b in [0xFF, 0, 0, 0] {
            w(&mut p, cart.as_mut(), 0x2004, b);
        }
    }
    w(&mut p, cart.as_mut(), 0x2000, ctrl);
    w(&mut p, cart.as_mut(), 0x2005, 0);
    w(&mut p, cart.as_mut(), 0x2005, 0);
    w(&mut p, cart.as_mut(), 0x2001, mask);
    let (mut hit, mut overflow) = (None, None);
    for t in 0..(262 * 341 + 241 * 341) {
        p.tick(cart.as_mut());
        if hit.is_none() && p.status & 0x40 != 0 {
            hit = Some(t);
        }
        if overflow.is_none() && p.status & 0x20 != 0 {
            overflow = Some(t);
        }
    }
    (p, hit, overflow)
}

fn px(p: &Ppu, x: usize, y: usize) -> u8 {
    p.framebuffer[y * 256 + x]
}

/// The 2005 blargg sprite-hit tests store their result code at $00F8 (the number of the test running, then
/// the final code); 1 means every test passed. The timing tests need about 55 frames, so run 120.
fn sprite_hit_result(name: &str) -> u8 {
    let mut nes = Nes::new(&rom_file(name)).unwrap();
    for _ in 0..120 {
        nes.run_frame();
    }
    nes.bus.ram[0xF8]
}

#[test]
fn sprite_drawn_one_line_below_y() {
    let p = run(&[[9, 1, 0, 20]], 0, 0x1E, false);
    assert_eq!(px(&p, 20, 10), 0x21);
    assert_eq!(px(&p, 19, 10), 0x0F);
    assert_eq!(px(&p, 21, 10), 0x0F);
    assert_eq!(px(&p, 20, 9), 0x0F);
    assert_eq!(px(&p, 20, 11), 0x0F);
}

#[test]
fn horizontal_flip() {
    let p = run(&[[9, 1, 0x40, 20]], 0, 0x1E, false);
    assert_eq!(px(&p, 20, 10), 0x0F);
    assert_eq!(px(&p, 27, 10), 0x21);
}

#[test]
fn vertical_flip() {
    let p = run(&[[9, 1, 0x80, 20]], 0, 0x1E, false);
    assert_eq!(px(&p, 20, 10), 0x0F);
    assert_eq!(px(&p, 20, 17), 0x21);
}

#[test]
fn sprite_zero_hit_over_background() {
    let p = run(&[[9, 1, 0, 16]], 0, 0x1E, true);
    assert_eq!(p.status & 0x40, 0x40);
    assert_eq!(px(&p, 16, 10), 0x21);
}

#[test]
fn behind_background_priority() {
    let p = run(&[[9, 1, 0x20, 16]], 0, 0x1E, true);
    assert_eq!(px(&p, 16, 10), 0x16);
    assert_eq!(p.status & 0x40, 0x40);
}

#[test]
fn no_hit_without_background() {
    let p = run(&[[9, 1, 0, 20]], 0, 0x1E, false);
    assert_eq!(p.status & 0x40, 0);
}

#[test]
fn nine_sprites_set_overflow() {
    let p = run(&[[30, 1, 0, 100]; 9], 0, 0x1E, false);
    assert_eq!(p.status & 0x20, 0x20);
}

#[test]
fn eight_sprites_no_overflow() {
    let p = run(&[[30, 1, 0, 100]; 8], 0, 0x1E, false);
    assert_eq!(p.status & 0x20, 0);
}

#[test]
fn sprites_disabled() {
    let p = run(&[[9, 1, 0, 20]], 0, 0x0E, false);
    assert_eq!(px(&p, 20, 10), 0x0F);
}

#[test]
fn left_clip_hides_sprite() {
    let p = run(&[[9, 1, 0, 4]], 0, 0x1A, false);
    assert_eq!(px(&p, 4, 10), 0x0F);
}

#[test]
fn double_height_uses_tile_pair() {
    let p = run(&[[9, 0, 0, 20]], 0x20, 0x1E, false);
    assert_eq!(px(&p, 20, 10), 0x0F);
    assert_eq!(px(&p, 20, 18), 0x21);
}

#[test]
fn sprite_hit_01_basics() {
    assert_eq!(sprite_hit_result("01.basics.nes"), 1);
}

#[test]
fn sprite_hit_02_alignment() {
    assert_eq!(sprite_hit_result("02.alignment.nes"), 1);
}

#[test]
fn sprite_hit_03_corners() {
    assert_eq!(sprite_hit_result("03.corners.nes"), 1);
}

#[test]
fn sprite_hit_04_flip() {
    assert_eq!(sprite_hit_result("04.flip.nes"), 1);
}

#[test]
fn sprite_hit_05_left_clip() {
    assert_eq!(sprite_hit_result("05.left_clip.nes"), 1);
}

#[test]
fn sprite_hit_06_right_edge() {
    assert_eq!(sprite_hit_result("06.right_edge.nes"), 1);
}

#[test]
fn sprite_hit_07_screen_bottom() {
    assert_eq!(sprite_hit_result("07.screen_bottom.nes"), 1);
}

#[test]
fn sprite_hit_08_double_height() {
    assert_eq!(sprite_hit_result("08.double_height.nes"), 1);
}

#[test]
fn sprite_hit_09_timing_basics() {
    assert_eq!(sprite_hit_result("09.timing_basics.nes"), 1);
}

#[test]
fn sprite_hit_10_timing_order() {
    assert_eq!(sprite_hit_result("10.timing_order.nes"), 1);
}

#[test]
fn sprite_hit_11_edge_timing() {
    assert_eq!(sprite_hit_result("11.edge_timing.nes"), 1);
}

// ---- Layer hiding: display only ----

/// `run` with a layer mask set before rendering starts.
fn run_masked(layers: tendy_nes::ppu::LayerMask) -> Ppu {
    let mut cart = load(&chr_ram_rom()).unwrap();
    let mut p = Ppu::new();
    p.layers = layers;
    for (r, v) in [
        (0x2006, 0x00),
        (0x2006, 0x10),
        (0x2007, 0x80),
        (0x2006, 0x00),
        (0x2006, 0x20),
    ] {
        w(&mut p, cart.as_mut(), r, v);
    }
    for _ in 0..8 {
        w(&mut p, cart.as_mut(), 0x2007, 0xFF);
    }
    for (r, v) in [
        (0x2006, 0x20),
        (0x2006, 0x22),
        (0x2007, 0x02),
        (0x2006, 0x3F),
        (0x2006, 0x00),
        (0x2007, 0x0F),
        (0x2007, 0x16),
        (0x2006, 0x3F),
        (0x2006, 0x11),
        (0x2007, 0x21),
        (0x2003, 0x00),
    ] {
        w(&mut p, cart.as_mut(), r, v);
    }
    for b in [9, 1, 0, 16] {
        w(&mut p, cart.as_mut(), 0x2004, b);
    }
    for _ in 1..64 {
        for b in [0xFF, 0, 0, 0] {
            w(&mut p, cart.as_mut(), 0x2004, b);
        }
    }
    for (r, v) in [(0x2000, 0), (0x2005, 0), (0x2005, 0), (0x2001, 0x1E)] {
        w(&mut p, cart.as_mut(), r, v);
    }
    for _ in 0..(262 * 341 + 241 * 341) {
        p.tick(cart.as_mut());
    }
    p
}

#[test]
fn hide_bg_drops_only_background_pixels() {
    let shown = run_masked(Default::default());
    let hid = run_masked(tendy_nes::ppu::LayerMask {
        hide_bg: true,
        hide_sprites: false,
    });
    // Visible: background 0x16 in its block, the sprite pixel 0x21 over it.
    assert_eq!(px(&shown, 18, 10), 0x16);
    assert_eq!(px(&shown, 16, 10), 0x21);
    // Hidden: no background colour anywhere, the sprite pixel is unchanged, hit still set.
    assert!(hid.framebuffer.iter().all(|&c| c != 0x16));
    assert_eq!(px(&hid, 18, 10), 0x0F);
    assert_eq!(px(&hid, 16, 10), 0x21);
    assert_eq!(hid.status & 0x40, 0x40);
    assert_eq!(hid.status, shown.status);
    assert_eq!(hid.frame, shown.frame);
}

#[test]
fn hide_sprites_drops_only_sprite_pixels() {
    let hid = run_masked(tendy_nes::ppu::LayerMask {
        hide_bg: false,
        hide_sprites: true,
    });
    assert!(hid.framebuffer.iter().all(|&c| c != 0x21));
    assert_eq!(px(&hid, 18, 10), 0x16);
    assert_eq!(px(&hid, 16, 10), 0x16);
    assert_eq!(hid.status & 0x40, 0x40);
}

#[test]
fn default_mask_is_bit_identical_to_unmasked() {
    let a = run_masked(Default::default());
    let b = run(&[[9, 1, 0, 16]], 0, 0x1E, true);
    assert_eq!(a.framebuffer, b.framebuffer);
    assert_eq!(a.emphasis, b.emphasis);
    assert_eq!(a.status, b.status);
}

// ---- Sprite-limit removal: display only ----

/// Sprite 0 overlaps the background block; sprites 1..=11 sit on the same line at x = 40 + 16 * i.
/// Tile 1 has its one pixel at the left of the top row, which is line 10 for a sprite with Y = 9.
fn twelve_on_a_line() -> Vec<[u8; 4]> {
    let mut v = vec![[9, 1, 0, 16]];
    for i in 0..11u8 {
        v.push([9, 1, 0, 40 + 16 * i]);
    }
    v
}

#[test]
fn limit_on_draws_eight_of_twelve() {
    let (p, _, _) = run_opt(&twelve_on_a_line(), 0, 0x1E, true, false);
    let drawn = (0..12)
        .filter(|&i| px(&p, if i == 0 { 16 } else { 40 + 16 * (i - 1) }, 10) == 0x21)
        .count();
    assert_eq!(drawn, 8);
}

#[test]
fn limit_removed_draws_all_twelve_and_leaves_flags_and_hit_dot_alone() {
    let sprites = twelve_on_a_line();
    let (off, off_hit, off_ovf) = run_opt(&sprites, 0, 0x1E, true, false);
    let (on, on_hit, on_ovf) = run_opt(&sprites, 0, 0x1E, true, true);
    for i in 0..12 {
        let x = if i == 0 { 16 } else { 40 + 16 * (i - 1) };
        assert_eq!(
            px(&on, x, 10),
            0x21,
            "sprite {i} shows with the limit removed"
        );
    }
    assert!(off_hit.is_some() && off_ovf.is_some());
    assert_eq!(on_hit, off_hit, "sprite 0 hit lands on the same dot");
    assert_eq!(on_ovf, off_ovf, "overflow is set on the same dot");
    assert_eq!(on.status, off.status);
    assert_eq!(on.frame, off.frame);
    // Everything outside the extra sprites' pixels is identical.
    let diff = (0..256 * 240)
        .filter(|&i| on.framebuffer[i] != off.framebuffer[i])
        .count();
    assert_eq!(diff, 4, "only the four sprites beyond the eighth appear");
}

#[test]
fn limit_removed_is_identical_without_more_than_eight_sprites() {
    let sprites = &twelve_on_a_line()[..8];
    let a = run_opt(sprites, 0, 0x1E, true, false);
    let b = run_opt(sprites, 0, 0x1E, true, true);
    assert_eq!(a.0.framebuffer, b.0.framebuffer);
    assert_eq!((a.1, a.2), (b.1, b.2));
}

#[test]
fn extra_sprites_respect_flip_and_the_left_clip() {
    // 8 sprites at x = 100.. then a flipped one (tile 1 row 0 mirrored: pixel at its right edge).
    let mut v: Vec<[u8; 4]> = (0..8).map(|i| [9, 1, 0, 100 + 10 * i]).collect();
    v.push([9, 1, 0x40, 200]);
    v.push([9, 1, 0, 4]);
    let (p, _, _) = run_opt(&v, 0, 0x1E, false, true);
    assert_eq!(px(&p, 207, 10), 0x21);
    assert_eq!(px(&p, 200, 10), 0x0F);
    // x = 4 is under the 8-pixel left clip when PPUMASK bit 2 is clear.
    let (q, _, _) = run_opt(&v, 0, 0x1A, false, true);
    assert_eq!(px(&q, 4, 10), 0x0F);
    let (r, _, _) = run_opt(&v, 0, 0x1E, false, true);
    assert_eq!(px(&r, 4, 10), 0x21);
}
