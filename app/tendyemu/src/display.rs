//! Where the picture goes in the window (DESIGN §2.4, §6.1, §6.4, §7.2).
//!
//! One window serves five picture shapes: 160x144 (DMG and CGB), 256x224 (a Super Game Boy
//! picture with its border, and a Super NES), 240x160 (GBA) and 256x240 (NES). The running
//! machine's own `frame()` is the size that counts; [`native_size`] is the same fact for callers
//! and tests that have no frame in hand, and [`Follower`] turns a change of the running picture
//! into the window resize §6.4 step 6 asks for.
//!
//! The arithmetic is the GB front end's, kept whole: the integer scale is the largest whole
//! multiple of the picture that fits the area above the status bar, placed on whole physical
//! pixels so the bars are whatever the remainder is. The `4:3` aspect is the NES front end's
//! rule — the console was made for a television, and the NES offered the choice.

use tendy_core::SystemKind;

use crate::app::App;
use crate::config;
use crate::menu::{Row, RowAct, RowKind};
use crate::settings::Direction;
use crate::status_bar::BAR_H;

/// The picture's size in emulated pixels: the one fact the drawer needs from the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
}

impl Picture {
    /// The Game Boy's 160x144 (DESIGN §2.4).
    pub const GAME_BOY: Picture = Picture {
        width: 160,
        height: 144,
    };
    /// A Super Game Boy picture with its border: the 160x144 screen inside a 256x224 frame.
    pub const SGB: Picture = Picture {
        width: 256,
        height: 224,
    };
    /// The Game Boy Advance's 240x160.
    pub const ADVANCE: Picture = Picture {
        width: 240,
        height: 160,
    };
    /// The NES's 256x240.
    pub const NES: Picture = Picture {
        width: 256,
        height: 240,
    };
    /// The Super NES's 256x224: the core never draws 512 wide or 239 lines (DESIGN §3.6 C7).
    pub const SUPER_NES: Picture = Picture {
        width: 256,
        height: 224,
    };

    /// A frame's own size, as `Machine::frame` reports it.
    pub const fn from_frame(frame: (u32, u32)) -> Picture {
        Picture {
            width: frame.0,
            height: frame.1,
        }
    }

    /// Pixels in the picture.
    pub fn pixels(self) -> usize {
        self.width as usize * self.height as usize
    }
}

impl Default for Picture {
    /// The Game Boy, so a caller with no machine yet gets the size this drawer was built for.
    fn default() -> Picture {
        Picture::GAME_BOY
    }
}

/// The native size of `system`, as DESIGN §2.4 declares it.
///
/// `sgb_border` is the Super Game Boy border, the only picture a single core reports at two
/// sizes. Callers with a running machine should prefer the frame's own size.
pub fn native_size(system: SystemKind, sgb_border: bool) -> Picture {
    match (system, sgb_border) {
        (SystemKind::Gb, true) => Picture::SGB,
        (SystemKind::Gb, false) => Picture::GAME_BOY,
        (SystemKind::Gba, _) => Picture::ADVANCE,
        (SystemKind::Nes, _) => Picture::NES,
        (SystemKind::Snes, _) => Picture::SUPER_NES,
    }
}

/// How the picture is fitted to the area: the console's square pixels or the 4:3 shape of the
/// television set it was made for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aspect {
    /// Each picture pixel is square.
    #[default]
    Square,
    /// The picture is drawn in a 4:3 box, as the NES front end offers.
    Four3,
}

/// The smallest integer window scale.
///
/// The three source apps allowed 2..=6 (GB), 1..=10 (NES) and 1..=4 (SNES). One window takes
/// 1..=6: the smallest and largest windows the two smaller sources offered, without the NES's
/// 7..=10 sizes, which no other system's picture was ever drawn at.
pub const MIN_SCALE: u32 = 1;
/// The largest integer window scale: 6.
pub const MAX_SCALE: u32 = 6;
/// The window scale a fresh session starts at (§6.2's `scale = 3`).
pub const DEFAULT_SCALE: u32 = 3;

/// The picture rules the window runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayOptions {
    /// Whole multiples of the picture in the window, from 1 to 6.
    pub scale: u32,
    /// Fill the screen instead of the window.
    pub fullscreen: bool,
    /// Whole multiples only, letterboxed, versus stretched to the area.
    pub integer_scale: bool,
    /// Square pixels or a 4:3 box.
    pub aspect: Aspect,
    /// The status bar takes `BAR_H` off the bottom.
    pub bar: bool,
}

impl DisplayOptions {
    /// The session's starting options: 3x, windowed, whole multiples, square pixels, with the bar.
    pub const DEFAULT: DisplayOptions = DisplayOptions {
        scale: DEFAULT_SCALE,
        fullscreen: false,
        integer_scale: true,
        aspect: Aspect::Square,
        bar: true,
    };

    /// The same options with the scale inside the range the rows offer.
    pub fn clamped(mut self) -> DisplayOptions {
        self.scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        self
    }
}

impl Default for DisplayOptions {
    fn default() -> DisplayOptions {
        DisplayOptions::DEFAULT
    }
}

/// The window and picture rules in force, read from the settings file (§6.2): the running
/// system's section, then `[app]`, then each key's default. With no game the Game Boy section
/// answers, as it does for the settings rows.
pub fn options(app: &App) -> DisplayOptions {
    let system = app.system().unwrap_or(SystemKind::Gb);
    let value = |key: &str| app.setting(system, key);
    let on = |key: &str, default: bool| match value(key).as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    };
    DisplayOptions {
        scale: value("scale")
            .and_then(|word| word.parse().ok())
            .unwrap_or(DEFAULT_SCALE),
        fullscreen: on("fullscreen", false),
        integer_scale: on("integer_scale", true),
        aspect: match value("aspect").as_deref() {
            Some("4:3") => Aspect::Four3,
            _ => Aspect::Square,
        },
        bar: on("status_bar", true),
    }
    .clamped()
}

/// The DPI factor to divide and multiply by, with the values that cannot mean a display (`0`, a
/// negative, `NaN`, `inf`) read as 1.
pub fn sane_dpi(dpi: f32) -> f32 {
    if dpi.is_finite() && dpi > 0.0 {
        dpi
    } else {
        1.0
    }
}

/// The area the picture may use: the window less the bar.
fn picture_area(win_w: f32, win_h: f32, bar: bool) -> (f32, f32) {
    let bar = if bar { BAR_H } else { 0.0 };
    (win_w.max(0.0), (win_h - bar).max(0.0))
}

/// The largest whole multiple of `picture` that fits the picture area, in whole physical pixels.
///
/// With the 4:3 aspect the multiple is of the picture's height: the box is `4/3` as wide, and the
/// factor is the largest whole number whose box still fits. The floor carries a 1e-4 nudge so a
/// division that lands on `4.999_999` still reads as the four it is.
pub fn fit_scale(
    picture: Picture,
    options: &DisplayOptions,
    win_w: f32,
    win_h: f32,
    dpi: f32,
) -> u32 {
    let (area_w, area_h) = picture_area(win_w, win_h, options.bar);
    let dpi = sane_dpi(dpi);
    let (area_w, area_h) = (area_w * dpi, area_h * dpi);
    let width = picture.width.max(1) as f32;
    let height = picture.height.max(1) as f32;
    let factor = match options.aspect {
        Aspect::Square => (area_w / width).min(area_h / height),
        // The tallest 4:3 box the area holds, measured in picture heights.
        Aspect::Four3 => area_h.min(area_w * 3.0 / 4.0) / height,
    };
    if !factor.is_finite() || factor <= 0.0 {
        // A window with no picture area at all: one multiple, so the picture is cropped rather
        // than divided by zero.
        return 1;
    }
    (factor + 0.000_1).floor().max(1.0) as u32
}

/// `(x, y, w, h)` of the picture inside the window, in the window's logical pixels.
///
/// With `integer_scale` on this is the [`fit_scale`] multiple, centred; `x` and `y` are whole
/// physical pixels divided by the DPI, so the bars are the rest of the area on each side. A window
/// too small for one multiple gets 1x: the picture is centred and cropped by the window edges
/// rather than stretched or shrunk to a fraction.
pub fn display_rect(
    picture: Picture,
    options: &DisplayOptions,
    win_w: f32,
    win_h: f32,
    dpi: f32,
) -> (f32, f32, f32, f32) {
    let (area_w, area_h) = picture_area(win_w, win_h, options.bar);
    let dpi = sane_dpi(dpi);
    let (area_w, area_h) = (area_w * dpi, area_h * dpi);
    let width = picture.width.max(1) as f32;
    let height = picture.height.max(1) as f32;

    let (w, h) = if options.integer_scale {
        let n = fit_scale(picture, options, win_w, win_h, dpi) as f32;
        match options.aspect {
            Aspect::Square => (width * n, height * n),
            Aspect::Four3 => (height * n * 4.0 / 3.0, height * n),
        }
    } else {
        match options.aspect {
            Aspect::Square => (area_w, area_h),
            // The largest 4:3 box the area holds.
            Aspect::Four3 => {
                let height = area_h.min(area_w * 3.0 / 4.0);
                (height * 4.0 / 3.0, height)
            }
        }
    };
    // Placement in physical pixels, then back to logical: the picture's edges land on whole
    // device pixels whatever the DPI factor is.
    let x = ((area_w - w) / 2.0).floor();
    let y = ((area_h - h) / 2.0).floor();
    (x / dpi, y / dpi, w / dpi, h / dpi)
}

/// The window size for `picture` at the options' scale, with the bar under it.
///
/// The window takes the picture's shape: square pixels are the picture's own size times the
/// scale, and the 4:3 aspect is a box `4/3` as wide as the picture is tall, so the picture fills
/// the window it asks for instead of sitting in letterbox bars either side.
pub fn window_size(picture: Picture, options: &DisplayOptions) -> (i32, i32) {
    let scale = options.scale.clamp(MIN_SCALE, MAX_SCALE);
    let height = picture.height.max(1).saturating_mul(scale);
    let width = match options.aspect {
        Aspect::Square => picture.width.max(1).saturating_mul(scale),
        Aspect::Four3 => height.saturating_mul(4) / 3,
    };
    let height = height.saturating_add(if options.bar { BAR_H as u32 } else { 0 });
    (
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    )
}

/// The window size a run of pictures asks for, so `main.rs` resizes when — and only when — the
/// running picture or the scale and bar change.
///
/// A window the user drags is left alone: `follow` answers `None` until the inputs differ from
/// the last ones it answered. `picture` is `None` while no game runs, and the next game resizes
/// again even when it has the shape of the last one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Follower {
    last: Option<(Picture, u32, bool, Aspect)>,
}

impl Follower {
    pub fn new() -> Follower {
        Follower::default()
    }

    /// The size the window should take now, or `None` when it is already the right one.
    pub fn follow(
        &mut self,
        picture: Option<Picture>,
        options: DisplayOptions,
    ) -> Option<(i32, i32)> {
        let scale = options.scale.clamp(MIN_SCALE, MAX_SCALE);
        let want = picture.map(|picture| (picture, scale, options.bar, options.aspect));
        if want == self.last {
            return None;
        }
        self.last = want;
        want.map(|(picture, _, _, _)| window_size(picture, &options))
    }
}

/// The most picture pixels a filter buffer may hold: 2 000 000, 8 MB of RGBA, past which the
/// filter pass costs more than the emulator.
pub const BUFFER_BUDGET: usize = 2_000_000;

/// The scale a filter buffer is rendered at: `scale` itself, or its largest whole divisor that
/// keeps the buffer inside [`BUFFER_BUDGET`] picture pixels.
///
/// The buffer is `width x scale` by `height x scale`, so its size grows with the square of the
/// scale. The GB LCD filter (#25) draws through this.
pub fn buffer_scale(picture: Picture, scale: u32) -> u32 {
    let scale = scale.max(1);
    let per_pixel = picture.pixels().max(1);
    let over = |s: u32| {
        (s as usize)
            .saturating_mul(s as usize)
            .saturating_mul(per_pixel)
            > BUFFER_BUDGET
    };
    if !over(scale) {
        return scale;
    }
    (1..scale)
        .rev()
        .find(|s| scale.is_multiple_of(*s) && !over(*s))
        .unwrap_or(1)
}

/// One of the display module's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Scale,
    Fullscreen,
    IntegerScale,
    Aspect,
}

/// The rows this picture gets, in drawer order.
///
/// The NES keeps its 4:3 aspect row; the handhelds' pixels are square, so the row would offer a
/// shape their screens never had. The GBA's section has no `integer_scale` key (#21), so it gets
/// no row to write; its picture is always drawn in whole multiples.
pub fn row_list(system: Option<SystemKind>) -> Vec<DisplayRow> {
    let mut rows = vec![DisplayRow::Scale, DisplayRow::Fullscreen];
    if system != Some(SystemKind::Gba) {
        rows.push(DisplayRow::IntegerScale);
    }
    if system == Some(SystemKind::Nes) {
        rows.push(DisplayRow::Aspect);
    }
    rows
}

/// The DISPLAY group of the drawer.
pub fn rows(app: &App) -> Vec<Row> {
    let options = options(app);
    row_list(app.system())
        .into_iter()
        .map(|row| {
            let (label, value, kind, desc) = match row {
                DisplayRow::Scale => (
                    "Scale",
                    format!("{}x", options.scale),
                    RowKind::Choice,
                    "Window size: whole multiples of the picture, 1x to 6x.",
                ),
                DisplayRow::Fullscreen => (
                    "Fullscreen",
                    on_off(options.fullscreen).to_string(),
                    RowKind::Toggle,
                    "Fill the screen; the picture keeps its shape.",
                ),
                DisplayRow::IntegerScale => (
                    "Integer scale",
                    on_off(options.integer_scale).to_string(),
                    RowKind::Toggle,
                    "Whole-multiple pixels, letterboxed, instead of stretching.",
                ),
                DisplayRow::Aspect => (
                    "Aspect",
                    match options.aspect {
                        Aspect::Square => "Square",
                        Aspect::Four3 => "4:3",
                    }
                    .to_string(),
                    RowKind::Choice,
                    "Square pixels, or the 4:3 shape of the set the NES was made for.",
                ),
            };
            Row {
                label: label.to_string(),
                value,
                kind,
                desc,
                hotkey: "",
                systems: crate::menu::Systems::ALL,
                requires: None,
                act: None,
            }
        })
        .collect()
}

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

/// What one row action does to the options, or `None` when nothing changes.
///
/// Pure, so the stepping rule is tested without touching the module's single session value.
pub fn stepped(options: DisplayOptions, row: DisplayRow, act: &RowAct) -> Option<DisplayOptions> {
    let direction = match act {
        RowAct::Step(direction) => *direction,
        RowAct::Activate => Direction::Right,
        RowAct::Key(_) => return None,
    };
    let mut next = options;
    match row {
        DisplayRow::Scale => {
            next.scale = match direction {
                Direction::Left => options.scale.saturating_sub(1),
                Direction::Right => options.scale.saturating_add(1),
            };
        }
        DisplayRow::Fullscreen => next.fullscreen = !options.fullscreen,
        DisplayRow::IntegerScale => next.integer_scale = !options.integer_scale,
        DisplayRow::Aspect => {
            next.aspect = match options.aspect {
                Aspect::Square => Aspect::Four3,
                Aspect::Four3 => Aspect::Square,
            }
        }
    }
    let next = next.clamped();
    (next != options).then_some(next)
}

/// A DISPLAY row's action: writes the value to the section that owns the key.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    let Some(row) = row_list(app.system()).get(index).copied() else {
        return false;
    };
    let Some(next) = stepped(options(app), row, &act) else {
        return false;
    };
    let system = app.system().unwrap_or(SystemKind::Gb);
    let (section, key, value) = match row {
        DisplayRow::Scale => (config::APP, "scale", next.scale.to_string()),
        DisplayRow::Fullscreen => (config::APP, "fullscreen", next.fullscreen.to_string()),
        DisplayRow::IntegerScale => (
            system.key(),
            "integer_scale",
            next.integer_scale.to_string(),
        ),
        DisplayRow::Aspect => (
            SystemKind::Nes.key(),
            "aspect",
            match next.aspect {
                Aspect::Square => "square",
                Aspect::Four3 => "4:3",
            }
            .to_string(),
        ),
    };
    match config::write_value(app.paths(), section, key, &value) {
        Ok(()) => true,
        Err(why) => {
            app.show_notice(format!("display setting not written: {why}"), true);
            false
        }
    }
}

/// Nothing to do per frame: the options are read from the settings file where they are used.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_BAR: DisplayOptions = DisplayOptions::DEFAULT;
    const GAME_BOY: Picture = Picture::GAME_BOY;

    #[test]
    fn the_fit_is_the_largest_multiple_that_fits_and_never_zero() {
        // 640x600 with the bar is 640x576: 4 x 160 is 640 and 4 x 144 is 576, exactly four.
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 640.0, 600.0, 1.0), 4);
        // One pixel narrower and four no longer fits.
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 639.0, 600.0, 1.0), 3);
        // A window with no picture area still answers one, not zero.
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 640.0, 10.0, 1.0), 1);
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 0.0, 0.0, 1.0), 1);
    }

    #[test]
    fn a_hundred_percent_dpi_keeps_the_rect_whole_and_the_bars_are_the_remainder() {
        let (x, y, w, h) = display_rect(GAME_BOY, &WITH_BAR, 777.0, 601.0, 1.0);
        assert_eq!((w, h), (640.0, 576.0), "4x");
        assert_eq!((x, y), (68.0, 0.0));
        // 777 - 640 - 68 = 69 px of bar on the right, the odd one there because x floored;
        // 577 - 576 = 1 px below the picture for the same reason.
        assert_eq!(777.0 - (x + w), 69.0);
        assert_eq!((601.0 - BAR_H) - (y + h), 1.0);
    }

    #[test]
    fn a_two_times_dpi_draws_the_same_logical_picture_at_twice_the_physical_texels() {
        // 640x600 logical at 200%: 1280x1152 of picture area. 160x144 fits eight times.
        let (x, y, w, h) = display_rect(GAME_BOY, &WITH_BAR, 640.0, 600.0, 2.0);
        assert_eq!(
            (w, h),
            (640.0, 576.0),
            "eight physical multiples, four logical"
        );
        assert_eq!((x, y), (0.0, 0.0));
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 640.0, 600.0, 2.0), 8);
    }

    #[test]
    fn a_fractional_dpi_keeps_the_pictures_edges_on_whole_physical_pixels() {
        // 800x720 logical at 125%: a 1000x870 picture area. Six multiples of 160x144 are 960x864.
        let (x, y, w, h) = display_rect(GAME_BOY, &WITH_BAR, 800.0, 720.0, 1.25);
        assert_eq!(fit_scale(GAME_BOY, &WITH_BAR, 800.0, 720.0, 1.25), 6);
        assert_eq!((w * 1.25, h * 1.25), (960.0, 864.0));
        // The bars are whole numbers of physical pixels: 20 / 1.25 logical is 16 physical.
        assert_eq!((x * 1.25, y * 1.25), (20.0, 3.0));
    }

    #[test]
    fn a_window_smaller_than_the_picture_is_cropped_not_shrunk() {
        let (x, y, w, h) = display_rect(Picture::ADVANCE, &WITH_BAR, 100.0, 80.0, 1.0);
        assert_eq!((w, h), (240.0, 160.0), "one multiple, the floor");
        // The 56 px picture area is narrower and shorter than the picture, so the rect's offsets
        // are negative: centred, and cropped at the window's own edges.
        assert_eq!((x, y), (-70.0, -52.0));
    }

    #[test]
    fn a_stretched_picture_fills_the_area() {
        let options = DisplayOptions {
            integer_scale: false,
            ..DisplayOptions::DEFAULT
        };
        assert_eq!(
            display_rect(GAME_BOY, &options, 500.0, 500.0, 1.0),
            (0.0, 0.0, 500.0, 476.0)
        );
    }

    #[test]
    fn the_window_is_the_picture_at_the_scale_plus_the_bar() {
        let three = DisplayOptions::DEFAULT;
        assert_eq!(window_size(GAME_BOY, &three), (480, 456));
        assert_eq!(window_size(Picture::SGB, &three), (768, 696));
        assert_eq!(window_size(Picture::NES, &three), (768, 744));
        let without_bar = DisplayOptions {
            bar: false,
            ..three
        };
        assert_eq!(window_size(GAME_BOY, &without_bar), (480, 432));
        // A 4:3 window is the box the picture is drawn in: 240 px tall at 3x is 720, so 960 wide.
        let four3 = DisplayOptions {
            aspect: Aspect::Four3,
            ..three
        };
        assert_eq!(window_size(Picture::NES, &four3), (960, 744));
    }

    #[test]
    fn the_buffer_is_a_divisor_of_the_drawn_scale() {
        assert_eq!(buffer_scale(GAME_BOY, 4), 4);
        assert_eq!(buffer_scale(GAME_BOY, 1), 1);
        // 160x96 by 144x96 is 212.9 million buffer pixels: over the budget, and the largest exact
        // divisor that fits is 8 (160x8 by 144x8 is 1 474 560 pixels).
        assert_eq!(buffer_scale(GAME_BOY, 96), 8);
        // A prime scale has only itself and one. 41x of a 240x160 picture is 64.6 million buffer
        // pixels, so the budget takes the single-pixel buffer and the GPU repeats it 41 times.
        assert_eq!(buffer_scale(Picture::ADVANCE, 41), 1);
        // 6x of 256x240 is 2.2 million pixels, just over; three is the divisor that fits.
        assert_eq!(buffer_scale(Picture::NES, 6), 3);
    }

    #[test]
    fn the_four_three_aspect_keeps_the_boxes_shape_and_fits() {
        let options = DisplayOptions {
            aspect: Aspect::Four3,
            ..DisplayOptions::DEFAULT
        };
        // 1000x700 with the bar leaves a 1000x676 area: one NES picture is 320x240 at 4:3, two
        // are 640x480, and three (960x720) are too tall.
        let (x, y, w, h) = display_rect(Picture::NES, &options, 1000.0, 700.0, 1.0);
        assert_eq!((w, h), (640.0, 480.0));
        assert_eq!((x, y), (180.0, 98.0));
        // Stretched, the box is the largest 4:3 shape the area holds.
        let stretched = DisplayOptions {
            integer_scale: false,
            ..options
        };
        let (_, _, w, h) = display_rect(Picture::NES, &stretched, 1000.0, 700.0, 1.0);
        assert!((w - 901.333_3).abs() < 0.01, "{w}");
        assert!((h - 676.0).abs() < 0.01, "{h}");
        assert!((w / h - 4.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn the_nes_keeps_its_aspect_row_and_the_others_do_not() {
        assert!(row_list(Some(SystemKind::Nes)).contains(&DisplayRow::Aspect));
        for system in [
            None,
            Some(SystemKind::Gb),
            Some(SystemKind::Gba),
            Some(SystemKind::Snes),
        ] {
            assert!(
                !row_list(system).contains(&DisplayRow::Aspect),
                "{system:?}"
            );
        }
        assert_eq!(
            row_list(Some(SystemKind::Gb)),
            vec![
                DisplayRow::Scale,
                DisplayRow::Fullscreen,
                DisplayRow::IntegerScale
            ]
        );
        assert_eq!(
            row_list(Some(SystemKind::Nes)),
            vec![
                DisplayRow::Scale,
                DisplayRow::Fullscreen,
                DisplayRow::IntegerScale,
                DisplayRow::Aspect
            ]
        );
        assert_eq!(
            row_list(Some(SystemKind::Gba)),
            vec![DisplayRow::Scale, DisplayRow::Fullscreen],
            "the GBA section has no integer-scale key to write"
        );
    }

    #[test]
    fn a_scale_step_stops_at_the_ends_and_a_toggle_flips() {
        let top = DisplayOptions {
            scale: MAX_SCALE,
            ..DisplayOptions::DEFAULT
        };
        assert_eq!(
            stepped(top, DisplayRow::Scale, &RowAct::Step(Direction::Right)),
            None
        );
        let bottom = DisplayOptions {
            scale: MIN_SCALE,
            ..DisplayOptions::DEFAULT
        };
        assert_eq!(
            stepped(bottom, DisplayRow::Scale, &RowAct::Step(Direction::Left)),
            None
        );
        let up = stepped(bottom, DisplayRow::Scale, &RowAct::Step(Direction::Right))
            .expect("a step inside the range changes the scale");
        assert_eq!(up.scale, MIN_SCALE + 1);
        let flipped = stepped(
            DisplayOptions::DEFAULT,
            DisplayRow::IntegerScale,
            &RowAct::Activate,
        )
        .expect("an activation flips a toggle");
        assert!(!flipped.integer_scale);
        let aspect = stepped(
            DisplayOptions::DEFAULT,
            DisplayRow::Aspect,
            &RowAct::Step(Direction::Right),
        )
        .expect("the aspect flips");
        assert_eq!(aspect.aspect, Aspect::Four3);
    }

    #[test]
    fn a_follower_answers_only_when_the_picture_or_the_scale_changes() {
        let mut follower = Follower::new();
        let options = DisplayOptions::DEFAULT;
        assert_eq!(
            follower.follow(Some(GAME_BOY), options),
            Some(window_size(GAME_BOY, &options))
        );
        assert_eq!(follower.follow(Some(GAME_BOY), options), None);
        let bigger = DisplayOptions {
            scale: 4,
            ..options
        };
        assert_eq!(
            follower.follow(Some(GAME_BOY), bigger),
            Some(window_size(GAME_BOY, &bigger))
        );
        assert_eq!(
            follower.follow(Some(Picture::NES), bigger),
            Some(window_size(Picture::NES, &bigger)),
            "a new picture resizes, even at the same scale"
        );
        assert_eq!(follower.follow(None, bigger), None);
        assert_eq!(
            follower.follow(Some(Picture::NES), bigger),
            Some(window_size(Picture::NES, &bigger)),
            "the game after an idle window is sized again"
        );
    }

    #[test]
    fn the_five_native_sizes_are_the_design_declared_ones() {
        assert_eq!(native_size(SystemKind::Gb, false), Picture::GAME_BOY);
        assert_eq!(native_size(SystemKind::Gb, true), Picture::SGB);
        assert_eq!(native_size(SystemKind::Gba, false), Picture::ADVANCE);
        assert_eq!(native_size(SystemKind::Nes, false), Picture::NES);
        assert_eq!(native_size(SystemKind::Snes, false), Picture::SUPER_NES);
        assert_eq!(
            (Picture::GAME_BOY.width, Picture::GAME_BOY.height),
            (160, 144)
        );
        assert_eq!((Picture::SGB.width, Picture::SGB.height), (256, 224));
        assert_eq!(
            (Picture::ADVANCE.width, Picture::ADVANCE.height),
            (240, 160)
        );
        assert_eq!((Picture::NES.width, Picture::NES.height), (256, 240));
        assert_eq!(
            (Picture::SUPER_NES.width, Picture::SUPER_NES.height),
            (256, 224)
        );
    }
}
