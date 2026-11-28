//! The windowed binary: one window for every core (DESIGN §6.1, §7.2).
//!
//! `tendyemu <file>` opens the file, and with no argument the window waits with the drawer's Open
//! ROM row. The command line is read before the window exists, so `--help`, a refused flag and a
//! headless sweep never flash one (issue #84). The loop passes the frame time to `App::tick`,
//! performs what the drawer asked for and paints; everything that can be decided without a window
//! lives in the library half.

use std::ffi::OsString;

use macroquad::prelude::*;
use tendyemu::app::App;
use tendyemu::display::{self, Follower, Picture};
use tendyemu::draw;
use tendyemu::file_pick;
use tendyemu::headless::{self, Command};
use tendyemu::menu;

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

fn main() {
    // The whole line is the library's to read (issue #84), so `--help`, a refused flag and a
    // sweep exit before the window exists. The file is still the one argument the line names —
    // `args_os().nth(1)` when it comes first, as it did when the window read one argument.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let command = match headless::parse(&args) {
        Ok(command) => command,
        Err(why) => {
            eprintln!("tendyemu: {why}");
            eprintln!("{}", headless::USAGE);
            std::process::exit(2);
        }
    };
    if command.help {
        print!("{}", headless::HELP);
        return;
    }
    if command.headless {
        std::process::exit(headless::cli(&args));
    }
    macroquad::Window::from_config(window_conf(), run(command));
}

async fn run(command: Command) {
    let mut app = App::with_default_paths();
    file_pick::set_interactive(true);
    if let Err(why) = command.apply(&mut app) {
        app.show_notice(why, true);
    }
    if let Some(path) = &command.path
        && let Err(why) = app.open(path)
    {
        app.show_notice(why, true);
    }

    let mut texture: Option<Texture2D> = None;
    let mut picture_size = (0u16, 0u16);
    let mut follower = Follower::new();
    // The window is created windowed; the first pass asks the display for its mode, so a
    // remembered `fullscreen = true` (or `--fullscreen`) really starts fullscreen.
    let mut fullscreen = false;

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
        let options = display::options(&app);
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

        if options.bar {
            draw::paint_bar(&app.bar(), win_w, win_h);
        }

        if app.menu().is_open() {
            let dw = menu::drawer_width(win_w);
            let entries = app.menu().entries();
            let layout = menu::layout(dw, win_h, &entries, app.menu().first());
            let title = app.cart_title();
            draw::paint_drawer(&title, app.menu(), &entries, &layout, win_h);
            draw::paint_state_thumb(&app, &entries, &layout, win_h, options.bar);
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

    // A game button keeps its key (#74), so a bound Escape neither opens the drawer nor is taken
    // away from the player; the same guard covers M, F1 and Tab below.
    if is_key_pressed(KeyCode::Escape) && !tendyemu::keys::claimed(app, "Escape") {
        app.menu_mut().open();
        app.refresh();
    }
    if is_key_pressed(KeyCode::M) && !tendyemu::keys::claimed(app, "M") {
        app.menu_mut().open();
        app.refresh();
    }
    if is_key_pressed(KeyCode::F1) && !tendyemu::keys::claimed(app, "F1") {
        app.menu_mut().open();
        app.refresh();
    }
    for (code, name) in [
        (KeyCode::P, "P"),
        (KeyCode::F2, "F2"),
        (KeyCode::F3, "F3"),
        (KeyCode::F5, "F5"),
        (KeyCode::F7, "F7"),
    ] {
        if is_key_pressed(code) {
            app.hotkey(name);
        }
    }
    app.set_fast_forward(is_key_down(KeyCode::Tab) && !tendyemu::keys::claimed(app, "Tab"));
    app.set_rewinding(tendyemu::rewind::held(app));
    let held = tendyemu::keys::held_names(app);
    app.set_held_buttons(held);
}
