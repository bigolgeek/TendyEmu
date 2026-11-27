//! The debugger panel's headless half: four viewers' data, the watch list, the
//! hex-address parser and the key state machine.
//!
//! Nothing here names macroquad or a window. `tests/debugger.rs` drives this module directly, and
//! `src/main.rs` only paints what it returns — the same split as `menu`, `draw` and `filter`
//! Every read is a `&[u8]` slice of the machine's memories or a `u8` register, so no
//! viewer can spend a machine cycle, mutate the PPU's ownership state or clear a latch.

use crate::{OAM_SIZE, VRAM_SIZE};

/// Tiles in `$8000-$97FF`: the tile data the background and the objects can address without the
/// signed `$8800` mode.
pub const TILES: usize = 384;
/// Bytes a tile takes: two per row, eight rows.
pub const TILE_BYTES: usize = 16;
/// Tiles across one background map.
pub const MAP_TILES: usize = 32;
/// Pixels across one background map (`MAP_TILES * 8`).
pub const MAP_PIXELS: usize = MAP_TILES * 8;
/// Sprites in OAM.
pub const SPRITES: usize = 40;
/// A transparent sprite pixel in [`sprites`]' canvas; the shades are 0..=3.
pub const TRANSPARENT: u8 = 0xFF;
/// Bytes a memory view row holds.
pub const ROW_BYTES: u16 = 16;
/// Tiles the grid draws to a row, and the cell they sit in: 8 pixels of tile and one of gap, so a
/// grid of 8x8 tiles reads as a grid rather than a wash.
pub const TILE_COLUMNS: usize = 16;
pub const TILE_CELL: usize = 9;
/// Watched addresses the list holds before `B` starts refusing.
pub const WATCH_LIMIT: usize = 32;

/// A rectangle in window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// True when the point is inside, the top-left edge included and the bottom-right excluded.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The panel's place in the picture area: the right 62 % of it, at least 240 px wide (the map's
/// 256-pixel grid and a 40-sprite list need the room) and never wider than the picture it sits on.
///
/// The panel is decoration over the picture, so it never leaves the picture area: the menu's mouse
/// hit test walks the drawer's own rows and its "click outside goes back a level" rule reads the
/// drawer's width, and neither can see this rectangle unless a caller hands it over.
pub fn panel_rect(picture_w: f32, picture_h: f32) -> Rect {
    let w = (picture_w * 0.62).max(240.0).min(picture_w);
    Rect::new(picture_w - w, 0.0, w, picture_h)
}

/// `src` scaled to fit inside `area` on whole pixels where there is room, centred. One scale for
/// both axes, so a tile stays square.
///
/// The scale is the largest whole multiple that fits — 1x, 2x, 3x — because a pixel stretched by
/// 1.4 has ragged edges; below 1x, where the source is bigger than the area (the 256-pixel map in a
/// narrow window), it is the exact fraction that fits.
pub fn fit_rect(area: Rect, src_w: f32, src_h: f32) -> Rect {
    if src_w <= 0.0 || src_h <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
        return Rect::new(area.x, area.y, 0.0, 0.0);
    }
    let fit = (area.w / src_w).min(area.h / src_h);
    let scale = if fit >= 1.0 { fit.floor() } else { fit };
    let (w, h) = (src_w * scale, src_h * scale);
    Rect::new(
        area.x + ((area.w - w) / 2.0).floor(),
        area.y + ((area.h - h) / 2.0).floor(),
        w,
        h,
    )
}

/// Which console the panel is looking at.
///
/// The panel's rows, keys and views are the same for both consoles; what changes is how wide an
/// address is, how many hex digits it prints with, and what the machine's memory map calls the
/// region an address falls in. One enum, so a caller cannot pass the console's width without the
/// console's map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Console {
    /// A Game Boy (or Game Boy Color): a 16-bit address space, four hex digits.
    #[default]
    GameBoy,
    /// A Game Boy Advance: 32 bits, eight hex digits, and GBATEK's memory map.
    Advance,
}

impl Console {
    /// The digits an address prints with: `$C000` is four, `$03007F00` eight.
    pub fn digits(self) -> usize {
        match self {
            Console::GameBoy => 4,
            Console::Advance => 8,
        }
    }

    /// Every bit of an address on this console: `$0000-$FFFF` or the whole 32-bit space.
    pub fn mask(self) -> u32 {
        match self {
            Console::GameBoy => 0xFFFF,
            Console::Advance => u32::MAX,
        }
    }

    /// Where `G` starts the Memory view, one screenful that is worth looking at: the Game Boy's
    /// work RAM at `$C000`, where a cartridge keeps its variables; the Advance's IWRAM at
    /// `$03000000`, which is where its stack and its variables live.
    pub fn home(self) -> u32 {
        match self {
            Console::GameBoy => 0xC000,
            Console::Advance => 0x0300_0000,
        }
    }

    /// The region `addr` falls in, by the names the platform's own documentation uses: Pan Docs'
    /// "Memory Map" for the Game Boy, GBATEK's *GBA Memory Map* for the Advance.
    ///
    /// The label describes the whole physical region the address sits in, so a mirror answers with
    /// the region's name rather than `none`: `$C123` and `$F123` are both work RAM on a Game Boy,
    /// the second through the echo at `$E000-$FDFF`.
    pub fn region(self, addr: u32) -> &'static str {
        match self {
            Console::GameBoy => match addr {
                0x0000..=0x3FFF => "ROM0",
                0x4000..=0x7FFF => "ROMX",
                0x8000..=0x9FFF => "VRAM",
                0xA000..=0xBFFF => "SRAM",
                0xC000..=0xCFFF => "WRAM0",
                0xD000..=0xDFFF => "WRAMX",
                0xE000..=0xFDFF => "ECHO",
                0xFE00..=0xFE9F => "OAM",
                0xFEA0..=0xFEFF => "unusable",
                0xFF00..=0xFF7F => "IO",
                0xFF80..=0xFFFE => "HRAM",
                0xFFFF..=0xFFFF => "IE",
                _ => "none",
            },
            Console::Advance => match addr {
                0x0000_0000..=0x0000_3FFF => "BIOS",
                0x0200_0000..=0x0203_FFFF => "EWRAM",
                0x0300_0000..=0x0300_7FFF => "IWRAM",
                0x0400_0000..=0x0400_03FE => "I/O",
                0x0500_0000..=0x0500_03FF => "PALETTE",
                0x0600_0000..=0x0601_7FFF => "VRAM",
                0x0700_0000..=0x0700_03FF => "OAM",
                0x0800_0000..=0x0DFF_FFFF => "ROM",
                0x0E00_0000..=0x0E00_FFFF => "SAVE",
                _ => "none",
            },
        }
    }
}

/// An address as the panel writes it: `$C123` on a Game Boy, `$080001A4` on an Advance.
pub fn format_addr(addr: u32, digits: usize) -> String {
    format!("${addr:0width$X}", width = digits)
}

/// The four viewers, in `Tab` order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Tiles,
    Map,
    Oam,
    Memory,
}

impl View {
    /// `Tab`'s order, wrapping.
    pub const ALL: [View; 4] = [View::Tiles, View::Map, View::Oam, View::Memory];

    pub fn next(self) -> View {
        match self {
            View::Tiles => View::Map,
            View::Map => View::Oam,
            View::Oam => View::Memory,
            View::Memory => View::Tiles,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Tiles => "Tiles",
            View::Map => "Map",
            View::Oam => "OAM",
            View::Memory => "Memory",
        }
    }
}

/// The PPU registers a viewer reads, copied out in one go so a frame cannot see half of one
/// moment and half of the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewRegs {
    pub lcdc: u8,
    pub scx: u8,
    pub scy: u8,
    pub bgp: u8,
    pub obp0: u8,
    pub obp1: u8,
    /// `LY` and `WX`/`WY` are shown in the header; the map's scroll marker uses `scx`/`scy`.
    pub ly: u8,
    pub wx: u8,
    pub wy: u8,
}

impl ViewRegs {
    /// LCDC bit 3: the background uses the `$9C00` map.
    pub fn map_select(&self) -> u8 {
        (self.lcdc >> 3) & 1
    }

    /// LCDC bit 2: objects are 8x16.
    pub fn tall_sprites(&self) -> bool {
        self.lcdc & 0x04 != 0
    }

    /// LCDC bit 6: the window is on.
    pub fn window_on(&self) -> bool {
        self.lcdc & 0x20 != 0
    }
}

/// A colour index through a palette register: BGP/OBP0/OBP1 map 2-bit colours to shades 0..=3.
pub fn shade(colour: u8, palette: u8) -> u8 {
    (palette >> ((colour & 3) * 2)) & 3
}

/// One tile's 64 colours (0..=3), row-major, top-left first.
///
/// `index` is the 0..=383 tile at `$8000 + index * 16`; out-of-range indices wrap, as the PPU's
/// own 13-bit mask does. Bit 0 of a row comes from the row's first byte and bit 1 from the second,
/// most significant bit leftmost.
pub fn tile_colours(vram: &[u8; VRAM_SIZE], index: usize) -> [u8; 64] {
    let mut out = [0u8; 64];
    let base = (index % TILES) * TILE_BYTES;
    for y in 0..8 {
        let lo = vram[base + y * 2];
        let hi = vram[base + y * 2 + 1];
        for x in 0..8 {
            let bit = 7 - x;
            out[y * 8 + x] = ((lo >> bit) & 1) | (((hi >> bit) & 1) << 1);
        }
    }
    out
}

/// The Tiles view's picture: all 384 tiles, 16 to a row on a [`TILE_CELL`]-pixel cell, so one
/// transparent column and row separates neighbours. Returns `(width, height, colours)` with
/// colours 0..=3 and [`TRANSPARENT`] in the gaps.
pub fn tiles_image(vram: &[u8; VRAM_SIZE]) -> (usize, usize, Vec<u8>) {
    let rows = TILES / TILE_COLUMNS;
    let (width, height) = (TILE_COLUMNS * TILE_CELL, rows * TILE_CELL);
    let mut out = vec![TRANSPARENT; width * height];
    for index in 0..TILES {
        let colours = tile_colours(vram, index);
        let (cx, cy) = tile_cell(index);
        for y in 0..8 {
            for x in 0..8 {
                out[(cy + y) * width + cx + x] = colours[y * 8 + x];
            }
        }
    }
    (width, height, out)
}

/// Where tile `index` sits in [`tiles_image`], in its pixels.
pub fn tile_cell(index: usize) -> (usize, usize) {
    (
        (index % TILE_COLUMNS) * TILE_CELL,
        (index / TILE_COLUMNS) * TILE_CELL,
    )
}

/// One 256x256 background map as shades 0..=3: the 32x32 tile indices at `$9800` (`map` 0) or
/// `$9C00` (`map` 1), each tile's colours through `bgp`.
///
/// The map is shown as 32 columns of tiles; unlike the screen it does not wrap at the scroll
/// position, because the point of the view is the grid the ROM wrote, not the window into it.
pub fn map_shades(vram: &[u8; VRAM_SIZE], map: u8, bgp: u8) -> Vec<u8> {
    let base = if map == 0 { 0x1800 } else { 0x1C00 };
    let mut out = vec![0u8; MAP_PIXELS * MAP_PIXELS];
    for ty in 0..MAP_TILES {
        for tx in 0..MAP_TILES {
            let index = usize::from(vram[base + ty * MAP_TILES + tx]);
            let colours = tile_colours(vram, index);
            for y in 0..8 {
                let row = (ty * 8 + y) * MAP_PIXELS + tx * 8;
                for x in 0..8 {
                    out[row + x] = shade(colours[y * 8 + x], bgp);
                }
            }
        }
    }
    out
}

/// One sprite as OAM holds it, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sprite {
    /// Its place in OAM: 0 is the highest priority.
    pub index: u8,
    /// Screen X and Y of the sprite's top-left pixel (the OAM bytes minus 8 and 16).
    pub x: i16,
    pub y: i16,
    /// The tile byte as written; in 8x16 mode the low bit is ignored.
    pub tile: u8,
    pub flags: u8,
}

impl Sprite {
    /// Flag bit 4: OBP1 instead of OBP0.
    pub fn palette(&self) -> u8 {
        self.flags & 0x10
    }

    /// Flag bit 5: mirrored horizontally.
    pub fn x_flip(&self) -> bool {
        self.flags & 0x20 != 0
    }

    /// Flag bit 6: mirrored vertically.
    pub fn y_flip(&self) -> bool {
        self.flags & 0x40 != 0
    }

    /// Flag bit 7: behind background colours 1-3.
    pub fn behind(&self) -> bool {
        self.flags & 0x80 != 0
    }

    /// 8 or 16, from LCDC bit 2.
    pub fn height(&self, tall: bool) -> i16 {
        if tall { 16 } else { 8 }
    }
}

impl Default for Sprite {
    fn default() -> Self {
        Sprite {
            index: 0,
            x: -8,
            y: -16,
            tile: 0,
            flags: 0,
        }
    }
}

/// The 40 sprites, in OAM order. Reads only the 160 OAM bytes: no mode gate, no DMA.
pub fn sprites(oam: &[u8; OAM_SIZE]) -> [Sprite; SPRITES] {
    let mut out = [Sprite::default(); SPRITES];
    for (i, sprite) in out.iter_mut().enumerate() {
        *sprite = Sprite {
            index: i as u8,
            x: i16::from(oam[i * 4 + 1]) - 8,
            y: i16::from(oam[i * 4]) - 16,
            tile: oam[i * 4 + 2],
            flags: oam[i * 4 + 3],
        };
    }
    out
}

/// The 40 sprites alone on a 160x144 canvas: `WIDTH * HEIGHT` shades, [`TRANSPARENT`] where no
/// sprite draws.
///
/// Every sprite is drawn, whatever its X and Y: a sprite parked at `x = 0` (off the left edge) is
/// in OAM and the point of this picture is to show what is in OAM. Where two sprites overlap the
/// lower OAM index wins, as the PPU's priority does; flag bit 7 (behind the background) is
/// deliberately ignored, because there is no background here to be behind.
pub fn sprite_canvas(vram: &[u8; VRAM_SIZE], oam: &[u8; OAM_SIZE], regs: ViewRegs) -> Vec<u8> {
    let mut out = vec![TRANSPARENT; crate::WIDTH * crate::HEIGHT];
    let tall = regs.tall_sprites();
    let list = sprites(oam);
    // Back to front, so index 0's pixels are written last and stay.
    for sprite in list.iter().rev() {
        let height = sprite.height(tall);
        let first = if tall {
            sprite.tile & 0xFE
        } else {
            sprite.tile
        };
        let palette = if sprite.palette() != 0 {
            regs.obp1
        } else {
            regs.obp0
        };
        for row in 0..height {
            let y = sprite.y + row;
            if !(0..crate::HEIGHT as i16).contains(&y) {
                continue;
            }
            let src_row = if sprite.y_flip() {
                (height - 1 - row) as usize
            } else {
                row as usize
            };
            // 8x16 sprites take the tile pair `first`, `first + 1`; only even tile numbers exist.
            let index = first as usize + src_row / 8;
            let colours = tile_colours(vram, index);
            for col in 0..8 {
                let x = sprite.x + col;
                if !(0..crate::WIDTH as i16).contains(&x) {
                    continue;
                }
                let src_col = if sprite.x_flip() { 7 - col } else { col };
                let colour = colours[(src_row % 8) * 8 + src_col as usize];
                // Colour 0 is transparent for every object palette.
                if colour == 0 {
                    continue;
                }
                out[y as usize * crate::WIDTH + x as usize] = shade(colour, palette);
            }
        }
    }
    out
}

/// `G`'s parser: `c000`, `$C000`, `0xC000` and `C000` all mean `$C000`.
///
/// One to four hex digits, either case, with an optional `$` or `0x`; leading and trailing spaces
/// are ignored. Anything else is `None` — an empty field, more than four digits, a stray sign,
/// `0x` with nothing after it, `xyz`. This is the Game Boy's width; a wider console asks
/// [`parse_hex_address_wide`] for its own.
pub fn parse_hex_address(text: &str) -> Option<u16> {
    parse_hex_address_wide(text, 4).map(|addr| addr as u16)
}

/// The same grammar, up to `digits` hex digits, for a console whose addresses are wider than 16
/// bits: `parse_hex_address_wide("080001a4", 8)` is `$080001A4`, and a ninth digit is refused
/// rather than silently dropped.
pub fn parse_hex_address_wide(text: &str, digits: usize) -> Option<u32> {
    let text = text.trim();
    let body = if let Some(rest) = text.strip_prefix('$') {
        rest
    } else if let Some(rest) = text.strip_prefix("0x") {
        rest
    } else if let Some(rest) = text.strip_prefix("0X") {
        rest
    } else {
        text
    };
    if body.is_empty() || body.len() > digits || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(body, 16).ok()
}

/// The watch list: the addresses a `B` `D` pair edits, in the order they were added.
///
/// A plain struct with no window and no machine in it, so `tests/debugger.rs` can drive the whole
/// of its behaviour. It is in-session only: it is not in the config file and not in a save state,
/// because a breakpoint describes the person at the keyboard, not the machine.
///
/// Addresses are 32 bits (H3): a watch on an Advance's `$080001A4` cannot be held in a `u16`, and
/// truncating one would watch a Game Boy address in the same machine. `digits` is only how they are
/// written — four hex digits for a Game Boy, eight for an Advance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watches {
    addrs: Vec<u32>,
    digits: usize,
}

impl Default for Watches {
    fn default() -> Self {
        Watches::new()
    }
}

impl Watches {
    /// A Game Boy's list: four-digit addresses, which is what every Game Boy watch has printed.
    pub fn new() -> Watches {
        Watches::with_width(4)
    }

    /// The same list, written with `digits` hex digits.
    pub fn with_width(digits: usize) -> Watches {
        Watches {
            addrs: Vec::new(),
            digits,
        }
    }

    /// Adds `addr`. False when it is already watched or the list is full (it does not grow past
    /// [`WATCH_LIMIT`]: the panel prints the list on one line).
    pub fn add(&mut self, addr: u32) -> bool {
        if self.addrs.contains(&addr) || self.addrs.len() >= WATCH_LIMIT {
            return false;
        }
        self.addrs.push(addr);
        true
    }

    /// Removes `addr`; false when it was not watched.
    pub fn remove(&mut self, addr: u32) -> bool {
        match self.addrs.iter().position(|a| *a == addr) {
            Some(at) => {
                self.addrs.remove(at);
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.addrs.clear();
    }

    pub fn contains(&self, addr: u32) -> bool {
        self.addrs.contains(&addr)
    }

    /// The address `pc` stops on, `None` when it is not watched. The same answer as `contains`,
    /// in the shape a caller wants when it has to say which one hit.
    pub fn hit(&self, pc: u32) -> Option<u32> {
        self.addrs.iter().copied().find(|a| *a == pc)
    }

    pub fn len(&self) -> usize {
        self.addrs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.addrs.iter().copied()
    }

    /// `$C123 $C456`, or `none`. The panel's one-line summary.
    pub fn text(&self) -> String {
        if self.addrs.is_empty() {
            return "none".to_string();
        }
        self.addrs
            .iter()
            .map(|a| format_addr(*a, self.digits))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One key the panel understands. `main.rs` owns the `KeyCode` translation, as it does for the
/// drawer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Tab,
    Space,
    Frame,
    Watch,
    Clear,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    /// `G`: start typing an address.
    Goto,
    /// A hex digit typed while `G` is waiting for one.
    Digit(char),
    Backspace,
    Enter,
    Escape,
}

/// What the front end must do after a key: step the machine, say something, or close.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// The panel handled it; the machine did not move.
    None,
    StepInstruction,
    StepFrame,
    /// Put this line in the bar's notice slot (and the panel's own header).
    Notice(String),
    /// `Esc` outside the address field: the panel asked to close.
    Close,
}

/// The panel's whole state: whether it is open, which view, where each view's cursor is, the
/// watch list and the address being typed.
#[derive(Clone, Debug)]
pub struct Debugger {
    pub open: bool,
    pub view: View,
    pub watches: Watches,
    /// The console the panel is looking at: how wide its addresses are and what its memory map
    /// calls them (H3).
    pub console: Console,
    /// The hex view's top row: always a multiple of [`ROW_BYTES`].
    pub memory_row: u32,
    /// The cursor's byte inside that row, 0..16.
    pub memory_byte: u8,
    /// The address being typed after `G`, or `None`.
    pub typing: Option<String>,
    /// The tile the Tiles view's cursor sits on, 0..[`TILES`].
    pub tile_cursor: usize,
    /// The cell the Map view's cursor sits on, `(x, y)`.
    pub map_cursor: (u8, u8),
    /// The last thing the panel said, shown under its title.
    pub message: Option<String>,
}

impl Default for Debugger {
    fn default() -> Self {
        Debugger::new()
    }
}

impl Debugger {
    /// A panel for a Game Boy: four-hex-digit addresses, `$C000` in the Memory view, Tiles first.
    pub fn new() -> Debugger {
        Debugger::on(Console::GameBoy)
    }

    /// The panel for `console`. An Advance opens on the Memory view, because its three Game Boy
    /// views draw a PPU it does not have; everything else — the rows, the keys, the four views in
    /// `Tab` order — is the same panel.
    pub fn on(console: Console) -> Debugger {
        Debugger {
            open: false,
            view: match console {
                Console::GameBoy => View::Tiles,
                Console::Advance => View::Memory,
            },
            watches: Watches::with_width(console.digits()),
            console,
            memory_row: console.home(),
            memory_byte: 0,
            typing: None,
            tile_cursor: 0,
            map_cursor: (0, 0),
            message: None,
        }
    }

    /// Point the panel at `console`, if it is not already. A console change takes the watch list
    /// with it: an address that named the Game Boy's code names something else on an Advance, and a
    /// breakpoint nobody meant to set is worse than no breakpoint.
    ///
    /// A Game Boy restarting as a Game Boy — the common case, every frame — costs one comparison.
    pub fn set_console(&mut self, console: Console) {
        if self.console == console {
            return;
        }
        self.console = console;
        self.view = match console {
            Console::GameBoy => View::Tiles,
            Console::Advance => View::Memory,
        };
        self.watches = Watches::with_width(console.digits());
        self.memory_row = console.home();
        self.memory_byte = 0;
        self.typing = None;
        self.tile_cursor = 0;
        self.map_cursor = (0, 0);
    }

    /// `F3`. Refused while the drawer is open — the drawer owns the keyboard and the mouse — and
    /// always allowed to close. Returns whether the panel is open afterwards.
    pub fn toggle(&mut self, drawer_open: bool) -> bool {
        if self.open {
            self.open = false;
            self.typing = None;
        } else if !drawer_open {
            self.open = true;
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
        self.typing = None;
    }

    /// The view's title line for the header: `Tiles (1 of 4)`.
    pub fn title(&self) -> String {
        format!("{} ({} of 4)", self.view.name(), self.view as u8 + 1)
    }

    /// The address field's text: the typed digits with a cursor, or the cursor's address.
    pub fn address_text(&self) -> String {
        match &self.typing {
            Some(text) => format!("{text}_"),
            None => format_addr(self.cursor_address(), self.console.digits()),
        }
    }

    /// The address under the Memory view's cursor.
    pub fn cursor_address(&self) -> u32 {
        self.memory_row.wrapping_add(u32::from(self.memory_byte))
    }

    /// One key, with the machine's PC for `B`/`D`. Everything the panel does to itself happens
    /// here; what it asks the front end to do comes back as an [`Action`].
    pub fn key(&mut self, key: Key, pc: u32) -> Action {
        // The address field swallows its own keys and enough to leave, so a mistyped
        // `G` cannot also open a breakpoint or step the machine.
        if self.typing.is_some() {
            let digits = self.console.digits();
            return match key {
                Key::Digit(c) => {
                    if let Some(text) = self.typing.as_mut()
                        && (c.is_ascii_hexdigit() && text.len() < digits)
                    {
                        text.push(c.to_ascii_uppercase());
                    }
                    Action::None
                }
                Key::Backspace => {
                    if let Some(text) = self.typing.as_mut() {
                        text.pop();
                    }
                    Action::None
                }
                Key::Enter => self.commit_address(),
                Key::Escape => {
                    self.typing = None;
                    let message = "address cancelled".to_string();
                    self.message = Some(message.clone());
                    Action::Notice(message)
                }
                _ => Action::None,
            };
        }

        match key {
            Key::Tab => {
                self.view = self.view.next();
                Action::None
            }
            Key::Space => Action::StepInstruction,
            Key::Frame => Action::StepFrame,
            Key::Watch => self.watch(pc),
            Key::Clear => self.clear_watches(),
            Key::Up => {
                self.move_cursor(0, -1);
                Action::None
            }
            Key::Down => {
                self.move_cursor(0, 1);
                Action::None
            }
            Key::Left => {
                self.move_cursor(-1, 0);
                Action::None
            }
            Key::Right => {
                self.move_cursor(1, 0);
                Action::None
            }
            Key::PageUp => {
                if self.view == View::Memory {
                    let mask = self.console.mask();
                    self.memory_row = self.memory_row.wrapping_sub(0x100) & mask;
                }
                Action::None
            }
            Key::PageDown => {
                if self.view == View::Memory {
                    let mask = self.console.mask();
                    self.memory_row = self.memory_row.wrapping_add(0x100) & mask;
                }
                Action::None
            }
            Key::Goto => {
                self.typing = Some(String::new());
                let message = "address (hex, Enter to go, Esc to stop)".to_string();
                self.message = Some(message.clone());
                Action::Notice(message)
            }
            Key::Digit(_) | Key::Backspace | Key::Enter => Action::None,
            Key::Escape => Action::Close,
        }
    }

    /// The panels' arrows: `dx`/`dy` in cells of the current view.
    fn move_cursor(&mut self, dx: i32, dy: i32) {
        match self.view {
            View::Tiles => {
                let at = self.tile_cursor as i32 + dx + dy * 16;
                self.tile_cursor = at.clamp(0, TILES as i32 - 1) as usize;
            }
            View::Map => {
                let (x, y) = self.map_cursor;
                let x = (i32::from(x) + dx).rem_euclid(MAP_TILES as i32) as u8;
                let y = (i32::from(y) + dy).rem_euclid(MAP_TILES as i32) as u8;
                self.map_cursor = (x, y);
            }
            View::Oam => {
                // The OAM list has no cursor; the arrows do nothing rather than pretend.
            }
            View::Memory => {
                let step = i64::from(dy) * i64::from(ROW_BYTES) + i64::from(dx);
                let span = i64::from(self.console.mask()) + 1;
                let at = (i64::from(self.cursor_address()) + step).rem_euclid(span) as u32;
                self.memory_row = at & !(u32::from(ROW_BYTES) - 1);
                self.memory_byte = (at & (u32::from(ROW_BYTES) - 1)) as u8;
            }
        }
    }

    /// `Enter` in the address field: go there, or say why not and keep the text.
    fn commit_address(&mut self) -> Action {
        let text = self.typing.clone().unwrap_or_default();
        let digits = self.console.digits();
        match parse_hex_address_wide(&text, digits) {
            Some(addr) => {
                self.memory_row = addr & !(u32::from(ROW_BYTES) - 1);
                self.memory_byte = (addr & (u32::from(ROW_BYTES) - 1)) as u8;
                self.typing = None;
                let message = format!("address {}", format_addr(addr, digits));
                self.message = Some(message.clone());
                Action::Notice(message)
            }
            None => {
                let message = format!("not a hex address: {text}");
                self.message = Some(message.clone());
                Action::Notice(message)
            }
        }
    }

    /// `B`: the current PC onto the watch list.
    fn watch(&mut self, pc: u32) -> Action {
        let at = format_addr(pc, self.console.digits());
        let message = if self.watches.add(pc) {
            format!("watch {at} added ({})", self.watches.len())
        } else if self.watches.contains(pc) {
            format!("watch {at} already watched")
        } else {
            format!("watch list full ({WATCH_LIMIT})")
        };
        self.message = Some(message.clone());
        Action::Notice(message)
    }

    /// `D`: the whole list, per "D clears it".
    fn clear_watches(&mut self) -> Action {
        let message = if self.watches.is_empty() {
            "no watches to clear".to_string()
        } else {
            let n = self.watches.len();
            self.watches.clear();
            format!("watches cleared ({n})")
        };
        self.message = Some(message.clone());
        Action::Notice(message)
    }
}

/// One row of the memory view as hex bytes: `00 3E C3 12 ...`, sixteen pairs.
pub fn hex_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The same row as printable ASCII, `.` for everything outside `$20-$7E`.
pub fn ascii_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            if (0x20..=0x7E).contains(b) {
                char::from(*b)
            } else {
                '.'
            }
        })
        .collect()
}

/// The panel's own colour for a shade: the four shades as the DMG's green, light to dark, so a
/// viewer reads the same way the picture does whatever palette the game is drawn in. The
/// [`TRANSPARENT`] marker is the panel's own background, which is how a gap or an empty sprite
/// pixel disappears into the panel.
pub fn panel_shade(shade: u8) -> [u8; 4] {
    if shade == TRANSPARENT {
        return PANEL_BG;
    }
    match shade & 3 {
        0 => [0x9B, 0xBC, 0x0F, 0xFF],
        1 => [0x8B, 0xAC, 0x0F, 0xFF],
        2 => [0x30, 0x62, 0x30, 0xFF],
        _ => [0x0F, 0x38, 0x0F, 0xFF],
    }
}

/// The panel's background, the colour a viewer's gaps and transparent pixels become:
/// `main.rs`'s `DRAWER_BG` in bytes, so a texture's gaps and the rectangle it sits on are one
/// colour.
pub const PANEL_BG: [u8; 4] = [0x0A, 0x0A, 0x10, 0xFF];
