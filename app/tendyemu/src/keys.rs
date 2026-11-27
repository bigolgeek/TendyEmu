//! Per-system keyboard bindings (DESIGN §6.1, §6.3; issue #17).
//!
//! One logical pad of twelve buttons — D-pad, A B X Y, L R, Start Select — is bound to host keys
//! per system, in `<system>/keys.txt`. The file format is the sources' `button = key` text: one
//! line a button, `#` and blank lines ignored, an unknown name or a key used twice refused with
//! the line number. Each system's file is independent, so a comfortable Game Boy layout and a
//! comfortable Super NES layout can differ (and the built-in layouts are each source's own).
//!
//! `main.rs` asks [`held_names`] which host keys are down; [`on_frame`] turns the names the app
//! holds into the logical pad. A rebind row takes the next key pressed while the drawer is open
//! ([`rows`], [`on_row`]), and writes the system's file at once.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use macroquad::prelude::{KeyCode, is_key_down, is_key_pressed};
use tendy_core::{Buttons, SystemKind};

use crate::app::App;
use crate::menu::{self, Module, Ref, Row, RowAct, RowKind};
use crate::paths::Paths;

/// A host key, by the name the keys file and the window table write: `"Z"`, `"Up"`, `"Enter"`.
pub type Key = &'static str;

/// Every name a keys file may use.
pub const KEY_NAMES: &[Key] = &[
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "Up",
    "Down",
    "Left",
    "Right",
    "Enter",
    "Backspace",
    "Tab",
    "Space",
    "Escape",
    "LeftShift",
    "RightShift",
    "LeftControl",
    "RightControl",
    "LeftAlt",
    "RightAlt",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Insert",
    "Delete",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
];

/// The row label of each button, in [`Buttons::ALL`] order.
const ROW_LABELS: [&str; 12] = [
    "Up", "Down", "Left", "Right", "A", "B", "X", "Y", "L", "R", "Start", "Select",
];

/// A Game Boy layout: the gb app's own, with the Advance's shoulders on its fixed keys.
const GB_KEYS: [Key; 12] = [
    "Up",
    "Down",
    "Left",
    "Right",
    "Z",
    "X",
    "Q",
    "W",
    "A",
    "S",
    "Enter",
    "Backspace",
];

/// An NES layout: the nes app's player-1 table (`X` is A, `Z` is B, `RightShift` Select).
const NES_KEYS: [Key; 12] = [
    "Up",
    "Down",
    "Left",
    "Right",
    "X",
    "Z",
    "Q",
    "W",
    "A",
    "S",
    "Enter",
    "RightShift",
];

/// A Super NES layout: the snes app's, which keeps the physical face diamond.
const SNES_KEYS: [Key; 12] = [
    "Up",
    "Down",
    "Left",
    "Right",
    "X",
    "Z",
    "S",
    "A",
    "Q",
    "W",
    "Enter",
    "Backspace",
];

/// The twelve logical buttons, in [`Buttons::ALL`] order, and the host key each is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub keys: [Key; 12],
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap::builtin(SystemKind::Gb)
    }
}

impl Keymap {
    /// The shipped layout for a system: each source app's own defaults, so a Game Boy and a
    /// Super NES both feel the way their users left them.
    pub fn builtin(system: SystemKind) -> Keymap {
        let keys = match system {
            SystemKind::Gb | SystemKind::Gba => GB_KEYS,
            SystemKind::Nes => NES_KEYS,
            SystemKind::Snes => SNES_KEYS,
        };
        Keymap { keys }
    }

    /// The file's `button = key` text. A button the file names takes that key; one it does not
    /// keeps the built-in key, so a three-line file is a keymap rather than an error.
    ///
    /// An unknown button or key, a line that is not `button = key`, and two buttons left holding
    /// one host key are each refused with the 1-based line number.
    pub fn from_text(system: SystemKind, text: &str) -> Result<Keymap, String> {
        let mut map = Keymap::builtin(system);
        let mut bound: Vec<(Key, usize)> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line_number = n + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, key) = line.split_once('=').ok_or_else(|| {
                format!("line {line_number}: expected `button = key`, got {line:?}")
            })?;
            let name = name.trim();
            let key = key.trim();
            let button = Buttons::ALL
                .iter()
                .find(|(_, button)| *button == name)
                .map(|(bits, _)| *bits)
                .ok_or_else(|| format!("line {line_number}: unknown button `{name}`"))?;
            let Some(known) = KEY_NAMES.iter().find(|known| **known == key) else {
                return Err(format!("line {line_number}: unknown key `{key}`"));
            };
            if let Some((_, first)) = bound.iter().find(|(bound_key, _)| bound_key == known) {
                return Err(format!(
                    "line {line_number}: {key:?} is bound to more than one button (already on line {first})"
                ));
            }
            bound.push((known, line_number));
            map.set(button, known);
        }
        map.check_unique()?;
        Ok(map)
    }

    /// The text a keys file holds: one line a button, in settings order, and no comment, so a
    /// written file reads back as the same keymap.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (button, name) in Buttons::ALL {
            out.push_str(&format!("{name} = {}\n", self.get(button)));
        }
        out
    }

    /// The key bound to `button`.
    pub fn get(&self, button: Buttons) -> Key {
        Keymap::slot(button).map_or("", |slot| self.keys[slot])
    }

    /// Binds `key` to `button`.
    pub fn set(&mut self, button: Buttons, key: Key) {
        if let Some(slot) = Keymap::slot(button) {
            self.keys[slot] = key;
        }
    }

    /// Swaps the two buttons' keys.
    pub fn swap(&mut self, a: Buttons, b: Buttons) {
        if let (Some(i), Some(j)) = (Keymap::slot(a), Keymap::slot(b)) {
            self.keys.swap(i, j);
        }
    }

    /// Binds `key` to `button`; a button already holding that key takes the old one, so one host
    /// key never presses two buttons.
    pub fn rebind(&mut self, button: Buttons, key: Key) {
        let old = self.get(button);
        if let Some(other) = Buttons::ALL
            .iter()
            .map(|(bits, _)| *bits)
            .find(|other| *other != button && self.get(*other) == key)
        {
            self.set(other, old);
        }
        self.set(button, key);
    }

    /// Whether `name` is a key this build knows.
    pub fn is_known(name: &str) -> bool {
        KEY_NAMES.contains(&name)
    }

    /// Refuses a map where two buttons hold one key; a file can otherwise name one key twice by
    /// picking a key another button already holds in the built-in table.
    fn check_unique(&self) -> Result<(), String> {
        for (index, (button, name)) in Buttons::ALL.iter().enumerate() {
            for (other, other_name) in &Buttons::ALL[index + 1..] {
                if self.get(*button) == self.get(*other) {
                    return Err(format!(
                        "`{}` is bound to both `{name}` and `{other_name}`",
                        self.get(*button)
                    ));
                }
            }
        }
        Ok(())
    }

    /// The array slot of one button: its position in [`Buttons::ALL`].
    fn slot(button: Buttons) -> Option<usize> {
        Buttons::ALL.iter().position(|(bits, _)| *bits == button)
    }
}

/// Reads `<system>/keys.txt`; a missing file is the built-in layout, and a malformed one is an
/// error the caller reports once before keeping the built-in layout.
pub fn load(paths: &Paths, system: SystemKind) -> Result<Keymap, String> {
    let file = paths.keys_file(system);
    match fs::read_to_string(&file) {
        Ok(text) => Keymap::from_text(system, &text),
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(Keymap::builtin(system)),
        Err(why) => Err(format!("cannot read {}: {why}", file.display())),
    }
}

/// Writes `<system>/keys.txt`: a temporary file first, then a rename over the old one.
pub fn save(paths: &Paths, system: SystemKind, map: &Keymap) -> Result<(), String> {
    let file = paths.keys_file(system);
    let dir = file.parent().unwrap_or_else(|| paths.root());
    fs::create_dir_all(dir).map_err(|why| format!("cannot create {}: {why}", dir.display()))?;
    let temp = file.with_extension("txt.tmp");
    fs::write(&temp, map.to_text())
        .map_err(|why| format!("cannot write {}: {why}", temp.display()))?;
    fs::rename(&temp, &file).map_err(|why| format!("cannot replace {}: {why}", file.display()))
}

/// The pad bits the bound keys among `held` press.
pub fn pad(held: &[String], map: &Keymap) -> Buttons {
    let mut pad = Buttons::NONE;
    for (button, _) in Buttons::ALL {
        if held.iter().any(|down| down == map.get(button)) {
            pad = pad.with(button);
        }
    }
    pad
}

/// The host-key names down this frame, as the window's key table reports them, for the running
/// system's bindings. `main.rs` puts the result into [`App::set_held_buttons`].
pub fn held_names(app: &App) -> Vec<String> {
    let Some((_, map)) = live_map(app) else {
        return Vec::new();
    };
    let mut held: Vec<String> = Vec::new();
    for (button, _) in Buttons::ALL {
        let name = map.get(button);
        if held.iter().any(|down| down == name) {
            continue;
        }
        if mc_keycode(name).is_some_and(is_key_down) {
            held.push(name.to_string());
        }
    }
    held
}

/// The keyboard's share of the pad for the running system, or `None` without a game (the gamepad
/// module merges its own bits onto this).
pub fn current_pad(app: &App) -> Option<Buttons> {
    live_map(app).map(|(_, map)| pad(app.held_buttons(), &map))
}

/// The Input page: one rebind row a button, then the keys-file dialog.
pub fn rows(app: &App) -> Vec<Row> {
    let Some((_, map)) = live_map(app) else {
        return vec![Row::info("No game", "", "Open a ROM to change its keys")];
    };
    let mut rows = Vec::with_capacity(ROW_LABELS.len() + 1);
    for (index, (button, _)) in Buttons::ALL.iter().enumerate() {
        rows.push(Row {
            label: ROW_LABELS[index].to_string(),
            value: map.get(*button).to_string(),
            kind: RowKind::Rebind,
            desc: "Enter, then press the new key",
            hotkey: "Enter",
            requires: None,
            act: None,
        });
    }
    rows.push(Row {
        label: "Load keys file...".to_string(),
        value: String::new(),
        kind: RowKind::Pick,
        desc: "Read a `button = key` file for this system",
        hotkey: "Enter",
        requires: None,
        act: None,
    });
    rows
}

/// A captured key rebinds the row's button and is written to the system's file at once. The last
/// row opens the keys-file dialog.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    let Some(system) = app.system() else {
        return false;
    };
    match act {
        RowAct::Key(name) => {
            let Some(button) = Buttons::ALL.get(index).map(|(bits, _)| *bits) else {
                return false;
            };
            let Some(key) = KEY_NAMES.iter().find(|known| **known == name) else {
                app.show_notice(format!("unknown key `{name}`"), true);
                return true;
            };
            let Some((_, mut map)) = live_map(app) else {
                return false;
            };
            map.rebind(button, key);
            let saved = save(app.paths(), system, &map);
            update_live(app, map);
            match saved {
                Ok(()) => app.show_notice(format!("{} = {key}", Buttons::ALL[index].1), false),
                Err(why) => app.show_notice(why, true),
            }
            true
        }
        RowAct::Activate if index == Buttons::ALL.len() => load_dialog(app),
        _ => false,
    }
}

/// Runs every frame: reports a keys-file problem once, takes the key a rebind row is waiting
/// for, and hands the keyboard's pad to the machine (the gamepad module adds its bits after).
pub fn on_frame(app: &mut App) {
    if let Some(why) = with_live(app, |entry| entry.notice.take()).flatten() {
        app.show_notice(why, true);
    }

    // The key that armed the capture is still "pressed" on the frame the drawer armed it, so the
    // first frame a capture exists is always skipped.
    let capture = app.menu().capture().filter(|at| at.module == Module::Keys);
    if with_live(app, |entry| {
        let first = entry.armed != capture;
        entry.armed = capture;
        first
    }) == Some(true)
    {
        return;
    }
    if capture.is_some()
        && let Some(name) = first_pressed_key()
    {
        menu::key(app.menu_mut(), name);
    }

    if let Some((_, map)) = live_map(app) {
        let pad = pad(app.held_buttons(), &map);
        if let Some(machine) = app.machine_mut() {
            machine.set_buttons(pad);
        }
    }
}

/// The keys-file dialog: read, parse, then replace the system's file with what was read.
fn load_dialog(app: &mut App) -> bool {
    let Some(system) = app.system() else {
        return false;
    };
    let Some(file) = crate::file_pick::keys(&crate::file_pick::start_dir(app.rom())) else {
        return false;
    };
    match fs::read_to_string(&file) {
        Ok(text) => match Keymap::from_text(system, &text) {
            Ok(map) => {
                if let Err(why) = save(app.paths(), system, &map) {
                    app.show_notice(why, true);
                    return true;
                }
                let name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.display().to_string());
                update_live(app, map);
                app.show_notice(format!("{name} loaded"), false);
                true
            }
            Err(why) => {
                app.show_notice(why, true);
                true
            }
        },
        Err(why) => {
            app.show_notice(format!("cannot read {}: {why}", file.display()), true);
            true
        }
    }
}

/// The first key pressed this frame, by its file name, for a waiting rebind row.
fn first_pressed_key() -> Option<Key> {
    KEY_NAMES
        .iter()
        .copied()
        .find(|name| mc_keycode(name).is_some_and(is_key_pressed))
}

/// A file name as the window's key code. `None` for a name this build does not know.
fn mc_keycode(name: &str) -> Option<KeyCode> {
    Some(match name {
        "A" => KeyCode::A,
        "B" => KeyCode::B,
        "C" => KeyCode::C,
        "D" => KeyCode::D,
        "E" => KeyCode::E,
        "F" => KeyCode::F,
        "G" => KeyCode::G,
        "H" => KeyCode::H,
        "I" => KeyCode::I,
        "J" => KeyCode::J,
        "K" => KeyCode::K,
        "L" => KeyCode::L,
        "M" => KeyCode::M,
        "N" => KeyCode::N,
        "O" => KeyCode::O,
        "P" => KeyCode::P,
        "Q" => KeyCode::Q,
        "R" => KeyCode::R,
        "S" => KeyCode::S,
        "T" => KeyCode::T,
        "U" => KeyCode::U,
        "V" => KeyCode::V,
        "W" => KeyCode::W,
        "X" => KeyCode::X,
        "Y" => KeyCode::Y,
        "Z" => KeyCode::Z,
        "0" => KeyCode::Key0,
        "1" => KeyCode::Key1,
        "2" => KeyCode::Key2,
        "3" => KeyCode::Key3,
        "4" => KeyCode::Key4,
        "5" => KeyCode::Key5,
        "6" => KeyCode::Key6,
        "7" => KeyCode::Key7,
        "8" => KeyCode::Key8,
        "9" => KeyCode::Key9,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "Space" => KeyCode::Space,
        "Escape" => KeyCode::Escape,
        "LeftShift" => KeyCode::LeftShift,
        "RightShift" => KeyCode::RightShift,
        "LeftControl" => KeyCode::LeftControl,
        "RightControl" => KeyCode::RightControl,
        "LeftAlt" => KeyCode::LeftAlt,
        "RightAlt" => KeyCode::RightAlt,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Insert" => KeyCode::Insert,
        "Delete" => KeyCode::Delete,
        "F1" => KeyCode::F1,
        "F2" => KeyCode::F2,
        "F3" => KeyCode::F3,
        "F4" => KeyCode::F4,
        "F5" => KeyCode::F5,
        "F6" => KeyCode::F6,
        "F7" => KeyCode::F7,
        "F8" => KeyCode::F8,
        "F9" => KeyCode::F9,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "F12" => KeyCode::F12,
        _ => return None,
    })
}

/// One system's live bindings, cached per config folder so the file is read once per session.
struct Live {
    root: PathBuf,
    system: SystemKind,
    map: Keymap,
    /// A keys-file problem to put in the bar once.
    notice: Option<String>,
    /// The rebind row the last frame saw armed, so the arming key is not taken as the new key.
    armed: Option<Ref>,
}

/// Every folder's live bindings. A test's scratch folder is its own entry, so tests running in
/// parallel never share one.
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());

/// Loads the system's file into the cache on first use, and reports a broken file in the bar.
fn ensure_live(app: &App) -> Option<SystemKind> {
    let system = app.system()?;
    let root = app.paths().root();
    let mut live = LIVE.lock().unwrap_or_else(|poison| poison.into_inner());
    if live
        .iter()
        .any(|entry| entry.root.as_path() == root && entry.system == system)
    {
        return Some(system);
    }
    let (map, notice) = match load(app.paths(), system) {
        Ok(map) => (map, None),
        Err(why) => (Keymap::builtin(system), Some(why)),
    };
    live.push(Live {
        root: root.to_path_buf(),
        system,
        map,
        notice,
        armed: None,
    });
    Some(system)
}

/// Runs `f` on the system's live entry, loading the file first when needed.
fn with_live<R>(app: &App, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    let system = ensure_live(app)?;
    let root = app.paths().root();
    let mut live = LIVE.lock().unwrap_or_else(|poison| poison.into_inner());
    let entry = live
        .iter_mut()
        .find(|entry| entry.root.as_path() == root && entry.system == system)?;
    Some(f(entry))
}

/// The running system and its cached bindings.
fn live_map(app: &App) -> Option<(SystemKind, Keymap)> {
    with_live(app, |entry| (entry.system, entry.map))
}

/// Puts a just-written map into the cache.
fn update_live(app: &App, map: Keymap) {
    with_live(app, |entry| entry.map = map);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_system_keeps_its_own_built_in_layout() {
        let gb = Keymap::builtin(SystemKind::Gb);
        assert_eq!(gb.get(Buttons::A), "Z");
        assert_eq!(gb.get(Buttons::B), "X");
        let nes = Keymap::builtin(SystemKind::Nes);
        assert_eq!(nes.get(Buttons::A), "X");
        assert_eq!(nes.get(Buttons::B), "Z");
        assert_eq!(nes.get(Buttons::SELECT), "RightShift");
        let snes = Keymap::builtin(SystemKind::Snes);
        assert_eq!(snes.get(Buttons::A), "X");
        assert_eq!(snes.get(Buttons::Y), "A");
        assert_eq!(snes.get(Buttons::X), "S");
        assert_eq!(snes.get(Buttons::L), "Q");
        assert_eq!(snes.get(Buttons::R), "W");
    }

    #[test]
    fn a_written_file_reads_back_as_the_same_keymap() {
        for system in SystemKind::ALL {
            let map = Keymap::builtin(system);
            let read = Keymap::from_text(system, &map.to_text()).expect("its own text parses");
            assert_eq!(read, map, "{}", system.key());
        }
    }

    #[test]
    fn a_file_may_name_only_some_buttons() {
        let map = Keymap::from_text(SystemKind::Gb, "a = K\n").expect("one line is a keymap");
        assert_eq!(map.get(Buttons::A), "K");
        assert_eq!(
            map.get(Buttons::B),
            Keymap::builtin(SystemKind::Gb).get(Buttons::B)
        );
    }

    #[test]
    fn a_key_on_two_buttons_is_refused() {
        let why = Keymap::from_text(SystemKind::Gb, "a = Q\nb = Q\n").expect_err("two buttons");
        assert!(why.contains("more than one button"), "{why}");
    }

    #[test]
    fn a_file_that_collides_with_the_built_in_table_is_refused() {
        // The Game Boy's B is X; binding A to X as well would press both.
        let why = Keymap::from_text(SystemKind::Gb, "a = X\n").expect_err("two buttons");
        assert!(why.contains("both"), "{why}");
    }

    #[test]
    fn two_names_press_two_bits_and_nothing_presses_nothing() {
        let map = Keymap::builtin(SystemKind::Gb);
        assert_eq!(pad(&[], &map), Buttons::NONE);
        let pad = pad(
            &[
                map.get(Buttons::UP).to_string(),
                map.get(Buttons::A).to_string(),
            ],
            &map,
        );
        assert!(pad.contains(Buttons::UP));
        assert!(pad.contains(Buttons::A));
        assert!(!pad.contains(Buttons::B));
    }

    #[test]
    fn a_rebind_swaps_the_previous_holder() {
        let mut map = Keymap::builtin(SystemKind::Snes);
        map.rebind(Buttons::A, "Z");
        assert_eq!(map.get(Buttons::A), "Z");
        assert_eq!(map.get(Buttons::B), "X");
    }
}
