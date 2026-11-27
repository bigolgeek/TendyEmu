//! The HD-pack compositor (`docs/hdpack-format.md` §5.5, tests 1-8).

use std::path::{Path, PathBuf};

use tendy_nes::bus::SysBus;
use tendy_nes::hdpack::Pack;
use tendy_nes::mapper::load;
use tendy_nes::nes::Nes;
use tendy_nes::ppu::palette::{NES_PALETTE, rgba_table, to_rgba_pal};
use tendy_nes::{SCREEN_HEIGHT, SCREEN_WIDTH};

const W: usize = SCREEN_WIDTH;
const H: usize = SCREEN_HEIGHT;
const RED: [u8; 4] = [200, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 200, 255];
const GREEN: [u8; 4] = [0, 200, 0, 255];
const YELLOW: [u8; 4] = [200, 200, 0, 255];
/// The background key the standard scene draws: CHR-ROM tile `$12`, palette `0F 16 27 30`.
const BG_RULE: &str = "0,12,0F162730,0,0,1,N";
const BG_FALLBACK: &str = "1,12,0F162730,0,0,1,N";
/// The sprite key the standard scene draws: tile `$2E`, sprite palette `FF 16 36 0F`.
const SPR_RULE: &str = "0,2E,FF16360F,0,0,1,N";
const SPR_FALLBACK: &str = "1,2E,FF16360F,0,0,1,N";

// ---------------------------------------------------------------------------------------------
// The machine: NROM with one CHR-ROM bank, so a rule keys a CHR-ROM tile index.
// ---------------------------------------------------------------------------------------------

/// Tiles 0 (empty), `$0C`/`$12`/`$2E` (solid colour 1), `$13` (row 0 transparent, then colour 1)
/// and `$14` (row 0 and the left column transparent, then colour 1).
fn chr_rom_image() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let chr = 16 + 16 * 1024;
    v.resize(chr + 8 * 1024, 0);
    for tile in [0x0Cusize, 0x12, 0x2E] {
        for row in 0..8 {
            v[chr + tile * 16 + row] = 0xFF;
        }
    }
    for row in 1..8 {
        v[chr + 0x13 * 16 + row] = 0xFF;
    }
    // 0x7F keeps bit 7, the leftmost pixel, clear: the sprite's first won pixel is inside its box.
    for row in 1..8 {
        v[chr + 0x14 * 16 + row] = 0x7F;
    }
    v
}

struct Machine {
    bus: SysBus,
}

impl Machine {
    /// Tile `cell0` in nametable cell (0,0), palette 1 there, the backdrop dark, sprite 0 on tile
    /// `$2E` in sprite palette 1 at x = 40, lines 20-27, and nothing else on screen.
    fn new(cell0: u8, attr: u8) -> Machine {
        let mut bus = SysBus::new(load(&chr_rom_image()).unwrap());
        bus.ppu.set_tile_trace(true);
        let (ppu, cart) = bus.ppu_and_cart();
        ppu.ctrl = 0x00;
        ppu.mask = 0x1E;
        ppu.mem_write(cart, 0x2000, cell0);
        // Attribute 0x01: the top-left 2x2 cells use background palette 1.
        ppu.mem_write(cart, 0x23C0, 0x01);
        for (addr, value) in [
            0x3F00u16, 0x3F04, 0x3F05, 0x3F06, 0x3F07, 0x3F14, 0x3F15, 0x3F16, 0x3F17,
        ]
        .into_iter()
        .zip([0x0Fu8, 0x0F, 0x16, 0x27, 0x30, 0x0F, 0x16, 0x36, 0x0F])
        {
            ppu.mem_write(cart, addr, value);
        }
        for n in 0..64 {
            ppu.oam[n * 4] = 0xFF;
        }
        ppu.oam[0..4].copy_from_slice(&[19, 0x2E, attr, 40]);
        Machine { bus }
    }

    fn write(&mut self, addr: u16, value: u8) {
        let (ppu, cart) = self.bus.ppu_and_cart();
        ppu.mem_write(cart, addr, value);
    }

    /// One more tile in the nametable, cell (x, y).
    fn cell(&mut self, x: u16, y: u16, tile: u8) {
        self.write(0x2000 + y * 32 + x, tile);
    }

    /// One presented frame: run to the start of vblank, where the frame counter still carries the
    /// number of the picture in the plane (the frontend's presentation point).
    fn frame(&mut self) {
        self.bus.ppu.frame_ready = false;
        while !self.bus.ppu.frame_ready {
            let (ppu, cart) = self.bus.ppu_and_cart();
            ppu.tick(cart);
        }
    }

    fn run(&mut self, frames: u64) {
        for _ in 0..frames {
            self.frame();
        }
    }

    /// Run until the plane holds `frame`'s picture, i.e. until the counter reads `frame` at vblank.
    fn run_to_frame(&mut self, frame: u64) {
        while self.bus.ppu.frame < frame {
            self.frame();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Composing, the way `src/main.rs` does: 1x RGBA, magnified, then the pack's tiles over it.
// ---------------------------------------------------------------------------------------------

fn magnify(src: &[u8], scale: usize, out: &mut [u8]) {
    let wide = W * scale;
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            for sy in 0..scale {
                for sx in 0..scale {
                    let d = ((y * scale + sy) * wide + x * scale + sx) * 4;
                    out[d..d + 4].copy_from_slice(&src[i..i + 4]);
                }
            }
        }
    }
}

fn compose(bus: &mut SysBus, pack: &Pack) -> Vec<u8> {
    let scale = usize::from(pack.scale);
    let mut base = vec![0u8; W * H * 4];
    to_rgba_pal(
        &bus.ppu.framebuffer,
        &bus.ppu.emphasis,
        &rgba_table(&NES_PALETTE),
        &mut base,
    );
    let mut out = vec![0u8; W * scale * H * scale * 4];
    magnify(&base, scale, &mut out);
    pack.compose(bus, &mut out);
    out
}

/// The composed pixel at screen (`x`, `y`).
fn pixel(out: &[u8], scale: usize, x: usize, y: usize) -> [u8; 4] {
    let i = ((y * scale) * W * scale + x * scale) * 4;
    [out[i], out[i + 1], out[i + 2], out[i + 3]]
}

/// Byte-for-byte comparison that reports the first differing screen pixel, not two whole frames.
fn assert_same(got: &[u8], want: &[u8], scale: usize, what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: buffer length");
    if let Some((i, _)) = got.iter().zip(want).enumerate().find(|(_, (a, b))| a != b) {
        let p = (i / 4) / (W * scale);
        let q = (i / 4) % (W * scale);
        let (x, y) = (q / scale, p / scale);
        panic!(
            "{what}: at ({x}, {y}) got {:?} want {:?}",
            pixel(got, scale, x, y),
            pixel(want, scale, x, y)
        );
    }
}

/// Draw one replacement's pixels over `out`, the way the hand-built expectation needs: art covers
/// the 8 screen pixels from (`x`, `y`), alpha below 128 leaves the picture alone.
fn paint(out: &mut [u8], scale: usize, art: &[u8], x: usize, y: usize) {
    let wide = W * scale;
    for cy in 0..8 {
        for cx in 0..8 {
            for sy in 0..scale {
                for sx in 0..scale {
                    let s = ((cy * scale + sy) * 8 * scale + cx * scale + sx) * 4;
                    if art[s + 3] < 128 {
                        continue;
                    }
                    let d = ((y + cy) * scale + sy) * wide + (x + cx) * scale + sx;
                    out[d * 4..d * 4 + 4].copy_from_slice(&art[s..s + 4]);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Packs written for the test (§6.2: no vendored art).
// ---------------------------------------------------------------------------------------------

fn pack_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nes_hdpack_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(path: &Path, rgba: &[u8; 16 * 16 * 4]) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 16, 16);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
}

fn solid(colour: [u8; 4]) -> [u8; 16 * 16 * 4] {
    let mut art = [0u8; 16 * 16 * 4];
    for px in art.as_chunks_mut::<4>().0 {
        *px = colour;
    }
    art
}

/// 16x16 with the left half `GREEN` and the right half `YELLOW`, so a mirror shows.
fn split() -> [u8; 16 * 16 * 4] {
    let mut art = [0u8; 16 * 16 * 4];
    for (i, px) in art.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *px = if i % 16 < 8 { GREEN } else { YELLOW };
    }
    art
}

/// 16x16, one tile at scale 2, asymmetric both ways: art row 0 `RED`, rows 1-6 `BLUE`, the last row
/// `GREEN`; columns 0 and 7 `YELLOW`. A displaced anchor therefore shows at a known screen pixel.
fn anchored() -> [u8; 16 * 16 * 4] {
    let mut art = [0u8; 16 * 16 * 4];
    for (i, px) in art.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (ax, ay) = ((i % 16) / 2, (i / 16) / 2);
        *px = match ay {
            0 => RED,
            7 => GREEN,
            _ if ax == 0 || ax == 7 => YELLOW,
            _ => BLUE,
        };
    }
    art
}

/// Write `hires.txt` plus `a.png` (red) and `b.png` (blue), and load it.
fn load_pack(name: &str, ver: u16, lines: &[&str]) -> (PathBuf, Option<Pack>, Vec<String>) {
    let dir = pack_dir(name);
    write_png(&dir.join("a.png"), &solid(RED));
    write_png(&dir.join("b.png"), &solid(BLUE));
    let mut text = format!("<ver>{ver}\n<scale>2\n<img>a.png\n<img>b.png\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(dir.join("hires.txt"), text).unwrap();
    let (pack, notices) = Pack::load(&dir);
    (dir, pack, notices)
}

/// The A/B rule pair for the background key: `terms` draws red `a.png`, the fallback blue `b.png`.
fn ab_bg(name: &str, extra: &[&str], terms: &str) -> (PathBuf, Pack) {
    let mut body: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
    body.push(format!("[{terms}]<tile>{BG_RULE}"));
    body.push(format!("<tile>{BG_FALLBACK}"));
    let refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let (dir, pack, notices) = load_pack(name, 105, &refs);
    (
        dir,
        pack.unwrap_or_else(|| panic!("{name} must load: {notices:?}")),
    )
}

/// The A/B rule pair for the sprite key (red/blue), which is where the three flags are read.
fn ab_sprite(name: &str, terms: &str) -> (PathBuf, Pack) {
    let body = [
        format!("[{terms}]<tile>{SPR_RULE}"),
        format!("<tile>{SPR_FALLBACK}"),
    ];
    let refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let (dir, pack, notices) = load_pack(name, 105, &refs);
    (
        dir,
        pack.unwrap_or_else(|| panic!("{name} must load: {notices:?}")),
    )
}

// ---------------------------------------------------------------------------------------------
// 1. The repository's own test pack loads as §7 says.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_test_pack_loads_two_rules_and_ignores_the_overscan_line() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/hdpack");
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("tests/data/hdpack must load: {notices:?}"));
    assert_eq!(pack.scale, 2);
    assert_eq!(pack.rules_total, 2, "two <tile> rules");
    assert_eq!(pack.ignored, 1, "the <overscan> line");
    assert_eq!(pack.images.len(), 2);
    assert_eq!(
        pack.images[0].rgba.len(),
        16 * 16 * 4,
        "bg.png is 16x16 RGBA"
    );
    // The art is opaque where it is drawn and the sprite art is an L on transparency.
    assert!(
        pack.images[0].rgba.chunks(4).all(|px| px[3] == 255),
        "bg.png is fully opaque"
    );
    assert!(
        pack.images[1].rgba.chunks(4).any(|px| px[3] == 0),
        "spr.png has transparency"
    );
    assert_eq!(
        notices.first().map(String::as_str),
        Some("hdpack: 2 rules, scale 2x, 1 ignored")
    );
    assert!(
        notices
            .iter()
            .any(|n| n == "hdpack: line 5: <overscan> is outside this subset"),
        "{notices:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. A synthetic CHR-ROM frame: tile `$12` palette 1, sprite tile `$2E` sprite palette 1.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_composed_buffer_matches_a_hand_built_picture() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/hdpack");
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap();
    assert!(!notices.is_empty());

    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    let got = compose(&mut m.bus, &pack);

    // The expectation: the magnified 1x picture with the two replacements painted by hand.
    let mut base = vec![0u8; W * H * 4];
    to_rgba_pal(
        &m.bus.ppu.framebuffer,
        &m.bus.ppu.emphasis,
        &rgba_table(&NES_PALETTE),
        &mut base,
    );
    let mut want = vec![0u8; W * 2 * H * 2 * 4];
    magnify(&base, 2, &mut want);
    paint(&mut want, 2, &pack.images[0].rgba, 0, 0);
    paint(&mut want, 2, &pack.images[1].rgba, 40, 20);
    assert_same(&got, &want, 2, "composed picture");
    // And the replacements really landed: the background replacement is not a NES palette colour.
    assert_eq!(
        pixel(&got, 2, 0, 0),
        &pack.images[0].rgba[0..4],
        "bg replacement at (0,0)"
    );
    assert_eq!(
        pixel(&got, 2, 40, 20),
        &pack.images[1].rgba[0..4],
        "sprite replacement at (40,20)"
    );
    assert_ne!(
        pixel(&base, 1, 0, 0),
        &pack.images[0].rgba[0..4],
        "the 1x picture is the NES tile"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Off by default: no pack, the trace stays off and `save_state()` is byte-identical.
// ---------------------------------------------------------------------------------------------

#[test]
fn without_a_pack_the_trace_is_off_and_the_composed_path_changes_nothing() {
    let rom = chr_rom_image();
    let mut plain = Nes::new(&rom).unwrap();
    for _ in 0..2 {
        plain.run_frame();
    }
    assert!(
        plain.bus.ppu.tile_trace().is_none(),
        "the tile trace is off without a pack"
    );
    let before = plain.save_state();

    // The same machine with a pack: trace on, the picture composed — the state must not move.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/hdpack");
    let (pack, _) = Pack::load(&dir);
    let pack = pack.unwrap();
    let mut traced = Nes::new(&rom).unwrap();
    traced.bus.ppu.set_tile_trace(true);
    for _ in 0..2 {
        traced.run_frame();
    }
    let mut out = vec![0u8; W * 2 * H * 2 * 4];
    magnify(&to_rgba(&traced.bus), 2, &mut out);
    pack.compose(&mut traced.bus, &mut out);
    assert_eq!(
        traced.save_state(),
        before,
        "§6.1/§6.2: a pack is presentation only"
    );
    assert_ne!(traced.save_state().len(), 0);

    // At scale 1 the magnified picture is the 1x picture itself, which is the no-pack path.
    let rgba = to_rgba(&plain.bus);
    let mut one = vec![0u8; W * H * 4];
    magnify(&rgba, 1, &mut one);
    assert_eq!(one, rgba);
}

fn to_rgba(bus: &SysBus) -> Vec<u8> {
    let mut out = vec![0u8; W * H * 4];
    to_rgba_pal(
        &bus.ppu.framebuffer,
        &bus.ppu.emphasis,
        &rgba_table(&NES_PALETTE),
        &mut out,
    );
    out
}

// ---------------------------------------------------------------------------------------------
// 4. Every §1.4 outcome, from packs written in the test.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_pack_that_cannot_be_drawn_is_rejected() {
    // <ver>102: the tile-data base changed at 103, so guessing would rename every tile.
    let dir = pack_dir("ver102");
    write_png(&dir.join("a.png"), &solid(RED));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>102\n<scale>2\n<img>a.png\n<tile>0,12,0F162730,0,0,1,N\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    assert!(pack.is_none(), "a <ver> outside 103..=105 rejects the pack");
    assert!(notices[0].contains("<ver> 102"), "{notices:?}");
    assert!(notices[0].ends_with("(no pack loaded)"), "{notices:?}");

    // An unknown tag: a typo'd tag would otherwise replace nothing, silently.
    let (_dir, pack, notices) = load_pack("badtag", 105, &["<bogus>1,2"]);
    assert!(pack.is_none());
    assert!(notices[0].contains("unknown tag <bogus>"), "{notices:?}");

    // A <tile> whose img index names no <img>: the rule has no art.
    let (_dir, pack, notices) = load_pack("noimg", 105, &["<tile>7,12,0F162730,0,0,1,N"]);
    assert!(pack.is_none());
    assert!(notices[0].contains("names no <img>"), "{notices:?}");
}

#[test]
fn a_bad_rule_or_an_unevaluable_condition_costs_one_rule() {
    // Ignored rules (§1.4): a negative brightness (2.0 is legal from <ver>105, §9.2.1), and x/y
    // past the image edge.
    let (_dir, pack, notices) = load_pack(
        "ignoredrules",
        105,
        &[
            "<tile>0,12,0F162730,0,0,-1,N",
            "<tile>0,12,0F162730,9,0,1,N",
            "<tile>0,12,0F162730,0,0,1,N",
        ],
    );
    let pack = pack.unwrap();
    assert_eq!((pack.rules_total, pack.ignored), (1, 2), "{notices:?}");
    assert!(
        notices.iter().any(|n| n.contains("brightness")),
        "{notices:?}"
    );
    assert!(
        notices.iter().any(|n| n.contains("past the image edge")),
        "{notices:?}"
    );

    // §3.4.6: an undefined name, an empty term, and a memory condition in the I/O window.
    let (_dir, pack, notices) = load_pack(
        "badconds",
        105,
        &[
            "<condition>m,memoryCheck,2001,==,10",
            "[undefined]<tile>0,12,0F162730,0,0,1,N",
            "[a&]<tile>0,12,0F162730,0,0,1,N",
            "[m]<tile>0,12,0F162730,0,0,1,N",
            "<tile>1,12,0F162730,0,0,1,N",
        ],
    );
    let pack = pack.unwrap();
    assert_eq!(
        pack.rules_total, 1,
        "only the unconditional rule survives: {notices:?}"
    );
    assert_eq!(pack.ignored, 3, "{notices:?}");
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "the stock tile's fallback art"
    );
}

// ---------------------------------------------------------------------------------------------
// 5. One test per evaluated type (§3.4.1), true and false, as an A/B rule pair.
// ---------------------------------------------------------------------------------------------

/// Art A is asymmetric (left half green, right half yellow) so a mirror reads in the buffer.
#[test]
fn hmirror_is_read_from_the_attribute_byte_and_mirrors_the_art() {
    let dir = pack_dir("hmirror");
    write_png(&dir.join("a.png"), &split());
    write_png(&dir.join("b.png"), &solid(BLUE));
    std::fs::write(
        dir.join("hires.txt"),
        format!("<ver>105\n<scale>2\n<img>a.png\n<img>b.png\n[hmirror]<tile>{SPR_RULE}\n<tile>{SPR_FALLBACK}\n"),
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    // attr 0x41: hmirror set (and sprite palette 1): art A, mirrored.
    let mut m = Machine::new(0x12, 0x41);
    m.run(2);
    let out = compose(&mut m.bus, &pack);
    assert_eq!(
        pixel(&out, 2, 40, 20),
        YELLOW,
        "the sprite's left column shows the art's right edge"
    );
    assert_eq!(
        pixel(&out, 2, 47, 20),
        GREEN,
        "and its right column the art's left edge"
    );
    // attr 0x01: no flip, so the conditional rule is skipped and the fallback draws.
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 40, 20), BLUE);
}

#[test]
fn vmirror_true_and_false() {
    let (_dir, pack) = ab_sprite("vmirror", "vmirror");
    let mut m = Machine::new(0x12, 0x81);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 40, 20),
        RED,
        "vmirror set draws A"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 40, 20),
        BLUE,
        "vmirror clear falls through"
    );
}

#[test]
fn bgpriority_true_and_false() {
    let (_dir, pack) = ab_sprite("bgpriority", "bgpriority");
    let mut m = Machine::new(0x12, 0x21);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 40, 20),
        RED,
        "bgpriority set draws A"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 40, 20),
        BLUE,
        "bgpriority clear falls through"
    );
}

#[test]
fn tile_at_position_true_and_false() {
    let (_dir, pack) = ab_bg(
        "tileat",
        &["<condition>c,tileAtPosition,0,0,12,0F162730"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "tile $12 at (0,0) draws A"
    );
    // The same condition at a coordinate that holds tile 0.
    let (_dir, pack) = ab_bg(
        "tileat_false",
        &["<condition>c,tileAtPosition,8,0,12,0F162730"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "tile 0 there: the fallback draws"
    );
}

#[test]
fn sprite_at_position_true_and_false() {
    let (_dir, pack) = ab_bg(
        "sprat",
        &["<condition>c,spriteAtPosition,40,20,2E,FF16360F"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "the sprite won (40,20)"
    );
    // One pixel past the sprite's right edge: no sprite there.
    let (_dir, pack) = ab_bg(
        "sprat_false",
        &["<condition>c,spriteAtPosition,48,20,2E,FF16360F"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
}

#[test]
fn tile_nearby_true_and_false() {
    // 8 pixels to the right is another $12 tile.
    let (_dir, pack) = ab_bg(
        "tilenear",
        &["<condition>c,tileNearby,8,0,12,0F162730"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.cell(1, 0, 0x12);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    // The page's own example reads the tile 8 pixels to the left: off the plane at (0,0).
    let (_dir, pack) = ab_bg(
        "tilenear_false",
        &["<condition>c,tileNearby,-8,0,12,0F162730"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "off the plane is false"
    );
}

#[test]
fn sprite_nearby_true_and_false() {
    let (_dir, pack) = ab_bg(
        "sprnear",
        &["<condition>c,spriteNearby,40,20,2E,FF16360F"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    let (_dir, pack) = ab_bg(
        "sprnear_false",
        &["<condition>c,spriteNearby,40,28,2E,FF16360F"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "below the sprite is false"
    );
}

#[test]
fn memory_check_compares_two_work_ram_bytes_and_a_mask() {
    let (_dir, pack) = ab_bg("mem", &["<condition>c,memoryCheck,10,==,11"], "c");
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.bus.ram[0x11] = 0x5A;
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "equal bytes draw A"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.bus.ram[0x11] = 0x5B;
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "different bytes: the fallback"
    );
    // A mask of F0 keeps the high nibble, so 5A and 5B agree again.
    let (_dir, pack) = ab_bg("mem_mask", &["<condition>c,memoryCheck,10,==,11,F0"], "c");
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.bus.ram[0x11] = 0x5B;
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "the mask keeps only the high nibble"
    );
}

#[test]
fn memory_check_constant_compares_a_work_ram_byte() {
    let (_dir, pack) = ab_bg(
        "memconst",
        &["<condition>c,memoryCheckConstant,10,==,5A"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5B;
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
    // The constant is compared as written, so the mask only trims the byte.
    let (_dir, pack) = ab_bg(
        "memconst_mask",
        &["<condition>c,memoryCheckConstant,10,==,50,F0"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
}

#[test]
fn ppu_memory_check_reads_a_nametable_and_a_palette_byte() {
    // $2000 and $2001 both hold tile $12.
    let (_dir, pack) = ab_bg("ppumem", &["<condition>c,ppuMemoryCheck,2000,==,2001"], "c");
    let mut m = Machine::new(0x12, 0x01);
    m.cell(1, 0, 0x12);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    // A nametable byte against a palette byte: $12 versus the backdrop $0F.
    let (_dir, pack) = ab_bg(
        "ppumem_false",
        &["<condition>c,ppuMemoryCheck,2000,==,3F00"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
}

#[test]
fn ppu_memory_check_constant_reads_a_nametable_byte() {
    let (_dir, pack) = ab_bg(
        "ppumemconst",
        &["<condition>c,ppuMemoryCheckConstant,2000,==,12"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    let (_dir, pack) = ab_bg(
        "ppumemconst_false",
        &["<condition>c,ppuMemoryCheckConstant,2000,==,13"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
}

#[test]
fn frame_range_draws_the_last_two_frames_of_ten() {
    let (_dir, pack) = ab_bg("frame", &["<condition>c,frameRange,10,8"], "c");
    let mut m = Machine::new(0x12, 0x01);
    m.run_to_frame(7);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "frame 7 is not in the range"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run_to_frame(8);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "frame 8 is"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run_to_frame(9);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "frame 9 is"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run_to_frame(10);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "frame 10 starts the range again"
    );
}

// ---------------------------------------------------------------------------------------------
// 6. `!` and `&` on the rule's prefix.
// ---------------------------------------------------------------------------------------------

#[test]
fn not_and_and_terms_decide_the_rule() {
    let at = |name: &str, x: u16| {
        let cond = format!("<condition>c,tileAtPosition,{x},0,12,0F162730");
        ab_bg(name, &[cond.as_str()], "!c")
    };
    // c holds, so [!c] fails.
    let pack = at("not_false", 0).1;
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
    // c fails, so [!c] holds.
    let pack = at("not_true", 8).1;
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);

    let pair = |name: &str, terms: &str, extra: &[&str]| ab_bg(name, extra, terms);
    // [c1&c2], both true.
    let pack = pair(
        "and_true",
        "c1&c2",
        &[
            "<condition>c1,tileAtPosition,0,0,12,0F162730",
            "<condition>c2,tileAtPosition,8,0,12,0F162730",
        ],
    )
    .1;
    let mut m = Machine::new(0x12, 0x01);
    m.cell(1, 0, 0x12);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    // [c1&c2] with c2 false.
    let pack = pair(
        "and_false",
        "c1&c2",
        &[
            "<condition>c1,tileAtPosition,0,0,12,0F162730",
            "<condition>c2,tileAtPosition,8,0,12,0F162730",
        ],
    )
    .1;
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
    // [c1&!c2] with c2 false.
    let pack = pair(
        "andnot_true",
        "c1&!c2",
        &[
            "<condition>c1,tileAtPosition,0,0,12,0F162730",
            "<condition>c2,tileAtPosition,8,0,12,0F162730",
        ],
    )
    .1;
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
    // [c1&!c2] with c2 true.
    let pack = pair(
        "andnot_false",
        "c1&!c2",
        &[
            "<condition>c1,tileAtPosition,0,0,12,0F162730",
            "<condition>c2,tileAtPosition,0,0,12,0F162730",
        ],
    )
    .1;
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), BLUE);
}

// ---------------------------------------------------------------------------------------------
// 7. Gap 4: the background tile is named under a transparent pixel and under a sprite.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_transparent_background_anchor_still_names_its_tile() {
    // Tile $13's row 0 is transparent: its anchor pixel is the backdrop, and still named.
    let lines = [
        "<condition>c,tileAtPosition,0,0,13,0F162730".to_string(),
        "[c]<tile>0,13,0F162730,0,0,1,N".to_string(),
        "<tile>1,13,0F162730,0,0,1,N".to_string(),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (_dir, pack, notices) = load_pack("transparent", 105, &refs);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x13, 0x01);
    m.run(2);
    let out = compose(&mut m.bus, &pack);
    assert_eq!(
        pixel(&out, 2, 0, 1),
        RED,
        "the tile's opaque rows are replaced"
    );
    assert_ne!(
        pixel(&out, 2, 0, 0),
        RED,
        "the backdrop pixel itself is not painted"
    );
}

#[test]
fn the_background_identity_is_recorded_where_a_sprite_won() {
    // Tile $12 also sits under the sprite, in attribute palette 1 (cells 4-5 x 2-3 of $23C1).
    let (_dir, pack) = ab_bg(
        "under_sprite",
        &["<condition>c,tileAtPosition,40,20,12,0F162730"],
        "c",
    );
    let mut m = Machine::new(0x12, 0x01);
    m.cell(5, 2, 0x12);
    m.cell(5, 3, 0x12);
    m.write(0x23C1, 0x10);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "the sprite-won pixel names the tile"
    );
}

// ---------------------------------------------------------------------------------------------
// 8. <ver>103 reads a condition's CHR-ROM index as decimal.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_ver_103_condition_index_is_decimal() {
    let draw = |name: &str, ver: u16, index: &str| {
        let lines = [
            format!("<condition>c,tileAtPosition,0,0,{index},0F162730"),
            "[c]<tile>0,C,0F162730,0,0,1,N".to_string(),
            "<tile>1,C,0F162730,0,0,1,N".to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (_dir, pack, notices) = load_pack(name, ver, &refs);
        let pack = pack.unwrap_or_else(|| panic!("{name}: {notices:?}"));
        let mut m = Machine::new(0x0C, 0x01);
        m.run(2);
        let art = pixel(&compose(&mut m.bus, &pack), 2, 0, 0);
        (art, pack.ignored, notices)
    };
    // Tile 12 (0x0C) at (0,0): under 103 `12` is decimal 12 -> the conditional rule draws.
    assert_eq!(
        draw("ver103", 103, "12").0,
        RED,
        "under <ver>103 the index 12 names tile 12"
    );
    // The same line under 104 names tile $12 -> the scene's tile 12 is not it.
    assert_eq!(
        draw("ver104", 104, "12").0,
        BLUE,
        "under <ver>104 the index 12 names tile $12"
    );
    // A non-decimal index under 103 is malformed: the condition line is ignored with a warning, and
    // so is the rule that names it.
    let (art, ignored, notices) = draw("ver103_g", 103, "G");
    assert_eq!(
        art, BLUE,
        "the rule naming the ignored condition is ignored too"
    );
    assert_eq!(ignored, 2, "{notices:?}");
}

// ---------------------------------------------------------------------------------------------
// 9. §3.4.2: a sprite group's art and conditions anchor at its box, not its first won pixel.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_sprite_anchors_its_art_at_its_box_not_its_first_won_pixel() {
    // Tile $14's row 0 and left column are transparent, so the only opaque pixels are rows 1-7,
    // columns 1-7: the first won pixel is (41, 21), one in from the box at (40, 20).
    let dir = pack_dir("sprite_anchor");
    write_png(&dir.join("a.png"), &anchored());
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<tile>0,14,FF16360F,0,0,1,N\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ppu.oam[1] = 0x14; // sprite 0 draws tile $14 instead of $2E
    m.run(2);
    let base = to_rgba(&m.bus);
    let out = compose(&mut m.bus, &pack);
    // Art row N lands on the box's line N and art column N on its column N.
    assert_eq!(
        pixel(&out, 2, 41, 21),
        BLUE,
        "line 21 draws art row 1, not row 0"
    );
    assert_eq!(
        pixel(&out, 2, 47, 21),
        YELLOW,
        "column 47 draws art column 7"
    );
    assert_eq!(
        pixel(&out, 2, 41, 27),
        GREEN,
        "the art's last row is drawn on the box's last line"
    );
    // The transparent corner won no pixel, so nothing is painted there.
    assert_eq!(
        pixel(&out, 2, 40, 21),
        pixel(&base, 1, 40, 21),
        "the transparent corner is untouched"
    );
}

// ---------------------------------------------------------------------------------------------
// 10. §1.2: hex fields take no sign, even though `u8::from_str_radix` would read `+1` as `01`.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_signed_hex_field_is_ignored_not_parsed() {
    let ram = format!("+0{}", "11".repeat(15)); // 32 chars, with a `+` in the first pair
    let lines = [
        format!("<tile>0,{ram},0F162730,0,0,1,N"),
        "<tile>0,12,+1+2+3+4,0,0,1,N".to_string(),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (_dir, pack, notices) = load_pack("signedhex", 105, &refs);
    let pack = pack.unwrap();
    assert_eq!(pack.rules_total, 0, "{notices:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("palette data is not 8 hex chars")),
        "{notices:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 11. An `<img>` larger than the 16-bit plane is refused from its header, before it is decoded.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_image_past_65535_a_side_is_rejected() {
    let dir = pack_dir("widepng");
    write_png(&dir.join("a.png"), &solid(RED));
    let file = std::fs::File::create(dir.join("b.png")).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 65536, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&vec![0u8; 65536 * 4])
        .unwrap();
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<img>b.png\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    assert!(
        pack.is_none(),
        "a 65536-wide image cannot be a plane: {notices:?}"
    );
    assert!(notices[0].contains("b.png"), "{notices:?}");
}

// ---------------------------------------------------------------------------------------------
// 12. An <img> past the §9.6 pin 2 cap is refused from its header, before any allocation.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_image_past_the_2048x1920_cap_is_rejected_from_its_header() {
    let dir = pack_dir("hugepng");
    write_png(&dir.join("a.png"), &solid(RED));
    // A 65535x65535 RGBA PNG holding no pixels: the header declares the 17 GB size, the file is a
    // few dozen bytes, and the decoder must refuse it from the header before allocating anything.
    std::fs::write(dir.join("b.png"), huge_png_header(65535, 65535)).unwrap();
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<img>b.png\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    assert!(
        pack.is_none(),
        "a 65535x65535 image cannot be a plane: {notices:?}"
    );
    assert!(notices[0].contains("b.png"), "{notices:?}");
    assert!(notices[0].contains("2048x1920 cap"), "{notices:?}");
}

/// A PNG whose IHDR declares `width`x`height` 8-bit RGBA and whose IDAT decompresses to nothing:
/// everything `read_info()` needs and no pixel data, so a 65535x65535 header costs no memory.
fn huge_png_header(width: u32, height: u32) -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, colour type RGBA, compression, filter, interlace
    for (kind, data) in [
        (&b"IHDR"[..], &ihdr[..]),
        (
            b"IDAT",
            &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01][..],
        ),
        (b"IEND", &[][..]),
    ] {
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(&body);
        png.extend_from_slice(&tendy_nes::checksum::crc32(&body).to_be_bytes());
    }
    png
}

// ---------------------------------------------------------------------------------------------
// §9.7 test 17. A bad <background> field costs one rule, never the pack (§9.1.1, §9.6).
// ---------------------------------------------------------------------------------------------

#[test]
fn a_bad_background_field_costs_one_rule_never_the_pack() {
    let dir = pack_dir("bg_bad_fields");
    write_png(&dir.join("a.png"), &solid(RED));
    write_png(&dir.join("b.png"), &solid(BLUE));
    // 2049 pixels wide: one past §9.6 pin 2's cap.
    let file = std::fs::File::create(dir.join("wide.png")).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 2049, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&vec![0u8; 2049 * 4])
        .unwrap();
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<img>b.png\n<background>a.png\n\
         <background>a.png,1.0,0.5,0,N,0,0\n<background>a.png,1.0,0.5\n\
         <background>a.png,1.0,0,0,X,0,0\n<background>a.png,1.0,0,0,N,x,0\n\
         <background>wide.png\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("the pack must still load: {notices:?}"));
    assert_eq!(
        (pack.backgrounds_total, pack.ignored),
        (1, 5),
        "{notices:?}"
    );
    for reason in [
        "scroll ratio is not 0",
        "neither Y nor N",
        "left or top is not a number",
        "past the 2048x1920 cap",
    ] {
        assert!(
            notices.iter().any(|n| n.contains(reason)),
            "{reason}: {notices:?}"
        );
    }
    // The good rule still draws: tile 0 under it is a backdrop hole.
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "the good background draws"
    );
}

// ---------------------------------------------------------------------------------------------
// §9.1.1 fields 6-7: an absent or empty `left`/`top` is 0, not a bad number.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_empty_background_offset_is_zero() {
    let (_dir, pack) = bg_pack("bg_offsets_empty", &["<background>a.png,1.0,0,0,N,,"]);
    assert_eq!(
        (pack.backgrounds_total, pack.ignored),
        (1, 0),
        "an empty offset is 0, not a warning"
    );
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "the image sits at 0,0"
    );
}

// ---------------------------------------------------------------------------------------------
// §9.7 test 18. A <background> name with `/` rejects the pack (§1.1, §1.4, §9.1.1 field 1).
// ---------------------------------------------------------------------------------------------

#[test]
fn a_background_name_with_a_slash_rejects_the_pack() {
    let (_dir, pack, notices) = load_pack("bg_slash", 105, &["<background>bg/art.png"]);
    assert!(
        pack.is_none(),
        "a name with '/' is a reject, not a warning: {notices:?}"
    );
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("line 5"), "{notices:?}");
    assert!(notices[0].contains("bg/art.png"), "{notices:?}");
    assert!(notices[0].ends_with("(no pack loaded)"), "{notices:?}");
}

// ---------------------------------------------------------------------------------------------
// §9: backgrounds, brightness, `default tile` (§9.7 tests 9-16).
// ---------------------------------------------------------------------------------------------

/// A pack with `a.png` (red), `b.png` (blue) and `lines`, loaded; panics with the notices.
fn bg_pack(name: &str, lines: &[&str]) -> (PathBuf, Pack) {
    let (dir, pack, notices) = load_pack(name, 105, lines);
    (
        dir,
        pack.unwrap_or_else(|| panic!("{name} must load: {notices:?}")),
    )
}

/// True when the composed pixel at (`x`, `y`) is still the 1x art the picture started from.
fn kept_the_1x_art(m: &mut Machine, pack: &Pack, x: usize, y: usize) -> bool {
    let base = to_rgba(&m.bus);
    pixel(&compose(&mut m.bus, pack), 2, x, y) == pixel(&base, 1, x, y)
}

/// §9.7 test 9. The §7 `bg/` fixture loads and draws.
#[test]
fn a_background_line_loads_and_draws() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/hdpack/bg");
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("tests/data/hdpack/bg must load: {notices:?}"));
    assert_eq!(
        (
            pack.scale,
            pack.rules_total,
            pack.backgrounds_total,
            pack.ignored
        ),
        (1, 0, 1, 0)
    );
    assert_eq!(notices.len(), 1, "no warnings: {notices:?}");
    assert_eq!(
        notices[0],
        "hdpack: 0 rules, 1 backgrounds, scale 1x, 0 ignored"
    );

    // Tile 0 at (0,0) is transparent, so the pixel is a backdrop hole the image fills.
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    let out = compose(&mut m.bus, &pack);
    // `FF 00 FF` at brightness 0.5 (§9.4): 255 → 128.
    assert_eq!(
        pixel(&out, 1, 0, 0),
        [128, 0, 128, 255],
        "the image's colour, halved"
    );
    // The 1x picture under it is the backdrop, and the 16x16 image stops at (16, 0).
    let base = to_rgba(&m.bus);
    assert_ne!(pixel(&base, 1, 0, 0), [128, 0, 128, 255]);
    assert_eq!(
        pixel(&out, 1, 16, 0),
        pixel(&base, 1, 16, 0),
        "outside the image's rect"
    );
}

/// §9.7 test 10. The §9.3 mask: only a backdrop hole takes the image.
#[test]
fn the_image_shows_only_through_backdrop_holes() {
    // Cell (0,0) holds tile $12 (an opaque background tile), cell (1,0) is empty.
    let (_dir, pack) = bg_pack("bg_mask", &["<background>a.png,1.0,0,0,N,0,0"]);
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    let base = to_rgba(&m.bus);
    let out = compose(&mut m.bus, &pack);
    assert_eq!(
        pixel(&out, 2, 0, 0),
        pixel(&base, 1, 0, 0),
        "an opaque background tile pixel keeps its picture"
    );
    assert_eq!(
        pixel(&out, 2, 8, 0),
        RED,
        "a backdrop hole takes the image's colour"
    );

    // The same image moved so the sprite's pixel falls inside its rect: a winning sprite is opaque.
    let (_dir, pack) = bg_pack("bg_mask_sprite", &["<background>a.png,1.0,0,0,N,-40,-20"]);
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    let base = to_rgba(&m.bus);
    let out = compose(&mut m.bus, &pack);
    assert_eq!(
        pixel(&out, 2, 40, 20),
        pixel(&base, 1, 40, 20),
        "a sprite pixel in front keeps its own art"
    );
    assert_eq!(pixel(&out, 2, 40, 30), RED, "and the image draws beside it");
}

/// §9.7 test 11. `showBehindBackgroundPrioritySprites` (absent, `N`, `Y`).
#[test]
fn show_behind_decides_whether_the_image_covers_a_background_priority_sprite() {
    // Sprite 0 on tile $2E, attribute bit 5 set, moved to cover (4..12, 4..12) — inside the image.
    let sprite = || {
        let mut m = Machine::new(0x00, 0x21);
        m.bus.ppu.oam[0..4].copy_from_slice(&[3, 0x2E, 0x21, 4]);
        m.run(2);
        m
    };
    for (name, line) in [
        ("bg_behind_absent", "<background>a.png"),
        ("bg_behind_n", "<background>a.png,1.0,0,0,N,0,0"),
    ] {
        let (_dir, pack) = bg_pack(name, &[line]);
        let mut m = sprite();
        let base = to_rgba(&m.bus);
        let out = compose(&mut m.bus, &pack);
        assert_eq!(
            pixel(&out, 2, 4, 4),
            RED,
            "{name}: the image covers the sprite"
        );
        assert_ne!(pixel(&base, 1, 4, 4), RED);
    }
    let (_dir, pack) = bg_pack("bg_behind_y", &["<background>a.png,1.0,0,0,Y,0,0"]);
    let mut m = sprite();
    let base = to_rgba(&m.bus);
    let out = compose(&mut m.bus, &pack);
    assert_eq!(
        pixel(&out, 2, 4, 4),
        pixel(&base, 1, 4, 4),
        "Y: the sprite stays in front of the image"
    );
}

/// §9.7 test 12. Conditions on a background rule, from one work-RAM byte.
#[test]
fn a_condition_on_a_background_rule_chooses_the_image() {
    let lines = [
        "<condition>c,memoryCheckConstant,10,==,5A",
        "[c]<background>a.png",
        "[!c]<background>b.png",
    ];
    let (_dir, pack) = bg_pack("bg_cond", &lines);
    assert_eq!(pack.backgrounds_total, 2);
    let mut m = Machine::new(0x00, 0x01);
    m.bus.ram[0x10] = 0x5A;
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "c holds: the red image"
    );
    let mut m = Machine::new(0x00, 0x01);
    m.bus.ram[0x10] = 0x5B;
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "c fails: the other image"
    );
}

/// §9.7 test 13. Two matching rules stack in file order (D1).
#[test]
fn two_matching_backgrounds_stack_in_file_order() {
    let (_dir, pack) = bg_pack("bg_stack", &["<background>a.png", "<background>b.png"]);
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "the later image wins where both cover"
    );
    // The same two rules the other way round: the order is the file's, not the colours'.
    let (_dir, pack) = bg_pack(
        "bg_stack_swapped",
        &["<background>b.png", "<background>a.png"],
    );
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert_eq!(pixel(&compose(&mut m.bus, &pack), 2, 0, 0), RED);
}

/// §9.7 test 14. `<tile>` brightness (§9.2.1, §9.4).
#[test]
fn tile_brightness_scales_the_channels_and_needs_ver_105_above_one() {
    let dir = pack_dir("tile_brightness_half");
    write_png(&dir.join("a.png"), &solid([0xFF, 0xFE, 0x00, 0xFF]));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<tile>0,12,0F162730,0,0,0.5,N\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        [0x80, 0x7F, 0x00, 255],
        "255 → 128, 254 → 127, and alpha is still a mask"
    );

    // 2.0 saturates: 200 × 2 clamps to 255.
    let dir = pack_dir("tile_brightness_two");
    write_png(&dir.join("a.png"), &solid([0xC8, 0x00, 0x00, 0xFF]));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<tile>0,12,0F162730,0,0,2.0,N\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        [255, 0, 0, 255],
        "200 × 2 clamps to 255"
    );

    // The same line under <ver>104 is ignored with one warning, and the 1x art stays.
    let dir = pack_dir("tile_brightness_104");
    write_png(&dir.join("a.png"), &solid([0xC8, 0x00, 0x00, 0xFF]));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>104\n<scale>2\n<img>a.png\n<tile>0,12,0F162730,0,0,2.0,N\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    assert_eq!((pack.rules_total, pack.ignored), (0, 1), "{notices:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("brightness above 1.0 needs <ver>105")),
        "{notices:?}"
    );
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert!(
        kept_the_1x_art(&mut m, &pack, 0, 0),
        "the rule is ignored: the 1x art stays"
    );
}

/// §9.7 test 15. `<background>` brightness.
#[test]
fn background_brightness_halves_the_channels_and_needs_ver_105_above_one() {
    let dir = pack_dir("bg_brightness_half");
    write_png(&dir.join("a.png"), &solid([0xFF, 0xFE, 0x00, 0xFF]));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>105\n<scale>2\n<img>a.png\n<background>a.png,0.5\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        [0x80, 0x7F, 0x00, 255],
        "the image halved"
    );

    let dir = pack_dir("bg_brightness_104");
    write_png(&dir.join("a.png"), &solid([0xFF, 0xFE, 0x00, 0xFF]));
    std::fs::write(
        dir.join("hires.txt"),
        "<ver>104\n<scale>2\n<img>a.png\n<background>a.png,1.5\n",
    )
    .unwrap();
    let (pack, notices) = Pack::load(&dir);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    assert_eq!(
        (pack.backgrounds_total, pack.ignored),
        (0, 1),
        "{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|n| n.contains("<background> brightness above 1.0 needs <ver>105")),
        "{notices:?}"
    );
    let mut m = Machine::new(0x00, 0x01);
    m.run(2);
    assert!(
        kept_the_1x_art(&mut m, &pack, 0, 0),
        "no image: the backdrop stays"
    );
}

/// §9.7 test 16. `default tile` (§9.2.2, D5).
#[test]
fn a_default_rule_draws_only_where_a_tile_data_has_no_rule_of_its_own() {
    // The scene's key is tile $12 in palette 1 (`0F 16 27 30`); the fallback names palette 2.
    let fallback = "<tile>0,12,0F071727,0,0,1,Y";
    let (_dir, pack, notices) = load_pack("default_only", 105, &[fallback]);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        RED,
        "no rule for palette 1: the default draws"
    );

    // A rule of its own for that key wins, and no warning is left over.
    let (_dir, pack, notices) = load_pack(
        "default_shadowed",
        105,
        &[fallback, "<tile>1,12,0F162730,0,0,1,N"],
    );
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    assert_eq!(pack.ignored, 0, "{notices:?}");
    let mut m = Machine::new(0x12, 0x01);
    m.run(2);
    assert_eq!(
        pixel(&compose(&mut m.bus, &pack), 2, 0, 0),
        BLUE,
        "the key's own rule draws"
    );

    // D5: the key has a rule whose condition fails — no fallback, the 1x art stays.
    let lines = [
        fallback,
        "<condition>c,memoryCheckConstant,10,==,01",
        "[c]<tile>1,12,0F162730,0,0,1,N",
    ];
    let (_dir, pack, notices) = load_pack("default_cond_false", 105, &lines);
    let pack = pack.unwrap_or_else(|| panic!("{notices:?}"));
    let mut m = Machine::new(0x12, 0x01);
    m.bus.ram[0x10] = 0x02;
    m.run(2);
    assert!(
        kept_the_1x_art(&mut m, &pack, 0, 0),
        "the key has a rule, so the default is not consulted"
    );
}
