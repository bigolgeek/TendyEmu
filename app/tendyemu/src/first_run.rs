//! The first-run offer (DESIGN §6.1, §6.5): copy the earlier apps' settings, recents, keys file,
//! battery saves and states into this app's folders, once.
//!
//! Nothing here changes an old app's folder. Every old file is opened read-only and every new file
//! is made under [`Paths::root`]. The offer is one row group on the Emulation page, plus one notice
//! the first time the app sees an old folder; Copy or Skip writes `first-run.done`, after which the
//! offer is silent for good.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tendy_core::SystemKind;

use crate::app::App;
use crate::battery;
use crate::config;
use crate::menu::{Row, RowAct, RowKind};
use crate::paths::Paths;
use crate::recents;

/// The two old apps' config folders, beside this app's config folder (§6.3).
const GB_FOLDER: &str = "gb-rust";
const SNES_FOLDER: &str = "snes-rust";

/// The offer's rows, in order.
const COPY: usize = 0;
const SKIP: usize = 1;
const FIND_NES: usize = 2;

/// The NES folder the user pointed at, per config root. It is a session choice until Copy or Skip
/// writes the marker, so it lives here rather than in the settings file, whose keys are frozen.
static NES_FOLDER: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

/// The config root whose announcement has already gone up.
static ANNOUNCED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What one copy did, for the notice and for the tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// New files written.
    pub copied: usize,
    /// Old files that were absent or already had a new counterpart.
    pub skipped: usize,
    /// Old settings this build does not know, dropped with a notice by §6.2's parser.
    pub unknown: usize,
    /// One line per failure; an empty list is a clean copy.
    pub problems: Vec<String>,
}

impl Report {
    fn merge(&mut self, other: Report) {
        self.copied += other.copied;
        self.skipped += other.skipped;
        self.unknown += other.unknown;
        self.problems.extend(other.problems);
    }

    /// The one line the bar shows after Copy.
    pub fn summary(&self) -> String {
        let files = match self.copied {
            0 => "no old files to copy".to_string(),
            1 => "copied 1 file".to_string(),
            count => format!("copied {count} files"),
        };
        let mut text = files;
        if self.unknown > 0 {
            text.push_str(&format!(
                "; {} old setting{} not recognised",
                self.unknown,
                if self.unknown == 1 { " was" } else { "s were" }
            ));
        }
        if !self.problems.is_empty() {
            text.push_str(&format!("; {} failed", self.problems.len()));
        }
        text
    }
}

/// The old app folders found beside this config folder (§6.5 step 1): the Game Boy app's and the
/// Super NES app's. The NES app's own folder is the one the user pointed at, if any.
pub fn old_apps(paths: &Paths) -> Vec<(SystemKind, PathBuf)> {
    let mut found = beside(paths);
    if let Some(dir) = nes_folder(paths) {
        found.push((SystemKind::Nes, dir));
    }
    found
}

/// The two folders §6.5 opens the offer for.
fn beside(paths: &Paths) -> Vec<(SystemKind, PathBuf)> {
    let Some(parent) = paths.root().parent() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (folder, system) in [(GB_FOLDER, SystemKind::Gb), (SNES_FOLDER, SystemKind::Snes)] {
        let dir = parent.join(folder);
        if dir.is_dir() {
            found.push((system, dir));
        }
    }
    found
}

/// The folder the user pointed the NES step at, when this session has one for `paths`.
pub fn nes_folder(paths: &Paths) -> Option<PathBuf> {
    let folders = NES_FOLDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    folders
        .iter()
        .find(|(root, _)| root == paths.root())
        .map(|(_, dir)| dir.clone())
}

/// Records the folder the NES dialog returned: the copy that follows takes its `config.toml`,
/// `recents.txt` and the files those point at.
pub fn remember_nes(paths: &Paths, dir: &Path) {
    let mut folders = NES_FOLDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let root = paths.root().to_path_buf();
    if let Some(entry) = folders.iter_mut().find(|(known, _)| *known == root) {
        entry.1 = dir.to_path_buf();
    } else {
        folders.push((root, dir.to_path_buf()));
    }
}

fn forget_nes(paths: &Paths) {
    let mut folders = NES_FOLDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    folders.retain(|(root, _)| root != paths.root());
}

/// The one-time offer's rows. Empty before an old folder is found and after either answer.
pub fn rows(app: &App) -> Vec<Row> {
    let paths = app.paths();
    if paths.first_run_done().exists() || beside(paths).is_empty() {
        return Vec::new();
    }
    let found = old_apps(paths)
        .iter()
        .map(|(system, _)| system.display_name())
        .collect::<Vec<_>>()
        .join(", ");
    let nes = nes_folder(paths)
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "not set".to_string());
    vec![
        Row {
            label: "Copy old files".to_string(),
            value: found,
            kind: RowKind::Action,
            desc: "Copy settings, saves and states; the old files stay where they are",
            hotkey: "",
            requires: None,
            act: None,
        },
        Row {
            label: "Skip".to_string(),
            value: String::new(),
            kind: RowKind::Action,
            desc: "Leave the old apps' files where they are, and do not ask again",
            hotkey: "",
            requires: None,
            act: None,
        },
        Row {
            label: "Find the NES app folder...".to_string(),
            value: nes,
            kind: RowKind::Action,
            desc: "The old NES app keeps its config.toml beside its program; pick that file",
            hotkey: "",
            requires: None,
            act: None,
        },
    ]
}

/// Copies every old folder the offer found, the report describing what happened.
pub fn copy_all(paths: &Paths) -> Report {
    let mut report = Report::default();
    for (system, dir) in old_apps(paths) {
        report.merge(copy_from(paths, system, &dir));
    }
    report
}

/// Copies one old app's files into `paths`: its `config.toml` keys into its system section, its
/// recents list, the keys file it names, and the battery save and states of every ROM the list
/// points at (§6.5 steps 2 and 4). `dir` is read, never written.
pub fn copy_from(paths: &Paths, system: SystemKind, dir: &Path) -> Report {
    let mut report = Report::default();
    let section = system.key();
    let mut new = config::load(paths).config;
    let mut changed = false;

    match fs::read_to_string(dir.join("config.toml")) {
        Ok(text) => match config::parse_named(&text, section, "config.toml") {
            Ok(loaded) => {
                report.unknown = loaded.notices.len();
                let mut pairs = pairs(&loaded.config, section);
                // The old app's keys file is copied, not its path: a stale or absolute old path
                // must never reach the new settings.
                let named = pairs
                    .iter()
                    .position(|(key, _)| key == "keys_file")
                    .map(|at| pairs.remove(at).1)
                    .filter(|named| !named.is_empty());
                changed = !pairs.is_empty();
                for (key, value) in pairs {
                    new.set(section, &key, value);
                }
                if let Some(named) = named {
                    match copy_keys(paths, system, dir, Path::new(&named)) {
                        Ok(Some(dest)) => {
                            new.set(section, "keys_file", dest.to_string_lossy());
                            changed = true;
                            report.copied += 1;
                        }
                        Ok(None) => {}
                        Err(why) => report.problems.push(format!("keys file not copied: {why}")),
                    }
                }
            }
            Err(why) => report
                .problems
                .push(format!("{} settings not copied: {why}", folder(system))),
        },
        Err(why) if why.kind() == io::ErrorKind::NotFound => {}
        Err(why) => report
            .problems
            .push(format!("{} settings not copied: {why}", folder(system))),
    }
    if changed && let Err(why) = config::save(paths, &new) {
        report.problems.push(format!("settings not written: {why}"));
    }

    for rom in old_recents(system, dir).iter().rev() {
        let system = rom_system(system, rom);
        if let Err(why) = recents::record(paths, system, rom) {
            report.problems.push(format!("recents not written: {why}"));
        }
        match old_battery(system, dir, rom) {
            Some(save) => match copy_file(&save, &battery::save_path(paths, system, rom)) {
                Ok(true) => report.copied += 1,
                Ok(false) => report.skipped += 1,
                Err(why) => report.problems.push(format!("battery not copied: {why}")),
            },
            None => report.skipped += 1,
        }
        copy_states(paths, system, rom, &mut report);
    }
    report
}

/// The keys a parsed old file holds in `section`, as the settings file would write them. The
/// parser has already checked every kind, so each pair is stored as it reads.
fn pairs(config: &config::Config, section: &str) -> Vec<(String, String)> {
    config::to_flat_text(config, section)
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(value);
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// One old keys file, into `<system>/keys.txt` (§6.3). `named` is the path the old settings file
/// gave; a relative one is looked for inside the old app's folder.
fn copy_keys(
    paths: &Paths,
    system: SystemKind,
    dir: &Path,
    named: &Path,
) -> io::Result<Option<PathBuf>> {
    let source = if named.is_absolute() {
        named.to_path_buf()
    } else {
        dir.join(named)
    };
    if !source.is_file() {
        return Ok(None);
    }
    let dest = paths.keys_file(system);
    copy_file(&source, &dest)?;
    Ok(Some(dest))
}

/// The old recents list: paths one per line, newest last on disk in every app; the NES app keeps
/// its list in `roms/` when that folder exists.
fn old_recents(system: SystemKind, dir: &Path) -> Vec<PathBuf> {
    let file = if system == SystemKind::Nes && dir.join("roms").is_dir() {
        dir.join("roms").join("recents.txt")
    } else {
        dir.join("recents.txt")
    };
    let Ok(text) = fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| line.trim_end_matches('\r').trim())
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The system a remembered ROM ran as. The Game Boy app ran GBA cartridges too, and named their
/// files with a GBA extension.
fn rom_system(app: SystemKind, rom: &Path) -> SystemKind {
    if app == SystemKind::Gb
        && rom
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gba") || ext.eq_ignore_ascii_case("agb"))
    {
        return SystemKind::Gba;
    }
    app
}

/// Where the old app kept the battery of `rom`: the Game Boy app in its own `saves/` folder under
/// the ROM's stem and CRC-32, the Super NES and NES apps beside the ROM (§6.3).
fn old_battery(app: SystemKind, dir: &Path, rom: &Path) -> Option<PathBuf> {
    match app {
        SystemKind::Gb | SystemKind::Gba => bracketed(&dir.join("saves"), rom),
        SystemKind::Snes => existing(&rom.with_extension("srm")),
        SystemKind::Nes => existing(&rom.with_extension("sav")),
    }
}

fn existing(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

/// `<stem> [<crc32>].sav`, the Game Boy app's name, with a plain `<stem>.sav` accepted too.
fn bracketed(saves: &Path, rom: &Path) -> Option<PathBuf> {
    let stem = rom.file_stem()?.to_string_lossy().into_owned();
    let plain = saves.join(format!("{stem}.sav"));
    if plain.is_file() {
        return Some(plain);
    }
    let start = format!("{stem} [");
    for entry in fs::read_dir(saves).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&start) && name.ends_with(".sav") {
            return Some(entry.path());
        }
    }
    None
}

/// The state files the old apps wrote beside the ROM: `<stem>.state`, `<stem>-<slot>.state` and
/// `<stem>.slot<slot>.state` all share the stem, and all are copied under their own names.
fn copy_states(paths: &Paths, system: SystemKind, rom: &Path, report: &mut Report) {
    let Some(dir) = rom.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return;
    };
    let Some(stem) = rom
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
    else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_suffix(".state") else {
            continue;
        };
        let same = rest == stem
            || rest
                .strip_prefix(&stem)
                .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('-'));
        if !same {
            continue;
        }
        let dest = paths.states_dir(system).join(&name);
        match copy_file(&entry.path(), &dest) {
            Ok(true) => report.copied += 1,
            Ok(false) => report.skipped += 1,
            Err(why) => report.problems.push(format!("{name} not copied: {why}")),
        }
    }
}

/// Copies `from` to `to`. `Ok(false)` when `to` is already there: the new app's own file wins, and
/// an old file is only ever read.
fn copy_file(from: &Path, to: &Path) -> io::Result<bool> {
    if to.exists() {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    Ok(true)
}

/// A folder's user-facing name, for a notice about a file that would not copy.
fn folder(system: SystemKind) -> &'static str {
    match system {
        SystemKind::Gb => GB_FOLDER,
        SystemKind::Snes => SNES_FOLDER,
        SystemKind::Gba => "gba",
        SystemKind::Nes => "nes",
    }
}

/// Writes `first-run.done` (§6.5 step 3) and forgets the session's NES folder.
fn answer(app: &mut App) -> bool {
    let marker = app.paths().first_run_done();
    if let Some(dir) = marker.parent()
        && let Err(why) = fs::create_dir_all(dir)
    {
        app.show_notice(format!("first run not marked: {why}"), true);
        return false;
    }
    if let Err(why) = fs::write(&marker, b"") {
        app.show_notice(format!("first run not marked: {why}"), true);
        return false;
    }
    forget_nes(app.paths());
    true
}

/// Copy, Skip or the NES folder row. Enter only: the rows are actions, not choices.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    if !matches!(act, RowAct::Activate) {
        return false;
    }
    match index {
        COPY => {
            let report = copy_all(app.paths());
            let clean = report.problems.is_empty();
            let summary = report.summary();
            if !answer(app) {
                return true;
            }
            if clean {
                app.show_notice(summary, false);
            } else {
                let mut text = summary;
                text.push_str(": ");
                text.push_str(&report.problems.join("; "));
                app.show_notice(text, true);
            }
            true
        }
        SKIP => {
            if answer(app) {
                app.show_notice("old app files left where they are", false);
            }
            true
        }
        FIND_NES => {
            let start = crate::file_pick::start_dir(app.rom());
            let Some(file) = crate::file_pick::keys(&start) else {
                return false;
            };
            let Some(dir) = file.parent() else {
                return false;
            };
            remember_nes(app.paths(), dir);
            app.show_notice(format!("NES folder set: {}", dir.display()), false);
            true
        }
        _ => false,
    }
}

/// Keeps nothing running: the offer is rows, not a frame task. The one notice tells a first-run
/// user where the offer is, because the drawer is the only place the shell can show it.
pub fn on_frame(app: &mut App) {
    let root = {
        let paths = app.paths();
        if paths.first_run_done().exists() || beside(paths).is_empty() {
            return;
        }
        paths.root().to_path_buf()
    };
    {
        let announced = ANNOUNCED
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if announced.as_ref() == Some(&root) {
            return;
        }
    }
    app.show_notice(
        "old app files found: open the menu (M) and copy them from Emulation",
        false,
    );
    let mut announced = ANNOUNCED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    *announced = Some(root);
}
