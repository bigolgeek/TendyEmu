//! The windowed binary: one window for every core (DESIGN §6.1, §7.2).
//!
//! `tendyemu <file>` opens the file, and with no argument the window waits with the drawer's Open
//! ROM row. The loop passes the frame time to `App::tick`, performs what the drawer asked for and
//! paints; everything that can be decided without a window lives in the library half.

use std::path::PathBuf;

use macroquad::prelude::*;
use tendyemu::app::App;
use tendyemu::display::{self, Follower, Picture};
use tendyemu::draw;
use tendyemu::file_pick;
use tendyemu::menu;
use tendyemu::timing::SPEEDS;

fn window_conf() -> Conf {
    Conf {
        window_title: "TendyEmu".to_string(),
        window_width: 960,
        window_height: 720,
        high_dpi: true,
        window_resizable: true,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let mut app = App::with_default_paths();
    file_pick::set_interactive(true);
    if let Some(argument) = std::env::args_os().nth(1) {
        let path = PathBuf::from(argument);
        if let Err(why) = app.open(&path) {
            app.show_notice(why, true);
        }
    }

    let mut texture: Option<Texture2D> = None;
    let mut picture_size = (0u16, 0u16);
    let mut follower = Follower::new();
    let mut fullscreen = display::options().fullscreen;

    loop {
        if is_quit_requested() || app.quit_requested() {
            break;
        }
        handle_keys(&mut app);
        while let Some(request) = app.take_request() {
            app.request(request);
        }

        app.tick(get_frame_time() as f64);
        while let Some(request) = app.take_request() {
            app.request(request);
        }

        let win_w = screen_width();
        let win_h = screen_height();
        let options = display::options();
        if options.fullscreen != fullscreen {
            set_fullscreen(options.fullscreen);
            fullscreen = options.fullscreen;
            // The display mode owns the size while fullscreen is on; on the way out the picture
            // sizes the window again, as the NES front end does.
            if !fullscreen {
                follower = Follower::new();
            }
        }
        clear_background(draw::BG);

        if let Some(frame) = app.frame() {
            let (width, height) = (frame.width as u16, frame.height as u16);
            if picture_size != (width, height) || texture.is_none() {
                let made = Texture2D::from_rgba8(width, height, frame.rgba);
                made.set_filter(FilterMode::Nearest);
                texture = Some(made);
                picture_size = (width, height);
            } else if let Some(texture) = &texture {
                texture.update_from_bytes(u32::from(width), u32::from(height), frame.rgba);
            }
            // The picture's size is the machine's, never a constant: a Super Game Boy border or
            // a core that changes shape mid-game resizes the window by the same rule (#18). While
            // fullscreen the display mode owns the size.
            let picture = Picture::from_frame((u32::from(width), u32::from(height)));
            if !options.fullscreen
                && let Some((w, h)) = follower.follow(Some(picture), options)
            {
                request_new_screen_size(w as f32, h as f32);
            }
            if let Some(texture) = &texture {
                draw::blit(
                    texture,
                    display::display_rect(picture, &options, win_w, win_h, screen_dpi_scale()),
                );
            }
        } else {
            texture = None;
            picture_size = (0, 0);
            follower.follow(None, options);
            match app.refusal() {
                Some(why) => draw::paint_error(why, win_w, win_h),
                None => draw::paint_idle(win_w, win_h, "Open a ROM to start"),
            }
        }

        draw::paint_bar(&app.bar(), win_w, win_h);

        if app.menu().is_open() {
            let dw = menu::drawer_width(win_w);
            let entries = app.menu().entries();
            let layout = menu::layout(dw, win_h, &entries, app.menu().first());
            let title = app.cart_title();
            draw::paint_drawer(&title, app.menu(), &entries, &layout, win_h);
        }

        next_frame().await;
    }

    app.write_battery();
}

/// The drawer's navigation keys, the session keys, and opening the drawer itself.
fn handle_keys(app: &mut App) {
    if app.menu().is_open() {
        let mut consumed = false;
        for (code, name) in [
            (KeyCode::Up, "Up"),
            (KeyCode::Down, "Down"),
            (KeyCode::Left, "Left"),
            (KeyCode::Right, "Right"),
            (KeyCode::Enter, "Enter"),
            (KeyCode::KpEnter, "Enter"),
            (KeyCode::Escape, "Escape"),
        ] {
            if is_key_pressed(code) {
                consumed |= menu::key(app.menu_mut(), name);
            }
        }
        if consumed {
            app.refresh();
        }
        if is_mouse_button_pressed(MouseButton::Left) {
            let (x, y) = mouse_position();
            let dw = menu::drawer_width(screen_width());
            let entries = app.menu().entries();
            let layout = menu::layout(dw, screen_height(), &entries, app.menu().first());
            if let Some(index) = menu::hit(&layout, x, y) {
                menu::click(app.menu_mut(), index, x);
                app.refresh();
            }
        }
        app.set_held_buttons(Vec::new());
        return;
    }

    // The refusal screen: Enter opens a ROM, Esc clears the refusal and shows the idle window.
    if app.refusal().is_some() && app.machine().is_none() {
        if is_key_pressed(KeyCode::Enter) {
            let _ = app.request(menu::Request::OpenRom);
        }
        if is_key_pressed(KeyCode::Escape) {
            app.dismiss_refusal();
        }
        app.set_held_buttons(Vec::new());
        return;
    }

    if is_key_pressed(KeyCode::Escape) || is_key_pressed(KeyCode::M) || is_key_pressed(KeyCode::F1)
    {
        app.menu_mut().open();
        app.refresh();
    }
    if is_key_pressed(KeyCode::P) {
        app.toggle_pause();
    }
    if is_key_pressed(KeyCode::F2) {
        app.cycle_speed();
    }
    if is_key_pressed(KeyCode::F3) {
        let at = SPEEDS
            .iter()
            .position(|speed| (*speed - app.speed()).abs() < f64::EPSILON)
            .unwrap_or(0);
        app.set_speed(SPEEDS[(at + SPEEDS.len() - 1) % SPEEDS.len()]);
    }
    app.set_fast_forward(is_key_down(KeyCode::Tab));
    let held = tendyemu::keys::held_names(app);
    app.set_held_buttons(held);
}
