use tendy_nes::cart::Mapper;
use tendy_nes::mapper::load;
use tendy_nes::ppu::Ppu;

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
/// Runs two full frames and returns the PPU.
fn render(scroll_x: u8, mask: u8) -> Ppu {
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
    for _ in 0..(2 * 262 * 341) {
        ppu.tick(cart.as_mut());
    }
    ppu
}

// PPUMASK has no emphasis bits (bits 5-7 clear): every pixel's emphasis is 0.
#[test]
fn emphasis_zero_when_no_emphasis_bits() {
    let p = render(0, 0x0A);
    assert!(p.emphasis.iter().all(|&e| e == 0));
}

// All three emphasis bits set (0xE0 = bits 5,6,7): every pixel's emphasis is 7.
#[test]
fn emphasis_all_set_when_emphasis_bits_set() {
    let p = render(0, 0xEA);
    assert!(p.emphasis.iter().all(|&e| e == 7));
}

// Only the red emphasis bit (0x20 = bit 5) set: emphasis == PPUMASK >> 5 == 1.
#[test]
fn emphasis_red_bit_only() {
    let p = render(0, 0x2A);
    assert_eq!(p.emphasis[0], 1);
}

// Emphasis is recorded per pixel but never changes the palette indices: the framebuffer is
// identical with emphasis on vs off (and greyscale bit 0 stays clear in both).
#[test]
fn emphasis_does_not_change_indices() {
    let with = render(0, 0xEA);
    let without = render(0, 0x0A);
    assert_eq!(with.framebuffer, without.framebuffer);
}
