//! PPU viewer data layer: read-only snapshots of the PPU for a debug UI.
//!
//! Every function borrows `&Ppu` plus what the PPU does not own — the cartridge mapper, which
//! supplies the mirroring mode and the live CHR mapping — and returns owned structs. Nothing is
//! mutated and no side effects run: this is a borrow/return API only. Pattern tables are read
//! through `Mapper::chr_peek`, the side-effect-free door into CHR, so a snapshot shows exactly the
//! banks (and MMC2/MMC4 latch state) the PPU is drawing with and leaves them alone.
//!
//! Everything a snapshot reports is state the console already holds: every field is either a
//! `Ppu` field that `Ppu::save_state` serialises or a cartridge register. The one exception is
//! `Position::lines_per_frame`, which `Ppu::set_region` sets from the machine's region (frame
//! geometry is deliberately not serialised), so a snapshot round trips through save/load only
//! while the same region is in force. There is no rendering, heatmap or live-update path here.

use super::Ppu;
use super::memory::{nametable_index, palette_index};
use crate::cart::Mapper;

/// One decoded 8x8 tile: pixel `[y][x]` is the 2-bit palette index built from the tile's two
/// bitplane bytes of row `y` (bit 7 = the leftmost pixel, per hardware).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tile {
    pub pixels: [[u8; 8]; 8],
}

impl Tile {
    /// Decode 16 bytes of a tile: bytes 0-7 the low bitplane, bytes 8-15 the high one.
    pub fn from_planes(bytes: &[u8]) -> Tile {
        let mut t = Tile::default();
        for y in 0..8 {
            let lo = bytes[y];
            let hi = bytes[8 + y];
            for (x, px) in t.pixels[y].iter_mut().enumerate() {
                let bit = 7 - x;
                *px = ((lo >> bit) & 1) | (((hi >> bit) & 1) << 1);
            }
        }
        t
    }
}

/// The two pattern tables as the PPU sees them *right now*: CHR read through the mapper's live
/// banking, so CHR-RAM contents, swapped banks and an MMC2/MMC4 latch mid-frame all show up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternTables {
    /// `[0]` is PPU $0000-$0FFF, `[1]` is $1000-$1FFF; 256 tiles each, tile `i` of table `t`
    /// decoded from the 16 bytes at `t * 0x1000 + i * 16`.
    pub tables: [[Tile; 256]; 2],
}

impl PatternTables {
    /// Tile `index` (0..=255) of table `table` (0 = $0000, 1 = $1000), by reference: the tile is
    /// 64 bytes and the view walkers read it a pixel at a time, so copying it per tile is waste.
    pub fn tile(&self, table: usize, index: u8) -> &Tile {
        &self.tables[table & 1][usize::from(index)]
    }
}

/// Decode both live pattern tables through `cart`: every byte goes through `Mapper::chr_peek`,
/// which follows the board's current mapping without latching, clocking or otherwise perturbing it.
pub fn pattern_tables(cart: &dyn Mapper) -> PatternTables {
    let mut tables = [[Tile::default(); 256]; 2];
    for (table_index, table) in tables.iter_mut().enumerate() {
        for (tile_index, tile) in table.iter_mut().enumerate() {
            let base = (table_index * 0x1000 + tile_index * 16) as u16;
            let mut bytes = [0u8; 16];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = cart.chr_peek(base + i as u16);
            }
            *tile = Tile::from_planes(&bytes);
        }
    }
    PatternTables { tables }
}

/// The four logical nametables, each 32x30 tiles plus its attribute table, read through the board
/// (`cart.nt_peek`) so mappers that own their nametables (MMC5, Namco 163 banking) show what the
/// renderer sees; boards that answer `None` fall back to the PPU's CIRAM under `cart.mirroring()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nametables {
    /// Tile bytes, row-major, 32 columns per row × 30 rows. Index 0 = $2000, 1 = $2400,
    /// 2 = $2800, 3 = $2C00.
    pub tiles: [[u8; 960]; 4],
    /// The 64 attribute bytes of each logical table (PPU $3C0-$3FF of its 1 KB block), row-major:
    /// 8x8 bytes, each one covering a 32x32 pixel (4x4 tile) area of that nametable.
    pub attrs: [[u8; 64]; 4],
}

pub fn nametables(ppu: &Ppu, cart: &dyn Mapper) -> Nametables {
    let mirroring = cart.mirroring();
    let byte = |addr: u16| {
        cart.nt_peek(addr, &ppu.vram)
            .unwrap_or_else(|| ppu.vram[nametable_index(addr, mirroring)])
    };
    let mut out = Nametables {
        tiles: [[0; 960]; 4],
        attrs: [[0; 64]; 4],
    };
    for table in 0..4u16 {
        let base = 0x2000 + table * 0x400;
        for row in 0..30u16 {
            for col in 0..32u16 {
                let addr = base + row * 32 + col;
                out.tiles[table as usize][usize::from(row) * 32 + usize::from(col)] = byte(addr);
            }
        }
        for i in 0..64u16 {
            out.attrs[table as usize][i as usize] = byte(base + 0x3C0 + i);
        }
    }
    out
}

/// One OAM entry, decoded from the four bytes stored at `index * 4`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sprite {
    /// OAM entry number, 0..=63 (sprite 0 is the hit-test sprite).
    pub index: u8,
    /// Byte 0: Y position of the top edge, minus 1.
    pub y: u8,
    /// Byte 1: tile number from the pattern table.
    pub tile: u8,
    /// Byte 2, masked to the five attribute bits that exist (bits 2-4 read back as 0).
    pub attr: u8,
    /// Byte 3: X position of the left edge.
    pub x: u8,
}

impl Sprite {
    /// Sprite palette number (attr bits 0-1).
    pub fn palette(&self) -> u8 {
        self.attr & 3
    }
    /// Drawn behind the background (attr bit 5).
    pub fn behind_background(&self) -> bool {
        self.attr & 0x20 != 0
    }
    /// Horizontal flip (attr bit 6).
    pub fn flip_horizontal(&self) -> bool {
        self.attr & 0x40 != 0
    }
    /// Vertical flip (attr bit 7).
    pub fn flip_vertical(&self) -> bool {
        self.attr & 0x80 != 0
    }
    /// For 8x16 sprites, the pattern table the pair is fetched from. Bit 0 of the OAM tile byte
    /// selects $0000 (0) or $1000 (1); PPUCTRL bit 3 only applies to 8x8 sprites.
    pub fn pattern_table_16(&self) -> u8 {
        self.tile & 1
    }
    /// For 8x16 sprites, the two consecutive tiles drawn as `(top, bottom)`:
    /// `(tile & 0xFE, tile & 0xFE | 1)`. Bit 0 is the table select, not part of the tile number.
    pub fn tile_pair_16(&self) -> (u8, u8) {
        let top = self.tile & 0xFE;
        (top, top | 1)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OamDump {
    /// $2003 OAMADDR at the time of the snapshot.
    pub oam_addr: u8,
    /// All 64 entries in OAM order.
    pub sprites: [Sprite; 64],
}

pub fn oam(ppu: &Ppu) -> OamDump {
    let mut sprites = [Sprite {
        index: 0,
        y: 0,
        tile: 0,
        attr: 0,
        x: 0,
    }; 64];
    for (i, sprite) in sprites.iter_mut().enumerate() {
        let a = i as u8 * 4;
        *sprite = Sprite {
            index: i as u8,
            y: ppu.oam_read(a),
            tile: ppu.oam_read(a | 1),
            attr: ppu.oam_read(a | 2),
            x: ppu.oam_read(a | 3),
        };
    }
    OamDump {
        oam_addr: ppu.oam_addr,
        sprites,
    }
}

/// Palette RAM (32 bytes) plus the mapping a renderer would currently apply to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteDump {
    /// The 32 palette entries in the order the PPU sees them: entry `i` is what an access to
    /// $3F00 + `i` returns. Every index goes through `palette_index`, so the sprite mirrors
    /// $3F10/$3F14/$3F18/$3F1C show the background backdrop bytes ($3F00/$3F04/$3F08/$3F0C) those
    /// addresses really alias — not the stale zeros the backing array holds there.
    pub ram: [u8; 32],
    /// PPUMASK greyscale bit: rendered values lose their low two bits.
    pub greyscale: bool,
    /// PPUMASK colour emphasis bits as the picture applies them, red and green swapped on
    /// PAL/Dendy; RGB output applies `crate::ppu::palette::emphasized_rgb` on top of the mapped
    /// indices.
    pub emphasis: u8,
    /// Background palettes: `[p][0]` is the universal backdrop (all of $3F00/$3F04/... share
    /// it), `[p][1..=3]` are palette p's colours. Values are palette indices with greyscale
    /// applied, the way `render.rs` reads them.
    pub bg: [[u8; 4]; 4],
    /// Sprite palettes: `[p][0]` mirrors the universal backdrop (the $3F10 class of entries),
    /// `[p][1..=3]` are $3F11-3F13 class colours, greyscale applied the same way.
    pub sprites: [[u8; 4]; 4],
}

pub fn palette(ppu: &Ppu) -> PaletteDump {
    let grey = ppu.mask & 1 != 0;
    let apply = |c: u8| if grey { c & 0x30 } else { c };
    let mut ram = [0u8; 32];
    for (i, entry) in ram.iter_mut().enumerate() {
        *entry = ppu.palette[palette_index(0x3F00 + i as u16)];
    }
    let mut dump = PaletteDump {
        ram,
        greyscale: grey,
        emphasis: ppu.emphasis_bits(),
        bg: [[0; 4]; 4],
        sprites: [[0; 4]; 4],
    };
    for p in 0..4 {
        dump.bg[p][0] = apply(ppu.palette[palette_index(0x3F00)]);
        dump.sprites[p][0] = apply(ppu.palette[palette_index(0x3F10)]);
        for e in 1..4 {
            dump.bg[p][e] = apply(ppu.palette[p * 4 + e]);
            dump.sprites[p][e] = apply(ppu.palette[0x10 + p * 4 + e]);
        }
    }
    dump
}

/// Loopy scroll registers ($2005/$2006 state) and the $2000 control bits a viewer needs to
/// label banks and sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollDump {
    /// $2000 PPUCTRL value.
    pub ctrl: u8,
    /// Current VRAM address v (15 bits: the line/scroll position mid-frame and the $2006/$2007 pointer otherwise).
    pub v: u16,
    /// Temporary address t.
    pub t: u16,
    /// Fine X scroll, 0..=7 (written by the first $2005 write).
    pub fine_x_scroll: u8,
    /// $2005/$2006 write toggle: 0 = next write is the first of a pair.
    pub write_toggle: u8,
}

impl ScrollDump {
    /// Coarse X scroll (t bits 0-4).
    pub fn t_coarse_x(&self) -> u8 {
        (self.t & 0x1F) as u8
    }
    /// Coarse Y scroll (t bits 5-9).
    pub fn t_coarse_y(&self) -> u8 {
        ((self.t >> 5) & 0x1F) as u8
    }
    /// Base nametable of t, 0..=3 ($2000/$2400/$2800/$2C00).
    pub fn t_nametable(&self) -> u8 {
        ((self.t >> 10) & 3) as u8
    }
    /// Fine Y scroll (t bits 12-14).
    pub fn t_fine_y(&self) -> u8 {
        ((self.t >> 12) & 7) as u8
    }
    /// Coarse X scroll of v (bits 0-4).
    pub fn v_coarse_x(&self) -> u8 {
        (self.v & 0x1F) as u8
    }
    /// Coarse Y scroll of v (bits 5-9).
    pub fn v_coarse_y(&self) -> u8 {
        ((self.v >> 5) & 0x1F) as u8
    }
    /// Base nametable of v, 0..=3.
    pub fn v_nametable(&self) -> u8 {
        ((self.v >> 10) & 3) as u8
    }
    /// Fine Y scroll of v (bits 12-14).
    pub fn v_fine_y(&self) -> u8 {
        ((self.v >> 12) & 7) as u8
    }
    /// Background pattern table select (PPUCTRL bit 4).
    pub fn bg_pattern_table(&self) -> u8 {
        (self.ctrl >> 4) & 1
    }
    /// Sprite pattern table select for 8x8 sprites (PPUCTRL bit 3).
    pub fn sprite_pattern_table(&self) -> u8 {
        (self.ctrl >> 3) & 1
    }
    /// Sprite size: true = 8x16 sprites (PPUCTRL bit 5).
    pub fn sprite_size_16(&self) -> bool {
        self.ctrl & 0x20 != 0
    }
}

pub fn scroll(ppu: &Ppu) -> ScrollDump {
    ScrollDump {
        ctrl: ppu.ctrl,
        v: ppu.v & 0x7FFF,
        t: ppu.t & 0x7FFF,
        fine_x_scroll: ppu.fine_x & 7,
        write_toggle: u8::from(ppu.w),
    }
}

/// Where the PPU is in its frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub scanline: u16,
    pub dot: u16,
    pub frame: u64,
    /// Geometry for the viewer's line maths: 262 NTSC, 312 PAL/Dendy. This comes from
    /// `Ppu::set_region`, not from a save state — frame geometry is not serialised, so a viewer
    /// reading a restored machine must take the region from the machine, not from the snapshot.
    pub lines_per_frame: u16,
}

pub fn position(ppu: &Ppu) -> Position {
    Position {
        scanline: ppu.scanline,
        dot: ppu.dot,
        frame: ppu.frame,
        lines_per_frame: ppu.lines_per_frame,
    }
}

/// Everything a viewer needs in one struct, captured in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PpuSnapshot {
    pub nametables: Nametables,
    pub pattern_tables: PatternTables,
    pub oam: OamDump,
    pub palette: PaletteDump,
    pub scroll: ScrollDump,
    pub position: Position,
}

/// Capture everything at once. `cart` supplies the mirroring mode, the live CHR view and the
/// board's own nametables, and is only read through `&self` methods (`mirroring`, `chr_peek`,
/// `nt_peek`), so nothing on the board changes.
pub fn snapshot(ppu: &Ppu, cart: &dyn Mapper) -> PpuSnapshot {
    PpuSnapshot {
        nametables: nametables(ppu, cart),
        pattern_tables: pattern_tables(cart),
        oam: oam(ppu),
        palette: palette(ppu),
        scroll: scroll(ppu),
        position: position(ppu),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Mapper;
    use crate::cart::{Header, Mirroring, RomData, split};
    use crate::mapper::cnrom::Cnrom;
    use crate::mapper::mmc1::Mmc1;
    use crate::mapper::mmc2_mmc4::Mmc2Mmc4;
    use crate::mapper::mmc3::Mmc3;
    use crate::mapper::mmc5::Mmc5;
    use crate::mapper::nrom::Nrom;
    use crate::ppu::POWER_UP_PALETTE;
    use crate::state::{StateReader, StateWriter};

    struct Cart {
        cart: Box<dyn Mapper>,
    }

    fn nrom_with(chr: Vec<u8>, mirroring: Mirroring, chr_is_ram: bool) -> Cart {
        let rom = RomData {
            header: Header {
                prg_banks: 1,
                chr_banks: 1,
                mapper: 0,
                mirroring,
                ..Default::default()
            },
            prg: vec![0; 16384],
            chr,
            chr_is_ram,
            trainer: None,
        };
        Cart {
            cart: Box::new(Nrom::new(rom)),
        }
    }

    /// A minimal iNES image for `mapper` with `prg_banks` 16 KB and `chr_banks` 8 KB banks.
    fn rom_bytes(mapper: u16, prg_banks: u8, chr_banks: u8) -> Vec<u8> {
        let flags6 = ((mapper & 0x0F) as u8) << 4;
        let flags7 = (mapper & 0xF0) as u8;
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, flags6, flags7, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        v.resize(
            16 + prg_banks as usize * 16384 + chr_banks as usize * 8192,
            0,
        );
        v
    }

    /// CNROM board whose 8 KiB CHR pages are each filled with their own page number.
    fn cnrom_board() -> Cnrom {
        let mut rom = split(&rom_bytes(3, 2, 4)).unwrap();
        for (i, b) in rom.chr.iter_mut().enumerate() {
            *b = (i / 0x2000) as u8;
        }
        Cnrom::new(rom)
    }

    /// MMC2 board whose 4 KiB CHR pages are each filled with their own page number.
    fn mmc2_board() -> Mmc2Mmc4 {
        let mut rom = split(&rom_bytes(9, 4, 8)).unwrap();
        for (i, b) in rom.chr.iter_mut().enumerate() {
            *b = (i / 0x1000) as u8;
        }
        Mmc2Mmc4::new(rom, false)
    }

    /// MMC4 board whose 4 KiB CHR pages are each filled with their own page number.
    fn mmc4_board() -> Mmc2Mmc4 {
        let mut rom = split(&rom_bytes(10, 2, 8)).unwrap();
        for (i, b) in rom.chr.iter_mut().enumerate() {
            *b = (i / 0x1000) as u8;
        }
        Mmc2Mmc4::new(rom, true)
    }

    /// Write one byte to PPU address space the way the CPU does: $2006 twice, then $2007.
    fn ppu_write(ppu: &mut Ppu, cart: &mut dyn Mapper, addr: u16, val: u8) {
        ppu.write_register(cart, 0x2006, (addr >> 8) as u8);
        ppu.write_register(cart, 0x2006, addr as u8);
        ppu.write_register(cart, 0x2007, val);
    }

    /// Serialise a board's mutable state so a test can assert a snapshot left it byte-identical.
    fn board_state(cart: &dyn Mapper) -> Vec<u8> {
        let mut w = StateWriter::new();
        cart.save_state(&mut w);
        w.finish()
    }

    #[test]
    fn tile_decodes_both_bitplanes() {
        let bytes = [
            0b1010_1100,
            0,
            0,
            0,
            0,
            0,
            0,
            0, // low plane, row 0
            0b0100_1001,
            0,
            0,
            0,
            0,
            0,
            0,
            0, // high plane, row 0
        ];
        let t = Tile::from_planes(&bytes);
        assert_eq!(t.pixels[0], [1, 2, 1, 0, 3, 1, 0, 2]);
        assert_eq!(t.pixels[1], [0; 8]);
        assert_eq!(t.pixels[7], [0; 8]);
    }

    /// The live decode follows the mapper's banking, not the image on disk: CNROM's bank register
    /// swaps the whole 8 KiB of CHR under both pattern tables.
    #[test]
    fn pattern_tables_follow_the_mapper_bank() {
        let mut cart = cnrom_board();
        // Power-on bank 0 is filled with zeros, so every tile is blank.
        assert_eq!(pattern_tables(&cart).tables[0][0], Tile::default());
        cart.cpu_write(0x8000, 1);
        let pt = pattern_tables(&cart);
        // Bank 1 is filled with $01, so every tile decodes that byte's bitplanes.
        let page_1 = Tile::from_planes(&[1u8; 16]);
        assert_ne!(page_1, Tile::default());
        assert_eq!(pt.tables[0][0], page_1);
        assert_eq!(pt.tables[1][255], page_1);
        assert_eq!(*pt.tile(1, 255), pt.tables[1][255]);
    }

    /// `chr_peek` must agree with the read path the PPU itself uses. CNROM and MMC1 have no read
    /// side effects, so the full `mem_read` path is a valid cross-check there (MMC2/MMC3/MMC4
    /// deliberately differ: their `ppu_read` latches banks and clocks the IRQ counter).
    #[test]
    fn chr_peek_agrees_with_the_ppu_read_path() {
        let mut cnrom = cnrom_board();
        cnrom.cpu_write(0x8000, 2);
        let mut ppu = Ppu::new();
        for addr in (0..0x2000u16).step_by(0x1FF) {
            assert_eq!(
                cnrom.chr_peek(addr),
                ppu.mem_read(&mut cnrom, addr),
                "CNROM ${addr:04X}"
            );
        }

        let mut mmc1: Box<dyn Mapper> = Box::new(Mmc1::new(split(&rom_bytes(1, 2, 4)).unwrap()));
        // Five serial writes select CHR bank 2 for the 8 KiB CHR window (control bit 4 clear).
        for _ in 0..5 {
            mmc1.cpu_write(0xA000, 1);
        }
        for addr in (0..0x2000u16).step_by(0x1FF) {
            let peeked = mmc1.chr_peek(addr);
            assert_eq!(
                peeked,
                ppu.mem_read(mmc1.as_mut(), addr),
                "MMC1 ${addr:04X}"
            );
        }
    }

    #[test]
    fn horizontal_mirroring_pairs_rows() {
        // Marker per logical table: $2000+$t*0x400 gets 0x01 | (t << 4).
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Horizontal, false);
        let mut ppu = Ppu::new();
        for t in 0..4u16 {
            let addr = 0x2000 + t * 0x400;
            ppu_write(&mut ppu, cart.cart.as_mut(), addr, 0x01 | (t << 4) as u8);
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        // Horizontal: tables 0/1 share physical 0 (last write there is t=1 → $11), 2/3 share physical 1 ($31).
        assert_eq!(nt.tiles[0][0], 0x11);
        assert_eq!(nt.tiles[1][0], 0x11);
        assert_eq!(nt.tiles[2][0], 0x31);
        assert_eq!(nt.tiles[3][0], 0x31);
        // Rows 1..30 and the attribute region keep their own routed bytes.
        assert_eq!(nt.tiles[0][959], 0);
        assert_eq!(nt.attrs[0][0], 0);
    }

    #[test]
    fn vertical_mirroring_alternates_columns() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Vertical, false);
        let mut ppu = Ppu::new();
        for t in 0..4u16 {
            ppu_write(
                &mut ppu,
                cart.cart.as_mut(),
                0x2000 + t * 0x400,
                0x01 | (t << 4) as u8,
            );
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        // Vertical: 0/2 share physical 0 ($21), 1/3 share physical 1 ($31).
        assert_eq!(nt.tiles[0][0], 0x21);
        assert_eq!(nt.tiles[1][0], 0x31);
        assert_eq!(nt.tiles[2][0], 0x21);
        assert_eq!(nt.tiles[3][0], 0x31);
    }

    #[test]
    fn single_screen_lower_mirroring() {
        // Every logical table shows the $2000 block; the writes land in turn, so $31 survives.
        let mut cart = nrom_with(vec![0; 8192], Mirroring::SingleScreenLower, false);
        let mut ppu = Ppu::new();
        for t in 0..4u16 {
            ppu_write(
                &mut ppu,
                cart.cart.as_mut(),
                0x2000 + t * 0x400,
                0x01 | (t << 4) as u8,
            );
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        for tiles in &nt.tiles {
            assert_eq!(tiles[0], 0x31);
        }
        assert_eq!(ppu.vram[0], 0x31);
        assert_eq!(
            ppu.vram[0x400], 0,
            "the upper block is unreachable in this mode"
        );
    }

    #[test]
    fn single_screen_upper_mirroring() {
        // Every logical table shows the $2400 block (physical table 1).
        let mut cart = nrom_with(vec![0; 8192], Mirroring::SingleScreenUpper, false);
        let mut ppu = Ppu::new();
        for t in 0..4u16 {
            ppu_write(
                &mut ppu,
                cart.cart.as_mut(),
                0x2000 + t * 0x400,
                0x01 | (t << 4) as u8,
            );
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        for tiles in &nt.tiles {
            assert_eq!(tiles[0], 0x31);
            assert_eq!(tiles[959], 0);
        }
        assert_eq!(ppu.vram[0x400], 0x31);
        assert_eq!(
            ppu.vram[0], 0,
            "the lower block is unreachable in this mode"
        );
    }

    /// Four-screen boards carry 4 KB of nametable RAM: each of the four logical tables is its own
    /// 1 KB block, so a write to each table address lands somewhere different and reads back
    /// unchanged.
    #[test]
    fn four_screen_mirroring_keeps_four_distinct_tables() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::FourScreen, false);
        let mut ppu = Ppu::new();
        for t in 0..4u16 {
            ppu_write(
                &mut ppu,
                cart.cart.as_mut(),
                0x2000 + t * 0x400,
                0x01 | (t << 4) as u8,
            );
        }
        for t in 0..4u16 {
            let addr = 0x2000 + t * 0x400;
            let want = 0x01 | (t << 4) as u8;
            assert_eq!(
                ppu.mem_read(cart.cart.as_mut(), addr),
                want,
                "read back {addr:#06x}"
            );
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        for (table, tiles) in nt.tiles.iter().enumerate() {
            assert_eq!(tiles[0], (0x01 | (table << 4)) as u8, "table {table}");
            assert_eq!(tiles[959], 0, "table {table} neighbour byte");
        }
    }

    /// A minimal MMC5 board: PRG stops mattering here, CHR is flat.
    fn mmc5_board() -> Mmc5 {
        let rom = RomData {
            header: Header {
                prg_banks: 2,
                chr_banks: 1,
                mapper: 5,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr: vec![0; 8192],
            chr_is_ram: false,
            trainer: None,
        };
        Mmc5::new(rom)
    }

    /// MMC5 keeps its nametables in its own 2 KiB of CIRAM and its ExRAM, not in the PPU's 2 KB:
    /// `mirroring()` answers `FourScreen` only as a placeholder for the runtime `$5105` map. The
    /// viewer must ask the board (`nt_peek`) or it shows `ppu.vram`, which MMC5 games never write. $5105 = $E4 routes table 0 to CIRAM page 0, table 1 to page 1, table 2 to ExRAM and
    /// table 3 to fill mode, so four different board sources reach the four panels.
    #[test]
    fn mmc5_nametables_come_from_the_board() {
        let mut board = mmc5_board();
        let mut ppu = Ppu::new();
        board.cpu_write(0x5105, 0xE4);
        for (table, val) in [
            (0x2000u16, 0x11u8),
            (0x2400, 0x22),
            (0x2800, 0x33),
            (0x2C00, 0x44),
        ] {
            ppu_write(&mut ppu, &mut board, table, val);
            ppu_write(&mut ppu, &mut board, table + 0x3C0, val | 0x80);
        }
        assert!(
            ppu.vram.iter().all(|&b| b == 0),
            "the PPU's own 2 KB must stay untouched: MMC5 takes every nametable write"
        );
        let before = board_state(&board);
        let nt = nametables(&ppu, &board);
        assert_eq!(nt.tiles[0][0], 0x11, "table 0 = CIRAM page 0");
        assert_eq!(nt.tiles[1][0], 0x22, "table 1 = CIRAM page 1");
        assert_eq!(nt.tiles[2][0], 0x33, "table 2 = ExRAM");
        assert_eq!(
            nt.tiles[3][0], 0x00,
            "table 3 = fill mode, which drops writes"
        );
        assert_eq!(nt.attrs[0][0], 0x91);
        assert_eq!(nt.attrs[1][0], 0xA2);
        assert_eq!(nt.attrs[2][0], 0xB3);
        assert_eq!(nt.attrs[3][0], 0x00);
        assert_eq!(
            board_state(&board),
            before,
            "the snapshot must not clock MMC5's split-screen fetch counter"
        );
    }

    /// Namco 163's bank registers 8-11 bank CHR-ROM (or a CIRAM page) into each nametable window,
    /// again outside the PPU's 2 KB. Each CHR 1 KiB page here holds its page number, so the
    /// four panels must show the four banked pages.
    #[test]
    fn namco163_banked_nametables_come_from_the_board() {
        let chr: Vec<u8> = (0..32768).map(|i| (i / 0x400) as u8).collect();
        let rom = RomData {
            header: Header {
                prg_banks: 2,
                chr_banks: 4,
                mapper: 19,
                mirroring: Mirroring::Vertical,
                ..Default::default()
            },
            prg: vec![0; 32768],
            chr,
            chr_is_ram: false,
            trainer: None,
        };
        let mut board = crate::mapper::n163::Namco163::new(rom);
        let ppu = Ppu::new();
        for (i, page) in (5u8..9).enumerate() {
            board.cpu_write(0xC000 + i as u16 * 0x800, page);
        }
        let nt = nametables(&ppu, &board);
        for (table, tiles) in nt.tiles.iter().enumerate() {
            assert_eq!(tiles[0], 5 + table as u8, "table {table}");
        }
        assert!(ppu.vram.iter().all(|&b| b == 0));
    }

    #[test]
    fn nametable_full_grid_and_attributes() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::SingleScreenLower, false);
        let mut ppu = Ppu::new();
        // Fill the whole logical $2000 block so every routing decision is exercised.
        for i in 0..1024u16 {
            ppu_write(&mut ppu, cart.cart.as_mut(), 0x2000 + i, (i % 251) as u8);
        }
        let nt = nametables(&ppu, cart.cart.as_ref());
        for (i, &tile) in nt.tiles[0].iter().enumerate() {
            assert_eq!(tile, (i % 251) as u8);
        }
        for (i, &a) in nt.attrs[0].iter().enumerate() {
            assert_eq!(a, ((0x3C0 + i as u16) % 251) as u8);
        }
    }

    #[test]
    fn oam_decode() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Horizontal, false);
        let mut ppu = Ppu::new();
        ppu.write_register(cart.cart.as_mut(), 0x2003, 0x00);
        // Sprite 0: y, tile, attr $FF (masked to $E3 by the attr mask), x.
        for &v in &[0x20u8, 0x05, 0xFF, 0x30] {
            ppu.write_register(cart.cart.as_mut(), 0x2004, v);
        }
        // Sprite 1: palette 0, priority behind, no flips.
        for &v in &[0x50u8, 0x11, 0x24, 0xF0] {
            ppu.write_register(cart.cart.as_mut(), 0x2004, v);
        }
        let dump = oam(&ppu);
        assert_eq!(dump.sprites.len(), 64);
        assert_eq!(dump.oam_addr, 8);
        let s0 = dump.sprites[0];
        assert_eq!(s0.index, 0);
        assert_eq!(s0.y, 0x20);
        assert_eq!(s0.tile, 0x05);
        assert_eq!(s0.attr, 0xE3); // bits 2-4 do not exist in OAM
        assert_eq!(s0.x, 0x30);
        assert_eq!(s0.palette(), 3);
        assert!(s0.behind_background());
        assert!(s0.flip_horizontal());
        assert!(s0.flip_vertical());
        let s1 = dump.sprites[1];
        assert_eq!(s1.attr, 0x20); // $24 with non-existent bit 2 masked out
        assert_eq!(s1.palette(), 0);
        assert!(s1.behind_background());
        assert!(!s1.flip_horizontal());
        assert!(!s1.flip_vertical());
        assert_eq!(s1.y, 0x50);
        assert_eq!(s1.tile, 0x11);
        assert_eq!(s1.x, 0xF0);
        // The remaining entries decode to power-on zero bytes.
        assert_eq!(
            dump.sprites[2],
            Sprite {
                index: 2,
                y: 0,
                tile: 0,
                attr: 0,
                x: 0
            }
        );
        assert_eq!(dump.sprites[63].x, 0);
    }

    /// An 8x16 sprite's tile byte is a table select (bit 0) plus a tile pair; PPUCTRL bit 3 does
    /// not apply to it.
    #[test]
    fn sprite_8x16_tile_pair_and_bank() {
        let sprite = Sprite {
            index: 0,
            y: 0,
            tile: 0x43,
            attr: 0,
            x: 0,
        };
        assert_eq!(sprite.pattern_table_16(), 1);
        assert_eq!(sprite.tile_pair_16(), (0x42, 0x43));
        let sprite = Sprite {
            tile: 0x42,
            ..sprite
        };
        assert_eq!(sprite.pattern_table_16(), 0);
        assert_eq!(sprite.tile_pair_16(), (0x42, 0x43));
        let sprite = Sprite {
            tile: 0xFF,
            ..sprite
        };
        assert_eq!(sprite.pattern_table_16(), 1);
        assert_eq!(sprite.tile_pair_16(), (0xFE, 0xFF));
        let sprite = Sprite {
            tile: 0x00,
            ..sprite
        };
        assert_eq!(sprite.pattern_table_16(), 0);
        assert_eq!(sprite.tile_pair_16(), (0x00, 0x01));
    }

    #[test]
    fn palette_mapping_and_greyscale() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Horizontal, false);
        let mut ppu = Ppu::new();
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F00, 0x0F);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F01, 0x0A);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F02, 0x0B);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F03, 0x0C);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F11, 0x2D);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F12, 0x2E);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F13, 0x2F);
        // $3F10 is the $3F00 mirror: writing there overwrites the backdrop, not sprite colour 0x10.
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F10, 0x2A);
        let dump = palette(&ppu);
        assert_eq!(dump.ram[0], 0x2A);
        assert_eq!(
            dump.ram[0x10], 0x2A,
            "the $3F10 mirror reads back the backdrop"
        );
        assert_eq!(dump.bg[0], [0x2A, 0x0A, 0x0B, 0x0C]);
        assert_eq!(dump.sprites[0], [0x2A, 0x2D, 0x2E, 0x2F]);
        // Sprite palette 1 was never written: the mirror entry reads back the backdrop and the
        // untouched entries hold the power-up palette, not zero-filled.
        assert_eq!(
            dump.sprites[1],
            [
                0x2A,
                POWER_UP_PALETTE[0x15],
                POWER_UP_PALETTE[0x16],
                POWER_UP_PALETTE[0x17]
            ]
        );
        assert!(!dump.greyscale);
        assert_eq!(dump.emphasis, 0);
        // Greyscale + emphasis from $2001.
        ppu.write_register(cart.cart.as_mut(), 0x2001, 0xC1);
        let dump = palette(&ppu);
        assert!(dump.greyscale);
        assert_eq!(dump.emphasis, 6);
        assert_eq!(dump.bg[0], [0x20, 0x00, 0x00, 0x00]); // 0x2A AND $30 = $20
        assert_eq!(dump.sprites[0], [0x20, 0x20, 0x20, 0x20]); // 0x2D-0x2F AND $30 = $20
    }

    /// The sprite mirrors $3F10/$3F14/$3F18/$3F1C have no storage of their own: the raw dump must
    /// show the backdrop bytes an address read returns, not the untouched power-up bytes
    /// still sitting in the backing array where `palette_index` folded the write into the
    /// background entry.
    #[test]
    fn palette_ram_shows_mirrored_entries() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Horizontal, false);
        let mut ppu = Ppu::new();
        for (addr, val) in [
            (0x3F00u16, 0x0Fu8),
            (0x3F04, 0x21),
            (0x3F08, 0x16),
            (0x3F0C, 0x27),
        ] {
            ppu_write(&mut ppu, cart.cart.as_mut(), addr, val);
        }
        // Only the backing entries were written, so palette[0x10] keeps its power-up colour,
        // not a zero-filled slot.
        assert_eq!(ppu.palette[0x10], POWER_UP_PALETTE[0x10]);
        let dump = palette(&ppu);
        assert_eq!(
            [
                dump.ram[0x10],
                dump.ram[0x14],
                dump.ram[0x18],
                dump.ram[0x1C]
            ],
            [0x0F, 0x21, 0x16, 0x27]
        );
        // The fold shows up position-by-position: each sprite-mirror entry reads back its
        // background twin. The power-up fill means the non-mirror sprite slots between
        // them no longer zero out to a whole-slice equality with their background twins.
        assert_eq!(dump.ram[0x10], dump.ram[0x00]);
        assert_eq!(dump.ram[0x14], dump.ram[0x04]);
        assert_eq!(dump.ram[0x18], dump.ram[0x08]);
        assert_eq!(dump.ram[0x1C], dump.ram[0x0C]);
        // The non-mirrored sprite entries keep their own untouched power-up bytes, not
        // zeros — the write loop never reached $3F11 or $3F1F.
        assert_eq!(dump.ram[0x11], POWER_UP_PALETTE[0x11]);
        assert_eq!(dump.ram[0x1F], POWER_UP_PALETTE[0x1F]);
    }

    #[test]
    fn scroll_registers_snapshot() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Horizontal, false);
        let mut ppu = Ppu::new();
        // $2006 pair sets v = t = $2155.
        ppu.write_register(cart.cart.as_mut(), 0x2006, 0x21);
        ppu.write_register(cart.cart.as_mut(), 0x2006, 0x55);
        // $2005 pair: first write coarse_x=31 fine_x=0, second fine_y=7, coarse_y upper bits.
        ppu.write_register(cart.cart.as_mut(), 0x2005, 0xF8);
        ppu.write_register(cart.cart.as_mut(), 0x2005, 0x1F);
        let s = scroll(&ppu);
        assert_eq!(s.v, 0x2155); // $2005 never touches v
        assert_eq!(s.t, 0x707F);
        assert_eq!(s.fine_x_scroll, 0);
        assert_eq!(s.write_toggle, 0); // a complete $2005/$2006 pair leaves the toggle clear
        assert_eq!(s.t_coarse_x(), 31);
        assert_eq!(s.t_coarse_y(), 3);
        assert_eq!(s.t_nametable(), 0);
        assert_eq!(s.t_fine_y(), 7);
        assert_eq!(s.v_coarse_x(), 0x15);
        assert_eq!(s.v_coarse_y(), 0x0A);
        assert_eq!(s.v_nametable(), 0);
        assert_eq!(s.v_fine_y(), 2);
        // PPUCTRL writes the nametable bits of t plus the viewer's size/bank flags.
        ppu.write_register(cart.cart.as_mut(), 0x2000, 0xB0);
        let s = scroll(&ppu);
        assert_eq!(s.ctrl, 0xB0);
        assert!(s.sprite_size_16());
        assert_eq!(s.bg_pattern_table(), 1);
        assert_eq!(s.sprite_pattern_table(), 0);
        // Two $2007 writes with the default +1 increment step v each time (rendering disabled,
        // so no render-time side effects).
        ppu.write_register(cart.cart.as_mut(), 0x2007, 0x01);
        ppu.write_register(cart.cart.as_mut(), 0x2007, 0x02);
        assert_eq!(scroll(&ppu).v, 0x2157);
    }

    #[test]
    fn position_snapshot() {
        let mut ppu = Ppu::new();
        let pos = position(&ppu);
        assert_eq!(pos.scanline, 0);
        assert_eq!(pos.dot, 0);
        assert_eq!(pos.frame, 0);
        assert_eq!(pos.lines_per_frame, 262);
        ppu.scanline = 123;
        ppu.dot = 45;
        ppu.frame = 7;
        assert_eq!(
            position(&ppu),
            Position {
                scanline: 123,
                dot: 45,
                frame: 7,
                lines_per_frame: 262
            }
        );
        // Frame geometry comes from the region, not from the save state.
        ppu.set_region(crate::cart::Region::Pal);
        assert_eq!(position(&ppu).lines_per_frame, 312);
    }

    #[test]
    fn snapshot_survives_save_load_state() {
        let mut cart = nrom_with(vec![0; 8192], Mirroring::Vertical, true);
        let mut ppu = Ppu::new();
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x2000, 0x12);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x2400, 0x34);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F01, 0x21);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x3F13, 0x2C);
        ppu.write_register(cart.cart.as_mut(), 0x2003, 0x00);
        for &b in &[0x30u8, 0x21, 0x02, 0x10] {
            ppu.write_register(cart.cart.as_mut(), 0x2004, b);
        }
        ppu.write_register(cart.cart.as_mut(), 0x2006, 0x21);
        ppu.write_register(cart.cart.as_mut(), 0x2006, 0x55);
        ppu.write_register(cart.cart.as_mut(), 0x2007, 0x77); // VRAM at $2155, v steps to $2156
        ppu.scanline = 10;

        let before = snapshot(&ppu, cart.cart.as_ref());

        let mut w = StateWriter::new();
        ppu.save_state(&mut w);
        let bytes = w.finish();
        let mut restored = Ppu::new();
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);

        let after = snapshot(&restored, cart.cart.as_ref());
        assert_eq!(before, after);
    }

    /// A CHR-RAM board: the patterns the PPU just wrote come back through the viewer straight off
    /// the mapper — no caller-side CHR copy, no `mem_read` loop.
    #[test]
    fn snapshot_pattern_tables_follow_chr_ram_writes() {
        let mut cart = nrom_with(vec![0xA5; 8192], Mirroring::Horizontal, true);
        let mut ppu = Ppu::new();
        // Tile 3, row 0 planes: lo byte $F0, hi byte $0F → left half colour 1, right half 2.
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x0030, 0xF0);
        ppu_write(&mut ppu, cart.cart.as_mut(), 0x0038, 0x0F);
        let snap = snapshot(&ppu, cart.cart.as_ref());
        let t3 = snap.pattern_tables.tables[0][3];
        assert_eq!(t3.pixels[0], [1, 1, 1, 1, 2, 2, 2, 2]);
        assert_eq!(t3.pixels[1], [3, 0, 3, 0, 0, 3, 0, 3]); // an untouched row shows the $A5 fill
        // The composed snapshot agrees with the individual functions.
        assert_eq!(snap.oam, oam(&ppu));
        assert_eq!(snap.palette, palette(&ppu));
        assert_eq!(snap.scroll, scroll(&ppu));
        assert_eq!(snap.position, position(&ppu));
        assert_eq!(snap.nametables, nametables(&ppu, cart.cart.as_ref()));
        assert_eq!(snap.pattern_tables, pattern_tables(cart.cart.as_ref()));
    }

    /// MMC2 switches its 4 KiB CHR banks when the PPU fetches tile $FD/$FE. A snapshot must read
    /// the latched banks without latching anything itself.
    #[test]
    fn mmc2_snapshot_leaves_latches_untouched() {
        let mut board = mmc2_board();
        // $B000 = latch-0 $FD bank, $C000 = latch-0 $FE bank (power-on latch is $FE);
        // $D000 = latch-1 $FD bank, $E000 = latch-1 $FE bank.
        board.cpu_write(0xB000, 2);
        board.cpu_write(0xC000, 3);
        board.cpu_write(0xD000, 4);
        board.cpu_write(0xE000, 5);
        // CHR page `p` is filled with the byte `p`, so the mapped page is visible in the value.
        assert_eq!(
            board.chr_peek(0x0000),
            3,
            "power-on latch 0 selects the $FE register"
        );
        assert_eq!(
            board.chr_peek(0x1000),
            5,
            "power-on latch 1 selects the $FE register"
        );
        // The PPU's own fetch of tile $FD switches latch 0 — that is the board working normally.
        assert_eq!(
            board.ppu_read(0x0FD8),
            3,
            "the triggering fetch still uses the old bank"
        );
        assert_eq!(
            board.chr_peek(0x0000),
            2,
            "latch 0 now selects the $FD bank"
        );

        let before = board_state(&board);
        let ppu = Ppu::new();
        let _ = snapshot(&ppu, &board);
        assert_eq!(
            board_state(&board),
            before,
            "snapshot moved MMC2 latch state"
        );
        assert_eq!(
            board.chr_peek(0x0000),
            2,
            "latch 0 was reset by the snapshot"
        );
        assert_eq!(
            board.chr_peek(0x1000),
            5,
            "latch 1 was moved by the snapshot"
        );
        // And the decode shows the latched banks, not the power-on ones.
        let pt = pattern_tables(&board);
        let page_2 = Tile::from_planes(&[2u8; 16]);
        let page_5 = Tile::from_planes(&[5u8; 16]);
        assert_ne!(page_2, page_5);
        assert_eq!(pt.tables[0][0], page_2, "table $0000 decodes the $FD bank");
        assert_eq!(pt.tables[1][0], page_5, "table $1000 decodes the $FE bank");
    }

    /// Same contract for MMC4, whose latches trigger on the wider $0FD8-$0FDF / $0FE8-$0FEF ranges.
    #[test]
    fn mmc4_snapshot_leaves_latches_untouched() {
        let mut board = mmc4_board();
        board.cpu_write(0xB000, 2);
        board.cpu_write(0xC000, 3);
        board.cpu_write(0xD000, 4);
        board.cpu_write(0xE000, 5);
        assert_eq!(board.chr_peek(0x0000), 3);
        assert_eq!(board.chr_peek(0x1000), 5);
        // Any address in the range triggers the latch, not just $0FD8.
        assert_eq!(board.ppu_read(0x0FDD), 3);
        assert_eq!(board.chr_peek(0x0000), 2);

        let before = board_state(&board);
        let ppu = Ppu::new();
        let _ = snapshot(&ppu, &board);
        assert_eq!(
            board_state(&board),
            before,
            "snapshot moved MMC4 latch state"
        );
        assert_eq!(
            board.chr_peek(0x0000),
            2,
            "latch 0 was reset by the snapshot"
        );
        assert_eq!(
            board.chr_peek(0x1000),
            5,
            "latch 1 was moved by the snapshot"
        );
    }

    /// MMC3 clocks its scanline counter on filtered A12 rises. A snapshot reads CHR through
    /// `chr_peek`, so neither the counter nor the A12 filter state moves.
    #[test]
    fn mmc3_snapshot_leaves_irq_untouched() {
        let rom = rom_bytes(4, 2, 2);
        let mut board = Mmc3::new(split(&rom).unwrap());
        board.cpu_write(0xC000, 3); // IRQ latch = 3
        board.cpu_write(0xC001, 0); // reload on the next rise
        board.cpu_write(0xE001, 0); // enable
        // One real A12 rise: low fetch, >= 3 CPU cycles, then a high fetch.
        board.ppu_read(0x0000);
        for _ in 0..4 {
            board.cpu_clock();
        }
        board.ppu_read(0x1000);
        assert_eq!(board.irq.counter, 3, "the counter reloaded from the latch");
        let before = board.irq.clone();

        let ppu = Ppu::new();
        let _ = snapshot(&ppu, &board);

        assert_eq!(
            board.irq.counter, before.counter,
            "snapshot clocked the scanline counter"
        );
        assert_eq!(
            board.irq, before,
            "snapshot moved the A12 filter / IRQ state"
        );
        // The peek still returns the live mapping, and the counter is still armed.
        assert_eq!(board.chr_peek(0x0000), 0);
        assert_eq!(board.chr_peek(0x1000), 0);
        assert!(board.irq.enabled);
    }
}
