//! Where the picture goes in the window (DESIGN §6.1, §7.2).
//!
//! A stub with the whole rule in it: the largest whole multiple of the machine's own size that
//! fits, centred, in the area above the status bar. #18 replaces the body with the filters and
//! the per-system sizes; the signatures stay.

use crate::app::App;
use crate::menu::{Row, RowAct};
use crate::status_bar::BAR_H;

/// The picture rules the window runs under.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayOptions {
    /// Whole multiples only, the default (§7.2).
    pub integer_scale: bool,
    /// The status bar takes `BAR_H` off the bottom.
    pub bar: bool,
}

/// `(x, y, w, h)` in physical pixels for `picture` (the machine's own `(width, height)`).
///
/// `dpi` is the window's scale factor: the rect is computed in logical pixels and multiplied, so
/// the picture is the same fraction of the window on a 2x display.
pub fn display_rect(
    picture: (u32, u32),
    options: &DisplayOptions,
    win_w: f32,
    win_h: f32,
    dpi: f32,
) -> (f32, f32, f32, f32) {
    let dpi = if dpi > 0.0 { dpi } else { 1.0 };
    let (win_w, win_h) = (win_w / dpi, win_h / dpi);
    let bar = if options.bar { BAR_H } else { 0.0 };
    let area_h = (win_h - bar).max(0.0);
    let (pw, ph) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
    if !options.integer_scale {
        return (0.0, 0.0, win_w * dpi, area_h * dpi);
    }
    let scale = (win_w / pw).floor().min((area_h / ph).floor()).max(1.0);
    let (w, h) = (pw * scale, ph * scale);
    let x = ((win_w - w) / 2.0).floor();
    let y = ((area_h - h) / 2.0).floor();
    (x * dpi, y * dpi, w * dpi, h * dpi)
}

/// The window size for a picture at `scale`, with the bar under it.
pub fn window_size(picture: (u32, u32), scale: u32) -> (i32, i32) {
    let scale = scale.max(1);
    (
        (picture.0.max(1) * scale) as i32,
        (picture.1.max(1) * scale) as i32 + BAR_H as i32,
    )
}

/// No rows until the display settings arrive.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet: the loop reads the options it was given.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_integer_scale_centres_the_picture() {
        let options = DisplayOptions {
            integer_scale: true,
            bar: true,
        };
        assert_eq!(
            display_rect((160, 144), &options, 640.0, 600.0, 1.0),
            (0.0, 0.0, 640.0, 576.0)
        );
    }

    #[test]
    fn a_stretched_picture_fills_the_area() {
        let options = DisplayOptions::default();
        assert_eq!(
            display_rect((160, 144), &options, 500.0, 500.0, 1.0),
            (0.0, 0.0, 500.0, 500.0)
        );
    }

    #[test]
    fn the_multiple_is_the_one_the_window_allows() {
        let options = DisplayOptions {
            integer_scale: true,
            bar: true,
        };
        // 700x600 with the bar is a 576 px picture area: 4 x 160 and 4 x 144 fit, 5 x 160 does not.
        let (x, y, w, h) = display_rect((160, 144), &options, 700.0, 600.0, 1.0);
        assert_eq!((w, h), (640.0, 576.0));
        assert_eq!(x, 30.0);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn a_two_x_display_doubles_the_physical_rect() {
        let options = DisplayOptions {
            integer_scale: true,
            bar: true,
        };
        let logical = display_rect((160, 144), &options, 640.0, 600.0, 1.0);
        let physical = display_rect((160, 144), &options, 1280.0, 1200.0, 2.0);
        assert_eq!(
            physical,
            (
                logical.0 * 2.0,
                logical.1 * 2.0,
                logical.2 * 2.0,
                logical.3 * 2.0
            )
        );
    }

    #[test]
    fn the_window_is_the_picture_at_the_scale_plus_the_bar() {
        assert_eq!(window_size((160, 144), 3), (480, 456));
        assert_eq!(window_size((256, 224), 3), (768, 696));
        assert_eq!(window_size((240, 160), 0), (240, 184));
    }
}
