use crate::cart::Mirroring;

/// Index into the 4 KB of nametable VRAM for a PPU address in 0x2000..=0x3EFF.
/// The four logical 1 KB tables map onto those physical blocks according to `mirroring`;
/// four-screen boards address all four, the other modes only the first 2 KB.
pub fn nametable_index(addr: u16, mirroring: Mirroring) -> usize {
    let a = (addr.wrapping_sub(0x2000) & 0x0FFF) as usize;
    let table = a / 0x400;
    let offset = a % 0x400;
    let physical = match mirroring {
        Mirroring::Horizontal => table / 2,
        Mirroring::Vertical => table & 1,
        Mirroring::SingleScreenLower => 0,
        Mirroring::SingleScreenUpper => 1,
        Mirroring::FourScreen => table,
    };
    physical * 0x400 + offset
}

/// The BG palette nametable tile `(col, row)` draws with: attribute bits 0-1 of `attr` pick one of
/// the four palettes, and the quadrant of the 4x4-tile attribute cell picks the bit pair — low pair
/// top-left, then right, then bottom-left, then bottom-right, per hardware. The renderer and the
/// viewer both call this, so the two cannot pick different bits.
#[inline]
pub fn attribute_palette(attr: u8, col: usize, row: usize) -> u8 {
    let shift = ((row % 4) / 2) * 4 + ((col % 4) / 2) * 2;
    (attr >> shift) & 3
}

/// Index into the 32-byte palette RAM for a PPU address in 0x3F00..=0x3FFF.
/// Entries 0x10, 0x14, 0x18 and 0x1C mirror 0x00, 0x04, 0x08 and 0x0C.
pub fn palette_index(addr: u16) -> usize {
    let i = (addr & 0x1F) as usize;
    if i >= 0x10 && i & 3 == 0 { i - 0x10 } else { i }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_mirroring() {
        assert_eq!(nametable_index(0x2000, Mirroring::Horizontal), 0);
        assert_eq!(nametable_index(0x2400, Mirroring::Horizontal), 0);
        assert_eq!(nametable_index(0x2800, Mirroring::Horizontal), 0x400);
        assert_eq!(nametable_index(0x2C05, Mirroring::Horizontal), 0x405);
    }

    #[test]
    fn vertical_mirroring() {
        assert_eq!(nametable_index(0x2000, Mirroring::Vertical), 0);
        assert_eq!(nametable_index(0x2400, Mirroring::Vertical), 0x400);
        assert_eq!(nametable_index(0x2800, Mirroring::Vertical), 0);
        assert_eq!(nametable_index(0x2C10, Mirroring::Vertical), 0x410);
    }

    #[test]
    fn single_screen() {
        assert_eq!(nametable_index(0x2C10, Mirroring::SingleScreenLower), 0x10);
        assert_eq!(nametable_index(0x2010, Mirroring::SingleScreenUpper), 0x410);
    }

    #[test]
    fn four_screen_uses_four_tables() {
        assert_eq!(nametable_index(0x2000, Mirroring::FourScreen), 0);
        assert_eq!(nametable_index(0x2400, Mirroring::FourScreen), 0x400);
        assert_eq!(nametable_index(0x2800, Mirroring::FourScreen), 0x800);
        assert_eq!(nametable_index(0x2C00, Mirroring::FourScreen), 0xC00);
    }

    #[test]
    fn mirror_region_3000() {
        assert_eq!(nametable_index(0x3005, Mirroring::Horizontal), 5);
    }

    #[test]
    fn palette_mirrors() {
        assert_eq!(palette_index(0x3F00), 0);
        assert_eq!(palette_index(0x3F10), 0);
        assert_eq!(palette_index(0x3F14), 4);
        assert_eq!(palette_index(0x3F1C), 0x0C);
        assert_eq!(palette_index(0x3F11), 0x11);
        assert_eq!(palette_index(0x3F20), 0);
        assert_eq!(palette_index(0x3FFF), 0x1F);
    }

    #[test]
    fn attribute_bits_select_the_four_quadrants() {
        // $E4 = 11 10 01 00, high pair bottom-right, low pair top-left.
        let attr = 0xE4u8;
        assert_eq!(attribute_palette(attr, 0, 0), 0);
        assert_eq!(attribute_palette(attr, 1, 1), 0);
        assert_eq!(attribute_palette(attr, 2, 0), 1);
        assert_eq!(attribute_palette(attr, 3, 3), 3);
        assert_eq!(attribute_palette(attr, 0, 2), 2);
        assert_eq!(attribute_palette(attr, 1, 3), 2);
        assert_eq!(attribute_palette(attr, 2, 2), 3);
        // The last tile of a table is in the top-right quadrant of its own cell (31 % 4, 29 % 4).
        assert_eq!(attribute_palette(attr, 31, 29), 1);
        // The quadrant repeats every four tiles in both directions.
        assert_eq!(attribute_palette(attr, 6, 6), attribute_palette(attr, 2, 2));
    }

    /// The renderer used to pick the pair from `v` itself — shift by 4 when `v & 0x40` (coarse Y
    /// bit 1), by 2 when `v & 0x02` (coarse X bit 1) — before it called the shared helper.
    /// Every 15-bit `v` (fine Y and the nametable bits included) and every attribute byte still
    /// selects the same two bits.
    #[test]
    fn attribute_palette_is_the_renderers_v_bit_rule() {
        for v in 0..0x8000u16 {
            for attr in 0..=255u8 {
                let mut at = attr;
                if v & 0x0040 != 0 {
                    at >>= 4;
                }
                if v & 0x0002 != 0 {
                    at >>= 2;
                }
                let helper =
                    attribute_palette(attr, usize::from(v & 0x1F), usize::from((v >> 5) & 0x1F));
                assert_eq!(helper, at & 0x03, "v ${v:04X} attr ${attr:02X}");
            }
        }
    }
}
