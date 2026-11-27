//! The drawer's settings rows (DESIGN §6.1, §6.2): the system-level keys of `config.toml`, their
//! value text and the write-through when a value changes. The rows the display, audio and input
//! modules own live with those modules; this file is the settings that have no other home.

use std::path::Path;

use tendy_core::SystemKind;

use crate::app::App;
use crate::config::{self, Config};
use crate::menu::{Row as DrawerRow, RowAct, RowKind};

/// Which way an arrow key steps a choice row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

/// The settings rows of the Emulation page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    StatusBar,
    Rewind,
    Console,
    BootRom,
    SgbSoundRom,
    GbaBios,
    NesFdsBios,
}

/// The rows in drawer order.
pub const ALL: [Row; 7] = [
    Row::StatusBar,
    Row::Rewind,
    Row::Console,
    Row::BootRom,
    Row::SgbSoundRom,
    Row::GbaBios,
    Row::NesFdsBios,
];

/// The `config.toml` key each row writes.
pub fn key(row: Row) -> &'static str {
    match row {
        Row::StatusBar => "status_bar",
        Row::Rewind => "rewind_seconds",
        Row::Console => "console",
        Row::BootRom => "boot_rom",
        Row::SgbSoundRom => "sgb_sound_rom",
        Row::GbaBios => "bios",
        Row::NesFdsBios => "fds_bios",
    }
}

/// The row's label.
pub fn label(row: Row) -> &'static str {
    match row {
        Row::StatusBar => "Status bar",
        Row::Rewind => "Rewind",
        Row::Console => "Console",
        Row::BootRom => "Boot ROM",
        Row::SgbSoundRom => "SGB sound ROM",
        Row::GbaBios => "GBA BIOS",
        Row::NesFdsBios => "FDS BIOS",
    }
}

fn desc(row: Row) -> &'static str {
    match row {
        Row::StatusBar => "The line under the picture",
        Row::Rewind => "How much play can be stepped back",
        Row::Console => "Which Game Boy the cartridge runs as",
        Row::BootRom => "A 256-byte boot ROM file you supply",
        Row::SgbSoundRom => "The SGB cartridge image that carries the sound",
        Row::GbaBios => "A GBA BIOS file you supply",
        Row::NesFdsBios => "An 8192-byte FDS BIOS file you supply",
    }
}

fn kind(row: Row) -> RowKind {
    match row {
        Row::StatusBar => RowKind::Toggle,
        Row::Rewind | Row::Console => RowKind::Choice,
        _ => RowKind::Pick,
    }
}

/// The section the row writes: `[app]` for the shared rows, the system's own section otherwise.
fn section(row: Row) -> &'static str {
    match row {
        Row::StatusBar | Row::Rewind => config::APP,
        Row::Console | Row::BootRom | Row::SgbSoundRom => "gb",
        Row::GbaBios => "gba",
        Row::NesFdsBios => "nes",
    }
}

/// The system whose game shows the row; `None` for a row every system shares.
fn system(row: Row) -> Option<SystemKind> {
    match row {
        Row::StatusBar | Row::Rewind => None,
        Row::Console | Row::BootRom | Row::SgbSoundRom => Some(SystemKind::Gb),
        Row::GbaBios => Some(SystemKind::Gba),
        Row::NesFdsBios => Some(SystemKind::Nes),
    }
}

fn visible(row: Row, running: Option<SystemKind>) -> bool {
    system(row).is_none_or(|system| running == Some(system))
}

const REWIND_CHOICES: [&str; 5] = ["0", "5", "10", "30", "60"];
const CONSOLE_CHOICES: [&str; 4] = ["auto", "dmg", "cgb", "sgb"];

/// The value the row currently shows: the game's system, then `[app]`, then the default.
fn current(config: &Config, running: Option<SystemKind>, row: Row) -> String {
    let system = system(row).or(running).unwrap_or(SystemKind::Gb);
    config
        .value(system, key(row))
        .unwrap_or_default()
        .to_string()
}

/// The row's value column.
pub fn text(config: &Config, running: Option<SystemKind>, row: Row) -> String {
    let value = current(config, running, row);
    match row {
        Row::StatusBar => match value.as_str() {
            "false" => "Off".to_string(),
            _ => "On".to_string(),
        },
        Row::Rewind => match value.as_str() {
            "0" | "" => "off".to_string(),
            seconds => format!("{seconds} s"),
        },
        Row::Console => match value.as_str() {
            "dmg" => "DMG".to_string(),
            "cgb" => "Colour".to_string(),
            "sgb" => "SGB".to_string(),
            _ => "Auto".to_string(),
        },
        _ if value.is_empty() => "(none)".to_string(),
        _ => Path::new(&value)
            .file_name()
            .map_or_else(|| value.clone(), |name| name.to_string_lossy().into_owned()),
    }
}

fn flipped(current: &str) -> &'static str {
    if current == "true" { "false" } else { "true" }
}

/// The next (or previous) word in a wrapped list, starting from the default if the value is not in
/// it.
fn next(choices: &[&'static str], current: &str, default: &str, direction: Direction) -> String {
    let at = choices
        .iter()
        .position(|choice| *choice == current)
        .or_else(|| choices.iter().position(|choice| *choice == default))
        .unwrap_or(0);
    let step = match direction {
        Direction::Left => choices.len() - 1,
        Direction::Right => 1,
    };
    choices[(at + step) % choices.len()].to_string()
}

fn direction(act: &RowAct) -> Option<Direction> {
    match act {
        RowAct::Step(direction) => Some(*direction),
        RowAct::Activate => Some(Direction::Right),
        RowAct::Key(_) => None,
    }
}

/// Applies a choice or toggle to `config`. Path rows are the caller's: they need a file dialog.
pub fn apply(config: &mut Config, running: Option<SystemKind>, row: Row, act: RowAct) -> bool {
    let Some(direction) = direction(&act) else {
        return false;
    };
    let current = current(config, running, row);
    let value = match row {
        Row::StatusBar => flipped(&current).to_string(),
        Row::Rewind => next(&REWIND_CHOICES, &current, "10", direction),
        Row::Console => next(&CONSOLE_CHOICES, &current, "auto", direction),
        _ => return false,
    };
    config.set(section(row), key(row), value);
    true
}

/// The drawer's `SETTINGS` rows for the running system.
pub fn rows(app: &App) -> Vec<DrawerRow> {
    let config = config::load(app.paths()).config;
    let running = app.system();
    ALL.iter()
        .filter(|row| visible(**row, running))
        .map(|row| DrawerRow {
            label: label(*row).to_string(),
            value: text(&config, running, *row),
            kind: kind(*row),
            desc: desc(*row),
            hotkey: "",
            requires: None,
            act: None,
        })
        .collect()
}

/// Writes one changed value. A path row opens the boot-ROM dialog and stores the file it returns.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    let running = app.system();
    let visible_rows: Vec<Row> = ALL
        .iter()
        .copied()
        .filter(|row| visible(*row, running))
        .collect();
    let Some(row) = visible_rows.get(index).copied() else {
        return false;
    };
    let mut config = config::load(app.paths()).config;
    let changed = match row {
        Row::BootRom | Row::SgbSoundRom | Row::GbaBios | Row::NesFdsBios => {
            if !matches!(act, RowAct::Activate) {
                return false;
            }
            let dir = crate::file_pick::start_dir(app.rom());
            let Some(path) = crate::file_pick::boot_rom(&dir) else {
                return false;
            };
            config.set(section(row), key(row), path.to_string_lossy());
            true
        }
        _ => apply(&mut config, running, row, act),
    };
    if !changed {
        return false;
    }
    match config::save(app.paths(), &config) {
        Ok(()) => true,
        Err(why) => {
            app.show_notice(format!("settings not written: {why}"), true);
            false
        }
    }
}

/// Nothing to do per frame: the rows read the file through its cache.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toggle_flips_both_ways() {
        let mut config = Config::new();
        assert!(apply(
            &mut config,
            Some(SystemKind::Gb),
            Row::StatusBar,
            RowAct::Step(Direction::Left)
        ));
        assert_eq!(config.get(config::APP, "status_bar"), Some("false"));
        assert!(apply(
            &mut config,
            Some(SystemKind::Gb),
            Row::StatusBar,
            RowAct::Step(Direction::Right)
        ));
        assert_eq!(config.get(config::APP, "status_bar"), Some("true"));
    }

    #[test]
    fn the_console_row_cycles_auto_dmg_colour_sgb() {
        let mut config = Config::new();
        let gb = Some(SystemKind::Gb);
        assert_eq!(text(&config, gb, Row::Console), "Auto");
        for (word, shown) in [("dmg", "DMG"), ("cgb", "Colour"), ("sgb", "SGB")] {
            apply(
                &mut config,
                gb,
                Row::Console,
                RowAct::Step(Direction::Right),
            );
            assert_eq!(config.get("gb", "console"), Some(word));
            assert_eq!(text(&config, gb, Row::Console), shown);
        }
        apply(
            &mut config,
            gb,
            Row::Console,
            RowAct::Step(Direction::Right),
        );
        assert_eq!(config.get("gb", "console"), Some("auto"));
        apply(&mut config, gb, Row::Console, RowAct::Step(Direction::Left));
        assert_eq!(config.get("gb", "console"), Some("sgb"));
    }

    #[test]
    fn every_row_has_a_label_and_a_key() {
        for row in ALL {
            assert!(!label(row).is_empty());
            assert!(!key(row).is_empty());
        }
    }

    #[test]
    fn a_path_row_shows_the_file_name_or_none() {
        let mut config = Config::new();
        assert_eq!(text(&config, Some(SystemKind::Gb), Row::BootRom), "(none)");
        config.set("gb", "boot_rom", "bios/dmg_boot.bin");
        assert_eq!(
            text(&config, Some(SystemKind::Gb), Row::BootRom),
            "dmg_boot.bin"
        );
    }
}
