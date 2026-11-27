//! The app: one running machine, the drawer's state, and the per-frame hook order
//! (DESIGN §6.1, §6.4).
//!
//! No window type appears in this file. `main.rs` drives [`App::tick`] and paints what the
//! accessors return, so every rule here — opening a file, a refusal that leaves the running game
//! alone, the battery on close, the recents list — is testable headless.

use std::path::{Path, PathBuf};

use tendy_core::{Capabilities, Frame, Machine, SystemKind};

use crate::battery;
use crate::menu::{self, ItemAct, Level, Menu, Module, Request, RowAct};
use crate::paths::Paths;
use crate::recents::{self, Recent};
use crate::status_bar::{self, BarState};
use crate::timing::{FramePacer, SPEEDS};
use crate::{
    audio, avi, caps, config, display, first_run, gamepad, games, keys, loader, record, rewind,
    screenshot, settings, states, switch, thumb,
};

/// How long a bucket of frames counts for before the bar's rate moves on.
///
/// One bucket alone cannot report 59.7 on a 144 Hz display: whole frames in half a second come to
/// 29 or 30, which reads 58 or 60. The bar averages the last complete bucket with the one filling.
const FPS_BUCKET_SECS: f64 = 0.5;

/// A line in the status bar's right-hand slot.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    /// An error notice does not time out: it stays until the next one replaces it.
    pub error: bool,
    /// When it went up, in the app's own clock.
    pub at: f64,
}

/// The running app.
pub struct App {
    paths: Paths,
    machine: Option<Box<dyn Machine>>,
    rom: Option<PathBuf>,
    system: Option<SystemKind>,
    menu: Menu,
    pacer: FramePacer,
    recents: Vec<Recent>,
    speed: f64,
    fast_forward: bool,
    paused: bool,
    muted: bool,
    quit: bool,
    held: Vec<String>,
    fps: f64,
    /// Frames and seconds in the bucket filling now, and in the one that closed before it.
    fps_frames: u32,
    fps_secs: f64,
    fps_prev: (u32, f64),
    /// Seconds since the app started, the clock notices age on.
    clock: f64,
    notice: Option<Notice>,
    /// The last refusal, kept whole for the refusal screen.
    refusal: Option<String>,
    sample_rate: u32,
}

impl App {
    /// An app with no game open, reading and writing under `paths`.
    pub fn new(paths: Paths) -> App {
        let recents = recents::load(&paths);
        App {
            paths,
            machine: None,
            rom: None,
            system: None,
            menu: Menu::new(),
            pacer: FramePacer::new(60.0),
            recents,
            speed: 1.0,
            fast_forward: false,
            paused: false,
            muted: false,
            quit: false,
            held: Vec::new(),
            fps: 0.0,
            fps_frames: 0,
            fps_secs: 0.0,
            fps_prev: (0, 0.0),
            clock: 0.0,
            notice: None,
            refusal: None,
            sample_rate: audio::SAMPLE_RATE,
        }
    }

    /// An app reading and writing the default folder (§6.3).
    pub fn with_default_paths() -> App {
        App::new(Paths::default())
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn machine(&self) -> Option<&dyn Machine> {
        self.machine.as_deref()
    }

    pub fn machine_mut(&mut self) -> Option<&mut (dyn Machine + 'static)> {
        match self.machine.as_mut() {
            Some(machine) => Some(machine.as_mut()),
            None => None,
        }
    }

    /// The file the running game was opened from.
    pub fn rom(&self) -> Option<&Path> {
        self.rom.as_deref()
    }

    pub fn system(&self) -> Option<SystemKind> {
        self.system
    }

    /// The running core's capabilities, or none when nothing is open.
    pub fn capabilities(&self) -> Capabilities {
        self.machine
            .as_ref()
            .map_or(Capabilities::NONE, |machine| machine.capabilities())
    }

    pub fn menu(&self) -> &Menu {
        &self.menu
    }

    pub fn menu_mut(&mut self) -> &mut Menu {
        &mut self.menu
    }

    pub fn recents(&self) -> &[Recent] {
        &self.recents
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            self.pacer.reset();
        }
    }

    pub fn toggle_pause(&mut self) {
        self.set_paused(!self.paused);
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// The session speed setting: 1x, 2x, 3x or 4x.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// The speed the pacer runs at right now: the held fast-forward wins over the setting.
    pub fn effective_speed(&self) -> f64 {
        if self.fast_forward { 4.0 } else { self.speed }
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = SPEEDS
            .into_iter()
            .find(|allowed| (*allowed - speed).abs() < f64::EPSILON)
            .unwrap_or(1.0);
    }

    /// Steps to the next speed, wrapping 4x back to 1x.
    pub fn cycle_speed(&mut self) {
        let at = SPEEDS
            .iter()
            .position(|speed| (*speed - self.speed).abs() < f64::EPSILON)
            .unwrap_or(0);
        self.speed = SPEEDS[(at + 1) % SPEEDS.len()];
    }

    /// The session keys by name, one action each: `P` pause, `F2` next speed, `F3` previous
    /// speed, `F5` save the selected state slot, `F7` load it. `false` for a key this window does
    /// not use, or for `F5`/`F7` with no game.
    pub fn hotkey(&mut self, key: &str) -> bool {
        // #74: a game button keeps its key; the window's own key does nothing while one is bound
        // to it, so the player never gets two actions from one press.
        if keys::claimed(self, key) {
            return false;
        }
        match key {
            "P" => {
                self.toggle_pause();
                true
            }
            "F2" => {
                self.cycle_speed();
                true
            }
            "F3" => {
                let at = SPEEDS
                    .iter()
                    .position(|speed| (*speed - self.speed).abs() < f64::EPSILON)
                    .unwrap_or(0);
                self.set_speed(SPEEDS[(at + SPEEDS.len() - 1) % SPEEDS.len()]);
                true
            }
            "F5" => states::save_selected(self),
            "F7" => states::load_selected(self),
            _ => false,
        }
    }

    pub fn fast_forward(&self) -> bool {
        self.fast_forward
    }

    pub fn set_fast_forward(&mut self, on: bool) {
        self.fast_forward = on;
        if on {
            self.pacer.reset();
        }
    }

    /// The rate the loop is reaching, for the bar.
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Whether the loop is held: a pause, the drawer, or no game.
    pub fn held(&self) -> bool {
        self.paused || self.menu.is_open() || self.machine.is_none()
    }

    /// The names of the buttons currently down, as `main.rs` reports them.
    pub fn held_buttons(&self) -> &[String] {
        &self.held
    }

    pub fn set_held_buttons(&mut self, held: Vec<String>) {
        self.held = held;
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate;
        if let Some(machine) = self.machine.as_mut() {
            machine.set_sample_rate(rate);
        }
    }

    /// A settings value, by system and §6.2 key: the game's own override first, then its section,
    /// then `[app]`, then the built-in default. `None` only for a key this build does not know.
    pub fn setting(&self, system: SystemKind, key: &str) -> Option<String> {
        config::setting(self, system, key)
    }

    /// The running game's name for the drawer and the bar: the file's name, else the console's.
    pub fn cart_title(&self) -> String {
        if let Some(name) = self.rom.as_deref().and_then(Path::file_name) {
            return name.to_string_lossy().into_owned();
        }
        self.machine
            .as_ref()
            .map_or_else(String::new, |machine| machine.console_name().to_string())
    }

    /// The bar's state for this frame.
    pub fn bar(&self) -> BarState {
        BarState {
            title: self.cart_title(),
            fps: self.fps,
            target_fps: self.pacer.fps(),
            speed: self.effective_speed(),
            system: self.system,
            cart: self
                .machine
                .as_ref()
                .map_or_else(String::new, |machine| machine.cart_storage().to_string()),
            frame: self.machine.as_ref().map_or(0, |machine| machine.frames()),
            muted: self.muted,
            paused: self.paused,
            held: self.held(),
            notice: self.notice.as_ref().map(|notice| notice.text.clone()),
            notice_error: self.notice.as_ref().is_some_and(|notice| notice.error),
        }
    }

    /// Puts a notice in the bar. A plain one goes after [`status_bar::NOTICE_SECS`]; an error stays.
    pub fn show_notice(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            error,
            at: self.clock,
        });
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// The last refusal, in the detector's or the core's own words, for the refusal screen.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    pub fn dismiss_refusal(&mut self) {
        self.refusal = None;
        if self.notice.as_ref().is_some_and(|notice| notice.error) {
            self.notice = None;
        }
    }

    /// The next frame of the running game, converted to RGBA8888.
    pub fn frame(&mut self) -> Option<Frame<'_>> {
        self.machine.as_mut().map(|machine| machine.frame())
    }

    /// Whether the user asked to quit.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Opens `path`: detect, build, load the battery, remember it. On a refusal the running game is
    /// untouched and the refusal text goes to the bar (§6.4).
    pub fn open(&mut self, path: &Path) -> Result<SystemKind, String> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let bytes =
            std::fs::read(path).map_err(|why| format!("refused: cannot read {name}: {why}"))?;
        self.open_bytes(&name, bytes, Some(path))
    }

    /// Opens bytes that came from somewhere other than a file of their own: an archive entry, or a
    /// test. `path` is the file the entry was found in, when there is one. A refusal is kept whole
    /// for the refusal screen and repeated in the bar.
    pub fn open_bytes(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        path: Option<&Path>,
    ) -> Result<SystemKind, String> {
        match self.try_open(name, bytes, path) {
            Ok(system) => {
                self.refusal = None;
                Ok(system)
            }
            Err(why) => {
                self.show_notice(why.clone(), true);
                self.refusal = Some(why.clone());
                Err(why)
            }
        }
    }

    fn try_open(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        path: Option<&Path>,
    ) -> Result<SystemKind, String> {
        let (entry, detected) =
            tendy_detect::open(name, bytes).map_err(|refusal| refusal.to_string())?;
        let mut machine = loader::build(&entry, &detected)?;
        let rate = machine.frame_rate();
        machine.set_sample_rate(self.sample_rate);

        // §6.4: the old game's battery is written before its core goes away.
        self.write_battery();
        self.machine = Some(machine);
        self.system = Some(detected.system);
        self.rom = path.map(Path::to_path_buf);
        self.pacer.set_fps(rate);
        self.paused = false;
        self.fast_forward = false;
        self.menu.close();

        let body = path.and_then(|path| battery::read(&self.paths, detected.system, path));
        if let (Some(body), Some(machine)) = (body, self.machine.as_mut()) {
            let _ = machine.load_battery_body(&body, battery::now_ms());
        }
        if let Some(list) =
            path.and_then(|path| recents::record(&self.paths, detected.system, path).ok())
        {
            self.recents = list;
        }
        self.show_notice(
            detected
                .notice
                .unwrap_or_else(|| format!("{} opened", detected.system.display_name())),
            false,
        );
        self.refresh();
        Ok(detected.system)
    }

    /// Writes the battery and forgets the game. A file that was never opened has nothing to write.
    pub fn close(&mut self) {
        self.write_battery();
        self.machine = None;
        self.rom = None;
        self.system = None;
        self.paused = false;
        self.fps = 0.0;
        self.refresh();
    }

    /// Writes the running game's battery, when it has one.
    pub fn write_battery(&mut self) {
        let (Some(system), Some(rom)) = (self.system, self.rom.clone()) else {
            return;
        };
        let Some(body) = self
            .machine
            .as_ref()
            .and_then(|machine| machine.battery_body())
        else {
            return;
        };
        if let Err(why) = battery::write(&self.paths, system, &rom, &body) {
            self.show_notice(format!("battery not written: {why}"), true);
        }
    }

    /// The battery and a quit.
    pub fn quit_app(&mut self) {
        self.write_battery();
        self.menu.close();
        self.quit = true;
    }

    /// Runs one tick of `dt` seconds: the emulated frames due, and the module hooks after each.
    /// Returns how many frames ran.
    pub fn tick(&mut self, dt: f64) -> u32 {
        self.clock += dt;
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| !notice.error && self.clock - notice.at > status_bar::NOTICE_SECS)
        {
            self.notice = None;
        }
        if self.menu.is_open() {
            self.refresh();
        }
        if self.held() {
            self.pacer.reset();
            self.fps = 0.0;
            self.fps_frames = 0;
            self.fps_secs = 0.0;
            self.fps_prev = (0, 0.0);
            self.run_hooks();
            return 0;
        }
        let speed = self.effective_speed();
        let due = self.pacer.frames_due_at(dt, speed);
        let mut ran = 0;
        for _ in 0..due {
            if let Some(machine) = self.machine.as_mut() {
                machine.run_frame();
                ran += 1;
            }
            self.run_hooks();
        }
        if ran == 0 {
            self.run_hooks();
        }
        self.fps_frames += ran;
        self.fps_secs += dt;
        if self.fps_secs >= FPS_BUCKET_SECS {
            self.fps_prev = (self.fps_frames, self.fps_secs);
            self.fps_frames = 0;
            self.fps_secs = 0.0;
        }
        let (prev_frames, prev_secs) = self.fps_prev;
        let secs = prev_secs + self.fps_secs;
        if secs > 0.0 {
            self.fps = f64::from(prev_frames + self.fps_frames) / secs;
        }
        ran
    }

    /// The fixed order every frame's module hooks run in. A new module appends a line here and
    /// nothing else changes.
    fn run_hooks(&mut self) {
        rewind::on_frame(self);
        states::on_frame(self);
        thumb::on_frame(self);
        screenshot::on_frame(self);
        record::on_frame(self);
        avi::on_frame(self);
        audio::on_frame(self);
        keys::on_frame(self);
        gamepad::on_frame(self);
        first_run::on_frame(self);
        games::on_frame(self);
        config::on_frame(self);
        settings::on_frame(self);
        switch::on_frame(self);
        display::on_frame(self);
        caps::on_frame(self);
        crate::paths::on_frame(self);
    }

    /// Re-asks every module for its rows. Called when the drawer is open, and after any action.
    pub fn refresh(&mut self) {
        let rows = menu::Rows::collect(self);
        self.menu.set_rows(rows);
        self.menu.set_recents(self.recents.clone());
    }

    /// Does what the drawer asked for. Returns whether anything happened, which keeps a click on a
    /// disabled row from looking like a command.
    pub fn request(&mut self, request: Request) -> bool {
        match request {
            Request::OpenRom => {
                let dir = crate::file_pick::start_dir(self.rom.as_deref());
                match crate::file_pick::rom(&dir) {
                    Some(path) => {
                        let _ = self.open(&path);
                        true
                    }
                    None => false,
                }
            }
            Request::OpenRecent(index) => match self.recents.get(index).cloned() {
                Some(recent) => {
                    let _ = self.open(&recent.path);
                    true
                }
                None => false,
            },
            Request::ClearRecents => {
                if let Ok(list) = recents::clear(&self.paths) {
                    self.recents = list;
                }
                self.refresh();
                true
            }
            Request::Enter(level) => {
                self.menu.set_level(level);
                self.refresh();
                true
            }
            Request::Resume => {
                self.set_paused(false);
                true
            }
            Request::Pause => {
                self.set_paused(true);
                true
            }
            Request::Reset => {
                if let Some(machine) = self.machine.as_mut() {
                    machine.reset();
                }
                true
            }
            Request::Quit => {
                self.quit_app();
                true
            }
            Request::Row(at, act) => {
                let acted = self.row(at, act);
                self.refresh();
                acted
            }
        }
    }

    /// Hands a module's row action to the module that owns it.
    fn row(&mut self, at: menu::Ref, act: RowAct) -> bool {
        match at.module {
            Module::Caps => caps::on_row(self, at.index, act),
            Module::Keys => keys::on_row(self, at.index, act),
            Module::Gamepad => gamepad::on_row(self, at.index, act),
            Module::Display => display::on_row(self, at.index, act),
            Module::Audio => audio::on_row(self, at.index, act),
            Module::Switch => switch::on_row(self, at.index, act),
            Module::Config => config::on_row(self, at.index, act),
            Module::Settings => settings::on_row(self, at.index, act),
            Module::Paths => crate::paths::on_row(self, at.index, act),
            Module::Games => games::on_row(self, at.index, act),
            Module::States => states::on_row(self, at.index, act),
            Module::Thumb => thumb::on_row(self, at.index, act),
            Module::FirstRun => first_run::on_row(self, at.index, act),
            Module::Rewind => rewind::on_row(self, at.index, act),
            Module::Screenshot => screenshot::on_row(self, at.index, act),
            Module::Record => record::on_row(self, at.index, act),
            Module::Avi => avi::on_row(self, at.index, act),
            // Own rows carry their request; they never come back as a module reference.
            Module::Menu => false,
        }
    }

    /// The drawer's request queue, drained once a tick by the window loop.
    pub fn take_request(&mut self) -> Option<Request> {
        self.menu.take_request()
    }

    /// The entry the drawer's cursor is on, for the header line.
    pub fn menu_level(&self) -> Level {
        self.menu.level()
    }

    /// Whether the cursor's row belongs to a module. Used by the window's key handler.
    pub fn menu_item_act(&self) -> Option<ItemAct> {
        self.menu.item().map(|item| item.act)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Paths {
        Paths::new(std::env::temp_dir().join(format!("tendyemu-app-{name}-{}", std::process::id())))
    }

    fn gb_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        rom[0x134..0x13C].copy_from_slice(b"SHELLGB ");
        let mut sum = 0u8;
        for byte in &rom[0x134..=0x14C] {
            sum = sum.wrapping_sub(*byte).wrapping_sub(1);
        }
        rom[0x14D] = sum;
        rom
    }

    #[test]
    fn a_fresh_app_has_no_game_and_takes_the_default_settings() {
        let app = App::new(scratch("fresh"));
        assert!(app.machine().is_none());
        assert!(app.system().is_none());
        assert!(app.held());
        assert_eq!(app.setting(SystemKind::Snes, "display.scale"), None);
        assert_eq!(app.effective_speed(), 1.0);
    }

    #[test]
    fn a_good_file_opens_and_names_its_system() {
        let paths = scratch("open");
        let mut app = App::new(paths);
        let system = app
            .open_bytes("test.gb", gb_rom(), None)
            .expect("the GB core accepts a 32 KiB cart");
        assert_eq!(system, SystemKind::Gb);
        assert_eq!(app.system(), Some(SystemKind::Gb));
        assert!(app.machine().is_some());
        assert!(!app.held());
    }

    #[test]
    fn a_bad_file_is_refused_and_leaves_the_running_game_alone() {
        let mut app = App::new(scratch("refuse"));
        app.open_bytes("test.gb", gb_rom(), None).expect("opens");
        let before = app.system();
        let why = app
            .open_bytes("junk.ng", vec![0u8; 16], None)
            .expect_err("16 bytes of nothing is not a ROM");
        assert!(why.starts_with("refused: "), "{why}");
        assert_eq!(app.system(), before);
        assert!(app.machine().is_some());
    }

    #[test]
    fn a_refusal_is_the_bar_s_notice_and_stays() {
        let mut app = App::new(scratch("notice"));
        let why = app
            .open_bytes("junk.xyz", vec![0u8; 16], None)
            .expect_err("not a ROM");
        app.show_notice(why, true);
        assert!(app.bar().notice_error);
        app.tick(10.0);
        assert!(app.notice().is_some(), "an error notice does not time out");
        app.show_notice("opened", false);
        app.tick(status_bar::NOTICE_SECS + 0.1);
        assert!(app.notice().is_none(), "a plain notice does time out");
    }

    #[test]
    fn the_menu_request_queue_is_drained_once() {
        let mut app = App::new(scratch("queue"));
        app.open_bytes("test.gb", gb_rom(), None).expect("opens");
        app.refresh();
        app.menu_mut().open();
        assert_eq!(app.menu_level(), Level::Top);
        app.menu_mut().set_level(Level::Game);
        assert!(!app.take_request().is_some());
        assert!(app.request(Request::Pause));
        assert!(app.paused());
        assert!(!app.held() || app.paused());
    }

    #[test]
    fn a_tick_runs_the_frames_the_rate_owes() {
        let mut app = App::new(scratch("tick"));
        app.open_bytes("test.gb", gb_rom(), None).expect("opens");
        let mut ran = 0;
        for _ in 0..60 {
            ran += app.tick(1.0 / 60.0);
        }
        // One second of wall clock at the Game Boy's 59.7275 fps is 59 whole frames, 60 if the
        // accumulation rounds up; either is the pacer doing its job.
        assert!((59..=60).contains(&ran), "{ran} frames");
        assert!(app.fps() > 0.0);
    }

    /// The bar's rate after `ticks` ticks of `dt` seconds at `speed`.
    fn fps_after(name: &str, ticks: u32, dt: f64, speed: f64) -> f64 {
        let mut app = App::new(scratch(name));
        app.open_bytes("test.gb", gb_rom(), None).expect("opens");
        app.set_speed(speed);
        for _ in 0..ticks {
            app.tick(dt);
        }
        app.fps()
    }

    #[test]
    fn the_fps_reading_is_a_window_not_one_tick() {
        // A 144 Hz tick runs one frame, which used to be read as 144 fps; three seconds of the
        // same ticks has to settle on the machine's own rate.
        let at_144 = fps_after("fps-144", 3 * 144, 1.0 / 144.0, 1.0);
        assert!((59.0..=61.0).contains(&at_144), "{at_144} fps at 144 Hz");
        let at_60 = fps_after("fps-60", 3 * 60, 1.0 / 60.0, 1.0);
        assert!((59.0..=61.0).contains(&at_60), "{at_60} fps at 60 Hz");
        let twice = fps_after("fps-2x", 3 * 144, 1.0 / 144.0, 2.0);
        assert!((118.0..=121.0).contains(&twice), "{twice} fps at 2x");
    }

    #[test]
    fn a_paused_app_runs_nothing() {
        let mut app = App::new(scratch("paused"));
        app.open_bytes("test.gb", gb_rom(), None).expect("opens");
        app.set_paused(true);
        assert_eq!(app.tick(0.5), 0);
    }
}
