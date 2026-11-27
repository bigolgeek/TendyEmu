//! Painting the window (DESIGN §6.1, §7.2): the picture, the status bar, the drawer, the refusal
//! screen. Together with `main.rs` this is the only place macroquad is named.

use macroquad::prelude::*;

use crate::menu::{self, Entry, Layout, Menu, RowKind};
use crate::status_bar::{self, BarState};

/// The window's background.
pub const BG: Color = Color::new(0.055, 0.06, 0.075, 1.0);
/// The drawer's panel and the bar.
pub const PANEL: Color = Color::new(0.10, 0.11, 0.14, 1.0);
/// Ordinary text.
pub const TEXT: Color = Color::new(0.90, 0.92, 0.95, 1.0);
/// Headings, values, hints.
pub const DIM: Color = Color::new(0.52, 0.56, 0.62, 1.0);
/// The selected row's fill.
pub const FILL: Color = Color::new(0.17, 0.21, 0.29, 1.0);
/// A value that can be changed.
pub const ACCENT: Color = Color::new(0.45, 0.66, 0.96, 1.0);
/// A refusal.
pub const ERROR: Color = Color::new(0.96, 0.45, 0.45, 1.0);

/// Character width at 16 px, for the fit checks that do not want to measure.
pub const CHAR_W_AT_16: f32 = 7.0;
/// A refusal line is wrapped to this many characters, as the GB front end does.
pub const ERROR_LINE_CHARS: usize = 40;

/// How wide `text` is at `size`, in pixels.
pub fn text_width(text: &str, size: f32) -> f32 {
    measure_text(text, None, size as u16, 1.0).width
}

/// `text` cut to `max_w` with an ellipsis: the drawer never runs a value into the window edge.
pub fn fit_with_ellipsis(text: &str, max_w: f32, size: f32) -> String {
    if text_width(text, size) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut candidate: String = chars.iter().collect();
        candidate.push('…');
        if text_width(&candidate, size) <= max_w {
            return candidate;
        }
    }
    String::new()
}

/// Draws the picture where `display::display_rect` put it.
pub fn blit(texture: &Texture2D, rect: (f32, f32, f32, f32)) {
    let (x, y, w, h) = rect;
    draw_texture_ex(
        texture,
        x,
        y,
        WHITE,
        DrawTextureParams {
            dest_size: Some(vec2(w, h)),
            ..Default::default()
        },
    );
}

/// What the window shows before a game opens.
pub fn paint_idle(win_w: f32, win_h: f32, message: &str) {
    let size = 20.0;
    let width = text_width(message, size);
    draw_text(
        message,
        ((win_w - width) / 2.0).max(8.0),
        win_h / 2.0 - 8.0,
        size,
        DIM,
    );
    let hint = "Esc for the menu";
    let width = text_width(hint, 14.0);
    draw_text(
        hint,
        ((win_w - width) / 2.0).max(8.0),
        win_h / 2.0 + 20.0,
        14.0,
        DIM,
    );
}

/// The status bar: the left-hand words, then the notice in the right-hand slot.
pub fn paint_bar(state: &BarState, win_w: f32, win_h: f32) {
    let top = win_h - status_bar::BAR_H;
    draw_rectangle(0.0, top, win_w, status_bar::BAR_H, PANEL);
    draw_line(0.0, top, win_w, top, 1.0, DIM);

    let size = 13.0;
    let notice_w = state.notice.as_ref().map_or(0.0, |notice| {
        text_width(notice, size) + status_bar::WORD_GAP
    });
    let words = status_bar::status_words(state);
    let width = (win_w - 2.0 * status_bar::PAD - notice_w).max(0.0);
    let (words, dropped) = status_bar::fitted(&words, width, CHAR_W_AT_16);
    let mut x = status_bar::PAD;
    let baseline = top + (status_bar::BAR_H + size) / 2.0 - 2.0;
    for word in &words {
        draw_text(&word.text, x, baseline, size, word_color(state, &word.kind));
        x += text_width(&word.text, size) + status_bar::SEP_W;
    }
    if dropped > 0 {
        draw_text(format!("+{dropped}"), x, baseline, size, DIM);
    }
    if let Some(notice) = &state.notice {
        let width = text_width(notice, size);
        let x = (win_w - width - status_bar::PAD).max(0.0);
        draw_text(
            notice,
            x,
            baseline,
            size,
            if state.notice_error { ERROR } else { TEXT },
        );
    }
}

fn word_color(state: &BarState, kind: &status_bar::WordKind) -> Color {
    match kind {
        status_bar::WordKind::Title => DIM,
        status_bar::WordKind::Word => TEXT,
        status_bar::WordKind::Notice => TEXT,
        status_bar::WordKind::Stat => {
            if state.held {
                DIM
            } else {
                ACCENT
            }
        }
    }
}

/// The drawer: the dimmed window, the panel, the header, the rows, the footer.
pub fn paint_drawer(title: &str, menu: &Menu, entries: &[Entry], layout: &Layout, win_h: f32) {
    let win_w = screen_width();
    draw_rectangle(0.0, 0.0, win_w, win_h, Color::new(0.0, 0.0, 0.0, 0.45));
    draw_rectangle(0.0, 0.0, layout.dw, win_h, PANEL);
    draw_line(layout.dw, 0.0, layout.dw, win_h, 1.0, FILL);

    // Header: where the page is, then its name.
    let parent = menu::parent_line(menu.level(), title);
    draw_text(
        fit_with_ellipsis(&parent, layout.dw - 2.0 * menu::ROW_X, 12.0),
        menu::ROW_X,
        18.0,
        12.0,
        DIM,
    );
    draw_text(
        menu.level().title(),
        menu::ROW_X,
        menu::HEADER_H - 6.0,
        16.0,
        TEXT,
    );
    draw_line(0.0, menu::HEADER_H, layout.dw, menu::HEADER_H, 1.0, FILL);

    for slot in &layout.slots {
        let Some(entry) = entries.get(slot.index) else {
            continue;
        };
        let baseline = slot.y + slot.h * 0.72;
        match entry {
            Entry::Gap => {}
            Entry::Heading(text) => {
                draw_text(text, menu::ROW_X, baseline, 11.0, DIM);
            }
            Entry::Item(item) => {
                let selected = slot.index == menu.cursor();
                if selected {
                    draw_rectangle(
                        menu::ROW_FILL_X,
                        slot.y + 1.0,
                        (layout.dw - menu::ROW_FILL_INSET).max(0.0),
                        slot.h - 2.0,
                        FILL,
                    );
                    draw_rectangle(menu::ROW_FILL_X, slot.y + 1.0, 2.0, slot.h - 2.0, ACCENT);
                }
                let colour = if item.enabled { TEXT } else { DIM };
                let value_w = if item.row.value.is_empty() {
                    0.0
                } else {
                    text_width(&item.row.value, layout.size) + menu::ROW_INSET
                };
                let label_w = layout.dw - menu::ROW_X - menu::ROW_INSET - value_w;
                let label = fit_with_ellipsis(&item.row.label, label_w.max(10.0), layout.size);
                draw_text(&label, menu::ROW_X, baseline, layout.size, colour);
                if !item.row.value.is_empty() {
                    let value = fit_with_ellipsis(
                        &item.row.value,
                        (layout.dw - menu::ROW_X) / 2.0,
                        layout.size,
                    );
                    let x = layout.dw - menu::ROW_INSET - text_width(&value, layout.size);
                    draw_text(&value, x, baseline, layout.size, DIM);
                }
                if slot.arrows && selected {
                    draw_text("<", menu::ROW_X + slot.h, baseline, layout.size, ACCENT);
                    let right = layout.dw - menu::ROW_INSET + 6.0;
                    draw_text(">", right, baseline, layout.size, ACCENT);
                }
                if let RowKind::Pick = item.row.kind {
                    draw_text(
                        "...",
                        menu::ROW_X + label_w.max(10.0),
                        baseline,
                        layout.size,
                        DIM,
                    );
                }
            }
        }
    }

    // Footer: the selected row's description, then the keys.
    let footer_top = win_h - menu::FOOTER_H;
    draw_line(0.0, footer_top, layout.dw, footer_top, 1.0, FILL);
    let desc = menu
        .item()
        .map(|item| item.row.desc)
        .filter(|desc| !desc.is_empty())
        .unwrap_or("");
    let lines = menu::wrap_desc(desc, menu::desc_chars(layout.dw));
    draw_text(
        lines.first().map_or("", String::as_str),
        menu::ROW_X,
        footer_top + 14.0,
        12.0,
        DIM,
    );
    let hints = menu::hints(menu.level(), menu.capture().is_some());
    draw_text(hints[0], menu::ROW_X, footer_top + 28.0, 11.0, DIM);
}

/// The refusal screen: the whole message, wrapped, and how to get back.
pub fn paint_error(message: &str, win_w: f32, win_h: f32) {
    draw_rectangle(0.0, 0.0, win_w, win_h, BG);
    let lines = menu::wrap_desc(message, ERROR_LINE_CHARS);
    let size = 16.0;
    let line_h = 22.0;
    let top = (win_h - lines.len() as f32 * line_h) / 2.0;
    for (index, line) in lines.iter().enumerate() {
        let width = text_width(line, size);
        draw_text(
            line,
            ((win_w - width) / 2.0).max(8.0),
            top + index as f32 * line_h,
            size,
            ERROR,
        );
    }
    let hint = "Esc closes  Enter opens a ROM";
    let width = text_width(hint, 13.0);
    draw_text(
        hint,
        ((win_w - width) / 2.0).max(8.0),
        top + lines.len() as f32 * line_h + 24.0,
        13.0,
        DIM,
    );
}
