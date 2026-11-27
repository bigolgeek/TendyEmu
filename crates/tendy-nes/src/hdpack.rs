//! HD pack: the Mesen `hires.txt` subset (`docs/hdpack-format.md`).
//!
//! Presentation only (§6.3): nothing in the emulation core reads a pack, so a pack cannot change a
//! frame's machine state. [`Pack::load`] turns a pack directory into images and rules; once per
//! presented frame [`Pack::compose`] walks that frame's own tile trace and copies each matched
//! replacement over the magnified 1x picture. The caller magnifies (the 1x RGBA buffer lives in
//! `src/main.rs`), so `compose` only ever writes the tiles it replaces.
//!
//! Grouping is per tile identity over the whole frame plane, not per 8x8 box: a tile crosses a
//! screen box whenever fine X or Y scroll is non-zero, and a per-box group would restart the
//! replacement art at each box edge. One group per identity also answers its conditions once
//! (`docs/hdpack-format.md` §3.4.2), at the first pixel of the identity in raster order.

use std::collections::HashMap;
use std::path::Path;

use crate::bus::SysBus;
use crate::cpu::Bus;
use crate::ppu::memory::{nametable_index, palette_index};
use crate::ppu::trace::{Layer, PixelTrace};
use crate::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// A load-time message for stderr, already formatted: a rejection, an ignored-line warning, or the
/// one-line summary of a loaded pack (§1.3, §1.4).
pub type Notice = String;

/// The `OAM index` a background group is keyed by, since a background pixel has none.
const BG_OAM: u8 = 0xFF;
/// The three conditions the format predefines as names (§3.4.3).
const BUILTINS: [&str; 3] = ["hmirror", "vmirror", "bgpriority"];
const HIRES: &str = "hires.txt";

/// What a rule keys on: the tile's identity and its palette bytes (§3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileId {
    /// A CHR-ROM pack: the tile's first byte offset in CHR ROM divided by 16 (§2.2 field 2).
    RomIndex(u32),
    /// A CHR RAM pack: the tile's 16 bytes in memory order.
    Ram([u8; 16]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub tile: TileId,
    /// The four bytes at `$3F00 + 4*palette`, in memory order; byte 0 is `FF` on a sprite rule.
    pub palette: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

/// The right-hand side of a memory condition: another address, or a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Right {
    Addr(u16),
    Const(u8),
}

/// One evaluated condition type (§3.4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hmirror,
    Vmirror,
    BgPriority,
    TileAt {
        x: u8,
        y: u8,
        key: Key,
    },
    SpriteAt {
        x: u8,
        y: u8,
        key: Key,
    },
    TileNear {
        dx: i8,
        dy: i8,
        key: Key,
    },
    SpriteNear {
        dx: i8,
        dy: i8,
        key: Key,
    },
    Mem {
        left: u16,
        op: Op,
        right: Right,
        mask: u8,
        ppu: bool,
    },
    FrameRange {
        divisor: u8,
        compare: u8,
    },
}

/// One term of a `[a&!b]` chain: a type, and whether the term is inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Condition {
    pub kind: Kind,
    pub not: bool,
}

/// One replacement rule: art `image` at (`x`, `y`), behind its conditions (empty = always true).
#[derive(Clone, Debug)]
pub struct Rule {
    pub image: u16,
    pub x: u16,
    pub y: u16,
    /// Per-channel multiplier (§9.4); 1.0 copies the art bytes.
    pub brightness: f32,
    /// `default` = Y: also the fallback for this tile data at every palette (§9.2.2).
    pub fallback: bool,
    pub conds: Vec<Condition>,
}

/// One `<background>` rule (§9.1): an image with no key, behind its conditions (empty = always true).
#[derive(Clone, Debug)]
pub struct Background {
    pub image: u16,
    pub brightness: f32,
    /// Signed PNG-pixel offsets: art pixel (`x + left`, `y + top`) draws at screen (`x`, `y`) (§9.1.1).
    pub left: i16,
    pub top: i16,
    /// `showBehindBackgroundPrioritySprites` = Y: the image covers a background-priority sprite.
    pub show_behind: bool,
    pub conds: Vec<Condition>,
}

/// One decoded `<img>`.
pub struct Image {
    pub width: u16,
    pub height: u16,
    /// 8-bit RGBA, row-major.
    pub rgba: Vec<u8>,
}

/// A loaded pack: what a `<tile>` rule is matched against, and the art to draw.
pub struct Pack {
    pub scale: u8,
    pub images: Vec<Image>,
    /// Rules per key, in file order; the first whose conditions hold wins (§3.7).
    rules: HashMap<Key, Vec<Rule>>,
    /// The `default=Y` rule per tile data, drew only where a key has no rule at all (§9.2.2).
    fallbacks: HashMap<TileId, Rule>,
    /// `<background>` rules in file order, drawn back to front (§9.1.3).
    backgrounds: Vec<Background>,
    /// Accepted `<tile>` rules, for the startup line.
    pub rules_total: usize,
    /// Accepted `<background>` rules, for the startup line.
    pub backgrounds_total: usize,
    /// Lines and rules the subset ignored, for the startup line.
    pub ignored: usize,
}

impl Pack {
    /// Read `dir/hires.txt` and the PNGs it names. `Err` is never returned: a pack that cannot be
    /// drawn at all comes back as `(None, [<reason>])` (§1.4), the rest as warnings beside the pack.
    pub fn load(dir: &Path) -> (Option<Pack>, Vec<Notice>) {
        match load_pack(dir) {
            Ok(loaded) => (Some(loaded.0), loaded.1),
            Err(message) => (None, vec![message]),
        }
    }

    /// Draw every matched replacement over `out`, which already holds the `scale`x magnified 1x
    /// picture: `scale*256` x `scale*240` RGBA. Reads the frame's tile trace, the palette RAM and
    /// the board through side-effect-free peeks only; writes nothing but `out`.
    pub fn compose(&self, bus: &mut SysBus, out: &mut [u8]) {
        let scale = usize::from(self.scale);
        let wide = SCREEN_WIDTH * scale;
        if out.len() != wide * SCREEN_HEIGHT * scale * 4 || bus.ppu.tile_trace().is_none() {
            return;
        }
        let groups = groups_of(bus);
        if !groups.is_empty() {
            let chosen: Vec<Option<&Rule>> = groups
                .iter()
                .map(|g| {
                    let key = self.group_key(bus, &g.id);
                    self.first_rule(bus, &key, g.anchor, g.attr)
                })
                .collect();
            self.draw_groups(bus, out, &groups, &chosen, wide, scale);
        }
        self.draw_backgrounds(bus, out, wide, scale);
    }

    /// Copy each group's art into its own pixels (§5.4). Pixels the other layer won, the backdrop,
    /// and anything unmatched keep the magnified 1x art they came in with.
    fn draw_groups(
        &self,
        bus: &SysBus,
        out: &mut [u8],
        groups: &[Group],
        chosen: &[Option<&Rule>],
        wide: usize,
        scale: usize,
    ) {
        let Some(plane) = bus.ppu.tile_trace() else {
            return;
        };
        let mut index: HashMap<GroupId, usize> = HashMap::with_capacity(groups.len());
        for (g, group) in groups.iter().enumerate() {
            index.insert(group.id, g);
        }
        let mut last: Option<(GroupId, usize)> = None;
        for (i, e) in plane.iter().enumerate() {
            let id = match e.layer {
                Layer::Background => bg_id(e),
                Layer::Sprite(oam) => winner_id(e, oam),
                _ => continue,
            };
            if last.map(|(l, _)| l) != Some(id) {
                match index.get(&id) {
                    Some(&g) => last = Some((id, g)),
                    None => {
                        last = None;
                        continue;
                    }
                }
            }
            let Some((_, g)) = last else {
                continue;
            };
            let Some(rule) = chosen[g] else {
                continue;
            };
            self.draw_pixel(
                out,
                rule,
                groups[g].anchor,
                groups[g].attr,
                i % SCREEN_WIDTH,
                i / SCREEN_WIDTH,
                wide,
                scale,
            );
        }
    }

    /// One replaced pixel: the art cell the screen pixel falls in, mirrored by the flip bits (§3.5).
    #[allow(clippy::too_many_arguments)]
    fn draw_pixel(
        &self,
        out: &mut [u8],
        rule: &Rule,
        anchor: (u16, u16),
        attr: u8,
        x: usize,
        y: usize,
        wide: usize,
        scale: usize,
    ) {
        let mut cx = (x + 8 - usize::from(anchor.0) % 8) % 8;
        let mut cy = (y + 8 - usize::from(anchor.1) % 8) % 8;
        if attr & 0x40 != 0 {
            cx = 7 - cx;
        }
        if attr & 0x80 != 0 {
            cy = 7 - cy;
        }
        let Some(img) = self.images.get(usize::from(rule.image)) else {
            return;
        };
        let px0 = usize::from(rule.x) + cx * scale;
        let py0 = usize::from(rule.y) + cy * scale;
        let row = usize::from(img.width);
        for sy in 0..scale {
            for sx in 0..scale {
                let src = ((py0 + sy) * row + px0 + sx) * 4;
                // Alpha is a mask, not a blend (§2.2): opaque at 128 and above, transparent below.
                if img.rgba.get(src + 3).is_none_or(|a| *a < 128) {
                    continue;
                }
                let px = shade(
                    [
                        img.rgba[src],
                        img.rgba[src + 1],
                        img.rgba[src + 2],
                        img.rgba[src + 3],
                    ],
                    rule.brightness,
                );
                let dst = ((y * scale + sy) * wide + x * scale + sx) * 4;
                out[dst..dst + 4].copy_from_slice(&px);
            }
        }
    }

    /// Each matching `<background>` image over the backdrop holes, in file order (§9.1.3, §9.3).
    /// Returns before touching the trace when the pack has no `<background>` line (§9.6 pin 1).
    fn draw_backgrounds(&self, bus: &mut SysBus, out: &mut [u8], wide: usize, scale: usize) {
        if self.backgrounds.is_empty() {
            return;
        }
        // Conditions are answered once per rule per frame, at (0, 0) with attr 0 (§9.1.3, D2).
        let shown: Vec<&Background> = self
            .backgrounds
            .iter()
            .filter(|bg| bg.conds.iter().all(|c| self.holds(bus, c, (0, 0), 0)))
            .collect();
        // D6: rules draw back to front, so the nearest matched rule's flag decides for all of them.
        let Some(show_behind) = shown.last().map(|bg| bg.show_behind) else {
            return;
        };
        let Some(plane) = bus.ppu.tile_trace() else {
            return;
        };
        for bg in shown {
            let Some(img) = self.images.get(usize::from(bg.image)) else {
                continue;
            };
            // Image pixels are frame pixels (§9.1.1): art (sx, sy) draws at (sx - left, sy - top).
            let (left, top) = (i32::from(bg.left), i32::from(bg.top));
            let (w, h) = (i32::from(img.width), i32::from(img.height));
            let x_to = w.min(SCREEN_WIDTH as i32 + left);
            let y_to = h.min(SCREEN_HEIGHT as i32 + top);
            for sy in 0.max(top)..y_to {
                let y = (sy - top) as usize;
                for sx in 0.max(left)..x_to {
                    let x = (sx - left) as usize;
                    if !shows_through(plane[y * SCREEN_WIDTH + x], show_behind) {
                        continue;
                    }
                    let src = (sy * w + sx) as usize * 4;
                    let a = img.rgba[src + 3];
                    if a < 128 {
                        continue;
                    }
                    let px = shade(
                        [img.rgba[src], img.rgba[src + 1], img.rgba[src + 2], a],
                        bg.brightness,
                    );
                    for py in 0..scale {
                        for pxx in 0..scale {
                            let dst = ((y * scale + py) * wide + x * scale + pxx) * 4;
                            out[dst..dst + 4].copy_from_slice(&px);
                        }
                    }
                }
            }
        }
    }

    /// The key a group's pixels carry: the tile's identity and the palette bytes of §3.3.
    fn group_key(&self, bus: &SysBus, id: &GroupId) -> Key {
        let tile = match id.rom {
            Some(index) => TileId::RomIndex(index),
            None => {
                // §4 gap 3: CHR RAM bytes come from the live, side-effect-free peek at composite
                // time, which is exact for a game that rewrites CHR in VBlank.
                let mut bytes = [0u8; 16];
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = bus.cart().chr_peek(id.base.wrapping_add(i as u16) & 0x1FFF);
                }
                TileId::Ram(bytes)
            }
        };
        let p = u16::from(id.palette);
        let mut palette = [0u8; 4];
        for (i, b) in palette.iter_mut().enumerate() {
            *b = bus.ppu.palette[palette_index(0x3F00 + 4 * p + i as u16)] & 0x3F;
        }
        if p >= 4 {
            // A sprite key never needs entry 0: it mirrors the backdrop (§3.3).
            palette[0] = 0xFF;
        }
        Key { tile, palette }
    }

    /// The first rule for `key` whose conditions all hold, in file order (§3.7); on a key with no
    /// rule at all, its tile data's `default=Y` rule (§9.2.2, decision D5).
    fn first_rule(
        &self,
        bus: &mut SysBus,
        key: &Key,
        anchor: (u16, u16),
        attr: u8,
    ) -> Option<&Rule> {
        if let Some(rules) = self.rules.get(key) {
            return rules
                .iter()
                .find(|rule| rule.conds.iter().all(|c| self.holds(bus, c, anchor, attr)));
        }
        let rule = self.fallbacks.get(&key.tile)?;
        rule.conds
            .iter()
            .all(|c| self.holds(bus, c, anchor, attr))
            .then_some(rule)
    }

    fn holds(&self, bus: &mut SysBus, c: &Condition, anchor: (u16, u16), attr: u8) -> bool {
        let value = match c.kind {
            // §3.4.3: properties of the sprite, read from the attribute byte the fetch recorded.
            // All three are false on a background rule, whose `attr` is 0.
            Kind::Hmirror => attr & 0x40 != 0,
            Kind::Vmirror => attr & 0x80 != 0,
            Kind::BgPriority => attr & 0x20 != 0,
            Kind::TileAt { x, y, key } => self.bg_key(bus, u16::from(x), u16::from(y)) == Some(key),
            Kind::SpriteAt { x, y, key } => {
                self.sprite_key(bus, u16::from(x), u16::from(y)) == Some(key)
            }
            Kind::TileNear { dx, dy, key } => match offset(anchor, dx, dy) {
                Some((x, y)) => self.bg_key(bus, x, y) == Some(key),
                None => false,
            },
            Kind::SpriteNear { dx, dy, key } => match offset(anchor, dx, dy) {
                Some((x, y)) => self.sprite_key(bus, x, y) == Some(key),
                None => false,
            },
            Kind::Mem {
                left,
                op,
                right,
                mask,
                ppu,
            } => {
                let a = if ppu {
                    Some(ppu_peek(bus, left))
                } else {
                    bus.peek(left)
                };
                let b = match right {
                    Right::Addr(addr) => {
                        if ppu {
                            Some(ppu_peek(bus, addr))
                        } else {
                            bus.peek(addr)
                        }
                    }
                    Right::Const(value) => Some(value),
                };
                match (a, b) {
                    (Some(a), Some(b)) => {
                        // The mask keeps its bits on both sides; the constant is compared as written.
                        let b = if matches!(right, Right::Addr(_)) {
                            b & mask
                        } else {
                            b
                        };
                        compare(a & mask, op, b)
                    }
                    _ => false,
                }
            }
            Kind::FrameRange {
                divisor,
                compare: from,
            } => bus.ppu.frame % u64::from(divisor) >= u64::from(from),
        };
        value != c.not
    }

    /// The background tile's key at a screen pixel, whatever layer won it, or `None` where the
    /// background layer drew nothing (§4 gap 4).
    fn bg_key(&self, bus: &SysBus, x: u16, y: u16) -> Option<Key> {
        let e = *bus
            .ppu
            .tile_trace()?
            .get(usize::from(y) * SCREEN_WIDTH + usize::from(x))?;
        if e.bg_palette >= 4 {
            return None;
        }
        Some(self.group_key(bus, &bg_id(&e)))
    }

    /// The sprite tile's key at a screen pixel, and only where a sprite won it (§3.4.2).
    fn sprite_key(&self, bus: &SysBus, x: u16, y: u16) -> Option<Key> {
        let e = *bus
            .ppu
            .tile_trace()?
            .get(usize::from(y) * SCREEN_WIDTH + usize::from(x))?;
        let oam = match e.layer {
            Layer::Sprite(oam) => oam,
            _ => return None,
        };
        Some(self.group_key(bus, &winner_id(&e, oam)))
    }
}

/// One tile identity's group: what to key on, where its conditions are answered, and the sprite
/// attribute byte (0 for the background).
#[derive(Clone, Copy)]
struct Group {
    id: GroupId,
    anchor: (u16, u16),
    attr: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GroupId {
    /// The OAM index of the sprite, or [`BG_OAM`] for a background tile.
    oam: u8,
    /// The tile's first pattern-table address, low four bits cleared (§3.2).
    base: u16,
    palette: u8,
    /// The tile's CHR-ROM index at the fetch (`offset / 16`), when the board has CHR ROM.
    rom: Option<u32>,
}

/// The group a pixel's background layer belongs to (§4 gap 4). The trace records the *row* address
/// and offset, so both are reduced to the tile's.
fn bg_id(e: &PixelTrace) -> GroupId {
    GroupId {
        oam: BG_OAM,
        base: e.bg_chr_addr & 0xFFF0,
        palette: e.bg_palette,
        rom: e.bg_chr_rom.map(|off| off / 16),
    }
}

/// The group a sprite pixel belongs to: the sprite that won it, and the tile it fetched.
fn winner_id(e: &PixelTrace, oam: u8) -> GroupId {
    GroupId {
        oam,
        base: e.chr_addr & 0xFFF0,
        palette: e.palette,
        rom: e.chr_rom.map(|off| off / 16),
    }
}

/// The distinct tile identities of the frame, each with the anchor its conditions are answered at:
/// the first of its own pixels in raster order (§3.4.2). A background tile names itself on every
/// entry the layer drew (§4 gap 4), so a tile under a sprite, or one transparent at its anchor,
/// still gets its own group.
fn groups_of(bus: &SysBus) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut seen: HashMap<GroupId, ()> = HashMap::new();
    let Some(plane) = bus.ppu.tile_trace() else {
        return groups;
    };
    for (i, e) in plane.iter().enumerate() {
        let anchor = ((i % SCREEN_WIDTH) as u16, (i / SCREEN_WIDTH) as u16);
        if e.bg_palette < 4 {
            let id = bg_id(e);
            if seen.insert(id, ()).is_none() {
                groups.push(Group {
                    id,
                    anchor,
                    attr: 0,
                });
            }
        }
        if let Layer::Sprite(oam) = e.layer {
            let id = winner_id(e, oam);
            if seen.insert(id, ()).is_none() {
                groups.push(Group {
                    id,
                    anchor: sprite_anchor(&bus.ppu.oam, oam, anchor.1),
                    attr: e.attr,
                });
            }
        }
    }
    groups
}

/// The anchor a sprite group is answered and drawn from (§3.4.2): the box's top-left — OAM x, one
/// line below OAM y — never the first pixel the sprite won, so a transparent corner cannot shift
/// its art. An 8x16 sprite's second tile is the box's lower half: `first_row`, the group's first
/// visible line, says which half it is. Clamped where the screen clips it.
fn sprite_anchor(oam: &[u8; 256], index: u8, first_row: u16) -> (u16, u16) {
    let i = usize::from(index) * 4;
    let top = u16::from(oam[i]) + 1;
    let y = if first_row >= top + 8 { top + 8 } else { top };
    (
        u16::from(oam[i + 3]).min(SCREEN_WIDTH as u16 - 1),
        y.min(SCREEN_HEIGHT as u16 - 1),
    )
}

/// `anchor + (dx, dy)`, or `None` off the 256x240 plane (§3.4.2: false, not an error).
fn offset(anchor: (u16, u16), dx: i8, dy: i8) -> Option<(u16, u16)> {
    let x = i32::from(anchor.0) + i32::from(dx);
    let y = i32::from(anchor.1) + i32::from(dy);
    if !(0..SCREEN_WIDTH as i32).contains(&x) || !(0..SCREEN_HEIGHT as i32).contains(&y) {
        return None;
    }
    Some((x as u16, y as u16))
}

/// The PPU byte at `addr` through the `&self` peeks (§3.4.4): CHR, nametables (the board's own
/// view first, then the plain CIRAM mapping), and palette RAM.
fn ppu_peek(bus: &SysBus, addr: u16) -> u8 {
    let a = addr & 0x3FFF;
    match a {
        0x0000..=0x1FFF => bus.cart().chr_peek(a),
        0x2000..=0x3EFF => bus
            .cart()
            .nt_peek(a, &bus.ppu.vram)
            .unwrap_or_else(|| bus.ppu.vram[nametable_index(a, bus.cart().mirroring())]),
        _ => bus.ppu.palette[palette_index(a)] & 0x3F,
    }
}

/// §9.4: per channel, multiply, round half away from zero, clamp to `0..=255`; alpha untouched —
/// scaling it would fade a replacement out and show the 1x art behind it.
fn shade(px: [u8; 4], b: f32) -> [u8; 4] {
    if b == 1.0 {
        return px;
    }
    let scaled = |c: u8| (c as f32 * b).round().clamp(0.0, 255.0) as u8;
    [scaled(px[0]), scaled(px[1]), scaled(px[2]), px[3]]
}

/// Whether a background image's pixel shows through the trace entry (§9.3): only into a backdrop
/// hole, and over a background-priority sprite unless the rule put itself behind it.
fn shows_through(e: PixelTrace, show_behind: bool) -> bool {
    match e.layer {
        // The NES drew no background tile there: the hole the image fills.
        Layer::Backdrop => true,
        // A background-priority sprite sits in front of the backdrop; the flag is whether it is
        // also in front of the image.
        Layer::Sprite(_) => e.attr & 0x20 != 0 && !show_behind,
        Layer::Background | Layer::None => false,
    }
}

fn compare(a: u8, op: Op, b: u8) -> bool {
    match op {
        Op::Eq => a == b,
        Op::Ne => a != b,
        Op::Gt => a > b,
        Op::Lt => a < b,
        Op::Ge => a >= b,
        Op::Le => a <= b,
    }
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

/// The lines a pack is read into before any rule is resolved: `<img>` indexes and condition names
/// may both come after the rules that use them (§2.1, §3.4).
struct Loader {
    path: std::path::PathBuf,
    dir: std::path::PathBuf,
    ver: Option<u16>,
    scale: Option<u8>,
    image_names: Vec<(String, usize, bool)>,
    /// `<tile>` lines, kept in file order: line number, condition prefix, payload.
    tiles: Vec<(usize, String, String)>,
    /// `<background>` lines, kept in file order: line number, condition prefix, payload.
    backgrounds: Vec<(usize, String, String)>,
    /// Declared conditions: name and evaluated type.
    conds: Vec<(String, Kind)>,
    /// Ignored-line classes: reason, first line, count.
    warnings: Vec<(String, usize, usize)>,
    ignored: usize,
    seen_line: bool,
}

/// What the `<tile>` lines resolved to: rules per key, the `default=Y` rule per tile data, and the
/// count of accepted rules (§9.5 item 7).
type TileRules = (HashMap<Key, Vec<Rule>>, HashMap<TileId, Rule>, usize);

/// The summary line plus the per-class warnings of a loaded pack (§1.3).
fn load_pack(dir: &Path) -> Result<(Pack, Vec<Notice>), String> {
    let mut l = Loader {
        path: dir.join(HIRES),
        dir: dir.to_path_buf(),
        ver: None,
        scale: None,
        image_names: Vec::new(),
        tiles: Vec::new(),
        backgrounds: Vec::new(),
        conds: Vec::new(),
        warnings: Vec::new(),
        ignored: 0,
        seen_line: false,
    };
    l.scan()?;
    let ver = l
        .ver
        .ok_or_else(|| reject(&l.path, 1, "<ver> is missing"))?;
    let scale = l
        .scale
        .ok_or_else(|| reject(&l.path, 1, "<scale> is missing"))?;
    l.append_background_images();
    let images = l.decode_images()?;
    let (rules, fallbacks, rules_total) = l.build_rules(ver, &images)?;
    let (backgrounds, backgrounds_total) = l.build_backgrounds(ver, &images)?;
    let pack = Pack {
        scale,
        images,
        rules,
        fallbacks,
        backgrounds,
        rules_total,
        backgrounds_total,
        ignored: l.ignored,
    };
    let summary = match backgrounds_total {
        0 => format!(
            "hdpack: {} rules, scale {scale}x, {} ignored",
            pack.rules_total, l.ignored
        ),
        n => format!(
            "hdpack: {} rules, {n} backgrounds, scale {scale}x, {} ignored",
            pack.rules_total, l.ignored
        ),
    };
    let mut notices = vec![summary];
    l.warnings.sort_by_key(|(_, line, _)| *line);
    for (reason, line, count) in &l.warnings {
        if *count > 1 {
            notices.push(format!("hdpack: line {line}: {reason} ({count} lines)"));
        } else {
            notices.push(format!("hdpack: line {line}: {reason}"));
        }
    }
    Ok((pack, notices))
}

fn reject(path: &Path, line: usize, why: &str) -> String {
    format!(
        "hdpack: {}: line {line}: {why} (no pack loaded)",
        path.display()
    )
}

impl Loader {
    /// One pass over the file: reject what cannot be drawn, keep the rest.
    fn scan(&mut self) -> Result<(), String> {
        let text = std::fs::read(&self.path)
            .map_err(|e| format!("hdpack: {}: {e} (no pack loaded)", self.path.display()))?;
        let text = String::from_utf8_lossy(&text);
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            if raw.trim().is_empty() {
                continue;
            }
            let first = !self.seen_line;
            self.seen_line = true;
            let (prefix, rest) = match raw.strip_prefix('[') {
                Some(after) => match after.find(']') {
                    Some(i) => (&after[..i], &after[i + 1..]),
                    None => {
                        self.ignore(line, "a '[' with no ']'");
                        continue;
                    }
                },
                None => ("", raw),
            };
            let Some(after) = rest.strip_prefix('<') else {
                self.ignore(line, "no <tag> on the line");
                continue;
            };
            let Some(end) = after.find('>') else {
                self.ignore(line, "a <tag> with no '>'");
                continue;
            };
            let (tag, payload) = (&after[..end], after[end + 1..].trim());
            if first && (tag != "ver" || !prefix.is_empty()) {
                return Err(reject(
                    &self.path,
                    line,
                    "the first non-blank line is not <ver>",
                ));
            }
            match tag {
                "ver" => {
                    if !prefix.is_empty() {
                        self.ignore(line, "a [...] prefix on <ver>");
                        continue;
                    }
                    if self.ver.is_some() {
                        return Err(reject(&self.path, line, "<ver> is repeated"));
                    }
                    let Some(v) = dec(payload) else {
                        return Err(reject(&self.path, line, "<ver> is not a number"));
                    };
                    if !(103..=105).contains(&v) {
                        return Err(reject(
                            &self.path,
                            line,
                            &format!("<ver> {v} is outside 103..=105"),
                        ));
                    }
                    self.ver = Some(v as u16);
                }
                "scale" => {
                    if !prefix.is_empty() {
                        self.ignore(line, "a [...] prefix on <scale>");
                        continue;
                    }
                    if self.scale.is_some() {
                        return Err(reject(&self.path, line, "<scale> is repeated"));
                    }
                    let Some(v) = dec(payload) else {
                        return Err(reject(&self.path, line, "<scale> is not a number"));
                    };
                    if !(1..=8).contains(&v) {
                        return Err(reject(
                            &self.path,
                            line,
                            &format!("<scale> {v} is outside 1..=8"),
                        ));
                    }
                    self.scale = Some(v as u8);
                }
                "img" => {
                    if !prefix.is_empty() {
                        self.ignore(line, "a [...] prefix on <img>");
                        continue;
                    }
                    if payload.is_empty() {
                        return Err(reject(&self.path, line, "<img> names no file"));
                    }
                    if payload.contains(['/', '\\']) {
                        return Err(reject(
                            &self.path,
                            line,
                            &format!("<img> {payload}: a pack cannot read outside its own folder"),
                        ));
                    }
                    self.image_names.push((payload.to_string(), line, false));
                }
                "tile" => self
                    .tiles
                    .push((line, prefix.to_string(), payload.to_string())),
                "background" => {
                    let name = payload.split(',').next().unwrap_or("").trim();
                    if name.is_empty() {
                        return Err(reject(&self.path, line, "<background> names no file"));
                    }
                    if name.contains(['/', '\\']) {
                        return Err(reject(
                            &self.path,
                            line,
                            &format!(
                                "<background> {name}: a pack cannot read outside its own folder"
                            ),
                        ));
                    }
                    self.backgrounds
                        .push((line, prefix.to_string(), payload.to_string()));
                }
                "condition" => {
                    if !prefix.is_empty() {
                        self.ignore(line, "a [...] prefix on <condition>");
                        continue;
                    }
                    self.condition(line, payload);
                }
                "overscan" | "patch" | "options" | "bgm" | "sfx" => {
                    if !prefix.is_empty() {
                        self.ignore(
                            line,
                            "a [...] prefix on a tag that is not <tile> or <background>",
                        );
                        continue;
                    }
                    self.ignore(line, &format!("<{tag}> is outside this subset"));
                }
                _ => return Err(reject(&self.path, line, &format!("unknown tag <{tag}>"))),
            }
        }
        Ok(())
    }

    /// A file only a `<background>` line names joins the image list after the last `<img>`, so a
    /// `<tile>` img index never moves and a file both tags name is decoded once (§9.1.1).
    fn append_background_images(&mut self) {
        for (line, _, payload) in &self.backgrounds {
            let name = payload.split(',').next().unwrap_or("").trim();
            if !self.image_names.iter().any(|(n, _, _)| n == name) {
                self.image_names.push((name.to_string(), *line, true));
            }
        }
    }

    fn decode_images(&self) -> Result<Vec<Image>, String> {
        let mut images = Vec::with_capacity(self.image_names.len());
        for (name, line, background) in &self.image_names {
            let path = self.dir.join(name);
            match decode_png(&path) {
                Ok(image) => images.push(image),
                Err(why) => {
                    let tag = if *background { "background" } else { "img" };
                    return Err(reject(&self.path, *line, &format!("<{tag}> {name}: {why}")));
                }
            }
        }
        Ok(images)
    }

    /// The image index a `<background>` name resolves to, by the list `decode_images` filled.
    fn image_index(&self, name: &str) -> Option<u16> {
        self.image_names
            .iter()
            .position(|(n, _, _)| n == name)
            .and_then(|i| u16::try_from(i).ok())
    }

    /// One `<condition>` declaration (§3.4.1). A line that does not parse is ignored, and so is
    /// every rule that names it.
    fn condition(&mut self, line: usize, payload: &str) {
        let f: Vec<&str> = payload.split(',').map(str::trim).collect();
        let name = f[0];
        if name.is_empty() || name.contains('&') || name.starts_with('!') {
            self.ignore(line, "<condition> has no usable name");
            return;
        }
        if BUILTINS.contains(&name) {
            self.ignore(line, "<condition> redefines a built-in condition");
            return;
        }
        if self.conds.iter().any(|(n, _)| n == name) {
            self.ignore(line, "<condition> repeats a name");
            return;
        }
        let ver = self.ver.unwrap_or(104);
        let Some(ty) = f.get(1) else {
            self.ignore(line, "<condition> has no type");
            return;
        };
        match kind_of(ty, &f[2..], ver) {
            Some(kind) => self.conds.push((name.to_string(), kind)),
            None => self.ignore(
                line,
                &format!("<condition> {name},{ty}: malformed or unknown type"),
            ),
        }
    }

    /// Resolve every `<tile>` line into a rule (§2.2), with its condition terms (§3.4).
    fn build_rules(&mut self, ver: u16, images: &[Image]) -> Result<TileRules, String> {
        let mut rules: HashMap<Key, Vec<Rule>> = HashMap::new();
        let mut fallbacks: HashMap<TileId, Rule> = HashMap::new();
        let mut total = 0;
        for (line, prefix, payload) in std::mem::take(&mut self.tiles) {
            let Some((key, rule)) = self.tile_rule(line, &prefix, &payload, ver, images)? else {
                continue;
            };
            if matches!(rules.get(&key), Some(list) if list.iter().any(|r| r.conds.is_empty())) {
                // §3.7: an unconditional rule is the fallback, so a later rule for that key is dead.
                self.ignore(line, "a rule for a key an unconditional rule already took");
                continue;
            }
            if rule.fallback {
                // §9.2.2: the first `Y` for a tile data wins; a second one is dead, like §3.7.
                if fallbacks.contains_key(&key.tile) {
                    self.ignore(
                        line,
                        "a second default=Y rule for a tile data the first one claimed",
                    );
                    continue;
                }
                fallbacks.insert(key.tile, rule.clone());
            }
            rules.entry(key).or_default().push(rule);
            total += 1;
        }
        Ok((rules, fallbacks, total))
    }

    /// Resolve every `<background>` line into a rule (§9.1), with its condition terms (§3.4).
    fn build_backgrounds(
        &mut self,
        ver: u16,
        images: &[Image],
    ) -> Result<(Vec<Background>, usize), String> {
        let mut backgrounds = Vec::new();
        for (line, prefix, payload) in std::mem::take(&mut self.backgrounds) {
            match self.background_rule(line, &prefix, &payload, ver, images) {
                Ok(Some(rule)) => backgrounds.push(rule),
                Ok(None) => {}
                Err(why) => return Err(why),
            }
        }
        let total = backgrounds.len();
        Ok((backgrounds, total))
    }

    fn tile_rule(
        &mut self,
        line: usize,
        prefix: &str,
        payload: &str,
        ver: u16,
        images: &[Image],
    ) -> Result<Option<(Key, Rule)>, String> {
        let Some(conds) = self.terms(line, prefix) else {
            return Ok(None);
        };
        let f: Vec<&str> = payload.split(',').map(str::trim).collect();
        if !(5..=7).contains(&f.len()) {
            self.ignore(line, "<tile> does not have 5 to 7 fields");
            return Ok(None);
        }
        let Some(image) = dec(f[0]).and_then(|i| u16::try_from(i).ok()) else {
            self.ignore(line, "<tile> img is not an index");
            return Ok(None);
        };
        if usize::from(image) >= images.len() {
            return Err(reject(
                &self.path,
                line,
                &format!("<tile> img {image} names no <img>"),
            ));
        }
        let Some(tile) = tile_id(f[1], ver, false) else {
            self.ignore(
                line,
                "<tile> tile data is neither 32 hex chars nor a 1-4 digit index",
            );
            return Ok(None);
        };
        let Some(palette) = palette_bytes(f[2]) else {
            self.ignore(line, "<tile> palette data is not 8 hex chars");
            return Ok(None);
        };
        let (Some(x), Some(y)) = (
            dec(f[3]).and_then(|v| u16::try_from(v).ok()),
            dec(f[4]).and_then(|v| u16::try_from(v).ok()),
        ) else {
            self.ignore(line, "<tile> x or y is not a number");
            return Ok(None);
        };
        let brightness = match brightness(ver, f.get(5).copied()) {
            Ok(b) => b,
            Err(why) => {
                self.ignore(line, &format!("<tile> {why}"));
                return Ok(None);
            }
        };
        let fallback = match f.get(6).copied() {
            None | Some("") | Some("N") => false,
            Some("Y") => true,
            Some(_) => {
                self.ignore(line, "<tile> default is neither Y nor N");
                return Ok(None);
            }
        };
        let side = u16::from(self.scale.unwrap_or(1)) * 8;
        let img = &images[usize::from(image)];
        if x.checked_add(side).is_none_or(|right| right > img.width)
            || y.checked_add(side).is_none_or(|bottom| bottom > img.height)
        {
            self.ignore(line, "<tile> x/y reach past the image edge");
            return Ok(None);
        }
        let key = Key { tile, palette };
        Ok(Some((
            key,
            Rule {
                image,
                x,
                y,
                brightness,
                fallback,
                conds,
            },
        )))
    }

    /// One `<background>` line (§9.1.1): a file name and six optional fields, behind conditions.
    fn background_rule(
        &mut self,
        line: usize,
        prefix: &str,
        payload: &str,
        ver: u16,
        images: &[Image],
    ) -> Result<Option<Background>, String> {
        let Some(conds) = self.terms(line, prefix) else {
            return Ok(None);
        };
        let f: Vec<&str> = payload.split(',').map(str::trim).collect();
        if f.len() > 7 {
            self.ignore(line, "<background> has more than 7 fields");
            return Ok(None);
        }
        // The name may have joined the image list at `append_background_images`, not yet decoded here.
        let Some(image) = self.image_index(f[0]) else {
            return Err(reject(
                &self.path,
                line,
                &format!("<background> {} names no file beside hires.txt", f[0]),
            ));
        };
        let brightness = match brightness(ver, f.get(1).copied()) {
            Ok(b) => b,
            Err(why) => {
                self.ignore(line, &format!("<background> {why}"));
                return Ok(None);
            }
        };
        // §9.1.2: a scroll ratio is relative to the NES scroll, which nothing records; only 0 is taken.
        for field in f.iter().skip(2).take(2) {
            if !field.is_empty() && !field.parse::<f64>().is_ok_and(|v| v == 0.0) {
                self.ignore(line, "<background> scroll ratio is not 0");
                return Ok(None);
            }
        }
        let show_behind = match f.get(4).copied() {
            None | Some("") | Some("N") => false,
            Some("Y") => true,
            Some(_) => {
                self.ignore(
                    line,
                    "<background> showBehindBackgroundPrioritySprites is neither Y nor N",
                );
                return Ok(None);
            }
        };
        let (Some(left), Some(top)) = (offset_px(f.get(5).copied()), offset_px(f.get(6).copied()))
        else {
            self.ignore(line, "<background> left or top is not a number");
            return Ok(None);
        };
        // §9.6 pin 2: past a whole frame at <scale>8 the image is ignored, not decoded into memory.
        let img = &images[usize::from(image)];
        if img.width > 8 * SCREEN_WIDTH as u16 || img.height > 8 * SCREEN_HEIGHT as u16 {
            self.ignore(line, "<background> image is past the 2048x1920 cap");
            return Ok(None);
        }
        Ok(Some(Background {
            image,
            brightness,
            left,
            top,
            show_behind,
            conds,
        }))
    }

    /// The `[a&!b]` terms of a rule (§3.4), resolved against the declared conditions.
    fn terms(&mut self, line: usize, prefix: &str) -> Option<Vec<Condition>> {
        if prefix.is_empty() {
            return Some(Vec::new());
        }
        if prefix.contains('[') {
            self.ignore(line, "a second [...] on the line");
            return None;
        }
        let mut conds = Vec::new();
        for term in prefix.split('&') {
            let (not, name) = match term.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, term),
            };
            if name.is_empty() || name.contains('!') {
                self.ignore(line, "a condition term names nothing");
                return None;
            }
            let kind = match name {
                "hmirror" => Kind::Hmirror,
                "vmirror" => Kind::Vmirror,
                "bgpriority" => Kind::BgPriority,
                _ => match self.conds.iter().find(|(n, _)| n == name) {
                    Some((_, kind)) => *kind,
                    None => {
                        self.ignore(line, "a rule names a condition no <condition> line defined");
                        return None;
                    }
                },
            };
            // §3.4.6: an I/O address has no side-effect-free read, so the rule is ignored.
            if let Kind::Mem {
                left,
                right,
                ppu: false,
                ..
            } = kind
                && (io_window(left) || matches!(right, Right::Addr(a) if io_window(a)))
            {
                self.ignore(line, "a memory condition reads the $2000-$5FFF I/O window");
                return None;
            }
            conds.push(Condition { kind, not });
        }
        Some(conds)
    }

    /// Count one ignored line or rule, and warn once per class (§1.3).
    fn ignore(&mut self, line: usize, reason: &str) {
        self.ignored += 1;
        self.warn(line, reason);
    }

    fn warn(&mut self, line: usize, reason: &str) {
        match self.warnings.iter_mut().find(|(r, _, _)| r == reason) {
            Some((_, _, count)) => *count += 1,
            None => self.warnings.push((reason.to_string(), line, 1)),
        }
    }
}

fn io_window(addr: u16) -> bool {
    (0x2000..=0x5FFF).contains(&addr)
}

/// `[0-9]+`.
fn dec(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `[0-9A-Fa-f]+`, no prefix, no separators.
fn hex(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 8 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// One byte of hex text, `[0-9A-Fa-f]{2}`: `u8::from_str_radix` would take a leading `+`,
/// and §1.2's form has no prefix and no sign.
fn hex_byte(s: &str) -> Option<u8> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u8::from_str_radix(s, 16).ok()
}

/// A signed decimal offset, `-128..=127` (§3.4.2).
fn signed(s: &str) -> Option<i8> {
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let v = dec(digits)?;
    let v = if negative {
        -(i64::from(v))
    } else {
        i64::from(v)
    };
    i8::try_from(v).ok()
}

/// A brightness field (§9.2.1, §9.4): absent or empty is 1.0; accepted when finite and `>= 0`;
/// above 1.0 only under `<ver>105`, the changelog's "now allowed".
fn brightness(ver: u16, field: Option<&str>) -> Result<f32, &'static str> {
    let Some(field) = field else {
        return Ok(1.0);
    };
    if field.is_empty() {
        return Ok(1.0);
    }
    let Ok(b) = field.parse::<f32>() else {
        return Err("brightness is not a number");
    };
    if !b.is_finite() || b < 0.0 {
        return Err("brightness is not a finite value >= 0");
    }
    if b > 1.0 && ver < 105 {
        return Err("brightness above 1.0 needs <ver>105");
    }
    Ok(b)
}

/// A signed decimal PNG-pixel offset (§9.1.1): absent or empty is 0.
fn offset_px(field: Option<&str>) -> Option<i16> {
    let Some(field) = field.filter(|s| !s.is_empty()) else {
        return Some(0);
    };
    let (negative, digits) = match field.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, field),
    };
    let v = i64::from(dec(digits)?);
    i16::try_from(if negative { -v } else { v }).ok()
}

/// A condition's `tile data` and `palette data` as a key (§2.2 fields 2 and 3, §3.4.1).
fn key_of(tile: &str, palette: &str, ver: u16) -> Option<Key> {
    Some(Key {
        tile: tile_id(tile, ver, true)?,
        palette: palette_bytes(palette)?,
    })
}

/// The tile identity a disk form names: 32 hex chars of CHR RAM bytes, or a CHR ROM tile index.
/// `condition` picks the index's base: `<tile>`'s own index has been hex since 103, a condition's
/// index is decimal under `<ver>103` and hex from 104 (§3.4.1).
fn tile_id(s: &str, ver: u16, condition: bool) -> Option<TileId> {
    if s.len() >= 32 {
        let mut bytes = [0u8; 16];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = hex_byte(s.get(i * 2..i * 2 + 2)?)?;
        }
        return Some(TileId::Ram(bytes));
    }
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let index = if condition && ver == 103 {
        dec(s)?
    } else {
        hex(s)?
    };
    Some(TileId::RomIndex(index))
}

fn palette_bytes(s: &str) -> Option<[u8; 4]> {
    if s.len() != 8 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = hex_byte(s.get(i * 2..i * 2 + 2)?)?;
    }
    Some(bytes)
}

/// One condition type and its fields (§3.4.1), or `None` when the line is malformed.
fn kind_of(ty: &str, f: &[&str], ver: u16) -> Option<Kind> {
    match ty {
        "hmirror" if f.is_empty() => Some(Kind::Hmirror),
        "vmirror" if f.is_empty() => Some(Kind::Vmirror),
        "bgpriority" if f.is_empty() => Some(Kind::BgPriority),
        "tileAtPosition" | "spriteAtPosition" => {
            if f.len() != 4 {
                return None;
            }
            let x = u8::try_from(dec(f[0])?).ok()?;
            let y = u8::try_from(dec(f[1])?).ok().filter(|y| *y <= 239)?;
            let key = key_of(f[2], f[3], ver)?;
            Some(if ty == "tileAtPosition" {
                Kind::TileAt { x, y, key }
            } else {
                Kind::SpriteAt { x, y, key }
            })
        }
        "tileNearby" | "spriteNearby" => {
            if f.len() != 4 {
                return None;
            }
            let (dx, dy) = (signed(f[0])?, signed(f[1])?);
            let key = key_of(f[2], f[3], ver)?;
            Some(if ty == "tileNearby" {
                Kind::TileNear { dx, dy, key }
            } else {
                Kind::SpriteNear { dx, dy, key }
            })
        }
        "memoryCheck" | "ppuMemoryCheck" | "memoryCheckConstant" | "ppuMemoryCheckConstant" => {
            if f.len() < 3 || f.len() > 4 {
                return None;
            }
            let ppu = ty.starts_with("ppu");
            let mask = match f.get(3) {
                None | Some(&"") => 0xFF,
                Some(m) if m.len() <= 2 => u8::try_from(hex(m)?).ok()?,
                Some(_) => return None,
            };
            let left = address(f[0], ppu)?;
            let right = if ty.ends_with("Constant") {
                Right::Const(u8::try_from(hex(f[2])?).ok()?)
            } else {
                Right::Addr(address(f[2], ppu)?)
            };
            Some(Kind::Mem {
                left,
                op: op_of(f[1])?,
                right,
                mask,
                ppu,
            })
        }
        "frameRange" => {
            if f.len() != 2 {
                return None;
            }
            let divisor = u8::try_from(dec(f[0])?).ok().filter(|d| *d >= 1)?;
            let compare = u8::try_from(dec(f[1])?).ok().filter(|c| *c < divisor)?;
            Some(Kind::FrameRange { divisor, compare })
        }
        _ => None,
    }
}

fn address(s: &str, ppu: bool) -> Option<u16> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let v = u16::try_from(hex(s)?).ok()?;
    Some(if ppu { v & 0x3FFF } else { v })
}

fn op_of(s: &str) -> Option<Op> {
    Some(match s {
        "==" => Op::Eq,
        "!=" => Op::Ne,
        ">" => Op::Gt,
        "<" => Op::Lt,
        ">=" => Op::Ge,
        "<=" => Op::Le,
        _ => return None,
    })
}

/// Decode one `<img>`: 8-bit RGBA, non-interlaced or interlaced, all five filters.
fn decode_png(path: &Path) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot read: {e}"))?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| format!("not a PNG: {e}"))?;
    // The header carries the size, so an image the 16-bit fields cannot hold is refused before
    // its pixels are decoded or allocated.
    let (Ok(width), Ok(height)) = (
        u16::try_from(reader.info().width),
        u16::try_from(reader.info().height),
    ) else {
        return Err(format!(
            "{}x{} is past 65535 a side",
            reader.info().width,
            reader.info().height
        ));
    };
    // A tile is a piece of one plane, and no plane at <scale>8 is larger than 2048x1920: refuse a
    // bigger declared size here, from the header, instead of letting `vec!` trust it. Same cap and
    // wording as the `<background>` path (§9.6 pin 2), which until now only saw decoded images.
    let cap = 8 * SCREEN_WIDTH as u64 * 8 * SCREEN_HEIGHT as u64;
    if u64::from(width) * u64::from(height) > cap {
        return Err(format!("{width}x{height} is past the 2048x1920 cap"));
    }
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut rgba)
        .map_err(|e| format!("cannot decode: {e}"))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "not 8-bit RGBA ({:?} {:?})",
            info.color_type, info.bit_depth
        ));
    }
    rgba.truncate(info.buffer_size());
    Ok(Image {
        width,
        height,
        rgba,
    })
}
