//! PPUMASK emphasis reaches the picture as an index into one NTSC-order 512-entry table (bit 5
//! red, bit 6 green, bit 7 blue). The 2C07 (PAL) and the Dendy clone swap the red and green
//! emphasis bits, so bit 5 ($20) emphasizes green there and bit 6 ($40) red: NESdev PPU registers,
//! `$2001`: "Emphasize red (green on PAL/Dendy)" / "Emphasize green (red on PAL/Dendy)". Blue
//! ($80) is never swapped. The swap belongs where the per-pixel plane is written, so every
//! consumer of the plane inherits it.

use tendy_nes::cart::{Mapper, Region};
use tendy_nes::mapper::load;
use tendy_nes::ppu::Ppu;
use tendy_nes::ppu::palette::palette_512;

/// NROM image with 8 KB of CHR-RAM (CHR bank count 0).
fn chr_ram_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384, 0);
    v
}

/// Tile 1 is solid colour 1, nametable cell (0, 0) holds it, palette 0 colour 1 is $16 (the
/// backdrop is $0F). Two frames of `mask` in `region`, then the PPU.
fn render(region: Region, mask: u8) -> Ppu {
    let mut cart = load(&chr_ram_rom()).unwrap();
    let mut ppu = Ppu::new();
    ppu.set_region(region);
    let write = |ppu: &mut Ppu, cart: &mut dyn Mapper, reg: u16, val: u8| {
        ppu.write_register(cart, reg, val)
    };
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
    write(&mut ppu, cart.as_mut(), 0x2001, mask);
    for _ in 0..(2 * usize::from(ppu.lines_per_frame) * 341) {
        ppu.tick(cart.as_mut());
    }
    ppu
}

/// NTSC is the reference order and must not move: bit 5 red, bit 6 green, bit 7 blue.
#[test]
fn ntsc_emphasis_plane_is_unchanged() {
    for (mask, plane) in [(0x2A, 1), (0x4A, 2), (0x8A, 4), (0xEA, 7), (0x0A, 0)] {
        let p = render(Region::Ntsc, mask);
        assert!(
            p.emphasis.iter().all(|&e| e == plane),
            "mask {mask:#04X} should give plane {plane}, saw {:?}",
            p.emphasis.iter().collect::<std::collections::BTreeSet<_>>()
        );
    }
}

/// PAL and Dendy swap red and green: $20 becomes green emphasis (plane 2) and $40 red (plane 1).
#[test]
fn pal_and_dendy_swap_the_red_and_green_emphasis_bits() {
    for region in [Region::Pal, Region::Dendy] {
        assert_eq!(
            render(region, 0x2A).emphasis[0],
            2,
            "{region:?}: $20 is green emphasis"
        );
        assert_eq!(
            render(region, 0x4A).emphasis[0],
            1,
            "{region:?}: $40 is red emphasis"
        );
        assert_eq!(
            render(region, 0x8A).emphasis[0],
            4,
            "{region:?}: $80 stays blue emphasis"
        );
        assert_eq!(
            render(region, 0xEA).emphasis[0],
            7,
            "{region:?}: all three bits still 7"
        );
        assert_eq!(
            render(region, 0x0A).emphasis[0],
            0,
            "{region:?}: no bits, no emphasis"
        );
    }
}

/// The plane is the 512-entry table's index, so the swapped value must paint the documented
/// colour: one set bit keeps its own channel at full level and dims the other two.
#[test]
fn the_swapped_plane_paints_the_documented_colour() {
    let table = palette_512();
    let white = 0x30; // the palette's white under this test's palette RAM
    let base = table[white];
    // NTSC $20: red emphasis, so R is untouched and G and B are dimmed.
    let (r, g, b) = table[(render(Region::Ntsc, 0x2A).emphasis[0] as usize) << 6 | white];
    assert!(
        r == base.0 && g < base.1 && b < base.2,
        "NTSC $20 paints red: ({r}, {g}, {b}) out of {base:?}"
    );
    // PAL $20: green emphasis, so G is untouched and R and B are dimmed.
    let (r, g, b) = table[(render(Region::Pal, 0x2A).emphasis[0] as usize) << 6 | white];
    assert!(
        g == base.1 && r < base.0 && b < base.2,
        "PAL $20 paints green: ({r}, {g}, {b}) out of {base:?}"
    );
}
