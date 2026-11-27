use tendy_nes::cart::Mapper;
use tendy_nes::mapper::load;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::Ppu;
use tendy_nes::ppu::palette::{NES_PALETTE, to_rgba_emph};
use tendy_nes::state::fnv1a64;

use crate::common::rom_file;

/// NROM image with 8 KB of CHR-RAM (CHR bank count 0).
fn chr_ram_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384, 0);
    v
}

fn write(ppu: &mut Ppu, cart: &mut dyn Mapper, reg: u16, val: u8) {
    ppu.write_register(cart, reg, val);
}

/// Tile 1 is solid colour 1 (low plane 0xFF, high plane 0x00); nametable cell (0, 0) holds tile 1;
/// backdrop colour 0x0F, palette 0 colour 1 is 0x16. Scroll X is `scroll_x`, PPUMASK is `mask`.
/// `v_at`, when set, is written to `$2006` (high, then low) after `$2001`, which is how a
/// backdrop-override test decides where `v` points when the frames run. Runs two full frames and
/// returns the PPU.
fn render(scroll_x: u8, mask: u8, v_at: Option<u16>) -> Ppu {
    let mut cart = load(&chr_ram_rom()).unwrap();
    let mut ppu = Ppu::new();
    write(&mut ppu, cart.as_mut(), 0x2006, 0x00);
    write(&mut ppu, cart.as_mut(), 0x2006, 0x10);
    for _ in 0..8 {
        write(&mut ppu, cart.as_mut(), 0x2007, 0xFF);
    }
    write(&mut ppu, cart.as_mut(), 0x2006, 0x20);
    write(&mut ppu, cart.as_mut(), 0x2006, 0x00);
    write(&mut ppu, cart.as_mut(), 0x2007, 0x01);
    write(&mut ppu, cart.as_mut(), 0x2006, 0x3F);
    write(&mut ppu, cart.as_mut(), 0x2006, 0x00);
    write(&mut ppu, cart.as_mut(), 0x2007, 0x0F);
    write(&mut ppu, cart.as_mut(), 0x2007, 0x16);
    write(&mut ppu, cart.as_mut(), 0x2000, 0x00);
    write(&mut ppu, cart.as_mut(), 0x2005, scroll_x);
    write(&mut ppu, cart.as_mut(), 0x2005, 0x00);
    write(&mut ppu, cart.as_mut(), 0x2001, mask);
    if let Some(v) = v_at {
        write(&mut ppu, cart.as_mut(), 0x2006, (v >> 8) as u8);
        write(&mut ppu, cart.as_mut(), 0x2006, v as u8);
    }
    for _ in 0..(2 * 262 * 341) {
        ppu.tick(cart.as_mut());
    }
    ppu
}

#[test]
fn tile_is_drawn_at_top_left() {
    let p = render(0, 0x0A, None);
    let fb = &p.framebuffer;
    assert_eq!(&fb[0..8], &[0x16; 8][..]);
    assert_eq!(fb[8], 0x0F);
    assert_eq!(fb[256 * 7 + 7], 0x16);
    assert_eq!(fb[256 * 8], 0x0F);
}

#[test]
fn fine_x_scroll_shifts_the_tile() {
    let p = render(4, 0x0A, None);
    let fb = &p.framebuffer;
    assert_eq!(&fb[0..4], &[0x16; 4][..]);
    assert_eq!(&fb[4..10], &[0x0F; 6][..]);
}

#[test]
fn left_column_clip_hides_background() {
    let p = render(0, 0x08, None);
    let fb = &p.framebuffer;
    assert_eq!(&fb[0..8], &[0x0F; 8][..]);
}

#[test]
fn grayscale_masks_colour() {
    let p = render(0, 0x0B, None);
    let fb = &p.framebuffer;
    assert_eq!(fb[0], 0x10);
}

/// Backdrop override: the palette writes above end with `v` = `$3F02`, so during forced
/// blank the screen shows palette entry 2 (`$00`, never written), not the backdrop `$0F`. Pinned
/// because the previous behaviour - always entry 0 - is what `full_nes_palette` renders as four
/// colours instead of the full palette.
#[test]
fn rendering_off_shows_the_entry_v_points_at() {
    let p = render(0, 0x00, None);
    assert_eq!(p.framebuffer[0], 0x00);
}

/// The override is `v`-driven, not "show the palette": with `v` outside palette RAM the backdrop is
/// entry 0 again.
#[test]
fn rendering_off_with_v_outside_the_palette_shows_entry_zero() {
    let p = render(0, 0x00, Some(0x2000));
    assert_eq!(p.framebuffer[0], 0x0F);
}

/// With either layer enabled the rendering pipeline owns the palette address, so the override is
/// not consulted: `v` still ends at `$3F02` from the palette writes, and the border stays `$0F`.
#[test]
fn rendering_on_ignores_the_override() {
    let p = render(0, 0x0A, None);
    assert_eq!(p.framebuffer[0], 0x16, "the tile itself still draws");
    assert_eq!(p.framebuffer[256 * 100], 0x0F, "the border keeps entry 0");
}

#[test]
fn palette_table_values() {
    assert_eq!(NES_PALETTE.len(), 64);
    assert_eq!(NES_PALETTE[0x0F], (0, 0, 0));
    assert_eq!(NES_PALETTE[0x30], (236, 238, 236));
    assert_eq!(NES_PALETTE[0x16], (152, 34, 32));
}

#[test]
fn to_rgba_emph_converts() {
    let mut out = [0u8; 8];
    to_rgba_emph(&[0x30, 0x0F], &[0, 0], &mut out);
    assert_eq!(out, [236, 238, 236, 255, 0, 0, 0, 255]);
}

#[test]
fn nestest_menu_frame() {
    let mut nes = Nes::new(&rom_file("nestest.nes")).unwrap();
    for _ in 0..60 {
        nes.run_frame();
    }
    // Reviewed snapshot of the nestest menu screen ("Run all tests", "Branch tests", ...).
    assert_eq!(fnv1a64(&nes.bus.ppu.framebuffer), 0x1783_cf08_98b5_8e81);
}
