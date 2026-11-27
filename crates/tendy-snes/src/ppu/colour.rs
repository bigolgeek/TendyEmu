//! Composition: the priority chart, windows, colour math, brightness and the frame row
//!
//! The chart is data. §5.4 transcribes fullsnes's "Background Priority Chart" into one array per
//! mode, front to back; [`rank`] is the index of the first row a pixel matches, and a pixel that
//! matches no row (a layer the mode does not draw, or a priority that layer cannot carry) is
//! invisible. Windows are applied as the pixel is offered rather than as a pass afterwards, because
//! `main[x]` holds only the frontmost pixel: masking a layer must let the one behind it show.

use super::layers;
use super::regs::PpuRegs;
use super::{Layer, Pixel, Ppu, WIDTH};

/// Which screen a pixel is being offered to; only `TMW`/`TSW` differ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Main,
    Sub,
}

/// One row of the priority chart: the layer and the priority value it must carry.
type Row = (Layer, u8);

/// fullsnes "Background Priority Chart", mode 0: 12 rows, then the backdrop.
static MODE0: [Row; 12] = [
    (Layer::Obj, 3),
    (Layer::Bg1, 1),
    (Layer::Bg2, 1),
    (Layer::Obj, 2),
    (Layer::Bg1, 0),
    (Layer::Bg2, 0),
    (Layer::Obj, 1),
    (Layer::Bg3, 1),
    (Layer::Bg4, 1),
    (Layer::Obj, 0),
    (Layer::Bg3, 0),
    (Layer::Bg4, 0),
];

/// Mode 1 with `$2105` bit 3 **set** (".1a"): BG3's high-priority tiles go in front of every OBJ.
static MODE1_HIGH: [Row; 10] = [
    (Layer::Bg3, 1),
    (Layer::Obj, 3),
    (Layer::Bg1, 1),
    (Layer::Bg2, 1),
    (Layer::Obj, 2),
    (Layer::Bg1, 0),
    (Layer::Bg2, 0),
    (Layer::Obj, 1),
    (Layer::Obj, 0),
    (Layer::Bg3, 0),
];

/// Mode 1 with `$2105` bit 3 **clear** (".1b" and ".0b"): BG3 sinks behind OBJ 1 and OBJ 0.
static MODE1_LOW: [Row; 10] = [
    (Layer::Obj, 3),
    (Layer::Bg1, 1),
    (Layer::Bg2, 1),
    (Layer::Obj, 2),
    (Layer::Bg1, 0),
    (Layer::Bg2, 0),
    (Layer::Obj, 1),
    (Layer::Bg3, 1),
    (Layer::Obj, 0),
    (Layer::Bg3, 0),
];

/// Modes 2-5: BG2.1 sits below BG1.0, and BG2.0 last.
static MODES_2345: [Row; 8] = [
    (Layer::Obj, 3),
    (Layer::Bg1, 1),
    (Layer::Obj, 2),
    (Layer::Bg2, 1),
    (Layer::Bg1, 0),
    (Layer::Obj, 1),
    (Layer::Obj, 0),
    (Layer::Bg2, 0),
];

/// Mode 6: BG1 only.
static MODE6: [Row; 6] = [
    (Layer::Obj, 3),
    (Layer::Bg1, 1),
    (Layer::Obj, 2),
    (Layer::Obj, 1),
    (Layer::Bg1, 0),
    (Layer::Obj, 0),
];

/// Mode 7: the single BG1 layer, with BG2's per-pixel priority rows ready for EXTBG (not drawn in
/// v1, §5.8).
static MODE7: [Row; 7] = [
    (Layer::Obj, 3),
    (Layer::Obj, 2),
    (Layer::Bg2, 1),
    (Layer::Obj, 1),
    (Layer::Bg1, 0),
    (Layer::Obj, 0),
    (Layer::Bg2, 0),
];

/// `BGMODE` bits 2-0 to the chart; mode 1 picks between its two columns by `$2105` bit 3.
fn chart(mode: u8, bg3_high: bool) -> &'static [Row] {
    match mode & 7 {
        0 => &MODE0,
        1 => {
            if bg3_high {
                &MODE1_HIGH
            } else {
                &MODE1_LOW
            }
        }
        2..=5 => &MODES_2345,
        6 => &MODE6,
        _ => &MODE7,
    }
}

/// Where a pixel sits in the chart: 0 is frontmost, [`u8::MAX`] means the mode does not draw it.
pub fn rank(mode: u8, bg3_high: bool, pixel: &Pixel) -> u8 {
    if pixel.layer == Layer::Backdrop {
        return u8::MAX;
    }
    for (index, &(layer, priority)) in chart(mode, bg3_high).iter().enumerate() {
        if layer == pixel.layer && priority == pixel.priority {
            return index as u8;
        }
    }
    u8::MAX
}

/// Offers a pixel to one screen: the window mask and the chart decide whether it lands.
pub fn offer(dst: &mut Pixel, regs: &PpuRegs, screen: Screen, x: usize, pixel: Pixel) {
    let mode = regs.bgmode & 7;
    let bg3_high = regs.bgmode & 0x08 != 0;
    let candidate = rank(mode, bg3_high, &pixel);
    if candidate >= rank(mode, bg3_high, dst) {
        return;
    }
    if masked(regs, pixel.layer, screen, x as u8) {
        // §5.6: `TMW`/`TSW` force the layer off inside its final window; what is behind shows.
        return;
    }
    *dst = pixel;
}

/// §5.6: inside → combine → mask, for one layer at `x`.
fn masked(regs: &PpuRegs, layer: Layer, screen: Screen, x: u8) -> bool {
    let bit = match layer {
        Layer::Bg1 => 0x01,
        Layer::Bg2 => 0x02,
        Layer::Bg3 => 0x04,
        Layer::Bg4 => 0x08,
        Layer::Obj => 0x10,
        Layer::Backdrop => return false,
    };
    let disable = match screen {
        Screen::Main => regs.tmw,
        Screen::Sub => regs.tsw,
    };
    disable & bit != 0 && inside_final_window(regs, layer, x)
}

/// The two 2-bit selectors and the 2-bit combining logic for a layer.
fn selectors(regs: &PpuRegs, layer: Layer) -> (u8, u8, u8) {
    match layer {
        Layer::Bg1 => (
            regs.w12sel & 0x03,
            (regs.w12sel >> 2) & 0x03,
            regs.wbglog & 0x03,
        ),
        Layer::Bg2 => (
            (regs.w12sel >> 4) & 0x03,
            (regs.w12sel >> 6) & 0x03,
            (regs.wbglog >> 2) & 0x03,
        ),
        Layer::Bg3 => (
            regs.w34sel & 0x03,
            (regs.w34sel >> 2) & 0x03,
            (regs.wbglog >> 4) & 0x03,
        ),
        Layer::Bg4 => (
            (regs.w34sel >> 4) & 0x03,
            (regs.w34sel >> 6) & 0x03,
            (regs.wbglog >> 6) & 0x03,
        ),
        Layer::Obj => (
            regs.wobjsel & 0x03,
            (regs.wobjsel >> 2) & 0x03,
            regs.wobjlog & 0x03,
        ),
        Layer::Backdrop => (0, 0, 0),
    }
}

/// `x` is inside a window: `WHn`/`WHn+1`, edges included, empty when the right edge is left of the
/// left one (fullsnes "SNES PPU Window").
fn in_window(regs: &PpuRegs, window: u8, x: u8) -> bool {
    let (left, right) = if window == 0 {
        (regs.wh[0], regs.wh[1])
    } else {
        (regs.wh[2], regs.wh[3])
    };
    left <= x && x <= right
}

/// A 2-bit selector is `enable:invert` — wiki `PPU_registers`, W12SEL ($2123):
///
/// ```text
/// 7  bit  0
/// ---- ----
/// DdCc BbAa
/// |||| ||||
/// |||| |||+- Invert window 1 for BG1
/// |||| ||+-- Enable window 1 for BG1
/// |||| |+--- Invert window 2 for BG1
/// |||| +---- Enable window 2 for BG1
/// ```
///
/// W34SEL (`HhGg FfEe`) and WOBJSEL (`LlKk JjIi`) are the same, WOBJSEL's `KL` being the colour
/// window. So `2` is enabled and not inverted, `3` is enabled and inverted (the window's area is
/// the complement of the `WHn`/`WHn+1` range), and `0`/`1` are not enabled — wiki `Windows`: "If
/// the window is disabled then invert has no effect (all 0 output)". `None` is that disabled
/// window; `Some` is whether the window outputs 1, which is where it hides its layer.
fn select(selector: u8, inside: bool) -> Option<bool> {
    match selector {
        2 => Some(inside),
        3 => Some(!inside),
        _ => None,
    }
}

/// The per-layer final window: one enabled window as-is, both combined by the logic, neither an
/// empty area (fullsnes "WBGLOG/WOBJLOG").
fn combined(one: Option<bool>, two: Option<bool>, logic: u8) -> bool {
    match (one, two) {
        (Some(a), Some(b)) => match logic {
            0 => a || b,
            1 => a && b,
            2 => a != b,
            _ => a == b,
        },
        (Some(area), None) | (None, Some(area)) => area,
        (None, None) => false,
    }
}

/// True when `x` is inside the layer's final window area.
fn inside_final_window(regs: &PpuRegs, layer: Layer, x: u8) -> bool {
    let (one, two, logic) = selectors(regs, layer);
    combined(
        select(one, in_window(regs, 0, x)),
        select(two, in_window(regs, 1, x)),
        logic,
    )
}

/// `CGWSEL` bits 5-4's window: window 1 and 2 combined by `WOBJLOG` bits 3-2.
fn inside_math_window(regs: &PpuRegs, x: u8) -> bool {
    combined(
        select((regs.wobjsel >> 4) & 0x03, in_window(regs, 0, x)),
        select((regs.wobjsel >> 6) & 0x03, in_window(regs, 1, x)),
        (regs.wobjlog >> 2) & 0x03,
    )
}

/// §5.2 steps 5-8: resolve both screens, apply colour math and brightness, write the frame row.
pub fn compose(ppu: &mut Ppu) {
    let regs = ppu.latched.clone();
    let mode = regs.bgmode & 7;
    let blank = regs.inidisp & 0x80 != 0;
    let brightness = regs.inidisp & 0x0F;
    for x in 0..WIDTH {
        let main = ppu.main[x];
        let sub = ppu.sub[x];
        let mut colour = resolve(ppu, &regs, mode, main);
        if blank {
            colour = 0;
        } else {
            colour = math(ppu, &regs, mode, x as u8, main, sub, colour);
            colour = brighten(colour, brightness);
        }
        ppu.line_out[x] = colour;
    }
    let line = usize::from(ppu.line);
    if (1..=224).contains(&line) {
        let start = (line - 1) * WIDTH;
        ppu.frame[start..start + WIDTH].copy_from_slice(&ppu.line_out);
    }
}

/// The 15-bit colour a pixel stands for: CGRAM, or the direct-colour word for a 256-colour BG when
/// `CGWSEL` bit 0 is set (§5.3, fullsnes "Direct Color Mode").
pub fn resolve(ppu: &Ppu, regs: &PpuRegs, mode: u8, pixel: Pixel) -> u16 {
    if pixel.layer == Layer::Backdrop {
        return ppu.cgram[0];
    }
    let depth = layers::bpp(mode, pixel.layer);
    if depth == 8 && regs.cgwsel & 0x01 != 0 && direct_colour_layer(mode, pixel.layer) {
        return direct_colour(pixel.index, pixel.palette);
    }
    let index = match depth {
        // §5.3's one piece of shared arithmetic: a page of 16 for a 16-colour layer, a page of 4
        // for a 4-colour one, and the raw index for a 256-colour layer.
        8 => u16::from(pixel.index),
        4 => u16::from(pixel.palette) * 16 + u16::from(pixel.index),
        _ => u16::from(pixel.palette) * 4 + u16::from(pixel.index),
    };
    ppu.cgram[usize::from(index & 0xFF)]
}

/// Direct colour is a BG1 feature in modes 3, 4 and 7 (fullsnes "Direct Color Mode": "BG1 in
/// Mode 3,4,7").
fn direct_colour_layer(mode: u8, layer: Layer) -> bool {
    layer == Layer::Bg1 && matches!(mode, 3 | 4 | 7)
}

/// `BBGGGRRR` plus the map entry's `bgr` low bits becomes `BBb00 RRRr0 GGGg0`
/// (fullsnes "Direct Color Mode"). Index 0 is transparent and never reaches here.
fn direct_colour(index: u8, palette: u8) -> u16 {
    let red = (u16::from(index & 0x07) << 2) | (u16::from(palette & 0x01) << 1);
    let green = (u16::from((index >> 3) & 0x07) << 2) | (u16::from(palette >> 1 & 0x01) << 1);
    let blue = (u16::from((index >> 6) & 0x03) << 3) | (u16::from(palette >> 2 & 0x01) << 2);
    (blue << 10) | (green << 5) | red
}

/// §5.7: the fixed colour when the sub screen has no pixel there, else the sub pixel's colour.
fn sub_colour(ppu: &Ppu, regs: &PpuRegs, mode: u8, sub: Pixel) -> (u16, bool) {
    let uses_layers = regs.cgwsel & 0x02 != 0;
    if uses_layers && sub.layer != Layer::Backdrop {
        (resolve(ppu, regs, mode, sub), true)
    } else {
        (ppu.fixed_colour, false)
    }
}

/// §5.7: the math layer bit for the frontmost main pixel, if the operation can apply to it at all.
fn math_bit(main: Pixel) -> Option<u8> {
    match main.layer {
        Layer::Backdrop => Some(5),
        // OBJ palettes 0-3 are never mathed; palettes 4-7 share `CGADSUB` bit 4 (fullsnes
        // "2131h - CGADSUB").
        Layer::Obj => {
            if main.palette >= 12 {
                Some(4)
            } else {
                None
            }
        }
        Layer::Bg1 => Some(0),
        Layer::Bg2 => Some(1),
        Layer::Bg3 => Some(2),
        Layer::Bg4 => Some(3),
    }
}

/// `CGWSEL` bits 5-4: 0 always, 1 the math window, 2 outside it, 3 never.
fn math_enabled(regs: &PpuRegs, x: u8) -> bool {
    match (regs.cgwsel >> 4) & 0x03 {
        0 => true,
        1 => inside_math_window(regs, x),
        2 => !inside_math_window(regs, x),
        _ => false,
    }
}

/// `CGWSEL` bits 7-6: 3 always, 2 the math window, 1 outside it, 0 never.
fn force_black(regs: &PpuRegs, x: u8) -> bool {
    match (regs.cgwsel >> 6) & 0x03 {
        3 => true,
        2 => inside_math_window(regs, x),
        1 => !inside_math_window(regs, x),
        _ => false,
    }
}

/// §5.7's arithmetic on one line, one pixel: force black, the layer condition, the operation, the
/// half divide and the clamp.
#[allow(clippy::too_many_arguments)]
fn math(
    ppu: &Ppu,
    regs: &PpuRegs,
    mode: u8,
    x: u8,
    main: Pixel,
    sub: Pixel,
    main_colour: u16,
) -> u16 {
    let black = force_black(regs, x);
    let base = if black { 0 } else { main_colour };
    if !math_enabled(regs, x) {
        return base;
    }
    let Some(bit) = math_bit(main) else {
        return base;
    };
    if regs.cgadsub >> bit & 0x01 == 0 {
        return base;
    }
    let (sub_value, sub_has_pixel) = sub_colour(ppu, regs, mode, sub);
    // No priority condition: the sub screen's pixel is the other operand wherever it comes from.
    // Wiki `Color_math` lists the rejections as a closed set — `CGADSUB`'s layer bits, OBJ palettes
    // 0-3, the colour window — and says the masking choice "is made based on the main screen only,
    // if we wished to blend with sprites using these palettes we could place them on the sub screen
    // instead". fullsnes states a main/sub priority comparison but marks its own wording "XXX or is
    // it same or lower -- or is it ANY priority?", so it is not a settled rule. Measured on the
    // 763-image library, removing v1's version of that condition (skip when the sub pixel is ahead
    // of the main one) moves 2 of a 24-title sample — DKC's logo glow, Mortal Kombat II's leftmost
    // column — and leaves the other 22 byte-identical.
    // The half divide is ignored on a transparent sub pixel and under force-black, and is not
    // ignored when `CGWSEL` bit 1 turned the sub screen's layers off (fullsnes "2131h - CGADSUB").
    let half = regs.cgadsub & 0x40 != 0 && !black && (sub_has_pixel || regs.cgwsel & 0x02 == 0);
    let subtract = regs.cgadsub & 0x80 != 0;
    let mut out = 0u16;
    for shift in [0u32, 5, 10] {
        let m = i32::from((base >> shift) & 0x1F);
        let s = i32::from((sub_value >> shift) & 0x1F);
        let mut value = if subtract { m - s } else { m + s };
        if half {
            value /= 2;
        }
        out |= (value.clamp(0, 31) as u16) << shift;
    }
    out
}

/// `INIDISP` bits 3-0: `N = 0` is black, otherwise the channel is multiplied by `N+1` and divided
/// by 16 (fullsnes "2100h - INIDISP": "0=Screen Black, or N=1..15: Brightness*(N+1)/16").
fn brighten(colour: u16, brightness: u8) -> u16 {
    if brightness == 0 {
        return 0;
    }
    let factor = u32::from(brightness) + 1;
    let mut out = 0u16;
    for shift in [0u32, 5, 10] {
        let channel = u32::from((colour >> shift) & 0x1F);
        out |= (((channel * factor) >> 4) as u16) << shift;
    }
    out
}
