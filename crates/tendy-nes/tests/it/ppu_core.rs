use tendy_nes::cpu::Bus;
use tendy_nes::mapper::load;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::{POWER_UP_PALETTE, Ppu};

use crate::common::loop_rom;
use crate::common::rom_file;

/// The 2005 blargg PPU tests store their result code at $00F0; 1 means every test passed.
fn blargg_ppu_result(name: &str) -> u8 {
    let mut nes = Nes::new(&rom_file(name)).unwrap();
    for _ in 0..30 {
        nes.run_frame();
    }
    nes.bus.ram[0xF0]
}

#[test]
fn ppudata_reads_are_buffered() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2006, 0x20);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);
    ppu.write_register(cart.as_mut(), 0x2007, 0x11);
    ppu.write_register(cart.as_mut(), 0x2007, 0x22);
    ppu.write_register(cart.as_mut(), 0x2006, 0x20);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);

    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0x00);
    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0x11);
    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0x22);
}

#[test]
fn palette_reads_are_not_buffered() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);
    ppu.write_register(cart.as_mut(), 0x2007, 0x15);
    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);

    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0x15);
}

/// The `$2007` palette read is one shared block in both `read_register` branches: 6 bits
/// from palette RAM, greyscale masking from `$2001` bit 0, PPU open bus on the top two bits.
#[test]
fn palette_read_while_rendering_masks_greyscale_and_adds_open_bus() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);
    ppu.write_register(cart.as_mut(), 0x2007, 0x2A);
    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);

    // Rendering on, on a visible line: the deferred path.
    ppu.mask = 0x19; // greyscale + background
    ppu.scanline = 100;
    ppu.dot = 10;
    ppu.io_latch = 0xA5;
    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0xA0);
    assert_eq!(ppu.v, 0x3F00, "a deferred read does not step v");

    // Rendering on but in vblank, so outside the window: the same block, after the read buffer
    // is filled from the nametable. Greyscale is still on.
    ppu.scanline = 241;
    ppu.io_latch = 0xA5;
    assert_eq!(ppu.read_register(cart.as_mut(), 0x2007), 0xA0);
    assert_eq!(ppu.v, 0x3F01, "an immediate read steps v");
}

#[test]
fn palette_10_mirrors_00() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x10);
    ppu.write_register(cart.as_mut(), 0x2007, 0x2A);

    assert_eq!(ppu.palette[0], 0x2A);
}

#[test]
fn increment_32_mode() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2000, 0x04);
    ppu.write_register(cart.as_mut(), 0x2006, 0x20);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);
    ppu.write_register(cart.as_mut(), 0x2007, 0x01);
    ppu.write_register(cart.as_mut(), 0x2007, 0x02);

    assert_eq!(ppu.vram[0], 1);
    assert_eq!(ppu.vram[32], 2);
}

#[test]
fn status_read_clears_vblank_and_toggle() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.status = 0x80;
    ppu.w = true;

    let s = ppu.read_register(cart.as_mut(), 0x2002);

    assert_eq!(s & 0x80, 0x80);
    assert_eq!(ppu.status & 0x80, 0);
    assert!(!ppu.w);
}

#[test]
fn scroll_writes_fill_t_and_fine_x() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2005, 0x7D);
    assert_eq!(ppu.t & 0x001F, 0x0F);
    assert_eq!(ppu.fine_x, 5);

    ppu.write_register(cart.as_mut(), 0x2005, 0x5E);
    assert_eq!(ppu.t, 0x616F);
}

#[test]
fn ctrl_sets_nametable_bits() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2000, 0x03);

    assert_eq!(ppu.t & 0x0C00, 0x0C00);
}

#[test]
fn ppuaddr_sets_v() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    ppu.write_register(cart.as_mut(), 0x2006, 0x10);
    assert_eq!(ppu.v, 0x3F10);

    ppu.write_register(cart.as_mut(), 0x2006, 0xFF);
    ppu.write_register(cart.as_mut(), 0x2006, 0x00);
    assert_eq!(ppu.v, 0x3F00);
}

#[test]
fn oam_write_increments_read_does_not() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    ppu.write_register(cart.as_mut(), 0x2004, 0xAB);

    assert_eq!(ppu.oam[0x10], 0xAB);
    assert_eq!(ppu.oam_addr, 0x11);

    ppu.write_register(cart.as_mut(), 0x2003, 0x10);

    assert_eq!(ppu.read_register(cart.as_mut(), 0x2004), 0xAB);
    assert_eq!(ppu.oam_addr, 0x10);
}

#[test]
fn vblank_starts_at_scanline_241() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    ppu.write_register(cart.as_mut(), 0x2000, 0x80);

    for _ in 0..(241 * 341 + 2) {
        ppu.tick(cart.as_mut());
    }

    assert_eq!(ppu.status & 0x80, 0x80);
    assert!(ppu.nmi_out);
    assert!(ppu.frame_ready);
}

#[test]
fn vblank_ends_at_prerender_line() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut ppu = Ppu::new();

    for _ in 0..(261 * 341 + 2) {
        ppu.tick(cart.as_mut());
    }

    assert_eq!(ppu.status & 0x80, 0);
}

#[test]
fn oam_dma_copies_a_page() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();

    for i in 0..256 {
        nes.bus.ram[0x200 + i] = i as u8;
    }

    nes.bus.write(0x4014, 0x02);
    let _ = nes.bus.read(0x0000); // DMA starts on the next CPU read

    assert_eq!(nes.bus.ppu.oam[0], 0);
    assert_eq!(nes.bus.ppu.oam[0x7F], 0x7F);
    assert_eq!(nes.bus.ppu.oam[0xFF], 0xFF);
    assert_eq!(nes.bus.dma_cycles, 513);
}

#[test]
fn run_frame_stops_at_vblank() {
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();

    nes.run_frame();

    assert_eq!(nes.bus.ppu.scanline, 241);
    assert!(nes.bus.ppu.frame_ready);
}

#[test]
fn blargg_palette_ram() {
    assert_eq!(blargg_ppu_result("palette_ram.nes"), 1);
}

#[test]
fn blargg_sprite_ram() {
    assert_eq!(blargg_ppu_result("sprite_ram.nes"), 1);
}

#[test]
fn blargg_vram_access() {
    assert_eq!(blargg_ppu_result("vram_access.nes"), 1);
}

/// A reset button press does not re-power the console: game-written palette entries survive and
/// the palette is no longer the power-up table (REVIEW-230).
#[test]
fn reset_keeps_game_written_palette_entries() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();
    let mut nes = Nes::new(&loop_rom(0, 0)).unwrap();
    assert_eq!(nes.bus.ppu.palette, POWER_UP_PALETTE);

    // A game writes $3F11/$3F12 through the real $2007 path.
    nes.bus.ppu.write_register(cart.as_mut(), 0x2006, 0x3F);
    nes.bus.ppu.write_register(cart.as_mut(), 0x2006, 0x11);
    nes.bus.ppu.write_register(cart.as_mut(), 0x2007, 0x11);
    nes.bus.ppu.write_register(cart.as_mut(), 0x2007, 0x22);
    assert_ne!(nes.bus.ppu.palette[0x11], POWER_UP_PALETTE[0x11]);
    assert_ne!(nes.bus.ppu.palette[0x12], POWER_UP_PALETTE[0x12]);

    nes.reset();

    // The written entries survive the reset and the palette is no longer the table.
    assert_eq!(nes.bus.ppu.palette[0x11], 0x11);
    assert_eq!(nes.bus.ppu.palette[0x12], 0x22);
    assert_ne!(nes.bus.ppu.palette, POWER_UP_PALETTE);
}

/// A `$2007` write while rendering steps `v` the way the renderer does — coarse X and fine Y
/// together — not by the linear 1/32 increment (NESdev PPU scrolling, `$2007`: "During rendering
/// (on the pre-render line and the visible lines 0-239, provided either background or sprite
/// rendering is enabled), it will update v in an odd way, triggering a coarse X increment and a Y
/// increment simultaneously (with normal wrapping behavior)"). The read path already does this at
/// its four-dot expiry; this pins the write path to the same rule.
#[test]
fn ppudata_writes_during_rendering_step_coarse_x_and_fine_y() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();

    // v = $2000 (fine Y 2, coarse X 0) plus one write: coarse X 0 -> 1, fine Y 2 -> 3.
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2001, 0x0A); // background on
    ppu.v = 0x2000;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(
        ppu.v, 0x3001,
        "rendering-time write: coarse X + fine Y, not +1"
    );
    assert_eq!(
        ppu.mem_read(cart.as_mut(), 0x2000),
        0x42,
        "the byte landed at the old address"
    );

    // The increment bit does not apply during rendering: still coarse X + fine Y.
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2000, 0x04); // increment by 32
    ppu.write_register(cart.as_mut(), 0x2001, 0x0A);
    ppu.v = 0x2000;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x3001, "the .INC32 bit is ignored while rendering");

    // Coarse X wraps to the next nametable, then fine Y rises.
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2001, 0x0A);
    ppu.v = 0x201F;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x3400);

    // Fine Y 7 rolls into coarse Y 0 -> 1, with coarse X's own step.
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2001, 0x0A);
    ppu.v = 0x7000; // fine Y 7, coarse X 0, coarse Y 0
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x0021);
}

/// Outside rendering every `$2007` transfer keeps the linear increment, which is what every normal
/// VRAM transfer (vblank, forced blank) relies on: +1, or +32 with PPUCTRL bit 2.
#[test]
fn ppudata_writes_outside_rendering_keep_the_linear_increment() {
    let mut cart = load(&loop_rom(0, 0)).unwrap();

    let mut ppu = Ppu::new();
    ppu.v = 0x2000;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x2001, "forced blank: +1");

    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2000, 0x04);
    ppu.v = 0x2000;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x2020, "forced blank with .INC32: +32");

    // Rendering enabled but past line 239 (vblank): the wiki's window is the pre-render line and
    // the visible lines only, so the linear increment stays.
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2001, 0x0A);
    ppu.scanline = 241;
    ppu.v = 0x2000;
    ppu.write_register(cart.as_mut(), 0x2007, 0x42);
    assert_eq!(ppu.v, 0x2001, "vblank with rendering on: linear");
}
