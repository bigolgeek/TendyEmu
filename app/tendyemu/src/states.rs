//! Save states (DESIGN §6.1, §2.7, §6.3). #22 owns this file: the slot rows, the container from
//! `tendy_core::state`, and the file naming each source app keeps.
//!
//! A state file is the core's own blob inside the shared container (§2.7), so a state of another
//! system is refused with a plain sentence instead of being handed to the running core. The file
//! lives under `<config>/<system>/states/` (§6.3); its name is the one its source app writes:
//!
//! | System | State file | Thumbnail |
//! |---|---|---|
//! | GB, GBA | `<stem>-<slot>.state`, slots 1–9 | `<stem>-<slot>.thumb.png` |
//! | NES | `<stem>.state`, one state | `<stem>.png` |
//! | SNES | `<stem>.slot<slot>.state`, slots 1–5 | `<stem>.slot<slot>.png` |
//!
//! The slot number, the save and the load also reach the window's `F5`/`F7` through
//! [`selected_slot`], [`save_selected`] and [`load_selected`].

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendy_core::state::{unwrap, wrap};

use crate::app::App;
use crate::battery;
use crate::config;
use crate::menu::{Row, RowAct, RowKind};
use crate::paths::Paths;
use crate::settings::Direction;
use crate::thumb;

/// The slots each source app offers today.
pub fn slot_count(system: SystemKind) -> u8 {
    match system {
        SystemKind::Gb | SystemKind::Gba => 9,
        SystemKind::Nes => 1,
        SystemKind::Snes => 5,
    }
}

/// The slot a save or a load uses: the running system's `state_slot` setting, inside the slots
/// that system has. No game means slot 1.
pub fn selected_slot(app: &App) -> u8 {
    let Some(system) = app.system() else {
        return 1;
    };
    let count = slot_count(system);
    app.setting(system, "state_slot")
        .and_then(|word| word.parse().ok())
        .unwrap_or(1)
        .clamp(1, count)
}

/// Moves the selected slot one step, wrapping at both ends, stores it as `state_slot` and returns
/// the new slot.
pub fn step_slot(app: &mut App, direction: Direction) -> u8 {
    let Some(system) = app.system() else {
        return 1;
    };
    let count = slot_count(system);
    let at = selected_slot(app);
    let next = match direction {
        Direction::Right if at >= count => 1,
        Direction::Right => at + 1,
        Direction::Left if at <= 1 => count,
        Direction::Left => at - 1,
    };
    if let Err(why) = config::write_value(
        app.paths(),
        system.key(),
        "state_slot",
        &next.to_string(),
    ) {
        app.show_notice(format!("state slot not written: {why}"), true);
    }
    next
}

/// The file slot `slot` of `rom` is kept in (§6.3, named as the source app names it).
pub fn state_path(paths: &Paths, system: SystemKind, rom: &Path, slot: u8) -> PathBuf {
    let slot = slot.clamp(1, slot_count(system));
    let stem = rom.file_stem().map_or_else(
        || "state".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let name = match system {
        SystemKind::Gb | SystemKind::Gba => format!("{stem}-{slot}.state"),
        SystemKind::Nes => format!("{stem}.state"),
        SystemKind::Snes => format!("{stem}.slot{slot}.state"),
    };
    paths.states_dir(system).join(name)
}

/// Saves the running game's state to `slot`, then its thumbnail. Returns the bar's notice.
///
/// The state is written first and on its own: a thumbnail that cannot be written leaves the state
/// valid and a notice that names the thumbnail's path and the reason behind it, so a full disk or a
/// read-only folder costs the picture but never the save. `Err` is the notice for a state that
/// could not be written at all.
pub fn save_slot(app: &mut App, slot: u8) -> Result<String, String> {
    let Some(system) = app.system() else {
        return Err("refused: no game is running".to_string());
    };
    let Some(rom) = app.rom().map(Path::to_path_buf) else {
        return Err("refused: the game is not a file on disk".to_string());
    };
    let slot = slot.clamp(1, slot_count(system));
    let Some(machine) = app.machine_mut() else {
        return Err("refused: no game is running".to_string());
    };
    let blob = machine.save_state();
    let file = state_path(app.paths(), system, &rom, slot);
    if let Some(dir) = file.parent()
        && let Err(why) = fs::create_dir_all(dir)
    {
        return Err(format!("cannot write {}: {why}", file.display()));
    }
    if let Err(why) = battery::write_atomic(&file, &wrap(system, &blob)) {
        return Err(format!("cannot write {}: {why}", file.display()));
    }
    let picture = app
        .frame()
        .map(|frame| (frame.width, frame.height, frame.rgba.to_vec()));
    Ok(match picture {
        Some((width, height, rgba)) => match thumb::write(&file, system, width, height, &rgba) {
            Ok(_) => format!("state saved to slot {slot}"),
            Err(why) => format!("state saved to slot {slot}; no thumbnail: {why}"),
        },
        None => format!("state saved to slot {slot}; no thumbnail: there is no picture"),
    })
}

/// Loads slot `slot` of the running game. `Err` is the whole notice, the container's or the core's
/// own words unchanged.
pub fn load_slot(app: &mut App, slot: u8) -> Result<(), String> {
    let Some(system) = app.system() else {
        return Err("refused: no game is running".to_string());
    };
    let Some(rom) = app.rom().map(Path::to_path_buf) else {
        return Err("refused: the game is not a file on disk".to_string());
    };
    let slot = slot.clamp(1, slot_count(system));
    let file = state_path(app.paths(), system, &rom, slot);
    let bytes = fs::read(&file).map_err(|why| format!("cannot read {}: {why}", file.display()))?;
    let blob = unwrap(&bytes, system).map_err(|why| why.to_string())?;
    let Some(machine) = app.machine_mut() else {
        return Err("refused: no game is running".to_string());
    };
    machine.load_state(blob).map_err(|why| why.to_string())
}

/// Saves the selected slot with the drawer row's notice. `false` when no game runs.
pub fn save_selected(app: &mut App) -> bool {
    let Some(system) = app.system() else {
        return false;
    };
    let slot = selected_slot(app).clamp(1, slot_count(system));
    match save_slot(app, slot) {
        Ok(text) => app.show_notice(text, false),
        Err(text) => app.show_notice(text, true),
    }
    true
}

/// Loads the selected slot with the drawer row's notice. `false` when no game runs.
pub fn load_selected(app: &mut App) -> bool {
    let Some(system) = app.system() else {
        return false;
    };
    let slot = selected_slot(app).clamp(1, slot_count(system));
    match load_slot(app, slot) {
        Ok(()) => app.show_notice(format!("slot {slot} loaded"), false),
        Err(why) => app.show_notice(why, true),
    }
    true
}

/// The drawer's slot rows: the slot itself (when there is a choice), save, load.
pub fn rows(app: &App) -> Vec<Row> {
    let Some(system) = app.system() else {
        return Vec::new();
    };
    if app.machine().is_none() {
        return Vec::new();
    }
    let Some(rom) = app.rom() else {
        return vec![Row::info(
            "Save state",
            "unavailable",
            "The game was not opened from a file",
        )];
    };
    let count = slot_count(system);
    let slot = selected_slot(app).clamp(1, count);
    let mut rows = Vec::new();
    if count > 1 {
        rows.push(Row {
            label: "State slot".to_string(),
            value: format!("{slot} of {count}"),
            kind: RowKind::Choice,
            desc: "The slot Save state and Load state use",
            hotkey: "",
            requires: None,
            act: None,
        });
    }
    rows.push(Row {
        label: "Save state".to_string(),
        value: String::new(),
        kind: RowKind::Action,
        desc: "Write the state and its picture to this slot",
        hotkey: "F5",
        requires: None,
        act: None,
    });
    let saved = state_path(app.paths(), system, rom, slot).is_file();
    rows.push(Row {
        label: "Load state".to_string(),
        value: if saved { "saved" } else { "empty" }.to_string(),
        kind: RowKind::Action,
        desc: "Return to the state in this slot",
        hotkey: "F7",
        requires: None,
        act: None,
    });
    rows
}

/// Steps the slot row, saves or loads. Returns whether the row was one of ours.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    let Some(system) = app.system() else {
        return false;
    };
    let count = slot_count(system);
    if count > 1 && index == 0 {
        if let RowAct::Step(direction) = act {
            step_slot(app, direction);
            return true;
        }
        return false;
    }
    let save_at = usize::from(count > 1);
    if index == save_at && act == RowAct::Activate {
        save_selected(app);
        return true;
    }
    if index == save_at + 1 && act == RowAct::Activate {
        load_selected(app);
        return true;
    }
    false
}

/// Nothing to do per frame: a state is only written when the user asks.
pub fn on_frame(_app: &mut App) {}
